//! EV Charging Electricity Measurement (EVCEM).
//!
//! What is actually flowing into the car, as opposed to what it was told it may take.
//! Three scenarios, and the split between them is the point:
//!
//! 1. **Measure EV charging current**, per phase, in amperes — `R` for the car and `M`
//!    for the energy manager.
//! 2. **Measure EV charging power**, per phase, in watts.
//! 3. **Measure EV charged energy**, in watt-hours, scope `charge`.
//!
//! It is the counterpart of [`opev`](super::opev) and the reason that use case can be
//! trusted: a manager that limits a car to 6 A and never reads back what it drew is
//! working from a number it has only ever asserted. [OPEV-002] lets a car charge
//! asymmetrically, and only this use case says which phase actually took what.
//!
//! The measurement machinery is [`monitoring`](crate::usecases::monitoring), shared with
//! MPC and MGCP: the same `Measurement` and `ElectricalConnection` description pair, the
//! same value states, the same reader. What EVCEM adds is
//! [`Quantity::EnergyCharged`](crate::usecases::monitoring::Quantity::EnergyCharged) —
//! scope `charge`, this session's energy into the battery, which is a different number
//! from a wallbox's lifetime import that happens to share a unit.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::ElectricalConnectionPhaseName as Phase;
//! use eebus::usecases::emobility::evcem;
//! use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Quantity, Readings};
//!
//! // A car reporting what it is drawing on each phase, and what it has taken so far.
//! let mut car = evcem::monitored_unit(1)
//!     .with(Measurand::on(Quantity::Current, Phase::A))
//!     .with(Measurand::unphased(Quantity::EnergyCharged));
//! car.set(&Measurand::on(Quantity::Current, Phase::A), 15.8, Duration::ZERO);
//! car.set(&Measurand::unphased(Quantity::EnergyCharged), 8_400.0, Duration::ZERO);
//!
//! // What the energy manager makes of it, having read the descriptions first.
//! let mut readings = Readings::new();
//! readings.describe(&car.measurement_descriptions());
//! readings.describe(&car.parameter_descriptions());
//! readings.apply(&car.measurements());
//!
//! assert_eq!(readings.value(&Measurand::on(Quantity::Current, Phase::A)), Some(15.8));
//! assert_eq!(readings.value(&Measurand::unphased(Quantity::EnergyCharged)), Some(8_400.0));
//! ```

use crate::model::{EntityType, FeatureType, Function};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
use crate::usecases::monitoring::{MonitoredUnit, Naming};

use super::{actors, names};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.1";

/// How a car names its measurement scopes for this use case.
pub const NAMING: Naming = Naming::EvCharging;

/// A [`MonitoredUnit`] set up to publish a car's charging measurements.
///
/// `connection` is the `electricalConnectionId`, which SHALL be the same one the car uses
/// for [`opev`](super::opev): the limits and the measurements describe one connection, and
/// a manager that saw two would not know which current belonged to which limit.
pub fn monitored_unit(connection: u32) -> MonitoredUnit {
    MonitoredUnit::new(connection).naming(NAMING)
}

const EV_ENTITIES: &[EntityType] = &[EntityType::EV];
const GUARD_ENTITIES: &[EntityType] = &[EntityType::CEM];

const EV_FUNCTIONS: &[FunctionUse] = &[
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

const GUARD_FUNCTIONS: &[FunctionUse] = &[
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

const NAMES: [&str; 3] = [
    "Measure EV charging current",
    "Measure EV charging power",
    "Measure EV charged energy",
];

/// The car: the actor that measures.
///
/// Table 1 marks scenario 1 `R` and scenarios 2 and 3 `O` for the car — a car that reports
/// its current and nothing else is conforming.
pub static EV: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVCEM,
    actor: actors::EV,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EV_ENTITIES,
    counterpart: actors::ENERGY_GUARD,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Recommended,
            functions: EV_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Optional,
            functions: EV_FUNCTIONS,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Optional,
            functions: EV_FUNCTIONS,
        },
    ],
};

/// The energy manager: the actor that reads.
///
/// All three scenarios are `M` here against the car's `R`/`O`/`O`. That is the same shape
/// as EVCC's scenarios 6 and 7: a manager must understand everything a car may send, and a
/// car need not send all of it.
pub static ENERGY_GUARD: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVCEM,
    actor: actors::ENERGY_GUARD,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: GUARD_ENTITIES,
    counterpart: actors::EV,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: GUARD_FUNCTIONS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Mandatory,
            functions: GUARD_FUNCTIONS,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Mandatory,
            functions: GUARD_FUNCTIONS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CmdData, ElectricalConnectionPhaseName as Phase, MeasurementType, ScopeType,
        UnitOfMeasurement,
    };
    use crate::usecases::monitoring::{Measurand, Quantity};
    use alloc::vec::Vec;
    use core::time::Duration;

    /// Table 6: the three descriptions, each with the scope its scenario fixes.
    #[test]
    fn the_three_scenarios_describe_themselves_as_the_specification_asks() {
        let unit = monitored_unit(1)
            .with(Measurand::on(Quantity::Current, Phase::A))
            .with(Measurand::on(Quantity::Power, Phase::A))
            .with(Measurand::unphased(Quantity::EnergyCharged));

        let CmdData::MeasurementDescriptionListData(list) = unit.measurement_descriptions() else {
            unreachable!("built above");
        };
        let entries = list.measurement_description_data.as_ref().unwrap();
        let scopes: Vec<_> = entries
            .iter()
            .filter_map(|entry| entry.scope_type.clone())
            .collect();
        assert_eq!(
            scopes,
            [ScopeType::AcCurrent, ScopeType::AcPower, ScopeType::Charge]
        );

        let units: Vec<_> = entries.iter().filter_map(|e| e.unit.clone()).collect();
        assert_eq!(
            units,
            [
                UnitOfMeasurement::A,
                UnitOfMeasurement::W,
                UnitOfMeasurement::Wh
            ]
        );
        assert_eq!(
            entries[2].measurement_type,
            Some(MeasurementType::Energy),
            "charged energy is an energy, distinguished by its scope"
        );
    }

    /// [EVCEM-003]: `charge` is this session into the battery, and a wallbox's lifetime
    /// import is a different number with the same unit.
    #[test]
    fn evcem_003_charged_energy_is_not_consumed_energy() {
        assert_eq!(
            Measurand::unphased(Quantity::EnergyCharged).scope_type(),
            ScopeType::Charge
        );
        assert_eq!(
            Measurand::total(Quantity::EnergyConsumed).scope_type(),
            ScopeType::AcEnergyConsumed
        );
        assert_ne!(
            Measurand::unphased(Quantity::EnergyCharged).scope_type(),
            Measurand::total(Quantity::EnergyConsumed).scope_type()
        );
    }

    /// The point of the use case: a manager can check what a car actually drew against
    /// what it was told, per phase.
    #[test]
    fn a_manager_can_read_back_what_each_phase_drew() {
        use crate::usecases::monitoring::Readings;

        let mut car = monitored_unit(1)
            .with(Measurand::on(Quantity::Current, Phase::A))
            .with(Measurand::on(Quantity::Current, Phase::B));
        car.set(
            &Measurand::on(Quantity::Current, Phase::A),
            16.0,
            Duration::ZERO,
        );
        car.set(
            &Measurand::on(Quantity::Current, Phase::B),
            6.0,
            Duration::ZERO,
        );

        let mut readings = Readings::new();
        readings.describe(&car.measurement_descriptions());
        readings.describe(&car.parameter_descriptions());
        readings.apply(&car.measurements());

        assert_eq!(
            readings.value(&Measurand::on(Quantity::Current, Phase::A)),
            Some(16.0)
        );
        assert_eq!(
            readings.value(&Measurand::on(Quantity::Current, Phase::B)),
            Some(6.0),
            "asymmetric charging, which is what [OPEV-002] permits and only this use case shows"
        );
    }

    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(EV.use_case_name().as_str(), names::EVCEM);
        assert_eq!(EV.use_case_actor().as_str(), "EV");
        assert_eq!(ENERGY_GUARD.use_case_actor().as_str(), "EnergyGuard");

        // The car: scenario 1 recommended, 2 and 3 optional.
        assert_eq!(EV.required_scenarios().collect::<Vec<_>>(), [1]);
        // The manager: all three mandatory.
        assert_eq!(
            ENERGY_GUARD.required_scenarios().collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // Nothing is written, so nothing binds.
        assert_eq!(ENERGY_GUARD.features_needing_binding().count(), 0);
    }
}
