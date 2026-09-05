//! What a Monitored Unit measures, and how the specification says to describe it.
//!
//! A measurement in SPINE is described twice over: once on the `Measurement` feature —
//! what quantity it is, in what unit — and once on the `ElectricalConnection` feature,
//! which binds it to the phases it was taken on. A client that reads only the first
//! cannot tell the current on phase A from the current on phase B, so both descriptions
//! have to agree, and getting them to agree by hand is where implementations go wrong.
//!
//! [`Measurand`] is the pair as one value. Everything the two descriptions need is
//! derived from it, which is why they cannot disagree.

use crate::model::{
    CommodityType, ElectricalConnectionPhaseName, MeasurementType, ScopeType, UnitOfMeasurement,
};

/// A quantity, without the phases it applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Quantity {
    /// Momentary active power, in watts.
    ///
    /// The total is scenario 1 of MPC ([MPC-011]) and scenario 2 of MGCP ([MGCP-021]);
    /// a per-phase value is [MPC-012].
    Power,
    /// Energy drawn from the grid, in watt-hours ([MPC-021], [MGCP-041]).
    EnergyConsumed,
    /// Energy fed into the grid, in watt-hours ([MPC-022], [MGCP-031]).
    EnergyProduced,
    /// Momentary current, in amperes ([MPC-031], [MGCP-051]).
    Current,
    /// Voltage, in volts ([MPC-041], [MGCP-061]).
    Voltage,
    /// Grid frequency, in hertz ([MPC-051], [MGCP-071]).
    Frequency,
    /// Energy delivered into a car in this charging session, in watt-hours ([EVCEM-003]).
    ///
    /// `charge`, not `acEnergyConsumed`: what matters to a car is what went *into the
    /// battery* this session, and a wallbox's lifetime import is a different number that
    /// happens to share a unit.
    EnergyCharged,
    /// How much of a battery's usable capacity is charged, as a percentage ([EVSOC-001]).
    StateOfCharge,
    /// How much of a battery's original capacity it still has, as a percentage
    /// ([EVSOC-003]).
    StateOfHealth,
    /// How far the car can still travel, in **metres** ([EVSOC-004]).
    ///
    /// Metres, which is what the specification fixes and not what a dashboard shows. A
    /// client that read it as kilometres would offer to charge a car that can already get
    /// home a thousand times over.
    TravelRange,

    // ---- what an inverter adds on top of MPC (MOI) -----------------------------
    /// Apparent power, in volt-amperes ([MOI-031], [MOI-032]).
    ApparentPower,
    /// Reactive power, in volt-amperes reactive ([MOI-033], [MOI-034]).
    ReactivePower,
    /// The power factor, dimensionless ([MOI-035]).
    PowerFactor,
    /// Energy produced today, in watt-hours ([MOI-041]).
    YieldDay,
    /// Energy produced this month.
    YieldMonth,
    /// Energy produced this year.
    YieldYear,
    /// Energy produced since the device was installed.
    YieldTotal,
    /// A component's temperature, in degrees Celsius.
    ///
    /// `componentTemperature` — an inverter's heatsink, say. Not the hot water: see
    /// [`DhwTemperature`](Self::DhwTemperature), which is a different scope on a different
    /// commodity and mixing the two would have a manager reading a heatsink as a tank.
    Temperature,
    /// The temperature of the domestic hot water, in degrees Celsius ([MDT-001]).
    ///
    /// `dhwTemperature` on `commodityType: domesticHotWater`, which is one of the three
    /// measurements in this crate that are not electricity. Comparing it against the
    /// setpoint [`cdt`](crate::usecases::hvac::cdt) wrote is how the energy demand for hot
    /// water is estimated, and how a manager finds out whether a temperature it asked for
    /// was actually reached.
    DhwTemperature,
    /// The air temperature of an indoor space, in degrees Celsius ([MRT-001]).
    ///
    /// `roomAirTemperature` on `commodityType: air`. The measurement a building's thermal
    /// model is fitted against: indoor temperature, outdoor temperature and delivered heat
    /// are what an `RC` model of a house is identified from, and this is the first of the
    /// three. See [`hvac::mrt`](crate::usecases::hvac::mrt).
    RoomTemperature,
    /// The outdoor air temperature, in degrees Celsius ([MOT-001]).
    ///
    /// `outsideAirTemperature` on `commodityType: air`, and the second input of that same
    /// model. A heat pump measures it anyway — it is what its own defrost and heating
    /// curve run on — so a building rarely needs a sensor of its own for it. See
    /// [`hvac::mot`](crate::usecases::hvac::mot).
    OutdoorTemperature,

    // ---- the direct-current side (MOB, MPS, MOI) --------------------------------
    /// Direct-current power, in watts.
    DcPower,
    /// Direct current, in amperes.
    DcCurrent,
    /// Direct-current voltage, in volts.
    DcVoltage,
    /// Energy on the direct-current side, in watt-hours ([MPS-041]).
    DcEnergy,
    /// Energy that has gone *into* a battery, in watt-hours ([MOB-061]).
    DcChargeEnergy,
    /// Energy that has come *out of* a battery, in watt-hours ([MOB-062]).
    DcDischargeEnergy,
    /// The energy currently stored, in watt-hours ([MOB-072]).
    ///
    /// `energy` and watt-hours, not a percentage: `stateOfEnergy` is how much is in there,
    /// where [`StateOfCharge`](Self::StateOfCharge) is what fraction of the capacity that
    /// is. A client that mixed them up would read 12 000 as a percentage.
    StateOfEnergy,
    /// The capacity a battery has actually retained, in watt-hours ([MOB-081]).
    UsableCapacity,
    /// How many charge/discharge cycles a battery has been through ([MOB-073]).
    LoadCycleCount,
    /// Insulation resistance, in ohms ([MPS-051]).
    InsulationResistance,
}

impl Quantity {
    /// The `measurementType` of the description.
    pub const fn measurement_type(self) -> MeasurementType {
        match self {
            Self::Power => MeasurementType::Power,
            Self::EnergyConsumed | Self::EnergyProduced => MeasurementType::Energy,
            Self::Current => MeasurementType::Current,
            Self::Voltage => MeasurementType::Voltage,
            Self::Frequency => MeasurementType::Frequency,
            Self::EnergyCharged => MeasurementType::Energy,
            Self::StateOfCharge | Self::StateOfHealth => MeasurementType::Percentage,
            Self::TravelRange => MeasurementType::Distance,
            Self::ApparentPower | Self::ReactivePower | Self::DcPower => MeasurementType::Power,
            Self::PowerFactor => MeasurementType::PowerFactor,
            Self::YieldDay
            | Self::YieldMonth
            | Self::YieldYear
            | Self::YieldTotal
            | Self::DcEnergy
            | Self::DcChargeEnergy
            | Self::DcDischargeEnergy
            | Self::StateOfEnergy => MeasurementType::Energy,
            Self::Temperature
            | Self::DhwTemperature
            | Self::RoomTemperature
            | Self::OutdoorTemperature => MeasurementType::Temperature,
            Self::DcCurrent => MeasurementType::Current,
            Self::DcVoltage => MeasurementType::Voltage,
            Self::UsableCapacity => MeasurementType::Capacity,
            Self::LoadCycleCount => MeasurementType::Count,
            Self::InsulationResistance => MeasurementType::Resistance,
        }
    }

    /// The `unit` of the description.
    pub const fn unit(self) -> UnitOfMeasurement {
        match self {
            Self::Power => UnitOfMeasurement::W,
            Self::EnergyConsumed | Self::EnergyProduced => UnitOfMeasurement::Wh,
            Self::Current => UnitOfMeasurement::A,
            Self::Voltage => UnitOfMeasurement::V,
            Self::Frequency => UnitOfMeasurement::Hz,
            Self::EnergyCharged => UnitOfMeasurement::Wh,
            Self::StateOfCharge | Self::StateOfHealth => UnitOfMeasurement::Pct,
            Self::TravelRange => UnitOfMeasurement::M,
            Self::ApparentPower => UnitOfMeasurement::VA,
            Self::ReactivePower => UnitOfMeasurement::Var,
            // `1`: a power factor and a cycle count are ratios and counts, and SPINE says
            // so with a unit rather than by omitting one.
            Self::PowerFactor | Self::LoadCycleCount => UnitOfMeasurement::V1,
            Self::YieldDay
            | Self::YieldMonth
            | Self::YieldYear
            | Self::YieldTotal
            | Self::DcEnergy
            | Self::DcChargeEnergy
            | Self::DcDischargeEnergy
            | Self::StateOfEnergy
            | Self::UsableCapacity => UnitOfMeasurement::Wh,
            Self::Temperature
            | Self::DhwTemperature
            | Self::RoomTemperature
            | Self::OutdoorTemperature => UnitOfMeasurement::DegC,
            Self::DcPower => UnitOfMeasurement::W,
            Self::DcCurrent => UnitOfMeasurement::A,
            Self::DcVoltage => UnitOfMeasurement::V,
            Self::InsulationResistance => UnitOfMeasurement::Ohm,
        }
    }

    /// The range this quantity can meaningfully take, where the specification fixes one.
    ///
    /// A percentage is nought to a hundred and a distance is not negative; a power at a
    /// grid connection point is negative when the building is exporting, so it has none.
    /// Used as the default constraint a Monitored Unit publishes, and as what makes a
    /// reading `outOfRange` rather than believed.
    pub const fn natural_range(self) -> Option<(f64, f64)> {
        match self {
            Self::StateOfCharge | Self::StateOfHealth => Some((0.0, 100.0)),
            Self::TravelRange | Self::EnergyCharged => Some((0.0, f64::MAX)),
            _ => None,
        }
    }
}

/// One measurement a Monitored Unit publishes: a quantity and the phases it covers.
///
/// `phases` is `abc` for a value that covers the whole connection and `a`, `b` or `c` for
/// a phase-specific one; the specifications also allow `ab`, `bc` and `ac` for a voltage
/// measured between two phases.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Measurand {
    /// What is measured.
    pub quantity: Quantity,
    /// Which phases it was measured on, or [`None`] for a measurement that has none.
    ///
    /// Direct current has no phases, and a battery or a PV string measured on its DC side
    /// publishes no `acMeasuredPhases` at all. That is a different thing from a
    /// three-phase total, and conflating the two would have a client looking for phase
    /// information that is not coming.
    pub phases: Option<ElectricalConnectionPhaseName>,
}

impl Measurand {
    /// A measurement covering the whole connection: `acMeasuredPhases = "abc"`.
    pub const fn total(quantity: Quantity) -> Self {
        Self {
            quantity,
            phases: Some(ElectricalConnectionPhaseName::Abc),
        }
    }

    /// A measurement on one phase.
    pub const fn on(quantity: Quantity, phase: ElectricalConnectionPhaseName) -> Self {
        Self {
            quantity,
            phases: Some(phase),
        }
    }

    /// A measurement that has no phases at all.
    ///
    /// Everything on a direct-current side — a battery, an inverter's DC input, a PV
    /// string — and everything that is not an electrical flow in the first place: a state
    /// of charge, a state of health, a travel range. `acMeasuredPhases` is omitted rather
    /// than set to anything, which is what tells a client not to wait for it.
    pub const fn unphased(quantity: Quantity) -> Self {
        Self {
            quantity,
            phases: None,
        }
    }

    /// The total active power ([MPC-011], [MGCP-021]).
    pub const fn total_power() -> Self {
        Self::total(Quantity::Power)
    }

    /// True when this covers the whole connection rather than a single phase.
    ///
    /// A direct-current measurement counts as total: there is one of it, and nothing to
    /// break it down by.
    pub const fn is_total(&self) -> bool {
        matches!(self.phases, Some(ElectricalConnectionPhaseName::Abc) | None)
    }

    /// Whether this is measured on a direct-current side.
    pub const fn is_dc(&self) -> bool {
        self.phases.is_none()
    }

    /// The `commodityType` of the `Measurement` description.
    ///
    /// Electricity for everything this crate measures but three, and all three are
    /// temperatures somebody heats or lives in: MDT Table 7 fixes `domesticHotWater` for
    /// the hot water, MRT Table 7 and MOT Table 7 fix `air` for a room and for outdoors.
    /// Each is `M`. A client filtering on the commodity — which is the point of the
    /// element — would not see a tank published as electricity.
    ///
    /// An inverter's heatsink is *not* one of them:
    /// [`Temperature`](Quantity::Temperature) is `componentTemperature` on a piece of
    /// electrical equipment, and stays `electricity`.
    pub const fn commodity_type(&self) -> CommodityType {
        match self.quantity {
            Quantity::DhwTemperature => CommodityType::DomesticHotWater,
            Quantity::RoomTemperature | Quantity::OutdoorTemperature => CommodityType::Air,
            _ => CommodityType::Electricity,
        }
    }

    /// The `scopeType` of the `Measurement` description.
    ///
    /// Total active power is `acPowerTotal` and a phase-specific one is `acPower`; the
    /// two energies name the direction, which is what distinguishes them, since both are
    /// `measurementType = "energy"` in watt-hours.
    pub const fn scope_type(&self) -> ScopeType {
        match self.quantity {
            Quantity::Power if self.is_total() => ScopeType::AcPowerTotal,
            Quantity::Power => ScopeType::AcPower,
            Quantity::EnergyConsumed => ScopeType::AcEnergyConsumed,
            Quantity::EnergyProduced => ScopeType::AcEnergyProduced,
            Quantity::Current => ScopeType::AcCurrent,
            Quantity::Voltage => ScopeType::AcVoltage,
            Quantity::Frequency => ScopeType::AcFrequency,
            Quantity::EnergyCharged => ScopeType::Charge,
            Quantity::StateOfCharge => ScopeType::StateOfCharge,
            Quantity::StateOfHealth => ScopeType::StateOfHealth,
            Quantity::TravelRange => ScopeType::TravelRange,
            // Apparent and reactive power name the total separately from a phase, exactly
            // as active power does.
            Quantity::ApparentPower if self.is_total() => ScopeType::AcPowerApparentTotal,
            Quantity::ApparentPower => ScopeType::AcPowerApparent,
            Quantity::ReactivePower if self.is_total() => ScopeType::AcPowerReactiveTotal,
            Quantity::ReactivePower => ScopeType::AcPowerReactive,
            Quantity::PowerFactor => ScopeType::AcCosPhi,
            Quantity::YieldDay => ScopeType::AcYieldDay,
            Quantity::YieldMonth => ScopeType::AcYieldMonth,
            Quantity::YieldYear => ScopeType::AcYieldYear,
            Quantity::YieldTotal => ScopeType::AcYieldTotal,
            Quantity::Temperature => ScopeType::ComponentTemperature,
            Quantity::DhwTemperature => ScopeType::DhwTemperature,
            Quantity::RoomTemperature => ScopeType::RoomAirTemperature,
            Quantity::OutdoorTemperature => ScopeType::OutsideAirTemperature,
            Quantity::DcPower => ScopeType::DcPower,
            Quantity::DcCurrent => ScopeType::DcCurrent,
            Quantity::DcVoltage => ScopeType::DcVoltage,
            Quantity::DcEnergy => ScopeType::DcEnergy,
            Quantity::DcChargeEnergy => ScopeType::DcChargeEnergy,
            Quantity::DcDischargeEnergy => ScopeType::DcDischargeEnergy,
            Quantity::StateOfEnergy => ScopeType::StateOfEnergy,
            Quantity::UsableCapacity => ScopeType::UseableCapacity,
            Quantity::LoadCycleCount => ScopeType::LoadCycleCount,
            Quantity::InsulationResistance => ScopeType::InsulationResistance,
        }
    }

    /// The `scopeType` a Grid Connection Point uses for the two energies.
    ///
    /// MGCP scenarios 3 and 4 name them from the grid's side — `gridFeedIn` and
    /// `gridConsumption` — where MPC names them from the appliance's. Everything else is
    /// the same scope in both use cases.
    pub const fn grid_scope_type(&self) -> ScopeType {
        match self.quantity {
            Quantity::EnergyConsumed => ScopeType::GridConsumption,
            Quantity::EnergyProduced => ScopeType::GridFeedIn,
            _ => self.scope_type(),
        }
    }

    /// The `unit` of the description.
    pub const fn unit(&self) -> UnitOfMeasurement {
        self.quantity.unit()
    }

    /// The `measurementType` of the description.
    pub const fn measurement_type(&self) -> MeasurementType {
        self.quantity.measurement_type()
    }
}

impl Measurand {
    /// The name this measurand is reported under in a debug interface.
    ///
    /// The quantity in `lowerCamelCase`, with the phases appended where the measurand is
    /// not the whole connection: `totalActivePower`, `activePowerA`, `voltageAb`.
    pub fn signal_name(&self) -> alloc::string::String {
        use alloc::string::ToString;

        let quantity = match self.quantity {
            Quantity::Power if self.is_total() => "totalActivePower",
            Quantity::Power => "activePower",
            Quantity::EnergyConsumed => "energyConsumed",
            Quantity::EnergyProduced => "energyProduced",
            Quantity::Current => "current",
            Quantity::Voltage => "voltage",
            Quantity::Frequency => "frequency",
            Quantity::EnergyCharged => "energyCharged",
            Quantity::StateOfCharge => "stateOfCharge",
            Quantity::StateOfHealth => "stateOfHealth",
            Quantity::TravelRange => "travelRange",
            Quantity::ApparentPower => "apparentPower",
            Quantity::ReactivePower => "reactivePower",
            Quantity::PowerFactor => "powerFactor",
            Quantity::YieldDay => "yieldDay",
            Quantity::YieldMonth => "yieldMonth",
            Quantity::YieldYear => "yieldYear",
            Quantity::YieldTotal => "yieldTotal",
            Quantity::Temperature => "temperature",
            Quantity::DhwTemperature => "dhwTemperature",
            Quantity::RoomTemperature => "roomTemperature",
            Quantity::OutdoorTemperature => "outdoorTemperature",
            Quantity::DcPower => "dcPower",
            Quantity::DcCurrent => "dcCurrent",
            Quantity::DcVoltage => "dcVoltage",
            Quantity::DcEnergy => "dcEnergy",
            Quantity::DcChargeEnergy => "dcChargeEnergy",
            Quantity::DcDischargeEnergy => "dcDischargeEnergy",
            Quantity::StateOfEnergy => "stateOfEnergy",
            Quantity::UsableCapacity => "usableCapacity",
            Quantity::LoadCycleCount => "loadCycleCount",
            Quantity::InsulationResistance => "insulationResistance",
        };
        let Some(phases) = self.phases.as_ref().filter(|_| !self.is_total()) else {
            return quantity.to_string();
        };
        let phases = phases.as_str();
        let mut name = alloc::string::String::from(quantity);
        let mut chars = phases.chars();
        if let Some(first) = chars.next() {
            name.extend(first.to_uppercase());
            name.push_str(chars.as_str());
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpc_scenario_1_describes_power_as_the_specification_asks() {
        // LPC/MPC Table 2 and the "Content of Function" tables of §3.2.2.2.
        let total = Measurand::total_power();
        assert_eq!(total.measurement_type(), MeasurementType::Power);
        assert_eq!(total.commodity_type(), CommodityType::Electricity);
        assert_eq!(total.unit(), UnitOfMeasurement::W);
        assert_eq!(total.scope_type(), ScopeType::AcPowerTotal);

        let phase = Measurand::on(Quantity::Power, ElectricalConnectionPhaseName::A);
        assert_eq!(
            phase.scope_type(),
            ScopeType::AcPower,
            "a phase-specific power is `acPower`, not `acPowerTotal`"
        );
    }

    #[test]
    fn mgcp_names_the_energies_from_the_grids_side() {
        let consumed = Measurand::total(Quantity::EnergyConsumed);
        assert_eq!(consumed.scope_type(), ScopeType::AcEnergyConsumed, "MPC");
        assert_eq!(
            consumed.grid_scope_type(),
            ScopeType::GridConsumption,
            "MGCP"
        );

        let produced = Measurand::total(Quantity::EnergyProduced);
        assert_eq!(produced.scope_type(), ScopeType::AcEnergyProduced, "MPC");
        assert_eq!(produced.grid_scope_type(), ScopeType::GridFeedIn, "MGCP");

        assert_eq!(
            Measurand::total_power().grid_scope_type(),
            ScopeType::AcPowerTotal,
            "everything else is named the same in both"
        );
    }
}
