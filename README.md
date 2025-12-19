# LocalDrop

**Cross-Platform Local Network File Sharing**

[![CI](https://github.com/arceus/localdrop/workflows/CI/badge.svg)](https://github.com/arceus/localdrop/actions)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![Rust Version](https://img.shields.io/badge/rust-1.86.0%2B-blue.svg)](https://www.rust-lang.org)

LocalDrop enables seamless peer-to-peer file transfers over local networks using simple, time-limited codes. Unlike cloud-based solutions, all data stays on your local network, ensuring privacy, speed, and zero bandwidth costs.

## Features

### Core Features

-   **Cross-platform foundation**: Rust-based core library for universal compatibility
-   **No account required**: Zero configuration, no cloud dependency
-   **Simple 4-character codes**: Easy discovery without IP addresses
-   **Private & secure**: TLS 1.3 encryption, data never leaves local network
-   **Fast transfers**: Chunked transfers with xxHash64 verification
-   **CLI interface**: Full-featured command-line tool
-   **Clipboard sharing**: One-shot transfer and live bidirectional sync

## Quick Start

### Share Files

```bash
# Share a single file
localdrop share document.pdf

# Share multiple files and folders
localdrop share photos/ videos/ notes.md

# Share with custom expiration
localdrop share project.zip --expire 10m
```

### Receive Files

```bash
# Receive using the 4-character code
localdrop receive A7K9

# Receive to specific directory
localdrop receive A7K9 --output ~/Downloads/

# Batch mode (auto-accept)
localdrop receive A7K9 --batch
```

### Clipboard Sharing

```bash
# Share current clipboard content (generates a code)
localdrop clipboard share

# Receive clipboard content using a code
localdrop clipboard receive A7K9

# Start bidirectional clipboard sync (host)
localdrop clipboard sync

# Join existing sync session
localdrop clipboard sync A7K9
```

## Installation

### From Source (Requires Rust 1.86.0+)

```bash
git clone https://github.com/arceus/localdrop
cd localdrop
cargo install --path crates/localdrop-cli
```

### Package Managers (Coming Soon)

```bash
# Cargo
cargo install localdrop

# Homebrew (macOS/Linux)
brew install localdrop

# apt (Debian/Ubuntu)
apt install localdrop

# winget (Windows)
winget install localdrop
```

## How It Works

1. **Sender** shares files and gets a 4-character code (e.g., `A7K9`)
2. **Receiver** enters the code on their device
3. **Discovery** happens via UDP broadcast on local network
4. **Transfer** occurs directly over TLS 1.3 encrypted TCP connection
5. **Verification** using xxHash64 per chunk, SHA-256 for complete file

```
┌─────────────┐           UDP Broadcast            ┌─────────────┐
│   Sender    │ ◄────────  Code: A7K9  ──────────► │  Receiver   │
│             │                                     │             │
│ Share A7K9  │           TCP + TLS 1.3            │ Receive A7K9│
│             │ ────────►  File Data  ───────────► │             │
└─────────────┘                                     └─────────────┘
```

## CLI Commands

```bash
# Sharing & Receiving
localdrop share <files...>          # Share files/folders
localdrop receive <code>             # Receive with code

# Clipboard Sharing
localdrop clipboard share            # Share clipboard content
localdrop clipboard receive <code>   # Receive clipboard content
localdrop clipboard sync [code]      # Bidirectional clipboard sync

# Advanced
localdrop scan                       # Scan for active shares
localdrop send <device> <files>      # Send to trusted device
localdrop trust list                 # Manage trusted devices
localdrop web                        # Start web interface
localdrop config                     # Manage configuration
localdrop diagnose                   # Network diagnostics
localdrop history                    # Transfer history
```

## Configuration

LocalDrop can be configured via TOML files:

-   **Linux**: `~/.config/localdrop/config.toml`
-   **macOS**: `~/Library/Application Support/LocalDrop/config.toml`
-   **Windows**: `%APPDATA%\LocalDrop\config.toml`

Example configuration:

```toml
[general]
device_name = "My-Laptop"
default_expire = "5m"

[network]
port = 52525
ipv6 = true

[transfer]
chunk_size = 1048576
parallel_chunks = 4
verify_checksum = true

[security]
tls_verify = true
rate_limit_attempts = 3
```

## Development

### Prerequisites

-   **Rust**: 1.86.0 or later
-   **Git**: For cloning the repository
-   **pre-commit** (optional): For git hooks

### Building

```bash
# Clone repository
git clone https://github.com/arceus/localdrop
cd localdrop

# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace

# Run with logging
RUST_LOG=debug cargo run --bin localdrop -- share test.txt
```

### Running Tests

```bash
# All tests
cargo test --workspace

# Unit tests only
cargo test --lib --workspace

# Integration tests only
cargo test --test integration_transfer

# With output
cargo test -- --nocapture
```

### Code Quality

```bash
# Format code
cargo fmt --all

# Lint with clippy
cargo clippy --workspace -- -D warnings

# Check without building
cargo check --workspace
```

## Minimum Supported Rust Version (MSRV)

LocalDrop requires **Rust 1.86.0** or later.

## Architecture

LocalDrop uses a custom binary protocol (LDRP) over TLS 1.3:

-   **Discovery**: UDP broadcast on port 52525
-   **Transfer**: TCP on ports 52530-52540
-   **Encryption**: TLS 1.3 with self-signed ephemeral certificates
-   **Integrity**: xxHash64 per chunk, SHA-256 per file
-   **Code Format**: 4 characters from `[2-9A-HJ-KMN-Z]` (avoiding ambiguous chars)

### Protocol Flow

```
Receiver                                              Sender
   │                                                    │
   │◄─────────────── TCP Connect ──────────────────────│
   │◄─────────────── TLS Handshake ───────────────────►│
   │◄─────────────────── HELLO ────────────────────────│
   │────────────────── HELLO_ACK ─────────────────────►│
   │◄─────────────── CODE_VERIFY ──────────────────────│
   │────────────── CODE_VERIFY_ACK ───────────────────►│
   │─────────────────── FILE_LIST ────────────────────►│
   │────────────────── FILE_LIST_ACK ─────────────────►│
   │─────────────────── CHUNK_DATA ───────────────────►│
   │────────────────── CHUNK_ACK ──────────────────────│
   │────────────── TRANSFER_COMPLETE ─────────────────►│
```

## Security

LocalDrop prioritizes security and privacy:

-   **Encryption**: All transfers use TLS 1.3 with perfect forward secrecy
-   **No persistence**: Ephemeral certificates, no long-term keys (except trusted devices)
-   **Rate limiting**: 3 failed attempts → 30 second lockout
-   **Local only**: No internet connectivity required or used
-   **Code verification**: HMAC-based verification prevents timing attacks

## Roadmap

-   [x] **Phase 1**: Core library and CLI (Complete)
-   [ ] **Phase 2**: Cross-platform desktop support
-   [ ] **Phase 3**: Android application
-   [ ] **Phase 4**: iOS application
-   [ ] **Phase 5**: Enhanced features (previews, web UI, trusted devices)
-   [ ] **Phase 6**: Distribution and packaging

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

Before submitting a PR:

1. Ensure all tests pass: `cargo test --workspace`
2. Format code: `cargo fmt --all`
3. Check lints: `cargo clippy --workspace -- -D warnings`
4. Update documentation if needed

## License

Licensed under either of:

-   **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   **MIT license** ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

## Acknowledgments

Built with Rust and powered by:

-   [tokio](https://tokio.rs/) - Async runtime
-   [rustls](https://github.com/rustls/rustls) - TLS implementation
-   [clap](https://github.com/clap-rs/clap) - CLI parsing
-   [serde](https://serde.rs/) - Serialization framework
