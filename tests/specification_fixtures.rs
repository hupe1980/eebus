//! The specification's own example datagrams, **served** rather than only parsed.
//!
//! `tests/fixtures/spine` is the Restricted Function Exchange annex of the SPINE
//! specification: twenty-nine worked examples of a read, a write, a notification and a
//! reply, each with the filters that make it partial, a delete, or both. `tests/codec.rs`
//! decodes them and re-encodes them byte for byte, which tests the codec — and only the
//! codec.
//!
//! **That is not enough, and the gap had a defect in it.** `RFE_W-M-*` and `RFE_N-M-*` are
//! the *mixed* cases: one command carrying a delete filter and a partial-update filter, the
//! shape LPC UC TS §3.4.1.4 is built on. The engine answered the delete and dropped the
//! update for the whole life of the project, with the datagram sitting in the fixture
//! directory the entire time. So this file drives every one of them through a real engine
//! and checks what came out.
//!
//! The two devices are the specification's own: `TestDevice-C` writes and reads,
//! `TestDevice-S` serves. Entity 1 feature 1 is `DeviceClassification` — a function that is
//! *not* a list — and entity 1 feature 2 is `Setpoint`, which is.

use core::time::Duration;

use eebus::model::{
    CmdClassifier, CmdData, Datagram, DeviceClassificationUserData, DeviceType, EntityType,
    FeatureAddress, FeatureType, Function, ScaledNumber, SetpointData, SetpointId,
    SetpointListData,
};
use eebus::spine::{
    Engine, LocalDevice, LocalEntity, LocalFeature, Operations, SpineEvent, feature_address,
};

const CLIENT: (&str, &str) = ("i:46925", "TestDevice-C");
const SERVER: (&str, &str) = ("i:46925", "TestDevice-S");

/// The state both devices start every case from.
///
/// The specification does not ship a "before" for its examples, so this is chosen to make
/// every one of them meaningful: both functions hold a complete value, so a partial update
/// has something to merge into and a delete has something to remove.
fn baseline_classification() -> CmdData {
    CmdData::DeviceClassificationUserData(DeviceClassificationUserData {
        user_node_identification: Some("node-1".into()),
        user_label: Some("oldUserLabel".into()),
        user_description: Some("oldUserDescription".into()),
    })
}

fn baseline_setpoints() -> CmdData {
    CmdData::SetpointListData(SetpointListData {
        setpoint_data: Some(vec![setpoint(1), setpoint(2)]),
    })
}

fn setpoint(id: u32) -> SetpointData {
    SetpointData {
        setpoint_id: Some(SetpointId(id)),
        value: Some(ScaledNumber::new(20, 0)),
        value_min: Some(ScaledNumber::new(10, 0)),
        value_max: Some(ScaledNumber::new(30, 0)),
        value_tolerance_absolute: Some(ScaledNumber::new(5, -1)),
        is_setpoint_changeable: Some(true),
        ..Default::default()
    }
}

/// A device of the specification's example pair, with both functions readable, writable
/// and partial in both directions.
fn test_device(id: (&str, &str)) -> Engine {
    let mut device = LocalDevice::new(id.0, id.1, DeviceType::Generic).expect("a device address");
    device
        .add_entity(
            LocalEntity::new([1], EntityType::Generic)
                .with_feature(
                    LocalFeature::new(
                        1,
                        FeatureType::DeviceClassification,
                        eebus::model::Role::Server,
                    )
                    .with_function(
                        Function::DeviceClassificationUserData,
                        Operations::read_write(),
                    ),
                )
                .with_feature(
                    LocalFeature::new(2, FeatureType::Setpoint, eebus::model::Role::Server)
                        .with_function(Function::SetpointListData, Operations::read_write()),
                ),
        )
        .expect("one feature per type and role");
    Engine::new(device)
}

/// The server, seeded and bound to the client, plus the two feature addresses.
fn seeded_server() -> (Engine, FeatureAddress, FeatureAddress) {
    let mut server = test_device(SERVER);
    let client_device = eebus::spine::device_address(CLIENT.0, CLIENT.1).expect("an address");
    let server_device = server.device().address().clone();

    for feature in [1u32, 2] {
        let local = feature_address(&server_device, &[1], feature);
        let remote = feature_address(&client_device, &[1], feature);
        server.insert_binding(&remote, &local);
        let data = if feature == 1 {
            baseline_classification()
        } else {
            baseline_setpoints()
        };
        server
            .device_mut()
            .resolve_mut(&local)
            .expect("the feature")
            .set_data(data)
            .expect("seed the function");
    }
    (
        server,
        feature_address(&server_device, &[1], 1),
        feature_address(&server_device, &[1], 2),
    )
}

fn fixtures() -> Vec<(String, Datagram)> {
    let mut out = Vec::new();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spine");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixture directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found in {}", dir.display());
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("read the fixture");
        let datagram = eebus::model::from_json_str(&text)
            .unwrap_or_else(|e| panic!("{name} does not decode: {e}"));
        out.push((name, datagram));
    }
    out
}

fn classifier(datagram: &Datagram) -> CmdClassifier {
    datagram
        .header
        .as_ref()
        .and_then(|h| h.cmd_classifier)
        .expect("every fixture names a classifier")
}

/// Every error number the server answered with, as it went on the wire.
fn answers(engine: &mut Engine) -> Vec<String> {
    core::iter::from_fn(|| engine.poll_transmit())
        .map(|datagram| eebus::model::to_json(&datagram).expect("encode"))
        .collect()
}

/// **Every request in the annex is answered, and none of them with an error.**
///
/// This is the assertion that would have caught D60: `RFE_W-M-N-1-01` and
/// `RFE_W-M-Y_1-2-01` are answered `errorNumber` 0 either way, so the *shape* of the
/// answer is not what was wrong — but the state afterwards is, and the two tests below
/// check that. What this one catches is the wider class: a filter the engine cannot serve,
/// a function it does not resolve, a selector it refuses.
#[test]
fn every_request_in_the_specifications_annex_is_served() {
    let mut requests = 0;
    for (name, datagram) in fixtures() {
        let cmd_classifier = classifier(&datagram);
        if !matches!(cmd_classifier, CmdClassifier::Read | CmdClassifier::Write) {
            continue;
        }
        requests += 1;

        let (mut server, _, _) = seeded_server();
        assert!(
            server.handle_datagram(&datagram, Duration::ZERO),
            "{name}: the server discarded the datagram without answering"
        );

        let sent = answers(&mut server);
        assert!(!sent.is_empty(), "{name}: no answer at all");
        for message in &sent {
            assert!(
                !message.contains("\"errorNumber\":") || message.contains("\"errorNumber\":0"),
                "{name}: answered with an error: {message}"
            );
        }
        if cmd_classifier == CmdClassifier::Read {
            assert!(
                sent.iter()
                    .any(|m| m.contains("\"cmdClassifier\":\"reply\"")),
                "{name}: a read was not answered with a reply: {sent:?}"
            );
        }
    }
    assert_eq!(requests, 16, "the annex's reads and writes");
}

/// Every notification and reply in the annex reaches the application, merged.
#[test]
fn every_notification_in_the_specifications_annex_is_merged() {
    let mut notifications = 0;
    for (name, datagram) in fixtures() {
        if !matches!(
            classifier(&datagram),
            CmdClassifier::Notify | CmdClassifier::Reply
        ) {
            continue;
        }
        notifications += 1;

        // The client is the one being told, so it is the client that receives — and it
        // starts from the same baseline the server holds, since that is what a client
        // that has read the function first would have.
        let mut client = test_device(CLIENT);
        let server_device = eebus::spine::device_address(SERVER.0, SERVER.1).expect("an address");
        for (feature, seed) in [(1u32, baseline_classification()), (2, baseline_setpoints())] {
            let remote = feature_address(&server_device, &[1], feature);
            let mut full = eebus::model::Cmd::with_data(seed);
            full.function = None;
            let seeded = Datagram {
                header: Some(eebus::model::Header {
                    specification_version: Some("1.3.0".into()),
                    address_source: Some(remote),
                    address_destination: Some(client.device().address_of(&[1], feature)),
                    msg_counter: Some(eebus::model::MsgCounter(u64::from(feature))),
                    cmd_classifier: Some(CmdClassifier::Notify),
                    ..Default::default()
                }),
                payload: Some(eebus::model::Payload {
                    cmd: Some(vec![full]),
                }),
            };
            client.handle_datagram(&seeded, Duration::ZERO);
        }
        while client.poll_event().is_some() {}

        assert!(
            client.handle_datagram(&datagram, Duration::ZERO),
            "{name}: the client discarded the datagram"
        );
        let reported = core::iter::from_fn(|| client.poll_event()).any(|event| {
            matches!(
                event,
                SpineEvent::DataNotified { .. } | SpineEvent::ReplyReceived { .. }
            )
        });
        assert!(reported, "{name}: nothing reached the application");
        for message in answers(&mut client) {
            assert!(
                !message.contains("\"errorNumber\":") || message.contains("\"errorNumber\":0"),
                "{name}: answered a notification with an error: {message}"
            );
        }
    }
    assert_eq!(notifications, 13, "the annex's notifications and replies");
}

/// `RFE_W-M-Y_1-2-01`: a delete of one element and a partial write, on a list, in one
/// command — and **both** happen.
///
/// The delete addresses `setpointId` 1 and removes its `valueMax`; the partial update in
/// the same command sets `valueMin` and `valueToleranceAbsolute`. Everything else about
/// that entry, and everything about `setpointId` 2, is untouched.
#[test]
fn rfe_w_m_y_1_2_01_deletes_an_element_and_writes_in_one_command() {
    let (mut server, _, setpoints) = seeded_server();
    let datagram = fixture("RFE_W-M-Y_1-2-01.json");
    assert!(server.handle_datagram(&datagram, Duration::ZERO));

    let Some(CmdData::SetpointListData(list)) = server
        .device()
        .resolve(&setpoints)
        .and_then(|f| f.data(&Function::SetpointListData))
    else {
        panic!("the setpoint list is gone");
    };
    let entries = list.setpoint_data.as_ref().expect("entries");
    assert_eq!(entries.len(), 2, "no entry was removed");

    let one = entries
        .iter()
        .find(|e| e.setpoint_id == Some(SetpointId(1)))
        .expect("the addressed entry");
    assert!(one.value_max.is_none(), "valueMax was deleted");
    assert_eq!(
        one.value_min.as_ref().and_then(ScaledNumber::to_f64),
        Some(4.0),
        "and the partial update in the same command was applied"
    );
    // And the merge rule in one line: the fixture sends `valueToleranceAbsolute` as a
    // bare `number` of 1, the stored value was 5 × 10⁻¹, and the omitted `scale` is
    // *unchanged* — so it means 0.1, not 1. Reading the fragment alone is off by ten.
    assert_eq!(
        one.value_tolerance_absolute
            .as_ref()
            .and_then(ScaledNumber::to_f64),
        Some(0.1),
    );
    assert_eq!(
        one.value.as_ref().and_then(ScaledNumber::to_f64),
        Some(20.0),
        "an element neither filter names is unchanged"
    );
    assert_eq!(one.is_setpoint_changeable, Some(true));

    let two = entries
        .iter()
        .find(|e| e.setpoint_id == Some(SetpointId(2)))
        .expect("the entry the selectors did not name");
    assert_eq!(
        two.value_max.as_ref().and_then(ScaledNumber::to_f64),
        Some(30.0),
        "an entry outside the selectors is untouched"
    );
}

/// `RFE_W-M-N-1-01`: the same, on a function that is **not** a list.
///
/// There are no entries to select, so the elements filter applies to the value itself:
/// `userNodeIdentification` goes, and the partial update replaces the two labels.
#[test]
fn rfe_w_m_n_1_01_deletes_an_element_of_a_plain_function_and_writes() {
    let (mut server, classification, _) = seeded_server();
    let datagram = fixture("RFE_W-M-N-1-01.json");
    assert!(server.handle_datagram(&datagram, Duration::ZERO));

    let Some(CmdData::DeviceClassificationUserData(user)) = server
        .device()
        .resolve(&classification)
        .and_then(|f| f.data(&Function::DeviceClassificationUserData))
    else {
        panic!("the function is gone");
    };
    assert!(
        user.user_node_identification.is_none(),
        "userNodeIdentification was deleted"
    );
    assert_eq!(
        user.user_label.as_ref().map(|l| l.as_str()),
        Some("newUserLabel"),
        "and the write in the same command was applied"
    );
    assert_eq!(
        user.user_description.as_ref().map(|d| d.as_str()),
        Some("newUserDescription"),
    );
}

/// `RFE_W-D-Y_1-2-01`: a delete that names entries *and* elements removes only the
/// elements, and only from the entries it names.
#[test]
fn rfe_w_d_y_1_2_01_deletes_an_element_without_taking_the_entry() {
    let (mut server, _, setpoints) = seeded_server();
    let datagram = fixture("RFE_W-D-Y_1-2-01.json");
    assert!(server.handle_datagram(&datagram, Duration::ZERO));

    let Some(CmdData::SetpointListData(list)) = server
        .device()
        .resolve(&setpoints)
        .and_then(|f| f.data(&Function::SetpointListData))
    else {
        panic!("the setpoint list is gone");
    };
    let entries = list.setpoint_data.as_ref().expect("entries");
    assert_eq!(
        entries.len(),
        2,
        "the entry survived its element's deletion"
    );

    let two = entries
        .iter()
        .find(|e| e.setpoint_id == Some(SetpointId(2)))
        .expect("the addressed entry");
    assert!(two.value_min.is_none(), "valueMin was deleted");
    assert_eq!(
        two.value.as_ref().and_then(ScaledNumber::to_f64),
        Some(20.0),
        "and nothing else was"
    );
    assert_eq!(two.setpoint_id, Some(SetpointId(2)), "identity survives");

    let one = entries
        .iter()
        .find(|e| e.setpoint_id == Some(SetpointId(1)))
        .expect("the entry outside the selectors");
    assert!(one.value_min.is_some(), "and no other entry was touched");
}

fn fixture(name: &str) -> Datagram {
    fixtures()
        .into_iter()
        .find(|(fixture, _)| fixture == name)
        .unwrap_or_else(|| panic!("{name} is not in tests/fixtures/spine"))
        .1
}
