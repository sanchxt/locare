//! Clipboard sharing session implementations.
//!
//! This module provides three session types:
//!
//! - `ClipboardShareSession`: One-shot clipboard share (sender)
//! - `ClipboardReceiveSession`: One-shot clipboard receive (receiver)
//! - `ClipboardSyncSession`: Live bidirectional clipboard sync

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::code::{CodeGenerator, ShareCode};
use crate::crypto::{self, TlsConfig};
use crate::discovery::{DiscoveryPacket, HybridBroadcaster, HybridListener};
use crate::error::{Error, Result};
use crate::protocol::{
    self, ClipboardAckPayload, ClipboardChangedPayload, ClipboardContentType, ClipboardMetaPayload,
    CodeVerifyAckPayload, CodeVerifyPayload, HelloPayload, MessageType,
};
use crate::transfer::TransferConfig;

use super::watcher::ClipboardWatcher;
use super::{create_clipboard, ClipboardContent, ClipboardMetadata};

/// Type alias for the TLS stream used by sessions
type ClientTlsStream = tokio_rustls::client::TlsStream<TcpStream>;
type ServerTlsStream = tokio_rustls::server::TlsStream<TcpStream>;

/// One-shot clipboard share session (sender side).
pub struct ClipboardShareSession {
    /// Share code
    code: ShareCode,
    /// Content being shared
    content: ClipboardContent,
    /// Metadata about the content
    metadata: ClipboardMetadata,
    /// Transfer configuration (retained for future features)
    _config: TransferConfig,
    /// Device name
    device_name: String,
    /// Session key for HMAC verification
    session_key: [u8; 32],
    /// TCP listener for incoming connections
    listener: TcpListener,
    /// TLS configuration
    tls_config: TlsConfig,
    /// Hybrid discovery broadcaster
    broadcaster: HybridBroadcaster,
}

impl ClipboardShareSession {
    /// Create a new session with current clipboard content.
    ///
    /// # Errors
    ///
    /// Returns an error if clipboard is empty or session cannot be created.
    pub async fn new(config: TransferConfig) -> Result<Self> {
        let mut clipboard = create_clipboard()?;
        let content = clipboard.read()?.ok_or(Error::ClipboardEmpty)?;

        Self::with_content(content, config).await
    }

    /// Create a new session with specific content.
    ///
    /// # Errors
    ///
    /// Returns an error if session cannot be created.
    pub async fn with_content(content: ClipboardContent, config: TransferConfig) -> Result<Self> {
        let code = CodeGenerator::new().generate()?;

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        let metadata = ClipboardMetadata::from_content(&content, &device_name);

        let session_key = crypto::derive_session_key(code.as_str());

        let tls_config = TlsConfig::server()?;

        let listener = TcpListener::bind(format!("0.0.0.0:{}", config.transfer_port)).await?;
        let local_addr = listener.local_addr()?;

        let broadcaster = HybridBroadcaster::new(config.discovery_port).await?;

        let device_id = uuid::Uuid::new_v4();
        let packet = DiscoveryPacket::new(
            &code,
            &device_name,
            device_id,
            local_addr.port(),
            1, // file_count = 1 for clipboard
            content.size(),
        );

        broadcaster.start(packet, config.broadcast_interval).await?;

        Ok(Self {
            code,
            content,
            metadata,
            _config: config,
            device_name,
            session_key,
            listener,
            tls_config,
            broadcaster,
        })
    }

    /// Get the share code.
    #[must_use]
    pub fn code(&self) -> &ShareCode {
        &self.code
    }

    /// Get content metadata.
    #[must_use]
    pub fn metadata(&self) -> &ClipboardMetadata {
        &self.metadata
    }

    /// Get content being shared.
    #[must_use]
    pub fn content(&self) -> &ClipboardContent {
        &self.content
    }

    /// Get content preview.
    #[must_use]
    pub fn preview(&self) -> String {
        self.content.preview(50)
    }

    /// Wait for a receiver to connect and complete the transfer.
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn wait(self) -> Result<()> {
        let (stream, peer_addr) = self.listener.accept().await?;
        tracing::info!("Connection from {}", peer_addr);

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

        let _receiver_name = self.do_handshake(&mut tls_stream).await?;
        self.do_code_verification(&mut tls_stream).await?;
        self.do_clipboard_transfer(&mut tls_stream).await?;

        self.broadcaster.stop().await;

        Ok(())
    }

    /// Cancel the share session.
    pub async fn cancel(&mut self) {
        self.broadcaster.stop().await;
    }

    async fn do_handshake<S>(&self, stream: &mut S) -> Result<String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello = HelloPayload {
            device_name: self.device_name.clone(),
            protocol_version: "1.0".to_string(),
        };
        let payload = protocol::encode_payload(&hello)?;
        protocol::write_frame(stream, MessageType::Hello, &payload).await?;

        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::HelloAck {
            return Err(Error::UnexpectedMessage {
                expected: "HelloAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let ack: HelloPayload = protocol::decode_payload(&payload)?;
        Ok(ack.device_name)
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

    async fn do_clipboard_transfer<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // Send metadata
        let meta = ClipboardMetaPayload {
            content_type: self.metadata.content_type,
            size: self.metadata.size,
            checksum: self.metadata.checksum,
            timestamp: self.metadata.timestamp.timestamp_millis(),
        };
        let meta_payload = protocol::encode_payload(&meta)?;
        protocol::write_frame(stream, MessageType::ClipboardMeta, &meta_payload).await?;

        // Wait for acceptance (may receive Ping/Pong while waiting)
        loop {
            let (header, payload) = protocol::read_frame(stream).await?;

            match header.message_type {
                MessageType::ClipboardAck => {
                    let ack: ClipboardAckPayload = protocol::decode_payload(&payload)?;
                    if !ack.success {
                        return Err(Error::TransferRejected);
                    }
                    break;
                }
                MessageType::Ping => {
                    protocol::write_frame(stream, MessageType::Pong, &[]).await?;
                }
                _ => {
                    return Err(Error::UnexpectedMessage {
                        expected: "ClipboardAck or Ping".to_string(),
                        actual: format!("{:?}", header.message_type),
                    });
                }
            }
        }

        // Send content data
        let data = self.content.to_bytes();

        // For clipboard, we encode: width(4) | height(4) | data
        let mut payload = Vec::with_capacity(8 + data.len());
        let (width, height) = match &self.content {
            ClipboardContent::Image { width, height, .. } => (*width, *height),
            ClipboardContent::Text(_) => (0, 0),
        };
        payload.extend_from_slice(&width.to_be_bytes());
        payload.extend_from_slice(&height.to_be_bytes());
        payload.extend_from_slice(&data);

        protocol::write_frame(stream, MessageType::ClipboardData, &payload).await?;

        // Wait for final ack
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::ClipboardAck {
            return Err(Error::UnexpectedMessage {
                expected: "ClipboardAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let ack: ClipboardAckPayload = protocol::decode_payload(&payload)?;
        if !ack.success {
            return Err(Error::TransferCancelled);
        }

        // Send transfer complete
        protocol::write_frame(stream, MessageType::TransferComplete, &[]).await?;

        Ok(())
    }
}

/// One-shot clipboard receive session (receiver side).
pub struct ClipboardReceiveSession {
    /// Sender address
    sender_addr: SocketAddr,
    /// Sender device name
    sender_name: String,
    /// Content metadata
    metadata: ClipboardMetadata,
    /// Share code (retained for future features like resumption)
    _code: ShareCode,
    /// TLS stream (stored after connect)
    tls_stream: Option<ClientTlsStream>,
    /// Keep-alive task handle
    keep_alive_handle: Option<KeepAliveHandle>,
}

struct KeepAliveHandle {
    stop_tx: oneshot::Sender<()>,
    task_handle: JoinHandle<Result<ClientTlsStream>>,
}

impl Drop for ClipboardReceiveSession {
    fn drop(&mut self) {
        if let Some(handle) = self.keep_alive_handle.take() {
            handle.task_handle.abort();
        }
    }
}

impl ClipboardReceiveSession {
    /// Connect to a sender using share code.
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails.
    pub async fn connect(code: &str, config: TransferConfig) -> Result<Self> {
        let code = ShareCode::parse(code)?;

        let listener = HybridListener::new(config.discovery_port).await?;
        let discovered = listener.find(&code, config.discovery_timeout).await?;

        if let Err(e) = listener.shutdown() {
            tracing::debug!("Listener shutdown: {e}");
        }

        tracing::info!(
            "Found share from {} at {}",
            discovered.packet.device_name,
            discovered.source
        );

        let transfer_addr =
            SocketAddr::new(discovered.source.ip(), discovered.packet.transfer_port);
        let stream = TcpStream::connect(transfer_addr).await?;

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

        let sender_name = Self::do_handshake(&mut tls_stream).await?;
        Self::do_code_verification(&mut tls_stream, &code, &session_key).await?;
        let metadata = Self::receive_metadata(&mut tls_stream, &sender_name).await?;

        Ok(Self {
            sender_addr: transfer_addr,
            sender_name,
            metadata,
            _code: code,
            tls_stream: Some(tls_stream),
            keep_alive_handle: None,
        })
    }

    /// Get content metadata as optional (for CLI compatibility).
    #[must_use]
    pub fn metadata(&self) -> Option<&ClipboardMetadata> {
        Some(&self.metadata)
    }

    /// Get sender info as (address, name) tuple.
    #[must_use]
    pub fn sender(&self) -> (SocketAddr, String) {
        (self.sender_addr, self.sender_name.clone())
    }

    /// Get sender device name.
    #[must_use]
    pub fn sender_name(&self) -> &str {
        &self.sender_name
    }

    /// Get sender address.
    #[must_use]
    pub fn sender_addr(&self) -> SocketAddr {
        self.sender_addr
    }

    /// Start keep-alive task.
    ///
    /// # Errors
    ///
    /// Returns an error if stream is not available.
    pub fn start_keep_alive(&mut self) -> Result<()> {
        if self.keep_alive_handle.is_some() {
            return Ok(());
        }

        let stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream".to_string()))?;

        let (stop_tx, stop_rx) = oneshot::channel();

        let task_handle = tokio::spawn(async move { Self::keep_alive_task(stream, stop_rx).await });

        self.keep_alive_handle = Some(KeepAliveHandle {
            stop_tx,
            task_handle,
        });

        Ok(())
    }

    async fn stop_keep_alive(&mut self) -> Result<()> {
        if let Some(handle) = self.keep_alive_handle.take() {
            let _ = handle.stop_tx.send(());

            match handle.task_handle.await {
                Ok(Ok(stream)) => {
                    self.tls_stream = Some(stream);
                    Ok(())
                }
                Ok(Err(e)) => Err(e),
                Err(e) => Err(Error::Internal(format!("keep-alive task panicked: {e}"))),
            }
        } else {
            Ok(())
        }
    }

    async fn keep_alive_task(
        mut stream: ClientTlsStream,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> Result<ClientTlsStream> {
        let interval = Duration::from_secs(5);
        let timeout_duration = Duration::from_secs(10);

        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    return Ok(stream);
                }
                () = tokio::time::sleep(interval) => {
                    protocol::write_frame(&mut stream, MessageType::Ping, &[]).await?;

                    match tokio::time::timeout(timeout_duration, protocol::read_frame(&mut stream)).await {
                        Ok(Ok((header, _))) if header.message_type == MessageType::Pong => {}
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => return Err(Error::KeepAliveFailed(timeout_duration.as_secs())),
                    }
                }
            }
        }
    }

    /// Accept and receive content.
    ///
    /// # Errors
    ///
    /// Returns an error if transfer fails.
    pub async fn accept(&mut self) -> Result<ClipboardContent> {
        self.stop_keep_alive().await?;

        let mut stream = self
            .tls_stream
            .take()
            .ok_or_else(|| Error::Internal("no TLS stream".to_string()))?;

        // Send acceptance
        let ack = ClipboardAckPayload {
            success: true,
            error: None,
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut stream, MessageType::ClipboardAck, &ack_payload).await?;

        // Receive content
        let (header, data) = protocol::read_frame(&mut stream).await?;
        if header.message_type != MessageType::ClipboardData {
            return Err(Error::UnexpectedMessage {
                expected: "ClipboardData".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        // Parse: width(4) | height(4) | data
        if data.len() < 8 {
            return Err(Error::ProtocolError("clipboard data too short".to_string()));
        }

        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let content_data = &data[8..];

        let content = ClipboardContent::from_bytes(
            self.metadata.content_type,
            content_data,
            Some(width),
            Some(height),
        )?;

        // Verify checksum
        if content.hash() != self.metadata.checksum {
            let ack = ClipboardAckPayload {
                success: false,
                error: Some("Checksum mismatch".to_string()),
            };
            let ack_payload = protocol::encode_payload(&ack)?;
            protocol::write_frame(&mut stream, MessageType::ClipboardAck, &ack_payload).await?;
            return Err(Error::ChecksumMismatch {
                file: "clipboard".to_string(),
                chunk: 0,
            });
        }

        // Send success ack
        let ack = ClipboardAckPayload {
            success: true,
            error: None,
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut stream, MessageType::ClipboardAck, &ack_payload).await?;

        // Wait for transfer complete
        let (header, _) = protocol::read_frame(&mut stream).await?;
        if header.message_type != MessageType::TransferComplete {
            return Err(Error::UnexpectedMessage {
                expected: "TransferComplete".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        Ok(content)
    }

    /// Accept and copy directly to clipboard.
    ///
    /// # Errors
    ///
    /// Returns an error if transfer fails.
    pub async fn accept_to_clipboard(&mut self) -> Result<()> {
        let content = self.accept().await?;
        let mut clipboard = create_clipboard()?;
        clipboard.write(&content)?;
        Ok(())
    }

    /// Decline the transfer.
    pub async fn decline(&mut self) {
        let _ = self.stop_keep_alive().await;

        if let Some(mut stream) = self.tls_stream.take() {
            let ack = ClipboardAckPayload {
                success: false,
                error: Some("Declined by user".to_string()),
            };
            if let Ok(ack_payload) = protocol::encode_payload(&ack) {
                let _ = protocol::write_frame(&mut stream, MessageType::ClipboardAck, &ack_payload)
                    .await;
            }
            let _ = stream.shutdown().await;
        }
    }

    async fn do_handshake<S>(stream: &mut S) -> Result<String>
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

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );
        let ack = HelloPayload {
            device_name,
            protocol_version: "1.0".to_string(),
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(stream, MessageType::HelloAck, &ack_payload).await?;

        Ok(hello.device_name)
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

    async fn receive_metadata<S>(stream: &mut S, sender_name: &str) -> Result<ClipboardMetadata>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::ClipboardMeta {
            return Err(Error::UnexpectedMessage {
                expected: "ClipboardMeta".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let meta: ClipboardMetaPayload = protocol::decode_payload(&payload)?;

        Ok(ClipboardMetadata {
            content_type: meta.content_type,
            size: meta.size,
            checksum: meta.checksum,
            timestamp: chrono::DateTime::from_timestamp_millis(meta.timestamp)
                .unwrap_or_else(chrono::Utc::now),
            source_device: sender_name.to_string(),
            width: None,
            height: None,
        })
    }
}

/// Statistics from a sync session.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    /// Session duration
    pub duration: Duration,
    /// Number of items sent
    pub items_sent: u64,
    /// Number of items received
    pub items_received: u64,
    /// Bytes sent
    pub bytes_sent: u64,
    /// Bytes received
    pub bytes_received: u64,
}

/// Live bidirectional clipboard sync session.
pub struct ClipboardSyncSession {
    /// Peer device name
    peer_name: String,
    /// Peer address
    peer_addr: SocketAddr,
    /// Local device name (retained for future features)
    _device_name: String,
    /// Last local content hash
    last_local_hash: Arc<AtomicU64>,
    /// Last remote content hash
    last_remote_hash: Arc<AtomicU64>,
    /// Sync statistics
    stats: SyncStats,
    /// Session start time
    started_at: Instant,
    /// Shutdown signal sender
    shutdown_tx: broadcast::Sender<()>,
}

impl ClipboardSyncSession {
    /// Start as sync host (generates code, waits for connection).
    ///
    /// # Errors
    ///
    /// Returns an error if session cannot be created.
    pub async fn host(config: TransferConfig) -> Result<(ShareCode, Self, SyncSessionRunner)> {
        let code = CodeGenerator::new().generate()?;

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        let session_key = crypto::derive_session_key(code.as_str());

        let tls_config = TlsConfig::server()?;

        let listener = TcpListener::bind(format!("0.0.0.0:{}", config.transfer_port)).await?;
        let local_addr = listener.local_addr()?;

        let broadcaster = HybridBroadcaster::new(config.discovery_port).await?;

        let device_id = uuid::Uuid::new_v4();
        let packet = DiscoveryPacket::new(
            &code,
            &device_name,
            device_id,
            local_addr.port(),
            0, // No files for sync
            0,
        );

        broadcaster.start(packet, config.broadcast_interval).await?;

        // Wait for connection
        let (stream, peer_addr) = listener.accept().await?;
        broadcaster.stop().await;

        let acceptor = TlsAcceptor::from(Arc::new(
            tls_config
                .server_config()
                .ok_or_else(|| Error::TlsError("no server config".to_string()))?
                .clone(),
        ));

        let mut tls_stream = acceptor
            .accept(stream)
            .await
            .map_err(|e| Error::TlsError(format!("TLS handshake failed: {e}")))?;

        // Handshake
        let hello = HelloPayload {
            device_name: device_name.clone(),
            protocol_version: "1.0".to_string(),
        };
        let payload = protocol::encode_payload(&hello)?;
        protocol::write_frame(&mut tls_stream, MessageType::Hello, &payload).await?;

        let (header, payload) = protocol::read_frame(&mut tls_stream).await?;
        if header.message_type != MessageType::HelloAck {
            return Err(Error::UnexpectedMessage {
                expected: "HelloAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }
        let peer_hello: HelloPayload = protocol::decode_payload(&payload)?;

        // Code verification
        let (header, payload) = protocol::read_frame(&mut tls_stream).await?;
        if header.message_type != MessageType::CodeVerify {
            return Err(Error::UnexpectedMessage {
                expected: "CodeVerify".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let verify: CodeVerifyPayload = protocol::decode_payload(&payload)?;
        let expected_hmac = crypto::hmac_sha256(&session_key, code.as_str().as_bytes());
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
        protocol::write_frame(&mut tls_stream, MessageType::CodeVerifyAck, &ack_payload).await?;

        if !success {
            return Err(Error::CodeNotFound(code.to_string()));
        }

        let (shutdown_tx, _) = broadcast::channel(1);

        let session = Self {
            peer_name: peer_hello.device_name,
            peer_addr,
            _device_name: device_name,
            last_local_hash: Arc::new(AtomicU64::new(0)),
            last_remote_hash: Arc::new(AtomicU64::new(0)),
            stats: SyncStats::default(),
            started_at: Instant::now(),
            shutdown_tx: shutdown_tx.clone(),
        };

        let runner = SyncSessionRunner {
            tls_stream: TlsStreamKind::Server(tls_stream),
            last_local_hash: Arc::clone(&session.last_local_hash),
            last_remote_hash: Arc::clone(&session.last_remote_hash),
            shutdown_rx: shutdown_tx.subscribe(),
        };

        Ok((code, session, runner))
    }

    /// Connect to a sync host using code.
    ///
    /// # Errors
    ///
    /// Returns an error if connection fails.
    pub async fn connect(code: &str, config: TransferConfig) -> Result<(Self, SyncSessionRunner)> {
        let code = ShareCode::parse(code)?;

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        let listener = HybridListener::new(config.discovery_port).await?;
        let discovered = listener.find(&code, config.discovery_timeout).await?;

        if let Err(e) = listener.shutdown() {
            tracing::debug!("Listener shutdown: {e}");
        }

        let transfer_addr =
            SocketAddr::new(discovered.source.ip(), discovered.packet.transfer_port);
        let stream = TcpStream::connect(transfer_addr).await?;

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

        // Handshake
        let (header, payload) = protocol::read_frame(&mut tls_stream).await?;
        if header.message_type != MessageType::Hello {
            return Err(Error::UnexpectedMessage {
                expected: "Hello".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }
        let peer_hello: HelloPayload = protocol::decode_payload(&payload)?;

        let ack = HelloPayload {
            device_name: device_name.clone(),
            protocol_version: "1.0".to_string(),
        };
        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(&mut tls_stream, MessageType::HelloAck, &ack_payload).await?;

        // Code verification
        let hmac = crypto::hmac_sha256(&session_key, code.as_str().as_bytes());
        let verify = CodeVerifyPayload {
            code_hmac: hmac.to_vec(),
        };
        let payload = protocol::encode_payload(&verify)?;
        protocol::write_frame(&mut tls_stream, MessageType::CodeVerify, &payload).await?;

        let (header, ack_payload) = protocol::read_frame(&mut tls_stream).await?;
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

        let (shutdown_tx, _) = broadcast::channel(1);

        let session = Self {
            peer_name: peer_hello.device_name,
            peer_addr: transfer_addr,
            _device_name: device_name,
            last_local_hash: Arc::new(AtomicU64::new(0)),
            last_remote_hash: Arc::new(AtomicU64::new(0)),
            stats: SyncStats::default(),
            started_at: Instant::now(),
            shutdown_tx: shutdown_tx.clone(),
        };

        let runner = SyncSessionRunner {
            tls_stream: TlsStreamKind::Client(tls_stream),
            last_local_hash: Arc::clone(&session.last_local_hash),
            last_remote_hash: Arc::clone(&session.last_remote_hash),
            shutdown_rx: shutdown_tx.subscribe(),
        };

        Ok((session, runner))
    }

    /// Get peer device name.
    #[must_use]
    pub fn peer_name(&self) -> &str {
        &self.peer_name
    }

    /// Get peer address.
    #[must_use]
    pub fn peer_addr(&self) -> &SocketAddr {
        &self.peer_addr
    }

    /// Get sync statistics.
    #[must_use]
    pub fn stats(&self) -> SyncStats {
        let mut stats = self.stats.clone();
        stats.duration = self.started_at.elapsed();
        stats
    }

    /// Signal shutdown.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Wrapper enum to handle both client and server TLS streams.
enum TlsStreamKind {
    Client(ClientTlsStream),
    Server(ServerTlsStream),
}

impl AsyncRead for TlsStreamKind {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Client(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Self::Server(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TlsStreamKind {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Client(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Self::Server(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Client(s) => std::pin::Pin::new(s).poll_flush(cx),
            Self::Server(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Client(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Self::Server(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Runner for the sync session that handles the actual sync loop.
pub struct SyncSessionRunner {
    tls_stream: TlsStreamKind,
    last_local_hash: Arc<AtomicU64>,
    last_remote_hash: Arc<AtomicU64>,
    shutdown_rx: broadcast::Receiver<()>,
}

impl SyncSessionRunner {
    /// Run the sync session.
    ///
    /// This runs until shutdown is signaled or an error occurs.
    ///
    /// # Returns
    ///
    /// Returns sync statistics and a channel for receiving sync events.
    ///
    /// # Errors
    ///
    /// Returns an error if sync fails.
    #[allow(clippy::too_many_lines)]
    pub async fn run(self) -> Result<(SyncStats, mpsc::Receiver<SyncEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(32);

        let mut stats = SyncStats::default();
        let started_at = Instant::now();

        // Create clipboard watcher
        let watcher = ClipboardWatcher::new();
        let clipboard = create_clipboard()?;
        let (mut change_rx, watcher_handle) = watcher.start(clipboard);

        // Take ownership of the stream and wrap in Arc<Mutex> for sharing
        let tls_stream = Arc::new(tokio::sync::Mutex::new(self.tls_stream));

        let last_local_hash = self.last_local_hash;
        let last_remote_hash = self.last_remote_hash;
        let mut shutdown_rx = self.shutdown_rx;
        let tls_stream_clone = Arc::clone(&tls_stream);
        let event_tx_clone = event_tx.clone();

        // Task to handle outbound clipboard changes
        let outbound_task = {
            let last_remote = Arc::clone(&last_remote_hash);
            let last_local = Arc::clone(&last_local_hash);
            let stream = Arc::clone(&tls_stream);
            tokio::spawn(async move {
                while let Some(change) = change_rx.recv().await {
                    // Skip if this is content we just received
                    if change.hash == last_remote.load(Ordering::SeqCst) {
                        continue;
                    }

                    last_local.store(change.hash, Ordering::SeqCst);

                    // Send ClipboardChanged notification
                    let notification = ClipboardChangedPayload {
                        content_type: change.content.content_type(),
                        size: change.content.size(),
                        checksum: change.hash,
                        timestamp: change.timestamp.timestamp_millis(),
                    };

                    let payload = protocol::encode_payload(&notification)?;
                    protocol::write_frame(
                        &mut *stream.lock().await,
                        MessageType::ClipboardChanged,
                        &payload,
                    )
                    .await?;

                    let _ = event_tx_clone
                        .send(SyncEvent::Sent {
                            content_type: change.content.content_type(),
                            size: change.content.size(),
                        })
                        .await;
                }
                Ok::<_, Error>(())
            })
        };

        // Task to handle inbound messages
        let inbound_task = {
            let last_remote = Arc::clone(&last_remote_hash);
            let stream = Arc::clone(&tls_stream_clone);
            tokio::spawn(async move {
                let mut clipboard = create_clipboard()?;

                loop {
                    let (header, payload) = {
                        let mut reader = stream.lock().await;
                        protocol::read_frame(&mut *reader).await?
                    };

                    match header.message_type {
                        MessageType::ClipboardChanged => {
                            let changed: ClipboardChangedPayload =
                                protocol::decode_payload(&payload)?;

                            // Request the content
                            protocol::write_frame(
                                &mut *stream.lock().await,
                                MessageType::ClipboardRequest,
                                &[],
                            )
                            .await?;

                            // Wait for content
                            let (header, data) = {
                                let mut reader = stream.lock().await;
                                protocol::read_frame(&mut *reader).await?
                            };
                            if header.message_type != MessageType::ClipboardData {
                                continue;
                            }

                            // Parse content
                            if data.len() < 8 {
                                continue;
                            }

                            let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                            let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                            let content_data = &data[8..];

                            if let Ok(content) = ClipboardContent::from_bytes(
                                changed.content_type,
                                content_data,
                                Some(width),
                                Some(height),
                            ) {
                                // Update hash before writing to prevent feedback
                                last_remote.store(changed.checksum, Ordering::SeqCst);

                                // Write to local clipboard
                                if clipboard.write(&content).is_ok() {
                                    let _ = event_tx
                                        .send(SyncEvent::Received {
                                            content_type: changed.content_type,
                                            size: changed.size,
                                        })
                                        .await;
                                }
                            }

                            // Send ack
                            let ack = ClipboardAckPayload {
                                success: true,
                                error: None,
                            };
                            let ack_payload = protocol::encode_payload(&ack)?;
                            protocol::write_frame(
                                &mut *stream.lock().await,
                                MessageType::ClipboardAck,
                                &ack_payload,
                            )
                            .await?;
                        }
                        MessageType::ClipboardRequest => {
                            // Peer is requesting our current content
                            if let Ok(Some(content)) = clipboard.read() {
                                let data = content.to_bytes();
                                let (width, height) = match &content {
                                    ClipboardContent::Image { width, height, .. } => {
                                        (*width, *height)
                                    }
                                    ClipboardContent::Text(_) => (0, 0),
                                };

                                let mut response = Vec::with_capacity(8 + data.len());
                                response.extend_from_slice(&width.to_be_bytes());
                                response.extend_from_slice(&height.to_be_bytes());
                                response.extend_from_slice(&data);

                                protocol::write_frame(
                                    &mut *stream.lock().await,
                                    MessageType::ClipboardData,
                                    &response,
                                )
                                .await?;
                            }
                        }
                        MessageType::Ping => {
                            protocol::write_frame(
                                &mut *stream.lock().await,
                                MessageType::Pong,
                                &[],
                            )
                            .await?;
                        }
                        MessageType::TransferCancel => {
                            break;
                        }
                        // ClipboardAck, Pong, and other messages are handled silently
                        _ => {}
                    }
                }

                Ok::<_, Error>(())
            })
        };

        // Wait for shutdown or error
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::debug!("Sync session shutdown requested");
            }
            result = outbound_task => {
                if let Err(e) = result {
                    tracing::warn!("Outbound task error: {}", e);
                }
            }
            result = inbound_task => {
                if let Err(e) = result {
                    tracing::warn!("Inbound task error: {}", e);
                }
            }
        }

        watcher_handle.stop().await;

        stats.duration = started_at.elapsed();
        Ok((stats, event_rx))
    }
}

/// Events during sync session.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    /// Content was sent to peer
    Sent {
        /// Content type
        content_type: ClipboardContentType,
        /// Size in bytes
        size: u64,
    },
    /// Content was received from peer
    Received {
        /// Content type
        content_type: ClipboardContentType,
        /// Size in bytes
        size: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_stats_default() {
        let stats = SyncStats::default();
        assert_eq!(stats.items_sent, 0);
        assert_eq!(stats.items_received, 0);
    }
}
