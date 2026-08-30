//! Monitoring of Power Consumption (MPC).
//!
//! A *Monitoring Appliance* — typically a CEM — reads what a *Monitored Unit* is drawing
//! or producing: momentary power, energy exchanged, current and voltage per phase, and
//! grid frequency. It is one of the four use cases certifiable since July 2026, and it
//! pairs with LPC: the limit LPC sets is checked against what MPC reports, which is why
//! the implementation guide requires the two to share a `measurementId`.
//!
//! Five scenarios make it up, of which only the first is mandatory:
//!
//! 1. **Monitor power** — the total active power, and optionally per phase.
//! 2. **Monitor energy** — energy consumed and produced.
//! 3. **Monitor current** — per phase.
//! 4. **Monitor voltage** — per phase, or between two phases.
//! 5. **Monitor frequency** — of the grid.
//!
//! Throughout, the load convention applies ([MPC-001]): consumption is positive and
//! production negative. The machinery is shared with [`crate::usecases::mgcp`] and lives
//! in [`crate::usecases::monitoring`]; this module carries the descriptors.

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::monitoring::Naming;

/// How a Monitored Unit names its energy scopes for this use case.
pub const NAMING: Naming = Naming::Appliance;

/// Entity types a Monitored Unit may live on (MPC §3.2.2.1.1).
const MONITORED_UNIT_ENTITIES: &[EntityType] = &[
    EntityType::Compressor,
    EntityType::ElectricalImmersionHeater,
    EntityType::EVSE,
    EntityType::HeatPumpAppliance,
    EntityType::Inverter,
    EntityType::SmartEnergyAppliance,
    EntityType::SubMeterElectricity,
];

/// The functions each side uses. Every scenario uses the same four.
///
/// MPC does not split its features by scenario the way LPC does: which measurements exist
/// is what says which scenarios are supported, and those are described in one place. So
/// the same four functions carry all five scenarios, and a peer tells them apart by the
/// `scopeType` of each measurement.
const SERVER_FUNCTIONS: &[FunctionUse] = &[
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

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
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

/// The Monitoring Appliance: the actor that reads the measurements.
///
/// Its entity list is empty because MPC §3.2.1.1 places no restriction — the use-case
/// data follows behind any entity type.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MPC,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: &[],
    counterpart: actors::MONITORED_UNIT,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor power",
            support: Support::Mandatory,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "Monitor energy",
            support: Support::Optional,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 3,
            name: "Monitor current",
            support: Support::Recommended,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 4,
            name: "Monitor voltage",
            support: Support::Optional,
            functions: CLIENT_FUNCTIONS,
        },
        Scenario {
            number: 5,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: CLIENT_FUNCTIONS,
        },
    ],
};

/// The Monitored Unit: the actor that measures.
pub static MONITORED_UNIT: UseCaseDescriptor = UseCaseDescriptor {
    name: names::MPC,
    actor: actors::MONITORED_UNIT,
    role: ActorRole::Server,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: MONITORED_UNIT_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Monitor power",
            support: Support::Mandatory,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: "Monitor energy",
            support: Support::Optional,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 3,
            // `R` in Table 1, and only where the unit knows which phases it is on.
            name: "Monitor current",
            support: Support::Recommended,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 4,
            name: "Monitor voltage",
            support: Support::Optional,
            functions: SERVER_FUNCTIONS,
        },
        Scenario {
            number: 5,
            name: "Monitor frequency",
            support: Support::Optional,
            functions: SERVER_FUNCTIONS,
        },
    ],
};
