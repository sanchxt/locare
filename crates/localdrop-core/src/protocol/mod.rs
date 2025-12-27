//! LDRP (LocalDrop Protocol) wire protocol implementation.
//!
//! LocalDrop uses a custom lightweight binary protocol over TLS 1.3.
//!
//! ## Frame Format
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │                      LDRP Frame                            │
//! ├────────────┬────────────┬────────────┬─────────────────────┤
//! │   Magic    │  Version   │    Type    │      Length         │
//! │  4 bytes   │  2 bytes   │   1 byte   │      4 bytes        │
//! ├────────────┴────────────┴────────────┴─────────────────────┤
//! │                        Payload                             │
//! │                    (variable length)                       │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! - Magic: `0x4C 0x44 0x52 0x50` ("LDRP")
//! - Version: `0x01 0x00` (1.0)
//! - Type: Message type byte
//! - Length: Payload length in bytes (big-endian)

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::error::{Error, Result};

/// Protocol magic bytes: "LDRP"
pub const MAGIC: [u8; 4] = [0x4C, 0x44, 0x52, 0x50];

/// Frame header size in bytes
pub const HEADER_SIZE: usize = 11;

/// Maximum payload size (16 MB)
pub const MAX_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// Message types in the LDRP protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    /// Initial handshake
    Hello = 0x01,
    /// Handshake response
    HelloAck = 0x02,
    /// Verify share code
    CodeVerify = 0x03,
    /// Code verification result
    CodeVerifyAck = 0x04,
    /// List of files to transfer
    FileList = 0x05,
    /// Accept/reject file list
    FileListAck = 0x06,
    /// Request file preview
    PreviewRequest = 0x07,
    /// Preview thumbnail/content
    PreviewData = 0x08,
    /// Begin file chunk
    ChunkStart = 0x10,
    /// Chunk payload
    ChunkData = 0x11,
    /// Chunk received confirmation
    ChunkAck = 0x12,
    /// All files transferred
    TransferComplete = 0x20,
    /// Cancel transfer
    TransferCancel = 0x21,
    /// Keep-alive
    Ping = 0x30,
    /// Keep-alive response
    Pong = 0x31,
    /// Resume request (receiver sends completed chunks)
    ResumeRequest = 0x40,
    /// Resume acknowledgment (sender confirms what to retransfer)
    ResumeAck = 0x41,
    /// Clipboard content metadata
    ClipboardMeta = 0x50,
    /// Clipboard content data
    ClipboardData = 0x51,
    /// Clipboard acknowledgment
    ClipboardAck = 0x52,
    /// Clipboard content changed notification (for sync mode)
    ClipboardChanged = 0x53,
    /// Clipboard content request (for sync mode)
    ClipboardRequest = 0x54,
    /// Trusted device hello (replaces Hello for trusted connections)
    TrustedHello = 0x60,
    /// Trusted device hello acknowledgment
    TrustedHelloAck = 0x61,
    /// Trusted device verification challenge
    TrustedVerify = 0x62,
    /// Trusted device verification response
    TrustedVerifyAck = 0x63,
    /// Error message
    Error = 0xFF,
}

impl MessageType {
    /// Parse a message type from a byte.
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::HelloAck),
            0x03 => Some(Self::CodeVerify),
            0x04 => Some(Self::CodeVerifyAck),
            0x05 => Some(Self::FileList),
            0x06 => Some(Self::FileListAck),
            0x07 => Some(Self::PreviewRequest),
            0x08 => Some(Self::PreviewData),
            0x10 => Some(Self::ChunkStart),
            0x11 => Some(Self::ChunkData),
            0x12 => Some(Self::ChunkAck),
            0x20 => Some(Self::TransferComplete),
            0x21 => Some(Self::TransferCancel),
            0x30 => Some(Self::Ping),
            0x31 => Some(Self::Pong),
            0x40 => Some(Self::ResumeRequest),
            0x41 => Some(Self::ResumeAck),
            0x50 => Some(Self::ClipboardMeta),
            0x51 => Some(Self::ClipboardData),
            0x52 => Some(Self::ClipboardAck),
            0x53 => Some(Self::ClipboardChanged),
            0x54 => Some(Self::ClipboardRequest),
            0x60 => Some(Self::TrustedHello),
            0x61 => Some(Self::TrustedHelloAck),
            0x62 => Some(Self::TrustedVerify),
            0x63 => Some(Self::TrustedVerifyAck),
            0xFF => Some(Self::Error),
            _ => None,
        }
    }
}

/// A protocol frame header.
#[derive(Debug, Clone)]
pub struct FrameHeader {
    /// Protocol version (major, minor)
    pub version: (u8, u8),
    /// Message type
    pub message_type: MessageType,
    /// Payload length
    pub payload_length: u32,
}

impl FrameHeader {
    /// Encode the header to bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = self.version.0;
        buf[5] = self.version.1;
        buf[6] = self.message_type as u8;
        buf[7..11].copy_from_slice(&self.payload_length.to_be_bytes());
        buf
    }

    /// Decode a header from bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the header is invalid.
    pub fn decode(buf: &[u8; HEADER_SIZE]) -> Result<Self> {
        if buf[0..4] != MAGIC {
            return Err(Error::ProtocolError("invalid magic bytes".to_string()));
        }

        let version = (buf[4], buf[5]);

        let message_type = MessageType::from_byte(buf[6])
            .ok_or_else(|| Error::ProtocolError(format!("unknown message type: {:#x}", buf[6])))?;

        let payload_length = u32::from_be_bytes([buf[7], buf[8], buf[9], buf[10]]);

        if payload_length as usize > MAX_PAYLOAD_SIZE {
            return Err(Error::ProtocolError(format!(
                "payload too large: {payload_length} bytes"
            )));
        }

        Ok(Self {
            version,
            message_type,
            payload_length,
        })
    }
}

/// Hello message payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloPayload {
    /// Device name
    pub device_name: String,
    /// Protocol version string
    pub protocol_version: String,
    /// Device ID (optional, for trust feature)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub device_id: Option<uuid::Uuid>,
    /// Base64-encoded Ed25519 public key (optional, for trust feature)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub public_key: Option<String>,
}

/// Code verification payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeVerifyPayload {
    /// HMAC of the code
    pub code_hmac: Vec<u8>,
}

/// Code verification acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeVerifyAckPayload {
    /// Whether verification succeeded
    pub success: bool,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// File list payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListPayload {
    /// Files to transfer
    pub files: Vec<crate::file::FileMetadata>,
    /// Total size
    pub total_size: u64,
}

/// File list acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListAckPayload {
    /// Whether transfer is accepted
    pub accepted: bool,
    /// Indices of accepted files (if partial accept)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_files: Option<Vec<usize>>,
}

/// Chunk start payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkStartPayload {
    /// File index
    pub file_index: usize,
    /// Chunk index
    pub chunk_index: u64,
    /// Total chunks for this file
    pub total_chunks: u64,
}

/// Chunk data payload (binary).
#[derive(Debug, Clone)]
pub struct ChunkDataPayload {
    /// File index
    pub file_index: usize,
    /// Chunk index
    pub chunk_index: u64,
    /// Chunk data
    pub data: Vec<u8>,
    /// xxHash64 checksum
    pub checksum: u64,
}

/// Chunk acknowledgment payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkAckPayload {
    /// File index
    pub file_index: usize,
    /// Chunk index
    pub chunk_index: u64,
    /// Whether chunk was received successfully
    pub success: bool,
}

/// Error payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Error code
    pub code: String,
    /// Error message
    pub message: String,
}

/// Resume request payload.
///
/// Sent by the receiver to resume an interrupted transfer.
/// Contains information about which chunks have already been received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeRequestPayload {
    /// Transfer ID from the original transfer
    pub transfer_id: uuid::Uuid,
    /// Map of file index -> completed chunk indices
    pub completed_chunks: std::collections::HashMap<usize, Vec<u64>>,
    /// Map of file index -> SHA-256 hash (hex encoded) for fully completed files
    pub completed_file_hashes: std::collections::HashMap<usize, String>,
}

/// Resume acknowledgment payload.
///
/// Sent by the sender in response to a resume request.
/// Specifies which files/chunks need to be retransferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeAckPayload {
    /// Whether the resume request was accepted
    pub accepted: bool,
    /// Indices of files that need to be fully retransferred
    /// (e.g., due to hash mismatch or file changes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransfer_files: Option<Vec<usize>>,
    /// Map of file index -> chunk indices that need to be retransferred
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransfer_chunks: Option<std::collections::HashMap<usize, Vec<u64>>>,
    /// Reason if not accepted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Clipboard content type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ClipboardContentType {
    /// Plain text content
    PlainText = 0x01,
    /// Image in PNG format
    ImagePng = 0x10,
}

/// Clipboard metadata payload (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardMetaPayload {
    /// Type of clipboard content
    pub content_type: ClipboardContentType,
    /// Size in bytes
    pub size: u64,
    /// xxHash64 checksum
    pub checksum: u64,
    /// Unix timestamp (milliseconds)
    pub timestamp: i64,
}

/// Clipboard acknowledgment payload (JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardAckPayload {
    /// Whether the operation succeeded
    pub success: bool,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Clipboard changed notification payload (JSON) - for sync mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardChangedPayload {
    /// Type of clipboard content
    pub content_type: ClipboardContentType,
    /// Size in bytes
    pub size: u64,
    /// xxHash64 checksum
    pub checksum: u64,
    /// Unix timestamp (milliseconds)
    pub timestamp: i64,
}

/// Trusted device hello payload.
///
/// Sent by the initiating device to establish a trusted connection.
/// Unlike the regular Hello, this includes device identity for signature verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedHelloPayload {
    /// Device name
    pub device_name: String,
    /// Protocol version string
    pub protocol_version: String,
    /// Unique device identifier
    pub device_id: uuid::Uuid,
    /// Base64-encoded Ed25519 public key
    pub public_key: String,
    /// Random nonce for challenge (32 bytes, base64-encoded)
    pub nonce: String,
    /// Ed25519 signature of the nonce using sender's private key (base64-encoded)
    pub nonce_signature: String,
}

/// Trusted device hello acknowledgment payload.
///
/// Sent in response to `TrustedHello` if the sender is trusted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedHelloAckPayload {
    /// Whether the sender is trusted
    pub trusted: bool,
    /// Device name (if trusted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Device ID (if trusted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<uuid::Uuid>,
    /// Base64-encoded Ed25519 public key (if trusted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Ed25519 signature of the received nonce (proves identity, base64-encoded)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce_signature: Option<String>,
    /// Error message if not trusted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Trust level of the sender in receiver's store
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
}

/// Trusted device verification challenge payload.
///
/// Optional additional verification step for AskEachTime trust level.
/// Contains a new challenge nonce for mutual authentication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedVerifyPayload {
    /// Random challenge nonce (32 bytes, base64-encoded)
    pub challenge: String,
    /// Files being offered (for sender confirmation in AskEachTime mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<crate::file::FileMetadata>>,
    /// Total size (if files are included)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
}

/// Trusted device verification response payload.
///
/// Response to `TrustedVerify` containing the challenge signature
/// and sender's confirmation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedVerifyAckPayload {
    /// Ed25519 signature of the challenge (base64-encoded)
    pub challenge_signature: String,
    /// Whether the sender confirms the transfer (for AskEachTime mode)
    pub confirmed: bool,
    /// Reason if not confirmed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Encode a message payload to JSON bytes.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn encode_payload<T: Serialize>(payload: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(payload).map_err(|e| Error::Serialization(e.to_string()))
}

/// Decode a message payload from JSON bytes.
///
/// # Errors
///
/// Returns an error if deserialization fails.
pub fn decode_payload<T: for<'de> Deserialize<'de>>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data).map_err(|e| Error::Serialization(e.to_string()))
}

/// Read a complete frame from a stream.
///
/// # Errors
///
/// Returns an error if reading fails or the frame is invalid.
pub async fn read_frame<R>(reader: &mut R) -> Result<(FrameHeader, Vec<u8>)>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf).await?;

    let header = FrameHeader::decode(&header_buf)?;

    let mut payload = vec![0u8; header.payload_length as usize];
    if header.payload_length > 0 {
        reader.read_exact(&mut payload).await?;
    }

    Ok((header, payload))
}

/// Write a complete frame to a stream.
///
/// # Errors
///
/// Returns an error if writing fails.
pub async fn write_frame<W>(writer: &mut W, message_type: MessageType, payload: &[u8]) -> Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
{
    #[allow(clippy::cast_possible_truncation)]
    let header = FrameHeader {
        version: (1, 0),
        message_type,
        payload_length: payload.len() as u32,
    };

    writer.write_all(&header.encode()).await?;
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }
    writer.flush().await?;

    Ok(())
}

/// Read a complete frame from a stream with a timeout.
///
/// # Errors
///
/// Returns `Error::Timeout` if the operation exceeds the specified duration.
/// Returns an error if reading fails or the frame is invalid.
pub async fn read_frame_with_timeout<R>(
    reader: &mut R,
    duration: Duration,
) -> Result<(FrameHeader, Vec<u8>)>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    timeout(duration, read_frame(reader))
        .await
        .map_err(|_| Error::Timeout(duration.as_secs()))?
}

/// Write a complete frame to a stream with a timeout.
///
/// # Errors
///
/// Returns `Error::Timeout` if the operation exceeds the specified duration.
/// Returns an error if writing fails.
pub async fn write_frame_with_timeout<W>(
    writer: &mut W,
    message_type: MessageType,
    payload: &[u8],
    duration: Duration,
) -> Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
{
    timeout(duration, write_frame(writer, message_type, payload))
        .await
        .map_err(|_| Error::Timeout(duration.as_secs()))?
}

/// Encode a ChunkData payload (binary format).
///
/// Format: file_index (4 bytes) | chunk_index (8 bytes) | checksum (8 bytes) | data
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn encode_chunk_data(payload: &ChunkDataPayload) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20 + payload.data.len());
    buf.extend_from_slice(&(payload.file_index as u32).to_be_bytes());
    buf.extend_from_slice(&payload.chunk_index.to_be_bytes());
    buf.extend_from_slice(&payload.checksum.to_be_bytes());
    buf.extend_from_slice(&payload.data);
    buf
}

/// Decode a ChunkData payload (binary format).
///
/// # Errors
///
/// Returns an error if the payload is too short.
pub fn decode_chunk_data(data: &[u8]) -> Result<ChunkDataPayload> {
    if data.len() < 20 {
        return Err(Error::ProtocolError(
            "chunk data payload too short".to_string(),
        ));
    }

    let file_index = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let chunk_index = u64::from_be_bytes([
        data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
    ]);
    let checksum = u64::from_be_bytes([
        data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19],
    ]);
    let chunk_data = data[20..].to_vec();

    Ok(ChunkDataPayload {
        file_index,
        chunk_index,
        data: chunk_data,
        checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_header_encode_decode() {
        let header = FrameHeader {
            version: (1, 0),
            message_type: MessageType::Hello,
            payload_length: 256,
        };

        let encoded = header.encode();
        let decoded = FrameHeader::decode(&encoded).expect("decode");

        assert_eq!(decoded.version, (1, 0));
        assert_eq!(decoded.message_type, MessageType::Hello);
        assert_eq!(decoded.payload_length, 256);
    }

    #[test]
    fn test_chunk_data_encode_decode() {
        let payload = ChunkDataPayload {
            file_index: 5,
            chunk_index: 42,
            data: vec![1, 2, 3, 4, 5],
            checksum: 0x1234_5678_9ABC_DEF0,
        };

        let encoded = encode_chunk_data(&payload);
        let decoded = decode_chunk_data(&encoded).expect("decode");

        assert_eq!(decoded.file_index, payload.file_index);
        assert_eq!(decoded.chunk_index, payload.chunk_index);
        assert_eq!(decoded.data, payload.data);
        assert_eq!(decoded.checksum, payload.checksum);
    }

    #[tokio::test]
    async fn test_read_write_frame() {
        let mut buffer = Vec::new();

        let payload = b"test payload";
        write_frame(&mut buffer, MessageType::Hello, payload)
            .await
            .expect("write frame");

        let mut cursor = std::io::Cursor::new(buffer);
        let (header, read_payload) = read_frame(&mut cursor).await.expect("read frame");

        assert_eq!(header.message_type, MessageType::Hello);
        assert_eq!(read_payload, payload);
    }

    #[tokio::test]
    async fn test_ping_pong_roundtrip() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, MessageType::Ping, &[])
            .await
            .expect("write ping");

        let mut cursor = std::io::Cursor::new(buffer);
        let (header, payload) = read_frame(&mut cursor).await.expect("read ping");

        assert_eq!(header.message_type, MessageType::Ping);
        assert!(payload.is_empty());

        let mut buffer = Vec::new();
        write_frame(&mut buffer, MessageType::Pong, &[])
            .await
            .expect("write pong");

        let mut cursor = std::io::Cursor::new(buffer);
        let (header, payload) = read_frame(&mut cursor).await.expect("read pong");

        assert_eq!(header.message_type, MessageType::Pong);
        assert!(payload.is_empty());
    }

    #[tokio::test]
    async fn test_read_frame_with_timeout_success() {
        let mut buffer = Vec::new();

        let payload = b"test data";
        write_frame(&mut buffer, MessageType::Hello, payload)
            .await
            .expect("write frame");

        let mut cursor = std::io::Cursor::new(buffer);
        let result = read_frame_with_timeout(&mut cursor, Duration::from_secs(5)).await;

        assert!(result.is_ok());
        let (header, read_payload) = result.unwrap();
        assert_eq!(header.message_type, MessageType::Hello);
        assert_eq!(read_payload, payload);
    }

    #[tokio::test]
    async fn test_read_frame_with_timeout_expires() {
        struct NeverReadyReader;

        impl tokio::io::AsyncRead for NeverReadyReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                _buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Pending
            }
        }

        let mut reader = NeverReadyReader;
        let result = read_frame_with_timeout(&mut reader, Duration::from_millis(50)).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            crate::error::Error::Timeout(secs) => {
                assert_eq!(secs, 0);
            }
            e => panic!("Expected Timeout error, got: {e:?}"),
        }
    }

    #[tokio::test]
    async fn test_write_frame_with_timeout_success() {
        let mut buffer = Vec::new();

        let payload = b"test data";
        let result = write_frame_with_timeout(
            &mut buffer,
            MessageType::Pong,
            payload,
            Duration::from_secs(5),
        )
        .await;

        assert!(result.is_ok());

        let mut cursor = std::io::Cursor::new(buffer);
        let (header, read_payload) = read_frame(&mut cursor).await.expect("read frame");
        assert_eq!(header.message_type, MessageType::Pong);
        assert_eq!(read_payload, payload);
    }

    #[test]
    fn test_trusted_hello_serialization() {
        let device_id = uuid::Uuid::new_v4();
        let payload = TrustedHelloPayload {
            device_name: "Test Device".to_string(),
            protocol_version: "1.0".to_string(),
            device_id,
            public_key: "base64_public_key".to_string(),
            nonce: "base64_nonce".to_string(),
            nonce_signature: "base64_signature".to_string(),
        };

        let encoded = encode_payload(&payload).expect("encode");
        let decoded: TrustedHelloPayload = decode_payload(&encoded).expect("decode");

        assert_eq!(decoded.device_name, payload.device_name);
        assert_eq!(decoded.device_id, payload.device_id);
        assert_eq!(decoded.public_key, payload.public_key);
        assert_eq!(decoded.nonce, payload.nonce);
        assert_eq!(decoded.nonce_signature, payload.nonce_signature);
    }

    #[test]
    fn test_trusted_hello_ack_trusted() {
        let device_id = uuid::Uuid::new_v4();
        let payload = TrustedHelloAckPayload {
            trusted: true,
            device_name: Some("Receiver Device".to_string()),
            device_id: Some(device_id),
            public_key: Some("receiver_public_key".to_string()),
            nonce_signature: Some("nonce_sig".to_string()),
            error: None,
            trust_level: Some("Full".to_string()),
        };

        let encoded = encode_payload(&payload).expect("encode");
        let decoded: TrustedHelloAckPayload = decode_payload(&encoded).expect("decode");

        assert!(decoded.trusted);
        assert_eq!(decoded.device_name, Some("Receiver Device".to_string()));
        assert_eq!(decoded.device_id, Some(device_id));
        assert_eq!(decoded.trust_level, Some("Full".to_string()));
    }

    #[test]
    fn test_trusted_hello_ack_not_trusted() {
        let payload = TrustedHelloAckPayload {
            trusted: false,
            device_name: None,
            device_id: None,
            public_key: None,
            nonce_signature: None,
            error: Some("Device not in trust store".to_string()),
            trust_level: None,
        };

        let encoded = encode_payload(&payload).expect("encode");
        let json = String::from_utf8(encoded.clone()).expect("valid utf8");

        assert!(!json.contains("device_name"));
        assert!(!json.contains("device_id"));
        assert!(json.contains("error"));

        let decoded: TrustedHelloAckPayload = decode_payload(&encoded).expect("decode");
        assert!(!decoded.trusted);
        assert_eq!(decoded.error, Some("Device not in trust store".to_string()));
    }

    #[test]
    fn test_trusted_verify_serialization() {
        let payload = TrustedVerifyPayload {
            challenge: "challenge_nonce".to_string(),
            files: None,
            total_size: None,
        };

        let encoded = encode_payload(&payload).expect("encode");
        let decoded: TrustedVerifyPayload = decode_payload(&encoded).expect("decode");

        assert_eq!(decoded.challenge, payload.challenge);
        assert!(decoded.files.is_none());
    }

    #[test]
    fn test_trusted_verify_ack_serialization() {
        let payload = TrustedVerifyAckPayload {
            challenge_signature: "signed_challenge".to_string(),
            confirmed: true,
            reason: None,
        };

        let encoded = encode_payload(&payload).expect("encode");
        let decoded: TrustedVerifyAckPayload = decode_payload(&encoded).expect("decode");

        assert_eq!(decoded.challenge_signature, payload.challenge_signature);
        assert!(decoded.confirmed);
        assert!(decoded.reason.is_none());
    }

    #[test]
    fn test_trusted_message_types() {
        assert_eq!(
            MessageType::from_byte(0x60),
            Some(MessageType::TrustedHello)
        );
        assert_eq!(
            MessageType::from_byte(0x61),
            Some(MessageType::TrustedHelloAck)
        );
        assert_eq!(
            MessageType::from_byte(0x62),
            Some(MessageType::TrustedVerify)
        );
        assert_eq!(
            MessageType::from_byte(0x63),
            Some(MessageType::TrustedVerifyAck)
        );
    }
}
