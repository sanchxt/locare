//! Scan command implementation.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

use localdrop_core::discovery::HybridListener;
use localdrop_core::file::format_size;

use super::receive;
use super::{ReceiveArgs, ScanArgs};
use crate::ui::parse_duration;

/// Run the scan command.
pub async fn run(args: ScanArgs) -> Result<()> {
    let global_config = super::load_config();

    let duration = parse_duration(&args.duration)
        .context("Invalid duration format. Use formats like '5s', '10s', '30s'")?;

    if !args.json {
        println!();
        println!("Scanning for active shares ({})...", args.duration);
        println!();
    }

    let listener = HybridListener::new(global_config.network.port)
        .await
        .context("Failed to create discovery listener")?;

    let shares = listener.scan(duration).await;

    if args.json {
        output_json_shares(&shares);
        if !args.interactive {
            return Ok(());
        }
    }

    if !args.json {
        display_shares(&shares);
    }

    if args.interactive {
        handle_interactive_mode(&shares, args.json).await?;
    }

    Ok(())
}

/// Output shares as JSON.
fn output_json_shares(shares: &[localdrop_core::discovery::DiscoveredShare]) {
    let output = serde_json::json!({
        "shares": shares.iter().map(|s| serde_json::json!({
            "code": s.packet.code,
            "device": s.packet.device_name,
            "device_id": s.packet.device_id.to_string(),
            "files": s.packet.file_count,
            "size": s.packet.total_size,
            "size_formatted": format_size(s.packet.total_size),
            "expires_at": s.packet.expires_at,
            "address": s.source.to_string(),
            "transfer_port": s.packet.transfer_port,
        })).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

/// Display shares as text table.
fn display_shares(shares: &[localdrop_core::discovery::DiscoveredShare]) {
    println!("Active Shares on Network:");
    println!("{}", "─".repeat(60));
    println!(
        "  {:6}  {:16}  {:6}  {:10}  {:7}",
        "Code", "Device", "Files", "Size", "Expires"
    );
    println!("{}", "─".repeat(60));

    if shares.is_empty() {
        println!("  (no active shares found)");
        println!("{}", "─".repeat(60));
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for share in shares {
        let expires_str = if share.packet.expires_at > now {
            let remaining = share.packet.expires_at - now;
            let mins = remaining / 60;
            let secs = remaining % 60;
            format!("{}:{:02}", mins, secs)
        } else if share.packet.expires_at == 0 {
            "N/A".to_string()
        } else {
            "expired".to_string()
        };

        println!(
            "  {:6}  {:16}  {:6}  {:10}  {:7}",
            share.packet.code,
            truncate_string(&share.packet.device_name, 16),
            share.packet.file_count,
            format_size(share.packet.total_size),
            expires_str
        );
    }

    println!("{}", "─".repeat(60));
}

/// Handle interactive mode for selecting and receiving a share.
async fn handle_interactive_mode(
    shares: &[localdrop_core::discovery::DiscoveredShare],
    json: bool,
) -> Result<()> {
    println!();
    println!("Enter a code to connect, or 'q' to quit:");
    print!("  > ");
    io::stdout().flush()?;

    let mut input = String::new();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    reader.read_line(&mut input).await?;
    let input = input.trim();

    if input.eq_ignore_ascii_case("q") || input.is_empty() {
        println!();
        println!("  Cancelled.");
        return Ok(());
    }

    let matching_share = shares
        .iter()
        .find(|s| s.packet.code.eq_ignore_ascii_case(input));

    if matching_share.is_none() {
        println!();
        println!(
            "  Code '{}' not in scan results, attempting to connect...",
            input
        );
    }

    let receive_args = ReceiveArgs {
        code: input.to_uppercase(),
        output: Some(PathBuf::from(".")),
        clipboard: false,
        quiet: false,
        verbose: false,
        json,
        batch: false,
    };

    println!();
    receive::run(receive_args).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_string() {
        assert_eq!(truncate_string("short", 10), "short");
        assert_eq!(truncate_string("exactly10!", 10), "exactly10!");
        assert_eq!(truncate_string("this is too long", 10), "this is t…");
    }
}
