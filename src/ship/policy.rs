//! Deciding which of two connections to the same peer survives.
//!
//! Two SHIP nodes that discover each other at the same moment both dial, and end up with
//! two TLS connections between the same pair. SHIP §12.2.3 resolves this without any
//! negotiation, using the one value both ends already agree on: the SKI.
//!
//! * The node with the **bigger** SKI decides. It keeps the **most recent** connection
//!   and closes the rest — gracefully, with a `connectionClose`, if the one it is closing
//!   has reached data exchange.
//! * The node with the **smaller** SKI waits three seconds for that to happen. If it has
//!   not, it pings each connection, closes any that does not answer, and may then close
//!   the older one itself.
//!
//! Both halves are here, because a node is on whichever side of the comparison its own
//! SKI puts it, and a stack that implements only the first half will sit forever holding
//! a duplicate against a peer that expects it to act.
//!
//! Note the deviation worth knowing about: `ship-go`, the reference implementation, keeps
//! the *initiator's* connection instead. The two rules converge on one connection anyway
//! — the smaller-SKI side's three-second fallback closes whatever the other end left
//! open — which is why interoperating with it works without a compatibility switch.

use core::time::Duration;

use super::ski::Ski;

/// How long the smaller-SKI node waits for the bigger one to act (§12.2.3).
pub const ARBITRATION_DELAY: Duration = Duration::from_secs(3);

/// Which side of the SKI comparison this node is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arbiter {
    /// This node's SKI is the bigger one: it decides, and it decides now.
    Local,
    /// The peer's SKI is bigger: wait [`ARBITRATION_DELAY`], then fall back to pinging.
    Peer,
}

/// Who resolves a double connection between these two nodes.
///
/// ```
/// use eebus::ship::{Arbiter, arbiter};
///
/// let small: eebus::ship::Ski = "1111111111111111111111111111111111111111".parse().unwrap();
/// let big: eebus::ship::Ski = "9999999999999999999999999999999999999999".parse().unwrap();
///
/// assert_eq!(arbiter(&big, &small), Arbiter::Local);
/// assert_eq!(arbiter(&small, &big), Arbiter::Peer);
/// ```
pub fn arbiter(local: &Ski, peer: &Ski) -> Arbiter {
    if local.as_bytes() > peer.as_bytes() {
        Arbiter::Local
    } else {
        Arbiter::Peer
    }
}

/// What to do about a set of connections to one peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Nothing to resolve: there is at most one connection.
    Settled,
    /// Close every connection but the most recent one.
    ///
    /// This node has the bigger SKI, so §12.2.3 makes the decision its own and immediate.
    KeepNewest,
    /// Wait: the peer has the bigger SKI and the grace period has not run out.
    Wait {
        /// When to look again.
        until: Duration,
    },
    /// The peer had the bigger SKI and did not act within three seconds.
    ///
    /// Ping every connection, close the ones that do not answer, and — if more than one
    /// still does — close the older.
    PingThenClose,
}

/// Decides what to do about the connections held to one peer.
///
/// `opened` is when each connection was established, on the same monotonic clock as
/// `now`. `duplicates_since` is when this node first saw more than one, which is what the
/// three-second grace period of §12.2.3 is measured from.
pub fn resolve(
    local: &Ski,
    peer: &Ski,
    opened: &[Duration],
    duplicates_since: Duration,
    now: Duration,
) -> Resolution {
    if opened.len() < 2 {
        return Resolution::Settled;
    }
    match arbiter(local, peer) {
        Arbiter::Local => Resolution::KeepNewest,
        Arbiter::Peer => {
            let deadline = duplicates_since + ARBITRATION_DELAY;
            if now < deadline {
                Resolution::Wait { until: deadline }
            } else {
                Resolution::PingThenClose
            }
        }
    }
}

/// The index of the connection to keep, given when each was opened.
///
/// The most recent one, which is what §12.2.3 says — not the one this node opened, and
/// not the oldest. Ties go to the later position, since a connection that arrived later
/// in the same instant is still the more recent.
pub fn newest(opened: &[Duration]) -> Option<usize> {
    opened
        .iter()
        .enumerate()
        .max_by_key(|(index, at)| (**at, *index))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn ski(byte: u8) -> Ski {
        Ski::from_bytes([byte; 20])
    }

    /// §12.2.3: the bigger SKI decides, and keeps the most recent connection.
    #[test]
    fn the_bigger_ski_keeps_the_newest_connection() {
        let opened = vec![Duration::from_secs(1), Duration::from_secs(5)];
        assert_eq!(
            resolve(
                &ski(0xFF),
                &ski(0x01),
                &opened,
                Duration::from_secs(5),
                Duration::from_secs(5)
            ),
            Resolution::KeepNewest
        );
        assert_eq!(newest(&opened), Some(1));
    }

    /// The smaller SKI waits three seconds before doing anything itself.
    #[test]
    fn the_smaller_ski_waits_three_seconds_then_pings() {
        let opened = vec![Duration::from_secs(1), Duration::from_secs(5)];
        assert_eq!(
            resolve(
                &ski(0x01),
                &ski(0xFF),
                &opened,
                Duration::from_secs(5),
                Duration::from_secs(6)
            ),
            Resolution::Wait {
                until: Duration::from_secs(8)
            }
        );
        assert_eq!(
            resolve(
                &ski(0x01),
                &ski(0xFF),
                &opened,
                Duration::from_secs(5),
                Duration::from_secs(8)
            ),
            Resolution::PingThenClose
        );
    }

    #[test]
    fn one_connection_needs_no_arbitration() {
        assert_eq!(
            resolve(
                &ski(0x01),
                &ski(0xFF),
                &[Duration::from_secs(1)],
                Duration::ZERO,
                Duration::from_secs(60)
            ),
            Resolution::Settled
        );
    }

    /// The comparison is over the whole 160-bit value, not a prefix.
    #[test]
    fn the_comparison_uses_the_full_ski() {
        let mut a = [0u8; 20];
        let mut b = [0u8; 20];
        a[19] = 1;
        assert_eq!(
            arbiter(&Ski::from_bytes(a), &Ski::from_bytes(b)),
            Arbiter::Local
        );
        b[19] = 2;
        assert_eq!(
            arbiter(&Ski::from_bytes(a), &Ski::from_bytes(b)),
            Arbiter::Peer
        );
    }
}
