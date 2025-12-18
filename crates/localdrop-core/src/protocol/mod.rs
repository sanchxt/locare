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

use serde::{Deserialize, Serialize};

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
}
