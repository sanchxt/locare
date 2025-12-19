//! # LocalDrop Core Library
//!
//! `localdrop-core` provides the core functionality for LocalDrop, a cross-platform
//! local network file sharing tool.
//!
//! ## Features
//!
//! - **Code-based discovery**: Simple 4-character codes for finding shares
//! - **Secure transfers**: TLS 1.3 encryption for all transfers
//! - **Fast transfers**: Chunked transfers with parallel streams
//! - **Cross-platform**: Works on Windows, Linux, macOS, Android, and iOS
//!
//! ## Modules
//!
//! - [`clipboard`] - Clipboard sharing (one-shot and live sync)
//! - [`code`] - Share code generation and validation
//! - [`config`] - Configuration management
//! - [`crypto`] - Cryptographic primitives (TLS, hashing, signatures)
//! - [`discovery`] - Network discovery via UDP broadcast and mDNS
//! - [`mod@file`] - File operations, chunking, and metadata
//! - [`preview`] - File preview generation (thumbnails, text snippets)
//! - [`protocol`] - LDRP wire protocol implementation
//! - [`transfer`] - File transfer engine
//! - [`trust`] - Trusted devices management
//! - [`web`] - Embedded web server for browser-based access
//!
//! ## Example
//!
//! ```rust,ignore
//! use localdrop_core::{ShareSession, ReceiveSession};
//!
//! // Create a share session
//! let session = ShareSession::new(&["file.txt"]).await?;
//! println!("Share code: {}", session.code());
//!
//! // On another device, receive using the code
//! let receiver = ReceiveSession::connect("A7K9").await?;
//! receiver.accept_all().await?;
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unused_async)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::missing_const_for_fn)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::unused_self)]

pub mod clipboard;
pub mod code;
pub mod config;
pub mod crypto;
pub mod discovery;
pub mod error;
pub mod file;
pub mod preview;
pub mod protocol;
pub mod transfer;
pub mod trust;

#[cfg(feature = "web")]
pub mod web;

pub use error::{Error, Result};

/// Library version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Protocol version for LDRP
pub const PROTOCOL_VERSION: (u8, u8) = (1, 0);

/// Default discovery port (UDP)
pub const DEFAULT_DISCOVERY_PORT: u16 = 52525;

/// Fallback discovery port (UDP)
pub const FALLBACK_DISCOVERY_PORT: u16 = 52526;

/// Default transfer port range start
pub const DEFAULT_TRANSFER_PORT_START: u16 = 52530;

/// Default transfer port range end
pub const DEFAULT_TRANSFER_PORT_END: u16 = 52540;

/// Default code expiration time in seconds
pub const DEFAULT_CODE_EXPIRATION_SECS: u64 = 300;

/// Default chunk size for file transfers (1 MB)
pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Maximum parallel chunk streams
pub const DEFAULT_PARALLEL_CHUNKS: usize = 4;
