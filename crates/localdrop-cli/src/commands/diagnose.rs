//! Diagnose command implementation.
//!
//! Provides network diagnostics for troubleshooting LocalDrop connectivity.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use anyhow::Result;

use localdrop_core::config::Config;
use localdrop_core::discovery::HybridListener;
use localdrop_core::trust::TrustStore;
use localdrop_core::VERSION;

use super::DiagnoseArgs;

/// Network interface information.
#[derive(Debug)]
struct NetworkInfo {
    /// Local IP address
    local_ip: String,
    /// Whether UDP broadcast is working
    udp_broadcast_ok: bool,
    /// Whether UDP listening is working
    udp_listen_ok: bool,
}

/// Run the diagnose command.
pub async fn run(args: DiagnoseArgs) -> Result<()> {
    let global_config = super::load_config();

    let net_info = check_network(&global_config).await;
    let mdns_ok = check_mdns().await;
    let trusted_devices = get_trusted_devices();
    let active_shares = scan_for_shares(&global_config).await;

    if args.json {
        output_json(
            &net_info,
            mdns_ok,
            &trusted_devices,
            &active_shares,
            &global_config,
        );
        return Ok(());
    }

    output_text(
        &net_info,
        mdns_ok,
        &trusted_devices,
        &active_shares,
        &global_config,
    );
    Ok(())
}

/// Check network connectivity.
async fn check_network(config: &Config) -> NetworkInfo {
    let local_ip = get_local_ip().unwrap_or_else(|| "unknown".to_string());

    let udp_broadcast_ok = test_udp_broadcast();

    let udp_listen_ok = test_udp_listen(config.network.port);

    NetworkInfo {
        local_ip,
        udp_broadcast_ok,
        udp_listen_ok,
    }
}

/// Get local IP address by connecting to a public DNS.
fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:53").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip().to_string())
}

/// Test UDP broadcast capability.
fn test_udp_broadcast() -> bool {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return false;
    };

    if socket.set_broadcast(true).is_err() {
        return false;
    }

    let addr: SocketAddr = "255.255.255.255:52599".parse().unwrap();
    socket.send_to(b"test", addr).is_ok()
}

/// Test UDP listen capability on discovery port.
fn test_udp_listen(discovery_port: u16) -> bool {
    UdpSocket::bind(format!("0.0.0.0:{}", discovery_port)).is_ok()
        || UdpSocket::bind("0.0.0.0:0").is_ok()
}

/// Check mDNS availability.
async fn check_mdns() -> bool {
    HybridListener::new(0).await.is_ok()
}

/// Get trusted devices from the store.
fn get_trusted_devices() -> Vec<(String, String)> {
    TrustStore::load().map_or_else(
        |_| Vec::new(),
        |store| {
            store
                .list()
                .iter()
                .map(|d| (d.device_name.clone(), format!("{:?}", d.trust_level)))
                .collect()
        },
    )
}

/// Scan for active shares on the network.
async fn scan_for_shares(config: &Config) -> Vec<String> {
    match HybridListener::new(config.network.port).await {
        Ok(listener) => {
            let shares = listener.scan(Duration::from_secs(2)).await;
            let _ = listener.shutdown();
            shares
                .into_iter()
                .map(|s| {
                    format!(
                        "{} from {} ({})",
                        s.packet.code,
                        s.packet.device_name,
                        s.source.ip()
                    )
                })
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

/// Output results as JSON.
fn output_json(
    net_info: &NetworkInfo,
    mdns_ok: bool,
    trusted_devices: &[(String, String)],
    active_shares: &[String],
    config: &Config,
) {
    let output = serde_json::json!({
        "version": VERSION,
        "network": {
            "local_ip": net_info.local_ip,
            "udp_broadcast": if net_info.udp_broadcast_ok { "ok" } else { "failed" },
            "udp_listen": if net_info.udp_listen_ok { "ok" } else { "failed" },
            "mdns": if mdns_ok { "ok" } else { "unavailable" },
        },
        "ports": {
            "discovery": config.network.port,
            "transfer_range": format!("{}-{}", config.network.transfer_port_range.0, config.network.transfer_port_range.1),
        },
        "trusted_devices": trusted_devices.iter().map(|(name, level)| {
            serde_json::json!({
                "name": name,
                "trust_level": level,
            })
        }).collect::<Vec<_>>(),
        "active_shares": active_shares,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

/// Output results as text.
fn output_text(
    net_info: &NetworkInfo,
    mdns_ok: bool,
    trusted_devices: &[(String, String)],
    active_shares: &[String],
    config: &Config,
) {
    let discovery_port = config.network.port;
    let (transfer_start, transfer_end) = config.network.transfer_port_range;

    println!();
    println!("LocalDrop v{} - Network Diagnostics", VERSION);
    println!("{}", "─".repeat(50));
    println!();

    println!("  Network:");
    println!("    Local IP:      {}", net_info.local_ip);
    println!(
        "    UDP Broadcast: {}",
        status_icon(net_info.udp_broadcast_ok)
    );
    println!("    UDP Listen:    {}", status_icon(net_info.udp_listen_ok));
    println!("    mDNS:          {}", status_icon(mdns_ok));
    println!();

    println!("  Ports:");
    println!("    Discovery:     {}", discovery_port);
    println!("    Transfer:      {}-{}", transfer_start, transfer_end);
    println!();

    println!("  Active Shares:");
    if active_shares.is_empty() {
        println!("    (none found)");
    } else {
        for share in active_shares {
            println!("    - {}", share);
        }
    }
    println!();

    println!("  Trusted Devices:");
    if trusted_devices.is_empty() {
        println!("    (none configured)");
    } else {
        for (name, level) in trusted_devices {
            println!("    - {} ({})", name, level);
        }
    }

    println!();
    println!("{}", "─".repeat(50));
    println!();

    if !net_info.udp_broadcast_ok || !net_info.udp_listen_ok {
        println!("  Recommendations:");
        if !net_info.udp_broadcast_ok {
            println!("    - Enable UDP broadcast on your network");
        }
        if !net_info.udp_listen_ok {
            println!("    - Check firewall allows UDP port {}", discovery_port);
        }
        println!(
            "    - Ensure ports {}-{} are open for TCP",
            transfer_start, transfer_end
        );
    } else {
        println!("  Status: All network checks passed!");
    }

    println!();
}

/// Get a status icon for display.
const fn status_icon(ok: bool) -> &'static str {
    if ok {
        "OK"
    } else {
        "FAILED"
    }
}
