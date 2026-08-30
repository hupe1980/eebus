//! LPC and LPP over the wire: the actor between the state machine and the engine.
//!
//! [`ControllableSystem`] decides *what* a system does with a limit; this module decides
//! *how* that reaches the wire — which features to build, how to read a
//! `loadControlLimitListData` write as a [`LimitWrite`], and which acknowledgement to
//! send back.
//!
//! Everything here serves both use cases. A [`Direction`] is what turns it into one or
//! the other, and it is a constructor argument rather than a module boundary so that the
//! two cannot drift apart.

use alloc::vec;
use core::time::Duration;

use crate::model::{
    CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyValueData,
    DeviceConfigurationKeyValueDescriptionData, DeviceConfigurationKeyValueDescriptionListData,
    DeviceConfigurationKeyValueListData, DeviceConfigurationKeyValueValue,
    DeviceDiagnosisHeartbeatData, FeatureAddress, FeatureType, Function, LoadControlCategory,
    LoadControlLimitData, LoadControlLimitDescriptionData, LoadControlLimitDescriptionListData,
    LoadControlLimitId, LoadControlLimitListData, LoadControlLimitType, MeasurementId, Role,
    ScaledNumber, ScopeType, UnitOfMeasurement,
};
use crate::spine::{
    Engine, ErrorNumber, HeartbeatProducer, LocalFeature, Operations, SpineEvent, WriteToken,
};

use super::audit::AuditLog;
use super::state::{ControllableSystem, LimitWrite, LimitationState, LocalDecision, WriteOutcome};
use super::{Direction, FAILSAFE_DURATION_MINIMUM_KEY};

/// The `limitId` this implementation uses for the active power limit.
///
/// The specification's placeholder `<l1#1>` leaves the number to the implementation; it
/// only has to be stable, because the Energy Guard addresses the limit by it.
pub const LIMIT_ID: LoadControlLimitId = LoadControlLimitId(1);

/// The `measurementId` the limit description points at.
///
/// The implementation guides §3.4 make this element mandatory whether or not a
/// matching measurand exists: a description without it is invalid, and where the device
/// has no `Measurement` feature the guide asks for a high number no measurand uses.
pub const MEASUREMENT_ID: MeasurementId = MeasurementId(1);

/// The `keyId` of the failsafe active power limit.
pub const FAILSAFE_LIMIT_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(1);

/// The `keyId` of the Failsafe Duration Minimum.
pub const FAILSAFE_DURATION_KEY: DeviceConfigurationKeyId = DeviceConfigurationKeyId(2);

/// Builds the `LoadControl` feature a Controllable System offers (LPC/LPP Table 21).
///
/// Writes are deferred: the Controllable System decides whether it can follow a limit,
/// and that decision is the acknowledgement.
pub fn load_control_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::LoadControl, Role::Server)
        .with_deferred_writes()
        .with_function(
            Function::LoadControlLimitDescriptionListData,
            Operations::read(),
        )
        .with_function(Function::LoadControlLimitListData, Operations::read_write())
}

/// Builds the `DeviceConfiguration` feature a Controllable System offers (LPC/LPP Table 24).
pub fn device_configuration_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceConfiguration, Role::Server)
        .with_deferred_writes()
        .with_function(
            Function::DeviceConfigurationKeyValueDescriptionListData,
            Operations::read(),
        )
        .with_function(
            Function::DeviceConfigurationKeyValueListData,
            Operations::read_write(),
        )
}

/// Builds the `DeviceDiagnosis` feature that carries the heartbeat (LPC/LPP Table 26).
///
/// Both actors serve one. The Energy Guard is a client actor, but its heartbeat runs
/// the other way — the Controllable System subscribes to it — and the use-case
/// implementation guide §2.1.3 says a secondary function running against the grain does
/// not change an actor's classification.
pub fn device_diagnosis_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Server)
        .with_function(Function::DeviceDiagnosisHeartbeatData, Operations::read())
}

/// The limit description a Controllable System publishes (LPC/LPP Table 22).
///
/// Every element here is fixed by the specification: the limit is an obligation,
/// expressed in watts, with a sign-dependent absolute value. `direction` is what makes it
/// a consumption limit or a production one.
pub fn limit_description(direction: Direction) -> CmdData {
    CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
        load_control_limit_description_data: Some(vec![LoadControlLimitDescriptionData {
            limit_id: Some(LIMIT_ID),
            limit_type: Some(LoadControlLimitType::SignDependentAbsValueLimit),
            limit_category: Some(LoadControlCategory::Obligation),
            limit_direction: Some(direction.energy_direction()),
            measurement_id: Some(MEASUREMENT_ID),
            unit: Some(UnitOfMeasurement::W),
            scope_type: Some(ScopeType::ActivePowerLimit),
            ..Default::default()
        }]),
    })
}

/// The current limit, as `loadControlLimitListData` (LPC/LPP Table 23).
pub fn limit_data(system: &ControllableSystem) -> CmdData {
    CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LIMIT_ID),
            is_limit_changeable: Some(true),
            is_limit_active: Some(system.is_limit_active()),
            value: Some(ScaledNumber::from_f64(
                system.effective_limit().watts().unwrap_or(0.0),
                0,
            )),
            ..Default::default()
        }]),
    })
}

/// The failsafe key descriptions (LPC/LPP Table 24).
pub fn failsafe_descriptions(direction: Direction) -> CmdData {
    CmdData::DeviceConfigurationKeyValueDescriptionListData(
        DeviceConfigurationKeyValueDescriptionListData {
            device_configuration_key_value_description_data: Some(vec![
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(FAILSAFE_LIMIT_KEY),
                    key_name: Some(direction.failsafe_limit_key()),
                    value_type: Some(crate::model::DeviceConfigurationKeyValueType::ScaledNumber),
                    unit: Some(UnitOfMeasurement::W),
                    ..Default::default()
                },
                DeviceConfigurationKeyValueDescriptionData {
                    key_id: Some(FAILSAFE_DURATION_KEY),
                    key_name: Some(FAILSAFE_DURATION_MINIMUM_KEY),
                    value_type: Some(crate::model::DeviceConfigurationKeyValueType::Duration),
                    ..Default::default()
                },
            ]),
        },
    )
}

/// The current failsafe values (LPC/LPP Table 25).
pub fn failsafe_values(system: &ControllableSystem) -> CmdData {
    CmdData::DeviceConfigurationKeyValueListData(DeviceConfigurationKeyValueListData {
        device_configuration_key_value_data: Some(vec![
            DeviceConfigurationKeyValueData {
                key_id: Some(FAILSAFE_LIMIT_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    scaled_number: Some(ScaledNumber::from_f64(system.config().failsafe_watts, 0)),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
            DeviceConfigurationKeyValueData {
                key_id: Some(FAILSAFE_DURATION_KEY),
                value: Some(DeviceConfigurationKeyValueValue {
                    duration: Some(crate::model::format_iso8601_duration(
                        system.config().failsafe_duration,
                    )),
                    ..Default::default()
                }),
                is_value_changeable: Some(true),
            },
        ]),
    })
}

/// A heartbeat message (LPC/LPP Table 26).
///
/// `heartbeatCounter` increases with every notification but not with a reply, and
/// `heartbeatTimeout` is at most sixty seconds — LPC uses sixty, and a shorter value is
/// permitted where another use case on the same connection needs one.
pub fn heartbeat(counter: u64, timeout: Duration, timestamp: &str) -> CmdData {
    CmdData::DeviceDiagnosisHeartbeatData(DeviceDiagnosisHeartbeatData {
        timestamp: Some(timestamp.into()),
        heartbeat_counter: Some(counter),
        heartbeat_timeout: Some(crate::model::format_iso8601_duration(timeout)),
    })
}

/// Reads a `loadControlLimitListData` payload as a limit change.
///
/// Returns [`None`] when the payload names a different limit, which the Controllable
/// System refuses rather than guesses at.
///
/// **Give this the resolved state, not a partial update.** It reads the payload as a
/// complete description of the limit, so an absent `isLimitActive` reads as *not active*.
/// That is right for a full value and wrong for a fragment, where an omitted element means
/// *unchanged* (SPINE IG §3.3) — a partial write adjusting only `value` would otherwise
/// read as a deactivation. Pass [`WriteRequest::resolved`], which is the update already
/// merged into what is stored; [`WriteRequest::data`] is for asking *which* entries the
/// write addresses.
///
/// [`WriteRequest::resolved`]: crate::spine::WriteRequest::resolved
/// [`WriteRequest::data`]: crate::spine::WriteRequest::data
pub fn read_limit_write(data: &CmdData) -> Option<LimitWrite> {
    let CmdData::LoadControlLimitListData(list) = data else {
        return None;
    };
    let entry = list
        .load_control_limit_data
        .as_ref()?
        .iter()
        .find(|e| e.limit_id == Some(LIMIT_ID))?;

    // A `value` that is present but unreadable — a `scale` that overflows `f64`, say —
    // makes the whole write unusable. Refusing it produces a NACK; substituting a number
    // the peer never sent would apply a limit nobody asked for.
    let watts = match entry.value.as_ref() {
        Some(value) => value.to_f64()?,
        None => 0.0,
    };

    Some(LimitWrite {
        is_active: entry.is_limit_active.unwrap_or(false),
        watts,
        duration: entry
            .time_period
            .as_ref()
            .and_then(|p| p.end_time.as_ref())
            .and_then(|t| t.as_duration()),
    })
}

/// Reads a heartbeat notification's counter.
pub fn read_heartbeat(data: &CmdData) -> Option<u64> {
    let CmdData::DeviceDiagnosisHeartbeatData(heartbeat) = data else {
        return None;
    };
    heartbeat.heartbeat_counter.or(Some(0))
}

/// What the Controllable System actor did.
#[derive(Clone, Debug, PartialEq)]
pub enum CsEvent {
    /// A write on the Active Power Limit was decided.
    ///
    /// Under §14a EnWG this is the record the operator has to be able to produce
    /// (implementation guide §4.1.5): what was asked for, and what was answered.
    LimitDecided {
        /// The `msgCounter` of the write, which the acknowledgement references.
        request: crate::model::MsgCounter,
        /// What the Energy Guard asked for.
        write: LimitWrite,
        /// What was answered.
        outcome: WriteOutcome,
    },
    /// A write on one of the failsafe values was decided.
    FailsafeDecided {
        /// The `msgCounter` of the write.
        request: crate::model::MsgCounter,
        /// What was answered.
        outcome: WriteOutcome,
    },
    /// An Energy Guard entity completed both bindings and is now the one in control.
    ///
    /// The LPC implementation guide §3.8 makes this the moment the heartbeat
    /// subscription may be sent: until both bindings arrive from one entity, which of a
    /// device's energy-manager entities is in charge is unknown.
    GuardIdentified {
        /// The entity that holds both bindings.
        guard: FeatureAddress,
    },
    /// The state machine changed state.
    StateChanged {
        /// What it was.
        from: LimitationState,
        /// What it is now.
        to: LimitationState,
    },
}

/// The Controllable System, wired to a SPINE engine.
///
/// Holds the state machine of [`ControllableSystem`] and the addresses of the three
/// features the use case needs, and turns engine events into state-machine input and
/// back. Everything the implementation guides ask of the *communication* — the binding
/// lock of §3.5, the single controlling entity of §3.8, the heartbeat of scenario 3 — is
/// here; everything they ask of the *decision* is in [`ControllableSystem`].
#[derive(Debug)]
pub struct ControllableSystemActor {
    system: ControllableSystem,
    direction: Direction,
    load_control: FeatureAddress,
    device_configuration: FeatureAddress,
    device_diagnosis: FeatureAddress,
    heartbeat: HeartbeatProducer,
    /// The Energy Guard entities holding each of the two bindings (§3.8).
    bound_load_control: Option<FeatureAddress>,
    bound_configuration: Option<FeatureAddress>,
    guard: Option<FeatureAddress>,
    /// The identified guard's heartbeat feature, and whether it has been subscribed to.
    guard_diagnosis: Option<FeatureAddress>,
    subscribed: bool,
    /// Whether the guard's own discovery has been asked for, so it is asked once.
    guard_discovery_asked: bool,
    audit: AuditLog,
}

impl ControllableSystemActor {
    /// Wires a state machine to the features it serves.
    ///
    /// `direction` selects the use case: [`Direction::Consumption`] for LPC,
    /// [`Direction::Production`] for LPP. It decides what this actor publishes, so it has
    /// to match the descriptor the device announces — [`crate::usecases::lpc`] and
    /// [`crate::usecases::lpp`] carry both as a pair.
    pub fn new(
        system: ControllableSystem,
        direction: Direction,
        load_control: FeatureAddress,
        device_configuration: FeatureAddress,
        device_diagnosis: FeatureAddress,
    ) -> Self {
        Self {
            system,
            direction,
            load_control,
            device_configuration,
            device_diagnosis,
            heartbeat: HeartbeatProducer::new(Duration::ZERO),
            bound_load_control: None,
            bound_configuration: None,
            guard: None,
            guard_diagnosis: None,
            subscribed: false,
            guard_discovery_asked: false,
            audit: AuditLog::new(),
        }
    }

    /// Which of the two limitation use cases this actor plays.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// The state machine.
    pub fn system(&self) -> &ControllableSystem {
        &self.system
    }

    /// The state machine, for the application to read the effective limit from.
    pub fn system_mut(&mut self) -> &mut ControllableSystem {
        &mut self.system
    }

    /// The record of every limit written to this system and how it was answered.
    ///
    /// Under §14a EnWG this is the operator's evidence that a limitation was honoured
    /// (LPC implementation guide §4.1.5). It fills itself; draining it into storage is
    /// the application's business, and so is how long the operator has to keep it.
    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    /// The record, for draining it into storage.
    pub fn audit_mut(&mut self) -> &mut AuditLog {
        &mut self.audit
    }

    /// Replaces the record, for a device that keeps more or less history.
    #[must_use]
    pub fn with_audit_log(mut self, audit: AuditLog) -> Self {
        self.audit = audit;
        self
    }

    /// The Energy Guard entity in control, once its bindings have settled (§3.8).
    pub fn guard(&self) -> Option<&FeatureAddress> {
        self.guard.as_ref()
    }

    /// Publishes what this actor serves, and tells the engine what belongs together.
    ///
    /// Call once the device is built. Besides the descriptions and current values, it
    /// locks `LoadControl` and `DeviceConfiguration` to one binding partner: the LPC
    /// implementation guide §3.5 (1.1.0) asks for that because the use case needs both,
    /// and two energy managers coming up together can otherwise win one each.
    pub fn install(&self, engine: &mut Engine, now: Duration) {
        engine
            .bind_features_together([self.load_control.clone(), self.device_configuration.clone()]);
        self.publish(engine, now);
    }

    /// Publishes the descriptions and current values this actor serves, and notifies
    /// anyone subscribed to them.
    ///
    /// Called by [`install`](Self::install), and again whenever the state machine's view
    /// changes, so that a peer reading — or watching — gets what it actually holds.
    pub fn publish(&self, engine: &mut Engine, now: Duration) {
        let load_control = self.load_control.clone();
        let configuration = self.device_configuration.clone();

        let mut limit_changed = false;
        if let Some(feature) = engine.device_mut().resolve_mut(&load_control) {
            let _ = feature.set_data(limit_description(self.direction));
            limit_changed = feature.set_data(limit_data(&self.system)).unwrap_or(false);
        }
        let mut failsafe_changed = false;
        if let Some(feature) = engine.device_mut().resolve_mut(&configuration) {
            let _ = feature.set_data(failsafe_descriptions(self.direction));
            failsafe_changed = feature
                .set_data(failsafe_values(&self.system))
                .unwrap_or(false);
        }

        // Implementation guide §2.4: only what changed is notified.
        if limit_changed {
            engine.notify(&load_control, &Function::LoadControlLimitListData, now);
        }
        if failsafe_changed {
            engine.notify(
                &configuration,
                &Function::DeviceConfigurationKeyValueListData,
                now,
            );
        }
    }

    /// Interrupts an active limitation for a permitted reason ([LPC/LPP-923]).
    ///
    /// The device's own decision, not the Energy Guard's: a heat pump that must run to
    /// defrost, an inverter a regulator requires to keep feeding in. The published limit
    /// follows, so the Energy Guard sees the deactivation rather than an appliance
    /// quietly exceeding what it was told.
    ///
    /// Returns whether anything changed; nothing does outside the `limited` state.
    pub fn interrupt(
        &mut self,
        engine: &mut Engine,
        reason: super::state::RejectReason,
        now: Duration,
    ) -> bool {
        if !self.system.interrupt(reason, now) {
            return false;
        }
        self.publish(engine, now);
        true
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called.
    pub fn poll_timeout(&self) -> Duration {
        match self.system.poll_timeout() {
            Some(state) => state.min(self.heartbeat.poll_timeout()),
            None => self.heartbeat.poll_timeout(),
        }
    }

    /// Advances the heartbeat and the state machine's timers.
    ///
    /// Returns a [`CsEvent::StateChanged`] when the effective limit changed, which is
    /// what the appliance acts on — the failsafe fallback of [LPC/LPP-911] arrives this
    /// way and no other.
    pub fn handle_timeout(&mut self, engine: &mut Engine, now: Duration) -> Option<CsEvent> {
        let diagnosis = self.device_diagnosis.clone();
        self.heartbeat.tick(engine, &diagnosis, now);

        let before = self.system.state();
        self.system.handle_timeout(now);
        let after = self.system.state();
        if before == after {
            return None;
        }
        self.publish(engine, now);
        Some(CsEvent::StateChanged {
            from: before,
            to: after,
        })
    }

    /// Handles one engine event, and resolves any write it carries.
    pub fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: &SpineEvent,
        now: Duration,
    ) -> Option<CsEvent> {
        match event {
            SpineEvent::DataNotified { data, .. } | SpineEvent::ReplyReceived { data, .. } => {
                // A heartbeat from the Energy Guard is what keeps the failsafe at bay.
                if read_heartbeat(data).is_some() {
                    let before = self.system.state();
                    self.system.on_heartbeat(now);
                    if self.system.state() != before {
                        return Some(CsEvent::StateChanged {
                            from: before,
                            to: self.system.state(),
                        });
                    }
                }
                None
            }
            SpineEvent::BindingGranted { client, server } => {
                let event = self.on_binding(client, server);
                self.follow_the_guards_heartbeat(engine, now);
                event
            }
            SpineEvent::DiscoveryUpdated { .. } => {
                // The Energy Guard's own features arrived; §3.8's subscription can now
                // be addressed.
                self.follow_the_guards_heartbeat(engine, now);
                None
            }
            SpineEvent::BindingReleased { server, .. } => {
                if same_entity_address(server, &self.load_control) {
                    self.bound_load_control = None;
                } else if same_entity_address(server, &self.device_configuration) {
                    self.bound_configuration = None;
                }
                self.guard = None;
                self.guard_diagnosis = None;
                self.subscribed = false;
                self.guard_discovery_asked = false;
                None
            }
            // `data` for the entries a write addresses, `resolved` for what they become.
            SpineEvent::WriteRequested(write) if write.feature == self.load_control => {
                Some(self.decide_limit(
                    engine,
                    write.token,
                    write.request,
                    &write.from,
                    &write.data,
                    &write.resolved,
                    now,
                ))
            }
            SpineEvent::WriteRequested(write) if write.feature == self.device_configuration => {
                Some(self.decide_failsafe(
                    engine,
                    write.token,
                    write.request,
                    &write.data,
                    &write.resolved,
                    now,
                ))
            }
            _ => None,
        }
    }

    /// §3.8: the Energy Guard entity is the one that binds both features.
    fn on_binding(&mut self, client: &FeatureAddress, server: &FeatureAddress) -> Option<CsEvent> {
        if server == &self.load_control {
            self.bound_load_control = Some(client.clone());
        } else if server == &self.device_configuration {
            self.bound_configuration = Some(client.clone());
        } else {
            return None;
        }

        let (Some(a), Some(b)) = (&self.bound_load_control, &self.bound_configuration) else {
            return None;
        };
        // The engine's binding lock already refuses a second entity, so reaching here
        // with two different ones is not possible; the check documents the rule and
        // costs nothing.
        if !crate::spine::same_entity(a, b) || self.guard.is_some() {
            return None;
        }
        self.guard = Some(a.clone());
        Some(CsEvent::GuardIdentified { guard: a.clone() })
    }

    /// §3.8 step 4: once one entity holds both bindings, subscribe to *its* heartbeat.
    ///
    /// Not before. An energy manager may expose several `CEM` entities and only one of
    /// them is in control; subscribing to the wrong one's heartbeat would keep the
    /// failsafe at bay on the word of an entity that is not the one writing limits.
    fn follow_the_guards_heartbeat(&mut self, engine: &mut Engine, now: Duration) {
        if self.subscribed {
            return;
        }
        let Some(guard) = self.guard.clone() else {
            return;
        };
        let Some(device) = guard.device.clone() else {
            return;
        };

        let entity = crate::spine::entity_path(&guard);
        let diagnosis = engine
            .peer(&device)
            .and_then(|remote| remote.entity(&entity))
            .and_then(|entity| entity.feature(&FeatureType::DeviceDiagnosis, Role::Server))
            .map(|feature| feature.address.clone());

        match diagnosis {
            Some(diagnosis) => {
                let local = self.device_diagnosis.clone();
                engine.request_subscription(&local, &diagnosis, now);
                self.guard_diagnosis = Some(diagnosis);
                self.subscribed = true;
            }
            None if !self.guard_discovery_asked => {
                // Ask who it is; the answer brings us back here through
                // `SpineEvent::DiscoveryUpdated`. Asked once: an Energy Guard that
                // serves no `DeviceDiagnosis` will not grow one, and re-reading on every
                // discovery update would answer its own reply forever.
                self.guard_discovery_asked = true;
                let source = crate::spine::node_management(engine.device().address());
                let destination = crate::spine::node_management(&device);
                engine.read(
                    &destination,
                    &source,
                    Function::NodeManagementDetailedDiscoveryData,
                    now,
                );
            }
            None => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_limit(
        &mut self,
        engine: &mut Engine,
        token: WriteToken,
        request: crate::model::MsgCounter,
        from: &FeatureAddress,
        data: &CmdData,
        resolved: &CmdData,
        now: Duration,
    ) -> CsEvent {
        let peer = from.device.clone();
        // `data` says whether this write addresses the active power limit at all;
        // `resolved` says what that limit becomes, with anything the partial write left
        // out filled in from what is stored (SPINE IG §3.3).
        let addressed = read_limit_write(data).is_some();
        let Some(write) = addressed.then(|| read_limit_write(resolved)).flatten() else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            let write = LimitWrite::deactivated();
            let outcome = WriteOutcome::Rejected(super::state::NackReason::NegativeValue);
            self.log(request, write, outcome, peer, now);
            return CsEvent::LimitDecided {
                request,
                write,
                outcome,
            };
        };

        // A real device asks its controller here whether it can follow the limit; the
        // state machine handles everything else — the ordering gate, the value range and
        // the transition.
        let outcome = self
            .system
            .on_limit_write(&write, LocalDecision::Apply, now);

        if outcome.is_accepted() {
            engine.accept_write(token, now);
            // The peer reads back what was applied, not what it asked for.
            self.publish(engine, now);
        } else {
            engine.reject_write(token, outcome.error_number(), now);
        }
        self.log(request, write, outcome, peer, now);
        CsEvent::LimitDecided {
            request,
            write,
            outcome,
        }
    }

    /// Files the §14a record of a limit and its answer.
    fn log(
        &mut self,
        request: crate::model::MsgCounter,
        write: LimitWrite,
        outcome: WriteOutcome,
        peer: Option<crate::model::AddressDevice>,
        now: Duration,
    ) {
        let mut record =
            crate::usecases::limitation::LimitRecord::new(now, request, write, outcome);
        record.peer = peer;
        self.audit.record(record);
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_failsafe(
        &mut self,
        engine: &mut Engine,
        token: WriteToken,
        request: crate::model::MsgCounter,
        data: &CmdData,
        resolved: &CmdData,
        now: Duration,
    ) -> CsEvent {
        let (
            CmdData::DeviceConfigurationKeyValueListData(written),
            CmdData::DeviceConfigurationKeyValueListData(effective),
        ) = (data, resolved)
        else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return CsEvent::FailsafeDecided {
                request,
                outcome: WriteOutcome::Rejected(super::state::NackReason::SequenceIncomplete),
            };
        };

        // Only the keys this write addresses are decided on — `resolved` also carries the
        // keys already stored, and re-applying those would replay a value the peer never
        // sent. Their *values*, though, come from `resolved`: a partial write may carry a
        // `scaledNumber` with a bare `number`, whose scale lives in the stored value.
        let mut outcome = WriteOutcome::Accepted;
        for key in written
            .device_configuration_key_value_data
            .iter()
            .flatten()
            .filter_map(|entry| entry.key_id)
        {
            let value = effective
                .device_configuration_key_value_data
                .iter()
                .flatten()
                .find(|entry| entry.key_id == Some(key))
                .and_then(|entry| entry.value.as_ref());
            let result = match key {
                FAILSAFE_LIMIT_KEY => {
                    let watts = value
                        .and_then(|v| v.scaled_number.as_ref())
                        .and_then(ScaledNumber::to_f64)
                        .unwrap_or(-1.0);
                    self.system.on_failsafe_limit_write(watts, now)
                }
                FAILSAFE_DURATION_KEY => {
                    let duration = value
                        .and_then(|v| v.duration.as_deref())
                        .and_then(crate::model::parse_iso8601_duration)
                        .unwrap_or_default();
                    self.system.on_failsafe_duration_write(duration, now)
                }
                _ => continue,
            };
            if !result.is_accepted() {
                outcome = result;
            }
        }

        if outcome.is_accepted() {
            engine.accept_write(token, now);
            // The peer reads back what was applied, not what it asked for.
            self.publish(engine, now);
        } else {
            engine.reject_write(token, outcome.error_number(), now);
        }
        CsEvent::FailsafeDecided { request, outcome }
    }
}

/// Whether two addresses name features of the same entity.
fn same_entity_address(a: &FeatureAddress, b: &FeatureAddress) -> bool {
    crate::spine::same_entity(a, b)
}
