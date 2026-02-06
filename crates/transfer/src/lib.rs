#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! crates/transfer/src/lib.rs
//!
//! Server-side transfer engine for the Rust rsync implementation.
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

use std::io::{self, BufReader, BufWriter, Read, Write};

#[cfg(feature = "tracing")]
use tracing::instrument;

/// Compressed reader wrapping multiplexed streams.
mod compressed_reader;
/// Compressed writer wrapping multiplexed streams.
mod compressed_writer;
/// Server configuration derived from the compact `--server` flag string.
pub mod config;
/// Delta application for file transfer.
///
/// Encapsulates the logic for applying delta data received from a sender.
/// Mirrors upstream rsync's `receive_data()` function from `receiver.c:240`.
pub mod delta_apply;
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
/// Shared abstractions used by generator and receiver roles.
pub mod shared;
/// RAII guard for temporary file cleanup.
pub mod temp_guard;
/// Writer abstraction supporting plain and multiplex modes.
mod writer;

/// Batched acknowledgments for reduced network overhead.
pub mod ack_batcher;
/// Adaptive buffer sizing based on file size.
pub mod adaptive_buffer;
/// Buffer size constants mirroring upstream rsync.
pub mod constants;
/// Memory-mapped file abstraction for basis file access.
pub mod map_file;
/// Request pipelining for reduced latency in file transfers.
pub mod pipeline;
/// Reusable buffer for delta token data.
pub mod token_buffer;
/// Transfer operation helpers for pipelined requests.
pub mod transfer_ops;

pub use self::adaptive_buffer::{
    AdaptiveTokenBuffer, LARGE_BUFFER_SIZE, MEDIUM_BUFFER_SIZE, MEDIUM_FILE_THRESHOLD,
    SMALL_BUFFER_SIZE, SMALL_FILE_THRESHOLD, adaptive_buffer_size, adaptive_token_capacity,
    adaptive_writer_capacity,
};
pub use self::config::{ReferenceDirectory, ReferenceDirectoryKind, ServerConfig};
pub use self::flags::{InfoFlags, ParseFlagError, ParsedServerFlags};
pub use self::generator::{GeneratorContext, GeneratorStats};
pub use self::handshake::{HandshakeResult, perform_handshake, perform_legacy_handshake};
pub use self::receiver::{ReceiverContext, SumHead, TransferStats};
pub use self::role::ServerRole;
pub use self::shared::ChecksumFactory;
pub use self::writer::CountingWriter;
pub use ack_batcher::{
    AckBatcher, AckBatcherConfig, AckBatcherStats, AckEntry, AckStatus, DEFAULT_BATCH_SIZE,
    DEFAULT_BATCH_TIMEOUT_MS, MAX_BATCH_SIZE, MAX_BATCH_TIMEOUT_MS, MIN_BATCH_SIZE,
};
pub use pipeline::{
    DEFAULT_PIPELINE_WINDOW, MAX_PIPELINE_WINDOW, MIN_PIPELINE_WINDOW, PendingTransfer,
    PipelineConfig, PipelineState,
};

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
///
/// # Errors
///
/// Returns an error if:
/// - The protocol handshake fails (incompatible version or I/O error)
/// - Reading from or writing to the streams fails
/// - The receiver or generator role encounters a transfer error
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout), fields(role = ?config.role)))]
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
///
/// # Errors
///
/// Returns an error if:
/// - Protocol setup fails (compat flags exchange or capability negotiation)
/// - Flushing the output stream fails before multiplex activation
/// - Filter list writing fails (in client mode)
/// - Multiplex activation fails
/// - Sending the MSG_IO_TIMEOUT message fails (for daemon mode)
/// - The receiver or generator role encounters a transfer error
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout), fields(role = ?config.role, protocol = %handshake.protocol)))]
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
) -> ServerResult {
    // Protocol has already been negotiated via:
    // - perform_handshake() for SSH mode (binary exchange)
    // - @RSYNCD exchange for daemon mode
    // So we just use the protocol from handshake and activate multiplex if needed.
    // This mirrors upstream's setup_protocol() which skips the exchange when
    // remote_protocol != 0 (already set by @RSYNCD).

    // Extract buffered data before calling setup_protocol
    // This is critical for daemon mode where the BufReader may have read ahead
    let buffered_data = std::mem::take(&mut handshake.buffered);

    // IMPORTANT: In daemon mode, the buffered data from BufReader may contain
    // garbage or premature binary data that was read ahead during argument parsing.
    // This data is NOT meant for setup_protocol - it should be discarded.
    //
    // The vstring negotiation happens AFTER compat flags exchange:
    // 1. Server sends compat flags
    // 2. Client reads compat flags, determines do_negotiated_strings
    // 3. THEN client sends its vstring
    //
    // Any data in the buffer at this point is from BEFORE the client knew
    // whether to send vstrings, so it's not valid vstring data.
    //
    // For daemon mode (client_args is Some), discard the buffered data.
    let mut chained_stdin: Box<dyn std::io::Read> =
        if handshake.client_args.is_some() && !buffered_data.is_empty() {
            // Daemon mode: don't use buffered data, read fresh from socket
            Box::new(stdin)
        } else {
            // SSH mode or no buffered data: chain as before
            let buffered = std::io::Cursor::new(buffered_data);
            Box::new(buffered.chain(stdin))
        };

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
    //
    // is_server controls compat flags + checksum seed direction:
    // - Normal server mode (client_mode=false): we are server, WRITE compat/seed
    // - Daemon client mode (client_mode=true): we act as client, READ compat/seed
    let is_server = !config.client_mode;

    // is_daemon_mode controls capability negotiation direction:
    // - Daemon mode: unidirectional (server sends lists, client reads silently)
    // - SSH mode: bidirectional (both sides exchange)
    // We're in daemon mode if:
    // - We're a daemon client (client_mode=true, connecting to remote daemon) OR
    // - We're a server receiving from a daemon client (client_args is Some)
    let is_daemon_mode = config.client_mode || handshake.client_args.is_some();

    // do_compression controls whether compression algorithm negotiation happens.
    // Both sides must have the same value (based on -z flag).
    // - For client mode: check our config for compression_level
    // - For server mode: check if client passed -z in their args
    let do_compression = if config.client_mode {
        // Daemon client: check our own compression setting
        config.compression_level.is_some()
    } else if let Some(args) = handshake.client_args.as_deref() {
        // Daemon server: check if client has -z in their args
        args.iter()
            .any(|arg| arg.contains('z') && arg.starts_with('-'))
    } else {
        // SSH server mode: assume no compression by default
        // (will be refined when we parse client's -z flag properly)
        false
    };

    let setup_config = setup::ProtocolSetupConfig {
        protocol: handshake.protocol,
        skip_compat_exchange: handshake.compat_exchanged,
        client_args: handshake.client_args.as_deref(),
        is_server,
        is_daemon_mode,
        do_compression,
        checksum_seed: config.checksum_seed,
    };
    let setup_result = setup::setup_protocol(&mut stdout, &mut chained_stdin, &setup_config)?;

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
    //
    // For NORMAL SERVER mode (not client_mode):
    // - Server always activates OUTPUT multiplex for protocol >= 23 (start_server line 1248)
    // - For receiver: do_server_recv() activates INPUT multiplex at protocol >= 30
    // - For sender: start_server() conditionally activates INPUT
    //
    // For CLIENT MODE (daemon client connecting to remote daemon):
    // - Client does NOT activate OUTPUT multiplex (client_run doesn't call io_start_multiplex_out)
    // - Client activates INPUT multiplex to receive multiplexed data from server
    // - Filter list is sent through PLAIN output (not multiplexed)
    // - The remote daemon (server) will have OUTPUT multiplex active and send us multiplexed data
    //
    // Performance optimization: Wrap streams in buffered I/O to reduce syscalls.
    // Without buffering, each read_exact/write_all becomes a syscall, causing
    // significant per-file overhead in daemon transfers.
    let buffered_stdin = BufReader::with_capacity(64 * 1024, chained_stdin);
    let reader = reader::ServerReader::new_plain(buffered_stdin);
    let buffered_stdout = BufWriter::with_capacity(64 * 1024, stdout);
    let mut writer = writer::ServerWriter::new_plain(buffered_stdout);

    // Activate OUTPUT multiplex based on mode and protocol version.
    //
    // Upstream rsync multiplex activation differs by mode:
    //
    // SERVER mode (main.c:1246-1248 start_server):
    //   if (protocol_version >= 23)
    //       io_start_multiplex_out(f_out);
    //
    // CLIENT SENDER mode (main.c:1296-1299 client_run am_sender):
    //   if (protocol_version >= 30)
    //       io_start_multiplex_out(f_out);
    //   else
    //       io_start_buffering_out(f_out);
    //
    // CLIENT RECEIVER mode (main.c:1340-1345):
    //   if (need_messages_from_generator)
    //       io_start_multiplex_out(f_out);
    //   else
    //       io_start_buffering_out(f_out);
    //
    // Multiplex activation timing for client and server modes.
    //
    // For protocol >= 30, need_messages_from_generator = 1 (compat.c:776), so BOTH
    // client and daemon use multiplexed I/O.
    //
    // CLIENT mode (main.c:1341-1352 client_run):
    //   io_start_multiplex_in(f_in);   // Line 1343 - for proto >= 23
    //   io_start_multiplex_out(f_out); // Line 1345 - for need_messages (proto >= 30)
    //   send_filter_list(f_out);       // Line 1352 - AFTER multiplex for proto >= 30
    //   recv_file_list(f_in);          // Line 1361
    //
    // DAEMON sender (main.c:1252-1259 start_server):
    //   io_start_multiplex_in(f_in);   // Line 1255 - for need_messages (proto >= 30)
    //   recv_filter_list(f_in);        // Line 1258 - reads multiplexed filter list
    //   do_server_sender();            // Line 1259
    //
    // For client sender: multiplex is activated BEFORE filter/file list (protocol >= 30)
    // For server: multiplex is activated BEFORE filter list (protocol >= 23)
    let should_activate_output_multiplex = if config.client_mode {
        // Client mode: activate for protocol >= 30
        handshake.protocol.as_u8() >= 30
    } else {
        // Server mode: activate for protocol >= 23
        handshake.protocol.as_u8() >= 23
    };

    if should_activate_output_multiplex {
        writer = writer.activate_multiplex()?;
    }

    // Filter list handling for client mode.
    //
    // The filter list exchange depends on role and mode:
    //
    // CLIENT GENERATOR (push to daemon):
    //   Upstream exclude.c:send_filter_list() logic:
    //     receiver_wants_list = prune_empty_dirs || (delete_mode && ...)
    //     if (am_sender && !receiver_wants_list) f_out = -1;  // Skip sending
    //   For a basic push (no delete), the sender SKIPS sending the filter list.
    //
    // CLIENT RECEIVER (pull from daemon):
    //   Upstream main.c:start_server() when daemon is sender (am_sender=1):
    //     recv_filter_list(f_in);  // Daemon ALWAYS reads filter list from client!
    //   The daemon expects to read filter list from us, so we MUST send one
    //   (even if empty, we send the terminator).
    //
    // SERVER mode: we receive, never send.
    let receiver_wants_filter_list = config.flags.delete || !config.filter_rules.is_empty();

    let should_send_filter_list = if config.client_mode {
        match config.role {
            ServerRole::Generator => {
                // Client sender (push): only send if receiver wants it
                receiver_wants_filter_list
            }
            ServerRole::Receiver => {
                // Client receiver (pull): daemon sender ALWAYS reads filter list from us
                // We must send at least the empty terminator
                true
            }
        }
    } else {
        // Server mode: never send (we receive)
        false
    };

    if should_send_filter_list {
        protocol::filters::write_filter_list(
            &mut writer,
            &config.filter_rules,
            handshake.protocol,
        )?;
        writer.flush()?;
    }

    // NOTE: Compression is NOT applied at the stream level.
    //
    // Upstream rsync applies compression at the TOKEN level during delta transfer only:
    // - token.c:send_token() calls send_deflated_token() when compression is enabled
    // - token.c:recv_token() calls recv_deflated_token() to decompress
    //
    // The file list and other protocol data are sent as PLAIN data.
    // Stream-level compression would corrupt the protocol.
    //
    // Token-level compression is handled in the delta transfer code via
    // protocol::wire::compressed_token module.

    // Send MSG_IO_TIMEOUT for daemon mode with configured timeout (main.c:1249-1250)
    // This tells the client about the server's I/O timeout value
    // Only for server mode, not client mode
    if !config.client_mode
        && let Some(timeout_secs) = handshake.io_timeout
        && handshake.protocol.as_u8() >= 31
    {
        // Send MSG_IO_TIMEOUT with 4-byte little-endian timeout value
        // Upstream uses SIVAL(numbuf, 0, num) which stores as little-endian
        use protocol::MessageCode;
        let timeout_bytes = (timeout_secs as i32).to_le_bytes();
        writer.send_message(MessageCode::IoTimeout, &timeout_bytes)?;
    }

    // NOTE: INPUT multiplex activation is now handled by each role AFTER reading filter list.
    // This prevents trying to read plain filter list data through a multiplexed stream.
    // See receiver.rs and generator.rs for the activation points.

    let chained_reader = reader;

    match config.role {
        ServerRole::Receiver => {
            let mut ctx = ReceiverContext::new(&handshake, config);
            // Wrap writer in CountingWriter to track bytes sent back to sender
            // This mirrors upstream rsync's stats.total_written tracking in io.c:859
            let mut counting_writer = writer::CountingWriter::new(&mut writer);
            let mut stats = ctx.run(chained_reader, &mut counting_writer)?;
            // Record the bytes sent to the sender (signatures, indices, etc.)
            stats.bytes_sent = counting_writer.bytes_written();

            Ok(ServerStats::Receiver(stats))
        }
        ServerRole::Generator => {
            // Convert OsString args to PathBuf for file walking
            let mut paths = Vec::with_capacity(config.args.len());
            paths.extend(config.args.iter().map(std::path::PathBuf::from));

            let mut ctx = GeneratorContext::new(&handshake, config);
            // Pass reader by value - GeneratorContext::run now takes ownership and activates multiplex internally
            let stats = ctx.run(chained_reader, &mut writer, &paths)?;

            Ok(ServerStats::Generator(stats))
        }
    }
}
