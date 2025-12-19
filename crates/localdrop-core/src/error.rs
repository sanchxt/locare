//! Error types for LocalDrop.
//!
//! This module provides a unified error type for all LocalDrop operations,
//! with specific error variants for different failure modes.

use std::io;
use std::net::SocketAddr;

use thiserror::Error;

/// A specialized `Result` type for LocalDrop operations.
pub type Result<T> = std::result::Result<T, Error>;

/// The main error type for LocalDrop.
#[derive(Error, Debug)]
pub enum Error {
    /// No network connection detected (E001)
    #[error("no network connection detected")]
    NoNetwork,

    /// Unable to broadcast on network (E002)
    #[error("unable to broadcast on network: {0}")]
    BroadcastFailed(String),

    /// Code not found on network (E003)
    #[error("code '{0}' not found on network")]
    CodeNotFound(String),

    /// Code has expired (E004)
    #[error("code has expired")]
    CodeExpired,

    /// Code collision detected during generation
    #[error("code collision detected, unable to generate unique code")]
    CodeCollision,

    /// Connection lost during transfer (E005)
    #[error("connection lost during transfer to {0}")]
    ConnectionLost(SocketAddr),

    /// Checksum mismatch detected (E006)
    #[error("checksum mismatch for chunk {chunk} of file '{file}'")]
    ChecksumMismatch {
        /// The file being transferred
        file: String,
        /// The chunk number that failed
        chunk: u64,
    },

    /// Transfer was cancelled
    #[error("transfer cancelled")]
    TransferCancelled,

    /// Transfer rejected by receiver
    #[error("transfer rejected by receiver")]
    TransferRejected,

    /// Resume state doesn't match current transfer
    #[error("resume mismatch: {0}")]
    ResumeMismatch(String),

    /// Resume request was rejected by sender
    #[error("resume rejected: {0}")]
    ResumeRejected(String),

    /// Cannot read file: permission denied (E007)
    #[error("cannot read file '{0}': permission denied")]
    PermissionDenied(String),

    /// Insufficient disk space (E008)
    #[error("insufficient disk space: need {needed} bytes, have {available} bytes")]
    InsufficientSpace {
        /// Bytes needed
        needed: u64,
        /// Bytes available
        available: u64,
    },

    /// File not found
    #[error("file not found: {0}")]
    FileNotFound(String),

    /// Invalid path
    #[error("invalid path: {0}")]
    InvalidPath(String),

    /// Too many failed attempts (E009)
    #[error("too many failed attempts, locked for {0} seconds")]
    RateLimited(u64),

    /// Connection rejected by sender (E010)
    #[error("connection rejected by sender")]
    ConnectionRejected,

    /// Invalid code format
    #[error("invalid code format: {0}")]
    InvalidCodeFormat(String),

    /// TLS handshake failed
    #[error("TLS handshake failed: {0}")]
    TlsError(String),

    /// Signature verification failed
    #[error("signature verification failed")]
    SignatureInvalid,

    /// Invalid protocol message
    #[error("invalid protocol message: {0}")]
    ProtocolError(String),

    /// Unsupported protocol version
    #[error("unsupported protocol version: {major}.{minor}")]
    UnsupportedVersion {
        /// Major version
        major: u8,
        /// Minor version
        minor: u8,
    },

    /// Unexpected message type
    #[error("unexpected message type: expected {expected}, got {actual}")]
    UnexpectedMessage {
        /// Expected message type
        expected: String,
        /// Actual message type received
        actual: String,
    },

    /// Configuration file error
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// Invalid configuration value
    #[error("invalid configuration value for '{key}': {reason}")]
    InvalidConfig {
        /// Configuration key
        key: String,
        /// Reason for invalidity
        reason: String,
    },

    /// Device not trusted
    #[error("device '{0}' is not trusted")]
    DeviceNotTrusted(String),

    /// Trust database error
    #[error("trust database error: {0}")]
    TrustDbError(String),

    /// Preview generation failed
    #[error("failed to generate preview for '{file}': {reason}")]
    PreviewFailed {
        /// File path
        file: String,
        /// Reason for failure
        reason: String,
    },

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Serialization error
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Internal error (should not happen)
    #[error("internal error: {0}")]
    Internal(String),

    /// Operation timeout
    #[error("operation timed out after {0} seconds")]
    Timeout(u64),

    /// Keep-alive failed (connection may be dead)
    #[error("keep-alive failed: no response after {0} seconds")]
    KeepAliveFailed(u64),

    /// Clipboard access failed
    #[error("clipboard error: {0}")]
    ClipboardError(String),

    /// Clipboard is empty
    #[error("clipboard is empty")]
    ClipboardEmpty,

    /// Unsupported clipboard content type
    #[error("unsupported clipboard content type: {0}")]
    UnsupportedClipboardType(String),
}

impl Error {
    /// Returns the error code associated with this error, if any.
    ///
    /// Error codes follow the pattern EXXX where XXX is a 3-digit number.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::NoNetwork => Some("E001"),
            Self::BroadcastFailed(_) => Some("E002"),
            Self::CodeNotFound(_) => Some("E003"),
            Self::CodeExpired => Some("E004"),
            Self::ConnectionLost(_) => Some("E005"),
            Self::ChecksumMismatch { .. } => Some("E006"),
            Self::PermissionDenied(_) => Some("E007"),
            Self::InsufficientSpace { .. } => Some("E008"),
            Self::RateLimited(_) => Some("E009"),
            Self::ConnectionRejected => Some("E010"),
            _ => None,
        }
    }

    /// Returns whether this error is recoverable (can be retried).
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::ConnectionLost(_)
                | Self::ChecksumMismatch { .. }
                | Self::RateLimited(_)
                | Self::Timeout(_)
                | Self::KeepAliveFailed(_)
        )
    }
}
