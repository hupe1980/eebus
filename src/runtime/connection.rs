//! One SHIP connection: a socket, and the handshake that makes it usable.

use alloc::string::String;
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
}

impl ShipConnection {
    /// The SKI of the peer on the other end.
    ///
    /// This is the identity everything else keys off, and it came from the certificate
    /// the peer proved it holds the key to.
    pub fn peer(&self) -> Ski {
        self.peer
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
            match self.recv_ship().await? {
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

    /// Waits for the next SHIP message, letting the handshake answer the control ones.
    pub async fn recv_ship(&mut self) -> Result<ShipMessage, ConnectionError> {
        let message = self.read_frame().await?;
        if matches!(message, ShipMessage::Control(_) | ShipMessage::End(_)) {
            self.handshake
                .handle_message(message.clone(), self.clock.elapsed())?;
            self.absorb_events();
            self.flush().await?;
        }
        Ok(message)
    }

    /// Closes the connection, telling the peer why (SHIP §13.4.8).
    ///
    /// The `connectionClose` goes out before the socket does, so the peer learns this was
    /// deliberate rather than a network failure — which is what stops it retrying against
    /// a node that has gone away on purpose.
    pub async fn close(mut self, reason: ConnectionCloseReason) -> Result<(), ConnectionError> {
        self.handshake.close(reason, Duration::from_secs(0));
        let _ = self.flush().await;
        let _ = self.socket.close(None).await;
        Ok(())
    }

    /// How long this connection has been up.
    pub fn age(&self) -> Duration {
        self.started.elapsed()
    }

    /// Sends everything the handshake has queued.
    async fn flush(&mut self) -> Result<(), ConnectionError> {
        while let Some(message) = self.handshake.poll_transmit() {
            let bytes = message.encode()?;
            self.socket.send(Message::Binary(bytes.into())).await?;
        }
        Ok(())
    }

    /// Takes what the handshake learned from a control message.
    fn absorb_events(&mut self) {
        while let Some(event) = self.handshake.poll_event() {
            // SHIP 1.1.0 makes `accessMethods.id` the peer's SHIP ID, which is what a
            // node needs to dial it back in the other direction.
            if let Event::PeerAccessMethods(methods) = event {
                self.peer_ship_id = methods.id;
            }
        }
    }

    /// One frame off the socket, refusing what SHIP §10.3 forbids.
    async fn read_frame(&mut self) -> Result<ShipMessage, ConnectionError> {
        loop {
            let Some(frame) = self.socket.next().await else {
                return Err(ConnectionError::Closed);
            };
            match frame? {
                Message::Binary(bytes) => return Ok(ShipMessage::decode(&bytes)?),
                Message::Text(_) => {
                    let _ = self
                        .socket
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

    loop {
        // Everything the state machine wants to send.
        while let Some(message) = handshake.poll_transmit() {
            let bytes = message.encode()?;
            socket.send(Message::Binary(bytes.into())).await?;
        }

        for event in core::iter::from_fn(|| handshake.poll_event()) {
            if let Event::Aborted(reason) = event {
                let _ = socket.close(None).await;
                return Err(ConnectionError::Aborted(reason));
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
