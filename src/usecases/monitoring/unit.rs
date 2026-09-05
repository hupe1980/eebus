//! The Monitored Unit: the side that measures and publishes.

use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    AbsoluteOrRelativeTime, CmdData, ElectricalConnectionDescriptionData,
    ElectricalConnectionDescriptionListData, ElectricalConnectionId,
    ElectricalConnectionParameterDescriptionData, ElectricalConnectionParameterDescriptionListData,
    ElectricalConnectionParameterId, ElectricalConnectionVoltageType, EnergyDirection, FeatureType,
    Function, MeasurementConstraintsData, MeasurementConstraintsListData, MeasurementData,
    MeasurementDescriptionData, MeasurementDescriptionListData, MeasurementId, MeasurementListData,
    MeasurementValueState, MeasurementValueType, Role, ScaledNumber,
};
use crate::spine::{Engine, LocalFeature, Operations};

use super::measurand::Measurand;

/// How many decimal places a measurement keeps on the wire.
///
/// [`ScaledNumber::from_f64`] picks the shortest exact representation within the limit, so
/// a whole 2300 W still goes out as `{"number": 2300, "scale": 0}` — the limit only
/// matters for a reading that has a fraction, and 3.5 A must not become 4 A.
const DECIMALS: u8 = 3;

/// How a Monitored Unit names its two energy measurements.
///
/// The scopes are the only place MPC and MGCP disagree: MPC names the energies from the
/// appliance's side, MGCP from the grid's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// `acEnergyConsumed` / `acEnergyProduced` — Monitoring of Power Consumption.
    Appliance,
    /// `gridConsumption` / `gridFeedIn` — Monitoring of the Grid Connection Point.
    GridConnectionPoint,
    /// A car being charged — EV Charging Electricity Measurement.
    ///
    /// The energies are named from the appliance's side as in MPC; what a car adds is
    /// [`Quantity::EnergyCharged`](super::Quantity::EnergyCharged), whose scope is
    /// `charge` in every use case.
    EvCharging,
    /// A car's battery — EV State of Charge.
    EvBattery,
}

impl Naming {
    /// The prefix runtime signals are named with: `mpc`, `mgcp`, `evcem` or `evsoc`.
    pub const fn signal_prefix(self) -> &'static str {
        match self {
            Self::Appliance => "mpc",
            Self::GridConnectionPoint => "mgcp",
            Self::EvCharging => "evcem",
            Self::EvBattery => "evsoc",
        }
    }
}

/// One measurement, with the identifiers it was assigned and its latest value.
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    measurand: Measurand,
    measurement_id: MeasurementId,
    parameter_id: ElectricalConnectionParameterId,
    value: Option<f64>,
    state: MeasurementValueState,
    range: Option<(f64, f64)>,
    /// When the sensor took the reading, where the application said. Sent as the
    /// `timestamp` element, which every measurement use case here permits and none
    /// requires.
    taken_at: Option<AbsoluteOrRelativeTime>,
}

/// A Monitored Unit: what it measures, and the last value of each measurement.
///
/// Identifiers are assigned as measurands are added and never move, because a client
/// binds a `measurementId` to a meaning once, at discovery, and reads every later
/// notification against it.
///
/// ```
/// use eebus::model::ElectricalConnectionPhaseName as Phase;
/// use eebus::usecases::monitoring::{Measurand, MonitoredUnit, Quantity};
///
/// let mut unit = MonitoredUnit::new(1)
///     .with(Measurand::total_power())
///     .with(Measurand::on(Quantity::Current, Phase::A));
///
/// unit.set(&Measurand::total_power(), 2_300.0);
/// assert_eq!(unit.value(&Measurand::total_power()), Some(2_300.0));
/// ```
#[derive(Clone, Debug)]
pub struct MonitoredUnit {
    connection: ElectricalConnectionId,
    naming: Naming,
    slots: Vec<Slot>,
}

impl MonitoredUnit {
    /// A unit with no measurements yet, on one electrical connection.
    pub fn new(connection: u32) -> Self {
        Self {
            connection: ElectricalConnectionId(connection),
            naming: Naming::Appliance,
            slots: Vec::new(),
        }
    }

    /// Names the energies from the grid's side, as MGCP scenarios 3 and 4 require.
    pub fn naming(mut self, naming: Naming) -> Self {
        self.naming = naming;
        self
    }

    /// Adds a measurement, assigning it the next `measurementId` and `parameterId`.
    ///
    /// Adding the same measurand twice is ignored: two descriptions of one quantity on
    /// one set of phases would leave a client unable to say which is authoritative.
    pub fn with(mut self, measurand: Measurand) -> Self {
        self.add(measurand);
        self
    }

    /// Adds a measurement, returning the `measurementId` it was given.
    pub fn add(&mut self, measurand: Measurand) -> MeasurementId {
        if let Some(slot) = self.slots.iter().find(|s| s.measurand == measurand) {
            return slot.measurement_id;
        }
        let next = self.slots.len() as u32 + 1;
        let measurement_id = MeasurementId(next);
        self.slots.push(Slot {
            measurand,
            measurement_id,
            parameter_id: ElectricalConnectionParameterId(next),
            value: None,
            state: MeasurementValueState::Normal,
            range: None,
            taken_at: None,
        });
        measurement_id
    }

    /// The `measurementId` a measurand was assigned, if it has one.
    pub fn measurement_id(&self, measurand: &Measurand) -> Option<MeasurementId> {
        self.slot(measurand).map(|s| s.measurement_id)
    }

    /// The last value recorded for a measurand.
    pub fn value(&self, measurand: &Measurand) -> Option<f64> {
        self.slot(measurand).and_then(|s| s.value)
    }

    /// Declares what range a measurement can report, as `measurementConstraintsListData`.
    ///
    /// MPC and MGCP mark the constraints as recommended, and they are what `outOfRange`
    /// means: without them a client is told a value is out of range without being told
    /// what range. Declaring them also makes [`set`](Self::set) flag such a value on its
    /// own.
    pub fn with_range(mut self, measurand: Measurand, min: f64, max: f64) -> Self {
        self.set_range(&measurand, min, max);
        self
    }

    /// Declares the range of a measurement already added.
    pub fn set_range(&mut self, measurand: &Measurand, min: f64, max: f64) {
        if let Some(slot) = self.slot_mut(measurand) {
            slot.range = Some((min, max));
        }
    }

    /// Records a new value.
    ///
    /// A value outside the declared range is marked `outOfRange` ([MPC-003]) rather than
    /// silently clamped or passed off as normal — the reading is what it is, and it is
    /// the client's business what to do about it. Otherwise recording a value clears any
    /// earlier state: a measurement that reads again is working again — and clears any
    /// timestamp, because a new reading taken at an unstated time is not the old one's
    /// time.
    pub fn set(&mut self, measurand: &Measurand, value: f64) {
        self.record(measurand, value, None);
    }

    /// The same, saying when the sensor took the reading.
    ///
    /// The `timestamp` element, which every measurement use case in this crate permits and
    /// none requires. Worth sending, and the reason is on the *reader's* side: a client
    /// subscribed to this unit hears from it when something changes, so the arrival time
    /// of a notification is the age of the change and not the age of the measurement. A
    /// quantity that is steady sends nothing at all, and to a client that ages its inputs
    /// by arrival that steadiness is indistinguishable from a sensor that stopped. See
    /// [`Reading::timestamp`](super::Reading::timestamp).
    ///
    /// The value is passed through verbatim: SPINE's element is an absolute time *or* an
    /// ISO 8601 duration, and which one this unit sends is the application's decision —
    /// this crate has no clock of its own.
    pub fn set_at(&mut self, measurand: &Measurand, value: f64, taken_at: AbsoluteOrRelativeTime) {
        self.record(measurand, value, Some(taken_at));
    }

    fn record(
        &mut self,
        measurand: &Measurand,
        value: f64,
        taken_at: Option<AbsoluteOrRelativeTime>,
    ) {
        if let Some(slot) = self.slot_mut(measurand) {
            slot.value = Some(value);
            slot.taken_at = taken_at;
            slot.state = match slot.range {
                Some((min, max)) if value < min || value > max => MeasurementValueState::OutOfRange,
                _ => MeasurementValueState::Normal,
            };
        }
    }

    /// Marks a measurement as unavailable or out of range ([MPC-003]).
    ///
    /// A client `SHALL` always consider `valueState`, and where it is `error` the value
    /// itself is to be ignored — so an implementation that keeps serving the last good
    /// reading through a sensor failure is reporting a number it knows to be wrong.
    pub fn set_state(&mut self, measurand: &Measurand, state: MeasurementValueState) {
        if let Some(slot) = self.slot_mut(measurand) {
            slot.state = state;
        }
    }

    fn slot(&self, measurand: &Measurand) -> Option<&Slot> {
        self.slots.iter().find(|s| s.measurand == *measurand)
    }

    fn slot_mut(&mut self, measurand: &Measurand) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.measurand == *measurand)
    }

    fn scope_of(&self, measurand: &Measurand) -> crate::model::ScopeType {
        match self.naming {
            // Only a grid connection point renames the two energies; everywhere else,
            // including a car, the scope follows from the measurand alone.
            Naming::GridConnectionPoint => measurand.grid_scope_type(),
            Naming::Appliance | Naming::EvCharging | Naming::EvBattery => measurand.scope_type(),
        }
    }

    /// The `ElectricalConnection` feature this unit serves.
    pub fn electrical_connection_feature(&self, address: u32) -> LocalFeature {
        LocalFeature::new(address, FeatureType::ElectricalConnection, Role::Server)
            .with_function(
                Function::ElectricalConnectionDescriptionListData,
                Operations::read(),
            )
            .with_function(
                Function::ElectricalConnectionParameterDescriptionListData,
                Operations::read(),
            )
    }

    /// The `Measurement` feature this unit serves.
    pub fn measurement_feature(&self, address: u32) -> LocalFeature {
        LocalFeature::new(address, FeatureType::Measurement, Role::Server)
            .with_function(Function::MeasurementDescriptionListData, Operations::read())
            .with_function(Function::MeasurementConstraintsListData, Operations::read())
            .with_function(Function::MeasurementListData, Operations::read())
    }

    /// The connection description (MPC/MGCP §3.2.2.2).
    ///
    /// `positiveEnergyDirection` is `consume` because both use cases apply the load
    /// convention ([MPC-001]): consumption is positive, production negative, and a client
    /// reading a negative total power is being told the unit is feeding in.
    pub fn connection_description(&self) -> CmdData {
        CmdData::ElectricalConnectionDescriptionListData(ElectricalConnectionDescriptionListData {
            electrical_connection_description_data: Some(vec![
                ElectricalConnectionDescriptionData {
                    electrical_connection_id: Some(self.connection),
                    power_supply_type: Some(ElectricalConnectionVoltageType::Ac),
                    positive_energy_direction: Some(EnergyDirection::Consume),
                    ..Default::default()
                },
            ]),
        })
    }

    /// The parameter descriptions, which bind each measurement to its phases.
    pub fn parameter_descriptions(&self) -> CmdData {
        CmdData::ElectricalConnectionParameterDescriptionListData(
            ElectricalConnectionParameterDescriptionListData {
                electrical_connection_parameter_description_data: Some(
                    self.slots
                        .iter()
                        .map(|slot| ElectricalConnectionParameterDescriptionData {
                            electrical_connection_id: Some(self.connection),
                            parameter_id: Some(slot.parameter_id),
                            measurement_id: Some(slot.measurement_id),
                            voltage_type: Some(ElectricalConnectionVoltageType::Ac),
                            // Omitted, not defaulted, for a direct-current measurement:
                            // there are no phases to name.
                            ac_measured_phases: slot.measurand.phases.clone(),
                            scope_type: Some(self.scope_of(&slot.measurand)),
                            ..Default::default()
                        })
                        .collect(),
                ),
            },
        )
    }

    /// The constraints on each measurement, where a range was declared.
    pub fn constraints(&self) -> CmdData {
        CmdData::MeasurementConstraintsListData(MeasurementConstraintsListData {
            measurement_constraints_data: Some(
                self.slots
                    .iter()
                    .filter_map(|slot| {
                        let (min, max) = slot.range?;
                        Some(MeasurementConstraintsData {
                            measurement_id: Some(slot.measurement_id),
                            value_range_min: Some(ScaledNumber::from_f64(min, DECIMALS)),
                            value_range_max: Some(ScaledNumber::from_f64(max, DECIMALS)),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
        })
    }

    /// The measurement descriptions: what each `measurementId` means.
    pub fn measurement_descriptions(&self) -> CmdData {
        CmdData::MeasurementDescriptionListData(MeasurementDescriptionListData {
            measurement_description_data: Some(
                self.slots
                    .iter()
                    .map(|slot| MeasurementDescriptionData {
                        measurement_id: Some(slot.measurement_id),
                        measurement_type: Some(slot.measurand.measurement_type()),
                        commodity_type: Some(slot.measurand.commodity_type()),
                        unit: Some(slot.measurand.unit()),
                        scope_type: Some(self.scope_of(&slot.measurand)),
                        ..Default::default()
                    })
                    .collect(),
            ),
        })
    }

    /// The current values.
    ///
    /// A measurement that has never been read is left out rather than sent as zero: an
    /// absent entry says "not known", where a zero says "no power", and for a grid
    /// connection point those are very different claims.
    pub fn measurements(&self) -> CmdData {
        CmdData::MeasurementListData(MeasurementListData {
            measurement_data: Some(
                self.slots
                    .iter()
                    .filter(|slot| {
                        slot.value.is_some() || slot.state != MeasurementValueState::Normal
                    })
                    .map(|slot| MeasurementData {
                        measurement_id: Some(slot.measurement_id),
                        value_type: Some(MeasurementValueType::Value),
                        value: slot.value.map(|v| ScaledNumber::from_f64(v, DECIMALS)),
                        value_state: (slot.state != MeasurementValueState::Normal)
                            .then(|| slot.state.clone()),
                        timestamp: slot.taken_at.clone(),
                        ..Default::default()
                    })
                    .collect(),
            ),
        })
    }

    /// Publishes the descriptions and the current values onto two features.
    pub fn publish(
        &self,
        engine: &mut Engine,
        electrical_connection: &crate::model::FeatureAddress,
        measurement: &crate::model::FeatureAddress,
    ) {
        let ec = electrical_connection.clone();
        let m = measurement.clone();
        if let Some(feature) = engine.device_mut().resolve_mut(&ec) {
            let _ = feature.set_data(self.connection_description());
            let _ = feature.set_data(self.parameter_descriptions());
        }
        if let Some(feature) = engine.device_mut().resolve_mut(&m) {
            let _ = feature.set_data(self.measurement_descriptions());
            let _ = feature.set_data(self.constraints());
            let _ = feature.set_data(self.measurements());
        }
    }

    /// Publishes the current values and notifies every subscriber.
    pub fn notify(
        &self,
        engine: &mut Engine,
        measurement: &crate::model::FeatureAddress,
        now: Duration,
    ) {
        let address = measurement.clone();
        let changed = engine
            .device_mut()
            .resolve_mut(&address)
            .and_then(|feature| feature.set_data(self.measurements()).ok())
            .unwrap_or(false);
        // Implementation guide §2.4: a reading that has not moved is not news.
        if changed {
            engine.notify(&address, &Function::MeasurementListData, now);
        }
    }
}

impl crate::usecases::signals::Signals for MonitoredUnit {
    /// Every measurand this unit publishes, and what it currently reads.
    ///
    /// Named `mpc:` or `mgcp:` after [`Naming`], because the same measurand means
    /// something different at an appliance and at a grid connection point — and the
    /// abstract test cases for the two are separate lists. A measurand that is not
    /// `normal` reports its value state instead of its value, which is what
    /// `ATC_MPC_SCE1_NT_MATotalActivePower_002` and its siblings are looking for.
    fn signals(&self, _: ()) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalSet, SignalValue};
        use alloc::borrow::Cow;
        use alloc::format;

        let prefix = self.naming.signal_prefix();
        self.slots
            .iter()
            .map(|slot| {
                let name = Cow::Owned(format!("{prefix}:{}", slot.measurand.signal_name()));
                let value = match &slot.state {
                    MeasurementValueState::Normal => SignalValue::number(slot.value),
                    MeasurementValueState::OutOfRange => {
                        SignalValue::Text(Cow::Borrowed("outOfRange"))
                    }
                    MeasurementValueState::Error => SignalValue::Text(Cow::Borrowed("error")),
                    other => {
                        SignalValue::Text(Cow::Owned(alloc::string::String::from(other.as_str())))
                    }
                };
                Signal::new(name, value)
                    .in_unit(alloc::string::String::from(slot.measurand.unit().as_str()))
            })
            .collect::<SignalSet>()
    }
}
