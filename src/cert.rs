//! Certificates: making, storing and restoring a node's own, and reading a peer's
//! identity out of one.
//!
//! SHIP has no public key infrastructure and no certificate authority. Every node signs
//! its own certificate, and a peer decides whether to trust it by comparing the
//! certificate's Subject Key Identifier against a [`Ski`] the user has seen — on a label,
//! in a QR code, or through the Pairing Service. That makes three things load-bearing,
//! and this module exists so that none of them can be got wrong by accident:
//!
//! * **The curve is secp256r1** (SHIP §9.3 makes it a SHALL). A certificate on another
//!   curve cannot complete the mandatory cipher suites, and a peer may not even probe for
//!   it.
//! * **The SKI is the SHA-1 of the public key** — RFC 5280 method 1, not the truncated
//!   SHA-256 that most certificate tooling now defaults to. A certificate carrying the
//!   wrong kind of key identifier still verifies as TLS, and then fails to match the SKI
//!   printed on the device.
//! * **The extension has to be present.** SHIP §12.2 lets a peer fall back to computing
//!   the digest itself, but a certificate that omits the extension is one a
//!   specification-following peer may reject outright.
//!
//! A node makes its identity once with [`self_signed`], stores
//! [`Identity::certificate_pem`] and [`Identity::key_pem`], and restores both with
//! [`Identity::from_pem`] on every later start.
//!
//! Requires the `cert` feature.
//!
//! ```
//! use eebus::cert::{self, CertParams};
//!
//! let identity = cert::self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();
//!
//! // The SKI is what goes on the label, in the QR code and in the mDNS record.
//! println!("{}", identity.ski.to_display_string());
//!
//! // And a peer reads the same value back out of the certificate it is sent.
//! assert_eq!(cert::ski_from_der(identity.certificate_der()).unwrap(), identity.ski);
//! ```

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ship::{Fingerprint, Ski};

/// The signature algorithm SHIP requires: ECDSA on secp256r1 with SHA-256.
const ALGORITHM: &rcgen::SignatureAlgorithm = &rcgen::PKCS_ECDSA_P256_SHA256;

/// The object identifier of the Subject Key Identifier extension, 2.5.29.14.
const OID_SUBJECT_KEY_IDENTIFIER: &[u64] = &[2, 5, 29, 14];

/// DER tag for an `OCTET STRING`.
const DER_OCTET_STRING: u8 = 0x04;

/// How PEM is written: the configuration `rcgen` uses, so a certificate's PEM is the same
/// text whether it was just signed or restored.
const PEM_ENCODING: pem::EncodeConfig =
    pem::EncodeConfig::new().set_line_ending(if cfg!(target_family = "windows") {
        pem::LineEnding::CRLF
    } else {
        pem::LineEnding::LF
    });

/// Why a certificate could not be made, read or restored.
///
/// No variant carries the bytes it was given: a refused private key must not end up in a
/// log because its error was printed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CertError {
    /// The certificate could not be generated.
    #[error("generating the certificate failed: {0}")]
    Generate(#[from] rcgen::Error),
    /// The bytes were not a DER-encoded X.509 certificate.
    #[error("the certificate could not be parsed: {0}")]
    Parse(String),
    /// The certificate's key is not on secp256r1, which SHIP §9.3 requires.
    #[error("a SHIP certificate uses secp256r1; this one does not")]
    WrongCurve,
    /// The PEM text held no certificate.
    #[error("no certificate found in the PEM input")]
    NoCertificate,
    /// The bytes were not a PKCS#8 private key.
    #[error("the private key could not be parsed: {0}")]
    InvalidKey(String),
    /// The PEM text held no `PRIVATE KEY` block. Only PKCS#8 is read — the form
    /// [`Identity::key_pem`] writes — so a SEC1 `EC PRIVATE KEY` block lands here too.
    #[error("no PKCS#8 private key (a `PRIVATE KEY` block) found in the PEM input")]
    NoPrivateKey,
    /// The private key is not the one the certificate was issued for.
    #[error("the private key does not belong to the certificate")]
    KeyMismatch,
    /// The certificate's Subject Key Identifier is not the SHA-1 of its public key
    /// (RFC 5280 §4.2.1.2 method 1), so peers would read a SKI this node does not have.
    #[error("the certificate's subject key identifier is not the SHA-1 of its public key")]
    SkiMismatch,
}

/// What a certificate is to say about the node it identifies.
#[derive(Clone, Debug)]
pub struct CertParams {
    /// The common name, which SHIP fills with the node's SHIP ID.
    pub common_name: String,
    /// The organisation, if the manufacturer wants one in the subject.
    pub organization: Option<String>,
    /// The country code, if the manufacturer wants one in the subject.
    pub country: Option<String>,
    /// How long the certificate is valid, in days from now.
    ///
    /// SHIP places no upper bound and a peer accepts an expired certificate — the trust
    /// decision is the SKI, not a validity window — so this is long by default. A device
    /// that cannot tell the time cannot renew, and a heat pump installed for twenty years
    /// should not stop talking to the grid because a date passed.
    pub validity_days: u32,
}

impl CertParams {
    /// Parameters for a node with this SHIP ID.
    pub fn new(ship_id: impl Into<String>) -> Self {
        Self {
            common_name: ship_id.into(),
            organization: None,
            country: None,
            validity_days: 365 * 50,
        }
    }

    /// Sets the organisation in the subject.
    pub fn organization(mut self, organization: impl Into<String>) -> Self {
        self.organization = Some(organization.into());
        self
    }

    /// Sets the two-letter country code in the subject.
    pub fn country(mut self, country: impl Into<String>) -> Self {
        self.country = Some(country.into());
        self
    }

    /// Sets how long the certificate is valid, in days from now.
    pub fn validity_days(mut self, days: u32) -> Self {
        self.validity_days = days;
        self
    }
}

/// A node's certificate and the private key that goes with it.
///
/// The key is never printed by [`Debug`]: `key_pem` is the one field in this crate whose
/// leak would let another device impersonate this one to the grid operator.
pub struct Identity {
    /// The SKI, which is this node's identity everywhere else in SHIP.
    pub ski: Ski,
    /// DER, exactly as issued: restoring an identity has to give back these bytes, and
    /// `rcgen` can only make a certificate by signing a new one.
    certificate: Vec<u8>,
    key: rcgen::KeyPair,
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Identity")
            .field("ski", &self.ski)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Restores a stored identity: the certificate byte for byte, and its key.
    ///
    /// This is the restart path. The node keeps its SKI *and* its fingerprint, which the
    /// Pairing Service trusts (SHIP §10.2) and the QR code carries as `FPH256`. Reads what
    /// [`certificate_pem`](Self::certificate_pem) and [`key_pem`](Self::key_pem) write;
    /// the first `CERTIFICATE` and the first `PRIVATE KEY` (PKCS#8) block are used.
    ///
    /// ```
    /// use eebus::cert::{self, CertParams, Identity};
    ///
    /// // First boot: make an identity and store both halves.
    /// let identity = cert::self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();
    /// let (certificate, key) = (identity.certificate_pem(), identity.key_pem());
    ///
    /// // Every boot after that: restore them.
    /// let restored = Identity::from_pem(&certificate, &key).unwrap();
    /// assert_eq!(restored.ski, identity.ski);
    /// assert_eq!(restored.fingerprint(), identity.fingerprint());
    /// ```
    ///
    /// # Errors
    ///
    /// Refuses, in this order: text that holds no certificate ([`CertError::Parse`],
    /// [`CertError::NoCertificate`]) or no PKCS#8 key ([`CertError::NoPrivateKey`],
    /// [`CertError::InvalidKey`]); a key or certificate that is not on secp256r1
    /// ([`CertError::WrongCurve`], SHIP §9.3); a key that is not the certificate's
    /// ([`CertError::KeyMismatch`]); and a certificate whose Subject Key Identifier is not
    /// the SHA-1 of its key ([`CertError::SkiMismatch`]). It never panics, and no error
    /// carries the input.
    pub fn from_pem(certificate_pem: &str, key_pem: &str) -> Result<Identity, CertError> {
        let certificate =
            first_block(certificate_pem, "CERTIFICATE")?.ok_or(CertError::NoCertificate)?;
        let key = first_block(key_pem, "PRIVATE KEY")?.ok_or(CertError::NoPrivateKey)?;
        Self::from_der(certificate.contents(), key.contents())
    }

    /// Restores a stored identity from the DER forms
    /// [`certificate_der`](Self::certificate_der) and [`key_der`](Self::key_der) wrote.
    ///
    /// Everything [`from_pem`](Self::from_pem) says applies; this is the same restore
    /// without the PEM wrapping.
    ///
    /// # Errors
    ///
    /// As [`from_pem`](Self::from_pem), except that there is no PEM to be missing a block.
    pub fn from_der(certificate_der: &[u8], key_der: &[u8]) -> Result<Identity, CertError> {
        use x509_parser::prelude::*;

        let (_, certificate) = X509Certificate::from_der(certificate_der)
            .map_err(|e| CertError::Parse(format!("{e}")))?;
        let key = key_from_der(key_der)?;
        if !is_ship_curve(&certificate) {
            return Err(CertError::WrongCurve);
        }
        // The same public-key bits the SKI is computed from on both sides, so a match here
        // is "the key's SKI is the certificate's SKI".
        let public_key = &certificate.public_key().subject_public_key.data;
        if public_key.as_ref() != rcgen::PublicKeyData::der_bytes(&key) {
            return Err(CertError::KeyMismatch);
        }
        let ski = ski_from_public_key(&key);
        // Of any length: one peer reading a 20-byte identifier and another falling back to
        // the digest would each see a different node.
        if ski_extension(&certificate).is_some_and(|id| id != ski.as_bytes()) {
            return Err(CertError::SkiMismatch);
        }
        Ok(Identity {
            ski,
            certificate: certificate_der.to_vec(),
            key,
        })
    }

    /// The certificate in DER form, which is what TLS sends.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }

    /// The SHA-256 fingerprint of the certificate: what the Pairing Service trusts, and
    /// what goes into the QR code as `FPH256`.
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_der(&self.certificate)
    }

    /// The certificate in PEM form, for storing on disk.
    pub fn certificate_pem(&self) -> String {
        pem::encode_config(
            &pem::Pem::new("CERTIFICATE", self.certificate.clone()),
            PEM_ENCODING,
        )
    }

    /// The private key in PKCS#8 DER form.
    ///
    /// Store it where only this device can read it. Anything holding this key is this
    /// node as far as every peer is concerned.
    pub fn key_der(&self) -> Vec<u8> {
        self.key.serialize_der()
    }

    /// The private key in PKCS#8 PEM form.
    pub fn key_pem(&self) -> String {
        self.key.serialize_pem()
    }
}

/// Generates a **new identity**: a fresh key, and a self-signed certificate over it.
///
/// The result satisfies everything SHIP asks of a node certificate: secp256r1, a SHA-1
/// Subject Key Identifier, and the SHIP ID as the common name.
///
/// # Call this once, at first boot
///
/// A fresh key is a new SKI, so every peer that approved the old one stops recognising
/// the device. Call this once, store [`Identity::certificate_pem`] and
/// [`Identity::key_pem`], and restore both with [`Identity::from_pem`] on every later
/// start. [`self_signed_with`] keeps only the SKI and is for a deliberate re-issue.
pub fn self_signed(params: CertParams) -> Result<Identity, CertError> {
    let key = rcgen::KeyPair::generate_for(ALGORITHM)?;
    self_signed_with(params, key)
}

/// Re-issues a self-signed certificate from an existing key.
///
/// For changing a certificate on purpose: a longer validity, a corrected common name.
/// The SKI follows the key and survives. **The fingerprint does not**: a peer that
/// trusted it through the Pairing Service (SHIP §10.2) needs the §12.1.3 certificate
/// update. To restart, restore with [`Identity::from_pem`] instead.
pub fn self_signed_with(params: CertParams, key: rcgen::KeyPair) -> Result<Identity, CertError> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyIdMethod,
        KeyUsagePurpose,
    };

    let ski = ski_from_public_key(&key);

    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, params.common_name.clone());
    if let Some(organization) = &params.organization {
        name.push(DnType::OrganizationName, organization.clone());
    }
    if let Some(country) = &params.country {
        name.push(DnType::CountryName, country.clone());
    }

    let now = time::OffsetDateTime::now_utc();
    let mut certificate = CertificateParams::default();
    certificate.distinguished_name = name;
    certificate.not_before = now - time::Duration::days(1);
    certificate.not_after = now + time::Duration::days(i64::from(params.validity_days));
    certificate.is_ca = IsCa::NoCa;
    certificate.key_usages = alloc::vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyAgreement,
    ];
    certificate.extended_key_usages = alloc::vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    // Both ends of a SHIP connection authenticate, and either may dial, so one
    // certificate has to serve as both a server's and a client's.
    certificate.key_identifier_method = KeyIdMethod::PreSpecified(ski.as_bytes().to_vec());
    certificate.custom_extensions = alloc::vec![subject_key_identifier_extension(&ski)];

    let certificate = certificate.self_signed(&key)?;
    Ok(Identity {
        ski,
        certificate: certificate.der().to_vec(),
        key,
    })
}

/// The Subject Key Identifier extension, written by hand.
///
/// `rcgen` emits this extension only for certificates marked as a certificate authority,
/// and a SHIP node certificate is a leaf. Without it a peer has to fall back to computing
/// the digest itself — which SHIP §12.2 permits, but which a specification-following peer
/// may equally decline to do, leaving a node that cannot be identified at all.
///
/// The value is `SubjectKeyIdentifier ::= KeyIdentifier ::= OCTET STRING`, so the content
/// is the twenty SHA-1 bytes wrapped in one DER octet string; `rcgen` wraps that in the
/// extension's own octet string in turn. Non-critical, as RFC 5280 §4.2.1.2 requires.
fn subject_key_identifier_extension(ski: &Ski) -> rcgen::CustomExtension {
    let mut content = Vec::with_capacity(Ski::LEN + 2);
    content.push(DER_OCTET_STRING);
    content.push(Ski::LEN as u8);
    content.extend_from_slice(ski.as_bytes());
    rcgen::CustomExtension::from_oid_content(OID_SUBJECT_KEY_IDENTIFIER, content)
}

/// Reads a private key back from the PKCS#8 DER [`Identity::key_der`] produced.
///
/// Pair it with [`self_signed_with`] to re-issue a certificate without changing the
/// node's identity.
pub fn key_from_der(der: &[u8]) -> Result<rcgen::KeyPair, CertError> {
    let key = rcgen::KeyPair::try_from(der).map_err(|e| CertError::InvalidKey(e.to_string()))?;
    on_ship_curve(key)
}

/// Reads a private key back from the PKCS#8 PEM [`Identity::key_pem`] produced.
///
/// The first block labelled `PRIVATE KEY` is used, and any other block is skipped, so a
/// file that also carries parameters or a certificate still reads. A SEC1
/// `EC PRIVATE KEY` block is not read: [`CertError::NoPrivateKey`].
pub fn key_from_pem(pem: &str) -> Result<rcgen::KeyPair, CertError> {
    key_from_der(
        first_block(pem, "PRIVATE KEY")?
            .ok_or(CertError::NoPrivateKey)?
            .contents(),
    )
}

/// The first PEM block with this label, if the text is PEM at all.
fn first_block(text: &str, label: &str) -> Result<Option<pem::Pem>, CertError> {
    let blocks = pem::parse_many(text).map_err(|e| CertError::Parse(e.to_string()))?;
    Ok(blocks.into_iter().find(|block| block.tag() == label))
}

/// Refuses a key that is not on secp256r1.
///
/// A key on another curve loads perfectly well and then produces a node no peer can
/// complete a handshake with, which is a much harder failure to read than this one.
fn on_ship_curve(key: rcgen::KeyPair) -> Result<rcgen::KeyPair, CertError> {
    if key.algorithm() == ALGORITHM {
        Ok(key)
    } else {
        Err(CertError::WrongCurve)
    }
}

/// The SKI of a key pair: the SHA-1 of its public key, per RFC 5280 method 1.
pub fn ski_from_public_key(key: &impl rcgen::PublicKeyData) -> Ski {
    ski_of(key.der_bytes())
}

/// Reads the SKI out of a DER-encoded certificate.
///
/// The Subject Key Identifier extension is used where it is present. Where it is not,
/// the digest is computed from the public key, which SHIP §12.2 permits and which is what
/// lets a device whose certificate predates this rule still be identified.
pub fn ski_from_der(der: &[u8]) -> Result<Ski, CertError> {
    use x509_parser::prelude::*;

    let (_, certificate) =
        X509Certificate::from_der(der).map_err(|e| CertError::Parse(format!("{e}")))?;

    if let Some(id) = ski_extension(&certificate)
        && let Ok(bytes) = <[u8; Ski::LEN]>::try_from(id)
    {
        return Ok(Ski::from_bytes(bytes));
    }

    Ok(ski_of(&certificate.public_key().subject_public_key.data))
}

/// The value of a certificate's Subject Key Identifier extension, of whatever length.
fn ski_extension<'a>(
    certificate: &'a x509_parser::certificate::X509Certificate<'_>,
) -> Option<&'a [u8]> {
    use x509_parser::extensions::ParsedExtension;

    let extension = certificate
        .get_extension_unique(&x509_parser::oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
        .ok()??;
    match extension.parsed_extension() {
        ParsedExtension::SubjectKeyIdentifier(id) => Some(id.0),
        _ => None,
    }
}

/// Reads the first certificate out of PEM text and returns its DER.
pub fn der_from_pem(pem: &str) -> Result<Vec<u8>, CertError> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| CertError::Parse(format!("{e}")))?;
    if parsed.label != "CERTIFICATE" {
        return Err(CertError::NoCertificate);
    }
    Ok(parsed.contents)
}

/// Checks that a certificate's key is on the curve SHIP requires.
///
/// A peer offering another curve cannot complete the cipher suites SHIP mandates, so this
/// is worth checking before a connection is attempted rather than after it fails.
pub fn uses_ship_curve(der: &[u8]) -> Result<(), CertError> {
    use x509_parser::prelude::*;

    let (_, certificate) =
        X509Certificate::from_der(der).map_err(|e| CertError::Parse(format!("{e}")))?;
    if is_ship_curve(&certificate) {
        Ok(())
    } else {
        Err(CertError::WrongCurve)
    }
}

/// Whether a parsed certificate's key is on secp256r1.
fn is_ship_curve(certificate: &x509_parser::certificate::X509Certificate<'_>) -> bool {
    matches!(
        certificate.public_key().parsed(),
        Ok(x509_parser::public_key::PublicKey::EC(point)) if point.key_size() == 256
    )
}

/// SHA-1 of a public key's bits.
fn ski_of(public_key: &[u8]) -> Ski {
    use sha1::{Digest, Sha1};

    let digest = Sha1::digest(public_key);
    let mut bytes = [0u8; Ski::LEN];
    bytes.copy_from_slice(&digest);
    Ski::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_certificate_carries_the_ski_a_peer_will_look_for() {
        let identity = self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();

        // The extension is present and holds the SHA-1, not a truncated SHA-256.
        assert_eq!(
            ski_from_der(identity.certificate_der()).unwrap(),
            identity.ski
        );
        assert_eq!(identity.ski.to_string().len(), 40);
    }

    /// RFC 5280 method 1, computed independently of how the certificate was built.
    ///
    /// The test above is circular on its own: it reads back the extension this crate
    /// wrote. This one derives the digest from the certificate's raw `subjectPublicKey`
    /// bits the way a peer that finds no extension would, and requires the two to agree.
    /// If they ever diverge, every device that computes the fallback sees a different
    /// SKI from the one on this node's label.
    #[test]
    fn the_extension_matches_what_a_peer_would_compute_for_itself() {
        use x509_parser::prelude::*;

        let identity = self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();
        let (_, certificate) = X509Certificate::from_der(identity.certificate_der()).unwrap();

        let computed = ski_of(&certificate.public_key().subject_public_key.data);
        assert_eq!(
            computed, identity.ski,
            "the SKI in the extension is not the SHA-1 of the public key bits"
        );

        let extension = certificate
            .get_extension_unique(&x509_parser::oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
            .unwrap()
            .expect("SHIP 12.2: the extension is present");
        let ParsedExtension::SubjectKeyIdentifier(id) = extension.parsed_extension() else {
            panic!("expected a subject key identifier");
        };
        assert_eq!(id.0.len(), 20, "SHA-1, not a truncated SHA-256");
        assert_eq!(id.0, identity.ski.as_bytes());
    }

    #[test]
    fn the_certificate_is_on_the_curve_ship_requires() {
        let identity = self_signed(CertParams::new("i:46925_u:Device")).unwrap();
        assert!(uses_ship_curve(identity.certificate_der()).is_ok());
    }

    #[test]
    fn the_ski_follows_the_key_not_the_certificate() {
        // A re-issue keeps the node's identity, which is what lets a device renew its
        // certificate without every peer having to be told about it again.
        let first = self_signed(CertParams::new("i:46925_u:Device")).unwrap();
        let key = key_from_der(&first.key_der()).unwrap();
        let reissued =
            self_signed_with(CertParams::new("i:46925_u:Device").validity_days(30), key).unwrap();

        assert_eq!(reissued.ski, first.ski);
        assert_ne!(reissued.certificate_der(), first.certificate_der());
    }

    #[test]
    fn two_nodes_do_not_share_an_identity() {
        let a = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        let b = self_signed(CertParams::new("i:46925_u:B")).unwrap();
        assert_ne!(a.ski, b.ski);
    }

    #[test]
    fn a_key_round_trips_through_pem_too() {
        let identity = self_signed(CertParams::new("i:46925_u:Device")).unwrap();
        let key = key_from_pem(&identity.key_pem()).unwrap();
        assert_eq!(ski_from_public_key(&key), identity.ski);
    }

    // ---- restoring a stored identity (issue #5) ----------------------------------------

    /// What a device that stored its identity at first boot gets back on every later
    /// start: the same certificate, so the same fingerprint the Pairing Service trusts.
    fn assert_same_identity(restored: &Identity, original: &Identity) {
        assert_eq!(restored.ski, original.ski);
        assert_eq!(restored.fingerprint(), original.fingerprint());
        assert_eq!(restored.certificate_der(), original.certificate_der());
        assert_eq!(restored.certificate_pem(), original.certificate_pem());
        assert_eq!(restored.key_der(), original.key_der());
    }

    #[test]
    fn restoring_from_pem_gives_back_the_same_certificate() {
        let original = self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();
        let restored =
            Identity::from_pem(&original.certificate_pem(), &original.key_pem()).unwrap();
        assert_same_identity(&restored, &original);
    }

    #[test]
    fn restoring_from_der_gives_back_the_same_certificate() {
        let original = self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();
        let restored = Identity::from_der(original.certificate_der(), &original.key_der()).unwrap();
        assert_same_identity(&restored, &original);
    }

    /// The defect issue #5 reports, as two lines: a re-issue keeps the SKI and moves the
    /// fingerprint, and a restore keeps both.
    #[test]
    fn a_re_issue_changes_the_fingerprint_and_a_restore_does_not() {
        let original = self_signed(CertParams::new("i:46925_u:HeatPump-1")).unwrap();

        let reissued = self_signed_with(
            CertParams::new("i:46925_u:HeatPump-1"),
            key_from_pem(&original.key_pem()).unwrap(),
        )
        .unwrap();
        assert_eq!(reissued.ski, original.ski, "the SKI follows the key");
        assert_ne!(
            reissued.fingerprint(),
            original.fingerprint(),
            "the fingerprint follows the certificate"
        );

        let restored =
            Identity::from_pem(&original.certificate_pem(), &original.key_pem()).unwrap();
        assert_eq!(restored.fingerprint(), original.fingerprint());
    }

    /// A self-signed P-256 certificate over `key`, shaped by `shape`: the way another
    /// tool would have made one, so the restore can be shown what it has to refuse.
    fn foreign_certificate(
        key: &rcgen::KeyPair,
        shape: impl FnOnce(&mut rcgen::CertificateParams),
    ) -> Vec<u8> {
        let mut params = rcgen::CertificateParams::new(alloc::vec![]).unwrap();
        shape(&mut params);
        params.self_signed(key).unwrap().der().to_vec()
    }

    fn p256() -> rcgen::KeyPair {
        rcgen::KeyPair::generate_for(ALGORITHM).unwrap()
    }

    fn p384() -> rcgen::KeyPair {
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap()
    }

    #[test]
    fn a_key_from_another_identity_is_refused() {
        let a = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        let b = self_signed(CertParams::new("i:46925_u:B")).unwrap();
        assert!(matches!(
            Identity::from_pem(&a.certificate_pem(), &b.key_pem()),
            Err(CertError::KeyMismatch)
        ));
        assert!(matches!(
            Identity::from_der(a.certificate_der(), &b.key_der()),
            Err(CertError::KeyMismatch)
        ));
    }

    #[test]
    fn a_certificate_on_another_curve_is_refused() {
        let key = p384();
        let certificate = foreign_certificate(&key, |_| {});
        let ship_key = self_signed(CertParams::new("i:46925_u:A"))
            .unwrap()
            .key_der();
        assert!(matches!(
            Identity::from_der(&certificate, &ship_key),
            Err(CertError::WrongCurve)
        ));
    }

    #[test]
    fn a_key_on_another_curve_is_refused() {
        let identity = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        assert!(matches!(
            Identity::from_der(identity.certificate_der(), &p384().serialize_der()),
            Err(CertError::WrongCurve)
        ));
    }

    /// Tooling that writes RFC 7093's truncated SHA-256 (or anything else) as the key
    /// identifier makes a certificate peers would read a SKI out of that is not this
    /// node's. It verifies as TLS, which is exactly why it has to be refused here.
    #[test]
    fn a_certificate_whose_ski_is_not_its_keys_is_refused() {
        let key = p256();
        let certificate = foreign_certificate(&key, |params| {
            params.custom_extensions = alloc::vec![subject_key_identifier_extension(
                &Ski::from_bytes([0xAA; Ski::LEN])
            )];
        });
        assert!(matches!(
            Identity::from_der(&certificate, &key.serialize_der()),
            Err(CertError::SkiMismatch)
        ));
    }

    #[test]
    fn swapped_arguments_are_refused() {
        let identity = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        assert!(matches!(
            Identity::from_pem(&identity.key_pem(), &identity.certificate_pem()),
            Err(CertError::NoCertificate)
        ));
        assert!(matches!(
            Identity::from_pem(&identity.certificate_pem(), &identity.certificate_pem()),
            Err(CertError::NoPrivateKey)
        ));
    }

    #[test]
    fn an_ec_private_key_block_is_refused() {
        let identity = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        let sec1_labelled = identity.key_pem().replace("PRIVATE KEY", "EC PRIVATE KEY");
        assert!(matches!(
            Identity::from_pem(&identity.certificate_pem(), &sec1_labelled),
            Err(CertError::NoPrivateKey)
        ));
        assert!(matches!(
            key_from_pem(&sec1_labelled),
            Err(CertError::NoPrivateKey)
        ));
    }

    #[test]
    fn garbage_is_refused() {
        let identity = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        assert!(matches!(
            Identity::from_der(b"not a certificate", &identity.key_der()),
            Err(CertError::Parse(_))
        ));
        assert!(matches!(
            Identity::from_der(identity.certificate_der(), b"not a key"),
            Err(CertError::InvalidKey(_))
        ));
        assert!(matches!(
            Identity::from_pem("", &identity.key_pem()),
            Err(CertError::NoCertificate)
        ));
        assert!(matches!(
            key_from_der(b"not a key"),
            Err(CertError::InvalidKey(_))
        ));
    }

    /// A refused key must not reach a log by way of its error message.
    #[test]
    fn no_refusal_quotes_its_input() {
        let a = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        let b = self_signed(CertParams::new("i:46925_u:B")).unwrap();
        let key_pem = b.key_pem();
        let secret = key_pem.lines().nth(1).unwrap();
        let truncated = &key_pem[..key_pem.len() / 2];
        let errors = [
            Identity::from_pem(&a.certificate_pem(), &key_pem).unwrap_err(),
            Identity::from_pem(&a.certificate_pem(), truncated).unwrap_err(),
            Identity::from_pem(&key_pem, &key_pem).unwrap_err(),
            Identity::from_der(a.certificate_der(), &b.key_der()[..20]).unwrap_err(),
        ];
        for error in errors {
            let text = alloc::format!("{error} {error:?}");
            for window in secret.as_bytes().windows(16) {
                let window = core::str::from_utf8(window).unwrap();
                assert!(!text.contains(window), "{text} quotes the key");
            }
        }
    }

    // ---- the edge cases a restore has to accept --------------------------------------

    /// SHIP has no certificate authority and peers do not judge validity, so an expired
    /// certificate is still this node's; renewing it is a re-issue, not a refusal to boot.
    #[test]
    fn an_expired_certificate_is_restored() {
        let key = p256();
        let certificate = foreign_certificate(&key, |params| {
            params.not_before = rcgen::date_time_ymd(2000, 1, 1);
            params.not_after = rcgen::date_time_ymd(2001, 1, 1);
        });
        let restored = Identity::from_der(&certificate, &key.serialize_der()).unwrap();
        assert_eq!(restored.certificate_der(), certificate);
    }

    /// §12.2 lets a peer compute the digest when the extension is missing, and what it
    /// computes is the key's SKI, so there is nothing to disagree with.
    #[test]
    fn a_certificate_without_a_ski_extension_is_restored() {
        let key = p256();
        let certificate = foreign_certificate(&key, |_| {});
        let restored = Identity::from_der(&certificate, &key.serialize_der()).unwrap();
        assert_eq!(restored.ski, ski_from_public_key(&key));
        assert_eq!(ski_from_der(&certificate).unwrap(), restored.ski);
    }

    #[test]
    fn the_first_certificate_and_the_first_private_key_are_used() {
        let ours = self_signed(CertParams::new("i:46925_u:A")).unwrap();
        let other = self_signed(CertParams::new("i:46925_u:B")).unwrap();
        let chain = alloc::format!("{}{}", ours.certificate_pem(), other.certificate_pem());
        let key = alloc::format!(
            "-----BEGIN EC PARAMETERS-----\nBggqhkjOPQMBBw==\n-----END EC PARAMETERS-----\n{}",
            ours.key_pem()
        );
        let restored = Identity::from_pem(&chain, &key).unwrap();
        assert_eq!(restored.fingerprint(), ours.fingerprint());
    }

    #[test]
    fn a_certificate_round_trips_through_pem() {
        let identity = self_signed(CertParams::new("i:46925_u:Device")).unwrap();
        let der = der_from_pem(&identity.certificate_pem()).unwrap();
        assert_eq!(der, identity.certificate_der());
        assert_eq!(ski_from_der(&der).unwrap(), identity.ski);
    }
}
