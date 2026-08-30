//! Certificates: making a node's own, and reading a peer's identity out of one.
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

use crate::ship::Ski;

/// The signature algorithm SHIP requires: ECDSA on secp256r1 with SHA-256.
const ALGORITHM: &rcgen::SignatureAlgorithm = &rcgen::PKCS_ECDSA_P256_SHA256;

/// The object identifier of the Subject Key Identifier extension, 2.5.29.14.
const OID_SUBJECT_KEY_IDENTIFIER: &[u64] = &[2, 5, 29, 14];

/// DER tag for an `OCTET STRING`.
const DER_OCTET_STRING: u8 = 0x04;

/// Why a certificate could not be made or read.
#[derive(Debug, thiserror::Error)]
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
    certificate: rcgen::Certificate,
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
    /// The certificate in DER form, which is what TLS sends.
    pub fn certificate_der(&self) -> &[u8] {
        self.certificate.der()
    }

    /// The certificate in PEM form, for storing on disk.
    pub fn certificate_pem(&self) -> String {
        self.certificate.pem()
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

/// Generates a self-signed certificate and the key that signs it.
///
/// The result satisfies everything SHIP asks of a node certificate: secp256r1, a SHA-1
/// Subject Key Identifier, and the SHIP ID as the common name.
pub fn self_signed(params: CertParams) -> Result<Identity, CertError> {
    let key = rcgen::KeyPair::generate_for(ALGORITHM)?;
    self_signed_with(params, key)
}

/// Generates a self-signed certificate from an existing key.
///
/// Use this to re-issue a certificate — a longer validity, a corrected common name —
/// without changing the node's identity: the SKI follows the key, so a re-issue from the
/// same key keeps every trust relationship the node has established.
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
        certificate,
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
    on_ship_curve(rcgen::KeyPair::try_from(der).map_err(CertError::Generate)?)
}

/// Reads a private key back from the PKCS#8 PEM [`Identity::key_pem`] produced.
pub fn key_from_pem(pem: &str) -> Result<rcgen::KeyPair, CertError> {
    on_ship_curve(rcgen::KeyPair::from_pem(pem).map_err(CertError::Generate)?)
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

    if let Ok(Some(extension)) =
        certificate.get_extension_unique(&oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
        && let ParsedExtension::SubjectKeyIdentifier(id) = extension.parsed_extension()
        && let Ok(bytes) = <[u8; Ski::LEN]>::try_from(id.0)
    {
        return Ok(Ski::from_bytes(bytes));
    }

    Ok(ski_of(&certificate.public_key().subject_public_key.data))
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
    match certificate.public_key().parsed() {
        Ok(x509_parser::public_key::PublicKey::EC(point)) if point.key_size() == 256 => Ok(()),
        _ => Err(CertError::WrongCurve),
    }
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
            .get_extension_unique(&oid_registry::OID_X509_EXT_SUBJECT_KEY_IDENTIFIER)
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

    #[test]
    fn a_certificate_round_trips_through_pem() {
        let identity = self_signed(CertParams::new("i:46925_u:Device")).unwrap();
        let der = der_from_pem(&identity.certificate_pem()).unwrap();
        assert_eq!(der, identity.certificate_der());
        assert_eq!(ski_from_der(&der).unwrap(), identity.ski);
    }
}
