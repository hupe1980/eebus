//! When a SPINE message is acknowledged, and with what.
//!
//! SPINE's acknowledgement concept (Protocol Specification §5.2.4–5.2.5) is small but
//! exact, and getting it wrong is visible from the outside: a stack that answers a
//! `read` with a success acknowledgement, or that acknowledges an acknowledgement, will
//! fail `TC_SPINE_DATA_006` and `TC_SPINE_DATA_007` in the laboratory and will confuse
//! peers in the field.
//!
//! | `cmdClassifier` | Kind of message | Scope of the acknowledgement |
//! |---|---|---|
//! | `read` | initial | application **error** only |
//! | `write` | initial | application success or error |
//! | `call` | initial | application success or error |
//! | `reply` | response | transmission success or error |
//! | `notify` | response | transmission success or error |
//! | `result` | acknowledgement | none — never answered |

use crate::model::CmdClassifier;

pub use crate::model::ErrorNumber;

/// The default maximum response delay: ten seconds (Protocol Specification §5.2.5.3).
///
/// A feature may raise it for itself by setting `maxResponseDelay` in its description
/// during detailed discovery, which is how a server that needs longer says so instead of
/// leaving the client to guess.
pub const DEFAULT_MAX_RESPONSE_DELAY: core::time::Duration = core::time::Duration::from_secs(10);

/// What an acknowledgement may report for a given classifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckScope {
    /// Only a failure is reported; success is implied by the reply that follows.
    ApplicationErrorOnly,
    /// Both outcomes are reported, because nothing else would tell the sender.
    ApplicationSuccessOrError,
    /// Whether the message was received and parsed, not what it meant.
    TransmissionSuccessOrError,
    /// Nothing is ever sent back.
    None,
}

/// The acknowledgement scope of a classifier (Protocol Specification Table 2).
pub const fn ack_scope(classifier: CmdClassifier) -> AckScope {
    match classifier {
        CmdClassifier::Read => AckScope::ApplicationErrorOnly,
        CmdClassifier::Write | CmdClassifier::Call => AckScope::ApplicationSuccessOrError,
        CmdClassifier::Reply | CmdClassifier::Notify => AckScope::TransmissionSuccessOrError,
        CmdClassifier::Result => AckScope::None,
    }
}

/// Whether a received message is to be answered with a `result`.
///
/// `ack_request` is the message's `ackRequest` element and `error` the outcome of
/// processing it. The rule is §5.2.5.1's four conditions taken together: the classifier
/// must permit an acknowledgement, the outcome must be within the scope that classifier
/// allows, and the sender must have asked for one.
///
/// ```
/// use eebus::model::CmdClassifier;
/// use eebus::spine::{owes_ack, ErrorNumber};
///
/// // A `read` that succeeded is answered by the reply itself, not by a result.
/// assert!(!owes_ack(CmdClassifier::Read, true, ErrorNumber::None));
/// // A `read` that failed still owes an error.
/// assert!(owes_ack(CmdClassifier::Read, true, ErrorNumber::CommandNotSupported));
/// // A `write` reports either way, but only when asked.
/// assert!(owes_ack(CmdClassifier::Write, true, ErrorNumber::None));
/// assert!(!owes_ack(CmdClassifier::Write, false, ErrorNumber::None));
/// // A `result` is never answered — that way lies a loop.
/// assert!(!owes_ack(CmdClassifier::Result, true, ErrorNumber::General));
/// ```
pub fn owes_ack(classifier: CmdClassifier, ack_request: bool, error: ErrorNumber) -> bool {
    match ack_scope(classifier) {
        AckScope::None => false,
        // An error indication is owed whether or not it was asked for: §5.2.4 says the
        // obligation to report a failure is independent of `ackRequest`.
        AckScope::ApplicationErrorOnly => !error.is_success(),
        AckScope::ApplicationSuccessOrError | AckScope::TransmissionSuccessOrError => {
            ack_request || !error.is_success()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TC_SPINE_DATA_007`: a successful read is answered by its reply, not by a result.
    #[test]
    fn tc_spine_data_007_ack_request_is_ignored_for_a_successful_read() {
        assert!(!owes_ack(CmdClassifier::Read, true, ErrorNumber::None));
        assert!(!owes_ack(CmdClassifier::Read, false, ErrorNumber::None));
    }

    /// A read that cannot be served still owes an error indication.
    #[test]
    fn a_failing_read_is_reported() {
        assert!(owes_ack(
            CmdClassifier::Read,
            false,
            ErrorNumber::CommandNotSupported
        ));
    }

    /// `TC_SPINE_DATA_005`: a notify carrying `ackRequest` is acknowledged.
    #[test]
    fn tc_spine_data_005_notify_datagrams_are_acknowledged_on_request() {
        assert!(owes_ack(CmdClassifier::Notify, true, ErrorNumber::None));
        assert!(!owes_ack(CmdClassifier::Notify, false, ErrorNumber::None));
    }

    /// `TC_SPINE_DATA_006` and `TC_SPINE_DATA_008`: a result is never answered, in
    /// either direction. Answering one would produce an unbounded exchange.
    #[test]
    fn tc_spine_data_006_008_results_are_never_answered() {
        for error in [ErrorNumber::None, ErrorNumber::General] {
            assert!(!owes_ack(CmdClassifier::Result, true, error));
            assert!(!owes_ack(CmdClassifier::Result, false, error));
        }
    }

    /// A write is the case where the sender learns nothing without an acknowledgement,
    /// which is why LPC's ACK/NACK — and the §14a evidence it provides — rides on it.
    #[test]
    fn a_write_reports_both_outcomes() {
        assert!(owes_ack(CmdClassifier::Write, true, ErrorNumber::None));
        assert!(owes_ack(
            CmdClassifier::Write,
            true,
            ErrorNumber::CommandRejected
        ));
        assert!(
            owes_ack(CmdClassifier::Write, false, ErrorNumber::CommandRejected),
            "a rejection is reported even unasked"
        );
    }

    #[test]
    fn the_scope_table_matches_the_specification() {
        assert_eq!(
            ack_scope(CmdClassifier::Read),
            AckScope::ApplicationErrorOnly
        );
        assert_eq!(
            ack_scope(CmdClassifier::Write),
            AckScope::ApplicationSuccessOrError
        );
        assert_eq!(
            ack_scope(CmdClassifier::Call),
            AckScope::ApplicationSuccessOrError
        );
        assert_eq!(
            ack_scope(CmdClassifier::Reply),
            AckScope::TransmissionSuccessOrError
        );
        assert_eq!(
            ack_scope(CmdClassifier::Notify),
            AckScope::TransmissionSuccessOrError
        );
        assert_eq!(ack_scope(CmdClassifier::Result), AckScope::None);
    }
}
