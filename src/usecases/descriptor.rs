//! Describing a use case: actors, scenarios and the SPINE resources each one needs.
//!
//! A use-case specification is, at bottom, a table: this actor, in this scenario, offers
//! these functions of these features. [`UseCaseDescriptor`] is that table as data, which
//! three things then read from one place:
//!
//! * the local device model, to build the entities and features an actor must expose;
//! * `nodeManagementUseCaseData`, so a peer's use-case discovery finds them; and
//! * the pre-scenario communication, which knows from the same table which features to
//!   bind and subscribe to.
//!
//! Keeping it as data rather than as code in each use case is what lets the engine drive
//! discovery, binding and subscription generically, and what makes the compliance matrix
//! something that can be generated rather than maintained by hand.

use crate::model::{EntityType, FeatureType, Function, Role, UseCaseActor, UseCaseName};

/// Whether a scenario has to be implemented (LPC UC TS §3.1.3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// `M`: the specification says SHALL.
    Mandatory,
    /// `R`: SHOULD.
    Recommended,
    /// `O`: MAY.
    Optional,
}

/// Whether an actor drives a use case or serves it.
///
/// The use-case implementation guide §2.1 introduced this distinction, which the
/// specifications themselves leave implicit: the *client actor* orchestrates — it reads
/// data and writes limits — while the *server actor* holds the state and reacts. A
/// secondary function that happens to run the other way, such as the Energy Guard's own
/// heartbeat, does not change the classification (§2.1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorRole {
    /// Orchestrates the use case.
    Client,
    /// Provides the resource the use case acts on.
    Server,
}

/// One function an actor offers or uses in a scenario.
///
/// Not `Copy`: SPINE's feature and function enumerations are extensible, so both carry
/// an `Other(String)` for values a later version may add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionUse {
    /// The feature that carries the function.
    pub feature: FeatureType,
    /// Whether this actor is the client or the server of that feature.
    pub role: Role,
    /// The function itself.
    pub function: Function,
    /// Whether the actor may write it.
    pub writeable: bool,
    /// Whether a binding is needed before writing (SPINE §7.3).
    pub needs_binding: bool,
}

impl FunctionUse {
    /// A function this actor serves and a peer only reads.
    pub const fn server(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Server,
            function,
            writeable: false,
            needs_binding: false,
        }
    }

    /// A function this actor serves and a bound peer may write.
    pub const fn server_writeable(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Server,
            function,
            writeable: true,
            needs_binding: true,
        }
    }

    /// A function this actor reads from a peer.
    pub const fn client(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Client,
            function,
            writeable: false,
            needs_binding: false,
        }
    }

    /// A function this actor writes on a peer, which requires a binding first.
    pub const fn client_writes(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Client,
            function,
            writeable: true,
            needs_binding: true,
        }
    }
}

/// One scenario of a use case.
#[derive(Clone, Debug)]
pub struct Scenario {
    /// The scenario number, as `useCaseScenarioSupport` reports it.
    pub number: u32,
    /// The scenario's name in the specification.
    pub name: &'static str,
    /// Whether this actor has to implement it.
    pub support: Support,
    /// The functions the scenario needs.
    pub functions: &'static [FunctionUse],
}

/// Everything one actor of one use case has to offer.
#[derive(Clone, Debug)]
pub struct UseCaseDescriptor {
    /// The `useCaseName` this actor announces.
    pub name: &'static str,
    /// The `actor` this actor announces.
    pub actor: &'static str,
    /// Whether this actor drives the use case or serves it.
    pub role: ActorRole,
    /// The `useCaseVersion`, which must be the exact official version string; the SPINE
    /// implementation guide §2.5 permits a peer to abort over a malformed one.
    pub version: &'static str,
    /// The `useCaseDocumentSubRevision`, which the same section makes mandatory.
    pub document_sub_revision: &'static str,
    /// The entity types this actor may live on.
    ///
    /// Empty means the specification places no restriction — some client actors, such as
    /// MPC's Monitoring Appliance, may sit behind any entity type.
    pub entity_types: &'static [EntityType],
    /// The actor on the other side of the use case.
    pub counterpart: &'static str,
    /// The scenarios, in order.
    pub scenarios: &'static [Scenario],
}

impl UseCaseDescriptor {
    /// Whether this actor may live on an entity of the given type.
    ///
    /// An empty [`entity_types`](Self::entity_types) permits any, which is what a
    /// specification that places no restriction means.
    pub fn permits_entity(&self, entity: &EntityType) -> bool {
        self.entity_types.is_empty() || self.entity_types.contains(entity)
    }

    /// The scenarios this actor has to implement: everything not marked optional.
    ///
    /// This is what the *specification* requires, not what a device has chosen to build.
    /// A device that implements optional scenarios as well announces them explicitly —
    /// see [`all_scenarios`](Self::all_scenarios) and
    /// `Engine::add_use_case_scenarios`.
    pub fn required_scenarios(&self) -> impl Iterator<Item = u32> + '_ {
        self.scenarios
            .iter()
            .filter(|s| s.support != Support::Optional)
            .map(|s| s.number)
    }

    /// Every scenario the use case defines for this actor.
    pub fn all_scenarios(&self) -> impl Iterator<Item = u32> + '_ {
        self.scenarios.iter().map(|s| s.number)
    }

    /// Whether the use case defines a scenario with this number for this actor.
    pub fn defines_scenario(&self, scenario: u32) -> bool {
        self.scenarios.iter().any(|s| s.number == scenario)
    }

    /// The features this actor has to expose, deduplicated across scenarios.
    ///
    /// A feature type may appear at most once per entity — the SPINE implementation
    /// guide §3.4 forbids two features of the same type on one entity, because a client
    /// looking for, say, `Measurement` would have no way to choose between them.
    pub fn server_features(&self) -> impl Iterator<Item = &FeatureType> + '_ {
        let mut seen: alloc::vec::Vec<&FeatureType> = alloc::vec::Vec::new();
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .filter(|f| f.role == Role::Server)
            .filter_map(move |f| {
                if seen.contains(&&f.feature) {
                    None
                } else {
                    seen.push(&f.feature);
                    Some(&f.feature)
                }
            })
    }

    /// The features on the *peer* this actor has to bind to before it can write.
    pub fn features_needing_binding(&self) -> impl Iterator<Item = &FeatureType> + '_ {
        let mut seen: alloc::vec::Vec<&FeatureType> = alloc::vec::Vec::new();
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .filter(|f| f.role == Role::Client && f.needs_binding)
            .filter_map(move |f| {
                if seen.contains(&&f.feature) {
                    None
                } else {
                    seen.push(&f.feature);
                    Some(&f.feature)
                }
            })
    }

    /// The `useCaseName` as the model's type.
    pub fn use_case_name(&self) -> UseCaseName {
        UseCaseName::from(self.name)
    }

    /// The `actor` as the model's type.
    pub fn use_case_actor(&self) -> UseCaseActor {
        UseCaseActor::from(self.actor)
    }
}

/// The four use cases EEBUS certification covers, by their `useCaseName` on the wire.
pub mod names {
    /// Limitation of Power Consumption, the §14a EnWG mechanism.
    pub const LPC: &str = "limitationOfPowerConsumption";
    /// Limitation of Power Production, the §9 EEG mechanism.
    pub const LPP: &str = "limitationOfPowerProduction";
    /// Monitoring of Power Consumption.
    pub const MPC: &str = "monitoringOfPowerConsumption";
    /// Monitoring of Grid Connection Point.
    pub const MGCP: &str = "monitoringOfGridConnectionPoint";
}

/// Actor names as they appear in `nodeManagementUseCaseData.useCaseInformation.actor`.
pub mod actors {
    /// Sets limits on behalf of the grid operator (LPC, LPP).
    pub const ENERGY_GUARD: &str = "EnergyGuard";
    /// Applies the limits it is sent (LPC, LPP).
    pub const CONTROLLABLE_SYSTEM: &str = "ControllableSystem";
    /// Collects measurements (MPC, MGCP).
    pub const MONITORING_APPLIANCE: &str = "MonitoringAppliance";
    /// Provides its own measurements (MPC).
    pub const MONITORED_UNIT: &str = "MonitoredUnit";
    /// Provides the measurements of the grid connection point (MGCP).
    pub const GRID_CONNECTION_POINT: &str = "GridConnectionPoint";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::lpc;

    #[test]
    fn a_descriptor_lists_its_mandatory_scenarios() {
        let scenarios: alloc::vec::Vec<_> = lpc::CONTROLLABLE_SYSTEM.required_scenarios().collect();
        assert_eq!(scenarios, [1, 2, 3, 4]);
    }

    #[test]
    fn features_are_reported_once_each() {
        let features: alloc::vec::Vec<_> = lpc::CONTROLLABLE_SYSTEM.server_features().collect();
        assert_eq!(
            features,
            [
                &FeatureType::LoadControl,
                &FeatureType::DeviceConfiguration,
                &FeatureType::DeviceDiagnosis,
                &FeatureType::ElectricalConnection,
            ]
        );
    }

    /// The Energy Guard must bind to LoadControl and DeviceConfiguration before it can
    /// write; the implementation guide §3.8 turns that into the rule for telling apart
    /// two energy-manager entities on one device.
    #[test]
    fn the_energy_guard_binds_to_the_features_it_writes() {
        let features: alloc::vec::Vec<_> = lpc::ENERGY_GUARD.features_needing_binding().collect();
        assert_eq!(
            features,
            [&FeatureType::LoadControl, &FeatureType::DeviceConfiguration]
        );
    }

    #[test]
    fn wire_names_match_the_specification() {
        assert_eq!(
            lpc::CONTROLLABLE_SYSTEM.use_case_name().as_str(),
            "limitationOfPowerConsumption"
        );
        assert_eq!(lpc::ENERGY_GUARD.use_case_actor().as_str(), "EnergyGuard");
        assert_eq!(lpc::CONTROLLABLE_SYSTEM.version, "1.0.0");
    }
}
