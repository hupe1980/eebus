//! One node, several peers: the connection table and the event loop over it.
//!
//! There is *one* [`Engine`], because there is one device with one set of features, one
//! subscription list and one set of bindings; and there are several connections, one per
//! peer. The [`Hub`] joins the two. It routes each datagram the engine produces to the
//! peer it addresses, runs the opening discovery so an application hears about a peer only
//! once it knows what that peer is, resolves double connections (SHIP §12.2.3), keeps idle
//! connections alive with the pings §10.4 asks for, dials remembered peers back, and drives
//! the clock — the engine's deadlines, the SHIP timers, and whatever the application asked
//! to be woken for.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use core::time::Duration;
//! use eebus::runtime::{Hub, HubEvent, Node, TrustStore};
//!
//! # let node: Node = unimplemented!();
//! # let engine: eebus::spine::Engine = unimplemented!();
//! let mut hub = Hub::new(node, engine);
//! hub.connect("192.0.2.10:4712").await?;
//!
//! loop {
//!     match hub.next().await? {
//!         HubEvent::PeerDiscovered { device, .. } => println!("found {}", device.as_str()),
//!         HubEvent::Spine(event) => { /* hand it to a use case */ }
//!         HubEvent::Disconnected { ski, .. } => println!("lost {ski}"),
//!         HubEvent::Tick => { /* a timer the application asked for */ }
//!         HubEvent::PeerKeysUpdated { .. } => { /* persist the trust store */ }
//!         HubEvent::Connected { .. } => {}
//!     }
//! }
//! # }
//! ```

use alloc::vec::Vec;
use core::time::Duration;
use std::time::Instant;

use std::net::SocketAddr;

use tokio::net::{TcpStream, ToSocketAddrs};

use crate::model::{AddressDevice, Datagram, MsgCounter};
use crate::ship::{CURVE_SECP256R1, ConnectionCloseReason, PeerKeys, Resolution, Ski};
use crate::spine::{Engine, SpineEvent};

use super::connection::{ConnectionError, ShipConnection};
use super::node::Node;
use super::reconnect::reconnect_delay_for;

/// How long a connection may be idle before a keep-alive ping goes out.
///
/// SHIP §10.4 sets the floor at fifty seconds; going lower wastes radio time on a
/// battery-powered peer, going higher risks a NAT dropping the flow.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(50);

/// How long a ping may go unanswered before the connection counts as dead (§10.4).
pub const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a peer is given to confirm a `connectionClose` (§13.4.8).
pub const CLOSE_MAX_TIME: Duration = Duration::from_secs(2);

/// How many connections one hub holds at once, unless told otherwise.
///
/// SHIP puts no cap on this, and the omission is the problem: nothing in the protocol
/// stops a malfunctioning — or hostile — device on the LAN from dialling in a thousand
/// times and taking the memory of every node that answers. A household has a handful of
/// EEBUS devices, and a node that has run out of room is a node that keeps serving the
/// peers it already had.
///
/// Raise it with [`Hub::set_max_connections`] on a gateway that really does serve more.
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;

/// How many connections to the *same* peer a hub tolerates.
///
/// Two is not a mistake: SHIP §12.2.3 exists precisely because two nodes that discover
/// each other at the same moment both dial, and the second connection is legitimate
/// until arbitration settles it. A third is not something the protocol produces.
pub const MAX_CONNECTIONS_PER_PEER: usize = 2;

/// How long a hub with nothing to wait for sleeps before reporting a tick.
///
/// A hub with no connections and no timers has nothing that can happen to it. Returning
/// immediately would spin a caller's loop; blocking forever would leave a caller that
/// wants to dial or reconnect with no chance to. A second is short enough to react and
/// long enough to cost nothing.
const IDLE_TICK: Duration = Duration::from_secs(1);

/// How long a dial may take before it is given up on.
///
/// The hub dials from inside its own event loop, so a peer that accepts the TCP
/// connection and then says nothing would otherwise stall every other connection for as
/// long as the operating system's own timeout — minutes, on some systems.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a connection ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disconnect {
    /// The peer closed it, or the socket failed.
    Remote,
    /// A keep-alive ping went unanswered.
    Unresponsive,
    /// It lost the double-connection arbitration of SHIP §12.2.3.
    Duplicate,
    /// The peer named a SPINE device address that is not its own to use: it restated a
    /// different one than this connection is bound to, or claimed one another connection
    /// holds. Routing is by that address, so either would misdeliver another peer's
    /// datagrams. Two devices sharing a vendor and serial produce it without any malice.
    AddressConflict,
    /// This node closed it.
    Local,
    /// A [`Hub::next`] future was dropped while it was writing to this connection.
    ///
    /// Cancelling `next` mid-write leaves a partial WebSocket frame on the wire, which
    /// puts the *peer's* parser out of step with the stream — every message after it is
    /// misread, and nothing on this side can tell. The hub notices on its next call and
    /// closes the connection rather than carrying on: a reconnection costs a second, and
    /// a corrupted stream costs the session.
    InterruptedWrite,
}

/// Something that happened on one of the hub's connections.
// `SpineEvent` carries a payload; boxing it would make every match arm indirect for the
// sake of a stack frame nobody is short of.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum HubEvent {
    /// A peer completed the SHIP handshake and is now connected.
    Connected {
        /// Its SKI.
        ski: Ski,
        /// The SHIP version the handshake settled on.
        ///
        /// Worth a line in a start-up log: the difference between 1.0 and 1.1 decides
        /// whether `accessMethods.id` is there to dial back with.
        version: Option<crate::ship::ShipVersion>,
    },
    /// A peer answered the opening discovery, so its device address and its use cases
    /// are now known.
    ///
    /// This is the event an application waits for: everything addressed in SPINE needs
    /// the device part, and until the peer has said what it is there is nothing to
    /// address.
    PeerDiscovered {
        /// Its SKI.
        ski: Ski,
        /// Its SPINE device address.
        device: AddressDevice,
    },
    /// A peer announced a certificate update, and the trust store has followed it.
    ///
    /// SHIP §12.1.3: the SKIs a peer will use from now on arrive over the connection its
    /// current certificate secures, so the trust survives a renewal that would otherwise
    /// send an installer round the building. Both lists are already applied; the event is
    /// for an application that persists its trust store — which it should, or the next
    /// restart forgets the renewal.
    PeerKeysUpdated {
        /// The peer whose key material changed.
        ski: Ski,
        /// SKIs now trusted for it.
        trusted: Vec<Ski>,
        /// SKIs no longer trusted for it.
        untrusted: Vec<Ski>,
    },
    /// A connection ended.
    Disconnected {
        /// The peer that was on it.
        ski: Ski,
        /// Why.
        reason: Disconnect,
    },
    /// The SPINE engine reported something.
    Spine(SpineEvent),
    /// A timer expired: either the engine's, or one the application asked for with
    /// [`Hub::wake_at`].
    Tick,
}

/// One connection, and what the hub has learned over it.
#[derive(Debug)]
struct Link {
    ski: Ski,
    connection: ShipConnection,
    /// The peer's SPINE device address, once its discovery has answered.
    device: Option<AddressDevice>,
    /// When the connection was established, for the double-connection rule.
    opened: Duration,
    /// When the last frame was seen, for the keep-alive.
    last_seen: Duration,
    /// When the outstanding keep-alive ping went out.
    pinged_at: Option<Duration>,
    /// The discovery requests still outstanding, which have no device address to route
    /// by and so are routed by counter.
    bootstrap: Vec<MsgCounter>,
    /// Whether the peer has been reported as discovered.
    announced: bool,
    /// Whether this connection has already asked the peer for its key material.
    asked_for_keys: bool,
}

/// A peer holding more than one connection, mid-arbitration (SHIP §12.2.3).
#[derive(Clone, Copy, Debug)]
struct Duplicate {
    ski: Ski,
    /// When more than one connection to this peer was first seen — what the
    /// three-second grace period is measured from.
    since: Duration,
    /// Whether this node has already put a ping down every connection.
    ///
    /// Without this the smaller-SKI side livelocks: it pings, both connections answer,
    /// and it has nothing left to do but ping again. §12.2.3 says it closes the older
    /// one itself, and this is what remembers that the round has been run.
    probed: bool,
}

/// A peer this hub is expected to stay connected to.
#[derive(Clone, Debug)]
struct Known {
    ski: Ski,
    address: SocketAddr,
    /// The mDNS instance it was discovered under, where it was discovered at all.
    ///
    /// A withdrawal names the instance and nothing else, so this is what ties one back to
    /// the SKI it belongs to.
    instance: Option<alloc::string::String>,
    /// How many attempts have failed since the last success.
    attempts: u32,
    /// When to dial next, if it is not connected.
    next_attempt: Duration,
}

/// A node's connections and the SPINE engine behind them.
#[derive(Debug)]
pub struct Hub {
    node: Node,
    engine: Engine,
    links: Vec<Link>,
    clock: Instant,
    /// When the application asked to be woken.
    wake_at: Option<Duration>,
    /// Peers seen holding more than one connection, and how far arbitration has got.
    duplicates: Vec<Duplicate>,
    /// The most connections this hub will hold at once.
    max_connections: usize,
    /// Peers to dial back when the connection to them drops.
    known: Vec<Known>,
    /// What is known about each peer's key material (SHIP §12.1.3).
    peer_keys: Vec<(Ski, PeerKeys)>,
    pending: Vec<HubEvent>,
    /// The connection a write is in flight on, if one is.
    ///
    /// Set immediately before an `await` that puts bytes on a socket and cleared after it,
    /// so that a `next` future dropped in between leaves a mark. Finding it set at the top
    /// of the next call is proof the previous one was cancelled mid-write — the one hazard
    /// `Hub::next` cannot defend against, made detectable instead of silent.
    writing: Option<usize>,
}

impl Hub {
    /// A hub serving `engine` over `node`'s connections.
    pub fn new(node: Node, engine: Engine) -> Self {
        Self {
            node,
            engine,
            links: Vec::new(),
            clock: Instant::now(),
            wake_at: None,
            duplicates: Vec::new(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            known: Vec::new(),
            peer_keys: Vec::new(),
            pending: Vec::new(),
            writing: None,
        }
    }

    /// The node's identity and trust store.
    pub fn node(&self) -> &Node {
        &self.node
    }

    /// This node's SHIP ID.
    pub fn ship_id(&self) -> &str {
        self.node.ship_id()
    }

    /// This node's SKI, which is what a peer is asked to trust.
    pub fn ski(&self) -> Ski {
        self.node.ski()
    }

    /// The SPINE engine, for publishing data and issuing reads and writes.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The SPINE engine, mutably.
    ///
    /// Anything queued on it goes out on the next [`next`](Self::next).
    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// The hub's monotonic clock, in the form every sans-IO core in this crate takes.
    pub fn now(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Asks to be woken at `at`, which [`next`](Self::next) answers with
    /// [`HubEvent::Tick`].
    ///
    /// This is how a use case's timers reach the event loop: an actor says when it next
    /// needs attention, and the hub folds that into its own deadlines. `at` is an instant
    /// on the same clock as [`now`](Self::now), not a delay.
    ///
    /// The earliest instant asked for wins, and it is cleared once it fires. An instant
    /// already in the past is not an error and does not starve the connections: the hub
    /// runs the timers and comes straight back, rather than reading with a zero-length
    /// timeout.
    pub fn wake_at(&mut self, at: Duration) {
        self.wake_at = Some(match self.wake_at {
            Some(existing) => existing.min(at),
            None => at,
        });
    }

    /// The peers currently connected.
    pub fn peers(&self) -> impl Iterator<Item = Ski> + '_ {
        self.links.iter().map(|l| l.ski)
    }

    /// The SPINE device address of a connected peer, once discovery has answered.
    pub fn device_of(&self, ski: &Ski) -> Option<&AddressDevice> {
        self.links.iter().find(|l| &l.ski == ski)?.device.as_ref()
    }

    /// The SHIP version negotiated with a connected peer.
    ///
    /// [`None`] when nothing is connected to that SKI, or when the handshake has not
    /// reached the protocol phase.
    pub fn ship_version(&self, ski: &Ski) -> Option<crate::ship::ShipVersion> {
        self.links
            .iter()
            .find(|l| &l.ski == ski)?
            .connection
            .ship_version()
    }

    /// The SKI a device address belongs to.
    pub fn ski_of(&self, device: &AddressDevice) -> Option<Ski> {
        self.links
            .iter()
            .find(|l| l.device.as_ref() == Some(device))
            .map(|l| l.ski)
    }

    /// Dials a peer and adds the connection.
    ///
    /// Subject to the same cap as [`accept`](Self::accept): a hub with no room closes the
    /// connection it just opened and reports
    /// [`ConnectionError::TooManyConnections`].
    pub async fn connect(&mut self, address: impl ToSocketAddrs) -> Result<Ski, ConnectionError> {
        let connection = self.node.connect(address).await?;
        match self.adopt(connection) {
            Ok(ski) => Ok(ski),
            Err(refused) => {
                let _ = refused
                    .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                    .await;
                Err(ConnectionError::TooManyConnections)
            }
        }
    }

    /// What this node has stored about a peer's key material.
    ///
    /// Persist it alongside the trust store: the counter is what tells a returning node
    /// whether it has missed a certificate update, and a node that forgets it will ask
    /// for the whole state again on every connection.
    pub fn peer_keys(&self, ski: &Ski) -> Option<&PeerKeys> {
        self.peer_keys
            .iter()
            .find(|(s, _)| s == ski)
            .map(|(_, k)| k)
    }

    /// Restores what was stored about a peer's key material.
    pub fn restore_peer_keys(&mut self, ski: Ski, keys: PeerKeys) {
        match self.peer_keys.iter_mut().find(|(s, _)| *s == ski) {
            Some((_, existing)) => *existing = keys,
            None => self.peer_keys.push((ski, keys)),
        }
    }

    /// Announces this node's key material to every connected peer (SHIP §12.1.3.2).
    ///
    /// Call it when a certificate renewal begins. Peers not currently connected are
    /// reached the next time they are, because the `updateCounter` in the `hello` tells
    /// them to ask.
    pub async fn announce_key_material(&mut self) -> Result<(), ConnectionError> {
        for index in 0..self.links.len() {
            let _ = self.links[index].connection.send_key_material().await;
        }
        Ok(())
    }

    /// Remembers a peer, so the hub dials it and dials it again when the link drops.
    ///
    /// This is what turns a discovered address into a connection that stays up: mDNS
    /// finds a `_ship._tcp` service and hands its SKI and address here, or an installer's
    /// configuration does. The hub dials it, and on a failure backs off from a second to
    /// two minutes, spread out by the peer's SKI so a building coming back from a power
    /// cut does not have every device dialling in the same instant.
    ///
    /// Only a trusted peer is worth remembering — an untrusted one will be held in the
    /// SHIP hello phase and time out — but the hub does not enforce that, because a user
    /// may be approving it while the connection is being made.
    pub fn remember(&mut self, ski: Ski, address: SocketAddr) {
        let now = self.now();
        match self.known.iter_mut().find(|k| k.ski == ski) {
            Some(known) => {
                known.address = address;
                known.attempts = 0;
                known.next_attempt = now;
            }
            None => self.known.push(Known {
                ski,
                address,
                instance: None,
                attempts: 0,
                next_attempt: now,
            }),
        }
    }

    /// Remembers a peer mDNS found, if it is one this node trusts.
    ///
    /// The whole of the discovery-to-connection path in one line: the browse loop hands
    /// each `_ship._tcp` service here, the ones already approved are dialled and kept
    /// dialled, and the rest are ignored until a person approves them. Returns whether
    /// the peer was taken up.
    ///
    /// Everything in a TXT record is a claim rather than a fact — the SKI included. It
    /// becomes a fact when TLS proves the peer holds the matching key, which is why the
    /// hub checks what it connected to rather than what it was told.
    #[cfg(feature = "mdns")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
    pub fn remember_discovered(&mut self, found: &crate::mdns::Discovered) -> bool {
        if !self.node.trust_store().is_trusted(&found.ski) {
            return false;
        }
        let Some(address) = found.socket_address() else {
            return false;
        };
        self.remember(found.ski, address);
        if let Some(known) = self.known.iter_mut().find(|k| k.ski == found.ski) {
            known.instance = Some(found.instance.clone());
        }
        true
    }

    /// Forgets the peer discovered under an mDNS instance name.
    ///
    /// What [`crate::mdns::BrowseEvent::Lost`] carries is the instance, so this is how a
    /// withdrawal reaches the redial schedule. Returns the SKI that was forgotten, or
    /// [`None`] if no remembered peer was discovered under that name.
    pub fn forget_discovered(&mut self, instance: &str) -> Option<Ski> {
        let index = self
            .known
            .iter()
            .position(|k| k.instance.as_deref() == Some(instance))?;
        Some(self.known.remove(index).ski)
    }

    /// Stops dialling a peer. Any connection to it stays up.
    pub fn forget_peer(&mut self, ski: &Ski) {
        self.known.retain(|k| &k.ski != ski);
    }

    /// The peers the hub will dial, connected or not.
    pub fn remembered(&self) -> impl Iterator<Item = (Ski, SocketAddr)> + '_ {
        self.known.iter().map(|k| (k.ski, k.address))
    }

    /// The most connections this hub will hold at once.
    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    /// Changes how many connections this hub will hold at once.
    ///
    /// [`DEFAULT_MAX_CONNECTIONS`] suits a household. A gateway serving a whole building
    /// raises it; a controller with a few kilobytes to spare lowers it. Connections
    /// already held are never dropped to meet a lowered limit — the cap decides what is
    /// *accepted*, and closing a working session to satisfy a setting would be worse
    /// than being over it.
    pub fn set_max_connections(&mut self, limit: usize) {
        self.max_connections = limit;
    }

    /// Runs the server side of the stack on an accepted socket and adds the connection.
    ///
    /// A connection the hub has no room for is closed with a `connectionClose` rather
    /// than dropped on the floor, and reported as
    /// [`ConnectionError::TooManyConnections`].
    pub async fn accept(&mut self, stream: TcpStream) -> Result<Ski, ConnectionError> {
        let connection = self.node.accept(stream).await?;
        match self.adopt(connection) {
            Ok(ski) => Ok(ski),
            Err(refused) => {
                let _ = refused
                    .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                    .await;
                Err(ConnectionError::TooManyConnections)
            }
        }
    }

    /// Adds a connection the caller established itself.
    ///
    /// # Errors
    ///
    /// Hands the connection back when the hub is already holding
    /// [`max_connections`](Self::max_connections), or a second connection to this peer
    /// already exists and is being arbitrated. The caller owns the refused connection and
    /// decides how to end it; [`accept`](Self::accept) closes it politely. It comes back
    /// boxed because a `ShipConnection` is two kilobytes and every ordinary call would
    /// otherwise carry that on the stack.
    pub fn adopt(&mut self, connection: ShipConnection) -> Result<Ski, Box<ShipConnection>> {
        let ski = connection.peer();
        let now = self.now();

        let to_this_peer = self.links.iter().filter(|l| l.ski == ski).count();
        if self.links.len() >= self.max_connections || to_this_peer >= MAX_CONNECTIONS_PER_PEER {
            return Err(Box::new(connection));
        }

        if to_this_peer > 0 && !self.duplicates.iter().any(|d| d.ski == ski) {
            self.duplicates.push(Duplicate {
                ski,
                since: now,
                probed: false,
            });
        }

        self.links.push(Link {
            ski,
            connection,
            device: None,
            opened: now,
            last_seen: now,
            pinged_at: None,
            bootstrap: Vec::new(),
            announced: false,
            asked_for_keys: false,
        });
        let version = self
            .links
            .last()
            .and_then(|link| link.connection.ship_version());
        self.pending.push(HubEvent::Connected { ski, version });
        self.start_discovery(self.links.len() - 1, now);
        Ok(ski)
    }

    /// Closes one peer's connections, telling it why.
    pub async fn disconnect(&mut self, ski: &Ski, reason: ConnectionCloseReason) {
        while let Some(index) = self.links.iter().position(|l| &l.ski == ski) {
            let link = self.links.remove(index);
            let _ = link.connection.close(reason, CLOSE_MAX_TIME).await;
        }
        self.forget(ski);
        self.pending.push(HubEvent::Disconnected {
            ski: *ski,
            reason: Disconnect::Local,
        });
    }

    /// Sends everything the engine has queued, without waiting for anything to arrive.
    ///
    /// [`next`](Self::next) does this on the way in, so a loop that keeps calling it never
    /// needs this. It is for the two places that are not a loop: before
    /// [`shutdown`](Self::shutdown), so that an acknowledgement a use case has just
    /// decided actually reaches the peer, and in a test that stops as soon as it has seen
    /// what it was waiting for.
    ///
    /// Deciding is not answering. A Controllable System that accepts a limit and then
    /// stops driving its hub has left the `result` message in the queue, and the Energy
    /// Guard is still waiting for it — which, under §14a, is the difference between a
    /// limitation that was honoured and one that cannot be shown to have been.
    pub async fn flush(&mut self) -> Result<(), ConnectionError> {
        self.dispatch().await
    }

    /// Closes every connection and shuts the hub down.
    ///
    /// Anything the engine still has queued is sent first: closing on top of an
    /// unacknowledged write would leave the peer waiting for an answer that was already
    /// decided.
    pub async fn shutdown(&mut self, reason: ConnectionCloseReason) {
        let _ = self.dispatch().await;
        for link in core::mem::take(&mut self.links) {
            let _ = link.connection.close(reason, CLOSE_MAX_TIME).await;
        }
        self.duplicates.clear();
    }

    /// Runs the hub's loop, handing every event to `handler`, until `handler` says stop.
    ///
    /// **This is the shape to reach for**, because [`next`](Self::next) is not cancel-safe
    /// and this loop cannot cancel it. `handler` returns [`ControlFlow::Break`](core::ops::ControlFlow::Break) to end the
    /// loop, and gets `&mut Hub` so that it can do anything the loop could — accept a
    /// socket, write a limit, ask for a tick.
    ///
    /// Work arriving from elsewhere goes where it goes in a hand-written loop: ask for a
    /// tick with [`wake_at`](Self::wake_at) and drain it when [`HubEvent::Tick`] arrives.
    ///
    /// ```no_run
    /// # async fn example(
    /// #     mut hub: eebus::runtime::Hub,
    /// #     mut inbox: tokio::sync::mpsc::Receiver<tokio::net::TcpStream>,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// use core::ops::ControlFlow;
    /// use core::time::Duration;
    /// use eebus::runtime::HubEvent;
    ///
    /// hub.wake_at(hub.now() + Duration::from_secs(1));
    /// hub.run(|hub, event| {
    ///     if let HubEvent::Tick = event {
    ///         while let Ok(stream) = inbox.try_recv() {
    ///             let _ = stream; // `hub.accept(stream)` — `run`'s handler is synchronous
    ///         }
    ///         hub.wake_at(hub.now() + Duration::from_secs(1));
    ///     }
    ///     ControlFlow::Continue(())
    /// })
    /// .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// The handler is synchronous on purpose: an `async` one would be a second place a
    /// caller could hold the hub across an await, which is the thing this method exists to
    /// prevent. Anything that has to await belongs after `run` returns, or behind a
    /// channel the handler writes to.
    pub async fn run<F>(&mut self, mut handler: F) -> Result<(), ConnectionError>
    where
        F: FnMut(&mut Self, HubEvent) -> core::ops::ControlFlow<()>,
    {
        loop {
            let event = self.next().await?;
            if handler(self, event).is_break() {
                return Ok(());
            }
        }
    }

    /// Waits for the next thing to happen.
    ///
    /// Everything the engine has queued goes out first, so an application that has just
    /// written a limit does not have to remember to flush.
    ///
    /// # This future is not cancel-safe
    ///
    /// **Do not put it in a [`tokio::select!`] or a `tokio::time::timeout`.**
    /// [`run`](Self::run) is the loop that cannot get this wrong.
    ///
    /// The hazard is the *sending* half, not the reading half. Reading is cancel-safe —
    /// `WebSocketStream`'s `Stream` implementation buffers a partial frame and resumes
    /// where it left off — but this future flushes the engine's queue before it reads,
    /// and `Sink::send` carries no such guarantee. Dropped part-way through, it leaves
    /// **half a frame on the wire**: the peer's parser is then out of step with the
    /// stream, and every datagram after it is misread.
    ///
    /// **The hub notices, which is the one thing it can do about it.** A write in flight
    /// is marked before the `await` and unmarked after, so a future dropped in between
    /// leaves the mark set; the next call finds it, closes that connection with
    /// [`Disconnect::InterruptedWrite`] and redials. A reconnection costs a second. The
    /// alternative — carrying on over a stream the peer can no longer parse — reappears
    /// minutes later as a subscription that was never granted, a heartbeat that never
    /// arrived, and a limit refused for want of one, with nothing in any log to connect
    /// them to a `select!` written weeks earlier.
    ///
    /// Everything a caller would reach for `select!` to do has somewhere else to go:
    ///
    /// * **Timers** — [`wake_at`](Self::wake_at). The hub folds the instant into its own
    ///   deadlines and answers with [`HubEvent::Tick`]; the read is never interrupted.
    /// * **Work arriving from elsewhere** — a listener accepting connections, an mDNS
    ///   browse, a command channel — is drained *between* calls, on the tick. Ask for a
    ///   tick a second from now, and each one is a chance to look:
    ///
    /// ```no_run
    /// # async fn example(
    /// #     mut hub: eebus::runtime::Hub,
    /// #     mut inbox: tokio::sync::mpsc::Receiver<tokio::net::TcpStream>,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// use core::time::Duration;
    ///
    /// loop {
    ///     // Between calls: whatever else has turned up.
    ///     while let Ok(stream) = inbox.try_recv() {
    ///         hub.accept(stream).await?;
    ///     }
    ///     hub.wake_at(hub.now() + Duration::from_secs(1));
    ///
    ///     // And then the one await that owns the sockets, uninterrupted.
    ///     let event = hub.next().await?;
    ///     # let _ = event;
    /// }
    /// # }
    /// ```
    pub async fn next(&mut self) -> Result<HubEvent, ConnectionError> {
        // A write was in flight when the last `next` future was dropped, so a partial
        // frame is on that socket and the peer's parser is out of step with it. Nothing
        // can repair that from this side; the connection goes, and `Hub` redials it.
        if let Some(index) = self.writing.take()
            && index < self.links.len()
        {
            self.drop_link(index, Disconnect::InterruptedWrite);
        }

        loop {
            if !self.pending.is_empty() {
                return Ok(self.pending.remove(0));
            }

            self.dispatch().await?;
            if let Some(event) = self.engine.poll_event() {
                return Ok(HubEvent::Spine(event));
            }
            if self.follow_key_material().await {
                continue;
            }

            if self.arbitrate().await? {
                continue;
            }

            let now = self.now();
            let deadline = self.next_deadline(now);
            if self.links.is_empty() {
                // Nothing to read from: only a timer can produce an event, and if there
                // is no timer either, the caller is told so at a pace it can act on.
                let wait = deadline.map_or(IDLE_TICK, |at| at.saturating_sub(now));
                tokio::time::sleep(wait).await;
                self.fire_timers(self.now());
                self.redial().await;
                if deadline.is_none() && self.links.is_empty() {
                    return Ok(HubEvent::Tick);
                }
                continue;
            }

            // A deadline that has already passed is dealt with *before* the sockets are
            // touched, and not by reading with a zero-length timeout. A zero timeout
            // cancels the read immediately, and a caller that keeps handing back a stale
            // instant — an actor whose timers have not caught up, most easily — would
            // spin here forever and never read a byte. The connection would look alive
            // and carry nothing.
            if deadline.is_some_and(|at| at <= now) {
                self.fire_timers(now);
                self.keepalive().await?;
                self.redial().await;
                continue;
            }

            let read = read_any(&mut self.links);
            let outcome = match deadline {
                Some(at) => {
                    let wait = at.saturating_sub(now);
                    tokio::time::timeout(wait, read).await.ok()
                }
                None => Some(read.await),
            };

            match outcome {
                Some((index, Ok(message))) => {
                    let now = self.now();
                    self.links[index].last_seen = now;
                    self.links[index].pinged_at = None;
                    let link = &mut self.links[index];
                    if link.connection.handle_message(&message).is_err() {
                        self.drop_link(index, Disconnect::Remote);
                        continue;
                    }
                    self.writing = Some(index);
                    let _ = self.links[index].connection.flush().await;
                    self.writing = None;

                    match message {
                        crate::ship::ShipMessage::Data(data) => {
                            // A payload this node cannot parse is dropped, not fatal: one
                            // peer sending nonsense must not take down the connections to
                            // every other peer. Its request expires on its own side.
                            if let Ok(Some(datagram)) = crate::ship::spine_datagram(&data)
                                && let Some(reason) = self.receive(index, &datagram, now)
                            {
                                self.drop_link(index, reason);
                                continue;
                            }
                        }
                        crate::ship::ShipMessage::End(_) => {
                            self.drop_link(index, Disconnect::Remote);
                        }
                        _ => {}
                    }
                }
                Some((index, Err(error))) => {
                    let link = self.links.remove(index);
                    let Link {
                        ski,
                        connection,
                        device,
                        ..
                    } = link;
                    if matches!(error, ConnectionError::NotBinary) {
                        // §10.3: tell the peer which rule it broke.
                        connection
                            .reject(1003, "SHIP carries binary frames only")
                            .await;
                    }
                    self.forget_peer_state(ski, device, Disconnect::Remote);
                }
                None => {
                    self.fire_timers(self.now());
                    self.keepalive().await?;
                    self.redial().await;
                }
            }
        }
    }

    // ---- internals -------------------------------------------------------------

    /// Sends everything the engine has queued, to whichever peer each datagram names.
    async fn dispatch(&mut self) -> Result<(), ConnectionError> {
        while let Some(datagram) = self.engine.poll_transmit() {
            let Some(index) = self.route(&datagram) else {
                // Nowhere to send it. Dropping is the only option, and it is not silent:
                // the engine's own response deadline will report the request as timed
                // out, which is what the application acts on.
                continue;
            };
            self.writing = Some(index);
            let sent = self.links[index].connection.send(&datagram).await;
            self.writing = None;
            if sent.is_err() {
                self.drop_link(index, Disconnect::Remote);
            }
        }
        Ok(())
    }

    /// Which connection a datagram belongs on.
    ///
    /// By the destination's device address, which is what the SPINE implementation guide
    /// §2.7 makes mandatory in every message — with one exception, the opening detailed
    /// discovery read, which is routed by the counter the hub recorded when it sent it.
    fn route(&self, datagram: &Datagram) -> Option<usize> {
        let header = datagram.header.as_ref()?;
        if let Some(device) = header
            .address_destination
            .as_ref()
            .and_then(|d| d.device.as_ref())
        {
            return self
                .links
                .iter()
                .position(|l| l.device.as_ref() == Some(device));
        }
        let counter = header.msg_counter?;
        self.links
            .iter()
            .position(|l| l.bootstrap.contains(&counter))
    }

    /// Reads the peer's identity and use cases as soon as the connection opens.
    ///
    /// Both are addressed without a device part, which §2.7 permits for exactly this
    /// message: the peer's address is what the answer contains.
    fn start_discovery(&mut self, index: usize, now: Duration) {
        self.links[index].bootstrap = self.engine.start_discovery(now).to_vec();
    }

    /// Takes a datagram off one connection into the engine, learning who is on it.
    ///
    /// The peer's SPINE device address is bound to the connection once and not afterwards:
    /// routing is *by* that address ([`route`](Self::route)), and SHIP authenticates a SKI,
    /// not anything a datagram says about who sent it.
    ///
    /// Returns the reason to close the connection when the peer contradicts that binding.
    fn receive(&mut self, index: usize, datagram: &Datagram, now: Duration) -> Option<Disconnect> {
        let peer_device = datagram
            .header
            .as_ref()
            .and_then(|h| h.address_source.as_ref())
            .and_then(|a| a.device.clone());

        // The same rule the engine applies, one layer earlier: this is the value the
        // connection is *bound* to, so an address that cannot be keyed on must not reach
        // the table at all.
        let peer_device =
            peer_device.filter(|device| crate::spine::is_usable_device_address(device.as_str()));

        if let Some(device) = peer_device {
            match self.links[index].device.as_ref() {
                Some(bound) if bound != &device => return Some(Disconnect::AddressConflict),
                Some(_) => {}
                None => {
                    let taken = self.links.iter().enumerate().any(|(other, link)| {
                        other != index && link.device.as_ref() == Some(&device)
                    });
                    if taken {
                        return Some(Disconnect::AddressConflict);
                    }
                    self.links[index].device = Some(device);
                }
            }
        }

        if let Some(reference) = datagram
            .header
            .as_ref()
            .and_then(|h| h.msg_counter_reference)
        {
            self.links[index].bootstrap.retain(|c| *c != reference);
        }

        self.engine.handle_datagram(datagram, now);

        // The peer is "discovered" once both opening reads have been answered, not on
        // the first datagram to carry its address: an application told about it earlier
        // would look for use cases the second answer has not delivered yet.
        let link = &mut self.links[index];
        if !link.announced
            && link.bootstrap.is_empty()
            && let Some(device) = link.device.clone()
        {
            link.announced = true;
            let ski = link.ski;
            self.pending.push(HubEvent::PeerDiscovered { ski, device });
        }
        None
    }

    /// SHIP §12.1.3: takes up a peer's certificate updates, and asks when behind.
    ///
    /// Returns `true` when something changed, so the caller re-runs the loop.
    async fn follow_key_material(&mut self) -> bool {
        let mut moved = false;

        for index in 0..self.links.len() {
            let ski = self.links[index].ski;
            let announced = self.links[index].connection.take_peer_key_material();
            for state in announced {
                // The curve is the one this connection negotiated. This backend offers
                // secp256r1 alone (SHIP §9.3 makes it the SHALL), so that is what a
                // peer's certificates are taken up for; §12.1.3.3 asks a node not to
                // trust key material for curves it has not verified.
                let keys = self.keys_for(ski);
                let Some(update) = keys.apply(&state, CURVE_SECP256R1) else {
                    continue;
                };
                if update.trust.is_empty() && update.untrust.is_empty() {
                    continue;
                }
                let store = self.node.trust_store();
                for new in &update.trust {
                    store.trust(*new);
                }
                for gone in &update.untrust {
                    // Never the SKI this connection is running on: the peer is still
                    // using it, and forgetting it mid-connection would refuse the next
                    // reconnection for no reason.
                    if gone != &ski {
                        store.forget(gone);
                    }
                }
                self.pending.push(HubEvent::PeerKeysUpdated {
                    ski,
                    trusted: update.trust,
                    untrusted: update.untrust,
                });
                moved = true;
            }

            // §12.1.3.4: a `hello` counter above what is stored means there is something
            // to catch up on, and the request carries what this node holds.
            if let Some(announced) = self.links[index].connection.peer_key_counter()
                && !self.links[index].asked_for_keys
            {
                let known = self.keys_for(ski).update_counter();
                if announced > known {
                    self.links[index].asked_for_keys = true;
                    let _ = self.links[index]
                        .connection
                        .request_key_material(known)
                        .await;
                    moved = true;
                }
            }
        }
        moved
    }

    /// The key-material record for a peer, created empty on first sight.
    fn keys_for(&mut self, ski: Ski) -> &mut PeerKeys {
        if let Some(index) = self.peer_keys.iter().position(|(s, _)| *s == ski) {
            return &mut self.peer_keys[index].1;
        }
        self.peer_keys.push((ski, PeerKeys::new()));
        &mut self.peer_keys.last_mut().expect("just pushed").1
    }

    /// SHIP §12.2.3: settles a peer holding more than one connection.
    ///
    /// Returns `true` when something changed, so the caller re-runs the loop rather than
    /// going to sleep on a decision it has just taken.
    async fn arbitrate(&mut self) -> Result<bool, ConnectionError> {
        let now = self.now();
        let Some(duplicate) = self.duplicates.first().copied() else {
            return Ok(false);
        };
        let Duplicate { ski, since, probed } = duplicate;

        let opened: Vec<Duration> = self
            .links
            .iter()
            .filter(|l| l.ski == ski)
            .map(|l| l.opened)
            .collect();
        let local = self.node.ski();

        match crate::ship::resolve(&local, &ski, &opened, since, probed, now) {
            Resolution::Settled => {
                self.duplicates.retain(|d| d.ski != ski);
                Ok(false)
            }
            Resolution::KeepNewest => {
                self.close_duplicates(&ski, now).await;
                Ok(true)
            }
            Resolution::Wait { until } => {
                self.wake_at(until);
                Ok(false)
            }
            Resolution::Probe => {
                // The peer had the bigger SKI and did not act. One ping round, and one
                // only — `Resolution` is what remembers that, so the decision below can
                // be reached from a unit test with no socket in it.
                for index in 0..self.links.len() {
                    if self.links[index].ski == ski {
                        let _ = self.links[index].connection.ping().await;
                    }
                }
                if let Some(entry) = self.duplicates.iter_mut().find(|d| d.ski == ski) {
                    entry.probed = true;
                }
                self.wake_at(now + PONG_TIMEOUT);
                Ok(false)
            }
            Resolution::CloseAfterProbe => {
                let unanswered: Vec<usize> = self
                    .links
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.ski == ski && l.connection.awaiting_pong())
                    .map(|(index, _)| index)
                    .collect();
                for index in unanswered.into_iter().rev() {
                    self.drop_link(index, Disconnect::Unresponsive);
                }
                // Whatever answered, keep the most recent of it — the same choice
                // §12.2.3 gives the other side, so the two agree on the survivor.
                self.close_duplicates(&ski, now).await;
                Ok(true)
            }
        }
    }

    /// Keeps the most recent connection to `ski` and closes the rest.
    async fn close_duplicates(&mut self, ski: &Ski, _now: Duration) {
        let keep = self
            .links
            .iter()
            .enumerate()
            .filter(|(_, l)| &l.ski == ski)
            .max_by_key(|(index, l)| (l.opened, *index))
            .map(|(index, _)| index);
        let Some(keep) = keep else {
            self.duplicates.retain(|d| &d.ski != ski);
            return;
        };

        let doomed: Vec<usize> = self
            .links
            .iter()
            .enumerate()
            .filter(|(index, l)| &l.ski == ski && *index != keep)
            .map(|(index, _)| index)
            .collect();
        for index in doomed.into_iter().rev() {
            let link = self.links.remove(index);
            // §12.2.3: a connection already exchanging data is closed by the handshake
            // of §13.4.8, not by pulling the socket out from under the peer.
            let _ = link
                .connection
                .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                .await;
        }
        self.duplicates.retain(|d| &d.ski != ski);
        self.pending
            .retain(|e| !matches!(e, HubEvent::Disconnected { ski: s, .. } if s == ski));
    }

    /// The earliest instant anything wants attention.
    fn next_deadline(&self, now: Duration) -> Option<Duration> {
        let mut next = self.wake_at;
        let mut consider = |at: Duration| {
            next = Some(next.map_or(at, |current: Duration| current.min(at)));
        };

        if let Some(at) = self.engine.poll_timeout() {
            consider(at);
        }
        for known in &self.known {
            if !self.links.iter().any(|l| l.ski == known.ski) {
                consider(known.next_attempt);
            }
        }
        for link in &self.links {
            if let Some(at) = link.connection.poll_timeout() {
                consider(at);
            }
            match link.pinged_at {
                Some(at) => consider(at + PONG_TIMEOUT),
                None => consider(link.last_seen + KEEPALIVE_INTERVAL),
            }
        }
        let _ = now;
        next
    }

    /// Runs every timer that is due, and queues what they produced.
    fn fire_timers(&mut self, now: Duration) {
        self.engine.handle_timeout(now);
        for link in &mut self.links {
            let _ = link.connection.handle_timeout();
        }

        // Connections that stopped answering, and connections that have gone quiet.
        let dead: Vec<usize> = self
            .links
            .iter()
            .enumerate()
            .filter(|(_, l)| l.pinged_at.is_some_and(|at| now >= at + PONG_TIMEOUT))
            .map(|(index, _)| index)
            .collect();
        for index in dead.into_iter().rev() {
            self.drop_link(index, Disconnect::Unresponsive);
        }

        let woken = self.wake_at.is_some_and(|at| now >= at);
        if woken {
            self.wake_at = None;
            self.pending.push(HubEvent::Tick);
        }
    }

    /// Removes a connection and everything the engine held for it.
    fn drop_link(&mut self, index: usize, reason: Disconnect) {
        let link = self.links.remove(index);
        self.forget_link(link, reason);
    }

    /// Forgets a connection that has already been taken out of the table.
    fn forget_link(&mut self, link: Link, reason: Disconnect) {
        self.forget_peer_state(link.ski, link.device, reason);
    }

    /// Drops what the engine held for a connection that has gone.
    fn forget_peer_state(&mut self, ski: Ski, device: Option<AddressDevice>, reason: Disconnect) {
        // The LPC implementation guide §2.17 draws the line here: the session artefacts
        // go, the use case's own state does not.
        if let Some(device) = &device {
            self.engine.remove_peer(device);
        }
        if !self.links.iter().any(|l| l.ski == ski) {
            self.forget(&ski);
        }
        self.pending.push(HubEvent::Disconnected { ski, reason });
    }

    fn forget(&mut self, ski: &Ski) {
        self.duplicates.retain(|d| &d.ski != ski);
    }

    /// Dials a remembered peer that is not connected and whose backoff has expired.
    ///
    /// Counts a failed attempt against a remembered peer and schedules the next one.
    fn defer_redial(&mut self, index: usize, now: Duration) {
        let known = &mut self.known[index];
        known.attempts = known.attempts.saturating_add(1);
        known.next_attempt = now + reconnect_delay_for(&known.ski, known.attempts);
    }

    /// Returns `true` when something changed, so the caller re-runs the loop rather than
    /// going to sleep on a connection it has just made.
    async fn redial(&mut self) -> bool {
        let now = self.now();
        let due = self.known.iter().position(|known| {
            now >= known.next_attempt && !self.links.iter().any(|l| l.ski == known.ski)
        });
        let Some(index) = due else {
            return false;
        };

        let (ski, address) = (self.known[index].ski, self.known[index].address);
        let attempt = tokio::time::timeout(CONNECT_TIMEOUT, self.node.connect(address)).await;
        match attempt.unwrap_or(Err(ConnectionError::Closed)) {
            Ok(connection) if connection.peer() == ski => {
                match self.adopt(connection) {
                    Ok(_) => {
                        self.known[index].attempts = 0;
                        self.known[index].next_attempt = now;
                    }
                    // No room. Backing off is what stops a full hub redialling in a
                    // tight loop, and the peer keeps its place in the schedule.
                    Err(refused) => {
                        let _ = refused
                            .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                            .await;
                        self.defer_redial(index, now);
                    }
                }
                true
            }
            Ok(connection) => {
                // Somebody else answered at that address. Adopting it is still right —
                // the peer proved an identity and the trust store decided about it — but
                // the peer we were looking for is not there.
                self.defer_redial(index, now);
                if let Err(refused) = self.adopt(connection) {
                    let _ = refused
                        .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                        .await;
                }
                true
            }
            Err(_) => {
                self.defer_redial(index, now);
                true
            }
        }
    }

    /// Sends the keep-alive pings that are due (§10.4).
    ///
    /// Called from [`next`](Self::next)'s timer path; separate because it is the only
    /// part of the timer handling that has to await.
    pub async fn keepalive(&mut self) -> Result<(), ConnectionError> {
        let now = self.now();
        for index in 0..self.links.len() {
            let link = &self.links[index];
            if link.pinged_at.is_none() && now.saturating_sub(link.last_seen) >= KEEPALIVE_INTERVAL
            {
                self.writing = Some(index);
                let _ = self.links[index].connection.ping().await;
                self.writing = None;
                self.links[index].pinged_at = Some(now);
            }
        }
        Ok(())
    }
}

/// Reads whichever connection speaks first.
///
/// Every future here is a single cancel-safe socket read, so dropping the losers costs
/// nothing — which is the property [`ShipConnection::next_message`] exists to provide, and
/// which holds for a more specific reason than "it does not write": the WebSocket layer
/// does answer a ping while reading, and never blocks doing it.
async fn read_any(
    links: &mut [Link],
) -> (usize, Result<crate::ship::ShipMessage, ConnectionError>) {
    use futures_util::future::{FutureExt, select_all};

    let futures: Vec<_> = links
        .iter_mut()
        .enumerate()
        .map(|(index, link)| async move { (index, link.connection.next_message().await) }.boxed())
        .collect();
    let (outcome, _, _) = select_all(futures).await;
    outcome
}

impl core::fmt::Display for Disconnect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Disconnect::Remote => "closed by the peer",
            Disconnect::Unresponsive => "unresponsive",
            Disconnect::Duplicate => "duplicate connection",
            Disconnect::AddressConflict => "claimed another peer's device address",
            Disconnect::Local => "closed locally",
            Disconnect::InterruptedWrite => "a write was interrupted by a cancelled `next`",
        })
    }
}
