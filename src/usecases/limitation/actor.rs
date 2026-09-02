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
    DeviceDiagnosisHeartbeatData, ElectricalConnectionCharacteristicContext,
    ElectricalConnectionCharacteristicData, ElectricalConnectionCharacteristicId,
    ElectricalConnectionCharacteristicListData, ElectricalConnectionCharacteristicType,
    ElectricalConnectionId, ElectricalConnectionParameterId, FeatureAddress, FeatureType, Function,
    LoadControlCategory, LoadControlLimitData, LoadControlLimitDescriptionData,
    LoadControlLimitDescriptionListData, LoadControlLimitId, LoadControlLimitListData,
    LoadControlLimitType, MeasurementId, Role, ScaledNumber, ScopeType, UnitOfMeasurement,
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

/// The `electricalConnectionId` the constraints of scenario 4 are published under.
///
/// LPC/LPP Table 27 requires this to be the *same* identifier the device uses for MPC's
/// `<ec1#1>`, where it implements MPC as well, so that an Energy Guard reading both sees
/// one electrical connection rather than two. [`crate::usecases::monitoring`] numbers
/// its connection the same way.
pub const ELECTRICAL_CONNECTION_ID: ElectricalConnectionId = ElectricalConnectionId(1);

/// The `parameterId` of the constraints, likewise shared with MPC.
pub const PARAMETER_ID: ElectricalConnectionParameterId = ElectricalConnectionParameterId(1);

/// The `characteristicId` of the nominal maximum.
pub const CHARACTERISTIC_ID: ElectricalConnectionCharacteristicId =
    ElectricalConnectionCharacteristicId(1);

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

/// Builds the `ElectricalConnection` feature scenario 4 is served from (LPC/LPP Table 27).
///
/// Reads only: the nominal maxima are what the device *is*, and no Energy Guard may write
/// them.
pub fn electrical_connection_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::ElectricalConnection, Role::Server).with_function(
        Function::ElectricalConnectionCharacteristicListData,
        Operations::read(),
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

/// Builds the **client**-role `DeviceDiagnosis` feature a Controllable System subscribes
/// from.
///
/// A subscription runs from a client feature to a server one (SPINE §5.3.6), and the
/// Controllable System is on the client end of exactly one relationship: the Energy
/// Guard's heartbeat. Subscribing from the server feature that carries its *own* heartbeat
/// is a role mismatch, which `spine-go` refuses; real devices carry a `DeviceDiagnosis`
/// server and client side by side for this reason.
///
pub fn device_diagnosis_client_feature(address: u32) -> LocalFeature {
    LocalFeature::new(address, FeatureType::DeviceDiagnosis, Role::Client)
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

/// The nominal maximum a Controllable System publishes in scenario 4.
///
/// The two are mutually exclusive, and which one applies is not a matter of taste: LPC
/// UC TS §2.6.4.1 says [LPC/LPP-041] "SHALL only be used if the CS is an energy consuming
/// device … not an energy manager", and [LPC/LPP-042] "SHALL only be used if the CS is an
/// energy manager … not a single device". A heat pump has a nameplate; an energy manager
/// has a contract. Publishing the wrong one tells the Energy Guard it is limiting
/// something other than what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NominalMax {
    /// [LPC/LPP-041]: the most this appliance can physically draw or feed in.
    Device(f64),
    /// [LPC/LPP-042]: the most this energy manager's customer contract allows.
    Contractual(f64),
}

impl NominalMax {
    /// The value in watts, whichever kind it is.
    pub fn watts(self) -> f64 {
        match self {
            NominalMax::Device(watts) | NominalMax::Contractual(watts) => watts,
        }
    }

    /// The `characteristicType` this value is published under.
    pub fn characteristic(self, direction: Direction) -> ElectricalConnectionCharacteristicType {
        match self {
            NominalMax::Device(_) => direction.nominal_max_characteristic(),
            NominalMax::Contractual(_) => direction.contractual_max_characteristic(),
        }
    }
}

/// Which nominal maximum this configuration should publish, if any.
///
/// An energy manager publishes its contract and a device publishes its nameplate; a
/// configuration that carries the value belonging to the *other* kind of actor publishes
/// nothing, rather than publishing it under a name the specification reserves.
pub fn nominal_max(system: &ControllableSystem) -> Option<NominalMax> {
    let config = system.config();
    if config.on_cem {
        config.contractual_max_watts.map(NominalMax::Contractual)
    } else {
        config.nominal_max_watts.map(NominalMax::Device)
    }
}

/// The constraints of scenario 4, as `electricalConnectionCharacteristicListData`
/// (LPC/LPP Table 27).
///
/// Empty when the device has no nominal maximum to publish — scenario 4 is `R` for a
/// Controllable System, and a device that does not know its own nameplate publishes
/// nothing rather than a guess.
pub fn constraints(system: &ControllableSystem, direction: Direction) -> CmdData {
    let entries = nominal_max(system)
        .map(|max| {
            vec![ElectricalConnectionCharacteristicData {
                electrical_connection_id: Some(ELECTRICAL_CONNECTION_ID),
                parameter_id: Some(PARAMETER_ID),
                characteristic_id: Some(CHARACTERISTIC_ID),
                characteristic_context: Some(ElectricalConnectionCharacteristicContext::Entity),
                characteristic_type: Some(max.characteristic(direction)),
                value: Some(ScaledNumber::from_f64(max.watts(), 0)),
                unit: Some(UnitOfMeasurement::W),
            }]
        })
        .unwrap_or_default();
    CmdData::ElectricalConnectionCharacteristicListData(
        ElectricalConnectionCharacteristicListData {
            electrical_connection_characteristic_data: Some(entries),
        },
    )
}

/// Reads the nominal maximum out of an `electricalConnectionCharacteristicListData`.
///
/// This is what an Energy Guard needs to turn a percentage from the grid operator into
/// watts. It reports which of the two it found, because the two mean different things:
/// exceeding a nameplate is impossible, and exceeding a contract is merely expensive.
pub fn read_constraints(data: &CmdData, direction: Direction) -> Option<NominalMax> {
    let CmdData::ElectricalConnectionCharacteristicListData(list) = data else {
        return None;
    };
    list.electrical_connection_characteristic_data
        .iter()
        .flatten()
        .find_map(|entry| {
            let watts = entry.value.as_ref().and_then(ScaledNumber::to_f64)?;
            match entry.characteristic_type.as_ref() {
                Some(kind) if *kind == direction.nominal_max_characteristic() => {
                    Some(NominalMax::Device(watts))
                }
                Some(kind) if *kind == direction.contractual_max_characteristic() => {
                    Some(NominalMax::Contractual(watts))
                }
                _ => None,
            }
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
    /// A write on the LoadControl feature could not be read as a limit, and was refused.
    ///
    /// Either the payload named a limit this system does not serve, or it named the right
    /// one and carried a value that cannot be represented. The two are told apart by the
    /// state: only the second reaches the state machine, because only the second is a
    /// limit write. Neither carries a [`LimitWrite`], which is why this is its own event
    /// rather than a [`CsEvent::LimitDecided`] with a value nobody sent.
    LimitUnreadable {
        /// The `msgCounter` of the write, which the refusal references.
        request: crate::model::MsgCounter,
        /// The refusal, which is always a [`WriteOutcome::Rejected`].
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
    /// The client-role `DeviceDiagnosis` this system subscribes to the guard's heartbeat
    /// from (SPINE §5.3.6: a subscription runs client → server).
    device_diagnosis_client: FeatureAddress,
    /// Where scenario 4's constraints are served from, for a device that publishes them.
    electrical_connection: Option<FeatureAddress>,
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

/// Where a Controllable System serves each of its three features.
///
/// A struct rather than three arguments, because all three are a [`FeatureAddress`] and
/// nothing but the order says which is which. Passing the heartbeat's address where the
/// limit's belongs compiles, runs, and produces a device that answers a grid operator's
/// limit write on the wrong feature — which is precisely the class of mistake the rest of
/// this crate spends distinct newtypes to prevent.
///
/// ```
/// use eebus::prelude::*;
/// use eebus::usecases::limitation::CsFeatures;
///
/// # fn wire(device: &LocalDevice) -> CsFeatures {
/// CsFeatures {
///     load_control: device.address_of(&[1], 1),
///     device_configuration: device.address_of(&[1], 2),
///     device_diagnosis: device.address_of(&[1], 3),
///     device_diagnosis_client: device.address_of(&[1], 5),
/// }
/// # }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct CsFeatures {
    /// `LoadControl`, where the active power limit is written.
    pub load_control: FeatureAddress,
    /// `DeviceConfiguration`, where the failsafe values live.
    pub device_configuration: FeatureAddress,
    /// `DeviceDiagnosis`, where this system's own heartbeat is published.
    pub device_diagnosis: FeatureAddress,
    /// The **client**-role `DeviceDiagnosis` this system subscribes to the guard's
    /// heartbeat from.
    ///
    /// A subscription runs client → server, so the system's own server feature cannot be
    /// the near end of it: `spine-go` refuses that with a role mismatch, and the heartbeat
    /// it would have carried is what LPC IG §2.11 gates every limit on. Build it with
    /// [`device_diagnosis_client_feature`].
    pub device_diagnosis_client: FeatureAddress,
}

/// Builds a [`ControllableSystemActor`], ending at [`install`](Self::install).
///
/// **There is no other way to make one**, and that is the point. The previous shape was a
/// constructor and a separate `install` call, and forgetting the second produced no
/// symptom at all: the device answers discovery, grants the bindings and the subscription,
/// exchanges heartbeats — and publishes no limit description, so the Energy Guard reads an
/// empty list, finds no `limitId` to write to, and **never sends a limit**. Nothing on the
/// wire says why. Under §14a EnWG that is an installation that looks commissioned and
/// silently is not.
#[derive(Debug)]
pub struct ControllableSystemBuilder {
    actor: ControllableSystemActor,
}

impl ControllableSystemBuilder {
    /// Serves scenario 4's constraints from an `ElectricalConnection` feature.
    ///
    /// Scenario 4 is `R` for a Controllable System, and the Energy Guard's side of it is
    /// `M`: without the nominal maximum, an operator that works in percentages has no way
    /// to turn one into watts ([LPC/LPP-041], [LPC/LPP-042]). Which of the two values is
    /// published follows from [`CsConfig`](super::CsConfig) — the nameplate for an
    /// appliance, the contract for an energy manager — and a device that has set neither
    /// publishes an empty list rather than a guess.
    ///
    /// The address must be a feature built by
    /// [`electrical_connection_feature`](super::electrical_connection_feature).
    #[must_use]
    pub fn with_electrical_connection(mut self, address: FeatureAddress) -> Self {
        self.actor.electrical_connection = Some(address);
        self
    }

    /// Replaces the § 14a record, for a device that keeps more or less history.
    #[must_use]
    pub fn with_audit_log(mut self, audit: AuditLog) -> Self {
        self.actor.audit = audit;
        self
    }

    /// Publishes what the actor serves, tells the engine what belongs together, and hands
    /// the actor back.
    ///
    /// Besides the descriptions and current values, it locks `LoadControl` and
    /// `DeviceConfiguration` to one binding partner: the LPC implementation guide §3.5
    /// (1.1.0) asks for that because the use case needs both, and two energy managers
    /// coming up together can otherwise win one each.
    #[must_use]
    pub fn install(self, engine: &mut Engine, now: Duration) -> ControllableSystemActor {
        let actor = self.actor;
        engine.bind_features_together([
            actor.load_control.clone(),
            actor.device_configuration.clone(),
        ]);
        actor.publish(engine, now);
        actor
    }
}

impl ControllableSystemActor {
    /// Begins wiring a state machine to the features it serves.
    ///
    /// `direction` selects the use case: [`Direction::Consumption`] for LPC,
    /// [`Direction::Production`] for LPP. It decides what this actor publishes, so it has
    /// to match the descriptor the device announces — [`crate::usecases::lpc`] and
    /// [`crate::usecases::lpp`] carry both as a pair.
    ///
    /// The builder ends at [`ControllableSystemBuilder::install`], which is the only way
    /// to obtain an actor — see that type for why.
    ///
    /// ```
    /// use core::time::Duration;
    /// use eebus::prelude::*;
    /// use eebus::usecases::limitation::{
    ///     self, ControllableSystem, ControllableSystemActor, CsConfig, CsFeatures,
    /// };
    /// use eebus::usecases::lpc;
    ///
    /// # fn build() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut device = LocalDevice::new("i:46925", "HeatPump-1", DeviceType::HeatGenerationSystem)?;
    /// device.add_entity(
    ///     LocalEntity::new([1], EntityType::HeatPumpAppliance)
    ///         .with_feature(limitation::load_control_feature(1))
    ///         .with_feature(limitation::device_configuration_feature(2))
    ///         .with_feature(limitation::device_diagnosis_feature(3))
    ///         .with_feature(limitation::device_diagnosis_client_feature(4)),
    /// )?;
    /// let mut engine = Engine::new(device);
    /// let features = CsFeatures {
    ///     load_control: engine.device().address_of(&[1], 1),
    ///     device_configuration: engine.device().address_of(&[1], 2),
    ///     device_diagnosis: engine.device().address_of(&[1], 3),
    ///     device_diagnosis_client: engine.device().address_of(&[1], 4),
    /// };
    ///
    /// let system = ControllableSystem::new(CsConfig::new(4_200.0, Duration::from_secs(7_200)),
    ///                                      Duration::ZERO);
    /// let actor = ControllableSystemActor::builder(system, lpc::DIRECTION, features)
    ///     .install(&mut engine, Duration::ZERO);
    /// # let _ = actor;
    /// # Ok(())
    /// # }
    /// # build().unwrap();
    /// ```
    pub fn builder(
        system: ControllableSystem,
        direction: Direction,
        features: CsFeatures,
    ) -> ControllableSystemBuilder {
        let CsFeatures {
            load_control,
            device_configuration,
            device_diagnosis,
            device_diagnosis_client,
        } = features;
        ControllableSystemBuilder {
            actor: Self {
                system,
                direction,
                load_control,
                device_configuration,
                device_diagnosis,
                device_diagnosis_client,
                electrical_connection: None,
                heartbeat: HeartbeatProducer::new(Duration::ZERO),
                bound_load_control: None,
                bound_configuration: None,
                guard: None,
                guard_diagnosis: None,
                subscribed: false,
                guard_discovery_asked: false,
                audit: AuditLog::new(),
            },
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

    /// The Energy Guard entity in control, once its bindings have settled (§3.8).
    pub fn guard(&self) -> Option<&FeatureAddress> {
        self.guard.as_ref()
    }

    /// Publishes the descriptions and current values this actor serves, and notifies
    /// anyone subscribed to them.
    ///
    /// Called by [`ControllableSystemBuilder::install`], and again whenever the state
    /// machine's view changes, so that a peer reading — or watching — gets what it
    /// actually holds.
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

        if let Some(address) = self.electrical_connection.as_ref()
            && let Some(feature) = engine.device_mut().resolve_mut(address)
        {
            // The nominal maxima do not change while the device runs, so nothing is
            // notified for them: a peer reads them once, in the pre-scenario exchange.
            let _ = feature.set_data(constraints(&self.system, self.direction));
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
            SpineEvent::DataNotified { resolved, .. }
            | SpineEvent::ReplyReceived { resolved, .. } => {
                // A heartbeat from the Energy Guard is what keeps the failsafe at bay.
                // `resolved`, not `data`: a notification may be partial, and a heartbeat
                // that carries only its counter still has the timeout the peer announced.
                if read_heartbeat(resolved).is_some() {
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
                // From the *client* feature: SPINE §5.3.6 runs a subscription client →
                // server, and `spine-go` refuses one whose near end is a server feature.
                let local = self.device_diagnosis_client.clone();
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
        let write = if addressed {
            read_limit_write(resolved)
        } else {
            None
        };
        let Some(write) = write else {
            // Two different refusals share this path, and they differ in one way that
            // matters: whether the state machine hears about it. A write that never named
            // this system's limit is not a limit write, so it does not; one that named it
            // and carried a value too large to represent *is*, and a refused limit write
            // moves the machine ([LPC/LPP-902], [LPC/LPP-918]).
            let outcome = if addressed {
                self.system.on_unreadable_limit_write(now)
            } else {
                WriteOutcome::Rejected(super::state::NackReason::Unreadable)
            };
            engine.reject_write(token, outcome.error_number(), now);
            let mut record = super::audit::LimitRecord::unreadable(now, request, outcome);
            record.peer = peer;
            self.audit.record(record);
            return CsEvent::LimitUnreadable { request, outcome };
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

impl crate::usecases::signals::Signals for ControllableSystemActor {
    /// The state machine's signals, plus what only the actor knows.
    ///
    /// `…:guard` is the Energy Guard entity that holds both bindings (implementation
    /// guide §3.8) — the answer to "which of these energy managers is in charge?", which
    /// a laboratory driving a device with two of them has to be able to see.
    fn signals(&self, _: ()) -> crate::usecases::signals::SignalSet {
        use crate::usecases::signals::{Signal, SignalValue};
        use alloc::borrow::Cow;
        use alloc::format;

        let prefix = self.direction.signal_prefix();
        self.system
            .signals(self.direction)
            .with(Signal::new(
                Cow::Owned(format!("{prefix}:guard")),
                match self.guard.as_ref() {
                    Some(guard) => SignalValue::Text(Cow::Owned(format!(
                        "{} entity {:?} feature {}",
                        guard
                            .device
                            .as_ref()
                            .map(|device| device.as_str())
                            .unwrap_or("(local)"),
                        guard.entity.as_deref().unwrap_or(&[]),
                        guard.feature.map_or(0, |f| f.0),
                    ))),
                    None => SignalValue::Absent,
                },
            ))
            .with(Signal::new(
                Cow::Owned(format!("{prefix}:auditRecords")),
                SignalValue::Number(self.audit.len() as f64),
            ))
    }
}
