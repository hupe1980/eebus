//! Subject Key Identifiers: the identity of a SHIP node.
//!
//! SHIP does not use a public key infrastructure. A node proves who it is with a
//! self-signed certificate, and peers decide whether to trust it by comparing the
//! certificate's Subject Key Identifier — the SHA-1 of the public key — against one the
//! user has seen on a label, in a QR code or in a commissioning tool. That makes the
//! SKI, not a name or an address, the thing everything else keys off.

use alloc::string::String;
use core::fmt;
use core::str::FromStr;

/// A 160-bit Subject Key Identifier.
///
/// SHIP §12.2 fixes the length at 20 bytes. The type is `Copy` and compares in
/// constant time, and its [`Display`](fmt::Display) is the lowercase hex form used on
/// the wire; [`Ski::to_display_string`] produces the spaced, uppercase form meant for
/// people.
///
/// ```
/// use eebus::ship::Ski;
///
/// let ski: Ski = "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222".parse().unwrap();
/// assert_eq!(ski.to_string(), "5555aaaaffff1111cccc3333eeeedddd99992222");
/// assert_eq!(ski.to_txt_value(), "5555AAAAFFFF1111CCCC3333EEEEDDDD99992222");
/// assert_eq!(ski.to_display_string(), "5555 AAAA FFFF 1111 CCCC 3333 EEEE DDDD 9999 2222");
///
/// // Whatever separators a user's transcription contains, the value is the same.
/// assert_eq!("5555 aaaa ffff 1111 cccc 3333 eeee dddd 9999 2222".parse(), Ok(ski));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ski([u8; Ski::LEN]);

/// Why a string could not be read as a [`Ski`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SkiParseError {
    /// The value did not contain exactly 40 hexadecimal digits.
    #[error("a SKI has 40 hexadecimal digits, found {0}")]
    Length(usize),
    /// The value contained a character that is neither a hex digit nor a separator.
    #[error("`{0}` is not a hexadecimal digit")]
    NotHex(char),
}

impl Ski {
    /// The length of a SKI in bytes.
    pub const LEN: usize = 20;

    /// Wraps raw bytes.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// The uppercase form required in the mDNS TXT record by SHIP 1.1.0 §5.4.
    pub fn to_txt_value(&self) -> String {
        self.hex(true, 0)
    }

    /// The human-facing form: uppercase hex in groups of four, as printed on labels and
    /// encoded in the QR code (installation requirements `SRIP-310/9`).
    pub fn to_display_string(&self) -> String {
        self.hex(true, 4)
    }

    fn hex(&self, upper: bool, group: usize) -> String {
        const LOWER: &[u8; 16] = b"0123456789abcdef";
        const UPPER: &[u8; 16] = b"0123456789ABCDEF";
        let alphabet = if upper { UPPER } else { LOWER };
        let mut out = String::with_capacity(Self::LEN * 2 + Self::LEN / 2);
        for (i, byte) in self.0.iter().enumerate() {
            if group != 0 && i != 0 && (i * 2) % group == 0 {
                out.push(' ');
            }
            out.push(alphabet[usize::from(byte >> 4)] as char);
            out.push(alphabet[usize::from(byte & 0x0f)] as char);
        }
        out
    }
}

impl FromStr for Ski {
    type Err = SkiParseError;

    /// Reads a SKI, ignoring spaces, colons and hyphens so that values transcribed from
    /// a label or a QR code parse without further cleanup.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; Self::LEN];
        let mut nibbles = 0usize;
        for c in s.chars() {
            if matches!(c, ' ' | ':' | '-' | '_') {
                continue;
            }
            let value = c.to_digit(16).ok_or(SkiParseError::NotHex(c))? as u8;
            if nibbles >= Self::LEN * 2 {
                nibbles += 1;
                continue;
            }
            let index = nibbles / 2;
            if nibbles % 2 == 0 {
                bytes[index] = value << 4;
            } else {
                bytes[index] |= value;
            }
            nibbles += 1;
        }
        if nibbles != Self::LEN * 2 {
            return Err(SkiParseError::Length(nibbles));
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Ski {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex(false, 0))
    }
}

impl fmt::Debug for Ski {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ski({})", self.hex(false, 0))
    }
}

impl serde::Serialize for Ski {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex(false, 0))
    }
}

impl<'de> serde::Deserialize<'de> for Ski {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <&str as serde::Deserialize>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "A80556A243F57CA17B0D577A59B711B79EB4856C";

    #[test]
    fn parses_the_pairing_service_example() {
        // SHIP Pairing Service TS 1.0.0, Annex A.1: devA's QR code carries this SKI in
        // groups of four.
        let grouped = "A805 56A2 43F5 7CA1 7B0D 577A 59B7 11B7 9EB4 856C";
        let ski: Ski = grouped.parse().unwrap();
        assert_eq!(ski.to_display_string(), grouped);
        assert_eq!(ski.to_txt_value(), EXAMPLE);
        assert_eq!(ski, EXAMPLE.parse().unwrap());
    }

    #[test]
    fn rejects_wrong_lengths_and_non_hex() {
        assert_eq!("abcd".parse::<Ski>(), Err(SkiParseError::Length(4)));
        assert_eq!(
            format!("{EXAMPLE}00").parse::<Ski>(),
            Err(SkiParseError::Length(42))
        );
        assert_eq!(
            "zzzz56A243F57CA17B0D577A59B711B79EB4856C".parse::<Ski>(),
            Err(SkiParseError::NotHex('z'))
        );
    }

    #[test]
    fn round_trips_through_json() {
        let ski: Ski = EXAMPLE.parse().unwrap();
        let json = serde_json::to_string(&ski).unwrap();
        assert_eq!(json, "\"a80556a243f57ca17b0d577a59b711b79eb4856c\"");
        assert_eq!(serde_json::from_str::<Ski>(&json).unwrap(), ski);
    }
}
