//! Monitoring of DHW System Function (MDSF).
//!
//! A *Monitoring Appliance* reads which operation mode a *DHW Circuit* is in, and whether
//! a one-time hot water heating is running. It is the half of the DHW pair that says
//! **what the circuit is doing**; [`cdt`](super::cdt) is the half that changes it.
//!
//! Two scenarios:
//!
//! 1. **Monitor DHW operation mode** — `auto`, `on`, `off` or `eco`, exactly one of them
//!    enabled at a time [MDSF-001]. Mandatory for both actors.
//! 2. **Monitor DHW overrun** — a "one-time DHW loading" that overrides the current mode
//!    until it finishes [MDSF-002]. Mandatory for the appliance, recommended for the
//!    circuit.
//!
//! # Why an energy manager wants this
//!
//! A setpoint written into the wrong operation mode is applied and does nothing. CDT
//! Table 10 relates each mode to the setpoints it uses, so "raise the tank to 60 °C" is
//! only a complete instruction once the current mode is known — and the mode is here.
//! [`DhwSystemFunction::current_setpoints`] is that join.
//!
//! The overrun matters for the opposite reason. A one-time heating overrides the mode
//! until it is done, so a manager that sees the tank drawing power while the mode says
//! `off` is not looking at a fault; it is looking at somebody pressing the button in the
//! bathroom. Reporting that as an anomaly is how an energy manager loses a user's trust.

use alloc::vec;
use alloc::vec::Vec;

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeDescriptionData,
    HvacOperationModeDescriptionListData, HvacOperationModeId, HvacOperationModeType,
    HvacOverrunData, HvacOverrunDescriptionData, HvacOverrunDescriptionListData, HvacOverrunId,
    HvacOverrunListData, HvacOverrunStatus, HvacOverrunType, HvacSystemFunctionData,
    HvacSystemFunctionId, HvacSystemFunctionListData, HvacSystemFunctionOperationModeRelationData,
    HvacSystemFunctionOperationModeRelationListData, Role,
};
use crate::spine::{LocalFeature, Operations};
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::{OperationModes, SYSTEM_FUNCTION_ID};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "monitoringOfDhwSystemFunction";

/// The actor that holds the hot water.
pub const DHW_CIRCUIT_ACTOR: &str = "DHWCircuit";

/// The actor that watches it.
pub const MONITORING_APPLIANCE_ACTOR: &str = "MonitoringAppliance";

/// The `overrunType` of a one-time hot water heating (Table 13).
pub const ONE_TIME_DHW: HvacOverrunType = HvacOverrunType::OneTimeDhw;

/// The `overrunId` **this** implementation publishes its one-time heating under.
///
/// A local choice, `<o1#(1..1)>`. A peer's is found by its `overrunType`.
pub const OVERRUN_ID: HvacOverrunId = HvacOverrunId(1);

/// The `operationModeId`s **this** implementation gives the four modes.
///
/// Local choices, `<om1#(2..4)>`. The specification's own worked example numbers them this
/// way — `auto` → `<om1#1>`, `on` → `<om1#2>`, `off` → `<om1#3>`, `eco` → `<om1#4>` — and
/// a peer's are read from [`OperationModes`] rather than assumed.
pub fn operation_mode_id(kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
    Some(HvacOperationModeId(match kind {
        HvacOperationModeType::Auto => 1,
        HvacOperationModeType::On => 2,
        HvacOperationModeType::Off => 3,
        HvacOperationModeType::Eco => 4,
        _ => return None,
    }))
}

// ---- the feature a DHW Circuit serves -----------------------------------------------

/// Builds the `HVAC` feature both scenarios are served from (Table 8).
///
/// Read-only throughout: this use case reports, and [`cdt`](super::cdt) is where anything
/// is changed. §3.2.2.2.1 adds a rule worth honouring — **at most one** `HVAC` feature per
/// entity — so a circuit serving CDT as well puts both use cases' functions on this one
/// feature. [`with_cdt`] does that.
pub fn hvac_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::HVAC, Role::Server)
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
        .with_function(Function::HvacSystemFunctionListData, Operations::read())
        .with_function(Function::HvacOverrunDescriptionListData, Operations::read())
        .with_function(Function::HvacOverrunListData, Operations::read())
}

/// The same feature, also carrying [`cdt`](super::cdt)'s setpoint relations.
///
/// §3.2.2.2.1 of both specifications says an entity holds at most one feature of a given
/// type, so a DHW Circuit serving MDSF *and* CDT — which is the ordinary case, since one
/// of the two system-function use cases is mandatory alongside CDT [CDT-005] — cannot have
/// an `HVAC` feature each. This is the one they share.
pub fn with_cdt(address: u32) -> LocalFeature {
    hvac_feature(address).with_function(
        Function::HvacSystemFunctionSetpointRelationListData,
        Operations::read(),
    )
}

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The operation modes the circuit supports (Table 10).
///
/// [`None`] for fewer than two, which §2.3.1.1 does not permit: a circuit with one mode
/// cannot report a *change* of mode, which is the whole of scenario 1. Refusing here is
/// the same choice [`cdt::system_function_relations`](super::cdt::system_function_relations)
/// makes — a payload that breaks the rule tells a Monitoring Appliance nothing it can act
/// on, and it is better not to publish it.
pub fn operation_mode_descriptions(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    if modes.len() < 2 {
        return None;
    }
    let mut data = Vec::new();
    for kind in modes {
        data.push(HvacOperationModeDescriptionData {
            operation_mode_id: Some(operation_mode_id(kind)?),
            operation_mode_type: Some(kind.clone()),
            ..Default::default()
        });
    }
    Some(CmdData::HvacOperationModeDescriptionListData(
        HvacOperationModeDescriptionListData {
            hvac_operation_mode_description_data: Some(data),
        },
    ))
}

/// Which modes belong to the DHW system function (Table 11).
///
/// At least two, for the reason [`operation_mode_descriptions`] gives.
pub fn operation_mode_relations(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    if modes.len() < 2 {
        return None;
    }
    let ids: Option<Vec<HvacOperationModeId>> = modes.iter().map(operation_mode_id).collect();
    Some(CmdData::HvacSystemFunctionOperationModeRelationListData(
        HvacSystemFunctionOperationModeRelationListData {
            hvac_system_function_operation_mode_relation_data: Some(vec![
                HvacSystemFunctionOperationModeRelationData {
                    system_function_id: Some(SYSTEM_FUNCTION_ID),
                    operation_mode_id: Some(ids?),
                },
            ]),
        },
    ))
}

/// The mode the circuit is in now, and whether an overrun is overriding it (Table 12).
///
/// `changeable` is `isOperationModeIdChangeable`, which is `O`: it says whether a
/// Configuration Appliance running "Configuration of DHW System Function" could change the
/// mode. This use case only reports.
pub fn system_function_state(
    current: HvacOperationModeId,
    overrun_active: bool,
    changeable: Option<bool>,
) -> CmdData {
    CmdData::HvacSystemFunctionListData(HvacSystemFunctionListData {
        hvac_system_function_data: Some(vec![HvacSystemFunctionData {
            system_function_id: Some(SYSTEM_FUNCTION_ID),
            current_operation_mode_id: Some(current),
            is_operation_mode_id_changeable: changeable,
            is_overrun_active: Some(overrun_active),
            ..Default::default()
        }]),
    })
}

/// The one-time hot water heating this circuit offers (Table 13).
pub fn overrun_description() -> CmdData {
    CmdData::HvacOverrunDescriptionListData(HvacOverrunDescriptionListData {
        hvac_overrun_description_data: Some(vec![HvacOverrunDescriptionData {
            overrun_id: Some(OVERRUN_ID),
            overrun_type: Some(ONE_TIME_DHW),
            affected_system_function_id: Some(vec![SYSTEM_FUNCTION_ID]),
            ..Default::default()
        }]),
    })
}

/// What the one-time heating is doing (Table 14).
///
/// Table 14 puts a rule on `finished` that is easy to get wrong and impossible to see: it
/// **MAY only be used as a notification directly after the overrun finished**, the status
/// SHALL become `inactive` after that, and it SHOULD NOT appear in a reply. A circuit that
/// leaves `finished` standing tells every appliance that reads it later that a heating has
/// just completed — repeatedly. [`OverrunReport`] is the type that makes that shape hard
/// to get wrong.
pub fn overrun_state(status: HvacOverrunStatus) -> CmdData {
    CmdData::HvacOverrunListData(HvacOverrunListData {
        hvac_overrun_data: Some(vec![HvacOverrunData {
            overrun_id: Some(OVERRUN_ID),
            overrun_status: Some(status),
            ..Default::default()
        }]),
    })
}

/// The one-time heating's status, with Table 14's transient built in.
///
/// `finished` is not a state a circuit rests in — it is an announcement, sent once as a
/// notification and then replaced by `inactive`. Modelling it as a state is what leads to
/// a reply that says a heating has just finished when it finished an hour ago.
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

// ---- what a Monitoring Appliance reads ----------------------------------------------

/// What a Monitoring Appliance has learned about one DHW circuit.
///
/// Six functions between them, and the identifiers in every one of them are the circuit's:
/// `<sf1#(1..1)>` for the system function, `<om1#(2..4)>` for the modes, `<o1#(1..1)>` for
/// the overrun. Feed it every payload that arrives from the circuit's `HVAC` feature.
///
/// Nothing is dropped for arriving early. Every payload is kept under the identifier it
/// named and resolved against the descriptions whenever those turn up, so the six replies
/// may come back in any order — which they may, and which no test whose other end is this
/// crate would ever show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DhwSystemFunction {
    /// The circuit's own `systemFunctionId` for the hot water.
    function: Option<HvacSystemFunctionId>,
    modes: OperationModes,
    /// The modes each system function relates to (Table 11), by function.
    related: Vec<(HvacSystemFunctionId, Vec<HvacOperationModeId>)>,
    /// Table 12, by function.
    states: Vec<(HvacSystemFunctionId, SystemFunctionState)>,
    /// Table 13, by overrun: what kind it is and which functions it affects.
    overruns: Vec<(HvacOverrunId, HvacOverrunType, Vec<HvacSystemFunctionId>)>,
    /// Table 14, by overrun.
    overrun_states: Vec<(HvacOverrunId, (OverrunReport, bool))>,
}

/// One system function's state, as Table 12 gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SystemFunctionState {
    current: Option<HvacOperationModeId>,
    changeable: Option<bool>,
    overrun_active: Option<bool>,
}

impl DhwSystemFunction {
    /// Nothing known yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes in one payload, and reports whether it was one this use case carries.
    pub fn learn(&mut self, data: &CmdData) -> bool {
        if self.modes.learn(data) {
            return true;
        }
        match data {
            CmdData::HvacSystemFunctionDescriptionListData(_) => {
                // An HVAC feature carries every system function the appliance has; only
                // the one typed `dhw` is this use case's.
                if let Some(id) = super::find_dhw_system_function(data) {
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
                    let found = SystemFunctionState {
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

    /// This circuit's state, once the hot water's identifier is known.
    fn state(&self) -> Option<&SystemFunctionState> {
        let function = self.function?;
        self.states
            .iter()
            .find(|(known, _)| *known == function)
            .map(|(_, state)| state)
    }

    /// The `overrunId` of the one-time heating that affects *this* system function.
    ///
    /// A heat pump describes all of its overruns together and a party mode is not hot
    /// water, so both the type and the affected function have to match. An overrun naming
    /// no affected function is taken as this one's — Table 13 makes the element mandatory,
    /// and `oneTimeDhw` is not ambiguous about what it heats.
    fn overrun_id(&self) -> Option<HvacOverrunId> {
        let function = self.function?;
        self.overruns
            .iter()
            .find(|(_, kind, affects)| {
                kind == &ONE_TIME_DHW && (affects.is_empty() || affects.contains(&function))
            })
            .map(|(id, ..)| *id)
    }

    /// The circuit's `systemFunctionId` for the hot water, once described.
    pub fn system_function(&self) -> Option<HvacSystemFunctionId> {
        self.function
    }

    /// The modes the circuit described.
    pub fn modes(&self) -> &OperationModes {
        &self.modes
    }

    /// The mode the circuit is in now [MDSF-001].
    pub fn mode(&self) -> Option<&HvacOperationModeType> {
        self.modes.kind_of(self.mode_id()?)
    }

    /// The identifier of that mode, which is what CDT's relations are keyed by.
    pub fn mode_id(&self) -> Option<HvacOperationModeId> {
        self.state()?.current
    }

    /// The modes this circuit relates to its hot water (Table 11).
    ///
    /// At least two, where the circuit follows §2.3.1.1. Empty until both the system
    /// function description and the relations have arrived.
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

    /// The setpoints CDT says the *current* mode uses.
    ///
    /// This is the join between the two halves of the family, and it is what makes a
    /// temperature write a complete instruction: a setpoint the circuit is not currently
    /// reading can be written, acknowledged, and change nothing anybody can measure.
    ///
    /// Empty until both the mode and CDT's relations have arrived, and for a mode the
    /// circuit relates to no setpoint — which `off` is allowed to be [CDT-003/3].
    pub fn current_setpoints<'a>(
        &self,
        setpoints: &'a super::cdt::DhwSetpoints,
    ) -> &'a [crate::model::SetpointId] {
        match self.mode_id() {
            Some(mode) => setpoints.for_mode(mode),
            None => &[],
        }
    }

    /// Whether the circuit says an overrun is overriding the mode (Table 12).
    pub fn overrun_active(&self) -> Option<bool> {
        self.state()?.overrun_active
    }

    /// Whether a Configuration Appliance could change the mode (Table 12, `O`).
    pub fn mode_changeable(&self) -> Option<bool> {
        self.state()?.changeable
    }

    /// What the one-time hot water heating is doing (Table 14).
    pub fn overrun(&self) -> Option<OverrunReport> {
        self.overrun_state().map(|(state, _)| state)
    }

    fn overrun_state(&self) -> Option<(OverrunReport, bool)> {
        let id = self.overrun_id()?;
        self.overrun_states
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, state)| *state)
    }

    /// Whether the last payload announced that a heating had *just* finished.
    ///
    /// Table 14's `finished` is a one-shot notification, not a state. This is it, and it
    /// is deliberately not part of [`overrun`](Self::overrun): a caller that stored
    /// `finished` would go on reporting a completed heating for as long as nothing else
    /// arrived.
    pub fn overrun_just_finished(&self) -> bool {
        self.overrun_state().is_some_and(|(_, finished)| finished)
    }

    /// Whether scenario 1 can be reported at all: the function, two modes, and a current one.
    pub fn is_complete(&self) -> bool {
        self.function.is_some() && self.modes.is_sufficient() && self.mode_id().is_some()
    }
}

// ---- descriptors ---------------------------------------------------------------------

const DHW_CIRCUIT_ENTITIES: &[EntityType] = &[EntityType::DHWCircuit];
/// The Monitoring Appliance sits behind any entity (Figure 4, `entityType = <any>`).
const MONITORING_APPLIANCE_ENTITIES: &[EntityType] = &[];

const SERVER_MODE: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacOperationModeDescriptionListData,
    ),
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionOperationModeRelationListData,
    ),
    FunctionUse::server(FeatureType::HVAC, Function::HvacSystemFunctionListData),
];

const SERVER_OVERRUN: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::server(FeatureType::HVAC, Function::HvacSystemFunctionListData),
    FunctionUse::server(FeatureType::HVAC, Function::HvacOverrunDescriptionListData),
    FunctionUse::server(FeatureType::HVAC, Function::HvacOverrunListData),
];

const CLIENT_MODE: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacOperationModeDescriptionListData,
    ),
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionOperationModeRelationListData,
    ),
    FunctionUse::client(FeatureType::HVAC, Function::HvacSystemFunctionListData),
];

const CLIENT_OVERRUN: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::client(FeatureType::HVAC, Function::HvacSystemFunctionListData),
    FunctionUse::client(FeatureType::HVAC, Function::HvacOverrunDescriptionListData),
    FunctionUse::client(FeatureType::HVAC, Function::HvacOverrunListData),
];

const MODE: &str = "Monitor DHW operation mode";
const OVERRUN: &str = "Monitor DHW overrun";

/// The DHW Circuit: the actor being watched.
pub static DHW_CIRCUIT: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: DHW_CIRCUIT_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: DHW_CIRCUIT_ENTITIES,
    counterpart: MONITORING_APPLIANCE_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: MODE,
            support: Support::Mandatory,
            functions: SERVER_MODE,
        },
        Scenario {
            number: 2,
            // Table 1: `R` for the circuit, `M` for the appliance — an appliance must be
            // able to read an overrun, a circuit need not offer one.
            name: OVERRUN,
            support: Support::Recommended,
            functions: SERVER_OVERRUN,
        },
    ],
};

/// The Monitoring Appliance: the actor watching.
pub static MONITORING_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: MONITORING_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: MONITORING_APPLIANCE_ENTITIES,
    counterpart: DHW_CIRCUIT_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: MODE,
            support: Support::Mandatory,
            functions: CLIENT_MODE,
        },
        Scenario {
            number: 2,
            name: OVERRUN,
            support: Support::Mandatory,
            functions: CLIENT_OVERRUN,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::hvac;

    fn a_circuit() -> DhwSystemFunction {
        let modes = [
            HvacOperationModeType::Auto,
            HvacOperationModeType::On,
            HvacOperationModeType::Off,
        ];
        let mut known = DhwSystemFunction::new();
        known.learn(&hvac::system_function_description());
        known.learn(&operation_mode_descriptions(&modes).expect("three modes"));
        known.learn(&operation_mode_relations(&modes).expect("three modes"));
        known
    }

    /// §2.3.1.1: two or more modes, or the use case reports nothing.
    #[test]
    fn a_circuit_with_one_operation_mode_publishes_nothing() {
        assert!(operation_mode_descriptions(&[HvacOperationModeType::On]).is_none());
        assert!(operation_mode_relations(&[HvacOperationModeType::On]).is_none());
        assert!(
            operation_mode_descriptions(&[HvacOperationModeType::On, HvacOperationModeType::Off])
                .is_some()
        );
    }

    /// [MDSF-001]: exactly one mode is enabled, and the appliance can name it.
    #[test]
    fn mdsf_001_the_current_mode_is_read_back_as_a_mode() {
        let mut known = a_circuit();
        assert!(
            !known.is_complete(),
            "no current mode has been notified yet"
        );

        let eco_free = operation_mode_id(&HvacOperationModeType::Off).unwrap();
        known.learn(&system_function_state(eco_free, false, Some(true)));

        assert!(known.is_complete());
        assert_eq!(known.mode(), Some(&HvacOperationModeType::Off));
        assert_eq!(known.mode_changeable(), Some(true));
        assert_eq!(known.overrun_active(), Some(false));
    }

    /// The DHW system function is found by its type, not by our identifier.
    ///
    /// A heat pump serves heating *and* hot water from one HVAC feature, and reading the
    /// heating circuit's operation mode as the tank's is a manager that thinks the water
    /// is off while the house is being heated.
    #[test]
    fn the_hot_water_is_found_among_the_other_system_functions() {
        use crate::model::{
            HvacSystemFunctionDescriptionData, HvacSystemFunctionDescriptionListData,
            HvacSystemFunctionType,
        };

        let theirs = HvacSystemFunctionId(4);
        let descriptions =
            CmdData::HvacSystemFunctionDescriptionListData(HvacSystemFunctionDescriptionListData {
                hvac_system_function_description_data: Some(vec![
                    HvacSystemFunctionDescriptionData {
                        system_function_id: Some(SYSTEM_FUNCTION_ID),
                        system_function_type: Some(HvacSystemFunctionType::Heating),
                        ..Default::default()
                    },
                    HvacSystemFunctionDescriptionData {
                        system_function_id: Some(theirs),
                        system_function_type: Some(HvacSystemFunctionType::Dhw),
                        ..Default::default()
                    },
                ]),
            });

        assert_eq!(hvac::find_dhw_system_function(&descriptions), Some(theirs));

        let mut known = DhwSystemFunction::new();
        known.learn(&descriptions);
        assert_eq!(known.system_function(), Some(theirs));

        // And the heating circuit's state is not read as the tank's.
        known.learn(&CmdData::HvacSystemFunctionListData(
            HvacSystemFunctionListData {
                hvac_system_function_data: Some(vec![HvacSystemFunctionData {
                    system_function_id: Some(SYSTEM_FUNCTION_ID),
                    current_operation_mode_id: Some(HvacOperationModeId(2)),
                    ..Default::default()
                }]),
            },
        ));
        assert_eq!(known.mode_id(), None, "that was the heating, not the tank");
    }

    /// Table 14: `finished` is an announcement, not a state to rest in.
    #[test]
    fn mdsf_002_a_finished_overrun_settles_to_inactive() {
        let mut known = a_circuit();
        known.learn(&overrun_description());

        known.learn(&overrun_state(HvacOverrunStatus::Running));
        assert_eq!(known.overrun(), Some(OverrunReport::Running));
        assert!(!known.overrun_just_finished());

        known.learn(&overrun_state(HvacOverrunStatus::Finished));
        assert_eq!(
            known.overrun(),
            Some(OverrunReport::Inactive),
            "the resting state after a heating completes"
        );
        assert!(
            known.overrun_just_finished(),
            "and the announcement, separately"
        );

        known.learn(&overrun_state(HvacOverrunStatus::Inactive));
        assert!(
            !known.overrun_just_finished(),
            "which does not survive the next payload"
        );
    }

    /// An overrun that affects another system function is not this one's.
    #[test]
    fn an_overrun_on_another_system_function_is_ignored() {
        let mut known = a_circuit();
        known.learn(&CmdData::HvacOverrunDescriptionListData(
            HvacOverrunDescriptionListData {
                hvac_overrun_description_data: Some(vec![HvacOverrunDescriptionData {
                    overrun_id: Some(HvacOverrunId(9)),
                    overrun_type: Some(HvacOverrunType::Party),
                    affected_system_function_id: Some(vec![HvacSystemFunctionId(7)]),
                    ..Default::default()
                }]),
            },
        ));
        known.learn(&CmdData::HvacOverrunListData(HvacOverrunListData {
            hvac_overrun_data: Some(vec![HvacOverrunData {
                overrun_id: Some(HvacOverrunId(9)),
                overrun_status: Some(HvacOverrunStatus::Running),
                ..Default::default()
            }]),
        }));
        assert_eq!(
            known.overrun(),
            None,
            "a party mode is not a one-time hot water heating"
        );
    }

    /// The join with CDT: which setpoint the mode the circuit is *in* actually reads.
    #[test]
    fn the_current_mode_names_the_setpoint_a_write_would_reach() {
        use crate::model::SetpointId;
        use crate::usecases::hvac::cdt;

        let auto = operation_mode_id(&HvacOperationModeType::Auto).unwrap();
        let off = operation_mode_id(&HvacOperationModeType::Off).unwrap();

        let mut setpoints = cdt::DhwSetpoints::new();
        setpoints.learn(
            &cdt::system_function_relations(&[
                (auto, HvacOperationModeType::Auto, vec![SetpointId(1)]),
                (off, HvacOperationModeType::Off, vec![]),
            ])
            .expect("well formed"),
        );

        let mut known = a_circuit();
        known.learn(&system_function_state(auto, false, None));
        assert_eq!(known.current_setpoints(&setpoints), [SetpointId(1)]);

        known.learn(&system_function_state(off, false, None));
        assert_eq!(
            known.current_setpoints(&setpoints),
            [],
            "in `off` a write reaches no setpoint the circuit is reading"
        );
    }

    /// Six replies, in the worst order, and nothing is lost.
    ///
    /// A client reads all six functions at once and the replies come back independently.
    /// Reading state before the description that says which system function it belongs to
    /// is entirely ordinary, and an implementation that dropped it would work perfectly
    /// against a peer that happened to answer in the order it asked — which is every test
    /// whose other end is this crate.
    #[test]
    fn the_six_replies_may_arrive_in_any_order() {
        let modes = [
            HvacOperationModeType::Auto,
            HvacOperationModeType::On,
            HvacOperationModeType::Off,
        ];
        let payloads = [
            // Deliberately backwards: every value before its description.
            overrun_state(HvacOverrunStatus::Running),
            system_function_state(
                operation_mode_id(&HvacOperationModeType::On).unwrap(),
                true,
                None,
            ),
            overrun_description(),
            operation_mode_relations(&modes).expect("three modes"),
            operation_mode_descriptions(&modes).expect("three modes"),
            hvac::system_function_description(),
        ];

        let mut known = DhwSystemFunction::new();
        for payload in &payloads {
            assert!(
                known.learn(payload),
                "every one of them belongs to this use case"
            );
        }

        assert!(known.is_complete());
        assert_eq!(known.mode(), Some(&HvacOperationModeType::On));
        assert_eq!(known.related_modes().len(), 3);
        assert_eq!(known.overrun(), Some(OverrunReport::Running));
        assert_eq!(known.overrun_active(), Some(true));
    }

    /// Table 1: scenario 1 is mandatory for both; scenario 2 is R for the circuit.
    #[test]
    fn the_scenarios_carry_the_support_the_table_gives_them() {
        let support = |d: &UseCaseDescriptor, n: u32| {
            d.scenarios
                .iter()
                .find(|s| s.number == n)
                .map(|s| s.support)
                .expect("the scenario is defined")
        };
        assert_eq!(support(&DHW_CIRCUIT, 1), Support::Mandatory);
        assert_eq!(
            support(&DHW_CIRCUIT, 2),
            Support::Recommended,
            "a circuit need not offer a one-time heating"
        );
        assert_eq!(support(&MONITORING_APPLIANCE, 1), Support::Mandatory);
        assert_eq!(
            support(&MONITORING_APPLIANCE, 2),
            Support::Mandatory,
            "but an appliance must be able to read one"
        );
        assert_eq!(DHW_CIRCUIT.counterpart, MONITORING_APPLIANCE.actor);
        assert_eq!(MONITORING_APPLIANCE.counterpart, DHW_CIRCUIT.actor);
    }

    /// §3.2.2.2.1: one `HVAC` feature per entity, so CDT shares this one.
    #[test]
    fn a_circuit_serving_both_use_cases_has_one_hvac_feature() {
        let shared = with_cdt(1);
        assert!(
            shared
                .functions()
                .iter()
                .any(|f| f.function == Function::HvacSystemFunctionSetpointRelationListData),
            "CDT's relations live on the same feature"
        );
        assert!(
            shared
                .functions()
                .iter()
                .any(|f| f.function == Function::HvacOverrunListData),
            "beside this use case's own"
        );
    }
}
