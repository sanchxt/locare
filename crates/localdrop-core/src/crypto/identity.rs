//! Device identity management for trusted device authentication.
//!
//! This module provides Ed25519 key pair generation and management for
//! device identity. Each device has a unique identity that is used for
//! signature-based authentication when communicating with trusted devices.
//!
//! ## Key Storage
//!
//! The identity is stored in a JSON file in the platform-specific data directory:
//! - Linux: `~/.local/share/localdrop/LocalDrop/identity.json`
//! - macOS: `~/Library/Application Support/LocalDrop/identity.json`
//! - Windows: `%APPDATA%\LocalDrop\identity.json`
//!
//! ## Security
//!
//! - Keys are generated using a cryptographically secure RNG
//! - The secret key is stored in base64 encoding
//! - Device ID is derived from the public key hash (stable across sessions)

use std::fs;
use std::path::PathBuf;

use base64::prelude::*;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};

/// Device identity containing an Ed25519 key pair.
///
/// This struct manages the cryptographic identity of a device, enabling
/// signature-based authentication for trusted device communication.
#[derive(Debug)]
pub struct DeviceIdentity {
    /// The Ed25519 signing key (contains both secret and public key)
    signing_key: SigningKey,
    /// Stable device ID derived from the public key
    device_id: Uuid,
    /// Path where this identity is stored (if loaded from/saved to disk)
    path: Option<PathBuf>,
}

/// Serializable representation of the identity for storage.
#[derive(Debug, Serialize, Deserialize)]
struct IdentityFile {
    /// Version for future compatibility
    version: u32,
    /// Base64-encoded secret key bytes
    secret_key: String,
    /// Device ID (cached for convenience)
    device_id: Uuid,
}

impl DeviceIdentity {
    /// Generate a new random device identity.
    ///
    /// Creates a new Ed25519 key pair using a cryptographically secure RNG.
    /// The device ID is derived from the public key hash.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation fails (should not happen with a
    /// properly functioning RNG).
    pub fn generate() -> Result<Self> {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let device_id = Self::derive_device_id(&signing_key.verifying_key());

        Ok(Self {
            signing_key,
            device_id,
            path: None,
        })
    }

    /// Load device identity from the default storage location.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The identity file doesn't exist
    /// - The file cannot be read or parsed
    /// - The stored key is invalid
    pub fn load() -> Result<Self> {
        let path = Self::default_path().ok_or_else(|| {
            Error::ConfigError("Cannot determine identity storage path".to_string())
        })?;
        Self::load_from(path)
    }

    /// Load device identity from a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load_from(path: PathBuf) -> Result<Self> {
        let content = fs::read_to_string(&path).map_err(|e| {
            Error::ConfigError(format!(
                "Failed to read identity file {}: {}",
                path.display(),
                e
            ))
        })?;

        let file: IdentityFile = serde_json::from_str(&content)
            .map_err(|e| Error::ConfigError(format!("Failed to parse identity file: {e}")))?;

        let secret_bytes = BASE64_STANDARD
            .decode(&file.secret_key)
            .map_err(|e| Error::ConfigError(format!("Failed to decode secret key: {e}")))?;

        let secret_array: [u8; 32] = secret_bytes
            .try_into()
            .map_err(|_| Error::ConfigError("Invalid secret key length".to_string()))?;

        let signing_key = SigningKey::from_bytes(&secret_array);
        let derived_id = Self::derive_device_id(&signing_key.verifying_key());

        if derived_id != file.device_id {
            return Err(Error::ConfigError(
                "Device ID mismatch in identity file".to_string(),
            ));
        }

        Ok(Self {
            signing_key,
            device_id: file.device_id,
            path: Some(path),
        })
    }

    /// Load device identity from storage, or generate a new one if not found.
    ///
    /// This is the recommended way to get a device identity. On first run,
    /// a new identity will be generated and saved. On subsequent runs, the
    /// existing identity will be loaded.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The identity file exists but cannot be read/parsed
    /// - A new identity cannot be generated or saved
    pub fn load_or_generate() -> Result<Self> {
        let path = Self::default_path().ok_or_else(|| {
            Error::ConfigError("Cannot determine identity storage path".to_string())
        })?;

        if path.exists() {
            Self::load_from(path)
        } else {
            let mut identity = Self::generate()?;
            identity.path = Some(path);
            identity.save()?;
            Ok(identity)
        }
    }

    /// Save the device identity to storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity cannot be saved.
    pub fn save(&self) -> Result<()> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| Error::ConfigError("No path set for identity".to_string()))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::ConfigError(format!(
                    "Failed to create identity directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let file = IdentityFile {
            version: 1,
            secret_key: BASE64_STANDARD.encode(self.signing_key.to_bytes()),
            device_id: self.device_id,
        };

        let content = serde_json::to_string_pretty(&file)
            .map_err(|e| Error::Serialization(format!("Failed to serialize identity: {e}")))?;

        fs::write(path, content)
            .map_err(|e| Error::ConfigError(format!("Failed to write identity file: {e}")))?;

        Ok(())
    }

    /// Get the default identity storage path.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "localdrop", "LocalDrop")
            .map(|dirs| dirs.data_dir().join("identity.json"))
    }

    /// Sign data with this identity's private key.
    ///
    /// # Arguments
    ///
    /// * `data` - The data to sign
    ///
    /// # Returns
    ///
    /// The Ed25519 signature as bytes.
    #[must_use]
    pub fn sign(&self, data: &[u8]) -> [u8; 64] {
        self.signing_key.sign(data).to_bytes()
    }

    /// Verify a signature against a public key.
    ///
    /// # Arguments
    ///
    /// * `public_key_bytes` - The 32-byte Ed25519 public key
    /// * `data` - The data that was signed
    /// * `signature_bytes` - The 64-byte signature
    ///
    /// # Returns
    ///
    /// `true` if the signature is valid, `false` otherwise.
    #[must_use]
    pub fn verify(public_key_bytes: &[u8; 32], data: &[u8], signature_bytes: &[u8; 64]) -> bool {
        let Ok(verifying_key) = VerifyingKey::from_bytes(public_key_bytes) else {
            return false;
        };

        let signature = Signature::from_bytes(signature_bytes);
        verifying_key.verify(data, &signature).is_ok()
    }

    /// Verify a signature using a base64-encoded public key.
    ///
    /// # Arguments
    ///
    /// * `public_key_base64` - The base64-encoded public key
    /// * `data` - The data that was signed
    /// * `signature_bytes` - The 64-byte signature
    ///
    /// # Returns
    ///
    /// `true` if the signature is valid, `false` otherwise.
    #[must_use]
    pub fn verify_base64(public_key_base64: &str, data: &[u8], signature_bytes: &[u8; 64]) -> bool {
        let Ok(public_key_bytes) = BASE64_STANDARD.decode(public_key_base64) else {
            return false;
        };

        let Ok(public_key_array): std::result::Result<[u8; 32], _> = public_key_bytes.try_into()
        else {
            return false;
        };

        Self::verify(&public_key_array, data, signature_bytes)
    }

    /// Get the public key as raw bytes.
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Get the public key as a base64-encoded string.
    ///
    /// This format is suitable for storage in the trust database and
    /// transmission in protocol messages.
    #[must_use]
    pub fn public_key_base64(&self) -> String {
        BASE64_STANDARD.encode(self.public_key_bytes())
    }

    /// Get the verifying key (public key).
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Get the stable device ID.
    ///
    /// This ID is derived from the public key and remains stable across
    /// sessions as long as the same identity is used.
    #[must_use]
    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// Derive a stable device ID from a public key.
    ///
    /// Uses SHA-256 of the public key bytes, then takes the first 16 bytes
    /// to form a UUID v4 (with version/variant bits set).
    fn derive_device_id(verifying_key: &VerifyingKey) -> Uuid {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(b"localdrop:device_id:");
        hasher.update(verifying_key.as_bytes());
        let hash = hasher.finalize();

        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash[..16]);

        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;

        Uuid::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_identity() {
        let identity = DeviceIdentity::generate().expect("should generate identity");

        assert!(!identity.device_id().is_nil());

        assert_eq!(identity.public_key_bytes().len(), 32);

        let decoded = BASE64_STANDARD
            .decode(identity.public_key_base64())
            .expect("should decode");
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let identity = DeviceIdentity::generate().expect("should generate identity");
        let data = b"Hello, LocalDrop!";

        let signature = identity.sign(data);
        assert_eq!(signature.len(), 64);

        let public_key = identity.public_key_bytes();
        assert!(DeviceIdentity::verify(&public_key, data, &signature));

        let public_key_b64 = identity.public_key_base64();
        assert!(DeviceIdentity::verify_base64(
            &public_key_b64,
            data,
            &signature
        ));

        assert!(!DeviceIdentity::verify(
            &public_key,
            b"wrong data",
            &signature
        ));

        let mut bad_signature = signature;
        bad_signature[0] ^= 0xff;
        assert!(!DeviceIdentity::verify(&public_key, data, &bad_signature));
    }

    #[test]
    fn test_device_id_is_deterministic() {
        let identity = DeviceIdentity::generate().expect("should generate identity");

        let derived = DeviceIdentity::derive_device_id(&identity.verifying_key());
        assert_eq!(derived, identity.device_id());
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = tempfile::tempdir().expect("should create temp dir");
        let path = temp_dir.path().join("test_identity.json");

        let mut identity = DeviceIdentity::generate().expect("should generate identity");
        identity.path = Some(path.clone());
        identity.save().expect("should save identity");

        let loaded = DeviceIdentity::load_from(path).expect("should load identity");

        assert_eq!(loaded.device_id(), identity.device_id());
        assert_eq!(loaded.public_key_bytes(), identity.public_key_bytes());
        assert_eq!(loaded.public_key_base64(), identity.public_key_base64());

        let data = b"test data";
        let signature = loaded.sign(data);
        assert!(DeviceIdentity::verify(
            &loaded.public_key_bytes(),
            data,
            &signature
        ));
    }

    #[test]
    fn test_different_identities_have_different_keys() {
        let id1 = DeviceIdentity::generate().expect("should generate identity");
        let id2 = DeviceIdentity::generate().expect("should generate identity");

        assert_ne!(id1.device_id(), id2.device_id());
        assert_ne!(id1.public_key_bytes(), id2.public_key_bytes());
    }

    #[test]
    fn test_cross_identity_verification_fails() {
        let id1 = DeviceIdentity::generate().expect("should generate identity");
        let id2 = DeviceIdentity::generate().expect("should generate identity");

        let data = b"test data";
        let signature = id1.sign(data);

        assert!(!DeviceIdentity::verify(
            &id2.public_key_bytes(),
            data,
            &signature
        ));

        assert!(DeviceIdentity::verify(
            &id1.public_key_bytes(),
            data,
            &signature
        ));
    }
}
