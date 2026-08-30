//! Monitoring of the Grid Connection Point (MGCP).
//!
//! A *Monitoring Appliance* reads what is crossing the building's grid connection: the
//! momentary power, the energy fed in and drawn, current and voltage per phase, the grid
//! frequency, and the factor by which PV feed-in is curtailed. It is the fourth of the
//! use cases certifiable since July 2026, and it is what tells an energy manager whether
//! the building as a whole is importing or exporting.
//!
//! Seven scenarios make it up:
//!
//! 1. **Monitor PV feed-in power limitation factor** — the percentage of the installed
//!    PV peak power that may be fed in, as `pvCurtailmentLimitFactor`.
//! 2. **Monitor momentary power consumption/production** — the total at the connection.
//! 3. **Monitor total feed-in energy**.
//! 4. **Monitor total consumed energy**.
//! 5. **Monitor momentary current consumption/production phase details**.
//! 6. **Monitor voltage phase details**.
//! 7. **Monitor frequency**.
//!
//! Scenarios 2 to 7 are the same exchange as MPC, and share its implementation in
//! [`crate::usecases::monitoring`]; what differs is that a grid connection point names
//! the two energies from the grid's side — `gridConsumption` and `gridFeedIn` — which is
//! what [`NAMING`] selects.

use alloc::vec;

use crate::model::{
    CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyName, DeviceConfigurationKeyValueData,
    DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
    DeviceConfigurationKeyValueListData, DeviceConfigurationKeyValueType,
    DeviceConfigurationKeyValueValue, EntityType, FeatureType, Function, Role, ScaledNumber,
    UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::monitoring::Naming;

/// How a Grid Connection Point names its energy scopes for this use case.
pub const NAMING: Naming = Naming::GridConnectionPoint;

/// The `keyName` of scenario 1's limitation factor (MGCP Table 23).
///
/// Its value is a percentage of the cumulated nominal peak power of the building's PV
/// systems: the maximum feed-in is that factor times that sum ([MGCP-011]).
pub const PV_CURTAILMENT_LIMIT_FACTOR: DeviceConfigurationKeyName =
    DeviceConfigurationKeyName::PvCurtailmentLimitFactor;

/// The `keyId` this implementation gives the limitation factor.
pub const CURTAILMENT_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(1);

/// The range the factor may take, as a percentage (MGCP Table 23).
pub const CURTAILMENT_RANGE: core::ops::RangeInclusive<f64> = 0.0..=100.0;

/// Builds the `DeviceConfiguration` feature scenario 1 is served from.
///
/// Reads only: the factor is set by whatever configures the connection point, not by a
/// Monitoring Appliance, which is why MGCP marks no write on it.
pub fn curtailment_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceConfiguration, Role::Server)
        .with_function(
            Function::DeviceConfigurationKeyValueDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::DeviceConfigurationKeyValueListData,
            Operations::read(),
        )
}

/// The description of the limitation factor (MGCP Table 23).
pub fn curtailment_description() -> CmdData {
    CmdData::DeviceConfigurationKeyValueDescriptionListData(
        DeviceConfigurationKeyValueDescriptionListData {
            device_configuration_key_value_description_data: Some(vec![
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(CURTAILMENT_KEY),
                    key_name: Some(PV_CURTAILMENT_LIMIT_FACTOR),
                    value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
                    unit: Some(UnitOfMeasurement::Pct),
                    ..Default::default()
                },
            ]),
        },
    )
}

/// The limitation factor's current value, as a percentage (MGCP Table 24).
///
/// Values outside `0..=100` are clamped: the specification fixes the range, and a factor
/// above 100 would tell a client it may feed in more than the building can produce.
pub fn curtailment_value(percent: f64) -> CmdData {
    let percent = percent.clamp(*CURTAILMENT_RANGE.start(), *CURTAILMENT_RANGE.end());
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![DeviceConfigurationKeyValueData {
            key_id: Some(CURTAILMENT_KEY),
            value: Some(DeviceConfigurationKeyValueValue {
                scaled_number: Some(ScaledNumber::from_f64(percent, 2)),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    })
}

/// Reads the limitation factor out of a `deviceConfigurationKeyValueListData`.
///
/// Returns the percentage, which a Monitoring Appliance multiplies by the building's
/// cumulated nominal PV peak power to get the maximum feed-in ([MGCP-011]).
pub fn read_curtailment(data: &CmdData) -> Option<f64> {
    let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
        return None;
    };
    list.device_configuration_key_value_data
        .iter()
        .flatten()
        .find(|entry| entry.key_id == Some(CURTAILMENT_KEY))
        .and_then(|entry| entry.value.as_ref())
        .and_then(|value| value.scaled_number.as_ref())
        .and_then(ScaledNumber::to_f64)
}

/// Entity types a Grid Connection Point may live on (MGCP §3.2.2.1.1).
///
/// On a `CEM` the actor is a surrogate: the energy manager copies the values from the
/// real connection point and serves them on to other appliances.
const GRID_CONNECTION_POINT_ENTITIES: &[EntityType] =
    &[EntityType::CEM, EntityType::GridConnectionPointOfPremises];

const SERVER_MEASUREMENTS: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
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

const CLIENT_MEASUREMENTS: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
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

/// Scenario 1 alone uses `DeviceConfiguration`, not `Measurement`: the curtailment factor
/// is a configured value, not something the connection point measures.
const SERVER_CURTAILMENT: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

const CLIENT_CURTAILMENT: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::DeviceConfiguration,
        Function::DeviceConfigurationKeyValueListData,
    ),
];

/// The Monitoring Appliance: the actor that reads.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MGCP,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: &[],
    counterpart: actors::GRID_CONNECTION_POINT,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor PV feed-in power limitation factor",
            support: Support::Optional,
            functions: CLIENT_CURTAILMENT,
        },
        Scenario {
            number: 2,
            name: "Monitor momentary power consumption/production",
            support: Support::Recommended,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 3,
            name: "Monitor total feed-in energy",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: "Monitor total consumed energy",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: "Monitor momentary current consumption/production phase details",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: "Monitor voltage phase details",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
    ],
};

/// The Grid Connection Point: the actor that measures.
pub static GRID_CONNECTION_POINT: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MGCP,
    actor: actors::GRID_CONNECTION_POINT,
    role: ActorRole::Server,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: GRID_CONNECTION_POINT_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor PV feed-in power limitation factor",
            support: Support::Optional,
            functions: SERVER_CURTAILMENT,
        },
        Scenario {
            number: 2,
            name: "Monitor momentary power consumption/production",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 3,
            name: "Monitor total feed-in energy",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: "Monitor total consumed energy",
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: "Monitor momentary current consumption/production phase details",
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: "Monitor voltage phase details",
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mgcp_011_the_curtailment_factor_round_trips() {
        let published = curtailment_value(70.0);
        assert_eq!(read_curtailment(&published), Some(70.0));

        // MGCP Table 23 fixes the range at 0 to 100.
        assert_eq!(read_curtailment(&curtailment_value(140.0)), Some(100.0));
        assert_eq!(read_curtailment(&curtailment_value(-5.0)), Some(0.0));

        // A fraction of a percent survives, which whole-number rounding would lose.
        assert_eq!(read_curtailment(&curtailment_value(62.5)), Some(62.5));
    }

    #[test]
    fn the_description_names_the_key_the_specification_asks_for() {
        let CmdData::DeviceConfigurationKeyValueDescriptionListData(list) =
            curtailment_description()
        else {
            unreachable!("built above");
        };
        let entry = &list
            .device_configuration_key_value_description_data
            .as_ref()
            .unwrap()[0];
        assert_eq!(
            entry.key_name.as_ref().map(|k| k.as_str()),
            Some("pvCurtailmentLimitFactor")
        );
        assert_eq!(entry.unit, Some(UnitOfMeasurement::Pct));
    }
}
