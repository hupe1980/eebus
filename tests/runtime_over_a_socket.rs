//! The whole stack over a real socket.
//!
//! TCP, TLS 1.2 with mutual authentication, the WebSocket upgrade, the SHIP handshake and
//! a SPINE datagram — on loopback, between two nodes that have never met. Everything
//! below the socket is a state machine tested against a virtual clock elsewhere; this is
//! the test that says the state machines are wired to a socket correctly.

#![cfg(feature = "runtime")]

use core::time::Duration;

use eebus::cert::{self, CertParams};
use eebus::model::{
    CmdClassifier, Datagram, DeviceType, EntityType, FeatureType, Function, Header, MsgCounter,
    Role, SpecificationVersion,
};
use eebus::runtime::{Node, TrustStore, reconnect_delay};
use eebus::ship::Ski;
use eebus::spine::{Engine, LocalDevice, LocalEntity, LocalFeature, node_management};
use eebus::tls::ShipTls;

/// A node with a fresh identity.
fn node(ship_id: &str, trust: TrustStore) -> Node {
    let identity = cert::self_signed(CertParams::new(ship_id)).expect("a certificate");
    Node::new(ship_id, ShipTls::new(identity), trust)
}

/// Two nodes that already trust each other, as after commissioning.
fn pair() -> (Node, Node) {
    let box_trust = TrustStore::new();
    let pump_trust = TrustStore::new();
    let control_box = node("i:46925_u:ControlBox-1", box_trust.clone());
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    box_trust.trust(heat_pump.ski());
    pump_trust.trust(control_box.ski());
    (control_box, heat_pump)
}

fn a_read() -> Datagram {
    let device = eebus::spine::device_address("i:46925", "ControlBox-1").unwrap();
    let source = node_management(&device);
    Datagram {
        header: Some(Header {
            specification_version: Some(SpecificationVersion::from("1.3.0")),
            address_source: Some(source),
            msg_counter: Some(MsgCounter(1)),
            cmd_classifier: Some(CmdClassifier::Read),
            ..Default::default()
        }),
        payload: Some(Default::default()),
    }
}

/// The whole stack, end to end, on loopback.
#[tokio::test]
async fn two_nodes_meet_over_a_socket_and_exchange_a_datagram() {
    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let box_ski = control_box.ski();
    let pump_ski = heat_pump.ski();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = heat_pump.accept(stream).await.expect("the handshake");
        assert_eq!(
            connection.peer(),
            box_ski,
            "the dialling node identified itself"
        );
        let received = connection.recv().await.expect("a datagram");
        connection.send(&received).await.expect("the echo");
        connection
    });

    let mut client = control_box.connect(address).await.expect("the handshake");
    assert_eq!(client.peer(), pump_ski, "and so did the answering one");
    assert_eq!(
        client.peer_ship_id(),
        None,
        "the access-methods answer has not arrived yet, and the implementation guide \
         §2.1 forbids waiting for it"
    );

    let sent = a_read();
    client.send(&sent).await.expect("the read");
    let echoed = client.recv().await.expect("the answer");
    assert_eq!(echoed, sent, "the datagram survived the whole stack");

    assert_eq!(
        client.peer_ship_id(),
        Some("i:46925_u:HeatPump-1"),
        "and by now it has: SHIP 1.1.0 makes `accessMethods.id` the peer's SHIP ID"
    );

    server.await.unwrap();
}

/// A SPINE engine driven over the socket, rather than a hand-built datagram: the control
/// box discovers what the heat pump is, across TLS and a WebSocket.
#[tokio::test]
async fn discovery_runs_across_a_real_connection() {
    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut connection = heat_pump.accept(stream).await.expect("the handshake");

        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    .with_feature(LocalFeature::new(1, FeatureType::LoadControl, Role::Server)),
            )
            .unwrap();
        let mut engine = Engine::new(device);

        // One read in, one reply out.
        let datagram = connection.recv().await.expect("the read");
        engine.handle_datagram(&datagram, Duration::ZERO);
        while let Some(answer) = engine.poll_transmit() {
            connection.send(&answer).await.expect("the reply");
        }
    });

    let mut client = control_box.connect(address).await.expect("the handshake");

    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::CEM).with_feature(LocalFeature::new(
                1,
                FeatureType::Generic,
                Role::Client,
            )),
        )
        .unwrap();
    let mut engine = Engine::new(device);

    let peer = eebus::spine::device_address("i:46925", "HeatPump-1").unwrap();
    let source = node_management(engine.device().address());
    engine.read(
        &node_management(&peer),
        &source,
        Function::NodeManagementDetailedDiscoveryData,
        Duration::ZERO,
    );
    while let Some(datagram) = engine.poll_transmit() {
        client.send(&datagram).await.unwrap();
    }

    let reply = client.recv().await.expect("the discovery reply");
    engine.handle_datagram(&reply, Duration::ZERO);

    let remote = engine.peer(&peer).expect("the heat pump was discovered");
    let entity = remote.entity(&[1]).expect("its appliance entity");
    assert!(
        entity
            .feature(&FeatureType::LoadControl, Role::Server)
            .is_some(),
        "and the feature a limit would be written to"
    );

    server.await.unwrap();
}

/// A peer nobody has approved does not get to exchange data. The connection is made — it
/// has to be, so a user can be shown the SKI — but the handshake holds in the pending
/// state until its timer runs out.
#[tokio::test]
async fn an_unapproved_peer_does_not_reach_the_data_phase() {
    let control_box = node("i:46925_u:ControlBox-1", TrustStore::new());
    let pump_trust = TrustStore::new();
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    // The heat pump trusts the control box; the control box has approved nobody.
    pump_trust.trust(control_box.ski());

    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        heat_pump.accept(stream).await
    });

    let opened =
        tokio::time::timeout(Duration::from_millis(500), control_box.connect(address)).await;
    assert!(
        opened.is_err(),
        "an unapproved peer waits rather than exchanging data"
    );
    server.abort();
}

/// [B1] A consumer driving `accept` itself can see, and answer, the peer that is waiting.
///
/// The SKI is proved by then — the peer completed TLS — and the SHIP pending state exists
/// so that a person can compare it with the label on the box. A driver that owns its own
/// engine, because it has to be testable without a socket, cannot use `Hub` to learn it;
/// this is the same thing without one.
#[tokio::test]
async fn a_pending_peer_is_reported_and_can_be_approved_without_a_hub() {
    use std::sync::Arc;

    let control_box = node("i:46925_u:ControlBox-1", TrustStore::new());
    let pump_trust = TrustStore::new();
    let heat_pump = Arc::new(node("i:46925_u:HeatPump-1", pump_trust.clone()));
    let box_ski = control_box.ski();
    let box_fingerprint = control_box.fingerprint();
    // The control box has approved the pump; the pump has approved nobody, so the only
    // outstanding decision is the one a person is about to be asked.
    control_box.trust_store().trust(heat_pump.ski());

    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let node = heat_pump.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        node.accept_reporting(
            stream,
            Some(Box::new(move |peer| {
                let _ = tx.send(peer);
            })),
        )
        .await
    });

    let dialling = tokio::spawn(async move { control_box.connect(address).await });

    let reported = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the waiting peer was reported")
        .expect("a peer");
    assert_eq!(reported.ski, box_ski, "the SKI its certificate proved");
    assert_eq!(
        reported.fingerprint, box_fingerprint,
        "and the fingerprint a QR code would carry"
    );

    // The same peer is listed for as long as it waits, for a caller that would rather ask.
    assert_eq!(
        heat_pump.pending_peers(),
        vec![reported],
        "an installer's screen reads this"
    );

    // A person says yes.
    pump_trust.trust(box_ski);
    let connection = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the handshake completed")
        .expect("the task did not panic")
        .expect("approving it let it through");
    assert_eq!(connection.peer(), box_ski);
    assert!(
        heat_pump.pending_peers().is_empty(),
        "and it is no longer waiting on anybody"
    );
    dialling.abort();
}

/// A peer that gives up while waiting leaves nothing behind.
#[tokio::test]
async fn a_pending_peer_that_goes_away_is_not_listed_for_ever() {
    use std::sync::Arc;

    let control_box = node("i:46925_u:ControlBox-1", TrustStore::new());
    let heat_pump = Arc::new(node("i:46925_u:HeatPump-1", TrustStore::new()));

    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let node = heat_pump.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = node
            .accept_reporting(
                stream,
                Some(Box::new(move |peer| {
                    let _ = tx.send(peer);
                })),
            )
            .await;
    });

    let dialling = tokio::spawn(async move { control_box.connect(address).await });
    let reported = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("reported")
        .expect("a peer");
    assert_eq!(heat_pump.pending_peers(), vec![reported]);

    // The far end walks away mid-decision.
    dialling.abort();
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    assert!(
        heat_pump.pending_peers().is_empty(),
        "a decision nobody is waiting for is worse than no list at all"
    );
}

/// The trust store is what a user approving a SKI amounts to.
#[test]
fn the_trust_store_holds_what_a_user_approved() {
    let ski: Ski = "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap();
    let store = TrustStore::new();
    assert!(!store.is_trusted(&ski));

    store.trust(ski);
    assert!(store.is_trusted(&ski));
    assert_eq!(store.all(), alloc_vec(ski));

    store.forget(&ski);
    assert!(!store.is_trusted(&ski));

    // And a store restored from disk trusts what it held.
    assert!(TrustStore::with([ski]).is_trusted(&ski));
}

fn alloc_vec(ski: Ski) -> Vec<Ski> {
    vec![ski]
}

/// Backing off after a failure: quick at first, then not hammering a device that is down.
#[test]
fn reconnecting_backs_off_and_stops_growing() {
    assert_eq!(reconnect_delay(0), Duration::from_secs(1));
    assert_eq!(reconnect_delay(1), Duration::from_secs(2));
    assert_eq!(reconnect_delay(4), Duration::from_secs(16));
    assert_eq!(reconnect_delay(7), Duration::from_secs(120), "capped");
    assert_eq!(
        reconnect_delay(1_000),
        Duration::from_secs(120),
        "and stays capped rather than overflowing"
    );
}

// ---- the Hub ---------------------------------------------------------------------

use eebus::runtime::{ConnectionError, Disconnect, Hub, HubEvent, Origin};
use eebus::ship::ConnectionCloseReason;

/// Drives `hub` until `pick` accepts an event, or `budget` runs out.
///
/// The deadline goes to `wake_at`, not around `hub.next()`: `next` is not cancel-safe, and
/// a test that models the misuse is a test people copy. A tick that arrives early — the
/// hub uses `wake_at` for its own arbitration too — is re-armed rather than mistaken for
/// the deadline.
async fn wait_for<T>(
    hub: &mut Hub,
    budget: Duration,
    mut pick: impl FnMut(&mut Hub, HubEvent) -> Option<T>,
) -> T {
    let deadline = hub.now() + budget;
    loop {
        hub.wake_at(deadline);
        let event = hub.next().await.expect("no transport error");
        if let HubEvent::Tick = event
            && hub.now() >= deadline
        {
            panic!("nothing acceptable happened within {budget:?}");
        }
        if let Some(found) = pick(hub, event) {
            return found;
        }
    }
}

/// The heat pump, with the features LPC asks of a Controllable System.
fn pump_engine() -> Engine {
    use eebus::usecases::{limitation, lpc};

    let mut device =
        LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::device_diagnosis_client_feature(5)),
        )
        .unwrap();
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
    engine
}

/// The control box, with the Energy Guard's single `Generic` client feature.
fn box_engine() -> Engine {
    use eebus::usecases::{limitation, lpc};

    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard)
                .with_feature(limitation::client_feature(1))
                .with_feature(limitation::device_diagnosis_feature(2)),
        )
        .unwrap();
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
    engine
}

/// Runs a hub's loop in the background for as long as the test needs it.
fn serve(mut hub: Hub) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if hub.next().await.is_err() {
                return;
            }
        }
    })
}

/// The hub does the pre-scenario work an application would otherwise write by hand:
/// listen, dial, run the handshakes off the loop, discover, learn the peer's device
/// address, and route by it.
#[tokio::test]
async fn the_hub_discovers_a_peer_and_routes_to_it() {
    let (control_box, heat_pump) = pair();
    let pump_ski = heat_pump.ski();

    let mut server = Hub::new(heat_pump, pump_engine());
    let address = server.listen("127.0.0.1:0").await.unwrap();
    let server = serve(server);

    let mut hub = Hub::new(control_box, box_engine());
    hub.dial(address);

    let ski = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::Connected { ski, .. } => Some(ski),
        HubEvent::HandshakeFailed { error, .. } => panic!("the handshake failed: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(ski, pump_ski, "the answering node identified itself");

    let device = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::PeerDiscovered { device, .. } => Some(device),
        HubEvent::Disconnected { .. } => panic!("the connection ended early"),
        _ => None,
    })
    .await;

    assert_eq!(device.as_str(), "d:_i:46925_HeatPump-1");
    assert_eq!(hub.ski_of(&device), Some(ski));
    assert_eq!(hub.device_of(&ski), Some(&device));

    let remote = hub.engine().peer(&device).expect("the peer");
    assert!(
        remote
            .use_case("limitationOfPowerConsumption", "ControllableSystem")
            .is_some(),
        "use-case discovery ran too, not just detailed discovery"
    );

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    server.abort();
}

/// A peer nobody has approved is reported, and approving it completes the handshake it
/// is waiting in — no reconnection, no timeout, no forty hex digits typed in advance.
///
/// This is the commissioning flow SHIP §13.4.4.1 describes: the peer completes TLS so its
/// SKI is proven, is told `hello: pending`, and a person decides. The heat pump here is
/// fresh out of the box and trusts nobody; the control box already trusts it.
#[tokio::test]
async fn an_unapproved_peer_is_reported_and_approved_interactively() {
    let pump_trust = TrustStore::new();
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    let control_box = node(
        "i:46925_u:ControlBox-1",
        TrustStore::with([heat_pump.ski()]),
    );
    let box_ski = control_box.ski();

    let mut pump = Hub::new(heat_pump, pump_engine());
    let address = pump.listen("127.0.0.1:0").await.unwrap();

    let mut control = Hub::new(control_box, box_engine());
    control.dial(address);
    let control = serve(control);

    let (asked, origin) = wait_for(&mut pump, Duration::from_secs(5), |_, event| match event {
        HubEvent::TrustRequested { peer, origin } => Some((peer, origin)),
        HubEvent::Connected { .. } => panic!("an unapproved peer reached the data phase"),
        _ => None,
    })
    .await;
    assert_eq!(
        asked.ski, box_ski,
        "the SKI the certificate proved, not a claim"
    );
    assert!(matches!(origin, Origin::Accepted { .. }), "it dialled us");
    assert!(
        !pump_trust.is_trusted(&box_ski),
        "nothing was decided by the hub"
    );

    // The user says yes. The handshake that was waiting continues on the same socket.
    pump.approve(box_ski);
    assert!(
        pump_trust.is_trusted(&box_ski),
        "and the store records it, for persisting"
    );

    let connected = wait_for(&mut pump, Duration::from_secs(5), |_, event| match event {
        HubEvent::Connected { ski, .. } => Some(ski),
        HubEvent::HandshakeFailed { error, .. } => panic!("the handshake failed: {error}"),
        _ => None,
    })
    .await;
    assert_eq!(connected, box_ski);
    let _ = wait_for(&mut pump, Duration::from_secs(5), |_, event| match event {
        HubEvent::PeerDiscovered { device, .. } => Some(device),
        _ => None,
    })
    .await;

    pump.shutdown(ConnectionCloseReason::Unspecific).await;
    control.abort();
}

/// The other answer: a refused peer is told `hello: aborted`, and both sides report the
/// handshake as failed rather than letting it time out.
#[tokio::test]
async fn a_refused_peer_is_told_so_and_the_handshake_ends() {
    let heat_pump = node("i:46925_u:HeatPump-1", TrustStore::new());
    let control_box = node(
        "i:46925_u:ControlBox-1",
        TrustStore::with([heat_pump.ski()]),
    );
    let box_ski = control_box.ski();

    let mut pump = Hub::new(heat_pump, pump_engine());
    let address = pump.listen("127.0.0.1:0").await.unwrap();
    let mut control = Hub::new(control_box, box_engine());
    control.dial(address);

    // Drive both hubs from one task, so the refusal can be observed on each side.
    let mut asked = false;
    let mut pump_failed = None;
    let mut box_failed = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while (pump_failed.is_none() || box_failed.is_none()) && std::time::Instant::now() < deadline {
        pump.wake_at(pump.now() + Duration::from_millis(50));
        match pump.next().await.expect("no error") {
            HubEvent::TrustRequested { peer, .. } => {
                assert_eq!(peer.ski, box_ski);
                asked = true;
                pump.refuse(peer.ski);
            }
            HubEvent::HandshakeFailed { error, ski, .. } => {
                assert_eq!(
                    ski,
                    Some(box_ski),
                    "the failure names the peer that was refused: it proved an identity \
                     in TLS, and without it the event cannot be told from a peer that \
                     never presented a certificate"
                );
                pump_failed = Some(error);
            }
            HubEvent::Connected { .. } => panic!("a refused peer reached the data phase"),
            _ => {}
        }
        control.wake_at(control.now() + Duration::from_millis(50));
        match control.next().await.expect("no error") {
            HubEvent::HandshakeFailed { error, .. } => box_failed = Some(error),
            HubEvent::Connected { .. } => panic!("the refusing peer opened data exchange"),
            _ => {}
        }
    }

    assert!(asked, "the pump was asked");
    let pump_failed = pump_failed.expect("the pump reported the refusal");
    assert!(
        matches!(
            *pump_failed,
            ConnectionError::Aborted(eebus::ship::AbortReason::TrustRejected)
        ),
        "the pump's own refusal: {pump_failed}"
    );
    let box_failed = box_failed.expect("the control box heard about it");
    assert!(
        matches!(
            *box_failed,
            ConnectionError::Aborted(eebus::ship::AbortReason::PeerAborted)
        ),
        "the control box was told, not left to time out: {box_failed}"
    );
    assert!(
        !pump.node().trust_store().is_trusted(&box_ski),
        "and nothing was recorded"
    );
}

/// A remembered peer is dialled, and dialled again when the connection drops.
///
/// This is what keeps a §14a installation working across a router reboot: nothing tells
/// the hub the peer is back, so it has to keep asking — with a backoff, so it does not
/// hammer a device that is genuinely gone. And it dials in the background: a peer that is
/// down never holds up the connections that are up.
#[tokio::test]
async fn a_remembered_peer_is_dialled_and_redialled() {
    let (control_box, heat_pump) = pair();
    let pump_ski = heat_pump.ski();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        // The first connection is dropped without a word, which is what a reboot looks
        // like from the far end. The second is held open.
        let (stream, _) = listener.accept().await.expect("a caller");
        drop(heat_pump.accept(stream).await.expect("the handshake"));

        let (stream, _) = listener.accept().await.expect("a second caller");
        let held = heat_pump.accept(stream).await.expect("the handshake again");
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(held);
    });

    let mut hub = Hub::new(control_box, box_engine());
    hub.remember(pump_ski, address);
    assert_eq!(hub.remembered().count(), 1);

    let mut connections = 0;
    let mut disconnections = 0;
    let deadline = hub.now() + Duration::from_secs(20);
    while connections < 2 && hub.now() < deadline {
        hub.wake_at(deadline);
        match hub.next().await {
            Ok(HubEvent::Connected { ski, version }) => {
                assert_eq!(ski, pump_ski);
                assert_eq!(
                    version,
                    Some(eebus::ship::ShipVersion::V1_1),
                    "a reconnection reports the version it settled on"
                );
                connections += 1;
            }
            Ok(HubEvent::Disconnected { .. }) => disconnections += 1,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    assert_eq!(
        connections, 2,
        "the hub dialled, lost the peer, and dialled again"
    );
    assert!(disconnections >= 1, "and noticed the loss in between");

    hub.forget_peer(&pump_ski);
    assert_eq!(hub.remembered().count(), 0);
    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}

/// A dial that finds nobody is reported, not waited for.
#[tokio::test]
async fn a_dial_that_finds_nobody_is_reported() {
    let (control_box, _) = pair();
    let mut hub = Hub::new(control_box, box_engine());
    // A port nothing listens on: bind one, learn it, and let it go.
    let unused = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };
    hub.dial(unused);
    let (origin, error) = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::HandshakeFailed { origin, error, .. } => Some((origin, error)),
        _ => None,
    })
    .await;
    assert_eq!(origin, Origin::Dialed { address: unused });
    assert!(matches!(*error, ConnectionError::Io(_)), "{error}");
}

/// SHIP §12.1.3 over a real connection: a peer renews its certificate, and the trust
/// follows without anybody rescanning a QR code.
///
/// This is the whole point of the mechanism. Certificate lifetimes are shortening; a
/// stack that cannot carry a renewal turns every renewal into a site visit.
#[tokio::test]
async fn a_certificate_renewal_updates_the_trust_store() {
    use eebus::ship::OwnKeys;

    let (control_box, heat_pump) = pair();
    let pump_ski = heat_pump.ski();
    let successor = Ski::from_bytes([0x5A; 20]);

    // The heat pump is mid-renewal: it still uses the certificate the control box
    // trusts, and announces the one it will move to.
    let mut keys = OwnKeys::new(pump_ski);
    keys.begin_update(successor).unwrap();
    let heat_pump = heat_pump.key_material(keys.clone());

    let mut server = Hub::new(heat_pump, pump_engine());
    let address = server.listen("127.0.0.1:0").await.unwrap();
    let server = serve(server);

    let trust = control_box.trust_store().clone();
    let mut hub = Hub::new(control_box, box_engine());
    hub.dial(address);

    let updated = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::PeerKeysUpdated { ski, trusted, .. } => Some((ski, trusted)),
        HubEvent::Disconnected { .. } => panic!("the connection ended early"),
        _ => None,
    })
    .await;

    assert_eq!(updated.0, pump_ski);
    assert!(
        updated.1.contains(&successor),
        "the successor arrived: {:?}",
        updated.1
    );
    assert!(
        trust.is_trusted(&successor),
        "and the trust store took it up, so the next connection works"
    );
    assert!(
        trust.is_trusted(&pump_ski),
        "while the certificate in use is still trusted"
    );
    assert_eq!(
        hub.peer_keys(&pump_ski)
            .map(eebus::ship::PeerKeys::update_counter),
        Some(keys.update_counter()),
        "and the counter is stored, so a restart does not ask again"
    );

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    server.abort();
}

/// Two peers cannot hold the same SPINE device address on one hub.
///
/// Routing is by that address, so the second claimant would take delivery of the first
/// one's datagrams — a limit meant for one heat pump arriving at another. Two devices
/// sharing a vendor and serial produce it without any malice, so it is reported.
#[tokio::test]
async fn a_second_peer_cannot_claim_the_first_peer_s_device_address() {
    let box_trust = TrustStore::new();
    let control_box = node("i:46925_u:ControlBox-1", box_trust.clone());

    // Two heat pumps with distinct identities but — the misconfiguration — the same SPINE
    // device address, which is what `pump_engine` builds.
    let mut addresses = Vec::new();
    let mut servers = Vec::new();
    for name in ["i:46925_u:HeatPump-A", "i:46925_u:HeatPump-B"] {
        let trust = TrustStore::new();
        let pump = node(name, trust.clone());
        trust.trust(control_box.ski());
        box_trust.trust(pump.ski());
        let mut server = Hub::new(pump, pump_engine());
        addresses.push(server.listen("127.0.0.1:0").await.unwrap());
        servers.push(serve(server));
    }

    let mut hub = Hub::new(control_box, box_engine());
    hub.dial(addresses[0]);
    hub.dial(addresses[1]);

    // Which of the two answers discovery first is a race, and immaterial: whichever
    // binds the address keeps it, and the other is closed.
    let mut connected = Vec::new();
    let mut discovered = Vec::new();
    let mut conflicted = Vec::new();
    let deadline = hub.now() + Duration::from_secs(5);
    while (discovered.is_empty() || conflicted.is_empty()) && hub.now() < deadline {
        hub.wake_at(deadline);
        match hub.next().await {
            Ok(HubEvent::Connected { ski, .. }) => connected.push(ski),
            Ok(HubEvent::PeerDiscovered { ski, device }) => discovered.push((ski, device)),
            Ok(HubEvent::Disconnected { ski, reason }) => {
                if reason == Disconnect::AddressConflict {
                    conflicted.push(ski);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    assert_eq!(connected.len(), 2, "both handshakes completed");
    assert_eq!(
        discovered.len(),
        1,
        "exactly one peer holds the address, saw {discovered:?}"
    );
    assert_eq!(
        conflicted.len(),
        1,
        "and the other was reported as a conflict, saw {conflicted:?}"
    );
    let (holder, device) = &discovered[0];
    assert_ne!(conflicted[0], *holder, "the holder was not the one closed");
    assert!(connected.contains(holder));
    assert!(connected.contains(&conflicted[0]));
    assert_eq!(
        hub.ski_of(device),
        Some(*holder),
        "the address still routes to the peer that bound it"
    );

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    for server in servers {
        server.abort();
    }
}

// ---- the §14a exchange, over a socket -----------------------------------------

/// From a socket to an accepted limit write.
///
/// Everything else in this file checks one layer. This checks the thing a consumer
/// actually wants and cannot assemble from the parts: a control box dials a household
/// appliance, discovery runs, the bindings and the heartbeat subscription settle, and a
/// limit is written and *accepted* — not accepted because the test arranged the
/// preconditions, but because the two actors arranged them between themselves over a real
/// connection.
///
/// It is deliberately the whole path. The in-memory pair in `limitation_both_actors.rs`
/// checks the same rules with the transport removed; if that passes and this does not,
/// what is wrong is the wiring, and the wiring is where a subscription silently not
/// arriving would hide.
#[tokio::test]
async fn a_limit_arrives_over_a_socket_and_is_accepted() {
    use eebus::usecases::limitation::{
        self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, CsFeatures,
        EnergyGuardActor, GuardEvent, LimitWrite, LimitationState,
    };
    use eebus::usecases::lpc;

    let (control_box, heat_pump) = pair();

    // ---- the appliance: listens, and applies what it is sent ------------------
    let mut device =
        LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::device_diagnosis_client_feature(5))
                .with_feature(limitation::electrical_connection_feature(4)),
        )
        .unwrap();
    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);
    let diagnosis_client = device.address_of(&[1], 5);
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
    let actor = ControllableSystemActor::builder(
        ControllableSystem::new(
            CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)).with_nominal_max(11_000.0),
            Duration::ZERO,
        ),
        lpc::DIRECTION,
        CsFeatures {
            load_control,
            device_configuration: configuration,
            device_diagnosis: diagnosis,
            device_diagnosis_client: diagnosis_client,
        },
    )
    .with_electrical_connection(electrical);

    let mut hub = Hub::new(heat_pump, engine);
    let address = hub.listen("127.0.0.1:0").await.unwrap();
    let now = hub.now();
    let mut actor = actor.install(hub.engine_mut(), now);

    let appliance = tokio::spawn(async move {
        let mut decided = None;
        for _ in 0..256 {
            hub.wake_at(actor.poll_timeout());
            let now = hub.now();
            let Ok(event) = hub.next().await else { break };
            match event {
                HubEvent::Spine(event) => {
                    if let Some(CsEvent::LimitDecided { write, outcome, .. }) =
                        actor.handle_event(hub.engine_mut(), &event, now)
                    {
                        // Deciding is not answering: the acknowledgement is queued on the
                        // engine, and the loop has to keep going for it to reach the wire.
                        // Breaking here is how a real application loses an ACK.
                        decided = Some((write, outcome));
                    }
                }
                HubEvent::Tick => {
                    actor.handle_timeout(hub.engine_mut(), now);
                }
                HubEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
        (
            decided,
            actor.system().state(),
            actor.system().effective_limit(),
        )
    });

    // ---- the control box: dials, and writes the limit -------------------------
    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard)
                .with_feature(limitation::client_feature(1))
                .with_feature(limitation::device_diagnosis_feature(2)),
        )
        .unwrap();
    let client = device.address_of(&[1], 1);
    let diagnosis = device.address_of(&[1], 2);
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);

    let mut guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, Duration::ZERO);
    let mut hub = Hub::new(control_box, engine);
    hub.dial(address);

    let mut accepted = None;
    let mut refusals = 0;
    for _ in 0..256 {
        hub.wake_at(guard.poll_timeout());
        let now = hub.now();
        let Ok(event) = hub.next().await else { break };
        let mut reports = Vec::new();
        match event {
            HubEvent::PeerDiscovered { device, .. } => {
                let remote = hub.engine().peer(&device).expect("just discovered");
                assert!(
                    limitation::locate(remote, lpc::DIRECTION).is_some(),
                    "it plays the use case"
                );
                guard.require(&device, Some(LimitWrite::active(3_000.0)), now);
            }
            HubEvent::Spine(event) => {
                reports.extend(guard.handle_event(hub.engine_mut(), &event, now))
            }
            HubEvent::Tick => reports = guard.handle_timeout(hub.engine_mut(), now),
            HubEvent::HandshakeFailed { error, .. } => panic!("the dial failed: {error}"),
            HubEvent::Disconnected { .. } => break,
            _ => {}
        }
        for report in reports {
            match report {
                GuardEvent::LimitAccepted { limit, .. } if limit.is_active => {
                    accepted = Some(limit);
                }
                GuardEvent::LimitRefused { .. } => refusals += 1,
                _ => {}
            }
        }
        if accepted.is_some() {
            break;
        }
    }

    let accepted = accepted.expect("the limit was never accepted over the socket");
    assert_eq!(accepted.watts, 3_000.0);
    assert_eq!(
        refusals, 0,
        "the actors arrange the heartbeat and the bindings themselves; a refusal here \
         means one of them wrote before the other could hear it"
    );

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    let (decided, state, effective) = tokio::time::timeout(Duration::from_secs(5), appliance)
        .await
        .expect("the appliance finished")
        .expect("without panicking");

    let (write, outcome) = decided.expect("the appliance decided a limit");
    assert_eq!(write.watts, 3_000.0);
    assert!(outcome.is_accepted());
    assert_eq!(state, LimitationState::Limited);
    assert_eq!(
        effective,
        eebus::usecases::limitation::EffectiveLimit::Active(3_000.0)
    );
}

/// A hub with no room turns a connection away instead of taking it.
///
/// SHIP puts no cap on connections, and the omission is exploitable: a device on the LAN
/// that dials in repeatedly takes the memory of every node that answers, and the node
/// that runs out is the one that stops serving the peers it already had. The cap counts
/// handshakes in progress as well as connections held, because a peer sitting in the
/// pending state has taken a slot too.
#[tokio::test]
async fn a_full_hub_refuses_a_connection_rather_than_growing() {
    let (control_box, heat_pump) = pair();
    let mut server = Hub::new(heat_pump, pump_engine());
    let address = server.listen("127.0.0.1:0").await.unwrap();
    let server = serve(server);

    let mut hub = Hub::new(control_box, box_engine());
    hub.set_max_connections(1);
    assert_eq!(hub.max_connections(), 1);

    hub.dial(address);
    wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::Connected { .. } => Some(()),
        HubEvent::HandshakeFailed { error, .. } => panic!("the first dial failed: {error}"),
        _ => None,
    })
    .await;

    // The second is refused by this side.
    hub.dial(address);
    let error = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::HandshakeFailed { error, .. } => Some(error),
        HubEvent::Connected { .. } => panic!("a full hub took a second connection"),
        _ => None,
    })
    .await;
    assert!(
        matches!(*error, ConnectionError::TooManyConnections),
        "{error}"
    );
    assert_eq!(hub.peers().count(), 1, "the refused dial was not adopted");

    // And so is a socket accepted with no room for it: dropped before TLS, so it costs
    // the hub nothing.
    let (stream, _) = tokio::io::duplex(1);
    drop(stream);
    let extra = tokio::net::TcpStream::connect(address).await.unwrap();
    hub.accept(extra);
    let error = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::HandshakeFailed { error, .. } => Some(error),
        _ => None,
    })
    .await;
    assert!(matches!(*error, ConnectionError::TooManyConnections));

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    server.abort();
}

/// A `next` future cancelled mid-write is noticed, and the connection goes.
///
/// This is D33's hazard made deterministic. Cancelling `Hub::next` while it is writing
/// leaves a partial WebSocket frame on the wire; the peer's parser is then out of step
/// with the stream and every message after it is misread, invisibly from this side. The
/// hub cannot repair that — but it can *know*, and closing the connection turns a session
/// that quietly stops working into a reconnection.
///
/// The peer here answers discovery and then stops reading, so the socket buffers fill and
/// the write is genuinely parked at an `await` rather than racing to completion.
#[tokio::test]
async fn a_write_interrupted_by_a_cancelled_next_closes_the_connection() {
    let (control_box, heat_pump) = pair();

    // A peer that answers discovery and then stops reading entirely. The connection stays
    // open — it is the *reading* that stops, which is what fills the buffers.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let mut server = Hub::new(heat_pump, pump_engine());
    let address = server.listen("127.0.0.1:0").await.unwrap();
    let server = tokio::spawn(async move {
        while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
            server.wake_at(server.now() + Duration::from_millis(20));
            if server.next().await.is_err() {
                break;
            }
        }
        core::future::pending::<()>().await;
    });

    let mut hub = Hub::new(control_box, box_engine());
    hub.dial(address);
    let device = wait_for(&mut hub, Duration::from_secs(5), |_, event| match event {
        HubEvent::PeerDiscovered { device, .. } => Some(device),
        HubEvent::Disconnected { .. } => panic!("the connection ended early"),
        _ => None,
    })
    .await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Enough traffic that the kernel and TLS buffers cannot swallow it all.
    let target = eebus::spine::feature_address(&device, &[1], 1);
    let source = hub.engine().device().address_of(&[1], 1);
    for _ in 0..50_000 {
        let now = hub.now();
        hub.engine_mut().read(
            &target,
            &source,
            eebus::model::Function::LoadControlLimitListData,
            now,
        );
    }

    // Cancel `next` while it is parked in that write. The peer is not reading, so this is
    // not a race: the future cannot make progress and the timeout must win.
    let cancelled = tokio::time::timeout(Duration::from_millis(500), hub.next()).await;
    assert!(
        cancelled.is_err(),
        "the write completed, so nothing was cancelled and the test proves nothing"
    );

    // The very next call has to report it rather than carry on over a stream the peer can
    // no longer parse.
    let event = tokio::time::timeout(Duration::from_secs(5), hub.next())
        .await
        .expect("the hub answered")
        .expect("no transport error");
    assert!(
        matches!(
            event,
            HubEvent::Disconnected {
                reason: Disconnect::InterruptedWrite,
                ..
            }
        ),
        "expected the interrupted write to be reported, saw {event:?}"
    );
    assert_eq!(hub.peers().count(), 0, "and the connection is gone");

    server.abort();
}

// ---- the SHIP Pairing Service ------------------------------------------------------

/// A control unit paired by certificate reaches the data phase without anybody having
/// approved a SKI.
///
/// Pairing Service §10.2 in one assertion: "the fingerprint of the certificate received
/// during the TLS handshake will be calculated instead and verified with the expected
/// fingerprint of the shippairing request. If the fingerprints match, this SHALL be seen
/// like a successful trust in an SKI". Nothing about `devZ`'s SKI is known to `devA`
/// beforehand — it cannot be, since it is not in the request — so a store that admitted
/// only SKIs would leave this connection in the pending state until it timed out.
#[tokio::test]
async fn a_unit_paired_by_certificate_is_let_through_without_its_ski() {
    use eebus::ship::pairing::{PairingRequest, PairingSecret, Receiver};

    // `devA`: the household energy manager, with a secret printed on its label.
    let secret = PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap();
    let manager_trust = TrustStore::new();
    let manager = node("i:46925_u:HeatPump-1", manager_trust.clone());

    // `devZ`: the control unit an installer has configured from `devA`'s QR code.
    let unit_trust = TrustStore::new();
    let unit = node("i:983327_u:ControlUnit-1", unit_trust.clone());
    unit_trust.trust(manager.ski());
    let (manager_fingerprint, unit_fingerprint) = (manager.fingerprint(), unit.fingerprint());

    let request = PairingRequest::new(
        manager.ship_id(),
        manager.fingerprint(),
        unit.ship_id(),
        unit.fingerprint(),
        "BDCEE427FA7208DF3C1F2A749BA6F4D4".parse().unwrap(),
    );
    let pairs = request.sign(&secret).unwrap().to_pairs();

    // `devA` evaluates the announcement it would have heard over mDNS…
    let mut receiver = Receiver::new(manager.ship_id(), manager.fingerprint(), secret);
    let accepted = receiver.evaluate(&pairs).expect("an authentic request");
    assert_eq!(accepted.trust_id, unit.ship_id());
    assert!(
        !manager_trust.is_trusted(&unit.ski()),
        "the request never named a SKI, and none was invented"
    );
    manager_trust.trust_unit(eebus::runtime::PairedUnit::from_request(&accepted));

    // …and the unit is admitted on the strength of the certificate alone.
    let listener = manager.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let unit_ski = unit.ski();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        manager.accept(stream).await.expect("the handshake")
    });
    let client = unit.connect(address).await.expect("the handshake");
    let served = server.await.unwrap();
    assert_eq!(served.peer(), unit_ski);
    assert_eq!(
        served.peer_fingerprint(),
        unit_fingerprint,
        "the certificate the request named is the one that turned up"
    );
    assert_eq!(client.peer_fingerprint(), manager_fingerprint);

    // §10.2's note: the SKI is recorded once it has been proved, so that everything else
    // — routing, the redial schedule, what a user is shown — can name the unit.
    assert_eq!(manager_trust.unit().unwrap().ski, Some(unit_ski));
    assert!(manager_trust.is_trusted(&unit_ski));
}

/// §10.3: a node considers at most one control unit, and pairing a second revokes the
/// first — including the SKI it had been reached under, or the old unit would still be
/// able to connect.
#[test]
fn pairing_a_second_control_unit_untrusts_the_first() {
    use eebus::runtime::PairedUnit;
    use eebus::ship::Fingerprint;

    let first_ski: Ski = "1111111111111111111111111111111111111111".parse().unwrap();
    let store = TrustStore::new();
    let first = PairedUnit {
        ship_id: "i:983327_u:Old".into(),
        fingerprint: Fingerprint::from_bytes([1; 32]),
        curve: "secp256r1".into(),
        ski: Some(first_ski),
        paired_at: None,
    };
    store.trust_unit(first.clone());
    assert!(store.is_trusted(&first_ski));

    let second = PairedUnit {
        ship_id: "i:983327_u:New".into(),
        fingerprint: Fingerprint::from_bytes([2; 32]),
        curve: "secp256r1".into(),
        ski: None,
        paired_at: None,
    };
    let displaced = store.trust_unit(second.clone());

    assert_eq!(displaced, Some(first));
    assert_eq!(store.unit(), Some(second));
    assert!(
        !store.is_trusted(&first_ski),
        "the replaced unit is untrusted, SKI included"
    );
}

/// §10.4: removing the trust is what reactivates the pairing process, so forgetting the
/// SKI a unit was reached under must not leave its certificate trusted.
#[test]
fn forgetting_a_paired_unit_by_ski_removes_the_certificate_trust() {
    use eebus::runtime::PairedUnit;
    use eebus::ship::Fingerprint;

    let ski: Ski = "2222222222222222222222222222222222222222".parse().unwrap();
    let fingerprint = Fingerprint::from_bytes([3; 32]);
    let store = TrustStore::new();
    store.trust_unit(PairedUnit {
        ship_id: "i:983327_u:Unit".into(),
        fingerprint,
        curve: "secp256r1".into(),
        ski: Some(ski),
        paired_at: None,
    });
    assert!(store.is_certificate_trusted(&fingerprint));

    store.forget(&ski);
    assert!(
        !store.is_certificate_trusted(&fingerprint),
        "the pairing is gone, not just the SKI it was reached under"
    );
    assert!(store.unit().is_none());
}

/// The whole store round-trips, both halves of it: §10.4 asks the Pairing Service's trust
/// to live in the same store as SHIP's, and a device writes one file.
#[test]
fn a_paired_unit_survives_a_round_trip_through_json() {
    use eebus::runtime::PairedUnit;
    use eebus::ship::Fingerprint;

    let ski: Ski = "3333333333333333333333333333333333333333".parse().unwrap();
    let store = TrustStore::new();
    store.trust(ski);
    let unit = PairedUnit {
        ship_id: "i:983327_u:Unit".into(),
        fingerprint: Fingerprint::from_bytes([4; 32]),
        curve: "secp256r1".into(),
        ski: None,
        paired_at: Some("2026-09-03T10:00:00Z".into()),
    };
    store.trust_unit(unit.clone());

    let restored = TrustStore::from_json(&store.to_json().unwrap()).unwrap();
    assert!(restored.is_trusted(&ski));
    assert_eq!(restored.unit(), Some(unit));
}

/// An EEBUS reset forgets the pairing too: SHIP §12.2.2 says *all* stored foreign key
/// material, and a certificate a control unit is admitted on is exactly that.
#[test]
fn an_eebus_reset_forgets_the_paired_unit() {
    use eebus::runtime::PairedUnit;
    use eebus::ship::Fingerprint;

    let store = TrustStore::new();
    store.trust("4444444444444444444444444444444444444444".parse().unwrap());
    store.trust_unit(PairedUnit {
        ship_id: "i:983327_u:Unit".into(),
        fingerprint: Fingerprint::from_bytes([5; 32]),
        curve: "secp256r1".into(),
        ski: None,
        paired_at: None,
    });

    assert_eq!(store.forget_all(), 2, "the peer and the unit");
    assert!(store.is_empty());
    assert!(store.unit().is_none());
}

/// A refused peer gives its slot back.
///
/// The cap only bounds anything if the slots are returned. They are released when a
/// handshake *ends* — and an accepted handshake that fails reports no SKI, so a cap keyed
/// on the SKI would never release one: after four refusals a node would refuse every
/// pairing for the rest of its life, which is a denial of service the node does to
/// itself. Refusing more peers in a row than the cap holds is the check.
#[tokio::test]
async fn refusing_a_peer_returns_the_slot_it_was_holding() {
    use eebus::runtime::MAX_PENDING_TRUST;

    let pump_trust = TrustStore::new();
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    let mut hub = Hub::new(heat_pump, pump_engine());
    let address = hub.listen("127.0.0.1:0").await.unwrap();

    // Twice the cap, one at a time, each refused before the next arrives.
    for index in 0..(MAX_PENDING_TRUST * 2) {
        let caller = node(&format!("i:46925_u:Caller-{index}"), TrustStore::new());
        let dial = tokio::spawn(async move { caller.connect(address).await });

        let refused = wait_for(
            &mut hub,
            Duration::from_secs(10),
            |hub, event| match event {
                HubEvent::TrustRequested { peer, .. } => {
                    hub.refuse(peer.ski);
                    None
                }
                HubEvent::HandshakeFailed { error, .. } => Some(error),
                _ => None,
            },
        )
        .await;
        assert!(
            !matches!(*refused, ConnectionError::TooManyPendingPairings),
            "caller {index} was turned away although every earlier one had been \
             refused and its slot released: {refused}"
        );
        dial.abort();
    }
}

/// Unapproved peers cannot fill the connection table between them.
///
/// A peer held in `hello: pending` occupies a slot for minutes on nobody's authority, so
/// past [`MAX_PENDING_TRUST`] a peer is answered `hello: aborted` at once rather than
/// queued behind the four already waiting.
#[tokio::test]
async fn only_so_many_peers_may_wait_for_approval_at_once() {
    use eebus::runtime::MAX_PENDING_TRUST;

    // A hub that trusts nobody, so every dial arrives as a trust request.
    let pump_trust = TrustStore::new();
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    let mut hub = Hub::new(heat_pump, pump_engine());
    hub.set_max_connections(64);
    let address = hub.listen("127.0.0.1:0").await.unwrap();

    // Each caller is a node of its own, because the cap counts distinct peers: one peer
    // asking twice is one decision.
    let mut callers = Vec::new();
    for index in 0..=MAX_PENDING_TRUST {
        let caller = node(&format!("i:46925_u:Caller-{index}"), TrustStore::with([]));
        callers.push(tokio::spawn(async move { caller.connect(address).await }));
    }

    let mut waiting = Vec::new();
    let error = wait_for(&mut hub, Duration::from_secs(10), |_, event| match event {
        HubEvent::TrustRequested { peer, .. } => {
            waiting.push(peer.ski);
            None
        }
        HubEvent::HandshakeFailed { error, .. } => Some(error),
        _ => None,
    })
    .await;

    assert!(
        matches!(*error, ConnectionError::TooManyPendingPairings),
        "the peer past the cap is turned away, not left pending: {error}"
    );
    assert_eq!(
        waiting.len(),
        MAX_PENDING_TRUST,
        "and exactly the cap were asked about"
    );

    for caller in callers {
        caller.abort();
    }
}

/// One peer cannot fill the pending-trust table by dialling repeatedly.
///
/// The cap is what stops unapproved peers occupying the connection table, and it is only
/// worth having if it cannot be spent by one device: a peer asking four times is still one
/// decision, because answering it releases every handshake that SKI is holding. So each SKI
/// gets one slot, and the rest of the table stays available for peers a user might actually
/// want to approve.
///
/// Keying on the SKI is what makes the rule safe to enforce — it came out of a completed
/// TLS handshake, so the peer proved it holds the key. An address would prove nothing.
#[tokio::test]
async fn one_peer_dialling_repeatedly_holds_one_pending_slot() {
    use eebus::runtime::MAX_PENDING_TRUST;
    use std::sync::Arc;

    let pump_trust = TrustStore::new();
    let heat_pump = node("i:46925_u:HeatPump-1", pump_trust.clone());
    let mut hub = Hub::new(heat_pump, pump_engine());
    hub.set_max_connections(64);
    let address = hub.listen("127.0.0.1:0").await.unwrap();

    // One caller, dialling more times than the whole table would hold.
    let caller = Arc::new(node("i:46925_u:Caller-1", TrustStore::with([])));
    let greedy = caller.ski();
    let mut dials = Vec::new();
    for _ in 0..(MAX_PENDING_TRUST + 2) {
        let caller = caller.clone();
        dials.push(tokio::spawn(async move { caller.connect(address).await }));
    }

    // …and one other peer, which must still be able to ask.
    let other = node("i:46925_u:Caller-2", TrustStore::with([]));
    let other_ski = other.ski();
    let second = tokio::spawn(async move { other.connect(address).await });

    let mut asked = Vec::new();
    let _ = wait_for(&mut hub, Duration::from_secs(10), |_, event| match event {
        HubEvent::TrustRequested { peer, .. } => {
            asked.push(peer.ski);
            (asked.len() == 2).then_some(())
        }
        _ => None,
    })
    .await;

    assert_eq!(
        asked.iter().filter(|ski| **ski == greedy).count(),
        1,
        "one peer is one decision, however many times it dials: {asked:?}"
    );
    assert!(
        asked.contains(&other_ski),
        "and the table still had room for somebody else: {asked:?}"
    );

    for dial in dials {
        dial.abort();
    }
    second.abort();
}
