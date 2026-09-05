//! Monitoring of DHW System Function (MDSF).
//!
//! A *Monitoring Appliance* reads which operation mode a *DHW Circuit* is in, and whether
//! a one-time hot water heating is running. It is the half of the DHW pair that says
//! **what the circuit is doing**; [`cdt`](super::cdt) is the half that changes the
//! temperature and [`cdsf`](super::cdsf) the half that changes the mode.
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
//! [`current_setpoints`](super::system_function::SystemFunction::current_setpoints) is
//! that join.
//!
//! The overrun matters for the opposite reason. A one-time heating overrides the mode
//! until it is done, so a manager that sees the tank drawing power while the mode says
//! `off` is not looking at a fault; it is looking at somebody pressing the button in the
//! bathroom. Reporting that as an anomaly is how an energy manager loses a user's trust.
//!
//! # The reader
//!
//! [`SystemFunction::dhw`] — the same reader the other five system-function use cases use,
//! told which function it is following. Everything it does is described in
//! [`super::system_function`].
//!
//! ```
//! use eebus::model::HvacOperationModeType;
//! use eebus::usecases::hvac::{mdsf, system_function::SystemFunction};
//!
//! let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];
//! let mut circuit = SystemFunction::dhw();
//! circuit.learn(&mdsf::system_function_description());
//! circuit.learn(&mdsf::operation_mode_descriptions(&modes).expect("two modes"));
//! circuit.learn(&mdsf::operation_mode_relations(&modes).expect("two modes"));
//! circuit.learn(&mdsf::system_function_state(
//!     mdsf::operation_mode_id(&HvacOperationModeType::Off).unwrap(),
//!     false,
//!     Some(true),
//! ));
//!
//! assert_eq!(circuit.mode(), Some(&HvacOperationModeType::Off));
//! ```

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacOverrunId, HvacOverrunStatus, HvacOverrunType, HvacSystemFunctionId,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::peer::{self, HvacPeer, Subject};
use super::system_function::{self, SystemFunction};
use super::{DHW, system_function_id};

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

/// The `systemFunctionId` **this** implementation publishes the hot water under.
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = system_function_id(&DHW);

/// The `operationModeId`s **this** implementation gives the four modes.
///
/// See [`system_function::operation_mode_id`], which every use case in the family shares:
/// modes are described once for the device, and which of them a function *has* is the
/// relation rather than a second numbering.
pub fn operation_mode_id(kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
    system_function::operation_mode_id(kind)
}

// ---- the feature a DHW Circuit serves -----------------------------------------------

/// Builds the `HVAC` feature both scenarios are served from (Table 8).
///
/// Read-only throughout: this use case reports, and [`cdsf`](super::cdsf) is where the mode
/// is changed. §3.2.2.2.1 adds a rule worth honouring — **at most one** `HVAC` feature per
/// entity — so a circuit serving CDT as well puts both use cases' functions on this one
/// feature. [`with_cdt`] does that.
pub fn hvac_feature(address: u32) -> LocalFeature {
    system_function::hvac_feature(address, true)
}

/// The same feature, also carrying [`cdt`](super::cdt)'s setpoint relations.
///
/// §3.2.2.2.1 of both specifications says an entity holds at most one feature of a given
/// type, so a DHW Circuit serving MDSF *and* CDT — which is the ordinary case, since one
/// of the two system-function use cases is mandatory alongside CDT [CDT-005] — cannot have
/// an `HVAC` feature each. This is the one they share.
pub fn with_cdt(address: u32) -> LocalFeature {
    system_function::with_setpoint_relations(hvac_feature(address))
}

/// What this use case is about, for [`HvacApplianceActor`](super::HvacApplianceActor):
/// which system function, which overrun where there is one, and which setpoint scope.
pub const SUBJECT: Subject = Subject::mode_with_overrun(DHW, ONE_TIME_DHW);

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The system function description this use case publishes (Table 9).
pub fn system_function_description() -> CmdData {
    system_function::description(SYSTEM_FUNCTION_ID, DHW)
}

/// The operation modes the circuit supports (Table 10).
///
/// [`None`] for fewer than two, which §2.3.1.1 does not permit.
pub fn operation_mode_descriptions(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_descriptions(modes)
}

/// Which modes belong to the DHW system function (Table 11).
pub fn operation_mode_relations(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_relations(SYSTEM_FUNCTION_ID, modes)
}

/// The mode the circuit is in now, and whether an overrun is overriding it (Table 12).
pub fn system_function_state(
    current: HvacOperationModeId,
    overrun_active: bool,
    changeable: Option<bool>,
) -> CmdData {
    system_function::state(SYSTEM_FUNCTION_ID, current, overrun_active, changeable)
}

/// The one-time hot water heating this circuit offers (Table 13).
pub fn overrun_description() -> CmdData {
    system_function::overrun_description(OVERRUN_ID, ONE_TIME_DHW, &[SYSTEM_FUNCTION_ID])
}

/// What the one-time heating is doing (Table 14).
///
/// Table 14 puts a rule on `finished` that is easy to get wrong and impossible to see: it
/// **MAY only be used as a notification directly after the overrun finished**, the status
/// SHALL become `inactive` after that, and it SHOULD NOT appear in a reply. A circuit that
/// leaves `finished` standing tells every appliance that reads it later that a heating has
/// just completed — repeatedly.
/// [`OverrunReport`](super::system_function::OverrunReport) is the type that makes that
/// shape hard to get wrong.
pub fn overrun_state(status: HvacOverrunStatus) -> CmdData {
    system_function::overrun_state(OVERRUN_ID, status)
}

/// A reader following this circuit's hot water.
pub fn reader() -> SystemFunction {
    SystemFunction::dhw()
}

// ---- what a Monitoring Appliance finds ----------------------------------

/// Finds a DHW circuit's features from its detailed discovery and use-case data.
///
/// The guide's §3.3 rule doing its work: the `HVAC` feature is the one on the entity that
/// **announced this use case**, not whichever the appliance happens to carry. A device
/// that heats water and two rooms has one `HVAC` feature per entity and every one of them
/// answers to the same lookup by type.
///
/// Six reads and one subscription — the same six [`cdsf`](super::cdsf) needs, because the
/// two use cases are the same data read and written. The subscription is what makes this
/// use case worth having at all: a circuit whose mode is changed at the wall panel changes
/// it without telling anybody otherwise.
///
/// Returns [`None`] until the peer has announced both the use case and the features its
/// scenarios are served from. What comes next is [`HvacPeer::follow`], and it is not
/// optional: a located peer is an address, not a conversation.
pub fn locate(remote: &crate::spine::RemoteDevice) -> Option<HvacPeer> {
    peer::locate(remote, &DHW_CIRCUIT, &SUBJECT)
}

/// Every DHW circuit on one device that serves it.
///
/// A device may hold more than one — a heat-pump gateway announces the use case once per
/// entity — and each is its own `HVAC` feature with its own state. See
/// [`peer::locate_all`].
pub fn locate_all(remote: &crate::spine::RemoteDevice) -> alloc::vec::Vec<HvacPeer> {
    peer::locate_all(remote, &DHW_CIRCUIT, &SUBJECT)
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

const MONITOR_MODE: &str = "Monitor DHW operation mode";
const MONITOR_OVERRUN: &str = "Monitor DHW overrun";

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
            name: MONITOR_MODE,
            support: Support::Mandatory,
            functions: SERVER_MODE,
        },
        Scenario {
            number: 2,
            name: MONITOR_OVERRUN,
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
            name: MONITOR_MODE,
            support: Support::Mandatory,
            functions: CLIENT_MODE,
        },
        Scenario {
            number: 2,
            name: MONITOR_OVERRUN,
            support: Support::Mandatory,
            functions: CLIENT_OVERRUN,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        HvacOverrunData, HvacOverrunDescriptionData, HvacOverrunDescriptionListData,
        HvacOverrunListData, HvacSystemFunctionData, HvacSystemFunctionListData,
    };
    use crate::usecases::hvac::{self, system_function::OverrunReport};
    use alloc::vec;

    fn a_circuit() -> SystemFunction {
        let modes = [
            HvacOperationModeType::Auto,
            HvacOperationModeType::On,
            HvacOperationModeType::Off,
        ];
        let mut known = reader();
        known.learn(&system_function_description());
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

        let off = operation_mode_id(&HvacOperationModeType::Off).unwrap();
        known.learn(&system_function_state(off, false, Some(true)));

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
        let theirs = HvacSystemFunctionId(4);
        let descriptions = system_function::descriptions(&[
            (SYSTEM_FUNCTION_ID, hvac::HEATING),
            (theirs, hvac::DHW),
        ]);

        assert_eq!(
            hvac::find_system_function(&descriptions, &hvac::DHW),
            Some(theirs)
        );

        let mut known = reader();
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

        let mut setpoints = cdt::reader();
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
            system_function_description(),
        ];

        let mut known = reader();
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

    /// Read-only: this use case reports, and `cdsf` is the one that changes anything.
    #[test]
    fn nothing_here_is_writeable() {
        let feature = hvac_feature(1);
        assert!(
            feature.functions().iter().all(|f| !f.operations.write),
            "MDSF Table 8 gives every function `read` and no `write`"
        );
    }
}
