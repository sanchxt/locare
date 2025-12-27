//! Cryptographic primitives for LocalDrop.
//!
//! This module provides:
//! - TLS 1.3 configuration for secure connections
//! - Ed25519 key pairs for device identity
//! - HMAC for code verification
//! - SHA-256 for file integrity
//! - xxHash for fast chunk verification
//!
//! ## Security Model
//!
//! - All transfers are encrypted with TLS 1.3
//! - Perfect forward secrecy via ephemeral ECDH keys
//! - Ed25519 signatures for trusted device verification
//! - HMAC-SHA256 for timing-attack-resistant code verification

mod identity;

pub use identity::DeviceIdentity;

use std::sync::Arc;

use crate::error::{Error, Result};

/// TLS configuration for LocalDrop connections.
///
/// This struct holds either a server or client configuration for TLS 1.3.
/// Use `TlsConfig::server()` to create a server configuration with a
/// self-signed certificate, or `TlsConfig::client()` to create a client
/// configuration that accepts self-signed certificates.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    server: Option<Arc<rustls::ServerConfig>>,
    client: Option<Arc<rustls::ClientConfig>>,
}

impl TlsConfig {
    /// Create a new TLS configuration for server mode.
    ///
    /// Generates an ephemeral self-signed certificate for TLS 1.3.
    ///
    /// # Errors
    ///
    /// Returns an error if certificate generation or configuration fails.
    pub fn server() -> Result<Self> {
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let cert_params = rcgen::CertificateParams::new(subject_alt_names)
            .map_err(|e| Error::TlsError(format!("Failed to create cert params: {e}")))?;

        let key_pair = rcgen::KeyPair::generate()
            .map_err(|e| Error::TlsError(format!("Failed to generate key pair: {e}")))?;

        let cert = cert_params
            .self_signed(&key_pair)
            .map_err(|e| Error::TlsError(format!("Failed to generate self-signed cert: {e}")))?;

        let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::try_from(key_pair.serialize_der())
            .map_err(|e| Error::TlsError(format!("Failed to convert private key: {e}")))?;

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .map_err(|e| Error::TlsError(format!("Failed to build server config: {e}")))?;

        Ok(Self {
            server: Some(Arc::new(config)),
            client: None,
        })
    }

    /// Create a new TLS configuration for client mode.
    ///
    /// The client is configured to accept self-signed certificates,
    /// which is necessary for LocalDrop's peer-to-peer model.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be created.
    pub fn client() -> Result<Self> {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertVerifier))
            .with_no_client_auth();

        Ok(Self {
            server: None,
            client: Some(Arc::new(config)),
        })
    }

    /// Get the server configuration, if this is a server config.
    #[must_use]
    pub fn server_config(&self) -> Option<&rustls::ServerConfig> {
        self.server.as_deref()
    }

    /// Get the client configuration, if this is a client config.
    #[must_use]
    pub fn client_config(&self) -> Option<&rustls::ClientConfig> {
        self.client.as_deref()
    }
}

/// Certificate verifier that accepts any certificate.
///
/// This is used for LocalDrop's client connections where we trust
/// the peer based on the share code HMAC rather than the certificate.
#[derive(Debug)]
struct AcceptAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Compute HMAC-SHA256 for code verification.
///
/// # Arguments
///
/// * `key` - The session key
/// * `data` - The data to authenticate (the share code)
///
/// # Returns
///
/// The HMAC as a byte array.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Compute SHA-256 hash of data.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Compute xxHash64 for fast chunk verification.
pub fn xxhash64(data: &[u8]) -> u64 {
    xxhash_rust::xxh64::xxh64(data, 0)
}

/// Constant-time comparison of two byte slices.
///
/// Returns `true` if the slices are equal, `false` otherwise.
/// This function takes the same amount of time regardless of where
/// the first difference occurs, preventing timing attacks.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Generate cryptographically secure random bytes.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;

    let mut bytes = [0u8; N];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

/// Generate a 32-byte session key for HMAC code verification.
///
/// This key is generated when a share session starts and is used
/// to verify the share code via HMAC during the handshake.
#[must_use]
pub fn generate_session_key() -> [u8; 32] {
    random_bytes::<32>()
}

/// Derive a session key from a share code.
///
/// Both sender and receiver derive the same key from the code,
/// allowing HMAC-based verification that the receiver knows the correct code.
///
/// # Arguments
///
/// * `code` - The share code string
///
/// # Returns
///
/// A 32-byte session key derived from the code using SHA-256.
#[must_use]
pub fn derive_session_key(code: &str) -> [u8; 32] {
    let mut data = Vec::with_capacity(19 + code.len());
    data.extend_from_slice(b"localdrop:session:");
    data.extend_from_slice(code.as_bytes());
    sha256(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_creation() {
        let config = TlsConfig::server();
        assert!(
            config.is_ok(),
            "TLS server config should be created successfully"
        );
        let config = config.unwrap();
        assert!(
            config.server_config().is_some(),
            "Should have server config"
        );
        assert!(
            config.client_config().is_none(),
            "Should not have client config"
        );
    }

    #[test]
    fn test_client_config_creation() {
        let config = TlsConfig::client();
        assert!(
            config.is_ok(),
            "TLS client config should be created successfully"
        );
        let config = config.unwrap();
        assert!(
            config.client_config().is_some(),
            "Should have client config"
        );
        assert!(
            config.server_config().is_none(),
            "Should not have server config"
        );
    }

    #[test]
    fn test_generate_session_key() {
        let key1 = generate_session_key();
        let key2 = generate_session_key();

        assert_eq!(key1.len(), 32);
        assert_eq!(key2.len(), 32);

        assert_ne!(key1, key2, "Generated keys should be unique");
    }

    #[tokio::test]
    async fn test_tls_handshake_loopback() {
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        use tokio_rustls::{TlsAcceptor, TlsConnector};

        let server_config = TlsConfig::server().expect("server config");
        let client_config = TlsConfig::client().expect("client config");

        let acceptor = TlsAcceptor::from(Arc::new(
            server_config
                .server_config()
                .expect("server config")
                .clone(),
        ));
        let connector = TlsConnector::from(Arc::new(
            client_config
                .client_config()
                .expect("client config")
                .clone(),
        ));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server_handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut tls_stream = acceptor.accept(stream).await.expect("tls accept");

            let mut buf = [0u8; 1024];
            let n = tls_stream.read(&mut buf).await.expect("read");
            tls_stream.write_all(&buf[..n]).await.expect("write");
        });

        let stream = TcpStream::connect(addr).await.expect("connect");
        let mut tls_stream = connector
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .expect("tls connect");

        let test_data = b"Hello, TLS!";
        tls_stream.write_all(test_data).await.expect("write");

        let mut buf = [0u8; 1024];
        let n = tls_stream.read(&mut buf).await.expect("read");

        assert_eq!(&buf[..n], test_data, "Echoed data should match");

        server_handle.await.expect("server task");
    }

    #[test]
    fn test_hmac_sha256() {
        let key = b"test_key";
        let data = b"test_data";

        let hmac1 = hmac_sha256(key, data);
        let hmac2 = hmac_sha256(key, data);

        assert_eq!(hmac1, hmac2);

        let hmac3 = hmac_sha256(key, b"different_data");
        assert_ne!(hmac1, hmac3);

        let hmac4 = hmac_sha256(b"different_key", data);
        assert_ne!(hmac1, hmac4);
    }

    #[test]
    fn test_sha256() {
        let data = b"test_data";

        let hash1 = sha256(data);
        let hash2 = sha256(data);

        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 32);

        let hash3 = sha256(b"different_data");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_xxhash64() {
        let data = b"test_data";

        let hash1 = xxhash64(data);
        let hash2 = xxhash64(data);

        assert_eq!(hash1, hash2);

        let hash3 = xxhash64(b"different_data");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_constant_time_eq() {
        let a = [1u8, 2, 3, 4];
        let b = [1u8, 2, 3, 4];
        let c = [1u8, 2, 3, 5];
        let d = [1u8, 2, 3];

        assert!(constant_time_eq(&a, &b), "Equal slices should return true");
        assert!(
            !constant_time_eq(&a, &c),
            "Different slices should return false"
        );
        assert!(
            !constant_time_eq(&a, &d),
            "Different length slices should return false"
        );
    }

    #[test]
    fn test_random_bytes() {
        let bytes1: [u8; 16] = random_bytes();
        let bytes2: [u8; 16] = random_bytes();

        assert_eq!(bytes1.len(), 16);
        assert_eq!(bytes2.len(), 16);
        assert_ne!(bytes1, bytes2, "Random bytes should be different");
    }
}
