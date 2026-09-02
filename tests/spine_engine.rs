//! Two SPINE engines holding a complete conversation.
//!
//! Discovery, binding, subscription, a partial write and its acknowledgement, run
//! against a virtual clock. Test names carry the identifier of the case in
//! `EEBus_SPINE_TestSpecification_V1.0.0` they cover.

use core::time::Duration;

use eebus::model::{
    CmdClassifier, CmdData, Datagram, DeviceType, EntityType, FeatureAddress, FeatureType,
    Function, LoadControlLimitData, LoadControlLimitId, LoadControlLimitListData, MsgCounter,
    Payload, ScaledNumber,
};
use eebus::spine::{
    Engine, ErrorNumber, LocalDevice, LocalEntity, LocalFeature, Operations, SpineEvent,
    node_management,
};
use eebus::usecases::lpc;

/// A heat pump: the Controllable System of LPC.
fn heat_pump() -> Engine {
    let mut device =
        LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
    let appliance = LocalEntity::new([1], EntityType::HeatPumpAppliance)
        .with_feature(
            LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Server)
                .with_function(
                    Function::LoadControlLimitDescriptionListData,
                    Operations::read(),
                )
                .with_function(Function::LoadControlLimitListData, Operations::read_write()),
        )
        .with_feature(
            LocalFeature::new(2, FeatureType::DeviceDiagnosis, eebus::model::Role::Server)
                .with_function(Function::DeviceDiagnosisHeartbeatData, Operations::read()),
        );
    device.add_entity(appliance).unwrap();

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
    engine
}

/// A sub-meter: a Monitored Unit publishing measurements.
fn sub_meter() -> Engine {
    let mut device =
        LocalDevice::new("i:67890", "SubMeter-1", DeviceType::ElectricitySupplySystem).unwrap();
    let meter = LocalEntity::new([1], EntityType::SubMeterElectricity).with_feature(
        LocalFeature::new(1, FeatureType::Measurement, eebus::model::Role::Server)
            .with_function(Function::MeasurementListData, Operations::read()),
    );
    device.add_entity(meter).unwrap();
    Engine::new(device)
}

/// A control box: the Energy Guard of LPC.
fn control_box() -> Engine {
    let mut device = LocalDevice::new(
        "i:12345",
        "ControlBox-1",
        DeviceType::ElectricitySupplySystem,
    )
    .unwrap();
    let guard = LocalEntity::new([1], EntityType::GridGuard).with_feature(LocalFeature::new(
        1,
        FeatureType::Generic,
        eebus::model::Role::Client,
    ));
    device.add_entity(guard).unwrap();

    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::ENERGY_GUARD);
    engine
}

/// Delivers everything both engines have queued, encoding and decoding on the way so
/// that the wire format is exercised too.
fn pump(a: &mut Engine, b: &mut Engine, now: Duration) {
    for _ in 0..32 {
        let mut moved = false;
        while let Some(datagram) = a.poll_transmit() {
            b.handle_datagram(&round_trip(&datagram), now);
            moved = true;
        }
        while let Some(datagram) = b.poll_transmit() {
            a.handle_datagram(&round_trip(&datagram), now);
            moved = true;
        }
        if !moved {
            return;
        }
    }
    panic!("the exchange did not settle");
}

fn round_trip(datagram: &Datagram) -> Datagram {
    let wire = eebus::model::to_json(datagram).expect("encode");
    let decoded = eebus::model::from_json_str(&wire).expect("decode");
    assert_eq!(&decoded, datagram, "the datagram survives the wire");
    decoded
}

fn events(engine: &mut Engine) -> Vec<SpineEvent> {
    core::iter::from_fn(|| engine.poll_event()).collect()
}

/// `TC_SPINE_DDISC_001`: a node discovers a partner it knows nothing about, and finds
/// its entities, features and use cases.
#[test]
fn tc_spine_ddisc_001_discovery_of_an_unknown_partner() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let heat_pump_address = pump_device.device().address().clone();
    let guard_nm = node_management(guard.device().address());
    let pump_nm = node_management(&heat_pump_address);

    guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );
    guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementUseCaseData,
        now,
    );
    pump(&mut guard, &mut pump_device, now);

    let remote = guard.peer(&heat_pump_address).expect("the peer");
    assert_eq!(remote.entities.len(), 2, "entity 0 and the appliance");

    let appliance = remote.entity(&[1]).expect("the appliance");
    assert_eq!(appliance.entity_type, Some(EntityType::HeatPumpAppliance));

    let load_control = appliance
        .feature(&FeatureType::LoadControl, eebus::model::Role::Server)
        .expect("load control");
    assert!(load_control.is_writeable(&Function::LoadControlLimitListData));

    let use_case = remote
        .use_case("limitationOfPowerConsumption", "ControllableSystem")
        .expect("the use case");
    assert!(use_case.supports_scenario(1));

    // And the events say so, which is what a use case reacts to.
    let seen = events(&mut guard);
    assert!(
        seen.iter()
            .any(|e| matches!(e, SpineEvent::DiscoveryUpdated { .. }))
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, SpineEvent::UseCasesUpdated { .. }))
    );
}

/// `TC_SPINE_BIND_002`: a write without a binding is refused with `errorNumber` 9, and
/// the stored data is untouched.
#[test]
fn tc_spine_bind_002_an_unbound_write_is_refused() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);

    guard.write(&target, &source, limit(3_000.0), true, now);
    pump(&mut guard, &mut pump_device, now);

    let result = events(&mut guard)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
        .expect("a result came back");
    assert_eq!(result, ErrorNumber::BindingRequired);

    let feature = pump_device.device().resolve(&target).unwrap();
    assert!(
        feature.data(&Function::LoadControlLimitListData).is_none(),
        "nothing was written"
    );
}

/// The full sequence: bind, subscribe, then write. This is the pre-scenario
/// communication of LPC §3.3 followed by scenario 1.
#[test]
fn a_bound_write_is_applied_and_acknowledged() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);

    guard.request_binding(&source, &target, now);
    guard.request_subscription(&source, &target, now);
    pump(&mut guard, &mut pump_device, now);

    assert!(
        pump_device.relations().is_bound(&source, &target),
        "the heat pump granted the binding"
    );
    assert!(pump_device.relations().is_subscribed(&source, &target));

    let _ = events(&mut guard);
    guard.write(&target, &source, limit(3_000.0), true, now);
    pump(&mut guard, &mut pump_device, now);

    // The heat pump applied it and told its use case.
    let written = events(&mut pump_device)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::DataWritten { function, .. } => Some(function),
            _ => None,
        })
        .expect("the write was reported");
    assert_eq!(written, Function::LoadControlLimitListData);

    let stored = pump_device
        .device()
        .resolve(&target)
        .unwrap()
        .data(&Function::LoadControlLimitListData)
        .expect("the limit was stored");
    let CmdData::LoadControlLimitListData(list) = stored else {
        panic!("expected the limit list");
    };
    assert_eq!(
        list.load_control_limit_data.as_ref().unwrap()[0]
            .value
            .as_ref()
            .unwrap()
            .to_f64(),
        Some(3_000.0)
    );

    // And the control box got its acknowledgement.
    let result = events(&mut guard)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
        .expect("an acknowledgement came back");
    assert_eq!(result, ErrorNumber::None);
}

/// A partial write merges into what is stored rather than replacing it — the rule the
/// SPINE implementation guide §3.3 devotes a table to, here over the wire.
#[test]
fn a_partial_write_over_the_wire_keeps_untouched_elements() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &target, now);
    pump(&mut guard, &mut pump_device, now);

    guard.write(&target, &source, limit(4_200.0), true, now);
    pump(&mut guard, &mut pump_device, now);

    // A second write that carries only the activation flag.
    let update = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(1)),
            is_limit_active: Some(false),
            ..Default::default()
        }]),
    });
    guard.write(&target, &source, update, true, now);
    pump(&mut guard, &mut pump_device, now);

    let CmdData::LoadControlLimitListData(list) = pump_device
        .device()
        .resolve(&target)
        .unwrap()
        .data(&Function::LoadControlLimitListData)
        .unwrap()
    else {
        panic!("expected the limit list");
    };
    let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
    assert_eq!(entry.is_limit_active, Some(false), "the update applied");
    assert_eq!(
        entry.value.as_ref().unwrap().to_f64(),
        Some(4_200.0),
        "the value survived the partial write"
    );
}

/// A subscriber is notified when the server's data changes.
#[test]
fn subscribers_are_notified_of_a_change() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 2);
    let source = guard.device().address_of(&[1], 1);
    guard.request_subscription(&source, &target, now);
    pump(&mut guard, &mut pump_device, now);
    let _ = events(&mut guard);

    pump_device
        .device_mut()
        .resolve_mut(&target)
        .unwrap()
        .set_data(CmdData::DeviceDiagnosisHeartbeatData(
            eebus::model::DeviceDiagnosisHeartbeatData {
                heartbeat_counter: Some(7),
                heartbeat_timeout: Some("PT1M".into()),
                ..Default::default()
            },
        ))
        .unwrap();
    pump_device.notify(&target, &Function::DeviceDiagnosisHeartbeatData, now);
    pump(&mut guard, &mut pump_device, now);

    let notified = events(&mut guard)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::DataNotified { data, .. } => Some(data),
            _ => None,
        })
        .expect("the notification arrived");
    let CmdData::DeviceDiagnosisHeartbeatData(heartbeat) = notified else {
        panic!("expected a heartbeat");
    };
    assert_eq!(heartbeat.heartbeat_counter, Some(7));
}

/// `TC_SPINE_FC_001`: a binding or subscription addressed anywhere but the primary
/// NodeManagement instance is refused.
#[test]
fn tc_spine_fc_001_a_binding_to_an_unknown_feature_is_refused() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let nowhere = pump_device.device().address_of(&[9], 9);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &nowhere, now);
    pump(&mut guard, &mut pump_device, now);

    let result = events(&mut guard)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::ResultReceived { error, .. } => Some(error),
            _ => None,
        })
        .expect("a result came back");
    assert_eq!(result, ErrorNumber::DestinationUnknown);
}

/// SPINE implementation guide §2.1: a datagram whose header is incomplete is discarded
/// without a response, because there is nowhere reliable to send one.
#[test]
fn ig_2_1_an_incomplete_header_is_discarded_silently() {
    let mut pump_device = heat_pump();
    let now = Duration::ZERO;

    for datagram in [
        Datagram::default(),
        Datagram {
            header: Some(eebus::model::Header::default()),
            payload: Some(Payload::default()),
        },
        Datagram {
            header: Some(eebus::model::Header {
                msg_counter: Some(MsgCounter(1)),
                // No classifier, so nothing can be done with it.
                ..Default::default()
            }),
            payload: Some(Payload::default()),
        },
    ] {
        assert!(!pump_device.handle_datagram(&datagram, now));
        assert!(
            pump_device.poll_transmit().is_none(),
            "and no answer is sent"
        );
    }
}

/// `TC_SPINE_DATA_007`: `ackRequest` on a read is ignored — the reply is the answer.
#[test]
fn tc_spine_data_007_a_read_is_answered_by_its_reply_alone() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let guard_nm = node_management(guard.device().address());
    let pump_nm = node_management(pump_device.device().address());
    let request = guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );

    let datagram = guard.poll_transmit().expect("the read");
    assert_eq!(
        datagram.header.as_ref().unwrap().ack_request,
        None,
        "a read does not ask for one"
    );
    pump_device.handle_datagram(&datagram, now);

    let answers: Vec<Datagram> = core::iter::from_fn(|| pump_device.poll_transmit()).collect();
    assert_eq!(answers.len(), 1, "exactly one message comes back");
    let header = answers[0].header.as_ref().unwrap();
    assert_eq!(header.cmd_classifier, Some(CmdClassifier::Reply));
    assert_eq!(
        header.msg_counter_reference,
        Some(request),
        "the reply names the read it answers"
    );
}

/// A partial read is answered with what the filter asked for, and nothing else.
///
/// SPINE §5.3.4.4: selectors name the entries, elements name the parts of them. The
/// reply says it is partial (§5.3.4.5) and keeps every identifier, because that is how
/// the client tells the entries apart.
#[test]
fn a_partial_read_returns_only_the_selected_entries() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 1);
    pump_device
        .device_mut()
        .resolve_mut(&target)
        .unwrap()
        .set_data(CmdData::LoadControlLimitListData(
            LoadControlLimitListData {
                load_control_limit_data: Some(vec![
                    LoadControlLimitData {
                        limit_id: Some(LoadControlLimitId(1)),
                        is_limit_active: Some(true),
                        value: Some(ScaledNumber::new(4_200, 0)),
                        ..Default::default()
                    },
                    LoadControlLimitData {
                        limit_id: Some(LoadControlLimitId(2)),
                        is_limit_active: Some(false),
                        value: Some(ScaledNumber::new(11_000, 0)),
                        ..Default::default()
                    },
                ]),
            },
        ))
        .unwrap();

    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);

    let filter = eebus::model::Filter::partial().select(
        eebus::model::FilterSelectors::LoadControlLimitListDataSelectors(
            eebus::model::LoadControlLimitListDataSelectors {
                limit_id: Some(LoadControlLimitId(2)),
            },
        ),
    );
    pump_device.handle_datagram(
        &read_with(&source, &target, Function::LoadControlLimitListData, filter),
        now,
    );

    let answer = pump_device.poll_transmit().expect("a reply");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    assert!(
        cmd.filter
            .iter()
            .flatten()
            .any(eebus::model::Filter::is_partial),
        "the reply announces itself as partial"
    );
    let Some(CmdData::LoadControlLimitListData(list)) = &cmd.data else {
        panic!("expected the limit list, got {:?}", cmd.data);
    };
    let entries = list.load_control_limit_data.as_ref().unwrap();
    assert_eq!(entries.len(), 1, "only the selected entry comes back");
    assert_eq!(entries[0].limit_id, Some(LoadControlLimitId(2)));
    assert_eq!(entries[0].value, Some(ScaledNumber::new(11_000, 0)));
}

/// An elements filter narrows what comes back, but never the identifiers: §5.3.4.5 says
/// they are full in a reply even when the read did not ask for them.
#[test]
fn an_elements_filter_keeps_the_identifiers() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let target = pump_device.device().address_of(&[1], 1);
    pump_device
        .device_mut()
        .resolve_mut(&target)
        .unwrap()
        .set_data(CmdData::LoadControlLimitListData(
            LoadControlLimitListData {
                load_control_limit_data: Some(vec![LoadControlLimitData {
                    limit_id: Some(LoadControlLimitId(1)),
                    is_limit_active: Some(true),
                    is_limit_changeable: Some(true),
                    value: Some(ScaledNumber::new(4_200, 0)),
                    ..Default::default()
                }]),
            },
        ))
        .unwrap();

    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);
    let filter = eebus::model::Filter::partial().covering(
        eebus::model::FilterElements::LoadControlLimitDataElements(
            eebus::model::LoadControlLimitDataElements {
                value: Some(Default::default()),
                ..Default::default()
            },
        ),
    );
    pump_device.handle_datagram(
        &read_with(&source, &target, Function::LoadControlLimitListData, filter),
        now,
    );

    let answer = pump_device.poll_transmit().expect("a reply");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::LoadControlLimitListData(list)) = &cmd.data else {
        panic!("expected the limit list");
    };
    let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
    assert_eq!(
        entry.limit_id,
        Some(LoadControlLimitId(1)),
        "the identifier survives"
    );
    assert_eq!(entry.value, Some(ScaledNumber::new(4_200, 0)));
    assert_eq!(
        entry.is_limit_active, None,
        "elements not asked for are dropped"
    );
    assert_eq!(entry.is_limit_changeable, None);
}

/// A selector this implementation cannot match by comparison — an interval, say — is
/// refused with `errorNumber` 8 rather than served as though it were absent.
///
/// Serving it would return entries the client explicitly excluded, which is a worse
/// answer than no answer: §5.3.4.9 provides number 8 for exactly this.
#[test]
fn an_unsupported_selector_is_refused() {
    let now = Duration::ZERO;
    let mut meter = sub_meter();
    let target = meter.device().address_of(&[1], 1);
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);

    let filter = eebus::model::Filter::partial().select(
        eebus::model::FilterSelectors::MeasurementListDataSelectors(
            eebus::model::MeasurementListDataSelectors {
                timestamp_interval: Some(Default::default()),
                ..Default::default()
            },
        ),
    );
    meter.handle_datagram(
        &read_with(&source, &target, Function::MeasurementListData, filter),
        now,
    );

    let answer = meter.poll_transmit().expect("an answer");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::ResultData(result)) = &cmd.data else {
        panic!("expected a result, got {:?}", cmd.data);
    };
    assert_eq!(
        result.error_number,
        Some(ErrorNumber::RestrictedExchangeNotSupported)
    );
}

/// A filter naming another function's selectors is the same kind of refusal.
#[test]
fn a_filter_for_another_function_is_refused() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let target = pump_device.device().address_of(&[1], 1);
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);

    let filter = eebus::model::Filter::partial().select(
        eebus::model::FilterSelectors::MeasurementListDataSelectors(Default::default()),
    );
    pump_device.handle_datagram(
        &read_with(&source, &target, Function::LoadControlLimitListData, filter),
        now,
    );

    let answer = pump_device.poll_transmit().expect("an answer");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::ResultData(result)) = &cmd.data else {
        panic!("expected a result, got {:?}", cmd.data);
    };
    assert_eq!(
        result.error_number,
        Some(ErrorNumber::RestrictedExchangeNotSupported)
    );
}

/// A read of a function that exists but holds nothing is answered with an empty payload.
///
/// Silence would leave the peer waiting out its ten-second response deadline for a
/// question that has a perfectly good answer.
#[test]
fn a_read_of_an_empty_function_is_still_answered() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let target = pump_device.device().address_of(&[1], 1);
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);

    pump_device.handle_datagram(
        &Datagram {
            header: Some(eebus::model::Header {
                msg_counter: Some(MsgCounter(1)),
                cmd_classifier: Some(CmdClassifier::Read),
                address_source: Some(source),
                address_destination: Some(target),
                ..Default::default()
            }),
            payload: Some(Payload {
                cmd: Some(vec![eebus::model::Cmd::read(
                    Function::LoadControlLimitListData,
                )]),
            }),
        },
        now,
    );

    let answer = pump_device.poll_transmit().expect("a reply");
    assert_eq!(
        answer.header.as_ref().unwrap().cmd_classifier,
        Some(CmdClassifier::Reply)
    );
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    assert!(matches!(
        cmd.data,
        Some(CmdData::LoadControlLimitListData(_))
    ));
}

/// A datagram addressed to some other device is not ours to serve: enhanced-mode
/// routing is not implemented, so §5.2.5.2's number 5 is the truthful answer.
#[test]
fn a_datagram_for_another_device_is_reported_unreachable() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);
    let elsewhere = eebus::spine::feature_address(
        &eebus::spine::device_address("i:99999", "SomeoneElse").unwrap(),
        &[1],
        1,
    );

    let accepted = pump_device.handle_datagram(
        &Datagram {
            header: Some(eebus::model::Header {
                msg_counter: Some(MsgCounter(1)),
                cmd_classifier: Some(CmdClassifier::Write),
                address_source: Some(source),
                address_destination: Some(elsewhere),
                ack_request: Some(true),
                ..Default::default()
            }),
            payload: Some(Payload {
                cmd: Some(vec![eebus::model::Cmd::with_data(
                    CmdData::LoadControlLimitListData(LoadControlLimitListData::default()),
                )]),
            }),
        },
        now,
    );

    assert!(!accepted);
    let answer = pump_device.poll_transmit().expect("a result");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::ResultData(result)) = &cmd.data else {
        panic!("expected a result");
    };
    assert_eq!(
        result.error_number,
        Some(ErrorNumber::DestinationUnreachable)
    );
}

/// A read of `function` carrying `filter`.
fn read_with(
    source: &FeatureAddress,
    destination: &FeatureAddress,
    function: Function,
    filter: eebus::model::Filter,
) -> Datagram {
    Datagram {
        header: Some(eebus::model::Header {
            msg_counter: Some(MsgCounter(1)),
            cmd_classifier: Some(CmdClassifier::Read),
            address_source: Some(source.clone()),
            address_destination: Some(destination.clone()),
            ack_request: Some(true),
            ..Default::default()
        }),
        payload: Some(Payload {
            cmd: Some(vec![eebus::model::Cmd::read(function).with_filter(filter)]),
        }),
    }
}

/// A request that goes unanswered expires, and the implementation guide §2.6.1 wants
/// that told apart from a refusal: only the first calls for a retry.
#[test]
fn an_unanswered_request_times_out() {
    let mut guard = control_box();
    let now = Duration::ZERO;

    let pump_nm = node_management(&heat_pump().device().address().clone());
    let guard_nm = node_management(guard.device().address());
    let request = guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );

    assert_eq!(
        guard.poll_timeout(),
        Some(Duration::from_secs(10)),
        "the default maximum response delay"
    );
    guard.handle_timeout(Duration::from_secs(10));

    assert_eq!(
        events(&mut guard).into_iter().find_map(|e| match e {
            SpineEvent::RequestTimedOut { request, .. } => Some(request),
            _ => None,
        }),
        Some(request)
    );
    assert_eq!(guard.poll_timeout(), None, "and it is not waited on twice");
}

/// A duplicate datagram is dropped rather than applied twice, which for a write would
/// mean applying the same limit change again.
#[test]
fn a_duplicate_datagram_is_not_processed_twice() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let target = pump_device.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &target, now);
    pump(&mut guard, &mut pump_device, now);

    guard.write(&target, &source, limit(3_000.0), true, now);
    let datagram = guard.poll_transmit().expect("the write");

    assert!(pump_device.handle_datagram(&datagram, now));
    assert!(
        !pump_device.handle_datagram(&datagram, now),
        "the same counter is not processed twice"
    );
}

fn limit(watts: f64) -> CmdData {
    CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(1)),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::from_f64(watts, 0)),
            ..Default::default()
        }]),
    })
}

/// The engine addresses every message with the device part, which the SPINE
/// implementation guide §2.7 requires in both directions.
#[test]
fn ig_2_7_every_address_carries_the_device_part() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let pump_device = heat_pump();

    let pump_nm = node_management(pump_device.device().address());
    let guard_nm = node_management(guard.device().address());
    guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementDetailedDiscoveryData,
        now,
    );

    let datagram = guard.poll_transmit().unwrap();
    let header = datagram.header.as_ref().unwrap();
    for address in [&header.address_source, &header.address_destination] {
        let address: &FeatureAddress = address.as_ref().unwrap();
        assert!(address.device.is_some(), "the device part is populated");
        assert!(address.entity.is_some());
        assert!(address.feature.is_some());
    }
}

/// Partial detailed discovery (§7.1.3): a client that only wants to know where one
/// feature lives asks for it by type, and gets that instead of the whole device tree.
#[test]
fn partial_detailed_discovery_returns_only_the_matching_features() {
    use eebus::model::{
        NodeManagementDetailedDiscoveryDataSelectors,
        NodeManagementDetailedDiscoveryDataSelectorsFeatureInformation,
    };

    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);
    let target = node_management(pump_device.device().address());

    let filter = eebus::model::Filter::partial().select(
        eebus::model::FilterSelectors::NodeManagementDetailedDiscoveryDataSelectors(
            NodeManagementDetailedDiscoveryDataSelectors {
                feature_information: Some(
                    NodeManagementDetailedDiscoveryDataSelectorsFeatureInformation {
                        feature_type: Some(FeatureType::LoadControl),
                        ..Default::default()
                    },
                ),
                ..Default::default()
            },
        ),
    );
    pump_device.handle_datagram(
        &read_with(
            &source,
            &target,
            Function::NodeManagementDetailedDiscoveryData,
            filter,
        ),
        now,
    );

    let answer = pump_device.poll_transmit().expect("a reply");
    let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::NodeManagementDetailedDiscoveryData(discovery)) = &cmd.data else {
        panic!("expected discovery data, got {:?}", cmd.data);
    };
    let features = discovery.feature_information.as_ref().unwrap();
    assert_eq!(features.len(), 1, "only the LoadControl feature comes back");
    assert_eq!(
        features[0]
            .description
            .as_ref()
            .unwrap()
            .feature_type
            .as_ref(),
        Some(&FeatureType::LoadControl)
    );
    assert!(
        discovery.device_information.is_some(),
        "the client still needs to know whose feature it is"
    );
}

/// A peer's write is a change, and a subscriber asked to be told about changes.
///
/// The use-case implementation guide §3.2.2 makes subscriptions the primary mechanism
/// and polling the fallback, which only works if a write actually produces a notify.
#[test]
fn a_write_notifies_the_subscribers_of_the_feature() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let mut guard = control_box();

    let client = guard.device().address_of(&[1], 1);
    let server = pump_device.device().address_of(&[1], 1);
    let watcher = eebus::spine::feature_address(
        &eebus::spine::device_address("i:11111", "Display").unwrap(),
        &[1],
        1,
    );
    pump_device.insert_binding(&client, &server);
    pump_device.insert_subscription(&watcher, &server);

    guard.write(
        &server,
        &client,
        CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_active: Some(true),
                value: Some(ScaledNumber::new(3_000, 0)),
                ..Default::default()
            }]),
        }),
        true,
        now,
    );
    let write = guard.poll_transmit().expect("the write");
    pump_device.handle_datagram(&write, now);

    let out: Vec<Datagram> = core::iter::from_fn(|| pump_device.poll_transmit()).collect();
    let notify = out
        .iter()
        .find(|d| {
            d.header.as_ref().unwrap().cmd_classifier == Some(CmdClassifier::Notify)
                && d.header.as_ref().unwrap().address_destination.as_ref() == Some(&watcher)
        })
        .expect("the subscriber was told");
    let cmd = &notify.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::LoadControlLimitListData(list)) = &cmd.data else {
        panic!("expected the limit list");
    };
    assert_eq!(
        list.load_control_limit_data.as_ref().unwrap()[0].value,
        Some(ScaledNumber::new(3_000, 0))
    );
}

/// `TC_SPINE_COMP_006`: a header whose `specificationVersion` breaks the implementation
/// guide's format rule is refused with an application error rather than acted on.
#[test]
fn tc_spine_comp_006_a_malformed_version_is_refused() {
    let now = Duration::ZERO;
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);

    for version in ["TS1.3.0", "V0.3.0", "v0.3.0", "0.3.0", "2.0.0"] {
        let mut pump_device = heat_pump();
        let target = node_management(pump_device.device().address());
        let accepted = pump_device.handle_datagram(
            &Datagram {
                header: Some(eebus::model::Header {
                    specification_version: Some(eebus::model::SpecificationVersion::from(version)),
                    msg_counter: Some(MsgCounter(1)),
                    cmd_classifier: Some(CmdClassifier::Read),
                    address_source: Some(source.clone()),
                    address_destination: Some(target),
                    ..Default::default()
                }),
                payload: Some(Payload {
                    cmd: Some(vec![eebus::model::Cmd::read(
                        Function::NodeManagementDetailedDiscoveryData,
                    )]),
                }),
            },
            now,
        );

        assert!(!accepted, "{version:?} was processed");
        let answer = pump_device.poll_transmit().expect("an answer");
        let cmd = &answer.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
        let Some(CmdData::ResultData(result)) = &cmd.data else {
            panic!("expected a result for {version:?}, got {:?}", cmd.data);
        };
        assert_eq!(
            result.error_number,
            Some(ErrorNumber::General),
            "{version:?}"
        );
    }
}

/// `TC_SPINE_COMP_002`: a peer one minor version ahead is served normally.
#[test]
fn tc_spine_comp_002_a_newer_minor_version_is_served() {
    let now = Duration::ZERO;
    let mut pump_device = heat_pump();
    let guard = control_box();
    let source = guard.device().address_of(&[1], 1);
    let target = node_management(pump_device.device().address());

    pump_device.handle_datagram(
        &Datagram {
            header: Some(eebus::model::Header {
                specification_version: Some(eebus::model::SpecificationVersion::from("1.4.0")),
                msg_counter: Some(MsgCounter(1)),
                cmd_classifier: Some(CmdClassifier::Read),
                address_source: Some(source),
                address_destination: Some(target),
                ..Default::default()
            }),
            payload: Some(Payload {
                cmd: Some(vec![eebus::model::Cmd::read(
                    Function::NodeManagementDetailedDiscoveryData,
                )]),
            }),
        },
        now,
    );

    let answer = pump_device.poll_transmit().expect("a reply");
    assert_eq!(
        answer.header.as_ref().unwrap().cmd_classifier,
        Some(CmdClassifier::Reply)
    );
}

/// §7.3.2 and §7.4.2: a node that announces its binding and subscription tables as
/// readable answers with the relations it actually holds.
///
/// An empty list — what a declared but never-stored function gives — would tell a peer
/// that the binding it is relying on does not exist.
#[test]
fn the_binding_and_subscription_tables_report_what_is_held() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let guard_nm = node_management(guard.device().address());
    let pump_nm = node_management(pump_device.device().address());
    let guard_client = guard.device().address_of(&[1], 1);
    let load_control = pump_device.device().address_of(&[1], 1);

    // The control box binds to and subscribes to the heat pump's LoadControl.
    guard.request_binding(&guard_client, &load_control, now);
    guard.request_subscription(&guard_client, &load_control, now);
    pump(&mut guard, &mut pump_device, now);
    assert_eq!(pump_device.relations().bindings().len(), 1);
    assert_eq!(pump_device.relations().subscriptions().len(), 1);

    // Now it reads the two tables back.
    for function in [
        Function::NodeManagementBindingData,
        Function::NodeManagementSubscriptionData,
    ] {
        guard.read(&pump_nm, &guard_nm, function.clone(), now);
    }
    pump(&mut guard, &mut pump_device, now);

    let replies: Vec<CmdData> = events(&mut guard)
        .into_iter()
        .filter_map(|event| match event {
            SpineEvent::ReplyReceived { data, .. } => Some(data),
            _ => None,
        })
        .collect();

    let bindings = replies
        .iter()
        .find_map(|data| match data {
            CmdData::NodeManagementBindingData(list) => Some(list),
            _ => None,
        })
        .expect("the binding table was served");
    let entry = &bindings.binding_entry.as_ref().expect("entries")[0];
    assert_eq!(entry.client_address.as_ref(), Some(&guard_client));
    assert_eq!(entry.server_address.as_ref(), Some(&load_control));
    assert!(
        entry.binding_id.is_some(),
        "the server's identifier is named"
    );

    let subscriptions = replies
        .iter()
        .find_map(|data| match data {
            CmdData::NodeManagementSubscriptionData(list) => Some(list),
            _ => None,
        })
        .expect("the subscription table was served");
    let entry = &subscriptions.subscription_entry.as_ref().expect("entries")[0];
    assert_eq!(entry.client_address.as_ref(), Some(&guard_client));
    assert_eq!(entry.server_address.as_ref(), Some(&load_control));
    assert!(entry.subscription_id.is_some());
}

/// A NodeManagement function this node does not serve is refused, not answered empty:
/// `possibleOperations` lists four, and a read of anything else is `errorNumber` 6.
#[test]
fn an_unserved_node_management_function_is_not_answered_with_an_empty_payload() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let guard_nm = node_management(guard.device().address());
    let pump_nm = node_management(pump_device.device().address());
    guard.read(
        &pump_nm,
        &guard_nm,
        Function::NodeManagementDestinationListData,
        now,
    );
    pump(&mut guard, &mut pump_device, now);

    let seen = events(&mut guard);
    assert!(
        seen.iter().any(|event| matches!(
            event,
            SpineEvent::ResultReceived {
                error: ErrorNumber::CommandNotSupported,
                ..
            }
        )),
        "expected errorNumber 6, saw {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, SpineEvent::ReplyReceived { .. })),
        "and no reply pretending the function is served"
    );
}

// ---- resource limits -------------------------------------------------------------
//
// A deferred write is memory the peer allocates and the application frees. Both ends of
// that arrangement have to be bounded, or a peer that writes faster than an application
// answers is a peer that can exhaust a heat-pump controller from the LAN.

/// A heat pump whose limit writes wait for the application, as LPC requires.
fn deciding_heat_pump() -> Engine {
    let mut device =
        LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
    let appliance = LocalEntity::new([1], EntityType::HeatPumpAppliance).with_feature(
        LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Server)
            .with_function(Function::LoadControlLimitListData, Operations::read_write())
            .with_deferred_writes(),
    );
    device.add_entity(appliance).unwrap();
    let mut engine = Engine::new(device);
    engine.add_use_case([1], 1, &lpc::CONTROLLABLE_SYSTEM);
    engine
}

/// Binds the guard to the pump's LoadControl and returns the two addresses.
fn bound_pair(
    guard: &mut Engine,
    cs: &mut Engine,
    now: Duration,
) -> (FeatureAddress, FeatureAddress) {
    let target = cs.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &target, now);
    pump(guard, cs, now);
    assert!(cs.relations().is_bound(&source, &target), "binding granted");
    let _ = events(guard);
    let _ = events(cs);
    (target, source)
}

/// A peer that writes faster than the application answers is told it is being turned
/// away, rather than costing the device a kilobyte per write.
///
/// `errorNumber` 3 is what §5.2.5 defines for exactly this: the recipient is overloaded.
#[test]
fn a_flood_of_undecided_writes_is_refused_rather_than_queued() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = deciding_heat_pump();
    let (target, source) = bound_pair(&mut guard, &mut pump_device, now);

    let mut deferred = 0usize;
    let mut overloaded = 0usize;
    for _ in 0..(eebus::spine::MAX_DEFERRED_WRITES * 3) {
        guard.write(&target, &source, limit(3_000.0), true, now);
        let datagram = guard.poll_transmit().expect("the write");
        pump_device.handle_datagram(&round_trip(&datagram), now);

        // The application deliberately never decides.
        let requested = events(&mut pump_device)
            .iter()
            .any(|e| matches!(e, SpineEvent::WriteRequested(_)));
        if requested {
            deferred += 1;
        }
        while let Some(answer) = pump_device.poll_transmit() {
            let json = eebus::model::to_json(&answer).expect("encode");
            if json.contains("\"errorNumber\":3") {
                overloaded += 1;
            }
            guard.handle_datagram(&answer, now);
        }
    }

    assert_eq!(
        deferred,
        eebus::spine::MAX_DEFERRED_WRITES,
        "the queue grew past its cap"
    );
    assert!(
        overloaded > 0,
        "a refused write must say why: `errorNumber` 3, overload"
    );
}

/// A write the application never decides is abandoned once the peer has stopped waiting.
///
/// Ten seconds is §5.2.5's maximum response delay. An answer after that arrives at a peer
/// that gave up, so the entry buys nothing — and under §14a a limit that was never
/// decided is worth an event rather than a silent disappearance.
#[test]
fn an_undecided_write_is_abandoned_once_the_peer_has_given_up() {
    let mut now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = deciding_heat_pump();
    let (target, source) = bound_pair(&mut guard, &mut pump_device, now);

    guard.write(&target, &source, limit(3_000.0), true, now);
    let datagram = guard.poll_transmit().expect("the write");
    pump_device.handle_datagram(&round_trip(&datagram), now);

    let token = events(&mut pump_device)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::WriteRequested(w) => Some(w.token),
            _ => None,
        })
        .expect("the write was put to the application");

    now += Duration::from_secs(30);
    pump_device.handle_timeout(now);

    let abandoned = events(&mut pump_device)
        .into_iter()
        .any(|e| matches!(e, SpineEvent::WriteAbandoned { token: t, .. } if t == token));
    assert!(abandoned, "the abandoned write was not reported");

    // And the queue is empty again, so the next write is served.
    guard.write(&target, &source, limit(4_200.0), true, now);
    let datagram = guard.poll_transmit().expect("the second write");
    pump_device.handle_datagram(&round_trip(&datagram), now);
    assert!(
        events(&mut pump_device)
            .iter()
            .any(|e| matches!(e, SpineEvent::WriteRequested(_))),
        "the slot was not released"
    );
}

/// A peer with a binding cannot grow a stored list one message at a time.
///
/// A partial write appends any entry whose identifier matches nothing stored, which is
/// correct — and, repeated with a fresh identifier each time, a way to spend a
/// controller's memory over a legitimate protocol flow. Nothing in SPINE caps it.
#[test]
fn a_stored_list_cannot_be_grown_without_bound() {
    use eebus::model::MeasurementId;

    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut meter = sub_meter_writeable();
    let (target, source) = bound_pair(&mut guard, &mut meter, now);

    let mut refusals = 0usize;
    for id in 0..(eebus::spine::MAX_LIST_ENTRIES + 8) {
        let entry = eebus::model::MeasurementData {
            measurement_id: Some(MeasurementId(id as u32)),
            value: Some(ScaledNumber::new(id as i64, 0)),
            ..Default::default()
        };
        let update = CmdData::MeasurementListData(eebus::model::MeasurementListData {
            measurement_data: Some(vec![entry]),
        });
        guard.write(&target, &source, update, true, now);
        let datagram = guard.poll_transmit().expect("the write");
        meter.handle_datagram(&round_trip(&datagram), now);
        while let Some(answer) = meter.poll_transmit() {
            if eebus::model::to_json(&answer)
                .expect("encode")
                .contains("\"errorNumber\":3")
            {
                refusals += 1;
            }
            guard.handle_datagram(&answer, now);
        }
        let _ = events(&mut meter);
        let _ = events(&mut guard);
    }

    assert!(
        refusals >= 8,
        "the list grew past its cap: {refusals} refusals"
    );

    let stored = meter
        .device()
        .resolve(&target)
        .unwrap()
        .data(&Function::MeasurementListData)
        .expect("the list");
    let CmdData::MeasurementListData(list) = stored else {
        panic!("expected the measurement list");
    };
    assert_eq!(
        list.measurement_data.as_ref().unwrap().len(),
        eebus::spine::MAX_LIST_ENTRIES,
        "the cap is the cap"
    );
}

/// A sub-meter whose measurement list a bound peer may write.
fn sub_meter_writeable() -> Engine {
    let mut device =
        LocalDevice::new("i:67890", "SubMeter-2", DeviceType::ElectricitySupplySystem).unwrap();
    let meter = LocalEntity::new([1], EntityType::SubMeterElectricity).with_feature(
        LocalFeature::new(1, FeatureType::Measurement, eebus::model::Role::Server)
            .with_function(Function::MeasurementListData, Operations::read_write()),
    );
    device.add_entity(meter).unwrap();
    Engine::new(device)
}

// ---- Restricted Function Exchange on a write --------------------------------------

/// LPC UC TS §3.4.1.4: one command that deletes a limit's `endTime` and writes its new
/// value, and both halves happen.
///
/// A write command may carry several filters (SPINE §5.3.4.2), and this is the shape the
/// use case is built on: the delete withdraws the duration, the partial update supplies
/// the value, and the Controllable System never sees the limit without one or the other.
/// Reading the command as "a delete" and stopping there removes the limit outright — a
/// curtailment lifted rather than made open-ended, which under §14a EnWG is the failure
/// that matters.
#[test]
fn lpc_3_4_1_4_a_delete_of_one_element_and_a_write_in_one_command() {
    use eebus::model::{
        Filter, FilterElements, FilterSelectors, LoadControlLimitDataElements,
        LoadControlLimitListDataSelectors, TimePeriod, TimePeriodElements,
    };

    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();
    let target = pump_device.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &target, now);
    pump(&mut guard, &mut pump_device, now);

    // A limit that runs until eleven o'clock.
    let stored = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(1)),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::from_f64(4_200.0, 0)),
            time_period: Some(TimePeriod {
                end_time: Some("2026-08-30T11:00:00Z".into()),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    });
    pump_device
        .device_mut()
        .resolve_mut(&target)
        .expect("the LoadControl feature")
        .set_data(stored)
        .expect("publish the limit");

    // Withdraw the end time, and lower the limit, in one command.
    let delete = Filter::delete()
        .select(FilterSelectors::LoadControlLimitListDataSelectors(
            LoadControlLimitListDataSelectors {
                limit_id: Some(LoadControlLimitId(1)),
            },
        ))
        .covering(FilterElements::LoadControlLimitDataElements(
            LoadControlLimitDataElements {
                time_period: Some(TimePeriodElements {
                    end_time: Some(eebus::codec::ElementTag),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ));
    let cmd = eebus::model::Cmd::with_data(limit(3_000.0))
        .with_filter(delete)
        .with_filter(Filter::partial());
    let counter = MsgCounter(4_242);
    let datagram = Datagram {
        header: Some(write_header(&source, &target, counter)),
        payload: Some(Payload {
            cmd: Some(vec![cmd]),
        }),
    };

    assert!(pump_device.handle_datagram(&round_trip(&datagram), now));

    let Some(CmdData::LoadControlLimitListData(list)) = pump_device
        .device()
        .resolve(&target)
        .and_then(|f| f.data(&Function::LoadControlLimitListData))
    else {
        panic!("the limit is gone entirely");
    };
    let entries = list.load_control_limit_data.as_ref().expect("entries");
    assert_eq!(entries.len(), 1, "the entry survived the delete");
    let entry = &entries[0];
    assert_eq!(entry.limit_id, Some(LoadControlLimitId(1)), "identity kept");
    assert_eq!(
        entry.value.as_ref().and_then(ScaledNumber::to_f64),
        Some(3_000.0),
        "the partial update in the same command was applied"
    );
    assert_eq!(entry.is_limit_active, Some(true), "still active");
    assert!(
        entry
            .time_period
            .as_ref()
            .is_none_or(|p| p.end_time.is_none()),
        "the end time was withdrawn"
    );

    let answered = pump_device.poll_transmit().expect("an acknowledgement");
    let json = eebus::model::to_json(&answered).expect("encode");
    assert!(
        json.contains("\"errorNumber\":0"),
        "the command was accepted: {json}"
    );
}

/// A `write` header addressed from `source` to `destination`.
fn write_header(
    source: &FeatureAddress,
    destination: &FeatureAddress,
    counter: MsgCounter,
) -> eebus::model::Header {
    eebus::model::Header {
        specification_version: Some("1.3.0".into()),
        address_source: Some(source.clone()),
        address_destination: Some(destination.clone()),
        msg_counter: Some(counter),
        cmd_classifier: Some(CmdClassifier::Write),
        ack_request: Some(true),
        ..Default::default()
    }
}

/// A write the application has not decided is a deadline the caller can wait on.
///
/// `handle_timeout` abandons an undecided write once §5.2.5's maximum response delay has
/// passed, but a caller only calls it when `poll_timeout` says to. A device that merely
/// *serves* writes has no request of its own outstanding, so leaving the deferred writes
/// out of the answer reported "nothing to wait for" — after which nothing expired, and
/// the queue `MAX_DEFERRED_WRITES` bounds filled up and stayed full.
#[test]
fn an_undecided_write_is_a_deadline_the_caller_is_told_about() {
    let mut now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = deciding_heat_pump();
    let (target, source) = bound_pair(&mut guard, &mut pump_device, now);
    assert_eq!(
        pump_device.poll_timeout(),
        None,
        "a device that only serves has nothing outstanding of its own"
    );

    guard.write(&target, &source, limit(3_000.0), true, now);
    let datagram = guard.poll_transmit().expect("the write");
    pump_device.handle_datagram(&round_trip(&datagram), now);
    assert!(
        events(&mut pump_device)
            .iter()
            .any(|e| matches!(e, SpineEvent::WriteRequested(_))),
        "the write was put to the application"
    );

    let deadline = pump_device
        .poll_timeout()
        .expect("the undecided write is a deadline");

    // Waiting on that deadline — which is all a `Hub`-shaped loop does — is enough.
    now = deadline;
    pump_device.handle_timeout(now);
    assert!(
        events(&mut pump_device)
            .iter()
            .any(|e| matches!(e, SpineEvent::WriteAbandoned { .. })),
        "the write was never abandoned"
    );
    assert_eq!(pump_device.poll_timeout(), None, "and nothing is left over");
}

/// A peer that goes away takes its undecided writes with it.
///
/// The answer had nowhere to go the moment the connection did, so holding the slot spends
/// one of `MAX_DEFERRED_WRITES` on a peer that is gone. The application is told rather
/// than left holding a token that will never resolve.
#[test]
fn a_disconnected_peers_undecided_writes_are_released() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = deciding_heat_pump();
    let (target, source) = bound_pair(&mut guard, &mut pump_device, now);

    guard.write(&target, &source, limit(3_000.0), true, now);
    let datagram = guard.poll_transmit().expect("the write");
    pump_device.handle_datagram(&round_trip(&datagram), now);
    let _ = events(&mut pump_device);

    pump_device.remove_peer(guard.device().address());

    assert!(
        events(&mut pump_device)
            .iter()
            .any(|e| matches!(e, SpineEvent::WriteAbandoned { .. })),
        "the application was not told the decision is no longer wanted"
    );
    assert_eq!(pump_device.poll_timeout(), None, "and the slot is free");
    let _ = source;
}

/// §7.1.3: NodeManagement's own data is computed, and a subscriber to it can be notified.
///
/// A peer may subscribe to NodeManagement to hear about a change to the entity list
/// rather than polling for it (§7.4.2). The subscription was granted and the notification
/// could never be built: `notify` read only *stored* data, and NodeManagement stores
/// none of its four functions.
#[test]
fn a_subscriber_to_node_management_is_notified_of_the_computed_data() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();

    let pump_nm = node_management(pump_device.device().address());
    let guard_nm = node_management(guard.device().address());
    guard.request_subscription(&guard_nm, &pump_nm, now);
    pump(&mut guard, &mut pump_device, now);
    assert!(
        pump_device
            .relations()
            .subscriptions()
            .iter()
            .any(|s| s.server.feature == pump_nm.feature),
        "the subscription was granted"
    );
    let _ = events(&mut guard);

    pump_device.notify(
        &pump_nm,
        &Function::NodeManagementDetailedDiscoveryData,
        now,
    );
    let notification = pump_device
        .poll_transmit()
        .expect("a notification for the subscriber");
    guard.handle_datagram(&round_trip(&notification), now);

    assert!(
        events(&mut guard).iter().any(|e| matches!(
            e,
            SpineEvent::DataNotified {
                data: CmdData::NodeManagementDetailedDiscoveryData(_),
                ..
            }
        )),
        "the discovery data never reached the subscriber"
    );
}

/// A result from one peer does not answer a request sent to another.
///
/// `msgCounter` is allocated per engine, so the same number means different things to
/// different peers. Matching a response on the counter alone lets any device on the LAN
/// claim an answer that belongs to somebody else's conversation — and under §14a EnWG the
/// counter and the answer that names it *are* the record that a limit was applied.
#[test]
fn a_result_from_the_wrong_peer_does_not_answer_the_request() {
    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();
    let mut meter = sub_meter();

    // Two peers, so the guard knows both device addresses.
    pump(&mut guard, &mut pump_device, now);
    let pump_nm = node_management(pump_device.device().address());
    let meter_nm = node_management(meter.device().address());
    let guard_nm = node_management(guard.device().address());
    guard.discover(pump_device.device().address(), now);
    pump(&mut guard, &mut pump_device, now);
    guard.discover(meter.device().address(), now);
    pump(&mut guard, &mut meter, now);
    let _ = events(&mut guard);

    // A read addressed to the heat pump, left unanswered.
    let counter = guard.read(&pump_nm, &guard_nm, Function::LoadControlLimitListData, now);
    let _ = guard.poll_transmit().expect("the read goes out");

    // The sub-meter answers it instead.
    let impostor = Datagram {
        header: Some(eebus::model::Header {
            specification_version: Some("1.3.0".into()),
            address_source: Some(meter_nm.clone()),
            address_destination: Some(guard_nm.clone()),
            msg_counter: Some(MsgCounter(9_001)),
            msg_counter_reference: Some(counter),
            cmd_classifier: Some(CmdClassifier::Result),
            ..Default::default()
        }),
        payload: Some(Payload {
            cmd: Some(vec![eebus::model::Cmd::with_data(CmdData::ResultData(
                eebus::model::ResultData {
                    error_number: Some(ErrorNumber::None),
                    description: None,
                },
            ))]),
        }),
    };
    guard.handle_datagram(&round_trip(&impostor), now);

    assert!(
        !events(&mut guard)
            .iter()
            .any(|e| matches!(e, SpineEvent::ResultReceived { .. })),
        "another peer's result was filed against this request"
    );

    // And the request is still outstanding, so it times out honestly.
    let deadline = guard.poll_timeout().expect("the read is still pending");
    guard.handle_timeout(deadline);
    assert!(
        events(&mut guard).iter().any(
            |e| matches!(e, SpineEvent::RequestTimedOut { request, .. } if *request == counter)
        ),
        "the unanswered read was never reported"
    );
    let _ = pump_nm;
}

// ---- what a client is told a notification means -----------------------------------

/// A partial notification is merged before the application sees it.
///
/// SPINE IG §3.2.2 asks a client to subscribe rather than poll, and a server is free to
/// notify a *partial* update — the specification's own `RFE_N-*` examples do. An omitted
/// element then means *unchanged* (§3.3), so a measurement notified as a bare `number`
/// with its `scale` left out is off by whatever the stored scale was: 30 W where 3 kW was
/// meant. That is the write path's most consequential rule, pointed the other way, and it
/// is why the event carries `resolved` as well as `data`.
#[test]
fn a_partial_notification_is_merged_into_what_the_peer_sent_before() {
    use eebus::model::{
        Filter, MeasurementData, MeasurementId, MeasurementListData, Number, Scale,
    };

    let now = Duration::ZERO;
    let mut manager = control_box();
    let meter_device = eebus::spine::device_address("i:67890", "SubMeter-1").expect("an address");
    let meter = eebus::spine::feature_address(&meter_device, &[1], 1);
    let client = manager.device().address_of(&[1], 1);

    let reading = |number: i64, scale: Option<i16>| {
        CmdData::MeasurementListData(MeasurementListData {
            measurement_data: Some(vec![MeasurementData {
                measurement_id: Some(MeasurementId(1)),
                value: Some(ScaledNumber {
                    number: Some(Number(number)),
                    scale: scale.map(Scale),
                }),
                ..Default::default()
            }]),
        })
    };
    let notify = |data: CmdData, counter: u64, partial: bool| {
        let mut cmd = eebus::model::Cmd::with_data(data);
        if partial {
            cmd = cmd.with_filter(Filter::partial());
        }
        Datagram {
            header: Some(eebus::model::Header {
                specification_version: Some("1.3.0".into()),
                address_source: Some(meter.clone()),
                address_destination: Some(client.clone()),
                msg_counter: Some(MsgCounter(counter)),
                cmd_classifier: Some(CmdClassifier::Notify),
                ..Default::default()
            }),
            payload: Some(Payload {
                cmd: Some(vec![cmd]),
            }),
        }
    };

    // 3 kW, in full: 3 × 10³.
    manager.handle_datagram(&round_trip(&notify(reading(3, Some(3)), 1, false)), now);
    let _ = events(&mut manager);

    // The next reading carries only the mantissa, which is what a partial update is for.
    manager.handle_datagram(&round_trip(&notify(reading(4, None), 2, true)), now);

    let (data, resolved) = events(&mut manager)
        .into_iter()
        .find_map(|e| match e {
            SpineEvent::DataNotified { data, resolved, .. } => Some((data, resolved)),
            _ => None,
        })
        .expect("the notification");

    let watts = |data: &CmdData| match data {
        CmdData::MeasurementListData(list) => list.measurement_data.as_ref().unwrap()[0]
            .value
            .as_ref()
            .and_then(ScaledNumber::to_f64),
        _ => None,
    };
    assert_eq!(watts(&data), Some(4.0), "the fragment alone says 4 W");
    assert_eq!(
        watts(&resolved),
        Some(4_000.0),
        "and it means 4 kW, because the scale it omitted is unchanged"
    );

    // And the engine will say so again without the application having kept the event.
    assert_eq!(
        manager
            .remote_data(&meter, &Function::MeasurementListData)
            .and_then(watts),
        Some(4_000.0)
    );
}

/// A `delete` in a notification removes what it addresses from the merged state.
#[test]
fn a_notified_delete_narrows_the_merged_state() {
    use eebus::model::{
        Filter, FilterElements, FilterSelectors, LoadControlLimitDataElements,
        LoadControlLimitListDataSelectors, TimePeriod, TimePeriodElements,
    };

    let now = Duration::ZERO;
    let mut manager = control_box();
    let pump_device = eebus::spine::device_address("i:67890", "HeatPump-1").expect("an address");
    let pump = eebus::spine::feature_address(&pump_device, &[1], 1);
    let client = manager.device().address_of(&[1], 1);

    let send = |cmd: eebus::model::Cmd, counter: u64| Datagram {
        header: Some(eebus::model::Header {
            specification_version: Some("1.3.0".into()),
            address_source: Some(pump.clone()),
            address_destination: Some(client.clone()),
            msg_counter: Some(MsgCounter(counter)),
            cmd_classifier: Some(CmdClassifier::Notify),
            ..Default::default()
        }),
        payload: Some(Payload {
            cmd: Some(vec![cmd]),
        }),
    };

    let full = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(1)),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::from_f64(4_200.0, 0)),
            time_period: Some(TimePeriod {
                end_time: Some("PT2H".into()),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    });
    manager.handle_datagram(
        &round_trip(&send(eebus::model::Cmd::with_data(full), 1)),
        now,
    );
    let _ = events(&mut manager);

    let withdraw = eebus::model::Cmd::with_data(CmdData::LoadControlLimitListData(
        LoadControlLimitListData::default(),
    ))
    .with_filter(
        Filter::delete()
            .select(FilterSelectors::LoadControlLimitListDataSelectors(
                LoadControlLimitListDataSelectors {
                    limit_id: Some(LoadControlLimitId(1)),
                },
            ))
            .covering(FilterElements::LoadControlLimitDataElements(
                LoadControlLimitDataElements {
                    time_period: Some(TimePeriodElements {
                        end_time: Some(eebus::codec::ElementTag),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
    );
    manager.handle_datagram(&round_trip(&send(withdraw, 2)), now);
    let _ = events(&mut manager);

    let Some(CmdData::LoadControlLimitListData(list)) =
        manager.remote_data(&pump, &Function::LoadControlLimitListData)
    else {
        panic!("the limit is gone entirely");
    };
    let entry = &list.load_control_limit_data.as_ref().expect("entries")[0];
    assert_eq!(entry.is_limit_active, Some(true), "still limited");
    assert_eq!(
        entry.value.as_ref().and_then(ScaledNumber::to_f64),
        Some(4_200.0),
        "at the same value"
    );
    assert!(
        entry
            .time_period
            .as_ref()
            .is_none_or(|p| p.end_time.is_none()),
        "and now without an end"
    );
}

/// A partial detailed-discovery notification does not erase what it does not mention.
///
/// §7.1.5 has a peer re-send its discovery document when its entities or features change,
/// and nothing stops that re-send being partial — the specification gives the function
/// selectors precisely so that it can be. Applied as though it were the whole document, a
/// notification carrying only `featureInformation` sets the device type, the entity list
/// and the SPINE version list to nothing: the peer is still connected and this device now
/// believes it has no entities.
#[test]
fn a_partial_discovery_notification_keeps_what_it_does_not_mention() {
    use eebus::model::{
        Filter, NodeManagementDetailedDiscoveryData,
        NodeManagementDetailedDiscoveryFeatureInformation,
        NodeManagementDetailedDiscoveryFeatureInformationDescription,
        NodeManagementDetailedDiscoveryFeatureInformationDescriptionFeatureAddress,
    };

    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut pump_device = heat_pump();
    let pump_address = pump_device.device().address().clone();

    // The whole document first, which is how a connection opens.
    guard.discover(&pump_address, now);
    pump(&mut guard, &mut pump_device, now);
    let before = guard.peer(&pump_address).expect("the peer").clone();
    assert_eq!(before.entities.len(), 2, "entity 0 and the appliance");
    assert!(before.device_type.is_some());
    assert!(!before.spine_versions.is_empty());
    let _ = events(&mut guard);

    // Then a partial notification that mentions one feature and nothing else.
    let fragment =
        CmdData::NodeManagementDetailedDiscoveryData(NodeManagementDetailedDiscoveryData {
            feature_information: Some(vec![NodeManagementDetailedDiscoveryFeatureInformation {
                description: Some(NodeManagementDetailedDiscoveryFeatureInformationDescription {
                    feature_address: Some(
                        NodeManagementDetailedDiscoveryFeatureInformationDescriptionFeatureAddress {
                            entity: Some(vec![eebus::model::AddressEntity(1)]),
                            feature: Some(eebus::model::AddressFeature(9)),
                        },
                    ),
                    feature_type: Some(FeatureType::Measurement),
                    role: Some(eebus::model::Role::Server),
                    ..Default::default()
                }),
            }]),
            ..Default::default()
        });
    let notification = Datagram {
        header: Some(eebus::model::Header {
            specification_version: Some("1.3.0".into()),
            address_source: Some(node_management(&pump_address)),
            address_destination: Some(node_management(guard.device().address())),
            msg_counter: Some(MsgCounter(9_100)),
            cmd_classifier: Some(CmdClassifier::Notify),
            ..Default::default()
        }),
        payload: Some(Payload {
            cmd: Some(vec![
                eebus::model::Cmd::with_data(fragment).with_filter(Filter::partial()),
            ]),
        }),
    };
    guard.handle_datagram(&round_trip(&notification), now);

    let after = guard.peer(&pump_address).expect("the peer still");
    assert_eq!(
        after.device_type, before.device_type,
        "the device type was not mentioned and must survive"
    );
    assert_eq!(
        after.spine_versions, before.spine_versions,
        "nor was the version list"
    );
    assert_eq!(
        after.entities.len(),
        2,
        "and the entity list is still the peer's, not the fragment's"
    );
    assert!(
        after.entity(&[1]).is_some_and(|e| e
            .feature(&FeatureType::Measurement, eebus::model::Role::Server)
            .is_some()),
        "while the feature the fragment did carry arrived"
    );
}

/// A feature that announced whole-function writes only does not serve a delete.
///
/// `possibleOperations` carries one `partial` flag for the whole of §5.3.4, so a feature
/// declaring `write` without it is saying it exchanges whole functions. Serving a delete
/// on it anyway would be doing something never announced — D14's rule pointed the other
/// way, and the answer is the same `errorNumber` 8.
#[test]
fn a_delete_on_a_whole_function_only_feature_is_refused() {
    use eebus::model::Filter;

    let now = Duration::ZERO;
    let mut guard = control_box();
    let mut device =
        LocalDevice::new("i:67890", "Plain-1", DeviceType::HeatGenerationSystem).unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance).with_feature(
                LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Server)
                    .with_function(
                        Function::LoadControlLimitListData,
                        Operations::read_write_full(),
                    ),
            ),
        )
        .unwrap();
    let mut plain = Engine::new(device);
    let target = plain.device().address_of(&[1], 1);
    let source = guard.device().address_of(&[1], 1);
    guard.request_binding(&source, &target, now);
    pump(&mut guard, &mut plain, now);

    let cmd = eebus::model::Cmd::with_data(limit(3_000.0)).with_filter(Filter::delete());
    let datagram = Datagram {
        header: Some(write_header(&source, &target, MsgCounter(7_007))),
        payload: Some(Payload {
            cmd: Some(vec![cmd]),
        }),
    };
    plain.handle_datagram(&round_trip(&datagram), now);

    let answer = eebus::model::to_json(&plain.poll_transmit().expect("an answer")).expect("encode");
    assert!(
        answer.contains("\"errorNumber\":8"),
        "expected `restricted exchange not supported`, saw {answer}"
    );

    // And a full write on the same feature is still served, so the refusal is about the
    // restricted exchange and not about the feature.
    let full = Datagram {
        header: Some(write_header(&source, &target, MsgCounter(7_008))),
        payload: Some(Payload {
            cmd: Some(vec![eebus::model::Cmd::with_data(limit(3_000.0))]),
        }),
    };
    plain.handle_datagram(&round_trip(&full), now);
    let answer = eebus::model::to_json(&plain.poll_transmit().expect("an answer")).expect("encode");
    assert!(
        answer.contains("\"errorNumber\":0"),
        "a full write must still work: {answer}"
    );
}

// ---- how long a peer is given to answer -------------------------------------------

/// §5.2.5.3: a feature that announces a longer `maxResponseDelay` is given it.
///
/// The default is ten seconds, and a client that assumes it reports a conformant peer as
/// unresponsive. The implementation guide §2.6.1 separates a timeout from a refusal
/// precisely because §2.6.2's staggered retry follows the first and not the second — so
/// guessing does not merely mislabel the peer, it retries against one that is answering as
/// fast as it said it would.
#[test]
fn a_feature_that_announced_a_longer_delay_is_given_it() {
    let now = Duration::ZERO;
    let mut guard = control_box();

    // A heat pump whose LoadControl says it may take a minute — an appliance that has to
    // ask a compressor controller before it can answer.
    let mut device =
        LocalDevice::new("i:67890", "SlowPump-1", DeviceType::HeatGenerationSystem).unwrap();
    device
        .add_entity(
            LocalEntity::new([1], EntityType::HeatPumpAppliance).with_feature(
                LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Server)
                    .with_function(Function::LoadControlLimitListData, Operations::read_write())
                    .with_deferred_writes()
                    .with_max_response_delay(Duration::from_secs(60)),
            ),
        )
        .unwrap();
    let mut slow = Engine::new(device);
    let pump_address = slow.device().address().clone();

    guard.discover(&pump_address, now);
    pump(&mut guard, &mut slow, now);
    let _ = events(&mut guard);

    // The guard read it off discovery.
    let target = eebus::spine::feature_address(&pump_address, &[1], 1);
    let remote = guard.peer(&pump_address).expect("the peer");
    assert_eq!(
        remote
            .feature_at(&target)
            .and_then(|f| f.max_response_delay),
        Some(Duration::from_secs(60)),
        "the announcement did not survive discovery"
    );

    // So a request to it is not called timed out after the default ten seconds.
    let source = guard.device().address_of(&[1], 1);
    let counter = guard.read(&target, &source, Function::LoadControlLimitListData, now);
    let _ = guard.poll_transmit();
    guard.handle_timeout(now + Duration::from_secs(30));
    assert!(
        !events(&mut guard)
            .iter()
            .any(|e| matches!(e, SpineEvent::RequestTimedOut { .. })),
        "the peer was called unresponsive while it was still within its own deadline"
    );

    guard.handle_timeout(now + Duration::from_secs(61));
    assert!(
        events(&mut guard).iter().any(
            |e| matches!(e, SpineEvent::RequestTimedOut { request, .. } if *request == counter)
        ),
        "and past it, the timeout still fires"
    );
}

/// The same figure holds a deferred write open: the peer waits, so the engine does too.
///
/// A write the application has not decided is abandoned once the peer has stopped waiting.
/// How long that is, is what the feature announced — so a Controllable System that said it
/// may take a minute is not abandoned after ten seconds, which would leave the Energy Guard
/// with no answer at all and, under §14a EnWG, no record that the limit was ever decided.
#[test]
fn a_slow_feature_keeps_its_undecided_writes_as_long_as_it_announced() {
    let now = Duration::ZERO;

    let deciding = |delay: Option<Duration>| {
        let mut device =
            LocalDevice::new("i:67890", "HeatPump-1", DeviceType::HeatGenerationSystem).unwrap();
        let mut feature =
            LocalFeature::new(1, FeatureType::LoadControl, eebus::model::Role::Server)
                .with_function(Function::LoadControlLimitListData, Operations::read_write())
                .with_deferred_writes();
        if let Some(delay) = delay {
            feature = feature.with_max_response_delay(delay);
        }
        device
            .add_entity(LocalEntity::new([1], EntityType::HeatPumpAppliance).with_feature(feature))
            .unwrap();
        Engine::new(device)
    };

    for (announced, expected) in [
        (None, Duration::from_secs(10)),
        (Some(Duration::from_secs(60)), Duration::from_secs(60)),
    ] {
        let mut guard = control_box();
        let mut pump_device = deciding(announced);
        let (target, source) = bound_pair(&mut guard, &mut pump_device, now);

        guard.write(&target, &source, limit(3_000.0), true, now);
        let datagram = guard.poll_transmit().expect("the write");
        pump_device.handle_datagram(&round_trip(&datagram), now);
        assert!(
            events(&mut pump_device)
                .iter()
                .any(|e| matches!(e, SpineEvent::WriteRequested(_))),
            "the write was put to the application"
        );
        assert_eq!(
            pump_device.poll_timeout(),
            Some(now + expected),
            "announced {announced:?}"
        );
    }
}
