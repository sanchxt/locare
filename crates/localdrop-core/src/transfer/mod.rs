//! File transfer engine for LocalDrop.
//!
//! This module handles the actual transfer of files between devices:
//!
//! - Establishing connections
//! - Chunked file transfers
//! - Progress tracking
//! - Resume support
//!
//! ## Transfer Protocol
//!
//! - Default chunk size: 1MB
//! - Adaptive sizing based on network conditions
//! - Parallel chunks: Up to 4 concurrent streams
//! - Checksum: xxHash64 per chunk, SHA-256 for complete file

pub mod resume;
pub mod trusted;

pub use resume::ResumeManager;
pub use trusted::{SenderInfo, TrustedReceiveSession, TrustedSendSession};

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

use crate::code::{CodeGenerator, ShareCode};
use crate::crypto::{self, TlsConfig};
use crate::discovery::{
    DiscoveryPacket, HybridBroadcaster, HybridListener, DEFAULT_DISCOVERY_PORT,
};
use crate::error::{Error, Result};
use crate::file::{
    enumerate_files, EnumerateOptions, FileChunk, FileChunker, FileMetadata, FileWriter,
};
use crate::protocol::{
    self, ChunkAckPayload, ChunkDataPayload, ChunkStartPayload, CodeVerifyAckPayload,
    CodeVerifyPayload, FileListAckPayload, FileListPayload, HelloPayload, MessageType,
};

/// Default transfer port.
pub const DEFAULT_TRANSFER_PORT: u16 = 52530;

/// Configure TCP keep-alive on a socket.
///
/// This enables OS-level TCP keep-alive to prevent network equipment
/// (routers, firewalls, NAT) from closing idle connections.
///
/// Configuration:
/// - Start probing after 10 seconds of idle time
/// - Send probes every 5 seconds
/// - Give up after 3 failed probes (~25 seconds total)
fn configure_tcp_keepalive(stream: &TcpStream) -> Result<()> {
    let socket_ref = SockRef::from(stream);

    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(10))
        .with_interval(Duration::from_secs(5));

    socket_ref
        .set_tcp_keepalive(&keepalive)
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;

    tracing::debug!("TCP keep-alive enabled on socket");
    Ok(())
}

/// Transfer direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Sending files
    Send,
    /// Receiving files
    Receive,
}

/// Transfer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    /// Preparing transfer
    Preparing,
    /// Waiting for connection
    Waiting,
    /// Connected, awaiting confirmation
    Connected,
    /// Transferring files
    Transferring,
    /// Transfer completed successfully
    Completed,
    /// Transfer was cancelled
    Cancelled,
    /// Transfer failed
    Failed,
}

/// Progress information for a transfer.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    /// Current state
    pub state: TransferState,
    /// Current file index
    pub current_file: usize,
    /// Total number of files
    pub total_files: usize,
    /// Current file name
    pub current_file_name: String,
    /// Bytes transferred for current file
    pub file_bytes_transferred: u64,
    /// Total bytes for current file
    pub file_total_bytes: u64,
    /// Total bytes transferred across all files
    pub total_bytes_transferred: u64,
    /// Total bytes across all files
    pub total_bytes: u64,
    /// Transfer speed in bytes per second
    pub speed_bps: u64,
    /// Estimated time remaining
    pub eta: Option<Duration>,
    /// When transfer started
    pub started_at: Instant,
}

impl TransferProgress {
    /// Create a new progress tracker.
    #[must_use]
    pub fn new(total_files: usize, total_bytes: u64) -> Self {
        Self {
            state: TransferState::Preparing,
            current_file: 0,
            total_files,
            current_file_name: String::new(),
            file_bytes_transferred: 0,
            file_total_bytes: 0,
            total_bytes_transferred: 0,
            total_bytes,
            speed_bps: 0,
            eta: None,
            started_at: Instant::now(),
        }
    }

    /// Get overall progress as a percentage (0.0 - 100.0).
    #[must_use]
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            100.0
        } else {
            (self.total_bytes_transferred as f64 / self.total_bytes as f64) * 100.0
        }
    }
}

/// Configuration for a transfer session.
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Chunk size in bytes
    pub chunk_size: usize,
    /// Number of parallel streams
    pub parallel_streams: usize,
    /// Bandwidth limit (bytes per second)
    pub bandwidth_limit: Option<u64>,
    /// Enable compression
    pub compress: bool,
    /// Verify checksums
    pub verify_checksums: bool,
    /// Discovery port
    pub discovery_port: u16,
    /// Transfer port
    pub transfer_port: u16,
    /// Discovery timeout
    pub discovery_timeout: Duration,
    /// Broadcast interval for discovery announcements
    pub broadcast_interval: Duration,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size: crate::DEFAULT_CHUNK_SIZE,
            parallel_streams: crate::DEFAULT_PARALLEL_CHUNKS,
            bandwidth_limit: None,
            compress: false,
            verify_checksums: true,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            transfer_port: DEFAULT_TRANSFER_PORT,
            discovery_timeout: Duration::from_secs(30),
            broadcast_interval: Duration::from_secs(2),
        }
    }
}

/// A share session (sender side).
pub struct ShareSession {
    /// Share code
    code: ShareCode,
    /// Files being shared
    files: Vec<FileMetadata>,
    /// File paths (for reading)
    file_paths: Vec<PathBuf>,
    /// Transfer configuration
    config: TransferConfig,
    /// Device name
    device_name: String,
    /// Device identity (for trust feature)
    identity: crypto::DeviceIdentity,
    /// Session key for HMAC verification
    session_key: [u8; 32],
    /// Progress sender
    progress_tx: watch::Sender<TransferProgress>,
    /// Progress receiver (for cloning to observers)
    progress_rx: watch::Receiver<TransferProgress>,
    /// TCP listener
    listener: TcpListener,
    /// TLS config
    tls_config: TlsConfig,
    /// Hybrid discovery broadcaster (UDP + mDNS)
    broadcaster: HybridBroadcaster,
    /// Receiver's device ID (captured after transfer)
    receiver_device_id: Option<Uuid>,
    /// Receiver's public key (captured after transfer)
    receiver_public_key: Option<String>,
    /// Receiver's device name (captured after transfer)
    receiver_name: Option<String>,
}

impl std::fmt::Debug for ShareSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareSession")
            .field("code", &self.code)
            .field("files", &self.files)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ShareSession {
    /// Create a new share session.
    ///
    /// # Arguments
    ///
    /// * `paths` - Paths to share
    /// * `config` - Transfer configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be created.
    pub async fn new(paths: &[PathBuf], config: TransferConfig) -> Result<Self> {
        let code = CodeGenerator::new().generate()?;

        let options = EnumerateOptions::default();
        let files = enumerate_files(paths, &options)?;

        if files.is_empty() {
            return Err(Error::FileNotFound("no files to share".to_string()));
        }

        let file_paths: Vec<PathBuf> = paths.to_vec();

        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        let progress = TransferProgress::new(files.len(), total_bytes);
        let (progress_tx, progress_rx) = watch::channel(progress);

        let session_key = crypto::derive_session_key(code.as_str());

        let tls_config = TlsConfig::server()?;

        let listener = TcpListener::bind(format!("0.0.0.0:{}", config.transfer_port)).await?;
        let local_addr = listener.local_addr()?;

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        let broadcaster = HybridBroadcaster::new(config.discovery_port).await?;

        let identity = crypto::DeviceIdentity::load_or_generate()?;
        let device_id = identity.device_id();

        let packet = DiscoveryPacket::new(
            &code,
            &device_name,
            device_id,
            local_addr.port(),
            files.len(),
            total_bytes,
        );

        broadcaster.start(packet, config.broadcast_interval).await?;

        Ok(Self {
            code,
            files,
            file_paths,
            config,
            device_name,
            identity,
            session_key,
            progress_tx,
            progress_rx,
            listener,
            tls_config,
            broadcaster,
            receiver_device_id: None,
            receiver_public_key: None,
            receiver_name: None,
        })
    }

    /// Get the share code.
    #[must_use]
    pub fn code(&self) -> &ShareCode {
        &self.code
    }

    /// Get the files being shared.
    #[must_use]
    pub fn files(&self) -> &[FileMetadata] {
        &self.files
    }

    /// Get a progress receiver.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TransferProgress> {
        self.progress_rx.clone()
    }

    /// Get the receiver's device ID (after transfer completes).
    #[must_use]
    pub fn receiver_device_id(&self) -> Option<Uuid> {
        self.receiver_device_id
    }

    /// Get the receiver's public key (after transfer completes).
    #[must_use]
    pub fn receiver_public_key(&self) -> Option<&str> {
        self.receiver_public_key.as_deref()
    }

    /// Get the receiver's device name (after transfer completes).
    #[must_use]
    pub fn receiver_name(&self) -> Option<&str> {
        self.receiver_name.as_deref()
    }

    /// Wait for a receiver to connect and complete the transfer.
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn wait(&mut self) -> Result<()> {
        self.update_state(TransferState::Waiting);

        let (stream, peer_addr) = self.listener.accept().await?;
        tracing::info!("Connection from {}", peer_addr);

        configure_tcp_keepalive(&stream)?;

        let acceptor = TlsAcceptor::from(Arc::new(
            self.tls_config
                .server_config()
                .ok_or_else(|| Error::TlsError("no server config".to_string()))?
                .clone(),
        ));
        let mut tls_stream = acceptor
            .accept(stream)
            .await
            .map_err(|e| Error::TlsError(format!("TLS handshake failed: {e}")))?;

        self.update_state(TransferState::Connected);

        let (receiver_name, receiver_device_id, receiver_public_key) =
            self.do_handshake(&mut tls_stream).await?;
        self.receiver_name = Some(receiver_name);
        self.receiver_device_id = receiver_device_id;
        self.receiver_public_key = receiver_public_key;

        self.do_code_verification(&mut tls_stream).await?;

        let accepted = self.do_file_list_exchange(&mut tls_stream).await?;
        if !accepted {
            self.update_state(TransferState::Cancelled);
            return Err(Error::TransferRejected);
        }

        self.update_state(TransferState::Transferring);

        self.do_transfer(&mut tls_stream).await?;

        self.broadcaster.stop().await;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    /// Cancel the share session.
    pub async fn cancel(&mut self) {
        self.broadcaster.stop().await;
        self.update_state(TransferState::Cancelled);
    }

    fn update_state(&self, state: TransferState) {
        let mut progress = self.progress_rx.borrow().clone();
        progress.state = state;
        let _ = self.progress_tx.send(progress);
    }

    async fn do_handshake<S>(
        &self,
        stream: &mut S,
    ) -> Result<(String, Option<Uuid>, Option<String>)>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello = HelloPayload {
            device_name: self.device_name.clone(),
            protocol_version: "1.0".to_string(),
            device_id: Some(self.identity.device_id()),
            public_key: Some(self.identity.public_key_base64()),
        };
        let payload = protocol::encode_payload(&hello)?;
        protocol::write_frame(stream, MessageType::Hello, &payload).await?;

        let (header, ack_payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::HelloAck {
            return Err(Error::UnexpectedMessage {
                expected: "HelloAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let ack: HelloPayload = protocol::decode_payload(&ack_payload)?;
        Ok((ack.device_name, ack.device_id, ack.public_key))
    }

    async fn do_code_verification<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::CodeVerify {
            return Err(Error::UnexpectedMessage {
                expected: "CodeVerify".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let verify: CodeVerifyPayload = protocol::decode_payload(&payload)?;

        let expected_hmac = crypto::hmac_sha256(&self.session_key, self.code.as_str().as_bytes());

        let success = crypto::constant_time_eq(&verify.code_hmac, &expected_hmac);

        let ack = CodeVerifyAckPayload {
            success,
            error: if success {
                None
            } else {
                Some("Invalid code".to_string())
            },
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(stream, MessageType::CodeVerifyAck, &ack_payload).await?;

        if !success {
            return Err(Error::CodeNotFound(self.code.to_string()));
        }

        Ok(())
    }

    async fn do_file_list_exchange<S>(&self, stream: &mut S) -> Result<bool>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let file_list = FileListPayload {
            files: self.files.clone(),
            total_size: self.files.iter().map(|f| f.size).sum(),
        };
        let payload = protocol::encode_payload(&file_list)?;
        protocol::write_frame(stream, MessageType::FileList, &payload).await?;

        loop {
            let (header, ack_payload) = protocol::read_frame(stream).await?;

            match header.message_type {
                MessageType::FileListAck => {
                    let ack: FileListAckPayload = protocol::decode_payload(&ack_payload)?;
                    return Ok(ack.accepted);
                }
                MessageType::Ping => {
                    tracing::debug!("Received Ping, responding with Pong");
                    protocol::write_frame(stream, MessageType::Pong, &[]).await?;
                }
                _ => {
                    return Err(Error::UnexpectedMessage {
                        expected: "FileListAck or Ping".to_string(),
                        actual: format!("{:?}", header.message_type),
                    });
                }
            }
        }
    }

    async fn do_transfer<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let chunker = FileChunker::new(self.config.chunk_size);

        for (file_index, file) in self.files.iter().enumerate() {
            {
                let mut progress = self.progress_rx.borrow().clone();
                progress.current_file = file_index;
                progress.current_file_name = file.file_name().to_string();
                progress.file_bytes_transferred = 0;
                progress.file_total_bytes = file.size;
                let _ = self.progress_tx.send(progress);
            }

            let file_path = self.find_file_path(&file.relative_path)?;

            let chunks = chunker.read_chunks(&file_path, file_index).await?;
            let total_chunks = chunks.len() as u64;

            if chunks.is_empty() {
                let start = ChunkStartPayload {
                    file_index,
                    chunk_index: 0,
                    total_chunks: 0,
                };
                let start_payload = protocol::encode_payload(&start)?;
                protocol::write_frame(stream, MessageType::ChunkStart, &start_payload).await?;
                tracing::debug!(
                    "Sent empty file marker for file {}: {}",
                    file_index,
                    file.file_name()
                );
            }

            for chunk in chunks {
                let start = ChunkStartPayload {
                    file_index,
                    chunk_index: chunk.chunk_index,
                    total_chunks,
                };
                let start_payload = protocol::encode_payload(&start)?;
                protocol::write_frame(stream, MessageType::ChunkStart, &start_payload).await?;

                let data = ChunkDataPayload {
                    file_index,
                    chunk_index: chunk.chunk_index,
                    data: chunk.data.clone(),
                    checksum: chunk.checksum,
                };
                let data_payload = protocol::encode_chunk_data(&data);
                protocol::write_frame(stream, MessageType::ChunkData, &data_payload).await?;

                let (header, ack_payload) = protocol::read_frame(stream).await?;
                if header.message_type != MessageType::ChunkAck {
                    return Err(Error::UnexpectedMessage {
                        expected: "ChunkAck".to_string(),
                        actual: format!("{:?}", header.message_type),
                    });
                }

                let ack: ChunkAckPayload = protocol::decode_payload(&ack_payload)?;
                if !ack.success {
                    return Err(Error::ChecksumMismatch {
                        file: file.file_name().to_string(),
                        chunk: chunk.chunk_index,
                    });
                }

                {
                    let mut progress = self.progress_rx.borrow().clone();
                    progress.file_bytes_transferred += chunk.data.len() as u64;
                    progress.total_bytes_transferred += chunk.data.len() as u64;
                    let elapsed = progress.started_at.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        {
                            progress.speed_bps =
                                (progress.total_bytes_transferred as f64 / elapsed) as u64;
                        }
                        let remaining = progress.total_bytes - progress.total_bytes_transferred;
                        if progress.speed_bps > 0 {
                            progress.eta =
                                Some(Duration::from_secs(remaining / progress.speed_bps));
                        }
                    }
                    let _ = self.progress_tx.send(progress);
                }
            }
        }

        protocol::write_frame(stream, MessageType::TransferComplete, &[]).await?;

        Ok(())
    }

    fn find_file_path(&self, relative_path: &Path) -> Result<PathBuf> {
        if self.file_paths.len() == 1 && self.file_paths[0].is_file() {
            return Ok(self.file_paths[0].clone());
        }

        for file_path in &self.file_paths {
            if file_path.is_file() {
                if let Some(name) = file_path.file_name() {
                    if name == relative_path.as_os_str() || file_path.ends_with(relative_path) {
                        return Ok(file_path.clone());
                    }
                }
            }
        }

        for base_path in &self.file_paths {
            if base_path.is_dir() {
                let full_path = base_path.join(relative_path);
                if full_path.exists() {
                    return Ok(full_path);
                }
            }
        }

        Err(Error::FileNotFound(relative_path.display().to_string()))
    }
}

/// Keep-alive interval for connection health checks (5 seconds)
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Timeout for Pong response (10 seconds)
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

/// Type alias for the TLS stream used by ReceiveSession
type ClientTlsStream = tokio_rustls::client::TlsStream<TcpStream>;

/// Handle to the keep-alive background task
struct KeepAliveHandle {
    /// Channel to signal the task to stop
    stop_tx: oneshot::Sender<()>,
    /// Handle to wait for the task and get the stream back
    task_handle: JoinHandle<Result<ClientTlsStream>>,
}

/// A receive session (receiver side).
pub struct ReceiveSession {
    /// Sender information
    sender_addr: SocketAddr,
    /// Sender device name
    sender_name: String,
    /// Sender device ID (for trust feature)
    sender_device_id: Option<Uuid>,
    /// Sender public key (for trust feature, base64-encoded)
    sender_public_key: Option<String>,
    /// Files being received
    files: Vec<FileMetadata>,
    /// Output directory
    output_dir: PathBuf,
    /// Transfer configuration (reserved for future use)
    _config: TransferConfig,
    /// Share code used for this transfer session
    code: ShareCode,
    /// Session key for HMAC verification (reserved for future use)
    _session_key: [u8; 32],
    /// Progress sender
    progress_tx: watch::Sender<TransferProgress>,
    /// Progress receiver
    progress_rx: watch::Receiver<TransferProgress>,
    /// TLS stream (stored after connect, None when keep-alive is running)
    tls_stream: Option<ClientTlsStream>,
    /// Keep-alive task handle (Some when keep-alive is running)
    keep_alive_handle: Option<KeepAliveHandle>,
}

impl std::fmt::Debug for ReceiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiveSession")
            .field("sender_addr", &self.sender_addr)
            .field("sender_name", &self.sender_name)
            .field("files", &self.files)
            .field("output_dir", &self.output_dir)
            .finish_non_exhaustive()
    }
}

impl Drop for ReceiveSession {
    fn drop(&mut self) {
        if let Some(handle) = self.keep_alive_handle.take() {
            handle.task_handle.abort();
        }
    }
}

impl ReceiveSession {
    /// Connect to a sender using a share code.
    ///
    /// # Arguments
    ///
    /// * `code` - The share code
    /// * `output_dir` - Directory to save files
    /// * `config` - Transfer configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub async fn connect(
        code: &ShareCode,
        output_dir: PathBuf,
        config: TransferConfig,
    ) -> Result<Self> {
        let listener = HybridListener::new(config.discovery_port).await?;
        let discovered = listener.find(code, config.discovery_timeout).await?;

        if let Err(e) = listener.shutdown() {
            tracing::debug!("Listener shutdown: {e}");
        }

        tracing::info!(
            "Found share from {} at {} (via hybrid discovery)",
            discovered.packet.device_name,
            discovered.source
        );

        let transfer_addr =
            SocketAddr::new(discovered.source.ip(), discovered.packet.transfer_port);
        let stream = TcpStream::connect(transfer_addr).await?;

        configure_tcp_keepalive(&stream)?;

        let tls_config = TlsConfig::client()?;
        let connector = TlsConnector::from(Arc::new(
            tls_config
                .client_config()
                .ok_or_else(|| Error::TlsError("no client config".to_string()))?
                .clone(),
        ));
        let mut tls_stream = connector
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .map_err(|e| Error::TlsError(format!("TLS handshake failed: {e}")))?;

        let session_key = crypto::derive_session_key(code.as_str());

        let (sender_device_id, sender_public_key) = Self::do_handshake(&mut tls_stream).await?;

        Self::do_code_verification(&mut tls_stream, code, &session_key).await?;

        let files = Self::receive_file_list(&mut tls_stream).await?;

        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        let progress = TransferProgress::new(files.len(), total_bytes);
        let (progress_tx, progress_rx) = watch::channel(progress);

        Ok(Self {
            sender_addr: transfer_addr,
            sender_name: discovered.packet.device_name,
            sender_device_id,
            sender_public_key,
            files,
            output_dir,
            _config: config,
            code: code.clone(),
            _session_key: session_key,
            progress_tx,
            progress_rx,
            tls_stream: Some(tls_stream),
            keep_alive_handle: None,
        })
    }

    /// Get sender information.
    #[must_use]
    pub fn sender(&self) -> (&SocketAddr, &str) {
        (&self.sender_addr, &self.sender_name)
    }

    /// Get sender device ID (if available, for trust feature).
    #[must_use]
    pub fn sender_device_id(&self) -> Option<Uuid> {
        self.sender_device_id
    }

    /// Get sender public key (if available, for trust feature).
    #[must_use]
    pub fn sender_public_key(&self) -> Option<&str> {
        self.sender_public_key.as_deref()
    }

    /// Get the files being received.
    #[must_use]
    pub fn files(&self) -> &[FileMetadata] {
        &self.files
    }

    /// Get the output directory.
    #[must_use]
    pub fn output_dir(&self) -> &PathBuf {
        &self.output_dir
    }

    /// Get a progress receiver.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TransferProgress> {
        self.progress_rx.clone()
    }

    /// Start the keep-alive background task.
    ///
    /// This spawns a background task that sends Ping messages at regular intervals
    /// to keep the connection alive while waiting for user input (e.g., Y/n prompt).
    ///
    /// Call `stop_keep_alive()` before calling `accept()` or `decline()`.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS stream is not available (already taken).
    pub fn start_keep_alive(&mut self) -> Result<()> {
        if self.keep_alive_handle.is_some() {
            return Ok(());
        }

        let stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream for keep-alive".to_string()))?;

        let (stop_tx, stop_rx) = oneshot::channel();

        let task_handle = tokio::spawn(async move { Self::keep_alive_task(stream, stop_rx).await });

        self.keep_alive_handle = Some(KeepAliveHandle {
            stop_tx,
            task_handle,
        });

        tracing::debug!("Keep-alive task started");
        Ok(())
    }

    /// Stop the keep-alive background task and recover the TLS stream.
    ///
    /// This must be called before `accept()` or `decline()` if keep-alive was started.
    ///
    /// # Errors
    ///
    /// Returns an error if the keep-alive task failed or the stream cannot be recovered.
    pub async fn stop_keep_alive(&mut self) -> Result<()> {
        if let Some(handle) = self.keep_alive_handle.take() {
            let _ = handle.stop_tx.send(());

            match handle.task_handle.await {
                Ok(Ok(stream)) => {
                    self.tls_stream = Some(stream);
                    tracing::debug!("Keep-alive task stopped, stream recovered");
                    Ok(())
                }
                Ok(Err(e)) => {
                    tracing::warn!("Keep-alive task encountered error: {}", e);
                    Err(e)
                }
                Err(e) => {
                    tracing::error!("Keep-alive task panicked: {}", e);
                    Err(Error::Internal(format!("keep-alive task panicked: {e}")))
                }
            }
        } else {
            Ok(())
        }
    }

    /// The background keep-alive task.
    ///
    /// Sends Ping messages at regular intervals and waits for Pong responses.
    /// Returns the stream when signaled to stop or when an error occurs.
    async fn keep_alive_task(
        mut stream: ClientTlsStream,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<ClientTlsStream> {
        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    tracing::debug!("Keep-alive task received stop signal");
                    return Ok(stream);
                }

                () = tokio::time::sleep(KEEPALIVE_INTERVAL) => {
                    tracing::debug!("Sending keep-alive Ping");

                    if let Err(e) = protocol::write_frame(&mut stream, MessageType::Ping, &[]).await {
                        tracing::warn!("Failed to send Ping: {}", e);
                        return Err(e);
                    }

                    match tokio::time::timeout(KEEPALIVE_TIMEOUT, protocol::read_frame(&mut stream)).await {
                        Ok(Ok((header, _))) => {
                            if header.message_type == MessageType::Pong {
                                tracing::debug!("Received Pong");
                            } else {
                                tracing::warn!("Expected Pong, got {:?}", header.message_type);
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("Failed to read Pong: {}", e);
                            return Err(e);
                        }
                        Err(_) => {
                            tracing::warn!("Pong timeout after {} seconds", KEEPALIVE_TIMEOUT.as_secs());
                            return Err(Error::KeepAliveFailed(KEEPALIVE_TIMEOUT.as_secs()));
                        }
                    }
                }
            }
        }
    }

    /// Accept the transfer and start receiving.
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn accept(&mut self) -> Result<()> {
        self.stop_keep_alive().await?;

        let mut stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream".to_string()))?;

        let ack = FileListAckPayload {
            accepted: true,
            accepted_files: None,
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut stream, MessageType::FileListAck, &ack_payload).await?;

        self.update_state(TransferState::Transferring);

        self.do_receive(&mut stream).await?;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    /// Accept specific files only.
    ///
    /// # Arguments
    ///
    /// * `indices` - Indices of files to accept
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn accept_files(&mut self, indices: &[usize]) -> Result<()> {
        self.stop_keep_alive().await?;

        let mut stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream".to_string()))?;

        let ack = FileListAckPayload {
            accepted: true,
            accepted_files: Some(indices.to_vec()),
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut stream, MessageType::FileListAck, &ack_payload).await?;

        self.update_state(TransferState::Transferring);

        self.do_receive(&mut stream).await?;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    /// Decline the transfer.
    pub async fn decline(&mut self) {
        let _ = self.stop_keep_alive().await;

        if let Some(mut stream) = self.tls_stream.take() {
            let ack = FileListAckPayload {
                accepted: false,
                accepted_files: None,
            };
            if let Ok(ack_payload) = protocol::encode_payload(&ack) {
                let _ = protocol::write_frame(&mut stream, MessageType::FileListAck, &ack_payload)
                    .await;
            }
            let _ = stream.shutdown().await;
        }
        self.update_state(TransferState::Cancelled);
    }

    /// Create a ResumeState for the current session.
    ///
    /// This can be used to persist the transfer state for later resumption.
    #[must_use]
    pub fn create_resume_state(&self, transfer_id: Uuid, sender_device_id: Uuid) -> ResumeState {
        ResumeState::new(
            transfer_id,
            self.code.as_str(),
            self.files.clone(),
            &self.sender_name,
            sender_device_id,
            self.output_dir.clone(),
        )
    }

    /// Connect to resume an interrupted transfer.
    ///
    /// # Arguments
    ///
    /// * `resume_state` - The saved state from a previous interrupted transfer
    /// * `config` - Transfer configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails or the sender doesn't accept the resume.
    pub async fn connect_with_resume(
        resume_state: ResumeState,
        config: TransferConfig,
    ) -> Result<Self> {
        let code = ShareCode::parse(&resume_state.code)?;

        let listener = HybridListener::new(config.discovery_port).await?;
        let discovered = listener.find(&code, config.discovery_timeout).await?;

        tracing::info!(
            "Found share from {} at {} for resume (via hybrid discovery)",
            discovered.packet.device_name,
            discovered.source
        );

        let transfer_addr =
            SocketAddr::new(discovered.source.ip(), discovered.packet.transfer_port);
        let stream = TcpStream::connect(transfer_addr).await?;

        configure_tcp_keepalive(&stream)?;

        let tls_config = TlsConfig::client()?;
        let connector = TlsConnector::from(Arc::new(
            tls_config
                .client_config()
                .ok_or_else(|| Error::TlsError("no client config".to_string()))?
                .clone(),
        ));
        let mut tls_stream = connector
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .map_err(|e| Error::TlsError(format!("TLS handshake failed: {e}")))?;

        let session_key = crypto::derive_session_key(code.as_str());

        let (sender_device_id, sender_public_key) = Self::do_handshake(&mut tls_stream).await?;
        Self::do_code_verification(&mut tls_stream, &code, &session_key).await?;

        let files = Self::receive_file_list(&mut tls_stream).await?;

        if files.len() != resume_state.files.len() {
            return Err(Error::ResumeMismatch(
                "File list has changed since the transfer was interrupted".to_string(),
            ));
        }

        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        let mut progress = TransferProgress::new(files.len(), total_bytes);
        progress.total_bytes_transferred = resume_state.bytes_received;
        let (progress_tx, progress_rx) = watch::channel(progress);

        Ok(Self {
            sender_addr: transfer_addr,
            sender_name: discovered.packet.device_name,
            sender_device_id,
            sender_public_key,
            files,
            output_dir: resume_state.output_dir,
            _config: config,
            code,
            _session_key: session_key,
            progress_tx,
            progress_rx,
            tls_stream: Some(tls_stream),
            keep_alive_handle: None,
        })
    }

    /// Accept the transfer with resume support.
    ///
    /// This sends a resume request to the sender indicating which chunks
    /// have already been received.
    ///
    /// # Arguments
    ///
    /// * `resume_state` - The saved state with completed chunks information
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn accept_with_resume(&mut self, resume_state: &ResumeState) -> Result<()> {
        self.stop_keep_alive().await?;

        let mut stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream".to_string()))?;

        let ack = FileListAckPayload {
            accepted: true,
            accepted_files: None,
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut stream, MessageType::FileListAck, &ack_payload).await?;

        let resume_request = protocol::ResumeRequestPayload {
            transfer_id: resume_state.transfer_id,
            completed_chunks: resume_state.completed_chunks.clone(),
            completed_file_hashes: resume_state.completed_file_hashes.clone(),
        };
        let resume_payload = protocol::encode_payload(&resume_request)?;
        protocol::write_frame(&mut stream, MessageType::ResumeRequest, &resume_payload).await?;

        let (header, payload) = protocol::read_frame(&mut stream).await?;
        if header.message_type != MessageType::ResumeAck {
            return Err(Error::UnexpectedMessage {
                expected: "ResumeAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let resume_ack: protocol::ResumeAckPayload = protocol::decode_payload(&payload)?;
        if !resume_ack.accepted {
            let reason = resume_ack.reason.unwrap_or_else(|| "unknown".to_string());
            return Err(Error::ResumeRejected(reason));
        }

        tracing::info!("Resume accepted by sender");

        self.update_state(TransferState::Transferring);

        self.do_receive_resumed(&mut stream, resume_state).await?;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    /// Receive files with resume support.
    #[allow(clippy::too_many_lines)]
    async fn do_receive_resumed<S>(&self, stream: &mut S, resume_state: &ResumeState) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let chunk_size = crate::DEFAULT_CHUNK_SIZE;
        let mut current_writer: Option<FileWriter> = None;
        let mut current_file_index: Option<usize> = None;

        loop {
            let (header, payload) = protocol::read_frame(stream).await?;

            match header.message_type {
                MessageType::ChunkStart => {
                    let start: ChunkStartPayload = protocol::decode_payload(&payload)?;

                    if current_file_index != Some(start.file_index) {
                        if let Some(writer) = current_writer.take() {
                            let _sha256 = writer.finalize_with_full_hash().await?;
                        }

                        let file = &self.files[start.file_index];
                        let output_path = self.output_dir.join(&file.relative_path);

                        let completed_chunks = resume_state.completed_chunks.get(&start.file_index);
                        let bytes_completed = completed_chunks
                            .map_or(0, |chunks| chunks.len() as u64 * chunk_size as u64);

                        current_writer = Some(
                            FileWriter::new_resumable(output_path, file.size, bytes_completed)
                                .await?,
                        );
                        current_file_index = Some(start.file_index);

                        {
                            let mut progress = self.progress_rx.borrow().clone();
                            progress.current_file = start.file_index;
                            progress.current_file_name = file.file_name().to_string();
                            progress.file_bytes_transferred = bytes_completed;
                            progress.file_total_bytes = file.size;
                            let _ = self.progress_tx.send(progress);
                        }
                    }
                }
                MessageType::ChunkData => {
                    let chunk_data = protocol::decode_chunk_data(&payload)?;

                    let chunk = FileChunk {
                        file_index: chunk_data.file_index,
                        chunk_index: chunk_data.chunk_index,
                        data: chunk_data.data.clone(),
                        checksum: chunk_data.checksum,
                        is_last: false,
                    };

                    let offset = chunk_data.chunk_index * chunk_size as u64;

                    let success = if let Some(ref mut writer) = current_writer {
                        writer.write_chunk_at(&chunk, offset).await.is_ok()
                    } else {
                        false
                    };

                    let ack = ChunkAckPayload {
                        file_index: chunk_data.file_index,
                        chunk_index: chunk_data.chunk_index,
                        success,
                    };
                    let ack_payload = protocol::encode_payload(&ack)?;
                    protocol::write_frame(stream, MessageType::ChunkAck, &ack_payload).await?;

                    if !success {
                        return Err(Error::ChecksumMismatch {
                            file: self.files[chunk_data.file_index].file_name().to_string(),
                            chunk: chunk_data.chunk_index,
                        });
                    }

                    {
                        let mut progress = self.progress_rx.borrow().clone();
                        progress.file_bytes_transferred += chunk_data.data.len() as u64;
                        progress.total_bytes_transferred += chunk_data.data.len() as u64;
                        let elapsed = progress.started_at.elapsed().as_secs_f64();
                        if elapsed > 0.0 {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            {
                                progress.speed_bps =
                                    (progress.total_bytes_transferred as f64 / elapsed) as u64;
                            }
                            let remaining = progress.total_bytes - progress.total_bytes_transferred;
                            if progress.speed_bps > 0 {
                                progress.eta =
                                    Some(Duration::from_secs(remaining / progress.speed_bps));
                            }
                        }
                        let _ = self.progress_tx.send(progress);
                    }
                }
                MessageType::TransferComplete => {
                    if let Some(writer) = current_writer.take() {
                        let _sha256 = writer.finalize_with_full_hash().await?;
                    }
                    break;
                }
                MessageType::TransferCancel => {
                    return Err(Error::TransferCancelled);
                }
                _ => {
                    return Err(Error::UnexpectedMessage {
                        expected: "ChunkStart, ChunkData, or TransferComplete".to_string(),
                        actual: format!("{:?}", header.message_type),
                    });
                }
            }
        }

        Ok(())
    }

    fn update_state(&self, state: TransferState) {
        let mut progress = self.progress_rx.borrow().clone();
        progress.state = state;
        let _ = self.progress_tx.send(progress);
    }

    /// Returns (sender_device_id, sender_public_key) from the Hello message.
    async fn do_handshake<S>(stream: &mut S) -> Result<(Option<Uuid>, Option<String>)>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::Hello {
            return Err(Error::UnexpectedMessage {
                expected: "Hello".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let hello: HelloPayload = protocol::decode_payload(&payload)?;

        let identity = crypto::DeviceIdentity::load_or_generate()?;
        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );
        let ack = HelloPayload {
            device_name,
            protocol_version: "1.0".to_string(),
            device_id: Some(identity.device_id()),
            public_key: Some(identity.public_key_base64()),
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(stream, MessageType::HelloAck, &ack_payload).await?;

        Ok((hello.device_id, hello.public_key))
    }

    async fn do_code_verification<S>(
        stream: &mut S,
        code: &ShareCode,
        session_key: &[u8; 32],
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hmac = crypto::hmac_sha256(session_key, code.as_str().as_bytes());
        let verify = CodeVerifyPayload {
            code_hmac: hmac.to_vec(),
        };
        let payload = protocol::encode_payload(&verify)?;
        protocol::write_frame(stream, MessageType::CodeVerify, &payload).await?;

        let (header, ack_payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::CodeVerifyAck {
            return Err(Error::UnexpectedMessage {
                expected: "CodeVerifyAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let ack: CodeVerifyAckPayload = protocol::decode_payload(&ack_payload)?;
        if !ack.success {
            return Err(Error::CodeNotFound(code.to_string()));
        }

        Ok(())
    }

    async fn receive_file_list<S>(stream: &mut S) -> Result<Vec<FileMetadata>>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::FileList {
            return Err(Error::UnexpectedMessage {
                expected: "FileList".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let file_list: FileListPayload = protocol::decode_payload(&payload)?;
        Ok(file_list.files)
    }

    async fn do_receive<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut current_writer: Option<FileWriter> = None;
        let mut current_file_index: Option<usize> = None;

        loop {
            let (header, payload) = protocol::read_frame(stream).await?;

            match header.message_type {
                MessageType::ChunkStart => {
                    let start: ChunkStartPayload = protocol::decode_payload(&payload)?;

                    if current_file_index != Some(start.file_index) {
                        if let Some(writer) = current_writer.take() {
                            let _sha256 = writer.finalize().await?;
                        }

                        let file = &self.files[start.file_index];
                        let output_path = self.output_dir.join(&file.relative_path);
                        current_writer = Some(FileWriter::new(output_path, file.size).await?);
                        current_file_index = Some(start.file_index);

                        {
                            let mut progress = self.progress_rx.borrow().clone();
                            progress.current_file = start.file_index;
                            progress.current_file_name = file.file_name().to_string();
                            progress.file_bytes_transferred = 0;
                            progress.file_total_bytes = file.size;
                            let _ = self.progress_tx.send(progress);
                        }
                    }
                }
                MessageType::ChunkData => {
                    let chunk_data = protocol::decode_chunk_data(&payload)?;

                    let chunk = FileChunk {
                        file_index: chunk_data.file_index,
                        chunk_index: chunk_data.chunk_index,
                        data: chunk_data.data.clone(),
                        checksum: chunk_data.checksum,
                        is_last: false,
                    };

                    let success = if let Some(ref mut writer) = current_writer {
                        writer.write_chunk(&chunk).await.is_ok()
                    } else {
                        false
                    };

                    let ack = ChunkAckPayload {
                        file_index: chunk_data.file_index,
                        chunk_index: chunk_data.chunk_index,
                        success,
                    };
                    let ack_payload = protocol::encode_payload(&ack)?;
                    protocol::write_frame(stream, MessageType::ChunkAck, &ack_payload).await?;

                    if !success {
                        return Err(Error::ChecksumMismatch {
                            file: self.files[chunk_data.file_index].file_name().to_string(),
                            chunk: chunk_data.chunk_index,
                        });
                    }

                    {
                        let mut progress = self.progress_rx.borrow().clone();
                        progress.file_bytes_transferred += chunk_data.data.len() as u64;
                        progress.total_bytes_transferred += chunk_data.data.len() as u64;
                        let elapsed = progress.started_at.elapsed().as_secs_f64();
                        if elapsed > 0.0 {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            {
                                progress.speed_bps =
                                    (progress.total_bytes_transferred as f64 / elapsed) as u64;
                            }
                            let remaining = progress.total_bytes - progress.total_bytes_transferred;
                            if progress.speed_bps > 0 {
                                progress.eta =
                                    Some(Duration::from_secs(remaining / progress.speed_bps));
                            }
                        }
                        let _ = self.progress_tx.send(progress);
                    }
                }
                MessageType::TransferComplete => {
                    if let Some(writer) = current_writer.take() {
                        let _sha256 = writer.finalize().await?;
                    }
                    break;
                }
                MessageType::TransferCancel => {
                    return Err(Error::TransferCancelled);
                }
                _ => {
                    return Err(Error::UnexpectedMessage {
                        expected: "ChunkStart, ChunkData, or TransferComplete".to_string(),
                        actual: format!("{:?}", header.message_type),
                    });
                }
            }
        }

        Ok(())
    }
}

/// Resume information for interrupted transfers.
///
/// This struct is persisted to disk as `.localdrop-resume` files to enable
/// resuming transfers across application restarts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResumeState {
    /// Unique transfer ID
    pub transfer_id: uuid::Uuid,
    /// Share code (for reconnection)
    pub code: String,
    /// Files in the transfer
    pub files: Vec<FileMetadata>,
    /// Completed chunks per file (file_index -> chunk_indices)
    pub completed_chunks: std::collections::HashMap<usize, Vec<u64>>,
    /// SHA-256 hashes of completed files for verification
    pub completed_file_hashes: std::collections::HashMap<usize, String>,
    /// Sender device name
    pub sender_device: String,
    /// Sender device ID
    pub sender_device_id: uuid::Uuid,
    /// Output directory for received files
    pub output_dir: PathBuf,
    /// When transfer started
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// When state was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Total bytes received so far
    pub bytes_received: u64,
    /// Total bytes to transfer
    pub total_bytes: u64,
    /// Protocol version used
    pub protocol_version: String,
}

impl ResumeState {
    /// Create a new resume state for a transfer.
    #[must_use]
    pub fn new(
        transfer_id: uuid::Uuid,
        code: &str,
        files: Vec<FileMetadata>,
        sender_device: &str,
        sender_device_id: uuid::Uuid,
        output_dir: PathBuf,
    ) -> Self {
        let total_bytes = files.iter().map(|f| f.size).sum();
        let now = chrono::Utc::now();

        Self {
            transfer_id,
            code: code.to_string(),
            files,
            completed_chunks: std::collections::HashMap::new(),
            completed_file_hashes: std::collections::HashMap::new(),
            sender_device: sender_device.to_string(),
            sender_device_id,
            output_dir,
            started_at: now,
            updated_at: now,
            bytes_received: 0,
            total_bytes,
            protocol_version: format!(
                "{}.{}",
                crate::PROTOCOL_VERSION.0,
                crate::PROTOCOL_VERSION.1
            ),
        }
    }

    /// Mark a chunk as completed.
    pub fn mark_chunk_completed(&mut self, file_index: usize, chunk_index: u64, chunk_size: u64) {
        let chunks = self.completed_chunks.entry(file_index).or_default();
        if !chunks.contains(&chunk_index) {
            chunks.push(chunk_index);
            self.bytes_received += chunk_size;
        }
        self.updated_at = chrono::Utc::now();
    }

    /// Mark a file as completed with its hash.
    pub fn mark_file_completed(&mut self, file_index: usize, sha256_hash: &[u8; 32]) {
        use std::fmt::Write;
        let hash_hex = sha256_hash.iter().fold(String::new(), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        });
        self.completed_file_hashes.insert(file_index, hash_hex);
        self.updated_at = chrono::Utc::now();
    }

    /// Check if a file is fully completed.
    #[must_use]
    pub fn is_file_completed(&self, file_index: usize) -> bool {
        self.completed_file_hashes.contains_key(&file_index)
    }

    /// Check if all files are completed.
    #[must_use]
    pub fn is_transfer_completed(&self) -> bool {
        self.completed_file_hashes.len() == self.files.len()
    }

    /// Get completed chunk indices for a file.
    #[must_use]
    pub fn get_completed_chunks(&self, file_index: usize) -> &[u64] {
        self.completed_chunks
            .get(&file_index)
            .map_or(&[], |v| v.as_slice())
    }

    /// Calculate progress percentage.
    #[must_use]
    pub fn progress_percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            100.0
        } else {
            (self.bytes_received as f64 / self.total_bytes as f64) * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_transfer_progress() {
        let progress = TransferProgress::new(5, 1000);
        assert_eq!(progress.state, TransferState::Preparing);
        assert_eq!(progress.total_files, 5);
        assert_eq!(progress.total_bytes, 1000);
        assert_eq!(progress.percentage(), 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_transfer_progress_percentage() {
        let mut progress = TransferProgress::new(2, 1000);
        progress.total_bytes_transferred = 500;
        assert_eq!(progress.percentage(), 50.0);

        progress.total_bytes_transferred = 1000;
        assert_eq!(progress.percentage(), 100.0);
    }

    #[test]
    fn test_transfer_config_default() {
        let config = TransferConfig::default();
        assert_eq!(config.chunk_size, crate::DEFAULT_CHUNK_SIZE);
        assert!(config.verify_checksums);
        assert_eq!(config.discovery_port, DEFAULT_DISCOVERY_PORT);
    }

    #[tokio::test]
    async fn test_share_session_creation() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "Hello, world!").expect("write file");

        let config = TransferConfig {
            transfer_port: 0,
            discovery_port: 0,
            ..Default::default()
        };

        let session = ShareSession::new(&[test_file], config).await;
        assert!(session.is_ok(), "ShareSession should be created");

        let mut session = session.unwrap();
        assert_eq!(session.files().len(), 1);
        assert_eq!(session.code().as_str().len(), 4);

        session.cancel().await;
    }
}
