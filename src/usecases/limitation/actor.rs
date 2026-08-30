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
use crate::spine::{Engine, ErrorNumber, LocalFeature, Operations, SpineEvent, WriteToken};

use super::state::{ControllableSystem, LimitWrite, LocalDecision, WriteOutcome};
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

/// Reads a `loadControlLimitListData` write as a limit change.
///
/// Returns [`None`] when the payload names a different limit, which the Controllable
/// System refuses rather than guesses at.
pub fn read_limit_write(data: &CmdData) -> Option<LimitWrite> {
    let CmdData::LoadControlLimitListData(list) = data else {
        return None;
    };
    let entry = list
        .load_control_limit_data
        .as_ref()?
        .iter()
        .find(|e| e.limit_id == Some(LIMIT_ID))?;

    Some(LimitWrite {
        is_active: entry.is_limit_active.unwrap_or(false),
        watts: entry
            .value
            .as_ref()
            .and_then(ScaledNumber::to_f64)
            .unwrap_or(0.0),
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

/// The Controllable System, wired to a SPINE engine.
///
/// Holds the state machine of [`ControllableSystem`] and the addresses of the three
/// features LPC needs, and turns engine events into state-machine input and back.
#[derive(Debug)]
pub struct ControllableSystemActor {
    system: ControllableSystem,
    direction: Direction,
    load_control: FeatureAddress,
    device_configuration: FeatureAddress,
    device_diagnosis: FeatureAddress,
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

    /// Publishes the descriptions and current values this actor serves.
    ///
    /// Called once the device is built, and again whenever the failsafe values change,
    /// so that a peer reading them gets what the state machine actually holds.
    pub fn publish(&self, engine: &mut Engine) {
        let load_control = self.load_control.clone();
        let configuration = self.device_configuration.clone();
        if let Some(feature) = engine.device_mut().resolve_mut(&load_control) {
            let _ = feature.set_data(limit_description(self.direction));
            let _ = feature.set_data(limit_data(&self.system));
        }
        if let Some(feature) = engine.device_mut().resolve_mut(&configuration) {
            let _ = feature.set_data(failsafe_descriptions(self.direction));
            let _ = feature.set_data(failsafe_values(&self.system));
        }
    }

    /// Sends a heartbeat to every subscriber.
    pub fn send_heartbeat(
        &self,
        engine: &mut Engine,
        counter: u64,
        timestamp: &str,
        now: Duration,
    ) {
        let address = self.device_diagnosis.clone();
        if let Some(feature) = engine.device_mut().resolve_mut(&address) {
            let _ = feature.set_data(heartbeat(counter, Duration::from_secs(60), timestamp));
        }
        engine.notify(&address, &Function::DeviceDiagnosisHeartbeatData, now);
    }

    /// Handles one engine event, and resolves any write it carries.
    ///
    /// Returns the outcome when the event was a write this actor owns, so the caller can
    /// log the acknowledgement — under §14a EnWG the operator has to be able to produce
    /// exactly that record (LPC implementation guide §4.1.5).
    pub fn handle_event(
        &mut self,
        engine: &mut Engine,
        event: &SpineEvent,
        now: Duration,
    ) -> Option<WriteOutcome> {
        match event {
            SpineEvent::DataNotified { data, .. } => {
                // A heartbeat from the Energy Guard is what keeps the failsafe at bay.
                if read_heartbeat(data).is_some() {
                    self.system.on_heartbeat(now);
                }
                None
            }
            SpineEvent::WriteRequested {
                token,
                feature,
                data,
                ..
            } if feature == &self.load_control => {
                Some(self.decide_limit(engine, *token, data, now))
            }
            SpineEvent::WriteRequested {
                token,
                feature,
                data,
                ..
            } if feature == &self.device_configuration => {
                Some(self.decide_failsafe(engine, *token, data, now))
            }
            _ => None,
        }
    }

    fn decide_limit(
        &mut self,
        engine: &mut Engine,
        token: WriteToken,
        data: &CmdData,
        now: Duration,
    ) -> WriteOutcome {
        let Some(write) = read_limit_write(data) else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return WriteOutcome::Rejected(super::state::NackReason::NegativeValue);
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
            self.publish(engine);
        } else {
            engine.reject_write(token, outcome.error_number(), now);
        }
        outcome
    }

    fn decide_failsafe(
        &mut self,
        engine: &mut Engine,
        token: WriteToken,
        data: &CmdData,
        now: Duration,
    ) -> WriteOutcome {
        let CmdData::DeviceConfigurationKeyValueListData(list) = data else {
            engine.reject_write(token, ErrorNumber::CommandRejected, now);
            return WriteOutcome::Rejected(super::state::NackReason::SequenceIncomplete);
        };

        let mut outcome = WriteOutcome::Accepted;
        for entry in list.device_configuration_key_value_data.iter().flatten() {
            let value = entry.value.as_ref();
            let result = match entry.key_id {
                Some(FAILSAFE_LIMIT_KEY) => {
                    let watts = value
                        .and_then(|v| v.scaled_number.as_ref())
                        .and_then(ScaledNumber::to_f64)
                        .unwrap_or(-1.0);
                    self.system.on_failsafe_limit_write(watts, now)
                }
                Some(FAILSAFE_DURATION_KEY) => {
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
            self.publish(engine);
        } else {
            engine.reject_write(token, outcome.error_number(), now);
        }
        outcome
    }
}
