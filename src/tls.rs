//! TLS as SHIP requires it, with the trust decision left to the SHIP handshake.
//!
//! SHIP's TLS is unusual in two ways, and both follow from there being no certificate
//! authority anywhere in the system.
//!
//! **Every certificate is self-signed, and that is not an error.** An ordinary TLS client
//! rejects a certificate that chains to nothing; a SHIP node accepts it, records the
//! Subject Key Identifier, and completes the handshake. The question "should I be talking
//! to this device?" is answered afterwards, in the SHIP hello phase, against a SKI a
//! person has approved — see [`Trust`](crate::ship::Trust). Deferring it is not laxity:
//! the connection has to exist before a user can be shown the SKI to approve.
//!
//! **Both ends authenticate, and either may dial.** Client authentication is mandatory,
//! so a server that cannot see the client's certificate has nothing to identify the peer
//! by and must abort.
//!
//! What is enforced here is everything else SHIP §9 fixes: TLS 1.2 only, since the
//! specification says in as many words that 1.3 "is not considered in this version";
//! records no larger than 1024 bytes, with 512 recommended; no compression, no
//! renegotiation, no session tickets shared across peers.
//!
//! Requires the `tls` feature.
//!
//! # The cipher-suite gap
//!
//! SHIP §9.4 marks `TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA256` a SHALL, and rustls
//! implements no CBC suite at all — a deliberate decision on its part, CBC-in-TLS having
//! a long history of padding-oracle attacks. This backend therefore offers the GCM and
//! ChaCha20-Poly1305 suites, which the installation requirements recommend, and
//! [`ShipTls::mandatory_suite_available`] reports the gap rather than hiding it. A peer
//! that offers CBC alone will not connect; the concept's answer is an OpenSSL backend for
//! that case.

use alloc::sync::Arc;
use alloc::vec::Vec;
use std::sync::Mutex;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, Error as RustlsError, ServerConfig,
    SignatureScheme,
};

use crate::cert::{self, Identity};
use crate::ship::{Fingerprint, Ski};

/// The largest TLS record SHIP §9.2 allows a node to send.
pub const MAX_RECORD_SIZE: usize = 1024;

/// The record size the parameter table recommends, and the default here.
pub const RECOMMENDED_RECORD_SIZE: usize = 512;

// Which backend `rustls` was built with. `src/lib.rs` refuses a build that names both or
// neither, so exactly one of these arms is ever compiled; the `not(...)` keeps the module
// well formed while that error is being reported.
#[cfg(feature = "aws-lc-rs")]
use rustls::crypto::aws_lc_rs as backend;
#[cfg(all(feature = "ring", not(feature = "aws-lc-rs")))]
use rustls::crypto::ring as backend;

/// The cipher suite SHIP §9.4 marks as SHALL, which rustls does not implement.
pub const MANDATORY_SUITE: &str = "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA256";

/// The suites SHIP permits that this backend can offer.
///
/// SHIP §9.4 allows ECDHE-ECDSA with AES-GCM and ChaCha20-Poly1305 alongside the CBC
/// suite it mandates. `TLS_ECDHE_ECDSA_WITH_AES_128_CCM_8` is deliberately absent: the
/// installation requirements Annex A.4 say it SHOULD NOT be used.
fn ship_cipher_suites() -> Vec<rustls::SupportedCipherSuite> {
    use backend::cipher_suite::*;

    alloc::vec![
        TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    ]
}

/// The key exchange group SHIP §9.3 requires.
///
/// secp256r1 is a SHALL; brainpoolP256r1 and P384r1 are permitted but neither backend
/// implements them, and offering a group a SHIP node may not use only makes failures
/// obscure. The post-quantum hybrids `aws-lc-rs` offers by default are dropped for the
/// same reason: a SHIP node is not allowed to negotiate one.
fn ship_kx_groups() -> Vec<&'static dyn rustls::crypto::SupportedKxGroup> {
    alloc::vec![backend::kx_group::SECP256R1]
}

/// A crypto provider offering exactly what SHIP §9 permits.
///
/// Built explicitly rather than taken from the process default, which matters more than
/// it looks: `rustls`' process default is a global, and a binary that links two providers
/// panics the first time anything asks for it. Nothing in this crate ever does, so a
/// consumer that has installed its own default keeps it, and one that has not is not
/// caught out by us.
fn ship_provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::CryptoProvider {
        cipher_suites: ship_cipher_suites(),
        kx_groups: ship_kx_groups(),
        ..backend::default_provider()
    }
}

/// The cryptography backend this build was compiled against: `ring` or `aws-lc-rs`.
///
/// Worth logging at start-up. The choice is the consumer's — see the crate documentation
/// — and knowing which one a running binary picked up is the difference between a
/// five-minute diagnosis and an afternoon.
pub const CRYPTO_PROVIDER: &str = if cfg!(feature = "aws-lc-rs") {
    "aws-lc-rs"
} else {
    "ring"
};

/// Why a TLS configuration could not be built.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// rustls refused the configuration.
    #[error("the TLS configuration was refused: {0}")]
    Rustls(#[from] RustlsError),
    /// The record size was outside what SHIP §9.2 permits.
    #[error("a SHIP record is at most {MAX_RECORD_SIZE} bytes, asked for {0}")]
    RecordTooLarge(usize),
    /// The backend's random number generator failed.
    #[error("the random number generator failed")]
    Random,
}

/// Fills `buffer` from the backend's cryptographic random number generator.
///
/// The Pairing Service §6.3 asks for a generator of this quality behind every nonce and
/// every secret, and a node's own backend is the one it already trusts with its keys.
///
/// # Errors
///
/// [`TlsError::Random`] if the generator refused, which on the platforms the backends
/// support does not happen.
pub fn random(buffer: &mut [u8]) -> Result<(), TlsError> {
    ship_provider()
        .secure_random
        .fill(buffer)
        .map_err(|_| TlsError::Random)
}

/// A node's TLS configuration: its identity, and what it will accept from a peer.
///
/// Building one does not decide whom to trust. It produces a client and a server config
/// that complete a handshake with any SHIP node; who it turned out to be is read off the
/// finished connection with [`peer_ski`], and the decision is the SHIP handshake's.
///
/// Both configurations are built once and shared. A `rustls` config carries a parsed
/// certificate and key and is designed to be reused across connections, which is why
/// [`ClientConfig`] and [`ServerConfig`] come back behind an [`Arc`].
#[derive(Debug)]
pub struct ShipTls {
    identity: Arc<Identity>,
    provider: Arc<rustls::crypto::CryptoProvider>,
    record_size: usize,
    client: Mutex<Option<Arc<ClientConfig>>>,
    server: Mutex<Option<Arc<ServerConfig>>>,
}

impl ShipTls {
    /// A configuration for a node with this identity.
    pub fn new(identity: Identity) -> Self {
        Self {
            identity: Arc::new(identity),
            provider: Arc::new(ship_provider()),
            record_size: RECOMMENDED_RECORD_SIZE,
            client: Mutex::new(None),
            server: Mutex::new(None),
        }
    }

    /// Sets the maximum record this node will send.
    ///
    /// SHIP §9.2 caps it at 1024 bytes and the parameter table recommends 512; a small
    /// record is what lets a microcontroller with a few kilobytes of RAM take part.
    pub fn record_size(mut self, bytes: usize) -> Result<Self, TlsError> {
        if bytes > MAX_RECORD_SIZE {
            return Err(TlsError::RecordTooLarge(bytes));
        }
        self.record_size = bytes;
        Ok(self)
    }

    /// This node's certificate fingerprint, which the Pairing Service identifies it by.
    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    /// This node's SKI, which is what a peer will be asked to trust.
    pub fn ski(&self) -> Ski {
        self.identity.ski
    }

    /// The configuration for dialling a peer, built on first use and shared thereafter.
    ///
    /// Who answered is read off the finished connection with [`peer_ski`], and the SHIP
    /// handshake checks it against the node's trust store.
    pub fn client_config(&self) -> Result<Arc<ClientConfig>, TlsError> {
        if let Some(config) = self.client.lock().ok().and_then(|held| held.clone()) {
            return Ok(config);
        }
        let mut config = ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS12])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnySelfSigned::new(self.provider.clone())))
            .with_client_auth_cert(self.chain(), self.key())?;
        config.max_fragment_size = Some(self.record_size);
        // A resumed session skips the certificate exchange, and this node identifies its
        // peer by the certificate. SHIP §9.6 makes resumption a SHOULD; correctness of
        // the trust decision comes first.
        config.resumption = rustls::client::Resumption::disabled();
        config.enable_sni = true;
        let config = Arc::new(config);
        if let Ok(mut held) = self.client.lock() {
            *held = Some(config.clone());
        }
        Ok(config)
    }

    /// The configuration for accepting a peer, built on first use and shared thereafter.
    ///
    /// Client authentication is mandatory: a SHIP server that cannot see the dialling
    /// node's certificate has no way to identify it.
    pub fn server_config(&self) -> Result<Arc<ServerConfig>, TlsError> {
        if let Some(config) = self.server.lock().ok().and_then(|held| held.clone()) {
            return Ok(config);
        }
        let mut config = ServerConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS12])?
            .with_client_cert_verifier(Arc::new(AnySelfSigned::new(self.provider.clone())))
            .with_single_cert(self.chain(), self.key())?;
        config.max_fragment_size = Some(self.record_size);
        config.session_storage = rustls::server::ServerSessionMemoryCache::new(0);
        let config = Arc::new(config);
        if let Ok(mut held) = self.server.lock() {
            *held = Some(config.clone());
        }
        Ok(config)
    }

    /// Whether the active provider offers the suite SHIP marks as SHALL.
    ///
    /// It does not, with rustls. The method exists so that a node can log the gap at
    /// start-up rather than discover it as an unexplained handshake failure against one
    /// particular vendor's firmware.
    pub fn mandatory_suite_available(&self) -> bool {
        self.provider
            .cipher_suites
            .iter()
            .any(|suite| alloc::format!("{:?}", suite.suite()).contains("CBC"))
    }

    /// The suites this node will offer, for logging at start-up.
    pub fn cipher_suites(&self) -> impl Iterator<Item = rustls::CipherSuite> + '_ {
        self.provider.cipher_suites.iter().map(|s| s.suite())
    }

    fn chain(&self) -> Vec<CertificateDer<'static>> {
        alloc::vec![CertificateDer::from(
            self.identity.certificate_der().to_vec()
        )]
    }

    fn key(&self) -> rustls::pki_types::PrivateKeyDer<'static> {
        PrivatePkcs8KeyDer::from(self.identity.key_der()).into()
    }
}

/// The SKI of the peer on an established `rustls` connection.
///
/// This is *the* way a SHIP node learns who it is talking to: the certificate is
/// self-signed and carries no name worth trusting, so the Subject Key Identifier
/// computed from its public key is the identity, and it is what the trust store holds
/// (SHIP §5.4). `rustls` keeps the peer's chain on the connection, so no configuration
/// needs to be built per connection to capture it.
pub fn peer_ski(state: &rustls::CommonState) -> Option<Ski> {
    let certificates = state.peer_certificates()?;
    cert::ski_from_der(certificates.first()?).ok()
}

/// The certificate fingerprint of the peer on an established `rustls` connection.
///
/// The Pairing Service's identity (§10.2): the SHA-256 of the DER certificate the peer
/// presented, to be compared with the `trustPar` it announced. A peer paired that way is
/// admitted on this rather than on its SKI.
pub fn peer_fingerprint(state: &rustls::CommonState) -> Option<Fingerprint> {
    let certificates = state.peer_certificates()?;
    Some(Fingerprint::of_der(certificates.first()?))
}

/// Accepts any well-formed certificate, having checked that SHIP could work with it.
///
/// This is the piece that would be a security hole in an ordinary TLS client and is the
/// specified behaviour here: there is no authority to chain to, so validity is not what
/// decides whether to talk to a peer. What is checked is that the certificate parses and
/// carries a key on the curve SHIP requires — a peer failing either cannot complete a
/// SHIP handshake anyway, and saying so here gives a legible error. Who the peer *is* is
/// read off the finished connection with [`peer_ski`]; the trust decision is the SHIP
/// handshake's, one layer up.
#[derive(Debug)]
struct AnySelfSigned {
    provider: Arc<rustls::crypto::CryptoProvider>,
    no_hints: Vec<DistinguishedName>,
}

impl AnySelfSigned {
    fn new(provider: Arc<rustls::crypto::CryptoProvider>) -> Self {
        Self {
            provider,
            no_hints: Vec::new(),
        }
    }

    /// Refuses a certificate SHIP could not work with.
    fn observe(&self, end_entity: &CertificateDer<'_>) -> Result<(), RustlsError> {
        use rustls::CertificateError;

        cert::uses_ship_curve(end_entity)
            .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        cert::ski_from_der(end_entity)
            .map_err(|_| RustlsError::InvalidCertificate(CertificateError::BadEncoding))?;
        Ok(())
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for AnySelfSigned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // Expiry is not checked. SHIP nodes are commissioned once and may run for
        // decades without a clock; the SKI is what identifies them, and it does not
        // expire.
        self.observe(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        // Unreachable: only TLS 1.2 is offered. Implemented rather than left to panic,
        // because a trait method that aborts the process is a worse failure than one that
        // verifies a signature nobody asked about.
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for AnySelfSigned {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // Naming acceptable issuers would be meaningless: there is no issuer to name.
        &self.no_hints
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        self.observe(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        // SHIP §12.1: a node identifies its peer by the peer's certificate, so a
        // connection without one has nothing to base a trust decision on.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cert::{self, CertParams};
    use std::io::{Read, Write};

    fn node(name: &str) -> ShipTls {
        ShipTls::new(cert::self_signed(CertParams::new(name)).unwrap())
    }

    /// Runs a real TLS 1.2 handshake between two in-memory connections.
    fn handshake(
        client: &ShipTls,
        server: &ShipTls,
    ) -> (
        Result<(), RustlsError>,
        rustls::ClientConnection,
        rustls::ServerConnection,
    ) {
        let mut client_connection = rustls::ClientConnection::new(
            client.client_config().unwrap(),
            ServerName::try_from("heatpump.local").unwrap(),
        )
        .unwrap();
        let mut server_connection =
            rustls::ServerConnection::new(server.server_config().unwrap()).unwrap();

        let mut result = Ok(());
        for _ in 0..16 {
            let mut buffer = Vec::new();
            client_connection.write_tls(&mut buffer).unwrap();
            if !buffer.is_empty() {
                server_connection.read_tls(&mut buffer.as_slice()).unwrap();
                if let Err(e) = server_connection.process_new_packets() {
                    result = Err(e);
                    break;
                }
            }
            let mut buffer = Vec::new();
            server_connection.write_tls(&mut buffer).unwrap();
            if !buffer.is_empty() {
                client_connection.read_tls(&mut buffer.as_slice()).unwrap();
                if let Err(e) = client_connection.process_new_packets() {
                    result = Err(e);
                    break;
                }
            }
            if !client_connection.is_handshaking() && !server_connection.is_handshaking() {
                break;
            }
        }
        (result, client_connection, server_connection)
    }

    #[test]
    fn two_self_signed_nodes_complete_a_handshake_and_learn_each_others_ski() {
        let client = node("i:46925_u:ControlBox-1");
        let server = node("i:46925_u:HeatPump-1");
        let (result, client_connection, server_connection) = handshake(&client, &server);

        result.expect("the handshake completes");
        assert!(!client_connection.is_handshaking());

        // Each end learned who the other is — which is the whole point, since there is no
        // authority to ask.
        assert_eq!(peer_ski(&client_connection), Some(server.ski()));
        assert_eq!(peer_ski(&server_connection), Some(client.ski()));
    }

    #[test]
    fn ship_9_1_the_connection_is_tls_1_2() {
        let (result, client_connection, _) = handshake(&node("i:46925_u:A"), &node("i:46925_u:B"));
        result.unwrap();
        assert_eq!(
            client_connection.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_2),
            "SHIP §9.1: TLS 1.3 is not considered in this version"
        );
    }

    #[test]
    fn ship_9_4_only_the_suites_the_specification_permits_are_offered() {
        let node = node("i:46925_u:A");
        let offered: Vec<_> = node.cipher_suites().collect();
        assert!(offered.contains(&rustls::CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256));
        assert!(
            !offered
                .iter()
                .any(|s| alloc::format!("{s:?}").contains("RSA")),
            "SHIP fixes the certificate to ECDSA, so an RSA suite could never be used"
        );
        assert!(
            !node.mandatory_suite_available(),
            "the CBC suite SHIP marks as SHALL is not implemented by rustls, and this \
             reports the gap rather than hiding it"
        );
    }

    #[test]
    fn ship_9_2_records_stay_within_the_size_the_specification_allows() {
        assert_eq!(
            node("i:46925_u:A")
                .client_config()
                .unwrap()
                .max_fragment_size,
            Some(RECOMMENDED_RECORD_SIZE)
        );
        assert!(matches!(
            node("i:46925_u:A").record_size(4096),
            Err(TlsError::RecordTooLarge(4096))
        ));
        assert!(node("i:46925_u:A").record_size(MAX_RECORD_SIZE).is_ok());
    }

    #[test]
    fn ship_12_1_a_client_that_offers_no_certificate_is_refused() {
        // A plain client config with no client certificate: the server has nothing to
        // identify the peer by, so it must abort rather than serve an anonymous node.
        let server = node("i:46925_u:HeatPump-1");
        let config = server.server_config().unwrap();

        let anonymous = ClientConfig::builder_with_provider(Arc::new(ship_provider()))
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnySelfSigned::new(Arc::new(
                ship_provider(),
            ))))
            .with_no_client_auth();

        let mut client_connection = rustls::ClientConnection::new(
            Arc::new(anonymous),
            ServerName::try_from("heatpump.local").unwrap(),
        )
        .unwrap();
        let mut server_connection = rustls::ServerConnection::new(config).unwrap();

        let mut refused = false;
        for _ in 0..16 {
            let mut buffer = Vec::new();
            client_connection.write_tls(&mut buffer).unwrap();
            if !buffer.is_empty() {
                server_connection.read_tls(&mut buffer.as_slice()).unwrap();
                if server_connection.process_new_packets().is_err() {
                    refused = true;
                    break;
                }
            }
            let mut buffer = Vec::new();
            server_connection.write_tls(&mut buffer).unwrap();
            if !buffer.is_empty() {
                client_connection.read_tls(&mut buffer.as_slice()).unwrap();
                if client_connection.process_new_packets().is_err() {
                    refused = true;
                    break;
                }
            }
            if !client_connection.is_handshaking() && !server_connection.is_handshaking() {
                break;
            }
        }
        assert!(refused, "client authentication is mandatory");
        assert_eq!(
            peer_ski(&server_connection),
            None,
            "and nobody was identified"
        );
    }

    #[test]
    fn application_data_crosses_the_connection() {
        let client = node("i:46925_u:A");
        let server = node("i:46925_u:B");
        let (result, mut client_connection, mut server_connection) = handshake(&client, &server);
        result.unwrap();

        client_connection
            .writer()
            .write_all(br#"{"connectionHello":[{"phase":"ready"}]}"#)
            .unwrap();
        let mut wire = Vec::new();
        client_connection.write_tls(&mut wire).unwrap();
        server_connection.read_tls(&mut wire.as_slice()).unwrap();
        server_connection.process_new_packets().unwrap();

        let mut received = Vec::new();
        server_connection.reader().read_to_end(&mut received).ok();
        assert_eq!(received, br#"{"connectionHello":[{"phase":"ready"}]}"#);
    }
}
