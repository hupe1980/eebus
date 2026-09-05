//! Monitoring of Room Temperature (MRT).
//!
//! A *Monitoring Appliance* reads the air temperature of an indoor space. The Actor **HVAC
//! Room** is "a logical or physical indoor space of a house or premises" (§2.2.1) — a
//! single room, or a whole floor — and a heat pump is usually the one device in a household
//! that already measures one, because its own heating curve runs on it.
//!
//! One scenario, mandatory for both actors, and the same two rules the rest of the family
//! states:
//!
//! * **[MRT-002]: only the latest value.** The `timestamp` element is permitted but
//!   "additional historical values are forbidden" — this is a thermometer, not a log.
//! * **[MRT-005]: the value state decides.** `outOfRange` and `error` **SHALL be ignored**
//!   by the appliance, which [`Readings`](crate::usecases::monitoring::Readings) already
//!   enforces: a flagged value is never handed back as a number.
//!
//! # Why it is worth having
//!
//! §2.1 puts it plainly: "with more sophisticated energy management capabilities, this
//! information could also be used to estimate energy demands for heating or cooling". That
//! is the whole reason a controller wants it. A building's thermal behaviour is identified
//! from three signals — indoor temperature, outdoor temperature, and the heat delivered
//! into it — and a fitted `RC` model is what turns "the roof will export at noon" into "run
//! the compressor at eleven". This use case is the first of the three;
//! [`mot`](super::mot) is the second.
//!
//! Without it, [`ohpcf`](crate::usecases::ohpcf) can start a compressor for a house whose
//! thermal state nothing on the wire reports — the plan is expressible and the state it is
//! planned against is not.
//!
//! # Reading it
//!
//! There is no reader of its own, for the same reason [`mdt`](super::mdt) has none: this is
//! a `Measurement`, and the measurement layer already resolves one against the descriptions
//! that give it meaning.
//!
//! ```
//! use eebus::usecases::hvac::mrt;
//! use eebus::usecases::monitoring::Readings;
//!
//! let mut readings = Readings::new();
//! readings.describe(&mrt::temperature_description());
//! readings.apply(&mrt::temperature(21.5));
//!
//! assert_eq!(readings.value(&mrt::MEASURAND), Some(21.5));
//! ```
//!
//! # More than one room
//!
//! A device that monitors four rooms announces this use case four times, once per
//! `HVACRoom` entity, and each has its own `Measurement` feature. [`locate_all`] returns all
//! of them; [`locate`] returns the first, which is what a building with one thermostat
//! wants. They are separate units to a
//! [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor),
//! told apart by [`UnitId`](crate::usecases::monitoring::UnitId).

use crate::model::{
    CmdData, EntityType, FeatureType, Function, MeasurementId, MeasurementValueSource,
    MeasurementValueState, UnitOfMeasurement,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
use crate::usecases::hvac::temperature;
use crate::usecases::monitoring::{Measurand, Quantity};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "monitoringOfRoomTemperature";

/// The actor that *is* the room: "a logical or physical indoor space" (§2.2.1).
pub const HVAC_ROOM_ACTOR: &str = "HVACRoom";

/// The actor that reads its temperature.
pub const MONITORING_APPLIANCE_ACTOR: &str = "MonitoringAppliance";

/// The `measurementId` **this** implementation publishes the room temperature under.
///
/// A local choice, `<m1#(1..1)>`. A peer's is found the way every measurement is: from its
/// description, by `scopeType: roomAirTemperature`, which
/// [`Readings`](crate::usecases::monitoring::Readings) does. Assuming `1` on a device that
/// also serves [`mdt`](super::mdt) would read the hot water as a living room.
pub const MEASUREMENT_ID: MeasurementId = MeasurementId(1);

/// What this use case measures, as the measurement layer names it.
pub const MEASURAND: Measurand = Measurand::unphased(Quantity::RoomTemperature);

// ---- the feature an HVAC Room serves --------------------------------------------------

/// Builds the `Measurement` feature scenario 1 is served from (Table 6).
///
/// Read-only, and alone: a room has no phases and no electrical connection to describe, so
/// there is no `ElectricalConnection` beside it.
pub fn measurement_feature(address: u32) -> LocalFeature {
    temperature::measurement_feature(address)
}

// ---- what an HVAC Room publishes ------------------------------------------------------

/// The measurement's description (Table 7).
///
/// `commodityType: air` is `M`, and is what tells a client filtering on the commodity that
/// this is not electricity and not the hot water.
pub fn temperature_description() -> CmdData {
    temperature_description_in(UnitOfMeasurement::DegC)
}

/// The same, in the unit the room's sensor actually works in.
///
/// Table 7 permits `degC`, `degF` and `K`. A thermostat reporting Fahrenheit and a manager
/// assuming Celsius disagree by more than the whole comfort band, and nothing on the wire
/// objects.
pub fn temperature_description_in(unit: UnitOfMeasurement) -> CmdData {
    temperature::description(&MEASURAND, MEASUREMENT_ID, unit)
}

/// The range and granularity the room's sensor can report (Table 8).
pub fn temperature_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    temperature::constraints(MEASUREMENT_ID, min, max, step)
}

/// The current room temperature (Table 9), measured.
pub fn temperature(degrees: f64) -> CmdData {
    temperature_from(degrees, MeasurementValueSource::MeasuredValue, None)
}

/// The same, saying where the number came from and whether it can be trusted.
///
/// `source` is `M` in Table 9: a `measuredValue` from a wall sensor, a `calculatedValue`
/// from a model of the floor, and an `empiricalValue` are different claims, and a
/// controller fitting a building against them should know which it has.
///
/// `state` follows [MRT-005]: omit it for a good value, and set `outOfRange` or `error` for
/// one the appliance **SHALL ignore**.
pub fn temperature_from(
    degrees: f64,
    source: MeasurementValueSource,
    state: Option<MeasurementValueState>,
) -> CmdData {
    // [MRT-002]: only the newest value, and no history.
    temperature::reported(MEASUREMENT_ID, degrees, source, state)
}

// ---- what a Monitoring Appliance finds ------------------------------------------------

/// Finds one HVAC Room's temperature feature from a peer's discovery and use-case data.
///
/// The first, where a device has several — see [`locate_all`]. It returns the same
/// [`MonitoredUnitPeer`](crate::usecases::monitoring::MonitoredUnitPeer) as
/// [`mdt::locate`](super::mdt::locate), so a room goes into the same
/// [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor) as a
/// hot water tank and a grid connection point.
///
/// ```
/// use eebus::usecases::hvac::mrt;
/// # fn example(remote: &eebus::spine::RemoteDevice) -> Option<()> {
/// let room = mrt::locate(remote)?;
/// assert!(room.electrical_connection.is_none()); // a room has none
/// # Some(())
/// # }
/// ```
///
/// Returns [`None`] until the peer has announced both the use case and the `Measurement`
/// server that carries it.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
) -> Option<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate(remote, NAME, HVAC_ROOM_ACTOR)
}

/// Every room a peer monitors.
///
/// A building is rarely one room, and §7.5 lets a device say so: the use-case information
/// is a list keyed by address, so a gateway announces `HVACRoom` once per entity. Each is
/// its own unit with its own `Measurement` feature and its own
/// [`UnitId`](crate::usecases::monitoring::UnitId).
///
/// ```
/// # use core::time::Duration;
/// use eebus::usecases::hvac::mrt;
/// use eebus::usecases::monitoring::MonitoringApplianceActor;
/// # fn example(
/// #     engine: &mut eebus::spine::Engine,
/// #     appliance: &mut MonitoringApplianceActor,
/// #     remote: &eebus::spine::RemoteDevice,
/// #     now: Duration,
/// # ) {
/// for room in mrt::locate_all(remote) {
///     appliance.attach(engine, room, now);
/// }
/// # }
/// ```
pub fn locate_all(
    remote: &crate::spine::RemoteDevice,
) -> alloc::vec::Vec<crate::usecases::monitoring::MonitoredUnitPeer> {
    temperature::locate_all(remote, NAME, HVAC_ROOM_ACTOR)
}

// ---- descriptors ---------------------------------------------------------------------

const HVAC_ROOM_ENTITIES: &[EntityType] = &[EntityType::HVACRoom];
/// The Monitoring Appliance sits behind any entity (§3.2.2.1).
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

const SCENARIO_NAME: &str = "Monitor HVAC room temperature";

/// The HVAC Room: the actor whose air is measured.
pub static HVAC_ROOM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: HVAC_ROOM_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: HVAC_ROOM_ENTITIES,
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
    use crate::usecases::monitoring::{ReadingState, Readings};
    use alloc::vec::Vec;

    /// Table 7: the three elements a client matches on.
    #[test]
    fn the_description_says_air_and_a_room() {
        let CmdData::MeasurementDescriptionListData(list) = temperature_description() else {
            panic!("expected the descriptions");
        };
        let entry = &list.measurement_description_data.as_ref().unwrap()[0];
        assert_eq!(
            entry.commodity_type.as_ref(),
            Some(&crate::model::CommodityType::Air),
            "MRT Table 7 fixes it, and it is mandatory"
        );
        assert_eq!(
            entry.scope_type.as_ref(),
            Some(&crate::model::ScopeType::RoomAirTemperature)
        );
        assert_eq!(
            entry.measurement_type.as_ref(),
            Some(&crate::model::MeasurementType::Temperature)
        );
        assert_eq!(entry.unit.as_ref(), Some(&UnitOfMeasurement::DegC));
    }

    /// [MRT-001]: the appliance reads it with the machinery it already has.
    #[test]
    fn mrt_001_the_temperature_is_read_through_the_measurement_layer() {
        let mut readings = Readings::new();
        assert!(readings.describe(&temperature_description()));
        readings.apply(&temperature(21.5));

        assert_eq!(readings.value(&MEASURAND), Some(21.5));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::Normal)
        );
        assert_eq!(MEASURAND.unit(), UnitOfMeasurement::DegC);
    }

    /// [MRT-005]: a flagged value is ignored, not handed back as a number.
    #[test]
    fn mrt_005_a_value_out_of_range_is_not_a_temperature() {
        let mut readings = Readings::new();
        readings.describe(&temperature_description());
        readings.apply(&temperature_from(
            -40.0,
            MeasurementValueSource::MeasuredValue,
            Some(MeasurementValueState::OutOfRange),
        ));
        assert_eq!(
            readings.get(&MEASURAND).map(|r| r.state),
            Some(ReadingState::OutOfRange)
        );
        assert_eq!(readings.value(&MEASURAND), None);
    }

    /// A room is not a tank and not a heatsink: three temperatures, three scopes.
    #[test]
    fn the_three_temperatures_stay_apart() {
        let room = MEASURAND;
        let tank = super::super::mdt::MEASURAND;
        let heatsink = Measurand::unphased(Quantity::Temperature);
        let outdoors = super::super::mot::MEASURAND;

        let scopes: Vec<_> = [&room, &tank, &heatsink, &outdoors]
            .iter()
            .map(|m| m.scope_type())
            .collect();
        let mut unique = scopes.clone();
        unique.sort_by_key(|s| alloc::string::String::from(s.as_str()));
        unique.dedup();
        assert_eq!(unique.len(), scopes.len(), "four scopes, all different");

        let mut readings = Readings::new();
        readings.describe(&temperature_description());
        readings.apply(&temperature(21.5));
        assert_eq!(readings.value(&tank), None);
        assert_eq!(readings.value(&heatsink), None);
        assert_eq!(readings.value(&outdoors), None);
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&HVAC_ROOM, &MONITORING_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert_eq!(HVAC_ROOM.use_case_actor().as_str(), "HVACRoom");
        assert!(HVAC_ROOM.permits_entity(&EntityType::HVACRoom));
        assert!(!HVAC_ROOM.permits_entity(&EntityType::DHWCircuit));
        assert!(MONITORING_APPLIANCE.permits_entity(&EntityType::CEM));
    }

    /// §3.2.1.2: the room serves only reads. Nothing here is written.
    #[test]
    fn the_room_is_read_only() {
        assert_eq!(
            HVAC_ROOM.features_needing_binding().count(),
            0,
            "no writeable function, so no binding"
        );
        assert_eq!(
            MONITORING_APPLIANCE.features_needing_binding().count(),
            0,
            "and the appliance never writes one"
        );
    }
}
