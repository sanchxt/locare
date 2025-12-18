//! Resume state persistence for interrupted transfers.
//!
//! This module handles saving and loading transfer state to/from disk,
//! enabling resumption of interrupted file transfers.

use std::path::{Path, PathBuf};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::error::{Error, Result};

use super::ResumeState;

/// File extension for resume state files.
pub const RESUME_FILE_EXTENSION: &str = ".localdrop-resume";

/// Default expiry duration for resume states (7 days).
const DEFAULT_EXPIRY_DAYS: i64 = 7;

/// Manages persistence of transfer resume states.
///
/// Resume files are stored in platform-specific directories:
/// - Linux: `~/.local/share/localdrop/resume/`
/// - macOS: `~/Library/Application Support/LocalDrop/resume/`
/// - Windows: `%APPDATA%\LocalDrop\resume\`
pub struct ResumeManager {
    /// Directory where resume files are stored.
    resume_dir: PathBuf,
}

impl ResumeManager {
    /// Create a new ResumeManager with the default platform-specific directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the resume directory cannot be created.
    pub async fn new() -> Result<Self> {
        let resume_dir = Self::default_resume_dir()?;
        Self::with_dir(resume_dir).await
    }

    /// Create a new ResumeManager with a custom directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub async fn with_dir(resume_dir: PathBuf) -> Result<Self> {
        // Ensure the directory exists
        fs::create_dir_all(&resume_dir).await.map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to create resume directory: {e}"),
            ))
        })?;

        Ok(Self { resume_dir })
    }

    /// Get the default platform-specific resume directory.
    fn default_resume_dir() -> Result<PathBuf> {
        let data_dir = directories::ProjectDirs::from("com", "localdrop", "LocalDrop")
            .map(|dirs| dirs.data_dir().to_path_buf())
            .unwrap_or_else(|| {
                // Fallback to current directory
                PathBuf::from(".localdrop")
            });

        Ok(data_dir.join("resume"))
    }

    /// Get the file path for a transfer's resume state.
    fn resume_file_path(&self, transfer_id: &Uuid) -> PathBuf {
        self.resume_dir
            .join(format!("{transfer_id}{RESUME_FILE_EXTENSION}"))
    }

    /// Save a resume state to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the state cannot be serialized or written.
    pub async fn save(&self, state: &ResumeState) -> Result<()> {
        let path = self.resume_file_path(&state.transfer_id);

        // Serialize to JSON
        let json = serde_json::to_string_pretty(state).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to serialize resume state: {e}"),
            ))
        })?;

        // Write atomically by writing to temp file first
        let temp_path = path.with_extension("tmp");

        let mut file = fs::File::create(&temp_path).await?;
        file.write_all(json.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);

        // Rename to final path (atomic on most filesystems)
        fs::rename(&temp_path, &path).await?;

        tracing::debug!(
            transfer_id = %state.transfer_id,
            path = %path.display(),
            "Saved resume state"
        );

        Ok(())
    }

    /// Load a resume state by transfer ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub async fn load(&self, transfer_id: &Uuid) -> Result<Option<ResumeState>> {
        let path = self.resume_file_path(transfer_id);

        if !path.exists() {
            return Ok(None);
        }

        self.load_from_path(&path).await.map(Some)
    }

    /// Load a resume state from a specific path.
    async fn load_from_path(&self, path: &Path) -> Result<ResumeState> {
        let mut file = fs::File::open(path).await?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).await?;

        let state: ResumeState = serde_json::from_str(&contents).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to parse resume state: {e}"),
            ))
        })?;

        tracing::debug!(
            transfer_id = %state.transfer_id,
            path = %path.display(),
            "Loaded resume state"
        );

        Ok(state)
    }

    /// Find a resume state by share code.
    ///
    /// Scans all resume files to find one matching the given code.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub async fn find_by_code(&self, code: &str) -> Result<Option<ResumeState>> {
        let mut entries = fs::read_dir(&self.resume_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // Skip non-resume files
            if !path
                .extension()
                .is_some_and(|ext| ext == "localdrop-resume")
            {
                // Check if filename ends with the extension (since it includes the dot)
                let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !filename.ends_with(RESUME_FILE_EXTENSION) {
                    continue;
                }
            }

            // Try to load and check the code
            if let Ok(state) = self.load_from_path(&path).await {
                if state.code == code {
                    return Ok(Some(state));
                }
            }
        }

        Ok(None)
    }

    /// Delete a resume state.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be deleted.
    pub async fn delete(&self, transfer_id: &Uuid) -> Result<()> {
        let path = self.resume_file_path(transfer_id);

        if path.exists() {
            fs::remove_file(&path).await?;
            tracing::debug!(
                transfer_id = %transfer_id,
                path = %path.display(),
                "Deleted resume state"
            );
        }

        Ok(())
    }

    /// Clean up expired resume states.
    ///
    /// Removes all resume states older than the default expiry (7 days).
    ///
    /// # Returns
    ///
    /// The number of expired states that were cleaned up.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub async fn cleanup_expired(&self) -> Result<usize> {
        self.cleanup_older_than(chrono::Duration::days(DEFAULT_EXPIRY_DAYS))
            .await
    }

    /// Clean up resume states older than the specified duration.
    ///
    /// # Returns
    ///
    /// The number of expired states that were cleaned up.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub async fn cleanup_older_than(&self, max_age: chrono::Duration) -> Result<usize> {
        let cutoff = chrono::Utc::now() - max_age;
        let mut cleaned = 0;

        let mut entries = fs::read_dir(&self.resume_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // Skip non-resume files
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !filename.ends_with(RESUME_FILE_EXTENSION) {
                continue;
            }

            // Try to load and check the age
            if let Ok(state) = self.load_from_path(&path).await {
                if state.updated_at < cutoff {
                    if let Err(e) = fs::remove_file(&path).await {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Failed to delete expired resume state"
                        );
                    } else {
                        tracing::debug!(
                            transfer_id = %state.transfer_id,
                            updated_at = %state.updated_at,
                            "Cleaned up expired resume state"
                        );
                        cleaned += 1;
                    }
                }
            }
        }

        if cleaned > 0 {
            tracing::info!(count = cleaned, "Cleaned up expired resume states");
        }

        Ok(cleaned)
    }

    /// List all resume states.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub async fn list(&self) -> Result<Vec<ResumeState>> {
        let mut states = Vec::new();
        let mut entries = fs::read_dir(&self.resume_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // Skip non-resume files
            let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !filename.ends_with(RESUME_FILE_EXTENSION) {
                continue;
            }

            // Try to load
            if let Ok(state) = self.load_from_path(&path).await {
                states.push(state);
            }
        }

        // Sort by updated_at descending (most recent first)
        states.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(states)
    }

    /// Get the resume directory path.
    #[must_use]
    pub fn resume_dir(&self) -> &Path {
        &self.resume_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::FileMetadata;
    use tempfile::TempDir;

    fn create_test_state(code: &str) -> ResumeState {
        let files = vec![FileMetadata {
            relative_path: PathBuf::from("test.txt"),
            size: 1024,
            mime_type: Some("text/plain".to_string()),
            created: None,
            modified: None,
            permissions: None,
            is_symlink: false,
            symlink_target: None,
        }];

        ResumeState::new(
            Uuid::new_v4(),
            code,
            files,
            "TestDevice",
            Uuid::new_v4(),
            PathBuf::from("/tmp/output"),
        )
    }

    #[tokio::test]
    async fn test_resume_manager_save_load() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
            .await
            .expect("create manager");

        let state = create_test_state("ABC-123");
        let transfer_id = state.transfer_id;

        // Save
        manager.save(&state).await.expect("save state");

        // Load
        let loaded = manager
            .load(&transfer_id)
            .await
            .expect("load state")
            .expect("state should exist");

        assert_eq!(loaded.transfer_id, transfer_id);
        assert_eq!(loaded.code, "ABC-123");
        assert_eq!(loaded.files.len(), 1);
    }

    #[tokio::test]
    async fn test_resume_manager_find_by_code() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
            .await
            .expect("create manager");

        // Save multiple states
        let state1 = create_test_state("ABC-123");
        let state2 = create_test_state("XYZ-789");

        manager.save(&state1).await.expect("save state1");
        manager.save(&state2).await.expect("save state2");

        // Find by code
        let found = manager
            .find_by_code("XYZ-789")
            .await
            .expect("find by code")
            .expect("state should exist");

        assert_eq!(found.code, "XYZ-789");

        // Should not find non-existent code
        let not_found = manager
            .find_by_code("NOT-EXIST")
            .await
            .expect("find by code");

        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_resume_manager_delete() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
            .await
            .expect("create manager");

        let state = create_test_state("ABC-123");
        let transfer_id = state.transfer_id;

        // Save
        manager.save(&state).await.expect("save state");

        // Verify it exists
        assert!(manager.load(&transfer_id).await.expect("load").is_some());

        // Delete
        manager.delete(&transfer_id).await.expect("delete state");

        // Verify it's gone
        assert!(manager.load(&transfer_id).await.expect("load").is_none());
    }

    #[tokio::test]
    async fn test_resume_manager_list() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
            .await
            .expect("create manager");

        // Save multiple states
        let state1 = create_test_state("ABC-123");
        let state2 = create_test_state("XYZ-789");

        manager.save(&state1).await.expect("save state1");
        manager.save(&state2).await.expect("save state2");

        // List all
        let states = manager.list().await.expect("list states");

        assert_eq!(states.len(), 2);
    }

    #[tokio::test]
    async fn test_resume_manager_load_nonexistent() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let manager = ResumeManager::with_dir(temp_dir.path().to_path_buf())
            .await
            .expect("create manager");

        let result = manager.load(&Uuid::new_v4()).await.expect("load");

        assert!(result.is_none());
    }
}
