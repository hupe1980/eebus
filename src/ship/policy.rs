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
//! # The rule is racy, and this is what is done about it
//!
//! "The most recent connection" is a judgement each node makes from its own clock, about
//! two events that happen a few milliseconds apart on two machines. Nothing in the
//! protocol makes the two judgements agree, and `enbility`'s 2025 analysis of the SHIP
//! specification calls the mechanism out for it: followed literally by two peers whose
//! views differ, it can end with **both connections closed and none left** — connection
//! starvation. `ship-go` deviates deliberately, keeping the *initiator's* connection,
//! which is a judgement both ends can make identically.
//!
//! This module follows §12.2.3, because a certification laboratory tests the
//! specification and not a reference implementation, and closes the hazard from the other
//! end instead:
//!
//! * The fallback is **bounded**. [`Resolution::Probe`] runs exactly one ping round;
//!   [`Resolution::CloseAfterProbe`] then decides, whatever came back. The earlier shape
//!   pinged again whenever every connection answered, which against a peer that had
//!   stopped arbitrating — crashed, or reading the rule the way `ship-go` does — never
//!   terminated.
//! * Losing every connection is recoverable rather than fatal:
//!   [`runtime::Hub`](crate::runtime::Hub) redials a remembered peer, so starvation costs
//!   a reconnection rather than the relationship.
//!
//! Interoperating with `ship-go` works without a compatibility switch: whichever rule the
//! far end applies, one side closes something, and the other side's bounded fallback
//! settles what is left.

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
    /// Ping every connection and look again after the pong timeout. Exactly one round:
    /// the answer to "they all replied" is [`CloseAfterProbe`](Self::CloseAfterProbe),
    /// never another ping.
    Probe,
    /// The ping round has been run. Drop whatever did not answer, and keep the newest of
    /// what did.
    ///
    /// This is the step that makes the fallback terminate. A peer that has stopped
    /// arbitrating leaves both connections perfectly healthy, and a node that only ever
    /// pings would sit on the duplicate for as long as the peer stayed up.
    CloseAfterProbe,
}

/// Decides what to do about the connections held to one peer.
///
/// `opened` is when each connection was established, on the same monotonic clock as
/// `now`. `duplicates_since` is when this node first saw more than one, which is what the
/// three-second grace period of §12.2.3 is measured from. `probed` says whether the ping
/// round has already gone out, and is what stops the fallback from repeating itself.
pub fn resolve(
    local: &Ski,
    peer: &Ski,
    opened: &[Duration],
    duplicates_since: Duration,
    probed: bool,
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
            } else if probed {
                Resolution::CloseAfterProbe
            } else {
                Resolution::Probe
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
                false,
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
                false,
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
                false,
                Duration::from_secs(8)
            ),
            Resolution::Probe
        );
    }

    /// The fallback runs one ping round and then decides, whatever came back.
    ///
    /// This is the case that used to livelock. Against a peer that has stopped
    /// arbitrating — crashed, or applying `ship-go`'s rule instead — every connection
    /// answers every ping, and a node that answered "all alive" with another ping would
    /// hold the duplicate for as long as the peer stayed up.
    #[test]
    fn the_fallback_terminates_even_when_every_connection_answers() {
        let opened = vec![Duration::from_secs(1), Duration::from_secs(5)];
        let after = |probed| {
            resolve(
                &ski(0x01),
                &ski(0xFF),
                &opened,
                Duration::from_secs(5),
                probed,
                Duration::from_secs(30),
            )
        };
        assert_eq!(after(false), Resolution::Probe, "one round goes out");
        assert_eq!(
            after(true),
            Resolution::CloseAfterProbe,
            "and then it is decided, not pinged again"
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
                false,
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
