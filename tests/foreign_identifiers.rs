//! A peer that numbers its own data, which is the only kind that exists.
//!
//! Every identifier LPC and LPP write to is a placeholder in the specification — `<l1#1>`
//! for the limit (Table 22), `<k1#1>` and `<k2#1>` for the two failsafe keys (Table 24) —
//! and "SHALL be used as the primary identifier" says only that the *device* keeps it
//! stable. The number is the device's to choose.
//!
//! No test written against this crate's own Controllable System can see that. Both ends
//! then agree on `limitId` 1 because both ends are this crate, and a guard that assumed
//! the number passes every one of them. So the Controllable System here is built by hand
//! and numbers everything differently, and the interesting case is deliberate: it serves
//! **both** LPC and LPP from one `LoadControl` feature, which is what a real appliance
//! with a battery does, and it is the *production* limit that sits on `limitId` 1. A
//! guard writing there is not failing — it is limiting the wrong direction, being
//! acknowledged, and recording a §14a limitation that never happened.

use core::time::Duration;

use eebus::model::{
    AddressDevice, CmdData, DeviceConfigurationKeyId, DeviceConfigurationKeyValueDescriptionData,
    DeviceConfigurationKeyValueDescriptionListData, DeviceConfigurationKeyValueType, DeviceType,
    EnergyDirection, EntityType, FeatureType, Function, LoadControlCategory, LoadControlLimitData,
    LoadControlLimitDescriptionData, LoadControlLimitDescriptionListData, LoadControlLimitId,
    LoadControlLimitListData, LoadControlLimitType, MeasurementId, Role, ScopeType,
    UnitOfMeasurement,
};
use eebus::spine::{
    Engine, LocalDevice, LocalEntity, LocalFeature, Operations, SpineEvent, node_management,
};
use eebus::usecases::limitation::{
    self, EnergyGuardActor, GuardEvent, LimitResolutionError, LimitWrite,
};
use eebus::usecases::{lpc, lpp};

/// What this appliance calls its consumption limit. Not 1, and not what the crate uses.
const THEIR_CONSUMPTION_LIMIT: LoadControlLimitId = LoadControlLimitId(7);
/// What it calls its production limit — the one that *is* on 1.
const THEIR_PRODUCTION_LIMIT: LoadControlLimitId = LoadControlLimitId(1);
/// And its two failsafe keys, behind three other configuration keys it happens to have.
const THEIR_FAILSAFE_LIMIT: DeviceConfigurationKeyId = DeviceConfigurationKeyId(5);
const THEIR_FAILSAFE_DURATION: DeviceConfigurationKeyId = DeviceConfigurationKeyId(6);

/// An Energy Guard, and an appliance that is not built out of this crate.
struct Foreign {
    guard_engine: Engine,
    guard: EnergyGuardActor,
    them: Engine,
    now: Duration,
    reports: Vec<GuardEvent>,
}

impl Foreign {
    /// `limits` is what the appliance's `LoadControl` feature describes.
    fn new(limits: CmdData) -> Self {
        let now = Duration::ZERO;

        let mut guard_device =
            LocalDevice::new("i:1", "Box-1", DeviceType::ElectricitySupplySystem).unwrap();
        guard_device
            .add_entity(
                LocalEntity::new([1], EntityType::GridGuard)
                    .with_feature(limitation::client_feature(1))
                    .with_feature(limitation::device_diagnosis_feature(2)),
            )
            .unwrap();
        let client = guard_device.address_of(&[1], 1);
        let diagnosis = guard_device.address_of(&[1], 2);
        let mut guard_engine = Engine::new(guard_device);
        guard_engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
        let guard = EnergyGuardActor::new(lpc::DIRECTION, client, diagnosis, now);

        let mut device =
            LocalDevice::new("i:2", "Battery-1", DeviceType::HeatGenerationSystem).unwrap();
        device
            .add_entity(
                LocalEntity::new([1], EntityType::HeatPumpAppliance)
                    // Built here rather than with the crate's constructors: those defer
                    // writes to a `ControllableSystem`, and this appliance is standing in
                    // for one that is not made of this crate. The engine stores what is
                    // written, which is what lets the test read back where it landed.
                    .with_feature(
                        LocalFeature::new(1, FeatureType::LoadControl, Role::Server)
                            .with_function(
                                Function::LoadControlLimitDescriptionListData,
                                Operations::read(),
                            )
                            .with_function(
                                Function::LoadControlLimitListData,
                                Operations::read_write(),
                            ),
                    )
                    .with_feature(
                        LocalFeature::new(2, FeatureType::DeviceConfiguration, Role::Server)
                            .with_function(
                                Function::DeviceConfigurationKeyValueDescriptionListData,
                                Operations::read(),
                            )
                            .with_function(
                                Function::DeviceConfigurationKeyValueListData,
                                Operations::read_write(),
                            ),
                    )
                    .with_feature(limitation::device_diagnosis_feature(3)),
            )
            .unwrap();
        let load_control = device.address_of(&[1], 1);
        let configuration = device.address_of(&[1], 2);
        let mut them = Engine::new(device);
        them.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);

        // Its published state, in its own numbering.
        set(&mut them, &load_control, limits);
        set(
            &mut them,
            &load_control,
            CmdData::LoadControlLimitListData(their_current_limits()),
        );
        set(&mut them, &configuration, their_failsafe_descriptions());

        Self {
            guard_engine,
            guard,
            them,
            now,
            reports: Vec::new(),
        }
    }

    fn device(&self) -> AddressDevice {
        self.them.device().address().clone()
    }

    /// Discovery both ways, then whatever the guard makes of it.
    fn commission(&mut self) {
        let ours = node_management(self.guard_engine.device().address());
        let theirs = node_management(self.them.device().address());
        for function in [
            Function::NodeManagementDetailedDiscoveryData,
            Function::NodeManagementUseCaseData,
        ] {
            self.guard_engine
                .read(&theirs, &ours, function.clone(), self.now);
            self.them.read(&ours, &theirs, function, self.now);
        }
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..128 {
            let mut moved = false;
            while let Some(datagram) = self.guard_engine.poll_transmit() {
                self.them.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while let Some(datagram) = self.them.poll_transmit() {
                self.guard_engine.handle_datagram(&datagram, self.now);
                moved = true;
            }
            while self.them.poll_event().is_some() {
                moved = true;
            }
            let events: Vec<SpineEvent> =
                core::iter::from_fn(|| self.guard_engine.poll_event()).collect();
            for event in &events {
                if let Some(report) =
                    self.guard
                        .handle_event(&mut self.guard_engine, event, self.now)
                {
                    self.reports.push(report);
                }
                moved = true;
            }
            if !moved {
                return;
            }
        }
        panic!("the exchange did not settle");
    }

    fn advance(&mut self, by: Duration) {
        self.now += by;
        let reports = self.guard.handle_timeout(&mut self.guard_engine, self.now);
        self.reports.extend(reports);
        self.guard_engine.handle_timeout(self.now);
        self.them.handle_timeout(self.now);
        self.settle();
    }

    /// What the appliance now holds under each of its two limits.
    fn stored_limit(&self, id: LoadControlLimitId) -> Option<LoadControlLimitData> {
        let address = self.them.device().address_of(&[1], 1);
        let CmdData::LoadControlLimitListData(list) = self
            .them
            .device()
            .resolve(&address)?
            .data(&Function::LoadControlLimitListData)?
        else {
            return None;
        };
        list.load_control_limit_data
            .iter()
            .flatten()
            .find(|entry| entry.limit_id == Some(id))
            .cloned()
    }
}

fn set(engine: &mut Engine, address: &eebus::model::FeatureAddress, data: CmdData) {
    engine
        .device_mut()
        .resolve_mut(address)
        .expect("the feature")
        .set_data(data)
        .expect("publishable");
}

/// One `LoadControl` feature describing both directions, production first on `1`.
fn their_limit_descriptions() -> CmdData {
    CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
        load_control_limit_description_data: Some(vec![
            described(THEIR_PRODUCTION_LIMIT, EnergyDirection::Produce),
            described(THEIR_CONSUMPTION_LIMIT, EnergyDirection::Consume),
        ]),
    })
}

/// The same appliance with only the production half — an LPP device, to an LPC guard.
fn their_production_only() -> CmdData {
    CmdData::LoadControlLimitDescriptionListData(LoadControlLimitDescriptionListData {
        load_control_limit_description_data: Some(vec![described(
            THEIR_PRODUCTION_LIMIT,
            EnergyDirection::Produce,
        )]),
    })
}

fn described(
    id: LoadControlLimitId,
    direction: EnergyDirection,
) -> LoadControlLimitDescriptionData {
    LoadControlLimitDescriptionData {
        limit_id: Some(id),
        limit_type: Some(LoadControlLimitType::SignDependentAbsValueLimit),
        limit_category: Some(LoadControlCategory::Obligation),
        limit_direction: Some(direction),
        measurement_id: Some(MeasurementId(id.get())),
        unit: Some(UnitOfMeasurement::W),
        scope_type: Some(ScopeType::ActivePowerLimit),
        ..Default::default()
    }
}

/// Five configuration keys, the two this use case wants at the back.
fn their_failsafe_descriptions() -> CmdData {
    let mut keys: Vec<DeviceConfigurationKeyValueDescriptionData> = (1..=4)
        .map(|id| DeviceConfigurationKeyValueDescriptionData {
            key_id: Some(DeviceConfigurationKeyId(id)),
            key_name: Some(eebus::model::DeviceConfigurationKeyName::PeakPowerOfPvSystem),
            value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
            ..Default::default()
        })
        .collect();
    keys.push(DeviceConfigurationKeyValueDescriptionData {
        key_id: Some(THEIR_FAILSAFE_LIMIT),
        key_name: Some(lpc::DIRECTION.failsafe_limit_key()),
        value_type: Some(DeviceConfigurationKeyValueType::ScaledNumber),
        unit: Some(UnitOfMeasurement::W),
        ..Default::default()
    });
    keys.push(DeviceConfigurationKeyValueDescriptionData {
        key_id: Some(THEIR_FAILSAFE_DURATION),
        key_name: Some(limitation::FAILSAFE_DURATION_MINIMUM_KEY),
        value_type: Some(DeviceConfigurationKeyValueType::Duration),
        ..Default::default()
    });
    CmdData::DeviceConfigurationKeyValueDescriptionListData(
        DeviceConfigurationKeyValueDescriptionListData {
            device_configuration_key_value_description_data: Some(keys),
        },
    )
}

/// The guard reads the peer's numbering and writes the limit where the peer keeps it.
#[test]
fn a_limit_is_written_to_the_identifier_the_peer_published() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();

    let device = pair.device();
    let ids = pair.guard.peer_ids(&device).expect("the peer is tracked");
    assert_eq!(
        ids.limit,
        Some(THEIR_CONSUMPTION_LIMIT),
        "the guard addresses the limit whose direction matches its use case"
    );
    assert!(
        pair.guard.is_ready(&device),
        "both bindings and the identifier: the pre-scenario communication is done"
    );

    pair.guard
        .require(&device, Some(LimitWrite::active(4_200.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(20));

    let written = pair
        .stored_limit(THEIR_CONSUMPTION_LIMIT)
        .expect("the consumption limit was written");
    assert_eq!(
        written.value.as_ref().and_then(|v| v.to_f64()),
        Some(4_200.0)
    );
    assert_eq!(written.is_limit_active, Some(true));

    // And the production limit — `limitId` 1, where a guard that assumed its own
    // numbering would have put it — was never touched.
    assert!(
        pair.stored_limit(THEIR_PRODUCTION_LIMIT)
            .unwrap()
            .value
            .is_none(),
        "the production limit is not this guard's to write"
    );
}

/// The failsafe values go to the keys the peer named, not to key 1 and key 2.
#[test]
fn the_failsafe_values_are_written_to_the_keys_the_peer_named() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let device = pair.device();

    let ids = pair.guard.peer_ids(&device).expect("tracked");
    assert_eq!(ids.failsafe_limit, Some(THEIR_FAILSAFE_LIMIT));
    assert_eq!(ids.failsafe_duration, Some(THEIR_FAILSAFE_DURATION));

    pair.guard
        .write_failsafe_limit(&mut pair.guard_engine, &device, 4_200.0, pair.now)
        .expect("the key is known, so the write goes out");
    pair.settle();

    let address = pair.them.device().address_of(&[1], 2);
    let CmdData::DeviceConfigurationKeyValueListData(list) = pair
        .them
        .device()
        .resolve(&address)
        .expect("the feature")
        .data(&Function::DeviceConfigurationKeyValueListData)
        .expect("something was written")
    else {
        unreachable!()
    };
    let entries = list
        .device_configuration_key_value_data
        .as_deref()
        .unwrap_or_default();
    assert_eq!(entries.len(), 1, "one key was written, not several");
    assert_eq!(
        entries[0].key_id,
        Some(THEIR_FAILSAFE_LIMIT),
        "the failsafe limit went to the peer's own key, not to key 1"
    );
}

/// An appliance that publishes no limit for this direction is reported, not written to.
#[test]
fn a_peer_that_publishes_no_matching_limit_is_reported_and_left_alone() {
    let mut pair = Foreign::new(their_production_only());
    pair.commission();
    let device = pair.device();

    assert!(
        pair.reports.iter().any(|r| matches!(
            r,
            GuardEvent::LimitUnavailable {
                reason: LimitResolutionError::NoDescription,
                ..
            }
        )),
        "the one place this installation's failure is visible: {:?}",
        pair.reports
    );
    assert!(
        !pair.guard.is_ready(&device),
        "there is nothing to write to, so the guard is not ready"
    );

    pair.guard
        .require(&device, Some(LimitWrite::active(4_200.0)), pair.now)
        .expect("a valid limit");
    pair.advance(Duration::from_secs(60));

    assert!(
        pair.stored_limit(THEIR_PRODUCTION_LIMIT)
            .unwrap()
            .value
            .is_none(),
        "an LPC guard does not write into an LPP limit for want of anything better"
    );
    assert!(
        !pair
            .reports
            .iter()
            .any(|r| matches!(r, GuardEvent::LimitAccepted { .. })),
        "and nothing was acknowledged, so nothing goes in the §14a record"
    );
}

fn their_current_limits() -> LoadControlLimitListData {
    LoadControlLimitListData {
        load_control_limit_data: Some(
            [THEIR_CONSUMPTION_LIMIT, THEIR_PRODUCTION_LIMIT]
                .into_iter()
                .map(|id| LoadControlLimitData {
                    limit_id: Some(id),
                    is_limit_changeable: Some(true),
                    ..Default::default()
                })
                .collect(),
        ),
    }
}

// 018-limitation-commissioning/SR-021
#[test]
fn one_writable_limit_is_resolved_independently_of_list_order() {
    use LimitResolutionError::*;
    let CmdData::LoadControlLimitDescriptionListData(descriptions) = their_limit_descriptions()
    else {
        unreachable!()
    };
    let current = their_current_limits();
    let id = THEIR_PRODUCTION_LIMIT;
    let check = |descriptions: LoadControlLimitDescriptionListData,
                 current: LoadControlLimitListData,
                 expected| {
        for reverse_descriptions in [false, true] {
            for reverse_current in [false, true] {
                let mut descriptions = descriptions.clone();
                let mut current = current.clone();
                if reverse_descriptions {
                    descriptions
                        .load_control_limit_description_data
                        .as_mut()
                        .unwrap()
                        .reverse();
                }
                if reverse_current {
                    current.load_control_limit_data.as_mut().unwrap().reverse();
                }
                assert_eq!(
                    limitation::resolve_writable_limit(&descriptions, &current, lpp::DIRECTION),
                    expected
                );
            }
        }
    };
    check(descriptions.clone(), current.clone(), Ok(id));
    assert_eq!(
        limitation::resolve_writable_limit(&descriptions, &current, lpc::DIRECTION),
        Ok(THEIR_CONSUMPTION_LIMIT)
    );
    for (entries, expected) in [
        (vec![described(id, EnergyDirection::Consume)], NoDescription),
        (
            vec![
                described(id, EnergyDirection::Produce),
                described(LoadControlLimitId(14), EnergyDirection::Produce),
            ],
            AmbiguousDescriptions,
        ),
        (
            vec![
                described(id, EnergyDirection::Produce),
                described(id, EnergyDirection::Consume),
            ],
            ReusedId(id),
        ),
    ] {
        check(
            LoadControlLimitDescriptionListData {
                load_control_limit_description_data: Some(entries),
            },
            current.clone(),
            Err(expected),
        );
    }
    for (field_id, unit, expected) in [
        (None, Some(UnitOfMeasurement::W), MissingId),
        (Some(id), None, WrongUnit(id)),
        (Some(id), Some(UnitOfMeasurement::Wh), WrongUnit(id)),
    ] {
        let mut descriptions = descriptions.clone();
        let first = &mut descriptions
            .load_control_limit_description_data
            .as_mut()
            .unwrap()[0];
        first.limit_id = field_id;
        first.unit = unit;
        check(descriptions, current.clone(), Err(expected));
    }
    for (flags, expected) in [
        (vec![], MissingCurrent(id)),
        (vec![Some(false)], NotChangeable(id)),
        (vec![None], NotChangeable(id)),
        (vec![Some(false), Some(true)], AmbiguousCurrent(id)),
        (vec![Some(true), Some(true)], AmbiguousCurrent(id)),
    ] {
        let current = LoadControlLimitListData {
            load_control_limit_data: Some(
                flags
                    .into_iter()
                    .map(|flag| LoadControlLimitData {
                        limit_id: Some(id),
                        is_limit_changeable: flag,
                        ..Default::default()
                    })
                    .collect(),
            ),
        };
        check(descriptions.clone(), current, Err(expected));
    }
}

// 018-limitation-commissioning/SR-021
#[test]
fn invalid_current_data_stops_pending_writes_and_a_correction_recommissions_the_target() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let device = pair.device();
    let target = pair.them.device().address_of(&[1], 1);
    pair.advance(Duration::from_secs(11));
    pair.guard
        .require(&device, Some(LimitWrite::active(4000.0)), pair.now)
        .unwrap();
    pair.guard.handle_timeout(&mut pair.guard_engine, pair.now);
    // Take the in-flight write off the wire but keep its ACK back until after revocation.
    let write = core::iter::from_fn(|| pair.guard_engine.poll_transmit())
        .find(|d| {
            d.header.as_ref().unwrap().cmd_classifier == Some(eebus::model::CmdClassifier::Write)
        })
        .unwrap();
    let counter = write.header.as_ref().unwrap().msg_counter.unwrap();
    let mut current = their_current_limits();
    current.load_control_limit_data.as_mut().unwrap()[0].is_limit_changeable = Some(false);
    set(
        &mut pair.them,
        &target,
        CmdData::LoadControlLimitListData(current.clone()),
    );
    pair.them
        .notify(&target, &Function::LoadControlLimitListData, pair.now);
    pair.settle();
    assert!(!pair.guard.is_ready(&device));
    assert_eq!(pair.guard.peer_ids(&device).unwrap().limit, None);
    assert!(pair.reports.iter().any(|event| matches!(
        event,
        GuardEvent::LimitUnavailable {
            reason: LimitResolutionError::NotChangeable(THEIR_CONSUMPTION_LIMIT),
            ..
        }
    )));
    pair.advance(Duration::from_secs(300));
    pair.advance(Duration::from_secs(300));
    assert_ne!(
        pair.stored_limit(THEIR_CONSUMPTION_LIMIT)
            .unwrap()
            .is_limit_active,
        Some(true),
        "the transport must not retry a revoked write"
    );
    let new = LoadControlLimitId(81);
    current
        .load_control_limit_data
        .as_mut()
        .unwrap()
        .push(LoadControlLimitData {
            limit_id: Some(new),
            is_limit_changeable: Some(true),
            ..Default::default()
        });
    set(
        &mut pair.them,
        &target,
        CmdData::LoadControlLimitListData(current),
    );
    pair.them
        .notify(&target, &Function::LoadControlLimitListData, pair.now);
    pair.settle();
    assert!(!pair.guard.is_ready(&device));
    let CmdData::LoadControlLimitDescriptionListData(mut descriptions) = their_limit_descriptions()
    else {
        unreachable!()
    };
    descriptions
        .load_control_limit_description_data
        .as_mut()
        .unwrap()[1]
        .limit_id = Some(new);
    set(
        &mut pair.them,
        &target,
        CmdData::LoadControlLimitDescriptionListData(descriptions),
    );
    pair.them.notify(
        &target,
        &Function::LoadControlLimitDescriptionListData,
        pair.now,
    );
    pair.settle();
    assert!(pair.guard.is_ready(&device));
    assert_eq!(pair.guard.peer_ids(&device).unwrap().limit, Some(new));
    assert_eq!(
        pair.stored_limit(new).unwrap().value.unwrap().to_f64(),
        Some(4000.0)
    );
    let late_ack = SpineEvent::ResultReceived {
        request: counter,
        error: eebus::spine::ErrorNumber::None,
    };
    assert_eq!(
        pair.guard
            .handle_event(&mut pair.guard_engine, &late_ack, pair.now),
        None
    );
}

// 018-limitation-commissioning/SR-021
#[test]
fn device_discovery_revokes_an_ambiguous_target_and_retains_the_requirement() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let device = pair.device();
    assert!(pair.guard.is_ready(&device));
    pair.guard
        .require(&device, Some(LimitWrite::active(4000.0)), pair.now)
        .unwrap();

    pair.them.add_use_case([2], 1, &lpc::CONTROLLABLE_SYSTEM);
    pair.commission();
    assert!(!pair.guard.is_ready(&device));
    assert_eq!(
        pair.guard.deferred_requirements().next(),
        Some((&device, Some(LimitWrite::active(4000.0))))
    );
    let mut remote = pair.guard_engine.peer(&device).unwrap().clone();
    for _ in 0..2 {
        assert!(limitation::locate(&remote, lpc::DIRECTION).is_none());
        remote.use_cases.reverse();
    }
}

// 018-limitation-commissioning/SR-021
#[test]
fn a_partial_list_after_cache_eviction_needs_a_new_full_read() {
    use eebus::model::Filter;
    use eebus::spine::MAX_REMOTE_FUNCTIONS;

    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let device = pair.device();
    let load = pair.them.device().address_of(&[1], 1);
    let client = pair.guard_engine.device().address_of(&[1], 1);
    let current = Function::LoadControlLimitListData;
    let descriptions = Function::LoadControlLimitDescriptionListData;
    for entity_id in 2..2 + MAX_REMOTE_FUNCTIONS as u32 {
        let entity = LocalEntity::new([entity_id], EntityType::HeatPumpAppliance)
            .with_feature(limitation::load_control_feature(1));
        pair.them.device_mut().add_entity(entity).unwrap();
        let address = pair.them.device().address_of(&[entity_id], 1);
        set(
            &mut pair.them,
            &address,
            CmdData::LoadControlLimitListData(their_current_limits()),
        );
        pair.guard_engine
            .read(&address, &client, current.clone(), pair.now);
        pair.settle();
    }
    assert!(
        pair.guard_engine
            .remote_data(&load, &descriptions)
            .is_none()
    );

    // The appliance reports a complete current list and a description fragment.
    // Hold its answer to the guard's new read until the fragment has been processed.
    pair.them.notify(&load, &current, pair.now);
    pair.them.notify(&load, &descriptions, pair.now);
    let full = pair.them.poll_transmit().unwrap();
    let mut fragment = pair.them.poll_transmit().unwrap();
    fragment.payload.as_mut().unwrap().cmd.as_mut().unwrap()[0].filter =
        Some(vec![Filter::partial()]);
    for message in [full, fragment] {
        pair.guard_engine.handle_datagram(&message, pair.now);
        while let Some(event) = pair.guard_engine.poll_event() {
            pair.guard
                .handle_event(&mut pair.guard_engine, &event, pair.now);
        }
    }
    assert!(!pair.guard.is_ready(&device));
    assert_eq!(pair.guard.peer_ids(&device).unwrap().limit, None);
    pair.settle();
    assert!(
        pair.guard.is_ready(&device),
        "the guard must fetch a fresh baseline"
    );
}

// 018-limitation-commissioning/SR-021
#[test]
fn reattachment_needs_fresh_complete_lists_from_the_selected_feature() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let device = pair.device();
    let peer = pair.guard.peers().next().unwrap().clone();
    let load = peer.load_control.clone();
    let elsewhere = peer.device_configuration.clone();
    pair.guard.attach(&mut pair.guard_engine, peer, pair.now);
    for (target, partial, data) in [
        (elsewhere, false, their_limit_descriptions()),
        (load.clone(), true, their_limit_descriptions()),
        (
            load,
            false,
            CmdData::LoadControlLimitListData(their_current_limits()),
        ),
    ] {
        let event = SpineEvent::DataNotified {
            feature: target,
            is_partial: partial,
            resolved: data.clone(),
            data,
        };
        pair.guard
            .handle_event(&mut pair.guard_engine, &event, pair.now);
    }
    assert_eq!(pair.guard.peer_ids(&device).unwrap().limit, None);
    pair.settle();
    assert!(pair.guard.is_ready(&device));
}

// 018-limitation-commissioning/SR-021
#[test]
fn discovery_requires_a_single_write_capable_load_control_feature() {
    let mut pair = Foreign::new(their_limit_descriptions());
    pair.commission();
    let mut remote = pair.guard_engine.peer(&pair.device()).unwrap().clone();
    let entity = remote
        .entities
        .iter_mut()
        .find(|entity| entity.address == [1])
        .unwrap();
    let load = entity
        .features
        .iter_mut()
        .find(|f| f.feature_type == FeatureType::LoadControl)
        .unwrap();
    load.functions
        .retain(|property| property.function != Some(Function::LoadControlLimitListData));
    assert!(limitation::locate(&remote, lpc::DIRECTION).is_none());

    let mut remote = pair.guard_engine.peer(&pair.device()).unwrap().clone();
    let entity = remote
        .entities
        .iter_mut()
        .find(|entity| entity.address == [1])
        .unwrap();
    let mut second = entity
        .feature(&FeatureType::LoadControl, Role::Server)
        .unwrap()
        .clone();
    second.address.feature = Some(eebus::model::AddressFeature(12));
    entity.features.push(second);
    assert!(limitation::locate(&remote, lpc::DIRECTION).is_none());
}
