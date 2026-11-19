//! WebPKI validates signatures, chain, validity and server/client EKU. Server
//! identity is the exact SPIFFE URI (standard URI-only SVIDs are supported);
//! optional DNS verification adds a constraint, never replaces URI verification.
use super::{
    Error, Mode, PeerRules, Result, ServiceIdentity, VerifiedPeer, der,
    secrets::{CredentialFiles, read_file},
};
use rustls::{
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject},
    server::{
        WebPkiClientVerifier,
        danger::{ClientCertVerified, ClientCertVerifier},
    },
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
struct Material {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    roots: Arc<rustls::RootCertStore>,
}
fn material(files: &CredentialFiles, identity: &ServiceIdentity) -> Result<Material> {
    let generation = files.generation()?;
    let ca = read_file(&generation.join("ca.pem"), false)?;
    let cert = read_file(&generation.join("cert.pem"), false)?;
    let key = read_file(&generation.join("key.pem"), true)?;
    let authorities = CertificateDer::pem_slice_iter(ca.expose())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error("workload_ca_invalid"))?;
    if authorities.is_empty() || authorities.len() > 32 {
        return Err(Error("workload_ca_invalid"));
    }
    let mut roots = rustls::RootCertStore::empty();
    for certificate in authorities {
        roots
            .add(certificate)
            .map_err(|_| Error("workload_ca_invalid"))?;
    }
    let chain = CertificateDer::pem_slice_iter(cert.expose())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error("workload_certificate_invalid"))?;
    if chain.is_empty() || chain.len() > 16 || der::identity(chain[0].as_ref())? != *identity {
        return Err(Error("workload_local_identity_mismatch"));
    }
    let key = PrivateKeyDer::from_pem_slice(key.expose())
        .map_err(|_| Error("workload_private_key_invalid"))?;
    Ok(Material {
        chain,
        key,
        roots: Arc::new(roots),
    })
}
fn identity_error() -> rustls::Error {
    rustls::Error::General("workload URI SAN rejected".into())
}
#[derive(Debug)]
struct ClientVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    rules: PeerRules,
}
impl ClientCertVerifier for ClientVerifier {
    fn offer_client_auth(&self) -> bool {
        self.inner.offer_client_auth()
    }
    fn client_auth_mandatory(&self) -> bool {
        self.inner.client_auth_mandatory()
    }
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }
    fn verify_client_cert(
        &self,
        leaf: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        let verified = self.inner.verify_client_cert(leaf, chain, now)?;
        let identity = der::identity(leaf.as_ref()).map_err(|_| identity_error())?;
        if !self.rules.admits(&identity) {
            return Err(identity_error());
        }
        Ok(verified)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
#[derive(Debug)]
struct ServerVerifier {
    inner: Arc<WebPkiServerVerifier>,
    expected: ServiceIdentity,
    roots: Arc<rustls::RootCertStore>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
    require_dns: bool,
}
impl ServerCertVerifier for ServerVerifier {
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let parsed = rustls::server::ParsedCertificate::try_from(leaf)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &self.roots,
            chain,
            now,
            self.algorithms.all,
        )?;
        if self.require_dns {
            rustls::client::verify_server_name(&parsed, name)?;
        }
        // OCSP is not the revocation authority in this private SVID profile;
        // short-lived SVIDs and overlapping/retired trust bundles are mandatory.
        let _ = ocsp;
        if der::identity(leaf.as_ref()).map_err(|_| identity_error())? != self.expected {
            return Err(identity_error());
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
/// Rebuild on every accepted handshake, before consuming any application bytes.
/// Invalid rotations reject new handshakes rather than silently retaining old keys.
#[derive(Clone)]
pub struct ServerCredentials {
    files: CredentialFiles,
    identity: ServiceIdentity,
    rules: PeerRules,
    mode: Mode,
}
impl ServerCredentials {
    pub fn new(
        files: CredentialFiles,
        identity: ServiceIdentity,
        rules: PeerRules,
        mode: Mode,
    ) -> Result<Self> {
        if mode == Mode::Disabled {
            return Err(Error("workload_tls_disabled"));
        }
        Ok(Self {
            files,
            identity,
            rules,
            mode,
        })
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn load(&self) -> Result<Arc<rustls::ServerConfig>> {
        let Material { chain, key, roots } = material(&self.files, &self.identity)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = WebPkiClientVerifier::builder_with_provider(roots, provider.clone());
        let inner = if self.mode == Mode::Dual {
            builder.allow_unauthenticated().build()
        } else {
            builder.build()
        }
        .map_err(|_| Error("workload_client_verifier_invalid"))?;
        let verifier = Arc::new(ClientVerifier {
            inner,
            rules: self.rules.clone(),
        });
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error("workload_tls_version_invalid"))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(|_| Error("workload_key_pair_invalid"))?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        config.send_tls13_tickets = 0;
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        Ok(Arc::new(config))
    }
    pub fn verified_peer(
        &self,
        connection: &rustls::ServerConnection,
    ) -> Result<Option<VerifiedPeer>> {
        if connection.is_handshaking() {
            return Err(Error("workload_tls_handshake_incomplete"));
        }
        let Some(chain) = connection.peer_certificates() else {
            return if self.mode == Mode::Dual {
                Ok(None)
            } else {
                Err(Error("workload_client_certificate_required"))
            };
        };
        let leaf = chain
            .first()
            .ok_or(Error("workload_client_certificate_required"))?;
        let identity = der::identity(leaf.as_ref())?;
        if !self.rules.admits(&identity) {
            return Err(Error("workload_peer_not_allowed"));
        }
        Ok(Some(VerifiedPeer {
            identity,
            leaf_sha256: format!("{:x}", Sha256::digest(leaf.as_ref())),
        }))
    }
}
#[derive(Clone)]
pub struct ClientCredentials {
    files: CredentialFiles,
    identity: ServiceIdentity,
}
impl ClientCredentials {
    pub fn new(files: CredentialFiles, identity: ServiceIdentity) -> Self {
        Self { files, identity }
    }
    pub fn load(&self, expected: ServiceIdentity) -> Result<rustls::ClientConfig> {
        self.load_profile(expected, false)
    }
    pub fn load_with_dns(&self, expected: ServiceIdentity) -> Result<rustls::ClientConfig> {
        self.load_profile(expected, true)
    }
    fn load_profile(
        &self,
        expected: ServiceIdentity,
        require_dns: bool,
    ) -> Result<rustls::ClientConfig> {
        let Material { chain, key, roots } = material(&self.files, &self.identity)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = WebPkiServerVerifier::builder_with_provider(roots.clone(), provider.clone())
            .build()
            .map_err(|_| Error("workload_server_verifier_invalid"))?;
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| Error("workload_tls_version_invalid"))?
            .with_root_certificates(roots.clone())
            .with_client_auth_cert(chain, key)
            .map_err(|_| Error("workload_key_pair_invalid"))?;
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(ServerVerifier {
                inner: verifier,
                expected,
                roots,
                algorithms: rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
                require_dns,
            }));
        config.resumption = rustls::client::Resumption::disabled();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(config)
    }
}
