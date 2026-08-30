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
    ConnectionCloseReason, ConnectionHello, ConnectionHelloKeyMaterialState, ConnectionHelloPhase,
    ConnectionPinError, ConnectionPinInput, ConnectionPinState, ControlMessage, EndMessage,
    FORMAT_JSON_UTF8, KeyMaterialState, KeyMaterialStateRequest, KeyMaterialStateResponse,
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
    /// This node's key material, if it takes part in SHIP 1.1.0 certificate updates.
    ///
    /// Present means the `hello` phase carries the `updateCounter` of §12.1.3.4, so a
    /// peer that has been away can tell at once that this node's certificate has changed
    /// and ask for the rest. Absent means the node behaves as a 1.0.1 node does.
    pub key_material: Option<super::OwnKeys>,
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
            key_material: None,
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

/// The penalty after the third to fifth invalid PIN (SHIP §13.4.4.3.4 rule 2).
///
/// The specification permits 10–15 seconds and the parameter table recommends 15; longer
/// only "in case of increased security requirements".
pub const PIN_PENALTY_SHORT: Duration = Duration::from_secs(15);

/// The penalty after the sixth invalid PIN and beyond (§13.4.4.3.4 rule 3).
pub const PIN_PENALTY_LONG: Duration = Duration::from_secs(90);

/// How many invalid PINs are tolerated before the short penalty applies.
const PIN_ATTEMPTS_BEFORE_SHORT_PENALTY: u32 = 3;

/// How many invalid PINs before the long penalty applies.
const PIN_ATTEMPTS_BEFORE_LONG_PENALTY: u32 = 6;

/// The penalty owed after `attempts` invalid PINs.
///
/// The point of the escalation is that a brute-force search over an eight-digit hex PIN
/// becomes arithmetically impossible rather than merely slow: at ninety seconds a try,
/// the search space outlives the device.
pub const fn pin_penalty(attempts: u32) -> Option<Duration> {
    if attempts >= PIN_ATTEMPTS_BEFORE_LONG_PENALTY {
        Some(PIN_PENALTY_LONG)
    } else if attempts >= PIN_ATTEMPTS_BEFORE_SHORT_PENALTY {
        Some(PIN_PENALTY_SHORT)
    } else {
        None
    }
}

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
    /// The peer's `hello` announced the state of its key material (SHIP §12.1.3.4).
    ///
    /// Compare it with what is stored for this peer: a higher counter means its
    /// certificate has changed, and [`Handshake::request_key_material`] asks for the
    /// rest once data exchange opens.
    PeerKeyMaterialCounter {
        /// What the peer announced.
        update_counter: u16,
    },
    /// The peer sent its key material.
    ///
    /// It has already been acknowledged; what to do with it is
    /// [`PeerKeys::apply`](super::PeerKeys::apply)'s answer, and the curve to pass is
    /// the one this connection's TLS negotiated.
    PeerKeyMaterial(KeyMaterialState),
    /// The peer asked for this node's key material, which has been sent.
    PeerAskedForKeyMaterial {
        /// The counter the peer says it holds.
        known_update_counter: u16,
    },
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
    /// When the peer's Wait-For-Ready-Timer expires, by its own last announcement.
    ///
    /// The instant, not the span: a prolongation request has to precede *that*, and
    /// re-deriving it from a later `now` — when this node re-announces, say — would slide
    /// the request past the deadline it exists to beat.
    hello_peer_deadline: Option<Duration>,

    /// What the server offered, kept to compare against the client's confirmation.
    proposed: Option<MessageProtocolHandshake>,
    negotiated: Option<((u16, u16), String)>,

    /// When the outstanding `keyMaterialState` was sent, for the resend of §12.1.3.2.
    key_material_sent_at: Option<Duration>,
    key_material_resent: bool,

    local_pin_state: PinState,
    peer_pin_state: Option<PinState>,
    pin_attempts: u32,
    /// When the current brute-force penalty ends (§13.4.4.3.4).
    pin_penalty_until: Option<Duration>,

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
    /// The brute-force penalty of §13.4.4.3.4, and the close handshake's `maxTime`.
    pin_penalty: Option<Duration>,
    close: Option<Duration>,
    /// Waiting for a `keyMaterialStateResponse` (§12.1.3.2).
    key_material: Option<Duration>,
}

impl Timers {
    fn earliest(&self) -> Option<Duration> {
        [
            self.cmi,
            self.wait_for_ready,
            self.send_prolongation,
            self.prolongation_reply,
            self.wait,
            self.pin_penalty,
            self.close,
            self.key_material,
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
            hello_peer_deadline: None,
            proposed: None,
            negotiated: None,
            key_material_sent_at: None,
            key_material_resent: false,
            local_pin_state,
            peer_pin_state: None,
            pin_attempts: 0,
            pin_penalty_until: None,
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
    ///
    /// The close is a handshake of its own: `connectionClose{announce}` goes out, the
    /// peer confirms, and only then is the connection finished. `max_time` is what the
    /// peer is told it has to answer in, and it is also how long this waits — a peer that
    /// has already gone cannot confirm anything, and a node that waited for a
    /// confirmation that will never come would hold the socket forever. When it expires,
    /// [`handle_timeout`](Self::handle_timeout) finishes the close.
    ///
    /// Announcing it matters: without it a peer cannot tell a deliberate shutdown from a
    /// network failure, and will keep retrying against a node that has gone on purpose.
    pub fn close(&mut self, reason: ConnectionCloseReason, max_time: Duration, now: Duration) {
        if matches!(self.phase, Phase::Closed | Phase::Aborted | Phase::Closing) {
            return;
        }
        self.phase = Phase::Closing;
        self.timers.close = Some(now + max_time);
        self.outbox
            .push_back(ShipMessage::End(EndMessage::ConnectionClose(
                ConnectionClose {
                    phase: Some(ConnectionClosePhase::Announce),
                    max_time: Some(max_time.as_millis().min(u128::from(u32::MAX)) as u32),
                    reason: Some(reason),
                },
            )));
    }

    /// Sends a PIN to the peer, as entered by a user.
    ///
    /// The peer having said it requires one, this is how the answer gets there: a device
    /// with a display shows the request, a person types the PIN off the other device's
    /// label, and it arrives here. Where the PIN is known in advance —
    /// [`HandshakeConfig::peer_pin`] — it is sent automatically instead.
    ///
    /// It stays available after this node's own data exchange has opened: §13.4.4.3.5.2
    /// lets each side decide for itself, so a node with no PIN of its own is already in
    /// data exchange while the peer is still holding back for one.
    pub fn send_pin(&mut self, pin: impl Into<String>) {
        if !matches!(self.phase, Phase::Pin | Phase::DataExchange) {
            return;
        }
        if !matches!(
            self.peer_pin_state,
            Some(PinState::Required | PinState::Optional)
        ) {
            return;
        }
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::ConnectionPinInput(
                ConnectionPinInput {
                    pin: Some(super::PinValue(pin.into())),
                },
            )));
    }

    /// Sends this node's key material to the peer (SHIP §12.1.3.2).
    ///
    /// Called when a certificate update begins, and answered by the peer with a
    /// `keyMaterialStateResponse`. If none arrives within
    /// [`STATE_RESPONSE_TIMEOUT`](super::STATE_RESPONSE_TIMEOUT) the message goes again;
    /// if the second attempt is also unanswered the connection is given up, because a
    /// peer that will not acknowledge a certificate update is a peer that will stop being
    /// able to talk to this node when the transition ends.
    ///
    /// Does nothing for a node with no key material configured, or before data exchange
    /// opens.
    pub fn send_key_material(&mut self, now: Duration) {
        let Some(message) = self
            .config
            .key_material
            .as_ref()
            .map(super::OwnKeys::to_message)
        else {
            return;
        };
        if !self.is_ready_for_data() {
            return;
        }
        self.outbox
            .push_back(ShipMessage::Control(ControlMessage::KeyMaterialState(
                message,
            )));
        self.key_material_sent_at = Some(now);
        self.key_material_resent = false;
        self.timers.key_material = Some(now + super::STATE_RESPONSE_TIMEOUT);
    }

    /// Asks the peer for its key material (SHIP §12.1.3.4).
    ///
    /// `known_update_counter` is what this node has stored for the peer. Sending it lets
    /// the peer answer only when it has something newer — and settles the race where both
    /// ends decide to talk about certificates at once.
    pub fn request_key_material(&mut self, known_update_counter: u16) {
        if !self.is_ready_for_data() {
            return;
        }
        self.outbox.push_back(ShipMessage::Control(
            ControlMessage::KeyMaterialStateRequest(KeyMaterialStateRequest {
                known_update_counter: Some(known_update_counter),
            }),
        ));
    }

    /// Whether a `keyMaterialState` is still waiting to be acknowledged.
    pub fn key_material_outstanding(&self) -> bool {
        self.key_material_sent_at.is_some()
    }

    /// How many invalid PINs this peer has sent.
    pub fn invalid_pin_attempts(&self) -> u32 {
        self.pin_attempts
    }

    /// Whether a brute-force penalty is in force, so `inputPermission` reads `busy`.
    pub fn pin_penalty_active(&self) -> bool {
        self.pin_penalty_until.is_some()
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
        if expired(&mut self.timers.close, now) {
            // §13.4.8: the peer was given `maxTime` to confirm and did not.
            self.finish(AbortReason::Closed("no confirmation within maxTime"));
            return Ok(());
        }
        if expired(&mut self.timers.key_material, now) {
            // §12.1.3.2: resend once, then give up on this connection and try a fresh one.
            if self.key_material_resent {
                self.key_material_sent_at = None;
                self.key_material_resent = false;
                self.abort(AbortReason::Timeout(Phase::DataExchange));
                return Ok(());
            }
            self.key_material_resent = true;
            if let Some(message) = self
                .config
                .key_material
                .as_ref()
                .map(super::OwnKeys::to_message)
            {
                self.outbox
                    .push_back(ShipMessage::Control(ControlMessage::KeyMaterialState(
                        message,
                    )));
                self.key_material_sent_at = Some(now);
                self.timers.key_material = Some(now + super::STATE_RESPONSE_TIMEOUT);
            }
        }
        if expired(&mut self.timers.pin_penalty, now) {
            // §13.4.4.3.5: the penalty is over, so input is permitted again.
            self.pin_penalty_until = None;
            self.send_pin_state();
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

    /// The key-material state to put in a `hello` (SHIP §12.1.3.4).
    ///
    /// Present only for a node that takes part in certificate updates. It is what lets a
    /// peer notice, in the first phase of a new connection, that this node's certificate
    /// has changed since they last spoke.
    fn hello_key_material(&self) -> Option<ConnectionHelloKeyMaterialState> {
        Some(ConnectionHelloKeyMaterialState {
            update_counter: Some(self.config.key_material.as_ref()?.update_counter()),
        })
    }

    /// Sends this node's current hello state and (re)starts the Wait-For-Ready timer.
    fn announce_hello(&mut self, now: Duration) {
        let waiting = self.config.hello_init;
        let key_material_state = self.hello_key_material();
        let hello = match self.trust {
            Trust::Trusted => {
                self.hello_local_ready = true;
                ConnectionHello {
                    phase: Some(ConnectionHelloPhase::Ready),
                    waiting: Some(millis(waiting)),
                    key_material_state,
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
                    key_material_state,
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
        self.arm_prolongation(now);
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
            self.hello_peer_deadline = Some(now + waiting);
        }

        // §12.1.3.4: the counter rides in every connection establishment, which is how a
        // node that has been switched off learns its stored key material is stale.
        if let Some(update_counter) = hello
            .key_material_state
            .as_ref()
            .and_then(|state| state.update_counter)
        {
            self.events
                .push_back(Event::PeerKeyMaterialCounter { update_counter });
        }

        match hello.phase {
            Some(ConnectionHelloPhase::Ready) => {
                self.hello_remote_ready = true;
                self.timers.prolongation_reply = None;
                self.try_finish_hello(now);
            }
            Some(ConnectionHelloPhase::Pending) => {
                self.hello_remote_ready = false;
                self.timers.prolongation_reply = None;
                if let Some(waiting) = peer_waiting {
                    self.events.push_back(Event::PeerAwaitingTrust { waiting });
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

        // A prolongation request may accompany either phase. Granting it restarts *our*
        // Wait-For-Ready-Timer, which is the one the asking node is trying to keep alive.
        if self.phase == Phase::Hello && hello.prolongation_request == Some(true) {
            self.accept_prolongation(now);
        }

        // Whether to keep asking is our own state's business, not the peer's phase.
        if self.phase == Phase::Hello {
            self.arm_prolongation(now);
        }
    }

    /// Arms the Send-Prolongation-Request timer, if this node is the one asking.
    ///
    /// SHIP §13.4.4.1.3: the node still in `pending` is the one holding the connection up,
    /// so it is the one that asks — before the *peer's* Wait-For-Ready-Timer expires, which
    /// the peer announced as `waiting`. Hence `waiting` less
    /// `T_hello_prolong_waiting_gap`. A node that is already `ready` never asks; it waits,
    /// and grants what it is asked.
    ///
    /// Below `T_hello_prolong_thr_inc` a peer's announced wait is not worth prolonging,
    /// which also stops one from driving the exchange with near-zero waits.
    fn arm_prolongation(&mut self, now: Duration) {
        if self.hello_local_ready {
            self.timers.send_prolongation = None;
            return;
        }
        let announced = self.hello_peer_waiting.unwrap_or(self.config.hello_init);
        if announced < T_HELLO_PROLONG_THR_INC {
            self.timers.send_prolongation = None;
            return;
        }
        let deadline = self
            .hello_peer_deadline
            .unwrap_or_else(|| now + self.config.hello_init);
        let at = deadline.saturating_sub(T_HELLO_PROLONG_WAITING_GAP);
        // Already inside the gap — which happens when the peer's grant crossed our own
        // re-announcement. Ask on the next turn of the crank rather than not at all: a
        // request that is late still has `T_hello_prolong_waiting_gap` to arrive in, and
        // not asking is a certain abort.
        self.timers.send_prolongation = if at.saturating_sub(now) < T_HELLO_PROLONG_MIN {
            Some(now)
        } else {
            Some(at)
        };
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
        self.send_pin_state();
    }

    /// Sends this node's PIN requirement (§13.4.4.3.5.1).
    ///
    /// `inputPermission` is present exactly when a PIN can be sent, and reads `busy`
    /// while a brute-force penalty is running — which is how the peer is told to stop
    /// trying rather than being left to guess from the silence.
    fn send_pin_state(&mut self) {
        let input_permission = match self.local_pin_state {
            PinState::Required | PinState::Optional => Some(if self.pin_penalty_until.is_some() {
                PinInputPermission::Busy
            } else {
                PinInputPermission::Ok
            }),
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
        // §13.4.4.3.5: while a penalty runs, an input is answered with the requirement —
        // reading `busy` — and not verified. Verifying it would let an attacker keep
        // guessing through the penalty.
        if self.pin_penalty_until.is_some() {
            self.send_pin_state();
            return;
        }

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
            self.pin_attempts = self.pin_attempts.saturating_add(1);
            self.outbox
                .push_back(ShipMessage::Control(ControlMessage::ConnectionPinError(
                    ConnectionPinError {
                        error: Some(super::ConnectionPinErrorError(1)),
                    },
                )));
            // §13.4.4.3.4: three invalid PINs cost fifteen seconds, six cost ninety.
            if let Some(penalty) = pin_penalty(self.pin_attempts) {
                self.pin_penalty_until = Some(now + penalty);
                self.timers.pin_penalty = Some(now + penalty);
                self.send_pin_state();
            }
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
            ControlMessage::KeyMaterialState(state) => {
                // §12.1.3.3: acknowledge within twenty seconds. The response says the
                // message arrived and nothing about whether the keys were taken up —
                // that decision belongs to the trust store, and the peer is not told.
                self.outbox.push_back(ShipMessage::Control(
                    ControlMessage::KeyMaterialStateResponse(KeyMaterialStateResponse {
                        accept: Some(true),
                    }),
                ));
                self.events.push_back(Event::PeerKeyMaterial(state));
            }
            ControlMessage::KeyMaterialStateResponse(_) => {
                self.key_material_sent_at = None;
                self.key_material_resent = false;
                self.timers.key_material = None;
            }
            ControlMessage::KeyMaterialStateRequest(request) => {
                // §12.1.3.4: answer only when what we hold differs from what the peer
                // says it has, which is also what settles the race with an unsolicited
                // announcement crossing the request.
                let known = request.known_update_counter.unwrap_or(0);
                let ours = self
                    .config
                    .key_material
                    .as_ref()
                    .map(super::OwnKeys::update_counter);
                if ours.is_some_and(|ours| ours != known) {
                    self.send_key_material(now);
                }
                self.events.push_back(Event::PeerAskedForKeyMaterial {
                    known_update_counter: known,
                });
            }
            // The SHIP commissioning family is out of scope: the Installation
            // Requirements Annex A.6 does not require it.
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
