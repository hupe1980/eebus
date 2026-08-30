//! A grid operator's control box limits a heat pump, end to end.
//!
//! This is the §14a EnWG exchange in miniature, using every layer of the crate: the SHIP
//! handshake brings two nodes to the point where they may exchange data, a SPINE
//! datagram carries the heartbeat and then the limit, and the Limitation of Power
//! Consumption state machine decides what the heat pump actually does with it.
//!
//! Run it with:
//!
//! ```sh
//! cargo run --example grid_limit
//! ```
//!
//! Everything here is in-memory and driven by a virtual clock: there is no socket, no
//! TLS and no mDNS, because none of the protocol logic needs any of them.

use core::time::Duration;

use eebus::model::{
    self, AddressDevice, AddressEntity, AddressFeature, Cmd, CmdClassifier, CmdData, Datagram,
    FeatureAddress, Filter, Header, LoadControlLimitData, LoadControlLimitId,
    LoadControlLimitListData, MsgCounter, Payload, ResultData, ScaledNumber, SpecificationVersion,
    TimePeriod,
};
use eebus::ship::{
    Data, DataMessage, Handshake, HandshakeConfig, ProtocolId, Role, ShipMessage, Trust,
};
use eebus::spine::{ErrorNumber, MsgCounterSource, owes_ack};
use eebus::usecases::lpc::{
    ControllableSystem, CsConfig, EffectiveLimit, LimitWrite, LocalDecision, LpcState,
};

/// The SPINE version both sides speak.
const SPINE_VERSION: &str = "1.3.0";

/// The `protocolId` that marks a SHIP data message as carrying SPINE.
const SPINE_PROTOCOL_ID: &str = "ee1.0";

fn main() {
    let mut now = Duration::ZERO;

    // ---- 1. The SHIP handshake -------------------------------------------------
    //
    // The control box dials the heat pump, so it is the SHIP client. Both already
    // trust each other's key — an installer scanned the QR codes at commissioning.

    let mut control_box = Handshake::new(
        Role::Client,
        HandshakeConfig {
            ship_id: Some("i:12345_u:ControlBox-1".into()),
            ..HandshakeConfig::default()
        },
        Trust::Trusted,
        now,
    );
    let mut heat_pump = Handshake::new(
        Role::Server,
        HandshakeConfig {
            ship_id: Some("i:67890_u:HeatPump-1".into()),
            ..HandshakeConfig::default()
        },
        Trust::Trusted,
        now,
    );

    let mut frames = 0;
    loop {
        let mut moved = false;
        while let Some(message) = control_box.poll_transmit() {
            frames += 1;
            deliver(&message, &mut heat_pump, now);
            moved = true;
        }
        while let Some(message) = heat_pump.poll_transmit() {
            frames += 1;
            deliver(&message, &mut control_box, now);
            moved = true;
        }
        if !moved {
            break;
        }
        now += Duration::from_millis(1);
    }

    let ((major, minor), format) = control_box.negotiated().cloned().expect("negotiated");
    println!("SHIP handshake complete after {frames} frames");
    println!("  version {major}.{minor}, format {format}\n");
    assert!(control_box.is_ready_for_data() && heat_pump.is_ready_for_data());

    // ---- 2. The heat pump's use-case state ------------------------------------
    //
    // Failsafe values are pre-configured by the installer: 4.2 kW, held for at least
    // two hours if the control box goes silent.

    let mut lpc = ControllableSystem::new(
        CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)).with_nominal_max(11_000.0),
        now,
    );
    println!("Heat pump starts in {:?}", lpc.state());
    println!("  limited to {:?}\n", lpc.effective_limit());
    assert_eq!(lpc.effective_limit(), EffectiveLimit::Failsafe(4_200.0));

    // ---- 3. A heartbeat, on the wire -------------------------------------------

    let mut counters = MsgCounterSource::default();
    now += Duration::from_secs(1);

    let heartbeat = datagram(
        Node::ControlBox,
        counters.next(),
        CmdClassifier::Notify,
        false,
        CmdData::DeviceDiagnosisHeartbeatData(model::DeviceDiagnosisHeartbeatData {
            timestamp: Some("2026-08-30T10:00:00Z".into()),
            heartbeat_counter: Some(1),
            heartbeat_timeout: Some("PT1M".into()),
        }),
        None,
    );
    let frame = send(&heartbeat);
    println!(
        "control box → heat pump, heartbeat ({} bytes, SHIP type 0x{:02x}):",
        frame.len(),
        frame[0]
    );
    println!("  {}\n", model::to_json(&heartbeat).expect("encode"));

    assert_eq!(receive(&frame), heartbeat, "the datagram survives the wire");
    lpc.on_heartbeat(now);

    // ---- 4. The limit ----------------------------------------------------------
    //
    // A partial write on `loadControlLimitListData`: 3 kW for fifteen minutes. The
    // filter marks it partial, so elements the control box leaves out keep the values
    // the heat pump already holds.

    now += Duration::from_secs(1);
    let limit_counter = counters.next();
    let write = datagram(
        Node::ControlBox,
        limit_counter,
        CmdClassifier::Write,
        true,
        CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_active: Some(true),
                time_period: Some(TimePeriod {
                    end_time: Some("PT15M".into()),
                    ..Default::default()
                }),
                value: Some(ScaledNumber::from_f64(3_000.0, 0)),
                ..Default::default()
            }]),
        }),
        Some(Filter::partial()),
    );
    let frame = send(&write);
    println!("control box → heat pump, limit:");
    println!("  {}\n", model::to_json(&write).expect("encode"));

    // The heat pump decodes it, hands the values to its use-case logic, and answers.
    let outcome = apply(&receive(&frame), &mut lpc, now);

    println!("Heat pump is now in {:?}", lpc.state());
    println!("  limited to {:?}", lpc.effective_limit());
    println!(
        "  answered with errorNumber {} ({outcome})\n",
        outcome.number()
    );
    assert_eq!(lpc.state(), LpcState::Limited);
    assert_eq!(lpc.effective_limit(), EffectiveLimit::Active(3_000.0));

    // The write asked for an acknowledgement, so one is owed either way — this is the
    // ACK that §14a expects the operator to be able to produce as evidence.
    assert!(owes_ack(CmdClassifier::Write, true, outcome));
    let mut result = datagram(
        Node::HeatPump,
        counters.next(),
        CmdClassifier::Result,
        false,
        CmdData::ResultData(ResultData {
            error_number: Some(outcome),
            description: None,
        }),
        None,
    );
    // The reference is what ties the answer to the limit it answers, and what makes the
    // pair usable as evidence.
    if let Some(header) = result.header.as_mut() {
        header.msg_counter_reference = Some(limit_counter);
    }
    println!(
        "heat pump → control box, result:\n  {}\n",
        model::to_json(&result).expect("encode")
    );

    // ---- 5. The control box goes quiet ----------------------------------------
    //
    // No disconnection is signalled and none is needed: the heartbeats simply stop.
    // Two minutes later the failsafe takes over, and it holds for at least the
    // Failsafe Duration Minimum — the rule the 2026 implementation guide added, after
    // implementations were found returning to `init` and going unlimited instead.

    now += Duration::from_secs(200);
    lpc.handle_timeout(now);
    println!("Two minutes of silence later: {:?}", lpc.state());
    println!("  limited to {:?}", lpc.effective_limit());
    assert_eq!(lpc.state(), LpcState::FailsafeState);
    assert_eq!(lpc.effective_limit(), EffectiveLimit::Failsafe(4_200.0));
}

/// Encodes a message, hands it to the peer, and checks it framed correctly on the way.
fn deliver(message: &ShipMessage, peer: &mut Handshake, now: Duration) {
    let bytes = message.encode().expect("encode");
    let decoded = ShipMessage::decode(&bytes).expect("decode");
    assert_eq!(&decoded, message, "framing round-trips");
    peer.handle_message(decoded, now).expect("handshake input");
}

/// Builds a datagram addressed from the control box to the heat pump.
fn datagram(
    from: Node,
    counter: MsgCounter,
    classifier: CmdClassifier,
    ack_request: bool,
    data: CmdData,
    filter: Option<Filter>,
) -> Datagram {
    let mut cmd = Cmd::with_data(data);
    if let Some(filter) = filter {
        cmd = cmd.with_filter(filter);
    }
    let (source, destination) = match from {
        Node::ControlBox => (CONTROL_BOX, HEAT_PUMP),
        Node::HeatPump => (HEAT_PUMP, CONTROL_BOX),
    };
    Datagram {
        header: Some(Header {
            specification_version: Some(SpecificationVersion::from(SPINE_VERSION)),
            address_source: Some(address(source.0, source.1, source.2)),
            address_destination: Some(address(destination.0, destination.1, destination.2)),
            msg_counter: Some(counter),
            cmd_classifier: Some(classifier),
            ack_request: Some(ack_request).filter(|v| *v),
            ..Default::default()
        }),
        payload: Some(Payload {
            cmd: Some(vec![cmd]),
        }),
    }
}

/// Which end of the connection a message comes from.
#[derive(Clone, Copy)]
enum Node {
    ControlBox,
    HeatPump,
}

/// `(device, entity, feature)` of each side's LPC feature.
const CONTROL_BOX: (&str, u32, u32) = ("d:_i:12345_ControlBox", 1, 1);
const HEAT_PUMP: (&str, u32, u32) = ("d:_i:67890_HeatPump", 1, 3);

/// The SPINE implementation guide §2.7 requires the device component of an address to
/// be populated in both directions.
fn address(device: &str, entity: u32, feature: u32) -> FeatureAddress {
    FeatureAddress {
        device: Some(AddressDevice::from(device)),
        entity: Some(vec![AddressEntity(entity)]),
        feature: Some(AddressFeature(feature)),
    }
}

/// The heat pump's side: read the limit out of the datagram and hand it to the use case.
///
/// This is what the feature layer will do once it exists; doing it by hand here keeps
/// the example honest about what the wire actually carries.
fn apply(datagram: &Datagram, lpc: &mut ControllableSystem, now: Duration) -> ErrorNumber {
    let Some(cmd) = datagram
        .payload
        .as_ref()
        .and_then(|p| p.cmd.as_ref())
        .and_then(|c| c.first())
    else {
        return ErrorNumber::General;
    };
    let Some(CmdData::LoadControlLimitListData(list)) = &cmd.data else {
        return ErrorNumber::CommandNotSupported;
    };
    let Some(entry) = list
        .load_control_limit_data
        .as_ref()
        .and_then(|e| e.first())
    else {
        return ErrorNumber::General;
    };

    let write = LimitWrite {
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
    };

    // A real heat pump would ask its controller whether it can follow the limit; here it
    // always can.
    match lpc.on_limit_write(&write, LocalDecision::Apply, now) {
        outcome if outcome.is_accepted() => ErrorNumber::None,
        _ => ErrorNumber::CommandRejected,
    }
}

/// Wraps a datagram in the SHIP data message it travels in, and frames it.
///
/// `protocolId` names the protocol inside; SHIP itself never looks at the payload.
fn send(datagram: &Datagram) -> Vec<u8> {
    let message = ShipMessage::Data(DataMessage::Data(Data {
        header: Some(eebus::ship::Header {
            protocol_id: Some(ProtocolId(SPINE_PROTOCOL_ID.into())),
        }),
        payload: Some(model::to_json_value(datagram).expect("encode")),
        extension: None,
    }));
    message.encode().expect("frame")
}

/// Unwraps a framed SHIP data message back into a datagram.
fn receive(frame: &[u8]) -> Datagram {
    let ShipMessage::Data(DataMessage::Data(data)) = ShipMessage::decode(frame).expect("decode")
    else {
        panic!("expected a data message");
    };
    assert_eq!(
        data.header.and_then(|h| h.protocol_id).map(|p| p.0),
        Some(SPINE_PROTOCOL_ID.to_string()),
        "the payload is SPINE"
    );
    model::from_json_value(data.payload.expect("payload")).expect("datagram")
}
