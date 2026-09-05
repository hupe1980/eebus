//! Monitoring of Outdoor Temperature (MOT).
//!
//! A *Monitoring Appliance* reads the outdoor air temperature from an *Outdoor Temperature
//! Sensor* — which in practice is a heat pump, because a heat pump measures it anyway: its
//! defrost logic and its heating curve run on nothing else.
//!
//! One scenario, mandatory for both actors, and the same two rules as the rest of the
//! family:
//!
//! * **[MOT-002]: only the latest value.** "Additional historical values are forbidden."
//! * **[MOT-005]: the value state decides.** `outOfRange` and `error` **SHALL be ignored**
//!   by the appliance, which [`Readings`](crate::usecases::monitoring::Readings) enforces.
//!
//! # Why it is worth having
//!
//! §2.1's added value is "monitoring the outdoor temperature can help estimate energy
//! demands", and for a controller that is an understatement: the outdoor temperature is one
//! of the three signals a building's thermal model is identified from — the others being
//! the indoor temperature ([`mrt`](super::mrt)) and the heat delivered into the building.
//! Fitting those three is what turns a weather forecast into a compressor schedule, and it
//! is the input [`ohpcf`](crate::usecases::ohpcf) plans against.
//!
//! Taking it from the appliance rather than from a weather service is not merely
//! convenient. A forecast is for a grid square; the sensor is on the wall of *this*
//! building, in its own shade and its own wind, and the difference is several degrees on
//! the days it matters.
//!
//! ```
//! use eebus::usecases::hvac::mot;
//! use eebus::usecases::monitoring::Readings;
//!
//! let mut readings = Readings::new();
//! readings.describe(&mot::temperature_description());
//! readings.apply(&mot::temperature(-3.5));
//!
//! assert_eq!(readings.value(&mot::MEASURAND), Some(-3.5));
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
pub const NAME: &str = "monitoringOfOutdoorTemperature";

/// The actor that measures it (§2.2.1).
pub const OUTDOOR_TEMPERATURE_SENSOR_ACTOR: &str = "OutdoorTemperatureSensor";

/// The actor that reads it.
pub const MONITORING_APPLIANCE_ACTOR: &str = "MonitoringAppliance";

/// The `measurementId` **this** implementation publishes the outdoor temperature under.
///
/// A local choice, `<m1#(1..1)>`. A peer's is found from its description, by
/// `scopeType: outsideAirTemperature` — a heat pump that serves this *and*
/// [`mrt`](super::mrt) *and* [`mdt`](super::mdt) publishes three temperatures, and only the
/// scope tells them apart.
pub const MEASUREMENT_ID: MeasurementId = MeasurementId(1);

/// What this use case measures, as the measurement layer names it.
pub const MEASURAND: Measurand = Measurand::unphased(Quantity::OutdoorTemperature);

// ---- the feature an Outdoor Temperature Sensor serves ----------------------------------

/// Builds the `Measurement` feature scenario 1 is served from (Table 6).
///
/// Read-only, and alone: a thermometer has no phases and no electrical connection to
/// describe.
pub fn measurement_feature(address: u32) -> LocalFeature {
    temperature::measurement_feature(address)
}

// ---- what an Outdoor Temperature Sensor publishes --------------------------------------

/// The measurement's description (Table 7).
pub fn temperature_description() -> CmdData {
    temperature_description_in(UnitOfMeasurement::DegC)
}

/// The same, in the unit the sensor actually works in.
///
/// Table 7 permits `degC`, `degF` and `K`.
pub fn temperature_description_in(unit: UnitOfMeasurement) -> CmdData {
    temperature::description(&MEASURAND, MEASUREMENT_ID, unit)
}

/// The range and granularity the sensor can report (Table 8).
pub fn temperature_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    temperature::constraints(MEASUREMENT_ID, min, max, step)
}

/// The current outdoor temperature (Table 9), measured.
pub fn temperature(degrees: f64) -> CmdData {
    temperature_from(degrees, MeasurementValueSource::MeasuredValue, None, None)
}

/// The same, saying where the number came from and whether it can be trusted.
///
/// `state` follows [MOT-005]: omit it for a good value, and set `outOfRange` or `error` for
/// one the appliance **SHALL ignore**. A defrosting heat pump reads its own outdoor sensor
/// as several degrees warm, and `outOfRange` is how it says so.
pub fn temperature_from(
    degrees: f64,
    source: MeasurementValueSource,
    state: Option<MeasurementValueState>,
    taken_at: Option<AbsoluteOrRelativeTime>,
) -> CmdData {
    // [MOT-002]: only the newest value, and no history. The timestamp is the element
    // that rule permits; the second entry is the one it forbids.
    temperature::reported(MEASUREMENT_ID, degrees, source, state, taken_at)
}

/// The current outdoor temperature, measured, and stamped with when.
///
/// [MOT-002] permits the `timestamp` element, and this is what it is for. An outdoor sensor that reports every few minutes and a forecast fitted against it
/// need the same clock, and the sensor's is the one that matters. A client
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

// ---- what a Monitoring Appliance finds -------------------------------------------------

/// Finds an Outdoor Temperature Sensor's feature from a peer's discovery and use-case data.
///
/// Returns [`None`] until the peer has announced both the use case and the `Measurement`
/// server that carries it.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
) -> Option<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate(remote, NAME, OUTDOOR_TEMPERATURE_SENSOR_ACTOR)
}

/// Every outdoor sensor a peer serves.
///
/// One, normally — there is only one outdoors — but the lookup is the same one
/// [`mrt::locate_all`](super::mrt::locate_all) uses, and a device announcing two sensors is
/// two units rather than one that shadows the other.
pub fn locate_all(
    remote: &crate::spine::RemoteDevice,
) -> alloc::vec::Vec<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate_all(remote, NAME, OUTDOOR_TEMPERATURE_SENSOR_ACTOR)
}

// ---- descriptors ---------------------------------------------------------------------

/// §3.2.1.1: the use-case data follow behind the entity type `TemperatureSensor`.
const SENSOR_ENTITIES: &[EntityType] = &[EntityType::TemperatureSensor];
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

const SCENARIO_NAME: &str = "Monitor outdoor temperature";

/// The Outdoor Temperature Sensor: the actor that measures.
pub static OUTDOOR_TEMPERATURE_SENSOR: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: OUTDOOR_TEMPERATURE_SENSOR_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: SENSOR_ENTITIES,
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
    counterpart: OUTDOOR_TEMPERATURE_SENSOR_ACTOR,
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
    use alloc::vec::Vec;

    /// Table 7: `air`, and the scope that says *which* air.
    #[test]
    fn the_description_says_air_and_outdoors() {
        let CmdData::MeasurementDescriptionListData(list) = temperature_description() else {
            panic!("expected the descriptions");
        };
        let entry = &list.measurement_description_data.as_ref().unwrap()[0];
        assert_eq!(
            entry.commodity_type.as_ref(),
            Some(&crate::model::CommodityType::Air)
        );
        assert_eq!(
            entry.scope_type.as_ref(),
            Some(&crate::model::ScopeType::OutsideAirTemperature),
            "the room and the outdoors share a commodity and differ only here"
        );
        assert_eq!(
            entry.measurement_type.as_ref(),
            Some(&crate::model::MeasurementType::Temperature)
        );
    }

    /// [MOT-001], and a temperature below zero, which is the interesting half of the range.
    #[test]
    fn mot_001_the_temperature_is_read_through_the_measurement_layer() {
        let mut readings = Readings::new();
        assert!(readings.describe(&temperature_description()));
        readings.apply(&temperature(-3.5));

        assert_eq!(readings.value(&MEASURAND), Some(-3.5));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::Normal)
        );
    }

    /// [MOT-005]: a sensor that knows it is wrong is not read.
    #[test]
    fn mot_005_a_flagged_value_is_not_a_temperature() {
        let mut readings = Readings::new();
        readings.describe(&temperature_description());
        readings.apply(&temperature_from(
            12.0,
            MeasurementValueSource::MeasuredValue,
            Some(MeasurementValueState::Error),
            None,
        ));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::Error)
        );
        assert_eq!(readings.value(&MEASURAND), None);
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&OUTDOOR_TEMPERATURE_SENSOR, &MONITORING_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert_eq!(
            OUTDOOR_TEMPERATURE_SENSOR.use_case_actor().as_str(),
            "OutdoorTemperatureSensor"
        );
        assert!(OUTDOOR_TEMPERATURE_SENSOR.permits_entity(&EntityType::TemperatureSensor));
        assert!(
            !OUTDOOR_TEMPERATURE_SENSOR.permits_entity(&EntityType::HVACRoom),
            "§3.2.1.1 puts it behind `TemperatureSensor`, not behind a room"
        );
    }
}
