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

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::monitoring::Naming;

/// How a Grid Connection Point names its energy scopes for this use case.
pub const NAMING: Naming = Naming::GridConnectionPoint;

/// The `keyName` of scenario 1's limitation factor (MGCP Table 12).
///
/// Its value is a percentage of the cumulated nominal peak power of the building's PV
/// systems: the maximum feed-in is that factor times that sum ([MGCP-011]).
pub const PV_CURTAILMENT_LIMIT_FACTOR: &str = "pvCurtailmentLimitFactor";

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
