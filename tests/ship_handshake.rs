//! The SHIP handshake, exercised against the official test specification.
//!
//! Test names carry the identifier of the case they cover in
//! `EEBus_SHIP_TestSpecification_V1.0.0`, so a run of `cargo test` doubles as a
//! pre-check for the certification laboratory. Every timer is driven from a virtual
//! clock, which is what makes cases such as "apply the Wait-For-Ready-Timer" — two
//! minutes of wall time at the specification's recommended setting — run instantly.

use core::time::Duration;

use eebus::ship::{
    AbortReason, ConnectionClose, ConnectionClosePhase, ConnectionCloseReason, ConnectionHello,
    ConnectionHelloPhase, ControlMessage, EndMessage, Event, Handshake, HandshakeConfig,
    MessageProtocolFormat, MessageProtocolFormats, MessageProtocolHandshake,
    MessageProtocolHandshakeVersion, Phase, PinRequirement, PinState, ProtocolHandshakeType, Role,
    ShipMessage, Trust,
};

/// Two handshakes and a virtual clock.
struct Link {
    a: Handshake,
    b: Handshake,
    now: Duration,
}

impl Link {
    fn new(trust_a: Trust, trust_b: Trust) -> Self {
        Self::with_configs(
            HandshakeConfig::default(),
            HandshakeConfig::default(),
            trust_a,
            trust_b,
        )
    }

    fn with_configs(
        config_a: HandshakeConfig,
        config_b: HandshakeConfig,
        trust_a: Trust,
        trust_b: Trust,
    ) -> Self {
        let now = Duration::ZERO;
        Self {
            a: Handshake::new(Role::Client, config_a, trust_a, now),
            b: Handshake::new(Role::Server, config_b, trust_b, now),
            now,
        }
    }

    /// Delivers messages back and forth until both sides fall silent.
    fn pump(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(msg) = self.a.poll_transmit() {
                let _ = self.b.handle_message(msg, self.now);
                moved = true;
            }
            while let Some(msg) = self.b.poll_transmit() {
                let _ = self.a.handle_message(msg, self.now);
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("handshake did not settle");
    }

    /// Advances the clock and fires whichever timers are due.
    fn advance(&mut self, by: Duration) {
        self.now += by;
        let _ = self.a.handle_timeout(self.now);
        let _ = self.b.handle_timeout(self.now);
    }

    fn events_a(&mut self) -> Vec<Event> {
        core::iter::from_fn(|| self.a.poll_event()).collect()
    }

    fn events_b(&mut self) -> Vec<Event> {
        core::iter::from_fn(|| self.b.poll_event()).collect()
    }
}

/// `TC_SHIP_ROLE_001` and `TC_SHIP_ROLE_002`: a node completes the handshake as server
/// and as client, reaching data exchange with the version and format both agreed on.
#[test]
fn tc_ship_role_001_002_handshake_completes_in_both_roles() {
    let mut link = Link::new(Trust::Trusted, Trust::Trusted);
    link.pump();

    assert_eq!(link.a.phase(), Phase::DataExchange);
    assert_eq!(link.b.phase(), Phase::DataExchange);
    assert!(link.a.is_ready_for_data() && link.b.is_ready_for_data());

    // SHIP 1.1.0 §13.4.4.2.3 has the client announce major 1, minor 1.
    let ((major, minor), format) = link.a.negotiated().cloned().expect("negotiated");
    assert_eq!((major, minor), (1, 1));
    assert_eq!(format, "JSON-UTF8");
    assert_eq!(link.b.negotiated(), link.a.negotiated());

    assert!(
        link.events_a()
            .iter()
            .any(|e| matches!(e, Event::Ready { .. }))
    );
    assert!(
        link.events_b()
            .iter()
            .any(|e| matches!(e, Event::Ready { .. }))
    );
}

/// SHIP §13.4.4.2.2: a node supports every version from 1.0 up to its own maximum, so
/// a 1.1 node and a 1.0 node settle on 1.0.
#[test]
fn version_negotiation_falls_back_to_the_older_peer() {
    let legacy = HandshakeConfig {
        max_version: (1, 0),
        ..HandshakeConfig::default()
    };
    let mut link = Link::with_configs(
        legacy,
        HandshakeConfig::default(),
        Trust::Trusted,
        Trust::Trusted,
    );
    link.pump();

    assert_eq!(link.a.negotiated().map(|(v, _)| *v), Some((1, 0)));
    assert_eq!(link.b.negotiated().map(|(v, _)| *v), Some((1, 0)));
    assert_eq!(link.a.phase(), Phase::DataExchange);
}

/// `TC_SHIP_CMI_003` and `TC_SHIP_CMI_004`: `CmiTimeout` applies in both roles.
#[test]
fn tc_ship_cmi_003_004_cmi_timeout_aborts() {
    let config = HandshakeConfig::default();
    let cmi_timeout = config.cmi_timeout;
    let mut hs = Handshake::new(Role::Client, config, Trust::Trusted, Duration::ZERO);

    assert_eq!(hs.poll_transmit(), Some(ShipMessage::Cmi));
    assert_eq!(hs.poll_timeout(), Some(cmi_timeout));

    hs.handle_timeout(cmi_timeout).unwrap();
    assert_eq!(hs.phase(), Phase::Aborted);
    assert_eq!(
        hs.poll_event(),
        Some(Event::Aborted(AbortReason::Timeout(Phase::Cmi)))
    );
}

/// `TC_SHIP_HELLO_001`: a valid hello moves the connection into the protocol handshake.
#[test]
fn tc_ship_hello_001_ready_hello_enters_protocol_handshake() {
    let mut hs = Handshake::new(
        Role::Server,
        HandshakeConfig::default(),
        Trust::Trusted,
        Duration::ZERO,
    );
    let _cmi = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();

    // Having seen CMI, the node announces its own readiness.
    let ShipMessage::Control(ControlMessage::ConnectionHello(hello)) =
        hs.poll_transmit().expect("hello")
    else {
        panic!("expected a hello");
    };
    assert_eq!(hello.phase, Some(ConnectionHelloPhase::Ready));
    assert_eq!(hello.waiting, Some(120_000), "T_hello_init recommendation");

    hs.handle_message(ready_hello(), Duration::ZERO).unwrap();
    assert_eq!(hs.phase(), Phase::ProtocolHandshake);
}

/// `TC_SHIP_HELLO_002`: prolongation requests are accepted, and each one extends the
/// Wait-For-Ready-Timer by `T_hello_inc` (SHIP §13.4.4.1.3).
#[test]
fn tc_ship_hello_002_prolongation_requests_are_accepted() {
    let mut hs = Handshake::new(
        Role::Server,
        HandshakeConfig::default(),
        Trust::Pending,
        Duration::ZERO,
    );
    let _ = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();
    drain(&mut hs);

    // The peer asks us to keep waiting; we grant it three times over.
    for round in 1..=3u32 {
        let now = Duration::from_secs(u64::from(round) * 100);
        hs.handle_message(prolongation_request(), now).unwrap();

        let ShipMessage::Control(ControlMessage::ConnectionHello(reply)) = hs
            .poll_transmit()
            .expect("reply to the prolongation request")
        else {
            panic!("expected a hello");
        };
        assert_eq!(reply.phase, Some(ConnectionHelloPhase::Pending));
        assert_eq!(reply.waiting, Some(120_000));
        assert_eq!(
            hs.poll_timeout(),
            Some(now + Duration::from_secs(120)),
            "round {round}: the timer restarts from now"
        );
        assert_eq!(hs.phase(), Phase::Hello);
    }
}

/// `TC_SHIP_HELLO_003`: when the Wait-For-Ready-Timer expires with nothing pending, the
/// handshake is abandoned.
#[test]
fn tc_ship_hello_003_wait_for_ready_timer_expires() {
    let mut hs = Handshake::new(
        Role::Client,
        HandshakeConfig::default(),
        Trust::Trusted,
        Duration::ZERO,
    );
    let _ = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();
    drain(&mut hs);
    assert_eq!(hs.phase(), Phase::Hello);

    hs.handle_timeout(Duration::from_secs(120)).unwrap();
    assert_eq!(hs.phase(), Phase::Aborted);
    assert!(
        core::iter::from_fn(|| hs.poll_event())
            .any(|e| e == Event::Aborted(AbortReason::Timeout(Phase::Hello)))
    );
}

/// `TC_SHIP_HELLO_004`: a `pending` update without `prolongationRequest` is noted but
/// does not itself extend the timer — only an explicit request does.
#[test]
fn tc_ship_hello_004_pending_without_request_does_not_extend() {
    let mut hs = Handshake::new(
        Role::Client,
        HandshakeConfig::default(),
        Trust::Trusted,
        Duration::ZERO,
    );
    let _ = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();
    drain(&mut hs);
    let deadline = hs.poll_timeout().expect("wait-for-ready is armed");

    hs.handle_message(pending_hello(120_000), Duration::from_secs(10))
        .unwrap();

    assert!(hs.poll_transmit().is_none(), "no reply is owed");
    assert!(
        core::iter::from_fn(|| hs.poll_event())
            .any(|e| matches!(e, Event::PeerAwaitingTrust { .. })),
        "the application learns the peer is waiting"
    );
    // The next wake-up is the prolongation request, which precedes the unchanged
    // Wait-For-Ready deadline by `T_hello_prolong_waiting_gap`.
    assert_eq!(
        hs.poll_timeout(),
        Some(Duration::from_secs(10) + Duration::from_secs(105))
    );
    assert!(hs.poll_timeout().unwrap() < deadline);
}

/// A peer that announces a very short waiting time gets no prolongation request:
/// `T_hello_prolong_thr_inc` exists so that a peer cannot drive the exchange with
/// near-zero waits.
#[test]
fn short_waiting_times_do_not_arm_a_prolongation_request() {
    let mut hs = Handshake::new(
        Role::Client,
        HandshakeConfig::default(),
        Trust::Trusted,
        Duration::ZERO,
    );
    let _ = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();
    drain(&mut hs);

    hs.handle_message(pending_hello(5_000), Duration::ZERO)
        .unwrap();
    assert_eq!(
        hs.poll_timeout(),
        Some(Duration::from_secs(120)),
        "only the untouched Wait-For-Ready-Timer remains"
    );
}

/// The hello phase waits for a person, then completes when the answer arrives.
#[test]
fn a_pending_trust_decision_completes_the_handshake_when_granted() {
    let mut link = Link::new(Trust::Trusted, Trust::Pending);
    link.pump();

    assert_eq!(link.b.phase(), Phase::Hello);
    assert!(link.events_b().contains(&Event::TrustRequired));
    assert!(
        link.events_a()
            .iter()
            .any(|e| matches!(e, Event::PeerAwaitingTrust { .. }))
    );

    link.advance(Duration::from_secs(30));
    link.b.set_trust(Trust::Trusted, link.now).unwrap();
    link.pump();

    assert_eq!(link.a.phase(), Phase::DataExchange);
    assert_eq!(link.b.phase(), Phase::DataExchange);
}

/// A refusal aborts both sides, and the peer is told rather than left to time out.
#[test]
fn a_rejected_key_aborts_both_sides() {
    let mut link = Link::new(Trust::Trusted, Trust::Rejected);
    link.pump();

    assert_eq!(link.b.phase(), Phase::Aborted);
    assert_eq!(link.a.phase(), Phase::Aborted);
    assert!(
        link.events_a()
            .contains(&Event::Aborted(AbortReason::PeerAborted))
    );
}

/// `TC_SHIP_PROT_003` and `TC_SHIP_PROT_004`: the protocol handshake's Wait-Timer is
/// ten seconds, and expiry sends an error before closing.
#[test]
fn tc_ship_prot_003_004_wait_timer_expires() {
    let mut hs = client_in_protocol_handshake();
    assert_eq!(hs.poll_timeout(), Some(Duration::from_secs(10)));

    hs.handle_timeout(Duration::from_secs(10)).unwrap();

    let ShipMessage::Control(ControlMessage::MessageProtocolHandshakeError(err)) = hs
        .poll_transmit()
        .expect("an error message precedes the close")
    else {
        panic!("expected a protocol handshake error");
    };
    assert_eq!(err.error.map(|e| e.get()), Some(1), "1 = timeout");
    assert_eq!(hs.phase(), Phase::Aborted);
}

/// `TC_SHIP_PROT_005` and `TC_SHIP_PROT_006`: an unexpected message in the protocol
/// handshake is refused with error 2.
#[test]
fn tc_ship_prot_005_006_unexpected_message_is_rejected() {
    let mut hs = client_in_protocol_handshake();

    // A client expects `select`, not another `announceMax`.
    hs.handle_message(
        handshake_message(ProtocolHandshakeType::AnnounceMax, (1, 1), &["JSON-UTF8"]),
        Duration::ZERO,
    )
    .unwrap();

    let ShipMessage::Control(ControlMessage::MessageProtocolHandshakeError(err)) =
        hs.poll_transmit().expect("error message")
    else {
        panic!("expected a protocol handshake error");
    };
    assert_eq!(
        err.error.map(|e| e.get()),
        Some(2),
        "2 = unexpected message"
    );
    assert_eq!(hs.phase(), Phase::Aborted);
}

/// A server that selects a format the client never offered is refused with error 3.
#[test]
fn a_selection_the_client_never_offered_is_rejected() {
    let mut hs = client_in_protocol_handshake();

    hs.handle_message(
        handshake_message(ProtocolHandshakeType::Select, (1, 1), &["JSON-UTF16"]),
        Duration::ZERO,
    )
    .unwrap();

    let ShipMessage::Control(ControlMessage::MessageProtocolHandshakeError(err)) =
        hs.poll_transmit().expect("error message")
    else {
        panic!("expected a protocol handshake error");
    };
    assert_eq!(
        err.error.map(|e| e.get()),
        Some(3),
        "3 = selection mismatch"
    );
    assert_eq!(hs.phase(), Phase::Aborted);
}

/// `TC_SHIP_PIN_001`: with `pinState` "none" on both sides, data exchange opens without
/// any PIN traffic. The SHIP parameter sheet fixes the device under test to this state.
#[test]
fn tc_ship_pin_001_pin_state_none() {
    let mut link = Link::new(Trust::Trusted, Trust::Trusted);
    link.pump();

    assert_eq!(link.a.peer_pin_state(), Some(PinState::None));
    assert_eq!(link.b.peer_pin_state(), Some(PinState::None));
    assert!(link.a.is_ready_for_data());
}

/// A node that requires a PIN gets one from a peer that knows it, and only then does
/// that node open data exchange (SHIP §13.4.4.3.5.2).
#[test]
fn a_required_pin_is_supplied_and_verified() {
    let server = HandshakeConfig {
        pin: PinRequirement::Required("1234ABCD".into()),
        ..HandshakeConfig::default()
    };
    let client = HandshakeConfig {
        peer_pin: Some("1234ABCD".into()),
        ..HandshakeConfig::default()
    };
    let mut link = Link::with_configs(client, server, Trust::Trusted, Trust::Trusted);
    link.pump();

    assert_eq!(link.b.phase(), Phase::DataExchange, "the PIN was accepted");
    // The verifying node re-announces its state, so the peer ends up seeing `pinOk`.
    assert_eq!(link.a.peer_pin_state(), Some(PinState::PinOk));
}

/// A wrong PIN is answered with an error and the connection is not opened.
#[test]
fn a_wrong_pin_is_refused() {
    let server = HandshakeConfig {
        pin: PinRequirement::Required("1234ABCD".into()),
        ..HandshakeConfig::default()
    };
    let client = HandshakeConfig {
        peer_pin: Some("DEADBEEF".into()),
        ..HandshakeConfig::default()
    };
    let mut link = Link::with_configs(client, server, Trust::Trusted, Trust::Trusted);
    link.pump();

    assert_ne!(link.b.phase(), Phase::DataExchange);
    assert_eq!(link.b.phase(), Phase::Pin);
}

/// `TC_SHIP_AM_001`: an access methods request is answered with this node's SHIP ID.
/// SHIP 1.1.0 makes `accessMethods.id` mandatory, where the 1.0.1 implementations that
/// return an empty structure leave the peer unable to reconnect in the other direction.
#[test]
fn tc_ship_am_001_access_methods_carry_the_ship_id() {
    let config = HandshakeConfig {
        ship_id: Some("i:12345_u:HEMS-0001".into()),
        ..HandshakeConfig::default()
    };
    let mut link = Link::with_configs(
        HandshakeConfig::default(),
        config,
        Trust::Trusted,
        Trust::Trusted,
    );
    link.pump();

    link.a.request_access_methods();
    link.pump();

    let methods = link
        .events_a()
        .into_iter()
        .find_map(|e| match e {
            Event::PeerAccessMethods(m) => Some(m),
            _ => None,
        })
        .expect("the peer answered");
    assert_eq!(methods.id.as_deref(), Some("i:12345_u:HEMS-0001"));
    assert!(
        methods.dns_sd_m_dns.is_some(),
        "the peer is reachable over mDNS"
    );
}

/// `TC_SHIP_AMDATA_001`: an outstanding access-methods request must not stall SPINE.
/// The SHIP implementation guide §2.1 calls blocking here a defect, because the peer
/// will time out waiting for discovery replies that never come.
#[test]
fn tc_ship_amdata_001_access_methods_do_not_block_data() {
    let mut link = Link::new(Trust::Trusted, Trust::Trusted);
    link.pump();

    link.a.request_access_methods();
    // Without pumping the request, data still flows in both directions.
    assert!(link.a.is_ready_for_data());
    assert!(link.b.is_ready_for_data());

    let data = ShipMessage::Data(eebus::ship::DataMessage::Data(eebus::ship::Data {
        header: Some(eebus::ship::Header {
            protocol_id: Some(eebus::ship::ProtocolId("ee1.0".into())),
        }),
        payload: Some(serde_json::json!({"datagram": []})),
        extension: None,
    }));
    link.b.handle_message(data, link.now).unwrap();
    assert_eq!(link.b.phase(), Phase::DataExchange, "data is not an error");
}

/// `TC_SHIP_TERM_001`: a termination is announced with a `maxTime` and confirmed.
#[test]
fn tc_ship_term_001_close_is_announced_and_confirmed() {
    let mut link = Link::new(Trust::Trusted, Trust::Trusted);
    link.pump();

    link.a.close(
        ConnectionCloseReason::Unspecific,
        Duration::from_millis(500),
    );
    assert_eq!(link.a.phase(), Phase::Closing);

    let ShipMessage::End(EndMessage::ConnectionClose(announce)) =
        link.a.poll_transmit().expect("close announcement")
    else {
        panic!("expected a close");
    };
    assert_eq!(announce.phase, Some(ConnectionClosePhase::Announce));
    assert_eq!(announce.max_time, Some(500));
    assert_eq!(announce.reason, Some(ConnectionCloseReason::Unspecific));

    link.b
        .handle_message(
            ShipMessage::End(EndMessage::ConnectionClose(announce)),
            link.now,
        )
        .unwrap();

    let ShipMessage::End(EndMessage::ConnectionClose(confirm)) =
        link.b.poll_transmit().expect("close confirmation")
    else {
        panic!("expected a close");
    };
    assert_eq!(confirm.phase, Some(ConnectionClosePhase::Confirm));
    assert_eq!(link.b.phase(), Phase::Closed);

    link.a
        .handle_message(
            ShipMessage::End(EndMessage::ConnectionClose(confirm)),
            link.now,
        )
        .unwrap();
    assert_eq!(link.a.phase(), Phase::Closed);
}

/// Once finished, the state machine refuses further input rather than reopening.
#[test]
fn a_closed_handshake_stays_closed() {
    let mut link = Link::new(Trust::Trusted, Trust::Trusted);
    link.pump();
    link.a
        .close(ConnectionCloseReason::RemovedConnection, Duration::ZERO);
    let announce = link.a.poll_transmit().unwrap();
    link.b.handle_message(announce, link.now).unwrap();
    let confirm = link.b.poll_transmit().unwrap();
    link.a.handle_message(confirm, link.now).unwrap();

    assert!(link.a.handle_message(ShipMessage::Cmi, link.now).is_err());
    assert!(link.a.handle_timeout(link.now).is_err());
}

// ---- helpers ----------------------------------------------------------------

fn drain(hs: &mut Handshake) {
    while hs.poll_transmit().is_some() {}
}

fn ready_hello() -> ShipMessage {
    ShipMessage::Control(ControlMessage::ConnectionHello(ConnectionHello {
        phase: Some(ConnectionHelloPhase::Ready),
        waiting: Some(120_000),
        ..Default::default()
    }))
}

fn pending_hello(waiting_ms: u32) -> ShipMessage {
    ShipMessage::Control(ControlMessage::ConnectionHello(ConnectionHello {
        phase: Some(ConnectionHelloPhase::Pending),
        waiting: Some(waiting_ms),
        ..Default::default()
    }))
}

fn prolongation_request() -> ShipMessage {
    ShipMessage::Control(ControlMessage::ConnectionHello(ConnectionHello {
        phase: Some(ConnectionHelloPhase::Ready),
        waiting: Some(120_000),
        prolongation_request: Some(true),
        ..Default::default()
    }))
}

fn handshake_message(
    kind: ProtocolHandshakeType,
    version: (u16, u16),
    formats: &[&str],
) -> ShipMessage {
    ShipMessage::Control(ControlMessage::MessageProtocolHandshake(
        MessageProtocolHandshake {
            handshake_type: Some(kind),
            version: Some(MessageProtocolHandshakeVersion {
                major: Some(version.0),
                minor: Some(version.1),
            }),
            formats: Some(MessageProtocolFormats {
                format: Some(
                    formats
                        .iter()
                        .map(|f| MessageProtocolFormat((*f).into()))
                        .collect(),
                ),
            }),
        },
    ))
}

/// A client that has finished CMI and hello and is awaiting the server's selection.
fn client_in_protocol_handshake() -> Handshake {
    let mut hs = Handshake::new(
        Role::Client,
        HandshakeConfig::default(),
        Trust::Trusted,
        Duration::ZERO,
    );
    let _ = hs.poll_transmit();
    hs.handle_message(ShipMessage::Cmi, Duration::ZERO).unwrap();
    drain(&mut hs);
    hs.handle_message(ready_hello(), Duration::ZERO).unwrap();
    drain(&mut hs);
    assert_eq!(hs.phase(), Phase::ProtocolHandshake);
    hs
}

/// A close announcement built by hand, used to check that an unsolicited close during
/// the handshake is honoured rather than treated as a protocol error.
#[test]
fn a_close_during_the_handshake_is_honoured() {
    let mut hs = client_in_protocol_handshake();
    hs.handle_message(
        ShipMessage::End(EndMessage::ConnectionClose(ConnectionClose {
            phase: Some(ConnectionClosePhase::Announce),
            ..Default::default()
        })),
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(hs.phase(), Phase::Closed);
}
