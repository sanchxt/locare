//! Trusted device transfer sessions.
//!
//! This module provides transfer sessions for trusted devices that don't require
//! share codes. Authentication is done via Ed25519 signatures instead.
//!
//! ## Flow
//!
//! ### Sender (TrustedSendSession)
//! 1. Look up target device in trust store
//! 2. Broadcast beacon with `looking_for` set to target device_id
//! 3. Wait for target device to appear on network
//! 4. Connect and perform trusted handshake
//! 5. Transfer files
//!
//! ### Receiver (TrustedReceiveSession)
//! 1. Broadcast availability beacon
//! 2. Wait for trusted device to connect
//! 3. Verify sender's signature against trust store
//! 4. Receive files

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::prelude::*;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

use crate::config::TrustLevel;
use crate::crypto::{self, DeviceIdentity, TlsConfig};
use crate::discovery::{BeaconBroadcaster, BeaconListener, DeviceBeacon, DiscoveredDevice};
use crate::error::{Error, Result};
use crate::file::{
    enumerate_files, EnumerateOptions, FileChunk, FileChunker, FileMetadata, FileWriter,
};
use crate::protocol::{
    self, ChunkAckPayload, ChunkDataPayload, ChunkStartPayload, FileListAckPayload,
    FileListPayload, MessageType, TrustedHelloAckPayload, TrustedHelloPayload,
};
use crate::trust::{TrustStore, TrustedDevice};

use super::{TransferConfig, TransferProgress, TransferState};

/// Configure TCP keep-alive on a socket.
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

/// A trusted send session (sender initiates to trusted device).
pub struct TrustedSendSession {
    /// Target device from trust store
    target_device: TrustedDevice,
    /// Our device identity
    identity: DeviceIdentity,
    /// Files being shared
    files: Vec<FileMetadata>,
    /// File paths (for reading)
    file_paths: Vec<PathBuf>,
    /// Transfer configuration
    config: TransferConfig,
    /// Device name
    device_name: String,
    /// Progress sender
    progress_tx: watch::Sender<TransferProgress>,
    /// Progress receiver
    progress_rx: watch::Receiver<TransferProgress>,
    /// Discovered target device
    discovered_target: Option<DiscoveredDevice>,
}

impl std::fmt::Debug for TrustedSendSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedSendSession")
            .field("target_device", &self.target_device.device_name)
            .field("files", &self.files)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl TrustedSendSession {
    /// Create a new trusted send session.
    ///
    /// # Arguments
    ///
    /// * `target_device` - The trusted device to send to
    /// * `identity` - Our device identity for signing
    /// * `paths` - Paths to share
    /// * `config` - Transfer configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be created.
    pub async fn new(
        target_device: TrustedDevice,
        identity: DeviceIdentity,
        paths: &[PathBuf],
        config: TransferConfig,
    ) -> Result<Self> {
        let options = EnumerateOptions::default();
        let files = enumerate_files(paths, &options)?;

        if files.is_empty() {
            return Err(Error::FileNotFound("no files to share".to_string()));
        }

        let file_paths: Vec<PathBuf> = paths.to_vec();

        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        let progress = TransferProgress::new(files.len(), total_bytes);
        let (progress_tx, progress_rx) = watch::channel(progress);

        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        Ok(Self {
            target_device,
            identity,
            files,
            file_paths,
            config,
            device_name,
            progress_tx,
            progress_rx,
            discovered_target: None,
        })
    }

    /// Get the target device.
    #[must_use]
    pub fn target(&self) -> &TrustedDevice {
        &self.target_device
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

    /// Discover the target device on the network.
    ///
    /// Broadcasts a beacon looking for the target device and waits for it to respond.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not found within the discovery timeout.
    pub async fn discover(&mut self) -> Result<&DiscoveredDevice> {
        self.update_state(TransferState::Waiting);

        let broadcaster = BeaconBroadcaster::new(self.config.discovery_port).await?;
        let listener = BeaconListener::new(self.config.discovery_port).await?;

        let beacon = DeviceBeacon::new(
            self.identity.device_id(),
            &self.device_name,
            &self.identity.public_key_base64(),
            self.config.transfer_port,
        )
        .looking_for(self.target_device.device_id);

        broadcaster
            .start(beacon, self.config.broadcast_interval)
            .await?;

        let discovered = listener
            .find_device(self.target_device.device_id, self.config.discovery_timeout)
            .await;

        broadcaster.stop().await;

        match discovered {
            Ok(device) => {
                tracing::info!(
                    "Found target device {} at {}",
                    device.beacon.device_name,
                    device.source
                );
                self.discovered_target = Some(device);
                Ok(self.discovered_target.as_ref().unwrap())
            }
            Err(e) => {
                self.update_state(TransferState::Failed);
                Err(e)
            }
        }
    }

    /// Connect to the discovered target and perform the transfer.
    ///
    /// Call `discover()` first to find the target device.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or transfer fails.
    pub async fn send(&mut self) -> Result<()> {
        let target = self
            .discovered_target
            .as_ref()
            .ok_or_else(|| Error::Internal("call discover() first".to_string()))?;

        let transfer_addr = target.transfer_addr();
        tracing::info!(
            "Connecting to {} at {}",
            target.beacon.device_name,
            transfer_addr
        );

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

        self.update_state(TransferState::Connected);

        self.do_trusted_handshake(&mut tls_stream).await?;

        let accepted = self.do_file_list_exchange(&mut tls_stream).await?;
        if !accepted {
            self.update_state(TransferState::Cancelled);
            return Err(Error::TransferRejected);
        }

        self.update_state(TransferState::Transferring);

        self.do_transfer(&mut tls_stream).await?;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    fn update_state(&self, state: TransferState) {
        let mut progress = self.progress_rx.borrow().clone();
        progress.state = state;
        let _ = self.progress_tx.send(progress);
    }

    async fn do_trusted_handshake<S>(&self, stream: &mut S) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let nonce: [u8; 32] = crypto::random_bytes();
        let nonce_base64 = BASE64_STANDARD.encode(nonce);

        let nonce_signature = self.identity.sign(&nonce);
        let nonce_signature_base64 = BASE64_STANDARD.encode(nonce_signature);

        let hello = TrustedHelloPayload {
            device_name: self.device_name.clone(),
            protocol_version: "1.0".to_string(),
            device_id: self.identity.device_id(),
            public_key: self.identity.public_key_base64(),
            nonce: nonce_base64,
            nonce_signature: nonce_signature_base64,
        };

        let payload = protocol::encode_payload(&hello)?;
        protocol::write_frame(stream, MessageType::TrustedHello, &payload).await?;

        let (header, ack_payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::TrustedHelloAck {
            return Err(Error::UnexpectedMessage {
                expected: "TrustedHelloAck".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let ack: TrustedHelloAckPayload = protocol::decode_payload(&ack_payload)?;

        if !ack.trusted {
            let error = ack.error.unwrap_or_else(|| "not trusted".to_string());
            return Err(Error::DeviceNotTrusted(error));
        }

        if let (Some(pub_key), Some(sig)) = (&ack.public_key, &ack.nonce_signature) {
            let sig_bytes = BASE64_STANDARD
                .decode(sig)
                .map_err(|e| Error::ProtocolError(format!("invalid signature: {e}")))?;

            let sig_array: [u8; 64] = sig_bytes
                .try_into()
                .map_err(|_| Error::ProtocolError("invalid signature length".to_string()))?;

            if !DeviceIdentity::verify_base64(pub_key, &nonce, &sig_array) {
                return Err(Error::SignatureInvalid);
            }

            tracing::debug!("Receiver signature verified successfully");
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

    fn find_file_path(&self, relative_path: &std::path::Path) -> Result<PathBuf> {
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

/// A trusted receive session (receiver waits for trusted senders).
pub struct TrustedReceiveSession {
    /// Our device identity
    identity: DeviceIdentity,
    /// Trust store for verifying senders
    trust_store: TrustStore,
    /// Output directory
    output_dir: PathBuf,
    /// Transfer configuration
    config: TransferConfig,
    /// Device name
    device_name: String,
    /// TCP listener
    listener: TcpListener,
    /// TLS config
    tls_config: TlsConfig,
    /// Beacon broadcaster
    broadcaster: BeaconBroadcaster,
    /// Progress sender
    progress_tx: watch::Sender<TransferProgress>,
    /// Progress receiver
    progress_rx: watch::Receiver<TransferProgress>,
    /// Connected sender info
    sender_info: Option<SenderInfo>,
    /// Files being received
    files: Vec<FileMetadata>,
    /// TLS stream (set after connection)
    tls_stream: Option<tokio_rustls::server::TlsStream<TcpStream>>,
}

/// Information about the connected sender.
#[derive(Debug, Clone)]
pub struct SenderInfo {
    /// Sender's device ID
    pub device_id: Uuid,
    /// Sender's device name
    pub device_name: String,
    /// Sender's public key
    pub public_key: String,
    /// Trust level in our store
    pub trust_level: TrustLevel,
    /// Sender's address
    pub address: SocketAddr,
}

impl std::fmt::Debug for TrustedReceiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustedReceiveSession")
            .field("output_dir", &self.output_dir)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl TrustedReceiveSession {
    /// Create a new trusted receive session.
    ///
    /// # Arguments
    ///
    /// * `identity` - Our device identity
    /// * `trust_store` - Trust store for verifying senders
    /// * `output_dir` - Directory to save received files
    /// * `config` - Transfer configuration
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be created.
    pub async fn new(
        identity: DeviceIdentity,
        trust_store: TrustStore,
        output_dir: PathBuf,
        config: TransferConfig,
    ) -> Result<Self> {
        let device_name = hostname::get().map_or_else(
            |_| "Unknown".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        let tls_config = TlsConfig::server()?;

        let listener = TcpListener::bind(format!("0.0.0.0:{}", config.transfer_port)).await?;
        let local_addr = listener.local_addr()?;

        let broadcaster = BeaconBroadcaster::new(config.discovery_port).await?;

        let beacon = DeviceBeacon::new(
            identity.device_id(),
            &device_name,
            &identity.public_key_base64(),
            local_addr.port(),
        )
        .ready_to_receive(true);

        broadcaster.start(beacon, config.broadcast_interval).await?;

        let progress = TransferProgress::new(0, 0);
        let (progress_tx, progress_rx) = watch::channel(progress);

        Ok(Self {
            identity,
            trust_store,
            output_dir,
            config,
            device_name,
            listener,
            tls_config,
            broadcaster,
            progress_tx,
            progress_rx,
            sender_info: None,
            files: Vec::new(),
            tls_stream: None,
        })
    }

    /// Get our device ID.
    #[must_use]
    pub fn device_id(&self) -> Uuid {
        self.identity.device_id()
    }

    /// Get a progress receiver.
    #[must_use]
    pub fn progress(&self) -> watch::Receiver<TransferProgress> {
        self.progress_rx.clone()
    }

    /// Wait for a trusted sender to connect.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails or sender is not trusted.
    pub async fn wait_for_sender(&mut self) -> Result<&SenderInfo> {
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

        let sender_info = self
            .do_trusted_handshake(&mut tls_stream, peer_addr)
            .await?;
        self.sender_info = Some(sender_info);

        self.files = self.receive_file_list(&mut tls_stream).await?;

        let total_bytes: u64 = self.files.iter().map(|f| f.size).sum();
        let mut progress = TransferProgress::new(self.files.len(), total_bytes);
        progress.state = TransferState::Connected;
        let _ = self.progress_tx.send(progress);

        self.tls_stream = Some(tls_stream);

        Ok(self.sender_info.as_ref().unwrap())
    }

    /// Get the sender info.
    #[must_use]
    pub fn sender(&self) -> Option<&SenderInfo> {
        self.sender_info.as_ref()
    }

    /// Get the files being received.
    #[must_use]
    pub fn files(&self) -> &[FileMetadata] {
        &self.files
    }

    /// Accept the transfer and receive files.
    ///
    /// # Errors
    ///
    /// Returns an error if the transfer fails.
    pub async fn accept(&mut self) -> Result<()> {
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

        self.broadcaster.stop().await;

        self.update_state(TransferState::Completed);

        Ok(())
    }

    /// Decline the transfer.
    pub async fn decline(&mut self) {
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
        self.broadcaster.stop().await;
        self.update_state(TransferState::Cancelled);
    }

    /// Cancel the session.
    pub async fn cancel(&mut self) {
        self.broadcaster.stop().await;
        self.update_state(TransferState::Cancelled);
    }

    fn update_state(&self, state: TransferState) {
        let mut progress = self.progress_rx.borrow().clone();
        progress.state = state;
        let _ = self.progress_tx.send(progress);
    }

    async fn do_trusted_handshake<S>(
        &self,
        stream: &mut S,
        peer_addr: SocketAddr,
    ) -> Result<SenderInfo>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (header, payload) = protocol::read_frame(stream).await?;
        if header.message_type != MessageType::TrustedHello {
            return Err(Error::UnexpectedMessage {
                expected: "TrustedHello".to_string(),
                actual: format!("{:?}", header.message_type),
            });
        }

        let hello: TrustedHelloPayload = protocol::decode_payload(&payload)?;

        let nonce = BASE64_STANDARD
            .decode(&hello.nonce)
            .map_err(|e| Error::ProtocolError(format!("invalid nonce: {e}")))?;

        let signature_bytes = BASE64_STANDARD
            .decode(&hello.nonce_signature)
            .map_err(|e| Error::ProtocolError(format!("invalid signature: {e}")))?;

        let signature: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| Error::ProtocolError("invalid signature length".to_string()))?;

        if !DeviceIdentity::verify_base64(&hello.public_key, &nonce, &signature) {
            return Err(Error::SignatureInvalid);
        }

        tracing::debug!("Sender signature verified");

        let trusted_device = self
            .trust_store
            .find_by_id(&hello.device_id)
            .ok_or_else(|| Error::DeviceNotTrusted(hello.device_name.clone()))?;

        if trusted_device.public_key != hello.public_key {
            return Err(Error::DeviceNotTrusted(format!(
                "public key mismatch for {}",
                hello.device_name
            )));
        }

        tracing::info!(
            "Verified trusted sender: {} (trust level: {:?})",
            hello.device_name,
            trusted_device.trust_level
        );

        let our_signature = self.identity.sign(&nonce);
        let our_signature_base64 = BASE64_STANDARD.encode(our_signature);

        let ack = TrustedHelloAckPayload {
            trusted: true,
            device_name: Some(self.device_name.clone()),
            device_id: Some(self.identity.device_id()),
            public_key: Some(self.identity.public_key_base64()),
            nonce_signature: Some(our_signature_base64),
            error: None,
            trust_level: Some(format!("{:?}", trusted_device.trust_level)),
        };

        let ack_payload = protocol::encode_payload(&ack)?;
        protocol::write_frame(stream, MessageType::TrustedHelloAck, &ack_payload).await?;

        Ok(SenderInfo {
            device_id: hello.device_id,
            device_name: hello.device_name,
            public_key: hello.public_key,
            trust_level: trusted_device.trust_level,
            address: peer_addr,
        })
    }

    async fn receive_file_list<S>(&self, stream: &mut S) -> Result<Vec<FileMetadata>>
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sender_info() {
        let info = SenderInfo {
            device_id: Uuid::new_v4(),
            device_name: "Test Device".to_string(),
            public_key: "public_key".to_string(),
            trust_level: TrustLevel::Full,
            address: "127.0.0.1:52530".parse().unwrap(),
        };

        assert_eq!(info.device_name, "Test Device");
        assert_eq!(info.trust_level, TrustLevel::Full);
    }
}
