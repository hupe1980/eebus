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
