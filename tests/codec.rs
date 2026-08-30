//! The EEBUS JSON codec, checked against messages taken from the specifications.
//!
//! The fixtures in `tests/fixtures/spine/` are the Restricted Function Exchange
//! examples shipped with SPINE 1.3.0, converted from the specification's XML to the
//! wire form by `cargo xtask fixtures`. Round-tripping them through the Rust model has
//! to reproduce them exactly, which pins down element order, the array-of-single-key-
//! objects encoding, empty-element tags and the number/string distinction all at once.

use eebus::model::*;

fn fixtures() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/spine");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .expect("fixture directory; run `cargo xtask fixtures`")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).unwrap();
            (name, text.trim().to_owned())
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures found");
    out
}

#[test]
fn spine_examples_round_trip_byte_for_byte() {
    for (name, json) in fixtures() {
        let datagram = from_json_str(&json).unwrap_or_else(|e| panic!("{name}: decode: {e}"));
        let encoded = to_json(&datagram).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
        assert_eq!(encoded, json, "{name} did not round-trip");
    }
}

#[test]
fn spine_examples_decode_into_the_expected_shape() {
    for (name, json) in fixtures() {
        let dg = from_json_str(&json).unwrap();
        let header = dg
            .header
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: header"));
        assert_eq!(
            header.specification_version.as_ref().map(|v| v.as_str()),
            Some("1.3.0"),
            "{name}: specification version"
        );
        assert!(header.msg_counter.is_some(), "{name}: msgCounter");
        assert!(header.cmd_classifier.is_some(), "{name}: cmdClassifier");
        let payload = dg.payload.as_ref().unwrap();
        let cmds = payload.cmd.as_ref().unwrap();
        assert_eq!(cmds.len(), 1, "{name}: one command per example");
        assert!(cmds[0].function.is_some(), "{name}: function");
    }
}

/// SPINE Protocol Specification §5.3.4 and LPC UC TS §3.4.1.4: a delete of one element
/// combined with a partial write, in a single command.
#[test]
fn lpc_delete_end_time_then_partial_write() {
    let json = concat!(
        r#"{"datagram":[{"header":[{"specificationVersion":"1.3.0"},"#,
        r#"{"addressSource":[{"device":"d:_i:12345_ControlBox"},{"entity":[1]},{"feature":1}]},"#,
        r#"{"addressDestination":[{"device":"d:_i:67890_HeatPump"},{"entity":[1]},{"feature":3}]},"#,
        r#"{"msgCounter":42},{"cmdClassifier":"write"},{"ackRequest":true}]},"#,
        r#"{"payload":[{"cmd":[[{"function":"loadControlLimitListData"},"#,
        r#"{"filter":[[{"cmdControl":[{"delete":[]}]},"#,
        r#"{"loadControlLimitListDataSelectors":[{"limitId":1}]},"#,
        r#"{"loadControlLimitDataElements":[{"timePeriod":[{"endTime":[]}]}]}],"#,
        r#"[{"cmdControl":[{"partial":[]}]}]]},"#,
        r#"{"loadControlLimitListData":[{"loadControlLimitData":"#,
        r#"[[{"limitId":1},{"isLimitActive":true},{"value":[{"number":4200},{"scale":0}]}]]}]}]]}]}]}"#
    );

    let dg = from_json_str(json).expect("decode");
    let cmd = &dg.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];

    assert_eq!(cmd.function, Some(Function::LoadControlLimitListData));
    assert!(cmd.is_delete(), "first filter deletes");
    assert!(cmd.is_partial(), "second filter is partial");

    let filters = cmd.filter.as_ref().unwrap();
    assert_eq!(filters.len(), 2);
    assert!(matches!(
        filters[0].selectors.as_deref(),
        Some([FilterSelectors::LoadControlLimitListDataSelectors(sel)])
            if sel.limit_id == Some(LoadControlLimitId(1))
    ));
    assert!(matches!(
        &filters[0].elements,
        Some(FilterElements::LoadControlLimitDataElements(el))
            if el.time_period.as_ref().is_some_and(|t| t.end_time.is_some())
    ));

    let Some(CmdData::LoadControlLimitListData(list)) = &cmd.data else {
        panic!("expected a loadControlLimitListData payload");
    };
    let entry = &list.load_control_limit_data.as_ref().unwrap()[0];
    assert_eq!(entry.limit_id, Some(LoadControlLimitId(1)));
    assert_eq!(entry.is_limit_active, Some(true));
    assert_eq!(entry.value, Some(ScaledNumber::new(4200, 0)));

    assert_eq!(to_json(&dg).unwrap(), json, "re-encoding is stable");
}

/// A `deviceDiagnosisHeartbeatData` notification, the message LPC's whole failsafe
/// mechanism keys off.
#[test]
fn heartbeat_notification() {
    let json = concat!(
        r#"{"datagram":[{"header":[{"specificationVersion":"1.3.0"},"#,
        r#"{"addressSource":[{"device":"d:_i:12345_ControlBox"},{"entity":[1]},{"feature":2}]},"#,
        r#"{"addressDestination":[{"device":"d:_i:67890_HeatPump"},{"entity":[1]},{"feature":4}]},"#,
        r#"{"msgCounter":7},{"cmdClassifier":"notify"}]},"#,
        r#"{"payload":[{"cmd":[[{"function":"deviceDiagnosisHeartbeatData"},"#,
        r#"{"deviceDiagnosisHeartbeatData":[{"timestamp":"2026-08-30T10:00:00Z"},"#,
        r#"{"heartbeatCounter":12},{"heartbeatTimeout":"PT1M"}]}]]}]}]}"#
    );

    let dg = from_json_str(json).unwrap();
    let cmd = &dg.payload.as_ref().unwrap().cmd.as_ref().unwrap()[0];
    let Some(CmdData::DeviceDiagnosisHeartbeatData(hb)) = &cmd.data else {
        panic!("expected a heartbeat payload");
    };
    assert_eq!(hb.heartbeat_counter, Some(12));
    assert_eq!(hb.heartbeat_timeout.as_deref(), Some("PT1M"));
    assert_eq!(to_json(&dg).unwrap(), json);
}

/// `TC_SPINE_COMP_003` / `TC_SPINE_RTS_004`: unknown elements are ignored, not fatal.
#[test]
fn unknown_elements_are_ignored() {
    let json = concat!(
        r#"{"datagram":[{"header":[{"specificationVersion":"1.3.0"},{"msgCounter":1},"#,
        r#"{"cmdClassifier":"read"},{"someFutureHeaderField":{"nested":[1,2,3]}}]},"#,
        r#"{"payload":[{"cmd":[[{"function":"deviceDiagnosisHeartbeatData"},"#,
        r#"{"unknownFunctionData":[{"x":1}]}]]}]}]}"#
    );
    let dg = from_json_str(json).expect("unknown keys must not fail the parse");
    assert_eq!(dg.header.unwrap().msg_counter, Some(MsgCounter(1)));
    let cmd = &dg.payload.unwrap().cmd.unwrap()[0];
    assert_eq!(cmd.function, Some(Function::DeviceDiagnosisHeartbeatData));
    assert!(cmd.data.is_none(), "the unknown payload is dropped");
}

/// SHIP implementation guide §2.2: whitespace in JSON is insignificant and a stack that
/// assumes minified input is non-compliant.
#[test]
fn pretty_printed_input_is_accepted() {
    let pretty = r#"
        {
            "datagram" : [
                { "header" : [ { "msgCounter" : 3 } ] },
                { "payload" : [] }
            ]
        }
    "#;
    let dg = from_json_str(pretty).expect("whitespace must be tolerated");
    assert_eq!(dg.header.unwrap().msg_counter, Some(MsgCounter(3)));
}

/// Extensible enumerations keep vendor values verbatim across a round trip.
#[test]
fn enum_extensions_survive_a_round_trip() {
    let raw = r#"[{"limitId":1},{"limitType":"_i:12345_vendorLimit"}]"#;
    let desc: LoadControlLimitDescriptionData = serde_json::from_str(raw).unwrap();
    assert_eq!(
        desc.limit_type,
        Some(LoadControlLimitType::Other("_i:12345_vendorLimit".into()))
    );
    assert!(desc.limit_type.as_ref().unwrap().is_extension());
    assert_eq!(serde_json::to_string(&desc).unwrap(), raw);
}

/// Closed enumerations reject values the schema does not define, so a malformed header
/// fails to parse rather than being silently misinterpreted.
#[test]
fn closed_enums_reject_unknown_values() {
    let err = serde_json::from_str::<Header>(r#"[{"cmdClassifier":"teleport"}]"#).unwrap_err();
    assert!(
        err.to_string().contains("teleport"),
        "error should name the offending value: {err}"
    );
}

/// Empty elements encode as `[]` (SHIP §11.4.6 rule 4), and an all-absent struct is
/// itself an empty array.
#[test]
fn element_tags_and_empty_structs_encode_as_empty_arrays() {
    let control = CmdControl {
        partial: Some(ElementTag),
        ..CmdControl::default()
    };
    assert_eq!(
        serde_json::to_string(&control).unwrap(),
        r#"[{"partial":[]}]"#
    );
    assert_eq!(serde_json::to_string(&CmdControl::default()).unwrap(), "[]");
    assert!(CmdControl::default().is_empty());
}

/// Repeated elements become one key with an array of values, never repeated keys.
#[test]
fn repeated_elements_become_a_single_keyed_array() {
    let address = FeatureAddress {
        device: Some(AddressDevice::from("d:_i:1_Device")),
        entity: Some(vec![AddressEntity(1), AddressEntity(2)]),
        feature: Some(AddressFeature(3)),
    };
    assert_eq!(
        serde_json::to_string(&address).unwrap(),
        r#"[{"device":"d:_i:1_Device"},{"entity":[1,2]},{"feature":3}]"#
    );
}

/// The command payload is a choice, so a command cannot carry two functions at once —
/// a state that implementations modelling the payload as ~200 optional fields allow.
#[test]
fn a_command_carries_at_most_one_payload() {
    let cmd = Cmd::with_data(CmdData::DeviceDiagnosisHeartbeatData(
        DeviceDiagnosisHeartbeatData {
            heartbeat_counter: Some(1),
            ..Default::default()
        },
    ));
    assert_eq!(cmd.function, Some(Function::DeviceDiagnosisHeartbeatData));
    assert_eq!(
        serde_json::to_string(&cmd).unwrap(),
        r#"[{"function":"deviceDiagnosisHeartbeatData"},{"deviceDiagnosisHeartbeatData":[{"heartbeatCounter":1}]}]"#
    );
}

/// Identifier newtypes make cross-feature mix-ups a compile error rather than a
/// silently wrong correlation; LPC §3.2.1.3.1 requires the `measurementId` of a limit
/// description to match the one used by Monitoring of Power Consumption.
#[test]
fn identifiers_are_distinct_types() {
    let desc = LoadControlLimitDescriptionData {
        limit_id: Some(LoadControlLimitId(1)),
        measurement_id: Some(MeasurementId(3)),
        ..Default::default()
    };
    assert_eq!(desc.limit_id.map(LoadControlLimitId::get), Some(1));
    assert_eq!(desc.measurement_id.map(MeasurementId::get), Some(3));
    // `desc.limit_id = Some(MeasurementId(3))` does not compile.
}
