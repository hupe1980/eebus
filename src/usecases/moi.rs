//! Monitoring of Inverter (MOI).
//!
//! What an inverter is doing, on top of what [MPC](crate::usecases::mpc) already says.
//! The specification is explicit about that layering (§2.5.1): an inverter **SHALL**
//! support MPC, which carries the ordinary AC values — active power, current, voltage,
//! frequency — and MOI adds the seven things that are specific to a machine converting
//! direct current into alternating:
//!
//! 1. **Identification** — manufacturer, model, firmware.
//! 2. **State** — working, standing by, or broken.
//! 3. **AC power details** — apparent and reactive power, total and per phase.
//! 4. **AC energy details** — the yields: today, this month, this year, and since it was
//!    installed.
//! 5. **AC additional details** — the power factor, cos φ.
//! 6. **Capabilities** — the nameplate, as `ElectricalConnection` characteristics.
//! 7. **Internal data** — component temperature.
//!
//! A photovoltaic or hybrid inverter usually also plays
//! [MPS](crate::usecases::mps) for its strings and [MOB](crate::usecases::mob) for its
//! battery, each on its own sub-entity. Everything here is built on
//! [`monitoring`](crate::usecases::monitoring), the same machinery MPC and MGCP use.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::moi;
//! use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
//!
//! let mut inverter = moi::monitored_unit(1)
//!     .with(Measurand::total(Quantity::ApparentPower))
//!     .with(Measurand::unphased(Quantity::YieldDay));
//! inverter.set(&Measurand::total(Quantity::ApparentPower), 4_100.0, Duration::ZERO);
//! inverter.set(&Measurand::unphased(Quantity::YieldDay), 18_400.0, Duration::ZERO);
//!
//! let mut readings = Readings::new();
//! readings.describe(&inverter.measurement_descriptions());
//! readings.describe(&inverter.parameter_descriptions());
//! readings.apply(&inverter.measurements());
//!
//! assert_eq!(readings.value(&Measurand::unphased(Quantity::YieldDay)), Some(18_400.0));
//! ```

use crate::model::EntityType;
use crate::usecases::descriptor::{ActorRole, Scenario, Support, UseCaseDescriptor, actors};
use crate::usecases::monitoring::functions::*;
use crate::usecases::monitoring::{MonitoredUnit, Naming};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` on the wire.
pub const NAME: &str = "monitoringOfInverter";

/// The actor an inverter announces itself as.
pub const INVERTER_ACTOR: &str = "Inverter";

/// A [`MonitoredUnit`] set up to publish an inverter's measurements.
pub fn monitored_unit(connection: u32) -> MonitoredUnit {
    MonitoredUnit::new(connection).naming(Naming::Appliance)
}

const INVERTER_ENTITIES: &[EntityType] = &[EntityType::Inverter];
const APPLIANCE_ENTITIES: &[EntityType] = &[];

const NAMES: [&str; 7] = [
    "Monitor Inverter identification",
    "Monitor Inverter state",
    "Monitor Inverter AC power details",
    "Monitor Inverter AC energy details",
    "Monitor Inverter AC additional details",
    "Monitor Inverter capabilities",
    "Monitor Inverter internal data",
];

/// The inverter: the actor that measures itself.
pub static INVERTER: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: INVERTER_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: INVERTER_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: SERVER_IDENTIFICATION,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: SERVER_STATE,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Recommended,
            functions: SERVER_CHARACTERISTICS,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
    ],
};

/// The Monitoring Appliance: the actor that reads.
///
/// Table 1 marks scenarios 3, 5 and 6 `M` here against the inverter's `R`. The same shape
/// as elsewhere in the standard: a reader must understand everything a device may send.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: APPLIANCE_ENTITIES,
    counterpart: INVERTER_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: CLIENT_IDENTIFICATION,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: CLIENT_STATE,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Mandatory,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Mandatory,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Mandatory,
            functions: CLIENT_CHARACTERISTICS,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CmdData, ElectricalConnectionPhaseName as Phase, ScopeType};
    use crate::usecases::monitoring::{Measurand, Quantity};
    use alloc::vec::Vec;

    /// Table 4 and Table 5: apparent and reactive power name their total separately from
    /// a phase, exactly as active power does.
    #[test]
    fn moi_031_to_035_use_the_scopes_the_specification_fixes() {
        let unit = monitored_unit(1)
            .with(Measurand::total(Quantity::ApparentPower))
            .with(Measurand::on(Quantity::ApparentPower, Phase::A))
            .with(Measurand::total(Quantity::ReactivePower))
            .with(Measurand::unphased(Quantity::PowerFactor))
            .with(Measurand::unphased(Quantity::YieldTotal));

        let CmdData::MeasurementDescriptionListData(list) = unit.measurement_descriptions() else {
            unreachable!("built above");
        };
        let scopes: Vec<_> = list
            .measurement_description_data
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|entry| entry.scope_type.clone())
            .collect();
        assert_eq!(
            scopes,
            [
                ScopeType::AcPowerApparentTotal,
                ScopeType::AcPowerApparent,
                ScopeType::AcPowerReactiveTotal,
                ScopeType::AcCosPhi,
                ScopeType::AcYieldTotal,
            ]
        );
    }

    /// §2.5.1: an inverter SHALL also play MPC, which is where the ordinary AC values live.
    /// The two use cases are separate announcements over one connection.
    #[test]
    fn moi_layers_on_top_of_mpc_rather_than_repeating_it() {
        use crate::usecases::mpc;

        assert_ne!(INVERTER.name, mpc::MONITORED_UNIT.name);
        // Nothing in MOI's own scenarios is the plain active power MPC carries.
        let unit = monitored_unit(1).with(Measurand::total_power());
        let CmdData::MeasurementDescriptionListData(list) = unit.measurement_descriptions() else {
            unreachable!("built above");
        };
        assert_eq!(
            list.measurement_description_data.as_ref().unwrap()[0].scope_type,
            Some(ScopeType::AcPowerTotal),
            "which is MPC's, and the same machinery serves it"
        );
    }

    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(INVERTER.use_case_name().as_str(), NAME);
        assert_eq!(INVERTER.use_case_actor().as_str(), "Inverter");
        assert_eq!(INVERTER.version, "1.0.0");

        assert_eq!(
            INVERTER.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            MONITORING_APPLIANCE
                .required_scenarios()
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6]
        );
        // Nothing is written, so nothing binds.
        assert_eq!(MONITORING_APPLIANCE.features_needing_binding().count(), 0);
    }
}
