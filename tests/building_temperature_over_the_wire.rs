//! MRT and MOT — the two temperatures a building is planned against — over real datagrams.
//!
//! What a controller needs from a heat pump, beyond the hot water, is the state of the
//! building: the air temperature inside it and the air temperature outside it. Both are
//! `Measurement` servers and both are read through the machinery MPC and MGCP already use,
//! so the interesting parts of this file are the two things that are *not* shared with
//! them:
//!
//! * a device is **several rooms**. §7.5 keys use-case information by address, so a gateway
//!   announces `monitoringOfRoomTemperature` once per `HVACRoom` entity, and each room is
//!   its own unit with its own `Measurement` feature. A Monitoring Appliance keyed by
//!   device would hold one of them and resolve the others' notifications against it.
//! * three temperatures on one device — the tank, a room, the outdoors — are told apart by
//!   `scopeType` alone. They share a `measurementType`, two of them share a `commodityType`,
//!   and all three are published under `measurementId` 1 by their own use case's reckoning.

use core::time::Duration;

use eebus::model::{CmdData, DeviceType, EntityType, Function};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::hvac::{mdt, mot, mrt};
use eebus::usecases::limitation;
use eebus::usecases::monitoring::{MonitoringApplianceActor, MonitoringEvent};

/// A heat pump that knows about two rooms, the weather and its own tank.
struct Building {
    pump: Engine,
    manager: Engine,
    appliance: MonitoringApplianceActor,
    /// The `Measurement` feature of each `HVACRoom`, in entity order.
    rooms: [eebus::model::FeatureAddress; 2],
    outdoors: eebus::model::FeatureAddress,
    tank: eebus::model::FeatureAddress,
    seen: Vec<MonitoringEvent>,
    now: Duration,
}

/// Entity `[1]` is the appliance; `[2]` and `[3]` are rooms; `[4]` is the outdoor sensor.
const LIVING_ROOM: [u32; 1] = [2];
const BEDROOM: [u32; 1] = [3];

impl Building {
    fn new() -> Self {
        let now = Duration::ZERO;

        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        // The tank, on the appliance entity.
        device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    .with_feature(mdt::measurement_feature(1)),
            )
            .unwrap();
        // Two rooms. Table 6 gives each entity at most one `Measurement` feature, so two
        // rooms are two entities and there is no other way to express them.
        for entity in [LIVING_ROOM, BEDROOM] {
            device
                .add_entity(
                    LocalEntity::new(entity, EntityType::HVACRoom)
                        .with_feature(mrt::measurement_feature(1)),
                )
                .unwrap();
        }
        // And the outdoor sensor, which MOT §3.2.1.1 puts behind `TemperatureSensor`.
        device
            .add_entity(
                LocalEntity::new([4], EntityType::TemperatureSensor)
                    .with_feature(mot::measurement_feature(1)),
            )
            .unwrap();

        let tank = device.address_of(&[1], 1);
        let rooms = [
            device.address_of(&LIVING_ROOM, 1),
            device.address_of(&BEDROOM, 1),
        ];
        let outdoors = device.address_of(&[4], 1);

        let mut pump = Engine::new(device);
        pump.add_use_case([1], 1, &mdt::DHW_CIRCUIT);
        for entity in [LIVING_ROOM, BEDROOM] {
            pump.add_use_case(entity, 1, &mrt::HVAC_ROOM);
        }
        pump.add_use_case([4], 1, &mot::OUTDOOR_TEMPERATURE_SENSOR);

        for payload in [
            mdt::temperature_description(),
            mdt::temperature_constraints(0.0, 95.0, Some(0.1)),
            mdt::temperature(49.0),
        ] {
            set(&mut pump, &tank, payload);
        }
        for (room, degrees) in rooms.iter().zip([21.5, 18.0]) {
            for payload in [
                mrt::temperature_description(),
                mrt::temperature_constraints(-10.0, 40.0, Some(0.1)),
                mrt::temperature(degrees),
            ] {
                set(&mut pump, room, payload);
            }
        }
        for payload in [
            mot::temperature_description(),
            mot::temperature_constraints(-40.0, 60.0, Some(0.1)),
            mot::temperature(-3.5),
        ] {
            set(&mut pump, &outdoors, payload);
        }

        let mut manager_device = LocalDevice::new(
            "i:46925",
            "EnergyManager-1",
            DeviceType::EnergyManagementSystem,
        )
        .unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM).with_feature(limitation::client_feature(1)),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let mut manager = Engine::new(manager_device);
        for descriptor in [
            &mrt::MONITORING_APPLIANCE,
            &mot::MONITORING_APPLIANCE,
            &mdt::MONITORING_APPLIANCE,
        ] {
            manager.add_use_case([1], 1, descriptor);
        }

        Self {
            pump,
            manager,
            appliance: MonitoringApplianceActor::new(client),
            rooms,
            outdoors,
            tank,
            seen: Vec::new(),
            now,
        }
    }

    /// Discovery, and nothing else: what each test attaches is its own business.
    fn discover(&mut self) {
        let ours = node_management(self.manager.device().address());
        let theirs = node_management(self.pump.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager.read(&theirs, &ours, function, self.now);
        }
        self.settle();
    }

    fn peer(&self) -> &eebus::spine::RemoteDevice {
        let device = self.pump.device().address().clone();
        self.manager.peer(&device).expect("the heat pump")
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                moved = true;
                self.pump.handle_datagram(&round_trip(&datagram), self.now);
            }
            while let Some(datagram) = self.pump.poll_transmit() {
                moved = true;
                self.manager
                    .handle_datagram(&round_trip(&datagram), self.now);
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                moved = true;
                self.seen.extend(self.appliance.handle_event(event));
            }
            while self.pump.poll_event().is_some() {
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }
}

fn round_trip(datagram: &eebus::model::Datagram) -> eebus::model::Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    eebus::model::from_json_str(&wire).expect("decode")
}

fn set(engine: &mut Engine, address: &eebus::model::FeatureAddress, data: CmdData) {
    engine
        .device_mut()
        .resolve_mut(address)
        .expect("the feature")
        .set_data(data)
        .expect("publishable");
}

/// [MRT-001]: the room temperature arrives, resolved, through the same actor a grid
/// connection point uses.
#[test]
fn mrt_001_a_room_temperature_is_read_by_the_monitoring_actor() {
    let mut building = Building::new();
    building.discover();

    let room = mrt::locate(building.peer()).expect("a room");
    assert_eq!(
        room.measurement.as_ref(),
        Some(&building.rooms[0]),
        "the feature is on the room's own entity, not on the appliance around it"
    );
    assert!(
        room.electrical_connection.is_none(),
        "a room has no phases, and locating it must not invent them"
    );

    building
        .appliance
        .attach(&mut building.manager, room, building.now);
    building.settle();

    let unit = building.appliance.units().next().expect("the room").id();
    assert_eq!(unit.entity, LIVING_ROOM.to_vec());
    assert_eq!(
        building
            .appliance
            .readings(&unit)
            .and_then(|r| r.value(&mrt::MEASURAND)),
        Some(21.5)
    );
    assert!(
        building
            .seen
            .iter()
            .any(|e| matches!(e, MonitoringEvent::UnitDescribed { .. })),
        "a room counts as described from its measurement description alone: \
         `roomAirTemperature` has no phases to wait for"
    );
}

/// Two rooms on one device are two units, and neither evicts the other.
///
/// The failure this guards against is silent: a Monitoring Appliance keyed by device holds
/// whichever room was attached last and resolves *both* rooms' notifications against it, so
/// a controller reads the bedroom and calls it the living room.
#[test]
fn two_rooms_on_one_device_are_two_units() {
    let mut building = Building::new();
    building.discover();

    let rooms = mrt::locate_all(building.peer());
    assert_eq!(rooms.len(), 2, "the device announced the use case twice");
    assert_eq!(
        rooms
            .iter()
            .map(|r| r.measurement.clone())
            .collect::<Vec<_>>(),
        building.rooms.iter().cloned().map(Some).collect::<Vec<_>>(),
    );
    assert_eq!(
        rooms[0].device, rooms[1].device,
        "the same device — which is exactly why the entity has to be part of the key"
    );

    for room in rooms {
        building
            .appliance
            .attach(&mut building.manager, room, building.now);
    }
    building.settle();

    let units: Vec<_> = building.appliance.units().map(|u| u.id()).collect();
    assert_eq!(units.len(), 2, "both are held");
    assert_eq!(
        units.iter().map(|u| u.entity.clone()).collect::<Vec<_>>(),
        vec![LIVING_ROOM.to_vec(), BEDROOM.to_vec()]
    );
    assert_eq!(
        units
            .iter()
            .map(|u| building
                .appliance
                .readings(u)
                .and_then(|r| r.value(&mrt::MEASURAND)))
            .collect::<Vec<_>>(),
        vec![Some(21.5), Some(18.0)],
        "each room keeps its own temperature"
    );

    // And each keeps its own when a value changes: the notification is attributed by the
    // feature it came from, not by the device.
    let bedroom = building.rooms[1].clone();
    set(&mut building.pump, &bedroom, mrt::temperature(19.5));
    building
        .pump
        .notify(&bedroom, &Function::MeasurementListData, building.now);
    building.seen.clear();
    building.settle();

    let measured: Vec<_> = building
        .seen
        .iter()
        .filter_map(|event| match event {
            MonitoringEvent::Measured { unit, readings } => {
                Some((unit.entity.clone(), readings.first()?.usable()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        measured,
        vec![(BEDROOM.to_vec(), Some(19.5))],
        "the bedroom moved, and the event says which room did"
    );
    assert_eq!(
        building
            .appliance
            .readings(&units[0])
            .and_then(|r| r.value(&mrt::MEASURAND)),
        Some(21.5),
        "and the living room was left alone"
    );
}

/// Three temperatures on one device, told apart by `scopeType` and nothing else.
///
/// All three are `measurementType: temperature`; the room and the outdoors share
/// `commodityType: air`; and each is published under `measurementId` 1 by its own use
/// case's numbering. A client that resolved them any other way would read the weather as
/// the living room.
#[test]
fn the_tank_the_room_and_the_outdoors_do_not_collide() {
    let mut building = Building::new();
    building.discover();

    let peers = [
        mdt::locate(building.peer()).expect("a tank"),
        mrt::locate(building.peer()).expect("a room"),
        mot::locate(building.peer()).expect("an outdoor sensor"),
    ];
    assert_eq!(
        peers
            .iter()
            .map(|p| p.measurement.clone())
            .collect::<Vec<_>>(),
        vec![
            Some(building.tank.clone()),
            Some(building.rooms[0].clone()),
            Some(building.outdoors.clone()),
        ],
        "three use cases, three entities, three features"
    );

    for peer in peers {
        building
            .appliance
            .attach(&mut building.manager, peer, building.now);
    }
    building.settle();

    let units: Vec<_> = building.appliance.units().map(|u| u.id()).collect();
    let (water, air, weather) = (mdt::MEASURAND, mrt::MEASURAND, mot::MEASURAND);
    let read = |unit: &eebus::usecases::monitoring::UnitId, measurand: &_| {
        building
            .appliance
            .readings(unit)
            .and_then(|r| r.value(measurand))
    };

    assert_eq!(read(&units[0], &water), Some(49.0));
    assert_eq!(read(&units[1], &air), Some(21.5));
    assert_eq!(read(&units[2], &weather), Some(-3.5));

    // And none of them answers to another's measurand, which is the whole point of the
    // scope: the tank is 49 °C and the room is not.
    assert_eq!(read(&units[0], &air), None);
    assert_eq!(read(&units[1], &water), None);
    assert_eq!(read(&units[1], &weather), None);
    assert_eq!(read(&units[2], &air), None);
}

/// [MOT-005]: a sensor that flags its own value is not read as a temperature.
///
/// A heat pump defrosting reads its outdoor sensor several degrees warm, and `outOfRange`
/// is how it says so. A building model fed the flagged number would fit a house that is
/// better insulated than it is.
#[test]
fn mot_005_a_flagged_outdoor_value_is_not_a_temperature() {
    let mut building = Building::new();
    building.discover();

    let sensor = mot::locate(building.peer()).expect("an outdoor sensor");
    building
        .appliance
        .attach(&mut building.manager, sensor, building.now);
    building.settle();

    let unit = building.appliance.units().next().expect("the sensor").id();
    assert_eq!(
        building
            .appliance
            .readings(&unit)
            .and_then(|r| r.value(&mot::MEASURAND)),
        Some(-3.5)
    );

    let outdoors = building.outdoors.clone();
    set(
        &mut building.pump,
        &outdoors,
        mot::temperature_from(
            12.0,
            eebus::model::MeasurementValueSource::MeasuredValue,
            Some(eebus::model::MeasurementValueState::OutOfRange),
        ),
    );
    building
        .pump
        .notify(&outdoors, &Function::MeasurementListData, building.now);
    building.settle();

    let readings = building.appliance.readings(&unit).expect("the sensor");
    assert_eq!(
        readings.value(&mot::MEASURAND),
        None,
        "[MOT-005]: `outOfRange` SHALL be ignored"
    );
    assert_eq!(
        readings.get(&mot::MEASURAND).and_then(|r| r.value),
        Some(12.0),
        "the raw number is still there for a display, which is a different thing"
    );
}

/// Nothing in this family is written, so nothing in it binds.
#[test]
fn no_actor_here_needs_a_binding() {
    for descriptor in [
        &mrt::HVAC_ROOM,
        &mrt::MONITORING_APPLIANCE,
        &mot::OUTDOOR_TEMPERATURE_SENSOR,
        &mot::MONITORING_APPLIANCE,
    ] {
        assert_eq!(
            descriptor.features_needing_binding().count(),
            0,
            "{} has no writeable function",
            descriptor.actor
        );
    }
}
