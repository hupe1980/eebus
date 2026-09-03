//! Certificate fingerprints: the identity the SHIP Pairing Service trusts.
//!
//! Plain SHIP identifies a node by the SHA-1 Subject Key Identifier of its public key.
//! The Pairing Service (TS 1.0.0 §6.2, §10.2) identifies it by the SHA-256 of the whole
//! DER-encoded certificate instead, and SHIP 1.1.0's QR code carries that value as
//! `FPH256`. A node that has been paired by fingerprint is admitted when the certificate
//! it presents at TLS hashes to the value that was announced — "this SHALL be seen like a
//! successful trust in an SKI" (§10.2).

use alloc::string::String;
use core::fmt;
use core::str::FromStr;

/// A SHA-256 fingerprint of a DER-encoded certificate.
///
/// The wire form — TXT record, QR code, JSON — is 64 uppercase hexadecimal digits, which
/// is also what [`Display`](fmt::Display) produces. Reading is strict about that: the
/// Pairing Service §5.4 declares a record invalid when a value does not match
/// `[0-9A-F]{64}`, and the digest is computed over the text as sent, so accepting a
/// lowercase value would mean verifying a message the sender never signed.
///
/// ```
/// use eebus::ship::Fingerprint;
///
/// let text = "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943";
/// let fingerprint: Fingerprint = text.parse().unwrap();
/// assert_eq!(fingerprint.to_string(), text);
/// assert!(text.to_lowercase().parse::<Fingerprint>().is_err());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; Fingerprint::LEN]);

/// Why a string could not be read as a [`Fingerprint`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintParseError {
    /// The value did not contain exactly 64 characters.
    #[error("a fingerprint has 64 hexadecimal digits, found {0}")]
    Length(usize),
    /// The value contained a character other than an uppercase hexadecimal digit.
    #[error("`{0}` is not an uppercase hexadecimal digit")]
    NotHex(char),
}

impl Fingerprint {
    /// The length of a SHA-256 digest in bytes.
    pub const LEN: usize = 32;

    /// Wraps raw bytes.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// The fingerprint of a DER-encoded certificate.
    ///
    /// Over the DER, not the PEM: the Pairing Service's Annex A is explicit that the
    /// PEM's base64 and line breaks are not what is hashed.
    #[cfg(any(feature = "pairing", feature = "cert"))]
    #[cfg_attr(docsrs, doc(cfg(any(feature = "pairing", feature = "cert"))))]
    pub fn of_der(der: &[u8]) -> Self {
        use sha2::Digest as _;
        Self(sha2::Sha256::digest(der).into())
    }

    /// The uppercase hexadecimal form the wire uses.
    pub fn to_hex(&self) -> String {
        const ALPHABET: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = String::with_capacity(Self::LEN * 2);
        for byte in &self.0 {
            out.push(ALPHABET[usize::from(byte >> 4)] as char);
            out.push(ALPHABET[usize::from(byte & 0x0f)] as char);
        }
        out
    }
}

impl FromStr for Fingerprint {
    type Err = FingerprintParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != Self::LEN * 2 {
            return Err(FingerprintParseError::Length(s.chars().count()));
        }
        let mut bytes = [0u8; Self::LEN];
        for (i, c) in s.chars().enumerate() {
            let value = match c {
                '0'..='9' => c as u8 - b'0',
                'A'..='F' => c as u8 - b'A' + 10,
                other => return Err(FingerprintParseError::NotHex(other)),
            };
            if i.is_multiple_of(2) {
                bytes[i / 2] = value << 4;
            } else {
                bytes[i / 2] |= value;
            }
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.to_hex())
    }
}

impl serde::Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <&str as serde::Deserialize>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pairing Service TS 1.0.0 Annex A.1: devA's certificate fingerprint.
    const DEV_A: &str = "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943";

    #[test]
    fn reads_and_writes_the_annex_a_value() {
        let fingerprint: Fingerprint = DEV_A.parse().unwrap();
        assert_eq!(fingerprint.to_string(), DEV_A);
        assert_eq!(fingerprint.as_bytes()[0], 0xC7);
        assert_eq!(fingerprint.as_bytes()[31], 0x43);
    }

    #[test]
    fn is_strict_about_case_and_length() {
        assert_eq!(
            DEV_A.to_lowercase().parse::<Fingerprint>(),
            Err(FingerprintParseError::NotHex('c'))
        );
        assert_eq!(
            DEV_A[..62].parse::<Fingerprint>(),
            Err(FingerprintParseError::Length(62))
        );
        assert_eq!(
            alloc::format!("{DEV_A}00").parse::<Fingerprint>(),
            Err(FingerprintParseError::Length(66))
        );
    }

    #[test]
    fn round_trips_through_json_in_the_wire_form() {
        let fingerprint: Fingerprint = DEV_A.parse().unwrap();
        let json = serde_json::to_string(&fingerprint).unwrap();
        assert_eq!(json, alloc::format!("\"{DEV_A}\""));
        assert_eq!(
            serde_json::from_str::<Fingerprint>(&json).unwrap(),
            fingerprint
        );
    }

    #[cfg(feature = "pairing")]
    #[test]
    fn hashes_the_der_not_the_text() {
        // SHA-256 of the empty input, a value every implementation agrees on.
        let empty = Fingerprint::of_der(&[]);
        assert_eq!(
            empty.to_string(),
            "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
        );
    }
}
