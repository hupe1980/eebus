//! The one exchange behind MDT, MRT and MOT.
//!
//! Three use cases, one scenario each, and the same scenario: a server publishes one
//! `Measurement` and a Monitoring Appliance reads it. They differ in three constants —
//! the `useCaseName`, the actor that serves it, and the `scopeType` that says *which*
//! temperature — and in nothing else. Table 6 is the same table in all three documents,
//! Tables 7 to 9 differ by one row, and [MDT-002], [MRT-002] and [MOT-002] are the same
//! sentence: only the latest value, no history.
//!
//! So the exchange is written once here and the use-case modules carry what distinguishes
//! them:
//!
//! | | [`mdt`](super::mdt) | [`mrt`](super::mrt) | [`mot`](super::mot) |
//! |---|---|---|---|
//! | server actor | `DHWCircuit` | `HVACRoom` | `OutdoorTemperatureSensor` |
//! | entity | `DHWCircuit` | `HVACRoom` | `TemperatureSensor` |
//! | `commodityType` | `domesticHotWater` | `air` | `air` |
//! | `scopeType` | `dhwTemperature` | `roomAirTemperature` | `outsideAirTemperature` |
//!
//! Nothing here is called directly in ordinary use: each module re-exports what it needs
//! under its own names, with its own specification references.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, FeatureType, MeasurementConstraintsData, MeasurementConstraintsListData,
    MeasurementData, MeasurementDescriptionData, MeasurementDescriptionListData, MeasurementId,
    MeasurementListData, MeasurementValueSource, MeasurementValueState, MeasurementValueType, Role,
    ScaledNumber, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::monitoring::{Measurand, MonitoredUnitPeer};

/// Builds the read-only `Measurement` feature all three use cases are served from
/// (Table 6).
///
/// There is no `ElectricalConnection` beside it, which is what makes this family unlike
/// MPC and MGCP: a tank, a room and the outdoors have no phases and no connection to
/// describe, so the measurement description stands alone.
pub fn measurement_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Measurement, Role::Server)
        .with_function(
            crate::model::Function::MeasurementDescriptionListData,
            Operations::read(),
        )
        .with_function(
            crate::model::Function::MeasurementConstraintsListData,
            Operations::read(),
        )
        .with_function(
            crate::model::Function::MeasurementListData,
            Operations::read(),
        )
}

/// The measurement's description (Table 7), in the unit the server works in.
///
/// All three documents permit `degC`, `degF` and `K`. A server that reports Fahrenheit and
/// an appliance that assumes Celsius disagree by forty degrees at the temperatures that
/// matter, and nothing on the wire objects.
pub fn description(measurand: &Measurand, id: MeasurementId, unit: UnitOfMeasurement) -> CmdData {
    CmdData::MeasurementDescriptionListData(MeasurementDescriptionListData {
        measurement_description_data: Some(vec![MeasurementDescriptionData {
            measurement_id: Some(id),
            measurement_type: Some(measurand.measurement_type()),
            commodity_type: Some(measurand.commodity_type()),
            unit: Some(unit),
            scope_type: Some(measurand.scope_type()),
            ..Default::default()
        }]),
    })
}

/// The range and granularity the server can report (Table 8).
///
/// Recommended rather than mandatory, and worth publishing: [Measurement value rules] tie
/// `outOfRange` to these bounds, so without them an appliance told a value is out of range
/// cannot say which way.
pub fn constraints(id: MeasurementId, min: f64, max: f64, step: Option<f64>) -> CmdData {
    CmdData::MeasurementConstraintsListData(MeasurementConstraintsListData {
        measurement_constraints_data: Some(vec![MeasurementConstraintsData {
            measurement_id: Some(id),
            value_range_min: Some(ScaledNumber::from_f64(min, 1)),
            value_range_max: Some(ScaledNumber::from_f64(max, 1)),
            value_step_size: step.map(|step| ScaledNumber::from_f64(step, 1)),
        }]),
    })
}

/// The current temperature (Table 9).
///
/// `source` is `M` in every one of the three tables and is not decoration: a
/// `calculatedValue` from a model and a `measuredValue` from a sensor are different claims.
/// `state` follows [MDT-005] / [MRT-005] / [MOT-005] — omit it for a good value, and set
/// `outOfRange` or `error` for one the appliance **SHALL ignore**.
pub fn reported(
    id: MeasurementId,
    degrees: f64,
    source: MeasurementValueSource,
    state: Option<MeasurementValueState>,
) -> CmdData {
    CmdData::MeasurementListData(MeasurementListData {
        measurement_data: Some(vec![MeasurementData {
            measurement_id: Some(id),
            value_type: Some(MeasurementValueType::Value),
            value: Some(ScaledNumber::from_f64(degrees, 1)),
            value_source: Some(source),
            value_state: state,
            // Only the newest value, and no history. A timestamp is permitted and adds
            // nothing here — a second entry would be the forbidden thing.
            ..Default::default()
        }]),
    })
}

/// Finds the first server of this use case on a peer.
///
/// What it does *not* look for is an `ElectricalConnection`:
/// [`monitoring::locate`](crate::usecases::monitoring::locate) searches for one, and on a
/// thermometer would find whatever else the device happens to serve or nothing at all.
pub fn locate(
    remote: &crate::spine::RemoteDevice,
    use_case: &str,
    actor: &str,
) -> Option<MonitoredUnitPeer> {
    locate_all(remote, use_case, actor).into_iter().next()
}

/// Finds **every** entity of a peer that serves this use case.
///
/// One device is regularly several: a heat-pump gateway announces one `HVACRoom` per room,
/// each with its own `Measurement` feature and its own temperature. Each is a separate
/// [`MonitoredUnitPeer`] with its own
/// [`UnitId`](crate::usecases::monitoring::UnitId), and a
/// [`MonitoringApplianceActor`](crate::usecases::monitoring::MonitoringApplianceActor)
/// holds them side by side.
///
/// Empty until the peer has announced both the use case and the `Measurement` server that
/// carries it.
pub fn locate_all(
    remote: &crate::spine::RemoteDevice,
    use_case: &str,
    actor: &str,
) -> Vec<MonitoredUnitPeer> {
    let Some(device) = remote.address.clone() else {
        return Vec::new();
    };
    let mut found: Vec<MonitoredUnitPeer> = Vec::new();
    for played in remote.use_cases_played(use_case, actor) {
        let Some(measurement) = remote.address_of(played, &FeatureType::Measurement, Role::Server)
        else {
            continue;
        };
        // A device that announces the same use case twice against one entity — which §7.5
        // does not forbid — is one thermometer, not two.
        if found
            .iter()
            .any(|peer| peer.measurement.as_ref() == Some(&measurement))
        {
            continue;
        }
        found.push(MonitoredUnitPeer::measuring(device.clone(), measurement));
    }
    found
}
