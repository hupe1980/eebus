//! The one exchange behind the three temperature-configuration use cases.
//!
//! A *Configuration Appliance* writes a temperature setpoint and the server applies it
//! within the range and step size it published. Three use cases are that one idea:
//!
//! | | server actor | `scopeType` | system function |
//! |---|---|---|---|
//! | [`cdt`](super::cdt) — hot water | `DHWCircuit` | `dhwTemperature` | `dhw` |
//! | [`crht`](super::crht) — room heating | `HVACRoom` | `roomAirTemperature` | `heating` |
//! | [`crct`](super::crct) — room cooling | `HVACRoom` | `roomAirTemperature` | `cooling` |
//!
//! **The last two share a scope**, which is the thing to understand about this family: a
//! room's heating setpoint and its cooling setpoint are both `roomAirTemperature` in the
//! same unit on the same `Setpoint` feature, and nothing in their descriptions tells them
//! apart. What tells them apart is the *relation* —
//! `hvacSystemFunctionSetpointRelationData` is keyed by `systemFunctionId` (PRIMARY) and
//! `operationModeId` (SUB) — so "which setpoint do I write" is only answerable once the
//! system function is known. Writing a heating setpoint into the cooling function is a
//! well-formed request that makes a room colder when it was asked to be warmer.
//!
//! [`Setpoints`] is the reader, and it keys its relations by both identifiers for exactly
//! that reason.
//!
//! # Three functions, and none of them is enough alone
//!
//! * `setpointDescriptionListData` — which `setpointId`s carry this temperature at all, and
//!   in which unit. `<st1#(1..4)>`: a server publishes **one to four** and numbers them
//!   itself.
//! * `setpointConstraintsListData` — the range and step size of each.
//! * `hvacSystemFunctionSetpointRelationListData` — which of them each operation mode of
//!   each system function reads.
//!
//! See [`crate::usecases::addressing`] for why none of this may be shortcut.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacSystemFunctionId, HvacSystemFunctionSetpointRelationData,
    HvacSystemFunctionSetpointRelationListData, Role, ScaledNumber, ScopeType,
    SetpointConstraintsData, SetpointConstraintsListData, SetpointData, SetpointDescriptionData,
    SetpointDescriptionListData, SetpointId, SetpointListData, SetpointType, UnitOfMeasurement,
};
use crate::spine::{LocalFeature, Operations};

/// The units a temperature may be published in.
pub const UNITS: [UnitOfMeasurement; 3] = [
    UnitOfMeasurement::DegC,
    UnitOfMeasurement::DegF,
    UnitOfMeasurement::K,
];

/// How many temperature setpoints an operation mode may relate to.
///
/// §2.3.1.1 gives three different rules — the same three in all three specifications — and
/// they are not decoration: a mode that relates to the wrong number of setpoints is a
/// server whose behaviour a Configuration Appliance cannot predict.
///
/// * `auto` — one to four [xDT-003/1]. Which of them the server picks, and when, is
///   vendor-specific and deliberately outside these use cases.
/// * `on` and `eco` — exactly one [xDT-003/2].
/// * `off` — none, or exactly one [xDT-003/3].
pub fn permitted_setpoints(mode: &HvacOperationModeType) -> core::ops::RangeInclusive<usize> {
    match mode {
        HvacOperationModeType::Auto => 1..=4,
        HvacOperationModeType::On | HvacOperationModeType::Eco => 1..=1,
        HvacOperationModeType::Off => 0..=1,
        // The list is open, and a mode this version does not name carries no rule of its
        // own beyond the table's `setpointId (1..4)`.
        _ => 0..=4,
    }
}

/// One operation mode of a system function, and the setpoints it reads.
///
/// What [`relations`] and [`relations_of`] take, and what the peer's
/// `hvacSystemFunctionSetpointRelationData` carries: the mode's identifier, its type — which
/// is what §2.3.1.1's cardinality rules are stated against — and the setpoints.
pub type ModeRelation = (HvacOperationModeId, HvacOperationModeType, Vec<SetpointId>);

/// Whether a relation between an operation mode and its setpoints is well formed.
pub fn relation_is_valid(mode: &HvacOperationModeType, setpoints: &[SetpointId]) -> bool {
    permitted_setpoints(mode).contains(&setpoints.len())
}

// ---- the features the server serves ----------------------------------------------------

/// Builds the `Setpoint` feature scenario 1 is served from.
///
/// `setpointListData` is the only writeable function: the description and the constraints
/// are the server's own statement of what it is and what it will accept, and a
/// Configuration Appliance that could rewrite them could talk itself into any temperature.
///
/// **Writes need no binding**, and that is §3.4.1.1 rather than a relaxation: "Binding
/// SHOULD NOT be used for this Scenario". A server that insisted on one would refuse every
/// conformant Configuration Appliance's write with `errorNumber` 9 — the use case simply
/// would not run. The decision that remains is the application's: writes here are deferred,
/// so a product that wants "only the manager I was commissioned with" enforces it where it
/// can see who is asking. See [`WriteBinding`](crate::spine::WriteBinding).
pub fn setpoint_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::Setpoint, Role::Server)
        .with_deferred_writes()
        .with_unbound_writes()
        .with_function(Function::SetpointDescriptionListData, Operations::read())
        .with_function(Function::SetpointConstraintsListData, Operations::read())
        .with_function(Function::SetpointListData, Operations::read_write())
}

/// Builds the `HVAC` feature that says which setpoint belongs to which operation mode.
///
/// A server that also serves one of the system-function use cases — which [CDT-005] and
/// its counterparts make mandatory beside these — puts both on the *same* feature: an
/// entity holds at most one `HVAC`. Use
/// [`system_function::with_setpoint_relations`](super::system_function::with_setpoint_relations)
/// for that.
pub fn hvac_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::HVAC, Role::Server).with_function(
        Function::HvacSystemFunctionSetpointRelationListData,
        Operations::read(),
    )
}

// ---- what the server publishes ----------------------------------------------------------

/// One setpoint's description.
///
/// `measurement` is a FOREIGN IDENTIFIER pointing at the measurement of the same
/// temperature, and every specification in this family requires it to be *the same number*
/// the matching monitoring use case publishes — `mdt` for the hot water, `mrt` for a room.
/// A server that does not serve that use case passes [`None`] rather than inventing a link
/// that leads nowhere.
pub fn description(
    id: SetpointId,
    scope: ScopeType,
    unit: UnitOfMeasurement,
    measurement: Option<crate::model::MeasurementId>,
) -> CmdData {
    CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
        setpoint_description_data: Some(vec![SetpointDescriptionData {
            setpoint_id: Some(id),
            measurement_id: measurement,
            setpoint_type: Some(SetpointType::ValueAbsolute),
            unit: Some(unit),
            scope_type: Some(scope),
            ..Default::default()
        }]),
    })
}

/// Several descriptions in one payload.
///
/// What a room that both heats and cools has to publish: one
/// `setpointDescriptionListData` carrying every setpoint the device has. Publishing them
/// one at a time replaces rather than adds.
pub fn descriptions(
    entries: &[(
        SetpointId,
        ScopeType,
        UnitOfMeasurement,
        Option<crate::model::MeasurementId>,
    )],
) -> CmdData {
    CmdData::SetpointDescriptionListData(SetpointDescriptionListData {
        setpoint_description_data: Some(
            entries
                .iter()
                .map(|(id, scope, unit, measurement)| SetpointDescriptionData {
                    setpoint_id: Some(*id),
                    measurement_id: *measurement,
                    setpoint_type: Some(SetpointType::ValueAbsolute),
                    unit: Some(unit.clone()),
                    scope_type: Some(scope.clone()),
                    ..Default::default()
                })
                .collect(),
        ),
    })
}

/// Several sets of constraints in one payload.
pub fn constraints_of(entries: &[(SetpointId, f64, f64, Option<f64>)]) -> CmdData {
    CmdData::SetpointConstraintsListData(SetpointConstraintsListData {
        setpoint_constraints_data: Some(
            entries
                .iter()
                .map(|(id, min, max, step)| SetpointConstraintsData {
                    setpoint_id: Some(*id),
                    setpoint_range_min: Some(ScaledNumber::from_f64(*min, 1)),
                    setpoint_range_max: Some(ScaledNumber::from_f64(*max, 1)),
                    setpoint_step_size: step.map(|step| ScaledNumber::from_f64(step, 1)),
                })
                .collect(),
        ),
    })
}

/// Several current values in one payload.
pub fn values(entries: &[(SetpointId, f64)]) -> CmdData {
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(
            entries
                .iter()
                .map(|(id, degrees)| SetpointData {
                    setpoint_id: Some(*id),
                    value: Some(ScaledNumber::from_f64(*degrees, 1)),
                    is_setpoint_changeable: Some(true),
                    ..Default::default()
                })
                .collect(),
        ),
    })
}

/// What the server will accept for one setpoint.
///
/// `step` is `R` rather than `M`: a server that accepts any value inside the range omits
/// it. Where it is published, a value that does not match SHALL be rounded **by the
/// server** — so a write off the step is not an error, and a Configuration Appliance that
/// wants to know what it will get should round before it sends.
pub fn constraints(id: SetpointId, min: f64, max: f64, step: Option<f64>) -> CmdData {
    CmdData::SetpointConstraintsListData(SetpointConstraintsListData {
        setpoint_constraints_data: Some(vec![SetpointConstraintsData {
            setpoint_id: Some(id),
            setpoint_range_min: Some(ScaledNumber::from_f64(min, 1)),
            setpoint_range_max: Some(ScaledNumber::from_f64(max, 1)),
            setpoint_step_size: step.map(|step| ScaledNumber::from_f64(step, 1)),
        }]),
    })
}

/// The current setpoint, in the unit the description named.
pub fn value(id: SetpointId, degrees: f64) -> CmdData {
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(vec![SetpointData {
            setpoint_id: Some(id),
            value: Some(ScaledNumber::from_f64(degrees, 1)),
            is_setpoint_changeable: Some(true),
            ..Default::default()
        }]),
    })
}

/// Which setpoints each operation mode of one system function uses.
///
/// [`None`] where a relation breaks §2.3.1.1 — see [`permitted_setpoints`]. A server that
/// told a Configuration Appliance that `on` maps to two setpoints has said nothing usable,
/// and the appliance would have to guess which of them its write takes effect on.
///
/// `function` is what keeps a room's heating relations apart from its cooling ones. Both
/// are `roomAirTemperature`, and it is the only thing that does.
pub fn relations(function: HvacSystemFunctionId, modes: &[ModeRelation]) -> Option<CmdData> {
    let mut data = Vec::new();
    for (id, mode, setpoints) in modes {
        if !relation_is_valid(mode, setpoints) {
            return None;
        }
        data.push(HvacSystemFunctionSetpointRelationData {
            system_function_id: Some(function),
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

/// The relations of **several** system functions, in one payload.
///
/// The one a room that both heats and cools must publish, and the reason the reader keys by
/// the pair: this list carries `(heating, auto) → [3]` and `(cooling, auto) → [4]` side by
/// side, and only the `systemFunctionId` distinguishes them.
pub fn relations_of(functions: &[(HvacSystemFunctionId, &[ModeRelation])]) -> Option<CmdData> {
    let mut data = Vec::new();
    for (function, modes) in functions {
        for (id, mode, setpoints) in *modes {
            if !relation_is_valid(mode, setpoints) {
                return None;
            }
            data.push(HvacSystemFunctionSetpointRelationData {
                system_function_id: Some(*function),
                operation_mode_id: Some(*id),
                setpoint_id: (!setpoints.is_empty()).then(|| setpoints.clone()),
            });
        }
    }
    Some(CmdData::HvacSystemFunctionSetpointRelationListData(
        HvacSystemFunctionSetpointRelationListData {
            hvac_system_function_setpoint_relation_data: Some(data),
        },
    ))
}

/// Reads a `setpointListData` write as a temperature.
///
/// `id` is the identifier on the device the payload belongs to — the server's own, when it
/// reads a write addressed to it.
///
/// **Give this the resolved state, not a partial update**, for the reason every reader in
/// this crate says so: an omitted element means *unchanged* (SPINE IG §3.3), and a fragment
/// read as a whole value is a temperature nobody asked for.
pub fn read_write(data: &CmdData, id: SetpointId) -> Option<f64> {
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

// ---- what a Configuration Appliance reads ------------------------------------------------

/// What a server will accept for one setpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Constraints {
    /// The lowest temperature the server accepts.
    pub min: f64,
    /// The highest.
    pub max: f64,
    /// The granularity, where the server published one.
    pub step: Option<f64>,
}

impl Constraints {
    /// Whether a temperature is inside the range.
    ///
    /// The step size is deliberately *not* part of this: a value off the step is rounded by
    /// the server, so it is accepted rather than refused. Use [`rounded`](Self::rounded) to
    /// find out what it will become.
    pub fn permits(&self, degrees: f64) -> bool {
        degrees >= self.min && degrees <= self.max
    }

    /// The value the server will actually hold, given its step size.
    pub fn rounded(&self, degrees: f64) -> f64 {
        let Some(step) = self.step.filter(|step| *step > 0.0) else {
            return degrees;
        };
        let steps = crate::model::round_half_away((degrees - self.min) / step);
        (self.min + steps * step).clamp(self.min, self.max)
    }
}

/// What a write to one setpoint would actually do, given the mode the server is in.
///
/// These use cases address their setpoints *through* the operation modes: the relations
/// tie each mode to the setpoints it reads, and a setpoint the current mode does not read
/// can be written, acknowledged, and change nothing anybody can measure. Nothing on the
/// wire says so — the server answers the write the same way either way — which is why this
/// exists and why the matching system-function use case is not optional equipment for a
/// manager that writes temperatures.
///
/// Computed by [`Setpoints::effect_of`]; [`Setpoints::write_effective`] refuses everything
/// but [`Effective`](Self::Effective).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetpointEffect {
    /// The server is in a mode that reads this setpoint. The write changes the temperature.
    Effective,
    /// The server is in a mode that reads some other setpoint, or none.
    ///
    /// The write would be applied and acknowledged and would heat nothing. `off` relating
    /// to no setpoint at all is the extreme case, and [xDT-003/3] allows it.
    NotInCurrentMode,
    /// An overrun is overriding the mode right now.
    ///
    /// The setpoint *is* the one the current mode reads, so the write lands where it was
    /// meant to — but the server is running an overrun over the top of it, and the
    /// temperature will follow the overrun until that finishes. Distinct from
    /// [`NotInCurrentMode`](Self::NotInCurrentMode) because the write is not misdirected:
    /// it takes effect, later.
    OverriddenByOverrun,
    /// Not enough is known to say.
    ///
    /// The system-function use case has not reported a current mode, or the relations have
    /// not arrived. Both are read once at commissioning; until they have, a manager writing
    /// a temperature is guessing.
    Unknown,
}

impl SetpointEffect {
    /// Whether the write would reach the temperature.
    pub fn is_effective(self) -> bool {
        self == Self::Effective
    }
}

/// Why a temperature could not be written.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum WriteRefused {
    /// The server published no such setpoint, or has not published its description yet.
    #[error("the server has published no such temperature setpoint in this scope")]
    UnknownSetpoint,
    /// The server is not in an operation mode that reads this setpoint.
    ///
    /// Raised only by [`Setpoints::write_effective`]. The plain [`write`](Setpoints::write)
    /// does not check it — a manager that means to pre-load the setpoint of a mode it is
    /// about to ask for is doing something sensible, and this is the refusal for the
    /// manager that expects a warm room now.
    #[error("the server is in an operation mode that does not read this setpoint")]
    NotInCurrentMode,
    /// The system-function use case has not said which mode the server is in, or the
    /// relations have not arrived.
    #[error("which operation mode the server is in is not known yet")]
    ModeUnknown,
    /// The server published no constraints for it, so nothing can be checked.
    #[error("the server has not published what this setpoint accepts")]
    NoConstraints,
    /// Outside [`Constraints::min`]..=[`Constraints::max`].
    #[error("{degrees} is outside the {min}..={max} the server accepts")]
    OutOfRange {
        /// What was asked for.
        degrees: f64,
        /// The lowest the server accepts.
        min: f64,
        /// The highest.
        max: f64,
    },
}

/// A Configuration Appliance's view of one server's setpoints, in one scope.
///
/// Told at construction which `scopeType` it collects — [`Setpoints::dhw`],
/// [`Setpoints::room_air`] — because a `Setpoint` feature carries every setpoint the device
/// has, and a room air temperature is the same `valueAbsolute` in the same `degC` as the
/// hot water. Writing 60 to the wrong one heats a living room.
///
/// Feed it every payload that arrives from the server's `Setpoint` and `HVAC` features, in
/// whatever order they come.
#[derive(Clone, Debug, PartialEq)]
pub struct Setpoints {
    scope: ScopeType,
    /// Setpoints whose description matched [`scope`](Self::scope), with the unit each is in.
    temperatures: Vec<(SetpointId, Option<UnitOfMeasurement>)>,
    constraints: Vec<(SetpointId, Constraints)>,
    values: Vec<(SetpointId, f64)>,
    /// Keyed by the pair the specification keys it by: `systemFunctionId` is the PRIMARY
    /// identifier of `hvacSystemFunctionSetpointRelationData` and `operationModeId` the
    /// SUB. A device with a heating function and a cooling function may relate the *same*
    /// `operationModeId` in both — the modes are described once, device-wide — so a reader
    /// keyed by the mode alone answers a question about the heating with the cooling's
    /// setpoint.
    relations: Vec<((HvacSystemFunctionId, HvacOperationModeId), Vec<SetpointId>)>,
}

impl Setpoints {
    /// A reader collecting `dhwTemperature` setpoints ([`cdt`](super::cdt)).
    pub fn dhw() -> Self {
        Self::of(ScopeType::DhwTemperature)
    }

    /// A reader collecting `roomAirTemperature` setpoints ([`crht`](super::crht),
    /// [`crct`](super::crct)).
    ///
    /// One reader serves both room use cases: they share the scope, and it is the *system
    /// function* passed to [`for_mode`](Self::for_mode) that says whether a given setpoint
    /// is the heating one or the cooling one.
    pub fn room_air() -> Self {
        Self::of(ScopeType::RoomAirTemperature)
    }

    /// A reader collecting setpoints of any scope.
    pub fn of(scope: ScopeType) -> Self {
        Self {
            scope,
            temperatures: Vec::new(),
            constraints: Vec::new(),
            values: Vec::new(),
            relations: Vec::new(),
        }
    }

    /// Which `scopeType` this reader collects.
    pub fn scope(&self) -> &ScopeType {
        &self.scope
    }

    /// Takes in one payload, and reports whether it was one this family carries.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        match data {
            CmdData::SetpointDescriptionListData(list) => {
                for entry in list.setpoint_description_data.iter().flatten() {
                    let Some(id) = entry.setpoint_id else {
                        continue;
                    };
                    // Only this scope's. A `Setpoint` feature carries whatever setpoints
                    // its device has, and the hot water one next door is a temperature in
                    // the same unit with a completely different meaning.
                    if entry.scope_type.as_ref() != Some(&self.scope) {
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
                    let (Some(function), Some(mode)) =
                        (entry.system_function_id, entry.operation_mode_id)
                    else {
                        continue;
                    };
                    let setpoints = entry.setpoint_id.clone().unwrap_or_default();
                    let key = (function, mode);
                    match self.relations.iter_mut().find(|(known, _)| *known == key) {
                        Some((_, stored)) => *stored = setpoints,
                        None => self.relations.push((key, setpoints)),
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The server's setpoints in this scope, in the order it described them.
    pub fn temperature_setpoints(&self) -> impl Iterator<Item = SetpointId> + '_ {
        self.temperatures.iter().map(|(id, _)| *id)
    }

    /// The unit one setpoint is expressed in, where the server named one.
    ///
    /// Worth checking rather than assuming: the table permits `degC`, `degF` and `K`, and
    /// writing 60 to a circuit working in Fahrenheit is a cold shower.
    pub fn unit_of(&self, id: SetpointId) -> Option<&UnitOfMeasurement> {
        self.temperatures
            .iter()
            .find(|(known, _)| *known == id)?
            .1
            .as_ref()
    }

    /// What the server will accept for one setpoint.
    pub fn constraints_of(&self, id: SetpointId) -> Option<Constraints> {
        self.constraints
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, constraints)| *constraints)
    }

    /// The temperature the server currently holds for one setpoint.
    pub fn temperature(&self, id: SetpointId) -> Option<f64> {
        self.values
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, value)| *value)
    }

    /// The setpoints one operation mode of one system function uses.
    ///
    /// **Both identifiers**, because the relation is keyed by both: `systemFunctionId` is
    /// the PRIMARY identifier and `operationModeId` the SUB. A room that both heats and
    /// cools relates the same `auto` to a heating setpoint and to a cooling one, and only
    /// the function tells the two entries apart.
    ///
    /// Empty for `off` where the server relates it to none, and for a mode the server has
    /// not described. A mode mapping to several setpoints is `auto`, where which one
    /// applies is the server's own business [xDT-003/1].
    pub fn for_mode(
        &self,
        function: HvacSystemFunctionId,
        mode: HvacOperationModeId,
    ) -> &[SetpointId] {
        let key = (function, mode);
        self.relations
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, setpoints)| setpoints.as_slice())
            .unwrap_or_default()
    }

    /// Whether the server has described a relation for this function and mode at all.
    ///
    /// A mode that relates to no setpoint and a mode nobody has described look the same
    /// from [`for_mode`](Self::for_mode), and only one of them is an answer.
    pub fn describes(&self, function: HvacSystemFunctionId, mode: HvacOperationModeId) -> bool {
        let key = (function, mode);
        self.relations.iter().any(|(known, _)| *known == key)
    }

    /// Builds the write that sets one setpoint, refusing what the server would not take.
    ///
    /// The range is checked here rather than on the wire: the bound is the server's own
    /// published constraint, and a write outside it comes back as a bare error number that
    /// a caller then has to interpret. A value off the step size is *not* refused — the
    /// table makes rounding the server's job — but [`Constraints::rounded`] says what it
    /// will become.
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

    /// What writing this setpoint would actually do, given the mode the server is in.
    ///
    /// The join this use case cannot make on its own: `state` is what the matching
    /// system-function use case learned, and the relations are what this holds. See
    /// [`SetpointEffect`] for what each answer means.
    pub fn effect_of(
        &self,
        id: SetpointId,
        state: &super::system_function::SystemFunction,
    ) -> SetpointEffect {
        let (Some(function), Some(mode)) = (state.system_function(), state.mode_id()) else {
            return SetpointEffect::Unknown;
        };
        if !self.describes(function, mode) {
            return SetpointEffect::Unknown;
        }
        if !self.for_mode(function, mode).contains(&id) {
            return SetpointEffect::NotInCurrentMode;
        }
        if state.overrun_active() == Some(true) {
            return SetpointEffect::OverriddenByOverrun;
        }
        SetpointEffect::Effective
    }

    /// Builds the write, and refuses one the server would apply without changing anything.
    ///
    /// [`write`](Self::write) checks what the *server* published about the setpoint — that
    /// it exists, and that the value is inside its constraints. This checks the one thing
    /// neither the setpoint nor the wire can tell you: whether the server is in an
    /// operation mode that reads it. A write into a mode the server is not in is applied,
    /// acknowledged and changes nothing — a box that reports success and heats nothing. The
    /// same shape as the limitation actor refusing a limit with no recent heartbeat.
    ///
    /// An overrun in progress is **not** refused. The write reaches the setpoint the
    /// current mode reads, and the overrun running over the top of it finishes; use
    /// [`effect_of`](Self::effect_of) where the difference matters.
    pub fn write_effective(
        &self,
        id: SetpointId,
        degrees: f64,
        state: &super::system_function::SystemFunction,
    ) -> Result<CmdData, WriteRefused> {
        match self.effect_of(id, state) {
            SetpointEffect::Effective | SetpointEffect::OverriddenByOverrun => {}
            SetpointEffect::NotInCurrentMode => return Err(WriteRefused::NotInCurrentMode),
            SetpointEffect::Unknown => return Err(WriteRefused::ModeUnknown),
        }
        self.write(id, degrees)
    }
}
