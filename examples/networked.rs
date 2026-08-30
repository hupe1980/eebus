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
//! Two nodes are created with fresh certificates and told to trust each other, which is
//! what an installer scanning two QR codes amounts to. Everything after that is what the
//! devices would do on a real network.

use core::time::Duration;

use eebus::cert::{self, CertParams};
use eebus::model::{
    CmdData, DeviceType, EntityType, FeatureType, Function, LoadControlLimitData,
    LoadControlLimitListData, Role, ScaledNumber,
};
use eebus::runtime::{Node, TrustStore};
use eebus::spine::{
    Engine, ErrorNumber, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management,
};
use eebus::tls::ShipTls;
use eebus::usecases::limitation::{ControllableSystem, ControllableSystemActor, CsConfig};
use eebus::usecases::{limitation, lpc};

const FAILSAFE_WATTS: f64 = 4_200.0;

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
        let (stream, from) = listener.accept().await.expect("a caller");
        let mut connection = heat_pump.accept(stream).await.expect("the SHIP handshake");
        println!(
            "[pump] {from} completed the handshake as {}",
            connection.peer()
        );

        let (mut engine, mut lpc_actor) = build_heat_pump();
        lpc_actor.publish(&mut engine);
        // The control box is on the far side of a live connection, so its heartbeat is
        // what this stands in for.
        lpc_actor.system_mut().on_heartbeat(Duration::from_secs(1));

        let mut now = Duration::from_secs(2);
        for _ in 0..8 {
            let Ok(datagram) = connection.recv().await else {
                break;
            };
            engine.handle_datagram(&datagram, now);

            let events: Vec<SpineEvent> = core::iter::from_fn(|| engine.poll_event()).collect();
            for event in &events {
                if let Some(outcome) = lpc_actor.handle_event(&mut engine, event, now) {
                    println!(
                        "[pump] a limit was {}; now {:?} at {:?}",
                        if outcome.is_accepted() {
                            "accepted"
                        } else {
                            "refused"
                        },
                        lpc_actor.system().state(),
                        lpc_actor.system().effective_limit(),
                    );
                }
            }
            while let Some(answer) = engine.poll_transmit() {
                connection.send(&answer).await.expect("the answer");
            }
            now += Duration::from_secs(1);
        }
    });

    // ---- The control box: discovers, binds, and sends a limit ---------------
    let mut connection = control_box.connect(address).await?;
    println!("[box]  connected to {}\n", connection.peer());

    let mut engine = build_control_box();
    let client = engine.device().address_of(&[1], 1);
    let peer = eebus::spine::device_address("i:46925", "HeatPump-1")?;
    let mut now = Duration::from_secs(2);

    // Discovery.
    engine.read(
        &node_management(&peer),
        &node_management(engine.device().address()),
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );
    engine.read(
        &node_management(&peer),
        &node_management(engine.device().address()),
        Function::NodeManagementUseCaseData,
        now,
    );
    pump(&mut engine, &mut connection, 2).await?;

    let remote = engine.peer(&peer).expect("the heat pump");
    let use_case = remote
        .use_case("limitationOfPowerConsumption", "ControllableSystem")
        .expect("it plays the Controllable System");
    let load_control = remote
        .address_of(use_case, &FeatureType::LoadControl, Role::Server)
        .expect("its LoadControl feature");
    println!(
        "[box]  discovered {} playing {} for scenarios {:?}\n",
        peer.as_str(),
        use_case.name,
        use_case.scenarios
    );

    // The binding is what authorises a write, and only one manager may hold it.
    now += Duration::from_secs(1);
    engine.request_binding(&client, &load_control, now);
    pump(&mut engine, &mut connection, 1).await?;
    println!("[box]  bound to the LoadControl feature\n");

    // The limit itself.
    now += Duration::from_secs(1);
    engine.write(
        &load_control,
        &client,
        CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(limitation::LIMIT_ID),
                is_limit_active: Some(true),
                value: Some(ScaledNumber::from_f64(3_000.0, 0)),
                ..Default::default()
            }]),
        }),
        true,
        now,
    );
    println!("[box]  wrote a 3000 W limit");
    pump(&mut engine, &mut connection, 1).await?;

    let result = core::iter::from_fn(|| engine.poll_event())
        .find_map(|event| match event {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
        .expect("the heat pump answered");
    println!("[box]  answered errorNumber {} ({result})", result.number());
    assert_eq!(result, ErrorNumber::None);

    connection
        .close(eebus::ship::ConnectionCloseReason::Unspecific)
        .await?;
    appliance.await?;
    println!("\ndone — the whole stack, over a socket");
    Ok(())
}

/// Sends everything the engine has queued, then waits for `expect` answers.
async fn pump(
    engine: &mut Engine,
    connection: &mut eebus::runtime::ShipConnection,
    expect: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Some(datagram) = engine.poll_transmit() {
        connection.send(&datagram).await?;
    }
    for _ in 0..expect {
        let datagram = tokio::time::timeout(Duration::from_secs(5), connection.recv()).await??;
        engine.handle_datagram(&datagram, Duration::from_secs(2));
    }
    Ok(())
}

fn build_control_box() -> Engine {
    let mut device = LocalDevice::new(
        "i:46925",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .expect("a valid address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard).with_feature(LocalFeature::new(
                1,
                FeatureType::Generic,
                Role::Client,
            )),
        )
        .expect("a fresh entity");
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
    engine
}

fn build_heat_pump() -> (Engine, ControllableSystemActor) {
    let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3)),
        )
        .expect("a fresh entity");

    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);

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
    );
    (engine, actor)
}
