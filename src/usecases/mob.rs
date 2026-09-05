//! Monitoring of Battery (MOB).
//!
//! What a stationary battery is doing, measured on its direct-current side. Nine
//! scenarios, and the split between them is by quantity rather than by purpose:
//!
//! 1. **Identification** — manufacturer, model, firmware.
//! 2. **State** — working, standing by, or broken.
//! 3. **DC power** — positive charging, negative discharging.
//! 4. **DC current**, 5. **DC voltage**.
//! 6. **DC energy** — how much has gone in, and how much has come out, as two separate
//!    running totals.
//! 7. **Additional details** — state of charge, state of health, state of energy, load
//!    cycle count.
//! 8. **Capabilities** — the usable capacity, as an `ElectricalConnection` characteristic.
//! 9. **Internal data** — component temperature.
//!
//! A battery lives on a `Battery` entity underneath an `Inverter`, which is usually also
//! playing [MOI](crate::usecases::moi) — the inverter is what the AC side sees, and this
//! is what is behind it.
//!
//! Three of these values are easy to confuse and mean different things:
//!
//! * **`stateOfCharge`** is a percentage: how full.
//! * **`stateOfEnergy`** is watt-hours: how much is in there.
//! * **`useableCapacity`** is watt-hours: how much it could hold, after ageing.
//!
//! A client that read `stateOfEnergy` as a percentage would see a 12 kWh battery as 12 000
//! per cent full, which is why they are separate quantities here rather than one number
//! with a unit attached.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::mob;
//! use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
//!
//! let mut battery = mob::monitored_unit(1)
//!     .with(Measurand::unphased(Quantity::DcPower))
//!     .with(Measurand::unphased(Quantity::StateOfCharge));
//! // Discharging: the sign says which way it is going.
//! battery.set(&Measurand::unphased(Quantity::DcPower), -3_200.0);
//! battery.set(&Measurand::unphased(Quantity::StateOfCharge), 61.0);
//!
//! let mut readings = Readings::new();
//! readings.describe(&battery.measurement_descriptions());
//! readings.describe(&battery.parameter_descriptions());
//! readings.apply(&battery.measurements());
//!
//! assert_eq!(readings.value(&Measurand::unphased(Quantity::DcPower)), Some(-3_200.0));
//! ```

use crate::model::EntityType;
use crate::usecases::descriptor::{ActorRole, Scenario, Support, UseCaseDescriptor, actors};
use crate::usecases::monitoring::functions::*;
use crate::usecases::monitoring::{MonitoredUnit, Naming};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` on the wire.
pub const NAME: &str = "monitoringOfBattery";

/// The actor a battery announces itself as.
pub const BATTERY_ACTOR: &str = "Battery";

/// A [`MonitoredUnit`] set up to publish a battery's measurements.
pub fn monitored_unit(connection: u32) -> MonitoredUnit {
    MonitoredUnit::new(connection).naming(Naming::Appliance)
}

const BATTERY_ENTITIES: &[EntityType] = &[EntityType::Battery];
const APPLIANCE_ENTITIES: &[EntityType] = &[];

const NAMES: [&str; 9] = [
    "Monitor Battery identification",
    "Monitor Battery state",
    "Monitor Battery DC power",
    "Monitor Battery DC current",
    "Monitor Battery DC voltage",
    "Monitor Battery DC energy",
    "Monitor Battery additional details",
    "Monitor Battery capabilities",
    "Monitor Battery internal data",
];

/// The battery: the actor that measures itself.
pub static BATTERY: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: BATTERY_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: BATTERY_ENTITIES,
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
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 6,
            name: NAMES[5],
            support: Support::Mandatory,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Recommended,
            functions: SERVER_MEASUREMENTS,
        },
        Scenario {
            number: 8,
            name: NAMES[7],
            support: Support::Recommended,
            functions: SERVER_CHARACTERISTICS,
        },
        Scenario {
            number: 9,
            name: NAMES[8],
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
    counterpart: BATTERY_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Recommended,
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
            support: Support::Recommended,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 7,
            name: NAMES[6],
            support: Support::Recommended,
            functions: CLIENT_MEASUREMENTS,
        },
        Scenario {
            number: 8,
            name: NAMES[7],
            support: Support::Mandatory,
            functions: CLIENT_CHARACTERISTICS,
        },
        Scenario {
            number: 9,
            name: NAMES[8],
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

    /// The three values a client must not confuse, and the units that tell them apart.
    #[test]
    fn mob_072_and_081_state_of_energy_is_not_a_percentage() {
        let charge = Measurand::unphased(Quantity::StateOfCharge);
        let energy = Measurand::unphased(Quantity::StateOfEnergy);
        let capacity = Measurand::unphased(Quantity::UsableCapacity);

        assert_eq!(charge.unit(), UnitOfMeasurement::Pct);
        assert_eq!(energy.unit(), UnitOfMeasurement::Wh);
        assert_eq!(capacity.unit(), UnitOfMeasurement::Wh);

        assert_eq!(charge.measurement_type(), MeasurementType::Percentage);
        assert_eq!(energy.measurement_type(), MeasurementType::Energy);
        assert_eq!(
            capacity.measurement_type(),
            MeasurementType::Capacity,
            "a capacity is its own measurement type, not an energy"
        );

        assert_ne!(charge.scope_type(), energy.scope_type());
    }

    /// [MOB-061] and [MOB-062]: two running totals, not one signed number. A battery that
    /// reported net energy would lose the losses.
    #[test]
    fn mob_061_charge_and_discharge_energy_are_two_separate_totals() {
        let unit = monitored_unit(1)
            .with(Measurand::unphased(Quantity::DcChargeEnergy))
            .with(Measurand::unphased(Quantity::DcDischargeEnergy));

        let CmdData::MeasurementDescriptionListData(list) = unit.measurement_descriptions() else {
            unreachable!("built above");
        };
        let scopes: Vec<_> = list
            .measurement_description_data
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|e| e.scope_type.clone())
            .collect();
        assert_eq!(
            scopes,
            [ScopeType::DcChargeEnergy, ScopeType::DcDischargeEnergy]
        );
    }

    /// A direct-current measurement names no phases, and omits `acMeasuredPhases` rather
    /// than defaulting it to `abc`.
    #[test]
    fn a_dc_measurement_publishes_no_phases() {
        let unit = monitored_unit(1).with(Measurand::unphased(Quantity::DcPower));
        let CmdData::ElectricalConnectionParameterDescriptionListData(list) =
            unit.parameter_descriptions()
        else {
            unreachable!("built above");
        };
        let entry = &list
            .electrical_connection_parameter_description_data
            .as_ref()
            .unwrap()[0];
        assert_eq!(
            entry.ac_measured_phases, None,
            "a client waiting for phases on a DC measurement would wait forever"
        );
        assert_eq!(entry.scope_type, Some(ScopeType::DcPower));
    }

    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(BATTERY.use_case_name().as_str(), NAME);
        assert_eq!(BATTERY.use_case_actor().as_str(), "Battery");
        assert_eq!(
            BATTERY.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            MONITORING_APPLIANCE
                .required_scenarios()
                .collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(MONITORING_APPLIANCE.features_needing_binding().count(), 0);
    }
}
