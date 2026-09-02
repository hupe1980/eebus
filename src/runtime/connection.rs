//! One SHIP connection: a socket, and the handshake that makes it usable.

use alloc::string::{String, ToString};
use core::time::Duration;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};

use crate::model::Datagram;
use crate::ship::{
    AbortReason, ConnectionCloseReason, Event, Handshake, HandshakeConfig, Role, SUBPROTOCOL,
    ShipMessage, Ski, Trust,
};

/// A stream that has been through TLS and the WebSocket upgrade.
type Socket = WebSocketStream<tokio_rustls::TlsStream<TcpStream>>;

/// Why a connection could not be made or kept.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// The socket failed.
    #[error("the connection failed: {0}")]
    Io(#[from] std::io::Error),
    /// The WebSocket layer failed.
    #[error("the WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    /// The peer did not agree to speak SHIP.
    #[error("the peer did not accept the `{SUBPROTOCOL}` subprotocol")]
    WrongSubprotocol,
    /// A SHIP message could not be encoded or decoded.
    #[error("a SHIP message was malformed: {0}")]
    Frame(#[from] crate::ship::FrameError),
    /// A message arrived that the handshake's current phase does not allow.
    #[error("the SHIP handshake refused a message: {0}")]
    Handshake(#[from] crate::ship::HandshakeError),
    /// The peer's certificate did not identify it.
    #[error("the peer presented no usable certificate")]
    NoPeerIdentity,
    /// The handshake ended without reaching the data phase.
    #[error("the SHIP handshake was aborted: {0:?}")]
    Aborted(AbortReason),
    /// The peer closed the connection.
    #[error("the peer closed the connection")]
    Closed,
    /// The peer sent a text frame, which SHIP §10.3 forbids.
    #[error("SHIP carries binary frames only")]
    NotBinary,
    /// The hub is already holding as many connections as it will.
    ///
    /// Not a protocol error: the peer completed the handshake and was then turned away,
    /// so that a device dialling in repeatedly cannot displace the peers already served.
    #[error("the hub is already holding as many connections as it will")]
    TooManyConnections,
    /// The TLS configuration was refused.
    #[error("{0}")]
    Tls(#[from] crate::tls::TlsError),
    /// A message could not be encoded or decoded as JSON.
    #[error("the message was malformed: {0}")]
    Json(#[from] serde_json::Error),
}

/// A SHIP connection that has reached the data phase.
///
/// Getting here means the peer completed TLS with a certificate, agreed to speak SHIP,
/// and was trusted; from here on the connection carries SPINE datagrams.
///
/// It keeps the [`Handshake`] rather than discarding it, because SHIP control traffic
/// does not stop when the handshake finishes: access methods are exchanged afterwards,
/// and either side may close the connection with a `connectionClose`. Control messages
/// are answered here, so an application working in SPINE never sees them.
#[derive(Debug)]
pub struct ShipConnection {
    socket: Socket,
    handshake: Handshake,
    clock: Instant,
    peer: Ski,
    peer_ship_id: Option<String>,
    started: Instant,
    awaiting_pong: bool,
    /// Key material the peer announced, waiting to be taken up (SHIP §12.1.3.3).
    peer_key_material: Vec<crate::ship::KeyMaterialState>,
    /// The `updateCounter` the peer's `hello` carried (§12.1.3.4).
    peer_key_counter: Option<u16>,
}

impl ShipConnection {
    /// The SKI of the peer on the other end.
    ///
    /// This is the identity everything else keys off, and it came from the certificate
    /// the peer proved it holds the key to.
    pub fn peer(&self) -> Ski {
        self.peer
    }

    /// The SHIP version this connection settled on, once the handshake reached it.
    ///
    /// A consumer that has to know whether it is speaking the certification minimum or
    /// something later can ask, instead of inferring it from behaviour. `Node`'s
    /// [`handshake_config`](super::Node::handshake_config) is where the ceiling is set;
    /// this is what came of it.
    ///
    /// ```no_run
    /// # fn example(connection: &eebus::runtime::ShipConnection) {
    /// use eebus::ship::ShipVersion;
    ///
    /// if connection.ship_version() == Some(ShipVersion::V1_0) {
    ///     // The peer is a 1.0 node; `accessMethods.id` will not be there.
    /// }
    /// # }
    /// ```
    pub fn ship_version(&self) -> Option<crate::ship::ShipVersion> {
        self.handshake.ship_version()
    }

    /// The message format both sides agreed on — `JSON-UTF8` in practice.
    pub fn message_format(&self) -> Option<&str> {
        self.handshake
            .negotiated()
            .map(|(_, format)| format.as_str())
    }

    /// The SHIP ID the peer announced, once it has.
    ///
    /// Known only after the peer answers the access-methods request, which the
    /// implementation guide §2.1 forbids waiting for — SPINE traffic must not be held up
    /// for it. Poll it, or read it after the first exchange.
    pub fn peer_ship_id(&self) -> Option<&str> {
        self.peer_ship_id.as_deref()
    }

    /// Asks the peer where else it can be reached (SHIP §13.4.6).
    ///
    /// Sent once automatically when the connection opens. Ask again after a peer changes
    /// address.
    pub async fn request_access_methods(&mut self) -> Result<(), ConnectionError> {
        self.handshake.request_access_methods();
        self.flush().await
    }

    /// Sends a SPINE datagram.
    pub async fn send(&mut self, datagram: &Datagram) -> Result<(), ConnectionError> {
        let message = crate::ship::spine_message(datagram)?;
        self.send_ship(&message).await
    }

    /// Sends a SHIP message as it stands.
    pub async fn send_ship(&mut self, message: &ShipMessage) -> Result<(), ConnectionError> {
        let bytes = message.encode()?;
        self.socket.send(Message::Binary(bytes.into())).await?;
        Ok(())
    }

    /// Waits for the next SPINE datagram.
    ///
    /// SHIP messages that are not data are handled on the way past, because an
    /// application working in SPINE has no use for them.
    pub async fn recv(&mut self) -> Result<Datagram, ConnectionError> {
        loop {
            let message = match self.next_message().await {
                Ok(message) => message,
                Err(ConnectionError::NotBinary) => {
                    let _ = self
                        .socket
                        .close(Some(CloseFrame {
                            code: 1003.into(),
                            reason: "SHIP carries binary frames only".into(),
                        }))
                        .await;
                    return Err(ConnectionError::NotBinary);
                }
                Err(error) => return Err(error),
            };
            self.handle_message(&message)?;
            self.flush().await?;
            match message {
                ShipMessage::Data(data) => {
                    if let Some(datagram) = crate::ship::spine_datagram(&data)? {
                        return Ok(datagram);
                    }
                    // A payload for another protocol; not ours to interpret.
                }
                ShipMessage::End(_) => return Err(ConnectionError::Closed),
                _ => {}
            }
        }
    }

    /// Reads one SHIP message off the socket, and sends nothing of its own.
    ///
    /// Cancel-safe, which is what lets a caller select over several connections at once —
    /// [`Hub`](crate::runtime::Hub) reads every link this way and drops the losers on
    /// every turn of its loop. Whatever it returns has to be passed to
    /// [`handle_message`](Self::handle_message); the state machine has not seen it yet.
    ///
    /// **It is not that nothing is written.** The WebSocket layer answers a ping with a
    /// pong and a close with a close while reading, so bytes can leave here — what makes
    /// that safe is that `tungstenite` "never blocks on write" for them and carries an
    /// unflushed one forward to the next read, so a dropped future leaves no half-written
    /// frame. Two things follow, and both are why this is worth stating rather than
    /// glossing: an inbound partial frame is buffered and resumed on the next call, which
    /// is a documented guarantee of `WebSocketStream`; and nothing the *caller* queued is
    /// ever fed here, which is the half [`Hub::next`](crate::runtime::Hub::next) cannot
    /// promise.
    ///
    /// A frame SHIP §10.3 forbids comes back as an error rather than being answered here,
    /// because that answer is a close frame this node composes rather than one the
    /// WebSocket layer owes. The answer is [`reject`](Self::reject).
    pub async fn next_message(&mut self) -> Result<ShipMessage, ConnectionError> {
        self.read_frame().await
    }

    /// Closes the socket with a WebSocket status code, for a frame SHIP does not allow.
    ///
    /// §10.3: a text frame is answered with 1003, "unsupported data". Telling the peer
    /// which rule it broke is the difference between a bug it can find and a connection
    /// that mysteriously drops.
    pub async fn reject(mut self, code: u16, reason: &str) {
        let _ = self
            .socket
            .close(Some(CloseFrame {
                code: code.into(),
                reason: reason.to_string().into(),
            }))
            .await;
    }

    /// Feeds a received message to the SHIP state machine.
    ///
    /// Control messages are answered from here — the answer is queued, and goes out on
    /// the next [`flush`](Self::flush) — so an application working in SPINE never sees
    /// them. Data messages are the caller's.
    pub fn handle_message(&mut self, message: &ShipMessage) -> Result<(), ConnectionError> {
        if matches!(message, ShipMessage::Control(_) | ShipMessage::End(_)) {
            self.handshake
                .handle_message(message.clone(), self.clock.elapsed())?;
            self.absorb_events();
        }
        Ok(())
    }

    /// Advances the SHIP timers, which the close handshake and its `maxTime` run on.
    pub fn handle_timeout(&mut self) -> Result<(), ConnectionError> {
        // A handshake that has finished has nothing left to time out, and says so by
        // refusing the call rather than by pretending.
        let _ = self.handshake.handle_timeout(self.clock.elapsed());
        self.absorb_events();
        Ok(())
    }

    /// When the SHIP state machine wants to be woken, if it does.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.handshake.poll_timeout()
    }

    /// Sends a WebSocket ping.
    ///
    /// SHIP §10.4 uses these two ways: as a keep-alive on an idle connection, at least
    /// every fifty seconds, and as the liveness probe the double-connection rule of
    /// §12.2.3 falls back to.
    pub async fn ping(&mut self) -> Result<(), ConnectionError> {
        self.socket.send(Message::Ping(Vec::new().into())).await?;
        self.awaiting_pong = true;
        Ok(())
    }

    /// Whether a ping is still unanswered.
    pub fn awaiting_pong(&self) -> bool {
        self.awaiting_pong
    }

    /// Whether the SHIP layer still considers the connection usable.
    pub fn is_open(&self) -> bool {
        self.handshake.is_ready_for_data()
    }

    /// Closes the connection, telling the peer why (SHIP §13.4.8).
    ///
    /// The close is a handshake of its own: `connectionClose{announce}` goes out, the
    /// peer confirms, and only then does the socket close. `max_time` is what the peer is
    /// told it has to answer in, and how long this waits before closing anyway — a peer
    /// that has already gone must not hold the caller up.
    ///
    /// Announcing it matters: without it a peer cannot tell a deliberate shutdown from a
    /// network failure, and will keep retrying against a node that has gone on purpose.
    pub async fn close(
        mut self,
        reason: ConnectionCloseReason,
        max_time: Duration,
    ) -> Result<(), ConnectionError> {
        self.handshake.close(reason, max_time, self.clock.elapsed());
        let _ = self.flush().await;

        // Wait for the confirmation, but not past the time the peer was given.
        let deadline = tokio::time::sleep(max_time);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                message = self.read_frame() => match message {
                    Ok(ShipMessage::End(_)) => break,
                    Ok(message) => {
                        let _ = self.handle_message(&message);
                    }
                    Err(_) => break,
                },
            }
        }

        let _ = self.socket.close(None).await;
        Ok(())
    }

    /// How long this connection has been up.
    pub fn age(&self) -> Duration {
        self.started.elapsed()
    }

    /// Sends everything the handshake has queued.
    ///
    /// Not cancel-safe: a dropped future may leave a control message half-written. Call
    /// it where cancellation cannot happen, which is what
    /// [`next_message`](Self::next_message) exists to make possible.
    pub async fn flush(&mut self) -> Result<(), ConnectionError> {
        while let Some(message) = self.handshake.poll_transmit() {
            let bytes = message.encode()?;
            self.socket.send(Message::Binary(bytes.into())).await?;
        }
        Ok(())
    }

    /// Takes what the handshake learned from a control message.
    fn absorb_events(&mut self) {
        while let Some(event) = self.handshake.poll_event() {
            match event {
                // SHIP 1.1.0 makes `accessMethods.id` the peer's SHIP ID, which is what a
                // node needs to dial it back in the other direction.
                Event::PeerAccessMethods(methods) => self.peer_ship_id = methods.id,
                Event::PeerKeyMaterial(state) => self.peer_key_material.push(state),
                Event::PeerKeyMaterialCounter { update_counter } => {
                    self.peer_key_counter = Some(update_counter);
                }
                _ => {}
            }
        }
    }

    /// The `updateCounter` the peer announced in its `hello`, if it announced one.
    ///
    /// A peer that announces nothing is a peer speaking SHIP 1.0.1, which has no
    /// certificate-update mechanism at all.
    pub fn peer_key_counter(&self) -> Option<u16> {
        self.peer_key_counter
    }

    /// Takes the key material the peer has announced since this was last called.
    pub fn take_peer_key_material(&mut self) -> Vec<crate::ship::KeyMaterialState> {
        core::mem::take(&mut self.peer_key_material)
    }

    /// Asks the peer for its key material (SHIP §12.1.3.4).
    ///
    /// `known_update_counter` is what this node has stored for it.
    pub async fn request_key_material(
        &mut self,
        known_update_counter: u16,
    ) -> Result<(), ConnectionError> {
        self.handshake.request_key_material(known_update_counter);
        self.flush().await
    }

    /// Announces this node's key material to the peer (SHIP §12.1.3.2).
    pub async fn send_key_material(&mut self) -> Result<(), ConnectionError> {
        self.handshake.send_key_material(self.clock.elapsed());
        self.flush().await
    }

    /// One frame off the socket, refusing what SHIP §10.3 forbids.
    async fn read_frame(&mut self) -> Result<ShipMessage, ConnectionError> {
        loop {
            let Some(frame) = self.socket.next().await else {
                return Err(ConnectionError::Closed);
            };
            match frame? {
                Message::Binary(bytes) => return Ok(ShipMessage::decode(&bytes)?),
                // §10.3 forbids it; the caller answers with 1003 through `reject`.
                Message::Text(_) => return Err(ConnectionError::NotBinary),
                Message::Close(_) => return Err(ConnectionError::Closed),
                Message::Pong(_) => self.awaiting_pong = false,
                // Tungstenite queues the pong itself while reading, and never blocks on
                // writing it — so a ping costs this loop nothing but another turn.
                Message::Ping(_) | Message::Frame(_) => {}
            }
        }
    }
}

/// Drives a [`Handshake`] over a socket until it reaches the data phase.
///
/// This is the whole of the sans-IO bargain in one function: the state machine says what
/// to send and when to wake it, and nothing here decides anything about the protocol.
pub(crate) async fn run_handshake(
    mut socket: Socket,
    role: Role,
    config: HandshakeConfig,
    trust: Trust,
    peer: Ski,
) -> Result<ShipConnection, ConnectionError> {
    let started = Instant::now();
    let clock = Instant::now();
    let mut handshake = Handshake::new(role, config, trust, Duration::ZERO);
    let peer_ship_id = None;
    // §12.1.3.4 puts the peer's key-material counter in the `hello`, which happens before
    // there is a `ShipConnection` to hold it. Kept here and handed over below, or the
    // one signal that says "your stored certificate is stale" is thrown away.
    let mut peer_key_counter = None;

    loop {
        // Everything the state machine wants to send.
        while let Some(message) = handshake.poll_transmit() {
            let bytes = message.encode()?;
            socket.send(Message::Binary(bytes.into())).await?;
        }

        for event in core::iter::from_fn(|| handshake.poll_event()) {
            match event {
                Event::Aborted(reason) => {
                    let _ = socket.close(None).await;
                    return Err(ConnectionError::Aborted(reason));
                }
                Event::PeerKeyMaterialCounter { update_counter } => {
                    peer_key_counter = Some(update_counter);
                }
                _ => {}
            }
        }

        if handshake.is_ready_for_data() {
            // §13.4.6: ask where else the peer can be reached. The answer arrives
            // whenever it arrives; the implementation guide §2.1 forbids waiting for it.
            handshake.request_access_methods();
            let mut connection = ShipConnection {
                socket,
                handshake,
                clock,
                peer,
                peer_ship_id,
                started,
                awaiting_pong: false,
                peer_key_material: Vec::new(),
                peer_key_counter,
            };
            connection.flush().await?;
            return Ok(connection);
        }

        // Wait for the peer, or for the next timer the state machine asked for.
        let deadline = handshake.poll_timeout();
        let frame = match deadline {
            Some(at) => {
                let now = clock.elapsed();
                let wait = at.saturating_sub(now);
                match tokio::time::timeout(wait, socket.next()).await {
                    Ok(frame) => frame,
                    Err(_) => {
                        handshake.handle_timeout(clock.elapsed())?;
                        continue;
                    }
                }
            }
            None => socket.next().await,
        };

        let Some(frame) = frame else {
            return Err(ConnectionError::Closed);
        };
        match frame? {
            Message::Binary(bytes) => {
                let message = ShipMessage::decode(&bytes)?;
                handshake.handle_message(message, clock.elapsed())?;
            }
            Message::Text(_) => {
                let _ = socket
                    .close(Some(CloseFrame {
                        code: 1003.into(),
                        reason: "SHIP carries binary frames only".into(),
                    }))
                    .await;
                return Err(ConnectionError::NotBinary);
            }
            Message::Close(_) => return Err(ConnectionError::Closed),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

/// Confirms the peer agreed to speak SHIP, and nothing else.
pub(crate) fn check_subprotocol(
    negotiated: Option<&tokio_tungstenite::tungstenite::http::HeaderValue>,
) -> Result<(), ConnectionError> {
    match negotiated.and_then(|value| value.to_str().ok()) {
        Some(SUBPROTOCOL) => Ok(()),
        _ => Err(ConnectionError::WrongSubprotocol),
    }
}
