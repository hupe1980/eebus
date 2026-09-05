//! The one exchange behind the six system-function use cases.
//!
//! An HVAC *system function* is one thing a heating system does — heat the water, heat a
//! room, cool a room — and its **operation mode** is which of `auto`, `on`, `off` and `eco`
//! it is in. Six use cases are that one idea, three of them reading it and three writing
//! it:
//!
//! | | monitor | configure |
//! |---|---|---|
//! | hot water | [`mdsf`](super::mdsf) | [`cdsf`](super::cdsf) |
//! | room heating | [`mrhsf`](super::mrhsf) | [`crhsf`](super::crhsf) |
//! | room cooling | [`mrcsf`](super::mrcsf) | [`crcsf`](super::crcsf) |
//!
//! They are the same four functions on one `HVAC` feature, and they differ in three
//! things: the `systemFunctionType` (`dhw`, `heating`, `cooling`), whether the actor may
//! write, and whether an **overrun** is in scope. Only the hot water has one — the
//! "one-time DHW loading" button in the bathroom — which is why [`OverrunReport`] and the
//! overrun functions appear in MDSF and CDSF and in none of the other four.
//!
//! # Why the identifiers have to be kept apart
//!
//! One `HVAC` feature carries **every** system function the appliance has (§3.2.2.2.1
//! gives an entity at most one feature of a type), so a heat pump that heats water and a
//! living room publishes both in the same lists. What tells them apart is
//! `systemFunctionId`, and the specification threads that identifier through every table
//! that could otherwise be ambiguous:
//!
//! * `hvacSystemFunctionOperationModeRelationData` is keyed by it — which modes this
//!   function has.
//! * `hvacSystemFunctionData` is keyed by it — which mode this function is *in*.
//! * `hvacSystemFunctionSetpointRelationData` is keyed by it **and** by
//!   `operationModeId` — a PRIMARY and a SUB identifier, which is
//!   [`setpoint::Setpoints`](super::setpoint::Setpoints)' business.
//!
//! Operation modes, by contrast, are described once for the device and shared: `auto` may
//! be `operationModeId` 1 for the hot water *and* for the room. A reader that keyed
//! anything by the mode alone would answer a question about the living room with a fact
//! about the tank.
//!
//! [`SystemFunction`] is the reader, and it is told at construction which function it is
//! following.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, FeatureType, Function, HvacOperationModeId, HvacOperationModeType, HvacOverrunData,
    HvacOverrunDescriptionData, HvacOverrunDescriptionListData, HvacOverrunId, HvacOverrunListData,
    HvacOverrunStatus, HvacOverrunType, HvacSystemFunctionData, HvacSystemFunctionDescriptionData,
    HvacSystemFunctionDescriptionListData, HvacSystemFunctionId, HvacSystemFunctionListData,
    HvacSystemFunctionOperationModeRelationData, HvacSystemFunctionOperationModeRelationListData,
    HvacSystemFunctionType, Role,
};
use crate::spine::{LocalFeature, Operations};

use super::OperationModes;

/// The `operationModeId`s **this** implementation gives the four modes.
///
/// Local choices, `<om1#(2..4)>`. The specification's own worked example numbers them this
/// way — `auto` → `<om1#1>`, `on` → `<om1#2>`, `off` → `<om1#3>`, `eco` → `<om1#4>` — and
/// a peer's are read from [`OperationModes`] rather than assumed.
///
/// Shared across system functions on purpose: the modes are described once for the device,
/// and which of them a given function has is
/// [`operation_mode_relations`] rather than a second numbering.
pub fn operation_mode_id(kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
    Some(HvacOperationModeId(match kind {
        HvacOperationModeType::Auto => 1,
        HvacOperationModeType::On => 2,
        HvacOperationModeType::Off => 3,
        HvacOperationModeType::Eco => 4,
        _ => return None,
    }))
}

// ---- the feature the server serves ----------------------------------------------------

/// Builds the read-only `HVAC` feature the three *monitoring* use cases are served from.
///
/// `overrun` adds the two overrun functions, which only the hot water has.
///
/// §3.2.2.2.1 puts **at most one** `HVAC` feature on an entity, so a circuit serving
/// several of this family — which is the ordinary case — puts all of their functions on
/// this one feature. [`with_setpoint_relations`] adds the temperature-configuration half,
/// and [`writeable`] is the same feature with the writes the configuration use cases need.
pub fn hvac_feature(address: u32, overrun: bool) -> LocalFeature {
    let feature = LocalFeature::new(address, FeatureType::HVAC, Role::Server)
        .with_function(
            Function::HvacSystemFunctionDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::HvacOperationModeDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::HvacSystemFunctionOperationModeRelationListData,
            Operations::read(),
        )
        .with_function(Function::HvacSystemFunctionListData, Operations::read());
    if !overrun {
        return feature;
    }
    feature
        .with_function(Function::HvacOverrunDescriptionListData, Operations::read())
        .with_function(Function::HvacOverrunListData, Operations::read())
}

/// The same feature with the writes a *configuration* use case needs.
///
/// `hvacSystemFunctionListData` becomes writeable — that is scenario 1 of CDSF, CRHSF and
/// CRCSF — and, where `overrun` is set, so does `hvacOverrunListData`, which is CDSF's
/// scenarios 2 and 3.
///
/// **Writes need no binding**, and that is the specification's own instruction rather than
/// a relaxation: every use case in this family says "Binding SHOULD NOT be used for this
/// Scenario". A circuit that insisted on one would refuse every conformant Configuration
/// Appliance. See [`WriteBinding`](crate::spine::WriteBinding).
///
/// Writes are **deferred**: a mode the function does not relate to, a mode the circuit has
/// said is not changeable, an overrun the circuit does not have — each is a refusal, and a
/// refusal has to come before the acknowledgement rather than after it. Feed each
/// [`SpineEvent::WriteRequested`](crate::spine::SpineEvent::WriteRequested) to
/// [`SystemFunction::apply`] and answer with
/// [`Engine::accept_write`](crate::spine::Engine::accept_write) or
/// [`reject_write`](crate::spine::Engine::reject_write).
pub fn writeable(address: u32, overrun: bool) -> LocalFeature {
    let feature = hvac_feature(address, overrun)
        .with_deferred_writes()
        .with_unbound_writes()
        .with_function(
            Function::HvacSystemFunctionListData,
            Operations::read_write(),
        );
    if !overrun {
        return feature;
    }
    feature.with_function(Function::HvacOverrunListData, Operations::read_write())
}

/// The same feature, also carrying the setpoint relations of a temperature use case.
///
/// §3.2.2.2.1 of every specification in the family says an entity holds at most one feature
/// of a given type, so a DHW Circuit serving MDSF *and* CDT — the ordinary case, since one
/// of the two system-function use cases is mandatory alongside CDT [CDT-005] — cannot have
/// an `HVAC` feature each. This is the one they share.
pub fn with_setpoint_relations(feature: LocalFeature) -> LocalFeature {
    feature.with_function(
        Function::HvacSystemFunctionSetpointRelationListData,
        Operations::read(),
    )
}

// ---- what the server publishes --------------------------------------------------------

/// The system function's description, which is what says *which* function an id is.
pub fn description(id: HvacSystemFunctionId, kind: HvacSystemFunctionType) -> CmdData {
    CmdData::HvacSystemFunctionDescriptionListData(HvacSystemFunctionDescriptionListData {
        hvac_system_function_description_data: Some(vec![HvacSystemFunctionDescriptionData {
            system_function_id: Some(id),
            system_function_type: Some(kind),
            ..Default::default()
        }]),
    })
}

/// Several of them at once, for an appliance that serves more than one function.
///
/// A heat pump that heats water and a room publishes both here, under different
/// identifiers, and a client picks its own out by `systemFunctionType`.
pub fn descriptions(functions: &[(HvacSystemFunctionId, HvacSystemFunctionType)]) -> CmdData {
    CmdData::HvacSystemFunctionDescriptionListData(HvacSystemFunctionDescriptionListData {
        hvac_system_function_description_data: Some(
            functions
                .iter()
                .map(|(id, kind)| HvacSystemFunctionDescriptionData {
                    system_function_id: Some(*id),
                    system_function_type: Some(kind.clone()),
                    ..Default::default()
                })
                .collect(),
        ),
    })
}

/// The operation modes the device supports.
///
/// [`None`] for fewer than two, which §2.3.1.1 does not permit: a function with one mode
/// cannot report a *change* of mode, which is the whole of scenario 1. Refusing here is the
/// same choice [`setpoint::relations`](super::setpoint::relations) makes — a payload that
/// breaks the rule tells a client nothing it can act on, and it is better not to publish it.
///
/// Device-wide, not per function: the modes are one list and
/// [`operation_mode_relations`] is what says which function has which of them.
pub fn operation_mode_descriptions(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    if modes.len() < 2 {
        return None;
    }
    let mut data = Vec::new();
    for kind in modes {
        data.push(crate::model::HvacOperationModeDescriptionData {
            operation_mode_id: Some(operation_mode_id(kind)?),
            operation_mode_type: Some(kind.clone()),
            ..Default::default()
        });
    }
    Some(CmdData::HvacOperationModeDescriptionListData(
        crate::model::HvacOperationModeDescriptionListData {
            hvac_operation_mode_description_data: Some(data),
        },
    ))
}

/// Which modes belong to one system function.
///
/// At least two, for the reason [`operation_mode_descriptions`] gives.
pub fn operation_mode_relations(
    function: HvacSystemFunctionId,
    modes: &[HvacOperationModeType],
) -> Option<CmdData> {
    if modes.len() < 2 {
        return None;
    }
    let ids: Option<Vec<HvacOperationModeId>> = modes.iter().map(operation_mode_id).collect();
    Some(CmdData::HvacSystemFunctionOperationModeRelationListData(
        HvacSystemFunctionOperationModeRelationListData {
            hvac_system_function_operation_mode_relation_data: Some(vec![
                HvacSystemFunctionOperationModeRelationData {
                    system_function_id: Some(function),
                    operation_mode_id: Some(ids?),
                },
            ]),
        },
    ))
}

/// Which modes belong to **several** system functions, in one payload.
///
/// What a device serving more than one function has to publish: the list is one function's
/// data, and a server that sent it twice would replace its own first entry. A room that
/// heats and cools publishes both here.
pub fn operation_mode_relations_of(
    functions: &[(HvacSystemFunctionId, &[HvacOperationModeType])],
) -> Option<CmdData> {
    let mut data = Vec::new();
    for (function, modes) in functions {
        if modes.len() < 2 {
            return None;
        }
        let ids: Option<Vec<HvacOperationModeId>> = modes.iter().map(operation_mode_id).collect();
        data.push(HvacSystemFunctionOperationModeRelationData {
            system_function_id: Some(*function),
            operation_mode_id: Some(ids?),
        });
    }
    Some(CmdData::HvacSystemFunctionOperationModeRelationListData(
        HvacSystemFunctionOperationModeRelationListData {
            hvac_system_function_operation_mode_relation_data: Some(data),
        },
    ))
}

/// The mode a function is in now, and whether an overrun is overriding it.
///
/// `changeable` is `isOperationModeIdChangeable`. It is what says whether a Configuration
/// Appliance running [`cdsf`](super::cdsf), [`crhsf`](super::crhsf) or
/// [`crcsf`](super::crcsf) may change the mode, and a server that serves one of those
/// SHALL set it `true`.
pub fn state(
    function: HvacSystemFunctionId,
    current: HvacOperationModeId,
    overrun_active: bool,
    changeable: Option<bool>,
) -> CmdData {
    CmdData::HvacSystemFunctionListData(HvacSystemFunctionListData {
        hvac_system_function_data: Some(vec![HvacSystemFunctionData {
            system_function_id: Some(function),
            current_operation_mode_id: Some(current),
            is_operation_mode_id_changeable: changeable,
            is_overrun_active: Some(overrun_active),
            ..Default::default()
        }]),
    })
}

/// The state of **several** functions, in one payload.
///
/// The counterpart of [`operation_mode_relations_of`], and needed for the same reason: one
/// `hvacSystemFunctionListData` carries every function's current mode, and publishing them
/// one at a time replaces rather than adds.
pub fn states(
    functions: &[(
        HvacSystemFunctionId,
        HvacOperationModeId,
        bool,
        Option<bool>,
    )],
) -> CmdData {
    CmdData::HvacSystemFunctionListData(HvacSystemFunctionListData {
        hvac_system_function_data: Some(
            functions
                .iter()
                .map(
                    |(function, current, overrun_active, changeable)| HvacSystemFunctionData {
                        system_function_id: Some(*function),
                        current_operation_mode_id: Some(*current),
                        is_operation_mode_id_changeable: *changeable,
                        is_overrun_active: Some(*overrun_active),
                        ..Default::default()
                    },
                )
                .collect(),
        ),
    })
}

/// An overrun this appliance offers, and which functions it affects.
pub fn overrun_description(
    id: HvacOverrunId,
    kind: HvacOverrunType,
    affects: &[HvacSystemFunctionId],
) -> CmdData {
    CmdData::HvacOverrunDescriptionListData(HvacOverrunDescriptionListData {
        hvac_overrun_description_data: Some(vec![HvacOverrunDescriptionData {
            overrun_id: Some(id),
            overrun_type: Some(kind),
            affected_system_function_id: Some(affects.to_vec()),
            ..Default::default()
        }]),
    })
}

/// What an overrun is doing.
///
/// The specification puts a rule on `finished` that is easy to get wrong and impossible to
/// see: it **MAY only be used as a notification directly after the overrun finished**, the
/// status SHALL become `inactive` after that, and it SHOULD NOT appear in a reply. A
/// circuit that leaves `finished` standing tells every appliance that reads it later that a
/// heating has just completed — repeatedly. [`OverrunReport`] is the type that makes that
/// shape hard to get wrong.
pub fn overrun_state(id: HvacOverrunId, status: HvacOverrunStatus) -> CmdData {
    CmdData::HvacOverrunListData(HvacOverrunListData {
        hvac_overrun_data: Some(vec![HvacOverrunData {
            overrun_id: Some(id),
            overrun_status: Some(status),
            ..Default::default()
        }]),
    })
}

/// An overrun's status, with the specification's transient built in.
///
/// `finished` is not a state a circuit rests in — it is an announcement, sent once as a
/// notification and then replaced by `inactive`. Modelling it as a state is what leads to a
/// reply that says a heating has just finished when it finished an hour ago.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrunReport {
    /// Requested, not yet heating.
    Active,
    /// Heating now.
    Running,
    /// Not running, and nothing pending.
    Inactive,
}

impl OverrunReport {
    /// The status a *reply* may carry.
    pub fn as_status(self) -> HvacOverrunStatus {
        match self {
            Self::Active => HvacOverrunStatus::Active,
            Self::Running => HvacOverrunStatus::Running,
            Self::Inactive => HvacOverrunStatus::Inactive,
        }
    }

    /// Reads a status into a resting state.
    ///
    /// `finished` reads as [`Inactive`](Self::Inactive) plus a `true` — the boolean is the
    /// announcement, and a caller that wants to know a heating *just* completed uses it
    /// rather than storing the status.
    pub fn read(status: &HvacOverrunStatus) -> Option<(Self, bool)> {
        Some(match status {
            HvacOverrunStatus::Active => (Self::Active, false),
            HvacOverrunStatus::Running => (Self::Running, false),
            HvacOverrunStatus::Inactive => (Self::Inactive, false),
            HvacOverrunStatus::Finished => (Self::Inactive, true),
            _ => return None,
        })
    }
}

// ---- what a Configuration Appliance writes --------------------------------------------

/// Sets one system function's operation mode.
///
/// A **partial** write, and the identifier is the peer's: `systemFunctionId` is
/// `<sf1#(1..1)>` and the mode must be one the function actually relates to — the
/// specification says so twice, once as a rule on the element ("only an operationModeId
/// value related there to the according systemFunctionId SHALL be used here") and once as
/// [CDSF-001]. [`SystemFunction::set_mode`] builds this from what the peer published and
/// refuses the rest.
pub fn set_operation_mode(function: HvacSystemFunctionId, mode: HvacOperationModeId) -> CmdData {
    CmdData::HvacSystemFunctionListData(HvacSystemFunctionListData {
        hvac_system_function_data: Some(vec![HvacSystemFunctionData {
            system_function_id: Some(function),
            current_operation_mode_id: Some(mode),
            ..Default::default()
        }]),
    })
}

/// Starts an overrun: `overrunStatus: active` ([CDSF-002]).
pub fn start_overrun(id: HvacOverrunId) -> CmdData {
    overrun_state(id, HvacOverrunStatus::Active)
}

/// Stops one: `overrunStatus: inactive` ([CDSF-003]).
pub fn stop_overrun(id: HvacOverrunId) -> CmdData {
    overrun_state(id, HvacOverrunStatus::Inactive)
}

/// Why a mode or overrun change could not be made.
///
/// The same shape the rest of the crate uses for a request that would be well formed on the
/// wire and wrong in the room: the refusal comes from what the *peer itself published*, so
/// it is available before anything is sent, and a server built on this crate can hand the
/// matching `errorNumber` back rather than storing a mode its function does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModeRefused {
    /// The peer has not described the system function yet.
    #[error("the peer has not said which system function this is")]
    FunctionUnknown,
    /// The write names no entry for **this** system function.
    ///
    /// Not an error on its own: one `HVAC` feature carries every function the appliance
    /// has, so a server that heats and cools keeps a reader for each and offers the write
    /// to both. This is the one that says "try the other one" — and only when *no* reader
    /// claims it is it an answer to send back.
    #[error("this write names some other system function")]
    NotAddressed,
    /// The mode is not one this system function relates to.
    ///
    /// A device describes every mode it has once, device-wide, and relates a subset of them
    /// to each function. `eco` may be a hot water mode and not a room heating one, and
    /// writing it to the room is a well-formed request for something that does not exist.
    #[error("this system function does not have that operation mode")]
    NotRelated,
    /// The circuit says its mode may not be changed (`isOperationModeIdChangeable`).
    #[error("the peer says its operation mode is not changeable")]
    NotChangeable,
    /// The peer described no overrun of the kind this use case carries.
    #[error("the peer has no overrun of this kind")]
    NoOverrun,
}

impl ModeRefused {
    /// The `errorNumber` a server answers a write with, having refused it for this reason.
    pub fn error_number(self) -> crate::model::ErrorNumber {
        match self {
            // "the command is not accepted in this state" — the mode exists and the
            // circuit will not take it now.
            Self::NotChangeable => crate::model::ErrorNumber::CommandRejected,
            // Everything else names something the request asked for that is not there.
            Self::FunctionUnknown | Self::NotAddressed | Self::NotRelated | Self::NoOverrun => {
                crate::model::ErrorNumber::DestinationUnknown
            }
        }
    }
}

// ---- what a client reads ---------------------------------------------------------------

/// One system function's state, as the specification's Table 12 gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FunctionState {
    current: Option<HvacOperationModeId>,
    changeable: Option<bool>,
    overrun_active: Option<bool>,
}

/// What a client has learned about **one** system function of a peer.
///
/// Told at construction which function it is following — [`dhw`](Self::dhw),
/// [`heating`](Self::heating), [`cooling`](Self::cooling) — because one `HVAC` feature
/// carries them all and picking the wrong one is invisible: the payloads are the same
/// shape, the identifiers are the peer's, and the answer is a fact about a different room.
///
/// Feed it every payload that arrives from the peer's `HVAC` feature. Nothing is dropped
/// for arriving early: every payload is kept under the identifier it named and resolved
/// against the descriptions whenever those turn up, so the replies may come back in any
/// order — which they may, and which no test whose other end is this crate would show.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemFunction {
    kind: HvacSystemFunctionType,
    /// The overrun this use case carries, where it has one. Only the hot water does.
    overrun_type: Option<HvacOverrunType>,
    /// The peer's own `systemFunctionId` for [`kind`](Self::kind).
    function: Option<HvacSystemFunctionId>,
    modes: OperationModes,
    /// The modes each system function relates to, by function.
    related: Vec<(HvacSystemFunctionId, Vec<HvacOperationModeId>)>,
    /// Each function's state, by function.
    states: Vec<(HvacSystemFunctionId, FunctionState)>,
    /// Each overrun: what kind it is and which functions it affects.
    overruns: Vec<(HvacOverrunId, HvacOverrunType, Vec<HvacSystemFunctionId>)>,
    /// Each overrun's status.
    overrun_states: Vec<(HvacOverrunId, (OverrunReport, bool))>,
}

impl SystemFunction {
    /// A reader following the hot water, with its one-time loading overrun.
    pub fn dhw() -> Self {
        Self::new(
            HvacSystemFunctionType::Dhw,
            Some(HvacOverrunType::OneTimeDhw),
        )
    }

    /// A reader following the room heating. No overrun is in scope for it.
    pub fn heating() -> Self {
        Self::new(HvacSystemFunctionType::Heating, None)
    }

    /// A reader following the room cooling.
    pub fn cooling() -> Self {
        Self::new(HvacSystemFunctionType::Cooling, None)
    }

    /// A reader following any function, with whatever overrun its use case defines.
    pub fn new(kind: HvacSystemFunctionType, overrun_type: Option<HvacOverrunType>) -> Self {
        Self {
            kind,
            overrun_type,
            function: None,
            modes: OperationModes::new(),
            related: Vec::new(),
            states: Vec::new(),
            overruns: Vec::new(),
            overrun_states: Vec::new(),
        }
    }

    /// Which system function this is following.
    pub fn kind(&self) -> &HvacSystemFunctionType {
        &self.kind
    }

    /// Takes in one payload, and reports whether it was one this family carries.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        if self.modes.learn(data) {
            return true;
        }
        match data {
            CmdData::HvacSystemFunctionDescriptionListData(list) => {
                // An `HVAC` feature carries every system function the appliance has; only
                // the one typed like this reader is this use case's.
                if let Some(id) = list
                    .hvac_system_function_description_data
                    .iter()
                    .flatten()
                    .find(|entry| entry.system_function_type.as_ref() == Some(&self.kind))
                    .and_then(|entry| entry.system_function_id)
                {
                    self.function = Some(id);
                }
                true
            }
            CmdData::HvacSystemFunctionOperationModeRelationListData(list) => {
                for entry in list
                    .hvac_system_function_operation_mode_relation_data
                    .iter()
                    .flatten()
                {
                    let Some(id) = entry.system_function_id else {
                        continue;
                    };
                    let modes = entry.operation_mode_id.clone().unwrap_or_default();
                    match self.related.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = modes,
                        None => self.related.push((id, modes)),
                    }
                }
                true
            }
            CmdData::HvacSystemFunctionListData(list) => {
                for entry in list.hvac_system_function_data.iter().flatten() {
                    let Some(id) = entry.system_function_id else {
                        continue;
                    };
                    let found = FunctionState {
                        current: entry.current_operation_mode_id,
                        changeable: entry.is_operation_mode_id_changeable,
                        overrun_active: entry.is_overrun_active,
                    };
                    match self.states.iter_mut().find(|(known, _)| *known == id) {
                        Some((_, stored)) => *stored = found,
                        None => self.states.push((id, found)),
                    }
                }
                true
            }
            CmdData::HvacOverrunDescriptionListData(list) => {
                for entry in list.hvac_overrun_description_data.iter().flatten() {
                    let (Some(id), Some(kind)) = (entry.overrun_id, entry.overrun_type.clone())
                    else {
                        continue;
                    };
                    let affects = entry
                        .affected_system_function_id
                        .clone()
                        .unwrap_or_default();
                    match self.overruns.iter_mut().find(|(known, ..)| *known == id) {
                        Some(stored) => *stored = (id, kind, affects),
                        None => self.overruns.push((id, kind, affects)),
                    }
                }
                true
            }
            CmdData::HvacOverrunListData(list) => {
                for entry in list.hvac_overrun_data.iter().flatten() {
                    let (Some(id), Some(status)) =
                        (entry.overrun_id, entry.overrun_status.as_ref())
                    else {
                        continue;
                    };
                    let Some(state) = OverrunReport::read(status) else {
                        continue;
                    };
                    match self
                        .overrun_states
                        .iter_mut()
                        .find(|(known, _)| *known == id)
                    {
                        Some((_, stored)) => *stored = state,
                        None => self.overrun_states.push((id, state)),
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn state(&self) -> Option<&FunctionState> {
        let function = self.function?;
        self.states
            .iter()
            .find(|(known, _)| *known == function)
            .map(|(_, state)| state)
    }

    /// The `overrunId` of the overrun that affects *this* system function.
    ///
    /// An appliance describes all of its overruns together and a party mode is not hot
    /// water, so both the type and the affected function have to match. An overrun naming
    /// no affected function is taken as this one's — the element is mandatory, and
    /// `oneTimeDhw` is not ambiguous about what it heats.
    pub fn overrun_id(&self) -> Option<HvacOverrunId> {
        let function = self.function?;
        let wanted = self.overrun_type.as_ref()?;
        self.overruns
            .iter()
            .find(|(_, kind, affects)| {
                kind == wanted && (affects.is_empty() || affects.contains(&function))
            })
            .map(|(id, ..)| *id)
    }

    /// The peer's `systemFunctionId` for this function, once described.
    pub fn system_function(&self) -> Option<HvacSystemFunctionId> {
        self.function
    }

    /// The modes the peer described, device-wide.
    pub fn modes(&self) -> &OperationModes {
        &self.modes
    }

    /// The mode this function is in now.
    pub fn mode(&self) -> Option<&HvacOperationModeType> {
        self.modes.kind_of(self.mode_id()?)
    }

    /// The identifier of that mode, which is what the setpoint relations are keyed by.
    pub fn mode_id(&self) -> Option<HvacOperationModeId> {
        self.state()?.current
    }

    /// The modes the peer relates to *this* function.
    ///
    /// At least two, where the peer follows §2.3.1.1. Empty until both the system function
    /// description and the relations have arrived.
    pub fn related_modes(&self) -> &[HvacOperationModeId] {
        let Some(function) = self.function else {
            return &[];
        };
        self.related
            .iter()
            .find(|(known, _)| *known == function)
            .map(|(_, modes)| modes.as_slice())
            .unwrap_or_default()
    }

    /// The modes this function has, as named types rather than identifiers.
    pub fn available_modes(
        &self,
    ) -> impl Iterator<Item = (HvacOperationModeId, &HvacOperationModeType)> {
        self.related_modes()
            .iter()
            .filter_map(|id| Some((*id, self.modes.kind_of(*id)?)))
    }

    /// The setpoints a temperature use case says the *current* mode uses.
    ///
    /// The join between the two halves of the family, and what makes a temperature write a
    /// complete instruction: a setpoint the peer is not currently reading can be written,
    /// acknowledged, and change nothing anybody can measure.
    ///
    /// Empty until the mode, the function's identifier and the relations have all arrived,
    /// and for a mode the peer relates to no setpoint — which `off` is allowed to be.
    pub fn current_setpoints<'a>(
        &self,
        setpoints: &'a super::setpoint::Setpoints,
    ) -> &'a [crate::model::SetpointId] {
        match (self.function, self.mode_id()) {
            (Some(function), Some(mode)) => setpoints.for_mode(function, mode),
            _ => &[],
        }
    }

    /// Whether the peer says an overrun is overriding the mode.
    pub fn overrun_active(&self) -> Option<bool> {
        self.state()?.overrun_active
    }

    /// Whether a Configuration Appliance could change the mode.
    pub fn mode_changeable(&self) -> Option<bool> {
        self.state()?.changeable
    }

    /// What this function's overrun is doing.
    pub fn overrun(&self) -> Option<OverrunReport> {
        self.overrun_report().map(|(state, _)| state)
    }

    fn overrun_report(&self) -> Option<(OverrunReport, bool)> {
        let id = self.overrun_id()?;
        self.overrun_states
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, state)| *state)
    }

    /// Whether the last payload announced that an overrun had *just* finished.
    ///
    /// `finished` is a one-shot notification, not a state. This is it, and it is
    /// deliberately not part of [`overrun`](Self::overrun): a caller that stored `finished`
    /// would go on reporting a completed heating for as long as nothing else arrived.
    pub fn overrun_just_finished(&self) -> bool {
        self.overrun_report().is_some_and(|(_, finished)| finished)
    }

    /// Whether scenario 1 can be reported at all: the function, two modes, and a current one.
    pub fn is_complete(&self) -> bool {
        self.function.is_some() && self.modes.is_sufficient() && self.mode_id().is_some()
    }

    // ---- the configuration half --------------------------------------------------------

    /// Builds the write that puts this function into `mode`, refusing what the peer would
    /// not take.
    ///
    /// Three things are checked, all of them from what the peer itself published:
    ///
    /// * the function has been described, so there is an identifier to address;
    /// * the mode is one **this** function relates to — a device describes every mode it
    ///   has once and relates a subset to each function, so `eco` may be a hot water mode
    ///   and not a room heating one;
    /// * `isOperationModeIdChangeable` is not `false`.
    ///
    /// Absent that last element the write is allowed: it is `O` in the table, and a peer
    /// that serves a configuration use case and says nothing has said nothing rather than
    /// no.
    pub fn set_mode(&self, mode: HvacOperationModeId) -> Result<CmdData, ModeRefused> {
        let function = self.function.ok_or(ModeRefused::FunctionUnknown)?;
        if !self.related_modes().contains(&mode) {
            return Err(ModeRefused::NotRelated);
        }
        if self.mode_changeable() == Some(false) {
            return Err(ModeRefused::NotChangeable);
        }
        Ok(set_operation_mode(function, mode))
    }

    /// The same, by the mode's *name* rather than by the peer's identifier.
    ///
    /// What an application actually wants to say — "put the hot water in `eco`" — and it
    /// resolves the number through what the peer described, which is the only correct way
    /// to get one.
    pub fn set_mode_named(&self, kind: &HvacOperationModeType) -> Result<CmdData, ModeRefused> {
        let id = self.modes.id_of(kind).ok_or(ModeRefused::NotRelated)?;
        self.set_mode(id)
    }

    /// Builds the write that starts this function's overrun.
    ///
    /// Refused where the peer described no overrun of the kind this use case carries — a
    /// circuit that does not serve the scenario, or one whose descriptions have not
    /// arrived. Starting one that is already running is *not* refused: the specification
    /// makes it idempotent, and a manager re-asserting a state it wants is doing the right
    /// thing after a reconnection.
    pub fn start_overrun(&self) -> Result<CmdData, ModeRefused> {
        Ok(start_overrun(
            self.overrun_id().ok_or(ModeRefused::NoOverrun)?,
        ))
    }

    /// Builds the write that stops it.
    pub fn stop_overrun(&self) -> Result<CmdData, ModeRefused> {
        Ok(stop_overrun(
            self.overrun_id().ok_or(ModeRefused::NoOverrun)?,
        ))
    }

    // ---- the server half ---------------------------------------------------------------

    /// Decides what an incoming write asks for, from a server's point of view.
    ///
    /// The counterpart of [`set_mode`](Self::set_mode) at the other end of the wire.
    ///
    /// **Give it `write.data`, the fragment — not `write.resolved`.** That is the opposite of
    /// the rule everywhere else here, because these are *list* functions with more than one
    /// entry: the resolved state holds every system function the appliance has, each with
    /// the mode it was already in, so it cannot say which entry the peer addressed. The
    /// fragment names exactly what the write asked to change.
    ///
    /// It is checked against the server's own published state, so a server keeps one of
    /// these too. A server with **several** system functions keeps one each and offers the
    /// write to all of them: [`ModeRefused::NotAddressed`] means "this names some other
    /// function, try the next", and only when none claims it is it an answer to send back.
    pub fn apply(&self, fragment: &CmdData) -> Result<Request, ModeRefused> {
        match fragment {
            CmdData::HvacSystemFunctionListData(list) => {
                let function = self.function.ok_or(ModeRefused::FunctionUnknown)?;
                let entry = list
                    .hvac_system_function_data
                    .iter()
                    .flatten()
                    .find(|entry| entry.system_function_id == Some(function))
                    .ok_or(ModeRefused::NotAddressed)?;
                let mode = entry
                    .current_operation_mode_id
                    .ok_or(ModeRefused::NotRelated)?;
                if !self.related_modes().contains(&mode) {
                    return Err(ModeRefused::NotRelated);
                }
                if self.mode_changeable() == Some(false) {
                    return Err(ModeRefused::NotChangeable);
                }
                Ok(Request::SetMode(mode))
            }
            CmdData::HvacOverrunListData(list) => {
                let id = self.overrun_id().ok_or(ModeRefused::NoOverrun)?;
                let status = list
                    .hvac_overrun_data
                    .iter()
                    .flatten()
                    .find(|entry| entry.overrun_id == Some(id))
                    .and_then(|entry| entry.overrun_status.as_ref())
                    .ok_or(ModeRefused::NotAddressed)?;
                match status {
                    HvacOverrunStatus::Active | HvacOverrunStatus::Running => {
                        Ok(Request::StartOverrun(id))
                    }
                    HvacOverrunStatus::Inactive => Ok(Request::StopOverrun(id)),
                    // `finished` is the server's announcement to make, not the client's.
                    _ => Err(ModeRefused::NoOverrun),
                }
            }
            _ => Err(ModeRefused::NotAddressed),
        }
    }
}

/// What a Configuration Appliance's write asked a server to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Put this system function into that operation mode (scenario 1).
    SetMode(HvacOperationModeId),
    /// Start the overrun ([CDSF-002]).
    StartOverrun(HvacOverrunId),
    /// Stop it ([CDSF-003]).
    StopOverrun(HvacOverrunId),
}
