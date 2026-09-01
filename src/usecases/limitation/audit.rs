//! The record §14a EnWG asks an operator to be able to produce.
//!
//! A limit is a `write` carrying a `msgCounter`; the answer is a `result` carrying the
//! same counter as `msgCounterReference`; the pair is the evidence that the limitation was
//! honoured. LPC implementation guide §4.1.5 adds one distinction: an energy manager that
//! passes its devices' answers through needs no further logging, and one that decides on
//! anything else — measured power at the grid connection point, say — must be able to
//! produce what it decided on.
//!
//! So this is a bounded list of what was written, what was answered, and what the decision
//! rested on. Nothing here writes to disk: the records are `serde`-serialisable, and only
//! the application knows where they belong and how long they must be kept.
//!
//! ```
//! use core::time::Duration;
//! use eebus::model::MsgCounter;
//! use eebus::usecases::limitation::{AuditLog, LimitRecord, LimitWrite, WriteOutcome};
//!
//! let mut log = AuditLog::new();
//! log.record(LimitRecord::new(
//!     Duration::from_secs(42),
//!     MsgCounter(7),
//!     LimitWrite::active(4_200.0),
//!     WriteOutcome::Accepted,
//! ));
//!
//! assert_eq!(log.len(), 1);
//! assert!(log.records().all(|r| r.outcome.is_accepted()));
//! ```

use alloc::collections::VecDeque;
use alloc::string::String;

use serde::{Deserialize, Serialize};

use crate::model::{AddressDevice, MsgCounter};

use super::state::{LimitWrite, WriteOutcome};

/// How many records are kept before the oldest is dropped.
///
/// A limit arrives at most every few minutes in normal operation — the implementation
/// guide §2.10 asks an Energy Guard not to write more often than once every five — so
/// this is several days of history on a device with no disk. An application that has to
/// keep more should drain the log into storage of its own.
pub const DEFAULT_CAPACITY: usize = 512;

/// One limit write and what was answered.
///
/// There is deliberately no [`Default`]: a record with a zeroed limit and an assumed
/// outcome is a record of something that did not happen, and this type exists to be
/// evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitRecord {
    /// When it happened, on the monotonic clock the rest of the crate is driven by.
    pub at: core::time::Duration,
    /// The wall-clock time, ISO 8601, where the device has a clock to read.
    ///
    /// The monotonic reading above is what orders the records; this is what makes them
    /// legible to a regulator, and a device without a real-time clock simply has none.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub timestamp: Option<String>,
    /// The peer the exchange was with.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub peer: Option<AddressDevice>,
    /// The `msgCounter` of the write, which the answer references.
    pub request: MsgCounter,
    /// What was written, where it could be read.
    ///
    /// [`None`] records a write on the limit that could not be read as one — a value the
    /// device cannot represent, or a payload naming a limit it does not serve. It is
    /// deliberately not a zeroed [`LimitWrite`]: this log is evidence, and a record
    /// saying an Energy Guard asked for nought watts when it asked for something
    /// unintelligible would be evidence of something that did not happen.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub write: Option<LimitWrite>,
    /// What was answered.
    pub outcome: WriteOutcome,
    /// What the decision was based on, where it was not a device's own answer.
    ///
    /// The guide's distinction: an energy manager that passed its devices' answers
    /// through needs no note here, and one that decided on measurements has to be able to
    /// say what they were.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub basis: Option<String>,
}

impl LimitRecord {
    /// A record of a write and its answer.
    pub fn new(
        at: core::time::Duration,
        request: MsgCounter,
        write: LimitWrite,
        outcome: WriteOutcome,
    ) -> Self {
        Self {
            at,
            timestamp: None,
            peer: None,
            request,
            write: Some(write),
            outcome,
            basis: None,
        }
    }

    /// A record of a write on the limit that could not be read as one.
    pub fn unreadable(
        at: core::time::Duration,
        request: MsgCounter,
        outcome: WriteOutcome,
    ) -> Self {
        Self {
            at,
            timestamp: None,
            peer: None,
            request,
            write: None,
            outcome,
            basis: None,
        }
    }

    /// Adds the wall-clock time.
    #[must_use]
    pub fn at_time(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Adds the peer the exchange was with.
    #[must_use]
    pub fn with_peer(mut self, peer: AddressDevice) -> Self {
        self.peer = Some(peer);
        self
    }

    /// Notes what the decision rested on, where it was not a device's own answer.
    #[must_use]
    pub fn on_basis(mut self, basis: impl Into<String>) -> Self {
        self.basis = Some(basis.into());
        self
    }
}

/// A bounded, append-only record of limit writes and their answers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditLog {
    records: VecDeque<LimitRecord>,
    capacity: usize,
    dropped: u64,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditLog {
    /// A log holding [`DEFAULT_CAPACITY`] records.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// A log holding `capacity` records; the oldest is dropped to make room.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    /// Appends a record, dropping the oldest if the log is full.
    pub fn record(&mut self, record: LimitRecord) {
        while self.records.len() >= self.capacity {
            self.records.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.records.push_back(record);
    }

    /// The records held, oldest first.
    pub fn records(&self) -> impl Iterator<Item = &LimitRecord> {
        self.records.iter()
    }

    /// The most recent record.
    pub fn last(&self) -> Option<&LimitRecord> {
        self.records.back()
    }

    /// How many records are held.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// How many records the capacity has cost.
    ///
    /// Not zero is a signal, not a detail: it means the evidence for that period is gone,
    /// and an operator required to keep it should be draining the log more often.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Takes every record held, leaving the log empty.
    ///
    /// The way to move history into storage without losing what arrives in between.
    pub fn drain(&mut self) -> alloc::vec::Vec<LimitRecord> {
        self.records.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::limitation::NackReason;
    use core::time::Duration;

    fn record(counter: u64) -> LimitRecord {
        LimitRecord::new(
            Duration::from_secs(counter),
            MsgCounter(counter),
            LimitWrite::active(4_200.0),
            WriteOutcome::Accepted,
        )
    }

    #[test]
    fn the_log_keeps_what_it_is_given_in_order() {
        let mut log = AuditLog::new();
        for counter in 1..=3 {
            log.record(record(counter));
        }
        let counters: alloc::vec::Vec<_> = log.records().map(|r| r.request.get()).collect();
        assert_eq!(counters, [1, 2, 3]);
        assert_eq!(log.last().unwrap().request, MsgCounter(3));
        assert_eq!(log.dropped(), 0);
    }

    /// A full log drops the oldest and says how many it has lost, because the alternative
    /// is an operator believing it has evidence it does not.
    #[test]
    fn a_full_log_reports_what_it_had_to_drop() {
        let mut log = AuditLog::with_capacity(2);
        for counter in 1..=5 {
            log.record(record(counter));
        }
        assert_eq!(log.len(), 2);
        assert_eq!(log.dropped(), 3);
        let counters: alloc::vec::Vec<_> = log.records().map(|r| r.request.get()).collect();
        assert_eq!(counters, [4, 5], "the most recent survive");
    }

    #[test]
    fn draining_moves_the_history_out_without_losing_what_follows() {
        let mut log = AuditLog::new();
        log.record(record(1));
        let taken = log.drain();
        assert_eq!(taken.len(), 1);
        assert!(log.is_empty());

        log.record(record(2));
        assert_eq!(log.len(), 1);
        assert_eq!(log.dropped(), 0, "draining is not dropping");
    }

    /// The record survives a round trip through JSON, which is what makes it storable.
    #[test]
    fn a_record_round_trips_through_json() {
        let record = LimitRecord::new(
            Duration::from_secs(90),
            MsgCounter(11),
            LimitWrite::active_for(3_000.0, Duration::from_secs(900)),
            WriteOutcome::Rejected(NackReason::NoRecentHeartbeat),
        )
        .at_time("2026-08-30T10:00:00Z")
        .on_basis("measured 4.8 kW at the grid connection point");

        let json = serde_json::to_string(&record).unwrap();
        let back: LimitRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
        assert!(json.contains("2026-08-30T10:00:00Z"));
        assert!(json.contains("grid connection point"));
    }

    /// A record with nothing optional set carries no empty fields into the file.
    #[test]
    fn absent_details_do_not_reach_the_file() {
        let json = serde_json::to_string(&record(1)).unwrap();
        assert!(!json.contains("timestamp"));
        assert!(!json.contains("basis"));
        assert!(!json.contains("peer"));
    }
}
