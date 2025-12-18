//! File operations for LocalDrop.
//!
//! This module handles:
//! - File and directory enumeration
//! - Chunking files for transfer
//! - Metadata preservation
//! - Path sanitization
//!
//! ## Metadata Preservation
//!
//! - Relative path structure
//! - File permissions (Unix only)
//! - Timestamps (created, modified)
//! - Symlinks: Option to follow or preserve
//!
//! ## Platform Support
//!
//! - Unix: Full permission support (mode bits), native symlinks
//! - Windows: No permission support, symlink fallback to copy

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::Result;

// ============================================================================
// Platform-Aware Permission Handling
// ============================================================================

/// Get Unix file permissions from metadata.
///
/// Returns the mode bits on Unix systems, or None on other platforms.
#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)] // Must return Option to match non-unix signature
fn get_permissions(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

/// Get Unix file permissions from metadata.
///
/// Returns None on non-Unix platforms as they don't use mode bits.
#[cfg(not(unix))]
fn get_permissions(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Apply Unix file permissions to a file.
///
/// On Unix systems, sets the mode bits. On other platforms, this is a no-op.
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `permissions` - Optional mode bits to apply
///
/// # Errors
///
/// Returns an error if permission change fails on Unix.
#[cfg(unix)]
pub fn apply_permissions(path: &Path, permissions: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = permissions {
        // Only apply user/group/other permissions (mask out file type bits)
        let mode = mode & 0o7777;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Apply Unix file permissions to a file.
///
/// No-op on non-Unix platforms.
#[cfg(not(unix))]
pub fn apply_permissions(_path: &Path, _permissions: Option<u32>) -> Result<()> {
    // Windows doesn't use Unix-style permissions
    Ok(())
}

// ============================================================================
// Symlink Handling
// ============================================================================

/// How to handle symlinks during file enumeration and transfer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkMode {
    /// Follow symlinks and transfer the target content (default)
    #[default]
    Follow,
    /// Preserve symlinks as symlinks (Unix only, falls back to Follow on Windows)
    Preserve,
    /// Skip symlinks entirely
    Skip,
}

/// Read the target of a symlink.
///
/// # Arguments
///
/// * `path` - Path to the symlink
/// * `is_symlink` - Whether the path is known to be a symlink
///
/// # Returns
///
/// The symlink target path, or None if not a symlink or read fails.
fn get_symlink_target(path: &Path, is_symlink: bool) -> Option<PathBuf> {
    if is_symlink {
        std::fs::read_link(path).ok()
    } else {
        None
    }
}

/// Create a symlink on Unix systems.
///
/// # Arguments
///
/// * `link_path` - Path where the symlink will be created
/// * `target` - Target path the symlink points to
///
/// # Errors
///
/// Returns an error if symlink creation fails.
#[cfg(unix)]
pub fn create_symlink(link_path: &Path, target: &Path) -> Result<()> {
    // Create parent directories if needed
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::os::unix::fs::symlink(target, link_path)?;
    Ok(())
}

/// Create a symlink on Windows systems.
///
/// Windows symlinks require elevated privileges or Developer Mode.
/// This function falls back to copying the target content instead.
///
/// # Arguments
///
/// * `link_path` - Path where the symlink would be created
/// * `target` - Target path the symlink points to
///
/// # Errors
///
/// Returns an error if the fallback copy fails.
#[cfg(windows)]
pub fn create_symlink(link_path: &Path, target: &Path) -> Result<()> {
    tracing::warn!(
        "Symlinks require elevation on Windows, copying target instead: {} -> {}",
        link_path.display(),
        target.display()
    );

    // Create parent directories if needed
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Fall back to copying
    if target.is_dir() {
        copy_dir_recursive(target, link_path)?;
    } else if target.exists() {
        std::fs::copy(target, link_path)?;
    } else {
        // Target doesn't exist, create an empty file as placeholder
        std::fs::write(link_path, b"")?;
        tracing::warn!(
            "Symlink target does not exist, created empty placeholder: {}",
            link_path.display()
        );
    }
    Ok(())
}

/// Recursively copy a directory (used as symlink fallback on Windows).
#[cfg(windows)]
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Metadata for a file being transferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Relative path from share root
    pub relative_path: PathBuf,
    /// File size in bytes
    pub size: u64,
    /// MIME type
    pub mime_type: Option<String>,
    /// Created timestamp
    pub created: Option<SystemTime>,
    /// Modified timestamp
    pub modified: Option<SystemTime>,
    /// Unix permissions (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<u32>,
    /// Whether this is a symlink
    pub is_symlink: bool,
    /// Symlink target (if is_symlink)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
}

impl FileMetadata {
    /// Create metadata from a file path.
    ///
    /// # Arguments
    ///
    /// * `path` - Absolute path to the file
    /// * `base` - Base directory for computing relative path
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_path(path: &Path, base: &Path) -> Result<Self> {
        // Use symlink_metadata to get info about the link itself, not the target
        let symlink_metadata = std::fs::symlink_metadata(path)?;
        let is_symlink = symlink_metadata.is_symlink();

        // For size and other attributes, follow the symlink if it exists
        let metadata = if is_symlink {
            std::fs::metadata(path).unwrap_or_else(|_| symlink_metadata.clone())
        } else {
            symlink_metadata
        };

        let relative_path = path.strip_prefix(base).unwrap_or(path).to_path_buf();
        let mime_type = mime_guess::from_path(path).first().map(|m| m.to_string());
        let permissions = get_permissions(&metadata);
        let symlink_target = get_symlink_target(path, is_symlink);

        Ok(Self {
            relative_path,
            size: metadata.len(),
            mime_type,
            created: metadata.created().ok(),
            modified: metadata.modified().ok(),
            permissions,
            is_symlink,
            symlink_target,
        })
    }

    /// Get the file name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.relative_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
    }
}

/// A file chunk for transfer.
#[derive(Debug, Clone)]
pub struct FileChunk {
    /// File index in the transfer
    pub file_index: usize,
    /// Chunk index within the file
    pub chunk_index: u64,
    /// Chunk data
    pub data: Vec<u8>,
    /// xxHash64 checksum
    pub checksum: u64,
    /// Whether this is the last chunk
    pub is_last: bool,
}

/// Options for file enumeration.
#[derive(Debug, Clone, Default)]
pub struct EnumerateOptions {
    /// How to handle symlinks during enumeration
    pub symlink_mode: SymlinkMode,
    /// Include hidden files (files starting with '.')
    pub include_hidden: bool,
    /// Maximum depth for directories (None = unlimited)
    pub max_depth: Option<usize>,
}

impl EnumerateOptions {
    /// Create options that follow symlinks (default behavior).
    #[must_use]
    pub fn follow_symlinks() -> Self {
        Self {
            symlink_mode: SymlinkMode::Follow,
            ..Default::default()
        }
    }

    /// Create options that preserve symlinks as symlinks.
    #[must_use]
    pub fn preserve_symlinks() -> Self {
        Self {
            symlink_mode: SymlinkMode::Preserve,
            ..Default::default()
        }
    }

    /// Create options that skip symlinks entirely.
    #[must_use]
    pub fn skip_symlinks() -> Self {
        Self {
            symlink_mode: SymlinkMode::Skip,
            ..Default::default()
        }
    }

    /// Set whether to include hidden files.
    #[must_use]
    pub fn with_hidden(mut self, include_hidden: bool) -> Self {
        self.include_hidden = include_hidden;
        self
    }

    /// Set maximum directory depth.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = Some(max_depth);
        self
    }

    /// Check if symlinks should be followed based on symlink_mode.
    #[must_use]
    pub fn should_follow_symlinks(&self) -> bool {
        matches!(self.symlink_mode, SymlinkMode::Follow)
    }
}

/// Enumerate files for sharing.
///
/// # Arguments
///
/// * `paths` - Paths to files and directories to share
/// * `options` - Enumeration options
///
/// # Errors
///
/// Returns an error if enumeration fails.
pub fn enumerate_files(paths: &[PathBuf], options: &EnumerateOptions) -> Result<Vec<FileMetadata>> {
    let mut files = Vec::new();

    for path in paths {
        if path.is_file() {
            let base = path.parent().unwrap_or(path);
            files.push(FileMetadata::from_path(path, base)?);
        } else if path.is_dir() {
            enumerate_directory(path, path, options, &mut files)?;
        }
    }

    Ok(files)
}

fn enumerate_directory(
    dir: &Path,
    base: &Path,
    options: &EnumerateOptions,
    files: &mut Vec<FileMetadata>,
) -> Result<()> {
    let walker = walkdir::WalkDir::new(dir)
        .follow_links(options.should_follow_symlinks())
        .max_depth(options.max_depth.unwrap_or(usize::MAX));

    for entry in walker.into_iter().filter_map(std::result::Result::ok) {
        let path = entry.path();

        // Skip hidden files if requested
        if !options.include_hidden {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
        }

        // Get symlink metadata (info about the link itself)
        let Ok(symlink_meta) = std::fs::symlink_metadata(path) else {
            continue; // Skip files we can't read metadata for
        };

        let is_symlink = symlink_meta.is_symlink();

        // Handle symlinks according to mode
        if is_symlink {
            match options.symlink_mode {
                SymlinkMode::Skip => {} // Do nothing, loop continues naturally
                SymlinkMode::Preserve => {
                    // Include the symlink itself as a file entry
                    files.push(FileMetadata::from_path(path, base)?);
                }
                SymlinkMode::Follow => {
                    // Follow the symlink - walkdir handles this, include if it's a file
                    if path.is_file() {
                        files.push(FileMetadata::from_path(path, base)?);
                    }
                }
            }
        } else if path.is_file() {
            files.push(FileMetadata::from_path(path, base)?);
        }
    }

    Ok(())
}

/// Chunker for reading file chunks.
#[derive(Debug)]
pub struct FileChunker {
    /// Chunk size in bytes
    pub chunk_size: usize,
}

impl FileChunker {
    /// Create a new chunker with the given chunk size.
    #[must_use]
    pub const fn new(chunk_size: usize) -> Self {
        Self { chunk_size }
    }

    /// Read chunks from a file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file
    /// * `file_index` - Index of the file in the transfer
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub async fn read_chunks(&self, path: &Path, file_index: usize) -> Result<Vec<FileChunk>> {
        use tokio::io::AsyncReadExt;

        use crate::crypto::xxhash64;

        let mut file = tokio::fs::File::open(path).await?;
        let metadata = file.metadata().await?;
        let file_size = metadata.len();

        let mut chunks = Vec::new();
        let mut chunk_index: u64 = 0;
        let mut bytes_read_total: u64 = 0;

        loop {
            let mut buffer = vec![0u8; self.chunk_size];
            let bytes_read = file.read(&mut buffer).await?;

            if bytes_read == 0 {
                break;
            }

            buffer.truncate(bytes_read);
            bytes_read_total += bytes_read as u64;

            let is_last = bytes_read_total >= file_size;

            let checksum = xxhash64(&buffer);

            chunks.push(FileChunk {
                file_index,
                chunk_index,
                data: buffer,
                checksum,
                is_last,
            });

            chunk_index += 1;
        }

        Ok(chunks)
    }
}

/// Sanitize a path to prevent directory traversal attacks.
///
/// # Arguments
///
/// * `base` - Base directory
/// * `relative` - Relative path from base
///
/// # Returns
///
/// The sanitized absolute path, or None if the path is invalid.
#[must_use]
pub fn sanitize_path(base: &Path, relative: &Path) -> Option<PathBuf> {
    for component in relative.components() {
        if component == std::path::Component::ParentDir {
            return None;
        }
    }

    let full_path = base.join(relative);

    if full_path.starts_with(base) {
        Some(full_path)
    } else {
        None
    }
}

/// Format a file size for display.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Writer for receiving file chunks and assembling them.
#[derive(Debug)]
pub struct FileWriter {
    /// Output file path
    pub output_path: PathBuf,
    /// Expected total file size
    pub expected_size: u64,
    /// File handle
    file: Option<tokio::fs::File>,
    /// Bytes written so far
    pub bytes_written: u64,
    /// SHA-256 hasher for final verification
    sha256_hasher: sha2::Sha256,
}

impl FileWriter {
    /// Create a new file writer.
    ///
    /// # Arguments
    ///
    /// * `output_path` - Path to write the file to
    /// * `expected_size` - Expected total file size
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub async fn new(output_path: PathBuf, expected_size: u64) -> Result<Self> {
        use tokio::fs::File;

        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let file = File::create(&output_path).await?;

        Ok(Self {
            output_path,
            expected_size,
            file: Some(file),
            bytes_written: 0,
            sha256_hasher: sha2::Sha256::new(),
        })
    }

    /// Create a new file writer for resuming a transfer.
    ///
    /// Opens an existing file for appending/writing at arbitrary positions.
    /// The file will be pre-allocated to the expected size if it doesn't already exist
    /// or is smaller than expected.
    ///
    /// # Arguments
    ///
    /// * `output_path` - Path to write the file to
    /// * `expected_size` - Expected total file size
    /// * `resume_offset` - Number of bytes already written (for progress tracking)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub async fn new_resumable(
        output_path: PathBuf,
        expected_size: u64,
        resume_offset: u64,
    ) -> Result<Self> {
        use tokio::fs::OpenOptions;

        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Open file for read/write, create if doesn't exist
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&output_path)
            .await?;

        // Pre-allocate file to expected size if needed
        let metadata = file.metadata().await?;
        if metadata.len() < expected_size {
            file.set_len(expected_size).await?;
        }

        Ok(Self {
            output_path,
            expected_size,
            file: Some(file),
            bytes_written: resume_offset,
            // Note: For resumed transfers, we can't continue the hash from the middle,
            // so we'll compute the full file hash at finalization time
            sha256_hasher: sha2::Sha256::new(),
        })
    }

    /// Write a chunk to the file.
    ///
    /// Verifies the xxHash64 checksum before writing.
    ///
    /// # Errors
    ///
    /// Returns an error if checksum verification fails or write fails.
    pub async fn write_chunk(&mut self, chunk: &FileChunk) -> Result<()> {
        use sha2::Digest;
        use tokio::io::AsyncWriteExt;

        use crate::crypto::xxhash64;
        use crate::error::Error;

        let computed_checksum = xxhash64(&chunk.data);
        if computed_checksum != chunk.checksum {
            return Err(Error::ChecksumMismatch {
                file: self.output_path.display().to_string(),
                chunk: chunk.chunk_index,
            });
        }

        if let Some(ref mut file) = self.file {
            file.write_all(&chunk.data).await?;
        }

        self.sha256_hasher.update(&chunk.data);
        self.bytes_written += chunk.data.len() as u64;

        Ok(())
    }

    /// Write a chunk at a specific offset in the file.
    ///
    /// Used for resumable transfers where chunks may arrive out of order
    /// or need to be written at specific positions.
    ///
    /// # Arguments
    ///
    /// * `chunk` - The chunk data to write
    /// * `offset` - The byte offset in the file where the chunk should be written
    ///
    /// # Errors
    ///
    /// Returns an error if checksum verification fails, seek fails, or write fails.
    pub async fn write_chunk_at(&mut self, chunk: &FileChunk, offset: u64) -> Result<()> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        use crate::crypto::xxhash64;
        use crate::error::Error;

        let computed_checksum = xxhash64(&chunk.data);
        if computed_checksum != chunk.checksum {
            return Err(Error::ChecksumMismatch {
                file: self.output_path.display().to_string(),
                chunk: chunk.chunk_index,
            });
        }

        if let Some(ref mut file) = self.file {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            file.write_all(&chunk.data).await?;
        }

        // Note: For out-of-order writes, we don't update the hasher here
        // since SHA-256 requires sequential data. The hash will be computed
        // during finalization by reading the complete file.
        self.bytes_written += chunk.data.len() as u64;

        Ok(())
    }

    /// Finalize the file and return the SHA-256 hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be synced.
    pub async fn finalize(mut self) -> Result<[u8; 32]> {
        use sha2::Digest;
        use tokio::io::AsyncWriteExt;

        if let Some(ref mut file) = self.file {
            file.flush().await?;
            file.sync_all().await?;
        }
        self.file = None;

        Ok(self.sha256_hasher.finalize().into())
    }

    /// Finalize the file and compute SHA-256 hash from the complete file.
    ///
    /// This is used for resumed transfers where chunks may have been written
    /// out of order, making the incremental hash invalid.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or synced.
    pub async fn finalize_with_full_hash(mut self) -> Result<[u8; 32]> {
        use sha2::Digest;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        if let Some(ref mut file) = self.file {
            file.flush().await?;
            file.sync_all().await?;
        }
        self.file = None;

        // Read the complete file and compute hash
        let mut hasher = sha2::Sha256::new();
        let mut file = tokio::fs::File::open(&self.output_path).await?;
        let mut buffer = vec![0u8; 64 * 1024]; // 64KB buffer

        loop {
            let n = file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(hasher.finalize().into())
    }

    /// Get the current bytes written count.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Get the expected file size.
    #[must_use]
    pub const fn expected_size(&self) -> u64 {
        self.expected_size
    }

    /// Check if writing is complete.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.bytes_written >= self.expected_size
    }
}

use sha2::Digest;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_sanitize_path_valid() {
        let base = Path::new("/home/user/downloads");
        let relative = Path::new("file.txt");
        let result = sanitize_path(base, relative);
        assert_eq!(result, Some(PathBuf::from("/home/user/downloads/file.txt")));
    }

    #[test]
    fn test_sanitize_path_nested_valid() {
        let base = Path::new("/home/user/downloads");
        let relative = Path::new("subdir/file.txt");
        let result = sanitize_path(base, relative);
        assert_eq!(
            result,
            Some(PathBuf::from("/home/user/downloads/subdir/file.txt"))
        );
    }

    #[test]
    fn test_sanitize_path_traversal_attack() {
        let base = Path::new("/home/user/downloads");
        let relative = Path::new("../../../etc/passwd");
        let result = sanitize_path(base, relative);
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_chunk_empty_file() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("empty.txt");
        std::fs::write(&file_path, b"").expect("write empty file");

        let chunker = FileChunker::new(1024 * 1024);
        let chunks = chunker
            .read_chunks(&file_path, 0)
            .await
            .expect("read chunks");

        assert!(chunks.is_empty(), "Empty file should produce zero chunks");
    }

    #[tokio::test]
    async fn test_file_writer_empty_file() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let output_path = temp_dir.path().join("empty_output.txt");

        // Create writer for empty file
        let writer = FileWriter::new(output_path.clone(), 0)
            .await
            .expect("create writer");

        // Finalize without writing any chunks
        let sha256 = writer.finalize().await.expect("finalize");

        // Verify file exists and is empty
        assert!(output_path.exists(), "Empty file should be created");
        let content = std::fs::read(&output_path).expect("read file");
        assert!(content.is_empty(), "File should be empty");

        // SHA-256 of empty data
        let expected_sha256 = crate::crypto::sha256(b"");
        assert_eq!(sha256, expected_sha256, "SHA-256 of empty file should match");
    }

    #[tokio::test]
    async fn test_chunk_small_file() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("small.txt");
        let content = b"Hello, LocalDrop!";
        std::fs::write(&file_path, content).expect("write file");

        let chunker = FileChunker::new(1024 * 1024);
        let chunks = chunker
            .read_chunks(&file_path, 0)
            .await
            .expect("read chunks");

        assert_eq!(chunks.len(), 1, "Small file should produce one chunk");

        let chunk = &chunks[0];
        assert_eq!(chunk.file_index, 0);
        assert_eq!(chunk.chunk_index, 0);
        assert_eq!(chunk.data, content);
        assert!(chunk.is_last, "Single chunk should be marked as last");

        let expected_checksum = crate::crypto::xxhash64(content);
        assert_eq!(chunk.checksum, expected_checksum);
    }

    #[tokio::test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    async fn test_chunk_large_file() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let file_path = temp_dir.path().join("large.bin");

        let chunk_size = 1024;
        let content: Vec<u8> = (0..2560).map(|i| (i % 256) as u8).collect();
        std::fs::write(&file_path, &content).expect("write file");

        let chunker = FileChunker::new(chunk_size);
        let chunks = chunker
            .read_chunks(&file_path, 0)
            .await
            .expect("read chunks");

        assert_eq!(
            chunks.len(),
            3,
            "2.5KB file with 1KB chunks should produce 3 chunks"
        );

        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].data.len(), 1024);
        assert!(!chunks[0].is_last);

        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[1].data.len(), 1024);
        assert!(!chunks[1].is_last);

        assert_eq!(chunks[2].chunk_index, 2);
        assert_eq!(chunks[2].data.len(), 512);
        assert!(chunks[2].is_last);

        for chunk in &chunks {
            let expected_checksum = crate::crypto::xxhash64(&chunk.data);
            assert_eq!(chunk.checksum, expected_checksum);
        }

        let reassembled: Vec<u8> = chunks.iter().flat_map(|c| c.data.iter().copied()).collect();
        assert_eq!(reassembled, content);
    }

    #[tokio::test]
    async fn test_file_writer_basic() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let output_path = temp_dir.path().join("output.txt");
        let content = b"Test content for writer";

        let chunk = FileChunk {
            file_index: 0,
            chunk_index: 0,
            data: content.to_vec(),
            checksum: crate::crypto::xxhash64(content),
            is_last: true,
        };

        let mut writer = FileWriter::new(output_path.clone(), content.len() as u64)
            .await
            .expect("create writer");
        writer.write_chunk(&chunk).await.expect("write chunk");
        let sha256 = writer.finalize().await.expect("finalize");

        let written_content = std::fs::read(&output_path).expect("read file");
        assert_eq!(written_content, content);

        let expected_sha256 = crate::crypto::sha256(content);
        assert_eq!(sha256, expected_sha256);
    }

    #[tokio::test]
    async fn test_file_writer_checksum_mismatch() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let output_path = temp_dir.path().join("output.txt");

        let chunk = FileChunk {
            file_index: 0,
            chunk_index: 0,
            data: b"Test content".to_vec(),
            checksum: 12345,
            is_last: true,
        };

        let mut writer = FileWriter::new(output_path, 12)
            .await
            .expect("create writer");

        let result = writer.write_chunk(&chunk).await;
        assert!(result.is_err(), "Should fail on checksum mismatch");
    }

    #[tokio::test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    async fn test_roundtrip_chunking() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let original_path = temp_dir.path().join("original.bin");
        let output_path = temp_dir.path().join("copy.bin");

        let content: Vec<u8> = (0..5632).map(|i| (i % 256) as u8).collect();
        std::fs::write(&original_path, &content).expect("write original");

        let chunk_size = 1024;
        let chunker = FileChunker::new(chunk_size);
        let chunks = chunker
            .read_chunks(&original_path, 0)
            .await
            .expect("read chunks");

        let mut writer = FileWriter::new(output_path.clone(), content.len() as u64)
            .await
            .expect("create writer");

        for chunk in &chunks {
            writer.write_chunk(chunk).await.expect("write chunk");
        }
        let sha256 = writer.finalize().await.expect("finalize");

        let output_content = std::fs::read(&output_path).expect("read output");
        assert_eq!(output_content, content, "Roundtrip should preserve content");

        let expected_sha256 = crate::crypto::sha256(&content);
        assert_eq!(sha256, expected_sha256, "SHA-256 should match");
    }
}
