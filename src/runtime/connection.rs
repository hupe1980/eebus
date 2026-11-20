//! One SHIP connection: a socket, and the handshake that makes it usable.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};

use crate::model::Datagram;
use crate::ship::{
    AbortReason, ConnectionCloseReason, Event, Handshake, HandshakeConfig, Role, SUBPROTOCOL,
    ShipMessage, Ski, Trust,
};

use super::node::Node;

/// A stream that has been through TLS and the WebSocket upgrade.
type Socket = WebSocketStream<tokio_rustls::TlsStream<TcpStream>>;

/// Why a connection could not be made or kept.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    /// As many peers are already waiting on a trust decision as the hub will hold.
    ///
    /// Not a protocol error, and not the peer's fault: it was told `hello: aborted` so
    /// that it retries rather than occupying a connection slot for minutes while nobody
    /// has approved it. See [`MAX_PENDING_TRUST`](crate::runtime::MAX_PENDING_TRUST).
    #[error("as many peers are already waiting to be approved as the hub will hold")]
    TooManyPendingPairings,
    /// The hub is already holding as many connections as it will.
    ///
    /// Not a protocol error: the peer was turned away so that a device dialling in
    /// repeatedly cannot displace the peers already served. A socket refused before its
    /// handshake is simply dropped; one refused after it is closed with a
    /// `connectionClose`.
    #[error("the hub is already holding as many connections as it will")]
    TooManyConnections,
    /// A dial did not complete within the hub's connect timeout.
    #[error("the peer did not complete the connection in time")]
    Timeout,
    /// The peer at the other end proved *this* node's own identity.
    ///
    /// TLS cannot refuse it: the certificate, the key and the SKI are the ones the verifier
    /// asks for. Nothing above it works, though — the two ends would share a message
    /// counter, a device address and a trust store — so the connection is refused as soon
    /// as the SKI is a proven fact. A node that announces `_ship._tcp` and browses for it
    /// meets its own announcement, which is the ordinary way to arrive here.
    #[error("the peer is this node itself")]
    SelfConnection,
    /// The peer has stopped taking what this node sends.
    ///
    /// [`MAX_OUTBOX`] messages are waiting for it, which is a table a peer grows by not
    /// reading — so it is capped like every other one, and the connection goes rather than
    /// the memory.
    #[error("the peer has stopped reading: {MAX_OUTBOX} messages are waiting for it")]
    Backlogged,
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
    peer_fingerprint: crate::ship::Fingerprint,
    peer_ship_id: Option<String>,
    awaiting_pong: bool,
    /// What happened about PINs, waiting to be reported (SHIP §12.5).
    pin_events: Vec<PinOutcome>,
    /// Key material the peer announced, waiting to be taken up (SHIP §12.1.3.3).
    peer_key_material: Vec<crate::ship::KeyMaterialState>,
    /// The `updateCounter` the peer's `hello` carried (§12.1.3.4).
    peer_key_counter: Option<u16>,
    /// What is waiting to be handed to the socket, oldest first.
    ///
    /// A message leaves it only in the poll in which the socket accepted it, so a write
    /// future dropped at any point loses nothing: what was not accepted is still here, and
    /// what was accepted is in the WebSocket layer's own buffer, which the next poll
    /// carries on flushing.
    outbox: VecDeque<Message>,
    /// Whether the socket holds accepted bytes that have not been flushed.
    unflushed: bool,
    /// Since when, on [`age`](Self::age)'s clock, output has waited without progress.
    stalled_since: Option<Duration>,
}

/// How many messages may wait for one peer before it is judged to have stopped reading.
///
/// The burst a connection opens with is a few dozen datagrams; a healthy peer never comes
/// near this, and a stalled one costs at most this many encoded messages.
pub const MAX_OUTBOX: usize = 256;

impl ShipConnection {
    /// The SKI of the peer on the other end.
    ///
    /// This is the identity everything else keys off, and it came from the certificate
    /// the peer proved it holds the key to.
    pub fn peer(&self) -> Ski {
        self.peer
    }

    /// The SHA-256 fingerprint of the certificate the peer presented.
    ///
    /// The identity the SHIP Pairing Service trusts (§10.2), and the one a node paired
    /// that way was admitted on. Every peer proves one, whichever way it was trusted.
    pub fn peer_fingerprint(&self) -> crate::ship::Fingerprint {
        self.peer_fingerprint
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
    ///
    /// Cancel-safe: the request is queued before anything awaits.
    pub async fn request_access_methods(&mut self) -> Result<(), ConnectionError> {
        self.handshake.request_access_methods();
        self.flush().await
    }

    /// Sends a SPINE datagram.
    ///
    /// Cancel-safe: [`queue`](Self::queue) followed by [`drain`](Self::drain).
    pub async fn send(&mut self, datagram: &Datagram) -> Result<(), ConnectionError> {
        self.queue(datagram)?;
        self.drain().await
    }

    /// Sends a SHIP message as it stands. Cancel-safe, like [`send`](Self::send).
    pub async fn send_ship(&mut self, message: &ShipMessage) -> Result<(), ConnectionError> {
        self.queue_ship(message)?;
        self.drain().await
    }

    /// Queues a SPINE datagram for the peer without waiting for the socket.
    ///
    /// It goes out on the next [`drain`](Self::drain) or [`poll_drain`](Self::poll_drain).
    /// Fails with [`ConnectionError::Backlogged`] when [`MAX_OUTBOX`] messages are already
    /// waiting.
    pub fn queue(&mut self, datagram: &Datagram) -> Result<(), ConnectionError> {
        let message = crate::ship::spine_message(datagram)?;
        self.queue_ship(&message)
    }

    /// Queues a SHIP message as it stands. See [`queue`](Self::queue).
    pub fn queue_ship(&mut self, message: &ShipMessage) -> Result<(), ConnectionError> {
        let bytes = message.encode()?;
        self.enqueue(Message::Binary(bytes.into()))
    }

    /// Queues what the SHIP state machine wants to send — control answers, the close
    /// announcement, key material. See [`queue`](Self::queue).
    pub fn queue_control(&mut self) -> Result<(), ConnectionError> {
        while let Some(message) = self.handshake.poll_transmit() {
            let bytes = message.encode()?;
            self.enqueue(Message::Binary(bytes.into()))?;
        }
        Ok(())
    }

    /// Queues a WebSocket ping, the SHIP §10.4 keep-alive. See [`queue`](Self::queue).
    pub fn queue_ping(&mut self) -> Result<(), ConnectionError> {
        self.enqueue(Message::Ping(Vec::new().into()))?;
        self.awaiting_pong = true;
        Ok(())
    }

    fn enqueue(&mut self, message: Message) -> Result<(), ConnectionError> {
        if self.outbox.len() >= MAX_OUTBOX {
            return Err(ConnectionError::Backlogged);
        }
        self.outbox.push_back(message);
        Ok(())
    }

    /// Whether anything is queued or accepted-but-unflushed.
    pub fn has_output(&self) -> bool {
        !self.outbox.is_empty() || self.unflushed
    }

    /// How long queued output has waited without the socket taking any of it.
    ///
    /// [`None`] while output moves, or when there is none. A peer that stops reading shows
    /// up here first: its socket buffers fill, and then nothing moves.
    pub fn stalled_for(&self) -> Option<Duration> {
        self.stalled_since
            .map(|since| self.clock.elapsed().saturating_sub(since))
    }

    /// Hands queued messages to the socket and flushes it, as far as it will go now.
    ///
    /// `Ready(Ok(()))` once everything queued is on its way; `Pending` while the socket
    /// pushes back. A message is taken out of the queue only in the same poll in which
    /// the socket accepted it, and accepted bytes stay in the WebSocket layer's buffer
    /// until a later poll flushes them — so this can be abandoned between any two polls
    /// without losing a message or leaving a partial frame on the wire.
    pub fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ConnectionError>> {
        use futures_util::Sink;
        let now = self.clock.elapsed();
        let mut moved = false;
        while !self.outbox.is_empty() {
            match Pin::new(&mut self.socket).poll_ready(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error.into())),
                Poll::Pending => return self.pending_drain(moved, now),
            }
            let message = self.outbox.pop_front().expect("checked non-empty");
            if let Err(error) = Pin::new(&mut self.socket).start_send(message) {
                return Poll::Ready(Err(error.into()));
            }
            self.unflushed = true;
            moved = true;
        }
        if self.unflushed {
            match Pin::new(&mut self.socket).poll_flush(cx) {
                Poll::Ready(Ok(())) => self.unflushed = false,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error.into())),
                Poll::Pending => return self.pending_drain(moved, now),
            }
        }
        self.stalled_since = None;
        Poll::Ready(Ok(()))
    }

    fn pending_drain(&mut self, moved: bool, now: Duration) -> Poll<Result<(), ConnectionError>> {
        if moved || self.stalled_since.is_none() {
            self.stalled_since = Some(now);
        }
        Poll::Pending
    }

    /// Waits until everything queued has been flushed. Cancel-safe; see
    /// [`poll_drain`](Self::poll_drain).
    pub async fn drain(&mut self) -> Result<(), ConnectionError> {
        core::future::poll_fn(|cx| self.poll_drain(cx)).await
    }

    /// Waits for the next SPINE datagram.
    ///
    /// SHIP messages that are not data are handled on the way past, because an
    /// application working in SPINE has no use for them.
    ///
    /// Cancel-safe: it may sit in a `select!` beside a shutdown signal or a deadline. What
    /// earlier calls queued — SHIP's answers to control messages — is sent first, and that
    /// [`drain`](Self::drain) is cancel-safe. The read is cancel-safe, and once a message
    /// has been taken off the socket nothing awaits before it is returned: the answers it
    /// provokes are queued and go out at the start of the next call (or with any
    /// [`send`](Self::send)).
    pub async fn recv(&mut self) -> Result<Datagram, ConnectionError> {
        loop {
            self.drain().await?;
            let message = match self.next_message().await {
                Ok(message) => message,
                Err(ConnectionError::NotBinary) => {
                    // A frame SHIP §10.3 forbids carries nothing to lose; the close that
                    // answers it is bounded and may be cut short.
                    let close = self.socket.close(Some(CloseFrame {
                        code: 1003.into(),
                        reason: "SHIP carries binary frames only".into(),
                    }));
                    let _ = tokio::time::timeout(super::CLOSE_MAX_TIME, close).await;
                    return Err(ConnectionError::NotBinary);
                }
                Err(error) => return Err(error),
            };
            self.handle_message(&message)?;
            self.queue_control()?;
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
    /// The WebSocket layer may write pong/close replies while reading; `tungstenite` never
    /// blocks on them and carries an unflushed one to the next read, so a dropped future
    /// leaves no partial frame. An inbound partial frame is buffered and resumed on the
    /// next call (a `WebSocketStream` guarantee). Nothing the caller queued is sent here;
    /// that is [`poll_drain`](Self::poll_drain)'s.
    ///
    /// A frame SHIP §10.3 forbids comes back as an error rather than being answered here,
    /// because that answer is a close frame this node composes rather than one the
    /// WebSocket layer owes. The answer is [`reject`](Self::reject).
    pub async fn next_message(&mut self) -> Result<ShipMessage, ConnectionError> {
        core::future::poll_fn(|cx| self.poll_next_message(cx)).await
    }

    /// [`next_message`](Self::next_message) as a poll, for a caller that multiplexes
    /// several connections in one future.
    pub fn poll_next_message(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<ShipMessage, ConnectionError>> {
        use futures_util::Stream;
        loop {
            let frame = match Pin::new(&mut self.socket).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Err(ConnectionError::Closed)),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error.into())),
                Poll::Ready(Some(Ok(frame))) => frame,
            };
            match frame {
                Message::Binary(bytes) => {
                    return Poll::Ready(ShipMessage::decode(&bytes).map_err(Into::into));
                }
                // §10.3 forbids it; the caller answers with 1003 through `reject`.
                Message::Text(_) => return Poll::Ready(Err(ConnectionError::NotBinary)),
                Message::Close(_) => return Poll::Ready(Err(ConnectionError::Closed)),
                Message::Pong(_) => self.awaiting_pong = false,
                // Tungstenite queues the pong itself while reading, and never blocks on
                // writing it — so a ping costs this loop nothing but another turn.
                Message::Ping(_) | Message::Frame(_) => {}
            }
        }
    }

    /// Closes the socket with a WebSocket status code, for a frame SHIP does not allow.
    ///
    /// §10.3: a text frame is answered with 1003, "unsupported data".
    ///
    /// Bounded by [`CLOSE_MAX_TIME`](super::CLOSE_MAX_TIME): a peer that sent something SHIP forbids and then
    /// stopped reading is not owed a wait.
    ///
    /// Cancelling it drops the socket without the close frame; the connection is gone either way.
    pub async fn reject(mut self, code: u16, reason: &str) {
        let close = self.socket.close(Some(CloseFrame {
            code: code.into(),
            reason: reason.to_string().into(),
        }));
        let _ = tokio::time::timeout(super::CLOSE_MAX_TIME, close).await;
    }

    /// Feeds a received message to the SHIP state machine.
    ///
    /// Control messages are answered from here — the answer is held by the state machine,
    /// and goes out on the next [`flush`](Self::flush) or [`queue_control`](Self::queue_control) — so an application working in SPINE never sees
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
        // A finished handshake refuses the call; that is expected.
        let _ = self.handshake.handle_timeout(self.clock.elapsed());
        self.absorb_events();
        Ok(())
    }

    /// When the SHIP state machine wants to be woken, if it does, measured like
    /// [`age`](Self::age) rather than on [`Hub::now`](super::Hub::now)'s clock.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.handshake.poll_timeout()
    }

    /// Sends a WebSocket ping.
    ///
    /// SHIP §10.4 uses these two ways: as a keep-alive on an idle connection, at least
    /// every fifty seconds, and as the liveness probe the double-connection rule of
    /// §12.2.3 falls back to.
    ///
    /// Cancel-safe: the ping is queued before anything awaits.
    pub async fn ping(&mut self) -> Result<(), ConnectionError> {
        self.queue_ping()?;
        self.drain().await
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
    ///
    /// Cancelling it drops the socket without finishing the close handshake; the connection is gone either way.
    pub async fn close(
        mut self,
        reason: ConnectionCloseReason,
        max_time: Duration,
    ) -> Result<(), ConnectionError> {
        self.handshake.close(reason, max_time, self.clock.elapsed());
        // Bounded like the wait below: a peer that stopped reading cannot hold the close.
        let _ = tokio::time::timeout(max_time, self.flush()).await;

        // Wait for the confirmation, but not past the time the peer was given.
        let deadline = tokio::time::sleep(max_time);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                message = self.next_message() => match message {
                    Ok(ShipMessage::End(_)) => break,
                    Ok(message) => {
                        let _ = self.handle_message(&message);
                    }
                    Err(_) => break,
                },
            }
        }

        let _ = tokio::time::timeout(max_time, self.socket.close(None)).await;
        Ok(())
    }

    /// How long this connection has been up.
    pub fn age(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Sends everything the handshake has queued, and everything queued before it.
    ///
    /// Cancel-safe: [`queue_control`](Self::queue_control) followed by
    /// [`drain`](Self::drain).
    pub async fn flush(&mut self) -> Result<(), ConnectionError> {
        self.queue_control()?;
        self.drain().await
    }

    /// Takes what the handshake learned from a control message.
    fn absorb_events(&mut self) {
        while let Some(event) = self.handshake.poll_event() {
            match event {
                // SHIP 1.1.0 makes `accessMethods.id` the peer's SHIP ID, which is what a
                // node needs to dial it back in the other direction.
                Event::PeerAccessMethods(methods) => self.peer_ship_id = methods.id,
                Event::PeerKeyMaterial(state) => self.peer_key_material.push(state),
                // Only reachable from a `send_pin` the caller made itself, after the
                // handshake opened: everything during it is handled in `run_handshake`,
                // which is where the trust store is.
                Event::PinWithheld { level } => {
                    self.pin_events.push(PinOutcome::Withheld { level });
                }
                Event::PeerPinVerified => {
                    self.pin_events.push(PinOutcome::Verified { level: None })
                }
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

    /// Takes what has happened about PINs since this was last called (SHIP §12.5).
    pub fn take_pin_events(&mut self) -> Vec<PinOutcome> {
        core::mem::take(&mut self.pin_events)
    }

    /// Takes the key material the peer has announced since this was last called.
    pub fn take_peer_key_material(&mut self) -> Vec<crate::ship::KeyMaterialState> {
        core::mem::take(&mut self.peer_key_material)
    }

    /// Asks the peer for its key material (SHIP §12.1.3.4).
    ///
    /// `known_update_counter` is what this node has stored for it.
    ///
    /// Cancel-safe: the request is queued before anything awaits.
    pub async fn request_key_material(
        &mut self,
        known_update_counter: u16,
    ) -> Result<(), ConnectionError> {
        self.handshake.request_key_material(known_update_counter);
        self.flush().await
    }

    /// Announces this node's key material to the peer (SHIP §12.1.3.2).
    ///
    /// Cancel-safe: the announcement is queued before anything awaits.
    pub async fn send_key_material(&mut self) -> Result<(), ConnectionError> {
        self.handshake.send_key_material(self.clock.elapsed());
        self.flush().await
    }

    /// Queues a request for the peer's key material; see
    /// [`request_key_material`](Self::request_key_material) and [`queue`](Self::queue).
    pub fn queue_key_request(&mut self, known_update_counter: u16) -> Result<(), ConnectionError> {
        self.handshake.request_key_material(known_update_counter);
        self.queue_control()
    }

    /// Queues this node's key material; see [`send_key_material`](Self::send_key_material)
    /// and [`queue`](Self::queue).
    pub fn queue_key_material(&mut self) -> Result<(), ConnectionError> {
        self.handshake.send_key_material(self.clock.elapsed());
        self.queue_control()
    }
}

/// What happened about a PIN on one connection (SHIP §12.5).
///
/// Surfaced so an installer can tell a withheld PIN from none, and see who became the
/// commissioning tool: §12.5 makes the first peer to present a valid PIN since a factory
/// default eligible for that role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PinOutcome {
    /// The peer proved it holds this node's PIN.
    Verified {
        /// What it is trusted at now, after
        /// [`TrustStore::award_pin_trust`](super::TrustStore::award_pin_trust) applied
        /// §12.5's once-only rule. [`None`] where the peer is not in the trust store, or
        /// where the caller sent the PIN itself after the handshake had opened.
        level: Option<crate::ship::TrustLevel>,
    },
    /// This node's PIN was **not** sent: §12.5 forbids it below user trust 32.
    Withheld {
        /// What the peer is trusted at, which was not enough.
        level: crate::ship::TrustLevel,
    },
}

/// Tells whoever is driving a node that a peer is waiting on a trust decision.
///
/// Called once per handshake, from the task running it, the moment the peer has been told
/// `hello: pending`. What a [`Hub`](crate::runtime::Hub) hands in turns it into
/// [`HubEvent::TrustRequested`](crate::runtime::HubEvent::TrustRequested); a consumer
/// driving [`Node::accept_reporting`](crate::runtime::Node::accept_reporting) directly
/// gets the same call.
///
/// It runs on the handshake's own task, so it must not block: send on a channel, or set a
/// flag. Every peer reported here is also in
/// [`Node::pending_peers`](crate::runtime::Node::pending_peers) for as long as it waits,
/// which is the answer for a caller that would rather poll than be called.
pub type TrustReporter = alloc::boxed::Box<dyn Fn(super::PendingPeer) + Send + Sync>;

/// Drives a [`Handshake`] over a socket until it reaches the data phase.
///
/// This is the whole of the sans-IO bargain in one function: the state machine says what
/// to send and when to wake it, and nothing here decides anything about the protocol.
///
/// Three things wake the loop: a frame, a SHIP timer, and a change to the node's trust
/// store. The third is what makes pairing interactive — a peer held in the pending state
/// is let through the moment a user approves its SKI, and turned away the moment a user
/// refuses it — and it is the reason the loop takes the [`Node`] rather than a fixed
/// [`Trust`].
pub(crate) async fn run_handshake(
    mut socket: Socket,
    role: Role,
    config: HandshakeConfig,
    trust: Trust,
    peer: super::node::PeerIdentity,
    node: &Node,
    report: Option<TrustReporter>,
) -> Result<ShipConnection, ConnectionError> {
    let clock = Instant::now();
    // Dropped however this function ends — the peer stopped waiting, the socket failed,
    // the caller's future was cancelled — so a pending entry cannot outlive the handshake
    // that put it there. `None` until the peer turns out to need a decision.
    let mut waiting: Option<super::node::PendingGuard<'_>> = None;
    let mut handshake = Handshake::new(role, config, trust, Duration::ZERO);
    let peer_ship_id = None;
    // §12.1.3.4 puts the peer's key-material counter in the `hello`, which happens before
    // there is a `ShipConnection` to hold it. Kept here and handed over below, or the
    // one signal that says "your stored certificate is stale" is thrown away.
    let mut peer_key_counter = None;
    let mut pin_events: Vec<PinOutcome> = Vec::new();
    // Subscribed before the first check, so a decision made between the two is not lost:
    // `changed` reports every version this receiver has not yet seen.
    let mut decisions = node.trust_store().watch();

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
                // §12.5: a verified PIN earns the peer a second factor; the store applies
                // the once-only rule because only it survives the connection.
                Event::PeerPinVerified => {
                    let level = node.trust_store().award_pin_trust(&peer.ski);
                    pin_events.push(PinOutcome::Verified { level });
                }
                Event::PinWithheld { level } => {
                    pin_events.push(PinOutcome::Withheld { level });
                }
                Event::TrustRequired => {
                    // Registered before the callback: a consumer woken by `report` and
                    // asking the node what is waiting must not race the bookkeeping.
                    let pending = super::PendingPeer {
                        ski: peer.ski,
                        fingerprint: peer.fingerprint,
                    };
                    waiting = Some(node.note_pending(pending));
                    if let Some(report) = &report {
                        report(pending);
                    }
                }
                _ => {}
            }
        }

        if handshake.is_ready_for_data() {
            // Approved: it is no longer waiting on anybody.
            drop(waiting.take());
            // §13.4.6: ask where else the peer can be reached. The answer arrives
            // whenever it arrives; the implementation guide §2.1 forbids waiting for it.
            handshake.request_access_methods();
            let mut connection = ShipConnection {
                socket,
                handshake,
                clock,
                peer: peer.ski,
                peer_fingerprint: peer.fingerprint,
                peer_ship_id,
                awaiting_pong: false,
                pin_events: core::mem::take(&mut pin_events),
                peer_key_material: Vec::new(),
                peer_key_counter,
                outbox: VecDeque::new(),
                unflushed: false,
                stalled_since: None,
            };
            connection.flush().await?;
            return Ok(connection);
        }

        // Wait for the peer, for the next timer the state machine asked for, or for a
        // trust decision. Reading the socket is cancel-safe, so losing the race costs
        // nothing; the timer and the watch are re-armed on the next turn.
        let deadline = handshake.poll_timeout();
        let wait = deadline.map(|at| at.saturating_sub(clock.elapsed()));
        let frame = tokio::select! {
            frame = socket.next() => frame,
            _ = tokio::time::sleep(wait.unwrap_or(Duration::ZERO)), if wait.is_some() => {
                handshake.handle_timeout(clock.elapsed())?;
                continue;
            }
            changed = decisions.changed() => {
                if changed.is_err() {
                    // The store is gone, which cannot happen while the node exists;
                    // nothing more will ever be decided, so wait on the wire alone.
                    continue;
                }
                if node.take_refusal(&peer.ski) {
                    handshake.set_trust(Trust::Rejected, clock.elapsed())?;
                } else if let Trust::Trusted(level) = node.trust_for(&peer) {
                    // Either way in: a user approved the SKI, or a pairing request named
                    // the certificate. §10.2 makes the two equivalent at this point, and
                    // the level says which — §12.5 reads it before sending a PIN.
                    handshake.set_trust(Trust::Trusted(level), clock.elapsed())?;
                }
                continue;
            }
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
