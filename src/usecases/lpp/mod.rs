//! Limitation of Power Production (LPP).
//!
//! An *Energy Guard* — a grid operator's control box, or an energy manager acting for
//! one — tells a *Controllable System* how much power it may feed into the grid. In
//! Germany it is the technical basis for EEG §9 (2023), which caps the feed-in of new PV
//! systems above 2 kWp; it has been certifiable alongside LPC since July 2026.
//!
//! It is LPC in reverse, and the specifications say so structurally: the same four
//! scenarios, the same tables, the same thirteen state transitions, the same timings.
//! The machinery therefore lives in [`crate::usecases::limitation`] and is shared; this
//! module carries what is specific to LPP — the use-case descriptors a device announces
//! and the [`DIRECTION`] the actor is built with.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::{limitation::{ControllableSystem, CsConfig}, lpp};
//!
//! // A PV inverter that falls back to 0 W when the control box goes quiet.
//! let system = ControllableSystem::new(
//!     CsConfig::new(0.0, Duration::from_secs(2 * 3_600)).with_nominal_max(10_000.0),
//!     Duration::ZERO,
//! );
//! assert_eq!(lpp::DIRECTION.failsafe_limit_key().as_str(), "failsafeProductionActivePowerLimit");
//! # let _ = system;
//! ```

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{
    ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor, actors, names,
};
use crate::usecases::limitation::{Direction, HEARTBEAT_PERIOD};

/// The direction a Controllable System actor is built with for this use case.
pub const DIRECTION: Direction = Direction::Production;

/// Entity types an Energy Guard may live on (LPP §3.2.1.1.1).
const ENERGY_GUARD_ENTITIES: &[EntityType] = &[EntityType::CEM, EntityType::GridGuard];

/// Entity types a Controllable System may live on (LPP §3.2.2.1.1), plus `BatterySystem`
/// and `PVSystem`, which implementation guide §3.10 added.
const CONTROLLABLE_SYSTEM_ENTITIES: &[EntityType] = &[
    EntityType::CEM,
    EntityType::EVSE,
    EntityType::Inverter,
    EntityType::SmartEnergyAppliance,
    EntityType::SubMeterElectricity,
    EntityType::BatterySystem,
    EntityType::PVSystem,
];

/// Scenario 1's name, as LPP §2.6.1 writes it.
const SCENARIO_ONE: &str = "Control active power production limit";

/// The Energy Guard: the actor that sets the limit.
pub static ENERGY_GUARD: UseCaseDescriptor = UseCaseDescriptor {
    name: names::LPP,
    actor: actors::ENERGY_GUARD,
    role: ActorRole::Client,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: ENERGY_GUARD_ENTITIES,
    counterpart: actors::CONTROLLABLE_SYSTEM,
    scenarios: &[
        Scenario {
            number: 1,
            name: SCENARIO_ONE,
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
            // [LPP-005] and [LPP-006]: "SHALL be sent at least every 60 seconds", in
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
    name: names::LPP,
    actor: actors::CONTROLLABLE_SYSTEM,
    role: ActorRole::Server,
    version: "1.0.0",
    document_sub_revision: "release",
    entity_types: CONTROLLABLE_SYSTEM_ENTITIES,
    counterpart: actors::ENERGY_GUARD,
    scenarios: &[
        Scenario {
            number: 1,
            name: SCENARIO_ONE,
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
            // [LPP-005] and [LPP-006]: "SHALL be sent at least every 60 seconds", in
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

/// The two configuration keys of scenario 2 (LPP Tables 24 and 25).
pub mod config_keys {
    /// `failsafeProductionActivePowerLimit` ([LPP-021]), a `scaledNumber` in watts.
    pub const FAILSAFE_PRODUCTION_ACTIVE_POWER_LIMIT: &str = "failsafeProductionActivePowerLimit";
    /// `failsafeDurationMinimum` ([LPP-022]), a duration between two and 24 hours.
    pub const FAILSAFE_DURATION_MINIMUM: &str = "failsafeDurationMinimum";
}
