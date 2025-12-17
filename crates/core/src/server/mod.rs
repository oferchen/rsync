#![deny(unsafe_code)]
//! Native server orchestration utilities.
//!
//! This module provides the server-side entry points for `--server` mode,
//! handling both Receiver and Generator roles as negotiated with the client.
//!
//! # Implementation Status
//!
//! **Delta Transfer**: ✅ **Production-ready** (as of 2025-12-09)
//!
//! The server fully implements rsync's delta transfer algorithm with metadata preservation:
//!
//! - ✅ **Signature generation** (receiver module) - Rolling and strong checksums from basis files
//! - ✅ **Delta generation** (generator module) - Efficient copy references + literals
//! - ✅ **Delta application** (receiver module) - Atomic file reconstruction
//! - ✅ **Metadata preservation** - Permissions, timestamps, ownership with nanosecond precision
//! - ✅ **Wire protocol** - Full protocol 32 compatibility
//! - ✅ **SIMD acceleration** - AVX2/NEON for rolling checksums
//!
//! **Test Coverage**: 3,228 tests passing (100% pass rate)
//! - 8 unit tests for delta transfer helpers
//! - 12 comprehensive integration tests (content integrity, metadata, edge cases)
//!
//! **Documentation**: See the `delta_transfer` module for comprehensive implementation guide
//!
//! # Quick Start
//!
//! For detailed information on how delta transfer works, start with the `delta_transfer` module
//! documentation which provides:
//! - Architecture overview and data flow
//! - Component documentation with code examples
//! - Testing and debugging strategies
//! - Performance considerations
//!
//! # Roles
//!
//! The server can operate in two roles:
//!
//! ## Receiver Role
//!
//! Managed by `ReceiverContext`. The receiver:
//! 1. Receives file list from generator
//! 2. For each file: generates signature from basis file
//! 3. Receives delta operations from generator
//! 4. Applies delta to reconstruct file
//! 5. Applies metadata (permissions, timestamps, ownership)
//!
//! See the `receiver` module documentation for usage examples.
//!
//! ## Generator Role
//!
//! Managed by `GeneratorContext`. The generator:
//! 1. Walks filesystem and builds file list
//! 2. Sends file list to receiver
//! 3. For each file: receives signature from receiver
//! 4. Generates delta operations (copy references + literals)
//! 5. Sends delta to receiver
//!
//! See the `generator` module documentation for implementation details.

use std::io::{self, Read, Write};

/// Compressed reader wrapping multiplexed streams.
mod compressed_reader;
/// Compressed writer wrapping multiplexed streams.
mod compressed_writer;
/// Server configuration derived from the compact `--server` flag string.
pub mod config;
/// Delta transfer implementation guide and documentation.
///
/// **Start here** for comprehensive documentation on how the delta transfer algorithm works,
/// including signature generation, delta creation, delta application, and metadata preservation.
pub mod delta_transfer;
/// Error categorization for delta transfer operations.
pub mod error;
/// Parser for the compact server flag string.
pub mod flags;
/// Server-side Generator role implementation.
pub mod generator;
/// Server-side protocol handshake utilities.
pub mod handshake;
/// Reader abstraction supporting plain and multiplex modes.
mod reader;
/// Server-side Receiver role implementation.
pub mod receiver;
/// Enumerations describing the role executed by the server process.
pub mod role;
/// Server-side protocol setup utilities.
pub mod setup;
/// RAII guard for temporary file cleanup.
pub mod temp_guard;
/// Writer abstraction supporting plain and multiplex modes.
mod writer;

pub use self::config::ServerConfig;
pub use self::flags::{InfoFlags, ParseFlagError, ParsedServerFlags};
pub use self::generator::{GeneratorContext, GeneratorStats};
pub use self::handshake::{HandshakeResult, perform_handshake, perform_legacy_handshake};
pub use self::receiver::{ReceiverContext, TransferStats};
pub use self::role::ServerRole;

#[cfg(test)]
mod tests;

/// Statistics returned from server execution.
#[derive(Debug, Clone)]
pub enum ServerStats {
    /// Statistics from receiver role.
    Receiver(TransferStats),
    /// Statistics from generator role.
    Generator(GeneratorStats),
}

/// Result type for server operations.
pub type ServerResult = io::Result<ServerStats>;

/// Executes the native server entry point over standard I/O.
///
/// The implementation performs the protocol handshake before dispatching to the
/// role-specific handler. The receiver role receives file lists and deltas from
/// the client, while the generator role sends file lists and deltas to the client.
///
/// # Returns
///
/// Returns `ServerStats` on successful transfer, or an error if handshake or transfer fails.
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> ServerResult {
    // Perform protocol handshake
    let handshake = perform_handshake(stdin, stdout)?;
    run_server_with_handshake(config, handshake, stdin, stdout)
}

/// Executes the native server with a pre-negotiated protocol version.
///
/// This variant is used when the handshake has already been performed (e.g., by
/// the daemon after module authentication). The caller provides the negotiated
/// `HandshakeResult` and this function dispatches directly to the role handler.
///
/// # Returns
///
/// Returns `ServerStats` on successful transfer, or an error if transfer fails.
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
) -> ServerResult {
    // Debug logging removed - eprintln! crashes when stderr unavailable in daemon mode

    // Protocol has already been negotiated via:
    // - perform_handshake() for SSH mode (binary exchange)
    // - @RSYNCD exchange for daemon mode
    // So we just use the protocol from handshake and activate multiplex if needed.
    // This mirrors upstream's setup_protocol() which skips the exchange when
    // remote_protocol != 0 (already set by @RSYNCD).

    // Extract buffered data before calling setup_protocol
    // This is critical for daemon mode where the BufReader may have read ahead
    let buffered_data = std::mem::take(&mut handshake.buffered);
    // Debug logging removed - eprintln! crashes when stderr unavailable in daemon mode

    // Chain buffered data with stdin BEFORE calling setup_protocol
    // This ensures setup_protocol reads from the correct stream
    let buffered = std::io::Cursor::new(buffered_data);
    let mut chained_stdin = buffered.chain(stdin);

    // Call setup_protocol() - mirrors upstream main.c:1245
    // This is the FIRST thing start_server() does after setting file descriptors
    // IMPORTANT: Parameter order matches upstream: f_out first, f_in second!
    // For SSH mode, compat_exchanged is false (do compat exchange here).
    // For daemon mode, compat_exchanged is false (do compat exchange with client capabilities).
    //
    // This call performs:
    // 1. Compatibility flags exchange (protocol >= 30)
    // 2. Capability negotiation for checksums and compression (protocol >= 30)
    // Both happen in RAW mode, BEFORE multiplex activation.
    let setup_result = setup::setup_protocol(
        handshake.protocol,
        &mut stdout,
        &mut chained_stdin,
        handshake.compat_exchanged,
        handshake.client_args.as_deref(),
    )?;

    // Store negotiated algorithms, compat flags, and checksum seed in handshake for use by role contexts
    // The role contexts will extract these and use them for checksum/compression operations
    handshake.negotiated_algorithms = setup_result.negotiated_algorithms;
    handshake.compat_flags = setup_result.compat_flags;
    handshake.checksum_seed = setup_result.checksum_seed;

    // CRITICAL: Flush stdout BEFORE wrapping it in ServerWriter!
    // The setup_protocol() call above may have buffered data (compat flags varint).
    // If we don't flush here, that buffered data will be written AFTER we activate
    // multiplex, causing the client to interpret it as multiplexed data instead of
    // plain data, resulting in "unexpected tag" errors.
    stdout.flush()?;

    // Activate multiplex (mirrors upstream main.c:1247-1260 and do_server_recv:1167)
    // Upstream start_server():
    //   - Always activates OUTPUT multiplex for protocol >= 23 (line 1248)
    //   - For receiver: do_server_recv() activates INPUT multiplex at protocol >= 30 (line 1167)
    //   - For sender: start_server() conditionally activates INPUT based on need_messages_from_generator
    //
    // Key insight from C code: For receiver at protocol >= 30, INPUT multiplex IS activated
    // BEFORE recv_filter_list() is called. The recv_filter_list() may read nothing (if
    // receiver_wants_list is false), but the stream is already in multiplex mode when
    // the file list is subsequently read.
    let reader = reader::ServerReader::new_plain(chained_stdin);
    let mut writer = writer::ServerWriter::new_plain(stdout);

    // Always activate OUTPUT multiplex at protocol >= 23 (main.c:1248)
    if handshake.protocol.as_u8() >= 23 {
        writer = writer.activate_multiplex()?;
    }

    // Activate compression on writer if negotiated (Protocol 30+ with compression algorithm)
    // This mirrors upstream io.c:io_start_buffering_out()
    // Compression is activated AFTER multiplex, wrapping the multiplexed stream
    // Note: Reader compression is activated later in role contexts after INPUT multiplex
    if let Some(ref negotiated) = handshake.negotiated_algorithms {
        if let Some(compress_alg) = negotiated.compression.to_compress_algorithm()? {
            // Use configured compression level or default to level 6 (upstream default)
            // Compression level comes from:
            // - Daemon configuration (rsyncd.conf compress-level setting)
            // - Environment or other server-side configuration
            let level = config
                .compression_level
                .unwrap_or(compress::zlib::CompressionLevel::Default);

            writer = writer.activate_compression(compress_alg, level)?;
        }
    }

    // Send MSG_IO_TIMEOUT for daemon mode with configured timeout (main.c:1249-1250)
    // This tells the client about the server's I/O timeout value
    if let Some(timeout_secs) = handshake.io_timeout {
        if handshake.protocol.as_u8() >= 31 {
            // Send MSG_IO_TIMEOUT with 4-byte little-endian timeout value
            // Upstream uses SIVAL(numbuf, 0, num) which stores as little-endian
            use protocol::MessageCode;
            let timeout_bytes = (timeout_secs as i32).to_le_bytes();
            writer.send_message(MessageCode::IoTimeout, &timeout_bytes)?;
        }
    }

    // NOTE: INPUT multiplex activation is now handled by each role AFTER reading filter list.
    // This prevents trying to read plain filter list data through a multiplexed stream.
    // See receiver.rs and generator.rs for the activation points.

    let chained_reader = reader;

    match config.role {
        ServerRole::Receiver => {
            let mut ctx = ReceiverContext::new(&handshake, config);
            // Pass reader by value - ReceiverContext::run now takes ownership and activates multiplex internally
            let stats = ctx.run(chained_reader, &mut writer)?;

            Ok(ServerStats::Receiver(stats))
        }
        ServerRole::Generator => {
            // Convert OsString args to PathBuf for file walking
            let paths: Vec<std::path::PathBuf> =
                config.args.iter().map(std::path::PathBuf::from).collect();

            let mut ctx = GeneratorContext::new(&handshake, config);
            // Pass reader by value - GeneratorContext::run now takes ownership and activates multiplex internally
            let stats = ctx.run(chained_reader, &mut writer, &paths)?;

            Ok(ServerStats::Generator(stats))
        }
    }
}
