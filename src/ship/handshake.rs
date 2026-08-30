//! The SHIP Message Exchange handshake, as a state machine with no I/O.
//!
//! A SHIP connection opens with five phases (SHIP §13.4.4): Connection Mode
//! Initialisation, hello, protocol handshake, PIN verification and — once data exchange
//! is running — access methods. [`Handshake`] implements them as a pure state machine:
//! you feed it received messages and the current time, and ask it what to send, when to
//! wake it again, and what happened.
//!
//! ```
//! use core::time::Duration;
//! use eebus::ship::{Handshake, HandshakeConfig, Role, ShipMessage, Trust};
//!
//! let t0 = Duration::ZERO;
//! let mut client = Handshake::new(Role::Client, HandshakeConfig::default(), Trust::Trusted, t0);
//! let mut server = Handshake::new(Role::Server, HandshakeConfig::default(), Trust::Trusted, t0);
//!
//! // Pump both sides until neither has anything left to say.
//! let mut now = t0;
//! loop {
//!     let mut moved = false;
//!     while let Some(msg) = client.poll_transmit() {
//!         server.handle_message(msg, now).unwrap();
//!         moved = true;
//!     }
//!     while let Some(msg) = server.poll_transmit() {
//!         client.handle_message(msg, now).unwrap();
//!         moved = true;
//!     }
//!     now += Duration::from_millis(1);
//!     if !moved {
//!         break;
//!     }
//! }
//!
//! assert!(client.is_ready_for_data());
//! assert!(server.is_ready_for_data());
//! ```
//!
//! Because nothing here touches a socket or a clock, the whole handshake — including
//! every timeout and the prolongation dance — is exercised in ordinary unit tests with
//! a virtual clock, and the same code runs under Tokio, in a simulator, or on an
//! embedded target.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use super::{
    AccessMethods, AccessMethodsDnsSdMDns, ConnectionClose, ConnectionClosePhase,
    ConnectionCloseReason, ConnectionHello, ConnectionHelloPhase, ConnectionPinError,
    ConnectionPinInput, ConnectionPinState, ControlMessage, EndMessage, FORMAT_JSON_UTF8,
    MessageProtocolFormat, MessageProtocolFormats, MessageProtocolHandshake,
    MessageProtocolHandshakeError, MessageProtocolHandshakeVersion, PinInputPermission, PinState,
    ProtocolHandshakeType, ShipMessage,
};

/// Which end of the TCP connection this node is.
///
/// SHIP is symmetric once data flows, but the protocol handshake is not: the client
/// proposes, the server selects, the client confirms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The node that opened the connection.
    Client,
    /// The node that accepted it.
    Server,
}

/// What this node currently thinks of the peer's public key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The key is known and accepted; the hello phase can complete immediately.
    Trusted,
    /// A person still has to approve the key. The peer is told to wait, and
    /// [`Handshake::set_trust`] delivers the answer when it arrives.
    Pending,
    /// The key is refused; the handshake aborts.
    Rejected,
}

/// Whether this node demands a PIN before it will exchange data (SHIP §13.4.4.3.5.1).
///
/// The default is [`None`](PinRequirement::None), which is also what EEBUS
/// certification exercises: the SHIP test specification fixes the device under test's
/// PIN requirement to `none`, and the specification itself warns that a node cannot
/// know whether its peer has any way to enter a PIN.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PinRequirement {
    /// This node has no PIN and grants unrestricted data exchange.
    #[default]
    None,
    /// A correct PIN is required before data exchange starts.
    Required(String),
    /// Data exchange starts without a PIN but stays restricted until one arrives.
    Optional(String),
}

/// Timers and capabilities of one SHIP node.
///
/// The defaults are the specification's recommended values (SHIP §4.2, Table 1), not
/// the minimum of each permitted range.
#[derive(Clone, Debug)]
pub struct HandshakeConfig {
    /// `CmiTimeout`, permitted 10–30 s, recommended 30 s.
    pub cmi_timeout: Duration,
    /// `T_hello_init`, permitted 60–240 s, recommended 120 s. Also `T_hello_inc`.
    pub hello_init: Duration,
    /// The generic `Wait-Timer` of the protocol handshake, PIN and access phases: 10 s.
    pub wait_timer: Duration,
    /// The highest SHIP version this node speaks; announced as `announceMax`.
    pub max_version: (u16, u16),
    /// Message formats this node supports, most preferred first.
    pub formats: Vec<String>,
    /// This node's PIN requirement.
    pub pin: PinRequirement,
    /// The PIN to send when the peer requires one, if it is known.
    pub peer_pin: Option<String>,
    /// This node's SHIP ID, returned in `accessMethods.id`.
    pub ship_id: Option<String>,
}

impl Default for HandshakeConfig {
    fn default() -> Self {
        Self {
            cmi_timeout: Duration::from_secs(30),
            hello_init: Duration::from_secs(120),
            wait_timer: Duration::from_secs(10),
            max_version: (1, 1),
            formats: vec![FORMAT_JSON_UTF8.to_string()],
            pin: PinRequirement::None,
            peer_pin: None,
            ship_id: None,
        }
    }
}

/// `T_hello_prolong_thr_inc` (SHIP §13.4.4.1.3): below this, a peer's announced waiting
/// time is too short to be worth prolonging.
const T_HELLO_PROLONG_THR_INC: Duration = Duration::from_secs(30);
/// `T_hello_prolong_waiting_gap`: how far ahead of the peer's deadline to ask.
const T_HELLO_PROLONG_WAITING_GAP: Duration = Duration::from_secs(15);
/// `T_hello_prolong_min`: a prolongation request timer below this is pointless, and
/// honouring one would let a peer drive the exchange with near-zero waiting times.
const T_HELLO_PROLONG_MIN: Duration = Duration::from_secs(1);

/// Which of the five phases the connection is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Exchanging the two-byte CMI message.
    Cmi,
    /// Establishing mutual trust.
    Hello,
    /// Agreeing on a SHIP version and message format.
    ProtocolHandshake,
    /// Exchanging PIN requirements.
    Pin,
    /// Data may flow; access-method queries run alongside it.
    DataExchange,
    /// A close has been announced and the connection is winding down.
    Closing,
    /// Finished, in either direction.
    Closed,
    /// Given up. The reason is reported as an [`Event::Aborted`].
    Aborted,
}

/// Something the application needs to know about.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The peer's key is not yet trusted and the peer has been told to wait. Show the
    /// SKI to the user and answer with [`Handshake::set_trust`].
    TrustRequired,
    /// The peer is waiting for *its* user to approve this node's key.
    PeerAwaitingTrust {
        /// How long the peer says it will keep waiting.
        waiting: Duration,
    },
    /// The handshake finished; data exchange is open.
    Ready {
        /// The SHIP version both sides settled on.
        version: (u16, u16),
        /// The message format both sides settled on.
        format: String,
    },
    /// The peer reported its PIN requirement.
    PeerPinState(PinState),
    /// The peer supplied its access methods.
    PeerAccessMethods(AccessMethods),
    /// The connection is finished or was given up.
    Aborted(AbortReason),
}

/// Why a handshake ended early.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AbortReason {
    /// A timer expired.
    #[error("timeout in the {0:?} phase")]
    Timeout(Phase),
    /// A message arrived that the current state does not allow.
    #[error("unexpected message in the {0:?} phase")]
    UnexpectedMessage(Phase),
    /// The version or format the peer selected is not one this node offered.
    #[error("protocol selection mismatch")]
    SelectionMismatch,
    /// The peer's key was refused locally.
    #[error("trust refused locally")]
    TrustRejected,
    /// The peer refused this node's key.
    #[error("peer refused trust")]
    PeerAborted,
    /// The peer closed the connection.
    #[error("connection closed by peer: {0}")]
    Closed(&'static str),
}

/// An error in driving the state machine, as opposed to a protocol-level abort.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    /// The handshake has already finished or aborted.
    #[error("the handshake is no longer running (phase {0:?})")]
    NotRunning(Phase),
}

/// The state of one side of a SHIP handshake.
#[derive(Debug)]
pub struct Handshake {
    role: Role,
    config: HandshakeConfig,
    phase: Phase,
    trust: Trust,

    cmi_sent: bool,
    cmi_received: bool,

    hello_local_ready: bool,
    hello_remote_ready: bool,
    hello_announced_pending: bool,
    /// The peer's most recently announced waiting time, used to size our own timers.
    hello_peer_waiting: Option<Duration>,

    /// What the server offered, kept to compare against the client's confirmation.
    proposed: Option<MessageProtocolHandshake>,
    negotiated: Option<((u16, u16), String)>,

    local_pin_state: PinState,
    peer_pin_state: Option<PinState>,
    pin_attempts: u32,

    timers: Timers,
    outbox: VecDeque<ShipMessage>,
    events: VecDeque<Event>,
}

#[derive(Debug, Default)]
struct Timers {
    cmi: Option<Duration>,
    wait_for_ready: Option<Duration>,
    send_prolongation: Option<Duration>,
    prolongation_reply: Option<Duration>,
    wait: Option<Duration>,
}

impl Timers {
    fn earliest(&self) -> Option<Duration> {
        [
            self.cmi,
            self.wait_for_ready,
            self.send_prolongation,
            self.prolongation_reply,
            self.wait,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn clear_hello(&mut self) {
        self.wait_for_ready = None;
        self.send_prolongation = None;
        self.prolongation_reply = None;
    }
}

impl Handshake {
    /// Starts a handshake and queues the CMI message.
    ///
    /// `now` is a reading of a monotonic clock; every later call must pass a value from
    /// the same clock. Nothing here reads the clock itself, which is what lets tests
    /// drive years of timeouts in microseconds.
    pub fn new(role: Role, config: HandshakeConfig, trust: Trust, now: Duration) -> Self {
        let local_pin_state = match config.pin {
            PinRequirement::None => PinState::None,
            PinRequirement::Required(_) => PinState::Required,
            PinRequirement::Optional(_) => PinState::Optional,
        };
        let mut hs = Self {
            role,
            config,
            phase: Phase::Cmi,
            trust,
            cmi_sent: false,
            cmi_received: false,
            hello_local_ready: false,
            hello_remote_ready: false,
            hello_announced_pending: false,
            hello_peer_waiting: None,
            proposed: None,
            negotiated: None,
            local_pin_state,
            peer_pin_state: None,
            pin_attempts: 0,
            timers: Timers::default(),
            outbox: VecDeque::new(),
            events: VecDeque::new(),
        };
        hs.outbox.push_back(ShipMessage::Cmi);
        hs.cmi_sent = true;
        hs.timers.cmi = Some(now + hs.config.cmi_timeout);
        hs
    }

    /// The phase the connection is in.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// True once SPINE messages may be exchanged.
    pub fn is_ready_for_data(&self) -> bool {
        matches!(self.phase, Phase::DataExchange | Phase::Closing)
    }

    /// The SHIP version and message format both sides agreed on.
    pub fn negotiated(&self) -> Option<&((u16, u16), String)> {
        self.negotiated.as_ref()
    }

    /// The peer's PIN requirement, once it has reported one.
    pub fn peer_pin_state(&self) -> Option<PinState> {
        self.peer_pin_state
    }

    /// The next message to put on the wire.
    pub fn poll_transmit(&mut self) -> Option<ShipMessage> {
        self.outbox.pop_front()
    }

    /// The next thing that happened.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called, if ever.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.timers.earliest()
    }

    /// Delivers the local trust decision for a peer whose key was unknown.
    pub fn set_trust(&mut self, trust: Trust, now: Duration) -> Result<(), HandshakeError> {
        if matches!(self.phase, Phase::Aborted | Phase::Closed) {
            return Err(HandshakeError::NotRunning(self.phase));
        }
        self.trust = trust;
        if self.phase == Phase::Hello {
            self.announce_hello(now);
            self.try_finish_hello(now);
        }
        Ok(())
    }

    /// Announces a connection termination (SHIP §13.4.8).
    pub fn close(&mut self, reason: ConnectionCloseReason, max_time: Duration) {
        if matches!(self.phase, Phase::Closed | Phase::Aborted) {
            return;
        }
        self.phase = Phase::Closing;
        self.outbox
            .push_back(ShipMessage::End(EndMessage::ConnectionClose(
                ConnectionClose {
                    phase: Some(ConnectionClosePhase::Announce),
                    max_time: Some(max_time.as_millis().min(u128::from(u32::MAX)) as u32),
                    reason: Some(reason),
                },
            )));
    }

    /// Asks the peer for the addresses it can be reached at (SHIP §13.4.6).
    ///
    /// Only meaningful once data exchange is running, and — per the SHIP implementation
    /// guide §2.1 — it must never stall SPINE traffic while the answer is outstanding.
    pub fn request_access_methods(&mut self) {
        if self.is_ready_for_data() {
            self.outbox
                .push_back(ShipMessage::Control(ControlMessage::AccessMethodsRequest(
                    Default::default(),
                )));
        }
    }

    /// Feeds a received message into the state machine.
    pub fn handle_message(
        &mut self,
        message: ShipMessage,
        now: Duration,
    ) -> Result<(), HandshakeError> {
        if matches!(self.phase, Phase::Aborted | Phase::Closed) {
            return Err(HandshakeError::NotRunning(self.phase));
        }

        match message {
            ShipMessage::Cmi => self.on_cmi(now),
            ShipMessage::End(EndMessage::ConnectionClose(close)) => self.on_close(close),
            ShipMessage::Data(_) => {
                // Data before the handshake completes is out of order; afterwards it is
                // the caller's business, not the handshake's.
                if !self.is_ready_for_data() {
                    self.abort(AbortReason::UnexpectedMessage(self.phase));
                }
            }
            ShipMessage::Control(control) => self.on_control(control, now),
        }
        Ok(())
    }

    /// Advances timers. Call at or after the instant [`poll_timeout`](Self::poll_timeout)
    /// reported.
    pub fn handle_timeout(&mut self, now: Duration) -> Result<(), HandshakeError> {
        if matches!(self.phase, Phase::Aborted | Phase::Closed) {
            return Err(HandshakeError::NotRunning(self.phase));
        }

        if expired(&mut self.timers.cmi, now) {
            self.abort(AbortReason::Timeout(Phase::Cmi));
            return Ok(());
        }
        if expired(&mut self.timers.wait, now) {
            let phase = self.phase;
            self.send_protocol_error(1); // 1 = timeout
            self.abort(AbortReason::Timeout(phase));
            return Ok(());
        }
        if expired(&mut self.timers.send_prolongation, now) {
            self.send_prolongation_request(now);
        }
        if expired(&mut self.timers.prolongation_reply, now) {
            self.abort(AbortReason::Timeout(Phase::Hello));
            return Ok(());
        }
        if expired(&mut self.timers.wait_for_ready, now) {
            if self.trust == Trust::Pending {
                // Still waiting for a person: renew our announcement rather than drop
                // the connection, so the peer can keep prolonging.
                self.announce_hello(now);
            } else {
                self.abort(AbortReason::Timeout(Phase::Hello));
            }
        }
        Ok(())
    }

    // ---- CMI ------------------------------------------------------------------

    fn on_cmi(&mut self, now: Duration) {
        if self.phase != Phase::Cmi {
            self.abort(AbortReason::UnexpectedMessage(self.phase));
            return;
        }
        self.cmi_received = true;
        if self.cmi_sent && self.cmi_received {
            self.timers.cmi = None;
            self.enter_hello(now);
        }
    }

    // ---- Hello ----------------------------------------------------------------

    fn enter_hello(&mut self, now: Duration) {
        self.phase = Phase::Hello;
        self.announce_hello(now);
    }

    /// Sends this node's current hello state and (re)starts the Wait-For-Ready timer.
    fn announce_hello(&mut self, now: Duration) {
        let waiting = self.config.hello_init;
        let hello = match self.trust {
            Trust::Trusted => {
                self.hello_local_ready = true;
                ConnectionHello {
                    phase: Some(ConnectionHelloPhase::Ready),
                    waiting: Some(millis(waiting)),
                    ..Default::default()
                }
            }
            Trust::Pending => {
                if !self.hello_announced_pending {
                    self.hello_announced_pending = true;
                    self.events.push_back(Event::TrustRequired);
                }
                ConnectionHello {
                    phase: Some(ConnectionHelloPhase::Pending),
                    waiting: Some(millis(waiting)),
                    ..Default::default()
                }
            }
            Trust::Rejected => {
                self.outbox
                    .push_back(ShipMessage::Control(ControlMessage::ConnectionHello(
                        ConnectionHello {
                            phase: Some(ConnectionHelloPhase::Aborted),
                            ..Default::default()
                        },
                    )));
                self.abort(AbortReason::TrustRejected);
                return;
            }
        };
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::ConnectionHello(hello)));
        self.timers.wait_for_ready = Some(now + waiting);
    }

    fn on_hello(&mut self, hello: ConnectionHello, now: Duration) {
        if self.phase != Phase::Hello {
            // A hello during data exchange is not an error worth dropping a working
            // connection over; ignore it.
            return;
        }
        let peer_waiting = hello.waiting.map(|ms| Duration::from_millis(u64::from(ms)));
        if let Some(waiting) = peer_waiting {
            self.hello_peer_waiting = Some(waiting);
        }

        match hello.phase {
            Some(ConnectionHelloPhase::Ready) => {
                self.hello_remote_ready = true;
                self.timers.send_prolongation = None;
                self.timers.prolongation_reply = None;
                self.try_finish_hello(now);
            }
            Some(ConnectionHelloPhase::Pending) => {
                self.hello_remote_ready = false;
                self.timers.prolongation_reply = None;
                if let Some(waiting) = peer_waiting {
                    self.events.push_back(Event::PeerAwaitingTrust { waiting });
                    self.arm_prolongation(waiting, now);
                }
            }
            Some(ConnectionHelloPhase::Aborted) => {
                self.abort(AbortReason::PeerAborted);
                return;
            }
            None => {
                self.abort(AbortReason::UnexpectedMessage(Phase::Hello));
                return;
            }
        }

        // A prolongation request may accompany either phase: the peer asking is
        // whichever side's Wait-For-Ready-Timer is about to expire, and that is
        // usually the side that is already `ready` and waiting on us.
        if self.phase == Phase::Hello && hello.prolongation_request == Some(true) {
            self.accept_prolongation(now);
        }
    }

    /// Arms the Send-Prolongation-Request timer from the peer's announced waiting time.
    ///
    /// SHIP §13.4.4.1.3: the new value is the peer's `waiting` less
    /// `T_hello_prolong_waiting_gap`, and the timer is disabled when that would fall
    /// below `T_hello_prolong_min` — which also stops a peer from driving us with
    /// near-zero waiting times.
    fn arm_prolongation(&mut self, peer_waiting: Duration, now: Duration) {
        if peer_waiting < T_HELLO_PROLONG_THR_INC {
            self.timers.send_prolongation = None;
            return;
        }
        let lead = peer_waiting.saturating_sub(T_HELLO_PROLONG_WAITING_GAP);
        if lead < T_HELLO_PROLONG_MIN {
            self.timers.send_prolongation = None;
            return;
        }
        self.timers.send_prolongation = Some(now + lead);
    }

    fn send_prolongation_request(&mut self, now: Duration) {
        let phase = if self.hello_local_ready {
            ConnectionHelloPhase::Ready
        } else {
            ConnectionHelloPhase::Pending
        };
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::ConnectionHello(
                ConnectionHello {
                    phase: Some(phase),
                    waiting: Some(millis(self.config.hello_init)),
                    prolongation_request: Some(true),
                    ..Default::default()
                },
            )));
        // Expect an answer within the peer's last announced waiting time.
        let reply_window = self
            .hello_peer_waiting
            .unwrap_or(self.config.hello_init)
            .max(T_HELLO_PROLONG_MIN);
        self.timers.prolongation_reply = Some(now + reply_window);
    }

    /// Grants a peer's prolongation request by extending our own Wait-For-Ready timer
    /// by `T_hello_inc` and telling the peer the new figure (SHIP §13.4.4.1.3).
    fn accept_prolongation(&mut self, now: Duration) {
        let extended = self.config.hello_init;
        self.timers.wait_for_ready = Some(now + extended);
        let phase = if self.hello_local_ready {
            ConnectionHelloPhase::Ready
        } else {
            ConnectionHelloPhase::Pending
        };
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::ConnectionHello(
                ConnectionHello {
                    phase: Some(phase),
                    waiting: Some(millis(extended)),
                    ..Default::default()
                },
            )));
    }

    fn try_finish_hello(&mut self, now: Duration) {
        if self.hello_local_ready && self.hello_remote_ready {
            self.timers.clear_hello();
            self.enter_protocol_handshake(now);
        }
    }

    // ---- Protocol handshake ---------------------------------------------------

    fn enter_protocol_handshake(&mut self, now: Duration) {
        self.phase = Phase::ProtocolHandshake;
        self.timers.wait = Some(now + self.config.wait_timer);
        if self.role == Role::Client {
            let (major, minor) = self.config.max_version;
            self.outbox.push_back(ShipMessage::Control(
                ControlMessage::MessageProtocolHandshake(MessageProtocolHandshake {
                    handshake_type: Some(ProtocolHandshakeType::AnnounceMax),
                    version: Some(MessageProtocolHandshakeVersion {
                        major: Some(major),
                        minor: Some(minor),
                    }),
                    formats: Some(MessageProtocolFormats {
                        format: Some(
                            self.config
                                .formats
                                .iter()
                                .map(|f| MessageProtocolFormat(f.clone()))
                                .collect(),
                        ),
                    }),
                }),
            ));
        }
    }

    fn on_protocol_handshake(&mut self, msg: MessageProtocolHandshake, now: Duration) {
        if self.phase != Phase::ProtocolHandshake {
            self.abort(AbortReason::UnexpectedMessage(self.phase));
            return;
        }
        self.timers.wait = None;

        match self.role {
            Role::Server if self.proposed.is_none() => {
                if msg.handshake_type != Some(ProtocolHandshakeType::AnnounceMax) {
                    self.send_protocol_error(2);
                    self.abort(AbortReason::UnexpectedMessage(Phase::ProtocolHandshake));
                    return;
                }
                let Some(version) = self.select_version(&msg) else {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                };
                let Some(format) = self.select_format(&msg) else {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                };
                let selection = MessageProtocolHandshake {
                    handshake_type: Some(ProtocolHandshakeType::Select),
                    version: Some(MessageProtocolHandshakeVersion {
                        major: Some(version.0),
                        minor: Some(version.1),
                    }),
                    formats: Some(MessageProtocolFormats {
                        format: Some(vec![MessageProtocolFormat(format.clone())]),
                    }),
                };
                self.proposed = Some(selection.clone());
                self.negotiated = Some((version, format));
                self.outbox.push_back(ShipMessage::Control(
                    ControlMessage::MessageProtocolHandshake(selection),
                ));
                self.timers.wait = Some(now + self.config.wait_timer);
            }
            Role::Server => {
                // The client's confirmation must be byte-for-byte what we proposed.
                if self.proposed.as_ref() != Some(&msg) {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                }
                self.enter_pin(now);
            }
            Role::Client => {
                if msg.handshake_type != Some(ProtocolHandshakeType::Select) {
                    self.send_protocol_error(2);
                    self.abort(AbortReason::UnexpectedMessage(Phase::ProtocolHandshake));
                    return;
                }
                let Some(version) = msg
                    .version
                    .as_ref()
                    .and_then(|v| Some((v.major?, v.minor?)))
                    .filter(|v| self.supports_version(*v))
                else {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                };
                let formats = msg
                    .formats
                    .as_ref()
                    .and_then(|f| f.format.as_ref())
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let [only] = formats else {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                };
                if !self.config.formats.iter().any(|f| f == &only.0) {
                    self.send_protocol_error(3);
                    self.abort(AbortReason::SelectionMismatch);
                    return;
                }
                self.negotiated = Some((version, only.0.clone()));
                // Echoing the server's message back confirms the choice.
                self.outbox.push_back(ShipMessage::Control(
                    ControlMessage::MessageProtocolHandshake(msg),
                ));
                self.enter_pin(now);
            }
        }
    }

    /// The highest version both sides speak.
    ///
    /// SHIP §13.4.4.2.2 requires every node to support all versions from 1.0 up to its
    /// own maximum, so the common maximum is simply the lower of the two.
    fn select_version(&self, msg: &MessageProtocolHandshake) -> Option<(u16, u16)> {
        let peer = msg
            .version
            .as_ref()
            .and_then(|v| Some((v.major?, v.minor?)))?;
        let ours = self.config.max_version;
        let chosen = if peer <= ours { peer } else { ours };
        (chosen.0 >= 1).then_some(chosen)
    }

    fn supports_version(&self, version: (u16, u16)) -> bool {
        version.0 >= 1 && version <= self.config.max_version
    }

    /// The first format this node prefers that the peer also offers.
    fn select_format(&self, msg: &MessageProtocolHandshake) -> Option<String> {
        let peer = msg.formats.as_ref()?.format.as_ref()?;
        self.config
            .formats
            .iter()
            .find(|ours| peer.iter().any(|theirs| &theirs.0 == *ours))
            .cloned()
    }

    fn send_protocol_error(&mut self, error: u8) {
        if self.phase == Phase::ProtocolHandshake {
            self.outbox.push_back(ShipMessage::Control(
                ControlMessage::MessageProtocolHandshakeError(MessageProtocolHandshakeError {
                    error: Some(super::MessageProtocolHandshakeErrorError(error)),
                }),
            ));
        }
    }

    // ---- PIN ------------------------------------------------------------------

    fn enter_pin(&mut self, now: Duration) {
        self.phase = Phase::Pin;
        self.timers.wait = Some(now + self.config.wait_timer);
        let input_permission = match self.local_pin_state {
            // §13.4.4.3.5.1: `inputPermission` is present exactly when a PIN can be sent.
            PinState::Required | PinState::Optional => Some(PinInputPermission::Ok),
            _ => None,
        };
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::ConnectionPinState(
                ConnectionPinState {
                    pin_state: Some(self.local_pin_state),
                    input_permission,
                },
            )));
    }

    fn on_pin_state(&mut self, state: ConnectionPinState, now: Duration) {
        let Some(pin_state) = state.pin_state else {
            self.abort(AbortReason::UnexpectedMessage(self.phase));
            return;
        };
        self.timers.wait = None;
        self.peer_pin_state = Some(pin_state);
        self.events.push_back(Event::PeerPinState(pin_state));

        // If the peer wants a PIN and we know it, send it; otherwise the peer will keep
        // data exchange restricted, which is its right.
        if matches!(pin_state, PinState::Required | PinState::Optional)
            && state.input_permission != Some(PinInputPermission::Busy)
            && let Some(pin) = self.config.peer_pin.clone()
        {
            self.outbox
                .push_back(ShipMessage::Control(ControlMessage::ConnectionPinInput(
                    ConnectionPinInput {
                        pin: Some(super::PinValue(pin)),
                    },
                )));
        }

        self.try_finish_pin(now);
    }

    fn on_pin_input(&mut self, input: ConnectionPinInput, now: Duration) {
        let expected = match &self.config.pin {
            PinRequirement::Required(p) | PinRequirement::Optional(p) => Some(p.clone()),
            PinRequirement::None => None,
        };
        let matches_pin = match (&expected, &input.pin) {
            (Some(expected), Some(got)) => constant_time_eq(expected.as_bytes(), got.0.as_bytes()),
            _ => false,
        };

        if matches_pin {
            self.local_pin_state = PinState::PinOk;
            self.outbox
                .push_back(ShipMessage::Control(ControlMessage::ConnectionPinState(
                    ConnectionPinState {
                        pin_state: Some(PinState::PinOk),
                        input_permission: None,
                    },
                )));
            self.try_finish_pin(now);
        } else {
            self.pin_attempts += 1;
            self.outbox
                .push_back(ShipMessage::Control(ControlMessage::ConnectionPinError(
                    ConnectionPinError {
                        error: Some(super::ConnectionPinErrorError(1)),
                    },
                )));
        }
    }

    /// Data exchange opens as soon as this node's own PIN requirement permits it
    /// (§13.4.4.3.5.2) and the peer has reported its requirement.
    fn try_finish_pin(&mut self, _now: Duration) {
        if self.phase != Phase::Pin {
            return;
        }
        let local_ok = matches!(
            self.local_pin_state,
            PinState::None | PinState::PinOk | PinState::Optional
        );
        if local_ok && self.peer_pin_state.is_some() {
            self.timers.wait = None;
            self.phase = Phase::DataExchange;
            if let Some((version, format)) = self.negotiated.clone() {
                self.events.push_back(Event::Ready { version, format });
            }
        }
    }

    // ---- Control dispatch and shutdown ----------------------------------------

    fn on_control(&mut self, control: ControlMessage, now: Duration) {
        match control {
            ControlMessage::ConnectionHello(hello) => self.on_hello(hello, now),
            ControlMessage::MessageProtocolHandshake(msg) => self.on_protocol_handshake(msg, now),
            ControlMessage::MessageProtocolHandshakeError(_) => {
                self.abort(AbortReason::SelectionMismatch);
            }
            ControlMessage::ConnectionPinState(state) => self.on_pin_state(state, now),
            ControlMessage::ConnectionPinInput(input) => self.on_pin_input(input, now),
            ControlMessage::ConnectionPinError(_) => {
                // The peer rejected our PIN. Data exchange may still proceed, restricted.
            }
            ControlMessage::AccessMethodsRequest(_) => {
                // The implementation guide §2.1 is explicit: answer at once and never
                // let this exchange hold up SPINE traffic in either direction.
                self.outbox
                    .push_back(ShipMessage::Control(ControlMessage::AccessMethods(
                        AccessMethods {
                            id: self.config.ship_id.clone(),
                            dns_sd_m_dns: Some(AccessMethodsDnsSdMDns::default()),
                            dns: None,
                        },
                    )));
            }
            ControlMessage::AccessMethods(methods) => {
                self.events.push_back(Event::PeerAccessMethods(methods));
            }
            // SHIP 1.1.0 key material exchange is handled above the handshake.
            _ => {}
        }
    }

    fn on_close(&mut self, close: ConnectionClose) {
        match close.phase {
            Some(ConnectionClosePhase::Announce) => {
                self.outbox
                    .push_back(ShipMessage::End(EndMessage::ConnectionClose(
                        ConnectionClose {
                            phase: Some(ConnectionClosePhase::Confirm),
                            ..Default::default()
                        },
                    )));
                self.finish(AbortReason::Closed("announced by peer"));
            }
            Some(ConnectionClosePhase::Confirm) => {
                self.finish(AbortReason::Closed("confirmed by peer"));
            }
            None => self.abort(AbortReason::UnexpectedMessage(self.phase)),
        }
    }

    fn abort(&mut self, reason: AbortReason) {
        if matches!(self.phase, Phase::Aborted | Phase::Closed) {
            return;
        }
        self.phase = Phase::Aborted;
        self.timers = Timers::default();
        self.events.push_back(Event::Aborted(reason));
    }

    fn finish(&mut self, reason: AbortReason) {
        if matches!(self.phase, Phase::Aborted | Phase::Closed) {
            return;
        }
        self.phase = Phase::Closed;
        self.timers = Timers::default();
        self.events.push_back(Event::Aborted(reason));
    }
}

fn millis(d: Duration) -> u32 {
    d.as_millis().min(u128::from(u32::MAX)) as u32
}

fn expired(slot: &mut Option<Duration>, now: Duration) -> bool {
    match *slot {
        Some(deadline) if now >= deadline => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// Compares two byte strings without leaking their contents through timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
