//! A node on the network: what it will accept, and whom it will dial.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use std::sync::Mutex;

use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as ServerRequest, Response as ServerResponse,
};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::ship::{DEFAULT_PATH, HandshakeConfig, Role, SUBPROTOCOL, Ski, Trust};
use crate::tls::{ShipTls, peer_ski};

use super::connection::{ConnectionError, ShipConnection, check_subprotocol, run_handshake};

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
        }
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
}

/// The peers a node is willing to exchange data with.
///
/// SHIP's whole trust model is this list. A peer whose SKI is not in it may still connect
/// and complete TLS — it has to, so that its SKI can be shown to a user — but the SHIP
/// handshake will hold it in the pending state rather than open the data phase.
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
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    trusted: Arc<Mutex<BTreeMap<Ski, TrustedPeer>>>,
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
    pub fn trust(&self, ski: Ski) {
        self.remember(TrustedPeer::new(ski));
    }

    /// Adds a peer, with whatever else is known about it.
    ///
    /// Replaces an existing record for the same SKI: re-approving a peer is a fresh
    /// decision, and the details that come with it are the current ones.
    pub fn remember(&self, peer: TrustedPeer) {
        if let Ok(mut trusted) = self.trusted.lock() {
            trusted.insert(peer.ski, peer);
        }
    }

    /// Removes a peer. Existing connections are not torn down by this.
    pub fn forget(&self, ski: &Ski) {
        if let Ok(mut trusted) = self.trusted.lock() {
            trusted.remove(ski);
        }
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
        match self.trusted.lock() {
            Ok(mut trusted) => {
                let count = trusted.len();
                trusted.clear();
                count
            }
            Err(_) => 0,
        }
    }

    /// Whether a peer is trusted.
    pub fn is_trusted(&self, ski: &Ski) -> bool {
        self.trusted
            .lock()
            .map(|trusted| trusted.contains_key(ski))
            .unwrap_or(false)
    }

    /// What is known about one trusted peer.
    pub fn get(&self, ski: &Ski) -> Option<TrustedPeer> {
        self.trusted
            .lock()
            .ok()
            .and_then(|trusted| trusted.get(ski).cloned())
    }

    /// How many peers are trusted.
    pub fn len(&self) -> usize {
        self.trusted
            .lock()
            .map(|trusted| trusted.len())
            .unwrap_or(0)
    }

    /// Whether nothing is trusted — a node fresh out of the box, or freshly reset.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every trusted SKI.
    pub fn all(&self) -> alloc::vec::Vec<Ski> {
        self.trusted
            .lock()
            .map(|trusted| trusted.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Every record, in SKI order, for persisting or for showing a user.
    pub fn peers(&self) -> alloc::vec::Vec<TrustedPeer> {
        self.trusted
            .lock()
            .map(|trusted| trusted.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The store as JSON, for writing to disk.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.peers())
    }

    /// A store read back from [`to_json`](Self::to_json).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        Ok(Self::restored(serde_json::from_str::<
            alloc::vec::Vec<TrustedPeer>,
        >(json)?))
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

    /// This node's SHIP ID.
    pub fn ship_id(&self) -> &str {
        &self.ship_id
    }

    /// The trust store, for approving a peer a user has just confirmed.
    pub fn trust_store(&self) -> &TrustStore {
        &self.trust
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
    /// trusted is held in the pending state until its timer runs out, which is the
    /// specified behaviour — the peer's user may be approving this node at the same time.
    pub async fn connect(
        &self,
        address: impl ToSocketAddrs,
    ) -> Result<ShipConnection, ConnectionError> {
        let stream = TcpStream::connect(address).await?;
        // SHIP §10.2: Nagle would delay the small control messages the handshake is made
        // of behind an acknowledgement that has nothing to do with them.
        stream.set_nodelay(true)?;
        self.connect_over(stream).await
    }

    /// Runs the stack over a socket that is already open.
    ///
    /// Useful for a transport this crate does not provide — a test harness, a tunnel, a
    /// serial bridge — and it is what [`connect`](Self::connect) does once it has a
    /// socket.
    pub async fn connect_over(&self, stream: TcpStream) -> Result<ShipConnection, ConnectionError> {
        let connector = TlsConnector::from(self.tls.client_config()?);

        // SHIP §9.5 requires SNI to be sent; a SHIP server is required to ignore it, so
        // any syntactically valid name serves.
        let server_name =
            rustls::pki_types::ServerName::try_from("ship.local").expect("a valid DNS name");
        let stream = connector.connect(server_name, stream).await?;
        // The certificate `rustls` kept is the peer's identity: SHIP trusts a Subject Key
        // Identifier, not a name.
        let peer = peer_ski(stream.get_ref().1).ok_or(ConnectionError::NoPeerIdentity)?;

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
        )
        .await
    }

    /// Accepts one peer on an open socket.
    pub async fn accept(&self, stream: TcpStream) -> Result<ShipConnection, ConnectionError> {
        stream.set_nodelay(true)?;
        let acceptor = TlsAcceptor::from(self.tls.server_config()?);
        let stream = acceptor.accept(stream).await?;
        let peer = peer_ski(stream.get_ref().1).ok_or(ConnectionError::NoPeerIdentity)?;

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
        )
        .await
    }

    /// Binds a listener, for a node that accepts connections.
    ///
    /// Hand each accepted socket to [`accept`](Self::accept). Whether to accept a second
    /// connection from a peer already connected is a decision SHIP §12.2.3 settles by
    /// SKI comparison, and belongs to the code owning the connection table.
    pub async fn listen(
        &self,
        address: impl ToSocketAddrs,
    ) -> Result<TcpListener, ConnectionError> {
        Ok(TcpListener::bind(address).await?)
    }

    fn trust_for(&self, peer: &Ski) -> Trust {
        if self.trust.is_trusted(peer) {
            Trust::Trusted
        } else {
            Trust::Pending
        }
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
