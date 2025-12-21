//! Linux-specific clipboard holder for persistent image clipboard.
//!
//! On Wayland (and to some extent X11), clipboard content is "owned" by the
//! application that sets it. When that application exits, the clipboard content
//! becomes unavailable unless a clipboard manager has claimed it.
//!
//! This module spawns a separate process that holds the clipboard content until
//! it's pasted or a timeout expires. Unlike fork(), this approach:
//! - Maintains Wayland session connection (no setsid())
//! - Creates a clean process without async runtime corruption
//! - Provides visible error logging

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};

/// Default timeout for the clipboard holder process (5 minutes).
pub const DEFAULT_HOLDER_TIMEOUT: Duration = Duration::from_secs(300);

/// Hold image content in a background process for clipboard persistence.
///
/// This function spawns a detached child process that:
/// 1. Creates a new clipboard instance
/// 2. Sets the image content
/// 3. Waits for the clipboard manager to claim it or timeout expires
/// 4. Exits cleanly
///
/// The parent process waits briefly for the child to initialize, then returns.
///
/// # Arguments
///
/// * `png_data` - PNG-encoded image data
/// * `width` - Image width in pixels
/// * `height` - Image height in pixels
/// * `timeout` - How long the holder process should wait before exiting
///
/// # Errors
///
/// Returns an error if:
/// - Cannot find the current executable
/// - Process spawn fails
/// - Writing data to child fails
pub fn hold_image_in_background(
    png_data: Vec<u8>,
    width: u32,
    height: u32,
    timeout: Duration,
) -> Result<()> {
    let display_server = DisplayServer::detect();
    tracing::info!(
        "Spawning clipboard holder for image {}x{} ({} bytes) on {:?}",
        width,
        height,
        png_data.len(),
        display_server
    );

    // Get the current executable path
    let exe = std::env::current_exe().map_err(|e| {
        Error::ClipboardError(format!("cannot find current executable for holder: {}", e))
    })?;

    tracing::debug!("Clipboard holder executable: {:?}", exe);

    // Spawn a new process with the internal command
    // Explicitly pass display environment variables to ensure clipboard access
    let mut cmd = Command::new(&exe);
    cmd.arg("internal-clipboard-hold")
        .arg("--content-type")
        .arg("image")
        .arg("--timeout")
        .arg(timeout.as_secs().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit()); // Keep stderr for debugging

    // Ensure Wayland/X11 display variables are passed to child
    if let Ok(val) = std::env::var("WAYLAND_DISPLAY") {
        cmd.env("WAYLAND_DISPLAY", val);
    }
    if let Ok(val) = std::env::var("DISPLAY") {
        cmd.env("DISPLAY", val);
    }
    if let Ok(val) = std::env::var("XDG_RUNTIME_DIR") {
        cmd.env("XDG_RUNTIME_DIR", val);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::ClipboardError(format!("failed to spawn clipboard holder: {}", e)))?;

    tracing::debug!(
        "Clipboard holder process spawned with PID {:?}",
        child.id()
    );

    // Write PNG data to child's stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&png_data).map_err(|e| {
            Error::ClipboardError(format!("failed to write image data to holder: {}", e))
        })?;
        // stdin is dropped here, signaling EOF to the child
    } else {
        return Err(Error::ClipboardError(
            "failed to get stdin pipe for clipboard holder".to_string(),
        ));
    }

    // Wait for child to initialize and set clipboard
    // This gives the holder process time to read stdin and set the clipboard
    std::thread::sleep(Duration::from_millis(500));

    tracing::debug!(
        "Clipboard holder process started for image {}x{}",
        width,
        height
    );

    // Child continues running in background - we don't wait for it
    // The child will exit after timeout or when clipboard is claimed
    Ok(())
}

/// Hold text content in a background process for clipboard persistence.
///
/// Similar to `hold_image_in_background` but for text content.
/// Generally not needed as text clipboard works more reliably, but
/// available for consistency.
///
/// # Arguments
///
/// * `text` - Text content to hold
/// * `timeout` - How long the holder process should wait before exiting
///
/// # Errors
///
/// Returns an error if process spawn fails or writing data fails.
pub fn hold_text_in_background(text: String, timeout: Duration) -> Result<()> {
    let display_server = DisplayServer::detect();
    tracing::info!(
        "Spawning clipboard holder for text ({} bytes) on {:?}",
        text.len(),
        display_server
    );

    let exe = std::env::current_exe().map_err(|e| {
        Error::ClipboardError(format!("cannot find current executable for holder: {}", e))
    })?;

    let mut cmd = Command::new(&exe);
    cmd.arg("internal-clipboard-hold")
        .arg("--content-type")
        .arg("text")
        .arg("--timeout")
        .arg(timeout.as_secs().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    // Ensure Wayland/X11 display variables are passed to child
    if let Ok(val) = std::env::var("WAYLAND_DISPLAY") {
        cmd.env("WAYLAND_DISPLAY", val);
    }
    if let Ok(val) = std::env::var("DISPLAY") {
        cmd.env("DISPLAY", val);
    }
    if let Ok(val) = std::env::var("XDG_RUNTIME_DIR") {
        cmd.env("XDG_RUNTIME_DIR", val);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::ClipboardError(format!("failed to spawn clipboard holder: {}", e)))?;

    tracing::debug!(
        "Clipboard holder process spawned with PID {:?}",
        child.id()
    );

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).map_err(|e| {
            Error::ClipboardError(format!("failed to write text data to holder: {}", e))
        })?;
    } else {
        return Err(Error::ClipboardError(
            "failed to get stdin pipe for clipboard holder".to_string(),
        ));
    }

    // Wait briefly for child to initialize
    std::thread::sleep(Duration::from_millis(300));

    tracing::debug!("Clipboard holder process started for text");
    Ok(())
}

/// Detect the current display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayServer {
    /// Wayland display server
    Wayland,
    /// X11 display server
    X11,
    /// Unknown or no display server
    Unknown,
}

impl DisplayServer {
    /// Detect the current display server from environment variables.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            DisplayServer::Wayland
        } else if std::env::var("DISPLAY").is_ok() {
            DisplayServer::X11
        } else {
            DisplayServer::Unknown
        }
    }

    /// Check if we're running on Wayland.
    #[must_use]
    pub fn is_wayland(&self) -> bool {
        matches!(self, DisplayServer::Wayland)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_server_detection() {
        // Just verify it doesn't panic
        let server = DisplayServer::detect();
        println!("Detected display server: {:?}", server);
    }

    #[test]
    fn test_default_timeout() {
        assert_eq!(DEFAULT_HOLDER_TIMEOUT, Duration::from_secs(300));
    }
}
