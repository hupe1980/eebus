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
