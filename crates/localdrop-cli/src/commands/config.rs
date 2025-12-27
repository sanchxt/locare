//! Config command implementation.

use anyhow::Result;

use super::{ConfigAction, ConfigArgs};

/// Run the config command.
#[allow(clippy::too_many_lines)]
pub async fn run(args: ConfigArgs) -> Result<()> {
    let mut config = localdrop_core::config::Config::load()?;

    match args.action {
        ConfigAction::Get { key } => {
            let value = get_config_value(&config, &key);
            match value {
                Some(v) => println!("{}: {}", key, v),
                None => println!("Unknown configuration key: {}", key),
            }
        }

        ConfigAction::Set { key, value } => {
            if set_config_value(&mut config, &key, &value)? {
                config.save()?;
                println!("Set {} = {}", key, value);
            } else {
                println!("Unknown configuration key: {}", key);
            }
        }

        ConfigAction::Show => {
            println!();
            println!("LocalDrop Configuration");
            println!("{}", "─".repeat(50));
            println!();

            // [general]
            println!("[general]");
            println!("  device_name = \"{}\"", config.general.device_name);
            println!(
                "  default_expire = \"{}s\"",
                config.general.default_expire.as_secs()
            );
            println!(
                "  default_output = {}",
                config
                    .general
                    .default_output
                    .as_ref()
                    .map_or_else(|| "none".to_string(), |p| format!("\"{}\"", p.display()))
            );
            println!();

            // [network]
            println!("[network]");
            println!("  port = {}", config.network.port);
            println!(
                "  transfer_port_range = \"{}-{}\"",
                config.network.transfer_port_range.0, config.network.transfer_port_range.1
            );
            println!("  interface = \"{}\"", config.network.interface);
            println!("  ipv6 = {}", config.network.ipv6);
            println!();

            // [transfer]
            println!("[transfer]");
            println!("  chunk_size = {}", config.transfer.chunk_size);
            println!("  parallel_chunks = {}", config.transfer.parallel_chunks);
            println!(
                "  bandwidth_limit = {}",
                config
                    .transfer
                    .bandwidth_limit
                    .map_or_else(|| "unlimited".to_string(), |b| b.to_string())
            );
            println!(
                "  compression = \"{}\"",
                format!("{:?}", config.transfer.compression).to_lowercase()
            );
            println!("  verify_checksum = {}", config.transfer.verify_checksum);
            println!();

            // [security]
            println!("[security]");
            println!("  require_pin = {}", config.security.require_pin);
            println!("  require_approval = {}", config.security.require_approval);
            println!("  tls_verify = {}", config.security.tls_verify);
            println!(
                "  rate_limit_attempts = {}",
                config.security.rate_limit_attempts
            );
            println!(
                "  rate_limit_window = \"{}s\"",
                config.security.rate_limit_window.as_secs()
            );
            println!();

            // [preview]
            println!("[preview]");
            println!("  enabled = {}", config.preview.enabled);
            println!("  max_image_size = {}", config.preview.max_image_size);
            println!("  max_text_length = {}", config.preview.max_text_length);
            println!();

            // [history]
            println!("[history]");
            println!("  enabled = {}", config.history.enabled);
            println!("  max_entries = {}", config.history.max_entries);
            println!(
                "  auto_clear_days = {}",
                config
                    .history
                    .auto_clear_days
                    .map_or_else(|| "disabled".to_string(), |d| d.to_string())
            );
            println!();

            // [trust]
            println!("[trust]");
            println!("  enabled = {}", config.trust.enabled);
            println!("  auto_prompt = {}", config.trust.auto_prompt);
            println!(
                "  default_level = \"{}\"",
                format!("{:?}", config.trust.default_level).to_lowercase()
            );
            println!();

            // [web]
            println!("[web]");
            println!("  enabled = {}", config.web.enabled);
            println!("  port = {}", config.web.port);
            println!("  auth = {}", config.web.auth);
            println!("  localhost_only = {}", config.web.localhost_only);
            println!();

            // [ui]
            println!("[ui]");
            println!("  theme = \"{}\"", config.ui.theme);
            println!("  show_qr = {}", config.ui.show_qr);
            println!("  notifications = {}", config.ui.notifications);
            println!("  sound = {}", config.ui.sound);
            println!();
        }

        ConfigAction::List => {
            println!();
            println!("Available configuration keys:");
            println!("{}", "─".repeat(60));
            println!();
            println!("[general]");
            println!("  device_name         Display name on network");
            println!("  default_expire      Default code expiration (e.g., 5m, 10m, 1h)");
            println!("  default_output      Default download directory path");
            println!();
            println!("[network]");
            println!("  port                Discovery port (UDP)");
            println!("  transfer_port_range Transfer port range (e.g., 52530-52540)");
            println!("  interface           Network interface (auto or specific)");
            println!("  ipv6                Enable IPv6 (true/false)");
            println!();
            println!("[transfer]");
            println!("  chunk_size          Chunk size for transfers (e.g., 1MB, 512KB)");
            println!("  parallel_chunks     Number of parallel chunk streams");
            println!("  bandwidth_limit     Bandwidth limit (e.g., 50MB, unlimited)");
            println!("  compression         Compression mode (auto, always, never)");
            println!("  verify_checksum     Verify checksums after transfer (true/false)");
            println!();
            println!("[security]");
            println!("  require_pin         Require additional PIN (true/false)");
            println!("  require_approval    Require manual approval (true/false)");
            println!("  tls_verify          Verify TLS certificates (true/false)");
            println!("  rate_limit_attempts Failed attempts before lockout");
            println!("  rate_limit_window   Lockout duration (e.g., 30s, 1m)");
            println!();
            println!("[preview]");
            println!("  preview.enabled         Enable preview generation (true/false)");
            println!("  preview.max_image_size  Max image thumbnail size (e.g., 50KB)");
            println!("  preview.max_text_length Max text preview length (chars)");
            println!();
            println!("[history]");
            println!("  history.enabled         Enable transfer history (true/false)");
            println!("  history.max_entries     Maximum history entries");
            println!("  history.auto_clear_days Auto-clear after N days (or disabled)");
            println!();
            println!("[trust]");
            println!("  trust.enabled       Enable trusted devices (true/false)");
            println!("  trust.auto_prompt   Prompt to trust after transfer (true/false)");
            println!("  trust.default_level Default trust level (full, ask_each_time)");
            println!();
            println!("[web]");
            println!("  web.enabled         Enable web server by default (true/false)");
            println!("  web.port            Web server port");
            println!("  web.auth            Require authentication (true/false)");
            println!("  web.localhost_only  Bind to localhost only (true/false)");
            println!();
            println!("[ui]");
            println!("  ui.theme            Theme (auto, light, dark)");
            println!("  ui.show_qr          Show QR codes (true/false)");
            println!("  ui.notifications    Enable notifications (true/false)");
            println!("  ui.sound            Play sound on complete (true/false)");
            println!();
        }

        ConfigAction::Path => {
            println!(
                "{}",
                localdrop_core::config::Config::config_path().display()
            );
        }

        ConfigAction::Reset => {
            let config = localdrop_core::config::Config::default();
            config.save()?;
            println!("Configuration reset to defaults.");
        }
    }

    Ok(())
}

fn get_config_value(config: &localdrop_core::config::Config, key: &str) -> Option<String> {
    match key {
        // general
        "device_name" => Some(config.general.device_name.clone()),
        "default_expire" => Some(format!("{}s", config.general.default_expire.as_secs())),
        "default_output" => config
            .general
            .default_output
            .as_ref()
            .map(|p| p.display().to_string()),

        // network
        "port" => Some(config.network.port.to_string()),
        "transfer_port_range" => Some(format!(
            "{}-{}",
            config.network.transfer_port_range.0, config.network.transfer_port_range.1
        )),
        "interface" => Some(config.network.interface.clone()),
        "ipv6" => Some(config.network.ipv6.to_string()),

        // transfer
        "chunk_size" => Some(config.transfer.chunk_size.to_string()),
        "parallel_chunks" => Some(config.transfer.parallel_chunks.to_string()),
        "bandwidth_limit" => Some(
            config
                .transfer
                .bandwidth_limit
                .map_or_else(|| "unlimited".to_string(), |b| b.to_string()),
        ),
        "compression" => Some(format!("{:?}", config.transfer.compression).to_lowercase()),
        "verify_checksum" => Some(config.transfer.verify_checksum.to_string()),

        // security
        "require_pin" => Some(config.security.require_pin.to_string()),
        "require_approval" => Some(config.security.require_approval.to_string()),
        "tls_verify" => Some(config.security.tls_verify.to_string()),
        "rate_limit_attempts" => Some(config.security.rate_limit_attempts.to_string()),
        "rate_limit_window" => Some(format!("{}s", config.security.rate_limit_window.as_secs())),

        // preview
        "preview.enabled" => Some(config.preview.enabled.to_string()),
        "preview.max_image_size" => Some(config.preview.max_image_size.to_string()),
        "preview.max_text_length" => Some(config.preview.max_text_length.to_string()),

        // history
        "history.enabled" => Some(config.history.enabled.to_string()),
        "history.max_entries" => Some(config.history.max_entries.to_string()),
        "history.auto_clear_days" => Some(
            config
                .history
                .auto_clear_days
                .map_or_else(|| "disabled".to_string(), |d| d.to_string()),
        ),

        // trust
        "trust.enabled" => Some(config.trust.enabled.to_string()),
        "trust.auto_prompt" => Some(config.trust.auto_prompt.to_string()),
        "trust.default_level" => Some(format!("{:?}", config.trust.default_level).to_lowercase()),

        // web
        "web.enabled" => Some(config.web.enabled.to_string()),
        "web.port" => Some(config.web.port.to_string()),
        "web.auth" => Some(config.web.auth.to_string()),
        "web.localhost_only" => Some(config.web.localhost_only.to_string()),

        // ui
        "ui.theme" => Some(config.ui.theme.clone()),
        "ui.show_qr" => Some(config.ui.show_qr.to_string()),
        "ui.notifications" => Some(config.ui.notifications.to_string()),
        "ui.sound" => Some(config.ui.sound.to_string()),

        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn set_config_value(
    config: &mut localdrop_core::config::Config,
    key: &str,
    value: &str,
) -> Result<bool> {
    match key {
        // general
        "device_name" => {
            config.general.device_name = value.to_string();
            Ok(true)
        }
        "default_expire" => {
            config.general.default_expire = parse_duration(value)?;
            Ok(true)
        }
        "default_output" => {
            if value.is_empty() || value == "none" {
                config.general.default_output = None;
            } else {
                config.general.default_output = Some(std::path::PathBuf::from(value));
            }
            Ok(true)
        }

        // network
        "port" => {
            config.network.port = value.parse()?;
            Ok(true)
        }
        "transfer_port_range" => {
            let parts: Vec<&str> = value.split('-').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid port range format. Use: start-end (e.g., 52530-52540)");
            }
            let start: u16 = parts[0].parse()?;
            let end: u16 = parts[1].parse()?;
            config.network.transfer_port_range = (start, end);
            Ok(true)
        }
        "interface" => {
            config.network.interface = value.to_string();
            Ok(true)
        }
        "ipv6" => {
            config.network.ipv6 = value.parse()?;
            Ok(true)
        }

        // transfer
        "chunk_size" => {
            config.transfer.chunk_size = parse_size(value)?;
            Ok(true)
        }
        "parallel_chunks" => {
            config.transfer.parallel_chunks = value.parse()?;
            Ok(true)
        }
        "bandwidth_limit" => {
            if value == "unlimited" || value.is_empty() {
                config.transfer.bandwidth_limit = None;
            } else {
                config.transfer.bandwidth_limit = Some(parse_size(value)? as u64);
            }
            Ok(true)
        }
        "compression" => {
            config.transfer.compression = match value.to_lowercase().as_str() {
                "auto" => localdrop_core::config::CompressionMode::Auto,
                "always" => localdrop_core::config::CompressionMode::Always,
                "never" => localdrop_core::config::CompressionMode::Never,
                _ => anyhow::bail!("Invalid compression mode. Use: auto, always, or never"),
            };
            Ok(true)
        }
        "verify_checksum" => {
            config.transfer.verify_checksum = value.parse()?;
            Ok(true)
        }

        // security
        "require_pin" => {
            config.security.require_pin = value.parse()?;
            Ok(true)
        }
        "require_approval" => {
            config.security.require_approval = value.parse()?;
            Ok(true)
        }
        "tls_verify" => {
            config.security.tls_verify = value.parse()?;
            Ok(true)
        }
        "rate_limit_attempts" => {
            config.security.rate_limit_attempts = value.parse()?;
            Ok(true)
        }
        "rate_limit_window" => {
            config.security.rate_limit_window = parse_duration(value)?;
            Ok(true)
        }

        // preview
        "preview.enabled" => {
            config.preview.enabled = value.parse()?;
            Ok(true)
        }
        "preview.max_image_size" => {
            config.preview.max_image_size = parse_size(value)?;
            Ok(true)
        }
        "preview.max_text_length" => {
            config.preview.max_text_length = value.parse()?;
            Ok(true)
        }

        // history
        "history.enabled" => {
            config.history.enabled = value.parse()?;
            Ok(true)
        }
        "history.max_entries" => {
            config.history.max_entries = value.parse()?;
            Ok(true)
        }
        "history.auto_clear_days" => {
            if value == "disabled" || value.is_empty() {
                config.history.auto_clear_days = None;
            } else {
                config.history.auto_clear_days = Some(value.parse()?);
            }
            Ok(true)
        }

        // trust
        "trust.enabled" => {
            config.trust.enabled = value.parse()?;
            Ok(true)
        }
        "trust.auto_prompt" => {
            config.trust.auto_prompt = value.parse()?;
            Ok(true)
        }
        "trust.default_level" => {
            config.trust.default_level = match value.to_lowercase().as_str() {
                "full" => localdrop_core::config::TrustLevel::Full,
                "ask_each_time" | "ask" => localdrop_core::config::TrustLevel::AskEachTime,
                _ => anyhow::bail!("Invalid trust level. Use: full or ask_each_time"),
            };
            Ok(true)
        }

        // web
        "web.enabled" => {
            config.web.enabled = value.parse()?;
            Ok(true)
        }
        "web.port" => {
            config.web.port = value.parse()?;
            Ok(true)
        }
        "web.auth" => {
            config.web.auth = value.parse()?;
            Ok(true)
        }
        "web.localhost_only" => {
            config.web.localhost_only = value.parse()?;
            Ok(true)
        }

        // ui
        "ui.theme" => {
            let theme = value.to_lowercase();
            if !["auto", "light", "dark"].contains(&theme.as_str()) {
                anyhow::bail!("Invalid theme. Use: auto, light, or dark");
            }
            config.ui.theme = theme;
            Ok(true)
        }
        "ui.show_qr" => {
            config.ui.show_qr = value.parse()?;
            Ok(true)
        }
        "ui.notifications" => {
            config.ui.notifications = value.parse()?;
            Ok(true)
        }
        "ui.sound" => {
            config.ui.sound = value.parse()?;
            Ok(true)
        }

        _ => Ok(false),
    }
}

/// Parse a duration string like "5m", "30s", "1h"
fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let s = s.trim();
    if let Some(secs) = s.strip_suffix('s') {
        Ok(std::time::Duration::from_secs(secs.parse()?))
    } else if let Some(mins) = s.strip_suffix('m') {
        Ok(std::time::Duration::from_secs(mins.parse::<u64>()? * 60))
    } else if let Some(hours) = s.strip_suffix('h') {
        Ok(std::time::Duration::from_secs(hours.parse::<u64>()? * 3600))
    } else {
        Ok(std::time::Duration::from_secs(s.parse()?))
    }
}

/// Parse a size string like "1MB", "50KB", "1024"
fn parse_size(s: &str) -> Result<usize> {
    let s = s.trim().to_uppercase();
    if let Some(mb) = s.strip_suffix("MB") {
        Ok(mb.trim().parse::<usize>()? * 1024 * 1024)
    } else if let Some(kb) = s.strip_suffix("KB") {
        Ok(kb.trim().parse::<usize>()? * 1024)
    } else if let Some(gb) = s.strip_suffix("GB") {
        Ok(gb.trim().parse::<usize>()? * 1024 * 1024 * 1024)
    } else if let Some(b) = s.strip_suffix('B') {
        Ok(b.trim().parse()?)
    } else {
        Ok(s.parse()?)
    }
}
