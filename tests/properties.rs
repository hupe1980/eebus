//! Properties that have to hold for every input, not just the ones we thought of.
//!
//! Everything in this file is reached by a peer on the local network, which means every
//! byte of it is untrusted. Two kinds of property are checked:
//!
//! * **Round trips.** What this crate writes, it must read back unchanged — otherwise a
//!   value drifts every time it crosses the wire, and a power limit is not a value that
//!   may drift.
//! * **Robustness.** Arbitrary bytes must produce an error, never a panic. A parser that
//!   panics on a malformed frame is a denial of service that any device on the subnet can
//!   trigger, and on a heat pump controller a panic is a reboot.
//!
//! These complement the fuzz targets in `fuzz/`: the same properties, driven by a
//! coverage-guided engine rather than by a random one, for the cases a random one will
//! not find.

use core::time::Duration;

use eebus::model::rfe;
use eebus::model::{
    self, CmdData, Datagram, LoadControlLimitData, LoadControlLimitId, LoadControlLimitListData,
    MeasurementData, MeasurementId, MeasurementListData, ScaledNumber,
};
use eebus::ship::{ShipMessage, Ski};
use eebus::usecases::limitation::{
    ControllableSystem, CsConfig, LimitWrite, LimitationState, LocalDecision, RejectReason,
};
use proptest::prelude::*;

// ---- strategies --------------------------------------------------------------

/// A scaled number in the range SPINE actually carries.
///
/// The scale is bounded because a `scale` of 300 is not a value any device sends and the
/// property under test is about arithmetic, not about `f64`'s limits.
fn scaled_number() -> impl Strategy<Value = ScaledNumber> {
    (any::<i32>(), -6i16..=6i16).prop_map(|(n, s)| ScaledNumber::new(i64::from(n), s))
}

/// One entry of a `loadControlLimitListData`.
fn limit_entry() -> impl Strategy<Value = LoadControlLimitData> {
    (1u32..64, any::<bool>(), any::<bool>(), scaled_number()).prop_map(
        |(id, active, changeable, value)| LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(id)),
            is_limit_changeable: Some(changeable),
            is_limit_active: Some(active),
            value: Some(value),
            ..Default::default()
        },
    )
}

/// A `measurementListData` with distinct identifiers.
fn measurement_list() -> impl Strategy<Value = MeasurementListData> {
    proptest::collection::vec((1u32..32, scaled_number()), 0..8).prop_map(|entries| {
        let mut seen = Vec::new();
        let data: Vec<_> = entries
            .into_iter()
            .filter(|(id, _)| {
                let fresh = !seen.contains(id);
                seen.push(*id);
                fresh
            })
            .map(|(id, value)| MeasurementData {
                measurement_id: Some(MeasurementId(id)),
                value: Some(value),
                ..Default::default()
            })
            .collect();
        MeasurementListData {
            measurement_data: Some(data),
        }
    })
}

// ---- round trips -------------------------------------------------------------

proptest! {
    /// The wire format is a bijection on the values this crate produces.
    ///
    /// If it were not, a limit would change every time it crossed the network, and the
    /// direction it changed in would be nobody's decision.
    #[test]
    fn a_datagram_survives_the_wire(entries in proptest::collection::vec(limit_entry(), 0..6)) {
        let datagram = Datagram {
            header: Some(model::Header {
                specification_version: Some(model::SpecificationVersion::from("1.3.0")),
                msg_counter: Some(model::MsgCounter(7)),
                cmd_classifier: Some(model::CmdClassifier::Notify),
                ..Default::default()
            }),
            payload: Some(model::Payload {
                cmd: Some(vec![model::Cmd::with_data(CmdData::LoadControlLimitListData(
                    LoadControlLimitListData {
                        load_control_limit_data: Some(entries),
                    },
                ))]),
            }),
        };

        let json = model::to_json(&datagram).expect("encode");
        let back = model::from_json_str(&json).expect("decode");
        prop_assert_eq!(&back, &datagram);

        // And encoding is stable: the same value always produces the same bytes, which
        // is what makes a captured exchange comparable with a replayed one.
        prop_assert_eq!(model::to_json(&back).expect("re-encode"), json);
    }

    /// A `scaledNumber` means the same number after a round trip through `f64`.
    #[test]
    fn a_scaled_number_keeps_its_value(number in -1_000_000i64..1_000_000, scale in -3i16..=3) {
        let original = ScaledNumber::new(number, scale);
        let value = original.to_f64().expect("in range");
        let rebuilt = ScaledNumber::from_f64(value, 3);
        prop_assert!(
            (rebuilt.to_f64().expect("in range") - value).abs() < 1e-6,
            "{original:?} became {rebuilt:?}"
        );
    }

    /// `from_f64` never loses more than the decimal limit it was given.
    #[test]
    fn from_f64_respects_its_decimal_limit(value in -100_000.0f64..100_000.0, decimals in 0u8..=4) {
        let approximated = ScaledNumber::from_f64(value, decimals)
            .to_f64()
            .expect("in range");
        let tolerance = 0.5 * 10f64.powi(-i32::from(decimals)) + 1e-9;
        prop_assert!(
            (approximated - value).abs() <= tolerance,
            "{value} became {approximated}, further than {tolerance}"
        );
    }

    /// An ISO 8601 duration reads back as the duration it was written from.
    #[test]
    fn a_duration_survives_iso_8601(seconds in 0u64..100_000_000) {
        let original = Duration::from_secs(seconds);
        let text = model::format_iso8601_duration(original);
        let back = model::parse_iso8601_duration(&text)
            .unwrap_or_else(|| panic!("{text:?} did not parse"));
        prop_assert_eq!(back, original);
    }

    /// A SKI is forty hex digits, however it was written.
    #[test]
    fn a_ski_round_trips_through_its_text_forms(bytes in any::<[u8; 20]>()) {
        let ski = Ski::from_bytes(bytes);
        prop_assert_eq!(ski.to_string().parse::<Ski>().unwrap(), ski);
        prop_assert_eq!(ski.to_txt_value().parse::<Ski>().unwrap(), ski);
        // The display form groups the digits in fours for a human to read off a label.
        prop_assert_eq!(ski.to_display_string().replace(' ', "").parse::<Ski>().unwrap(), ski);
    }
}

// ---- restricted function exchange laws ----------------------------------------

proptest! {
    /// Applying the same partial update twice is applying it once.
    ///
    /// The implementation guide §3.3 defines a merge, and a merge that is not idempotent
    /// would make a retransmission — which SHIP gives no way to rule out — change the
    /// result.
    #[test]
    fn a_partial_update_is_idempotent(
        stored in measurement_list(),
        update in measurement_list(),
    ) {
        let mut once = stored.clone();
        rfe::apply_partial(&mut once, update.clone());

        let mut twice = stored;
        rfe::apply_partial(&mut twice, update.clone());
        rfe::apply_partial(&mut twice, update);

        prop_assert_eq!(once, twice);
    }

    /// A partial update never removes an entry.
    ///
    /// "Omitted means unchanged" all the way down: an update that mentions one
    /// measurement must not take the others with it. Removing is what `delete` is for.
    #[test]
    fn a_partial_update_never_loses_an_entry(
        stored in measurement_list(),
        update in measurement_list(),
    ) {
        use rfe::ListData;

        let before: Vec<_> = stored
            .entries()
            .unwrap_or_default()
            .iter()
            .map(|e| e.measurement_id)
            .collect();

        let mut merged = stored;
        rfe::apply_partial(&mut merged, update);

        let after: Vec<_> = merged
            .entries()
            .unwrap_or_default()
            .iter()
            .map(|e| e.measurement_id)
            .collect();

        for id in before {
            prop_assert!(after.contains(&id), "{id:?} was lost");
        }
    }

    /// Deleting what was just written leaves what was there before.
    #[test]
    fn a_delete_undoes_the_entries_it_names(stored in measurement_list()) {
        use rfe::ListData;

        let mut emptied = stored.clone();
        rfe::delete_entries(&mut emptied, &stored);
        prop_assert_eq!(emptied.entries().unwrap_or_default().len(), 0);
    }

    /// A restricted read returns a subset, never something that was not stored.
    #[test]
    fn a_restricted_read_only_returns_what_was_there(stored in measurement_list(), wanted in 1u32..32) {
        use rfe::ListData;

        let selectors = model::FilterSelectors::MeasurementListDataSelectors(
            model::MeasurementListDataSelectors {
                measurement_id: Some(MeasurementId(wanted)),
                ..Default::default()
            },
        );
        let data = CmdData::MeasurementListData(stored.clone());
        let CmdData::MeasurementListData(narrowed) = data
            .restrict(Some(&selectors), None)
            .expect("the selector addresses this function")
        else {
            panic!("the function changed under a filter");
        };

        let kept = narrowed.entries().unwrap_or_default();
        prop_assert!(kept.len() <= stored.entries().unwrap_or_default().len());
        for entry in kept {
            prop_assert_eq!(entry.measurement_id, Some(MeasurementId(wanted)));
            prop_assert!(
                stored.entries().unwrap_or_default().contains(entry),
                "an entry came back that was never stored"
            );
        }
    }
}

// ---- robustness ---------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// A SHIP frame off the network never panics, whatever it holds.
    #[test]
    fn ship_framing_survives_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = ShipMessage::decode(&bytes);
    }

    /// Nor does one that starts with a valid type byte and continues with nonsense.
    #[test]
    fn ship_framing_survives_a_plausible_prefix(
        kind in 0u8..=3,
        body in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let mut frame = vec![kind];
        frame.extend_from_slice(&body);
        let _ = ShipMessage::decode(&frame);
    }

    /// The SPINE decoder never panics on arbitrary text.
    #[test]
    fn the_codec_survives_arbitrary_text(text in ".{0,256}") {
        let _ = model::from_json_str(&text);
    }

    /// Nor on JSON that is well-formed but means nothing.
    #[test]
    fn the_codec_survives_well_formed_nonsense(value in json_value()) {
        let text = serde_json::to_string(&value).expect("serialisable");
        let _ = model::from_json_str(&text);
    }

    /// The installation QR payload parser never panics.
    #[test]
    fn the_qr_parser_survives_arbitrary_text(text in ".{0,256}") {
        let _ = text.parse::<eebus::ship::ShipQr>();
    }

    /// Nor does one that looks like a QR payload.
    #[test]
    fn the_qr_parser_survives_a_plausible_payload(fields in ".{0,120}") {
        let _ = alloc_format(&fields).parse::<eebus::ship::ShipQr>();
    }

    /// A payload that parses is one this crate can write back unchanged.
    ///
    /// An installer's tool reads a code and re-renders it — onto a screen after a
    /// certificate update, say — so a field that survives the parser and not the writer
    /// is a field that disappears off the sticker.
    #[test]
    fn a_qr_payload_that_parses_round_trips(text in qr_payload()) {
        if let Ok(qr) = text.parse::<eebus::ship::ShipQr>() {
            let written = qr.to_payload();
            let back = written
                .parse::<eebus::ship::ShipQr>()
                .expect("what we wrote does not parse");
            prop_assert_eq!(back, qr, "unstable payload: {}", written);
        }
    }

    /// The `_ship._tcp` TXT record parser never panics.
    #[test]
    fn the_txt_parser_survives_arbitrary_pairs(
        pairs in proptest::collection::vec((".{0,16}", ".{0,32}"), 0..12),
    ) {
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let _ = eebus::ship::ShipTxtRecord::from_pairs(&borrowed);
    }

    /// A duration string off the wire never panics.
    #[test]
    fn the_duration_parser_survives_arbitrary_text(text in ".{0,64}") {
        let _ = model::parse_iso8601_duration(&text);
    }

    /// A device address off the wire never panics.
    #[test]
    fn the_address_validator_survives_arbitrary_text(text in ".{0,300}") {
        let _ = eebus::spine::validate_device_address(&text);
    }
}

/// `SHIP;…;ENDSHIP;` around whatever was generated.
fn alloc_format(fields: &str) -> String {
    format!("SHIP;{fields};ENDSHIP;")
}

/// A QR payload carrying a real SKI, so the round trip is actually reached.
///
/// Random text almost never contains forty hexadecimal digits, and without them the
/// parser refuses the payload before any of it is exercised.
fn qr_payload() -> impl Strategy<Value = String> {
    (
        any::<[u8; 20]>(),
        proptest::collection::vec((".{0,12}", ".{0,24}"), 0..6),
    )
        .prop_map(|(ski, extra)| {
            let mut out = format!("SHIP;SKI:{};", Ski::from_bytes(ski).to_txt_value());
            for (key, value) in extra {
                out.push_str(&format!("{key}:{value};"));
            }
            out.push_str("ENDSHIP;");
            out
        })
}

/// Arbitrary JSON, up to a few levels deep.
fn json_value() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i32>().prop_map(|n| serde_json::Value::Number(n.into())),
        ".{0,12}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
            proptest::collection::vec((".{0,8}", inner), 0..4)
                .prop_map(|pairs| { serde_json::Value::Object(pairs.into_iter().collect()) }),
        ]
    })
}

proptest! {
    /// A `scaledNumber` never reads as a value that is not a number.
    ///
    /// `scale` is signed 16-bit, so a peer can pair a `number` near `i64::MAX` with a
    /// scale of a few hundred and overflow `f64` in one message.
    #[test]
    fn a_scaled_number_never_reads_as_infinity(number: i64, scale: i16) {
        if let Some(value) = ScaledNumber::new(number, scale).to_f64() {
            prop_assert!(value.is_finite(), "{number}e{scale} read as {value}");
        }
    }

    /// A duration string yields a usable timer or nothing: never a panic, and never a
    /// span pointing backwards.
    #[test]
    fn a_duration_string_never_panics_and_never_runs_backwards(s in ".{0,32}") {
        if let Some(parsed) = model::parse_iso8601_duration(&s)
            && s.starts_with('-')
        {
            prop_assert_eq!(parsed, Duration::ZERO, "{:?} pointed backwards", s);
        }
    }
}

/// A limit whose value is present but unrepresentable is refused, not approximated:
/// substituting `0.0` would apply a limit the peer never asked for.
#[test]
fn an_unrepresentable_limit_value_is_refused() {
    let data = CmdData::LoadControlLimitListData(LoadControlLimitListData {
        load_control_limit_data: Some(vec![LoadControlLimitData {
            limit_id: Some(LoadControlLimitId(1)),
            is_limit_active: Some(true),
            value: Some(ScaledNumber::new(i64::MAX, 308)),
            ..Default::default()
        }]),
    });
    assert_eq!(
        eebus::usecases::limitation::read_limit_write(&data, LoadControlLimitId(1)),
        None,
        "an overflowing value makes the whole write unusable"
    );
}

/// And a duration that is present but not a relative time is refused for the same reason,
/// where reading it as "no duration" would be the *dangerous* half of the mistake.
///
/// LPC §3.1.8.2: "Durations used within this Use Case SHALL be presented as relative
/// times. The same holds for the `endTime` Element used for the duration of validity
/// ([LPC-004])." The schema's own union permits `xs:dateTime` there, so a guard written
/// against the schema rather than the use case sends one in good faith — and an
/// `endTime` read as absent means *no expiry*, so a limit meant to lift after two hours
/// would stay until something else replaced it.
#[test]
fn a_limit_duration_that_is_not_a_relative_time_is_refused() {
    let with_end_time = |end_time: &str| {
        CmdData::LoadControlLimitListData(LoadControlLimitListData {
            load_control_limit_data: Some(vec![LoadControlLimitData {
                limit_id: Some(LoadControlLimitId(1)),
                is_limit_active: Some(true),
                value: Some(ScaledNumber::from_f64(4_200.0, 0)),
                time_period: Some(eebus::model::TimePeriod {
                    end_time: Some(end_time.into()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
        })
    };

    for unreadable in ["2026-09-05T10:00:00Z", "", "P1M", "two hours"] {
        assert_eq!(
            eebus::usecases::limitation::read_limit_write(
                &with_end_time(unreadable),
                LoadControlLimitId(1)
            ),
            None,
            "{unreadable:?} is not a relative time, so the write is unusable"
        );
    }

    // A relative one is read, and `PT0S` — the expired duration [LPC-004] permits a peer
    // to leave in place — is a duration and not an absence.
    let read = eebus::usecases::limitation::read_limit_write(
        &with_end_time("PT2H"),
        LoadControlLimitId(1),
    )
    .expect("a well-formed write");
    assert_eq!(read.duration, Some(Duration::from_secs(7_200)));
    let expired = eebus::usecases::limitation::read_limit_write(
        &with_end_time("PT0S"),
        LoadControlLimitId(1),
    )
    .expect("a well-formed write");
    assert_eq!(expired.duration, Some(Duration::ZERO));
}

// ---- the limitation state machine, driven at random ---------------------------

/// One thing that can happen to a Controllable System.
#[derive(Clone, Copy, Debug)]
enum CsStep {
    Heartbeat,
    Write {
        active: bool,
        watts: f64,
        duration: Option<u64>,
    },
    Reject,
    Interrupt,
    FailsafeLimit(f64),
    FailsafeDuration(u64),
    Tick,
    /// Jump to whatever the machine said its next deadline was.
    Deadline,
}

fn cs_step() -> impl Strategy<Value = (CsStep, u64)> {
    let step = prop_oneof![
        2 => Just(CsStep::Heartbeat),
        3 => (any::<bool>(), 0.0f64..50_000.0, proptest::option::of(0u64..7_200))
            .prop_map(|(active, watts, duration)| CsStep::Write { active, watts, duration }),
        1 => Just(CsStep::Reject),
        1 => Just(CsStep::Interrupt),
        1 => (0.0f64..20_000.0).prop_map(CsStep::FailsafeLimit),
        1 => (0u64..30 * 3_600).prop_map(CsStep::FailsafeDuration),
        2 => Just(CsStep::Tick),
        3 => Just(CsStep::Deadline),
    ];
    (step, 0u64..400)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The invariant the whole sans-IO contract rests on: a caller that only ever waits
    /// for [`ControllableSystem::poll_timeout`] must still reach every timed transition.
    /// `None` is returned in exactly one state — `unlimited/autonomous`, where nothing is
    /// pending — so a `None` anywhere else would make the failsafe of [LPC/LPP-911]
    /// unreachable from a caller that waits on it.
    #[test]
    fn poll_timeout_is_none_only_when_nothing_is_pending(
        steps in proptest::collection::vec(cs_step(), 1..40),
    ) {
        let mut cs = ControllableSystem::new(
            CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)),
            Duration::ZERO,
        );
        let mut now = Duration::ZERO;

        for (step, advance) in steps {
            now += Duration::from_secs(advance);
            match step {
                CsStep::Heartbeat => cs.on_heartbeat(now),
                CsStep::Write { active, watts, duration } => {
                    let write = LimitWrite {
                        is_active: active,
                        watts,
                        duration: duration.map(Duration::from_secs),
                    };
                    cs.on_limit_write(&write, LocalDecision::Apply, now);
                }
                CsStep::Reject => {
                    cs.on_limit_write(
                        &LimitWrite::active(1_000.0),
                        LocalDecision::Reject(RejectReason::SelfProtection),
                        now,
                    );
                }
                CsStep::Interrupt => {
                    cs.interrupt(RejectReason::SafetyRelated, now);
                }
                CsStep::FailsafeLimit(watts) => {
                    cs.on_failsafe_limit_write(watts, now);
                }
                CsStep::FailsafeDuration(secs) => {
                    cs.on_failsafe_duration_write(Duration::from_secs(secs), now);
                }
                CsStep::Tick => cs.handle_timeout(now),
                CsStep::Deadline => {
                    if let Some(deadline) = cs.poll_timeout() {
                        now = now.max(deadline);
                        cs.handle_timeout(now);
                    }
                }
            }

            let pending = cs.poll_timeout();
            let autonomous = cs.state() == LimitationState::UnlimitedAutonomous;
            prop_assert_eq!(
                pending.is_none(),
                autonomous,
                "state {:?} reported {:?} as its next deadline",
                cs.state(),
                pending
            );
        }
    }

    /// Waiting on the deadline, and doing nothing else, always ends somewhere quiet.
    ///
    /// This is the shape of every caller: sleep until `poll_timeout`, call
    /// `handle_timeout`, repeat. It has to terminate — a deadline that reproduces itself
    /// unchanged would spin a real event loop at full speed.
    #[test]
    fn following_the_deadlines_terminates(watts in 0.0f64..20_000.0, hold in 0u64..7_200) {
        let mut cs = ControllableSystem::new(
            CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600)),
            Duration::ZERO,
        );
        cs.on_heartbeat(Duration::from_secs(1));
        cs.on_limit_write(
            &LimitWrite {
                is_active: true,
                watts,
                duration: Some(Duration::from_secs(hold)),
            },
            LocalDecision::Apply,
            Duration::from_secs(2),
        );

        // Five is generous: limited → unlimited/controlled → failsafe → autonomous.
        let mut steps = 0;
        while let Some(deadline) = cs.poll_timeout() {
            cs.handle_timeout(deadline);
            steps += 1;
            prop_assert!(steps <= 8, "the deadlines did not settle: {:?}", cs.state());
        }
        prop_assert_eq!(cs.state(), LimitationState::UnlimitedAutonomous);
    }

    /// The number an appliance acts on is never above the Energy Guard's limit and never
    /// above the contract ([LPC/LPP-042]) — including when the Energy Guard says nothing.
    #[test]
    fn the_power_ceiling_never_exceeds_either_bound(
        limit in 0.0f64..30_000.0,
        contract in 0.0f64..30_000.0,
    ) {
        let mut cs = ControllableSystem::new(
            CsConfig::new(4_200.0, Duration::from_secs(2 * 3_600))
                .with_contractual_max(contract),
            Duration::ZERO,
        );
        cs.on_heartbeat(Duration::from_secs(1));
        cs.on_limit_write(
            &LimitWrite::active(limit),
            LocalDecision::Apply,
            Duration::from_secs(2),
        );

        let ceiling = cs.power_ceiling().expect("a contract is always a ceiling");
        prop_assert!(ceiling <= contract + f64::EPSILON, "{ceiling} above the contract");
        if let Some(effective) = cs.effective_limit().watts() {
            prop_assert!(ceiling <= effective + f64::EPSILON, "{ceiling} above the limit");
        }
    }
}
