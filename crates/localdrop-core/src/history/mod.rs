//! Transfer history tracking for LocalDrop.
//!
//! This module provides persistent storage for transfer history,
//! allowing users to review past file transfers.
//!
//! ## Features
//!
//! - Records all completed transfers (sent and received)
//! - Respects `max_entries` limit from configuration
//! - Auto-clears old entries based on `auto_clear_days`
//! - Persists history to JSON file

use std::fs;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::HistoryConfig;
use crate::error::{Error, Result};

/// Direction of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    /// Files were sent to another device
    Sent,
    /// Files were received from another device
    Received,
}

impl std::fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sent => write!(f, "Sent"),
            Self::Received => write!(f, "Received"),
        }
    }
}

/// State of a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Transfer completed successfully
    Completed,
    /// Transfer failed
    Failed,
    /// Transfer was cancelled
    Cancelled,
}

impl std::fmt::Display for TransferState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Information about a file in a transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryFileEntry {
    /// File name
    pub name: String,
    /// File size in bytes
    pub size: u64,
    /// Whether this file was transferred successfully
    pub success: bool,
}

/// A single transfer history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferHistoryEntry {
    /// Unique identifier for this transfer
    pub id: Uuid,
    /// Unix timestamp when transfer started
    pub timestamp: u64,
    /// Direction of the transfer
    pub direction: TransferDirection,
    /// Name of the remote device
    pub device_name: String,
    /// ID of the remote device (if known)
    pub device_id: Option<Uuid>,
    /// Share code used for the transfer
    pub share_code: String,
    /// Files included in the transfer
    pub files: Vec<HistoryFileEntry>,
    /// Total size of all files in bytes
    pub total_bytes: u64,
    /// Bytes actually transferred
    pub bytes_transferred: u64,
    /// Final state of the transfer
    pub state: TransferState,
    /// Duration of the transfer in seconds
    pub duration_secs: u64,
    /// Transfer speed in bytes per second (if completed)
    pub speed_bps: Option<u64>,
    /// Output directory (for received files)
    pub output_dir: Option<PathBuf>,
    /// Error message (if failed)
    pub error_message: Option<String>,
}

impl TransferHistoryEntry {
    /// Create a new history entry with the current timestamp.
    #[must_use]
    pub fn new(direction: TransferDirection, device_name: String, share_code: String) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            id: Uuid::new_v4(),
            timestamp,
            direction,
            device_name,
            device_id: None,
            share_code,
            files: Vec::new(),
            total_bytes: 0,
            bytes_transferred: 0,
            state: TransferState::Completed,
            duration_secs: 0,
            speed_bps: None,
            output_dir: None,
            error_message: None,
        }
    }

    /// Set the device ID.
    #[must_use]
    pub fn with_device_id(mut self, device_id: Uuid) -> Self {
        self.device_id = Some(device_id);
        self
    }

    /// Add files to the entry.
    #[must_use]
    pub fn with_files(mut self, files: Vec<HistoryFileEntry>) -> Self {
        self.total_bytes = files.iter().map(|f| f.size).sum();
        self.files = files;
        self
    }

    /// Set the transfer state.
    #[must_use]
    pub fn with_state(mut self, state: TransferState) -> Self {
        self.state = state;
        self
    }

    /// Set transfer statistics.
    #[must_use]
    pub fn with_stats(mut self, bytes_transferred: u64, duration_secs: u64) -> Self {
        self.bytes_transferred = bytes_transferred;
        self.duration_secs = duration_secs;
        if duration_secs > 0 {
            self.speed_bps = Some(bytes_transferred / duration_secs);
        }
        self
    }

    /// Set the output directory.
    #[must_use]
    pub fn with_output_dir(mut self, path: PathBuf) -> Self {
        self.output_dir = Some(path);
        self
    }

    /// Set an error message.
    #[must_use]
    pub fn with_error(mut self, message: String) -> Self {
        self.error_message = Some(message);
        self.state = TransferState::Failed;
        self
    }

    /// Get the timestamp as a human-readable string.
    #[must_use]
    pub fn formatted_timestamp(&self) -> String {
        use chrono::{DateTime, Utc};
        let timestamp_i64 = i64::try_from(self.timestamp).unwrap_or(i64::MAX);
        let dt = DateTime::<Utc>::from_timestamp(timestamp_i64, 0);
        dt.map_or_else(
            || "Unknown".to_string(),
            |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
        )
    }
}

/// Serializable wrapper for the history database.
#[derive(Debug, Serialize, Deserialize)]
struct HistoryDatabase {
    /// Version of the history database format
    version: u32,
    /// List of transfer history entries
    entries: Vec<TransferHistoryEntry>,
}

impl Default for HistoryDatabase {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

/// Transfer history store.
#[derive(Debug)]
pub struct HistoryStore {
    /// Path to the history database file
    path: PathBuf,
    /// History entries (newest first)
    entries: Vec<TransferHistoryEntry>,
    /// Configuration settings
    config: HistoryConfig,
}

impl HistoryStore {
    /// Load the history store from the default location.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be loaded.
    pub fn load() -> Result<Self> {
        let path = Self::default_path().unwrap_or_else(|| PathBuf::from("history.json"));
        Self::load_from(path, HistoryConfig::default())
    }

    /// Load the history store with custom configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be loaded.
    pub fn load_with_config(config: HistoryConfig) -> Result<Self> {
        let path = Self::default_path().unwrap_or_else(|| PathBuf::from("history.json"));
        Self::load_from(path, config)
    }

    /// Load from a specific path.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be loaded.
    pub fn load_from(path: PathBuf, config: HistoryConfig) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path,
                entries: Vec::new(),
                config,
            });
        }

        let file = fs::File::open(&path).map_err(|e| {
            Error::ConfigError(format!(
                "Failed to open history store at {}: {}",
                path.display(),
                e
            ))
        })?;

        let reader = BufReader::new(file);
        let db: HistoryDatabase = serde_json::from_reader(reader).map_err(|e| {
            Error::ConfigError(format!(
                "Failed to parse history store at {}: {}",
                path.display(),
                e
            ))
        })?;

        let mut store = Self {
            path,
            entries: db.entries,
            config,
        };

        store.apply_auto_clear();

        Ok(store)
    }

    /// Get the default history store path.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "localdrop", "LocalDrop")
            .map(|dirs| dirs.data_dir().join("history.json"))
    }

    /// Save the history store.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be saved.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::ConfigError(format!(
                    "Failed to create history store directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let db = HistoryDatabase {
            version: 1,
            entries: self.entries.clone(),
        };

        let file = fs::File::create(&self.path).map_err(|e| {
            Error::ConfigError(format!(
                "Failed to create history store at {}: {}",
                self.path.display(),
                e
            ))
        })?;

        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &db).map_err(|e| {
            Error::ConfigError(format!(
                "Failed to write history store at {}: {}",
                self.path.display(),
                e
            ))
        })?;

        Ok(())
    }

    /// Add a new entry to the history.
    ///
    /// The entry is added at the beginning (newest first).
    /// Old entries are pruned if we exceed `max_entries`.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be saved.
    pub fn add(&mut self, entry: TransferHistoryEntry) -> Result<()> {
        self.entries.insert(0, entry);

        if self.entries.len() > self.config.max_entries {
            self.entries.truncate(self.config.max_entries);
        }

        self.save()
    }

    /// List history entries.
    ///
    /// # Arguments
    ///
    /// * `limit` - Maximum number of entries to return (None for all)
    #[must_use]
    pub fn list(&self, limit: Option<usize>) -> &[TransferHistoryEntry] {
        limit.map_or_else(
            || &self.entries[..],
            |n| &self.entries[..n.min(self.entries.len())],
        )
    }

    /// Get an entry by index (0 = most recent).
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&TransferHistoryEntry> {
        self.entries.get(index)
    }

    /// Get an entry by ID.
    #[must_use]
    pub fn find_by_id(&self, id: &Uuid) -> Option<&TransferHistoryEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    /// Get the total number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the history is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all history entries.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be saved.
    pub fn clear(&mut self) -> Result<()> {
        self.entries.clear();
        self.save()
    }

    /// Get the path to the history store file.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Apply auto-clear based on configuration.
    fn apply_auto_clear(&mut self) {
        if let Some(days) = self.config.auto_clear_days {
            let cutoff = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(Duration::from_secs(u64::from(days) * 24 * 60 * 60).as_secs());

            let len_before = self.entries.len();
            self.entries.retain(|e| e.timestamp >= cutoff);

            if self.entries.len() < len_before {
                tracing::debug!(
                    removed = len_before - self.entries.len(),
                    "Auto-cleared old history entries"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_entry() -> TransferHistoryEntry {
        TransferHistoryEntry::new(
            TransferDirection::Sent,
            "Test Device".to_string(),
            "A7K9".to_string(),
        )
        .with_files(vec![HistoryFileEntry {
            name: "test.txt".to_string(),
            size: 1024,
            success: true,
        }])
        .with_stats(1024, 1)
    }

    #[test]
    fn test_history_store_save_and_load() {
        let tmp_dir = TempDir::new().unwrap();
        let history_path = tmp_dir.path().join("history.json");

        let mut store =
            HistoryStore::load_from(history_path.clone(), HistoryConfig::default()).unwrap();
        let entry = create_test_entry();
        let entry_id = entry.id;
        store.add(entry).unwrap();

        let loaded_store = HistoryStore::load_from(history_path, HistoryConfig::default()).unwrap();
        assert_eq!(loaded_store.len(), 1);
        assert!(loaded_store.find_by_id(&entry_id).is_some());
    }

    #[test]
    fn test_history_max_entries() {
        let tmp_dir = TempDir::new().unwrap();
        let history_path = tmp_dir.path().join("history.json");

        let config = HistoryConfig {
            enabled: true,
            max_entries: 3,
            auto_clear_days: None,
        };

        let mut store = HistoryStore::load_from(history_path, config).unwrap();

        for i in 0..5 {
            let entry = TransferHistoryEntry::new(
                TransferDirection::Sent,
                format!("Device {i}"),
                format!("CODE{i}"),
            );
            store.add(entry).unwrap();
        }

        assert_eq!(store.len(), 3);

        assert_eq!(store.get(0).unwrap().share_code, "CODE4");
        assert_eq!(store.get(1).unwrap().share_code, "CODE3");
        assert_eq!(store.get(2).unwrap().share_code, "CODE2");
    }

    #[test]
    fn test_history_clear() {
        let tmp_dir = TempDir::new().unwrap();
        let history_path = tmp_dir.path().join("history.json");

        let mut store = HistoryStore::load_from(history_path, HistoryConfig::default()).unwrap();
        store.add(create_test_entry()).unwrap();
        store.add(create_test_entry()).unwrap();

        assert_eq!(store.len(), 2);

        store.clear().unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn test_load_nonexistent_file() {
        let tmp_dir = TempDir::new().unwrap();
        let history_path = tmp_dir.path().join("nonexistent.json");

        let store = HistoryStore::load_from(history_path, HistoryConfig::default()).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn test_transfer_direction_display() {
        assert_eq!(format!("{}", TransferDirection::Sent), "Sent");
        assert_eq!(format!("{}", TransferDirection::Received), "Received");
    }

    #[test]
    fn test_transfer_state_display() {
        assert_eq!(format!("{}", TransferState::Completed), "Completed");
        assert_eq!(format!("{}", TransferState::Failed), "Failed");
        assert_eq!(format!("{}", TransferState::Cancelled), "Cancelled");
    }

    #[test]
    fn test_entry_builder() {
        let entry = TransferHistoryEntry::new(
            TransferDirection::Received,
            "Sender".to_string(),
            "ABCD".to_string(),
        )
        .with_device_id(Uuid::new_v4())
        .with_files(vec![
            HistoryFileEntry {
                name: "file1.txt".to_string(),
                size: 100,
                success: true,
            },
            HistoryFileEntry {
                name: "file2.txt".to_string(),
                size: 200,
                success: true,
            },
        ])
        .with_stats(300, 10)
        .with_output_dir(PathBuf::from("/downloads"));

        assert_eq!(entry.total_bytes, 300);
        assert_eq!(entry.bytes_transferred, 300);
        assert_eq!(entry.duration_secs, 10);
        assert_eq!(entry.speed_bps, Some(30));
        assert!(entry.device_id.is_some());
        assert!(entry.output_dir.is_some());
    }

    #[test]
    fn test_entry_with_error() {
        let entry = TransferHistoryEntry::new(
            TransferDirection::Sent,
            "Device".to_string(),
            "CODE".to_string(),
        )
        .with_error("Connection lost".to_string());

        assert_eq!(entry.state, TransferState::Failed);
        assert_eq!(entry.error_message, Some("Connection lost".to_string()));
    }
}
