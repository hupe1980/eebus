//! The SHIP Pairing Service: automated trust from a scanned secret.
//!
//! Plain SHIP asks a person to compare a forty-digit SKI on two screens. The Pairing
//! Service (TS 1.0.0) replaces that with a secret printed alongside the QR code: the
//! device being added — `devZ`, a control unit — proves it knows the secret by announcing
//! a `_shippairing._tcp` record whose `digest` is an HMAC over the record's own fields.
//! The device it is being added to — `devA`, the energy manager — recomputes the HMAC and,
//! if it matches, trusts the announced certificate without further interaction.
//!
//! Two properties make that safe, and both are implemented here:
//!
//! * the digest covers the certificate **fingerprints** of both devices, so a
//!   man-in-the-middle cannot substitute its own certificate; and
//! * a nonce chosen by the requesting device, together with a [`ReplayGuard`], stops a
//!   captured announcement from being replayed later.
//!
//! # The two ends
//!
//! [`Receiver`] is `devA`'s end: given the node's own SHIP ID, fingerprint and secret, it
//! evaluates a TXT record by the rules of §9 and hands back the request to trust.
//! [`Requester`] is `devZ`'s end: it decides *when* the announcement is on the air — up
//! from the moment it is configured, down once one connection has held for fifteen
//! minutes (§4.2). Neither touches a socket or a clock: the
//! [`runtime::Hub`](crate::runtime::Hub) drives the receiver, and an mDNS responder
//! carries out what the requester asks for.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;
use core::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::{Fingerprint, constant_time_eq};

/// The DNS-SD service type of the pairing service.
pub const SERVICE_TYPE: &str = super::PAIRING_SERVICE_TYPE;

/// The only digest algorithm this version of the specification defines.
pub const ALG_HMAC_SHA256: &str = "hmacSha256";

/// The only parameter type: a SHA-256 fingerprint of the DER certificate.
pub const PAR_TYPE_FP_SHA256: &str = "fpSha256";

/// The only request type: add a control unit.
pub const TYPE_ADD_CU: &str = "addCu";

/// The elliptic curve the trust applies to.
pub const CURVE_SECP256R1: &str = "secp256r1";

/// The curves §5.4 permits in `trustCurve`.
const CURVES: [&str; 3] = [CURVE_SECP256R1, "brainpoolP256r1", "brainpoolP384r1"];

/// How long a SHIP connection has to hold before `devZ` withdraws its request (§4.2).
pub const SETTLED_AFTER: Duration = Duration::from_secs(15 * 60);

/// How long `devA` has to have been unable to reach its control unit before it will
/// consider another one (§4.3, rule 1a).
pub const REPLACEABLE_AFTER: Duration = Duration::from_secs(15 * 60);

/// A pairing secret, printed with the QR code as `SPSEC`.
///
/// The type refuses to print itself and zeroes its storage when dropped, so a secret
/// cannot reach a log through a stray `{:?}`, and it compares in constant time so that
/// `a == b` cannot be turned into a search.
#[derive(Clone)]
pub struct PairingSecret(Vec<u8>);

impl PartialEq for PairingSecret {
    /// Constant time over the shared prefix, and no earlier exit than the length check.
    ///
    /// A derived comparison stops at the first differing byte, which turns equality into
    /// an oracle: an attacker who can ask "is the secret this?" learns it one byte at a
    /// time. That the secret is short is what makes it worth defending rather than what
    /// makes it safe.
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(&self.0, &other.0)
    }
}

impl Eq for PairingSecret {}

impl PairingSecret {
    /// Wraps raw secret bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Reads the hex form used in the `SPSEC` field of a QR code.
    pub fn from_hex(hex: &str) -> Result<Self, PairingError> {
        Ok(Self(decode_hex(hex)?))
    }

    /// The hex form for a QR code, uppercase.
    pub fn to_hex(&self) -> String {
        encode_hex_upper(&self.0)
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

impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret(<redacted>)")
    }
}

/// The 128-bit nonce `devZ` chooses for each request (§5.4, `trustNonce`).
///
/// §6.3 asks for a cryptographic random number generator behind it, and §5.4 forbids
/// deriving it from the secret in any way. The runtime's [`crate::tls::random`] is the
/// generator this crate offers; the type itself only carries the value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Nonce([u8; Nonce::LEN]);

impl Nonce {
    /// The length in bytes.
    pub const LEN: usize = 16;

    /// Wraps raw bytes.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl FromStr for Nonce {
    type Err = PairingError;

    /// Reads the wire form: 32 uppercase hexadecimal digits, nothing else.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || PairingError::InvalidField {
            field: "trustNonce",
            expected: "32 uppercase hexadecimal digits",
        };
        if s.len() != Self::LEN * 2 || !is_upper_hex(s) {
            return Err(invalid());
        }
        let bytes = decode_hex(s).map_err(|_| invalid())?;
        let mut out = [0u8; Self::LEN];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode_hex_upper(&self.0))
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Nonce({self})")
    }
}

/// Why a pairing announcement could not be built, read or verified.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// A hex-encoded value was malformed.
    #[error("invalid hexadecimal value")]
    InvalidHex,
    /// A mandatory TXT key was absent (§5.4).
    #[error("the TXT record has no `{field}` key")]
    Missing {
        /// The absent key.
        field: &'static str,
    },
    /// A field did not have the length, case or content the specification fixes.
    #[error("`{field}` must be {expected}")]
    InvalidField {
        /// The offending TXT key.
        field: &'static str,
        /// What the specification requires.
        expected: &'static str,
    },
    /// An algorithm, parameter type, curve or request type this version does not define.
    #[error("unsupported `{field}` value `{value}`")]
    Unsupported {
        /// The offending TXT key.
        field: &'static str,
        /// The value that was rejected.
        value: String,
    },
    /// The request names another `devA` (§9 step 1), and is nothing to this node.
    #[error("the request is addressed to another node")]
    NotForThisNode,
    /// The digest did not match, so the announcement is not authentic.
    #[error("digest mismatch: the announcement is not authentic")]
    DigestMismatch,
    /// The digest has been seen before; this is a replay (§9 step 2, §11).
    #[error("digest replay: this announcement was already processed")]
    Replay,
}

/// A `_shippairing._tcp` request, before it is signed.
///
/// Field order is load-bearing: the digest is computed over a concatenation in exactly
/// the order below (§7.4), and the specification is explicit that deviating from it —
/// including reordering — is not permitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingRequest {
    /// The SHIP ID of the device being paired *to* (`devA`).
    pub for_id: String,
    /// `devA`'s certificate fingerprint.
    pub for_par: Fingerprint,
    /// The SHIP ID of the device requesting the pairing (`devZ`).
    pub trust_id: String,
    /// `devZ`'s certificate fingerprint.
    pub trust_par: Fingerprint,
    /// The elliptic curve of `devZ`'s key: one of `secp256r1`, `brainpoolP256r1`,
    /// `brainpoolP384r1`.
    pub trust_curve: String,
    /// The nonce `devZ` chose for this request.
    pub trust_nonce: Nonce,
}

impl PairingRequest {
    /// A request from `devZ` to be trusted by `devA`, over a `secp256r1` certificate.
    ///
    /// `for_id` and `for_par` come off `devA`'s QR code (`ID` and `FPH256`); the other
    /// two are `devZ`'s own; the nonce is fresh for every request (§5.4).
    pub fn new(
        for_id: impl Into<String>,
        for_par: Fingerprint,
        trust_id: impl Into<String>,
        trust_par: Fingerprint,
        trust_nonce: Nonce,
    ) -> Self {
        Self {
            for_id: for_id.into(),
            for_par,
            trust_id: trust_id.into(),
            trust_par,
            trust_curve: CURVE_SECP256R1.into(),
            trust_nonce,
        }
    }

    /// The message the digest is computed over (§7.4).
    ///
    /// ```
    /// use eebus::ship::pairing::PairingRequest;
    ///
    /// // Annex A.3 of the SHIP Pairing Service specification.
    /// let request = PairingRequest::new(
    ///     "i:983327_u:C8277H008F-3",
    ///     "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943".parse().unwrap(),
    ///     "i:46925_u:43652bk-2-gt1",
    ///     "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4".parse().unwrap(),
    ///     "BDCEE427FA7208DF3C1F2A749BA6F4D4".parse().unwrap(),
    /// );
    /// assert!(request.digest_message().starts_with("txtvers=1;parType=fpSha256;"));
    /// assert!(request.digest_message().ends_with(";alg=hmacSha256;"));
    /// ```
    pub fn digest_message(&self) -> String {
        let mut m = String::with_capacity(320);
        for (key, value) in self.static_pairs() {
            m.push_str(key);
            m.push('=');
            m.push_str(&value);
            m.push(';');
        }
        m
    }

    /// Computes the `digest` field.
    ///
    /// The HMAC key is `devA`'s secret followed by `devZ`'s nonce, as raw bytes (§7.3);
    /// the result is uppercase hex.
    ///
    /// # Errors
    ///
    /// A `trust_curve` outside §5.4's three; nothing else in the request can be
    /// malformed, because the typed fields cannot hold a malformed value.
    pub fn digest(&self, secret: &PairingSecret) -> Result<String, PairingError> {
        self.validate()?;
        let mut key = Vec::with_capacity(secret.as_bytes().len() + Nonce::LEN);
        key.extend_from_slice(secret.as_bytes());
        key.extend_from_slice(self.trust_nonce.as_bytes());

        let mut mac =
            <Hmac<Sha256>>::new_from_slice(&key).expect("HMAC accepts a key of any length");
        mac.update(self.digest_message().as_bytes());
        let out = encode_hex_upper(&mac.finalize().into_bytes());

        key.iter_mut().for_each(|b| *b = 0);
        Ok(out)
    }

    /// Signs the request: what `devZ` announces (chapter 8).
    pub fn sign(&self, secret: &PairingSecret) -> Result<PairingAnnouncement, PairingError> {
        Ok(PairingAnnouncement {
            digest: self.digest(secret)?,
            request: self.clone(),
        })
    }

    /// Checks a received digest against the secret, in constant time (§9 step 3).
    ///
    /// This is the authenticity check alone; [`Receiver::evaluate`] is the whole of §9,
    /// replay guard included.
    pub fn verify_digest(&self, secret: &PairingSecret, digest: &str) -> Result<(), PairingError> {
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
        Ok(())
    }

    /// Checks a received announcement against the secret and the replay guard.
    ///
    /// §9 in order: a digest the `guard` already holds is refused before anything is
    /// computed, the digest is then verified in constant time, and only a digest that
    /// verified is recorded.
    pub fn verify(
        &self,
        secret: &PairingSecret,
        digest: &str,
        guard: &mut ReplayGuard,
    ) -> Result<(), PairingError> {
        if guard.contains(digest) {
            return Err(PairingError::Replay);
        }
        self.verify_digest(secret, digest)?;
        guard.accept(digest);
        Ok(())
    }

    /// The static keys, in the order §5.4 fixes.
    fn static_pairs(&self) -> [(&'static str, String); 10] {
        [
            ("txtvers", super::TXTVERS.to_string()),
            ("parType", PAR_TYPE_FP_SHA256.to_string()),
            ("forId", self.for_id.clone()),
            ("forPar", self.for_par.to_hex()),
            ("trustId", self.trust_id.clone()),
            ("trustPar", self.trust_par.to_hex()),
            ("trustCurve", self.trust_curve.clone()),
            ("type", TYPE_ADD_CU.to_string()),
            ("trustNonce", self.trust_nonce.to_string()),
            ("alg", ALG_HMAC_SHA256.to_string()),
        ]
    }

    fn validate(&self) -> Result<(), PairingError> {
        if self.for_id.is_empty() {
            return Err(PairingError::InvalidField {
                field: "forId",
                expected: "a SHIP ID",
            });
        }
        if self.trust_id.is_empty() {
            return Err(PairingError::InvalidField {
                field: "trustId",
                expected: "a SHIP ID",
            });
        }
        if !CURVES.contains(&self.trust_curve.as_str()) {
            return Err(PairingError::Unsupported {
                field: "trustCurve",
                value: self.trust_curve.clone(),
            });
        }
        Ok(())
    }
}

/// A signed request: the TXT record `devZ` puts on the air, and what `devA` reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingAnnouncement {
    /// The request.
    pub request: PairingRequest,
    /// Its digest, uppercase hex.
    pub digest: String,
}

impl PairingAnnouncement {
    /// Renders the TXT record, in the order the specification fixes.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .request
            .static_pairs()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect();
        pairs.push(("digest".to_string(), self.digest.clone()));
        pairs
    }

    /// Reads a TXT record by the rules of §5.4.
    ///
    /// `txtvers` must come first and be `1`; every mandatory key must be present with a
    /// permitted value; keys the specification does not name are ignored, as it asks.
    /// The digest is *not* verified here — that needs the secret, which is
    /// [`Receiver`]'s.
    pub fn from_pairs(pairs: &[(String, String)]) -> Result<Self, PairingError> {
        match pairs.first() {
            Some((key, value)) if key == "txtvers" && value == super::TXTVERS => {}
            _ => {
                return Err(PairingError::InvalidField {
                    field: "txtvers",
                    expected: "the first key, with the value 1",
                });
            }
        }
        let get = |field: &'static str| -> Result<&str, PairingError> {
            pairs
                .iter()
                .find(|(key, _)| key == field)
                .map(|(_, value)| value.as_str())
                .ok_or(PairingError::Missing { field })
        };
        let exactly = |field: &'static str, expected: &str| -> Result<(), PairingError> {
            let value = get(field)?;
            if value == expected {
                Ok(())
            } else {
                Err(PairingError::Unsupported {
                    field,
                    value: value.to_string(),
                })
            }
        };
        let fingerprint = |field: &'static str| -> Result<Fingerprint, PairingError> {
            get(field)?.parse().map_err(|_| PairingError::InvalidField {
                field,
                expected: "64 uppercase hexadecimal digits",
            })
        };
        let id = |field: &'static str| -> Result<String, PairingError> {
            let value = get(field)?;
            if value.is_empty() {
                return Err(PairingError::InvalidField {
                    field,
                    expected: "a SHIP ID",
                });
            }
            Ok(value.to_string())
        };

        exactly("parType", PAR_TYPE_FP_SHA256)?;
        exactly("type", TYPE_ADD_CU)?;
        exactly("alg", ALG_HMAC_SHA256)?;
        let trust_curve = get("trustCurve")?.to_string();
        if !CURVES.contains(&trust_curve.as_str()) {
            return Err(PairingError::Unsupported {
                field: "trustCurve",
                value: trust_curve,
            });
        }
        let digest = get("digest")?.to_string();
        if digest.len() != 64 || !is_upper_hex(&digest) {
            return Err(PairingError::InvalidField {
                field: "digest",
                expected: "64 uppercase hexadecimal digits",
            });
        }

        Ok(Self {
            request: PairingRequest {
                for_id: id("forId")?,
                for_par: fingerprint("forPar")?,
                trust_id: id("trustId")?,
                trust_par: fingerprint("trustPar")?,
                trust_curve,
                trust_nonce: get("trustNonce")?.parse()?,
            },
            digest,
        })
    }
}

/// Remembers accepted digests so that none is honoured twice: the ring buffer of §11.
///
/// The specification wants at least the last ten kept, and kept across a restart; an
/// application persists [`entries`](Self::entries) and restores them with
/// [`from_entries`](Self::from_entries).
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

    /// Whether `digest` has been accepted before.
    pub fn contains(&self, digest: &str) -> bool {
        self.entries.iter().any(|e| e == digest)
    }

    /// Records `digest`, returning `false` if it was already known.
    pub fn accept(&mut self, digest: &str) -> bool {
        if self.contains(digest) {
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

/// `devA`'s end: evaluates the requests the network announces (§9).
///
/// Holds what evaluation needs — the node's own SHIP ID and fingerprint, so a request
/// for some other device is recognised as none of this node's business; the secret;
/// and the replay guard. What it does *not* hold is the policy of §4.3 — whether the node
/// is currently taking requests at all, which depends on whether its current control unit
/// is reachable — because that needs the connection table, and lives in the
/// [`Hub`](crate::runtime::Hub).
///
/// ```
/// use eebus::ship::pairing::{PairingRequest, PairingSecret, Receiver};
///
/// // Annex A: devA's own identity and secret …
/// let secret = PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap();
/// let for_par = "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943".parse().unwrap();
/// let mut receiver = Receiver::new("i:983327_u:C8277H008F-3", for_par, secret.clone());
///
/// // … and devZ's request, as it would arrive off the air.
/// let request = PairingRequest::new(
///     "i:983327_u:C8277H008F-3",
///     for_par,
///     "i:46925_u:43652bk-2-gt1",
///     "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4".parse().unwrap(),
///     "BDCEE427FA7208DF3C1F2A749BA6F4D4".parse().unwrap(),
/// );
/// let pairs = request.sign(&secret).unwrap().to_pairs();
///
/// let accepted = receiver.evaluate(&pairs).unwrap();
/// assert_eq!(accepted.trust_id, "i:46925_u:43652bk-2-gt1");
/// assert!(receiver.evaluate(&pairs).is_err(), "and never the same announcement twice");
/// ```
#[derive(Debug)]
pub struct Receiver {
    ship_id: String,
    fingerprint: Fingerprint,
    secret: PairingSecret,
    guard: ReplayGuard,
}

impl Receiver {
    /// A receiver for the node with this SHIP ID and certificate, holding this secret.
    pub fn new(
        ship_id: impl Into<String>,
        fingerprint: Fingerprint,
        secret: PairingSecret,
    ) -> Self {
        Self {
            ship_id: ship_id.into(),
            fingerprint,
            secret,
            guard: ReplayGuard::default(),
        }
    }

    /// Restores the replay guard persisted from a previous run.
    #[must_use]
    pub fn with_guard(mut self, guard: ReplayGuard) -> Self {
        self.guard = guard;
        self
    }

    /// The replay guard, whose entries are what to persist after an acceptance.
    pub fn guard(&self) -> &ReplayGuard {
        &self.guard
    }

    /// Evaluates one announced TXT record: §9, steps 1 to 4.
    ///
    /// A request addressed to another node is [`PairingError::NotForThisNode`], which is
    /// not a fault and not worth a log line on a network with several energy managers.
    /// Anything else in `Err` is a request that *was* for this node and could not be
    /// honoured — an installer who mistyped the secret gets
    /// [`PairingError::DigestMismatch`].
    pub fn evaluate(&mut self, pairs: &[(String, String)]) -> Result<PairingRequest, PairingError> {
        // Step 1, the cheap half first: a record for some other devA is not looked at
        // closely, and not reported either.
        let addressed = pairs
            .iter()
            .any(|(key, value)| key == "forId" && *value == self.ship_id);
        if !addressed {
            return Err(PairingError::NotForThisNode);
        }
        let announcement = PairingAnnouncement::from_pairs(pairs)?;
        if announcement.request.for_par != self.fingerprint {
            return Err(PairingError::NotForThisNode);
        }
        // Steps 2 to 4.
        announcement
            .request
            .verify(&self.secret, &announcement.digest, &mut self.guard)?;
        Ok(announcement.request)
    }
}

/// What a [`Requester`] wants done with its announcement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequesterAction {
    /// Put the request on the air.
    Announce,
    /// Take it off, and do not put it back.
    Withdraw,
}

/// `devZ`'s end: when the request is on the air (§4.2).
///
/// The request goes up **as soon as it is configured**, and that ordering is the whole
/// point: `devA` cannot trust `devZ` until it has heard the request, so a `devZ` that
/// waited for a working SHIP connection before announcing would be waiting for something
/// its own silence prevents. §4.2 note 2 says as much from the other side — "as long as
/// devA does not trust devZ, devZ might need to repeat SHIP connection attempts".
///
/// What a connection decides is when the request comes *down*: it stays up across
/// interruptions, and is withdrawn for good — reboots included — once one uninterrupted
/// connection has lasted [`SETTLED_AFTER`]. An interruption before that restarts the clock
/// from zero rather than resuming it (§4.2 note 1).
///
/// Actions are **polled**, like everything else sans-IO in this crate, so the opening
/// announcement cannot be missed by a caller that only reacts to events. Drive it with an
/// mDNS responder the application owns:
///
/// ```
/// use core::time::Duration;
/// use eebus::ship::pairing::{Requester, RequesterAction, SETTLED_AFTER};
/// # use eebus::ship::pairing::{PairingRequest, PairingSecret};
/// # let request = PairingRequest::new(
/// #     "i:983327_u:C8277H008F-3",
/// #     "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943".parse().unwrap(),
/// #     "i:46925_u:43652bk-2-gt1",
/// #     "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4".parse().unwrap(),
/// #     "BDCEE427FA7208DF3C1F2A749BA6F4D4".parse().unwrap(),
/// # );
/// # let secret = PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap();
/// # let announcement = request.sign(&secret).unwrap();
///
/// let mut requester = Requester::new(announcement);
/// // On the air immediately, before any connection exists.
/// assert_eq!(requester.poll_action(), Some(RequesterAction::Announce));
/// assert_eq!(requester.poll_action(), None, "and only once");
///
/// // A connection starts the clock; losing it stops it, and the request stays up.
/// requester.on_connected(Duration::ZERO);
/// requester.on_disconnected();
/// requester.handle_timeout(Duration::from_secs(60 * 60));
/// assert_eq!(requester.poll_action(), None, "an interrupted connection settles nothing");
///
/// // Fifteen uninterrupted minutes from the reconnect, and it comes down for good.
/// requester.on_connected(Duration::from_secs(60));
/// requester.handle_timeout(Duration::from_secs(60) + SETTLED_AFTER);
/// assert_eq!(requester.poll_action(), Some(RequesterAction::Withdraw));
/// assert!(requester.is_settled());
/// ```
#[derive(Debug)]
pub struct Requester {
    announcement: PairingAnnouncement,
    state: RequesterState,
    /// The action the caller has not yet carried out.
    pending: Option<RequesterAction>,
}

#[derive(Debug, PartialEq, Eq)]
enum RequesterState {
    /// On the air; `since` is when the current connection began, if one is up.
    Announcing { since: Option<Duration> },
    /// Withdrawn for good.
    Settled,
}

impl Requester {
    /// A configured request, on the air from this moment.
    ///
    /// The first [`poll_action`](Self::poll_action) returns [`RequesterAction::Announce`];
    /// nothing is on the wire until the caller acts on it.
    pub fn new(announcement: PairingAnnouncement) -> Self {
        Self {
            announcement,
            state: RequesterState::Announcing { since: None },
            pending: Some(RequesterAction::Announce),
        }
    }

    /// A requester whose pairing completed in an earlier run: it will never announce.
    ///
    /// §4.2 is explicit that a settled request does not come back "after reboot or other
    /// kinds of connection interruptions", so a device that has recorded the pairing as
    /// complete constructs this rather than [`new`](Self::new).
    pub fn settled(announcement: PairingAnnouncement) -> Self {
        Self {
            announcement,
            state: RequesterState::Settled,
            pending: None,
        }
    }

    /// The signed request.
    pub fn announcement(&self) -> &PairingAnnouncement {
        &self.announcement
    }

    /// The next thing for the caller to do with its mDNS responder, if anything.
    pub fn poll_action(&mut self) -> Option<RequesterAction> {
        self.pending.take()
    }

    /// Whether the request is on the air.
    pub fn is_announcing(&self) -> bool {
        matches!(self.state, RequesterState::Announcing { .. })
    }

    /// Whether the request has been withdrawn for good.
    pub fn is_settled(&self) -> bool {
        self.state == RequesterState::Settled
    }

    /// A SHIP connection with `devA` reached the data phase: the clock starts.
    ///
    /// Called again on a later connection, it restarts from zero — which is what §4.2
    /// note 1 asks for, and why an unstable link never settles the request.
    pub fn on_connected(&mut self, now: Duration) {
        if let RequesterState::Announcing { since } = &mut self.state {
            *since = Some(now);
        }
    }

    /// The SHIP connection with `devA` ended. The announcement stays up; the clock stops.
    pub fn on_disconnected(&mut self) {
        if let RequesterState::Announcing { since } = &mut self.state {
            *since = None;
        }
    }

    /// When [`handle_timeout`](Self::handle_timeout) should next be called, if ever.
    pub fn poll_timeout(&self) -> Option<Duration> {
        match self.state {
            RequesterState::Announcing { since: Some(since) } => Some(since + SETTLED_AFTER),
            _ => None,
        }
    }

    /// Advances the clock; settles the request once a connection has held long enough.
    pub fn handle_timeout(&mut self, now: Duration) {
        if let RequesterState::Announcing { since: Some(since) } = self.state
            && now >= since + SETTLED_AFTER
        {
            self.settle();
        }
    }

    /// Withdraws at once: the administrator has untrusted `devA`, which §4.2 requires to
    /// take the request down immediately.
    pub fn withdraw(&mut self) {
        self.settle();
    }

    fn settle(&mut self) {
        let was_up = self.is_announcing();
        self.state = RequesterState::Settled;
        self.pending = was_up.then_some(RequesterAction::Withdraw);
    }
}

fn is_upper_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, PairingError> {
    let cleaned: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !cleaned.len().is_multiple_of(2) {
        return Err(PairingError::InvalidHex);
    }
    cleaned
        .as_chunks::<2>()
        .0
        .iter()
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

#[cfg(test)]
mod tests {
    use super::*;

    const DEV_A_ID: &str = "i:983327_u:C8277H008F-3";
    const DEV_A_PAR: &str = "C74B7855D3479415F62CC01E5F6D9A93EBC676057D85417ADA16FD1384338943";
    const DEV_Z_ID: &str = "i:46925_u:43652bk-2-gt1";
    const DEV_Z_PAR: &str = "2CC72E781F7A7D2A08D50196C50FEDF0F7BA583F43F76C8C0DDEC9EEF0D005B4";
    const DIGEST: &str = "BCBB62B2176DA2CEE545784CEB1F2A55E049451B12A549C98E8CA213F001DA25";

    /// Annex A of the SHIP Pairing Service specification, worked through end to end.
    fn annex_a() -> (PairingRequest, PairingSecret) {
        (
            PairingRequest::new(
                DEV_A_ID,
                DEV_A_PAR.parse().unwrap(),
                DEV_Z_ID,
                DEV_Z_PAR.parse().unwrap(),
                "BDCEE427FA7208DF3C1F2A749BA6F4D4".parse().unwrap(),
            ),
            PairingSecret::from_hex("7A37DCF81BDB50F8E92CFA4160CCB3DE").unwrap(),
        )
    }

    fn receiver() -> Receiver {
        let (_, secret) = annex_a();
        Receiver::new(DEV_A_ID, DEV_A_PAR.parse().unwrap(), secret)
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
        assert_eq!(request.digest(&secret).unwrap(), DIGEST);
    }

    #[test]
    fn verification_accepts_the_test_vector_once() {
        let (request, secret) = annex_a();
        let mut guard = ReplayGuard::default();

        assert_eq!(request.verify(&secret, DIGEST, &mut guard), Ok(()));
        assert_eq!(
            request.verify(&secret, DIGEST, &mut guard),
            Err(PairingError::Replay),
            "a captured announcement must not be honoured twice"
        );
    }

    #[test]
    fn a_wrong_secret_is_refused() {
        let (request, _) = annex_a();
        let wrong = PairingSecret::from_hex("00000000000000000000000000000000").unwrap();
        assert_eq!(
            request.verify(&wrong, DIGEST, &mut ReplayGuard::default()),
            Err(PairingError::DigestMismatch)
        );
    }

    /// A substituted certificate changes `trustPar`, which the digest covers, so the
    /// announcement no longer verifies. This is what stops a man in the middle.
    #[test]
    fn a_substituted_certificate_breaks_the_digest() {
        let (mut request, secret) = annex_a();
        request.trust_par = Fingerprint::from_bytes([0; 32]);
        assert_eq!(
            request.verify(&secret, DIGEST, &mut ReplayGuard::default()),
            Err(PairingError::DigestMismatch)
        );
    }

    #[test]
    fn a_malformed_digest_is_rejected_before_any_comparison() {
        let (request, secret) = annex_a();
        assert_eq!(
            request.verify_digest(&secret, &DIGEST.to_lowercase()),
            Err(PairingError::InvalidField {
                field: "digest",
                expected: "64 uppercase hexadecimal digits",
            })
        );
    }

    #[test]
    fn the_txt_record_keeps_the_prescribed_order_and_reads_back() {
        let (request, secret) = annex_a();
        let announcement = request.sign(&secret).unwrap();
        let pairs = announcement.to_pairs();
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
        assert_eq!(PairingAnnouncement::from_pairs(&pairs), Ok(announcement));
    }

    /// §5.4: `txtvers` first, unknown keys ignored, missing or malformed keys invalid.
    #[test]
    fn a_txt_record_is_read_by_the_rules_of_the_specification() {
        let (request, secret) = annex_a();
        let good = request.sign(&secret).unwrap().to_pairs();

        let mut shuffled = good.clone();
        shuffled.swap(0, 1);
        assert_eq!(
            PairingAnnouncement::from_pairs(&shuffled).unwrap_err(),
            PairingError::InvalidField {
                field: "txtvers",
                expected: "the first key, with the value 1"
            }
        );

        let mut extended = good.clone();
        extended.push(("future".into(), "whatever".into()));
        assert!(
            PairingAnnouncement::from_pairs(&extended).is_ok(),
            "keys this version does not name are ignored"
        );

        let missing: Vec<_> = good
            .iter()
            .filter(|(k, _)| k != "trustNonce")
            .cloned()
            .collect();
        assert_eq!(
            PairingAnnouncement::from_pairs(&missing).unwrap_err(),
            PairingError::Missing {
                field: "trustNonce"
            }
        );

        let lowered: Vec<_> = good
            .iter()
            .map(|(k, v)| {
                if k == "trustPar" {
                    (k.clone(), v.to_lowercase())
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        assert_eq!(
            PairingAnnouncement::from_pairs(&lowered).unwrap_err(),
            PairingError::InvalidField {
                field: "trustPar",
                expected: "64 uppercase hexadecimal digits"
            }
        );

        let other_alg: Vec<_> = good
            .iter()
            .map(|(k, v)| {
                if k == "alg" {
                    (k.clone(), "hmacSha512".to_string())
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        assert_eq!(
            PairingAnnouncement::from_pairs(&other_alg).unwrap_err(),
            PairingError::Unsupported {
                field: "alg",
                value: "hmacSha512".into()
            }
        );
    }

    /// §9 step 1: a request for another node is nobody's business here, whatever its
    /// state; one for this node with the wrong secret is a refusal worth reporting.
    #[test]
    fn a_receiver_evaluates_by_chapter_nine() {
        let (request, secret) = annex_a();
        let mut receiver = receiver();

        let mut elsewhere = request.clone();
        elsewhere.for_id = "i:983327_u:SomeoneElse".into();
        let pairs = elsewhere.sign(&secret).unwrap().to_pairs();
        assert_eq!(receiver.evaluate(&pairs), Err(PairingError::NotForThisNode));

        let mut other_certificate = request.clone();
        other_certificate.for_par = Fingerprint::from_bytes([1; 32]);
        let pairs = other_certificate.sign(&secret).unwrap().to_pairs();
        assert_eq!(
            receiver.evaluate(&pairs),
            Err(PairingError::NotForThisNode),
            "the fingerprint identifies the certificate, not only the SHIP ID"
        );

        let wrong = PairingSecret::from_hex("00000000000000000000000000000000").unwrap();
        let pairs = request.sign(&wrong).unwrap().to_pairs();
        assert_eq!(receiver.evaluate(&pairs), Err(PairingError::DigestMismatch));
        assert!(
            receiver.guard().entries().is_empty(),
            "a digest that did not verify is not remembered"
        );

        let pairs = request.sign(&secret).unwrap().to_pairs();
        assert_eq!(receiver.evaluate(&pairs), Ok(request));
        assert_eq!(receiver.guard().entries(), [DIGEST]);
        assert_eq!(receiver.evaluate(&pairs), Err(PairingError::Replay));
    }

    /// §4.2: on the air from configuration — *not* from the first connection, which is
    /// the ordering that matters, since `devA` cannot trust `devZ` until it has heard the
    /// request — across interruptions, until one connection has held fifteen minutes.
    #[test]
    fn a_requester_announces_from_configuration_until_a_connection_has_held() {
        let (request, secret) = annex_a();
        let mut requester = Requester::new(request.sign(&secret).unwrap());
        let t = |s: u64| Duration::from_secs(s);

        assert!(
            requester.is_announcing(),
            "the request is up before any connection exists: waiting for one would be \
             waiting for something its own silence prevents"
        );
        assert_eq!(requester.poll_action(), Some(RequesterAction::Announce));
        assert_eq!(requester.poll_action(), None, "and only once");
        assert_eq!(
            requester.poll_timeout(),
            None,
            "with no connection there is nothing to time"
        );

        requester.on_connected(t(0));
        assert_eq!(requester.poll_timeout(), Some(SETTLED_AFTER));

        // Interrupted after ten minutes: still announcing, and the clock is stopped.
        requester.on_disconnected();
        assert!(requester.is_announcing());
        assert_eq!(requester.poll_timeout(), None);
        requester.handle_timeout(t(20 * 60));
        assert_eq!(requester.poll_action(), None, "nothing settled");

        // Reconnected: a new fifteen minutes, from now, not a resumed ten.
        requester.on_connected(t(20 * 60));
        assert_eq!(requester.poll_timeout(), Some(t(20 * 60) + SETTLED_AFTER));
        requester.handle_timeout(t(30 * 60));
        assert_eq!(requester.poll_action(), None);
        requester.handle_timeout(t(35 * 60));
        assert_eq!(requester.poll_action(), Some(RequesterAction::Withdraw));
        assert!(requester.is_settled());

        requester.on_connected(t(36 * 60));
        assert_eq!(requester.poll_action(), None, "never again");
        assert!(!requester.is_announcing());
    }

    /// §4.2: a request that settled in an earlier run does not come back after a reboot.
    #[test]
    fn a_settled_request_never_announces_again() {
        let (request, secret) = annex_a();
        let mut requester = Requester::settled(request.sign(&secret).unwrap());
        assert!(requester.is_settled());
        assert_eq!(requester.poll_action(), None);
        requester.on_connected(Duration::ZERO);
        requester.handle_timeout(Duration::from_secs(3_600));
        assert_eq!(requester.poll_action(), None);
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
        assert_eq!(secret.to_hex(), "7A37DCF81BDB50F8E92CFA4160CCB3DE");
    }
}
