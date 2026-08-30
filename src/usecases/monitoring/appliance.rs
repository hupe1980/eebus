//! The Monitoring Appliance: the side that reads.
//!
//! A `measurementListData` notification carries identifiers and numbers and nothing
//! else — `{"measurementId": 3, "value": {"number": 2300}}`. What that means is in two
//! other functions, read once at commissioning. [`Readings`] holds those descriptions and
//! resolves each notification against them, so an application sees a quantity and a
//! phase rather than an integer it has to remember the meaning of.

use alloc::vec::Vec;

use crate::model::{
    CmdData, ElectricalConnectionPhaseName, MeasurementId, MeasurementValueState, ScopeType,
};

use super::measurand::{Measurand, Quantity};

/// One resolved value from a Monitored Unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// What was measured, and on which phases.
    pub measurand: Measurand,
    /// The value, in the measurand's unit. Absent when the unit reported an error.
    pub value: Option<f64>,
    /// Whether the value can be trusted ([MPC-003]).
    pub state: ReadingState,
}

/// What a measurement's `valueState` says about its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingState {
    /// A value within its constraints. `valueState` absent or `normal`.
    Normal,
    /// A value outside the constraints the unit published.
    OutOfRange,
    /// The unit could not measure. Any value sent with this is to be ignored.
    Error,
}

impl Reading {
    /// The value, unless the unit flagged it as an error.
    ///
    /// [MPC-003]: where `valueState` is `error` the content of `value` `SHALL` be
    /// ignored, so a caller that wants a number it can act on should ask for it here
    /// rather than reading the field.
    pub fn usable(&self) -> Option<f64> {
        match self.state {
            ReadingState::Error => None,
            _ => self.value,
        }
    }
}

/// What one `measurementId` is known to mean.
///
/// The quantity comes from either description; the phases come only from the parameter
/// descriptions, so they stay unknown until those are read.
#[derive(Clone, Debug, PartialEq)]
struct Known {
    id: MeasurementId,
    quantity: Quantity,
    phases: Option<ElectricalConnectionPhaseName>,
}

/// What a Monitoring Appliance has learned about one Monitored Unit.
///
/// Feed it the two description functions once, then every `measurementListData` that
/// arrives; ask it for the values you care about by measurand.
#[derive(Clone, Debug, Default)]
pub struct Readings {
    known: Vec<Known>,
    values: Vec<(MeasurementId, Reading)>,
}

impl Readings {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Learns what each `measurementId` means, from either description function.
    ///
    /// Both carry a `scopeType`, which gives the quantity. Only the parameter
    /// descriptions carry `acMeasuredPhases`, so a measurement stays without phases until
    /// those are read — which is why MPC §3.2.2.2 makes them mandatory. The two may
    /// arrive in either order.
    pub fn describe(&mut self, data: &CmdData) -> bool {
        match data {
            CmdData::MeasurementDescriptionListData(list) => {
                for entry in list.measurement_description_data.iter().flatten() {
                    let (Some(id), Some(scope)) = (entry.measurement_id, entry.scope_type.as_ref())
                    else {
                        continue;
                    };
                    let Some(quantity) = quantity_of(scope) else {
                        continue;
                    };
                    // A scope that covers the whole connection says its own phases; one
                    // that does not waits for the parameter description.
                    let phases =
                        covers_everything(scope).then_some(ElectricalConnectionPhaseName::Abc);
                    self.learn(id, quantity, phases);
                }
                true
            }
            CmdData::ElectricalConnectionParameterDescriptionListData(list) => {
                for entry in list
                    .electrical_connection_parameter_description_data
                    .iter()
                    .flatten()
                {
                    let (Some(id), Some(scope)) = (entry.measurement_id, entry.scope_type.as_ref())
                    else {
                        continue;
                    };
                    let Some(quantity) = quantity_of(scope) else {
                        continue;
                    };
                    self.learn(id, quantity, entry.ac_measured_phases.clone());
                }
                true
            }
            _ => false,
        }
    }

    /// Records what a description said, without unlearning phases already known.
    fn learn(
        &mut self,
        id: MeasurementId,
        quantity: Quantity,
        phases: Option<ElectricalConnectionPhaseName>,
    ) {
        match self.known.iter_mut().find(|k| k.id == id) {
            Some(known) => {
                known.quantity = quantity;
                if phases.is_some() {
                    known.phases = phases;
                }
            }
            None => self.known.push(Known {
                id,
                quantity,
                phases,
            }),
        }
    }

    /// Applies a `measurementListData`, returning what it resolved to.
    ///
    /// Values whose `measurementId` has no description, or whose phases are not yet
    /// known, are dropped: a number without a meaning is worse than no number, because it
    /// invites a guess.
    pub fn apply(&mut self, data: &CmdData) -> Vec<Reading> {
        let CmdData::MeasurementListData(list) = data else {
            return Vec::new();
        };
        let mut applied = Vec::new();
        for entry in list.measurement_data.iter().flatten() {
            let Some(id) = entry.measurement_id else {
                continue;
            };
            let Some(measurand) = self.measurand_of(id) else {
                continue;
            };
            let state = match entry.value_state.as_ref() {
                Some(MeasurementValueState::OutOfRange) => ReadingState::OutOfRange,
                Some(MeasurementValueState::Error) => ReadingState::Error,
                _ => ReadingState::Normal,
            };
            let reading = Reading {
                measurand,
                value: entry.value.as_ref().and_then(|v| v.to_f64()),
                state,
            };
            match self.values.iter_mut().find(|(known, _)| *known == id) {
                Some((_, stored)) => *stored = reading.clone(),
                None => self.values.push((id, reading.clone())),
            }
            applied.push(reading);
        }
        applied
    }

    /// What a `measurementId` means, once both its descriptions have been read.
    pub fn measurand_of(&self, id: MeasurementId) -> Option<Measurand> {
        let known = self.known.iter().find(|k| k.id == id)?;
        Some(Measurand {
            quantity: known.quantity,
            phases: known.phases.clone()?,
        })
    }

    /// The latest reading of a measurand.
    pub fn get(&self, measurand: &Measurand) -> Option<Reading> {
        self.values
            .iter()
            .find(|(_, reading)| reading.measurand == *measurand)
            .map(|(_, reading)| reading.clone())
    }

    /// The latest usable value of a measurand, in its unit.
    pub fn value(&self, measurand: &Measurand) -> Option<f64> {
        self.get(measurand).and_then(|r| r.usable())
    }

    /// The latest total active power, in watts — scenario 1 of MPC, scenario 2 of MGCP.
    ///
    /// Positive means the unit is drawing power and negative that it is feeding in, per
    /// the load convention both use cases apply ([MPC-001]).
    pub fn total_power(&self) -> Option<f64> {
        self.value(&Measurand::total_power())
    }

    /// Every reading held, in the order the measurements were first described.
    pub fn all(&self) -> impl Iterator<Item = &Reading> {
        self.values.iter().map(|(_, reading)| reading)
    }
}

/// Whether a scope is inherently about the whole connection rather than some phases.
///
/// `acPower`, `acCurrent` and `acVoltage` are the phase-specific ones; everything else
/// these two use cases publish is a total.
fn covers_everything(scope: &ScopeType) -> bool {
    !matches!(
        scope,
        ScopeType::AcPower | ScopeType::AcCurrent | ScopeType::AcVoltage
    )
}

/// The quantity a scope names, in either use case's vocabulary.
fn quantity_of(scope: &ScopeType) -> Option<Quantity> {
    Some(match scope {
        ScopeType::AcPowerTotal | ScopeType::AcPower => Quantity::Power,
        ScopeType::AcEnergyConsumed | ScopeType::GridConsumption => Quantity::EnergyConsumed,
        ScopeType::AcEnergyProduced | ScopeType::GridFeedIn => Quantity::EnergyProduced,
        ScopeType::AcCurrent => Quantity::Current,
        ScopeType::AcVoltage => Quantity::Voltage,
        ScopeType::AcFrequency => Quantity::Frequency,
        _ => return None,
    })
}
