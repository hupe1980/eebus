//! The room half of the HVAC family, over real datagrams.
//!
//! A heat pump that heats *and* cools a room serves six use cases from one `HVAC` feature
//! and one `Setpoint` feature: MRHSF/CRHSF and MRCSF/CRCSF for the two operation modes,
//! CRHT/CRCT for the two setpoints, MRT for what the room actually got to. §3.2.2.2.1 gives
//! an entity one feature of a type, so there is no other shape available.
//!
//! So this file is about two things.
//!
//! **The heating and the cooling are told apart by `systemFunctionId` and by nothing
//! else.** Both setpoints are `scopeType: roomAirTemperature`, described identically; both
//! functions relate the same `operationModeId`, since the modes are described once for the
//! device; and `hvacSystemFunctionSetpointRelationData` is keyed by `systemFunctionId`
//! (PRIMARY) and `operationModeId` (SUB). A client keyed by the mode alone answers both
//! questions with whichever entry arrived last — and warms a room it was asked to cool,
//! having been acknowledged.
//!
//! **These writes carry no binding.** Every HVAC specification says "Binding SHOULD NOT be
//! used for this Scenario", so the manager here never sends a binding request, and a server
//! that required one would fail every test below with `errorNumber` 9.

use core::time::Duration;

use eebus::model::{
    DeviceType, EntityType, Function, HvacOperationModeId, HvacOperationModeType, SetpointId,
    UnitOfMeasurement,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::hvac::setpoint::{SetpointEffect, Setpoints, WriteRefused};
use eebus::usecases::hvac::system_function::{ModeRefused, Request, SystemFunction};
use eebus::usecases::hvac::{
    self, HvacApplianceActor, HvacEvent, crcsf, crct, crhsf, crht, mrcsf, mrhsf, mrt,
};
use eebus::usecases::limitation;
use eebus::usecases::monitoring::Readings;

/// The room's own numbering, deliberately not this crate's: Table 7 spells the identifier
/// `<st1#(1..4)>` and the room chooses. Heating is 3 here and cooling is 4.
const THEIR_HEATING: SetpointId = SetpointId(3);
const THEIR_COOLING: SetpointId = SetpointId(4);

struct Room {
    manager: Engine,
    room: Engine,
    /// What the manager learned about the room's setpoints — one reader for both use cases,
    /// because they share a scope.
    setpoints: Setpoints,
    heating: SystemFunction,
    cooling: SystemFunction,
    readings: Readings,
    setpoint_feature: eebus::model::FeatureAddress,
    hvac_feature: eebus::model::FeatureAddress,
    measurement_feature: eebus::model::FeatureAddress,
    client: eebus::model::FeatureAddress,
    /// Every write the room applied, and what it decided about it.
    applied: Vec<Result<Applied, ModeRefused>>,
    /// The room's own view of each of its two system functions, which is what it decides a
    /// write against. One reader each: they are two functions on one feature.
    own_heating: SystemFunction,
    own_cooling: SystemFunction,
    /// Every acknowledgement the manager got back, by the counter it asked under.
    answers: Vec<(eebus::model::MsgCounter, eebus::spine::ErrorNumber)>,
    /// The actor, where a test drives one, and everything it reported.
    actor: Option<HvacApplianceActor>,
    seen: Vec<HvacEvent>,
    now: Duration,
}

#[derive(Debug, PartialEq)]
enum Applied {
    Setpoint(SetpointId, f64),
    Mode(HvacOperationModeId),
}

impl Room {
    fn new() -> Self {
        let now = Duration::ZERO;

        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::HVACRoom)
                    .with_feature(crht::setpoint_feature(1))
                    // One `HVAC` feature for all four system-function use cases *and* both
                    // temperature ones (§3.2.2.2.1). The writeable one, because CRHSF and
                    // CRCSF are served from it — the two monitoring use cases are the same
                    // functions read-only, so one feature covers all four.
                    .with_feature(crhsf::with_setpoints(2))
                    .with_feature(mrt::measurement_feature(3)),
            )
            .unwrap();
        let setpoint_feature = device.address_of(&[1], 1);
        let hvac_feature = device.address_of(&[1], 2);
        let measurement_feature = device.address_of(&[1], 3);

        let mut room = Engine::new(device);
        for descriptor in [
            &mrhsf::HVAC_ROOM,
            &crhsf::HVAC_ROOM,
            &mrcsf::HVAC_ROOM,
            &crcsf::HVAC_ROOM,
            &crht::HVAC_ROOM,
            &crct::HVAC_ROOM,
        ] {
            room.add_use_case([1], 2, descriptor);
        }
        room.add_use_case([1], 3, &mrt::HVAC_ROOM);

        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];
        let heating_id = hvac::system_function_id(&hvac::HEATING);
        let cooling_id = hvac::system_function_id(&hvac::COOLING);

        // Both functions, described together, sharing the device's two operation modes.
        // Every one of these is *one* list carrying both functions: a server that published
        // them one at a time would replace its own first entry.
        for payload in [
            hvac::system_function::descriptions(&[
                (heating_id, hvac::HEATING),
                (cooling_id, hvac::COOLING),
            ]),
            mrhsf::operation_mode_descriptions(&modes).expect("two modes"),
            hvac::system_function::operation_mode_relations_of(&[
                (heating_id, &modes),
                (cooling_id, &modes),
            ])
            .expect("two modes each"),
            hvac::system_function::states(&[
                (heating_id, auto(), false, Some(true)),
                (cooling_id, auto(), false, Some(true)),
            ]),
            // The relations that matter: the same `auto`, twice, for two functions.
            hvac::setpoint::relations_of(&[
                (
                    heating_id,
                    &[
                        (auto(), HvacOperationModeType::Auto, vec![THEIR_HEATING]),
                        (off(), HvacOperationModeType::Off, vec![]),
                    ][..],
                ),
                (
                    cooling_id,
                    &[
                        (auto(), HvacOperationModeType::Auto, vec![THEIR_COOLING]),
                        (off(), HvacOperationModeType::Off, vec![]),
                    ][..],
                ),
            ])
            .expect("well formed"),
        ] {
            set(&mut room, &hvac_feature, payload);
        }

        // Two setpoints, described identically — same scope, same unit, same type.
        for payload in [
            hvac::setpoint::descriptions(&[
                (
                    THEIR_HEATING,
                    crht::TEMPERATURE_SCOPE,
                    UnitOfMeasurement::DegC,
                    Some(mrt::MEASUREMENT_ID),
                ),
                (
                    THEIR_COOLING,
                    crct::TEMPERATURE_SCOPE,
                    UnitOfMeasurement::DegC,
                    Some(mrt::MEASUREMENT_ID),
                ),
            ]),
            hvac::setpoint::constraints_of(&[
                (THEIR_HEATING, 16.0, 26.0, Some(0.5)),
                (THEIR_COOLING, 18.0, 30.0, Some(0.5)),
            ]),
            hvac::setpoint::values(&[(THEIR_HEATING, 20.0), (THEIR_COOLING, 26.0)]),
        ] {
            set(&mut room, &setpoint_feature, payload);
        }

        for payload in [
            mrt::temperature_description(),
            mrt::temperature_constraints(-10.0, 40.0, Some(0.1)),
            mrt::temperature(19.5),
        ] {
            set(&mut room, &measurement_feature, payload);
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
            &mrhsf::MONITORING_APPLIANCE,
            &mrcsf::MONITORING_APPLIANCE,
            &crhsf::CONFIGURATION_APPLIANCE,
            &crcsf::CONFIGURATION_APPLIANCE,
            &crht::CONFIGURATION_APPLIANCE,
            &crct::CONFIGURATION_APPLIANCE,
            &mrt::MONITORING_APPLIANCE,
        ] {
            manager.add_use_case([1], 1, descriptor);
        }

        Self {
            manager,
            room,
            setpoints: crht::reader(),
            heating: mrhsf::reader(),
            cooling: mrcsf::reader(),
            readings: Readings::new(),
            setpoint_feature,
            hvac_feature,
            measurement_feature,
            client,
            applied: Vec::new(),
            own_heating: own(mrhsf::reader(), heating_id, hvac::HEATING, &modes),
            own_cooling: own(mrcsf::reader(), cooling_id, hvac::COOLING, &modes),
            answers: Vec::new(),
            actor: None,
            seen: Vec::new(),
            now,
        }
    }

    /// Both discovery reads, and nothing else.
    fn discover(&mut self) {
        let ours = node_management(self.manager.device().address());
        let theirs = node_management(self.room.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager.read(&theirs, &ours, function, self.now);
        }
        self.settle();
    }

    /// The room as the manager knows it once discovery has settled.
    fn peer(&self) -> &eebus::spine::RemoteDevice {
        let device = self.room.device().address().clone();
        self.manager.peer(&device).expect("the room")
    }

    /// Discovery, then every function of the three features — and **no binding request**.
    fn commission(&mut self) {
        self.discover();

        for function in [
            Function::HvacSystemFunctionDescriptionListData,
            Function::HvacOperationModeDescriptionListData,
            Function::HvacSystemFunctionOperationModeRelationListData,
            Function::HvacSystemFunctionListData,
            Function::HvacSystemFunctionSetpointRelationListData,
        ] {
            self.manager
                .read(&self.hvac_feature.clone(), &self.client, function, self.now);
        }
        for function in [
            Function::SetpointDescriptionListData,
            Function::SetpointConstraintsListData,
            Function::SetpointListData,
        ] {
            self.manager.read(
                &self.setpoint_feature.clone(),
                &self.client,
                function,
                self.now,
            );
        }
        for function in [
            Function::MeasurementDescriptionListData,
            Function::MeasurementListData,
        ] {
            self.manager.read(
                &self.measurement_feature.clone(),
                &self.client,
                function,
                self.now,
            );
        }
        for feature in [
            self.hvac_feature.clone(),
            self.setpoint_feature.clone(),
            self.measurement_feature.clone(),
        ] {
            self.manager
                .request_subscription(&self.client, &feature, self.now);
        }
        self.settle();
    }

    fn write(&mut self, feature: &eebus::model::FeatureAddress, data: eebus::model::CmdData) {
        self.manager
            .write(feature, &self.client.clone(), data, true, self.now);
        self.settle();
    }

    fn settle(&mut self) {
        // The room's own view of what it published, for deciding on writes.
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.room.handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.room.poll_transmit() {
                self.manager
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }

            let events: Vec<SpineEvent> = core::iter::from_fn(|| self.room.poll_event()).collect();
            for event in events {
                moved = true;
                let SpineEvent::WriteRequested(write) = event else {
                    continue;
                };
                // A setpoint write, or a mode write. The room decides before it answers.
                //
                // **Which** setpoint was written comes from the *fragment*, and what it now
                // holds from the *resolved* state. Reading the id out of `resolved` would
                // find the room's other setpoint too — it is in the same list, unchanged —
                // and attribute the write to whichever came first.
                let written: Vec<SetpointId> = match &write.data {
                    eebus::model::CmdData::SetpointListData(list) => list
                        .setpoint_data
                        .iter()
                        .flatten()
                        .filter_map(|entry| entry.setpoint_id)
                        .collect(),
                    _ => Vec::new(),
                };
                let decision = if let Some(id) = written.first().copied() {
                    let degrees = crht::read_setpoint_write(&write.resolved, id)
                        .expect("the resolved state holds what it now is");
                    Ok(Applied::Setpoint(id, degrees))
                } else {
                    // The room keeps a reader per system function — one `HVAC` feature
                    // carries both — and offers the write to each in turn. `NotAddressed`
                    // is "this names the other function"; only when neither claims it is it
                    // an answer to send back.
                    let mut decided = Err(ModeRefused::NotAddressed);
                    for own in [&self.own_heating, &self.own_cooling] {
                        match own.apply(&write.data) {
                            Err(ModeRefused::NotAddressed) => continue,
                            other => {
                                decided = other;
                                break;
                            }
                        }
                    }
                    decided.map(|request| match request {
                        Request::SetMode(mode) => Applied::Mode(mode),
                        other => panic!("no overrun in a room: {other:?}"),
                    })
                };
                match &decision {
                    Ok(_) => {
                        self.room
                            .accept_write(write.token, self.now)
                            .expect("the feature can store it");
                    }
                    Err(refused) => {
                        self.room
                            .reject_write(write.token, refused.error_number(), self.now);
                    }
                }
                self.applied.push(decision);
            }

            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                moved = true;
                if let Some(actor) = self.actor.as_mut() {
                    self.seen.extend(actor.handle_event(event));
                }
                if let SpineEvent::ResultReceived { request, error } = event {
                    self.answers.push((*request, *error));
                }
                if let SpineEvent::ReplyReceived { resolved, .. }
                | SpineEvent::DataNotified { resolved, .. } = event
                {
                    self.setpoints.learn(resolved);
                    self.heating.learn(resolved);
                    self.cooling.learn(resolved);
                    if !self.readings.describe(resolved) {
                        self.readings.apply(resolved);
                    }
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }
}

/// What the room knows about one of its own system functions: the payloads it just
/// published, read back. A server decides on a write against its own published state,
/// which is the only thing a peer could have been going by.
fn own(
    mut reader: SystemFunction,
    id: eebus::model::HvacSystemFunctionId,
    kind: eebus::model::HvacSystemFunctionType,
    modes: &[HvacOperationModeType],
) -> SystemFunction {
    reader.learn(&hvac::system_function::descriptions(&[(id, kind)]));
    reader.learn(&hvac::system_function::operation_mode_descriptions(modes).unwrap());
    reader.learn(&hvac::system_function::operation_mode_relations(id, modes).unwrap());
    reader.learn(&hvac::system_function::states(&[(
        id,
        auto(),
        false,
        Some(true),
    )]));
    reader
}

fn set(engine: &mut Engine, address: &eebus::model::FeatureAddress, data: eebus::model::CmdData) {
    engine
        .device_mut()
        .resolve_mut(address)
        .expect("the feature")
        .set_data(data)
        .expect("publishable");
}

fn auto() -> HvacOperationModeId {
    mrhsf::operation_mode_id(&HvacOperationModeType::Auto).expect("a numbered mode")
}
fn off() -> HvacOperationModeId {
    mrhsf::operation_mode_id(&HvacOperationModeType::Off).expect("a numbered mode")
}

fn round_trip(datagram: &eebus::model::Datagram) -> eebus::model::Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    eebus::model::from_json_str(&wire).expect("decode")
}

/// The two functions are read separately from one feature, and neither shadows the other.
#[test]
fn the_heating_and_the_cooling_are_two_system_functions_on_one_feature() {
    let mut room = Room::new();
    room.commission();

    assert!(room.heating.is_complete());
    assert!(room.cooling.is_complete());
    assert_ne!(
        room.heating.system_function(),
        room.cooling.system_function(),
        "one `HVAC` feature, two `systemFunctionId`s"
    );
    assert_eq!(room.heating.mode(), Some(&HvacOperationModeType::Auto));
    assert_eq!(room.cooling.mode(), Some(&HvacOperationModeType::Auto));
    assert_eq!(
        room.heating.mode_id(),
        room.cooling.mode_id(),
        "and the *same* operation mode, which is exactly why the relations need both"
    );
}

/// The relation is keyed by the pair, so the two setpoints do not collide.
///
/// This is the failure the pairing exists to prevent: a reader keyed by `operationModeId`
/// alone stores one entry for `auto`, whichever arrived last, and then tells a manager that
/// heating to 21 °C means writing the *cooling* setpoint. The room applies it, acknowledges
/// it, and gets colder.
#[test]
fn a_setpoint_relation_is_keyed_by_the_function_and_the_mode() {
    let mut room = Room::new();
    room.commission();

    let heating = hvac::system_function_id(&hvac::HEATING);
    let cooling = hvac::system_function_id(&hvac::COOLING);

    assert_eq!(room.setpoints.for_mode(heating, auto()), [THEIR_HEATING]);
    assert_eq!(room.setpoints.for_mode(cooling, auto()), [THEIR_COOLING]);
    assert_eq!(room.setpoints.for_mode(heating, off()), []);
    assert!(room.setpoints.describes(heating, off()));

    // And the same reader answers both, because both are `roomAirTemperature`.
    let described: Vec<_> = room.setpoints.temperature_setpoints().collect();
    assert_eq!(
        described,
        [THEIR_HEATING, THEIR_COOLING],
        "one scope, two setpoints; the descriptions cannot tell them apart"
    );

    // Which the effect gate then uses: the heating setpoint is effective under the heating
    // function and not under the cooling one.
    assert_eq!(
        room.setpoints.effect_of(THEIR_HEATING, &room.heating),
        SetpointEffect::Effective
    );
    assert_eq!(
        room.setpoints.effect_of(THEIR_HEATING, &room.cooling),
        SetpointEffect::NotInCurrentMode,
        "the cooling function reads the other setpoint"
    );
}

/// [CRHT-001]: the manager writes a room heating setpoint, and it lands — with no binding.
///
/// Every HVAC specification says "Binding SHOULD NOT be used for this Scenario", so nothing
/// above ever sent a binding request. A server that required one would answer this write
/// with `errorNumber` 9 and the use case would not run at all.
#[test]
fn crht_001_a_room_heating_setpoint_is_written_without_a_binding() {
    let mut room = Room::new();
    room.commission();

    assert!(
        room.manager.relations().bindings().is_empty(),
        "the manager holds no binding, and asked for none"
    );

    let write = room
        .setpoints
        .write_effective(THEIR_HEATING, 21.5, &room.heating)
        .expect("the room is in a mode that reads it");
    room.write(&room.setpoint_feature.clone(), write);

    assert_eq!(
        room.applied,
        [Ok(Applied::Setpoint(THEIR_HEATING, 21.5))],
        "the write reached the application rather than being refused at the door"
    );
    assert_eq!(
        room.setpoints.temperature(THEIR_HEATING),
        Some(21.5),
        "and the notification came back through the subscription"
    );
    assert_eq!(
        room.setpoints.temperature(THEIR_COOLING),
        Some(26.0),
        "leaving the cooling setpoint alone"
    );
}

/// [CRCT-001]: the same, for cooling, refused where it is out of the published range.
#[test]
fn crct_001_the_cooling_setpoint_has_its_own_constraints() {
    let mut room = Room::new();
    room.commission();

    // The cooling range is 18..=30 and the heating range 16..=26 — two setpoints, two
    // constraints, on one feature.
    assert!(matches!(
        room.setpoints.write(THEIR_COOLING, 16.0),
        Err(WriteRefused::OutOfRange { .. })
    ));
    assert!(matches!(
        room.setpoints.write(THEIR_HEATING, 28.0),
        Err(WriteRefused::OutOfRange { .. })
    ));

    let write = room
        .setpoints
        .write_effective(THEIR_COOLING, 24.0, &room.cooling)
        .expect("in range and in the current mode");
    room.write(&room.setpoint_feature.clone(), write);
    assert_eq!(room.applied, [Ok(Applied::Setpoint(THEIR_COOLING, 24.0))]);
}

/// A write into the mode the room is *not* in never reaches the wire.
#[test]
fn a_setpoint_the_current_mode_does_not_read_is_refused_before_it_is_sent() {
    let mut room = Room::new();
    room.commission();

    // Put the heating into `off`, which this room relates to no setpoint at all.
    let heating_id = hvac::system_function_id(&hvac::HEATING);
    let cooling_id = hvac::system_function_id(&hvac::COOLING);
    set(
        &mut room.room,
        &room.hvac_feature.clone(),
        hvac::system_function::states(&[
            (heating_id, off(), false, Some(true)),
            (cooling_id, auto(), false, Some(true)),
        ]),
    );
    let feature = room.hvac_feature.clone();
    room.room
        .notify(&feature, &Function::HvacSystemFunctionListData, room.now);
    room.settle();

    assert_eq!(room.heating.mode(), Some(&HvacOperationModeType::Off));
    assert_eq!(
        room.setpoints.effect_of(THEIR_HEATING, &room.heating),
        SetpointEffect::NotInCurrentMode
    );
    assert!(matches!(
        room.setpoints
            .write_effective(THEIR_HEATING, 21.5, &room.heating),
        Err(WriteRefused::NotInCurrentMode)
    ));
    assert!(
        room.applied.is_empty(),
        "nothing was sent, so nothing was applied"
    );

    // The cooling function is still in `auto`, and its setpoint is still reachable.
    assert_eq!(
        room.setpoints.effect_of(THEIR_COOLING, &room.cooling),
        SetpointEffect::Effective,
        "one function changed mode; the other did not"
    );
}

/// [CRHSF-001]: the manager sets the room heating operation mode, by name.
#[test]
fn crhsf_001_the_operation_mode_is_set_over_the_wire() {
    let mut room = Room::new();
    room.commission();

    let write = room
        .heating
        .set_mode_named(&HvacOperationModeType::Off)
        .expect("this room has `off`");
    room.write(&room.hvac_feature.clone(), write);

    assert_eq!(room.applied, [Ok(Applied::Mode(off()))]);
    assert_eq!(
        room.heating.mode(),
        Some(&HvacOperationModeType::Off),
        "and the notification came back"
    );
    assert_eq!(
        room.cooling.mode(),
        Some(&HvacOperationModeType::Auto),
        "the cooling function was not touched: the write named one `systemFunctionId`"
    );
}

/// A mode this room does not have is refused before anything is sent.
#[test]
fn a_mode_the_room_never_described_is_refused() {
    let mut room = Room::new();
    room.commission();

    assert_eq!(
        room.heating.set_mode_named(&HvacOperationModeType::Eco),
        Err(ModeRefused::NotRelated),
        "this room has `auto` and `off`"
    );
    assert_eq!(
        ModeRefused::NotRelated.error_number(),
        eebus::model::ErrorNumber::DestinationUnknown
    );
}

/// [CRHT-004]: the setpoint points at the measurement that reports it.
#[test]
fn crht_004_the_setpoint_and_the_measurement_are_the_same_temperature() {
    let mut room = Room::new();
    room.commission();

    assert_eq!(
        room.readings.value(&mrt::MEASURAND),
        Some(19.5),
        "MRT says what the room is"
    );
    assert_eq!(
        room.setpoints.temperature(THEIR_HEATING),
        Some(20.0),
        "and CRHT says what it was asked to be"
    );
    // Which is the point: 19.5 against 20.0 is a room still warming up, and neither number
    // alone says so.
    assert!(room.setpoints.unit_of(THEIR_HEATING) == Some(&UnitOfMeasurement::DegC));
}

/// A mode write names one `systemFunctionId`, and the room's other function ignores it.
///
/// A server with two functions on one feature offers each write to both readers.
/// `NotAddressed` is what lets it dispatch; without it the first reader would refuse
/// everything meant for the second.
#[test]
fn a_write_for_one_system_function_is_not_claimed_by_the_other() {
    use eebus::usecases::hvac::system_function;

    let mut room = Room::new();
    room.commission();

    let heating = hvac::system_function_id(&hvac::HEATING);
    let cooling = hvac::system_function_id(&hvac::COOLING);

    let to_heating = system_function::set_operation_mode(heating, off());
    assert_eq!(
        room.own_cooling.apply(&to_heating),
        Err(ModeRefused::NotAddressed),
        "the cooling reader hands it on rather than refusing it"
    );
    assert_eq!(
        room.own_heating.apply(&to_heating),
        Ok(Request::SetMode(off()))
    );

    let to_cooling = system_function::set_operation_mode(cooling, off());
    assert_eq!(
        room.own_heating.apply(&to_cooling),
        Err(ModeRefused::NotAddressed)
    );
    assert_eq!(
        room.own_cooling.apply(&to_cooling),
        Ok(Request::SetMode(off()))
    );

    // And over the wire, both land where they were addressed.
    let feature = room.hvac_feature.clone();
    room.write(&feature, to_cooling);
    assert_eq!(room.applied, [Ok(Applied::Mode(off()))]);
    assert_eq!(
        room.cooling.mode(),
        Some(&HvacOperationModeType::Off),
        "the cooling function moved"
    );
    assert_eq!(
        room.heating.mode(),
        Some(&HvacOperationModeType::Auto),
        "and the heating did not"
    );
}

/// [B1] Four use cases on one `HVAC` feature, each located and followed on its own.
///
/// The hard case for a lookup by feature type: this room serves heating, cooling and both
/// temperatures from the *same* `HVAC` feature, so every one of the four `locate` calls
/// returns the same address — and that is right, because §3.2.2.2.1 gives an entity one
/// feature of a type. What tells the use cases apart is not the address but the
/// `systemFunctionType` each reader is told to follow, which is why locating cannot be the
/// end of it.
#[test]
fn every_use_case_on_one_feature_is_located_and_followed_separately() {
    let mut room = Room::new();
    room.discover();

    let (heating, cooling, warm, cool, thermometer) = {
        let remote = room.peer();
        (
            crhsf::locate(remote).expect("room heating"),
            crcsf::locate(remote).expect("room cooling"),
            crht::locate(remote).expect("the heating setpoint"),
            crct::locate(remote).expect("the cooling setpoint"),
            mrt::locate(remote).expect("the thermometer"),
        )
    };
    for peer in [&heating, &cooling, &warm, &cool] {
        assert_eq!(
            peer.hvac, room.hvac_feature,
            "one entity, one `HVAC` feature, four use cases"
        );
    }
    assert_eq!(heating.setpoint, None);
    assert_eq!(warm.setpoint, Some(room.setpoint_feature.clone()));
    assert_eq!(cool.setpoint, Some(room.setpoint_feature.clone()));
    assert_eq!(
        thermometer.measurement,
        Some(room.measurement_feature.clone())
    );

    let client = room.client.clone();
    let now = room.now;
    let following = heating.follow(&mut room.manager, &client, now);
    cooling.follow(&mut room.manager, &client, now);
    warm.follow(&mut room.manager, &client, now);
    room.settle();

    for (counter, error) in &room.answers {
        assert_eq!(
            *error,
            eebus::spine::ErrorNumber::None,
            "{:?} was refused",
            following.function_of(*counter)
        );
    }

    // Both readers were fed from the same four payloads and disagree about the mode,
    // because they follow different system functions.
    assert!(room.heating.is_complete() && room.cooling.is_complete());
    assert_eq!(room.heating.mode(), Some(&HvacOperationModeType::Auto));
    assert_eq!(
        room.heating.system_function(),
        Some(hvac::system_function_id(&hvac::HEATING))
    );
    assert_eq!(
        room.cooling.system_function(),
        Some(hvac::system_function_id(&hvac::COOLING))
    );
    assert_eq!(
        room.heating.overrun(),
        None,
        "no overrun is in scope outside the hot water"
    );

    // The join, for the heating: the setpoint `auto` actually uses.
    assert_eq!(
        room.heating.current_setpoints(&room.setpoints),
        [THEIR_HEATING]
    );
    assert_eq!(
        room.cooling.current_setpoints(&room.setpoints),
        [THEIR_COOLING],
        "the same mode identifier, a different function, a different setpoint"
    );
}

/// [B1] One room, four use cases, one actor — and the heating and the cooling do not mix.
///
/// The hardest case the family has, and the one every part of this design exists for. This
/// room serves `crhsf`, `crcsf`, `crht` and `crct` from **one** `HVAC` feature and **one**
/// `Setpoint` feature: both system functions arrive in the same lists under the same
/// `operationModeId`s, and both setpoints are `roomAirTemperature` in `degC`. Nothing about
/// a payload says which is which. What separates them is the `systemFunctionId` the
/// relations are keyed by, and a manager that got it wrong would ask for 21 °C of heating
/// and write the cooling setpoint — the room applies it, acknowledges it, and gets colder.
#[test]
fn the_actor_keeps_a_rooms_heating_and_cooling_apart() {
    let mut room = Room::new();
    room.actor = Some(HvacApplianceActor::new(room.client.clone()));
    room.discover();

    let peers = {
        let remote = room.peer();
        [
            crhsf::locate(remote).expect("room heating"),
            crcsf::locate(remote).expect("room cooling"),
            crht::locate(remote).expect("the heating setpoint"),
            crct::locate(remote).expect("the cooling setpoint"),
        ]
    };
    let unit = peers[0].id();
    assert!(
        peers.iter().all(|peer| peer.id() == unit),
        "four use cases, one entity, one unit"
    );

    let now = room.now;
    let mut actor = room.actor.take().expect("the actor");
    for peer in peers {
        actor.attach(&mut room.manager, peer, now);
    }
    room.actor = Some(actor);
    room.settle();

    let actor = room.actor.as_ref().expect("the actor");
    assert_eq!(actor.units().count(), 1);
    for function in [hvac::HEATING, hvac::COOLING] {
        assert!(
            room.seen.contains(&HvacEvent::FunctionDescribed {
                unit: unit.clone(),
                function: function.clone(),
            }),
            "{function:?} was described: {:?}",
            room.seen
        );
        assert_eq!(
            actor.mode(&unit, &function),
            Some(&HvacOperationModeType::Auto)
        );
    }

    // The whole point: the same mode, the same reader, two different setpoints.
    assert_eq!(
        actor.temperature(&unit, &hvac::HEATING),
        Some((THEIR_HEATING, 20.0))
    );
    assert_eq!(
        actor.temperature(&unit, &hvac::COOLING),
        Some((THEIR_COOLING, 26.0))
    );
    assert!(
        core::ptr::eq(
            actor.setpoints(&unit, &hvac::HEATING).expect("a reader"),
            actor.setpoints(&unit, &hvac::COOLING).expect("a reader"),
        ),
        "and it really is one reader: they share `roomAirTemperature`"
    );

    // Ask for warmth, and only the heating setpoint moves.
    room.seen.clear();
    let actor = room.actor.take().expect("the actor");
    actor
        .set_temperature(&mut room.manager, &unit, &hvac::HEATING, 21.5, now)
        .expect("the room is in a mode that reads it");
    room.actor = Some(actor);
    room.settle();

    assert_eq!(room.applied, [Ok(Applied::Setpoint(THEIR_HEATING, 21.5))]);
    assert!(
        room.seen.contains(&HvacEvent::SetpointChanged {
            unit: unit.clone(),
            setpoint: THEIR_HEATING,
            degrees: 21.5,
        }),
        "reported back under the identifier the room published: {:?}",
        room.seen
    );
    let actor = room.actor.as_ref().expect("the actor");
    assert_eq!(
        actor.temperature(&unit, &hvac::HEATING),
        Some((THEIR_HEATING, 21.5))
    );
    assert_eq!(
        actor.temperature(&unit, &hvac::COOLING),
        Some((THEIR_COOLING, 26.0)),
        "and the cooling setpoint was not touched"
    );

    // `off` relates to no setpoint at all [CRHT-003/3], so there is nothing to set.
    room.seen.clear();
    let actor = room.actor.take().expect("the actor");
    actor
        .set_mode(
            &mut room.manager,
            &unit,
            &hvac::HEATING,
            &HvacOperationModeType::Off,
            now,
        )
        .expect("a mode this room relates to");
    room.actor = Some(actor);
    room.settle();

    let actor = room.actor.as_ref().expect("the actor");
    assert_eq!(
        actor.mode(&unit, &hvac::HEATING),
        Some(&HvacOperationModeType::Off)
    );
    assert_eq!(
        actor.mode(&unit, &hvac::COOLING),
        Some(&HvacOperationModeType::Auto),
        "the write named one system function and moved only that one"
    );
    assert_eq!(
        actor.set_temperature(&mut room.manager, &unit, &hvac::HEATING, 22.0, now),
        Err(WriteRefused::NotInCurrentMode),
        "an `off` heating reads no setpoint; a write would be applied and heat nothing"
    );
    assert_eq!(
        actor.temperature(&unit, &hvac::HEATING),
        None,
        "and there is no temperature it is working to"
    );
    assert!(
        actor
            .set_temperature(&mut room.manager, &unit, &hvac::COOLING, 25.0, now)
            .is_ok(),
        "the cooling is still in `auto`, and its setpoint is still live"
    );

    // No overrun outside the hot water, and the actor says so rather than inventing one.
    assert_eq!(
        actor
            .start_overrun(&mut room.manager, &unit, &hvac::HEATING, now)
            .unwrap_err(),
        ModeRefused::NoOverrun
    );
}
