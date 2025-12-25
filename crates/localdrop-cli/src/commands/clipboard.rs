//! Clipboard sharing command implementation.

use std::io::{self, Write};

use anyhow::{bail, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

use localdrop_core::clipboard::{
    diagnose_clipboard, ClipboardReceiveSession, ClipboardShareSession, ClipboardSyncSession,
    SyncSessionRunner,
};
use localdrop_core::transfer::TransferConfig;

use super::{ClipboardAction, ClipboardArgs};
use crate::ui::CodeBox;

/// Run the clipboard command.
pub async fn run(args: ClipboardArgs) -> Result<()> {
    match args.action {
        ClipboardAction::Share(share_args) => run_share(share_args, args.quiet, args.json).await,
        ClipboardAction::Receive(recv_args) => run_receive(recv_args, args.quiet, args.json).await,
        ClipboardAction::Sync(sync_args) => run_sync(sync_args, args.quiet, args.json).await,
    }
}

/// Run clipboard share (one-shot).
#[allow(clippy::unused_async)]
async fn run_share(_args: super::ClipboardShareArgs, quiet: bool, json: bool) -> Result<()> {
    if !quiet && !json {
        println!();
        println!("LocalDrop Clipboard Share");
        println!("{}", "-".repeat(37));
        println!();
    }

    let config = TransferConfig::default();

    let session = match ClipboardShareSession::new(config).await {
        Ok(s) => s,
        Err(e) => {
            let error_str = format!("{}", e);
            if json {
                let output = serde_json::json!({
                    "status": "error",
                    "error": &error_str,
                    "diagnostics": diagnose_clipboard(),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                eprintln!("  Error: {}", e);
                print_clipboard_troubleshooting(&error_str);
            }
            bail!("{}", e);
        }
    };

    let code = session.code().to_string();
    let content_preview = session.content().preview(50);
    let content_size = session.content().format_size();

    if json {
        let output = serde_json::json!({
            "status": "waiting",
            "code": code,
            "content": {
                "preview": content_preview,
                "size": content_size,
                "type": format!("{:?}", session.content().content_type()),
            },
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if !quiet {
        println!("  Sharing clipboard: {}", content_preview);
        println!("  Size: {}", content_size);
        println!();
        CodeBox::new(&code).display();
        println!();
        println!("  Waiting for receiver...");
        println!();
    }

    let result = session.wait().await;

    match result {
        Ok(()) => {
            if json {
                let output = serde_json::json!({
                    "status": "complete",
                    "code": code,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                println!("  Clipboard sent successfully!");
                println!();
            }
            Ok(())
        }
        Err(e) => {
            if json {
                let output = serde_json::json!({
                    "status": "error",
                    "error": format!("{}", e),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                eprintln!("  Error: {}", e);
            }
            Err(e.into())
        }
    }
}

/// Run clipboard receive (one-shot).
#[allow(clippy::too_many_lines)]
async fn run_receive(args: super::ClipboardReceiveArgs, quiet: bool, json: bool) -> Result<()> {
    if !quiet && !json {
        println!();
        println!("LocalDrop Clipboard Receive");
        println!("{}", "-".repeat(37));
        println!();
        println!("  Searching for code {}...", args.code);
        println!();
    }

    if json {
        let output = serde_json::json!({
            "status": "searching",
            "code": &args.code,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    }

    let config = TransferConfig::default();

    let mut session = match ClipboardReceiveSession::connect(&args.code, config).await {
        Ok(s) => s,
        Err(e) => {
            if json {
                let output = serde_json::json!({
                    "status": "error",
                    "error": format!("{}", e),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                eprintln!("  Error: {}", e);
            }
            bail!("{}", e);
        }
    };

    let (sender_addr, sender_name) = session.sender();
    let metadata = session.metadata();
    let preview = metadata.as_ref().map_or_else(
        || "unknown".to_string(),
        |m| format!("{:?}, {} bytes", m.content_type, m.size),
    );

    if json {
        let output = serde_json::json!({
            "status": "connected",
            "sender": {
                "name": sender_name,
                "address": sender_addr.to_string(),
            },
            "content": {
                "type": metadata.as_ref().map(|m| format!("{:?}", m.content_type)),
                "size": metadata.as_ref().map(|m| m.size),
            },
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if !quiet {
        println!("  Found sender: {} ({})", sender_name, sender_addr);
        println!("  Content: {}", preview);
        println!();
    }

    let accepted = if !args.batch && !json && !quiet {
        session.start_keep_alive()?;

        print!("  Accept clipboard content? [Y/n] ");
        io::stdout().flush()?;

        let mut input = String::new();
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        reader.read_line(&mut input).await?;
        let input = input.trim().to_lowercase();

        input.is_empty() || input == "y" || input == "yes"
    } else {
        true
    };

    if !accepted {
        session.decline().await;
        if !quiet && !json {
            println!();
            println!("  Transfer declined.");
            println!();
        }
        if json {
            let output = serde_json::json!({
                "status": "declined",
                "code": &args.code,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        return Ok(());
    }

    let result = session.accept_to_clipboard().await;

    match result {
        Ok(()) => {
            if json {
                let output = serde_json::json!({
                    "status": "complete",
                    "code": &args.code,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                println!();
                println!("  Clipboard received and copied!");
                println!();
            }
            Ok(())
        }
        Err(e) => {
            if json {
                let output = serde_json::json!({
                    "status": "error",
                    "error": format!("{}", e),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                eprintln!("  Error: {}", e);
            }
            Err(e.into())
        }
    }
}

/// Run clipboard sync (bidirectional live sync).
async fn run_sync(args: super::ClipboardSyncArgs, quiet: bool, json: bool) -> Result<()> {
    if !quiet && !json {
        println!();
        println!("LocalDrop Clipboard Sync");
        println!("{}", "-".repeat(37));
        println!();
    }

    let config = TransferConfig::default();

    if let Some(ref code_str) = args.code {
        if !quiet && !json {
            println!("  Connecting to sync session {}...", code_str);
            println!();
        }

        let (session, runner) = match ClipboardSyncSession::connect(code_str, config).await {
            Ok(s) => s,
            Err(e) => {
                if json {
                    let output = serde_json::json!({
                        "status": "error",
                        "error": format!("{}", e),
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else if !quiet {
                    eprintln!("  Error: {}", e);
                }
                bail!("{}", e);
            }
        };

        run_sync_session(session, runner, quiet, json).await
    } else {
        let (code, session, runner) = match ClipboardSyncSession::host(config).await {
            Ok(result) => result,
            Err(e) => {
                if json {
                    let output = serde_json::json!({
                        "status": "error",
                        "error": format!("{}", e),
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else if !quiet {
                    eprintln!("  Error: {}", e);
                }
                bail!("{}", e);
            }
        };

        if json {
            let output = serde_json::json!({
                "status": "waiting",
                "code": code.as_str(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if !quiet {
            CodeBox::new(code.as_str()).display();
            println!();
            println!("  Waiting for peer to connect...");
            println!("  Press Ctrl+C to stop sync");
            println!();
        }

        run_sync_session(session, runner, quiet, json).await
    }
}

/// Run the sync session loop.
async fn run_sync_session(
    session: ClipboardSyncSession,
    runner: SyncSessionRunner,
    quiet: bool,
    json: bool,
) -> Result<()> {
    use localdrop_core::clipboard::SyncEvent;

    if !quiet && !json {
        println!("  Sync active! Clipboard changes will be shared.");
        println!("  Connected to: {}", session.peer_name());
        println!();
    }

    let result = runner.run().await;

    match result {
        Ok((stats, mut event_rx)) => {
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    SyncEvent::Sent { content_type, size } => {
                        if !quiet && !json {
                            println!("  -> Sent {:?} ({} bytes)", content_type, size);
                        }
                    }
                    SyncEvent::Received { content_type, size } => {
                        if !quiet && !json {
                            println!("  <- Received {:?} ({} bytes)", content_type, size);
                        }
                    }
                }
            }

            if json {
                let output = serde_json::json!({
                    "status": "complete",
                    "stats": {
                        "duration_secs": stats.duration.as_secs(),
                        "items_sent": stats.items_sent,
                        "items_received": stats.items_received,
                        "bytes_sent": stats.bytes_sent,
                        "bytes_received": stats.bytes_received,
                    },
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                println!();
                println!("  Sync session ended.");
                println!(
                    "  Sent: {} items ({} bytes)",
                    stats.items_sent, stats.bytes_sent
                );
                println!(
                    "  Received: {} items ({} bytes)",
                    stats.items_received, stats.bytes_received
                );
                println!();
            }

            session.shutdown();

            Ok(())
        }
        Err(e) => {
            if json {
                let output = serde_json::json!({
                    "status": "error",
                    "error": format!("{}", e),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if !quiet {
                eprintln!("  Sync error: {}", e);
            }
            Err(e.into())
        }
    }
}

/// Print platform-specific troubleshooting hints for clipboard errors.
fn print_clipboard_troubleshooting(error: &str) {
    let is_empty = error.contains("clipboard is empty");
    let is_access_error = error.contains("Cannot access clipboard");

    if !is_empty && !is_access_error {
        return;
    }

    eprintln!();
    eprintln!("  Troubleshooting:");

    #[cfg(target_os = "linux")]
    {
        if is_empty {
            eprintln!("  - Make sure you've copied something to the clipboard first");
        }
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            eprintln!("  - Running on Wayland - clipboard access should work");
            if is_access_error {
                eprintln!("  - Check if your compositor supports wlr-data-control protocol");
            }
        } else if std::env::var("DISPLAY").is_ok() {
            eprintln!("  - Running on X11 - clipboard access should work");
        } else {
            eprintln!("  - No display server detected (DISPLAY/WAYLAND_DISPLAY not set)");
            eprintln!("  - Run this command from a graphical terminal session");
        }
        eprintln!("  - Run with RUST_LOG=debug for detailed diagnostics");
    }

    #[cfg(target_os = "macos")]
    {
        eprintln!("  - Make sure you've copied something (Cmd+C) first");
        eprintln!("  - Check System Preferences > Privacy & Security for clipboard access");
    }

    #[cfg(target_os = "windows")]
    {
        eprintln!("  - Make sure you've copied something (Ctrl+C) first");
        eprintln!("  - Try closing other applications that might be locking the clipboard");
    }

    eprintln!();
}
