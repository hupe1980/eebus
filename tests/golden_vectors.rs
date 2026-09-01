//! Golden vectors: exactly what this crate puts on the wire for a §14a day.
//!
//! The fixtures in `tests/fixtures/spine/` are the specification's own examples, and they
//! pin the *codec*. This file pins the other half — what the use-case actors actually
//! send — by driving the whole LPC exchange against a virtual clock and comparing every
//! datagram, byte for byte, against a file checked into the repository.
//!
//! It is here for two reasons:
//!
//! * **A consumer can trust the wire without a control box on the desk.** The file is
//!   plain text: an implementer of the other side can read what to expect, and a
//!   maintainer of this crate can see in a diff when a change moves a byte.
//! * **A change to the encoding cannot pass unnoticed.** Element order, the
//!   array-of-single-key-objects shape, which elements are omitted when absent, the
//!   `scaledNumber` scale — all of it is fixed by the file rather than by a round-trip
//!   test, which would happily agree with itself if both ends changed together.
//!
//! To re-record it after a deliberate change:
//!
//! ```sh
//! UPDATE_GOLDEN=1 cargo test --test golden_vectors
//! ```
//!
//! and read the diff before committing it. A diff nobody can explain is the point of the
//! file.

mod common;

use core::time::Duration;

use common::Pair;
use eebus::usecases::limitation::{self, LimitWrite};

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden/lpc_exchange.txt"
);

/// Drives the exchange every §14a installation runs, and records it.
///
/// Discovery, use-case announcement, the two bindings, the heartbeat subscription, the
/// opening deactivation, a limit, its acknowledgement, and the failsafe values.
fn record() -> String {
    let mut pair = Pair::new();
    pair.commission();

    let device = pair.device();
    pair.guard
        .require(&device, Some(LimitWrite::active(3_000.0)), pair.now);
    pair.advance(Duration::from_secs(1));

    pair.guard
        .write_failsafe_limit(&mut pair.guard_engine, &device, 4_200.0, pair.now);
    pair.settle();
    pair.guard.write_failsafe_duration(
        &mut pair.guard_engine,
        &device,
        Duration::from_secs(2 * 3_600),
        pair.now,
    );
    pair.settle();

    // The limit's duration expires and the system says so of its own accord.
    pair.advance(limitation::MIN_WRITE_INTERVAL);

    let mut out = String::new();
    out.push_str(
        "# The LPC exchange as it goes on the wire. Re-record with UPDATE_GOLDEN=1.\n\
         # Left column: which node sent it.\n\n",
    );
    for line in pair.wire() {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn the_lpc_exchange_matches_the_recorded_wire() {
    let recorded = record();

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN).parent().unwrap())
            .expect("the fixture directory");
        std::fs::write(GOLDEN, &recorded).expect("write the golden file");
        return;
    }

    let expected = std::fs::read_to_string(GOLDEN).unwrap_or_else(|error| {
        panic!("{GOLDEN}: {error}\nrun `UPDATE_GOLDEN=1 cargo test --test golden_vectors`")
    });

    if expected != recorded {
        let expected_lines: Vec<&str> = expected.lines().collect();
        let recorded_lines: Vec<&str> = recorded.lines().collect();
        for (index, (was, now)) in expected_lines.iter().zip(&recorded_lines).enumerate() {
            assert_eq!(
                was,
                now,
                "line {} of the recorded exchange changed",
                index + 1
            );
        }
        panic!(
            "the exchange is {} lines where the recording has {}",
            recorded_lines.len(),
            expected_lines.len()
        );
    }
}

/// Every recorded datagram is one this crate can read back — the recording is a wire
/// format, not a debug rendering that happens to look like one.
#[test]
fn every_recorded_datagram_decodes() {
    let text = std::fs::read_to_string(GOLDEN).expect("the golden file");
    let mut seen = 0;
    for line in text.lines() {
        let Some((_, json)) = line.split_once('|') else {
            continue;
        };
        let json = json.trim();
        let datagram = eebus::model::from_json_str(json).expect("a datagram this crate wrote");
        assert_eq!(
            eebus::model::to_json(&datagram).expect("re-encodable"),
            json,
            "byte for byte"
        );
        seen += 1;
    }
    assert!(seen > 20, "the exchange is more than a handful of messages");
}
