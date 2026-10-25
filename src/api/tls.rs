use std::{fmt::Debug, sync::Arc};

use log::warn;
use rustls_pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio_rustls::{
    client::TlsStream,
    rustls::{
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
        ClientConfig, DigitallySignedStruct, Error as RustlsError, RootCertStore, SignatureScheme,
    },
    TlsConnector,
};

use super::error::Error;

/// A custom certificate verifier that accepts any server certificate without validation.
///
/// # Warning
/// This disables certificate verification, leaving the connection vulnerable to
/// Man-in-the-Middle (MitM) attacks. Only use this for development, testing, or
/// connecting to trusted devices with self-signed certificates.
#[derive(Debug)]
pub struct InsecureServerCertVerifier {
    provider: Arc<CryptoProvider>,
}

impl InsecureServerCertVerifier {
    /// Creates a new `InsecureServerCertVerifier` with the given crypto provider.
    pub fn new(provider: Arc<CryptoProvider>) -> Self {
        Self { provider }
    }
}

impl ServerCertVerifier for InsecureServerCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls12_signature(
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
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Configuration options for establishing an encrypted SSL/TLS connection to RouterOS API-SSL.
#[derive(Clone, Debug)]
pub struct TlsConfig {
    /// If true, server certificate validation and hostname checks are bypassed.
    pub danger_accept_invalid_certs: bool,
    /// Custom CA certificates in PEM format.
    pub ca_certs: Vec<Vec<u8>>,
    /// Whether to load the native operating system certificate store.
    pub use_native_roots: bool,
    /// Whether to include the Mozilla compiled-in certificate roots (`webpki-roots`).
    pub use_webpki_roots: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            danger_accept_invalid_certs: false,
            ca_certs: Vec::new(),
            use_native_roots: true,
            use_webpki_roots: true,
        }
    }
}

impl TlsConfig {
    /// Creates a new [`TlsConfigBuilder`] with default settings.
    pub fn builder() -> TlsConfigBuilder {
        TlsConfigBuilder::default()
    }
}

/// Builder for constructing [`TlsConfig`] instances.
#[derive(Default, Debug)]
pub struct TlsConfigBuilder {
    config: TlsConfig,
}

impl TlsConfigBuilder {
    /// Creates a new `TlsConfigBuilder` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets whether to accept invalid or self-signed certificates without verification.
    ///
    /// # Warning
    /// Disabling certificate verification allows Man-in-the-Middle (MitM) attacks.
    pub fn danger_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.config.danger_accept_invalid_certs = accept;
        self
    }

    /// Adds a custom CA root certificate from a byte slice containing PEM data.
    pub fn add_ca_cert_pem(mut self, pem_bytes: Vec<u8>) -> Self {
        self.config.ca_certs.push(pem_bytes);
        self
    }

    /// Adds custom CA root certificates by reading a PEM file from disk.
    pub fn add_ca_cert_file<P: AsRef<std::path::Path>>(self, path: P) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(self.add_ca_cert_pem(bytes))
    }

    /// Sets whether to load the operating system native root certificate store.
    pub fn use_native_roots(mut self, use_native: bool) -> Self {
        self.config.use_native_roots = use_native;
        self
    }

    /// Sets whether to load the Mozilla compiled-in certificate roots.
    pub fn use_webpki_roots(mut self, use_webpki: bool) -> Self {
        self.config.use_webpki_roots = use_webpki;
        self
    }

    /// Builds and returns the final [`TlsConfig`].
    pub fn build(self) -> TlsConfig {
        self.config
    }
}

/// Creates a `rustls::ClientConfig` according to the provided [`TlsConfig`].
pub fn create_client_config(config: &TlsConfig) -> Result<Arc<ClientConfig>, Error> {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());

    if config.danger_accept_invalid_certs {
        let verifier = Arc::new(InsecureServerCertVerifier::new(provider.clone()));
        let client_config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Tls(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();

        return Ok(Arc::new(client_config));
    }

    let mut root_store = RootCertStore::empty();

    if config.use_native_roots {
        let native_certs = rustls_native_certs::load_native_certs();
        for err in &native_certs.errors {
            warn!("error loading native root certificate: {}", err);
        }
        for cert in native_certs.certs {
            if let Err(e) = root_store.add(cert) {
                warn!("failed to add native root certificate to store: {}", e);
            }
        }
    }

    if config.use_webpki_roots {
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    for pem_data in &config.ca_certs {
        let mut parsed_any = false;
        for cert_result in CertificateDer::pem_slice_iter(pem_data) {
            match cert_result {
                Ok(cert) => {
                    root_store
                        .add(cert)
                        .map_err(|e| Error::Tls(format!("invalid CA certificate: {}", e)))?;
                    parsed_any = true;
                }
                Err(e) => {
                    return Err(Error::Tls(format!(
                        "failed to parse CA certificate PEM: {}",
                        e
                    )));
                }
            }
        }
        if !parsed_any {
            return Err(Error::Tls(
                "no valid certificates found in provided CA PEM data".to_string(),
            ));
        }
    }

    let client_config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Tls(e.to_string()))?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(Arc::new(client_config))
}

/// Connects to a remote server using TCP and performs a TLS handshake.
pub async fn connect_tls_stream<A: ToSocketAddrs>(
    addr: A,
    domain: &str,
    config: &TlsConfig,
) -> Result<TlsStream<TcpStream>, Error> {
    let socket = TcpStream::connect(addr).await?;

    let clean_domain = domain.trim_start_matches('[').trim_end_matches(']');
    let server_name = ServerName::try_from(clean_domain)
        .map_err(|e| Error::Tls(format!("invalid server name '{}': {}", domain, e)))?
        .to_owned();

    let client_config = create_client_config(config)?;
    let connector = TlsConnector::from(client_config);

    let tls_stream = connector.connect(server_name, socket).await?;

    Ok(tls_stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_config_builder() {
        let config = TlsConfig::builder()
            .danger_accept_invalid_certs(true)
            .use_native_roots(false)
            .use_webpki_roots(false)
            .add_ca_cert_pem(b"test".to_vec())
            .build();

        assert!(config.danger_accept_invalid_certs);
        assert!(!config.use_native_roots);
        assert!(!config.use_webpki_roots);
        assert_eq!(config.ca_certs.len(), 1);
        assert_eq!(config.ca_certs[0], b"test");
    }

    #[test]
    fn test_create_client_config_insecure() {
        let config = TlsConfig::builder()
            .danger_accept_invalid_certs(true)
            .build();

        let client_config = create_client_config(&config);
        assert!(client_config.is_ok());
    }

    #[test]
    fn test_create_client_config_webpki() {
        let config = TlsConfig::builder()
            .danger_accept_invalid_certs(false)
            .use_native_roots(false)
            .use_webpki_roots(true)
            .build();

        let client_config = create_client_config(&config);
        assert!(client_config.is_ok());
    }

    #[test]
    fn test_create_client_config_empty_ca_fails() {
        let config = TlsConfig::builder()
            .danger_accept_invalid_certs(false)
            .add_ca_cert_pem(b"not a pem cert".to_vec())
            .build();

        let result = create_client_config(&config);
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Tls(msg) => {
                assert!(msg.contains("no valid certificates found"));
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn test_insecure_verifier() {
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let verifier = InsecureServerCertVerifier::new(provider);

        let fake_cert = CertificateDer::from(vec![0u8; 10]);
        let server_name = ServerName::try_from("router.lan").unwrap();
        let res = verifier.verify_server_cert(&fake_cert, &[], &server_name, &[], UnixTime::now());
        assert!(res.is_ok());
    }
}
