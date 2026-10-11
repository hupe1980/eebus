//! Whether a link's keep-alive ping has been answered (SHIP §10.4).
//!
//! The connection consumes a pong inside its read loop and only clears `awaiting_pong`;
//! nothing surfaces as a message, so the hub checks here on every timer pass.

use core::time::Duration;

/// What a link's keep-alive says at a given instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeepAlive {
    /// No ping is outstanding.
    Idle,
    /// A ping is outstanding, and there is still time for the pong.
    Waiting,
    /// The ping was answered: the link is alive as of now.
    Answered,
    /// The ping went unanswered for the whole timeout.
    Dead,
}

/// Decides a link's keep-alive state.
///
/// `pinged_at` is when the hub sent the outstanding ping, if one is; `awaiting_pong` is
/// the connection's own flag, which a pong clears. The hub records `pinged_at` only for a
/// ping that was actually sent — a send that fails closes the link instead — so a cleared
/// flag beside a recorded ping can only mean the pong arrived.
pub(crate) fn check(
    pinged_at: Option<Duration>,
    awaiting_pong: bool,
    now: Duration,
    timeout: Duration,
) -> KeepAlive {
    let Some(at) = pinged_at else {
        return KeepAlive::Idle;
    };
    if !awaiting_pong {
        return KeepAlive::Answered;
    }
    if now >= at + timeout {
        KeepAlive::Dead
    } else {
        KeepAlive::Waiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);
    const PINGED: Duration = Duration::from_secs(50);

    #[test]
    fn no_ping_outstanding_is_idle() {
        assert_eq!(check(None, false, PINGED, TIMEOUT), KeepAlive::Idle);
    }

    /// A pong with no other traffic keeps the link, however late the hub looks.
    #[test]
    fn a_pong_alone_keeps_the_link() {
        assert_eq!(
            check(Some(PINGED), false, PINGED + TIMEOUT, TIMEOUT),
            KeepAlive::Answered
        );
        assert_eq!(
            check(
                Some(PINGED),
                false,
                PINGED + Duration::from_secs(1),
                TIMEOUT
            ),
            KeepAlive::Answered
        );
    }

    #[test]
    fn an_unanswered_ping_waits_then_fails() {
        assert_eq!(
            check(Some(PINGED), true, PINGED + Duration::from_secs(9), TIMEOUT),
            KeepAlive::Waiting
        );
        assert_eq!(
            check(Some(PINGED), true, PINGED + TIMEOUT, TIMEOUT),
            KeepAlive::Dead
        );
    }
}
