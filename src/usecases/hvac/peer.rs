//! Finding an HVAC peer, and the one call that starts the conversation with it.
//!
//! Nine of the twelve use cases in this family are served from an `HVAC` feature — the six
//! [`system_function`](super::system_function) ones outright, and the three
//! [`setpoint`](super::setpoint) ones for the relation that says which setpoint a mode
//! uses. Locating one is the same lookup every time, and it is the lookup the use-case
//! implementation guide §3.3 makes easy to get wrong: the feature is on the entity that
//! **announced the actor**, not on whichever entity of the appliance happens to carry an
//! `HVAC` server. A heat-pump gateway with four rooms has four of those.
//!
//! So it is written once here, and each use case re-exports it under its own name:
//! [`cdsf::locate`](super::cdsf::locate) and its eight siblings.
//!
//! # Locating is not enough
//!
//! A located peer is an address, not a conversation. A [`SystemFunction`] answers nothing
//! until six payloads have arrived — the system function descriptions, the operation mode
//! descriptions, the relations between them, the state, and, for the hot water, the two
//! overrun functions — and a client that read the descriptions and stopped has a reader
//! that refuses every write it is asked to build. [`HvacPeer::follow`] is all of it in one
//! call, in the order the scenario tables are written in.
//!
//! **It never binds.** Every use case in this family says "Binding SHOULD NOT be used for
//! this Scenario", including the six that write — the opposite of every grid use case,
//! where an unbound write is refused with `errorNumber` 9. See
//! [`WriteBinding`](crate::spine::WriteBinding).
//!
//! [`SystemFunction`]: super::system_function::SystemFunction

use alloc::vec::Vec;
use core::time::Duration;

use crate::model::{
    AddressDevice, FeatureAddress, FeatureType, Function, HvacOverrunType, HvacSystemFunctionType,
    MsgCounter, Role, ScopeType,
};
use crate::spine::{Engine, RemoteDevice};
use crate::usecases::UnitId;
use crate::usecases::descriptor::UseCaseDescriptor;

/// What one of the nine use cases is *about*, as opposed to how it is served.
///
/// The descriptor says which functions to read; this says what they will turn out to mean.
/// One `HVAC` feature carries every system function the appliance has and one `Setpoint`
/// feature every setpoint, so the payloads of MRHSF and MRCSF are the same shape, arrive
/// in the same lists, and differ only in the `systemFunctionType` a reader is told to
/// follow. Picking the wrong one is invisible: the answer is a fact about a different
/// function of the same appliance.
///
/// Each use-case module carries its own as a constant — [`cdsf::SUBJECT`](super::cdsf::SUBJECT)
/// and its eight siblings — and [`locate`] stamps it onto the peer, so
/// [`HvacApplianceActor`](super::HvacApplianceActor) can build the right readers from a
/// located peer alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    /// The system function: `dhw`, `heating` or `cooling`.
    ///
    /// Present for all nine, including the three that write a temperature: a setpoint is
    /// addressed *through* a system function's operation mode, so
    /// `hvacSystemFunctionSetpointRelationData` is keyed by `systemFunctionId` before
    /// `operationModeId`.
    pub function: HvacSystemFunctionType,
    /// The overrun the use case carries, where it has one.
    ///
    /// `Some(OneTimeDhw)` for [`mdsf`](super::mdsf) and [`cdsf`](super::cdsf), and [`None`]
    /// everywhere else: the one-time hot water loading is the only overrun in the family.
    pub overrun: Option<HvacOverrunType>,
    /// The `scopeType` of the setpoints, for the three use cases that write a temperature.
    ///
    /// [`None`] for the six system-function use cases, which have no setpoint in scope.
    /// [`crht`](super::crht) and [`crct`](super::crct) share `roomAirTemperature`, and it
    /// is [`function`](Self::function) that tells their setpoints apart.
    pub scope: Option<ScopeType>,
}

impl Subject {
    /// A system-function use case with no overrun and no setpoint.
    pub const fn mode(function: HvacSystemFunctionType) -> Self {
        Self {
            function,
            overrun: None,
            scope: None,
        }
    }

    /// The same, with the overrun the hot water has — and only the hot water.
    pub const fn mode_with_overrun(
        function: HvacSystemFunctionType,
        overrun: HvacOverrunType,
    ) -> Self {
        Self {
            function,
            overrun: Some(overrun),
            scope: None,
        }
    }

    /// A temperature use case: the system function its relations name, and the scope of
    /// the setpoints it writes.
    pub const fn temperature(function: HvacSystemFunctionType, scope: ScopeType) -> Self {
        Self {
            function,
            overrun: None,
            scope: Some(scope),
        }
    }

    /// The reader this subject calls for, ready to be fed.
    pub fn system_function(&self) -> super::system_function::SystemFunction {
        super::system_function::SystemFunction::new(self.function.clone(), self.overrun.clone())
    }
}

/// Where one HVAC use case of a peer lives.
///
/// One **entity**, not one device: §3.2.2.2.1 puts at most one `HVAC` feature on an entity,
/// and a gateway that announces `HVACRoom` per room is several of these side by side. Use
/// [`locate_all`] where that is possible, which for every room use case it is.
#[derive(Clone, Debug, PartialEq)]
pub struct HvacPeer {
    /// The peer's device address.
    pub device: AddressDevice,
    /// Its `HVAC` feature: the system functions, the operation modes, the relations
    /// between them, and — where the use case has one — the overrun.
    ///
    /// Required, and required for all nine: even [`cdt`](super::cdt), whose scenario is a
    /// temperature, reads `hvacSystemFunctionSetpointRelationListData` from here. A peer
    /// that announced one of these use cases and serves no `HVAC` server has no scenario
    /// it could be conformant in, which is why [`locate`] reports it as [`None`] rather
    /// than as a peer with nothing to read.
    pub hvac: FeatureAddress,
    /// Its `Setpoint` feature, for the three use cases that write a temperature.
    ///
    /// [`None`] for the six [`system_function`](super::system_function) use cases, which
    /// have no setpoint in scope at all.
    pub setpoint: Option<FeatureAddress>,
    /// Which system function, overrun and setpoint scope this use case is about.
    pub subject: Subject,
    /// What to read, as the use case's own scenario tables list it.
    reads: Vec<(FeatureAddress, Function)>,
}

/// The exchange [`HvacPeer::follow`] started.
///
/// Every counter here is one a [`ResultReceived`](crate::spine::SpineEvent::ResultReceived)
/// may come back under. A *successful* read arrives as
/// [`ReplyReceived`](crate::spine::SpineEvent::ReplyReceived), which carries the feature
/// rather than the counter — so these are the counters that say something went wrong, not
/// the ones that say it went right.
#[derive(Clone, Debug, PartialEq)]
pub struct Following {
    /// The subscription to the `HVAC` feature.
    ///
    /// Refused, a client has to poll instead: an appliance whose mode is changed at the
    /// wall panel changes it without telling anybody otherwise, and the mode a manager
    /// thinks it set is the one it wrote rather than the one in force.
    pub subscription: MsgCounter,
    /// The subscription to the `Setpoint` feature, for the three use cases that have one.
    pub setpoint_subscription: Option<MsgCounter>,
    /// Each read, in the order the use case's scenario tables list it.
    pub reads: Vec<(Function, MsgCounter)>,
}

impl Following {
    /// Which read a counter belongs to, for turning a failure into a sentence.
    ///
    /// `errorNumber` 7 against `hvacOverrunDescriptionListData` is a circuit that does not
    /// serve the one-time loading scenario; the same number against
    /// `hvacSystemFunctionListData` is a circuit that cannot report its own mode. Without
    /// the function the two are the same event.
    pub fn function_of(&self, counter: MsgCounter) -> Option<&Function> {
        self.reads
            .iter()
            .find(|(_, known)| *known == counter)
            .map(|(function, _)| function)
    }

    /// Whether a counter is one of the subscription requests.
    pub fn is_subscription(&self, counter: MsgCounter) -> bool {
        self.subscription == counter || self.setpoint_subscription == Some(counter)
    }
}

impl HvacPeer {
    /// Starts following an HVAC peer: subscribes, then reads everything its use case
    /// publishes.
    ///
    /// The pre-scenario communication, in one call:
    ///
    /// 1. **Subscribe** to the `HVAC` feature, and — for the three temperature use cases —
    ///    to the `Setpoint` one. Before the reads: a mode that changes between the reply
    ///    and a later subscription request is a change nothing ever hears about.
    /// 2. **Read** every function the use case's scenario tables name, in their order.
    ///    Descriptions first, because they say which `systemFunctionId` and which
    ///    `operationModeId`s the state is talking about.
    ///
    /// **No binding**, which is the specification's own instruction: all nine say "Binding
    /// SHOULD NOT be used for this Scenario", and a conformant server would refuse one.
    ///
    /// Calling it again restarts both, which is what a reconnection needs. `client` is the
    /// appliance's own `Generic` client feature, which
    /// [`limitation::client_feature`](crate::usecases::limitation::client_feature) builds;
    /// feed every reply and notification to the use case's `reader`, which takes them in
    /// any order.
    ///
    /// ```no_run
    /// # use core::time::Duration;
    /// use eebus::usecases::hvac::cdsf;
    /// # fn example(
    /// #     engine: &mut eebus::spine::Engine,
    /// #     remote: &eebus::spine::RemoteDevice,
    /// #     client: &eebus::model::FeatureAddress,
    /// #     now: Duration,
    /// # ) -> Option<()> {
    /// let circuit = cdsf::locate(remote)?;
    /// let pending = circuit.follow(engine, client, now);
    /// let mut reader = cdsf::reader();
    /// // `reader.learn(&resolved)` for every reply and notification from `circuit.hvac`,
    /// // and `reader.set_mode_named(&HvacOperationModeType::Eco)` once it is complete.
    /// # let _ = (pending, reader);
    /// # Some(())
    /// # }
    /// ```
    pub fn follow(&self, engine: &mut Engine, client: &FeatureAddress, now: Duration) -> Following {
        let subscription = engine.request_subscription(client, &self.hvac, now);
        let setpoint_subscription = self
            .setpoint
            .as_ref()
            .map(|feature| engine.request_subscription(client, feature, now));
        let reads = self
            .reads
            .iter()
            .map(|(feature, function)| {
                (
                    function.clone(),
                    engine.read(feature, client, function.clone(), now),
                )
            })
            .collect();
        Following {
            subscription,
            setpoint_subscription,
            reads,
        }
    }

    /// What [`follow`](Self::follow) will read, and from which feature.
    ///
    /// The use case's scenario tables, resolved against what this peer actually declared.
    /// Worth looking at before commissioning a device: a circuit that announced CDSF and
    /// declares no `hvacOverrunListData` cannot be asked for a one-time loading, and that
    /// is knowable from discovery rather than from a refusal.
    pub fn reads(&self) -> impl Iterator<Item = (&FeatureAddress, &Function)> {
        self.reads
            .iter()
            .map(|(feature, function)| (feature, function))
    }

    /// Whether this peer declared a function of its use case at all.
    pub fn serves_function(&self, function: &Function) -> bool {
        self.reads.iter().any(|(_, known)| known == function)
    }

    /// Whether `feature` is one of the two this peer serves the use case from.
    ///
    /// What routes an incoming event to the right reader when several units of one device
    /// are being followed. The device is checked first and separately:
    /// [`same_feature`](crate::spine::same_feature) compares device addresses only where
    /// both carry one, and two devices may well number an entity and a feature the same
    /// way.
    pub fn serves(&self, feature: &FeatureAddress) -> bool {
        if feature.device.as_ref().is_none_or(|d| d != &self.device) {
            return false;
        }
        core::iter::once(&self.hvac)
            .chain(self.setpoint.as_ref())
            .any(|known| crate::spine::same_feature(known, feature))
    }

    /// The entity path the use case is served from: `[1]`, `[1, 2]`.
    ///
    /// What tells one room of a gateway from the next.
    pub fn entity(&self) -> Vec<u32> {
        crate::spine::entity_path(&self.hvac)
    }

    /// Which unit this is: its device, and the entity its features live on.
    ///
    /// The same [`UnitId`] a
    /// [`MonitoredUnitPeer`](crate::usecases::monitoring::MonitoredUnitPeer) on the same
    /// entity reports, so a room's thermometer and its setpoints answer to one key.
    pub fn id(&self) -> UnitId {
        UnitId {
            device: self.device.clone(),
            entity: self.entity(),
        }
    }
}

/// Finds the first entity of a peer that serves `descriptor`'s use case.
///
/// `descriptor` is the **server** actor's — [`cdsf::DHW_CIRCUIT`](super::cdsf::DHW_CIRCUIT),
/// not `CONFIGURATION_APPLIANCE` — because what it is read for is the list of functions
/// the server publishes; `subject` is what those functions will turn out to mean. Each use
/// case module wraps this so a caller cannot pair the wrong two; this is public for a use
/// case built outside the crate on the same shape.
///
/// Returns [`None`] until the peer has announced both the use case and every feature its
/// scenarios are served from.
pub fn locate(
    remote: &RemoteDevice,
    descriptor: &UseCaseDescriptor,
    subject: &Subject,
) -> Option<HvacPeer> {
    locate_all(remote, descriptor, subject).into_iter().next()
}

/// Finds **every** entity of a peer that serves it.
///
/// One device is regularly several: a heat-pump gateway announces one `HVACRoom` per room,
/// each with its own `HVAC` feature, its own operation mode and its own setpoints. Each is
/// a separate [`HvacPeer`], told from the next by [`HvacPeer::entity`].
///
/// Empty until the peer has announced both the use case and the features that carry it.
pub fn locate_all(
    remote: &RemoteDevice,
    descriptor: &UseCaseDescriptor,
    subject: &Subject,
) -> Vec<HvacPeer> {
    let Some(device) = remote.address.clone() else {
        return Vec::new();
    };
    let mut found: Vec<HvacPeer> = Vec::new();
    for played in remote.use_cases_played(descriptor.name, descriptor.actor) {
        let mut hvac = None;
        let mut setpoint = None;
        let mut reads: Vec<(FeatureAddress, Function)> = Vec::new();
        let mut complete = true;
        for (feature_type, function) in descriptor.server_functions() {
            let Some(feature) = remote.feature_for(played, feature_type, Role::Server) else {
                // A feature the scenarios are served from and the peer does not have.
                // Nothing this use case does is reachable without it.
                complete = false;
                break;
            };
            match feature_type {
                FeatureType::HVAC => hvac = Some(feature.address.clone()),
                FeatureType::Setpoint => setpoint = Some(feature.address.clone()),
                // No other feature type appears in this family, and one that did would
                // still be read from — the address is what matters, not the name.
                _ => {}
            }
            // Only what the peer declared. A read of a function a feature never announced
            // is answered with `errorNumber` 7, and discovery already said so.
            //
            // A feature that declared *no* function is taken the other way, because
            // "declared nothing" is not "declared nothing is supported": 23 of the 108
            // features in `tests/fixtures/devices` carry no `supportedFunction` at all —
            // evcc.io's HEMS announces its entire server-side feature set that way —  and
            // filtering on an empty list would read nothing from any of them.
            if feature.functions.is_empty() || feature.supports(function) {
                reads.push((feature.address.clone(), function.clone()));
            }
        }
        let (true, Some(hvac)) = (complete, hvac) else {
            continue;
        };
        // A device that announces the same use case twice against one entity — which §7.5
        // does not forbid — is one circuit, not two.
        if found.iter().any(|peer| peer.hvac == hvac) {
            continue;
        }
        found.push(HvacPeer {
            device: device.clone(),
            hvac,
            setpoint,
            subject: subject.clone(),
            reads,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    use crate::model::{DeviceType, EntityType};
    use crate::spine::{LocalDevice, LocalEntity, detailed_discovery, use_case_data};
    use crate::usecases::hvac::{cdsf, cdt, crhsf, mdsf, mrhsf};

    /// A heat pump that heats the hot water and two rooms — one entity each, one `HVAC`
    /// feature each, exactly as §3.2.2.2.1 requires.
    fn gateway() -> LocalDevice {
        let mut device =
            LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::DHWCircuit)
                    .with_feature(cdt::setpoint_feature(1))
                    .with_feature(cdsf::with_cdt(2)),
            )
            .unwrap();
        for room in [2u32, 3] {
            device
                .add_entity(
                    LocalEntity::new([room], EntityType::HVACRoom)
                        .with_feature(crhsf::hvac_feature(1)),
                )
                .unwrap();
        }
        device
    }

    /// The peer as a client sees it after both discovery reads.
    fn discovered(device: &LocalDevice) -> RemoteDevice {
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(device));
        remote.apply_use_case_data(&use_case_data(
            device,
            &[
                (vec![1], 2, &cdsf::DHW_CIRCUIT, vec![1, 2, 3]),
                (vec![1], 2, &mdsf::DHW_CIRCUIT, vec![1, 2]),
                (vec![1], 1, &cdt::DHW_CIRCUIT, vec![1]),
                (vec![2], 1, &crhsf::HVAC_ROOM, vec![1]),
                (vec![3], 1, &crhsf::HVAC_ROOM, vec![1]),
            ],
        ));
        remote
    }

    /// §3.3: the feature is the one on the entity that announced the actor.
    ///
    /// This gateway has three `HVAC` features. A lookup by type alone would find the hot
    /// water's and report a living room's operation mode from it.
    #[test]
    fn the_feature_is_the_one_on_the_entity_that_announced_the_use_case() {
        let device = gateway();
        let remote = discovered(&device);

        let circuit = cdsf::locate(&remote).expect("the DHW circuit");
        assert_eq!(circuit.hvac, device.address_of(&[1], 2));
        assert_eq!(circuit.entity(), vec![1]);
        assert_eq!(
            circuit.setpoint, None,
            "a system-function use case has no setpoint in scope"
        );

        let room = crhsf::locate(&remote).expect("a room");
        assert_eq!(room.hvac, device.address_of(&[2], 1));
        assert_ne!(room.hvac, circuit.hvac);
    }

    /// One device, one `HVACRoom` per room, and each is its own peer.
    #[test]
    fn a_gateway_with_two_rooms_is_two_peers() {
        let device = gateway();
        let remote = discovered(&device);

        let rooms = crhsf::locate_all(&remote);
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].entity(), vec![2]);
        assert_eq!(rooms[1].entity(), vec![3]);
        assert_eq!(
            crhsf::locate(&remote).as_ref(),
            Some(&rooms[0]),
            "`locate` is the first of them"
        );
    }

    /// A temperature use case is served from two features, and both are found.
    #[test]
    fn a_temperature_use_case_locates_the_setpoint_beside_the_hvac_feature() {
        let device = gateway();
        let remote = discovered(&device);

        let circuit = cdt::locate(&remote).expect("the DHW circuit");
        assert_eq!(circuit.setpoint, Some(device.address_of(&[1], 1)));
        assert_eq!(
            circuit.hvac,
            device.address_of(&[1], 2),
            "the relation that says which mode uses which setpoint lives here"
        );
    }

    /// A use case nothing announced is not located, and neither is one whose features are
    /// missing.
    #[test]
    fn a_peer_that_serves_none_of_it_is_not_located() {
        let device = gateway();
        let remote = discovered(&device);
        assert_eq!(
            mrhsf::locate(&remote),
            None,
            "this gateway never announced the monitoring use case"
        );

        // A circuit that announced CDT from an entity carrying no `Setpoint` feature.
        let mut bare =
            LocalDevice::new("i:46925", "HeatPump-2", DeviceType::HeatGenerationSystem).unwrap();
        bare.add_entity(
            LocalEntity::new([1], EntityType::DHWCircuit).with_feature(mdsf::hvac_feature(1)),
        )
        .unwrap();
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(&bare));
        remote.apply_use_case_data(&use_case_data(
            &bare,
            &[(vec![1], 1, &cdt::DHW_CIRCUIT, vec![1])],
        ));
        assert_eq!(
            cdt::locate(&remote),
            None,
            "a setpoint use case with no `Setpoint` feature has no scenario it can serve"
        );
        assert!(
            mdsf::locate(&remote).is_none(),
            "and the use case it did not announce is not located either"
        );
    }

    /// The six reads CDSF needs, in the order its scenario tables put them, plus the
    /// subscription — one call.
    #[test]
    fn follow_subscribes_and_reads_everything_the_scenarios_name() {
        let device = gateway();
        let remote = discovered(&device);
        let circuit = cdsf::locate(&remote).expect("the DHW circuit");

        let mut manager =
            LocalDevice::new("i:12345", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        manager
            .add_entity(
                LocalEntity::new([1], EntityType::CEM)
                    .with_feature(crate::usecases::limitation::client_feature(1)),
            )
            .unwrap();
        let client = manager.address_of(&[1], 1);
        let mut engine = crate::spine::Engine::new(manager);

        let pending = circuit.follow(&mut engine, &client, Duration::ZERO);
        let read: Vec<&Function> = pending.reads.iter().map(|(f, _)| f).collect();
        assert_eq!(
            read,
            [
                &Function::HvacSystemFunctionDescriptionListData,
                &Function::HvacOperationModeDescriptionListData,
                &Function::HvacSystemFunctionOperationModeRelationListData,
                &Function::HvacSystemFunctionListData,
                &Function::HvacOverrunDescriptionListData,
                &Function::HvacOverrunListData,
            ],
            "the descriptions before the state, because they say what the state means"
        );
        assert_eq!(
            pending.setpoint_subscription, None,
            "there is no `Setpoint` feature to subscribe to"
        );
        assert!(pending.is_subscription(pending.subscription));
        assert_eq!(
            pending.function_of(pending.reads[4].1),
            Some(&Function::HvacOverrunDescriptionListData),
            "which is what turns `errorNumber` 7 into a sentence"
        );
        assert_eq!(pending.function_of(pending.subscription), None);
    }

    /// A room's four reads, and both subscriptions where a `Setpoint` feature is in scope.
    #[test]
    fn a_temperature_use_case_subscribes_to_both_features() {
        let device = gateway();
        let remote = discovered(&device);
        let circuit = cdt::locate(&remote).expect("the DHW circuit");

        let mut manager =
            LocalDevice::new("i:12345", "CEM-1", DeviceType::EnergyManagementSystem).unwrap();
        manager
            .add_entity(
                LocalEntity::new([1], EntityType::CEM)
                    .with_feature(crate::usecases::limitation::client_feature(1)),
            )
            .unwrap();
        let client = manager.address_of(&[1], 1);
        let mut engine = crate::spine::Engine::new(manager);

        let pending = circuit.follow(&mut engine, &client, Duration::ZERO);
        assert!(
            pending.setpoint_subscription.is_some(),
            "a setpoint changed at the wall panel is a change this appliance has to hear"
        );
        let read: Vec<&Function> = pending.reads.iter().map(|(f, _)| f).collect();
        assert_eq!(
            read,
            [
                &Function::SetpointDescriptionListData,
                &Function::SetpointConstraintsListData,
                &Function::SetpointListData,
                &Function::HvacSystemFunctionSetpointRelationListData,
            ]
        );
    }

    /// Only what the peer declared: a circuit whose `HVAC` feature carries no overrun
    /// functions is not asked for them.
    ///
    /// CDSF marks scenario 3 recommended and its Configuration Appliance's scenarios
    /// optional, so a circuit serving the mode and not the one-time loading is conformant.
    /// Reading a function it never announced earns `errorNumber` 7, and discovery already
    /// said so.
    #[test]
    fn a_function_the_peer_never_declared_is_not_read() {
        let mut bare =
            LocalDevice::new("i:46925", "HeatPump-3", DeviceType::HeatGenerationSystem).unwrap();
        bare.add_entity(
            LocalEntity::new([1], EntityType::DHWCircuit)
                // The mode, without the overrun: `hvac_feature(address, false)`.
                .with_feature(crate::usecases::hvac::system_function::writeable(1, false)),
        )
        .unwrap();
        let mut remote = RemoteDevice::default();
        remote.apply_detailed_discovery(&detailed_discovery(&bare));
        remote.apply_use_case_data(&use_case_data(
            &bare,
            &[(vec![1], 1, &cdsf::DHW_CIRCUIT, vec![1])],
        ));

        let circuit = cdsf::locate(&remote).expect("a circuit serving scenario 1");
        let read: Vec<&Function> = circuit.reads().map(|(_, function)| function).collect();
        assert_eq!(read.len(), 4, "four functions, not six: {read:?}");
        assert!(!circuit.serves_function(&Function::HvacOverrunListData));
        assert!(circuit.serves_function(&Function::HvacSystemFunctionListData));
    }

    /// Routing an event to the right unit is by feature, not by device.
    #[test]
    fn a_peer_recognises_its_own_features() {
        let device = gateway();
        let remote = discovered(&device);
        let circuit = cdt::locate(&remote).expect("the DHW circuit");
        let room = crhsf::locate(&remote).expect("a room");

        assert!(circuit.serves(&device.address_of(&[1], 2)));
        assert!(circuit.serves(&device.address_of(&[1], 1)));
        assert!(!circuit.serves(&device.address_of(&[2], 1)));
        assert!(room.serves(&device.address_of(&[2], 1)));
        assert!(!room.serves(&device.address_of(&[1], 2)));
    }
}
