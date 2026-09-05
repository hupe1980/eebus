//! EV State of Charge (EVSOC).
//!
//! How full the battery is, and three things that put that number in context. Four
//! scenarios, one mandatory and three optional:
//!
//! 1. **Monitor EV state of charge** — how much of the usable capacity is charged, as a
//!    percentage ([EVSOC-001]).
//! 2. **Monitor EV nominal capacity** — the battery's usable size in watt-hours
//!    ([EVSOC-002]). Not a measurement: a *characteristic* on the `ElectricalConnection`
//!    feature, `energyCapacityNominalMax`, because it does not change while the car is
//!    plugged in.
//! 3. **Monitor EV state of health** — how much of the original capacity is left, as a
//!    percentage ([EVSOC-003]).
//! 4. **Monitor EV actual travel range** — in **metres** ([EVSOC-004]).
//!
//! Scenarios 1 and 2 are worth having together and nearly useless apart. A percentage says
//! nothing about how long charging will take; a capacity says nothing about how much is
//! needed. [`Battery::energy_to_full`] is the pair, in watt-hours, which is the number an
//! energy manager actually plans with — and it answers [`None`] rather than guessing when
//! only one of the two has arrived.
//!
//! The whole use case is available only over a data link: a car on IEC 61851 has a pilot
//! wire and cannot answer any of it. [`evcc::EvProfile::supports_data_exchange`] is what
//! says so before the first read goes out.
//!
//! ```
//! use core::time::Duration;
//! use eebus::usecases::emobility::evsoc::{self, Battery};
//! use eebus::usecases::monitoring::{Measurand, Quantity, Readings};
//!
//! // A car that is 40 % full, on a 77 kWh battery.
//! let mut car = evsoc::monitored_unit(1).with(Measurand::unphased(Quantity::StateOfCharge));
//! car.set(&Measurand::unphased(Quantity::StateOfCharge), 40.0);
//!
//! let mut readings = Readings::new();
//! readings.describe(&car.measurement_descriptions());
//! readings.describe(&car.parameter_descriptions());
//! readings.apply(&car.measurements());
//!
//! let mut battery = Battery::new();
//! battery.apply(&car.measurements(), &readings);
//! battery.apply(&evsoc::nominal_capacity(77_000.0), &readings);
//!
//! assert_eq!(battery.state_of_charge, Some(40.0));
//! assert_eq!(battery.energy_to_full(), Some(46_200.0));
//! ```
//!
//! [`evcc::EvProfile::supports_data_exchange`]: super::evcc::EvProfile::supports_data_exchange

use alloc::vec;

use crate::model::{
    CmdData, ElectricalConnectionCharacteristicContext, ElectricalConnectionCharacteristicData,
    ElectricalConnectionCharacteristicId, ElectricalConnectionCharacteristicListData,
    ElectricalConnectionCharacteristicType, ElectricalConnectionId,
    ElectricalConnectionParameterId, EntityType, FeatureType, Function, Role, ScaledNumber,
    UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};
use crate::usecases::monitoring::{Measurand, MonitoredUnit, Naming, Quantity, Readings};

use super::{actors, names};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// How a car names its measurement scopes for this use case.
pub const NAMING: Naming = Naming::EvBattery;

/// The `electricalConnectionId` scenario 2's capacity is published under.
pub const ELECTRICAL_CONNECTION: ElectricalConnectionId = ElectricalConnectionId(1);

/// The `parameterId` of that characteristic.
pub const CAPACITY_PARAMETER: ElectricalConnectionParameterId = ElectricalConnectionParameterId(1);

/// The `characteristicId` of the nominal capacity.
pub const CAPACITY_CHARACTERISTIC: ElectricalConnectionCharacteristicId =
    ElectricalConnectionCharacteristicId(1);

/// A [`MonitoredUnit`] set up to publish a car's battery measurements.
///
/// Scenarios 1, 3 and 4 only; scenario 2 is a characteristic rather than a measurement,
/// and [`nominal_capacity`] builds it.
pub fn monitored_unit(connection: u32) -> MonitoredUnit {
    MonitoredUnit::new(connection).naming(NAMING)
}

/// Scenario 2's nominal capacity, in watt-hours (Table 10).
///
/// `energyCapacityNominalMax` on the `ElectricalConnection` feature rather than a
/// measurement, because the size of a battery is a property of the car and not something
/// read off it moment by moment.
pub fn nominal_capacity(watt_hours: f64) -> CmdData {
    CmdData::ElectricalConnectionCharacteristicListData(
        ElectricalConnectionCharacteristicListData {
            electrical_connection_characteristic_data: Some(vec![
                ElectricalConnectionCharacteristicData {
                    electrical_connection_id: Some(ELECTRICAL_CONNECTION),
                    parameter_id: Some(CAPACITY_PARAMETER),
                    characteristic_id: Some(CAPACITY_CHARACTERISTIC),
                    characteristic_context: Some(ElectricalConnectionCharacteristicContext::Entity),
                    characteristic_type: Some(
                        ElectricalConnectionCharacteristicType::EnergyCapacityNominalMax,
                    ),
                    value: Some(ScaledNumber::from_f64(watt_hours, 0)),
                    unit: Some(UnitOfMeasurement::Wh),
                },
            ]),
        },
    )
}

/// Reads the nominal capacity out of an `electricalConnectionCharacteristicListData`.
pub fn read_nominal_capacity(data: &CmdData) -> Option<f64> {
    let CmdData::ElectricalConnectionCharacteristicListData(list) = data else {
        return None;
    };
    list.electrical_connection_characteristic_data
        .iter()
        .flatten()
        .find(|entry| {
            entry.characteristic_type.as_ref()
                == Some(&ElectricalConnectionCharacteristicType::EnergyCapacityNominalMax)
        })
        .and_then(|entry| entry.value.as_ref())
        .and_then(ScaledNumber::to_f64)
}

/// Builds the `ElectricalConnection` feature scenario 2 is served from.
pub fn characteristic_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::ElectricalConnection, Role::Server)
        .with_function(
            Function::ElectricalConnectionDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::ElectricalConnectionParameterDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::ElectricalConnectionCharacteristicListData,
            Operations::read(),
        )
}

/// What an energy manager has learned about one car's battery.
///
/// The four scenarios collected into the shape a planner wants, with every field optional
/// because three of the four are `O` in Table 1 — a car that reports only its state of
/// charge is conforming, and a manager that required more would refuse to work with it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Battery {
    /// Scenario 1: how much of the usable capacity is charged, 0 to 100.
    pub state_of_charge: Option<f64>,
    /// Scenario 2: the usable capacity, in watt-hours.
    pub nominal_capacity: Option<f64>,
    /// Scenario 3: how much of the original capacity is left, 0 to 100.
    pub state_of_health: Option<f64>,
    /// Scenario 4: how far the car can still travel, in **metres**.
    pub travel_range: Option<f64>,
}

impl Battery {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes in whatever the car has published, whichever scenario it belongs to.
    ///
    /// `readings` is the manager's [`Readings`], which is what turns a `measurementId`
    /// back into a measurand — the descriptions have to have arrived first, which is
    /// exactly the pre-scenario communication the specification asks for. Returns whether
    /// anything changed.
    pub fn apply(&mut self, data: &CmdData, readings: &Readings) -> bool {
        let before = *self;
        match data {
            CmdData::MeasurementListData(_) => {
                let mut updated = *self;
                let mut probe = readings.clone();
                probe.apply(data);
                updated.state_of_charge =
                    probe.value(&Measurand::unphased(Quantity::StateOfCharge));
                updated.state_of_health =
                    probe.value(&Measurand::unphased(Quantity::StateOfHealth));
                updated.travel_range = probe.value(&Measurand::unphased(Quantity::TravelRange));
                // Only replace what actually arrived: a partial notification carrying the
                // state of charge must not erase a state of health read a minute ago.
                self.state_of_charge = updated.state_of_charge.or(self.state_of_charge);
                self.state_of_health = updated.state_of_health.or(self.state_of_health);
                self.travel_range = updated.travel_range.or(self.travel_range);
            }
            CmdData::ElectricalConnectionCharacteristicListData(_) => {
                if let Some(capacity) = read_nominal_capacity(data) {
                    self.nominal_capacity = Some(capacity);
                }
            }
            _ => {}
        }
        *self != before
    }

    /// The energy still needed to fill the battery, in watt-hours.
    ///
    /// The number a planner actually wants, and the reason scenarios 1 and 2 are worth
    /// having together: a percentage says nothing about how long charging will take, and a
    /// capacity says nothing about how much is needed. [`None`] when either half is
    /// missing — a manager that assumed a capacity would plan a charge for a battery it
    /// invented.
    pub fn energy_to_full(&self) -> Option<f64> {
        let charge = self.state_of_charge?;
        let capacity = self.nominal_capacity?;
        Some((100.0 - charge.clamp(0.0, 100.0)) / 100.0 * capacity)
    }

    /// The energy currently in the battery, in watt-hours.
    pub fn energy_stored(&self) -> Option<f64> {
        let charge = self.state_of_charge?;
        let capacity = self.nominal_capacity?;
        Some(charge.clamp(0.0, 100.0) / 100.0 * capacity)
    }

    /// The capacity the battery has actually retained, in watt-hours.
    ///
    /// Nominal capacity times state of health: an eight-year-old car's nameplate is not
    /// what it will take. [`None`] unless both scenarios 2 and 3 are supported.
    pub fn usable_capacity(&self) -> Option<f64> {
        let capacity = self.nominal_capacity?;
        let health = self.state_of_health?;
        Some(health.clamp(0.0, 100.0) / 100.0 * capacity)
    }
}

// ---- descriptors -------------------------------------------------------------------

const EV_ENTITIES: &[EntityType] = &[EntityType::EV];
const APPLIANCE_ENTITIES: &[EntityType] = &[EntityType::CEM];

const EV_MEASUREMENTS: &[FunctionUse] = &[
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
    FunctionUse::server(FeatureType::Measurement, Function::MeasurementListData),
];

const EV_CAPACITY: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionCharacteristicListData,
    ),
];

const APPLIANCE_MEASUREMENTS: &[FunctionUse] = &[
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
    FunctionUse::client(FeatureType::Measurement, Function::MeasurementListData),
];

const APPLIANCE_CAPACITY: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionParameterDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::ElectricalConnection,
        Function::ElectricalConnectionCharacteristicListData,
    ),
];

const NAMES: [&str; 4] = [
    "Monitor EV state of charge",
    "Monitor EV nominal capacity",
    "Monitor EV state of health",
    "Monitor EV actual travel range",
];

/// The car: the actor that reports its battery.
pub static EV: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVSOC,
    actor: actors::EV,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: EV_ENTITIES,
    counterpart: actors::MONITORING_APPLIANCE,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: EV_MEASUREMENTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Optional,
            functions: EV_CAPACITY,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Optional,
            functions: EV_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Optional,
            functions: EV_MEASUREMENTS,
        },
    ],
};

/// The Monitoring Appliance: the actor that reads the battery.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: names::EVSOC,
    actor: actors::MONITORING_APPLIANCE,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: APPLIANCE_ENTITIES,
    counterpart: actors::EV,
    scenarios: &[
        Scenario {
            number: 1,
            name: NAMES[0],
            support: Support::Mandatory,
            functions: APPLIANCE_MEASUREMENTS,
        },
        Scenario {
            number: 2,
            name: NAMES[1],
            support: Support::Optional,
            functions: APPLIANCE_CAPACITY,
        },
        Scenario {
            number: 3,
            name: NAMES[2],
            support: Support::Optional,
            functions: APPLIANCE_MEASUREMENTS,
        },
        Scenario {
            number: 4,
            name: NAMES[3],
            support: Support::Optional,
            functions: APPLIANCE_MEASUREMENTS,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ScopeType;
    use alloc::vec::Vec;

    fn a_car() -> (MonitoredUnit, Readings) {
        let mut unit = monitored_unit(1)
            .with(Measurand::unphased(Quantity::StateOfCharge))
            .with(Measurand::unphased(Quantity::StateOfHealth))
            .with(Measurand::unphased(Quantity::TravelRange));
        unit.set(&Measurand::unphased(Quantity::StateOfCharge), 40.0);
        unit.set(&Measurand::unphased(Quantity::StateOfHealth), 92.0);
        unit.set(&Measurand::unphased(Quantity::TravelRange), 180_000.0);

        let mut readings = Readings::new();
        readings.describe(&unit.measurement_descriptions());
        readings.describe(&unit.parameter_descriptions());
        (unit, readings)
    }

    /// Table 8: the three measured scenarios, each with the scope and unit it fixes.
    #[test]
    fn the_measured_scenarios_use_the_scopes_the_specification_fixes() {
        let (unit, _) = a_car();
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
                ScopeType::StateOfCharge,
                ScopeType::StateOfHealth,
                ScopeType::TravelRange
            ]
        );

        let units: Vec<_> = entries.iter().filter_map(|e| e.unit.clone()).collect();
        assert_eq!(
            units,
            [
                UnitOfMeasurement::Pct,
                UnitOfMeasurement::Pct,
                UnitOfMeasurement::M
            ],
            "[EVSOC-004] is in metres, which is not what a dashboard shows"
        );
    }

    /// Scenario 2 round-trips through the characteristic, not a measurement.
    #[test]
    fn evsoc_002_the_capacity_is_a_characteristic_and_not_a_measurement() {
        let published = nominal_capacity(77_000.0);
        assert_eq!(read_nominal_capacity(&published), Some(77_000.0));

        let CmdData::ElectricalConnectionCharacteristicListData(list) = &published else {
            unreachable!("built above");
        };
        let entry = &list
            .electrical_connection_characteristic_data
            .as_ref()
            .unwrap()[0];
        assert_eq!(
            entry.characteristic_type.as_ref().map(|t| t.as_str()),
            Some("energyCapacityNominalMax")
        );
        assert_eq!(entry.unit, Some(UnitOfMeasurement::Wh));
    }

    /// The pair that makes the use case worth having, and the honest answer when only half
    /// of it has arrived.
    #[test]
    fn evsoc_001_and_002_together_give_the_number_a_planner_wants() {
        let (unit, readings) = a_car();
        let mut battery = Battery::new();

        battery.apply(&unit.measurements(), &readings);
        assert_eq!(battery.state_of_charge, Some(40.0));
        assert_eq!(
            battery.energy_to_full(),
            None,
            "a percentage alone plans nothing"
        );

        battery.apply(&nominal_capacity(77_000.0), &readings);
        assert_eq!(battery.energy_to_full(), Some(46_200.0));
        assert_eq!(battery.energy_stored(), Some(30_800.0));
        assert_eq!(
            battery.usable_capacity(),
            Some(70_840.0),
            "[EVSOC-003]: an eight-year-old car's nameplate is not what it will take"
        );
    }

    /// A partial notification carrying one measurand must not erase another read earlier.
    #[test]
    fn a_partial_notification_does_not_erase_what_it_does_not_carry() {
        let (unit, readings) = a_car();
        let mut battery = Battery::new();
        battery.apply(&unit.measurements(), &readings);
        assert_eq!(battery.state_of_health, Some(92.0));

        // A notification carrying only the state of charge.
        let mut only_charge = monitored_unit(1).with(Measurand::unphased(Quantity::StateOfCharge));
        only_charge.set(&Measurand::unphased(Quantity::StateOfCharge), 55.0);
        battery.apply(&only_charge.measurements(), &readings);

        assert_eq!(battery.state_of_charge, Some(55.0));
        assert_eq!(battery.state_of_health, Some(92.0), "still known");
        assert_eq!(battery.travel_range, Some(180_000.0), "still known");
    }

    #[test]
    fn the_descriptors_say_what_table_1_says() {
        assert_eq!(EV.use_case_name().as_str(), names::EVSOC);
        assert_eq!(EV.version, "1.0.0");
        assert_eq!(
            MONITORING_APPLIANCE.use_case_actor().as_str(),
            "MonitoringAppliance"
        );

        // Only scenario 1 is mandatory, for both actors.
        assert_eq!(EV.required_scenarios().collect::<Vec<_>>(), [1]);
        assert_eq!(
            MONITORING_APPLIANCE
                .required_scenarios()
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(EV.all_scenarios().collect::<Vec<_>>(), [1, 2, 3, 4]);
    }
}
