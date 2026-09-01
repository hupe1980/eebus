//! The §14a exchange over a real network connection.
//!
//! `grid_limit` runs the same protocol against a virtual clock with no socket, which is
//! how every rule in it is tested. This one runs it over loopback: TCP, TLS 1.2 with
//! mutual authentication, a WebSocket upgrade, the SHIP handshake, and then SPINE
//! datagrams carrying a limit and the acknowledgement that answers it.
//!
//! ```sh
//! cargo run --example networked --features runtime
//! ```
//!
//! Both sides are a [`Hub`] driving a [`Engine`](eebus::spine::Engine) and one use-case
//! actor. The loop is the same on each: ask the hub what happened, hand it to the actor,
//! tell the hub when the actor next wants waking. Discovery, routing, keep-alives and
//! the double-connection rule are the hub's business and do not appear here.
//!
//! Two nodes are created with fresh certificates and told to trust each other, which is
//! what an installer scanning two QR codes amounts to. Everything after that is what the
//! devices would do on a real network.

use core::time::Duration;

use eebus::cert::{self, CertParams};
use eebus::model::{DeviceType, EntityType};
use eebus::runtime::{Hub, HubEvent, Node, TrustStore};
use eebus::spine::{Engine, LocalDevice, LocalEntity};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsEvent, EnergyGuardActor,
    GuardEvent, LimitWrite,
};
use eebus::usecases::lpc;

const FAILSAFE_WATTS: f64 = 4_200.0;
const REQUIRED_WATTS: f64 = 3_000.0;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ---- Commissioning: two identities, and a user approving each ------------
    let control_box_trust = TrustStore::new();
    let heat_pump_trust = TrustStore::new();

    let control_box = Node::new(
        "i:46925_u:ControlBox-1",
        ShipTls::new(cert::self_signed(CertParams::new(
            "i:46925_u:ControlBox-1",
        ))?),
        control_box_trust.clone(),
    );
    let heat_pump = Node::new(
        "i:46925_u:HeatPump-1",
        ShipTls::new(cert::self_signed(CertParams::new("i:46925_u:HeatPump-1"))?),
        heat_pump_trust.clone(),
    );

    println!("control box  {}", control_box.ski().to_display_string());
    println!("heat pump    {}", heat_pump.ski().to_display_string());
    println!("             — the numbers an installer reads off each label\n");

    control_box_trust.trust(heat_pump.ski());
    heat_pump_trust.trust(control_box.ski());

    let listener = heat_pump.listen("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    println!("heat pump listening on {address}\n");

    // ---- The heat pump: applies limits, and answers for them ----------------
    let appliance = tokio::spawn(async move {
        let (engine, mut actor) = build_heat_pump();
        let mut hub = Hub::new(heat_pump, engine);

        let (stream, from) = listener.accept().await.expect("a caller");
        let ski = hub.accept(stream).await.expect("the SHIP handshake");
        println!("[pump] {from} completed the handshake as {ski}");
        actor.install(hub.engine_mut(), Duration::ZERO);

        loop {
            let now = hub.now();
            let event = match hub.next().await {
                Ok(event) => event,
                Err(_) => break,
            };
            match event {
                HubEvent::Spine(event) => {
                    if let Some(CsEvent::LimitDecided { write, outcome, .. }) =
                        actor.handle_event(hub.engine_mut(), &event, now)
                    {
                        println!(
                            "[pump] a {:.0} W limit was {}; now {:?} at {:?}",
                            write.watts,
                            if outcome.is_accepted() {
                                "accepted"
                            } else {
                                "refused"
                            },
                            actor.system().state(),
                            actor.system().effective_limit(),
                        );
                    }
                }
                HubEvent::Tick => {
                    actor.handle_timeout(hub.engine_mut(), now);
                }
                HubEvent::Disconnected { .. } => break,
                _ => {}
            }
            let next = actor.poll_timeout();
            hub.wake_at(next);
        }
        println!("[pump] the connection ended");
    });

    // ---- The control box: discovers, binds, and sends a limit ---------------
    let (engine, client, diagnosis) = build_control_box();
    let mut guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, Duration::ZERO);
    let mut hub = Hub::new(control_box, engine);

    let ski = hub.connect(address).await?;
    println!("[box]  connected to {ski}\n");

    // The grid needs 3 kW. Everything from here — the heartbeat that has to precede the
    // limit, the binding it needs first — belongs to the actor.
    let mut required = Some(LimitWrite::active(REQUIRED_WATTS));

    for _ in 0..64 {
        let now = hub.now();
        let event = tokio::time::timeout(Duration::from_secs(5), hub.next()).await??;
        let mut reports = Vec::new();

        match event {
            HubEvent::PeerDiscovered { device, .. } => {
                let remote = hub.engine().peer(&device).expect("the peer we just heard");
                let peer = limitation::locate(remote, lpc::DIRECTION)
                    .expect("it plays the Controllable System");
                println!(
                    "[box]  discovered {} playing the Controllable System",
                    device.as_str()
                );
                guard.attach(hub.engine_mut(), peer, now);
                if let Some(limit) = required.take() {
                    guard.require(&device, Some(limit), now);
                }
            }
            HubEvent::Spine(event) => {
                reports.extend(guard.handle_event(hub.engine_mut(), &event, now));
            }
            HubEvent::Tick => reports = guard.handle_timeout(hub.engine_mut(), now),
            HubEvent::Disconnected { .. } => break,
            HubEvent::Connected { .. } | HubEvent::PeerKeysUpdated { .. } => {}
        }

        let mut done = false;
        for report in reports {
            match report {
                GuardEvent::Ready { .. } => println!("[box]  bound to the LoadControl feature"),
                GuardEvent::ConstraintsLearned { nominal_max, .. } => println!(
                    "[box]  it can draw at most {:.0} W — scenario 4",
                    nominal_max.watts()
                ),
                GuardEvent::LimitAccepted { limit, request, .. } => {
                    println!(
                        "[box]  {:.0} W accepted, answering msgCounter {} — the §14a evidence",
                        limit.watts,
                        request.get()
                    );
                    done = true;
                }
                GuardEvent::LimitRefused { limit, error, .. } => {
                    println!("[box]  {:.0} W refused: {error}", limit.watts);
                    done = true;
                }
                GuardEvent::PeerHeartbeatLost { .. } => {}
            }
        }
        if done {
            break;
        }
        hub.wake_at(guard.poll_timeout());
    }

    hub.shutdown(eebus::ship::ConnectionCloseReason::Unspecific)
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), appliance).await;
    println!("\ndone — the whole stack, over a socket");
    Ok(())
}

/// The control box: an Energy Guard on a `GridGuard` entity.
fn build_control_box() -> (
    Engine,
    eebus::model::FeatureAddress,
    eebus::model::FeatureAddress,
) {
    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .expect("a valid address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard)
                .with_feature(limitation::client_feature(1))
                .with_feature(limitation::device_diagnosis_feature(2)),
        )
        .expect("a fresh entity");

    let client = device.address_of(&[1], 1);
    let diagnosis = device.address_of(&[1], 2);
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
    (engine, client, diagnosis)
}

/// The heat pump: a Controllable System with the features LPC asks of it.
fn build_heat_pump() -> (Engine, ControllableSystemActor) {
    let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::electrical_connection_feature(4)),
        )
        .expect("a fresh entity");

    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

    let actor = ControllableSystemActor::new(
        ControllableSystem::new(
            CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                .with_nominal_max(11_000.0),
            Duration::ZERO,
        ),
        lpc::DIRECTION,
        load_control,
        configuration,
        diagnosis,
    )
    .with_electrical_connection(electrical);
    (engine, actor)
}
