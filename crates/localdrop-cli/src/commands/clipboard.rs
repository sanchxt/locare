//! Clipboard sharing command implementation.

use std::io::{self, Write};

use anyhow::{bail, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

use localdrop_core::clipboard::{
    ClipboardReceiveSession, ClipboardShareSession, ClipboardSyncSession, SyncSessionRunner,
};
use localdrop_core::transfer::TransferConfig;

use super::{ClipboardAction, ClipboardArgs};

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
        println!("  +-----------------------------------+");
        println!("  |                                   |");
        println!("  |       Code:  {}                   |", code);
        println!("  |                                   |");
        println!("  +-----------------------------------+");
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

    // Ask for confirmation unless in batch mode
    let accepted = if !args.batch && !json && !quiet {
        // Start keep-alive to prevent connection timeout while user reads prompt
        session.start_keep_alive()?;

        print!("  Accept clipboard content? [Y/n] ");
        io::stdout().flush()?;

        // Use async stdin to not block the tokio runtime
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
        // Connect to existing sync session
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
        // Host new sync session
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
            println!("  +-----------------------------------+");
            println!("  |                                   |");
            println!("  |       Code:  {}                   |", code.as_str());
            println!("  |                                   |");
            println!("  +-----------------------------------+");
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

    // Run the sync session
    let result = runner.run().await;

    match result {
        Ok((stats, mut event_rx)) => {
            // Drain any remaining events
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

            // Print final stats
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

            // Signal session shutdown
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
