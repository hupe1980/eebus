//! When to dial a peer again.
//!
//! SHIP says only that a node must not hammer a peer that is down; the schedule is left
//! to the implementation. This one doubles from a second to two minutes, and spreads the
//! attempts out so that a building whose power has just come back does not have every
//! device dialling every other device in the same instant.
//!
//! The spreading is derived from the peer's SKI rather than from a random number
//! generator: two nodes back off differently because their identities differ, which is
//! deterministic, needs no entropy source, and works the same on a microcontroller.

use core::time::Duration;

use crate::ship::Ski;

/// The delay before the first retry.
pub const BASE_DELAY: Duration = Duration::from_secs(1);

/// The longest this will ever wait between attempts.
pub const MAX_DELAY: Duration = Duration::from_secs(120);

/// How much of the delay is spread out, as a fraction: up to a quarter, either way.
const JITTER_SHIFT: u32 = 2;

/// How long to wait before dialling a peer again after `attempt` failures.
///
/// ```
/// use core::time::Duration;
/// use eebus::runtime::reconnect_delay;
///
/// assert_eq!(reconnect_delay(0), Duration::from_secs(1));
/// assert_eq!(reconnect_delay(3), Duration::from_secs(8));
/// assert_eq!(reconnect_delay(30), Duration::from_secs(120), "capped");
/// ```
pub fn reconnect_delay(attempt: u32) -> Duration {
    let seconds = BASE_DELAY
        .as_secs()
        .saturating_mul(1u64 << attempt.min(7))
        .min(MAX_DELAY.as_secs());
    Duration::from_secs(seconds)
}

/// The same, spread out by the peer's identity.
///
/// The delay lands within ±25 % of [`reconnect_delay`], at a point fixed by the SKI. Two
/// devices that lost the same peer at the same moment will not come back at the same
/// moment, and each is consistent with itself across restarts.
pub fn reconnect_delay_for(peer: &Ski, attempt: u32) -> Duration {
    let base = reconnect_delay(attempt).as_millis() as u64;
    let spread = base >> JITTER_SHIFT;
    if spread == 0 {
        return Duration::from_millis(base);
    }
    // Fold the whole SKI into one number, so every part of the identity contributes.
    let mixed = peer.as_bytes().iter().fold(0u64, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(*byte as u64)
    });
    let offset = mixed % (2 * spread);
    Duration::from_millis(base - spread + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay_doubles_and_then_stops() {
        assert_eq!(reconnect_delay(0), Duration::from_secs(1));
        assert_eq!(reconnect_delay(1), Duration::from_secs(2));
        assert_eq!(reconnect_delay(2), Duration::from_secs(4));
        assert_eq!(reconnect_delay(7), Duration::from_secs(120));
        assert_eq!(reconnect_delay(u32::MAX), Duration::from_secs(120));
    }

    #[test]
    fn the_spread_stays_within_a_quarter_and_is_stable() {
        let ski = Ski::from_bytes([0x42; 20]);
        for attempt in 0..10 {
            let base = reconnect_delay(attempt);
            let spread = reconnect_delay_for(&ski, attempt);
            let quarter = base / 4;
            assert!(
                spread >= base - quarter && spread <= base + quarter,
                "attempt {attempt}: {spread:?} is not within a quarter of {base:?}"
            );
            assert_eq!(
                spread,
                reconnect_delay_for(&ski, attempt),
                "and it does not move between calls"
            );
        }
    }

    #[test]
    fn two_peers_do_not_come_back_together() {
        let a = Ski::from_bytes([0x01; 20]);
        let b = Ski::from_bytes([0x02; 20]);
        assert_ne!(reconnect_delay_for(&a, 5), reconnect_delay_for(&b, 5));
    }
}
