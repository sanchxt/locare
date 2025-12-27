//! Device beacon for trusted device discovery.
//!
//! This module provides on-demand device beaconing for discovering trusted devices
//! on the local network. Unlike the share code-based discovery, beacons are used
//! for direct device-to-device communication without requiring a share code.
//!
//! ## Protocol
//!
//! Device beacons are broadcast on the same port as discovery packets (52525 UDP)
//! but use a different packet type (`beacon_type: "device"`).
//!
//! ## Flow
//!
//! 1. Sender runs `send` command targeting a trusted device
//! 2. Sender broadcasts beacon with `looking_for` set to target device_id
//! 3. Target device (if running) broadcasts its beacon when it sees the request
//! 4. Sender connects to target device using the discovered address
//!
//! This is an on-demand system - beacons are only broadcast when needed,
//! not continuously.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use crate::error::{Error, Result};

use super::DEFAULT_DISCOVERY_PORT;

/// Device beacon broadcast by devices announcing availability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceBeacon {
    /// Packet type identifier (always "device" for beacons)
    pub beacon_type: String,
    /// Protocol identifier
    pub protocol: String,
    /// Protocol version
    pub version: String,
    /// Unique device identifier
    pub device_id: Uuid,
    /// Device display name
    pub device_name: String,
    /// Base64-encoded Ed25519 public key
    pub public_key: String,
    /// Port for file transfer
    pub transfer_port: u16,
    /// Target device_id if looking for a specific device
    #[serde(skip_serializing_if = "Option::is_none")]
    pub looking_for: Option<Uuid>,
    /// Whether this device is ready to receive files
    pub ready_to_receive: bool,
    /// Timestamp for deduplication and freshness
    pub timestamp: u64,
}

impl DeviceBeacon {
    /// Create a new device beacon.
    ///
    /// # Arguments
    ///
    /// * `device_id` - Unique device identifier
    /// * `device_name` - Human-readable device name
    /// * `public_key` - Base64-encoded Ed25519 public key
    /// * `transfer_port` - Port for file transfers
    #[must_use]
    pub fn new(device_id: Uuid, device_name: &str, public_key: &str, transfer_port: u16) -> Self {
        Self {
            beacon_type: "device".to_string(),
            protocol: "localdrop".to_string(),
            version: "1.0".to_string(),
            device_id,
            device_name: device_name.to_string(),
            public_key: public_key.to_string(),
            transfer_port,
            looking_for: None,
            ready_to_receive: false,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    /// Set the target device this beacon is looking for.
    #[must_use]
    pub fn looking_for(mut self, device_id: Uuid) -> Self {
        self.looking_for = Some(device_id);
        self
    }

    /// Mark this beacon as ready to receive files.
    #[must_use]
    pub fn ready_to_receive(mut self, ready: bool) -> Self {
        self.ready_to_receive = ready;
        self
    }

    /// Check if this is a valid device beacon.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.beacon_type == "device" && self.protocol == "localdrop" && self.version == "1.0"
    }

    /// Check if this beacon is looking for a specific device.
    #[must_use]
    pub fn is_looking_for(&self, device_id: Uuid) -> bool {
        self.looking_for == Some(device_id)
    }

    /// Update the timestamp.
    pub fn refresh_timestamp(&mut self) {
        self.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
    }
}

/// A discovered device on the network.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    /// The device beacon
    pub beacon: DeviceBeacon,
    /// Source address
    pub source: SocketAddr,
    /// When this device was discovered
    pub discovered_at: Instant,
}

impl DiscoveredDevice {
    /// Get the transfer address for this device.
    #[must_use]
    pub fn transfer_addr(&self) -> SocketAddr {
        let ip = match self.source {
            SocketAddr::V4(addr) => *addr.ip(),
            SocketAddr::V6(_) => Ipv4Addr::LOCALHOST,
        };
        SocketAddr::V4(SocketAddrV4::new(ip, self.beacon.transfer_port))
    }
}

/// Broadcaster for announcing device availability.
#[derive(Debug)]
pub struct BeaconBroadcaster {
    /// UDP socket for broadcasting
    socket: Arc<UdpSocket>,
    /// Discovery port
    port: u16,
    /// Shutdown signal sender
    shutdown_tx: broadcast::Sender<()>,
    /// Whether broadcasting is active
    is_active: Arc<Mutex<bool>>,
}

impl BeaconBroadcaster {
    /// Create a new beacon broadcaster on the specified port.
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

    /// Create a new beacon broadcaster on the default port.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created.
    pub async fn new_default() -> Result<Self> {
        Self::new(DEFAULT_DISCOVERY_PORT).await
    }

    /// Start broadcasting a device beacon.
    ///
    /// # Arguments
    ///
    /// * `beacon` - The device beacon to broadcast
    /// * `interval` - How often to broadcast
    ///
    /// # Errors
    ///
    /// Returns an error if broadcasting fails.
    pub async fn start(&self, beacon: DeviceBeacon, interval: Duration) -> Result<()> {
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
            let mut current_beacon = beacon;

            loop {
                current_beacon.refresh_timestamp();

                let json = match serde_json::to_vec(&current_beacon) {
                    Ok(json) => json,
                    Err(e) => {
                        tracing::error!("Failed to serialize device beacon: {}", e);
                        break;
                    }
                };

                if let Err(e) = socket.send_to(&json, broadcast_addr).await {
                    tracing::warn!("Failed to send beacon: {}", e);
                }

                tokio::select! {
                    () = tokio::time::sleep(interval) => {}
                    _ = shutdown_rx.recv() => {
                        tracing::debug!("BeaconBroadcaster received shutdown signal");
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

/// Listener for discovering devices on the network.
#[derive(Debug)]
pub struct BeaconListener {
    /// UDP socket for receiving beacons
    socket: Arc<UdpSocket>,
}

impl BeaconListener {
    /// Create a new beacon listener on the specified port.
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

    /// Create a new beacon listener on the default port.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket cannot be created.
    pub async fn new_default() -> Result<Self> {
        Self::new(DEFAULT_DISCOVERY_PORT).await
    }

    /// Wait for a device with the given device_id.
    ///
    /// # Arguments
    ///
    /// * `device_id` - The device ID to look for
    /// * `timeout` - How long to wait
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not found within the timeout.
    pub async fn find_device(
        &self,
        device_id: Uuid,
        timeout: Duration,
    ) -> Result<DiscoveredDevice> {
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::DeviceNotTrusted(device_id.to_string()));
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(beacon) = serde_json::from_slice::<DeviceBeacon>(&buf[..len]) {
                        if beacon.is_valid() && beacon.device_id == device_id {
                            return Ok(DiscoveredDevice {
                                beacon,
                                source,
                                discovered_at: Instant::now(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving beacon: {}", e);
                }
                Err(_) => {
                    return Err(Error::DeviceNotTrusted(device_id.to_string()));
                }
            }
        }
    }

    /// Wait for a device with the given name.
    ///
    /// # Arguments
    ///
    /// * `device_name` - The device name to look for (case-insensitive)
    /// * `timeout` - How long to wait
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not found within the timeout.
    pub async fn find_by_name(
        &self,
        device_name: &str,
        timeout: Duration,
    ) -> Result<DiscoveredDevice> {
        let deadline = Instant::now() + timeout;
        let name_lower = device_name.to_lowercase();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::DeviceNotTrusted(device_name.to_string()));
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(beacon) = serde_json::from_slice::<DeviceBeacon>(&buf[..len]) {
                        if beacon.is_valid() && beacon.device_name.to_lowercase() == name_lower {
                            return Ok(DiscoveredDevice {
                                beacon,
                                source,
                                discovered_at: Instant::now(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving beacon: {}", e);
                }
                Err(_) => {
                    return Err(Error::DeviceNotTrusted(device_name.to_string()));
                }
            }
        }
    }

    /// Wait for any device that is looking for us.
    ///
    /// # Arguments
    ///
    /// * `our_device_id` - Our device ID to match against looking_for
    /// * `timeout` - How long to wait
    ///
    /// # Errors
    ///
    /// Returns an error if no device is found within the timeout.
    pub async fn find_looking_for_us(
        &self,
        our_device_id: Uuid,
        timeout: Duration,
    ) -> Result<DiscoveredDevice> {
        let deadline = Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout(timeout.as_secs()));
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(beacon) = serde_json::from_slice::<DeviceBeacon>(&buf[..len]) {
                        if beacon.is_valid() && beacon.is_looking_for(our_device_id) {
                            return Ok(DiscoveredDevice {
                                beacon,
                                source,
                                discovered_at: Instant::now(),
                            });
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving beacon: {}", e);
                }
                Err(_) => {
                    return Err(Error::Timeout(timeout.as_secs()));
                }
            }
        }
    }

    /// List all devices broadcasting beacons on the network.
    ///
    /// # Arguments
    ///
    /// * `duration` - How long to listen for beacons
    ///
    /// # Returns
    ///
    /// A list of discovered devices, deduplicated by device_id.
    pub async fn scan(&self, duration: Duration) -> Vec<DiscoveredDevice> {
        let deadline = Instant::now() + duration;
        let mut devices: HashMap<Uuid, DiscoveredDevice> = HashMap::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            let mut buf = [0u8; 4096];
            let result = tokio::time::timeout(remaining, self.socket.recv_from(&mut buf)).await;

            match result {
                Ok(Ok((len, source))) => {
                    if let Ok(beacon) = serde_json::from_slice::<DeviceBeacon>(&buf[..len]) {
                        if beacon.is_valid() {
                            let device_id = beacon.device_id;
                            devices.insert(
                                device_id,
                                DiscoveredDevice {
                                    beacon,
                                    source,
                                    discovered_at: Instant::now(),
                                },
                            );
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("Error receiving beacon: {}", e);
                }
                Err(_) => {
                    break;
                }
            }
        }

        devices.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_beacon_creation() {
        let device_id = Uuid::new_v4();
        let beacon = DeviceBeacon::new(device_id, "Test Device", "public_key_base64", 52530);

        assert_eq!(beacon.beacon_type, "device");
        assert_eq!(beacon.protocol, "localdrop");
        assert_eq!(beacon.version, "1.0");
        assert_eq!(beacon.device_id, device_id);
        assert_eq!(beacon.device_name, "Test Device");
        assert_eq!(beacon.transfer_port, 52530);
        assert!(beacon.is_valid());
        assert!(!beacon.ready_to_receive);
        assert!(beacon.looking_for.is_none());
    }

    #[test]
    fn test_beacon_looking_for() {
        let device_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let beacon =
            DeviceBeacon::new(device_id, "Test Device", "key", 52530).looking_for(target_id);

        assert_eq!(beacon.looking_for, Some(target_id));
        assert!(beacon.is_looking_for(target_id));
        assert!(!beacon.is_looking_for(Uuid::new_v4()));
    }

    #[test]
    fn test_beacon_ready_to_receive() {
        let device_id = Uuid::new_v4();
        let beacon =
            DeviceBeacon::new(device_id, "Test Device", "key", 52530).ready_to_receive(true);

        assert!(beacon.ready_to_receive);
    }

    #[test]
    fn test_beacon_serialization() {
        let device_id = Uuid::new_v4();
        let beacon = DeviceBeacon::new(device_id, "Test Device", "public_key", 52530)
            .looking_for(Uuid::new_v4())
            .ready_to_receive(true);

        let json = serde_json::to_string(&beacon).expect("serialize");
        let deserialized: DeviceBeacon = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.device_id, beacon.device_id);
        assert_eq!(deserialized.device_name, beacon.device_name);
        assert_eq!(deserialized.looking_for, beacon.looking_for);
        assert_eq!(deserialized.ready_to_receive, beacon.ready_to_receive);
    }

    #[test]
    fn test_beacon_without_looking_for_skips_field() {
        let device_id = Uuid::new_v4();
        let beacon = DeviceBeacon::new(device_id, "Test Device", "key", 52530);

        let json = serde_json::to_string(&beacon).expect("serialize");
        assert!(!json.contains("looking_for"));
    }

    #[tokio::test]
    async fn test_broadcaster_creation() {
        let broadcaster = BeaconBroadcaster::new(0).await;
        assert!(broadcaster.is_ok(), "BeaconBroadcaster should be created");
    }

    #[tokio::test]
    async fn test_listener_creation() {
        let listener = BeaconListener::new(0).await;
        assert!(listener.is_ok(), "BeaconListener should be created");
    }

    #[tokio::test]
    async fn test_broadcaster_start_stop() {
        let broadcaster = BeaconBroadcaster::new(0).await.expect("create broadcaster");
        let device_id = Uuid::new_v4();
        let beacon = DeviceBeacon::new(device_id, "Test Device", "key", 52530);

        broadcaster
            .start(beacon, Duration::from_millis(100))
            .await
            .expect("start broadcasting");

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(broadcaster.is_broadcasting().await);

        broadcaster.stop().await;
        assert!(!broadcaster.is_broadcasting().await);
    }

    #[test]
    fn test_discovered_device_transfer_addr() {
        let device_id = Uuid::new_v4();
        let beacon = DeviceBeacon::new(device_id, "Test", "key", 52530);
        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 100), 52525));

        let discovered = DiscoveredDevice {
            beacon,
            source,
            discovered_at: Instant::now(),
        };

        let transfer_addr = discovered.transfer_addr();
        assert_eq!(
            transfer_addr,
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 100), 52530))
        );
    }
}
