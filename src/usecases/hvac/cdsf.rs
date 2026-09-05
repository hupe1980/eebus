//! Configuration of DHW System Function (CDSF).
//!
//! A *Configuration Appliance* sets a *DHW Circuit*'s operation mode, and starts and stops
//! its one-time hot water loading. It is the writeable counterpart of
//! [`mdsf`](super::mdsf), and [CDT-005] makes one of the two mandatory beside
//! [`cdt`](super::cdt): "a DHW Circuit that does not serve Monitoring of DHW System
//! Function SHALL serve Configuration of DHW System Function".
//!
//! Three scenarios (Table 1), and the asterisk matters:
//!
//! | | | circuit | appliance |
//! |---|---|---|---|
//! | 1 | **Set DHW operation mode** [CDSF-001] | M | O\* |
//! | 2 | **Start one-time DHW loading** [CDSF-002] | M | O\* |
//! | 3 | **Stop one-time DHW loading** [CDSF-003] | R | O |
//!
//! \* "At least one of the Scenarios SHALL be supported" — by the appliance. A circuit
//! serves 1 and 2 or it does not serve this use case.
//!
//! # The second lever that asks for *more*
//!
//! Scenario 2 is the one an energy manager reaches for. A one-time DHW loading is the
//! button in the bathroom, and starting it over EEBUS is a direct instruction to heat the
//! tank **now** — not a setpoint the circuit's controller may decline to act on, and not a
//! process announcement like [`ohpcf`](crate::usecases::ohpcf)'s. When the roof starts
//! exporting, this is the shortest path from "there is surplus" to "the tank is absorbing
//! it", and scenario 3 is how the manager gives it back when a cloud arrives.
//!
//! Scenario 1 is the slower lever and the more careful one: `eco` while the grid is
//! expensive, `on` while it is not. It is only available where the circuit says so —
//! `isOperationModeIdChangeable` — and only for modes the circuit relates to the hot water,
//! which is why [`SystemFunction::set_mode`] resolves it from what the peer published
//! rather than from a number.
//!
//! ```
//! use eebus::model::HvacOperationModeType;
//! use eebus::usecases::hvac::{cdsf, mdsf};
//!
//! # fn example() -> Result<(), eebus::usecases::hvac::system_function::ModeRefused> {
//! let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Eco];
//! let mut circuit = cdsf::reader();
//! circuit.learn(&cdsf::system_function_description());
//! circuit.learn(&cdsf::operation_mode_descriptions(&modes).expect("two modes"));
//! circuit.learn(&cdsf::operation_mode_relations(&modes).expect("two modes"));
//! circuit.learn(&cdsf::system_function_state(
//!     mdsf::operation_mode_id(&HvacOperationModeType::Auto).unwrap(),
//!     false,
//!     Some(true),
//! ));
//!
//! // The grid is expensive: ask for `eco`, by name.
//! let write = circuit.set_mode_named(&HvacOperationModeType::Eco)?;
//! # let _ = write;
//! # Ok(())
//! # }
//! ```
//!
//! # No binding
//!
//! §3.4.1.1, §3.4.2.1 and §3.4.3.1 all say the same thing: "Binding SHOULD NOT be used for
//! this Scenario". A circuit that required one would refuse every conformant Configuration
//! Appliance. [`hvac_feature`] is built accordingly; the decision that remains is the
//! application's, since its writes are deferred.

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacOverrunId, HvacOverrunStatus, HvacOverrunType, HvacSystemFunctionId,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::system_function::{self, SystemFunction};
use super::{DHW, system_function_id};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "configurationOfDhwSystemFunction";

/// The actor that holds the hot water.
pub const DHW_CIRCUIT_ACTOR: &str = "DHWCircuit";

/// The actor that configures it.
pub const CONFIGURATION_APPLIANCE_ACTOR: &str = "ConfigurationAppliance";

/// The `overrunType` of a one-time hot water heating (Table 7).
pub const ONE_TIME_DHW: HvacOverrunType = HvacOverrunType::OneTimeDhw;

/// The `overrunId` **this** implementation publishes its one-time heating under.
pub const OVERRUN_ID: HvacOverrunId = super::mdsf::OVERRUN_ID;

/// The `systemFunctionId` **this** implementation publishes the hot water under.
///
/// The same one [`mdsf`](super::mdsf) and [`cdt`](super::cdt) use: §2.4.1 requires a server
/// providing scenarios of both this use case and CDT to "provide the Scenarios on the same
/// Entity", and the identifiers are the entity's.
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = system_function_id(&DHW);

/// The `operationModeId`s **this** implementation gives the four modes.
pub fn operation_mode_id(kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
    system_function::operation_mode_id(kind)
}

// ---- the feature a DHW Circuit serves -----------------------------------------------

/// Builds the writeable `HVAC` feature the three scenarios are served from (Table 8).
///
/// `hvacSystemFunctionListData` is writeable for scenario 1 and `hvacOverrunListData` for
/// scenarios 2 and 3; everything else is read-only, because the modes and the relations are
/// the circuit's own statement of what it can do.
///
/// Writes are deferred and need no binding — see
/// [`system_function::writeable`]. Feed each
/// [`WriteRequested`](crate::spine::SpineEvent::WriteRequested) to
/// [`SystemFunction::apply`].
pub fn hvac_feature(address: u32) -> LocalFeature {
    system_function::writeable(address, true)
}

/// The same feature, also carrying [`cdt`](super::cdt)'s setpoint relations.
///
/// §2.4.1: a server providing both SHALL provide them on the same entity, and §3.2.2.2.1
/// gives an entity one `HVAC` feature. This is it.
pub fn with_cdt(address: u32) -> LocalFeature {
    system_function::with_setpoint_relations(hvac_feature(address))
}

// ---- what a DHW Circuit publishes ---------------------------------------------------

/// The system function description (Table 9).
pub fn system_function_description() -> CmdData {
    system_function::description(SYSTEM_FUNCTION_ID, DHW)
}

/// The operation modes the circuit supports (Table 10).
pub fn operation_mode_descriptions(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_descriptions(modes)
}

/// Which modes belong to the DHW system function (Table 11).
pub fn operation_mode_relations(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_relations(SYSTEM_FUNCTION_ID, modes)
}

/// The mode the circuit is in now (Table 12).
///
/// `changeable` is `isOperationModeIdChangeable`, and Table 6 fixes it at `"true"` for a
/// circuit serving scenario 1: an appliance told `false` will not try, which is the point of
/// publishing it, and a circuit that serves the scenario and says `false` is contradicting
/// itself.
pub fn system_function_state(
    current: HvacOperationModeId,
    overrun_active: bool,
    changeable: Option<bool>,
) -> CmdData {
    system_function::state(SYSTEM_FUNCTION_ID, current, overrun_active, changeable)
}

/// The one-time hot water heating this circuit offers (Table 7).
pub fn overrun_description() -> CmdData {
    system_function::overrun_description(OVERRUN_ID, ONE_TIME_DHW, &[SYSTEM_FUNCTION_ID])
}

/// What the one-time heating is doing (Table 7).
pub fn overrun_state(status: HvacOverrunStatus) -> CmdData {
    system_function::overrun_state(OVERRUN_ID, status)
}

/// A reader — and writer — for this circuit's hot water.
pub fn reader() -> SystemFunction {
    SystemFunction::dhw()
}

// ---- descriptors ---------------------------------------------------------------------

const DHW_CIRCUIT_ENTITIES: &[EntityType] = &[EntityType::DHWCircuit];
/// The Configuration Appliance sits behind any entity (Figure 4, `entityType = <any>`).
const CONFIGURATION_APPLIANCE_ENTITIES: &[EntityType] = &[];

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
    FunctionUse::server_writeable_unbound(FeatureType::HVAC, Function::HvacSystemFunctionListData),
];

const SERVER_OVERRUN: &[FunctionUse] = &[
    FunctionUse::server(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::server(FeatureType::HVAC, Function::HvacSystemFunctionListData),
    FunctionUse::server(FeatureType::HVAC, Function::HvacOverrunDescriptionListData),
    FunctionUse::server_writeable_unbound(FeatureType::HVAC, Function::HvacOverrunListData),
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
    FunctionUse::client_writes_unbound(FeatureType::HVAC, Function::HvacSystemFunctionListData),
];

const CLIENT_OVERRUN: &[FunctionUse] = &[
    FunctionUse::client(
        FeatureType::HVAC,
        Function::HvacSystemFunctionDescriptionListData,
    ),
    FunctionUse::client(FeatureType::HVAC, Function::HvacSystemFunctionListData),
    FunctionUse::client(FeatureType::HVAC, Function::HvacOverrunDescriptionListData),
    FunctionUse::client_writes_unbound(FeatureType::HVAC, Function::HvacOverrunListData),
];

const SET_MODE: &str = "Set DHW operation mode";
const START_OVERRUN: &str = "Start one-time DHW loading";
const STOP_OVERRUN: &str = "Stop one-time DHW loading";

/// The DHW Circuit: the actor being configured.
pub static DHW_CIRCUIT: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: DHW_CIRCUIT_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: DHW_CIRCUIT_ENTITIES,
    counterpart: CONFIGURATION_APPLIANCE_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: SET_MODE,
            support: Support::Mandatory,
            functions: SERVER_MODE,
        },
        Scenario {
            number: 2,
            name: START_OVERRUN,
            support: Support::Mandatory,
            functions: SERVER_OVERRUN,
        },
        Scenario {
            number: 3,
            name: STOP_OVERRUN,
            support: Support::Recommended,
            functions: SERVER_OVERRUN,
        },
    ],
};

/// The Configuration Appliance: the actor configuring it.
///
/// Every scenario is optional for this actor, with the footnote that at least one has to be
/// supported. [`required_scenarios`](UseCaseDescriptor::required_scenarios) is therefore
/// empty, which is the specification's shape rather than an omission — an appliance that
/// can only start a one-time loading is a conformant Configuration Appliance.
pub static CONFIGURATION_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CONFIGURATION_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CONFIGURATION_APPLIANCE_ENTITIES,
    counterpart: DHW_CIRCUIT_ACTOR,
    scenarios: &[
        Scenario {
            number: 1,
            name: SET_MODE,
            support: Support::Optional,
            functions: CLIENT_MODE,
        },
        Scenario {
            number: 2,
            name: START_OVERRUN,
            support: Support::Optional,
            functions: CLIENT_OVERRUN,
        },
        Scenario {
            number: 3,
            name: STOP_OVERRUN,
            support: Support::Optional,
            functions: CLIENT_OVERRUN,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spine::WriteBinding;
    use crate::usecases::hvac::system_function::{ModeRefused, Request};
    use alloc::vec::Vec;

    fn a_circuit() -> SystemFunction {
        let modes = [
            HvacOperationModeType::Auto,
            HvacOperationModeType::On,
            HvacOperationModeType::Eco,
        ];
        let mut known = reader();
        known.learn(&system_function_description());
        known.learn(&operation_mode_descriptions(&modes).expect("three modes"));
        known.learn(&operation_mode_relations(&modes).expect("three modes"));
        known.learn(&overrun_description());
        known.learn(&system_function_state(
            operation_mode_id(&HvacOperationModeType::Auto).unwrap(),
            false,
            Some(true),
        ));
        known
    }

    /// [CDSF-001]: the mode is set by name, resolved through what the circuit published.
    #[test]
    fn cdsf_001_the_mode_is_set_by_name() {
        let known = a_circuit();
        let write = known
            .set_mode_named(&HvacOperationModeType::Eco)
            .expect("eco is one of this circuit's modes");
        let CmdData::HvacSystemFunctionListData(list) = &write else {
            panic!("expected the system function list");
        };
        let entry = &list.hvac_system_function_data.as_ref().unwrap()[0];
        assert_eq!(entry.system_function_id, Some(SYSTEM_FUNCTION_ID));
        assert_eq!(
            entry.current_operation_mode_id,
            operation_mode_id(&HvacOperationModeType::Eco)
        );
        assert!(
            entry.is_overrun_active.is_none(),
            "a partial write sets the one element it means to"
        );
    }

    /// A mode the circuit does not relate to its hot water is refused before it is sent.
    ///
    /// The modes are described once for the device and a subset of them is related to each
    /// system function, so `off` may be a room heating mode and not a hot water one. Writing
    /// it is well formed and asks for something that does not exist.
    #[test]
    fn cdsf_001_a_mode_this_function_does_not_have_is_refused() {
        let known = a_circuit();
        assert_eq!(
            known.set_mode_named(&HvacOperationModeType::Off),
            Err(ModeRefused::NotRelated)
        );
        assert_eq!(
            known.set_mode(HvacOperationModeId(99)),
            Err(ModeRefused::NotRelated)
        );
    }

    /// `isOperationModeIdChangeable: false` is a refusal the circuit published in advance.
    #[test]
    fn a_circuit_that_says_its_mode_is_fixed_is_believed() {
        let mut known = a_circuit();
        known.learn(&system_function_state(
            operation_mode_id(&HvacOperationModeType::Auto).unwrap(),
            false,
            Some(false),
        ));
        assert_eq!(
            known.set_mode_named(&HvacOperationModeType::Eco),
            Err(ModeRefused::NotChangeable)
        );
    }

    /// [CDSF-002] and [CDSF-003]: start and stop the one-time loading.
    #[test]
    fn cdsf_002_and_003_the_one_time_loading_is_started_and_stopped() {
        let known = a_circuit();

        let start = known.start_overrun().expect("the circuit described one");
        let CmdData::HvacOverrunListData(list) = &start else {
            panic!("expected the overrun list");
        };
        let entry = &list.hvac_overrun_data.as_ref().unwrap()[0];
        assert_eq!(entry.overrun_id, Some(OVERRUN_ID));
        assert_eq!(
            entry.overrun_status.as_ref(),
            Some(&HvacOverrunStatus::Active)
        );

        let stop = known.stop_overrun().expect("the same one");
        let CmdData::HvacOverrunListData(list) = &stop else {
            panic!("expected the overrun list");
        };
        assert_eq!(
            list.hvac_overrun_data.as_ref().unwrap()[0]
                .overrun_status
                .as_ref(),
            Some(&HvacOverrunStatus::Inactive)
        );
    }

    /// A circuit that never described an overrun cannot be asked to start one.
    #[test]
    fn an_overrun_the_circuit_does_not_have_is_refused() {
        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::On];
        let mut known = reader();
        known.learn(&system_function_description());
        known.learn(&operation_mode_descriptions(&modes).unwrap());
        known.learn(&operation_mode_relations(&modes).unwrap());
        assert_eq!(known.start_overrun(), Err(ModeRefused::NoOverrun));
    }

    /// The server's side: a write is read back into what it asked for, or refused.
    #[test]
    fn the_circuit_decides_before_it_acknowledges() {
        let circuit = a_circuit();

        let write = circuit.set_mode_named(&HvacOperationModeType::On).unwrap();
        assert_eq!(
            circuit.apply(&write),
            Ok(Request::SetMode(
                operation_mode_id(&HvacOperationModeType::On).unwrap()
            ))
        );

        assert_eq!(
            circuit.apply(&circuit.start_overrun().unwrap()),
            Ok(Request::StartOverrun(OVERRUN_ID))
        );
        assert_eq!(
            circuit.apply(&circuit.stop_overrun().unwrap()),
            Ok(Request::StopOverrun(OVERRUN_ID))
        );

        // The fragment, not the resolved state. A circuit with two system functions holds
        // both entries in the same list, so the resolved payload cannot say which one was
        // written — it holds the other's current mode too, unchanged and indistinguishable
        // from an instruction.
        let elsewhere = system_function::set_operation_mode(
            crate::usecases::hvac::system_function_id(&crate::usecases::hvac::HEATING),
            operation_mode_id(&HvacOperationModeType::On).unwrap(),
        );
        assert_eq!(
            circuit.apply(&elsewhere),
            Err(ModeRefused::NotAddressed),
            "a hot water reader hands on a write addressed to the room heating"
        );

        // A mode this function does not have, arriving from a peer that guessed.
        let bogus = system_function::set_operation_mode(SYSTEM_FUNCTION_ID, HvacOperationModeId(7));
        assert_eq!(circuit.apply(&bogus), Err(ModeRefused::NotRelated));
        assert_eq!(
            ModeRefused::NotRelated.error_number(),
            crate::model::ErrorNumber::DestinationUnknown
        );

        // `finished` is the circuit's announcement, not the appliance's instruction.
        let finished = system_function::overrun_state(OVERRUN_ID, HvacOverrunStatus::Finished);
        assert_eq!(circuit.apply(&finished), Err(ModeRefused::NoOverrun));
    }

    /// §3.4.x: "Binding SHOULD NOT be used for this Scenario", on both sides.
    #[test]
    fn cdsf_writes_need_no_binding() {
        assert_eq!(hvac_feature(1).write_binding(), WriteBinding::NotRequired);
        assert_eq!(
            CONFIGURATION_APPLIANCE.features_needing_binding().count(),
            0
        );
    }

    /// Table 1, including the footnote: every appliance scenario is optional.
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
        assert_eq!(support(&DHW_CIRCUIT, 2), Support::Mandatory);
        assert_eq!(
            support(&DHW_CIRCUIT, 3),
            Support::Recommended,
            "a circuit that can start a one-time loading need not be able to stop one"
        );
        assert_eq!(
            CONFIGURATION_APPLIANCE
                .required_scenarios()
                .collect::<Vec<_>>(),
            [] as [u32; 0],
            "\"at least one of the Scenarios SHALL be supported\" is not \"all of them\""
        );
        assert_eq!(DHW_CIRCUIT.counterpart, CONFIGURATION_APPLIANCE.actor);
    }
}
