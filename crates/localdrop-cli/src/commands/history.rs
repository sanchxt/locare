//! History command implementation.

use anyhow::{Context, Result};

use localdrop_core::file::format_size;
use localdrop_core::history::{HistoryStore, TransferState};

use super::HistoryArgs;

/// Run the history command.
pub async fn run(args: HistoryArgs) -> Result<()> {
    let global_config = super::load_config();

    let mut store = HistoryStore::load().context("Failed to load transfer history")?;

    if args.clear {
        store.clear().context("Failed to clear history")?;
        println!();
        println!("  History cleared.");
        println!();
        return Ok(());
    }

    if args.json {
        return output_json(&store, args.details);
    }

    if let Some(index) = args.details {
        show_details(&store, index);
        return Ok(());
    }

    show_list(&store, global_config.history.max_entries);
    Ok(())
}

/// Output history as JSON.
fn output_json(store: &HistoryStore, details: Option<usize>) -> Result<()> {
    if let Some(index) = details {
        if let Some(entry) = store.get(index) {
            let output = serde_json::json!({
                "transfer": {
                    "id": entry.id.to_string(),
                    "timestamp": entry.timestamp,
                    "date": entry.formatted_timestamp(),
                    "direction": format!("{}", entry.direction),
                    "device_name": entry.device_name,
                    "device_id": entry.device_id.map(|id| id.to_string()),
                    "share_code": entry.share_code,
                    "files": entry.files.iter().map(|f| serde_json::json!({
                        "name": f.name,
                        "size": f.size,
                        "size_formatted": format_size(f.size),
                        "success": f.success,
                    })).collect::<Vec<_>>(),
                    "total_bytes": entry.total_bytes,
                    "total_bytes_formatted": format_size(entry.total_bytes),
                    "bytes_transferred": entry.bytes_transferred,
                    "state": format!("{}", entry.state),
                    "duration_secs": entry.duration_secs,
                    "speed_bps": entry.speed_bps,
                    "output_dir": entry.output_dir.as_ref().map(|p| p.display().to_string()),
                    "error_message": entry.error_message,
                }
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            let output = serde_json::json!({
                "error": format!("No transfer at index {}", index),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    } else {
        let output = serde_json::json!({
            "transfers": store.list(None).iter().enumerate().map(|(i, entry)| serde_json::json!({
                "index": i,
                "id": entry.id.to_string(),
                "timestamp": entry.timestamp,
                "date": entry.formatted_timestamp(),
                "direction": format!("{}", entry.direction),
                "device_name": entry.device_name,
                "share_code": entry.share_code,
                "file_count": entry.files.len(),
                "total_bytes": entry.total_bytes,
                "total_bytes_formatted": format_size(entry.total_bytes),
                "state": format!("{}", entry.state),
            })).collect::<Vec<_>>(),
            "total_count": store.len(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

/// Show detailed info for a single transfer.
fn show_details(store: &HistoryStore, index: usize) {
    println!();

    let Some(entry) = store.get(index) else {
        println!("  No transfer at index {}.", index);
        println!();
        return;
    };

    println!("Transfer #{}", index);
    println!("{}", "─".repeat(50));

    println!("  Date:        {}", entry.formatted_timestamp());
    println!("  Direction:   {}", entry.direction);
    println!("  Device:      {}", entry.device_name);
    if let Some(device_id) = &entry.device_id {
        println!("  Device ID:   {}", device_id);
    }
    println!("  Share Code:  {}", entry.share_code);
    println!("  Status:      {}", format_state(entry.state));

    println!();
    println!("  Files ({}):", entry.files.len());
    for file in &entry.files {
        let status = if file.success { "✓" } else { "✗" };
        println!("    {} {} ({})", status, file.name, format_size(file.size));
    }

    println!();
    println!("  Statistics:");
    println!("    Total Size:     {}", format_size(entry.total_bytes));
    println!(
        "    Transferred:    {}",
        format_size(entry.bytes_transferred)
    );
    if entry.duration_secs > 0 {
        println!("    Duration:       {}s", entry.duration_secs);
    }
    if let Some(speed) = entry.speed_bps {
        println!("    Speed:          {}/s", format_size(speed));
    }

    if let Some(output_dir) = &entry.output_dir {
        println!();
        println!("  Output:          {}", output_dir.display());
    }

    if let Some(error) = &entry.error_message {
        println!();
        println!("  Error:           {}", error);
    }

    println!("{}", "─".repeat(50));
    println!();
}

/// Show the history list.
fn show_list(store: &HistoryStore, max_entries: usize) {
    println!();
    println!("Recent Transfers:");
    println!("{}", "─".repeat(72));
    println!(
        "  {:3}  {:14}  {:10}  {:16}  {:6}  {:10}  {:8}",
        "#", "Date", "Direction", "Device", "Files", "Size", "Status"
    );
    println!("{}", "─".repeat(72));

    if store.is_empty() {
        println!("  (no transfer history)");
    } else {
        for (index, entry) in store.list(Some(max_entries)).iter().enumerate() {
            println!(
                "  {:3}  {:14}  {:10}  {:16}  {:6}  {:10}  {:8}",
                index,
                entry.formatted_timestamp(),
                format!("{}", entry.direction),
                truncate_string(&entry.device_name, 16),
                entry.files.len(),
                format_size(entry.total_bytes),
                format_state(entry.state),
            );
        }

        if store.len() > max_entries {
            println!();
            println!("  (showing {} of {} transfers)", max_entries, store.len());
        }
    }

    println!("{}", "─".repeat(72));

    if !store.is_empty() {
        println!();
        println!("  Use --details <#> to see transfer details.");
        println!("  Use --clear to remove all history.");
    }

    println!();
}

/// Format transfer state with color hints.
const fn format_state(state: TransferState) -> &'static str {
    match state {
        TransferState::Completed => "Done",
        TransferState::Failed => "Failed",
        TransferState::Cancelled => "Cancel",
    }
}

/// Truncate a string to fit within a maximum width.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len - 1).collect();
        format!("{}…", truncated)
    }
}
