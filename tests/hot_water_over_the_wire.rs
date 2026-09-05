//! The DHW trio — CDT, MDSF and MDT — both actors of each, over real datagrams.
//!
//! A DHW Circuit serving CDT must serve one of the DHW System Function use cases too
//! [CDT-005], and §3.2.2.2.1 gives it **one** `HVAC` feature to do both from. So this
//! circuit is the conformant shape: one `Setpoint` feature, one `HVAC` feature carrying
//! MDSF's six functions and CDT's relations together, and one `Measurement` feature for
//! MDT — whose `measurementId` is the one CDT's setpoint description points at.
//!
//! The DHW Circuit here is built by hand and numbers its setpoints its own way: Table 7's
//! `<st1#(1..4)>` says a circuit publishes one to four temperature setpoints and chooses
//! the identifiers, and this one keeps a *room air* temperature on `1` and the hot water on
//! `3`. Writing 60 °C to setpoint 1 would be accepted, applied, and would heat a living
//! room — which is why a Configuration Appliance resolves the identifier from
//! `scopeType: dhwTemperature` and never from its own numbering.

use core::time::Duration;

use eebus::model::{
    CmdData, DeviceType, EntityType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacOverrunStatus, ScaledNumber, ScopeType, SetpointDescriptionData,
    SetpointDescriptionListData, SetpointId, SetpointType, UnitOfMeasurement,
};
use eebus::spine::{Engine, LocalDevice, LocalEntity, SpineEvent, node_management};
use eebus::usecases::hvac::cdt;
use eebus::usecases::hvac::mdt;
use eebus::usecases::hvac::setpoint::{SetpointEffect, Setpoints, WriteRefused};
use eebus::usecases::hvac::system_function::{ModeRefused, OverrunReport, Request, SystemFunction};
use eebus::usecases::hvac::{cdsf, mdsf};
use eebus::usecases::monitoring::Readings;

/// The circuit's own identifier for the hot water. Not 1, which is its room air setpoint.
const THEIR_DHW: SetpointId = SetpointId(3);
const THEIR_ROOM: SetpointId = SetpointId(1);

struct Pair {
    manager: Engine,
    circuit: Engine,
    known: Setpoints,
    /// What MDSF told the appliance about the same circuit.
    state: SystemFunction,
    /// And what MDT said the water actually got to.
    readings: Readings,
    measurement_feature: eebus::model::FeatureAddress,
    setpoint_feature: eebus::model::FeatureAddress,
    hvac_feature: eebus::model::FeatureAddress,
    client: eebus::model::FeatureAddress,
    now: Duration,
    /// Every write the circuit was asked to make, once it has applied it.
    applied: Vec<(SetpointId, f64)>,
    /// The circuit's own view of what it published, which is what it decides against.
    own: SystemFunction,
    /// Every CDSF request it was sent, and what it decided.
    requested: Vec<Result<Request, ModeRefused>>,
    /// Every acknowledgement the manager got back, by the counter it asked under.
    answers: Vec<(eebus::model::MsgCounter, eebus::spine::ErrorNumber)>,
}

impl Pair {
    fn new() -> Self {
        let now = Duration::ZERO;

        let mut device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::DHWCircuit)
                    .with_feature(cdt::setpoint_feature(1))
                    // One HVAC feature for all three use cases (§3.2.2.2.1): MDSF's six
                    // functions, CDSF's writes on two of them, and CDT's relations.
                    .with_feature(cdsf::with_cdt(2))
                    .with_feature(mdt::measurement_feature(3)),
            )
            .unwrap();
        let setpoint_feature = device.address_of(&[1], 1);
        let hvac_feature = device.address_of(&[1], 2);
        let measurement_feature = device.address_of(&[1], 3);
        let mut circuit = Engine::new(device);
        circuit.add_use_case([1], 1, &cdt::DHW_CIRCUIT);
        // Scenario 2 is only `R` of a circuit, and this one offers it.
        circuit.add_use_case([1], 1, &mdsf::DHW_CIRCUIT);
        circuit.add_use_case([1], 1, &cdsf::DHW_CIRCUIT);
        circuit.add_use_case([1], 1, &mdt::DHW_CIRCUIT);

        // MDT: what the water is, which is a different question from what was asked for.
        for payload in [
            mdt::temperature_description(),
            mdt::temperature_constraints(0.0, 95.0, Some(0.1)),
            mdt::temperature(49.0),
        ] {
            set(&mut circuit, &measurement_feature, payload);
        }

        // What this circuit publishes, in its own numbering.
        set(&mut circuit, &setpoint_feature, their_descriptions());
        set(&mut circuit, &setpoint_feature, their_constraints());
        set(&mut circuit, &setpoint_feature, their_value(52.0));
        // MDSF: what the circuit is and which modes it has.
        let modes = [
            HvacOperationModeType::Auto,
            HvacOperationModeType::On,
            HvacOperationModeType::Off,
        ];
        for payload in [
            mdsf::system_function_description(),
            mdsf::operation_mode_descriptions(&modes).expect("three modes"),
            mdsf::operation_mode_relations(&modes).expect("three modes"),
            mdsf::system_function_state(auto(), false, Some(true)),
            mdsf::overrun_description(),
            mdsf::overrun_state(HvacOverrunStatus::Inactive),
            // CDT: which setpoint each of those modes reads.
            cdt::system_function_relations(&[
                (auto(), HvacOperationModeType::Auto, vec![THEIR_DHW]),
                (on(), HvacOperationModeType::On, vec![THEIR_DHW]),
                (off(), HvacOperationModeType::Off, vec![]),
            ])
            .expect("well-formed relations"),
        ] {
            set(&mut circuit, &hvac_feature, payload);
        }

        let mut manager_device = LocalDevice::new(
            "i:12345",
            "EnergyManager-1",
            DeviceType::EnergyManagementSystem,
        )
        .unwrap();
        manager_device
            .add_entity(
                LocalEntity::new([1], EntityType::CEM)
                    // One `Generic` client feature for everything this actor reads and
                    // writes (LPC IG §3.3), which is the same one an Energy Guard holds.
                    .with_feature(eebus::usecases::limitation::client_feature(1)),
            )
            .unwrap();
        let client = manager_device.address_of(&[1], 1);
        let mut manager = Engine::new(manager_device);
        for descriptor in [
            &cdt::CONFIGURATION_APPLIANCE,
            &cdsf::CONFIGURATION_APPLIANCE,
            &mdsf::MONITORING_APPLIANCE,
        ] {
            manager.add_use_case([1], 1, descriptor);
        }

        // What the circuit knows about itself: the same six payloads it just published,
        // read back. A server decides on a write against its own published state, which is
        // the only thing a peer could have been going by.
        let mut own = cdsf::reader();
        for payload in [
            cdsf::system_function_description(),
            cdsf::operation_mode_descriptions(&modes).expect("three modes"),
            cdsf::operation_mode_relations(&modes).expect("three modes"),
            cdsf::overrun_description(),
            cdsf::system_function_state(auto(), false, Some(true)),
        ] {
            own.learn(&payload);
        }

        Self {
            manager,
            circuit,
            known: cdt::reader(),
            state: mdsf::reader(),
            readings: Readings::new(),
            measurement_feature,
            setpoint_feature,
            hvac_feature,
            client,
            now,
            applied: Vec::new(),
            own,
            requested: Vec::new(),
            answers: Vec::new(),
        }
    }

    /// Discovery, then the three reads a Configuration Appliance owes before it writes.
    fn commission(&mut self) {
        let ours = node_management(self.manager.device().address());
        let theirs = node_management(self.circuit.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.manager
                .read(&theirs, &ours, function.clone(), self.now);
            self.circuit.read(&ours, &theirs, function, self.now);
        }
        self.settle();

        // §3.4.1.1: **no binding**, and that is the specification's own instruction —
        // "Binding SHOULD NOT be used for this Scenario". Subscribe, to hear the circuit
        // change its own mind, and then write. Everything below this line is a
        // Configuration Appliance that never bound to anything.
        self.manager
            .request_subscription(&self.client, &self.setpoint_feature.clone(), self.now);
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
            // The descriptions first: they say which system function and which modes the
            // rest of it is talking about.
            Function::HvacSystemFunctionDescriptionListData,
            Function::HvacOperationModeDescriptionListData,
            Function::HvacSystemFunctionOperationModeRelationListData,
            Function::HvacSystemFunctionListData,
            Function::HvacOverrunDescriptionListData,
            Function::HvacOverrunListData,
            Function::HvacSystemFunctionSetpointRelationListData,
        ] {
            self.manager
                .read(&self.hvac_feature.clone(), &self.client, function, self.now);
        }
        self.manager
            .request_subscription(&self.client, &self.hvac_feature.clone(), self.now);
        for function in [
            Function::MeasurementDescriptionListData,
            Function::MeasurementConstraintsListData,
            Function::MeasurementListData,
        ] {
            self.manager.read(
                &self.measurement_feature.clone(),
                &self.client,
                function,
                self.now,
            );
        }
        self.manager.request_subscription(
            &self.client,
            &self.measurement_feature.clone(),
            self.now,
        );
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = self.manager.poll_transmit() {
                self.circuit
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            while let Some(datagram) = self.circuit.poll_transmit() {
                self.manager
                    .handle_datagram(&round_trip(&datagram), self.now);
                moved = true;
            }
            // The circuit applies what it is asked for, inside what it published.
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.circuit.poll_event()).collect();
            for event in events {
                if let SpineEvent::WriteRequested(write) = event {
                    // Two kinds of write reach this circuit: CDT's setpoint on the
                    // `Setpoint` feature, and CDSF's mode or overrun on the `HVAC` one.
                    if let Some(degrees) = cdt::read_setpoint_write(&write.resolved, THEIR_DHW) {
                        // `resolved`, not `data`: a partial write leaves the rest unchanged.
                        self.circuit
                            .accept_write(write.token, self.now)
                            .expect("the feature can store it");
                        self.applied.push((THEIR_DHW, degrees));
                    } else {
                        // CDSF: the circuit decides against its own published state before
                        // it acknowledges, exactly as LPC's Controllable System does.
                        match self.own.apply(&write.data) {
                            Ok(request) => {
                                self.circuit
                                    .accept_write(write.token, self.now)
                                    .expect("the feature can store it");
                                self.requested.push(Ok(request));
                            }
                            Err(refused) => {
                                self.circuit.reject_write(
                                    write.token,
                                    refused.error_number(),
                                    self.now,
                                );
                                self.requested.push(Err(refused));
                            }
                        }
                    }
                }
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.manager.poll_event()).collect();
            for event in &events {
                if let SpineEvent::ResultReceived { request, error } = event {
                    self.answers.push((*request, *error));
                }
                if let SpineEvent::ReplyReceived { resolved, .. }
                | SpineEvent::DataNotified { resolved, .. } = event
                {
                    // Each reader takes what belongs to it; a payload of the other's kind
                    // is ignored rather than refused.
                    self.known.learn(resolved);
                    self.state.learn(resolved);
                    if !self.readings.describe(resolved) {
                        self.readings.apply(resolved);
                    }
                }
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }
}

fn auto() -> HvacOperationModeId {
    mdsf::operation_mode_id(&HvacOperationModeType::Auto).expect("a numbered mode")
}
fn on() -> HvacOperationModeId {
    mdsf::operation_mode_id(&HvacOperationModeType::On).expect("a numbered mode")
}
fn off() -> HvacOperationModeId {
    mdsf::operation_mode_id(&HvacOperationModeType::Off).expect("a numbered mode")
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

/// Two setpoints in the same unit, and only one of them is the hot water.
fn their_descriptions() -> CmdData {
    CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
        setpoint_description_data: Some(vec![
            SetpointDescriptionData {
                setpoint_id: Some(THEIR_ROOM),
                setpoint_type: Some(SetpointType::ValueAbsolute),
                unit: Some(UnitOfMeasurement::DegC),
                scope_type: Some(ScopeType::RoomAirTemperature),
                ..Default::default()
            },
            SetpointDescriptionData {
                setpoint_id: Some(THEIR_DHW),
                setpoint_type: Some(SetpointType::ValueAbsolute),
                unit: Some(UnitOfMeasurement::DegC),
                scope_type: Some(ScopeType::DhwTemperature),
                ..Default::default()
            },
        ]),
    })
}

fn their_constraints() -> CmdData {
    use eebus::model::{SetpointConstraintsData, SetpointConstraintsListData};
    CmdData::SetpointConstraintsListData(SetpointConstraintsListData {
        setpoint_constraints_data: Some(vec![
            SetpointConstraintsData {
                setpoint_id: Some(THEIR_ROOM),
                setpoint_range_min: Some(ScaledNumber::from_f64(16.0, 1)),
                setpoint_range_max: Some(ScaledNumber::from_f64(26.0, 1)),
                setpoint_step_size: Some(ScaledNumber::from_f64(0.5, 1)),
            },
            SetpointConstraintsData {
                setpoint_id: Some(THEIR_DHW),
                setpoint_range_min: Some(ScaledNumber::from_f64(40.0, 1)),
                setpoint_range_max: Some(ScaledNumber::from_f64(65.0, 1)),
                setpoint_step_size: Some(ScaledNumber::from_f64(1.0, 1)),
            },
        ]),
    })
}

fn their_value(degrees: f64) -> CmdData {
    use eebus::model::{SetpointData, SetpointListData};
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(vec![SetpointData {
            setpoint_id: Some(THEIR_DHW),
            value: Some(ScaledNumber::from_f64(degrees, 1)),
            is_setpoint_changeable: Some(true),
            ..Default::default()
        }]),
    })
}

/// [CDT-001]: the appliance finds the hot water setpoint and raises it.
#[test]
fn the_appliance_finds_the_hot_water_setpoint_and_sets_it() {
    let mut pair = Pair::new();
    pair.commission();

    let found: Vec<SetpointId> = pair.known.temperature_setpoints().collect();
    assert_eq!(
        found,
        [THEIR_DHW],
        "the room air setpoint on 1 is not a DHW temperature"
    );
    assert_eq!(pair.known.temperature(THEIR_DHW), Some(52.0));
    assert_eq!(
        pair.known.unit_of(THEIR_DHW),
        Some(&UnitOfMeasurement::DegC)
    );

    // Store the afternoon's surplus as hot water.
    let write = pair
        .known
        .write(THEIR_DHW, 60.0)
        .expect("60 °C is inside what the circuit published");
    pair.manager.write(
        &pair.setpoint_feature.clone(),
        &pair.client.clone(),
        write,
        true,
        pair.now,
    );
    pair.settle();

    assert_eq!(
        pair.applied,
        [(THEIR_DHW, 60.0)],
        "the circuit applied the temperature it was sent"
    );
    assert_eq!(
        pair.known.temperature(THEIR_DHW),
        Some(60.0),
        "and notified the appliance, which had subscribed by writing"
    );
}

/// The circuit's own constraints are what a write is checked against, before it is sent.
#[test]
fn a_temperature_the_circuit_would_not_take_never_reaches_the_wire() {
    let mut pair = Pair::new();
    pair.commission();

    assert!(matches!(
        pair.known.write(THEIR_DHW, 70.0),
        Err(WriteRefused::OutOfRange { max, .. }) if max == 65.0
    ));
    // And the room air setpoint is not a DHW setpoint, whatever its scope's unit.
    assert_eq!(
        pair.known.write(THEIR_ROOM, 22.0),
        Err(WriteRefused::UnknownSetpoint)
    );
    assert!(
        pair.applied.is_empty(),
        "nothing was written, so nothing was applied"
    );
}

/// Table 10: which setpoint a mode uses, which is what says whether a write takes effect.
#[test]
fn the_relations_name_the_setpoint_each_operation_mode_uses() {
    let mut pair = Pair::new();
    pair.commission();

    // Both identifiers: `systemFunctionId` is the relation's PRIMARY identifier and
    // `operationModeId` its SUB, and a circuit that also heated a room would relate the
    // same `auto` twice.
    let dhw = cdt::SYSTEM_FUNCTION_ID;
    assert_eq!(pair.known.for_mode(dhw, auto()), [THEIR_DHW]);
    assert_eq!(pair.known.for_mode(dhw, on()), [THEIR_DHW]);
    assert_eq!(
        pair.known.for_mode(dhw, off()),
        [],
        "`off` relates to no setpoint on this circuit [CDT-003/3]"
    );
    assert!(
        pair.known.describes(dhw, off()),
        "and that is an answer, not an absence"
    );
    assert!(
        !pair.known.describes(
            eebus::usecases::hvac::system_function_id(&eebus::usecases::hvac::HEATING),
            auto()
        ),
        "this circuit heats no room, so it relates nothing to one"
    );
}

/// MDSF and CDT together: the mode says which setpoint a write would actually reach.
///
/// This is the reason [CDT-005] makes a system-function use case mandatory. A setpoint the
/// circuit is not currently reading can be written, acknowledged, and change nothing — and
/// nothing on the wire says so.
#[test]
fn the_current_mode_says_whether_a_write_will_do_anything() {
    let mut pair = Pair::new();
    pair.commission();

    assert_eq!(pair.state.system_function(), Some(mdsf::SYSTEM_FUNCTION_ID));
    assert!(
        pair.state.modes().is_sufficient(),
        "§2.3.1.1: two or more modes"
    );
    assert_eq!(pair.state.mode(), Some(&HvacOperationModeType::Auto));
    assert_eq!(
        pair.state.mode_changeable(),
        Some(true),
        "this circuit serves CDSF as well, and Table 6 fixes the element at `true` for one that does"
    );

    // In `auto` the circuit reads the hot water setpoint, so a write lands.
    assert_eq!(pair.state.current_setpoints(&pair.known), [THEIR_DHW]);

    // Switch it off: the same write would now reach nothing.
    set(
        &mut pair.circuit,
        &pair.hvac_feature.clone(),
        mdsf::system_function_state(off(), false, Some(false)),
    );
    let feature = pair.hvac_feature.clone();
    pair.circuit
        .notify(&feature, &Function::HvacSystemFunctionListData, pair.now);
    pair.settle();

    assert_eq!(pair.state.mode(), Some(&HvacOperationModeType::Off));
    assert_eq!(
        pair.state.current_setpoints(&pair.known),
        [],
        "in `off` this circuit relates the mode to no setpoint [CDT-003/3]"
    );
}

/// [F3] The gate: a write into a mode the circuit is not in is refused rather than sent.
///
/// The failure it exists to stop is the quiet one — the circuit accepts the write,
/// acknowledges it, and heats nothing, because Table 10 says that mode reads some other
/// setpoint. `write` still builds it, for a manager deliberately pre-loading the setpoint
/// of a mode it is about to ask for; `write_effective` is for the manager that expects hot
/// water now.
#[test]
fn a_write_into_a_mode_the_circuit_is_not_in_is_refused() {
    let mut pair = Pair::new();
    pair.commission();

    // In `auto` the circuit reads the hot water setpoint.
    assert_eq!(
        pair.known.effect_of(THEIR_DHW, &pair.state),
        SetpointEffect::Effective
    );
    assert!(
        pair.known
            .write_effective(THEIR_DHW, 60.0, &pair.state)
            .is_ok()
    );

    // The room air setpoint is a real setpoint of this circuit and is read by no DHW mode.
    assert_eq!(
        pair.known.effect_of(THEIR_ROOM, &pair.state),
        SetpointEffect::NotInCurrentMode,
        "writing it would heat a living room, and the circuit would say yes"
    );

    // Switch the circuit off, where it reads no setpoint at all [CDT-003/3].
    set(
        &mut pair.circuit,
        &pair.hvac_feature.clone(),
        mdsf::system_function_state(off(), false, Some(false)),
    );
    let feature = pair.hvac_feature.clone();
    pair.circuit
        .notify(&feature, &Function::HvacSystemFunctionListData, pair.now);
    pair.settle();

    assert_eq!(
        pair.known.effect_of(THEIR_DHW, &pair.state),
        SetpointEffect::NotInCurrentMode
    );
    assert_eq!(
        pair.known.write_effective(THEIR_DHW, 60.0, &pair.state),
        Err(WriteRefused::NotInCurrentMode),
        "the write never reaches the wire"
    );
    assert!(
        pair.known.write(THEIR_DHW, 60.0).is_ok(),
        "and the unconditional write is still there for a caller that means it"
    );

    // A one-time heating is not a refusal: the write lands where it was meant to.
    set(
        &mut pair.circuit,
        &pair.hvac_feature.clone(),
        mdsf::system_function_state(auto(), true, Some(false)),
    );
    pair.circuit
        .notify(&feature, &Function::HvacSystemFunctionListData, pair.now);
    pair.settle();
    assert_eq!(pair.state.overrun_active(), Some(true));
    assert_eq!(
        pair.known.effect_of(THEIR_DHW, &pair.state),
        SetpointEffect::OverriddenByOverrun
    );
    assert!(
        pair.known
            .write_effective(THEIR_DHW, 60.0, &pair.state)
            .is_ok(),
        "it takes effect when the overrun finishes, which is not the same as never"
    );
}

/// Before MDSF has spoken, the gate says so rather than guessing.
#[test]
fn the_gate_refuses_while_the_mode_is_unknown() {
    let known = cdt::reader();
    let state = mdsf::reader();
    assert_eq!(
        known.effect_of(THEIR_DHW, &state),
        SetpointEffect::Unknown,
        "nothing has been read yet"
    );
    assert_eq!(
        known.write_effective(THEIR_DHW, 60.0, &state),
        Err(WriteRefused::ModeUnknown),
        "and a manager writing a temperature into that is guessing"
    );
}

/// [MDSF-002]: a one-time heating overrides the mode, and `finished` is announced once.
///
/// A manager that sees the tank drawing power while the mode says `off` is not looking at
/// a fault — it is looking at somebody pressing the button in the bathroom.
#[test]
fn a_one_time_heating_is_reported_and_settles() {
    let mut pair = Pair::new();
    pair.commission();
    assert_eq!(pair.state.overrun(), Some(OverrunReport::Inactive));
    assert_eq!(pair.state.overrun_active(), Some(false));

    let feature = pair.hvac_feature.clone();
    for (status, overriding) in [
        (HvacOverrunStatus::Active, true),
        (HvacOverrunStatus::Running, true),
        (HvacOverrunStatus::Finished, false),
    ] {
        set(&mut pair.circuit, &feature, mdsf::overrun_state(status));
        set(
            &mut pair.circuit,
            &feature,
            mdsf::system_function_state(auto(), overriding, Some(false)),
        );
        for function in [
            Function::HvacOverrunListData,
            Function::HvacSystemFunctionListData,
        ] {
            pair.circuit.notify(&feature, &function, pair.now);
        }
        pair.settle();
    }

    assert_eq!(
        pair.state.overrun(),
        Some(OverrunReport::Inactive),
        "`finished` is not a state to rest in (Table 14)"
    );
    assert!(
        pair.state.overrun_just_finished(),
        "but the appliance was told a heating had just completed"
    );
    assert_eq!(pair.state.overrun_active(), Some(false));
}

/// The trio closes the loop: ask for a temperature, and find out whether the water got it.
///
/// A setpoint is a request. What the tank is at depends on the mode, on the circuit's own
/// step size, and on whether somebody has just had a shower — none of which is visible from
/// the setpoint, and all of which decides whether an energy manager's plan happened.
#[test]
fn the_measured_temperature_is_not_the_setpoint() {
    let mut pair = Pair::new();
    pair.commission();

    let tank = mdt::MEASURAND;
    assert_eq!(pair.readings.value(&tank), Some(49.0), "what the water is");
    assert_eq!(
        pair.known.temperature(THEIR_DHW),
        Some(52.0),
        "and what was asked of it — a different number, and both are true"
    );

    // Ask for more, and the setpoint moves at once.
    let write = pair.known.write(THEIR_DHW, 60.0).expect("inside the range");
    pair.manager.write(
        &pair.setpoint_feature.clone(),
        &pair.client.clone(),
        write,
        true,
        pair.now,
    );
    pair.settle();

    assert_eq!(pair.known.temperature(THEIR_DHW), Some(60.0));
    assert_eq!(
        pair.readings.value(&tank),
        Some(49.0),
        "the water has not moved yet, which is the whole point of measuring it"
    );

    // The tank heats.
    let feature = pair.measurement_feature.clone();
    set(&mut pair.circuit, &feature, mdt::temperature(59.8));
    pair.circuit
        .notify(&feature, &Function::MeasurementListData, pair.now);
    pair.settle();
    assert_eq!(pair.readings.value(&tank), Some(59.8));
}

/// [MDT-005]: a reading the circuit flagged is not handed back as a number.
#[test]
fn a_flagged_reading_is_not_acted_on() {
    use eebus::model::{MeasurementValueSource, MeasurementValueState};
    use eebus::usecases::monitoring::ReadingState;

    let mut pair = Pair::new();
    pair.commission();

    let feature = pair.measurement_feature.clone();
    set(
        &mut pair.circuit,
        &feature,
        mdt::temperature_from(
            120.0,
            MeasurementValueSource::MeasuredValue,
            Some(MeasurementValueState::OutOfRange),
        ),
    );
    pair.circuit
        .notify(&feature, &Function::MeasurementListData, pair.now);
    pair.settle();

    let reading = pair.readings.get(&mdt::MEASURAND).expect("a reading");
    assert_eq!(reading.state, ReadingState::OutOfRange);
    assert_eq!(
        pair.readings.value(&mdt::MEASURAND),
        None,
        "§2.5.1 says an out-of-range value SHALL be ignored"
    );
}

/// §3.2.1.2.2.1: the setpoint's `measurementId` is the one MDT publishes, not a new number.
#[test]
fn the_setpoint_points_at_the_measurement_that_governs_it() {
    use eebus::model::CmdData;

    let CmdData::SetpointDescriptionListData(list) =
        cdt::setpoint_description_measuring(UnitOfMeasurement::DegC, mdt::MEASUREMENT_ID)
    else {
        panic!("expected the descriptions");
    };
    assert_eq!(
        list.setpoint_description_data.as_ref().unwrap()[0].measurement_id,
        Some(mdt::MEASUREMENT_ID),
        "a FOREIGN IDENTIFIER that points somewhere"
    );
}

/// [B4] `mdt::locate` finds the tank, and the monitoring actor reads it.
///
/// A DHW circuit has no `ElectricalConnection`, so `monitoring::locate` cannot describe
/// it: searching for a feature the use case does not define finds whatever else the
/// device happens to serve, or nothing, and either answer is wrong. This is the use
/// case's own lookup, and what it returns goes into the same
/// `MonitoringApplianceActor` a grid connection point does — one actor, one client
/// feature, both kinds of peer.
#[test]
fn the_tank_is_located_by_its_own_use_case_and_read_by_the_monitoring_actor() {
    use eebus::usecases::monitoring::{MonitoringApplianceActor, MonitoringEvent};

    let mut pair = Pair::new();
    // Discovery only: this test does the reading through the actor rather than by hand.
    let ours = node_management(pair.manager.device().address());
    let theirs = node_management(pair.circuit.device().address());
    for function in [
        Function::NodeManagementDetailedDiscoveryData,
        Function::NodeManagementUseCaseData,
    ] {
        pair.manager.read(&theirs, &ours, function, pair.now);
    }
    pair.settle();

    let device = pair.circuit.device().address().clone();
    let remote = pair.manager.peer(&device).expect("the circuit");
    let tank = mdt::locate(remote).expect("a DHW circuit that announced MDT");
    assert_eq!(
        tank.measurement.as_ref(),
        Some(&pair.measurement_feature),
        "the one feature Table 6 gives this use case"
    );
    assert!(
        tank.electrical_connection.is_none() && tank.curtailment.is_none(),
        "a tank has neither, and locating it must not invent them"
    );

    let mut appliance = MonitoringApplianceActor::new(pair.client.clone());
    appliance.attach(&mut pair.manager, tank, pair.now);

    // The actor's own settling: every engine event goes to it rather than to the
    // hand-written readers `Pair::settle` feeds.
    fn pump(
        pair: &mut Pair,
        appliance: &mut MonitoringApplianceActor,
        seen: &mut Vec<MonitoringEvent>,
    ) {
        for _ in 0..64 {
            let mut moved = false;
            while let Some(datagram) = pair.manager.poll_transmit() {
                moved = true;
                pair.circuit
                    .handle_datagram(&round_trip(&datagram), pair.now);
            }
            while let Some(datagram) = pair.circuit.poll_transmit() {
                moved = true;
                pair.manager
                    .handle_datagram(&round_trip(&datagram), pair.now);
            }
            while let Some(event) = pair.manager.poll_event() {
                moved = true;
                seen.extend(appliance.handle_event(&event));
            }
            while pair.circuit.poll_event().is_some() {
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    let mut seen = Vec::new();
    pump(&mut pair, &mut appliance, &mut seen);

    let unit = appliance.units().next().expect("the tank").id();
    assert_eq!(unit.device, device, "the unit is on the circuit's device");
    assert_eq!(
        appliance
            .readings(&unit)
            .and_then(|r| r.value(&mdt::MEASURAND)),
        Some(49.0),
        "the actor read the descriptions and the value with no electrical connection"
    );
    assert!(
        seen.iter()
            .any(|event| matches!(event, MonitoringEvent::UnitDescribed { .. })),
        "the tank counts as described from the measurement descriptions alone: \
         `dhwTemperature` has no phases to wait for"
    );

    // And it keeps reading: the subscription it asked for carries the next value.
    let feature = pair.measurement_feature.clone();
    set(&mut pair.circuit, &feature, mdt::temperature(58.5));
    pair.circuit
        .notify(&feature, &Function::MeasurementListData, pair.now);

    seen.clear();
    pump(&mut pair, &mut appliance, &mut seen);
    let measured = seen.iter().find_map(|event| match event {
        MonitoringEvent::Measured { readings, .. } => readings.first().and_then(|r| r.usable()),
        _ => None,
    });
    assert_eq!(measured, Some(58.5), "the subscription delivered it");
}

/// [CDSF-002] and [CDSF-003]: the manager starts and stops the one-time hot water loading.
///
/// The shortest path there is from "the roof is exporting" to "the tank is absorbing it".
/// Unlike a setpoint, which the circuit's own controller may decline to act on, and unlike
/// `ohpcf`'s process, which the compressor has to have announced first, this is the button
/// in the bathroom pressed over the wire — and scenario 3 is how the manager gives it back
/// when a cloud arrives.
///
/// And no binding anywhere: §3.4.2.1 says "Binding SHOULD NOT be used for this Scenario",
/// so `Pair::commission` never asked for one.
#[test]
fn cdsf_002_and_003_a_one_time_loading_is_started_and_stopped_over_the_wire() {
    let mut pair = Pair::new();
    pair.commission();
    assert_eq!(pair.state.overrun(), Some(OverrunReport::Inactive));

    let start = pair
        .state
        .start_overrun()
        .expect("the circuit described one");
    let feature = pair.hvac_feature.clone();
    pair.manager
        .write(&feature, &pair.client.clone(), start, true, pair.now);
    pair.settle();

    assert_eq!(
        pair.requested,
        [Ok(Request::StartOverrun(mdsf::OVERRUN_ID))],
        "the circuit read the write as the request it was"
    );

    // The circuit gets on with it and says so; the manager's subscription carries it.
    set(
        &mut pair.circuit,
        &feature,
        cdsf::overrun_state(HvacOverrunStatus::Running),
    );
    pair.circuit
        .notify(&feature, &Function::HvacOverrunListData, pair.now);
    pair.settle();
    assert_eq!(pair.state.overrun(), Some(OverrunReport::Running));

    // A cloud arrives.
    pair.requested.clear();
    let stop = pair.state.stop_overrun().expect("the same one");
    pair.manager
        .write(&feature, &pair.client.clone(), stop, true, pair.now);
    pair.settle();
    assert_eq!(pair.requested, [Ok(Request::StopOverrun(mdsf::OVERRUN_ID))]);
}

/// [CDSF-001]: the manager sets the operation mode, by name, and a mode the circuit does
/// not have is refused before anything is sent.
#[test]
fn cdsf_001_the_operation_mode_is_set_by_name() {
    let mut pair = Pair::new();
    pair.commission();

    // This circuit has `auto`, `on` and `off` — and not `eco`.
    assert_eq!(
        pair.state.set_mode_named(&HvacOperationModeType::Eco),
        Err(ModeRefused::NotRelated),
        "a mode the circuit never described is not a number to guess at"
    );

    let write = pair
        .state
        .set_mode_named(&HvacOperationModeType::On)
        .expect("`on` is one of them");
    let feature = pair.hvac_feature.clone();
    pair.manager
        .write(&feature, &pair.client.clone(), write, true, pair.now);
    pair.settle();

    assert_eq!(pair.requested, [Ok(Request::SetMode(on()))]);
    assert!(
        pair.applied.is_empty(),
        "a mode write is not a setpoint write, and the circuit did not confuse the two"
    );
}

/// A CDSF write the circuit would refuse is refused *before* it is acknowledged.
///
/// The same shape as LPC's Controllable System declining a limit: the decision has to come
/// before the answer, because the answer is what the other end acts on.
#[test]
fn cdsf_a_mode_the_circuit_does_not_have_is_rejected_on_the_wire() {
    use eebus::usecases::hvac::system_function;

    let mut pair = Pair::new();
    pair.commission();

    // A peer that guessed at the numbering: `eco` is `<om1#4>` by this crate's reckoning,
    // and this circuit relates only three modes to its hot water.
    let bogus = system_function::set_operation_mode(
        mdsf::SYSTEM_FUNCTION_ID,
        mdsf::operation_mode_id(&HvacOperationModeType::Eco).unwrap(),
    );
    let feature = pair.hvac_feature.clone();
    let counter = pair
        .manager
        .write(&feature, &pair.client.clone(), bogus, true, pair.now);
    pair.settle();

    assert_eq!(pair.requested, [Err(ModeRefused::NotRelated)]);
    let answered = pair
        .answers
        .iter()
        .find(|(request, _)| *request == counter)
        .map(|(_, error)| *error);
    assert_eq!(
        answered,
        Some(eebus::spine::ErrorNumber::DestinationUnknown),
        "the refusal names what was asked for and is not there"
    );
    assert_eq!(
        pair.state.mode(),
        Some(&HvacOperationModeType::Auto),
        "and the circuit stayed where it was"
    );
}
