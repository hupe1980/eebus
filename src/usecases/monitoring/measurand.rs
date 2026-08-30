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
    /// Which phases it was measured on.
    pub phases: ElectricalConnectionPhaseName,
}

impl Measurand {
    /// A measurement covering the whole connection: `acMeasuredPhases = "abc"`.
    pub const fn total(quantity: Quantity) -> Self {
        Self {
            quantity,
            phases: ElectricalConnectionPhaseName::Abc,
        }
    }

    /// A measurement on one phase.
    pub const fn on(quantity: Quantity, phase: ElectricalConnectionPhaseName) -> Self {
        Self {
            quantity,
            phases: phase,
        }
    }

    /// The total active power ([MPC-011], [MGCP-021]).
    pub const fn total_power() -> Self {
        Self::total(Quantity::Power)
    }

    /// True when this covers the whole connection rather than a single phase.
    pub const fn is_total(&self) -> bool {
        matches!(self.phases, ElectricalConnectionPhaseName::Abc)
    }

    /// The `commodityType` of the description, which is `electricity` throughout.
    pub const fn commodity_type(&self) -> CommodityType {
        CommodityType::Electricity
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
