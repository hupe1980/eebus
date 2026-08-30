//! `resultData.errorNumber`: the SPINE acknowledgement code.
//!
//! Written by hand rather than generated. The schema types it as a bare unsigned
//! integer, but the Resource Specification's "Result" table gives ten of those numbers a
//! meaning and the SPINE implementation guide §3.5 says which to use when — so a plain
//! newtype would throw away exactly the part callers need. Modelling it as an
//! enumeration with an `Other` arm keeps the meaning without losing a number this
//! version of the specification has not seen.

use crate::codec::Merge;

/// The values of `resultData.errorNumber`.
///
/// Zero is a positive acknowledgement; everything else is a negative one
/// (Protocol Specification §5.2.5.2).
///
/// ```
/// use eebus::model::ErrorNumber;
///
/// assert!(ErrorNumber::None.is_success());
/// assert_eq!(ErrorNumber::CommandRejected.number(), 7);
///
/// // Serialises as the bare number the schema calls for.
/// assert_eq!(serde_json::to_string(&ErrorNumber::BindingRequired).unwrap(), "9");
///
/// // A number this version does not define is still an error, and survives a round trip.
/// let future: ErrorNumber = serde_json::from_str("42").unwrap();
/// assert_eq!(future, ErrorNumber::Other(42));
/// assert!(!future.is_success());
/// assert_eq!(serde_json::to_string(&future).unwrap(), "42");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorNumber {
    /// No error: a positive acknowledgement.
    #[default]
    None,
    /// An error that fits no other category.
    General,
    /// A timeout.
    Timeout,
    /// The recipient is overloaded and cannot process the message.
    Overload,
    /// `addressDestination` names nothing the recipient knows.
    DestinationUnknown,
    /// `addressDestination` is known but unreachable, in enhanced communication mode.
    DestinationUnreachable,
    /// The command is not supported, such as a read of an unimplemented function.
    CommandNotSupported,
    /// The command is refused.
    ///
    /// This is the NACK a use case means when it declines a write — LPC's rejection of a
    /// limit, and under §14a EnWG part of the evidence the operator must be able to
    /// produce (LPC implementation guide §4.1.5).
    CommandRejected,
    /// The Restricted Function Exchange combination in the command is not valid.
    RestrictedExchangeNotSupported,
    /// The client has no binding on the feature it tried to write.
    BindingRequired,
    /// A value this version of the specification does not define.
    Other(u32),
}

impl ErrorNumber {
    /// Reads a received error number.
    pub const fn from_number(n: u32) -> Self {
        match n {
            0 => Self::None,
            1 => Self::General,
            2 => Self::Timeout,
            3 => Self::Overload,
            4 => Self::DestinationUnknown,
            5 => Self::DestinationUnreachable,
            6 => Self::CommandNotSupported,
            7 => Self::CommandRejected,
            8 => Self::RestrictedExchangeNotSupported,
            9 => Self::BindingRequired,
            other => Self::Other(other),
        }
    }

    /// The number as it appears in `resultData`.
    pub const fn number(self) -> u32 {
        match self {
            Self::None => 0,
            Self::General => 1,
            Self::Timeout => 2,
            Self::Overload => 3,
            Self::DestinationUnknown => 4,
            Self::DestinationUnreachable => 5,
            Self::CommandNotSupported => 6,
            Self::CommandRejected => 7,
            Self::RestrictedExchangeNotSupported => 8,
            Self::BindingRequired => 9,
            Self::Other(n) => n,
        }
    }

    /// True for a positive acknowledgement.
    pub const fn is_success(self) -> bool {
        matches!(self, Self::None)
    }
}

impl From<u32> for ErrorNumber {
    fn from(n: u32) -> Self {
        Self::from_number(n)
    }
}

impl From<ErrorNumber> for u32 {
    fn from(e: ErrorNumber) -> Self {
        e.number()
    }
}

impl core::fmt::Display for ErrorNumber {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::None => f.write_str("no error"),
            Self::General => f.write_str("general error"),
            Self::Timeout => f.write_str("timeout"),
            Self::Overload => f.write_str("overload"),
            Self::DestinationUnknown => f.write_str("destination unknown"),
            Self::DestinationUnreachable => f.write_str("destination unreachable"),
            Self::CommandNotSupported => f.write_str("command not supported"),
            Self::CommandRejected => f.write_str("command rejected"),
            Self::RestrictedExchangeNotSupported => {
                f.write_str("restricted function exchange combination not supported")
            }
            Self::BindingRequired => f.write_str("binding is necessary for this command"),
            Self::Other(n) => write!(f, "unknown error {n}"),
        }
    }
}

impl serde::Serialize for ErrorNumber {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.number())
    }
}

impl<'de> serde::Deserialize<'de> for ErrorNumber {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_number(u32::deserialize(deserializer)?))
    }
}

impl Merge for ErrorNumber {
    fn merge(&mut self, update: Self) {
        *self = update;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_defined_number_round_trips() {
        for n in 0..=9u32 {
            let error = ErrorNumber::from_number(n);
            assert_eq!(error.number(), n);
            assert!(!matches!(error, ErrorNumber::Other(_)), "{n} is defined");
        }
    }

    #[test]
    fn only_zero_is_a_success() {
        assert!(ErrorNumber::None.is_success());
        for n in 1..=12u32 {
            assert!(!ErrorNumber::from_number(n).is_success(), "{n}");
        }
    }

    #[test]
    fn unknown_numbers_survive_a_round_trip() {
        let future = ErrorNumber::from_number(200);
        assert_eq!(future, ErrorNumber::Other(200));
        assert_eq!(future.number(), 200);
        assert_eq!(serde_json::to_string(&future).unwrap(), "200");
    }
}
