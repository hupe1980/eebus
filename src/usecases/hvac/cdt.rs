//! Configuration of DHW Temperature (CDT).
//!
//! A *Configuration Appliance* — a CEM, usually — sets the domestic hot water temperature
//! setpoint of a *DHW Circuit*. It is the smallest use case in this crate and the one that
//! answers a question the limitation family cannot: [`limitation`] tells a heat pump how
//! much power it **may not exceed**, and this tells it what to **aim for**.
//!
//! Those are different levers and they are not interchangeable. A ceiling can only ever
//! make a heat pump do less; a setpoint can make it do more, which is what turns a hot
//! water tank into the cheapest thermal battery in the building. Raising the DHW setpoint
//! by ten degrees while the roof is exporting stores a few kilowatt-hours that would
//! otherwise have been sold at the feed-in tariff and bought back at the retail one — and
//! no limit, however carefully written, can ask for that.
//!
//! One scenario, and both actors must implement it (Table 1):
//!
//! 1. **Set DHW temperature setpoint** — the appliance writes a temperature, the circuit
//!    applies it within the range and step size it published.
//!
//! **This use case is not complete on its own.** §2.4.2 and §2.4.3: a DHW Circuit that
//! does not serve "Monitoring of DHW System Function" SHALL serve "Configuration of DHW
//! System Function" [CDT-005] — one of the two is mandatory. The reason is
//! [`system_function_relations`]: a setpoint is reached *through* an operation mode, so
//! "write 60 °C" is only a complete instruction once the mode the circuit is in is known,
//! and that is what [`mdsf`](super::mdsf) reports.
//! [`mdsf::DhwSystemFunction::current_setpoints`](super::mdsf::DhwSystemFunction::current_setpoints)
//! is the join. A write into a mode the circuit is not in is applied, acknowledged, and
//! changes nothing anybody can measure.
//!
//! # The identifiers are the circuit's
//!
//! `setpointId` is `<st1#(1..4)>` — a DHW Circuit publishes **one to four** temperature
//! setpoints and numbers them itself — and `systemFunctionId` is `<sf1#(1..1)>`. Which
//! setpoint a Configuration Appliance should write is not a number it can assume: it is
//! the one whose description says `scopeType: dhwTemperature`, and which of *those* applies
//! is decided by the operation mode the circuit is in, published in
//! `hvacSystemFunctionSetpointRelationListData`. [`DhwSetpoints`] is the reader that puts
//! the three functions together; see [`crate::usecases::addressing`] for why none of this
//! may be shortcut.
//!
//! ```
//! use eebus::usecases::hvac::cdt::{self, DhwSetpoints};
//! use eebus::model::UnitOfMeasurement;
//!
//! // What the circuit publishes.
//! let mut known = DhwSetpoints::new();
//! known.learn(&cdt::setpoint_description(UnitOfMeasurement::DegC));
//! known.learn(&cdt::setpoint_constraints(40.0, 65.0, Some(0.5)));
//!
//! let id = known.temperature_setpoints().next().expect("a DHW setpoint");
//!
//! // 60 °C is inside the range and on the step size, so it can be written.
//! assert!(known.write(id, 60.0).is_ok());
//! // 70 °C is not, and is refused here rather than by the circuit.
//! assert!(known.write(id, 70.0).is_err());
//! ```
//!
//! [`limitation`]: crate::usecases::limitation

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacSystemFunctionSetpointRelationData, HvacSystemFunctionSetpointRelationListData, Role,
    ScaledNumber, ScopeType, SetpointConstraintsData, SetpointConstraintsListData, SetpointData,
    SetpointDescriptionData, SetpointDescriptionListData, SetpointId, SetpointListData,
    SetpointType, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::SYSTEM_FUNCTION_ID;

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "configurationOfDhwTemperature";

/// The actor that holds the hot water.
pub const DHW_CIRCUIT_ACTOR: &str = "DHWCircuit";

/// The actor that sets its temperature.
pub const CONFIGURATION_APPLIANCE_ACTOR: &str = "ConfigurationAppliance";

/// The `scopeType` that marks a setpoint as the DHW temperature (Table 7).
///
/// This is the fixed half — what a Configuration Appliance matches on to find the
/// setpoint it may write.
pub const TEMPERATURE_SCOPE: ScopeType = ScopeType::DhwTemperature;

/// The `setpointId` **this** implementation publishes its DHW temperature under.
///
/// A local choice. Table 7 spells it `<st1#(1..4)>`: a circuit publishes one to four
/// temperature setpoints and numbers them itself, and which one is in force depends on the
/// operation mode. Find a peer's with [`DhwSetpoints`], never by assuming this.
pub const SETPOINT_ID: SetpointId = SetpointId(1);

/// The units a DHW temperature may be published in (Table 7).
pub const UNITS: [UnitOfMeasurement; 3] = [
    UnitOfMeasurement::DegC,
    UnitOfMeasurement::DegF,
    UnitOfMeasurement::K,
];

/// How many temperature setpoints an operation mode may relate to.
///
/// §2.3.1.1 gives three different rules and they are not decoration — a mode that relates
/// to the wrong number of setpoints is a circuit whose behaviour a Configuration Appliance
/// cannot predict.
///
/// * `auto` — one to four [CDT-003/1]. Which of them the circuit picks, and when, is
///   vendor-specific and deliberately outside this use case.
/// * `on` and `eco` — exactly one [CDT-003/2].
/// * `off` — none, or exactly one [CDT-003/3].
pub fn permitted_setpoints(mode: &HvacOperationModeType) -> core::ops::RangeInclusive<usize> {
    match mode {
        HvacOperationModeType::Auto => 1..=4,
        HvacOperationModeType::On | HvacOperationModeType::Eco => 1..=1,
        HvacOperationModeType::Off => 0..=1,
        // The list is open, and a mode this version does not name carries no rule of its
        // own beyond Table 10's `setpointId (1..4)`.
        _ => 0..=4,
    }
}

/// Whether a relation between an operation mode and its setpoints is well formed.
pub fn relation_is_valid(mode: &HvacOperationModeType, setpoints: &[SetpointId]) -> bool {
    permitted_setpoints(mode).contains(&setpoints.len())
}

// ---- the features a DHW Circuit serves ---------------------------------------------

/// Builds the `Setpoint` feature scenario 1 is served from (Table 6).
///
/// `setpointListData` is the only writeable function: the description and the constraints
/// are the circuit's own statement of what it is and what it will accept, and a
/// Configuration Appliance that could rewrite them could talk itself into any temperature.
pub fn setpoint_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Setpoint, Role::Server)
        .with_deferred_writes()
        .with_function(Function::SetpointDescriptionListData, Operations::read())
        .with_function(Function::SetpointConstraintsListData, Operations::read())
        .with_function(Function::SetpointListData, Operations::read_write())
}

/// Builds the `HVAC` feature that says which setpoint belongs to which operation mode.
pub fn hvac_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::HVAC, Role::Server).with_function(
        Function::HvacSystemFunctionSetpointRelationListData,
        Operations::read(),
    )
}

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The setpoint's description (Table 7).
///
/// `measurementId` is deliberately absent. It is a FOREIGN IDENTIFIER pointing at the
/// measurement of the same temperature, and §3.2.1.2.2.1 requires it to be *the same
/// number* "Monitoring of DHW Temperature" uses — so a circuit that does not serve that
/// use case omits the element rather than inventing a link that leads nowhere. Use
/// [`setpoint_description_measuring`] where the two are served together.
pub fn setpoint_description(unit: UnitOfMeasurement) -> CmdData {
    described(SETPOINT_ID, unit, None)
}

/// The same, for a circuit that also serves [`mdt`](super::mdt).
///
/// `measurement` must be the `measurementId` that use case publishes the DHW temperature
/// under — §3.2.1.2.2.1 says the two SHALL be identical — which is what lets a
/// Configuration Appliance tie the reading to the setpoint that governs it, and so read
/// back the effect of what it wrote rather than only the number it asked for.
/// [`mdt::MEASUREMENT_ID`](super::mdt::MEASUREMENT_ID) is this crate's own.
///
/// ```
/// use eebus::model::UnitOfMeasurement;
/// use eebus::usecases::hvac::{cdt, mdt};
///
/// // A circuit serving both: the setpoint points at the measurement.
/// let described = cdt::setpoint_description_measuring(
///     UnitOfMeasurement::DegC,
///     mdt::MEASUREMENT_ID,
/// );
/// ```
pub fn setpoint_description_measuring(
    unit: UnitOfMeasurement,
    measurement: crate::model::MeasurementId,
) -> CmdData {
    described(SETPOINT_ID, unit, Some(measurement))
}

fn described(
    id: SetpointId,
    unit: UnitOfMeasurement,
    measurement: Option<crate::model::MeasurementId>,
) -> CmdData {
    CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
        setpoint_description_data: Some(vec![SetpointDescriptionData {
            setpoint_id: Some(id),
            measurement_id: measurement,
            setpoint_type: Some(SetpointType::ValueAbsolute),
            unit: Some(unit),
            scope_type: Some(TEMPERATURE_SCOPE),
            ..Default::default()
        }]),
    })
}

/// What the circuit will accept (Table 8).
///
/// `step` is `R` rather than `M`: a circuit that accepts any value inside the range omits
/// it. Where it is published, a value that does not match SHALL be rounded **by the
/// server** — so a write off the step is not an error, and a Configuration Appliance that
/// wants to know what it will get should round before it sends.
pub fn setpoint_constraints(min: f64, max: f64, step: Option<f64>) -> CmdData {
    CmdData::SetpointConstraintsListData(SetpointConstraintsListData {
        setpoint_constraints_data: Some(vec![SetpointConstraintsData {
            setpoint_id: Some(SETPOINT_ID),
            setpoint_range_min: Some(ScaledNumber::from_f64(min, 1)),
            setpoint_range_max: Some(ScaledNumber::from_f64(max, 1)),
            setpoint_step_size: step.map(|step| ScaledNumber::from_f64(step, 1)),
        }]),
    })
}

/// The current setpoint (Table 9), in the unit the description named.
pub fn setpoint_value(degrees: f64) -> CmdData {
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(vec![SetpointData {
            setpoint_id: Some(SETPOINT_ID),
            value: Some(ScaledNumber::from_f64(degrees, 1)),
            is_setpoint_changeable: Some(true),
            ..Default::default()
        }]),
    })
}

/// Which setpoints each operation mode uses (Table 10).
///
/// The relations are refused rather than published when one breaks §2.3.1.1 — see
/// [`permitted_setpoints`]. A circuit that told a Configuration Appliance that `on` maps
/// to two setpoints has said nothing usable, and the appliance would have to guess which
/// of them its write will take effect on.
pub fn system_function_relations(
    relations: &[(HvacOperationModeId, HvacOperationModeType, Vec<SetpointId>)],
) -> Option<CmdData> {
    let mut data = Vec::new();
    for (id, mode, setpoints) in relations {
        if !relation_is_valid(mode, setpoints) {
            return None;
        }
        data.push(HvacSystemFunctionSetpointRelationData {
            system_function_id: Some(SYSTEM_FUNCTION_ID),
            operation_mode_id: Some(*id),
            setpoint_id: (!setpoints.is_empty()).then(|| setpoints.clone()),
        });
    }
    Some(CmdData::HvacSystemFunctionSetpointRelationListData(
        HvacSystemFunctionSetpointRelationListData {
            hvac_system_function_setpoint_relation_data: Some(data),
        },
    ))
}

/// Reads a `setpointListData` write as a temperature.
///
/// `id` is the identifier on the device the payload belongs to — this circuit's own
/// [`SETPOINT_ID`] when it reads a write addressed to it.
///
/// **Give this the resolved state, not a partial update**, for the reason every reader in
/// this crate says so: an omitted element means *unchanged* (SPINE IG §3.3), and a
/// fragment read as a whole value is a temperature nobody asked for.
pub fn read_setpoint_write(data: &CmdData, id: SetpointId) -> Option<f64> {
    let CmdData::SetpointListData(list) = data else {
        return None;
    };
    list.setpoint_data
        .iter()
        .flatten()
        .find(|entry| entry.setpoint_id == Some(id))?
        .value
        .as_ref()
        .and_then(ScaledNumber::to_f64)
}

// ---- what a Configuration Appliance reads -------------------------------------------

/// What a DHW Circuit will accept for one setpoint (Table 8).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Constraints {
    /// The lowest temperature the circuit accepts.
    pub min: f64,
    /// The highest.
    pub max: f64,
    /// The granularity, where the circuit published one.
    pub step: Option<f64>,
}

impl Constraints {
    /// Whether a temperature is inside the range.
    ///
    /// The step size is deliberately *not* part of this: a value off the step is rounded
    /// by the server (Table 8), so it is accepted rather than refused. Use
    /// [`rounded`](Self::rounded) to find out what it will become.
    pub fn permits(&self, degrees: f64) -> bool {
        degrees >= self.min && degrees <= self.max
    }

    /// The value the circuit will actually hold, given its step size.
    pub fn rounded(&self, degrees: f64) -> f64 {
        let Some(step) = self.step.filter(|step| *step > 0.0) else {
            return degrees;
        };
        let steps = crate::model::round_half_away((degrees - self.min) / step);
        (self.min + steps * step).clamp(self.min, self.max)
    }
}

/// What a write to one setpoint would actually do, given the mode the circuit is in.
///
/// CDT addresses its setpoints *through* the operation modes: Table 10 relates each mode
/// to the setpoints it reads, and a setpoint the current mode does not read can be
/// written, acknowledged, and change nothing anybody can measure. Nothing on the wire
/// says so — the circuit answers the write the same way either way — which is why this
/// exists and why [`mdsf`](super::mdsf) is not optional equipment for a manager that
/// writes temperatures.
///
/// Computed by [`DhwSetpoints::effect_of`]; [`DhwSetpoints::write_effective`] refuses
/// everything but [`Effective`](Self::Effective).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetpointEffect {
    /// The circuit is in a mode that reads this setpoint. The write changes the water.
    Effective,
    /// The circuit is in a mode that reads some other setpoint, or none.
    ///
    /// The write would be applied and acknowledged and would heat nothing. `off` relating
    /// to no setpoint at all is the extreme case, and [CDT-003/3] allows it.
    NotInCurrentMode,
    /// A one-time heating is overriding the mode right now (MDSF Table 12).
    ///
    /// The setpoint *is* the one the current mode reads, so the write lands where it was
    /// meant to — but the circuit is running an overrun over the top of it, and the water
    /// will follow the overrun until that finishes. Distinct from
    /// [`NotInCurrentMode`](Self::NotInCurrentMode) because the write is not misdirected:
    /// it takes effect, later.
    OverriddenByOverrun,
    /// Not enough is known to say.
    ///
    /// MDSF has not reported a current mode, or CDT's mode-to-setpoint relations have not
    /// arrived. Both are read once at commissioning; until they have, a manager writing a
    /// temperature is guessing.
    Unknown,
}

impl SetpointEffect {
    /// Whether the write would reach the water.
    pub fn is_effective(self) -> bool {
        self == Self::Effective
    }
}

/// Why a temperature could not be written.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum WriteRefused {
    /// The circuit published no such setpoint, or has not published its description yet.
    #[error("the circuit has published no such DHW temperature setpoint")]
    UnknownSetpoint,
    /// The circuit is not in an operation mode that reads this setpoint.
    ///
    /// Raised only by [`DhwSetpoints::write_effective`]. The plain
    /// [`write`](DhwSetpoints::write) does not check it — a manager that means to
    /// pre-load the setpoint of a mode it is about to ask for is doing something
    /// sensible, and this is the refusal for the manager that expects hot water now.
    #[error("the circuit is in an operation mode that does not read this setpoint")]
    NotInCurrentMode,
    /// MDSF has not said which mode the circuit is in, or the relations have not arrived.
    #[error("which operation mode the circuit is in is not known yet")]
    ModeUnknown,
    /// The circuit published no constraints for it, so nothing can be checked.
    #[error("the circuit has not published what this setpoint accepts")]
    NoConstraints,
    /// Outside [`Constraints::min`]..=[`Constraints::max`].
    #[error("{degrees} is outside the {min}..={max} the circuit accepts")]
    OutOfRange {
        /// What was asked for.
        degrees: f64,
        /// The lowest the circuit accepts.
        min: f64,
        /// The highest.
        max: f64,
    },
}

/// The Configuration Appliance's view of a DHW Circuit's setpoints.
///
/// Three functions say between them which setpoint to write and what it will accept, and
/// none of them says it alone:
///
/// * `setpointDescriptionListData` — which `setpointId`s are DHW temperatures at all, and
///   in which unit.
/// * `setpointConstraintsListData` — the range and step of each.
/// * `hvacSystemFunctionSetpointRelationListData` — which of them the circuit uses in
///   which operation mode.
///
/// Feed it every payload that arrives from the circuit's `Setpoint` and `HVAC` features,
/// in whatever order they come.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DhwSetpoints {
    /// Setpoints whose description said `dhwTemperature`, with the unit each is in.
    temperatures: Vec<(SetpointId, Option<UnitOfMeasurement>)>,
    constraints: Vec<(SetpointId, Constraints)>,
    values: Vec<(SetpointId, f64)>,
    relations: Vec<(HvacOperationModeId, Vec<SetpointId>)>,
}

impl DhwSetpoints {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes in one payload, and reports whether it was one this use case carries.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        match data {
            CmdData::SetpointDescriptionListData(list) => {
                for entry in list.setpoint_description_data.iter().flatten() {
                    let Some(id) = entry.setpoint_id else {
                        continue;
                    };
                    // Only the DHW temperatures. A `Setpoint` feature carries whatever
                    // setpoints its device has, and the room heating one next door is a
                    // temperature in the same unit with a completely different meaning.
                    if entry.scope_type.as_ref() != Some(&TEMPERATURE_SCOPE) {
                        self.temperatures.retain(|(known, _)| *known != id);
                        continue;
                    }
                    let unit = entry.unit.clone();
                    match self.temperatures.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = unit,
                        None => self.temperatures.push((id, unit)),
                    }
                }
                true
            }
            CmdData::SetpointConstraintsListData(list) => {
                for entry in list.setpoint_constraints_data.iter().flatten() {
                    let (Some(id), Some(min), Some(max)) = (
                        entry.setpoint_id,
                        entry
                            .setpoint_range_min
                            .as_ref()
                            .and_then(ScaledNumber::to_f64),
                        entry
                            .setpoint_range_max
                            .as_ref()
                            .and_then(ScaledNumber::to_f64),
                    ) else {
                        continue;
                    };
                    let found = Constraints {
                        min,
                        max,
                        step: entry
                            .setpoint_step_size
                            .as_ref()
                            .and_then(ScaledNumber::to_f64),
                    };
                    match self.constraints.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = found,
                        None => self.constraints.push((id, found)),
                    }
                }
                true
            }
            CmdData::SetpointListData(list) => {
                for entry in list.setpoint_data.iter().flatten() {
                    let (Some(id), Some(value)) = (
                        entry.setpoint_id,
                        entry.value.as_ref().and_then(ScaledNumber::to_f64),
                    ) else {
                        continue;
                    };
                    match self.values.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = value,
                        None => self.values.push((id, value)),
                    }
                }
                true
            }
            CmdData::HvacSystemFunctionSetpointRelationListData(list) => {
                for entry in list
                    .hvac_system_function_setpoint_relation_data
                    .iter()
                    .flatten()
                {
                    let Some(mode) = entry.operation_mode_id else {
                        continue;
                    };
                    let setpoints = entry.setpoint_id.clone().unwrap_or_default();
                    match self.relations.iter_mut().find(|(known, _)| *known == mode) {
                        Some((_, stored)) => *stored = setpoints,
                        None => self.relations.push((mode, setpoints)),
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The circuit's setpoints that are DHW temperatures, in the order it described them.
    pub fn temperature_setpoints(&self) -> impl Iterator<Item = SetpointId> + '_ {
        self.temperatures.iter().map(|(id, _)| *id)
    }

    /// The unit one setpoint is expressed in, where the circuit named one.
    ///
    /// Worth checking rather than assuming: Table 7 permits `degC`, `degF` and `K`, and
    /// writing 60 to a circuit working in Fahrenheit is a cold shower.
    pub fn unit_of(&self, id: SetpointId) -> Option<&UnitOfMeasurement> {
        self.temperatures
            .iter()
            .find(|(known, _)| *known == id)?
            .1
            .as_ref()
    }

    /// What the circuit will accept for one setpoint.
    pub fn constraints_of(&self, id: SetpointId) -> Option<Constraints> {
        self.constraints
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, constraints)| *constraints)
    }

    /// The temperature the circuit currently holds for one setpoint.
    pub fn temperature(&self, id: SetpointId) -> Option<f64> {
        self.values
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, value)| *value)
    }

    /// The setpoints one operation mode uses (Table 10).
    ///
    /// Empty for `off` where the circuit relates it to none, and for a mode the circuit
    /// has not described. A mode mapping to several setpoints is `auto`, where which one
    /// applies is the circuit's own business [CDT-003/1].
    pub fn for_mode(&self, mode: HvacOperationModeId) -> &[SetpointId] {
        self.relations
            .iter()
            .find(|(known, _)| *known == mode)
            .map(|(_, setpoints)| setpoints.as_slice())
            .unwrap_or_default()
    }

    /// Builds the write that sets one setpoint, refusing what the circuit would not take.
    ///
    /// The range is checked here rather than on the wire, for the reason the failsafe
    /// duration is: the bound is the circuit's own published constraint, and a write
    /// outside it comes back as a bare error number that a caller then has to interpret.
    /// A value off the step size is *not* refused — Table 8 makes rounding the server's
    /// job — but [`Constraints::rounded`] says what it will become.
    pub fn write(&self, id: SetpointId, degrees: f64) -> Result<CmdData, WriteRefused> {
        if !self.temperatures.iter().any(|(known, _)| *known == id) {
            return Err(WriteRefused::UnknownSetpoint);
        }
        let constraints = self.constraints_of(id).ok_or(WriteRefused::NoConstraints)?;
        if !constraints.permits(degrees) {
            return Err(WriteRefused::OutOfRange {
                degrees,
                min: constraints.min,
                max: constraints.max,
            });
        }
        Ok(CmdData::SetpointListData(SetpointListData {
            setpoint_data: Some(vec![SetpointData {
                setpoint_id: Some(id),
                value: Some(ScaledNumber::from_f64(degrees, 1)),
                ..Default::default()
            }]),
        }))
    }

    /// What writing this setpoint would actually do, given the mode the circuit is in.
    ///
    /// The join CDT cannot make on its own: `state` is what
    /// [`mdsf`](super::mdsf) learned, and Table 10's relations are what this holds. See
    /// [`SetpointEffect`] for what each answer means.
    pub fn effect_of(
        &self,
        id: SetpointId,
        state: &super::mdsf::DhwSystemFunction,
    ) -> SetpointEffect {
        let Some(mode) = state.mode_id() else {
            return SetpointEffect::Unknown;
        };
        let reads = self.for_mode(mode);
        if reads.is_empty() && !self.relations.iter().any(|(known, _)| *known == mode) {
            // The mode is known and its relations are not: a mode that relates to no
            // setpoint and a mode nobody has described look the same from `for_mode`
            // alone, and only one of them is an answer.
            return SetpointEffect::Unknown;
        }
        if !reads.contains(&id) {
            return SetpointEffect::NotInCurrentMode;
        }
        if state.overrun_active() == Some(true) {
            return SetpointEffect::OverriddenByOverrun;
        }
        SetpointEffect::Effective
    }

    /// Builds the write, and refuses one the circuit would apply without heating anything.
    ///
    /// [`write`](Self::write) checks what the *circuit* published about the setpoint —
    /// that it exists, and that the value is inside its constraints. This checks the one
    /// thing neither the setpoint nor the wire can tell you: whether the circuit is in an
    /// operation mode that reads it. A write into a mode the circuit is not in is
    /// applied, acknowledged and changes nothing, and every consumer otherwise finds that
    /// out the same way — a box that reports success and heats no water.
    ///
    /// It is the same shape as the limitation actor refusing a limit with no recent
    /// heartbeat: the message would be accepted, and accepting it would mean nothing.
    ///
    /// An overrun in progress is **not** refused. The write reaches the setpoint the
    /// current mode reads, and the one-time heating running over the top of it finishes;
    /// use [`effect_of`](Self::effect_of) where the difference matters.
    ///
    /// ```
    /// # use eebus::usecases::hvac::{cdt::{self, DhwSetpoints, SetpointEffect, WriteRefused}, mdsf::DhwSystemFunction};
    /// # fn example(known: &DhwSetpoints, state: &DhwSystemFunction, dhw: eebus::model::SetpointId) {
    /// match known.write_effective(dhw, 60.0, state) {
    ///     Ok(write) => { /* send it */ }
    ///     Err(WriteRefused::NotInCurrentMode) => { /* ask for the mode first */ }
    ///     Err(other) => { /* the circuit would refuse it anyway */ }
    /// }
    /// # }
    /// ```
    pub fn write_effective(
        &self,
        id: SetpointId,
        degrees: f64,
        state: &super::mdsf::DhwSystemFunction,
    ) -> Result<CmdData, WriteRefused> {
        match self.effect_of(id, state) {
            SetpointEffect::Effective | SetpointEffect::OverriddenByOverrun => {}
            SetpointEffect::NotInCurrentMode => return Err(WriteRefused::NotInCurrentMode),
            SetpointEffect::Unknown => return Err(WriteRefused::ModeUnknown),
        }
        self.write(id, degrees)
    }
}

// ---- descriptors ---------------------------------------------------------------------

/// A DHW Circuit lives on its own entity type (§3.2.1.1).
const DHW_CIRCUIT_ENTITIES: &[EntityType] = &[EntityType::DHWCircuit];

/// The Configuration Appliance may sit behind any entity (Figure 5, `entityType = <any>`).
const CONFIGURATION_APPLIANCE_ENTITIES: &[EntityType] = &[];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::server(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::server_writeable(FeatureType::Setpoint, Function::SetpointListData),
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionSetpointRelationListData,
    ),
];

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointDescriptionListData),
    FunctionUse::client(FeatureType::Setpoint, Function::SetpointConstraintsListData),
    FunctionUse::client_writes(FeatureType::Setpoint, Function::SetpointListData),
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionSetpointRelationListData,
    ),
];

const SCENARIO_NAME: &str = "Set DHW temperature setpoint";

/// The DHW Circuit: the actor whose temperature is set.
pub static DHW_CIRCUIT: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: DHW_CIRCUIT_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: DHW_CIRCUIT_ENTITIES,
    counterpart: CONFIGURATION_APPLIANCE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: SERVER_FUNCTIONS,
    }],
};

/// The Configuration Appliance: the actor that sets it.
pub static CONFIGURATION_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CONFIGURATION_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CONFIGURATION_APPLIANCE_ENTITIES,
    counterpart: DHW_CIRCUIT_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: CLIENT_FUNCTIONS,
    }],
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 7: the elements the specification fixes are the ones a client matches on.
    #[test]
    fn the_description_says_what_the_setpoint_is() {
        let CmdData::SetpointDescriptionListData(list) =
            setpoint_description(UnitOfMeasurement::DegC)
        else {
            panic!("expected the descriptions");
        };
        let entry = &list.setpoint_description_data.as_ref().unwrap()[0];
        assert_eq!(entry.scope_type.as_ref(), Some(&TEMPERATURE_SCOPE));
        assert_eq!(
            entry.setpoint_type.as_ref(),
            Some(&SetpointType::ValueAbsolute)
        );
        assert_eq!(entry.unit.as_ref(), Some(&UnitOfMeasurement::DegC));
        assert!(
            entry.measurement_id.is_none(),
            "a FOREIGN IDENTIFIER with nothing to point at is omitted, not invented"
        );
    }

    /// [CDT-001]: what is written comes back.
    #[test]
    fn cdt_001_the_temperature_round_trips() {
        let published = setpoint_value(58.5);
        assert_eq!(read_setpoint_write(&published, SETPOINT_ID), Some(58.5));
        assert_eq!(
            read_setpoint_write(&published, SetpointId(4)),
            None,
            "another setpoint of the circuit's is not this one"
        );
    }

    /// A circuit that numbers its DHW setpoint anything but 1 is read correctly.
    ///
    /// Table 7's `<st1#(1..4)>` is a placeholder, and a `Setpoint` feature carries every
    /// setpoint the device has — a room air temperature is the same `valueAbsolute` in
    /// the same `degC`, and writing 60 into it is a very warm living room.
    #[test]
    fn the_dhw_setpoint_is_found_by_its_scope_and_not_by_our_identifier() {
        let theirs = SetpointId(3);
        let descriptions = CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
            setpoint_description_data: Some(vec![
                SetpointDescriptionData {
                    setpoint_id: Some(SETPOINT_ID),
                    setpoint_type: Some(SetpointType::ValueAbsolute),
                    unit: Some(UnitOfMeasurement::DegC),
                    scope_type: Some(ScopeType::RoomAirTemperature),
                    ..Default::default()
                },
                SetpointDescriptionData {
                    setpoint_id: Some(theirs),
                    setpoint_type: Some(SetpointType::ValueAbsolute),
                    unit: Some(UnitOfMeasurement::DegC),
                    scope_type: Some(TEMPERATURE_SCOPE),
                    ..Default::default()
                },
            ]),
        });

        let mut known = DhwSetpoints::new();
        assert!(known.learn(&descriptions));
        assert_eq!(
            known.temperature_setpoints().collect::<Vec<_>>(),
            [theirs],
            "the room temperature on setpoint 1 is not a DHW temperature"
        );
        assert_eq!(known.unit_of(theirs), Some(&UnitOfMeasurement::DegC));
    }

    /// A write outside what the circuit published is refused here, not on the wire.
    #[test]
    fn a_temperature_outside_the_published_range_is_refused() {
        let mut known = DhwSetpoints::new();
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));

        assert_eq!(
            known.write(SETPOINT_ID, 60.0),
            Err(WriteRefused::NoConstraints),
            "nothing can be checked until the circuit says what it accepts"
        );

        known.learn(&setpoint_constraints(40.0, 65.0, Some(0.5)));
        assert!(known.write(SETPOINT_ID, 60.0).is_ok());
        assert!(matches!(
            known.write(SETPOINT_ID, 70.0),
            Err(WriteRefused::OutOfRange { .. })
        ));
        assert_eq!(
            known.write(SetpointId(2), 60.0),
            Err(WriteRefused::UnknownSetpoint)
        );
    }

    /// Table 8: a value off the step size is rounded by the server, so it is not refused —
    /// but a caller can find out what it will get.
    #[test]
    fn a_value_off_the_step_size_is_accepted_and_rounds() {
        let mut known = DhwSetpoints::new();
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));
        known.learn(&setpoint_constraints(40.0, 65.0, Some(0.5)));

        let constraints = known.constraints_of(SETPOINT_ID).expect("published");
        assert!(constraints.permits(60.2), "in range, if not on the step");
        assert_eq!(constraints.rounded(60.2), 60.0);
        assert_eq!(constraints.rounded(60.3), 60.5);
        assert!(known.write(SETPOINT_ID, 60.2).is_ok());
    }

    /// A value that arrives before its description is kept, not dropped.
    #[test]
    fn the_three_functions_may_arrive_in_any_order() {
        let mut known = DhwSetpoints::new();
        known.learn(&setpoint_value(58.0));
        assert_eq!(
            known.temperature_setpoints().count(),
            0,
            "nothing is a DHW setpoint until a description says so"
        );

        known.learn(&setpoint_constraints(40.0, 65.0, None));
        known.learn(&setpoint_description(UnitOfMeasurement::DegC));

        assert_eq!(known.temperature(SETPOINT_ID), Some(58.0));
        assert!(known.write(SETPOINT_ID, 62.0).is_ok());
    }

    /// §2.3.1.1: `on` and `eco` relate to exactly one setpoint, `auto` to up to four.
    #[test]
    fn cdt_003_the_operation_mode_rules_are_enforced() {
        let one = vec![SETPOINT_ID];
        let two = vec![SETPOINT_ID, SetpointId(2)];

        assert!(relation_is_valid(&HvacOperationModeType::On, &one));
        assert!(!relation_is_valid(&HvacOperationModeType::On, &two));
        assert!(!relation_is_valid(&HvacOperationModeType::Eco, &[]));
        assert!(relation_is_valid(&HvacOperationModeType::Auto, &two));
        assert!(relation_is_valid(&HvacOperationModeType::Off, &[]));
        assert!(!relation_is_valid(&HvacOperationModeType::Off, &two));

        assert!(
            system_function_relations(&[(
                HvacOperationModeId(1),
                HvacOperationModeType::On,
                two.clone()
            )])
            .is_none(),
            "a relation that breaks the rule is not published"
        );
    }

    /// The relations tell a Configuration Appliance which setpoint its write will hit.
    #[test]
    fn the_relations_say_which_setpoint_a_mode_uses() {
        let eco = HvacOperationModeId(2);
        let payload = system_function_relations(&[
            (
                HvacOperationModeId(1),
                HvacOperationModeType::Auto,
                vec![SETPOINT_ID, SetpointId(2)],
            ),
            (eco, HvacOperationModeType::Eco, vec![SetpointId(2)]),
            (HvacOperationModeId(3), HvacOperationModeType::Off, vec![]),
        ])
        .expect("every relation is well formed");

        let mut known = DhwSetpoints::new();
        assert!(known.learn(&payload));
        assert_eq!(known.for_mode(eco), [SetpointId(2)]);
        assert_eq!(known.for_mode(HvacOperationModeId(3)), []);
        assert_eq!(
            known.for_mode(HvacOperationModeId(9)),
            [],
            "a mode the circuit never described"
        );
    }

    /// Both actors announce the one scenario, and it is mandatory for each (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&DHW_CIRCUIT, &CONFIGURATION_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(
                descriptor.required_scenarios().collect::<Vec<_>>(),
                [1],
                "Table 1 marks scenario 1 M for both"
            );
        }
        assert_eq!(DHW_CIRCUIT.counterpart, CONFIGURATION_APPLIANCE.actor);
        assert_eq!(CONFIGURATION_APPLIANCE.counterpart, DHW_CIRCUIT.actor);
        assert!(DHW_CIRCUIT.permits_entity(&EntityType::DHWCircuit));
        assert!(
            CONFIGURATION_APPLIANCE.permits_entity(&EntityType::CEM),
            "Figure 5 places the appliance behind any entity type"
        );
    }
}
