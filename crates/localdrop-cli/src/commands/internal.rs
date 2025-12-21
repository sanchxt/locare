//! Internal commands for clipboard holder subprocess.
//!
//! These commands are not user-facing. They are invoked by the main process
//! to spawn a subprocess that holds clipboard content on Linux (Wayland/X11).

use std::io::Read;
use std::time::Duration;

use anyhow::{bail, Result};
use arboard::Clipboard;

/// Run the internal clipboard hold command.
///
/// This is called by a spawned subprocess to hold clipboard content.
/// It reads binary data from stdin, sets the clipboard, and holds until timeout.
///
/// # Arguments
///
/// * `content_type` - "image" or "text"
/// * `timeout_secs` - How long to hold the clipboard before exiting
///
/// # Errors
///
/// Returns an error if clipboard cannot be set.
pub fn run_clipboard_hold(content_type: &str, timeout_secs: u64) -> Result<()> {
    // Read all data from stdin
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;

    if data.is_empty() {
        bail!("No data received on stdin");
    }

    eprintln!(
        "Clipboard holder: received {} bytes of {} data",
        data.len(),
        content_type
    );

    // Create a fresh clipboard instance
    let mut clipboard = Clipboard::new().map_err(|e| {
        anyhow::anyhow!("Failed to access clipboard in holder process: {}", e)
    })?;

    match content_type {
        "image" => {
            // Decode PNG to RGBA
            let img = image::load_from_memory(&data)
                .map_err(|e| anyhow::anyhow!("Failed to decode image: {}", e))?;

            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();

            eprintln!(
                "Clipboard holder: setting image {}x{} to clipboard",
                width, height
            );

            let image_data = arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: std::borrow::Cow::Owned(rgba.into_raw()),
            };

            clipboard.set_image(image_data).map_err(|e| {
                anyhow::anyhow!("Failed to set image in holder: {}", e)
            })?;

            eprintln!("Clipboard holder: image set successfully");
        }
        "text" => {
            let text = String::from_utf8(data)
                .map_err(|e| anyhow::anyhow!("Invalid UTF-8 text: {}", e))?;

            eprintln!(
                "Clipboard holder: setting {} bytes of text to clipboard",
                text.len()
            );

            clipboard.set_text(text).map_err(|e| {
                anyhow::anyhow!("Failed to set text in holder: {}", e)
            })?;

            eprintln!("Clipboard holder: text set successfully");
        }
        other => {
            bail!("Unknown content type: {}", other);
        }
    }

    // Hold the clipboard for the specified duration
    // On Wayland, the clipboard content is only available while this process runs.
    // A clipboard manager will eventually claim the content, allowing us to exit.
    // If no manager claims it, we wait until the timeout.
    eprintln!(
        "Clipboard holder: holding clipboard for up to {} seconds",
        timeout_secs
    );

    std::thread::sleep(Duration::from_secs(timeout_secs));

    eprintln!("Clipboard holder: timeout reached, exiting");
    Ok(())
}
