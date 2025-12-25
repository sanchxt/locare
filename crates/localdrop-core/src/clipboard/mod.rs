//! Clipboard sharing functionality for LocalDrop.
//!
//! This module provides clipboard sharing capabilities including:
//!
//! - One-shot clipboard transfer (share/receive)
//! - Live bidirectional clipboard synchronization
//!
//! ## Usage
//!
//! ### One-shot transfer
//!
//! ```rust,ignore
//! // Sender
//! let session = ClipboardShareSession::new(config).await?;
//! println!("Share code: {}", session.code());
//! session.wait().await?;
//!
//! // Receiver
//! let mut session = ClipboardReceiveSession::connect("A7K9", config).await?;
//! session.accept_to_clipboard().await?;
//! ```
//!
//! ### Live sync
//!
//! ```rust,ignore
//! // Host
//! let (code, mut session) = ClipboardSyncSession::host(config).await?;
//! println!("Share code: {}", code);
//! session.run().await?;
//!
//! // Connector
//! let mut session = ClipboardSyncSession::connect("A7K9", config).await?;
//! session.run().await?;
//! ```

pub mod access;
#[cfg(target_os = "linux")]
pub mod linux_holder;
pub mod session;
pub mod watcher;

pub use access::{create_clipboard, diagnose_clipboard, ClipboardAccess, NativeClipboard};
#[cfg(target_os = "linux")]
pub use linux_holder::{hold_image_in_background, DisplayServer, DEFAULT_HOLDER_TIMEOUT};
pub use session::{
    ClipboardReceiveSession, ClipboardShareSession, ClipboardSyncSession, SyncEvent,
    SyncHostSession, SyncSessionRunner, SyncStats,
};
pub use watcher::{ClipboardChange, ClipboardWatcher};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::xxh64;

use crate::protocol::ClipboardContentType;

/// Clipboard content that can be transferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipboardContent {
    /// Plain text content
    Text(String),
    /// Image data (PNG format)
    Image {
        /// Raw PNG bytes
        data: Vec<u8>,
        /// Image width in pixels
        width: u32,
        /// Image height in pixels
        height: u32,
    },
}

impl ClipboardContent {
    /// Compute xxHash64 of content.
    #[must_use]
    pub fn hash(&self) -> u64 {
        match self {
            Self::Text(text) => xxh64(text.as_bytes(), 0),
            Self::Image { data, .. } => xxh64(data, 0),
        }
    }

    /// Get size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Self::Text(text) => text.len() as u64,
            Self::Image { data, .. } => data.len() as u64,
        }
    }

    /// Get content type.
    #[must_use]
    pub const fn content_type(&self) -> ClipboardContentType {
        match self {
            Self::Text(_) => ClipboardContentType::PlainText,
            Self::Image { .. } => ClipboardContentType::ImagePng,
        }
    }

    /// Serialize to bytes for transfer.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Text(text) => text.as_bytes().to_vec(),
            Self::Image { data, .. } => data.clone(),
        }
    }

    /// Deserialize from bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the content cannot be parsed.
    pub fn from_bytes(
        content_type: ClipboardContentType,
        data: &[u8],
        width: Option<u32>,
        height: Option<u32>,
    ) -> crate::Result<Self> {
        match content_type {
            ClipboardContentType::PlainText => {
                let text = String::from_utf8(data.to_vec())
                    .map_err(|e| crate::Error::ClipboardError(format!("invalid UTF-8: {e}")))?;
                Ok(Self::Text(text))
            }
            ClipboardContentType::ImagePng => Ok(Self::Image {
                data: data.to_vec(),
                width: width.unwrap_or(0),
                height: height.unwrap_or(0),
            }),
        }
    }

    /// Generate preview string (truncated text or image dimensions).
    #[must_use]
    pub fn preview(&self, max_len: usize) -> String {
        match self {
            Self::Text(text) => {
                if text.len() <= max_len {
                    format!("\"{}\"", text.replace('\n', "\\n"))
                } else {
                    format!(
                        "\"{}...\"",
                        text.chars()
                            .take(max_len)
                            .collect::<String>()
                            .replace('\n', "\\n")
                    )
                }
            }
            Self::Image { width, height, .. } => {
                format!("Image ({width}x{height})")
            }
        }
    }

    /// Format size for display.
    #[must_use]
    pub fn format_size(&self) -> String {
        crate::file::format_size(self.size())
    }
}

/// Metadata about clipboard content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardMetadata {
    /// Type of content
    pub content_type: ClipboardContentType,
    /// Size in bytes
    pub size: u64,
    /// xxHash64 checksum
    pub checksum: u64,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Source device name
    pub source_device: String,
    /// Image dimensions (for images only)
    pub width: Option<u32>,
    /// Image dimensions (for images only)
    pub height: Option<u32>,
}

impl ClipboardMetadata {
    /// Create metadata from clipboard content.
    #[must_use]
    pub fn from_content(content: &ClipboardContent, source_device: &str) -> Self {
        let (width, height) = match content {
            ClipboardContent::Image { width, height, .. } => (Some(*width), Some(*height)),
            ClipboardContent::Text(_) => (None, None),
        };

        Self {
            content_type: content.content_type(),
            size: content.size(),
            checksum: content.hash(),
            timestamp: chrono::Utc::now(),
            source_device: source_device.to_string(),
            width,
            height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_content_text_hash() {
        let content = ClipboardContent::Text("Hello, world!".to_string());
        let hash1 = content.hash();
        let hash2 = content.hash();
        assert_eq!(hash1, hash2);

        let content2 = ClipboardContent::Text("Different text".to_string());
        assert_ne!(content.hash(), content2.hash());
    }

    #[test]
    fn test_clipboard_content_size() {
        let content = ClipboardContent::Text("Hello".to_string());
        assert_eq!(content.size(), 5);

        let content = ClipboardContent::Image {
            data: vec![0u8; 100],
            width: 10,
            height: 10,
        };
        assert_eq!(content.size(), 100);
    }

    #[test]
    fn test_clipboard_content_type() {
        let content = ClipboardContent::Text("Hello".to_string());
        assert_eq!(content.content_type(), ClipboardContentType::PlainText);

        let content = ClipboardContent::Image {
            data: vec![],
            width: 0,
            height: 0,
        };
        assert_eq!(content.content_type(), ClipboardContentType::ImagePng);
    }

    #[test]
    fn test_clipboard_content_preview() {
        let content = ClipboardContent::Text("Hello, world!".to_string());
        assert_eq!(content.preview(100), "\"Hello, world!\"");
        assert_eq!(content.preview(5), "\"Hello...\"");

        let content = ClipboardContent::Image {
            data: vec![],
            width: 1920,
            height: 1080,
        };
        assert_eq!(content.preview(100), "Image (1920x1080)");
    }

    #[test]
    fn test_clipboard_content_serialization() {
        let content = ClipboardContent::Text("Hello, world!".to_string());
        let bytes = content.to_bytes();
        let restored =
            ClipboardContent::from_bytes(ClipboardContentType::PlainText, &bytes, None, None)
                .unwrap();

        if let ClipboardContent::Text(text) = restored {
            assert_eq!(text, "Hello, world!");
        } else {
            panic!("Expected Text variant");
        }
    }

    #[test]
    fn test_clipboard_metadata_from_content() {
        let content = ClipboardContent::Text("Test".to_string());
        let metadata = ClipboardMetadata::from_content(&content, "TestDevice");

        assert_eq!(metadata.content_type, ClipboardContentType::PlainText);
        assert_eq!(metadata.size, 4);
        assert_eq!(metadata.source_device, "TestDevice");
        assert!(metadata.width.is_none());
        assert!(metadata.height.is_none());
    }
}
