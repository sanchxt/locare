//! Cross-platform clipboard access.
//!
//! This module provides a platform-agnostic interface for reading and writing
//! clipboard content using the `arboard` crate.

use std::time::Duration;

use arboard::Clipboard;
use image::ImageEncoder;

use crate::error::{Error, Result};
use crate::protocol::ClipboardContentType;

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

    /// Write content to clipboard and wait for a clipboard manager to claim it.
    ///
    /// On Linux (especially Wayland), clipboard content is "hosted" by the application.
    /// When the `Clipboard` object is dropped, the content becomes inaccessible unless
    /// another application (like a clipboard manager) has claimed it. This method
    /// waits until the content is claimed or the timeout expires.
    ///
    /// On non-Linux platforms, this behaves the same as `write()`.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard access fails or timeout expires without a claim.
    fn write_and_wait(&mut self, content: &ClipboardContent, timeout: Duration) -> Result<()>;

    /// Get hash of current content (for change detection).
    ///
    /// Returns 0 if clipboard is empty or unreadable.
    fn content_hash(&mut self) -> u64;

    /// Read clipboard content with an expected type hint.
    ///
    /// When expecting an image, tries to read image first before text.
    /// This helps with verification after setting image content, since
    /// trying text first might return stale text data.
    ///
    /// # Arguments
    ///
    /// * `expected` - Optional hint about expected content type
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard access fails.
    fn read_expected(
        &mut self,
        expected: Option<ClipboardContentType>,
    ) -> Result<Option<ClipboardContent>>;
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
    #[cfg(target_os = "linux")]
    #[allow(clippy::cognitive_complexity)]
    fn write_and_wait(&mut self, content: &ClipboardContent, timeout: Duration) -> Result<()> {
        use super::linux_holder::{hold_image_in_background, DisplayServer};
        use arboard::SetExtLinux;
        use std::time::Instant;

        let deadline = Instant::now() + timeout;

        match content {
            ClipboardContent::Text(text) => {
                self.clipboard
                    .set()
                    .wait_until(deadline)
                    .text(text.clone())
                    .map_err(|e| Error::ClipboardError(format!("failed to set text: {e}")))?;
                tracing::debug!("Clipboard: text written and claimed by clipboard manager");
            }
            ClipboardContent::Image {
                data,
                width,
                height,
            } => {
                use super::linux_holder::DEFAULT_HOLDER_TIMEOUT;

                let display_server = DisplayServer::detect();
                tracing::debug!(
                    "Clipboard: writing image {}x{} on {:?}",
                    width,
                    height,
                    display_server
                );

                hold_image_in_background(data, *width, *height, DEFAULT_HOLDER_TIMEOUT)?;

                tracing::debug!("Clipboard: image written via background holder");
            }
        }

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn write_and_wait(&mut self, content: &ClipboardContent, _timeout: Duration) -> Result<()> {
        self.write(content)
    }

    #[allow(clippy::cognitive_complexity)]
    fn read(&mut self) -> Result<Option<ClipboardContent>> {
        match self.clipboard.get_text() {
            Ok(text) if !text.is_empty() => {
                tracing::trace!("Clipboard: read {} bytes of text", text.len());
                return Ok(Some(ClipboardContent::Text(text)));
            }
            Ok(_) => {
                tracing::trace!("Clipboard: text is empty, trying image");
            }
            Err(e) => {
                tracing::debug!("Clipboard: failed to read text: {}", e);
            }
        }

        match self.clipboard.get_image() {
            Ok(image) => {
                let width = u32::try_from(image.width)
                    .map_err(|_| Error::ClipboardError("image width too large".to_string()))?;
                let height = u32::try_from(image.height)
                    .map_err(|_| Error::ClipboardError("image height too large".to_string()))?;

                let rgba_data = image.bytes.into_owned();

                let mut png_data = Vec::new();
                let encoder = image::codecs::png::PngEncoder::new_with_quality(
                    &mut png_data,
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::Adaptive,
                );

                encoder
                    .write_image(&rgba_data, width, height, image::ExtendedColorType::Rgba8)
                    .map_err(|e| Error::ClipboardError(format!("failed to encode PNG: {e}")))?;

                tracing::trace!("Clipboard: read image {}x{}", width, height);
                return Ok(Some(ClipboardContent::Image {
                    data: png_data,
                    width,
                    height,
                }));
            }
            Err(e) => {
                tracing::debug!("Clipboard: failed to read image: {}", e);
            }
        }

        tracing::trace!("Clipboard: no text or image content found");
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
                let img = image::load_from_memory_with_format(data, image::ImageFormat::Png)
                    .map_err(|e| Error::ClipboardError(format!("failed to decode PNG: {e}")))?;

                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();

                let image_data = arboard::ImageData {
                    width: w as usize,
                    height: h as usize,
                    bytes: std::borrow::Cow::Owned(rgba.into_raw()),
                };

                self.clipboard
                    .set_image(image_data)
                    .map_err(|e| Error::ClipboardError(format!("failed to set image: {e}")))?;

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

    fn read_expected(
        &mut self,
        expected: Option<ClipboardContentType>,
    ) -> Result<Option<ClipboardContent>> {
        match expected {
            Some(ClipboardContentType::ImagePng) => {
                if let Some(content) = self.try_read_image()? {
                    return Ok(Some(content));
                }
                Ok(self.try_read_text())
            }
            Some(ClipboardContentType::PlainText) | None => self.read(),
        }
    }
}

impl NativeClipboard {
    /// Try to read text from clipboard.
    fn try_read_text(&mut self) -> Option<ClipboardContent> {
        match self.clipboard.get_text() {
            Ok(text) if !text.is_empty() => {
                tracing::trace!("Clipboard: read {} bytes of text", text.len());
                Some(ClipboardContent::Text(text))
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("Clipboard: failed to read text: {e}");
                None
            }
        }
    }

    /// Try to read image from clipboard.
    fn try_read_image(&mut self) -> Result<Option<ClipboardContent>> {
        match self.clipboard.get_image() {
            Ok(image) => {
                let width = u32::try_from(image.width)
                    .map_err(|_| Error::ClipboardError("image width too large".to_string()))?;
                let height = u32::try_from(image.height)
                    .map_err(|_| Error::ClipboardError("image height too large".to_string()))?;

                let rgba_data = image.bytes.into_owned();

                let mut png_data = Vec::new();
                let encoder = image::codecs::png::PngEncoder::new_with_quality(
                    &mut png_data,
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::Adaptive,
                );

                encoder
                    .write_image(&rgba_data, width, height, image::ExtendedColorType::Rgba8)
                    .map_err(|e| Error::ClipboardError(format!("failed to encode PNG: {e}")))?;

                tracing::trace!("Clipboard: read image {}x{}", width, height);
                Ok(Some(ClipboardContent::Image {
                    data: png_data,
                    width,
                    height,
                }))
            }
            Err(e) => {
                tracing::debug!("Clipboard: failed to read image: {}", e);
                Ok(None)
            }
        }
    }

    /// Verify clipboard is accessible (for early failure detection).
    ///
    /// This attempts to read from the clipboard to verify access is working.
    /// Use this at startup to detect platform-specific clipboard access issues early.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard cannot be accessed.
    pub fn verify_access(&mut self) -> Result<()> {
        match self.clipboard.get_text() {
            Ok(_) => {
                tracing::trace!("Clipboard: access verified (text)");
                Ok(())
            }
            Err(text_err) => match self.clipboard.get_image() {
                Ok(_) => {
                    tracing::trace!("Clipboard: access verified (image)");
                    Ok(())
                }
                Err(image_err) => {
                    let msg = format!(
                        "Cannot access clipboard (text: {text_err}, image: {image_err}). \
                             Check display server connection."
                    );
                    tracing::warn!("Clipboard: {msg}");
                    Err(Error::ClipboardError(msg))
                }
            },
        }
    }
}

/// Diagnose clipboard accessibility and return diagnostic info.
///
/// This function checks the environment and clipboard access to help debug issues.
/// Returns a human-readable string describing the clipboard status.
#[must_use]
pub fn diagnose_clipboard() -> String {
    let mut info = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Ok(display) = std::env::var("WAYLAND_DISPLAY") {
            info.push(format!(
                "Wayland session detected (WAYLAND_DISPLAY={display})"
            ));
        }
        if let Ok(display) = std::env::var("DISPLAY") {
            info.push(format!("X11 display available (DISPLAY={display})"));
        }
        if std::env::var("WAYLAND_DISPLAY").is_err() && std::env::var("DISPLAY").is_err() {
            info.push(
                "WARNING: No display server detected (DISPLAY and WAYLAND_DISPLAY not set)"
                    .to_string(),
            );
        }
        if let Ok(session_type) = std::env::var("XDG_SESSION_TYPE") {
            info.push(format!("Session type: {session_type}"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        info.push("macOS clipboard (NSPasteboard)".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        info.push("Windows clipboard (Win32 API)".to_string());
    }

    match Clipboard::new() {
        Ok(mut cb) => {
            info.push("Clipboard initialized successfully".to_string());

            match cb.get_text() {
                Ok(text) => {
                    if text.is_empty() {
                        info.push("Text clipboard: accessible (empty)".to_string());
                    } else {
                        info.push(format!("Text clipboard: accessible ({} bytes)", text.len()));
                    }
                }
                Err(e) => {
                    info.push(format!("Text clipboard error: {e}"));
                }
            }

            match cb.get_image() {
                Ok(img) => {
                    info.push(format!(
                        "Image clipboard: accessible ({}x{})",
                        img.width, img.height
                    ));
                }
                Err(e) => {
                    info.push(format!("Image clipboard: {e}"));
                }
            }
        }
        Err(e) => {
            info.push(format!("ERROR: Cannot initialize clipboard: {e}"));
        }
    }

    info.join("\n")
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
    use std::sync::Mutex;

    // Mutex to serialize clipboard tests since they share the system clipboard.
    static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

    // Clipboard tests are skipped on Windows (heap corruption) and macOS (SIGSEGV)
    // in CI due to issues when accessing clipboard in headless environments.
    // See: https://github.com/1Password/arboard/issues/30

    #[test]
    #[cfg_attr(any(windows, target_os = "macos"), ignore)]
    fn test_create_clipboard() {
        let _lock = CLIPBOARD_LOCK.lock().unwrap();
        let result = create_clipboard();
        if result.is_err() {
            eprintln!("Skipping clipboard test (no display available)");
            return;
        }
        assert!(result.is_ok());
    }

    #[test]
    #[cfg_attr(any(windows, target_os = "macos"), ignore)]
    fn test_clipboard_text_roundtrip() {
        let _lock = CLIPBOARD_LOCK.lock().unwrap();
        let clipboard = create_clipboard();
        if clipboard.is_err() {
            eprintln!("Skipping clipboard test (no display available)");
            return;
        }
        let mut clipboard = clipboard.unwrap();

        let test_content = ClipboardContent::Text("LocalDrop test content".to_string());

        let write_result = clipboard.write(&test_content);
        if write_result.is_err() {
            eprintln!("Skipping clipboard test (write failed)");
            return;
        }

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
    #[cfg_attr(any(windows, target_os = "macos"), ignore)]
    fn test_content_hash_consistency() {
        let _lock = CLIPBOARD_LOCK.lock().unwrap();
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

        assert_eq!(hash1, hash2);
    }
}
