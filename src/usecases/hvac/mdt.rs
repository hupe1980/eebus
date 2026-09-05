//! Monitoring of DHW Temperature (MDT).
//!
//! A *Monitoring Appliance* reads the temperature of the hot water. It is the third of the
//! DHW trio and the one that closes the loop: [`cdt`](super::cdt) asks for a temperature,
//! [`mdsf`](super::mdsf) says which mode the circuit is in, and this says what the water
//! actually got to.
//!
//! One scenario, mandatory for both actors, and two rules worth naming:
//!
//! * **[MDT-002]: only the latest value.** The `timestamp` element is permitted but
//!   "additional historical values are forbidden" — this is a thermometer, not a log.
//! * **[MDT-005]: the value state decides.** `outOfRange` and `error` **SHALL be ignored**
//!   by the appliance, which is the same rule MPC states as [MPC-003] and which
//!   [`Readings`](crate::usecases::monitoring::Readings) already enforces: a flagged value
//!   is never handed back as a number.
//!
//! # Why it is worth having
//!
//! Comparing what the tank reached against what was asked for is the only way to find out
//! whether a setpoint write did anything. A write into an operation mode the circuit is not
//! in is applied and acknowledged and changes nothing (see [`mdsf`](super::mdsf)); a write
//! the circuit rounded to its step size lands somewhere else; and a tank that a shower has
//! just emptied is at 38 °C whatever the setpoint says. None of that is visible from the
//! setpoint alone.
//!
//! It is also what makes the `measurementId` link real. CDT Table 7 marks that element a
//! FOREIGN IDENTIFIER and §3.2.1.2.2.1 requires it to be **the same number this use case
//! publishes** — so a circuit serving both uses [`MEASUREMENT_ID`] in
//! [`cdt::setpoint_description_measuring`](super::cdt::setpoint_description_measuring),
//! and an appliance can tie the reading to the setpoint that governs it.
//!
//! # Reading it
//!
//! There is no reader of its own. The measurement layer already resolves a
//! `measurementListData` against the descriptions that give it meaning, so an appliance
//! that runs MPC or MGCP reads this with the machinery it already has:
//!
//! ```
//! use eebus::usecases::hvac::mdt;
//! use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
//!
//! let mut readings = Readings::new();
//! readings.describe(&mdt::temperature_description());
//! readings.apply(&mdt::temperature(58.5));
//!
//! let tank = Measurand::unphased(Quantity::DhwTemperature);
//! assert_eq!(readings.value(&tank), Some(58.5));
//! ```

use crate::model::{
    AbsoluteOrRelativeTime, CmdData, EntityType, FeatureType, Function, MeasurementId,
    MeasurementValueSource, MeasurementValueState, UnitOfMeasurement,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
use crate::usecases::hvac::temperature;
use crate::usecases::monitoring::{Measurand, Quantity};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "monitoringOfDhwTemperature";

/// The actor that holds the hot water.
pub const DHW_CIRCUIT_ACTOR: &str = "DHWCircuit";

/// The actor that reads its temperature.
pub const MONITORING_APPLIANCE_ACTOR: &str = "MonitoringAppliance";

/// The `measurementId` **this** implementation publishes the hot water temperature under.
///
/// A local choice, `<m1#(1..1)>` — and the one identifier in this family that a *second*
/// use case is required to reuse: CDT §3.2.1.2.2.1 says its setpoint's `measurementId`
/// SHALL be this same number where both use cases are served. That is what
/// [`cdt::setpoint_description_measuring`](super::cdt::setpoint_description_measuring)
/// takes.
///
/// A peer's is found the way every measurement is: from its description, by
/// `scopeType: dhwTemperature`, which
/// [`Readings`](crate::usecases::monitoring::Readings) does.
pub const MEASUREMENT_ID: MeasurementId = MeasurementId(1);

/// What this use case measures, as the measurement layer names it.
pub const MEASURAND: Measurand = Measurand::unphased(Quantity::DhwTemperature);

// ---- the feature a DHW Circuit serves -----------------------------------------------

/// Builds the `Measurement` feature scenario 1 is served from (Table 6).
///
/// Read-only. There is no `ElectricalConnection` beside it, which is what makes this use
/// case unlike MPC and MGCP: a tank has no phases and no connection to describe, so the
/// measurement description stands alone.
pub fn measurement_feature(address: u32) -> LocalFeature {
    temperature::measurement_feature(address)
}

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The measurement's description (Table 7).
///
/// `commodityType: domesticHotWater` is `M`, and is one of the three places in this crate
/// where a measurement is not electricity — the other two being a room and the outdoors,
/// both `air` ([`mrt`](super::mrt), [`mot`](super::mot)). A client that filters on the
/// commodity — which is what the element is for — would not find a tank published as
/// electricity.
pub fn temperature_description() -> CmdData {
    temperature_description_in(UnitOfMeasurement::DegC)
}

/// The same, in the unit the circuit actually works in.
///
/// Table 7 permits `degC`, `degF` and `K`. A circuit that reports Fahrenheit and an
/// appliance that assumes Celsius disagree by forty degrees at the temperatures that
/// matter, and nothing on the wire objects.
pub fn temperature_description_in(unit: UnitOfMeasurement) -> CmdData {
    temperature::description(&MEASURAND, MEASUREMENT_ID, unit)
}

/// The range and granularity the circuit can report (Table 8).
///
/// Recommended rather than mandatory, and worth publishing: [Measurement value rules] tie
/// `outOfRange` to these bounds, so without them an appliance told a value is out of range
/// cannot say which way.
pub fn temperature_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    temperature::constraints(MEASUREMENT_ID, min, max, step)
}

/// The current temperature (Table 9), measured.
pub fn temperature(degrees: f64) -> CmdData {
    temperature_from(degrees, MeasurementValueSource::MeasuredValue, None, None)
}

/// The same, saying where the number came from and whether it can be trusted.
///
/// `source` is `M` in Table 9 and is not decoration: a `calculatedValue` from a model of
/// the tank and a `measuredValue` from a sensor in it are different claims, and an
/// appliance deciding whether a shower is available should know which it has.
///
/// `state` follows [MDT-005]: omit it for a good value, and set `outOfRange` or `error`
/// for one the appliance **SHALL ignore**.
pub fn temperature_from(
    degrees: f64,
    source: MeasurementValueSource,
    state: Option<MeasurementValueState>,
    taken_at: Option<AbsoluteOrRelativeTime>,
) -> CmdData {
    // [MDT-002]: only the newest value, and no history. The timestamp is the element
    // that rule permits; the second entry is the one it forbids.
    temperature::reported(MEASUREMENT_ID, degrees, source, state, taken_at)
}

/// The current hot water temperature, measured, and stamped with when.
///
/// [MDT-002] permits the `timestamp` element, and this is what it is for. A tank that has not changed temperature since the last notification is a tank
/// nobody has drawn from, which is a different thing from a probe that stopped. A client
/// that has only the arrival time cannot tell the two apart; one that has this can. See
/// [`Reading::timestamp`](crate::usecases::monitoring::Reading::timestamp).
pub fn temperature_at(degrees: f64, taken_at: AbsoluteOrRelativeTime) -> CmdData {
    temperature_from(
        degrees,
        MeasurementValueSource::MeasuredValue,
        None,
        Some(taken_at),
    )
}

// ---- what a Monitoring Appliance finds ----------------------------------------------

/// Finds a DHW Circuit's temperature feature from its detailed discovery and use-case
/// data.
///
/// The counterpart of [`monitoring::locate`](crate::usecases::monitoring::locate), and it
/// returns the same type, so a hot water tank goes into the same
/// [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor) as
/// a grid connection point. What it does *not* look for is an `ElectricalConnection`:
/// Table 6 gives this use case one feature, because a tank has no phases and no
/// connection to describe, and `monitoring::locate` — which searches for one — would find
/// whatever else the device happens to serve or nothing at all.
///
/// ```
/// use eebus::usecases::hvac::mdt;
/// # fn example(remote: &eebus::spine::RemoteDevice) -> Option<()> {
/// let circuit = mdt::locate(remote)?;
/// assert!(circuit.electrical_connection.is_none()); // a tank has none
/// # Some(())
/// # }
/// ```
///
/// Returns [`None`] until the circuit has announced both the use case and the
/// `Measurement` server that carries it.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
) -> Option<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate(remote, NAME, DHW_CIRCUIT_ACTOR)
}

/// Every DHW circuit on one device.
///
/// A device may hold more than one — a heat pump with two tanks announces the use case
/// once per `DHWCircuit` entity — and each is its own unit with its own temperature. See
/// [`temperature::locate_all`].
pub fn locate_all(
    remote: &crate::spine::RemoteDevice,
) -> alloc::vec::Vec<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate_all(remote, NAME, DHW_CIRCUIT_ACTOR)
}

// ---- descriptors ---------------------------------------------------------------------

const DHW_CIRCUIT_ENTITIES: &[EntityType] = &[EntityType::DHWCircuit];
/// The Monitoring Appliance sits behind any entity.
const MONITORING_APPLIANCE_ENTITIES: &[EntityType] = &[];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::Measurement,
        Function::MeasurementDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::Measurement,
        Function::MeasurementConstraintsListData,
    ),
    FunctionUse::server(FeatureType::Measurement, Function::MeasurementListData),
];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::Measurement,
        Function::MeasurementDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::Measurement,
        Function::MeasurementConstraintsListData,
    ),
    FunctionUse::client(FeatureType::Measurement, Function::MeasurementListData),
];

const SCENARIO_NAME: &str = "Monitor DHW temperature";

/// The DHW Circuit: the actor whose water is measured.
pub static DHW_CIRCUIT: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: DHW_CIRCUIT_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: DHW_CIRCUIT_ENTITIES,
    counterpart: MONITORING_APPLIANCE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: SERVER_FUNCTIONS,
    }],
};

/// The Monitoring Appliance: the actor reading it.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: MONITORING_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: MONITORING_APPLIANCE_ENTITIES,
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
    use crate::usecases::monitoring::{ReadingState, Readings};

    /// Table 7: the elements a client matches on, and the one commodity that is not power.
    #[test]
    fn the_description_says_hot_water_and_not_electricity() {
        let CmdData::MeasurementDescriptionListData(list) = temperature_description() else {
            panic!("expected the descriptions");
        };
        let entry = &list.measurement_description_data.as_ref().unwrap()[0];
        assert_eq!(
            entry.commodity_type.as_ref(),
            Some(&crate::model::CommodityType::DomesticHotWater),
            "MDT Table 7 fixes it, and it is mandatory"
        );
        assert_eq!(
            entry.scope_type.as_ref(),
            Some(&crate::model::ScopeType::DhwTemperature)
        );
        assert_eq!(
            entry.measurement_type.as_ref(),
            Some(&crate::model::MeasurementType::Temperature)
        );
    }

    /// [MDT-001]: the appliance reads it with the machinery it already has.
    #[test]
    fn mdt_001_the_temperature_is_read_through_the_measurement_layer() {
        let mut readings = Readings::new();
        assert!(readings.describe(&temperature_description()));
        readings.apply(&temperature(58.5));

        assert_eq!(readings.value(&MEASURAND), Some(58.5));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::Normal)
        );
        assert_eq!(
            MEASURAND.unit(),
            UnitOfMeasurement::DegC,
            "and it knows what unit that is"
        );
    }

    /// [MDT-005]: a flagged value is ignored, not handed back as a number.
    #[test]
    fn mdt_005_a_value_out_of_range_is_not_a_temperature() {
        let mut readings = Readings::new();
        readings.describe(&temperature_description());

        readings.apply(&temperature_from(
            -40.0,
            MeasurementValueSource::MeasuredValue,
            Some(MeasurementValueState::OutOfRange),
            None,
        ));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::OutOfRange)
        );
        assert_eq!(
            readings.value(&MEASURAND),
            None,
            "a value the specification says to ignore is not returned"
        );
    }

    /// A tank is not a heatsink: the two temperature scopes stay apart.
    #[test]
    fn the_hot_water_is_not_a_component_temperature() {
        let tank = Measurand::unphased(Quantity::DhwTemperature);
        let heatsink = Measurand::unphased(Quantity::Temperature);
        assert_ne!(tank.scope_type(), heatsink.scope_type());
        assert_ne!(tank.commodity_type(), heatsink.commodity_type());

        let mut readings = Readings::new();
        readings.describe(&temperature_description());
        readings.apply(&temperature(58.5));
        assert_eq!(
            readings.value(&heatsink),
            None,
            "reading one as the other is how a manager reads a tank as an inverter"
        );
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&DHW_CIRCUIT, &MONITORING_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert!(DHW_CIRCUIT.permits_entity(&EntityType::DHWCircuit));
        assert!(MONITORING_APPLIANCE.permits_entity(&EntityType::CEM));
    }
}
