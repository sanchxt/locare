//! Network discovery for LocalDrop.
//!
//! This module handles device discovery on the local network using:
//! - UDP broadcast for simple discovery
//! - mDNS/DNS-SD for more robust discovery (when `mdns` feature is enabled)
//!
//! ## Protocol
//!
//! - Primary port: 52525 (UDP)
//! - Fallback port: 52526 (UDP)
//! - Broadcast interval: Every 2 seconds while sharing
//!
//! ## Discovery Packet
//!
//! ```json
//! {
//!   "protocol": "localdrop",
//!   "version": "1.0",
//!   "code": "A7K9",
//!   "device_name": "Marcus-Laptop",
//!   "device_id": "uuid-v4",
//!   "expires_at": 1699900000,
//!   "transfer_port": 52530,
//!   "supports": ["tcp", "quic"],
//!   "file_count": 3,
//!   "total_size": 157286400,
//!   "preview_available": true
//! }
//! ```
//!
//! ## mDNS Discovery
//!
//! When the `mdns` feature is enabled, LocalDrop also advertises and discovers
//! shares via mDNS/DNS-SD using the service type `_localdrop._tcp.local.`.
//! This provides better reliability in networks where UDP broadcast is blocked.

#[cfg(feature = "mdns")]
pub mod mdns;

mod beacon;
mod hybrid;

pub use beacon::{BeaconBroadcaster, BeaconListener, DeviceBeacon, DiscoveredDevice};
pub use hybrid::{HybridBroadcaster, HybridListener};

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use crate::code::ShareCode;
use crate::error::{Error, Result};

/// Default discovery port.
pub const DEFAULT_DISCOVERY_PORT: u16 = 52525;

/// Discovery packet broadcast by sharers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryPacket {
    /// Protocol identifier
    pub protocol: String,
    /// Protocol version
    pub version: String,
    /// Share code
    pub code: String,
    /// Device display name
    pub device_name: String,
    /// Unique device identifier
    pub device_id: Uuid,
    /// Unix timestamp when code expires
    pub expires_at: u64,
    /// Port for file transfer
    pub transfer_port: u16,
    /// Supported transport protocols
    pub supports: Vec<String>,
    /// Number of files being shared
    pub file_count: usize,
    /// Total size in bytes
    pub total_size: u64,
    /// Whether previews are available
    pub preview_available: bool,
}

impl DiscoveryPacket {
    /// Create a new discovery packet.
    #[must_use]
    pub fn new(
        code: &ShareCode,
        device_name: &str,
        device_id: Uuid,
        transfer_port: u16,
        file_count: usize,
        total_size: u64,
    ) -> Self {
        Self {
            protocol: "localdrop".to_string(),
            version: "1.0".to_string(),
            code: code.to_string(),
            device_name: device_name.to_string(),
            device_id,
            expires_at: 0,
            transfer_port,
            supports: vec!["tcp".to_string()],
            file_count,
            total_size,
            preview_available: true,
        }
    }

    /// Check if this is a valid LocalDrop packet.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.protocol == "localdrop" && self.version == "1.0"
    }
}

/// A discovered share on the network.
#[derive(Debug, Clone)]
pub struct DiscoveredShare {
    /// The discovery packet
    pub packet: DiscoveryPacket,
    /// Source address
    pub source: SocketAddr,
    /// When this share was discovered
    pub discovered_at: Instant,
}

/// Broadcaster for announcing shares on the network.
#[derive(Debug)]
pub struct Broadcaster {
    /// UDP socket for broadcasting
    socket: Arc<UdpSocket>,
    /// Discovery port
    port: u16,
    /// Shutdown signal sender
    shutdown_tx: broadcast::Sender<()>,
    /// Whether broadcasting is active
    is_active: Arc<Mutex<bool>>,
}

impl Broadcaster {
    /// Create a new broadcaster on the specified port.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created.
    pub async fn new(port: u16) -> Result<Self> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;

        socket.set_broadcast(true)?;
        socket.set_reuse_address(true)?;

        #[cfg(target_os = "macos")]
        socket.set_reuse_port(true)?;

        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0);
        socket.bind(&addr.into())?;

        socket.set_nonblocking(true)?;

        let std_socket: std::net::UdpSocket = socket.into();
        let socket = UdpSocket::from_std(std_socket)?;

        let (shutdown_tx, _) = broadcast::channel(1);

        Ok(Self {
            socket: Arc::new(socket),
            port,
            shutdown_tx,
            is_active: Arc::new(Mutex::new(false)),
        })
    }

    /// Start broadcasting a share.
    ///
    /// # Arguments
    ///
    /// * `packet` - The discovery packet to broadcast
    /// * `interval` - How often to broadcast
    ///
    /// # Errors
    ///
    /// Returns an error if broadcasting fails.
    pub async fn start(&self, packet: DiscoveryPacket, interval: Duration) -> Result<()> {
        let mut is_active = self.is_active.lock().await;
        if *is_active {
            return Ok(());
        }
        *is_active = true;
        drop(is_active);

        let socket = Arc::clone(&self.socket);
        let port = self.port;
        let is_active = Arc::clone(&self.is_active);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            let broadcast_addr = SocketAddrV4::new(Ipv4Addr::BROADCAST, port);

            loop {
                let json = match serde_json::to_vec(&packet) {
                    Ok(json) => json,
                    Err(e) => {
                        tracing::error!("Failed to serialize discovery packet: {}", e);
                        break;
                    }
                };

                if let Err(e) = socket.send_to(&json, broadcast_addr).await {
                    tracing::warn!("Failed to send broadcast: {}", e);
                }

                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("Broadcaster received shutdown signal");
                        break;
                    }
                }
            }

            *is_active.lock().await = false;
        });

        Ok(())
    }

    /// Stop broadcasting.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(());
        while *self.is_active.lock().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Check if currently broadcasting.
    pub async fn is_broadcasting(&self) -> bool {
        *self.is_active.lock().await
    }
}

/// Listener for discovering shares on the network.
#[derive(Debug)]
pub struct Listener {
    /// UDP socket for receiving broadcasts
    socket: Arc<UdpSocket>,
}

impl Listener {
    /// Create a new listener on the specified port.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created.
    pub async fn new(port: u16) -> Result<Self> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;

        socket.set_reuse_address(true)?;

        #[cfg(target_os = "macos")]
        socket.set_reuse_port(true)?;

        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        socket.bind(&addr.into())?;

        socket.set_nonblocking(true)?;

        let std_socket: std::net::UdpSocket = socket.into();
        let socket = UdpSocket::from_std(std_socket)?;

        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    /// Wait for a share with the given code.
    ///
    /// # Arguments
    ///
    /// * `code` - The share code to look for
    /// * `timeout` - How long to wait
    ///
    /// # Errors
    ///
    /// Returns an error if the code is not found within the timeout.
    pub async fn find(&self, code: &ShareCode, timeout: Duration) -> Result<DiscoveredShare> {
        let deadline = Instant::now() + timeout;
        let code_str = code.to_string();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::CodeNotFound(code_str));
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(packet) = serde_json::from_slice::<DiscoveryPacket>(&buf[..len]) {
                        if packet.is_valid() && packet.code == code_str {
                            return Ok(DiscoveredShare {
                                packet,
                                source,
                                discovered_at: Instant::now(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving UDP packet: {}", e);
                }
                Err(_) => {
                    return Err(Error::CodeNotFound(code_str));
                }
            }
        }
    }

    /// List all active shares on the network.
    ///
    /// # Arguments
    ///
    /// * `duration` - How long to listen for shares
    ///
    /// # Returns
    ///
    /// A list of discovered shares, deduplicated by device_id.
    pub async fn scan(&self, duration: Duration) -> Vec<DiscoveredShare> {
        let deadline = Instant::now() + duration;
        let mut shares: HashMap<Uuid, DiscoveredShare> = HashMap::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(packet) = serde_json::from_slice::<DiscoveryPacket>(&buf[..len]) {
                        if packet.is_valid() {
                            let device_id = packet.device_id;
                            shares.insert(
                                device_id,
                                DiscoveredShare {
                                    packet,
                                    source,
                                    discovered_at: Instant::now(),
                                },
                            );
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving UDP packet: {}", e);
                }
                Err(_) => {
                    break;
                }
            }
        }

        shares.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::CodeGenerator;

    fn generate_code() -> ShareCode {
        CodeGenerator::new().generate().expect("generate code")
    }

    #[test]
    fn test_discovery_packet_creation() {
        let code = generate_code();
        let device_id = Uuid::new_v4();
        let packet = DiscoveryPacket::new(&code, "Test Device", device_id, 52530, 5, 1024 * 1024);

        assert_eq!(packet.protocol, "localdrop");
        assert_eq!(packet.version, "1.0");
        assert_eq!(packet.code, code.to_string());
        assert_eq!(packet.device_name, "Test Device");
        assert_eq!(packet.device_id, device_id);
        assert_eq!(packet.transfer_port, 52530);
        assert_eq!(packet.file_count, 5);
        assert_eq!(packet.total_size, 1024 * 1024);
        assert!(packet.is_valid());
    }

    #[test]
    fn test_discovery_packet_serialization() {
        let code = generate_code();
        let device_id = Uuid::new_v4();
        let packet = DiscoveryPacket::new(&code, "Test Device", device_id, 52530, 5, 1024 * 1024);

        let json = serde_json::to_string(&packet).expect("serialize");

        let deserialized: DiscoveryPacket = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.protocol, packet.protocol);
        assert_eq!(deserialized.code, packet.code);
        assert_eq!(deserialized.device_id, packet.device_id);
    }

    #[tokio::test]
    async fn test_broadcaster_creation() {
        let broadcaster = Broadcaster::new(0).await;
        assert!(broadcaster.is_ok(), "Broadcaster should be created");
    }

    #[tokio::test]
    async fn test_listener_creation() {
        let listener = Listener::new(0).await;
        assert!(listener.is_ok(), "Listener should be created");
    }

    #[tokio::test]
    async fn test_broadcaster_start_stop() {
        let broadcaster = Broadcaster::new(0).await.expect("create broadcaster");
        let code = generate_code();
        let device_id = Uuid::new_v4();
        let packet = DiscoveryPacket::new(&code, "Test Device", device_id, 52530, 1, 1024);

        broadcaster
            .start(packet, Duration::from_millis(100))
            .await
            .expect("start broadcasting");

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(broadcaster.is_broadcasting().await);

        broadcaster.stop().await;
        assert!(!broadcaster.is_broadcasting().await);
    }

    #[tokio::test]
    #[ignore = "UDP broadcast unreliable in CI environments (especially macOS)"]
    async fn test_discovery_loopback() {
        let port = 52600 + (std::process::id() % 100) as u16;

        let listener = Listener::new(port).await.expect("create listener");

        let broadcaster = Broadcaster::new(port).await.expect("create broadcaster");

        let code = generate_code();
        let device_id = Uuid::new_v4();
        let packet = DiscoveryPacket::new(&code, "Test Device", device_id, 52530, 3, 2048);

        broadcaster
            .start(packet.clone(), Duration::from_millis(50))
            .await
            .expect("start broadcasting");

        let result = listener.find(&code, Duration::from_secs(2)).await;

        broadcaster.stop().await;

        assert!(result.is_ok(), "Should find the share");
        let share = result.unwrap();
        assert_eq!(share.packet.code, code.to_string());
        assert_eq!(share.packet.device_id, device_id);
        assert_eq!(share.packet.file_count, 3);
    }

    #[tokio::test]
    async fn test_find_timeout() {
        let port = 52700 + (std::process::id() % 100) as u16;
        let listener = Listener::new(port).await.expect("create listener");

        let code = generate_code();
        let result = listener.find(&code, Duration::from_millis(100)).await;

        assert!(result.is_err(), "Should timeout for non-existent code");
        assert!(matches!(result.unwrap_err(), Error::CodeNotFound(_)));
    }

    #[tokio::test]
    #[ignore = "UDP broadcast unreliable in CI environments (especially macOS)"]
    async fn test_scan_multiple_shares() {
        let port = 52800 + (std::process::id() % 100) as u16;

        let listener = Listener::new(port).await.expect("create listener");

        let mut broadcasters = Vec::new();
        for i in 0..3u16 {
            let broadcaster = Broadcaster::new(port).await.expect("create broadcaster");
            let code = generate_code();
            let device_id = Uuid::new_v4();
            let packet = DiscoveryPacket::new(
                &code,
                &format!("Device {i}"),
                device_id,
                52530 + i,
                i as usize,
                1024,
            );

            broadcaster
                .start(packet, Duration::from_millis(30))
                .await
                .expect("start broadcasting");
            broadcasters.push(broadcaster);
        }

        let shares = listener.scan(Duration::from_millis(300)).await;

        for broadcaster in &broadcasters {
            broadcaster.stop().await;
        }

        assert_eq!(
            shares.len(),
            3,
            "Should find all 3 shares (found {})",
            shares.len()
        );
    }
}
