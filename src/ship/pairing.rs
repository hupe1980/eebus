//! The SHIP Pairing Service: automated trust from a scanned secret.
//!
//! Plain SHIP asks a person to compare a forty-digit SKI on two screens. The pairing
//! service replaces that with a secret printed alongside the QR code: the device being
//! added proves it knows the secret by announcing a `_shippairing._tcp` record whose
//! `digest` is an HMAC over the record's own fields. The target device recomputes the
//! HMAC and, if it matches, trusts the announced certificate without further
//! interaction.
//!
//! Two properties make that safe, and both are implemented here:
//!
//! * the digest covers the certificate **fingerprints** of both devices, so a
//!   man-in-the-middle cannot substitute its own certificate; and
//! * a nonce chosen by the requesting device, together with a [`ReplayGuard`], stops a
//!   captured announcement from being replayed later.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The DNS-SD service type of the pairing service.
pub const SERVICE_TYPE: &str = super::PAIRING_SERVICE_TYPE;

/// The only digest algorithm this version of the specification defines.
pub const ALG_HMAC_SHA256: &str = "hmacSha256";

/// The only parameter type: a SHA-256 fingerprint of the DER certificate.
pub const PAR_TYPE_FP_SHA256: &str = "fpSha256";

/// The only request type: add a credential user.
pub const TYPE_ADD_CU: &str = "addCu";

/// The elliptic curve the trust applies to.
pub const CURVE_SECP256R1: &str = "secp256r1";

/// A pairing secret, printed with the QR code as `SPSEC`.
///
/// The type refuses to print itself and zeroes its storage when dropped, so a secret
/// cannot reach a log through a stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingSecret(Vec<u8>);

impl PairingSecret {
    /// Wraps raw secret bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Reads the hex form used in the `SPSEC` field of a QR code.
    pub fn from_hex(hex: &str) -> Result<Self, PairingError> {
        Ok(Self(decode_hex(hex)?))
    }

    /// The secret's bytes.
    fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for PairingSecret {
    fn drop(&mut self) {
        // Overwrite before the allocation is returned. `write_volatile` would be
        // stronger, but this crate forbids `unsafe`, and a plain overwrite of a `Vec`
        // that is subsequently read by `len()` is not eliminated in practice.
        self.0.iter_mut().for_each(|b| *b = 0);
        self.0.clear();
    }
}

impl core::fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PairingSecret(<redacted>)")
    }
}

/// Why a pairing announcement could not be built or verified.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// A hex-encoded value was malformed.
    #[error("invalid hexadecimal value")]
    InvalidHex,
    /// A field did not have the length or case the specification fixes.
    #[error("`{field}` must be {expected}")]
    InvalidField {
        /// The offending TXT key.
        field: &'static str,
        /// What the specification requires.
        expected: &'static str,
    },
    /// An algorithm, parameter type or request type this version does not define.
    #[error("unsupported `{field}` value `{value}`")]
    Unsupported {
        /// The offending TXT key.
        field: &'static str,
        /// The value that was rejected.
        value: String,
    },
    /// The digest did not match, so the announcement is not authentic.
    #[error("digest mismatch: the announcement is not authentic")]
    DigestMismatch,
    /// The digest has been seen before; this is a replay.
    #[error("digest replay: this announcement was already processed")]
    Replay,
}

/// A `_shippairing._tcp` announcement.
///
/// Field order is load-bearing: the digest is computed over a concatenation in exactly
/// the order below (Pairing Service §7.4), and the specification is explicit that
/// deviating from it — including reordering — is not permitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    /// The SHIP ID of the device being paired *to* (`devA`).
    pub for_id: String,
    /// `devA`'s certificate fingerprint, uppercase hex of the SHA-256 of the DER.
    pub for_par: String,
    /// The SHIP ID of the device requesting the pairing (`devZ`).
    pub trust_id: String,
    /// `devZ`'s certificate fingerprint.
    pub trust_par: String,
    /// The elliptic curve the trust applies to.
    pub trust_curve: String,
    /// A 128-bit nonce chosen by `devZ`, uppercase hex.
    pub trust_nonce: String,
}

impl PairingRequest {
    /// The message the digest is computed over.
    ///
    /// ```
    /// use eebus::ship::pairing::PairingRequest;
    ///
    /// // Annex A.3 of the SHIP Pairing Service specification.
    /// let request = PairingRequest {
    ///     for_id: "i:983327_u:C8277H008F-3".into(),
    ///     for_par: "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943".into(),
    ///     trust_id: "i:46925_u:43652bk-2-gt1".into(),
    ///     trust_par: "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4".into(),
    ///     trust_curve: "secp256r1".into(),
    ///     trust_nonce: "BDCEE427FA7208DF3C1F2A749BA6F4D4".into(),
    /// };
    /// assert!(request.digest_message().starts_with("txtvers=1;parType=fpSha256;"));
    /// assert!(request.digest_message().ends_with(";alg=hmacSha256;"));
    /// ```
    pub fn digest_message(&self) -> String {
        let mut m = String::with_capacity(320);
        m.push_str("txtvers=");
        m.push_str(super::TXTVERS);
        m.push_str(";parType=");
        m.push_str(PAR_TYPE_FP_SHA256);
        m.push_str(";forId=");
        m.push_str(&self.for_id);
        m.push_str(";forPar=");
        m.push_str(&self.for_par);
        m.push_str(";trustId=");
        m.push_str(&self.trust_id);
        m.push_str(";trustPar=");
        m.push_str(&self.trust_par);
        m.push_str(";trustCurve=");
        m.push_str(&self.trust_curve);
        m.push_str(";type=");
        m.push_str(TYPE_ADD_CU);
        m.push_str(";trustNonce=");
        m.push_str(&self.trust_nonce);
        m.push_str(";alg=");
        m.push_str(ALG_HMAC_SHA256);
        m.push(';');
        m
    }

    /// Computes the `digest` field.
    ///
    /// The HMAC key is `devA`'s secret followed by `devZ`'s nonce, as raw bytes
    /// (Pairing Service §7.3); the result is uppercase hex.
    pub fn digest(&self, secret: &PairingSecret) -> Result<String, PairingError> {
        self.validate()?;
        let mut key = Vec::with_capacity(secret.as_bytes().len() + 16);
        key.extend_from_slice(secret.as_bytes());
        key.extend_from_slice(&decode_hex(&self.trust_nonce)?);

        let mut mac =
            <Hmac<Sha256>>::new_from_slice(&key).expect("HMAC accepts a key of any length");
        mac.update(self.digest_message().as_bytes());
        let out = encode_hex_upper(&mac.finalize().into_bytes());

        key.iter_mut().for_each(|b| *b = 0);
        Ok(out)
    }

    /// Checks a received announcement against the secret, in constant time.
    ///
    /// The `guard` records accepted digests so that a captured announcement replayed
    /// later is refused (Pairing Service §11).
    pub fn verify(
        &self,
        secret: &PairingSecret,
        digest: &str,
        guard: &mut ReplayGuard,
    ) -> Result<(), PairingError> {
        if digest.len() != 64 || !is_upper_hex(digest) {
            return Err(PairingError::InvalidField {
                field: "digest",
                expected: "64 uppercase hexadecimal digits",
            });
        }
        let expected = self.digest(secret)?;
        if !constant_time_eq(expected.as_bytes(), digest.as_bytes()) {
            return Err(PairingError::DigestMismatch);
        }
        if !guard.accept(digest) {
            return Err(PairingError::Replay);
        }
        Ok(())
    }

    /// Renders the TXT record, in the order the specification fixes.
    pub fn to_pairs(&self, digest: &str) -> Vec<(String, String)> {
        alloc::vec![
            ("txtvers".to_string(), super::TXTVERS.to_string()),
            ("parType".to_string(), PAR_TYPE_FP_SHA256.to_string()),
            ("forId".to_string(), self.for_id.clone()),
            ("forPar".to_string(), self.for_par.clone()),
            ("trustId".to_string(), self.trust_id.clone()),
            ("trustPar".to_string(), self.trust_par.clone()),
            ("trustCurve".to_string(), self.trust_curve.clone()),
            ("type".to_string(), TYPE_ADD_CU.to_string()),
            ("trustNonce".to_string(), self.trust_nonce.clone()),
            ("alg".to_string(), ALG_HMAC_SHA256.to_string()),
            ("digest".to_string(), digest.to_string()),
        ]
    }

    /// Checks the format rules the specification calls "load-bearing for security":
    /// fingerprints and nonces of an exact length, in uppercase, so that a malformed
    /// record cannot carry a digest that happens to verify.
    fn validate(&self) -> Result<(), PairingError> {
        for (field, value, len) in [
            ("forPar", &self.for_par, 64),
            ("trustPar", &self.trust_par, 64),
            ("trustNonce", &self.trust_nonce, 32),
        ] {
            if value.len() != len || !is_upper_hex(value) {
                return Err(PairingError::InvalidField {
                    field,
                    expected: if len == 64 {
                        "64 uppercase hexadecimal digits"
                    } else {
                        "32 uppercase hexadecimal digits"
                    },
                });
            }
        }
        if self.trust_curve != CURVE_SECP256R1
            && self.trust_curve != "brainpoolP256r1"
            && self.trust_curve != "brainpoolP384r1"
        {
            return Err(PairingError::Unsupported {
                field: "trustCurve",
                value: self.trust_curve.clone(),
            });
        }
        Ok(())
    }
}

/// Remembers recently accepted digests so that none is honoured twice.
///
/// The specification calls for a ring buffer that survives a restart; this type holds
/// the ring, and an application that wants persistence saves and restores
/// [`ReplayGuard::entries`].
#[derive(Clone, Debug)]
pub struct ReplayGuard {
    entries: Vec<String>,
    capacity: usize,
}

impl ReplayGuard {
    /// A guard remembering the last `capacity` digests.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    /// Restores a guard from persisted entries.
    pub fn from_entries(entries: Vec<String>, capacity: usize) -> Self {
        let mut guard = Self::new(capacity);
        guard.entries = entries;
        guard.truncate();
        guard
    }

    /// The remembered digests, oldest first, for persisting.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Records `digest`, returning `false` if it was already known.
    pub fn accept(&mut self, digest: &str) -> bool {
        if self.entries.iter().any(|e| e == digest) {
            return false;
        }
        self.entries.push(digest.to_string());
        self.truncate();
        true
    }

    fn truncate(&mut self) {
        while self.entries.len() > self.capacity {
            self.entries.remove(0);
        }
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new(64)
    }
}

fn is_upper_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, PairingError> {
    let cleaned: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.len() % 2 != 0 {
        return Err(PairingError::InvalidHex);
    }
    cleaned
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char)
                .to_digit(16)
                .ok_or(PairingError::InvalidHex)?;
            let lo = (pair[1] as char)
                .to_digit(16)
                .ok_or(PairingError::InvalidHex)?;
            Ok(((hi << 4) | lo) as u8)
        })
        .collect()
}

fn encode_hex_upper(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(ALPHABET[usize::from(b >> 4)] as char);
        out.push(ALPHABET[usize::from(b & 0x0f)] as char);
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Annex A of the SHIP Pairing Service specification, worked through end to end.
    fn annex_a() -> (PairingRequest, PairingSecret) {
        (
            PairingRequest {
                for_id: "i:983327_u:C8277H008F-3".into(),
                for_par: "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943".into(),
                trust_id: "i:46925_u:43652bk-2-gt1".into(),
                trust_par: "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4"
                    .into(),
                trust_curve: CURVE_SECP256R1.into(),
                trust_nonce: "BDCEE427FA7208DF3C1F2A749BA6F4D4".into(),
            },
            PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap(),
        )
    }

    #[test]
    fn digest_message_matches_the_specification_verbatim() {
        let (request, _) = annex_a();
        assert_eq!(
            request.digest_message(),
            "txtvers=1;parType=fpSha256;forId=i:983327_u:C8277H008F-3;\
             forPar=C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943;\
             trustId=i:46925_u:43652bk-2-gt1;\
             trustPar=2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4;\
             trustCurve=secp256r1;type=addCu;trustNonce=BDCEE427FA7208DF3C1F2A749BA6F4D4;\
             alg=hmacSha256;"
        );
    }

    #[test]
    fn digest_matches_the_specification_test_vector() {
        let (request, secret) = annex_a();
        assert_eq!(
            request.digest(&secret).unwrap(),
            "BCBB62B2176DA2CEE545784CEB1F2A55E049451B12A549C98E8CA213F001DA25"
        );
    }

    #[test]
    fn verification_accepts_the_test_vector_once() {
        let (request, secret) = annex_a();
        let digest = request.digest(&secret).unwrap();
        let mut guard = ReplayGuard::default();

        assert_eq!(request.verify(&secret, &digest, &mut guard), Ok(()));
        assert_eq!(
            request.verify(&secret, &digest, &mut guard),
            Err(PairingError::Replay),
            "a captured announcement must not be honoured twice"
        );
    }

    #[test]
    fn a_wrong_secret_is_refused() {
        let (request, _) = annex_a();
        let wrong = PairingSecret::from_hex("00000000000000000000000000000000").unwrap();
        let digest = "BCBB62B2176DA2CEE545784CEB1F2A55E049451B12A549C98E8CA213F001DA25";
        assert_eq!(
            request.verify(&wrong, digest, &mut ReplayGuard::default()),
            Err(PairingError::DigestMismatch)
        );
    }

    /// A substituted certificate changes `trustPar`, which the digest covers, so the
    /// announcement no longer verifies. This is what stops a man in the middle.
    #[test]
    fn a_substituted_certificate_breaks_the_digest() {
        let (mut request, secret) = annex_a();
        let digest = request.digest(&secret).unwrap();
        request.trust_par =
            "0000000000000000000000000000000000000000000000000000000000000000".into();
        assert_eq!(
            request.verify(&secret, &digest, &mut ReplayGuard::default()),
            Err(PairingError::DigestMismatch)
        );
    }

    #[test]
    fn malformed_fields_are_rejected_before_any_comparison() {
        let (mut request, secret) = annex_a();
        request.trust_nonce = request.trust_nonce.to_lowercase();
        assert_eq!(
            request.digest(&secret),
            Err(PairingError::InvalidField {
                field: "trustNonce",
                expected: "32 uppercase hexadecimal digits",
            })
        );
    }

    #[test]
    fn the_txt_record_keeps_the_prescribed_order() {
        let (request, secret) = annex_a();
        let digest = request.digest(&secret).unwrap();
        let pairs = request.to_pairs(&digest);
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "txtvers",
                "parType",
                "forId",
                "forPar",
                "trustId",
                "trustPar",
                "trustCurve",
                "type",
                "trustNonce",
                "alg",
                "digest"
            ]
        );
    }

    #[test]
    fn the_replay_guard_forgets_the_oldest_entries() {
        let mut guard = ReplayGuard::new(2);
        assert!(guard.accept("A"));
        assert!(guard.accept("B"));
        assert!(guard.accept("C"));
        assert_eq!(guard.entries(), ["B", "C"]);
        assert!(guard.accept("A"), "evicted digests may be seen again");
    }

    #[test]
    fn secrets_do_not_print_themselves() {
        let secret = PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap();
        assert_eq!(alloc::format!("{secret:?}"), "PairingSecret(<redacted>)");
    }
}
