//! One node, several peers: the connection table and the event loop over it.
//!
//! There is *one* [`Engine`], because there is one device with one set of features, one
//! subscription list and one set of bindings; and there are several connections, one per
//! peer. The [`Hub`] joins the two. It listens and dials, runs every TLS and SHIP handshake
//! off the loop so that a slow or unapproved peer holds nothing else up, asks the
//! application when a peer needs a trust decision, routes each datagram the engine produces
//! to the peer it addresses, runs the opening discovery so an application hears about a
//! peer only once it knows what that peer is, resolves double connections (SHIP §12.2.3),
//! keeps idle connections alive with the pings §10.4 asks for, dials remembered peers back,
//! and drives the clock — the engine's deadlines, the SHIP timers, and whatever the
//! application asked to be woken for.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use core::time::Duration;
//! use eebus::runtime::{Hub, HubEvent, Node, TrustStore};
//!
//! # let node: Node = unimplemented!();
//! # let engine: eebus::spine::Engine = unimplemented!();
//! let mut hub = Hub::new(node, engine);
//! hub.listen("0.0.0.0:4712").await?;                 // the household end listens…
//! hub.dial("192.0.2.10:4712".parse()?);              // …and a control box is dialled
//!
//! loop {
//!     match hub.next().await? {
//!         HubEvent::TrustRequested { ski, .. } => hub.approve(ski), // or show it to a user
//!         HubEvent::PeerDiscovered { device, .. } => println!("found {}", device.as_str()),
//!         HubEvent::Spine(event) => { /* hand it to a use case */ }
//!         HubEvent::Disconnected { ski, .. } => println!("lost {ski}"),
//!         HubEvent::Tick => { /* a timer the application asked for */ }
//!         HubEvent::PeerKeysUpdated { .. } => { /* persist the trust store */ }
//!         _ => {}
//!     }
//! }
//! # }
//! ```

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::time::Duration;
use std::time::Instant;

use std::net::SocketAddr;

use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::mpsc;

use crate::model::{AddressDevice, Datagram, MsgCounter};
use crate::ship::{CURVE_SECP256R1, ConnectionCloseReason, PeerKeys, Resolution, Ski};
use crate::spine::{Engine, SpineEvent};

use super::connection::{ConnectionError, ShipConnection, TrustReporter};
use super::node::Node;
#[cfg(all(feature = "mdns", feature = "pairing"))]
use super::node::PairedUnit;
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
/// The cap counts handshakes still in progress as well as connections held: a peer that
/// dials in and then sits in the SHIP pending state has taken a slot, and a hundred of
/// them would otherwise be a hundred TLS sessions waiting for a user who is not there.
///
/// Raise it with [`Hub::set_max_connections`] on a gateway that really does serve more.
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;

/// How many connections to the *same* peer a hub tolerates.
///
/// Two is not a mistake: SHIP §12.2.3 exists precisely because two nodes that discover
/// each other at the same moment both dial, and the second connection is legitimate
/// until arbitration settles it. A third is not something the protocol produces.
pub const MAX_CONNECTIONS_PER_PEER: usize = 2;

/// How long a dial may take to reach a TCP connection before it is given up on.
///
/// A peer that is switched off, or behind a firewall that drops rather than refuses,
/// would otherwise hold the attempt for as long as the operating system's own timeout —
/// minutes, on some systems — and the redial schedule with it. The SHIP handshake that
/// follows has timers of its own and is not covered by this.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many peers may be waiting on a trust decision at once.
///
/// A peer held in the SHIP `hello: pending` state occupies a connection slot for as long
/// as its own timers allow — up to four minutes, and longer if it keeps asking for
/// prolongation — while nothing about it has been approved by anybody. Four is more than
/// a commissioning visit needs and few enough that unapproved peers cannot fill the
/// table: past it a peer is answered `hello: aborted` at once, which tells it to try
/// again later rather than leaving it to time out.
pub const MAX_PENDING_TRUST: usize = 4;

/// How many untrusted peers seen on the network a hub remembers the address of.
///
/// Enough to hold every device on a large installation between "found" and "approved";
/// bounded because mDNS is unauthenticated and the list is filled by whoever announces.
#[cfg(feature = "mdns")]
const MAX_SIGHTINGS: usize = 64;

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

/// Which end opened a connection, and where the other end is.
///
/// Carried by the events about a connection that has not yet reached the peer's SPINE
/// address — a trust request, a failed handshake — because until then the socket address
/// is all there is to name it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The peer dialled this node.
    Accepted {
        /// The address it dialled from.
        from: SocketAddr,
    },
    /// This node dialled the peer.
    Dialed {
        /// The address it was dialled at.
        address: SocketAddr,
    },
}

impl Origin {
    /// The peer's socket address, whichever end dialled.
    pub fn address(&self) -> SocketAddr {
        match self {
            Origin::Accepted { from } => *from,
            Origin::Dialed { address } => *address,
        }
    }
}

impl core::fmt::Display for Origin {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Origin::Accepted { from } => write!(f, "accepted from {from}"),
            Origin::Dialed { address } => write!(f, "dialled at {address}"),
        }
    }
}

/// Something that happened on one of the hub's connections.
// `SpineEvent` carries a payload; boxing it would make every match arm indirect for the
// sake of a stack frame nobody is short of.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum HubEvent {
    /// A peer this node has not approved has connected and is waiting for a decision.
    ///
    /// The peer has completed TLS — so the SKI is proven, not claimed — and has been told
    /// `hello: pending` (SHIP §13.4.4.1). Show the SKI to a user and answer with
    /// [`Hub::approve`] or [`Hub::refuse`]; until then the peer keeps the connection up
    /// for as long as its own SHIP timers allow, prolonging where it can, and nothing
    /// else on the hub waits for it. A peer approved some other way — the SKI added to
    /// the [`TrustStore`](super::TrustStore) directly, or scanned from a QR code — gets
    /// through just the same.
    TrustRequested {
        /// The peer, as its certificate proved it.
        ski: Ski,
        /// Which end dialled, and where the peer is.
        origin: Origin,
    },
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
    /// A connection did not reach the data phase.
    ///
    /// A dial that found nobody, a TLS handshake that failed, a SHIP handshake the peer
    /// aborted or a trust decision that never came — and a hub with no room, which is
    /// [`ConnectionError::TooManyConnections`]. A remembered peer is dialled again on its
    /// backoff; anything else is the application's to retry.
    HandshakeFailed {
        /// Which end dialled, and where the peer is.
        origin: Origin,
        /// The peer, where the connection got far enough to prove one.
        ski: Option<Ski>,
        /// What went wrong.
        error: Arc<ConnectionError>,
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
    /// A SHIP node announced itself on the network, or changed what it announces.
    ///
    /// Reported by a [`browse`](Hub::browse). A trusted peer has already been remembered
    /// and is being dialled; an untrusted one is reported so a user can be shown it, and
    /// is dialled the moment [`Hub::approve`] names its SKI. Everything in the record is
    /// a claim until TLS proves the SKI.
    #[cfg(feature = "mdns")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
    Found {
        /// What was announced.
        peer: crate::mdns::Discovered,
        /// Whether this node already trusts it, and so has taken it up.
        trusted: bool,
    },
    /// A SHIP node withdrew its announcement, and has been dropped from the redial
    /// schedule.
    #[cfg(feature = "mdns")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
    Lost {
        /// The mDNS instance that has gone.
        instance: String,
        /// The SKI it was discovered under, where it was a remembered peer.
        ski: Option<Ski>,
    },
    /// A control unit paired itself through the SHIP Pairing Service.
    ///
    /// Its certificate is trusted from this moment, and a connection it makes reaches the
    /// data phase with nobody having compared a SKI (Pairing Service §10.2). Persist the
    /// trust store: this is a pairing, and losing it means an installer's visit.
    ///
    /// `displaced` is the unit this one replaced, if any. §10.3 permits exactly one at a
    /// time, so a replacement is also a revocation, and worth telling a user about.
    ///
    /// The unit's `ski` is [`None`] here and filled in when it connects — the request
    /// does not carry one (§10.2). A device that persists on this event alone therefore
    /// stores the pairing without it, which still admits the unit, because the
    /// fingerprint is what the trust rests on; persisting on [`Connected`](Self::Connected)
    /// as well is what records the whole of it.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    Paired {
        /// The unit now trusted.
        unit: PairedUnit,
        /// The unit it replaced.
        displaced: Option<PairedUnit>,
    },
    /// A pairing request addressed to this node was not honoured.
    ///
    /// Almost always an installer who mistyped the secret or scanned the wrong QR code,
    /// which is exactly the case the Pairing Service §5.5 expects to be corrected and
    /// re-announced — so this is worth showing, where a request for some *other* node is
    /// not reported at all.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    PairingRefused {
        /// The mDNS instance the request was announced under.
        instance: String,
        /// Why it was refused.
        error: crate::ship::pairing::PairingError,
    },
    /// The SPINE engine reported something.
    Spine(SpineEvent),
    /// A timer expired: either the engine's, or one the application asked for with
    /// [`Hub::wake_at`].
    Tick,
}

/// What a background task delivers to the loop.
enum Inbound {
    /// The listener accepted a socket; the hub decides whether to run it.
    Accepted(TcpStream),
    /// A handshake in progress found the peer unapproved and told it to wait.
    TrustRequested { ski: Ski, origin: Origin },
    /// A handshake reached the data phase.
    Established {
        connection: Box<ShipConnection>,
        origin: Origin,
        /// The SKI a redial was looking for.
        expected: Option<Ski>,
    },
    /// A handshake did not.
    Failed {
        origin: Origin,
        expected: Option<Ski>,
        ski: Option<Ski>,
        error: ConnectionError,
    },
    #[cfg(feature = "mdns")]
    Found(Box<crate::mdns::Discovered>),
    #[cfg(feature = "mdns")]
    Lost(String),
    /// A `_shippairing._tcp` request was announced or withdrawn.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    Pairing(crate::mdns::PairingEvent),
}

/// What woke the loop.
enum Wake {
    Inbound(Inbound),
    Frame((usize, Result<crate::ship::ShipMessage, ConnectionError>)),
    Deadline,
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
    instance: Option<String>,
    /// How many attempts have failed since the last success.
    attempts: u32,
    /// When to dial next, if it is not connected.
    next_attempt: Duration,
    /// Whether a dial is running in the background right now.
    dialing: bool,
}

/// An untrusted peer seen on the network, kept so an approval can dial it.
#[derive(Clone, Debug)]
struct Sighting {
    ski: Ski,
    address: SocketAddr,
    instance: String,
}

/// `devA`'s end of the SHIP Pairing Service, and the §4.3 policy over it.
#[cfg(all(feature = "mdns", feature = "pairing"))]
#[derive(Debug)]
struct Pairing {
    receiver: crate::ship::pairing::Receiver,
    /// When the paired unit was last in SHIP message exchange.
    ///
    /// [`None`] means "not since this hub started", which is the same thing for §4.3's
    /// purposes: the rule is about a *continuously running* node that has been unable to
    /// reach its unit for fifteen minutes, and a hub that has just started has been
    /// unable to reach it for exactly as long as it has been running.
    last_exchange: Option<Duration>,
}

#[cfg(all(feature = "mdns", feature = "pairing"))]
impl Pairing {
    /// Whether `addCu` requests are being processed at all (Pairing Service §4.3).
    ///
    /// Off while the pairing is working, which is what stops a captured or malicious
    /// announcement breaking a pairing that is doing its job — the specification is
    /// explicit that this matters most when the secret is static. On again only once the
    /// paired unit has been unreachable for [`REPLACEABLE_AFTER`](crate::ship::pairing::REPLACEABLE_AFTER),
    /// which is how a broken control unit gets replaced without a factory reset.
    fn accepting(&self, paired: bool, now: Duration) -> bool {
        use crate::ship::pairing::REPLACEABLE_AFTER;
        !paired || now >= self.last_exchange.unwrap_or(Duration::ZERO) + REPLACEABLE_AFTER
    }
}

/// A node's connections and the SPINE engine behind them.
#[derive(Debug)]
pub struct Hub {
    node: Arc<Node>,
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
    /// Untrusted peers the network has announced, waiting on an approval.
    sightings: Vec<Sighting>,
    /// What is known about each peer's key material (SHIP §12.1.3).
    peer_keys: Vec<(Ski, PeerKeys)>,
    pending: VecDeque<HubEvent>,
    /// The connection a write is in flight on, if one is.
    ///
    /// Set immediately before an `await` that puts bytes on a socket and cleared after it,
    /// so that a `next` future dropped in between leaves a mark. Finding it set at the top
    /// of the next call is proof the previous one was cancelled mid-write — the one hazard
    /// `Hub::next` cannot defend against, made detectable instead of silent.
    writing: Option<usize>,
    /// What the background tasks deliver: accepted sockets, finished handshakes, trust
    /// requests, and what mDNS found.
    inbox: mpsc::UnboundedReceiver<Inbound>,
    inbox_tx: mpsc::UnboundedSender<Inbound>,
    /// Handshakes running in the background, counted against the cap.
    in_flight: usize,
    /// Handshakes held in the SHIP pending state, waiting on a decision.
    ///
    /// Keyed by [`Origin`] rather than by SKI, because that is what the outcome comes
    /// back with: a handshake that fails before reaching the data phase reports where it
    /// was, and the SKI is remembered here so the failure can still name the peer.
    /// Bounded by [`MAX_PENDING_TRUST`] — an unapproved peer holds a connection slot on
    /// nobody's authority, so the number of them is not left to whoever is on the wire.
    awaiting_trust: Vec<(Origin, Ski)>,
    /// `devA`'s end of the SHIP Pairing Service, once an application has turned it on.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    pairing: Option<Pairing>,
    /// The listener and browse tasks this hub owns, ended with it.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Hub {
    /// A hub serving `engine` over `node`'s connections.
    pub fn new(node: Node, engine: Engine) -> Self {
        let (inbox_tx, inbox) = mpsc::unbounded_channel();
        Self {
            node: Arc::new(node),
            engine,
            links: Vec::new(),
            clock: Instant::now(),
            wake_at: None,
            duplicates: Vec::new(),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            known: Vec::new(),
            sightings: Vec::new(),
            peer_keys: Vec::new(),
            pending: VecDeque::new(),
            writing: None,
            inbox,
            inbox_tx,
            in_flight: 0,
            awaiting_trust: Vec::new(),
            #[cfg(all(feature = "mdns", feature = "pairing"))]
            pairing: None,
            tasks: Vec::new(),
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

    /// This node's certificate fingerprint, which the Pairing Service identifies it by.
    pub fn fingerprint(&self) -> crate::ship::Fingerprint {
        self.node.fingerprint()
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

    // ---- pairing ---------------------------------------------------------------

    /// Approves a peer: adds its SKI to the trust store, lets a handshake waiting on it
    /// through, and dials it if the network has announced it.
    ///
    /// The answer to [`HubEvent::TrustRequested`], and to a [`HubEvent::Found`] that was
    /// not trusted. Adding the SKI to the [`TrustStore`](super::TrustStore) directly does
    /// the first two of those as well — the store is what the handshake watches — so an
    /// application that approves from a user interface thread needs only that; this is
    /// the same, plus the dial. Persist the store afterwards, or the approval is gone at
    /// the next restart.
    pub fn approve(&mut self, ski: Ski) {
        self.node.trust_store().trust(ski);
        if let Some(index) = self.sightings.iter().position(|s| s.ski == ski) {
            let sighting = self.sightings.remove(index);
            self.remember(ski, sighting.address);
            if let Some(known) = self.known.iter_mut().find(|k| k.ski == ski) {
                known.instance = Some(sighting.instance);
            }
        }
    }

    /// Refuses a peer whose handshake is waiting on a decision.
    ///
    /// The other answer to [`HubEvent::TrustRequested`]: the peer is told `hello: aborted`
    /// and the connection ends, reported as [`HubEvent::HandshakeFailed`]. It says nothing
    /// about the future — the peer may dial again and be asked about again — and it does
    /// not forget a SKI already in the trust store; that is
    /// [`TrustStore::forget`](super::TrustStore::forget).
    pub fn refuse(&self, ski: Ski) {
        self.node.refuse_pairing(ski);
    }

    // ---- opening connections ---------------------------------------------------

    /// Binds a listener and accepts on it for as long as the hub lives.
    ///
    /// Every socket accepted goes through [`accept`](Self::accept): TLS, the WebSocket
    /// upgrade and the SHIP handshake run in the background, and the outcome arrives as
    /// [`HubEvent::Connected`], [`HubEvent::TrustRequested`] or
    /// [`HubEvent::HandshakeFailed`]. Returns the address bound, which is what an mDNS
    /// announcement needs.
    pub async fn listen(
        &mut self,
        address: impl ToSocketAddrs,
    ) -> Result<SocketAddr, ConnectionError> {
        let listener = self.node.listen(address).await?;
        let bound = listener.local_addr()?;
        let inbox = self.inbox_tx.clone();
        self.tasks.push(tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        if inbox.send(Inbound::Accepted(stream)).is_err() {
                            return;
                        }
                    }
                    // Out of descriptors, most likely. Spinning on the error would make
                    // it worse; a pause lets the load pass.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        }));
        Ok(bound)
    }

    /// Dials a peer once, in the background.
    ///
    /// For an address that came from configuration rather than from discovery — a peer to
    /// dial once and see. The outcome arrives as [`HubEvent::Connected`],
    /// [`HubEvent::TrustRequested`] or [`HubEvent::HandshakeFailed`], and nothing on the
    /// hub waits for it. A peer that should be dialled *again* whenever the connection
    /// drops is [`remember`](Self::remember).
    pub fn dial(&mut self, address: SocketAddr) {
        self.spawn_dial(address, None);
    }

    /// Runs the server side of the stack on an accepted socket, in the background.
    ///
    /// [`listen`](Self::listen) calls this for every socket it accepts; it is public for
    /// a listener the application owns. The outcome arrives as an event. A hub with no
    /// room — connections held plus handshakes running — drops the socket at once and
    /// reports [`ConnectionError::TooManyConnections`], so that a device dialling in
    /// repeatedly cannot displace the peers already served.
    pub fn accept(&mut self, stream: TcpStream) {
        let from = stream
            .peer_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let origin = Origin::Accepted { from };
        if !self.has_room() {
            drop(stream);
            self.pending.push_back(HubEvent::HandshakeFailed {
                origin,
                ski: None,
                error: Arc::new(ConnectionError::TooManyConnections),
            });
            return;
        }
        self.in_flight += 1;
        let node = self.node.clone();
        let inbox = self.inbox_tx.clone();
        let report = self.reporter(origin);
        tokio::spawn(async move {
            let outcome = node.accept_reporting(stream, Some(report)).await;
            let _ = inbox.send(match outcome {
                Ok(connection) => Inbound::Established {
                    connection: Box::new(connection),
                    origin,
                    expected: None,
                },
                Err(error) => Inbound::Failed {
                    origin,
                    expected: None,
                    ski: None,
                    error,
                },
            });
        });
    }

    /// Adds a connection the caller established itself.
    ///
    /// # Errors
    ///
    /// Hands the connection back when the hub is already holding
    /// [`max_connections`](Self::max_connections), or a second connection to this peer
    /// already exists and is being arbitrated. The caller owns the refused connection and
    /// decides how to end it. It comes back boxed because a `ShipConnection` is two
    /// kilobytes and every ordinary call would otherwise carry that on the stack.
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
        self.pending.push_back(HubEvent::Connected { ski, version });
        self.start_discovery(self.links.len() - 1, now);
        Ok(ski)
    }

    /// Whether another handshake fits under the cap.
    fn has_room(&self) -> bool {
        self.links.len() + self.in_flight < self.max_connections
    }

    /// A callback that turns a handshake's trust request into an event on this hub.
    fn reporter(&self, origin: Origin) -> TrustReporter {
        let inbox = self.inbox_tx.clone();
        Box::new(move |ski| {
            let _ = inbox.send(Inbound::TrustRequested { ski, origin });
        })
    }

    /// Starts a dial in the background, looking for `expected` if it is a redial.
    fn spawn_dial(&mut self, address: SocketAddr, expected: Option<Ski>) {
        let origin = Origin::Dialed { address };
        if !self.has_room() {
            self.pending.push_back(HubEvent::HandshakeFailed {
                origin,
                ski: expected,
                error: Arc::new(ConnectionError::TooManyConnections),
            });
            if let Some(ski) = expected {
                let now = self.now();
                self.defer_redial(&ski, now);
            }
            return;
        }
        self.in_flight += 1;
        if let Some(known) = expected.and_then(|ski| self.known.iter_mut().find(|k| k.ski == ski)) {
            known.dialing = true;
        }
        let node = self.node.clone();
        let inbox = self.inbox_tx.clone();
        let report = self.reporter(origin);
        tokio::spawn(async move {
            // The timeout covers reaching the peer, not the handshake: a peer that answers
            // and then holds this node in the pending state for a user to approve is
            // doing what SHIP asks, and has timers of its own.
            let stream =
                match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
                    Ok(Ok(stream)) => Ok(stream),
                    Ok(Err(error)) => Err(ConnectionError::Io(error)),
                    Err(_) => Err(ConnectionError::Timeout),
                };
            let outcome = match stream {
                Ok(stream) => match stream.set_nodelay(true) {
                    Ok(()) => node.connect_over_reporting(stream, Some(report)).await,
                    Err(error) => Err(ConnectionError::Io(error)),
                },
                Err(error) => Err(error),
            };
            let _ = inbox.send(match outcome {
                Ok(connection) => Inbound::Established {
                    connection: Box::new(connection),
                    origin,
                    expected,
                },
                Err(error) => Inbound::Failed {
                    origin,
                    expected,
                    ski: None,
                    error,
                },
            });
        });
    }

    // ---- peers to keep ---------------------------------------------------------

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
                dialing: false,
            }),
        }
    }

    /// Remembers a peer mDNS found, if it is one this node trusts.
    ///
    /// [`browse`](Self::browse) does this for every announcement it sees; it is public
    /// for an application that runs its own browse. A trusted peer is dialled and kept
    /// dialled; an untrusted one is kept aside, and dialled the moment
    /// [`approve`](Self::approve) names it. Returns whether the peer was taken up.
    ///
    /// Everything in a TXT record is a claim rather than a fact — the SKI included. It
    /// becomes a fact when TLS proves the peer holds the matching key, which is why the
    /// hub checks what it connected to rather than what it was told.
    #[cfg(feature = "mdns")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
    pub fn remember_discovered(&mut self, found: &crate::mdns::Discovered) -> bool {
        let Some(address) = found.socket_address() else {
            return false;
        };
        if !self.node.trust_store().is_trusted(&found.ski) {
            self.sightings.retain(|s| s.ski != found.ski);
            if self.sightings.len() >= MAX_SIGHTINGS {
                self.sightings.remove(0);
            }
            self.sightings.push(Sighting {
                ski: found.ski,
                address,
                instance: found.instance.clone(),
            });
            return false;
        }
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
        self.sightings.retain(|s| s.instance != instance);
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

    /// Turns on `devA`'s end of the SHIP Pairing Service.
    ///
    /// From here on, [`browse_pairing`](Self::browse_pairing) evaluates every
    /// `_shippairing._tcp` request the network announces against the secret printed on
    /// this node's label, and an authentic one is trusted without asking anybody:
    /// [`HubEvent::Paired`]. This is what makes a metering control unit installable by an
    /// electrician who never sees either device's screen.
    ///
    /// The [`Receiver`](crate::ship::pairing::Receiver) carries the replay guard of §11,
    /// whose entries a device persists and restores — without that, a captured
    /// announcement replayed after a reboot is honoured again.
    ///
    /// ```no_run
    /// # fn example(hub: &mut eebus::runtime::Hub, mdns: &eebus::mdns::Mdns)
    /// #     -> Result<(), Box<dyn std::error::Error>> {
    /// use eebus::ship::pairing::{PairingSecret, Receiver};
    ///
    /// let secret = PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE")?;
    /// let fingerprint = hub.node().fingerprint();
    /// hub.accept_pairing_requests(Receiver::new(hub.ship_id().to_string(), fingerprint, secret));
    /// hub.browse_pairing(mdns)?;
    /// # Ok(()) }
    /// ```
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    pub fn accept_pairing_requests(&mut self, receiver: crate::ship::pairing::Receiver) {
        self.pairing = Some(Pairing {
            receiver,
            last_exchange: None,
        });
    }

    /// Stops evaluating pairing requests. The unit already paired stays trusted.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    pub fn refuse_pairing_requests(&mut self) {
        self.pairing = None;
    }

    /// The replay guard, whose entries are what to persist after a [`HubEvent::Paired`].
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    pub fn pairing_guard(&self) -> Option<&crate::ship::pairing::ReplayGuard> {
        self.pairing.as_ref().map(|p| p.receiver.guard())
    }

    /// Browses for `_shippairing._tcp` requests for as long as the hub lives.
    ///
    /// Requests are evaluated by [`accept_pairing_requests`](Self::accept_pairing_requests)'s
    /// receiver; without one, nothing is looked at. A request for another node is not
    /// reported — on a network with two energy managers each sees the other's — and one
    /// for this node that fails becomes [`HubEvent::PairingRefused`].
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    #[cfg_attr(docsrs, doc(cfg(all(feature = "mdns", feature = "pairing"))))]
    pub fn browse_pairing(
        &mut self,
        mdns: &crate::mdns::Mdns,
    ) -> Result<(), crate::mdns::MdnsError> {
        let browse = mdns.browse_pairing()?;
        let inbox = self.inbox_tx.clone();
        std::thread::Builder::new()
            .name("eebus-mdns-pairing".into())
            .spawn(move || {
                loop {
                    if inbox.is_closed() {
                        return;
                    }
                    let Some(event) = browse.recv_timeout(Duration::from_millis(500)) else {
                        continue;
                    };
                    if inbox.send(Inbound::Pairing(event)).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| {
                crate::mdns::MdnsError::Daemon(mdns_sd::Error::Msg(error.to_string()))
            })?;
        Ok(())
    }

    /// Evaluates one announced pairing request (Pairing Service §9), and acts on it.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    fn evaluate_pairing(&mut self, event: crate::mdns::PairingEvent) {
        use crate::mdns::PairingEvent;
        use crate::ship::pairing::PairingError;

        // A withdrawal is `devZ` saying the request is finished with. Nothing to undo:
        // the trust it established is trust, and §4.2 has the request disappear as soon
        // as the connection it asked for is working.
        let PairingEvent::Announced { instance, pairs } = event else {
            return;
        };
        let now = self.now();
        let paired = self.node.trust_store().unit().is_some();
        let Some(pairing) = self.pairing.as_mut() else {
            return;
        };
        if !pairing.accepting(paired, now) {
            return;
        }
        let request = match pairing.receiver.evaluate(&pairs) {
            Ok(request) => request,
            // Not addressed here. Silent by design: this is the ordinary case on any
            // network with more than one energy manager on it.
            Err(PairingError::NotForThisNode) => return,
            Err(error) => {
                self.pending
                    .push_back(HubEvent::PairingRefused { instance, error });
                return;
            }
        };
        let unit = PairedUnit::from_request(&request);
        // A unit that re-pairs — a reboot, a fresh nonce — displaces "itself", which is
        // not a revocation and must not close its connection or be reported as one.
        let displaced = self
            .node
            .trust_store()
            .trust_unit(unit.clone())
            .filter(|previous| previous.fingerprint != unit.fingerprint);
        // §4.3: the pairing is now the one being protected, so it is what the fifteen
        // minutes are measured against — not the unit that was just replaced.
        if let Some(pairing) = self.pairing.as_mut() {
            pairing.last_exchange = Some(now);
        }
        // A unit that has been displaced is gone: its connection is not authorised any
        // more, and leaving it up would let the old control unit keep writing limits.
        if let Some(ski) = displaced.as_ref().and_then(|unit| unit.ski) {
            self.close_links_to(&ski);
        }
        self.pending.push_back(HubEvent::Paired { unit, displaced });
    }

    /// Notes that the paired control unit is reachable (Pairing Service §4.3, rule 1.b.ii).
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    fn note_unit_exchange(&mut self, fingerprint: crate::ship::Fingerprint, now: Duration) {
        if self.pairing.is_none() {
            return;
        }
        if self
            .node
            .trust_store()
            .unit()
            .is_some_and(|unit| unit.fingerprint == fingerprint)
            && let Some(pairing) = self.pairing.as_mut()
        {
            pairing.last_exchange = Some(now);
        }
    }

    /// Closes every connection to a peer, off the loop.
    ///
    /// The same shape as the double-connection close: `connectionClose{announce}` goes
    /// out and the peer is given `maxTime` to confirm, but nothing here waits for it —
    /// this is called from the event path, where an await would hold up every other
    /// connection. Only the Pairing Service needs it today, for §10.3's displaced
    /// control unit; [`disconnect`](Self::disconnect) is the public equivalent for a
    /// caller that can wait.
    #[cfg(all(feature = "mdns", feature = "pairing"))]
    fn close_links_to(&mut self, ski: &Ski) {
        let doomed: Vec<usize> = self
            .links
            .iter()
            .enumerate()
            .filter(|(_, link)| &link.ski == ski)
            .map(|(index, _)| index)
            .collect();
        for index in doomed.into_iter().rev() {
            let link = self.links.remove(index);
            let device = link.device.clone();
            tokio::spawn(async move {
                let _ = link
                    .connection
                    .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                    .await;
            });
            self.forget_peer_state(*ski, device, Disconnect::Local);
        }
    }

    /// Browses for `_ship._tcp` on `mdns` for as long as the hub lives.
    ///
    /// Every announcement arrives as [`HubEvent::Found`], having already been taken up
    /// through [`remember_discovered`](Self::remember_discovered); every withdrawal as
    /// [`HubEvent::Lost`], having already been dropped from the redial schedule. With a
    /// listener and an announcement beside it, this is the whole of a device's networking:
    /// it finds its peers, is found by them, and asks the application only when a trust
    /// decision is needed.
    #[cfg(feature = "mdns")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mdns")))]
    pub fn browse(&mut self, mdns: &crate::mdns::Mdns) -> Result<(), crate::mdns::MdnsError> {
        let browse = mdns.browse()?;
        let inbox = self.inbox_tx.clone();
        // The mDNS receiver blocks a thread rather than a task, so it gets a thread of
        // its own, and checks between events whether the hub is still there to tell.
        std::thread::Builder::new()
            .name("eebus-mdns-browse".into())
            .spawn(move || {
                loop {
                    if inbox.is_closed() {
                        return;
                    }
                    let Some(event) = browse.recv_timeout(Duration::from_millis(500)) else {
                        continue;
                    };
                    let inbound = match event {
                        crate::mdns::BrowseEvent::Found(found) => Inbound::Found(Box::new(found)),
                        crate::mdns::BrowseEvent::Lost { instance } => Inbound::Lost(instance),
                    };
                    if inbox.send(inbound).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| {
                crate::mdns::MdnsError::Daemon(mdns_sd::Error::Msg(error.to_string()))
            })?;
        Ok(())
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

    // ---- key material -----------------------------------------------------------

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
            self.writing = Some(index);
            let _ = self.links[index].connection.send_key_material().await;
            self.writing = None;
        }
        Ok(())
    }

    // ---- closing -----------------------------------------------------------------

    /// Closes one peer's connections, telling it why.
    pub async fn disconnect(&mut self, ski: &Ski, reason: ConnectionCloseReason) {
        while let Some(index) = self.links.iter().position(|l| &l.ski == ski) {
            let link = self.links.remove(index);
            let _ = link.connection.close(reason, CLOSE_MAX_TIME).await;
        }
        self.forget(ski);
        self.pending.push_back(HubEvent::Disconnected {
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
    /// decided. The listener and browse tasks end with it.
    pub async fn shutdown(&mut self, reason: ConnectionCloseReason) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        let _ = self.dispatch().await;
        for link in core::mem::take(&mut self.links) {
            let _ = link.connection.close(reason, CLOSE_MAX_TIME).await;
        }
        self.duplicates.clear();
    }

    // ---- the loop ----------------------------------------------------------------

    /// Runs the hub's loop, handing every event to `handler`, until `handler` says stop.
    ///
    /// **This is the shape to reach for**, because [`next`](Self::next) is not cancel-safe
    /// and this loop cannot cancel it. `handler` returns [`ControlFlow::Break`](core::ops::ControlFlow::Break) to end the
    /// loop, and gets `&mut Hub` so that it can do anything the loop could — approve a
    /// peer, write a limit, ask for a tick.
    ///
    /// ```no_run
    /// # async fn example(mut hub: eebus::runtime::Hub) -> Result<(), Box<dyn std::error::Error>> {
    /// use core::ops::ControlFlow;
    /// use eebus::runtime::HubEvent;
    ///
    /// hub.listen("0.0.0.0:4712").await?;
    /// hub.run(|hub, event| {
    ///     if let HubEvent::TrustRequested { ski, .. } = event {
    ///         println!("approve {ski}?"); // a real device asks its user
    ///         hub.approve(ski);
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
    /// written a limit does not have to remember to flush. Then it waits — on every
    /// connection, on the handshakes and dials running in the background, on the
    /// listener and the browse, and on the earliest deadline — and returns the first
    /// thing that arrives.
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
    /// * **Sockets** — [`listen`](Self::listen), [`dial`](Self::dial) and
    ///   [`browse`](Self::browse) run in the background and arrive as events.
    /// * **Anything else arriving from elsewhere** — a command channel, say — is drained
    ///   *between* calls, on a tick asked for with `wake_at`.
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
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
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
            self.redial(now);
            let deadline = self.next_deadline(now);

            // A deadline that has already passed is dealt with *before* the sockets are
            // touched, and not by reading with a zero-length timeout. A zero timeout
            // cancels the read immediately, and a caller that keeps handing back a stale
            // instant — an actor whose timers have not caught up, most easily — would
            // spin here forever and never read a byte. The connection would look alive
            // and carry nothing.
            if deadline.is_some_and(|at| at <= now) {
                self.fire_timers(now);
                self.keepalive().await?;
                continue;
            }

            let wake = {
                let Hub { inbox, links, .. } = &mut *self;
                let wait = deadline.map(|at| at.saturating_sub(now));
                // Every branch is cancel-safe: the channel hands a message over whole
                // or not at all, and the reads are what `ShipConnection::next_message`
                // exists to make droppable. Losing the race costs nothing.
                tokio::select! {
                    biased;
                    inbound = inbox.recv() => match inbound {
                        Some(inbound) => Wake::Inbound(inbound),
                        // The hub holds a sender, so this cannot happen; treated as a
                        // wake-up rather than an error.
                        None => Wake::Deadline,
                    },
                    outcome = read_any(links) => Wake::Frame(outcome),
                    _ = tokio::time::sleep(wait.unwrap_or(Duration::ZERO)), if wait.is_some() => {
                        Wake::Deadline
                    }
                }
            };

            match wake {
                Wake::Inbound(inbound) => self.handle_inbound(inbound),
                Wake::Frame((index, Ok(message))) => {
                    let now = self.now();
                    self.links[index].last_seen = now;
                    self.links[index].pinged_at = None;
                    // Pairing Service §4.3 rule 1.a counts *SHIP Message Exchange*, not
                    // connections: this is where a working pairing keeps proving it works,
                    // and so keeps the node from entertaining a request to replace it.
                    #[cfg(all(feature = "mdns", feature = "pairing"))]
                    {
                        let fingerprint = self.links[index].connection.peer_fingerprint();
                        self.note_unit_exchange(fingerprint, now);
                    }
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
                Wake::Frame((index, Err(error))) => {
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
                Wake::Deadline => {
                    let now = self.now();
                    self.fire_timers(now);
                    self.keepalive().await?;
                }
            }
        }
    }

    // ---- internals -------------------------------------------------------------

    /// Takes what a background task delivered.
    fn handle_inbound(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Accepted(stream) => self.accept(stream),
            Inbound::TrustRequested { ski, origin } => {
                if self.awaiting_trust.iter().any(|(held, _)| held == &origin) {
                    // The same handshake reporting twice, which it does not, but a
                    // duplicate must not consume a second slot.
                    return;
                }
                if self.awaiting_trust.len() >= MAX_PENDING_TRUST {
                    // Refused rather than queued: the peer is told `hello: aborted` and
                    // its slot comes back at once, where leaving it pending would hold a
                    // connection for minutes on nobody's authority. It may ask again.
                    self.node.refuse_pairing(ski);
                    self.pending.push_back(HubEvent::HandshakeFailed {
                        origin,
                        ski: Some(ski),
                        error: Arc::new(ConnectionError::TooManyPendingPairings),
                    });
                    return;
                }
                self.awaiting_trust.push((origin, ski));
                // The peer whose SKI is *already* waiting on a decision is not asked
                // about twice: one peer is one decision, and answering it releases every
                // handshake it is holding, because the trust store is what they watch.
                if self
                    .awaiting_trust
                    .iter()
                    .filter(|(_, waiting)| waiting == &ski)
                    .count()
                    == 1
                {
                    self.pending
                        .push_back(HubEvent::TrustRequested { ski, origin });
                }
            }
            #[cfg(all(feature = "mdns", feature = "pairing"))]
            Inbound::Pairing(event) => self.evaluate_pairing(event),
            Inbound::Established {
                connection,
                origin,
                expected,
            } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                let now = self.now();
                let ski = connection.peer();
                self.awaiting_trust.retain(|(held, _)| held != &origin);
                #[cfg(all(feature = "mdns", feature = "pairing"))]
                self.note_unit_exchange(connection.peer_fingerprint(), now);
                if let Some(expected) = expected
                    && let Some(known) = self.known.iter_mut().find(|k| k.ski == expected)
                {
                    known.dialing = false;
                }
                match self.adopt(*connection) {
                    Ok(_) => match expected {
                        Some(expected) if expected == ski => {
                            if let Some(known) = self.known.iter_mut().find(|k| k.ski == ski) {
                                known.attempts = 0;
                                known.next_attempt = now;
                            }
                        }
                        // Somebody else answered at that address. Adopting it is still
                        // right — the peer proved an identity and the trust store decided
                        // about it — but the peer we were looking for is not there.
                        Some(expected) => self.defer_redial(&expected, now),
                        None => {}
                    },
                    Err(refused) => {
                        // No room, or a third connection to one peer. Closed politely,
                        // off the loop: the peer is given `maxTime` to confirm and nothing
                        // here waits for it.
                        tokio::spawn(async move {
                            let _ = refused
                                .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                                .await;
                        });
                        self.pending.push_back(HubEvent::HandshakeFailed {
                            origin,
                            ski: Some(ski),
                            error: Arc::new(ConnectionError::TooManyConnections),
                        });
                        if let Some(expected) = expected {
                            self.defer_redial(&expected, now);
                        }
                    }
                }
            }
            Inbound::Failed {
                origin,
                expected,
                ski,
                error,
            } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                // One lookup, two jobs: name the peer, which a handshake that reached
                // the pending state proved, and give its slot back — whatever the
                // failure, or a refused peer's slot never returns to the pool.
                let index = self
                    .awaiting_trust
                    .iter()
                    .position(|(held, _)| held == &origin);
                let ski = match index.map(|index| self.awaiting_trust.remove(index)) {
                    Some((_, waiting)) => Some(waiting),
                    None => ski,
                };
                if let Some(expected) = expected {
                    let now = self.now();
                    if let Some(known) = self.known.iter_mut().find(|k| k.ski == expected) {
                        known.dialing = false;
                    }
                    self.defer_redial(&expected, now);
                }
                self.pending.push_back(HubEvent::HandshakeFailed {
                    origin,
                    ski,
                    error: Arc::new(error),
                });
            }
            #[cfg(feature = "mdns")]
            Inbound::Found(found) => {
                let trusted = self.remember_discovered(&found);
                self.pending.push_back(HubEvent::Found {
                    peer: *found,
                    trusted,
                });
            }
            #[cfg(feature = "mdns")]
            Inbound::Lost(instance) => {
                let ski = self.forget_discovered(&instance);
                self.pending.push_back(HubEvent::Lost { instance, ski });
            }
        }
    }

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
            self.pending
                .push_back(HubEvent::PeerDiscovered { ski, device });
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
                self.pending.push_back(HubEvent::PeerKeysUpdated {
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
                    self.writing = Some(index);
                    let _ = self.links[index]
                        .connection
                        .request_key_material(known)
                        .await;
                    self.writing = None;
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
                self.close_duplicates(&ski);
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
                        self.writing = Some(index);
                        let _ = self.links[index].connection.ping().await;
                        self.writing = None;
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
                self.close_duplicates(&ski);
                Ok(true)
            }
        }
    }

    /// Keeps the most recent connection to `ski` and closes the rest.
    fn close_duplicates(&mut self, ski: &Ski) {
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
            // of §13.4.8, not by pulling the socket out from under the peer — and off
            // the loop, because the peer is given two seconds to confirm and nothing
            // else should wait for it.
            tokio::spawn(async move {
                let _ = link
                    .connection
                    .close(ConnectionCloseReason::Unspecific, CLOSE_MAX_TIME)
                    .await;
            });
        }
        self.duplicates.retain(|d| &d.ski != ski);
        self.pending
            .retain(|e| !matches!(e, HubEvent::Disconnected { ski: s, .. } if s == ski));
    }

    /// The earliest instant anything wants attention.
    fn next_deadline(&self, _now: Duration) -> Option<Duration> {
        let mut next = self.wake_at;
        let mut consider = |at: Duration| {
            next = Some(next.map_or(at, |current: Duration| current.min(at)));
        };

        if let Some(at) = self.engine.poll_timeout() {
            consider(at);
        }
        for known in &self.known {
            if !known.dialing && !self.links.iter().any(|l| l.ski == known.ski) {
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
            self.pending.push_back(HubEvent::Tick);
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
        self.pending
            .push_back(HubEvent::Disconnected { ski, reason });
    }

    fn forget(&mut self, ski: &Ski) {
        self.duplicates.retain(|d| &d.ski != ski);
    }

    /// Counts a failed attempt against a remembered peer and schedules the next one.
    fn defer_redial(&mut self, ski: &Ski, now: Duration) {
        if let Some(known) = self.known.iter_mut().find(|k| &k.ski == ski) {
            known.attempts = known.attempts.saturating_add(1);
            known.next_attempt = now + reconnect_delay_for(&known.ski, known.attempts);
        }
    }

    /// Starts a dial for every remembered peer that is not connected, not already being
    /// dialled, and whose backoff has expired.
    fn redial(&mut self, now: Duration) {
        let due: Vec<(Ski, SocketAddr)> = self
            .known
            .iter()
            .filter(|known| {
                !known.dialing
                    && now >= known.next_attempt
                    && !self.links.iter().any(|l| l.ski == known.ski)
            })
            .map(|known| (known.ski, known.address))
            .collect();
        for (ski, address) in due {
            self.spawn_dial(address, Some(ski));
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

impl Drop for Hub {
    /// The listener and browse tasks are the hub's, and end with it.
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

/// Reads whichever connection speaks first.
///
/// Every future here is a single cancel-safe socket read, so dropping the losers costs
/// nothing — which is the property [`ShipConnection::next_message`] exists to provide, and
/// which holds for a more specific reason than "it does not write": the WebSocket layer
/// does answer a ping while reading, and never blocks doing it. With nothing to read from
/// it never resolves, and the loop waits on its other sources alone.
async fn read_any(
    links: &mut [Link],
) -> (usize, Result<crate::ship::ShipMessage, ConnectionError>) {
    use futures_util::future::{FutureExt, select_all};

    if links.is_empty() {
        return core::future::pending().await;
    }
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
