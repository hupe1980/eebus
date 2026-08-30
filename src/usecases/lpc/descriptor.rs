//! The LPC use case as data: actors, scenarios and the SPINE resources each one needs.
//!
//! Taken from LPC UC TS 1.0.0 Tables 2, 12 and 21, with the entity-type list of §3.10 of
//! the 1.1.0 implementation guide.

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};

/// Entity types an Energy Guard may live on (LPC §3.2.1.1.1).
const ENERGY_GUARD_ENTITIES: &[EntityType] = &[EntityType::CEM, EntityType::GridGuard];

/// Entity types a Controllable System may live on (LPC §3.2.2.1.1), plus `BatterySystem`,
/// which implementation guide §3.10 added.
const CONTROLLABLE_SYSTEM_ENTITIES: &[EntityType] = &[
    EntityType::CEM,
    EntityType::Compressor,
    EntityType::EVSE,
    EntityType::HeatPumpAppliance,
    EntityType::Inverter,
    EntityType::SmartEnergyAppliance,
    EntityType::SubMeterElectricity,
    EntityType::BatterySystem,
];

/// The Energy Guard: the actor that sets the limit.
pub static ENERGY_GUARD: UseCaseDescriptor = UseCaseDescriptor {
    name: names::LPC,
    actor: actors::ENERGY_GUARD,
    role: ActorRole::Client,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: ENERGY_GUARD_ENTITIES,
    counterpart: actors::CONTROLLABLE_SYSTEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Control active power consumption limit",
            support: Support::Mandatory,
            functions: &[
                FunctionUse::client(
                    FeatureType::LoadControl,
                    Function::LoadControlLimitDescriptionListData,
                ),
                FunctionUse::client_writes(
                    FeatureType::LoadControl,
                    Function::LoadControlLimitListData,
                ),
            ],
        },
        Scenario {
            number: 2,
            name: "Failsafe values",
            support: Support::Mandatory,
            functions: &[
                FunctionUse::client(
                    FeatureType::DeviceConfiguration,
                    Function::DeviceConfigurationKeyValueDescriptionListData,
                ),
                FunctionUse::client_writes(
                    FeatureType::DeviceConfiguration,
                    Function::DeviceConfigurationKeyValueListData,
                ),
            ],
        },
        Scenario {
            number: 3,
            name: "Heartbeat",
            support: Support::Mandatory,
            // The Energy Guard both serves its own heartbeat and reads the Controllable
            // System's. Implementation guide §2.1.3: that reversal does not make it a
            // server actor.
            functions: &[
                FunctionUse::server(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                ),
                FunctionUse::client(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                ),
            ],
        },
        Scenario {
            number: 4,
            name: "Constraints",
            support: Support::Mandatory,
            functions: &[FunctionUse::client(
                FeatureType::ElectricalConnection,
                Function::ElectricalConnectionCharacteristicListData,
            )],
        },
    ],
};

/// The Controllable System: the actor that applies the limit.
pub static CONTROLLABLE_SYSTEM: UseCaseDescriptor = UseCaseDescriptor {
    name: names::LPC,
    actor: actors::CONTROLLABLE_SYSTEM,
    role: ActorRole::Server,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: CONTROLLABLE_SYSTEM_ENTITIES,
    counterpart: actors::ENERGY_GUARD,
    scenarios: &[
        Scenario {
            number: 1,
            name: "Control active power consumption limit",
            support: Support::Mandatory,
            functions: &[
                FunctionUse::server(
                    FeatureType::LoadControl,
                    Function::LoadControlLimitDescriptionListData,
                ),
                FunctionUse::server_writeable(
                    FeatureType::LoadControl,
                    Function::LoadControlLimitListData,
                ),
            ],
        },
        Scenario {
            number: 2,
            name: "Failsafe values",
            support: Support::Mandatory,
            functions: &[
                FunctionUse::server(
                    FeatureType::DeviceConfiguration,
                    Function::DeviceConfigurationKeyValueDescriptionListData,
                ),
                FunctionUse::server_writeable(
                    FeatureType::DeviceConfiguration,
                    Function::DeviceConfigurationKeyValueListData,
                ),
            ],
        },
        Scenario {
            number: 3,
            name: "Heartbeat",
            support: Support::Mandatory,
            functions: &[
                FunctionUse::server(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                ),
                FunctionUse::client(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                ),
            ],
        },
        Scenario {
            number: 4,
            name: "Constraints",
            // `R` for the Controllable System in Table 2, but the Energy Guard needs the
            // nominal maxima to compute a limit at all, so it is offered by default.
            support: Support::Recommended,
            functions: &[FunctionUse::server(
                FeatureType::ElectricalConnection,
                Function::ElectricalConnectionCharacteristicListData,
            )],
        },
    ],
};

/// The two configuration keys of scenario 2 (LPC Tables 24 and 25).
pub mod config_keys {
    /// `failsafeConsumptionActivePowerLimit` ([LPC-021]), a `scaledNumber` in watts.
    pub const FAILSAFE_CONSUMPTION_ACTIVE_POWER_LIMIT: &str = "failsafeConsumptionActivePowerLimit";
    /// `failsafeDurationMinimum` ([LPC-022]), a duration between two and 24 hours.
    pub const FAILSAFE_DURATION_MINIMUM: &str = "failsafeDurationMinimum";
}
