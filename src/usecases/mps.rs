//! Monitoring of PV String (MPS).
//!
//! What one photovoltaic string is producing, measured on its direct-current side —
//! before the inverter, and therefore per string rather than per building. Six scenarios:
//!
//! 1. **DC power**, 2. **DC current**, 3. **DC voltage**, 4. **DC energy**.
//! 5. **Additional details** — insulation resistance, which is a safety measurement rather
//!    than a yield one.
//! 6. **Internal data** — component temperature.
//!
//! A string lives on a `PVString` entity underneath an `Inverter`, and an installation with
//! several strings has several of them: the point of this use case rather than
//! [MGCP](crate::usecases::mgcp) is that a shaded or failing string is invisible in the
//! building's total, and visible here.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::mps;
//! use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
//!
//! // Two strings on one inverter, each its own electrical connection.
//! let mut south = mps::monitored_unit(1).with(Measurand::unphased(Quantity::DcPower));
//! south.set(&Measurand::unphased(Quantity::DcPower), 3_900.0, Duration::ZERO);
//!
//! let mut readings = Readings::new();
//! readings.describe(&south.measurement_descriptions());
//! readings.describe(&south.parameter_descriptions());
//! readings.apply(&south.measurements());
//!
//! assert_eq!(readings.value(&Measurand::unphased(Quantity::DcPower)), Some(3_900.0));
//! ```

use crate::model::EntityType;
use crate::usecases::descriptor::{ActorRole, Scenario, Support, UseCaseDescriptor, actors};
use crate::usecases::monitoring::functions::*;
use crate::usecases::monitoring::{MonitoredUnit, Naming};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` on the wire.
pub const NAME: &str = "monitoringOfPvString";

/// The actor a string announces itself as.
pub const PV_STRING_ACTOR: &str = "PVString";

/// A [`MonitoredUnit`] set up to publish one string's measurements.
///
/// `connection` is the `electricalConnectionId`, and an inverter with several strings gives
/// each its own: the identifiers are how a client tells the south roof from the east one.
pub fn monitored_unit(connection: u32) -> MonitoredUnit {
    MonitoredUnit::new(connection).naming(Naming::Appliance)
}

const PV_STRING_ENTITIES: &[EntityType] = &[EntityType::PVString];
const APPLIANCE_ENTITIES: &[EntityType] = &[];

const NAMES: [&str; 6] = [
    "Monitor PV String DC power",
    "Monitor PV String DC current",
    "Monitor PV String DC voltage",
    "Monitor PV String DC energy",
    "Monitor PV String additional details",
    "Monitor PV String internal data",
];

/// The string: the actor that measures itself.
pub static PV_STRING: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: PV_STRING_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: PV_STRING_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Optional,
            functions: SERVER_MEASUREMENTS,
        },
    ],
};

/// The Monitoring Appliance: the actor that reads.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: APPLIANCE_ENTITIES,
    counterpart: PV_STRING_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: CLIENT_MEASUREMENTS,
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
            support: Support::Recommended,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 5,
            name: NAMES[4],
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Optional,
            functions: CLIENT_MEASUREMENTS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CmdData, MeasurementType, ScopeType, UnitOfMeasurement};
    use crate::usecases::monitoring::{Measurand, Quantity};
    use alloc::vec::Vec;

    /// Table 6: the four DC scopes, plus the two that are not about yield at all.
    #[test]
    fn the_six_scenarios_use_the_scopes_the_specification_fixes() {
        let unit = monitored_unit(1)
            .with(Measurand::unphased(Quantity::DcPower))
            .with(Measurand::unphased(Quantity::DcCurrent))
            .with(Measurand::unphased(Quantity::DcVoltage))
            .with(Measurand::unphased(Quantity::DcEnergy))
            .with(Measurand::unphased(Quantity::InsulationResistance))
            .with(Measurand::unphased(Quantity::Temperature));

        let CmdData::MeasurementDescriptionListData(list) = unit.measurement_descriptions() else {
            unreachable!("built above");
        };
        let entries = list.measurement_description_data.as_ref().unwrap();
        let scopes: Vec<_> = entries
            .iter()
            .filter_map(|e| e.scope_type.clone())
            .collect();
        assert_eq!(
            scopes,
            [
                ScopeType::DcPower,
                ScopeType::DcCurrent,
                ScopeType::DcVoltage,
                ScopeType::DcEnergy,
                ScopeType::InsulationResistance,
                ScopeType::ComponentTemperature,
            ]
        );

        let units: Vec<_> = entries.iter().filter_map(|e| e.unit.clone()).collect();
        assert_eq!(
            units,
            [
                UnitOfMeasurement::W,
                UnitOfMeasurement::A,
                UnitOfMeasurement::V,
                UnitOfMeasurement::Wh,
                UnitOfMeasurement::Ohm,
                UnitOfMeasurement::DegC,
            ]
        );
        assert_eq!(
            entries[4].measurement_type,
            Some(MeasurementType::Resistance),
            "[MPS-051] is a safety measurement, not a yield one"
        );
    }

    /// The point of the use case: several strings on one inverter, told apart by their
    /// electrical connection.
    #[test]
    fn several_strings_are_told_apart_by_their_connection() {
        let south = monitored_unit(1).with(Measurand::unphased(Quantity::DcPower));
        let east = monitored_unit(2).with(Measurand::unphased(Quantity::DcPower));

        let CmdData::ElectricalConnectionDescriptionListData(south_list) =
            south.connection_description()
        else {
            unreachable!("built above");
        };
        let CmdData::ElectricalConnectionDescriptionListData(east_list) =
            east.connection_description()
        else {
            unreachable!("built above");
        };
        assert_ne!(
            south_list
                .electrical_connection_description_data
                .as_ref()
                .unwrap()[0]
                .electrical_connection_id,
            east_list
                .electrical_connection_description_data
                .as_ref()
                .unwrap()[0]
                .electrical_connection_id,
            "a shaded string is invisible in the building's total and visible here"
        );
    }

    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(PV_STRING.use_case_name().as_str(), NAME);
        assert_eq!(PV_STRING.use_case_actor().as_str(), "PVString");
        assert_eq!(
            PV_STRING.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(
            MONITORING_APPLIANCE
                .required_scenarios()
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
    }
}
