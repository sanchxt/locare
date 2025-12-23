//! Tests for mDNS/DNS-SD discovery.
//!
//! These tests verify the mDNS service registration, discovery, and hybrid
//! discovery mechanisms.

use std::time::Duration;

use uuid::Uuid;

use localdrop_core::code::CodeGenerator;
use localdrop_core::discovery::{
    mdns::{MdnsBroadcaster, MdnsListener, MdnsProperties, SERVICE_TYPE},
    HybridBroadcaster, HybridListener,
};

/// Test that `MdnsProperties` can be converted to TXT properties correctly.
#[test]
fn test_mdns_properties_to_txt_properties() {
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

    let code_prop = txt.iter().find(|(k, _)| *k == "code");
    assert!(code_prop.is_some());
    assert_eq!(code_prop.unwrap().1, "TEST-123");

    let name_prop = txt.iter().find(|(k, _)| *k == "device_name");
    assert!(name_prop.is_some());
    assert_eq!(name_prop.unwrap().1, "TestDevice");

    let id_prop = txt.iter().find(|(k, _)| *k == "device_id");
    assert!(id_prop.is_some());
    assert_eq!(id_prop.unwrap().1, Uuid::nil().to_string());

    let count_prop = txt.iter().find(|(k, _)| *k == "file_count");
    assert!(count_prop.is_some());
    assert_eq!(count_prop.unwrap().1, "5");

    let size_prop = txt.iter().find(|(k, _)| *k == "total_size");
    assert!(size_prop.is_some());
    assert_eq!(size_prop.unwrap().1, "1024000");

    let version_prop = txt.iter().find(|(k, _)| *k == "version");
    assert!(version_prop.is_some());
    assert_eq!(version_prop.unwrap().1, "1.0");
}

/// Test the mDNS service type format.
#[test]
fn test_mdns_service_type_format() {
    assert!(SERVICE_TYPE.starts_with("_localdrop"));
    assert!(SERVICE_TYPE.contains("._tcp"));
    assert!(SERVICE_TYPE.ends_with(".local."));
}

/// Test creating an mDNS broadcaster.
#[test]
fn test_mdns_broadcaster_creation() {
    let result = MdnsBroadcaster::new();

    if result.is_err() {
        eprintln!(
            "mDNS broadcaster creation failed (may be expected in CI): {:?}",
            result.err()
        );
    }
}

/// Test mDNS service registration lifecycle.
#[tokio::test]
async fn test_mdns_service_registration_lifecycle() {
    let broadcaster = match MdnsBroadcaster::new() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Skipping test: mDNS not available ({e})");
            return;
        }
    };

    let props = MdnsProperties {
        code: "REG-TEST".to_string(),
        device_name: "RegistrationTest".to_string(),
        device_id: Uuid::new_v4(),
        transfer_port: 52531,
        file_count: 1,
        total_size: 1024,
        protocol_version: "1.0".to_string(),
    };

    let result = broadcaster.register(props).await;
    assert!(result.is_ok(), "Registration should succeed: {result:?}");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let result = broadcaster.unregister().await;
    assert!(result.is_ok(), "Unregistration should succeed: {result:?}");

    let result = broadcaster.shutdown();
    assert!(result.is_ok(), "Shutdown should succeed: {result:?}");
}

/// Test creating an mDNS listener.
#[test]
fn test_mdns_listener_creation() {
    let result = MdnsListener::new();

    if result.is_err() {
        eprintln!(
            "mDNS listener creation failed (may be expected in CI): {:?}",
            result.err()
        );
    }
}

/// Test mDNS listener scan returns (possibly empty) results.
#[tokio::test]
async fn test_mdns_listener_scan() {
    let listener = match MdnsListener::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Skipping test: mDNS not available ({e})");
            return;
        }
    };

    let shares = listener.scan(Duration::from_millis(200)).await;

    eprintln!("Found {} shares during scan", shares.len());

    let _ = listener.shutdown();
}

/// Test mDNS find with timeout returns `CodeNotFound`.
#[tokio::test]
async fn test_mdns_listener_find_timeout() {
    let listener = match MdnsListener::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Skipping test: mDNS not available ({e})");
            return;
        }
    };

    let code = CodeGenerator::new().generate().expect("generate code");
    let result = listener.find(&code, Duration::from_millis(200)).await;

    assert!(result.is_err(), "Should timeout for non-existent code");

    let _ = listener.shutdown();
}

/// Test hybrid broadcaster creation.
#[tokio::test]
async fn test_hybrid_broadcaster_creation() {
    let result = HybridBroadcaster::new(0).await;
    assert!(result.is_ok(), "HybridBroadcaster should be created");
}

/// Test hybrid listener creation.
#[tokio::test]
async fn test_hybrid_listener_creation() {
    let result = HybridListener::new(0).await;
    assert!(result.is_ok(), "HybridListener should be created");
}

/// Test hybrid broadcaster start/stop lifecycle.
#[tokio::test]
async fn test_hybrid_broadcaster_lifecycle() {
    let broadcaster = HybridBroadcaster::new(0).await.expect("create broadcaster");
    let code = CodeGenerator::new().generate().expect("generate code");
    let device_id = Uuid::new_v4();

    let packet = localdrop_core::discovery::DiscoveryPacket::new(
        &code,
        "HybridTest",
        device_id,
        52532,
        2,
        2048,
    );

    broadcaster
        .start(packet, Duration::from_millis(100))
        .await
        .expect("start broadcasting");

    assert!(broadcaster.is_broadcasting().await);

    tokio::time::sleep(Duration::from_millis(250)).await;

    broadcaster.stop().await;
    assert!(!broadcaster.is_broadcasting().await);

    let result = broadcaster.shutdown();
    assert!(result.is_ok());
}

/// Test hybrid listener find with timeout.
#[tokio::test]
async fn test_hybrid_listener_find_timeout() {
    let port = 53200 + (std::process::id() % 100) as u16;
    let listener = HybridListener::new(port).await.expect("create listener");

    let code = CodeGenerator::new().generate().expect("generate code");
    let result = listener.find(&code, Duration::from_millis(200)).await;

    assert!(result.is_err(), "Should timeout for non-existent code");
}

/// Test hybrid listener scan.
#[tokio::test]
async fn test_hybrid_listener_scan() {
    let port = 53300 + (std::process::id() % 100) as u16;
    let listener = HybridListener::new(port).await.expect("create listener");

    let shares = listener.scan(Duration::from_millis(100)).await;
    eprintln!("Hybrid scan found {} shares", shares.len());
}

/// Test hybrid listener sequential find with UDP preference.
#[tokio::test]
async fn test_hybrid_listener_sequential_udp_first() {
    let port = 53400 + (std::process::id() % 100) as u16;
    let listener = HybridListener::new(port).await.expect("create listener");

    let code = CodeGenerator::new().generate().expect("generate code");
    let result = listener
        .find_sequential(&code, Duration::from_millis(200), false)
        .await;

    assert!(result.is_err(), "Should timeout for non-existent code");
}

/// Test hybrid listener sequential find with mDNS preference.
#[tokio::test]
async fn test_hybrid_listener_sequential_mdns_first() {
    let port = 53500 + (std::process::id() % 100) as u16;
    let listener = HybridListener::new(port).await.expect("create listener");

    let code = CodeGenerator::new().generate().expect("generate code");
    let result = listener
        .find_sequential(&code, Duration::from_millis(200), true)
        .await;

    assert!(result.is_err(), "Should timeout for non-existent code");
}

/// Integration test: register service and discover it.
/// This test is ignored by default as it requires real mDNS network support.
#[tokio::test]
#[ignore = "Requires real mDNS network support"]
async fn test_mdns_registration_discovery_roundtrip() {
    let broadcaster = MdnsBroadcaster::new().expect("create broadcaster");

    let code = "E2E-TEST";
    let device_id = Uuid::new_v4();

    let props = MdnsProperties {
        code: code.to_string(),
        device_name: "E2ETestDevice".to_string(),
        device_id,
        transfer_port: 52533,
        file_count: 3,
        total_size: 4096,
        protocol_version: "1.0".to_string(),
    };

    broadcaster.register(props).await.expect("register");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let listener = MdnsListener::new().expect("create listener");
    let share_code = localdrop_core::code::ShareCode::parse(code).expect("parse code");

    let result = listener.find(&share_code, Duration::from_secs(5)).await;

    broadcaster.unregister().await.expect("unregister");
    broadcaster.shutdown().expect("shutdown broadcaster");
    listener.shutdown().expect("shutdown listener");

    let discovered = result.expect("should find service");
    assert_eq!(discovered.code, code);
    assert_eq!(discovered.device_name, "E2ETestDevice");
    assert_eq!(discovered.device_id, device_id);
    assert_eq!(discovered.transfer_port, 52533);
    assert_eq!(discovered.file_count, 3);
    assert_eq!(discovered.total_size, 4096);
}

/// Integration test: hybrid discovery with both methods.
/// This test is ignored by default as it requires real network support.
#[tokio::test]
#[ignore = "Requires real network support"]
async fn test_hybrid_discovery_roundtrip() {
    let port = 53600;

    let broadcaster = HybridBroadcaster::new(port)
        .await
        .expect("create broadcaster");

    let code = CodeGenerator::new().generate().expect("generate code");
    let device_id = Uuid::new_v4();

    let packet = localdrop_core::discovery::DiscoveryPacket::new(
        &code,
        "HybridE2ETest",
        device_id,
        52534,
        1,
        1024,
    );

    broadcaster
        .start(packet.clone(), Duration::from_millis(100))
        .await
        .expect("start");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let listener = HybridListener::new(port).await.expect("create listener");
    let result = listener.find(&code, Duration::from_secs(5)).await;

    broadcaster.stop().await;
    broadcaster.shutdown().expect("shutdown broadcaster");
    listener.shutdown().expect("shutdown listener");

    let discovered = result.expect("should find service");
    assert_eq!(discovered.packet.code, code.to_string());
    assert_eq!(discovered.packet.device_name, "HybridE2ETest");
    assert_eq!(discovered.packet.device_id, device_id);
}
