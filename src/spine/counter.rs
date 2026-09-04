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
//!
//! They are also not a *sequence*. §5.2.4 makes the counter a sender-unique identifier
//! for `msgCounterReference` to point at; nothing makes arrival order counter order. A
//! peer that allocates a counter in one task and writes the datagram from another sends
//! them interleaved, which `eebus-go` does — so a receiver that keeps only the highest
//! counter it has seen reads the overtaken message as a duplicate and drops it. That is
//! why [`MsgCounterTracker`] remembers a window rather than a maximum.

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
}

/// What a receiver made of an incoming counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
    /// A counter below the highest one seen, and not seen before: a message overtaken in
    /// flight by one its sender numbered later. Processed like any other.
    Reordered,
    /// The counter repeated, or is too far back to tell a late message from one already
    /// processed.
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
    /// The highest counter accepted so far.
    last: Option<u64>,
    /// Which of the [`WINDOW`] counters below `last` have already been seen: bit `i` is
    /// the counter `last - 1 - i`. `last` itself is seen by definition.
    seen: u64,
}

/// How far a counter may go backwards before it reads as a restart rather than a
/// duplicate.
///
/// A peer that reboots begins again from a low number, while a genuine duplicate repeats
/// a counter close to the last one. The threshold separates the two without a handshake.
const RESTART_THRESHOLD: u64 = 1_000;

/// How many counters below the highest one are remembered individually.
///
/// Only inside this window can a message overtaken in flight be told from one already
/// processed. SHIP runs over a WebSocket, so the stream itself is ordered and lossless
/// and the reordering to absorb is only what a sender's own concurrency produces —
/// sixty-four is far past any of it, and costs one word per peer.
const WINDOW: u64 = u64::BITS as u64;

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
    /// // One its sender numbered earlier but wrote later is still news.
    /// assert_eq!(tracker.observe(MsgCounter(8)), CounterCheck::Reordered);
    /// assert_eq!(tracker.observe(MsgCounter(8)), CounterCheck::Duplicate);
    ///
    /// // A peer that reboots starts over from a low number.
    /// tracker.observe(MsgCounter(40_000));
    /// assert_eq!(tracker.observe(MsgCounter(1)), CounterCheck::Restarted);
    /// ```
    pub fn observe(&mut self, counter: MsgCounter) -> CounterCheck {
        let value = counter.get();
        let Some(last) = self.last else {
            self.last = Some(value);
            return CounterCheck::Ascending;
        };

        if value > last {
            let ahead = value - last;
            // `last` becomes a member of the window, at bit `ahead - 1`; everything the
            // window already held moves down by the same amount, and what falls off the
            // end is older than this receiver can reason about.
            self.seen = match ahead < WINDOW {
                true => (self.seen << ahead) | (1 << (ahead - 1)),
                false => 0,
            };
            self.last = Some(value);
            return match ahead {
                1 => CounterCheck::Ascending,
                _ => CounterCheck::Skipped { by: ahead - 1 },
            };
        }

        let behind = last - value;
        // Far enough back to be a reboot or a wrap of the counter space.
        if behind >= RESTART_THRESHOLD {
            self.last = Some(value);
            self.seen = 0;
            return CounterCheck::Restarted;
        }
        // Inside the window and not yet seen: a message its sender numbered before one
        // that overtook it. Outside the window there is nothing left to tell it from a
        // counter already processed, so it is refused.
        if behind == 0 || behind > WINDOW {
            return CounterCheck::Duplicate;
        }
        let bit = 1u64 << (behind - 1);
        if self.seen & bit != 0 {
            return CounterCheck::Duplicate;
        }
        self.seen |= bit;
        CounterCheck::Reordered
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
        assert_eq!(tracker.observe(MsgCounter(9)), CounterCheck::Reordered);
        assert_eq!(tracker.observe(MsgCounter(9)), CounterCheck::Duplicate);
        assert_eq!(
            tracker.last(),
            Some(MsgCounter(10)),
            "neither a duplicate nor a late arrival moves the sequence on"
        );
    }

    /// The exchange that found this: `eebus-go` numbers a binding request 10, then sends
    /// a result numbered 11 from another goroutine first.
    ///
    /// A tracker that keeps only the highest counter reads 10 as a duplicate and drops
    /// it — and because the dropped message was a `call` with `ackRequest`, the peer gets
    /// no result either, so nothing at either end reports that a binding was never made.
    #[test]
    fn a_message_overtaken_by_a_later_one_is_still_processed() {
        let mut tracker = MsgCounterTracker::new();
        for counter in 1..=9 {
            assert!(tracker.observe(MsgCounter(counter)).is_acceptable());
        }
        assert_eq!(
            tracker.observe(MsgCounter(11)),
            CounterCheck::Skipped { by: 1 }
        );
        assert_eq!(tracker.observe(MsgCounter(10)), CounterCheck::Reordered);
        assert_eq!(tracker.observe(MsgCounter(12)), CounterCheck::Ascending);
    }

    /// The window is what makes the distinction possible, so it has an edge.
    #[test]
    fn beyond_the_window_a_late_message_cannot_be_told_from_a_duplicate() {
        let mut tracker = MsgCounterTracker::new();
        tracker.observe(MsgCounter(100));
        assert_eq!(
            tracker.observe(MsgCounter(100 - WINDOW)),
            CounterCheck::Reordered
        );
        assert_eq!(
            tracker.observe(MsgCounter(100 - WINDOW - 1)),
            CounterCheck::Duplicate,
            "one past the window, and it is refused rather than guessed at"
        );
    }

    /// A jump clears what the window held, because none of it is inside any more.
    #[test]
    fn a_large_jump_forward_empties_the_window() {
        let mut tracker = MsgCounterTracker::new();
        tracker.observe(MsgCounter(1));
        tracker.observe(MsgCounter(2));
        assert_eq!(
            tracker.observe(MsgCounter(500)),
            CounterCheck::Skipped { by: 497 }
        );
        // 499 was never sent, and is close enough behind to be a plausible late arrival.
        assert_eq!(tracker.observe(MsgCounter(499)), CounterCheck::Reordered);
        // 2 was seen, but the window no longer reaches it.
        assert_eq!(tracker.observe(MsgCounter(2)), CounterCheck::Duplicate);
    }

    #[test]
    fn the_source_never_hands_out_zero() {
        let mut source = MsgCounterSource::starting_at(u64::MAX);
        assert_eq!(source.next().get(), u64::MAX);
        assert_eq!(source.next().get(), 1, "wraps to one, not to zero");
        assert_eq!(MsgCounterSource::starting_at(0).next().get(), 1);
    }
}
