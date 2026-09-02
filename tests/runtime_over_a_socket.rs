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

/// The hub does the pre-scenario work an application would otherwise write by hand:
/// dial, discover, learn the peer's device address, and route by it.
#[tokio::test]
async fn the_hub_discovers_a_peer_and_routes_to_it() {
    use eebus::runtime::{Hub, HubEvent};

    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let mut hub = Hub::new(heat_pump, pump_engine());
        let (stream, _) = listener.accept().await.expect("a caller");
        hub.accept(stream).await.expect("the handshake");
        // Answer whatever arrives until the client is done with us.
        for _ in 0..32 {
            if hub.next().await.is_err() {
                break;
            }
        }
    });

    let mut hub = Hub::new(control_box, box_engine());
    let ski = hub.connect(address).await.expect("a connection");

    let device = loop {
        // A deadline goes to `wake_at`, not around `hub.next()`: `next` is not
        // cancel-safe, and a test that models the misuse is a test people copy.
        hub.wake_at(hub.now() + Duration::from_secs(5));
        match hub.next().await.expect("no error") {
            HubEvent::PeerDiscovered { device, .. } => break device,
            HubEvent::Disconnected { .. } => panic!("the connection ended early"),
            HubEvent::Tick => panic!("no peer was discovered within five seconds"),
            _ => {}
        }
    };

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

    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
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

/// A remembered peer is dialled, and dialled again when the connection drops.
///
/// This is what keeps a §14a installation working across a router reboot: nothing tells
/// the hub the peer is back, so it has to keep asking — with a backoff, so it does not
/// hammer a device that is genuinely gone.
#[tokio::test]
async fn a_remembered_peer_is_dialled_and_redialled() {
    use eebus::runtime::{Hub, HubEvent};

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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while connections < 2 && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, hub.next()).await {
            Ok(Ok(HubEvent::Connected { ski, version })) => {
                assert_eq!(ski, pump_ski);
                assert_eq!(
                    version,
                    Some(eebus::ship::ShipVersion::V1_1),
                    "a reconnection reports the version it settled on"
                );
                connections += 1;
            }
            Ok(Ok(HubEvent::Disconnected { .. })) => disconnections += 1,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }

    assert_eq!(
        connections, 2,
        "the hub dialled, lost the peer, and dialled again"
    );
    assert!(disconnections >= 1, "and noticed the loss in between");

    hub.forget_peer(&pump_ski);
    assert_eq!(hub.remembered().count(), 0);
    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}

/// SHIP §12.1.3 over a real connection: a peer renews its certificate, and the trust
/// follows without anybody rescanning a QR code.
///
/// This is the whole point of the mechanism. Certificate lifetimes are shortening; a
/// stack that cannot carry a renewal turns every renewal into a site visit.
#[tokio::test]
async fn a_certificate_renewal_updates_the_trust_store() {
    use eebus::runtime::{Hub, HubEvent};
    use eebus::ship::OwnKeys;

    let (control_box, heat_pump) = pair();
    let pump_ski = heat_pump.ski();
    let successor = Ski::from_bytes([0x5A; 20]);

    // The heat pump is mid-renewal: it still uses the certificate the control box
    // trusts, and announces the one it will move to.
    let mut keys = OwnKeys::new(pump_ski);
    keys.begin_update(successor).unwrap();
    let heat_pump = heat_pump.key_material(keys.clone());

    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let mut hub = Hub::new(heat_pump, pump_engine());
        let (stream, _) = listener.accept().await.expect("a caller");
        hub.accept(stream).await.expect("the handshake");
        for _ in 0..64 {
            if hub.next().await.is_err() {
                break;
            }
        }
    });

    let trust = control_box.trust_store().clone();
    let mut hub = Hub::new(control_box, box_engine());
    hub.connect(address).await.expect("a connection");

    let updated = loop {
        // A deadline goes to `wake_at`, not around `hub.next()`: `next` is not
        // cancel-safe, and a test that models the misuse is a test people copy.
        hub.wake_at(hub.now() + Duration::from_secs(5));
        match hub.next().await.expect("no error") {
            HubEvent::PeerKeysUpdated { ski, trusted, .. } => break (ski, trusted),
            HubEvent::Disconnected { .. } => panic!("the connection ended early"),
            HubEvent::Tick => panic!("no key material arrived within five seconds"),
            _ => {}
        }
    };

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

    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
}

/// Two peers cannot hold the same SPINE device address on one hub.
///
/// Routing is by that address, so the second claimant would take delivery of the first
/// one's datagrams — a limit meant for one heat pump arriving at another. Two devices
/// sharing a vendor and serial produce it without any malice, so it is reported.
#[tokio::test]
async fn a_second_peer_cannot_claim_the_first_peer_s_device_address() {
    use eebus::runtime::{Disconnect, Hub, HubEvent};

    let box_trust = TrustStore::new();
    let control_box = node("i:46925_u:ControlBox-1", box_trust.clone());

    // Two heat pumps with distinct identities but — the misconfiguration — the same SPINE
    // device address, which is what `pump_engine` builds.
    let mut listeners = Vec::new();
    for name in ["i:46925_u:HeatPump-A", "i:46925_u:HeatPump-B"] {
        let trust = TrustStore::new();
        let pump = node(name, trust.clone());
        trust.trust(control_box.ski());
        box_trust.trust(pump.ski());
        let listener = pump.listen("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        listeners.push(address);
        tokio::spawn(async move {
            let mut hub = Hub::new(pump, pump_engine());
            if let Ok((stream, _)) = listener.accept().await {
                let _ = hub.accept(stream).await;
                for _ in 0..32 {
                    if hub.next().await.is_err() {
                        break;
                    }
                }
            }
        });
    }

    let mut hub = Hub::new(control_box, box_engine());
    let first = hub.connect(listeners[0]).await.expect("the first peer");
    let second = hub.connect(listeners[1]).await.expect("the second peer");
    assert_ne!(first, second, "two distinct identities");

    // Which of the two answers discovery first is a race, and immaterial: whichever
    // binds the address keeps it, and the other is closed.
    let mut discovered = Vec::new();
    let mut conflicted = Vec::new();
    for _ in 0..64 {
        if !discovered.is_empty() && !conflicted.is_empty() {
            break;
        }
        hub.wake_at(hub.now() + Duration::from_secs(2));
        match hub.next().await {
            Ok(HubEvent::PeerDiscovered { ski, device }) => discovered.push((ski, device)),
            Ok(HubEvent::Disconnected { ski, reason }) => {
                if reason == Disconnect::AddressConflict {
                    conflicted.push(ski);
                }
            }
            // The deadline, arriving as a tick rather than as a cancelled read.
            Ok(HubEvent::Tick) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

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
    assert!([first, second].contains(holder));
    assert!([first, second].contains(&conflicted[0]));
    assert_eq!(
        hub.ski_of(device),
        Some(*holder),
        "the address still routes to the peer that bound it"
    );

    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
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
    use eebus::runtime::{Hub, HubEvent};
    use eebus::usecases::limitation::{
        self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, CsFeatures,
        EnergyGuardActor, GuardEvent, LimitWrite, LimitationState,
    };
    use eebus::usecases::lpc;

    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    // ---- the appliance: listens, and applies what it is sent ------------------
    let appliance = tokio::spawn(async move {
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
        let (stream, _) = listener.accept().await.unwrap();
        hub.accept(stream).await.expect("the handshake");
        let mut actor = actor.install(hub.engine_mut(), Duration::ZERO);

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
    hub.connect(address).await.expect("the handshake");

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

    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
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
/// that runs out is the one that stops serving the peers it already had. The refusal is a
/// `connectionClose` rather than a dropped socket, so the peer is told rather than left
/// guessing.
#[tokio::test]
async fn a_full_hub_refuses_a_connection_rather_than_growing() {
    use eebus::runtime::{ConnectionError, Hub};
    use eebus::ship::ConnectionCloseReason;

    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let mut hub = Hub::new(heat_pump, pump_engine());
        for _ in 0..2 {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let _ = hub.accept(stream).await;
        }
        hub
    });

    let mut hub = Hub::new(control_box, box_engine());
    hub.set_max_connections(1);
    assert_eq!(hub.max_connections(), 1);

    hub.connect(address).await.expect("the first connection");

    // The second is refused by this side, and the peer is closed politely.
    let refused = hub.connect(address).await;
    assert!(
        matches!(refused, Err(ConnectionError::TooManyConnections)),
        "a full hub took a second connection: {refused:?}"
    );
    assert_eq!(
        hub.peers().count(),
        1,
        "the refused connection was still adopted"
    );

    hub.shutdown(ConnectionCloseReason::Unspecific).await;
    let _ = server.await;
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
    use eebus::runtime::{Disconnect, Hub, HubEvent};

    let (control_box, heat_pump) = pair();
    let listener = heat_pump.listen("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    // A peer that answers discovery and then stops reading entirely. The connection stays
    // open — it is the *reading* that stops, which is what fills the buffers.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let server = tokio::spawn(async move {
        let mut hub = Hub::new(heat_pump, pump_engine());
        let (stream, _) = listener.accept().await.expect("a caller");
        hub.accept(stream).await.expect("the handshake");
        while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
            hub.wake_at(hub.now() + Duration::from_millis(20));
            if hub.next().await.is_err() {
                break;
            }
        }
        core::future::pending::<()>().await;
    });

    let mut hub = Hub::new(control_box, box_engine());
    hub.connect(address).await.expect("a connection");

    let device = loop {
        // A deadline goes to `wake_at`, not around `hub.next()`: `next` is not
        // cancel-safe, and a test that models the misuse is a test people copy.
        hub.wake_at(hub.now() + Duration::from_secs(5));
        match hub.next().await.expect("no error") {
            HubEvent::PeerDiscovered { device, .. } => break device,
            HubEvent::Disconnected { .. } => panic!("the connection ended early"),
            HubEvent::Tick => panic!("no peer was discovered within five seconds"),
            _ => {}
        }
    };
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
