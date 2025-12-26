//! Configuration management for LocalDrop.
//!
//! This module handles loading, saving, and managing LocalDrop configuration.
//!
//! ## Configuration File Locations
//!
//! | Platform | Path |
//! |----------|------|
//! | Linux | `~/.config/localdrop/config.toml` |
//! | macOS | `~/Library/Application Support/LocalDrop/config.toml` |
//! | Windows | `%APPDATA%\LocalDrop\config.toml` |
//!
//! ## Example
//!
//! ```rust,ignore
//! use localdrop_core::config::Config;
//!
//! let config = Config::load()?;
//! println!("Device name: {}", config.general.device_name);
//! ```

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Main configuration struct for LocalDrop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// General settings
    pub general: GeneralConfig,
    /// Network settings
    pub network: NetworkConfig,
    /// Transfer settings
    pub transfer: TransferConfig,
    /// Security settings
    pub security: SecurityConfig,
    /// Preview settings
    pub preview: PreviewConfig,
    /// History settings
    pub history: HistoryConfig,
    /// Trust settings
    pub trust: TrustConfig,
    /// Web interface settings
    pub web: WebConfig,
    /// UI settings
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            network: NetworkConfig::default(),
            transfer: TransferConfig::default(),
            security: SecurityConfig::default(),
            preview: PreviewConfig::default(),
            history: HistoryConfig::default(),
            trust: TrustConfig::default(),
            web: WebConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

/// General configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Display name on network
    pub device_name: String,
    /// Default code expiration time
    #[serde(with = "humantime_serde")]
    pub default_expire: Duration,
    /// Default output directory for received files
    pub default_output: Option<PathBuf>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            device_name: hostname::get().map_or_else(
                |_| "LocalDrop Device".to_string(),
                |h| h.to_string_lossy().to_string(),
            ),
            default_expire: Duration::from_secs(300),
            default_output: None,
        }
    }
}

/// Network configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Discovery port (UDP)
    pub port: u16,
    /// Transfer port range
    pub transfer_port_range: (u16, u16),
    /// Network interface (auto or specific)
    pub interface: String,
    /// Enable IPv6
    pub ipv6: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            port: crate::DEFAULT_DISCOVERY_PORT,
            transfer_port_range: (
                crate::DEFAULT_TRANSFER_PORT_START,
                crate::DEFAULT_TRANSFER_PORT_END,
            ),
            interface: "auto".to_string(),
            ipv6: true,
        }
    }
}

/// Transfer configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferConfig {
    /// Chunk size for transfers
    pub chunk_size: usize,
    /// Number of parallel chunk streams
    pub parallel_chunks: usize,
    /// Bandwidth limit (bytes per second, None for unlimited)
    pub bandwidth_limit: Option<u64>,
    /// Enable compression
    pub compression: CompressionMode,
    /// Verify checksums after transfer
    pub verify_checksum: bool,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size: crate::DEFAULT_CHUNK_SIZE,
            parallel_chunks: crate::DEFAULT_PARALLEL_CHUNKS,
            bandwidth_limit: None,
            compression: CompressionMode::Auto,
            verify_checksum: true,
        }
    }
}

/// Compression mode for transfers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompressionMode {
    /// Automatically compress compressible files
    #[default]
    Auto,
    /// Always compress
    Always,
    /// Never compress
    Never,
}

/// Security configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Require additional PIN
    pub require_pin: bool,
    /// Require manual approval
    pub require_approval: bool,
    /// Verify TLS certificates
    pub tls_verify: bool,
    /// Failed attempts before lockout
    pub rate_limit_attempts: u32,
    /// Lockout duration
    #[serde(with = "humantime_serde")]
    pub rate_limit_window: Duration,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_pin: false,
            require_approval: false,
            tls_verify: true,
            rate_limit_attempts: 3,
            rate_limit_window: Duration::from_secs(30),
        }
    }
}

/// Preview configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewConfig {
    /// Enable preview generation
    pub enabled: bool,
    /// Maximum image thumbnail size
    pub max_image_size: usize,
    /// Maximum text preview length
    pub max_text_length: usize,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_image_size: 50 * 1024,
            max_text_length: 1024,
        }
    }
}

/// History configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// Enable transfer history
    pub enabled: bool,
    /// Maximum history entries
    pub max_entries: usize,
    /// Auto-clear after days
    pub auto_clear_days: Option<u32>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 100,
            auto_clear_days: Some(30),
        }
    }
}

/// Trust configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustConfig {
    /// Enable trusted devices
    pub enabled: bool,
    /// Prompt to trust after transfer
    pub auto_prompt: bool,
    /// Default trust level
    pub default_level: TrustLevel,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_prompt: true,
            default_level: TrustLevel::AskEachTime,
        }
    }
}

/// Trust level for devices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Full trust - auto-connect
    Full,
    /// Ask for confirmation each time
    #[default]
    AskEachTime,
}

/// Web interface configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Enable web server by default
    pub enabled: bool,
    /// Web server port
    pub port: u16,
    /// Require authentication
    pub auth: bool,
    /// Bind to localhost only
    pub localhost_only: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 8080,
            auth: false,
            localhost_only: false,
        }
    }
}

/// UI configuration options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Theme (auto, light, dark)
    pub theme: String,
    /// Show QR codes
    pub show_qr: bool,
    /// Enable notifications
    pub notifications: bool,
    /// Play sound on complete
    pub sound: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "auto".to_string(),
            show_qr: true,
            notifications: true,
            sound: true,
        }
    }
}

impl Config {
    /// Load configuration from the default location.
    ///
    /// If the configuration file doesn't exist, returns the default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration file exists but cannot be read or parsed.
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| crate::error::Error::ConfigError(format!("Failed to read config: {e}")))?;

        toml::from_str(&content)
            .map_err(|e| crate::error::Error::ConfigError(format!("Failed to parse config: {e}")))
    }

    /// Save configuration to the default location.
    ///
    /// Creates the configuration directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be written.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::error::Error::ConfigError(format!("Failed to create config directory: {e}"))
            })?;
        }

        let content = toml::to_string_pretty(self).map_err(|e| {
            crate::error::Error::ConfigError(format!("Failed to serialize config: {e}"))
        })?;

        std::fs::write(&path, content)
            .map_err(|e| crate::error::Error::ConfigError(format!("Failed to write config: {e}")))
    }

    /// Get the default configuration directory path.
    #[must_use]
    pub fn config_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "localdrop", "LocalDrop")
            .map(|dirs| dirs.config_dir().to_path_buf())
    }

    /// Get the full path to the configuration file.
    #[must_use]
    pub fn config_path() -> PathBuf {
        Self::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("config.toml")
    }
}

mod humantime_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!("{}s", duration.as_secs()))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.strip_suffix('s')
            .map(|secs| {
                secs.parse()
                    .map(Duration::from_secs)
                    .map_err(serde::de::Error::custom)
            })
            .or_else(|| {
                s.strip_suffix('m').map(|mins| {
                    mins.parse::<u64>()
                        .map(|m| Duration::from_secs(m * 60))
                        .map_err(serde::de::Error::custom)
                })
            })
            .unwrap_or_else(|| Err(serde::de::Error::custom("invalid duration format")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Helper to create a temp config environment for testing
    fn setup_temp_config(dir: &TempDir) -> PathBuf {
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        config_dir.join("config.toml")
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert_eq!(config.network.port, crate::DEFAULT_DISCOVERY_PORT);
        assert!(config.security.tls_verify);
        assert_eq!(config.general.default_expire, Duration::from_secs(300));
    }

    #[test]
    fn test_config_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = setup_temp_config(&temp_dir);

        let mut original = Config::default();
        original.general.device_name = "Test Device".to_string();
        original.network.port = 12345;
        original.security.require_pin = true;
        original.transfer.chunk_size = 2 * 1024 * 1024;

        let content = toml::to_string_pretty(&original).expect("serialize");
        std::fs::write(&config_path, &content).expect("write");

        let loaded_content = std::fs::read_to_string(&config_path).expect("read");
        let loaded: Config = toml::from_str(&loaded_content).expect("parse");

        assert_eq!(loaded.general.device_name, "Test Device");
        assert_eq!(loaded.network.port, 12345);
        assert!(loaded.security.require_pin);
        assert_eq!(loaded.transfer.chunk_size, 2 * 1024 * 1024);
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config);
        assert!(toml_str.is_ok(), "Config should serialize to TOML");

        let toml_str = toml_str.unwrap();
        assert!(
            toml_str.contains("[general]"),
            "Should have [general] section"
        );
        assert!(
            toml_str.contains("[network]"),
            "Should have [network] section"
        );
        assert!(
            toml_str.contains("[transfer]"),
            "Should have [transfer] section"
        );
    }

    #[test]
    fn test_config_deserialization_partial() {
        let partial_toml = r#"
[general]
device_name = "My Custom Device"

[network]
port = 9999
"#;

        let config: Config = toml::from_str(partial_toml).expect("parse partial config");

        assert_eq!(config.general.device_name, "My Custom Device");
        assert_eq!(config.network.port, 9999);

        assert_eq!(config.general.default_expire, Duration::from_secs(300));
        assert_eq!(config.transfer.chunk_size, crate::DEFAULT_CHUNK_SIZE);
    }

    #[test]
    fn test_config_path() {
        let path = Config::config_path();
        assert!(
            path.ends_with("config.toml"),
            "Config path should end with config.toml"
        );
    }

    #[test]
    fn test_humantime_duration_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");

        assert!(
            toml_str.contains("300s") || toml_str.contains("5m"),
            "Duration should be serialized as human-readable"
        );
    }

    #[test]
    fn test_compression_mode_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");

        assert!(
            toml_str.contains("compression = \"auto\""),
            "Compression mode should be serialized as lowercase"
        );
    }

    #[test]
    fn test_trust_level_serialization() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");

        assert!(
            toml_str.contains("default_level = \"ask_each_time\""),
            "Trust level should be serialized as snake_case"
        );
    }
}
