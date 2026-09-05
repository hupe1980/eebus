//! Limitation of Power Consumption (LPC).
//!
//! An *Energy Guard* — a grid operator's control box, or an energy manager acting for
//! one — tells a *Controllable System* how much power it may draw. The use case is the
//! technical basis for §14a EnWG in Germany, and the first one the EEBUS certification
//! covers.
//!
//! Four scenarios make it up:
//!
//! 1. **Control active power consumption limit** — the limit itself, with an optional
//!    duration, acknowledged or refused by the Controllable System.
//! 2. **Failsafe values** — what applies when the Energy Guard falls silent, and for how
//!    long at least.
//! 3. **Heartbeat** — a message every sixty seconds in each direction, whose absence is
//!    what triggers the failsafe.
//! 4. **Constraints** — the nominal maxima, so the Energy Guard knows what it is
//!    limiting.
//!
//! LPC and LPP are the same use case in opposite directions, so the state machine and the
//! actor live in [`crate::usecases::limitation`] and serve both. This module carries what
//! is specific to LPC: the descriptors taken from UC TS 1.0.0 Tables 2, 12 and 21 with
//! the entity-type list of the 1.1.0 implementation guide §3.10, and the [`DIRECTION`] an
//! actor is built with.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::{limitation::{ControllableSystem, CsConfig}, lpc};
//!
//! // A heat pump that falls back to the 4.2 kW §14a leaves it.
//! let system = ControllableSystem::new(
//!     CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)).with_nominal_max(11_000.0),
//!     Duration::ZERO,
//! );
//! assert_eq!(lpc::DIRECTION.failsafe_limit_key().as_str(), "failsafeConsumptionActivePowerLimit");
//! # let _ = system;
//! ```

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::limitation::{Direction, HEARTBEAT_PERIOD};

/// The direction a Controllable System actor is built with for this use case.
pub const DIRECTION: Direction = Direction::Consumption;

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
            // [LPC-005] and [LPC-006]: "SHALL be sent at least every 60 seconds", in
            // both directions. It is the one function of the use case whose *silence*
            // means something — see `Delivery::Periodic`.
            functions: &[
                FunctionUse::server(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                )
                .periodic(HEARTBEAT_PERIOD),
                FunctionUse::client(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                )
                .periodic(HEARTBEAT_PERIOD),
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
            // [LPC-005] and [LPC-006]: "SHALL be sent at least every 60 seconds", in
            // both directions. It is the one function of the use case whose *silence*
            // means something — see `Delivery::Periodic`.
            functions: &[
                FunctionUse::server(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                )
                .periodic(HEARTBEAT_PERIOD),
                FunctionUse::client(
                    FeatureType::DeviceDiagnosis,
                    Function::DeviceDiagnosisHeartbeatData,
                )
                .periodic(HEARTBEAT_PERIOD),
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
