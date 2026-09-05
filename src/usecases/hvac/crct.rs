//! Configuration of Room Cooling Temperature (CRCT).
//!
//! A *Configuration Appliance* sets the cooling temperature setpoint of an *HVAC Room* —
//! "a logical or physical indoor space of a house or premises". It is
//! [`crht`](super::crht) pointed at the other system function.
//!
//! One scenario, mandatory for both actors (Table 1):
//!
//! 1. **Set room cooling temperature setpoint** — the appliance writes a temperature, the
//!    room applies it within the range and step size it published [CRCT-001].
//!
//! # Heating and cooling share a scope
//!
//! **This is the thing to understand about the room temperature use cases.** A room's
//! heating setpoint and its cooling setpoint are both `scopeType: roomAirTemperature`, in
//! the same unit, on the same `Setpoint` feature, and their descriptions are identical.
//! Nothing in `setpointDescriptionListData` tells them apart.
//!
//! What tells them apart is the **relation**: `hvacSystemFunctionSetpointRelationData` is
//! keyed by `systemFunctionId` (PRIMARY) and `operationModeId` (SUB), and the system
//! function is `cooling` for this use case and `heating` for
//! [`crht`](super::crht). So which setpoint to write is only answerable once
//! [`mrcsf`](super::mrcsf) has said which mode the cooling function is in —
//! [`Setpoints::for_mode`](super::setpoint::Setpoints::for_mode) takes both identifiers for
//! that reason, and a reader keyed by the mode alone would cool a room it was asked to warm.
//!
//! [CRCT-005] says the same thing: an actor here SHOULD also support "Configuration of Room
//! Cooling System Function", and a Configuration Appliance that does not SHOULD support the
//! monitoring one.
//!
//! # And whether it worked
//!
//! [CRCT-004] ties the setpoint to the measurement: its `measurementId` SHALL be the one
//! [`mrt`](super::mrt) publishes the room temperature under, so an appliance can compare
//! what it asked for against what the room reached. [`setpoint_description_measuring`] is
//! that link.
//!
//! # No binding
//!
//! §3.4.1.1: "Binding SHOULD NOT be used for this Scenario". [`setpoint_feature`] is built
//! accordingly, and its writes are deferred to the application instead.
//!
//! ```
//! use eebus::model::UnitOfMeasurement;
//! use eebus::usecases::hvac::crct;
//!
//! let mut known = crct::reader();
//! known.learn(&crct::setpoint_description(UnitOfMeasurement::DegC));
//! known.learn(&crct::setpoint_constraints(18.0, 30.0, Some(0.5)));
//!
//! let id = known.temperature_setpoints().next().expect("a room air setpoint");
//! assert!(known.write(id, 24.0).is_ok());
//! assert!(known.write(id, 10.0).is_err());
//! ```

use alloc::vec::Vec;

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacSystemFunctionId, ScopeType, SetpointId, UnitOfMeasurement,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::setpoint::{self, Setpoints};
use super::{COOLING, system_function_id};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "configurationOfRoomCoolingTemperature";

/// The actor that *is* the room.
pub const HVAC_ROOM_ACTOR: &str = "HVACRoom";

/// The actor that sets its temperature.
pub const CONFIGURATION_APPLIANCE_ACTOR: &str = "ConfigurationAppliance";

/// The `scopeType` that marks a setpoint as a room air temperature (Table 7).
///
/// **Shared with [`crht`](super::crht)**, which is why it is not enough on its
/// own — see the module documentation.
pub const TEMPERATURE_SCOPE: ScopeType = ScopeType::RoomAirTemperature;

/// The `systemFunctionId` **this** implementation relates its setpoints to.
///
/// The same number [`mrcsf`](super::mrcsf) publishes the room cooling under, which
/// [CRCT-005] requires, and *not* the one [`crht`](super::crht) uses.
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = system_function_id(&COOLING);

/// The `setpointId` **this** implementation publishes its room temperature under.
///
/// A local choice, `<st1#(1..4)>`. A room that both heats and cools publishes at least two
/// and numbers them itself; find a peer's with [`reader`] and the relations, never by
/// assuming this.
pub const SETPOINT_ID: SetpointId = SetpointId(2);

/// The units a room air temperature may be published in (Table 7).
pub const UNITS: [UnitOfMeasurement; 3] = setpoint::UNITS;

/// How many temperature setpoints an operation mode may relate to.
pub fn permitted_setpoints(mode: &HvacOperationModeType) -> core::ops::RangeInclusive<usize> {
    setpoint::permitted_setpoints(mode)
}

/// Whether a relation between an operation mode and its setpoints is well formed.
pub fn relation_is_valid(mode: &HvacOperationModeType, setpoints: &[SetpointId]) -> bool {
    setpoint::relation_is_valid(mode, setpoints)
}

// ---- the features an HVAC Room serves -------------------------------------------------

/// Builds the `Setpoint` feature the scenario is served from (Table 6).
///
/// Deferred writes, and **no binding** — §3.4.1.1.
pub fn setpoint_feature(address: u32) -> LocalFeature {
    setpoint::setpoint_feature(address)
}

/// Builds the `HVAC` feature that says which setpoint belongs to which operation mode.
///
/// A room that also serves [`mrcsf`](super::mrcsf) puts both on one feature:
/// [`mrcsf::with_setpoints`](super::mrcsf::with_setpoints).
pub fn hvac_feature(address: u32) -> LocalFeature {
    setpoint::hvac_feature(address)
}

// ---- what an HVAC Room publishes ------------------------------------------------------

/// The setpoint's description (Table 7), without the measurement link.
pub fn setpoint_description(unit: UnitOfMeasurement) -> CmdData {
    setpoint::description(SETPOINT_ID, TEMPERATURE_SCOPE, unit, None)
}

/// The same, for a room that also serves [`mrt`](super::mrt).
///
/// [CRCT-004]: the `measurementId` SHALL be the one that use case publishes the room
/// temperature under. [`mrt::MEASUREMENT_ID`](super::mrt::MEASUREMENT_ID) is this crate's.
pub fn setpoint_description_measuring(
    unit: UnitOfMeasurement,
    measurement: crate::model::MeasurementId,
) -> CmdData {
    setpoint::description(SETPOINT_ID, TEMPERATURE_SCOPE, unit, Some(measurement))
}

/// What the room will accept (Table 8).
pub fn setpoint_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    setpoint::constraints(SETPOINT_ID, min, max, step)
}

/// The current setpoint (Table 9), in the unit the description named.
pub fn setpoint_value(degrees: f64) -> CmdData {
    setpoint::value(SETPOINT_ID, degrees)
}

/// Which setpoints each operation mode of the cooling function uses (Table 10).
///
/// Published under [`SYSTEM_FUNCTION_ID`], which is what keeps these relations apart from
/// [`crht`](super::crht)'s on the same feature.
pub fn system_function_relations(
    relations: &[(HvacOperationModeId, HvacOperationModeType, Vec<SetpointId>)],
) -> Option<CmdData> {
    setpoint::relations(SYSTEM_FUNCTION_ID, relations)
}

/// Reads a `setpointListData` write as a temperature.
pub fn read_setpoint_write(data: &CmdData, id: SetpointId) -> Option<f64> {
    setpoint::read_write(data, id)
}

/// A reader collecting a room's air temperature setpoints.
///
/// One reader serves both room temperature use cases — they share the scope — and the
/// system function passed to
/// [`for_mode`](super::setpoint::Setpoints::for_mode) is what says which of the setpoints
/// it collected is the cooling one.
pub fn reader() -> Setpoints {
    Setpoints::room_air()
}

// ---- descriptors ---------------------------------------------------------------------

const HVAC_ROOM_ENTITIES: &[EntityType] = &[EntityType::HVACRoom];
/// The Configuration Appliance may sit behind any entity.
const CONFIGURATION_APPLIANCE_ENTITIES: &[EntityType] = &[];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::server_writeable_unbound(FeatureType::Setpoint, Function::SetpointListData),
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionSetpointRelationListData,
    ),
];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::client_writes_unbound(FeatureType::Setpoint, Function::SetpointListData),
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionSetpointRelationListData,
    ),
];

const SCENARIO_NAME: &str = "Set room cooling temperature setpoint";

/// The HVAC Room: the actor whose temperature is set.
pub static HVAC_ROOM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: HVAC_ROOM_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: HVAC_ROOM_ENTITIES,
    counterpart: CONFIGURATION_APPLIANCE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: SERVER_FUNCTIONS,
    }],
};

/// The Configuration Appliance: the actor setting it.
pub static CONFIGURATION_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CONFIGURATION_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CONFIGURATION_APPLIANCE_ENTITIES,
    counterpart: HVAC_ROOM_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: CLIENT_FUNCTIONS,
    }],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::WriteBinding;
    use crate::usecases::hvac::mrcsf;
    use crate::usecases::hvac::setpoint::{SetpointEffect, WriteRefused};
    use alloc::vec;

    /// Table 7: a room air temperature, absolute, and pointed at the measurement.
    #[test]
    fn the_description_says_room_air() {
        let CmdData::SetpointDescriptionListData(list) = setpoint_description_measuring(
            UnitOfMeasurement::DegC,
            crate::usecases::hvac::mrt::MEASUREMENT_ID,
        ) else {
            panic!("expected the descriptions");
        };
        let entry = &list.setpoint_description_data.as_ref().unwrap()[0];
        assert_eq!(entry.scope_type.as_ref(), Some(&TEMPERATURE_SCOPE));
        assert_eq!(
            entry.measurement_id,
            Some(crate::usecases::hvac::mrt::MEASUREMENT_ID),
            "[CRCT-004]: the setpoint points at the measurement that reports it"
        );
    }

    /// A temperature outside the published range never reaches the wire.
    #[test]
    fn a_value_the_room_would_refuse_is_refused_here() {
        let mut known = reader();
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));
        known.learn(&setpoint_constraints(18.0, 30.0, Some(0.5)));
        let id = known.temperature_setpoints().next().unwrap();
        assert!(known.write(id, 24.0).is_ok());
        assert!(matches!(
            known.write(id, 10.0),
            Err(WriteRefused::OutOfRange { .. })
        ));
    }

    /// The gate: a write into a mode the room's cooling is not in changes nothing.
    #[test]
    fn a_write_into_the_wrong_mode_is_refused() {
        let auto = mrcsf::operation_mode_id(&HvacOperationModeType::Auto).unwrap();
        let off = mrcsf::operation_mode_id(&HvacOperationModeType::Off).unwrap();

        let mut known = reader();
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));
        known.learn(&setpoint_constraints(18.0, 30.0, Some(0.5)));
        known.learn(
            &system_function_relations(&[
                (auto, HvacOperationModeType::Auto, vec![SETPOINT_ID]),
                (off, HvacOperationModeType::Off, vec![]),
            ])
            .expect("well formed"),
        );

        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];
        let mut state = mrcsf::reader();
        state.learn(&mrcsf::system_function_description());
        state.learn(&mrcsf::operation_mode_descriptions(&modes).unwrap());
        state.learn(&mrcsf::operation_mode_relations(&modes).unwrap());

        state.learn(&mrcsf::system_function_state(off, false, None));
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::NotInCurrentMode
        );
        assert!(matches!(
            known.write_effective(SETPOINT_ID, 24.0, &state),
            Err(WriteRefused::NotInCurrentMode)
        ));

        state.learn(&mrcsf::system_function_state(auto, false, None));
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::Effective
        );
        assert!(known.write_effective(SETPOINT_ID, 24.0, &state).is_ok());
    }

    /// §3.4.1.1: "Binding SHOULD NOT be used for this Scenario", on both sides.
    #[test]
    fn writes_here_need_no_binding() {
        assert_eq!(
            setpoint_feature(1).write_binding(),
            WriteBinding::NotRequired
        );
        assert_eq!(
            CONFIGURATION_APPLIANCE.features_needing_binding().count(),
            0
        );
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&HVAC_ROOM, &CONFIGURATION_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert!(HVAC_ROOM.permits_entity(&EntityType::HVACRoom));
        assert!(CONFIGURATION_APPLIANCE.permits_entity(&EntityType::CEM));
    }
}
