//! A node on the network: what it will accept, and whom it will dial.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use std::sync::Mutex;

use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::sync::watch;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as ServerRequest, Response as ServerResponse,
};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::ship::{
    COMMISSIONING_TRUST, DEFAULT_PATH, Fingerprint, HandshakeConfig, Role, SUBPROTOCOL, Ski, Trust,
    TrustLevel,
};
use crate::tls::{ShipTls, peer_fingerprint, peer_ski};

use super::connection::{
    ConnectionError, ShipConnection, TrustReporter, check_subprotocol, run_handshake,
};

/// One remembered peer.
///
/// The SKI is what the trust decision is made on; everything else is what SHIP §12.2.2.1
/// calls optional storage, and is here because a store that can only say "40 hex digits"
/// is a store a user cannot audit. A person deciding whether to revoke something needs to
/// see which box it was.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustedPeer {
    /// The Subject Key Identifier, which is the identity SHIP trusts.
    pub ski: Ski,
    /// What the user calls it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    /// The SHIP ID it announced over mDNS, `<IANA PEN>_<vendor product ID>`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ship_id: Option<String>,
    /// When trust was established, ISO 8601, where the device had a clock to read.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub trusted_at: Option<String>,
    /// The SHA-256 fingerprint of the certificate it presented.
    ///
    /// Recorded when it is known — a peer paired through the Pairing Service announces
    /// it, and every peer that completes TLS proves it — and used for nothing on its
    /// own: the trust decision for this record is the SKI. It is here because §10.4 asks
    /// a store to carry what it has learned about a peer, and because a person auditing
    /// a pairing is owed the value that was on the sticker.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fingerprint: Option<Fingerprint>,
    /// What this key is trusted **at** (SHIP §12.3.2, Table 10).
    ///
    /// Not decoration, and not a boolean by accident: §12.3.2 and §12.5 state three rules
    /// about the numbers, and the one that bites is "the SHIP node PIN SHALL NOT be
    /// transmitted if the public key of the corresponding communication partner has a user
    /// trust level that is less than '32'". A store that only knows "trusted" cannot answer
    /// that question, and answers it wrongly by default.
    ///
    /// [`TrustLevel::USER_VERIFIED`] is the default because it is what
    /// [`TrustStore::trust`] means: a person compared the SKI with the label and said yes.
    /// A store restored from JSON written before this field existed reads the same way.
    #[serde(default = "default_trust_level")]
    pub trust: TrustLevel,
}

fn default_trust_level() -> TrustLevel {
    TrustLevel::USER_VERIFIED
}

impl TrustedPeer {
    /// A peer known only by its SKI, which is all trust actually requires.
    ///
    /// There is deliberately no [`Default`]: a trust record with no SKI in it is not a
    /// record of trusting anything.
    pub fn new(ski: Ski) -> Self {
        Self {
            ski,
            name: None,
            ship_id: None,
            trusted_at: None,
            fingerprint: None,
            trust: TrustLevel::USER_VERIFIED,
        }
    }

    /// Records what this key is trusted at (§12.3.2, Table 10).
    ///
    /// Use it where the mechanism was not a person comparing forty hex digits: a device
    /// admitted by a commissioning tool is [`TrustLevel::commissioned`], one whose key a
    /// person typed in is [`TrustLevel::USER_INPUT`].
    #[must_use]
    pub fn at_level(mut self, trust: TrustLevel) -> Self {
        self.trust = trust;
        self
    }

    /// Names it, so a user can tell one line of the store from the next.
    #[must_use]
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Records the SHIP ID it announced.
    #[must_use]
    pub fn with_ship_id(mut self, ship_id: impl Into<String>) -> Self {
        self.ship_id = Some(ship_id.into());
        self
    }

    /// Records when trust was established.
    #[must_use]
    pub fn at_time(mut self, timestamp: impl Into<String>) -> Self {
        self.trusted_at = Some(timestamp.into());
        self
    }

    /// Records the fingerprint of the certificate it presented.
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: Fingerprint) -> Self {
        self.fingerprint = Some(fingerprint);
        self
    }
}

/// A control unit trusted through the SHIP Pairing Service.
///
/// The Pairing Service trusts a **certificate**, not a key identifier: what `devZ`
/// announces is the SHA-256 fingerprint of the certificate it will present, and §10.2
/// says that matching it at the TLS handshake "SHALL be seen like a successful trust in
/// an SKI". So it is stored as what it is, rather than as a `TrustedPeer` with an
/// invented SKI — the SKI is not known until the unit connects, and §10.4 makes the SHIP
/// ID the primary identifier instead.
///
/// A node holds at most one of these: §10.3 is explicit that "devA will never consider
/// more than one SHIP node as devZ", and accepting a new request untrusts the previous
/// unit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PairedUnit {
    /// The unit's SHIP ID, from the request's `trustId`. §10.4 calls this the primary
    /// identifier throughout a node's lifetime.
    pub ship_id: String,
    /// The fingerprint of the certificate it will present, from `trustPar`.
    pub fingerprint: Fingerprint,
    /// The curve of its key, from `trustCurve`.
    pub curve: String,
    /// Its SKI, once a connection has proved one. §10.2 makes recording it optional and
    /// explicitly not part of the authentication; it is kept so that everything else in
    /// this crate — routing, the redial schedule, a user interface — can name the unit
    /// the way it names every other peer.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ski: Option<Ski>,
    /// When the request was accepted, ISO 8601, where the device had a clock to read.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub paired_at: Option<String>,
}

impl PairedUnit {
    /// The unit an accepted request describes.
    #[cfg(feature = "pairing")]
    #[cfg_attr(docsrs, doc(cfg(feature = "pairing")))]
    pub fn from_request(request: &crate::ship::pairing::PairingRequest) -> Self {
        Self {
            ship_id: request.trust_id.clone(),
            fingerprint: request.trust_par,
            curve: request.trust_curve.clone(),
            ski: None,
            paired_at: None,
        }
    }

    /// A unit trusted by SHIP ID and certificate fingerprint.
    ///
    /// [`from_request`](Self::from_request) is the ordinary source; this is for a store
    /// restored from disk, and for a device whose pairing was configured some other way.
    pub fn new(
        ship_id: impl Into<String>,
        fingerprint: Fingerprint,
        curve: impl Into<String>,
    ) -> Self {
        Self {
            ship_id: ship_id.into(),
            fingerprint,
            curve: curve.into(),
            ski: None,
            paired_at: None,
        }
    }

    /// Records when the pairing was accepted.
    #[must_use]
    pub fn at_time(mut self, timestamp: impl Into<String>) -> Self {
        self.paired_at = Some(timestamp.into());
        self
    }
}

/// The peers a node is willing to exchange data with.
///
/// SHIP's whole trust model is this list. A peer whose SKI is not in it may still connect
/// and complete TLS — it has to, so that its SKI can be shown to a user — but the SHIP
/// handshake will hold it in the pending state rather than open the data phase. **Adding
/// the SKI while that handshake is waiting is what lets it through**: every change to the
/// store wakes the handshakes that are pending on it, so a user approving a SKI off a
/// screen is a pairing completed, not a reconnection owed.
///
/// The store is shared: clones refer to the same list, so the one a [`Node`] was built
/// with and the one an application keeps a handle on are one store.
///
/// # Persistence
///
/// SHIP §12.2.2 is emphatic that this list should survive a restart: "to avoid
/// re-verification by user interaction, persistent storage of mandatory key material is
/// STRONGLY RECOMMENDED". Nothing here writes to disk — where a device keeps its state is
/// the device's business — but [`to_json`](Self::to_json) and
/// [`from_json`](Self::from_json) are the two calls that make saving it a one-liner:
///
/// ```no_run
/// use eebus::runtime::TrustStore;
///
/// let store = TrustStore::from_json(&std::fs::read_to_string("trust.json")?)?;
/// // … a user approves another box …
/// std::fs::write("trust.json", store.to_json()?)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Resetting
///
/// SHIP §12.2.2 also states a hard requirement: "a SHIP node SHALL offer a possibility to
/// delete all stored foreign public keys (e.g. via factory reset)". That is
/// [`forget_all`](Self::forget_all), and it is what an "EEBUS reset" on a device's user
/// interface has to reach.
#[derive(Clone, Debug)]
pub struct TrustStore {
    inner: Arc<Mutex<Trusted>>,
    /// Bumped on every change, so a handshake waiting for a decision is woken by it.
    changed: watch::Sender<u64>,
}

/// What a store holds: peers trusted by SKI, and the one unit trusted by certificate.
#[derive(Debug, Default)]
struct Trusted {
    peers: BTreeMap<Ski, TrustedPeer>,
    /// §10.3: at most one, ever.
    unit: Option<PairedUnit>,
}

/// The persisted form of a [`TrustStore`].
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct StoredTrust {
    peers: Vec<TrustedPeer>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    unit: Option<PairedUnit>,
}

impl Default for TrustStore {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            changed: watch::Sender::new(0),
        }
    }
}

impl TrustStore {
    /// An empty store: nothing is trusted yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that already trusts these peers, as loaded from disk at start-up.
    pub fn with(skis: impl IntoIterator<Item = Ski>) -> Self {
        let store = Self::new();
        for ski in skis {
            store.trust(ski);
        }
        store
    }

    /// A store holding these records, as loaded from disk at start-up.
    pub fn restored(peers: impl IntoIterator<Item = TrustedPeer>) -> Self {
        let store = Self::new();
        for peer in peers {
            store.remember(peer);
        }
        store
    }

    /// Adds a peer, which is what a user approving a SKI amounts to.
    ///
    /// A handshake held in the pending state for this SKI proceeds at once.
    pub fn trust(&self, ski: Ski) {
        self.remember(TrustedPeer::new(ski));
    }

    /// Adds a peer, with whatever else is known about it.
    ///
    /// Replaces an existing record for the same SKI: re-approving a peer is a fresh
    /// decision, and the details that come with it are the current ones.
    pub fn remember(&self, peer: TrustedPeer) {
        if let Ok(mut trusted) = self.inner.lock() {
            trusted.peers.insert(peer.ski, peer);
        }
        self.touch();
    }

    /// Trusts a control unit's certificate: the outcome of an accepted pairing request.
    ///
    /// §10.3: a node considers at most one unit at a time, so this replaces any previous
    /// one — and untrusts the SKI that unit was reached under, which is what "remove SHIP
    /// trust of past devZ" means in a store keyed by SKI. Returns the unit it displaced.
    pub fn trust_unit(&self, unit: PairedUnit) -> Option<PairedUnit> {
        let displaced = match self.inner.lock() {
            Ok(mut trusted) => {
                // A unit restored from disk already knows the SKI it presents, so the
                // SKI-keyed half of the store is filled in here rather than waiting for
                // the connection that proves it again.
                let known = unit.ski.map(|ski| {
                    TrustedPeer::new(ski)
                        .with_ship_id(unit.ship_id.clone())
                        .with_fingerprint(unit.fingerprint)
                });
                let previous = trusted.unit.replace(unit);
                if let Some(ski) = previous.as_ref().and_then(|p| p.ski) {
                    trusted.peers.remove(&ski);
                }
                if let Some(known) = known {
                    trusted.peers.insert(known.ski, known);
                }
                previous
            }
            Err(_) => None,
        };
        self.touch();
        displaced
    }

    /// The control unit trusted through the Pairing Service, if there is one.
    pub fn unit(&self) -> Option<PairedUnit> {
        self.inner.lock().ok().and_then(|t| t.unit.clone())
    }

    /// Forgets the paired control unit, and the SKI it was reached under.
    ///
    /// §10.4: removing this trust is what reactivates the processing of pairing
    /// requests, which is why it is a call of its own rather than a `forget` of a SKI.
    pub fn forget_unit(&self) -> Option<PairedUnit> {
        let unit = match self.inner.lock() {
            Ok(mut trusted) => {
                let unit = trusted.unit.take();
                if let Some(ski) = unit.as_ref().and_then(|p| p.ski) {
                    trusted.peers.remove(&ski);
                }
                unit
            }
            Err(_) => None,
        };
        self.touch();
        unit
    }

    /// Whether a certificate is the paired unit's (§10.2).
    ///
    /// This is the whole of the Pairing Service's authentication: the fingerprint of the
    /// certificate presented at TLS against the one the request announced.
    pub fn is_certificate_trusted(&self, fingerprint: &Fingerprint) -> bool {
        self.inner
            .lock()
            .map(|trusted| {
                trusted
                    .unit
                    .as_ref()
                    .is_some_and(|u| &u.fingerprint == fingerprint)
            })
            .unwrap_or(false)
    }

    /// Records the SKI a paired unit turned out to present (§10.2, note).
    ///
    /// Called once its first connection has proved one, so that everything keyed by SKI —
    /// routing, the redial schedule, what a user is shown — can name it. Nothing about
    /// the trust decision changes: the fingerprint is what admitted it.
    pub(crate) fn observe_unit_ski(&self, fingerprint: &Fingerprint, ski: Ski) {
        let changed = match self.inner.lock() {
            Ok(mut trusted) => {
                let Some(unit) = trusted
                    .unit
                    .as_mut()
                    .filter(|u| &u.fingerprint == fingerprint)
                else {
                    return;
                };
                if unit.ski == Some(ski) {
                    return;
                }
                let previous = unit.ski.replace(ski);
                let ship_id = unit.ship_id.clone();
                if let Some(previous) = previous {
                    trusted.peers.remove(&previous);
                }
                trusted.peers.insert(
                    ski,
                    TrustedPeer::new(ski)
                        .with_ship_id(ship_id)
                        .with_fingerprint(*fingerprint),
                );
                true
            }
            Err(_) => false,
        };
        if changed {
            self.touch();
        }
    }

    /// Removes a peer. Existing connections are not torn down by this.
    ///
    /// A peer that was trusted through the Pairing Service is untrusted *there* as well:
    /// §10.4 makes removing the trust the thing that reactivates the pairing process, and
    /// leaving the certificate trusted would let the unit straight back in.
    pub fn forget(&self, ski: &Ski) {
        if let Ok(mut trusted) = self.inner.lock() {
            trusted.peers.remove(ski);
            if trusted.unit.as_ref().and_then(|u| u.ski) == Some(*ski) {
                trusted.unit = None;
            }
        }
        self.touch();
    }

    /// Forgets every peer: the "delete all stored foreign public keys" of SHIP §12.2.2.
    ///
    /// This is half of an EEBUS reset. The other half is the device's own — restoring the
    /// original certificate and private key, so that the SKI printed on the label is the
    /// one the node presents again (SHIP §12.1.1) — and this crate cannot do that for it,
    /// because it never chose where the identity was stored.
    ///
    /// Returns how many were forgotten, for the log entry a user-initiated reset deserves.
    /// Existing connections are not torn down: SHIP re-checks trust on the next handshake,
    /// and dropping a live limitation exchange mid-reset would be its own hazard. A device
    /// that means to disconnect as well should say so through its [`Hub`](super::Hub).
    pub fn forget_all(&self) -> usize {
        let count = match self.inner.lock() {
            Ok(mut trusted) => {
                let count = trusted.peers.len() + usize::from(trusted.unit.is_some());
                trusted.peers.clear();
                trusted.unit = None;
                count
            }
            Err(_) => 0,
        };
        self.touch();
        count
    }

    /// Awards a peer the second-factor trust it earned by proving it holds this node's
    /// PIN (§12.5), and reports how much.
    ///
    /// The rule is once-only and the store is the only thing that can apply it: "The first
    /// communication partner after factory default that sends the SHIP node PIN MAY gain a
    /// higher second factor trust level of '32' […] In SHIP, it is not possible that two
    /// SHIP nodes may gain a second factor trust of '32' with the SHIP node PIN. Any SHIP
    /// node that sends the PIN afterwards SHALL only get a second factor trust of '16'."
    ///
    /// So the first caller since the store was empty of any such award gets
    /// [`TrustLevel::SECOND_FACTOR_PIN_FIRST`] and every later one
    /// [`TrustLevel::SECOND_FACTOR_PIN_LATER`] — and the "after factory reset" half is
    /// [`forget_all`](Self::forget_all), which is what an EEBUS reset calls.
    ///
    /// A peer that is not in the store gains nothing: a second factor is trust *added* to a
    /// key, not a way in on its own.
    ///
    /// Answer [`Event::PeerPinVerified`](crate::ship::Event::PeerPinVerified) with this.
    pub fn award_pin_trust(&self, ski: &Ski) -> Option<TrustLevel> {
        let mut held = self.inner.lock().ok()?;
        let taken = held
            .peers
            .values()
            .any(|peer| peer.trust.second_factor >= TrustLevel::SECOND_FACTOR_PIN_FIRST);
        let awarded = if taken {
            TrustLevel::SECOND_FACTOR_PIN_LATER
        } else {
            TrustLevel::SECOND_FACTOR_PIN_FIRST
        };
        let peer = held.peers.get_mut(ski)?;
        peer.trust = peer
            .trust
            .merged(TrustLevel::UNTRUSTED.with_second_factor(awarded));
        let level = peer.trust;
        drop(held);
        self.touch();
        Some(level)
    }

    /// Whether a peer is trusted.
    pub fn is_trusted(&self, ski: &Ski) -> bool {
        self.inner
            .lock()
            .map(|trusted| trusted.peers.contains_key(ski))
            .unwrap_or(false)
    }

    /// What is known about one trusted peer.
    pub fn get(&self, ski: &Ski) -> Option<TrustedPeer> {
        self.inner
            .lock()
            .ok()
            .and_then(|trusted| trusted.peers.get(ski).cloned())
    }

    /// How many peers are trusted.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|trusted| trusted.peers.len())
            .unwrap_or(0)
    }

    /// Whether nothing is trusted — a node fresh out of the box, or freshly reset.
    pub fn is_empty(&self) -> bool {
        self.len() == 0 && self.unit().is_none()
    }

    /// Every trusted SKI.
    pub fn all(&self) -> Vec<Ski> {
        self.inner
            .lock()
            .map(|trusted| trusted.peers.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Every record, in SKI order, for persisting or for showing a user.
    pub fn peers(&self) -> Vec<TrustedPeer> {
        self.inner
            .lock()
            .map(|trusted| trusted.peers.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The store as JSON, for writing to disk.
    ///
    /// Both halves: the peers trusted by SKI, and the control unit trusted by
    /// certificate. The Pairing Service §10.4 is explicit that it needs no store of its
    /// own — "a SHIP implementation is responsible to store trust from a shippairing
    /// request like it stores trust from classic SHIP trust mechanisms" — so this is the
    /// one file a device writes.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&StoredTrust {
            peers: self.peers(),
            unit: self.unit(),
        })
    }

    /// A store read back from [`to_json`](Self::to_json).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let stored: StoredTrust = serde_json::from_str(json)?;
        let store = Self::restored(stored.peers);
        if let Some(unit) = stored.unit {
            store.trust_unit(unit);
        }
        Ok(store)
    }

    /// A receiver that is woken by every change to the store.
    ///
    /// This is how a SHIP handshake held in the pending state learns that the user has
    /// decided: it waits on this rather than polling, and re-reads the store when it
    /// wakes. The value is a change counter and carries no meaning of its own.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Wakes everything waiting on [`watch`](Self::watch).
    ///
    /// Every mutating method calls it; a [`Node`] calls it too when a pairing is refused,
    /// which is a decision about a peer that leaves the store itself unchanged.
    pub(crate) fn touch(&self) {
        self.changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

/// The largest SHIP message this node will accept.
///
/// SHIP fixes no maximum, and neither does SPINE. But a peer on the same subnet can send
/// whatever it likes once TLS is up, and the default in the WebSocket layer is 64 MiB —
/// enough to exhaust a heat-pump controller by accident. The largest legitimate message
/// is a detailed-discovery reply from a device with a great many entities, which is a few
/// tens of kilobytes; a megabyte is generous.
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

/// The WebSocket configuration SHIP asks for.
///
/// Bounded on both sides, and unmasked frames refused: RFC 6455 requires a client to mask,
/// and accepting unmasked frames from one is accepting a peer that is not speaking
/// WebSocket properly.
fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_SIZE))
        .max_frame_size(Some(MAX_MESSAGE_SIZE))
        .accept_unmasked_frames(false)
}

/// A SHIP node: an identity, a trust store, and the sockets that use them.
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use eebus::cert::{self, CertParams};
/// use eebus::runtime::{Node, TrustStore};
/// use eebus::tls::ShipTls;
///
/// let identity = cert::self_signed(CertParams::new("i:46925_u:ControlBox-1"))?;
/// let node = Node::new("i:46925_u:ControlBox-1", ShipTls::new(identity), TrustStore::new());
///
/// // A device the installer has approved.
/// node.trust_store().trust("5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse()?);
/// let mut connection = node.connect("192.0.2.10:4712").await?;
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct Node {
    ship_id: String,
    tls: ShipTls,
    trust: TrustStore,
    handshake: HandshakeConfig,
    /// Peers whose pending pairing a user has said no to, consumed by the handshake that
    /// was waiting. A refusal is about *this* attempt: the peer may ask again, and the
    /// user may answer differently.
    refusals: Mutex<Vec<Ski>>,
    /// Peers whose handshake is holding, waiting on a trust decision right now.
    ///
    /// One entry per waiting *handshake*, so a peer that dialled twice appears twice —
    /// which is the truth an installer's screen should show, and what makes the entry
    /// disappear when its own handshake ends rather than when some other one does.
    pending: Mutex<Vec<PendingPeer>>,
    /// Bumped whenever `pending` changes, so a user interface can wait rather than poll.
    pending_changed: watch::Sender<u64>,
}

/// A peer that has proved who it is and is waiting for somebody to approve it.
///
/// It has completed TLS — the SKI and the fingerprint are *proved*, not claimed — and has
/// been told `hello: pending` (SHIP §13.4.4.1). It stays connected for as long as its own
/// SHIP timers allow, so there is a window in which a person can be shown these forty hex
/// digits and asked whether they match the label on the box in front of them. Field
/// reports make that exchange the most common §14a commissioning failure, and a box that
/// cannot *display* the SKI cannot take part in it at all.
///
/// Answer with [`TrustStore::trust`] or [`Node::refuse_pairing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingPeer {
    /// The Subject Key Identifier of the peer's public key: what SHIP §12.2 trusts, and
    /// what is printed on the box.
    pub ski: Ski,
    /// The SHA-256 fingerprint of the whole certificate.
    ///
    /// The other identity SHIP knows a node by (Pairing Service §10.2), and the one a QR
    /// code carries as `FPH256`. A peer may be admitted on either.
    pub fingerprint: Fingerprint,
}

/// Keeps a [`PendingPeer`] listed for as long as its handshake is waiting.
///
/// Dropped when the handshake ends however it ends — approved, refused, timed out, socket
/// gone, or the caller's future cancelled — because a list of peers waiting for a decision
/// that nobody is waiting for any more is worse than no list.
pub(crate) struct PendingGuard<'a> {
    node: &'a Node,
    ski: Ski,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.node.forget_pending(&self.ski);
    }
}

impl Node {
    /// A node with this SHIP ID, TLS identity and trust store.
    pub fn new(ship_id: impl Into<String>, tls: ShipTls, trust: TrustStore) -> Self {
        let ship_id = ship_id.into();
        Self {
            handshake: HandshakeConfig {
                ship_id: Some(ship_id.clone()),
                ..HandshakeConfig::default()
            },
            ship_id,
            tls,
            trust,
            refusals: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            pending_changed: watch::Sender::new(0),
        }
    }

    /// Declares this node's key material, so it takes part in certificate updates.
    ///
    /// SHIP §12.1.3: without it the node behaves as a 1.0.1 node — its certificate can
    /// only be replaced by re-establishing trust by hand. With it, the `updateCounter`
    /// rides in every `hello` and a renewal reaches peers over the connections the old
    /// certificate still secures.
    #[must_use]
    pub fn key_material(mut self, keys: crate::ship::OwnKeys) -> Self {
        self.handshake.key_material = Some(keys);
        self
    }

    /// Overrides the handshake timers, for a device with unusual constraints.
    pub fn handshake_config(mut self, config: HandshakeConfig) -> Self {
        self.handshake = HandshakeConfig {
            ship_id: config.ship_id.or_else(|| Some(self.ship_id.clone())),
            ..config
        };
        self
    }

    /// This node's SKI, which is what a peer is asked to trust.
    pub fn ski(&self) -> Ski {
        self.tls.ski()
    }

    /// This node's certificate fingerprint.
    ///
    /// The identity the SHIP Pairing Service uses (§6.2): what goes on this device's QR
    /// code as `FPH256`, and what a control unit puts in the `forPar` of a request
    /// addressed to it.
    pub fn fingerprint(&self) -> crate::ship::Fingerprint {
        self.tls.fingerprint()
    }

    /// This node's SHIP ID.
    pub fn ship_id(&self) -> &str {
        &self.ship_id
    }

    /// The trust store, for approving a peer a user has just confirmed.
    pub fn trust_store(&self) -> &TrustStore {
        &self.trust
    }

    /// Refuses a peer whose handshake is waiting for a decision.
    ///
    /// The other answer to a pending pairing. Approving is
    /// [`trust_store().trust(ski)`](TrustStore::trust); this tells the waiting handshake to
    /// abort with `hello: aborted` instead, so the peer learns it was turned down rather
    /// than timing out. It says nothing about the future — the peer may ask again — and
    /// nothing at all about a peer that is not currently waiting.
    pub fn refuse_pairing(&self, ski: Ski) {
        if let Ok(mut refusals) = self.refusals.lock()
            && !refusals.contains(&ski)
        {
            refusals.push(ski);
        }
        self.trust.touch();
    }

    /// The peers whose handshake is waiting on a trust decision right now.
    ///
    /// **This is what a box shows an installer.** An unapproved peer completes TLS, so its
    /// SKI is proved rather than claimed, and is then held in the SHIP pending state —
    /// which exists precisely so that a person can compare it with the label on the
    /// device. Without a way to read it out, the SKI has to come off the other box
    /// instead, and that exchange is the most common §14a commissioning failure there is.
    ///
    /// Every peer here is also reported once through the
    /// [`TrustReporter`](super::TrustReporter) a call to
    /// [`accept_reporting`](Self::accept_reporting) or
    /// [`connect_reporting`](Self::connect_reporting) passed, if it passed one. This is
    /// the same thing for a caller that would rather ask than be told — a plain
    /// [`accept`](Self::accept) fills it in too.
    ///
    /// One entry per waiting handshake. An entry disappears when its handshake ends,
    /// whether that is an approval, a refusal, a timeout or a dropped socket.
    ///
    /// ```no_run
    /// # async fn example(node: &eebus::runtime::Node) {
    /// let mut changes = node.watch_pending();
    /// loop {
    ///     if changes.changed().await.is_err() {
    ///         break;
    ///     }
    ///     for peer in node.pending_peers() {
    ///         println!("approve {}? (fingerprint {})", peer.ski, peer.fingerprint);
    ///     }
    /// }
    /// # }
    /// ```
    pub fn pending_peers(&self) -> Vec<PendingPeer> {
        self.pending
            .lock()
            .map(|pending| pending.clone())
            .unwrap_or_default()
    }

    /// Wakes whenever [`pending_peers`](Self::pending_peers) changes.
    ///
    /// The receiver carries a counter that means nothing on its own; what it is for is
    /// `changed().await`. It is marked as seen when handed out, so the first wake is a
    /// real change rather than the state at the time of subscribing — call
    /// [`pending_peers`](Self::pending_peers) once after subscribing if a peer may already
    /// be waiting.
    pub fn watch_pending(&self) -> watch::Receiver<u64> {
        self.pending_changed.subscribe()
    }

    /// Lists a peer as waiting, and keeps it listed until the guard is dropped.
    pub(crate) fn note_pending(&self, peer: PendingPeer) -> PendingGuard<'_> {
        let ski = peer.ski;
        if let Ok(mut pending) = self.pending.lock() {
            // Bounded by the connections the caller is running: a `Hub` caps those with
            // `MAX_PENDING_TRUST`, and a consumer driving `accept` itself decides how many
            // handshakes it starts. Nothing a peer sends adds an entry on its own.
            pending.push(peer);
        }
        self.pending_changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        PendingGuard { node: self, ski }
    }

    /// Drops one waiting entry for `ski` — the one whose handshake has ended.
    fn forget_pending(&self, ski: &Ski) {
        if let Ok(mut pending) = self.pending.lock()
            && let Some(index) = pending.iter().position(|peer| &peer.ski == ski)
        {
            pending.remove(index);
        }
        self.pending_changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Takes a refusal for `ski`, if one was recorded.
    pub(crate) fn take_refusal(&self, ski: &Ski) -> bool {
        let Ok(mut refusals) = self.refusals.lock() else {
            return false;
        };
        let Some(index) = refusals.iter().position(|s| s == ski) else {
            return false;
        };
        refusals.remove(index);
        true
    }

    /// The part of an "EEBUS reset" this crate can perform: forgetting every peer.
    ///
    /// SHIP §12.2.2 requires a node to offer this — "at least the SHIP node SHALL offer a
    /// possibility to delete all stored foreign public keys" — and the installation
    /// process test specification treats it as a prerequisite, because an installer
    /// removing a control box has no other way to undo the pairing.
    ///
    /// Returns how many peers were forgotten. What it deliberately does *not* do:
    ///
    /// * **Restore the node's own identity.** SHIP §12.1.1 asks that a factory reset bring
    ///   back the original certificate and key, so that the SKI printed on the label is
    ///   the one presented again. Only the device knows where that was stored.
    /// * **Delete anything from disk.** [`TrustStore::to_json`] is what persists the
    ///   store; writing the now-empty store back out is the caller's step, and doing it
    ///   for them would mean guessing at a path.
    /// * **Close open connections.** Trust is re-checked on the next handshake. Tearing
    ///   down a live limitation exchange in the middle of a reset would be its own
    ///   hazard; [`Hub::shutdown`](super::Hub::shutdown) is there for a device that means
    ///   to.
    pub fn eebus_reset(&self) -> usize {
        self.trust.forget_all()
    }

    /// Dials a peer and runs the whole stack: TCP, TLS, WebSocket, SHIP handshake.
    ///
    /// Returns once the connection is ready for SPINE data. A peer whose SKI is not
    /// trusted is held in the pending state, which is the specified behaviour — the peer's
    /// user may be approving this node at the same time — and an approval added to this
    /// node's [`TrustStore`] meanwhile lets it through; otherwise it ends when the SHIP
    /// timers run out.
    pub async fn connect(
        &self,
        address: impl ToSocketAddrs,
    ) -> Result<ShipConnection, ConnectionError> {
        self.connect_reporting(address, None).await
    }

    /// [`connect`](Self::connect), telling `report` when the peer turns out to be one
    /// this node has not approved.
    ///
    /// The dialling half of [`accept_reporting`](Self::accept_reporting); see there.
    pub async fn connect_reporting(
        &self,
        address: impl ToSocketAddrs,
        report: Option<TrustReporter>,
    ) -> Result<ShipConnection, ConnectionError> {
        let stream = TcpStream::connect(address).await?;
        // SHIP §10.2: Nagle would delay the small control messages the handshake is made
        // of behind an acknowledgement that has nothing to do with them.
        stream.set_nodelay(true)?;
        self.connect_over_reporting(stream, report).await
    }

    /// Runs the stack over a socket that is already open.
    ///
    /// Useful for a transport this crate does not provide — a test harness, a tunnel, a
    /// serial bridge — and it is what [`connect`](Self::connect) does once it has a
    /// socket.
    pub async fn connect_over(&self, stream: TcpStream) -> Result<ShipConnection, ConnectionError> {
        self.connect_over_reporting(stream, None).await
    }

    /// [`connect_over`](Self::connect_over), telling `report` when the peer turns out to
    /// be one this node has not approved.
    pub async fn connect_over_reporting(
        &self,
        stream: TcpStream,
        report: Option<TrustReporter>,
    ) -> Result<ShipConnection, ConnectionError> {
        let connector = TlsConnector::from(self.tls.client_config()?);

        // SHIP §9.5 requires SNI to be sent; a SHIP server is required to ignore it, so
        // any syntactically valid name serves.
        let server_name =
            rustls::pki_types::ServerName::try_from("ship.local").expect("a valid DNS name");
        let stream = connector.connect(server_name, stream).await?;
        // The certificate `rustls` kept is the peer's identity: SHIP trusts a Subject Key
        // Identifier, not a name.
        let peer = PeerIdentity::of(stream.get_ref().1)?;

        let mut request = alloc::format!("wss://ship.local{DEFAULT_PATH}")
            .into_client_request()
            .map_err(ConnectionError::WebSocket)?;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );

        let (socket, response) = tokio_tungstenite::client_async_with_config(
            request,
            tokio_rustls::TlsStream::Client(stream),
            Some(socket_config()),
        )
        .await?;
        check_subprotocol(response.headers().get("Sec-WebSocket-Protocol"))?;

        run_handshake(
            socket,
            Role::Client,
            self.handshake.clone(),
            self.trust_for(&peer),
            peer,
            self,
            report,
        )
        .await
    }

    /// Accepts one peer on an open socket.
    ///
    /// A peer this node has not approved is held in the SHIP pending state — which is what
    /// lets a user read its SKI off a screen — and an approval added to the
    /// [`TrustStore`] meanwhile completes the handshake; a
    /// [`refuse_pairing`](Self::refuse_pairing) ends it with `hello: aborted`.
    pub async fn accept(&self, stream: TcpStream) -> Result<ShipConnection, ConnectionError> {
        self.accept_reporting(stream, None).await
    }

    /// [`accept`](Self::accept), telling `report` when the peer is one this node has not
    /// approved and is now waiting on a decision.
    ///
    /// The peer has completed TLS by then, so the [`PendingPeer`] handed to `report` is
    /// proved rather than claimed, and the handshake goes on holding the connection while
    /// somebody decides. This is the hook a consumer needs to **show** an installer the
    /// SKI of the box that is asking; without it the only way to learn the number is to
    /// read it off the other device, which is the §14a commissioning step that most often
    /// goes wrong.
    ///
    /// The callback runs on this task, so it must not block — send it on a channel. A
    /// caller that would rather poll can use [`pending_peers`](Self::pending_peers)
    /// instead and pass [`None`] here; both are filled in either way.
    ///
    /// ```no_run
    /// # async fn example(node: &eebus::runtime::Node, stream: tokio::net::TcpStream)
    /// # -> Result<(), Box<dyn std::error::Error>> {
    /// let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    /// let connection = node
    ///     .accept_reporting(stream, Some(Box::new(move |peer| { let _ = tx.send(peer); })))
    ///     .await?;
    /// # let _ = connection;
    /// # Ok(()) }
    /// ```
    ///
    /// [`Hub`](super::Hub) uses this to raise
    /// [`HubEvent::TrustRequested`](super::HubEvent::TrustRequested); a driver that owns
    /// its own [`Engine`](crate::spine::Engine) — because it has to be testable without a
    /// socket — calls it directly.
    pub async fn accept_reporting(
        &self,
        stream: TcpStream,
        report: Option<TrustReporter>,
    ) -> Result<ShipConnection, ConnectionError> {
        stream.set_nodelay(true)?;
        let acceptor = TlsAcceptor::from(self.tls.server_config()?);
        let stream = acceptor.accept(stream).await?;
        let peer = PeerIdentity::of(stream.get_ref().1)?;

        let socket = tokio_tungstenite::accept_hdr_async_with_config(
            tokio_rustls::TlsStream::Server(stream),
            offer_ship,
            Some(socket_config()),
        )
        .await?;

        run_handshake(
            socket,
            Role::Server,
            self.handshake.clone(),
            self.trust_for(&peer),
            peer,
            self,
            report,
        )
        .await
    }

    /// Binds a listener, for a node that accepts connections.
    ///
    /// Hand each accepted socket to [`accept`](Self::accept), or let a
    /// [`Hub`](super::Hub) do both with [`Hub::listen`](super::Hub::listen). Whether to
    /// accept a second connection from a peer already connected is a decision SHIP
    /// §12.2.3 settles by SKI comparison, and belongs to the code owning the connection
    /// table.
    pub async fn listen(
        &self,
        address: impl ToSocketAddrs,
    ) -> Result<TcpListener, ConnectionError> {
        Ok(TcpListener::bind(address).await?)
    }

    /// Whether a peer that has just completed TLS is one this node will talk to, and at
    /// what level.
    ///
    /// Two ways in, and they are equal in standing. The SKI is classic SHIP §12.2. The
    /// certificate fingerprint is the Pairing Service §10.2, which says that matching it
    /// "SHALL be seen like a successful trust in an SKI from the perspective of the SHIP
    /// specification" — so a control unit paired that way reaches the data phase without
    /// anybody having compared forty hex digits. Matching it also records the SKI, which
    /// is what every table in this crate is keyed by.
    ///
    /// The **level** comes from what the store recorded (§12.3.2, Table 10). A peer added
    /// with [`TrustStore::trust`] is `user verified`, because that is what approving a SKI
    /// off a screen is; a control unit trusted through the Pairing Service is a
    /// commissioning mechanism and is recorded as such. It matters at the next phase but
    /// one: §12.5 forbids sending this node's PIN to a peer below user trust 32, and a
    /// stack whose trust is a boolean has nothing to check that against.
    pub(crate) fn trust_for(&self, peer: &PeerIdentity) -> Trust {
        if let Some(known) = self.trust.get(&peer.ski) {
            return Trust::Trusted(known.trust);
        }
        if self.trust.is_certificate_trusted(&peer.fingerprint) {
            self.trust.observe_unit_ski(&peer.fingerprint, peer.ski);
            // Pairing Service §10.2 admits the unit on a fingerprint a user scanned off a
            // QR code, which Table 10 calls `commissioned`.
            return Trust::Trusted(TrustLevel::commissioned(COMMISSIONING_TRUST));
        }
        Trust::Pending
    }
}

/// Who the peer on a finished TLS connection turned out to be.
///
/// Both identities SHIP knows a node by: the Subject Key Identifier of its public key,
/// and the SHA-256 fingerprint of the whole certificate. They are carried together
/// because a node may have been trusted by either — a user approving a SKI, or a Pairing
/// Service request naming a fingerprint — and the handshake has to be able to ask about
/// both without going back to `rustls`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PeerIdentity {
    pub(crate) ski: Ski,
    pub(crate) fingerprint: Fingerprint,
}

impl PeerIdentity {
    /// Reads both identities off a completed TLS connection.
    fn of(state: &rustls::CommonState) -> Result<Self, ConnectionError> {
        Ok(Self {
            ski: peer_ski(state).ok_or(ConnectionError::NoPeerIdentity)?,
            fingerprint: peer_fingerprint(state).ok_or(ConnectionError::NoPeerIdentity)?,
        })
    }
}

/// Accepts the upgrade only if the client asked for SHIP.
///
/// SHIP §10.1 requires the subprotocol on both the request and the response. A peer that
/// omits it is speaking some other protocol on the same port, and answering it as if it
/// were SHIP would produce a confusing failure a few frames later.
#[allow(clippy::result_large_err)] // The signature is tungstenite's, not ours to choose.
fn offer_ship(
    request: &ServerRequest,
    mut response: ServerResponse,
) -> Result<ServerResponse, ErrorResponse> {
    let asked = request
        .headers()
        .get_all("Sec-WebSocket-Protocol")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.split(',').any(|p| p.trim() == SUBPROTOCOL));

    if !asked {
        let mut refusal = ErrorResponse::new(Some(alloc::format!(
            "this endpoint speaks `{SUBPROTOCOL}` only"
        )));
        *refusal.status_mut() = StatusCode::BAD_REQUEST;
        return Err(refusal);
    }

    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    Ok(response)
}
