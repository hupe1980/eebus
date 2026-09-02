//! A grid operator's control box limits a heat pump, end to end.
//!
//! This is the §14a EnWG exchange in miniature, using every layer of the crate: the SHIP
//! handshake brings two nodes to the point where they may exchange data, the SPINE engine
//! discovers what the other one is and negotiates the permission to write, and the two
//! Limitation of Power Consumption actors — Energy Guard on one side, Controllable System
//! on the other — do the rest.
//!
//! Run it with:
//!
//! ```sh
//! cargo run --example grid_limit
//! ```
//!
//! Everything is in-memory and driven by a virtual clock: there is no socket, no TLS and
//! no mDNS, because none of the protocol logic needs any of them. Both sides are the real
//! actors, so the ordering rules the implementation guides impose — heartbeat before
//! limit, never deactivate on reconnection, retry a refusal rather than dropping the
//! device — are the crate's, not this file's.

use core::time::Duration;

use eebus::model::{self, Datagram, DeviceType, EntityType, Function};
use eebus::ship::{
    Data, DataMessage, Handshake, HandshakeConfig, ProtocolId, Role as ShipRole, ShipMessage, Trust,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::limitation::{
    self, ControllableSystem, ControllableSystemActor, CsConfig, CsFeatures, EffectiveLimit,
    EnergyGuardActor, GuardEvent, LimitWrite, LimitationState,
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

    let mut control_link = handshake(ShipRole::Client, "i:12345_u:ControlBox-1", now);
    let mut pump_link = handshake(ShipRole::Server, "i:67890_u:HeatPump-1", now);

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

    let (version, format) = control_link.negotiated().cloned().expect("negotiated");
    assert!(control_link.is_ready_for_data() && pump_link.is_ready_for_data());
    println!("1. SHIP handshake complete after {frames} frames");
    println!("   SHIP {version}, format {format}\n");

    // ---- 2. The two SPINE nodes and their use-case actors -----------------------

    let (mut box_engine, mut guard) = build_control_box(now);
    let (mut pump_engine, mut pump) = build_heat_pump(now);

    println!("2. The heat pump starts in {:?}", pump.system().state());
    println!(
        "   limited to {:?}\n   — a restart runs on the failsafe value, not unlimited\n",
        pump.system().effective_limit()
    );
    assert_eq!(
        pump.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );

    // ---- 3. Discovery -----------------------------------------------------------

    let control_nm = node_management(box_engine.device().address());
    let pump_nm = node_management(pump_engine.device().address());
    for function in [
        Function::NodeManagementDetailedDiscoveryData,
        Function::NodeManagementUseCaseData,
    ] {
        box_engine.read(&pump_nm, &control_nm, function, now);
    }
    let mut link = Link {
        guard: &mut guard,
        pump: &mut pump,
    };
    link.exchange(&mut box_engine, &mut pump_engine, now);

    let pump_address = pump_engine.device().address().clone();
    let remote = box_engine.peer(&pump_address).expect("the peer");
    let peer = limitation::locate(remote, lpc::DIRECTION).expect("a Controllable System");
    let use_case = remote
        .use_case("limitationOfPowerConsumption", "ControllableSystem")
        .expect("the heat pump plays the Controllable System");

    println!("3. Discovery found the heat pump");
    println!("   device    {}", pump_address.as_str());
    println!("   plays     {} as {}", use_case.name, use_case.actor);
    println!("   scenarios {:?}", use_case.scenarios);
    println!("   limit at  {}\n", describe(&peer.load_control));

    // ---- 4. The Energy Guard takes control --------------------------------------
    //
    // Two bindings — LoadControl for the limit, DeviceConfiguration for the failsafe
    // values — and they have to come from the same entity: implementation guide §3.8 is
    // what makes it unambiguous which of an energy manager's entities is in charge.

    now += Duration::from_secs(1);
    assert_eq!(peer.device, pump_engine.device().address().clone());
    link.exchange(&mut box_engine, &mut pump_engine, now);

    let guarded = link
        .pump
        .guard()
        .expect("the heat pump knows who is in control");
    println!("4. Bound as the Energy Guard");
    println!("   the heat pump takes limits from {}\n", describe(guarded));

    // ---- 5. The grid needs 3 kW -------------------------------------------------
    //
    // `require` is the whole application-facing API: what the grid situation demands.
    // The heartbeat that has to precede the limit (§2.11), the refusal handling (§2.5)
    // and the five-minute write ceiling (§2.10) are the actor's business.

    now += Duration::from_secs(1);
    link.guard
        .require(&pump_address, Some(LimitWrite::active(3_000.0)), now);
    let events = link.tick(&mut box_engine, &mut pump_engine, now);
    println!("5. Heartbeat, then a 3000 W limit");

    let accepted = events
        .iter()
        .find_map(|event| match event {
            GuardEvent::LimitAccepted { limit, .. } => Some(*limit),
            _ => None,
        })
        .expect("the heat pump answered");

    assert_eq!(pump.system().state(), LimitationState::Limited);
    assert_eq!(
        pump.system().effective_limit(),
        EffectiveLimit::Active(3_000.0)
    );
    println!("6. The heat pump is now in {:?}", pump.system().state());
    println!("   limited to {:?}", pump.system().effective_limit());
    println!(
        "   the guard recorded {:.0} W accepted — the §14a evidence\n",
        accepted.watts
    );

    // ---- 6. The control box goes quiet ------------------------------------------
    //
    // No disconnection is signalled and none is needed: the heartbeats stop. Two minutes
    // later the failsafe takes over, and holds — the rule the 2026 implementation guide
    // added, after implementations were found returning to `init` and going unlimited.

    now += Duration::from_secs(200);
    pump.handle_timeout(&mut pump_engine, now);
    assert_eq!(pump.system().state(), LimitationState::FailsafeState);
    assert_eq!(
        pump.system().effective_limit(),
        EffectiveLimit::Failsafe(FAILSAFE_WATTS)
    );
    println!(
        "7. After two minutes of silence: {:?}",
        pump.system().state()
    );
    println!("   limited to {:?}", pump.system().effective_limit());
}

/// The two actors, so the exchange helper can drive both sides.
struct Link<'a> {
    guard: &'a mut EnergyGuardActor,
    pump: &'a mut ControllableSystemActor,
}

impl Link<'_> {
    /// Carries datagrams both ways until neither side has anything left to say, feeding
    /// each engine's events to the actor that owns it.
    ///
    /// A real deployment does not need this: [`eebus::runtime::Hub`] is the same loop
    /// over a socket. Here it is written out so the exchange is visible.
    fn exchange(
        &mut self,
        box_engine: &mut Engine,
        pump_engine: &mut Engine,
        now: Duration,
    ) -> Vec<GuardEvent> {
        let mut guard_events = Vec::new();
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = box_engine.poll_transmit() {
                pump_engine.handle_datagram(&over_ship(&datagram), now);
                moved = true;
            }
            while let Some(datagram) = pump_engine.poll_transmit() {
                box_engine.handle_datagram(&over_ship(&datagram), now);
                moved = true;
            }
            for event in drain(pump_engine) {
                if self.pump.handle_event(pump_engine, &event, now).is_some() {
                    moved = true;
                }
            }
            for event in drain(box_engine) {
                if let Some(event) = self.guard.handle_event(box_engine, &event, now) {
                    guard_events.push(event);
                    moved = true;
                }
            }
            if !moved {
                return guard_events;
            }
        }
        panic!("the exchange did not settle");
    }

    /// Fires the Energy Guard's timers — its heartbeat, and any limit that is due — and
    /// then carries the result to the heat pump.
    fn tick(
        &mut self,
        box_engine: &mut Engine,
        pump_engine: &mut Engine,
        now: Duration,
    ) -> Vec<GuardEvent> {
        let mut events = self.guard.handle_timeout(box_engine, now);
        events.extend(self.exchange(box_engine, pump_engine, now));
        events
    }
}

fn drain(engine: &mut Engine) -> Vec<SpineEvent> {
    core::iter::from_fn(|| engine.poll_event()).collect()
}

/// A feature address as `entity/feature`, for printing.
fn describe(address: &eebus::model::FeatureAddress) -> String {
    let entity: Vec<String> = eebus::spine::entity_path(address)
        .iter()
        .map(u32::to_string)
        .collect();
    format!(
        "entity {} feature {}",
        entity.join("."),
        address.feature.map_or(0, |f| f.get())
    )
}

/// One side of the SHIP handshake.
fn handshake(role: ShipRole, ship_id: &str, now: Duration) -> Handshake {
    Handshake::new(
        role,
        HandshakeConfig {
            ship_id: Some(ship_id.into()),
            ..HandshakeConfig::default()
        },
        Trust::Trusted,
        now,
    )
}

/// The control box: an Energy Guard on a `GridGuard` entity.
fn build_control_box(now: Duration) -> (Engine, EnergyGuardActor) {
    let mut device = LocalDevice::new(
        "i:12345",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .expect("a valid device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::GridGuard)
                // The use-case implementation guide §3.3 asks an actor to use one
                // `Generic` client feature for all of its client functionality.
                .with_feature(limitation::client_feature(1))
                .with_feature(limitation::device_diagnosis_feature(2)),
        )
        .expect("a fresh entity");

    let client = device.address_of(&[1], 1);
    let diagnosis = device.address_of(&[1], 2);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);

    let guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, now);
    (engine, guard)
}

/// The heat pump: a Controllable System with the four features LPC asks of it.
fn build_heat_pump(now: Duration) -> (Engine, ControllableSystemActor) {
    let mut device = LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem)
        .expect("a valid device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance)
                .with_feature(limitation::load_control_feature(1))
                .with_feature(limitation::device_configuration_feature(2))
                .with_feature(limitation::device_diagnosis_feature(3))
                .with_feature(limitation::device_diagnosis_client_feature(5))
                .with_feature(limitation::electrical_connection_feature(4)),
        )
        .expect("a fresh entity");

    let load_control = device.address_of(&[1], 1);
    let configuration = device.address_of(&[1], 2);
    let diagnosis = device.address_of(&[1], 3);
    let diagnosis_client = device.address_of(&[1], 5);
    let electrical = device.address_of(&[1], 4);

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

    let actor = ControllableSystemActor::builder(
        ControllableSystem::new(
            CsConfig::new(FAILSAFE_WATTS, Duration::from_secs(2 * 3_600))
                .with_nominal_max(11_000.0),
            now,
        ),
        lpc::DIRECTION,
        CsFeatures {
            load_control,
            device_configuration: configuration,
            device_diagnosis: diagnosis,
            device_diagnosis_client: diagnosis_client,
        },
    )
    // Scenario 4: the nameplate the Energy Guard needs to turn a percentage into watts.
    .with_electrical_connection(electrical)
    .install(&mut engine, now);
    (engine, actor)
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
