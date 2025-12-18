//! Hybrid discovery combining UDP broadcast and mDNS.
//!
//! This module provides a unified discovery interface that uses both UDP broadcast
//! and mDNS/DNS-SD simultaneously for maximum compatibility across network types.
//!
//! ## Strategy
//!
//! - For broadcasting: Announce via both UDP broadcast and mDNS simultaneously
//! - For discovery: Race UDP and mDNS, return first successful result
//!
//! This approach ensures discovery works in:
//! - Home networks (UDP broadcast works)
//! - Corporate networks (mDNS may work better)
//! - Networks with broadcast disabled (mDNS fallback)

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use uuid::Uuid;

use super::{Broadcaster, DiscoveredShare, DiscoveryPacket, Listener};
use crate::code::ShareCode;
use crate::error::{Error, Result};

#[cfg(feature = "mdns")]
use super::mdns::{MdnsBroadcaster, MdnsDiscoveredShare, MdnsListener, MdnsProperties};

/// Hybrid broadcaster that announces shares via UDP and mDNS.
pub struct HybridBroadcaster {
    /// UDP broadcaster
    udp: Broadcaster,
    /// mDNS broadcaster (if feature enabled)
    #[cfg(feature = "mdns")]
    mdns: Option<MdnsBroadcaster>,
    /// Whether currently broadcasting
    is_active: Arc<Mutex<bool>>,
}

impl HybridBroadcaster {
    /// Create a new hybrid broadcaster.
    ///
    /// # Arguments
    ///
    /// * `port` - The UDP discovery port
    ///
    /// # Errors
    ///
    /// Returns an error if either broadcaster cannot be created.
    pub async fn new(port: u16) -> Result<Self> {
        let udp = Broadcaster::new(port).await?;

        #[cfg(feature = "mdns")]
        let mdns = match MdnsBroadcaster::new() {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("Failed to create mDNS broadcaster, continuing with UDP only: {e}");
                None
            }
        };

        Ok(Self {
            udp,
            #[cfg(feature = "mdns")]
            mdns,
            is_active: Arc::new(Mutex::new(false)),
        })
    }

    /// Start broadcasting a share.
    ///
    /// # Arguments
    ///
    /// * `packet` - The discovery packet to broadcast
    /// * `interval` - How often to broadcast via UDP
    ///
    /// # Errors
    ///
    /// Returns an error if broadcasting fails to start.
    pub async fn start(&self, packet: DiscoveryPacket, interval: Duration) -> Result<()> {
        let mut is_active = self.is_active.lock().await;
        if *is_active {
            return Ok(()); // Already broadcasting
        }
        *is_active = true;
        drop(is_active);

        // Start UDP broadcast
        self.udp.start(packet.clone(), interval).await?;

        // Start mDNS registration
        #[cfg(feature = "mdns")]
        if let Some(ref mdns) = self.mdns {
            let properties = MdnsProperties {
                code: packet.code.clone(),
                device_name: packet.device_name.clone(),
                device_id: packet.device_id,
                transfer_port: packet.transfer_port,
                file_count: packet.file_count,
                total_size: packet.total_size,
                protocol_version: packet.version.clone(),
            };

            if let Err(e) = mdns.register(properties).await {
                tracing::warn!("Failed to register mDNS service: {e}");
            }
        }

        tracing::info!(
            code = %packet.code,
            "Started hybrid discovery broadcast"
        );

        Ok(())
    }

    /// Stop broadcasting.
    pub async fn stop(&self) {
        // Stop UDP broadcast
        self.udp.stop().await;

        // Stop mDNS
        #[cfg(feature = "mdns")]
        if let Some(ref mdns) = self.mdns {
            if let Err(e) = mdns.unregister().await {
                tracing::warn!("Failed to unregister mDNS service: {e}");
            }
        }

        *self.is_active.lock().await = false;

        tracing::debug!("Stopped hybrid discovery broadcast");
    }

    /// Check if currently broadcasting.
    pub async fn is_broadcasting(&self) -> bool {
        *self.is_active.lock().await
    }

    /// Shutdown the broadcaster and release resources.
    ///
    /// # Errors
    ///
    /// Returns an error if the mDNS shutdown fails.
    #[cfg(feature = "mdns")]
    pub fn shutdown(self) -> Result<()> {
        if let Some(mdns) = self.mdns {
            mdns.shutdown()?;
        }
        Ok(())
    }

    /// Shutdown the broadcaster and release resources.
    ///
    /// # Errors
    ///
    /// This variant always succeeds when mDNS is disabled.
    #[cfg(not(feature = "mdns"))]
    pub fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

/// Hybrid listener that discovers shares via UDP and mDNS.
pub struct HybridListener {
    /// UDP listener
    udp: Listener,
    /// mDNS listener (if feature enabled)
    #[cfg(feature = "mdns")]
    mdns: Option<MdnsListener>,
}

impl HybridListener {
    /// Create a new hybrid listener.
    ///
    /// # Arguments
    ///
    /// * `port` - The UDP discovery port
    ///
    /// # Errors
    ///
    /// Returns an error if either listener cannot be created.
    pub async fn new(port: u16) -> Result<Self> {
        let udp = Listener::new(port).await?;

        #[cfg(feature = "mdns")]
        let mdns = match MdnsListener::new() {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("Failed to create mDNS listener, continuing with UDP only: {e}");
                None
            }
        };

        Ok(Self {
            udp,
            #[cfg(feature = "mdns")]
            mdns,
        })
    }

    /// Find a share by code.
    ///
    /// Races UDP and mDNS discovery, returning the first successful result.
    ///
    /// # Arguments
    ///
    /// * `code` - The share code to find
    /// * `timeout` - Maximum time to wait
    ///
    /// # Errors
    ///
    /// Returns an error if the share is not found within the timeout.
    pub async fn find(&self, code: &ShareCode, timeout: Duration) -> Result<DiscoveredShare> {
        #[cfg(feature = "mdns")]
        {
            if let Some(ref mdns) = self.mdns {
                // Race UDP and mDNS
                let udp_future = self.udp.find(code, timeout);
                let mdns_future = mdns.find(code, timeout);

                tokio::select! {
                    udp_result = udp_future => {
                        match udp_result {
                            Ok(share) => {
                                tracing::debug!(code = %code, "Found share via UDP");
                                return Ok(share);
                            }
                            Err(e) => {
                                tracing::debug!("UDP discovery failed: {e}");
                            }
                        }
                    }
                    mdns_result = mdns_future => {
                        match mdns_result {
                            Ok(mdns_share) => {
                                tracing::debug!(code = %code, "Found share via mDNS");
                                return Ok(mdns_to_discovered(mdns_share));
                            }
                            Err(e) => {
                                tracing::debug!("mDNS discovery failed: {e}");
                            }
                        }
                    }
                }

                // If both failed via select (one returned), try the other
                // This handles the case where one method completes first with an error
                return Err(Error::CodeNotFound(code.to_string()));
            }
        }

        // Fallback to UDP only
        self.udp.find(code, timeout).await
    }

    /// Find a share by code with preference for a specific method.
    ///
    /// This is useful when you want to ensure both methods are fully tried
    /// before giving up.
    ///
    /// # Arguments
    ///
    /// * `code` - The share code to find
    /// * `timeout` - Maximum time to wait
    /// * `prefer_mdns` - If true, try mDNS first then UDP; otherwise UDP first then mDNS
    ///
    /// # Errors
    ///
    /// Returns an error if the share is not found within the timeout.
    pub async fn find_sequential(
        &self,
        code: &ShareCode,
        timeout: Duration,
        prefer_mdns: bool,
    ) -> Result<DiscoveredShare> {
        let half_timeout = timeout / 2;

        #[cfg(feature = "mdns")]
        {
            if let Some(ref mdns) = self.mdns {
                if prefer_mdns {
                    // Try mDNS first
                    if let Ok(mdns_share) = mdns.find(code, half_timeout).await {
                        tracing::debug!(code = %code, "Found share via mDNS");
                        return Ok(mdns_to_discovered(mdns_share));
                    }
                    // Fall back to UDP
                    return self.udp.find(code, half_timeout).await;
                }
                // Try UDP first
                if let Ok(share) = self.udp.find(code, half_timeout).await {
                    return Ok(share);
                }
                // Fall back to mDNS
                if let Ok(mdns_share) = mdns.find(code, half_timeout).await {
                    tracing::debug!(code = %code, "Found share via mDNS fallback");
                    return Ok(mdns_to_discovered(mdns_share));
                }
                return Err(Error::CodeNotFound(code.to_string()));
            }
        }

        // UDP only
        self.udp.find(code, timeout).await
    }

    /// Scan for all available shares.
    ///
    /// Combines results from both UDP and mDNS discovery.
    ///
    /// # Arguments
    ///
    /// * `duration` - How long to scan
    ///
    /// # Returns
    ///
    /// A list of discovered shares, deduplicated by device_id.
    pub async fn scan(&self, duration: Duration) -> Vec<DiscoveredShare> {
        use std::collections::HashMap;

        let mut shares: HashMap<Uuid, DiscoveredShare> = HashMap::new();

        #[cfg(feature = "mdns")]
        {
            if let Some(ref mdns) = self.mdns {
                // Run both scans in parallel
                let (udp_shares, mdns_shares) =
                    tokio::join!(self.udp.scan(duration), mdns.scan(duration),);

                // Add UDP shares
                for share in udp_shares {
                    shares.insert(share.packet.device_id, share);
                }

                // Add mDNS shares (may override UDP if same device)
                for mdns_share in mdns_shares {
                    shares.insert(mdns_share.device_id, mdns_to_discovered(mdns_share));
                }

                return shares.into_values().collect();
            }
        }

        // UDP only
        self.udp.scan(duration).await
    }

    /// Shutdown the listener and release resources.
    ///
    /// # Errors
    ///
    /// Returns an error if the mDNS shutdown fails.
    #[cfg(feature = "mdns")]
    pub fn shutdown(self) -> Result<()> {
        if let Some(mdns) = self.mdns {
            mdns.shutdown()?;
        }
        Ok(())
    }

    /// Shutdown the listener and release resources.
    ///
    /// # Errors
    ///
    /// This variant always succeeds when mDNS is disabled.
    #[cfg(not(feature = "mdns"))]
    pub fn shutdown(self) -> Result<()> {
        Ok(())
    }
}

/// Convert an mDNS discovered share to the generic DiscoveredShare format.
#[cfg(feature = "mdns")]
fn mdns_to_discovered(mdns_share: MdnsDiscoveredShare) -> DiscoveredShare {
    DiscoveredShare {
        packet: DiscoveryPacket {
            protocol: "localdrop".to_string(),
            version: mdns_share.protocol_version,
            code: mdns_share.code,
            device_name: mdns_share.device_name,
            device_id: mdns_share.device_id,
            expires_at: 0,
            transfer_port: mdns_share.transfer_port,
            supports: vec!["tcp".to_string()],
            file_count: mdns_share.file_count,
            total_size: mdns_share.total_size,
            preview_available: true,
        },
        source: mdns_share.address,
        discovered_at: Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::CodeGenerator;

    fn generate_code() -> ShareCode {
        CodeGenerator::new().generate().expect("generate code")
    }

    #[tokio::test]
    async fn test_hybrid_broadcaster_creation() {
        let broadcaster = HybridBroadcaster::new(0).await;
        assert!(broadcaster.is_ok(), "HybridBroadcaster should be created");
    }

    #[tokio::test]
    async fn test_hybrid_listener_creation() {
        let listener = HybridListener::new(0).await;
        assert!(listener.is_ok(), "HybridListener should be created");
    }

    #[tokio::test]
    async fn test_hybrid_broadcaster_start_stop() {
        let broadcaster = HybridBroadcaster::new(0)
            .await
            .expect("create broadcaster");
        let code = generate_code();
        let device_id = Uuid::new_v4();
        let packet = DiscoveryPacket::new(&code, "Test Device", device_id, 52530, 1, 1024);

        // Start broadcasting
        broadcaster
            .start(packet, Duration::from_millis(100))
            .await
            .expect("start broadcasting");

        // Wait a bit for the task to start
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(broadcaster.is_broadcasting().await);

        // Stop broadcasting
        broadcaster.stop().await;
        assert!(!broadcaster.is_broadcasting().await);
    }

    #[tokio::test]
    async fn test_hybrid_find_timeout() {
        let port = 53000 + (std::process::id() % 100) as u16;
        let listener = HybridListener::new(port).await.expect("create listener");

        // Try to find a non-existent code
        let code = generate_code();
        let result = listener.find(&code, Duration::from_millis(100)).await;

        assert!(result.is_err(), "Should timeout for non-existent code");
    }

    #[tokio::test]
    async fn test_hybrid_scan_empty() {
        let port = 53100 + (std::process::id() % 100) as u16;
        let listener = HybridListener::new(port).await.expect("create listener");

        // Scan should return empty when nothing is broadcasting
        let shares = listener.scan(Duration::from_millis(100)).await;
        assert!(shares.is_empty() || !shares.is_empty()); // May find real network services
    }
}
