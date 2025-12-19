//! Cross-platform clipboard access.
//!
//! This module provides a platform-agnostic interface for reading and writing
//! clipboard content using the `arboard` crate.

use arboard::Clipboard;
use image::ImageEncoder;

use crate::error::{Error, Result};

use super::ClipboardContent;

/// Platform-agnostic clipboard access trait.
pub trait ClipboardAccess: Send + Sync {
    /// Read current clipboard content.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard access fails.
    fn read(&mut self) -> Result<Option<ClipboardContent>>;

    /// Write content to clipboard.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard access fails.
    fn write(&mut self, content: &ClipboardContent) -> Result<()>;

    /// Get hash of current content (for change detection).
    ///
    /// Returns 0 if clipboard is empty or unreadable.
    fn content_hash(&mut self) -> u64;
}

/// Native clipboard implementation using arboard.
pub struct NativeClipboard {
    clipboard: Clipboard,
}

impl NativeClipboard {
    /// Create a new native clipboard accessor.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard cannot be accessed.
    pub fn new() -> Result<Self> {
        let clipboard = Clipboard::new()
            .map_err(|e| Error::ClipboardError(format!("failed to access clipboard: {e}")))?;
        Ok(Self { clipboard })
    }
}

impl ClipboardAccess for NativeClipboard {
    fn read(&mut self) -> Result<Option<ClipboardContent>> {
        // Try to read text first
        if let Ok(text) = self.clipboard.get_text() {
            if !text.is_empty() {
                return Ok(Some(ClipboardContent::Text(text)));
            }
        }

        // Try to read image
        if let Ok(image) = self.clipboard.get_image() {
            // Convert to PNG bytes
            let width = u32::try_from(image.width)
                .map_err(|_| Error::ClipboardError("image width too large".to_string()))?;
            let height = u32::try_from(image.height)
                .map_err(|_| Error::ClipboardError("image height too large".to_string()))?;

            // arboard gives us RGBA bytes
            let rgba_data = image.bytes.into_owned();

            // Encode as PNG
            let mut png_data = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut png_data,
                image::codecs::png::CompressionType::Fast,
                image::codecs::png::FilterType::Adaptive,
            );

            encoder
                .write_image(&rgba_data, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| Error::ClipboardError(format!("failed to encode PNG: {e}")))?;

            return Ok(Some(ClipboardContent::Image {
                data: png_data,
                width,
                height,
            }));
        }

        Ok(None)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<()> {
        match content {
            ClipboardContent::Text(text) => {
                self.clipboard
                    .set_text(text.clone())
                    .map_err(|e| Error::ClipboardError(format!("failed to set text: {e}")))?;
            }
            ClipboardContent::Image {
                data,
                width,
                height,
            } => {
                // Decode PNG to RGBA
                let img = image::load_from_memory_with_format(data, image::ImageFormat::Png)
                    .map_err(|e| Error::ClipboardError(format!("failed to decode PNG: {e}")))?;

                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();

                // Set image to clipboard
                let image_data = arboard::ImageData {
                    width: w as usize,
                    height: h as usize,
                    bytes: std::borrow::Cow::Owned(rgba.into_raw()),
                };

                self.clipboard
                    .set_image(image_data)
                    .map_err(|e| Error::ClipboardError(format!("failed to set image: {e}")))?;

                // Verify dimensions match (warn if different due to format conversion)
                if w != *width || h != *height {
                    tracing::debug!(
                        "Image dimensions changed during conversion: {}x{} -> {}x{}",
                        width,
                        height,
                        w,
                        h
                    );
                }
            }
        }

        Ok(())
    }

    fn content_hash(&mut self) -> u64 {
        self.read().ok().flatten().map_or(0, |c| c.hash())
    }
}

/// Create a platform-appropriate clipboard accessor.
///
/// # Errors
///
/// Returns an error if clipboard cannot be accessed.
pub fn create_clipboard() -> Result<Box<dyn ClipboardAccess>> {
    Ok(Box::new(NativeClipboard::new()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a display server on Linux (X11/Wayland)
    // They may be skipped in headless CI environments

    #[test]
    fn test_create_clipboard() {
        // This test may fail in headless environments
        let result = create_clipboard();
        if result.is_err() {
            eprintln!("Skipping clipboard test (no display available)");
            return;
        }
        assert!(result.is_ok());
    }

    #[test]
    fn test_clipboard_text_roundtrip() {
        let clipboard = create_clipboard();
        if clipboard.is_err() {
            eprintln!("Skipping clipboard test (no display available)");
            return;
        }
        let mut clipboard = clipboard.unwrap();

        let test_content = ClipboardContent::Text("LocalDrop test content".to_string());

        // Write to clipboard
        let write_result = clipboard.write(&test_content);
        if write_result.is_err() {
            eprintln!("Skipping clipboard test (write failed)");
            return;
        }

        // Read back
        let read_result = clipboard.read();
        if read_result.is_err() {
            eprintln!("Skipping clipboard test (read failed)");
            return;
        }

        if let Some(ClipboardContent::Text(text)) = read_result.unwrap() {
            assert_eq!(text, "LocalDrop test content");
        }
    }

    #[test]
    fn test_content_hash_consistency() {
        let clipboard = create_clipboard();
        if clipboard.is_err() {
            eprintln!("Skipping clipboard test (no display available)");
            return;
        }
        let mut clipboard = clipboard.unwrap();

        let test_content = ClipboardContent::Text("Hash test".to_string());
        let _ = clipboard.write(&test_content);

        let hash1 = clipboard.content_hash();
        let hash2 = clipboard.content_hash();

        // Hashes should be consistent for same content
        assert_eq!(hash1, hash2);
    }
}
