//! Web command implementation.

use anyhow::Result;

use super::WebArgs;

/// Run the web command.
pub async fn run(args: WebArgs) -> Result<()> {
    let global_config = super::load_config();

    let port = if args.port == 8080 {
        global_config.web.port
    } else {
        args.port
    };

    let auth_enabled = args.auth || global_config.web.auth;
    let localhost_only = args.localhost_only || global_config.web.localhost_only;

    println!();
    println!("LocalDrop Web UI");
    println!("{}", "─".repeat(40));
    println!();

    let config = localdrop_core::web::WebServerConfig {
        port,
        localhost_only,
        auth_enabled,
        auth_password: if auth_enabled {
            Some(localdrop_core::web::WebServerConfig::generate_password())
        } else {
            None
        },
    };

    println!("  http://localhost:{}", port);
    if !localhost_only {
        println!("  http://192.168.x.x:{} (for other devices)", port);
    }

    if let Some(ref password) = config.auth_password {
        println!();
        println!("  Password: {}", password);
    }

    println!();
    println!("Press Ctrl+C to stop the server.");

    let mut server = localdrop_core::web::WebServer::new(config);
    server.start().await?;

    println!();
    println!("Server stopped.");

    Ok(())
}
