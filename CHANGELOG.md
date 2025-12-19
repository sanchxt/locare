# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Clipboard sharing module** (localdrop-core)
  - `ClipboardContent` type supporting text and PNG images
  - `ClipboardMetadata` for content type, size, and checksum
  - `NativeClipboard` wrapper using arboard for cross-platform access
  - `ClipboardWatcher` for polling-based change detection (500ms interval)
  - `ClipboardShareSession` for one-shot clipboard sharing
  - `ClipboardReceiveSession` for one-shot clipboard receiving with keep-alive
  - `ClipboardSyncSession` for live bidirectional clipboard synchronization
  - New protocol message types: ClipboardMeta, ClipboardData, ClipboardAck, ClipboardChanged, ClipboardRequest
  - xxHash64 content hashing for efficient change detection

- **Clipboard CLI commands** (localdrop)
  - `localdrop clipboard share` - Share current clipboard content with a code
  - `localdrop clipboard receive <code>` - Receive clipboard content using a code
  - `localdrop clipboard sync [code]` - Start or join bidirectional clipboard sync
  - JSON output mode (`--json`) for scripting
  - Quiet mode (`--quiet`) for minimal output
  - Batch mode (`--batch`) for non-interactive receiving

## [0.1.0] - 2025-12-17

### Added

#### Core Library (localdrop-core)

- **Discovery module**: UDP broadcast-based network discovery with collision detection
  - Broadcasting on port 52525 with configurable intervals
  - mDNS/DNS-SD support for device discovery
  - Discovery packet serialization with device metadata
- **Code module**: 4-character share code generation and validation
  - Charset of 32 characters (2-9, A-H, J-K, M, N, P-Z) avoiding ambiguous characters
  - Code collision detection and regeneration
  - Expiration tracking (default 5 minutes, configurable 1-30 minutes)
- **Transfer module**: Complete file transfer engine
  - Share session management with progress tracking
  - Receive session with preview and acceptance flow
  - Multi-file transfer support
  - Transfer state machine (Waiting, Connected, Transferring, Completed, etc.)
- **Protocol module**: LDRP (LocalDrop Protocol) wire format implementation
  - Binary frame format with magic bytes, version, type, and length
  - Message types: HELLO, FILE_LIST, CHUNK_DATA, CHUNK_ACK, etc.
  - Efficient chunk data encoding/decoding
- **Crypto module**: Security primitives
  - TLS 1.3 configuration with rustls
  - Self-signed certificate generation using rcgen
  - HMAC-SHA256 for code verification (timing-attack resistant)
  - xxHash64 for fast chunk verification
  - SHA-256 for complete file integrity
  - Constant-time comparison utilities
- **File module**: File operations and chunking
  - File metadata extraction (size, MIME type, timestamps)
  - Recursive directory enumeration with walkdir
  - Chunked file reading (default 1MB chunks, configurable)
  - Path sanitization to prevent directory traversal
  - FileWriter for receiving and assembling chunks
- **Config module**: Configuration management system
  - TOML-based configuration files
  - Platform-specific config directory detection
  - Hierarchical config structure (general, network, transfer, security, etc.)
  - Human-readable duration serialization
- **Error module**: Comprehensive error handling
  - Error codes (E001-E010) for common failures
  - Detailed error context and recovery information
  - Integration with std::io::Error and thiserror
- **Preview module** (stub): File preview generation scaffolding
  - Preview type enumeration (Thumbnail, Text, Archive, Icon)
  - Text preview implementation
  - Image thumbnail and archive listing (TODO)
- **Trust module** (stub): Trusted devices management scaffolding
  - Trust level enumeration (Full, AskEachTime)
  - Device record structure with Ed25519 key placeholders
  - Trust store API (TODO)
- **Web module** (stub): Web server scaffolding
  - WebSocket message types defined
  - Configuration structure
  - API endpoint planning (TODO)

#### CLI Application (localdrop)

- **Command structure**: Comprehensive CLI using clap
  - `share` - Share files and directories with progress display
  - `receive` - Receive files with code, preview support, and acceptance prompt
  - `send` - Send to trusted devices (stub)
  - `scan` - Scan network for active shares (stub)
  - `trust` - Manage trusted devices (stub)
  - `web` - Start web interface (stub)
  - `config` - Configuration management (stub)
  - `diagnose` - Network diagnostics (stub)
  - `history` - Transfer history (stub)
- **Progress tracking**: Real-time transfer progress with percentage, speed, and ETA
- **JSON output mode**: Machine-readable output for scripting
- **Batch mode**: Non-interactive operation for automation
- **Logging**: Integrated tracing with configurable log levels

#### Testing & Quality

- **Unit tests**: 41 unit tests covering core functionality
  - Code generation and validation
  - Protocol encoding/decoding
  - File chunking and assembly
  - Cryptographic operations
  - Configuration serialization
  - Discovery packet handling
- **Integration tests**: 7 end-to-end tests
  - Single file transfer
  - Multiple file transfer
  - Large file transfer (multi-chunk)
  - Directory transfer
  - Transfer decline
  - Invalid code rejection
  - Progress tracking
- **Test utilities**: Common test helpers for temp files, port allocation, and assertions

#### Infrastructure

- **Cargo workspace**: Multi-crate structure with localdrop-core and localdrop-cli
- **GitHub Actions CI**:
  - Format checking (rustfmt)
  - Linting (clippy with -D warnings)
  - Testing across multiple Rust versions
  - Build verification
  - Documentation generation
  - MSRV enforcement (Rust 1.86.0)
- **Pre-commit hooks**: Automated formatting and linting on commit
  - `cargo fmt --all --check`
  - `cargo clippy --workspace -- -D warnings`
- **Dual licensing**: MIT and Apache-2.0
- **Documentation**: README, CHANGELOG, CONTRIBUTING, and inline code documentation
- **Editor configuration**: .editorconfig for consistent code style
- **Makefile**: Common development tasks (build, test, lint, format)

### Performance

- Chunked transfers with configurable chunk size (default 1MB)
- Zero-copy operations where possible
- Efficient binary protocol (LDRP)
- xxHash64 for fast chunk verification
- Parallel chunk streaming support (up to 4 concurrent, configurable)

### Security

- TLS 1.3 encryption for all transfers
- Perfect forward secrecy via ephemeral ECDH keys
- HMAC-based code verification preventing timing attacks
- Rate limiting (3 failed attempts → 30s lockout)
- Path sanitization preventing directory traversal
- No data leaves local network
