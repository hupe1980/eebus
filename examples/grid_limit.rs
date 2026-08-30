//! A grid operator's control box limits a heat pump, end to end.
//!
//! This is the §14a EnWG exchange in miniature, using every layer of the crate: the SHIP
//! handshake brings two nodes to the point where they may exchange data, the SPINE engine
//! discovers what the other one is and negotiates the permission to write, and the
//! Limitation of Power Consumption state machine decides what the heat pump actually does
//! with the limit it is sent.
//!
//! Run it with:
//!
//! ```sh
//! cargo run --example grid_limit
//! ```
//!
//! Everything is in-memory and driven by a virtual clock: there is no socket, no TLS and
//! no mDNS, because none of the protocol logic needs any of them.

use core::time::Duration;

use eebus::model::{
    self, CmdData, Datagram, DeviceType, EntityType, FeatureType, Function, LoadControlLimitData,
    LoadControlLimitListData, Role, ScaledNumber,
};
use eebus::ship::{
    Data, DataMessage, Handshake, HandshakeConfig, ProtocolId, Role as ShipRole, ShipMessage, Trust,
};
use eebus::spine::{
    Engine, ErrorNumber, LocalDevice, LocalEntity, LocalFeature, SpineEvent, node_management,
};
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, EffectiveLimit,
};
use eebus::usecases::lpc;

/// The `protocolId` that marks a SHIP data message as carrying SPINE.
const SPINE_PROTOCOL_ID: &str = "ee1.0";

/// The power §14a EnWG leaves a controllable consumer.
const FAILSAFE_WATTS: f64 = 4_200.0;

fn main() {
    let mut now = Duration::ZERO;

    // ---- 1. SHIP: two nodes reach the point where they may exchange data --------
    //
    // The control box dials the heat pump, so it is the SHIP client. Both already trust
    // each other's key — an installer scanned the QR codes at commissioning.

    let mut control_link = Handshake::new(
        ShipRole::Client,
        HandshakeConfig {
            ship_id: Some("i:12345_u:ControlBox-1".into()),
            ..HandshakeConfig::default()
        },
        Trust::Trusted,
        now,
    );
    let mut pump_link = Handshake::new(
        ShipRole::Server,
        HandshakeConfig {
            ship_id: Some("i:67890_u:HeatPump-1".into()),
            ..HandshakeConfig::default()
        },
        Trust::Trusted,
        now,
    );

    let mut frames = 0;
    loop {
        let mut moved = false;
        while let Some(message) = control_link.poll_transmit() {
            frames += 1;
            deliver(&message, &mut pump_link, now);
            moved = true;
        }
        while let Some(message) = pump_link.poll_transmit() {
            frames += 1;
            deliver(&message, &mut control_link, now);
            moved = true;
        }
        if !moved {
            break;
        }
        now += Duration::from_millis(1);
    }

    let ((major, minor), format) = control_link.negotiated().cloned().expect("negotiated");
    assert!(control_link.is_ready_for_data() && pump_link.is_ready_for_data());
    println!("1. SHIP handshake complete after {frames} frames");
    println!("   version {major}.{minor}, format {format}\n");

    // ---- 2. The two SPINE nodes -------------------------------------------------

    let mut control_box = build_control_box();
    let (mut heat_pump, mut lpc) = build_heat_pump();
    lpc.publish(&mut heat_pump);

    println!("2. The heat pump starts in {:?}", lpc.system().state());
    println!(
        "   limited to {:?}\n   — a restart runs on the failsafe value, not unlimited\n",
        lpc.system().effective_limit()
    );
    assert_eq!(
        lpc.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );

    // ---- 3. Discovery -----------------------------------------------------------

    let control_nm = node_management(control_box.device().address());
    let pump_nm = node_management(heat_pump.device().address());
    control_box.read(
        &pump_nm,
        &control_nm,
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );
    control_box.read(
        &pump_nm,
        &control_nm,
        Function::NodeManagementUseCaseData,
        now,
    );
    exchange(&mut control_box, &mut heat_pump, &mut lpc, now);

    let pump_address = heat_pump.device().address().clone();
    let remote = control_box.peer(&pump_address).expect("the peer");
    let use_case = remote
        .use_case("limitationOfPowerConsumption", "ControllableSystem")
        .expect("the heat pump plays the Controllable System");
    let load_control = remote
        .address_of(use_case, &FeatureType::LoadControl, Role::Server)
        .expect("the LoadControl feature");

    println!("3. Discovery found the heat pump");
    println!("   device    {}", pump_address.as_str());
    println!("   plays     {} as {}", use_case.name, use_case.actor);
    println!("   scenarios {:?}", use_case.scenarios);
    println!("   writes    loadControlLimitListData\n");

    // ---- 4. Binding and subscription --------------------------------------------
    //
    // A write without a binding is refused with errorNumber 9, which is what stops a
    // second energy manager from overriding this one's limit.

    let client = control_box.device().address_of(&[1], 1);
    control_box.request_binding(&client, &load_control, now);
    control_box.request_subscription(&client, &load_control, now);
    exchange(&mut control_box, &mut heat_pump, &mut lpc, now);
    drain(&mut control_box);

    assert!(heat_pump.relations().is_bound(&client, &load_control));
    println!("4. Bound and subscribed to the LoadControl feature\n");

    // ---- 5. Heartbeat, then the limit -------------------------------------------
    //
    // The order matters: the implementation guide §2.11 evaluates a limit only when a
    // heartbeat arrived within the preceding sixty seconds, so an Energy Guard that has
    // lost its own upstream link cannot lift a limitation it can no longer justify.

    now += Duration::from_secs(1);
    lpc.system_mut().on_heartbeat(now);
    println!("5. Heartbeat received");

    now += Duration::from_secs(1);
    let limit = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(limitation::LIMIT_ID),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::from_f64(3_000.0, 0)),
            ..Default::default()
        }]),
    });
    control_box.write(&load_control, &client, limit, true, now);
    println!("   3000 W limit written\n");

    exchange(&mut control_box, &mut heat_pump, &mut lpc, now);

    let result = drain(&mut control_box)
        .into_iter()
        .find_map(|event| match event {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
        .expect("the heat pump answered");

    assert_eq!(result, ErrorNumber::None);
    assert_eq!(
        lpc.system().effective_limit(),
        EffectiveLimit::Active(3_000.0)
    );
    println!("6. The heat pump is now in {:?}", lpc.system().state());
    println!("   limited to {:?}", lpc.system().effective_limit());
    println!(
        "   answered errorNumber {} ({result}) — the §14a evidence\n",
        result.number()
    );

    // ---- 6. The control box goes quiet ------------------------------------------
    //
    // No disconnection is signalled and none is needed: the heartbeats stop. Two minutes
    // later the failsafe takes over, and holds — the rule the 2026 implementation guide
    // added, after implementations were found returning to `init` and going unlimited.

    now += Duration::from_secs(200);
    lpc.system_mut().handle_timeout(now);
    assert_eq!(
        lpc.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
    println!(
        "7. After two minutes of silence: {:?}",
        lpc.system().state()
    );
    println!("   limited to {:?}", lpc.system().effective_limit());
}

/// The control box: an Energy Guard on a `GridGuard` entity.
fn build_control_box() -> Engine {
    let mut device = LocalDevice::new(
        "i:12345",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .expect("a valid device address");
    device
        .add_entity(
            // The use-case implementation guide §3.3 asks an actor to use one `Generic`
            // client feature for all of its client functionality.
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

/// The heat pump: a Controllable System with the three features LPC asks of it.
fn build_heat_pump() -> (Engine, ControllableSystemActor) {
    let mut device = LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid device address");
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

/// Carries datagrams both ways over SHIP, letting the use case decide on any write.
fn exchange(
    control_box: &mut Engine,
    heat_pump: &mut Engine,
    lpc: &mut ControllableSystemActor,
    now: Duration,
) {
    for _ in 0..64 {
        let mut moved = false;
        while let Some(datagram) = control_box.poll_transmit() {
            heat_pump.handle_datagram(&over_ship(&datagram), now);
            moved = true;
        }
        while let Some(datagram) = heat_pump.poll_transmit() {
            control_box.handle_datagram(&over_ship(&datagram), now);
            moved = true;
        }
        // The use case decides on any write that is waiting, which is what produces the
        // acknowledgement the control box is expecting.
        let events: Vec<SpineEvent> = core::iter::from_fn(|| heat_pump.poll_event()).collect();
        for event in &events {
            if lpc.handle_event(heat_pump, event, now).is_some() {
                moved = true;
            }
        }
        if !moved {
            return;
        }
    }
    panic!("the exchange did not settle");
}

/// Wraps a datagram in a SHIP data message, frames it, and unwraps it again.
///
/// The round trip is what makes this an honest demonstration rather than two objects
/// passing each other in memory: everything crosses the wire format in both directions.
fn over_ship(datagram: &Datagram) -> Datagram {
    let message = ShipMessage::Data(DataMessage::Data(Data {
        header: Some(eebus::ship::Header {
            protocol_id: Some(ProtocolId(SPINE_PROTOCOL_ID.into())),
        }),
        payload: Some(model::to_json_value(datagram).expect("encode")),
        extension: None,
    }));
    let frame = message.encode().expect("frame");

    let ShipMessage::Data(DataMessage::Data(data)) = ShipMessage::decode(&frame).expect("decode")
    else {
        panic!("expected a data message");
    };
    assert_eq!(
        data.header.and_then(|h| h.protocol_id).map(|p| p.0),
        Some(SPINE_PROTOCOL_ID.to_string()),
        "the payload is SPINE"
    );
    let decoded = model::from_json_value(data.payload.expect("payload")).expect("datagram");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}

/// Encodes a SHIP message, hands it to the peer, and checks the framing on the way.
fn deliver(message: &ShipMessage, peer: &mut Handshake, now: Duration) {
    let bytes = message.encode().expect("encode");
    let decoded = ShipMessage::decode(&bytes).expect("decode");
    assert_eq!(&decoded, message, "framing round-trips");
    peer.handle_message(decoded, now).expect("handshake input");
}

fn drain(engine: &mut Engine) -> Vec<SpineEvent> {
    core::iter::from_fn(|| engine.poll_event()).collect()
}
