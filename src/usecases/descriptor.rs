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

use core::time::Duration;

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

/// When a function's data arrives, once the actor that reads it has subscribed.
///
/// **Every scenario of every use case here is subscription-driven**, and that is the
/// specification's own answer rather than this crate's shortcut: each UC TS §3.4.n.1
/// says "Actors SHALL create a subscription for each server Feature that is relevant for
/// the corresponding Actor within this Scenario", and §3.3.4 makes polling the
/// *fallback* for a subscription that was refused. So the question a consumer needs
/// answered is not "notification or poll" — it is **what silence means**, and that is
/// what this says.
///
/// It is the difference between a monitored value and a heartbeat, and getting it wrong
/// costs a real appliance: age a room's temperature by its arrival time and the room
/// disappears from the site the moment it stops changing, which is the moment the
/// protocol is working best.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Sent when the value changes, and at no other time.
    ///
    /// **The age of the last value says nothing about the peer.** A room holding its
    /// temperature, a heat pump staying in `auto`, a tank at its setpoint: each sends
    /// nothing for hours and each is working. Liveness comes from a
    /// [`Periodic`](Self::Periodic) function — the heartbeat — or from the SHIP
    /// connection, never from the timestamp of a measurement.
    OnChange,
    /// Sent at least this often whether the value changed or not.
    ///
    /// Silence past this **is** a fault, which is the whole purpose of the element: the
    /// LPC heartbeat is sent every 60 s ([LPC-005], [LPC-006]) and its absence is what
    /// arms the failsafe, and the EV heartbeat of [OPEV-005] and [OSCEV-005] runs at 4 s
    /// because a car draws its next ampere in less time than that.
    ///
    /// The period is the specification's "at least every", not a tolerance. How many
    /// missed beats to allow before acting is the consumer's: LPC allows two —
    /// [`limitation::HEARTBEAT_TIMEOUT`](crate::usecases::limitation::HEARTBEAT_TIMEOUT)
    /// is 120 s against a 60 s cadence — and OPEV allows none.
    Periodic(Duration),
}

impl Delivery {
    /// The cadence, for a function that has one.
    pub const fn period(&self) -> Option<Duration> {
        match self {
            Self::OnChange => None,
            Self::Periodic(period) => Some(*period),
        }
    }
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
    /// When this function's data arrives once the reader has subscribed.
    ///
    /// [`Delivery::OnChange`] unless the specification fixes a cadence, which in this
    /// crate only the heartbeats do — see [`periodic`](Self::periodic).
    pub delivery: Delivery,
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
            delivery: Delivery::OnChange,
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
            delivery: Delivery::OnChange,
        }
    }

    /// A function this actor serves and **any** peer may write.
    ///
    /// For a use case that says so, which in this crate is every one in
    /// [`hvac`](crate::usecases::hvac): "Binding SHOULD NOT be used for this Scenario". See
    /// [`WriteBinding`](crate::spine::WriteBinding) for the reasoning and for the local
    /// feature that has to agree with this.
    pub const fn server_writeable_unbound(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Server,
            function,
            writeable: true,
            needs_binding: false,
            delivery: Delivery::OnChange,
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
            delivery: Delivery::OnChange,
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
            delivery: Delivery::OnChange,
        }
    }

    /// A function this actor writes on a peer **without** binding to it first.
    ///
    /// The HVAC counterpart of [`client_writes`](Self::client_writes). A Configuration
    /// Appliance that bound anyway would not be refused — a server may grant a binding it
    /// does not require — but it would be doing something its use case tells it not to,
    /// and on a server that limits its bindings it would be taking a slot from an actor
    /// that needs one.
    pub const fn client_writes_unbound(feature: FeatureType, function: Function) -> Self {
        Self {
            feature,
            role: Role::Client,
            function,
            writeable: true,
            needs_binding: false,
            delivery: Delivery::OnChange,
        }
    }

    /// The same function, sent on a fixed cadence rather than only when it changes.
    ///
    /// For the heartbeats, and so far for nothing else: they are the only functions any
    /// of these specifications puts a clock on. `every` is the "SHALL be sent at least
    /// every" of the use case — 60 s for LPC, LPP and COB, 4 s for OPEV and OSCEV — and
    /// not the tolerance a reader should apply before it acts on the silence.
    #[must_use]
    pub const fn periodic(mut self, every: Duration) -> Self {
        self.delivery = Delivery::Periodic(every);
        self
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

impl Scenario {
    /// When this scenario's data arrives.
    ///
    /// [`Delivery::Periodic`] where any function of the scenario has a cadence, at the
    /// shortest of them: that is the one a reader can time out against, and it is the
    /// scenario's promise that *something* keeps arriving. [`Delivery::OnChange`]
    /// otherwise, which is every scenario but the heartbeats — see [`Delivery`] for why
    /// that is not the same as "polled".
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::usecases::descriptor::Delivery;
    /// use eebus::usecases::{lpc, mpc};
    ///
    /// let heartbeat = lpc::CONTROLLABLE_SYSTEM.scenario(3).expect("scenario 3");
    /// assert_eq!(heartbeat.delivery(), Delivery::Periodic(Duration::from_secs(60)));
    ///
    /// // A measurement arrives when it changes, so its age is not a liveness signal.
    /// let power = mpc::MONITORING_APPLIANCE.scenario(1).expect("scenario 1");
    /// assert_eq!(power.delivery(), Delivery::OnChange);
    /// ```
    pub fn delivery(&self) -> Delivery {
        match self
            .functions
            .iter()
            .filter_map(|f| f.delivery.period())
            .min()
        {
            Some(period) => Delivery::Periodic(period),
            None => Delivery::OnChange,
        }
    }
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

    /// One scenario by its number, as `useCaseScenarioSupport` reports it.
    pub fn scenario(&self, number: u32) -> Option<&Scenario> {
        self.scenarios.iter().find(|s| s.number == number)
    }

    /// The features on the *peer* this actor has to subscribe to.
    ///
    /// Every UC TS §3.4.n.1 says the same thing — "Actors SHALL create a subscription for
    /// each server Feature that is relevant for the corresponding Actor within this
    /// Scenario" — so for a client actor this is every feature it reads, whether or not it
    /// also writes to it, and one subscription covers all the scenarios that share a
    /// feature (§3.3.1). Polling is what §3.3.4 falls back to when a subscription request
    /// is *refused*, not an alternative an implementation may choose.
    ///
    /// The counterpart of [`features_needing_binding`](Self::features_needing_binding),
    /// and the list an actor's pre-scenario communication works through:
    /// [`MonitoringApplianceActor::attach`](crate::usecases::monitoring::MonitoringApplianceActor::attach)
    /// and [`HvacPeer::follow`](crate::usecases::hvac::peer::HvacPeer::follow) subscribe to
    /// exactly these, resolved against the addresses the peer announced.
    ///
    /// ```
    /// use eebus::model::FeatureType;
    /// use eebus::usecases::mpc;
    ///
    /// let features: Vec<_> = mpc::MONITORING_APPLIANCE
    ///     .features_needing_subscription()
    ///     .collect();
    /// assert_eq!(
    ///     features,
    ///     [&FeatureType::ElectricalConnection, &FeatureType::Measurement],
    /// );
    /// ```
    pub fn features_needing_subscription(&self) -> impl Iterator<Item = &FeatureType> + '_ {
        let mut seen: alloc::vec::Vec<&FeatureType> = alloc::vec::Vec::new();
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .filter(|f| f.role == Role::Client)
            .filter_map(move |f| {
                if seen.contains(&&f.feature) {
                    None
                } else {
                    seen.push(&f.feature);
                    Some(&f.feature)
                }
            })
    }

    /// When one function's data arrives, or [`None`] if this actor has no such function.
    ///
    /// What a consumer asks before it ages a value: [`Delivery::OnChange`] means silence
    /// is the protocol working and the age of the last value is not a health signal, and
    /// [`Delivery::Periodic`] means silence past the period is a fault.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::model::{FeatureType, Function};
    /// use eebus::usecases::descriptor::Delivery;
    /// use eebus::usecases::{lpc, mpc};
    ///
    /// assert_eq!(
    ///     mpc::MONITORING_APPLIANCE
    ///         .delivery_of(&FeatureType::Measurement, &Function::MeasurementListData),
    ///     Some(Delivery::OnChange),
    /// );
    /// assert_eq!(
    ///     lpc::CONTROLLABLE_SYSTEM.delivery_of(
    ///         &FeatureType::DeviceDiagnosis,
    ///         &Function::DeviceDiagnosisHeartbeatData,
    ///     ),
    ///     Some(Delivery::Periodic(Duration::from_secs(60))),
    /// );
    /// // Not this actor's function at all, which is a different answer from `OnChange`.
    /// assert_eq!(
    ///     mpc::MONITORING_APPLIANCE.delivery_of(
    ///         &FeatureType::DeviceDiagnosis,
    ///         &Function::DeviceDiagnosisHeartbeatData,
    ///     ),
    ///     None,
    /// );
    /// ```
    pub fn delivery_of(&self, feature: &FeatureType, function: &Function) -> Option<Delivery> {
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .find(|f| f.feature == *feature && f.function == *function)
            .map(|f| f.delivery)
    }

    /// Every function this actor sees on a fixed cadence, with that cadence.
    ///
    /// The liveness signals of the use case, and the only functions whose *absence* means
    /// anything. Empty for a use case that defines no heartbeat, which is most of them —
    /// there, a peer that has gone quiet is indistinguishable from a peer with nothing to
    /// say, and the answer is the SHIP connection rather than a timer over the data.
    pub fn periodic_functions(
        &self,
    ) -> impl Iterator<Item = (&FeatureType, &Function, Duration)> + '_ {
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .filter_map(|f| Some((&f.feature, &f.function, f.delivery.period()?)))
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

    /// Every `(feature, function)` this actor serves, deduplicated, in scenario order.
    ///
    /// The specification's own table read back as a list of reads, so a client knows what
    /// to ask a located peer for — descriptions before state, as the scenario tables are
    /// written — without a second list to drift from the first.
    /// [`hvac::peer::HvacPeer::follow`](crate::usecases::hvac::peer::HvacPeer::follow)
    /// reads it.
    ///
    /// Take it from the **server** actor's descriptor: the client-side one lists the same
    /// functions under [`Role::Client`] and yields nothing here.
    ///
    /// ```
    /// use eebus::model::{FeatureType, Function};
    /// use eebus::usecases::hvac::cdt;
    ///
    /// let reads: Vec<_> = cdt::DHW_CIRCUIT.server_functions().collect();
    /// assert_eq!(
    ///     reads.first(),
    ///     Some(&(&FeatureType::Setpoint, &Function::SetpointDescriptionListData)),
    /// );
    /// // The relation lives on the `HVAC` feature, not on the `Setpoint` one.
    /// assert!(reads.contains(&(
    ///     &FeatureType::HVAC,
    ///     &Function::HvacSystemFunctionSetpointRelationListData,
    /// )));
    /// ```
    pub fn server_functions(&self) -> impl Iterator<Item = (&FeatureType, &Function)> + '_ {
        let mut seen: alloc::vec::Vec<(&FeatureType, &Function)> = alloc::vec::Vec::new();
        self.scenarios
            .iter()
            .flat_map(|s| s.functions.iter())
            .filter(|f| f.role == Role::Server)
            .filter_map(move |f| {
                let pair = (&f.feature, &f.function);
                if seen.contains(&pair) {
                    None
                } else {
                    seen.push(pair);
                    Some(pair)
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
