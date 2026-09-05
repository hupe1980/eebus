//! Configuration of DHW Temperature (CDT).
//!
//! A *Configuration Appliance* — a CEM, usually — sets the domestic hot water temperature
//! setpoint of a *DHW Circuit*. It answers a question the limitation family cannot:
//! [`limitation`] tells a heat pump how much power it **may not exceed**, and this tells it
//! what to **aim for**.
//!
//! Those are different levers and they are not interchangeable. A ceiling can only ever
//! make a heat pump do less; a setpoint can make it do more, which is what turns a hot
//! water tank into the cheapest thermal battery in the building. Raising the DHW setpoint
//! by ten degrees while the roof is exporting stores a few kilowatt-hours that would
//! otherwise have been sold at the feed-in tariff and bought back at the retail one — and
//! no limit, however carefully written, can ask for that.
//!
//! One scenario, and both actors must implement it (Table 1):
//!
//! 1. **Set DHW temperature setpoint** — the appliance writes a temperature, the circuit
//!    applies it within the range and step size it published.
//!
//! **This use case is not complete on its own.** §2.4.2 and §2.4.3: a DHW Circuit that
//! does not serve "Monitoring of DHW System Function" SHALL serve "Configuration of DHW
//! System Function" [CDT-005] — one of the two is mandatory. The reason is
//! [`system_function_relations`]: a setpoint is reached *through* an operation mode, so
//! "write 60 °C" is only a complete instruction once the mode the circuit is in is known,
//! and that is what [`mdsf`](super::mdsf) reports and [`cdsf`](super::cdsf) changes.
//! [`SystemFunction::current_setpoints`](super::system_function::SystemFunction::current_setpoints)
//! is the join. A write into a mode the circuit is not in is applied, acknowledged, and
//! changes nothing anybody can measure.
//!
//! # No binding
//!
//! §3.4.1.1 is explicit: "Binding SHOULD NOT be used for this Scenario". That is unlike
//! every grid use case in this crate, where a write without a binding is refused, and it is
//! why [`setpoint_feature`] is built with
//! [`with_unbound_writes`](crate::spine::LocalFeature::with_unbound_writes). A circuit that
//! insisted on a binding would refuse every conformant Configuration Appliance.
//!
//! # The identifiers are the circuit's
//!
//! `setpointId` is `<st1#(1..4)>` — a DHW Circuit publishes **one to four** temperature
//! setpoints and numbers them itself — and `systemFunctionId` is `<sf1#(1..1)>`. Which
//! setpoint a Configuration Appliance should write is not a number it can assume: it is
//! the one whose description says `scopeType: dhwTemperature`, and which of *those* applies
//! is decided by the operation mode the circuit is in, published in
//! `hvacSystemFunctionSetpointRelationListData`.
//! [`Setpoints`] is the reader that puts the three functions
//! together; see [`crate::usecases::addressing`] for why none of this may be shortcut.
//!
//! ```
//! use eebus::usecases::hvac::cdt;
//! use eebus::model::UnitOfMeasurement;
//!
//! // What the circuit publishes.
//! let mut known = cdt::reader();
//! known.learn(&cdt::setpoint_description(UnitOfMeasurement::DegC));
//! known.learn(&cdt::setpoint_constraints(40.0, 65.0, Some(0.5)));
//!
//! let id = known.temperature_setpoints().next().expect("a DHW setpoint");
//!
//! // 60 °C is inside the range and on the step size, so it can be written.
//! assert!(known.write(id, 60.0).is_ok());
//! // 70 °C is not, and is refused here rather than by the circuit.
//! assert!(known.write(id, 70.0).is_err());
//! ```
//!
//! [`limitation`]: crate::usecases::limitation

use alloc::vec::Vec;

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacSystemFunctionId, ScopeType, SetpointId, UnitOfMeasurement,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::setpoint::{self, Setpoints};
use super::{DHW, system_function_id};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "configurationOfDhwTemperature";

/// The actor that holds the hot water.
pub const DHW_CIRCUIT_ACTOR: &str = "DHWCircuit";

/// The actor that sets its temperature.
pub const CONFIGURATION_APPLIANCE_ACTOR: &str = "ConfigurationAppliance";

/// The `scopeType` that marks a setpoint as the DHW temperature (Table 7).
///
/// This is the fixed half — what a Configuration Appliance matches on to find the
/// setpoint it may write.
pub const TEMPERATURE_SCOPE: ScopeType = ScopeType::DhwTemperature;

/// The `systemFunctionId` **this** implementation relates its setpoints to.
///
/// The same number [`mdsf`](super::mdsf) publishes the hot water under, which
/// §3.2.1.2.3.1's first footnote requires: "the systemFunctionId SHALL be the same used
/// within the Use Case Monitoring of DHW System Function for the system function dhw".
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = system_function_id(&DHW);

/// The `setpointId` **this** implementation publishes its DHW temperature under.
///
/// A local choice. Table 7 spells it `<st1#(1..4)>`: a circuit publishes one to four
/// temperature setpoints and numbers them itself, and which one is in force depends on the
/// operation mode. Find a peer's with [`reader`], never by assuming this.
pub const SETPOINT_ID: SetpointId = SetpointId(1);

/// The units a DHW temperature may be published in (Table 7).
pub const UNITS: [UnitOfMeasurement; 3] = setpoint::UNITS;

/// How many temperature setpoints an operation mode may relate to.
///
/// §2.3.1.1 gives three different rules — see
/// [`setpoint::permitted_setpoints`].
pub fn permitted_setpoints(mode: &HvacOperationModeType) -> core::ops::RangeInclusive<usize> {
    setpoint::permitted_setpoints(mode)
}

/// Whether a relation between an operation mode and its setpoints is well formed.
pub fn relation_is_valid(mode: &HvacOperationModeType, setpoints: &[SetpointId]) -> bool {
    setpoint::relation_is_valid(mode, setpoints)
}

// ---- the features a DHW Circuit serves ---------------------------------------------

/// Builds the `Setpoint` feature scenario 1 is served from (Table 6).
///
/// `setpointListData` is the only writeable function, writes are deferred to the
/// application, and **no binding is required** — §3.4.1.1. See
/// [`setpoint::setpoint_feature`].
pub fn setpoint_feature(address: u32) -> LocalFeature {
    setpoint::setpoint_feature(address)
}

/// Builds the `HVAC` feature that says which setpoint belongs to which operation mode.
///
/// A circuit that also serves [`mdsf`](super::mdsf) — which [CDT-005] all but requires —
/// puts both on one feature: [`mdsf::with_cdt`](super::mdsf::with_cdt).
pub fn hvac_feature(address: u32) -> LocalFeature {
    setpoint::hvac_feature(address)
}

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The setpoint's description (Table 7).
///
/// `measurementId` is deliberately absent. It is a FOREIGN IDENTIFIER pointing at the
/// measurement of the same temperature, and §3.2.1.2.2.1 requires it to be *the same
/// number* "Monitoring of DHW Temperature" uses — so a circuit that does not serve that
/// use case omits the element rather than inventing a link that leads nowhere. Use
/// [`setpoint_description_measuring`] where the two are served together.
pub fn setpoint_description(unit: UnitOfMeasurement) -> CmdData {
    setpoint::description(SETPOINT_ID, TEMPERATURE_SCOPE, unit, None)
}

/// The same, for a circuit that also serves [`mdt`](super::mdt).
///
/// `measurement` must be the `measurementId` that use case publishes the DHW temperature
/// under — §3.2.1.2.2.1 says the two SHALL be identical — which is what lets a
/// Configuration Appliance tie the reading to the setpoint that governs it, and so read
/// back the effect of what it wrote rather than only the number it asked for.
/// [`mdt::MEASUREMENT_ID`](super::mdt::MEASUREMENT_ID) is this crate's own.
///
/// ```
/// use eebus::model::UnitOfMeasurement;
/// use eebus::usecases::hvac::{cdt, mdt};
///
/// // A circuit serving both: the setpoint points at the measurement.
/// let described = cdt::setpoint_description_measuring(
///     UnitOfMeasurement::DegC,
///     mdt::MEASUREMENT_ID,
/// );
/// ```
pub fn setpoint_description_measuring(
    unit: UnitOfMeasurement,
    measurement: crate::model::MeasurementId,
) -> CmdData {
    setpoint::description(SETPOINT_ID, TEMPERATURE_SCOPE, unit, Some(measurement))
}

/// What the circuit will accept (Table 8).
pub fn setpoint_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    setpoint::constraints(SETPOINT_ID, min, max, step)
}

/// The current setpoint (Table 9), in the unit the description named.
pub fn setpoint_value(degrees: f64) -> CmdData {
    setpoint::value(SETPOINT_ID, degrees)
}

/// Which setpoints each operation mode uses (Table 10).
///
/// The relations are refused rather than published when one breaks §2.3.1.1 — see
/// [`permitted_setpoints`]. A circuit that told a Configuration Appliance that `on` maps
/// to two setpoints has said nothing usable, and the appliance would have to guess which
/// of them its write will take effect on.
pub fn system_function_relations(
    relations: &[(HvacOperationModeId, HvacOperationModeType, Vec<SetpointId>)],
) -> Option<CmdData> {
    setpoint::relations(SYSTEM_FUNCTION_ID, relations)
}

/// Reads a `setpointListData` write as a temperature.
///
/// `id` is the identifier on the device the payload belongs to — this circuit's own
/// [`SETPOINT_ID`] when it reads a write addressed to it.
///
/// **Give this the resolved state, not a partial update**: an omitted element means
/// *unchanged* (SPINE IG §3.3), and a fragment read as a whole value is a temperature
/// nobody asked for.
pub fn read_setpoint_write(data: &CmdData, id: SetpointId) -> Option<f64> {
    setpoint::read_write(data, id)
}

/// A reader collecting this circuit's hot water setpoints.
pub fn reader() -> Setpoints {
    Setpoints::dhw()
}

// ---- descriptors ---------------------------------------------------------------------

/// A DHW Circuit lives on its own entity type (§3.2.1.1).
const DHW_CIRCUIT_ENTITIES: &[EntityType] = &[EntityType::DHWCircuit];

/// The Configuration Appliance may sit behind any entity (Figure 5, `entityType = <any>`).
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

const SCENARIO_NAME: &str = "Set DHW temperature setpoint";

/// The DHW Circuit: the actor whose temperature is set.
pub static DHW_CIRCUIT: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: DHW_CIRCUIT_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: DHW_CIRCUIT_ENTITIES,
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
    counterpart: DHW_CIRCUIT_ACTOR,
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
    use crate::usecases::hvac::mdsf;
    use crate::usecases::hvac::setpoint::{SetpointEffect, WriteRefused};
    use alloc::vec;

    fn a_circuit() -> Setpoints {
        let mut known = reader();
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));
        known.learn(&setpoint_constraints(40.0, 65.0, Some(0.5)));
        known
    }

    /// Table 7: the elements a Configuration Appliance matches on.
    #[test]
    fn the_description_says_hot_water_and_an_absolute_value() {
        let CmdData::SetpointDescriptionListData(list) =
            setpoint_description(UnitOfMeasurement::DegC)
        else {
            panic!("expected the descriptions");
        };
        let entry = &list.setpoint_description_data.as_ref().unwrap()[0];
        assert_eq!(entry.scope_type.as_ref(), Some(&TEMPERATURE_SCOPE));
        assert_eq!(
            entry.setpoint_type.as_ref(),
            Some(&crate::model::SetpointType::ValueAbsolute)
        );
        assert!(
            entry.measurement_id.is_none(),
            "a FOREIGN IDENTIFIER is omitted rather than invented"
        );
    }

    /// A temperature outside the published range never reaches the wire.
    #[test]
    fn cdt_001_a_value_the_circuit_would_refuse_is_refused_here() {
        let known = a_circuit();
        let id = known.temperature_setpoints().next().unwrap();
        assert!(known.write(id, 60.0).is_ok());
        assert!(matches!(
            known.write(id, 70.0),
            Err(WriteRefused::OutOfRange { .. })
        ));
        assert!(matches!(
            known.write(SetpointId(9), 60.0),
            Err(WriteRefused::UnknownSetpoint)
        ));
    }

    /// A room air setpoint on the same feature is not a hot water setpoint.
    #[test]
    fn only_the_dhw_scope_is_collected() {
        let mut known = reader();
        known.learn(&setpoint::description(
            SetpointId(7),
            ScopeType::RoomAirTemperature,
            UnitOfMeasurement::DegC,
            None,
        ));
        assert_eq!(
            known.temperature_setpoints().count(),
            0,
            "writing 60 to a room air setpoint heats a living room"
        );
    }

    /// The gate: a write into a mode the circuit is not in changes nothing.
    #[test]
    fn cdt_002_a_write_into_the_wrong_mode_is_refused() {
        let auto = mdsf::operation_mode_id(&HvacOperationModeType::Auto).unwrap();
        let off = mdsf::operation_mode_id(&HvacOperationModeType::Off).unwrap();

        let mut known = a_circuit();
        known.learn(
            &system_function_relations(&[
                (auto, HvacOperationModeType::Auto, vec![SETPOINT_ID]),
                (off, HvacOperationModeType::Off, vec![]),
            ])
            .expect("well formed"),
        );

        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];
        let mut state = mdsf::reader();
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::Unknown,
            "nothing has been reported yet"
        );

        state.learn(&mdsf::system_function_description());
        state.learn(&mdsf::operation_mode_descriptions(&modes).unwrap());
        state.learn(&mdsf::operation_mode_relations(&modes).unwrap());
        state.learn(&mdsf::system_function_state(off, false, None));
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::NotInCurrentMode
        );
        assert!(matches!(
            known.write_effective(SETPOINT_ID, 60.0, &state),
            Err(WriteRefused::NotInCurrentMode)
        ));

        state.learn(&mdsf::system_function_state(auto, false, None));
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::Effective
        );
        assert!(known.write_effective(SETPOINT_ID, 60.0, &state).is_ok());

        state.learn(&mdsf::system_function_state(auto, true, None));
        assert_eq!(
            known.effect_of(SETPOINT_ID, &state),
            SetpointEffect::OverriddenByOverrun,
            "the write lands; the one-time heating is over the top of it"
        );
        assert!(
            known.write_effective(SETPOINT_ID, 60.0, &state).is_ok(),
            "an overrun is `later`, not `never`"
        );
    }

    /// §2.3.1.1: `on` relates to exactly one setpoint, and a relation that breaks the rule
    /// is not published.
    #[test]
    fn cdt_003_the_relation_cardinalities_are_enforced() {
        let on = mdsf::operation_mode_id(&HvacOperationModeType::On).unwrap();
        assert!(
            system_function_relations(&[(
                on,
                HvacOperationModeType::On,
                vec![SetpointId(1), SetpointId(2)]
            )])
            .is_none()
        );
        assert!(
            system_function_relations(&[(on, HvacOperationModeType::On, vec![SetpointId(1)])])
                .is_some()
        );
        assert_eq!(permitted_setpoints(&HvacOperationModeType::Auto), 1..=4);
        assert_eq!(permitted_setpoints(&HvacOperationModeType::Off), 0..=1);
    }

    /// §3.4.1.1: "Binding SHOULD NOT be used for this Scenario", on both sides.
    #[test]
    fn cdt_writes_need_no_binding() {
        assert_eq!(
            setpoint_feature(1).write_binding(),
            WriteBinding::NotRequired,
            "a circuit that required one would refuse every conformant appliance"
        );
        assert_eq!(
            CONFIGURATION_APPLIANCE.features_needing_binding().count(),
            0,
            "and the appliance is told not to ask for one"
        );
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&DHW_CIRCUIT, &CONFIGURATION_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert!(DHW_CIRCUIT.permits_entity(&EntityType::DHWCircuit));
        assert!(CONFIGURATION_APPLIANCE.permits_entity(&EntityType::CEM));
    }
}
