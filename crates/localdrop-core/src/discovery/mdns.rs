//! mDNS/DNS-SD discovery for LocalDrop.
//!
//! This module provides an alternative discovery mechanism using mDNS/DNS-SD,
//! which works better in environments where UDP broadcast is blocked or unreliable.
//!
//! ## Service Type
//!
//! LocalDrop registers as `_localdrop._tcp.local.` with TXT records containing
//! the share code and transfer metadata.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::code::ShareCode;
use crate::error::{Error, Result};

/// mDNS service type for LocalDrop.
pub const SERVICE_TYPE: &str = "_localdrop._tcp.local.";

/// TXT record keys for service properties.
pub mod txt_keys {
    /// Share code key
    pub const CODE: &str = "code";
    /// Device name key
    pub const DEVICE_NAME: &str = "device_name";
    /// Device ID key
    pub const DEVICE_ID: &str = "device_id";
    /// File count key
    pub const FILE_COUNT: &str = "file_count";
    /// Total size key
    pub const TOTAL_SIZE: &str = "total_size";
    /// Protocol version key
    pub const VERSION: &str = "version";
}

/// Properties for mDNS service registration.
#[derive(Debug, Clone)]
pub struct MdnsProperties {
    /// Share code
    pub code: String,
    /// Device name
    pub device_name: String,
    /// Device ID
    pub device_id: Uuid,
    /// Transfer port
    pub transfer_port: u16,
    /// Number of files
    pub file_count: usize,
    /// Total size in bytes
    pub total_size: u64,
    /// Protocol version
    pub protocol_version: String,
}

impl MdnsProperties {
    /// Convert to TXT record properties.
    #[must_use]
    pub fn to_txt_properties(&self) -> Vec<(&str, String)> {
        vec![
            (txt_keys::CODE, self.code.clone()),
            (txt_keys::DEVICE_NAME, self.device_name.clone()),
            (txt_keys::DEVICE_ID, self.device_id.to_string()),
            (txt_keys::FILE_COUNT, self.file_count.to_string()),
            (txt_keys::TOTAL_SIZE, self.total_size.to_string()),
            (txt_keys::VERSION, self.protocol_version.clone()),
        ]
    }
}

/// Information about a discovered mDNS service.
#[derive(Debug, Clone)]
pub struct MdnsDiscoveredShare {
    /// Share code
    pub code: String,
    /// Device name
    pub device_name: String,
    /// Device ID
    pub device_id: Uuid,
    /// Socket address for transfer
    pub address: SocketAddr,
    /// Transfer port
    pub transfer_port: u16,
    /// File count
    pub file_count: usize,
    /// Total size
    pub total_size: u64,
    /// Protocol version
    pub protocol_version: String,
}

impl MdnsDiscoveredShare {
    /// Parse from a resolved ServiceInfo.
    fn from_service_info(info: &ServiceInfo) -> Option<Self> {
        let properties = info.get_properties();

        // Helper to extract string value from TxtProperty
        let get_str =
            |key: &str| -> Option<String> { properties.get(key).map(|p| p.val_str().to_string()) };

        let code = get_str(txt_keys::CODE)?;
        let device_name = get_str(txt_keys::DEVICE_NAME)?;
        let device_id = get_str(txt_keys::DEVICE_ID).and_then(|s| Uuid::parse_str(&s).ok())?;
        let file_count = get_str(txt_keys::FILE_COUNT)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let total_size = get_str(txt_keys::TOTAL_SIZE)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let protocol_version = get_str(txt_keys::VERSION).unwrap_or_else(|| "1.0".to_string());

        // Get the first IPv4 address
        let addresses = info.get_addresses();
        let ip = addresses.iter().find(|addr| addr.is_ipv4())?;
        let port = info.get_port();

        Some(Self {
            code,
            device_name,
            device_id,
            address: SocketAddr::new(*ip, port),
            transfer_port: port,
            file_count,
            total_size,
            protocol_version,
        })
    }
}

/// mDNS service broadcaster.
///
/// Registers a LocalDrop share as an mDNS service on the local network.
pub struct MdnsBroadcaster {
    /// The mDNS daemon (wrapped in Option to support Drop)
    daemon: Option<ServiceDaemon>,
    /// Instance name for the registered service
    instance_name: Arc<Mutex<Option<String>>>,
}

impl MdnsBroadcaster {
    /// Create a new mDNS broadcaster.
    ///
    /// # Errors
    ///
    /// Returns an error if the mDNS daemon cannot be created.
    pub fn new() -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|e| Error::Internal(format!("mDNS daemon error: {e}")))?;

        Ok(Self {
            daemon: Some(daemon),
            instance_name: Arc::new(Mutex::new(None)),
        })
    }

    /// Register a share as an mDNS service.
    ///
    /// # Arguments
    ///
    /// * `properties` - The service properties
    ///
    /// # Errors
    ///
    /// Returns an error if registration fails.
    pub async fn register(&self, properties: MdnsProperties) -> Result<()> {
        // Generate a unique instance name
        let instance_name = format!("LocalDrop-{}", &properties.code);

        // Build TXT properties
        let txt_props: Vec<_> = properties.to_txt_properties();

        // Get local hostname and ensure it ends with .local.
        let raw_hostname = hostname::get().map_or_else(
            |_| "localhost".to_string(),
            |h| h.to_string_lossy().to_string(),
        );

        // mDNS requires hostnames to end with .local.
        let hostname = if raw_hostname.ends_with(".local.") {
            raw_hostname
        } else if raw_hostname.to_lowercase().ends_with(".local") {
            format!("{raw_hostname}.")
        } else {
            format!("{raw_hostname}.local.")
        };

        // Create service info
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &hostname,
            (),
            properties.transfer_port,
            txt_props.as_slice(),
        )
        .map_err(|e| Error::Internal(format!("Failed to create mDNS service info: {e}")))?;

        // Register the service
        self.daemon
            .as_ref()
            .ok_or_else(|| Error::Internal("mDNS daemon already shutdown".to_string()))?
            .register(service_info)
            .map_err(|e| Error::Internal(format!("Failed to register mDNS service: {e}")))?;

        // Store instance name for later unregistration
        *self.instance_name.lock().await = Some(instance_name.clone());

        tracing::info!(
            code = %properties.code,
            instance = %instance_name,
            "Registered mDNS service"
        );

        Ok(())
    }

    /// Unregister the mDNS service.
    ///
    /// # Errors
    ///
    /// Returns an error if unregistration fails.
    pub async fn unregister(&self) -> Result<()> {
        let instance_name = self.instance_name.lock().await.take();
        if let Some(instance_name) = instance_name {
            let full_name = format!("{instance_name}.{SERVICE_TYPE}");

            self.daemon
                .as_ref()
                .ok_or_else(|| Error::Internal("mDNS daemon already shutdown".to_string()))?
                .unregister(&full_name)
                .map_err(|e| Error::Internal(format!("Failed to unregister mDNS service: {e}")))?;

            tracing::info!(instance = %instance_name, "Unregistered mDNS service");
        }

        Ok(())
    }

    /// Shutdown the broadcaster.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown fails.
    pub fn shutdown(mut self) -> Result<()> {
        if let Some(daemon) = self.daemon.take() {
            daemon
                .shutdown()
                .map_err(|e| Error::Internal(format!("Failed to shutdown mDNS daemon: {e}")))?;
        }
        Ok(())
    }
}

impl Drop for MdnsBroadcaster {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            if let Err(e) = daemon.shutdown() {
                tracing::debug!("mDNS broadcaster shutdown during drop: {e}");
            }
        }
    }
}

/// mDNS service listener.
///
/// Discovers LocalDrop shares advertised via mDNS on the local network.
pub struct MdnsListener {
    /// The mDNS daemon (wrapped in Option to support Drop)
    daemon: Option<ServiceDaemon>,
    /// Receiver for service events
    receiver: flume::Receiver<ServiceEvent>,
}

impl MdnsListener {
    /// Create a new mDNS listener.
    ///
    /// # Errors
    ///
    /// Returns an error if the mDNS daemon cannot be created.
    pub fn new() -> Result<Self> {
        let daemon =
            ServiceDaemon::new().map_err(|e| Error::Internal(format!("mDNS daemon error: {e}")))?;

        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| Error::Internal(format!("Failed to browse mDNS services: {e}")))?;

        Ok(Self {
            daemon: Some(daemon),
            receiver,
        })
    }

    /// Find a share by code.
    ///
    /// # Arguments
    ///
    /// * `code` - The share code to search for
    /// * `timeout` - Maximum time to wait
    ///
    /// # Errors
    ///
    /// Returns an error if the share is not found within the timeout.
    pub async fn find(&self, code: &ShareCode, timeout: Duration) -> Result<MdnsDiscoveredShare> {
        let code_str = code.as_str();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::CodeNotFound(code_str.to_string()));
            }

            // Use tokio timeout for async compatibility
            let result =
                tokio::time::timeout(remaining, async { self.receiver.recv_async().await }).await;

            match result {
                Ok(Ok(event)) => {
                    if let ServiceEvent::ServiceResolved(info) = event {
                        if let Some(discovered) = MdnsDiscoveredShare::from_service_info(&info) {
                            if discovered.code == code_str {
                                tracing::info!(
                                    code = %code_str,
                                    device = %discovered.device_name,
                                    "Found share via mDNS"
                                );
                                return Ok(discovered);
                            }
                        }
                    }
                }
                // Channel closed or timeout - share not found
                Ok(Err(_)) | Err(_) => {
                    return Err(Error::CodeNotFound(code_str.to_string()));
                }
            }
        }
    }

    /// Scan for all available shares.
    ///
    /// # Arguments
    ///
    /// * `duration` - How long to scan
    ///
    /// # Returns
    ///
    /// A list of discovered shares.
    pub async fn scan(&self, duration: Duration) -> Vec<MdnsDiscoveredShare> {
        let mut discovered = HashMap::new();
        let deadline = tokio::time::Instant::now() + duration;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            let result =
                tokio::time::timeout(remaining, async { self.receiver.recv_async().await }).await;

            match result {
                Ok(Ok(event)) => {
                    if let ServiceEvent::ServiceResolved(info) = event {
                        if let Some(share) = MdnsDiscoveredShare::from_service_info(&info) {
                            discovered.insert(share.code.clone(), share);
                        }
                    }
                }
                // Channel closed or timeout
                Ok(Err(_)) | Err(_) => break,
            }
        }

        discovered.into_values().collect()
    }

    /// Stop browsing for services.
    ///
    /// This should be called before shutdown to properly clean up the browse operation.
    fn stop_browsing(&self) {
        if let Some(ref daemon) = self.daemon {
            if let Err(e) = daemon.stop_browse(SERVICE_TYPE) {
                tracing::debug!("Failed to stop mDNS browse: {e}");
            }
        }
    }

    /// Shutdown the listener.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown fails.
    pub fn shutdown(mut self) -> Result<()> {
        // Stop browsing first to properly clean up
        self.stop_browsing();

        if let Some(daemon) = self.daemon.take() {
            daemon
                .shutdown()
                .map_err(|e| Error::Internal(format!("Failed to shutdown mDNS daemon: {e}")))?;
        }
        Ok(())
    }
}

impl Drop for MdnsListener {
    fn drop(&mut self) {
        // Stop browsing first to prevent "sending on a closed channel" error
        if let Some(ref daemon) = self.daemon {
            if let Err(e) = daemon.stop_browse(SERVICE_TYPE) {
                tracing::debug!("Failed to stop mDNS browse during drop: {e}");
            }
        }

        if let Some(daemon) = self.daemon.take() {
            if let Err(e) = daemon.shutdown() {
                tracing::debug!("mDNS listener shutdown during drop: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdns_properties_to_txt() {
        let props = MdnsProperties {
            code: "TEST-123".to_string(),
            device_name: "TestDevice".to_string(),
            device_id: Uuid::nil(),
            transfer_port: 52530,
            file_count: 5,
            total_size: 1_024_000,
            protocol_version: "1.0".to_string(),
        };

        let txt = props.to_txt_properties();
        assert_eq!(txt.len(), 6);

        // Check that code is in the properties
        let code_prop = txt.iter().find(|(k, _)| *k == txt_keys::CODE);
        assert!(code_prop.is_some());
        assert_eq!(code_prop.unwrap().1, "TEST-123");
    }

    #[test]
    fn test_service_type_format() {
        // Verify service type ends with .local.
        assert!(SERVICE_TYPE.ends_with(".local."));
        assert!(SERVICE_TYPE.starts_with("_localdrop._tcp"));
    }
}
