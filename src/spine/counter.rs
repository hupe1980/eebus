//! Message counters: numbering what is sent, and correlating what comes back.
//!
//! Every SPINE datagram carries a `msgCounter`, and every response carries the
//! `msgCounterReference` of the message it answers. Together they are what makes a
//! request and its answer a pair — and under §14a EnWG, what makes an acceptance or
//! rejection attributable to a particular limit (LPC implementation guide §4.1.5).
//!
//! The rules a receiver has to live with are looser than they look. Counters ascend, but
//! gaps are permitted (`TC_SPINE_DATA_002`) because a sender may number messages it
//! never sends, and a counter may reset or wrap when a peer restarts
//! (`TC_SPINE_DATA_003`). A receiver that insists on `+1` will drop traffic from
//! perfectly compliant peers.

use crate::model::MsgCounter;

/// Allocates the message counters this node sends.
///
/// ```
/// use eebus::spine::MsgCounterSource;
///
/// let mut counters = MsgCounterSource::default();
/// assert_eq!(counters.next().0, 1);
/// assert_eq!(counters.next().0, 2);
/// ```
#[derive(Clone, Debug)]
pub struct MsgCounterSource {
    next: u64,
}

impl Default for MsgCounterSource {
    /// Starts at one, so that zero is never a valid counter and an unset field cannot be
    /// mistaken for a real one.
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl MsgCounterSource {
    /// A source starting from `start`.
    pub fn starting_at(start: u64) -> Self {
        Self { next: start.max(1) }
    }

    /// The next counter, wrapping back to one rather than to zero.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> MsgCounter {
        let value = self.next;
        self.next = self.next.checked_add(1).unwrap_or(1);
        MsgCounter(value)
    }

    /// The counter that will be handed out next, without taking it.
    pub fn peek(&self) -> MsgCounter {
        MsgCounter(self.next)
    }
}

/// What a receiver made of an incoming counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CounterCheck {
    /// The expected case: the counter advanced.
    Ascending,
    /// The counter jumped forward. Permitted: a sender may number messages it does not
    /// send (`TC_SPINE_DATA_002`).
    Skipped {
        /// How many counters were passed over.
        by: u64,
    },
    /// The counter went backwards far enough to be a restart or a wrap
    /// (`TC_SPINE_DATA_003`), so the sequence starts again from here.
    Restarted,
    /// The counter repeated or went slightly backwards, which is a duplicate rather than
    /// a restart.
    Duplicate,
}

impl CounterCheck {
    /// Whether the message should be processed.
    ///
    /// Everything but a duplicate is: SPINE has no retransmission of its own, so a
    /// counter that merely looks odd is not grounds for dropping a message.
    pub fn is_acceptable(self) -> bool {
        !matches!(self, CounterCheck::Duplicate)
    }
}

/// Follows the counters a peer sends.
#[derive(Clone, Debug, Default)]
pub struct MsgCounterTracker {
    last: Option<u64>,
}

/// How far a counter may go backwards before it reads as a restart rather than a
/// duplicate.
///
/// A peer that reboots begins again from a low number, while a genuine duplicate repeats
/// a counter close to the last one. The threshold separates the two without a handshake.
const RESTART_THRESHOLD: u64 = 1_000;

impl MsgCounterTracker {
    /// A tracker that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last counter accepted from this peer.
    pub fn last(&self) -> Option<MsgCounter> {
        self.last.map(MsgCounter)
    }

    /// Records an incoming counter and says what to make of it.
    ///
    /// ```
    /// use eebus::model::MsgCounter;
    /// use eebus::spine::{CounterCheck, MsgCounterTracker};
    ///
    /// let mut tracker = MsgCounterTracker::new();
    /// assert_eq!(tracker.observe(MsgCounter(1)), CounterCheck::Ascending);
    /// assert_eq!(tracker.observe(MsgCounter(2)), CounterCheck::Ascending);
    /// assert_eq!(tracker.observe(MsgCounter(9)), CounterCheck::Skipped { by: 6 });
    /// assert_eq!(tracker.observe(MsgCounter(9)), CounterCheck::Duplicate);
    ///
    /// // A peer that reboots starts over from a low number.
    /// tracker.observe(MsgCounter(40_000));
    /// assert_eq!(tracker.observe(MsgCounter(1)), CounterCheck::Restarted);
    /// ```
    pub fn observe(&mut self, counter: MsgCounter) -> CounterCheck {
        let value = counter.get();
        let outcome = match self.last {
            None => CounterCheck::Ascending,
            Some(last) if value == last.wrapping_add(1) => CounterCheck::Ascending,
            Some(last) if value > last => CounterCheck::Skipped {
                by: value - last - 1,
            },
            // Far enough back to be a reboot or a wrap of the counter space.
            Some(last) if last.saturating_sub(value) >= RESTART_THRESHOLD => {
                CounterCheck::Restarted
            }
            Some(_) => CounterCheck::Duplicate,
        };

        if outcome.is_acceptable() {
            self.last = Some(value);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TC_SPINE_DATA_001`: counters ascend.
    #[test]
    fn tc_spine_data_001_counters_ascend() {
        let mut source = MsgCounterSource::default();
        let mut tracker = MsgCounterTracker::new();
        for expected in 1..=5u64 {
            let counter = source.next();
            assert_eq!(counter.get(), expected);
            assert_eq!(tracker.observe(counter), CounterCheck::Ascending);
        }
    }

    /// `TC_SPINE_DATA_002`: a gap is permitted, not an error.
    #[test]
    fn tc_spine_data_002_gaps_are_accepted() {
        let mut tracker = MsgCounterTracker::new();
        tracker.observe(MsgCounter(1));
        let outcome = tracker.observe(MsgCounter(100));
        assert_eq!(outcome, CounterCheck::Skipped { by: 98 });
        assert!(outcome.is_acceptable());
        assert_eq!(tracker.last(), Some(MsgCounter(100)));
    }

    /// `TC_SPINE_DATA_003`: a peer that restarts begins again from a low number, and its
    /// messages must keep being processed.
    #[test]
    fn tc_spine_data_003_a_restart_is_tolerated() {
        let mut tracker = MsgCounterTracker::new();
        tracker.observe(MsgCounter(50_000));
        let outcome = tracker.observe(MsgCounter(1));
        assert_eq!(outcome, CounterCheck::Restarted);
        assert!(outcome.is_acceptable());
        assert_eq!(tracker.last(), Some(MsgCounter(1)));
    }

    #[test]
    fn a_repeated_counter_is_a_duplicate_not_a_restart() {
        let mut tracker = MsgCounterTracker::new();
        tracker.observe(MsgCounter(10));
        assert_eq!(tracker.observe(MsgCounter(10)), CounterCheck::Duplicate);
        assert_eq!(tracker.observe(MsgCounter(9)), CounterCheck::Duplicate);
        assert_eq!(
            tracker.last(),
            Some(MsgCounter(10)),
            "a duplicate does not move the sequence"
        );
    }

    #[test]
    fn the_source_never_hands_out_zero() {
        let mut source = MsgCounterSource::starting_at(u64::MAX);
        assert_eq!(source.next().get(), u64::MAX);
        assert_eq!(source.next().get(), 1, "wraps to one, not to zero");
        assert_eq!(MsgCounterSource::starting_at(0).next().get(), 1);
    }
}
