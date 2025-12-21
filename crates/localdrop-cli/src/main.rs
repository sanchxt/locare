//! LocalDrop CLI - Cross-platform local network file sharing
//!
//! LocalDrop enables seamless peer-to-peer file transfers over local networks
//! using simple, time-limited codes.
//!
//! ## Quick Start
//!
//! ```bash
//! # Share files
//! localdrop share ./document.pdf
//!
//! # Receive files (on another device)
//! localdrop receive A7K9
//! ```

#![allow(clippy::doc_markdown)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::unused_async)]
#![allow(clippy::struct_excessive_bools)]

use anyhow::Result;
use clap::Parser;

mod commands;

use commands::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let cli = Cli::parse();

    match cli.command {
        Command::Share(args) => commands::share::run(args).await,
        Command::Receive(args) => commands::receive::run(args).await,
        Command::Send(args) => commands::send::run(args).await,
        Command::Clipboard(args) => commands::clipboard::run(args).await,
        Command::Scan(args) => commands::scan::run(args).await,
        Command::Trust(args) => commands::trust::run(args).await,
        Command::Web(args) => commands::web::run(args).await,
        Command::Config(args) => commands::config::run(args).await,
        Command::Diagnose(args) => commands::diagnose::run(args).await,
        Command::History(args) => commands::history::run(args).await,
        Command::InternalClipboardHold(args) => {
            // Internal command - runs synchronously to hold clipboard content
            commands::internal::run_clipboard_hold(&args.content_type, args.timeout)
        }
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,localdrop=info,localdrop_core=info"));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).without_time())
        .with(filter)
        .init();
}
