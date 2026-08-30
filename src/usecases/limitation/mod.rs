//! The machinery shared by Limitation of Power Consumption and Production.
//!
//! LPC and LPP are the same use case pointed in opposite directions. Their technical
//! specifications are structurally identical — the same four scenarios, the same table
//! numbers, the same thirteen state transitions, the same 120-second heartbeat window and
//! two-to-twenty-four-hour failsafe duration — and their requirement identifiers differ
//! only in the prefix: [LPC-901] and [LPP-901] state the same rule. The 2026
//! implementation guides run in parallel too, down to the section numbers.
//!
//! So the state machine and the actor are written once here and pointed by
//! [`Direction`]. What differs between the two use cases is the direction announced in
//! the limit description, the name of the failsafe configuration key, and the use-case
//! descriptor a device publishes — everything else is shared, which means a fix to one is
//! a fix to both.
//!
//! Use it through [`crate::usecases::lpc`] or [`crate::usecases::lpp`], which carry the
//! descriptors and the direction constant for their use case.

use crate::model::{DeviceConfigurationKeyName, EnergyDirection};
use crate::usecases::descriptor::names;

mod actor;
pub use actor::*;

mod state;
pub use state::*;

/// Which of the two limitation use cases an actor is playing.
///
/// The state machine does not depend on this — a limit is a number of watts either way —
/// but everything a Controllable System *publishes* does: the `limitDirection` in the
/// limit description, and the name of the failsafe configuration key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Limitation of Power Consumption: how much the device may draw.
    ///
    /// The basis of §14a EnWG in Germany.
    Consumption,
    /// Limitation of Power Production: how much the device may feed in.
    ///
    /// The basis of EEG §9 (2023) in Germany.
    Production,
}

impl Direction {
    /// The `limitDirection` of the limit description (LPC/LPP Table 22).
    pub const fn energy_direction(self) -> EnergyDirection {
        match self {
            Self::Consumption => EnergyDirection::Consume,
            Self::Production => EnergyDirection::Produce,
        }
    }

    /// The `keyName` of the failsafe power limit ([LPC-021], [LPP-021]).
    ///
    /// The schema names both, so this is a variant rather than a string: a typo cannot
    /// reach the wire as an `Other` extension nobody recognises.
    pub const fn failsafe_limit_key(self) -> DeviceConfigurationKeyName {
        match self {
            Self::Consumption => DeviceConfigurationKeyName::FailsafeConsumptionActivePowerLimit,
            Self::Production => DeviceConfigurationKeyName::FailsafeProductionActivePowerLimit,
        }
    }

    /// The use-case name a device announces in `nodeManagementUseCaseData`.
    pub const fn use_case_name(self) -> &'static str {
        match self {
            Self::Consumption => names::LPC,
            Self::Production => names::LPP,
        }
    }

    /// The name of scenario 1, as the specification writes it.
    pub const fn scenario_one_name(self) -> &'static str {
        match self {
            Self::Consumption => "Control active power consumption limit",
            Self::Production => "Control active power production limit",
        }
    }
}

/// The `keyName` of the failsafe duration, which both use cases share
/// ([LPC-022], [LPP-022]).
pub const FAILSAFE_DURATION_MINIMUM_KEY: DeviceConfigurationKeyName =
    DeviceConfigurationKeyName::FailsafeDurationMinimum;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_directions_publish_what_their_specifications_ask_for() {
        assert_eq!(
            Direction::Consumption.failsafe_limit_key().as_str(),
            "failsafeConsumptionActivePowerLimit",
            "LPC Table 24"
        );
        assert_eq!(
            Direction::Production.failsafe_limit_key().as_str(),
            "failsafeProductionActivePowerLimit",
            "LPP Table 24"
        );
        assert_eq!(
            FAILSAFE_DURATION_MINIMUM_KEY.as_str(),
            "failsafeDurationMinimum",
            "shared by both, LPC/LPP Table 24"
        );
        assert_eq!(
            Direction::Consumption.energy_direction(),
            EnergyDirection::Consume
        );
        assert_eq!(
            Direction::Production.energy_direction(),
            EnergyDirection::Produce
        );
    }
}
