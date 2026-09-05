//! Configuration of Room Heating System Function (CRHSF).
//!
//! A *Configuration Appliance* sets the operation mode of a room's heating. It is the
//! writeable counterpart of [`mrhsf`](super::mrhsf), and the lever that lets a
//! controller put a room into `eco` while the grid is expensive and back into `auto` when
//! it is not.
//!
//! One scenario, mandatory for both actors (Table 1):
//!
//! 1. **Set room heating operation mode** — `auto`, `on`, `off` or `eco`, exactly one of
//!    them enabled at a time.
//!
//! # No binding
//!
//! §3.4.1.1: "Binding SHOULD NOT be used for this Scenario". A room that insisted on one
//! would refuse every conformant Configuration Appliance, so [`hvac_feature`] is built with
//! [`with_unbound_writes`](crate::spine::LocalFeature::with_unbound_writes) and its writes
//! are deferred to the application instead — which is where a policy of "only the manager I
//! was commissioned with" belongs, since that is the layer that can see who is asking.
//!
//! # What a write is checked against
//!
//! [`SystemFunction::set_mode_named`](super::system_function::SystemFunction::set_mode_named)
//! resolves a mode by name through what the room published, and refuses a mode this
//! function does not relate to or one the room said is not changeable. The modes are
//! described once for the device and a subset is related to each function, so `eco` may be
//! a hot water mode and not a room one — and writing it is a well-formed request for
//! something that does not exist.
//!
//! # One `HVAC` feature, three system functions
//!
//! §3.2.2.2.1 gives an entity at most one feature of a type, so a room that both heats and
//! cools publishes both functions on the same `HVAC` feature, in the same lists, told apart
//! by `systemFunctionId` alone. [`reader`] is told which one it is following, and
//! [`SYSTEM_FUNCTION_ID`] is the number *this* implementation publishes it under —
//! never the number to assume of a peer.

use crate::model::{
    CmdData, EntityType, FeatureType, Function, HvacOperationModeId, HvacOperationModeType,
    HvacSystemFunctionId,
};
use crate::spine::LocalFeature;
use crate::usecases::descriptor::{ActorRole, FunctionUse, Scenario, Support, UseCaseDescriptor};

use super::peer::{self, HvacPeer, Subject};
use super::system_function::{self, SystemFunction};
use super::{HEATING, system_function_id};

/// The version this implementation speaks.
pub const VERSION: &str = "1.0.0";

/// The `useCaseName` both actors announce.
pub const NAME: &str = "configurationOfRoomHeatingSystemFunction";

/// The actor that *is* the room: "a logical or physical indoor space" of a building.
pub const HVAC_ROOM_ACTOR: &str = "HVACRoom";

/// The actor on the other end.
pub const CONFIGURATION_APPLIANCE_ACTOR: &str = "ConfigurationAppliance";

/// The `systemFunctionId` **this** implementation publishes the room heating under.
///
/// Distinct from the hot water's and from the cooling's, which is what lets one
/// `HVAC` feature carry all three. See
/// [`system_function_id`](super::system_function_id()).
pub const SYSTEM_FUNCTION_ID: HvacSystemFunctionId = system_function_id(&HEATING);

/// The `operationModeId`s **this** implementation gives the four modes.
///
/// Shared with every other system function on the device: the modes are described once and
/// [`operation_mode_relations`] is what says which function has which of them.
pub fn operation_mode_id(kind: &HvacOperationModeType) -> Option<HvacOperationModeId> {
    system_function::operation_mode_id(kind)
}

// ---- the feature an HVAC Room serves --------------------------------------------------

/// Builds the `HVAC` feature the scenario is served from (Table 7).
///
/// `hvacSystemFunctionListData` is writeable — that is the whole scenario — and everything
/// else is read-only, because the modes and the relations are the room's own statement of
/// what it can do. Writes are deferred and need no binding.
pub fn hvac_feature(address: u32) -> LocalFeature {
    system_function::writeable(address, false)
}

/// The same feature, also carrying a room temperature use case's setpoint relations.
///
/// A room serving [`crht`](super::crht) or [`crct`](super::crct) as well puts their
/// relations here: one entity, one `HVAC` feature.
pub fn with_setpoints(address: u32) -> LocalFeature {
    system_function::with_setpoint_relations(hvac_feature(address))
}

// ---- what an HVAC Room publishes ------------------------------------------------------

/// The system function description.
pub fn system_function_description() -> CmdData {
    system_function::description(SYSTEM_FUNCTION_ID, HEATING)
}

/// The operation modes the room supports.
///
/// [`None`] for fewer than two: a function with one mode cannot report a *change* of mode,
/// which is the whole of the scenario.
pub fn operation_mode_descriptions(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_descriptions(modes)
}

/// Which of those modes belong to the room heating.
pub fn operation_mode_relations(modes: &[HvacOperationModeType]) -> Option<CmdData> {
    system_function::operation_mode_relations(SYSTEM_FUNCTION_ID, modes)
}

/// The mode the room heating is in now.
///
/// There is no overrun in this use case — only the hot water has one — so `overrun_active`
/// is `false` unless some *other* use case's overrun affects this function.
pub fn system_function_state(
    current: HvacOperationModeId,
    overrun_active: bool,
    changeable: Option<bool>,
) -> CmdData {
    system_function::state(SYSTEM_FUNCTION_ID, current, overrun_active, changeable)
}

/// A reader following the room heating.
pub fn reader() -> SystemFunction {
    SystemFunction::heating()
}

/// What this use case is about, for [`HvacApplianceActor`](super::HvacApplianceActor):
/// which system function, which overrun where there is one, and which setpoint scope.
pub const SUBJECT: Subject = Subject::mode(HEATING);

// ---- what a Configuration Appliance finds ----------------------------------

/// Finds a room's features from its detailed discovery and use-case data.
///
/// The guide's §3.3 rule doing its work: the `HVAC` feature is the one on the entity that
/// **announced this use case**, not whichever the appliance happens to carry. A device
/// that heats water and two rooms has one `HVAC` feature per entity and every one of them
/// answers to the same lookup by type.
///
/// Four reads and one subscription. There is no overrun in scope: only the hot water has
/// one.
///
/// Returns [`None`] until the peer has announced both the use case and the features its
/// scenarios are served from. What comes next is [`HvacPeer::follow`], and it is not
/// optional: a located peer is an address, not a conversation.
pub fn locate(remote: &crate::spine::RemoteDevice) -> Option<HvacPeer> {
    peer::locate(remote, &HVAC_ROOM, &SUBJECT)
}

/// Every room on one device that serves it.
///
/// A device may hold more than one — a heat-pump gateway announces the use case once per
/// entity — and each is its own `HVAC` feature with its own state. See
/// [`peer::locate_all`].
pub fn locate_all(remote: &crate::spine::RemoteDevice) -> alloc::vec::Vec<HvacPeer> {
    peer::locate_all(remote, &HVAC_ROOM, &SUBJECT)
}

// ---- descriptors ---------------------------------------------------------------------

const HVAC_ROOM_ENTITIES: &[EntityType] = &[EntityType::HVACRoom];
/// The other actor sits behind any entity.
const CONFIGURATION_APPLIANCE_ENTITIES: &[EntityType] = &[];

const SERVER_FUNCTIONS: &[FunctionUse] = &[
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

const CLIENT_FUNCTIONS: &[FunctionUse] = &[
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

const SCENARIO_NAME: &str = "Set room heating operation mode";

/// The HVAC Room: the actor whose heating is set.
pub static HVAC_ROOM: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: HVAC_ROOM_ACTOR,
    role: ActorRole::Server,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: HVAC_ROOM_ENTITIES,
    counterpart: CONFIGURATION_APPLIANCE_ACTOR,
    scenarios: &[Scenario {
        number: 1,
        name: SCENARIO_NAME,
        support: Support::Mandatory,
        functions: SERVER_FUNCTIONS,
    }],
};

/// The Configuration Appliance: the actor setting it.
pub static CONFIGURATION_APPLIANCE: UseCaseDescriptor = UseCaseDescriptor {
    name: NAME,
    actor: CONFIGURATION_APPLIANCE_ACTOR,
    role: ActorRole::Client,
    version: VERSION,
    document_sub_revision: "release",
    entity_types: CONFIGURATION_APPLIANCE_ENTITIES,
    counterpart: HVAC_ROOM_ACTOR,
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
    use crate::usecases::hvac;
    use alloc::vec::Vec;

    fn a_room() -> SystemFunction {
        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];
        let mut known = reader();
        known.learn(&system_function_description());
        known.learn(&operation_mode_descriptions(&modes).expect("two modes"));
        known.learn(&operation_mode_relations(&modes).expect("two modes"));
        known
    }

    /// The current mode is read back as a mode.
    #[test]
    fn the_current_mode_is_read_back_as_a_mode() {
        let mut known = a_room();
        assert!(
            !known.is_complete(),
            "no current mode has been notified yet"
        );

        known.learn(&system_function_state(
            operation_mode_id(&HvacOperationModeType::Off).unwrap(),
            false,
            Some(true),
        ));
        assert!(known.is_complete());
        assert_eq!(known.mode(), Some(&HvacOperationModeType::Off));
        assert_eq!(known.system_function(), Some(SYSTEM_FUNCTION_ID));
    }

    /// The heating and the hot water live on one feature and do not collide.
    ///
    /// Both are published in the same three lists under different `systemFunctionId`s, and
    /// a reader that matched on anything else would answer a question about the room with a
    /// fact about the tank.
    #[test]
    fn the_room_and_the_hot_water_do_not_collide() {
        let descriptions = system_function::descriptions(&[
            (hvac::system_function_id(&hvac::DHW), hvac::DHW),
            (SYSTEM_FUNCTION_ID, HEATING),
        ]);
        let modes = [HvacOperationModeType::Auto, HvacOperationModeType::Off];

        let mut room = reader();
        let mut tank = hvac::mdsf::reader();
        for payload in [
            descriptions,
            operation_mode_descriptions(&modes).unwrap(),
            operation_mode_relations(&modes).unwrap(),
            system_function_state(
                operation_mode_id(&HvacOperationModeType::Off).unwrap(),
                false,
                None,
            ),
        ] {
            room.learn(&payload);
            tank.learn(&payload);
        }

        assert_eq!(room.mode(), Some(&HvacOperationModeType::Off));
        assert_eq!(
            tank.mode(),
            None,
            "the hot water's own state was never published"
        );
        assert_ne!(room.system_function(), tank.system_function());
    }

    /// Both actors implement the one scenario (Table 1).
    #[test]
    fn both_actors_implement_the_only_scenario() {
        for descriptor in [&HVAC_ROOM, &CONFIGURATION_APPLIANCE] {
            assert_eq!(descriptor.name, NAME);
            assert_eq!(descriptor.version, "1.0.0");
            assert_eq!(descriptor.required_scenarios().collect::<Vec<_>>(), [1]);
        }
        assert!(HVAC_ROOM.permits_entity(&EntityType::HVACRoom));
        assert!(!HVAC_ROOM.permits_entity(&EntityType::DHWCircuit));
        assert_eq!(HVAC_ROOM.counterpart, CONFIGURATION_APPLIANCE.actor);
    }

    /// §3.4.1.1: "Binding SHOULD NOT be used for this Scenario", on both sides.
    #[test]
    fn writes_here_need_no_binding() {
        use crate::spine::WriteBinding;
        assert_eq!(hvac_feature(1).write_binding(), WriteBinding::NotRequired);
        assert_eq!(
            CONFIGURATION_APPLIANCE.features_needing_binding().count(),
            0
        );
    }

    /// A mode the room does not relate to this function is refused before it is sent.
    #[test]
    fn a_mode_this_function_does_not_have_is_refused() {
        use crate::usecases::hvac::system_function::ModeRefused;
        let mut known = a_room();
        known.learn(&system_function_state(
            operation_mode_id(&HvacOperationModeType::Auto).unwrap(),
            false,
            Some(true),
        ));
        assert!(known.set_mode_named(&HvacOperationModeType::Auto).is_ok());
        assert_eq!(
            known.set_mode_named(&HvacOperationModeType::Eco),
            Err(ModeRefused::NotRelated),
            "this room has `auto` and `off`, and nothing else"
        );
    }
}
