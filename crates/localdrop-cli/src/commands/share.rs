//! Share command implementation.

use std::io::{self, Write};
use std::time::Instant;

use anyhow::Result;
use tokio::sync::watch;

use localdrop_core::file::format_size;
use localdrop_core::transfer::{ShareSession, TransferConfig, TransferProgress, TransferState};

use super::ShareArgs;
use crate::ui::{format_remaining, parse_duration, CodeBox};

/// Run the share command.
pub async fn run(args: ShareArgs) -> Result<()> {
    let config = TransferConfig {
        compress: args.compress,
        ..Default::default()
    };

    let mut session = ShareSession::new(&args.paths, config).await?;

    if !args.quiet {
        println!();
        println!("LocalDrop v{}", localdrop_core::VERSION);
        println!("{}", "-".repeat(37));
        println!();
    }

    let files = session.files();
    let total_files = files.len();
    let total_size: u64 = files.iter().map(|f| f.size).sum();

    if !args.quiet {
        println!(
            "  Sharing {} items ({})",
            total_files,
            format_size(total_size)
        );
        println!();
        for file in files {
            println!("  {} {}", file_icon(file), file.file_name());
        }
        println!();
    }

    let code = session.code().to_string();

    if args.json {
        let output = serde_json::json!({
            "code": code,
            "files": files.iter().map(|f| serde_json::json!({
                "name": f.file_name(),
                "size": f.size,
                "path": f.relative_path.display().to_string(),
            })).collect::<Vec<_>>(),
            "total_size": total_size,
            "expire": args.expire,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if !args.quiet {
        CodeBox::new(&code).with_expire(&args.expire).display();
        println!();
    }

    let progress_rx = session.progress();
    let expire_duration = parse_duration(&args.expire);
    let start_time = Instant::now();

    let quiet = args.quiet;
    let json = args.json;
    let progress_handle = if !quiet && !json {
        Some(tokio::spawn(display_progress(
            progress_rx,
            expire_duration,
            start_time,
        )))
    } else {
        None
    };

    let result = session.wait().await;

    if let Some(handle) = progress_handle {
        let _ = handle.await;
    }

    match result {
        Ok(()) => {
            if !args.quiet {
                println!();
                println!("  Transfer complete!");
                println!();
            }
            if args.json {
                let output = serde_json::json!({
                    "status": "complete",
                    "code": code,
                    "total_transferred": total_size,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            Ok(())
        }
        Err(e) => {
            if !args.quiet {
                eprintln!();
                eprintln!("  Transfer failed: {}", e);
                eprintln!();
            }
            Err(e.into())
        }
    }
}

fn file_icon(file: &localdrop_core::file::FileMetadata) -> &'static str {
    if file.is_symlink {
        "->"
    } else if let Some(ref mime) = file.mime_type {
        if mime.starts_with("image/") {
            "[img]"
        } else if mime.starts_with("video/") {
            "[vid]"
        } else if mime.starts_with("audio/") {
            "[aud]"
        } else if mime.starts_with("text/") {
            "[txt]"
        } else {
            "[file]"
        }
    } else {
        "[file]"
    }
}

async fn display_progress(
    mut rx: watch::Receiver<TransferProgress>,
    expire_duration: Option<std::time::Duration>,
    start_time: Instant,
) {
    let mut last_state = TransferState::Preparing;
    let mut waiting_printed = false;

    loop {
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed()).await;

        let progress = rx.borrow().clone();

        if progress.state != last_state {
            if waiting_printed {
                println!();
                waiting_printed = false;
            }
            last_state = progress.state;

            match progress.state {
                TransferState::Connected => {
                    println!("  Receiver connected!");
                }
                TransferState::Transferring => {
                    println!("  Starting transfer...");
                }
                TransferState::Completed => {
                    break;
                }
                TransferState::Cancelled => {
                    println!("  Transfer cancelled.");
                    break;
                }
                TransferState::Failed => {
                    println!("  Transfer failed.");
                    break;
                }
                TransferState::Preparing | TransferState::Waiting => {}
            }
        }

        if progress.state == TransferState::Waiting {
            if let Some(expire) = expire_duration {
                let elapsed = start_time.elapsed();
                let remaining = expire.saturating_sub(elapsed);
                print!(
                    "\r  Waiting for receiver... ({} remaining)   ",
                    format_remaining(remaining)
                );
                let _ = io::stdout().flush();
                waiting_printed = true;
            } else if !waiting_printed {
                print!("\r  Waiting for receiver...   ");
                let _ = io::stdout().flush();
                waiting_printed = true;
            }
        } else if progress.state == TransferState::Transferring {
            let pct = progress.percentage();
            let speed = format_size(progress.speed_bps);
            let eta = progress
                .eta
                .map_or_else(|| "--".to_string(), |d| format!("{}s", d.as_secs()));

            print!(
                "\r  [{:>6.2}%] {} - {}/s - ETA: {}    ",
                pct, progress.current_file_name, speed, eta
            );
            let _ = io::stdout().flush();
        }

        if timeout.is_err() {
            continue;
        }
        if timeout.unwrap().is_err() {
            break;
        }
    }

    println!();
}
