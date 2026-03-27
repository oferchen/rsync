#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! Transfer coordination between sender, receiver, and generator roles.
//!
//! This crate provides the server-side entry points for `--server` mode,
//! handling both Receiver and Generator roles as negotiated with the client.
//! It implements the rsync delta transfer algorithm with full protocol 32
//! compatibility, SIMD-accelerated checksums, and metadata preservation.
//!
//! # Architecture
//!
//! The transfer engine is structured around a pipeline of protocol phases:
//!
//! ```text
//! ┌────────────┐    ┌────────────┐    ┌────────────┐    ┌──────────────┐
//! │ Handshake  │───▶│  Protocol  │───▶│  Multiplex │───▶│  Role-based  │
//! │ (version)  │    │   Setup    │    │ Activation │    │  Transfer    │
//! └────────────┘    └────────────┘    └────────────┘    └──────────────┘
//!    handshake          setup           writer/reader     generator or
//!    module             module          modules           receiver
//! ```
//!
//! 1. **Handshake** ([`handshake`]) - Binary or legacy ASCII protocol version exchange
//! 2. **Protocol Setup** ([`setup`]) - Compatibility flags, checksum/compression negotiation, seed exchange
//! 3. **Multiplex Activation** - Output stream wrapped for protocol-framed I/O
//! 4. **Role Dispatch** - [`GeneratorContext`] (sender) or [`ReceiverContext`] (receiver) runs the transfer
//!
//! Within a transfer, the receiver uses a **request pipeline** ([`pipeline`]) to overlap
//! signature generation and delta application with network I/O, and an **ACK batcher**
//! ([`ack_batcher`]) to amortize per-file acknowledgment overhead.
//!
//! # Key Modules
//!
//! - [`setup`] - Protocol compatibility exchange, capability negotiation, and seed exchange.
//! - [`generator`] - Generator role: walks the file tree, sends the file list, reads
//!   signatures, generates and transmits delta streams.
//! - [`receiver`] - Receiver role: receives the file list, produces signatures from basis
//!   files, receives delta streams, applies them, and commits metadata.
//! - [`delta_apply`] - Applies a delta stream to a basis file to reconstruct the target,
//!   mirroring upstream `receiver.c:receive_data()`.
//! - [`map_file`] - Memory-mapped file abstraction used to provide basis-file data during
//!   signature generation and delta application without copying.
//! - [`transfer_ops`] - Per-file transfer helpers shared between pipelined request handling
//!   and single-file transfer paths.
//! - [`pipeline`] - Bounded-concurrency request pipeline that overlaps network I/O with
//!   signature and delta processing, reducing per-file round-trip latency.
//! - [`disk_commit`] - SPSC disk-commit channel that decouples network receives from disk
//!   writes. The network thread enqueues completed delta buffers; a dedicated disk thread
//!   drains the queue and commits files, preventing disk latency from stalling the wire.
//!
//! # Wire Protocol and Pipelining
//!
//! Multiplex framing (`MSG_DATA`, `MSG_ERROR`, `MSG_INFO`, `MSG_IO_ERROR`) is activated
//! after the raw-mode setup exchange. The receiver role activates multiplex input once
//! the filter list has been read. All subsequent I/O travels through the framed stream,
//! allowing out-of-band error messages to be interleaved with data.
//!
//! The [`pipeline`] module maintains a sliding window of in-flight file requests so the
//! receiver can issue the next signature before the current delta arrives, keeping both
//! the network and disk busy. The [`disk_commit`] SPSC channel ensures that disk writes
//! never block the network reader: the consumer thread calls `fsync` and renames temp
//! files while the producer continues receiving the next delta over the wire.
//!
//! # Roles
//!
//! The server can operate in two roles, selected by [`ServerRole`]:
//!
//! ## Receiver Role
//!
//! Managed by [`ReceiverContext`]. The receiver:
//! 1. Receives file list from the generator (sender)
//! 2. For each file: generates a signature from the local basis file
//! 3. Receives delta operations from the generator
//! 4. Applies the delta to reconstruct the file atomically via a temporary file
//! 5. Applies metadata (permissions, timestamps, ownership)
//!
//! ## Generator Role
//!
//! Managed by [`GeneratorContext`]. The generator:
//! 1. Walks the local filesystem and builds a file list
//! 2. Sends the file list to the receiver (client)
//! 3. For each file: receives a signature from the receiver
//! 4. Generates delta operations (copy references + literal data)
//! 5. Sends the delta stream to the receiver
//!
//! # Entry Points
//!
//! - [`run_server_stdio`] - Full server lifecycle over stdin/stdout (handshake + transfer)
//! - [`run_server_with_handshake`] - Transfer with a pre-negotiated handshake (daemon mode)
//!
//! # Delta Transfer Details
//!
//! For a comprehensive guide to how the delta transfer algorithm works, see the
//! [`delta_transfer`] module documentation.

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

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
/// Delta generator configuration parameter object.
///
/// Provides `DeltaGeneratorConfig` struct for encapsulating delta generation parameters.
pub mod delta_config;
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
/// Role trailer formatting for error and warning messages.
///
/// Upstream rsync appends `[role=VERSION]` suffixes to diagnostic output.
pub(crate) mod role_trailer;
/// Path sanitization mirroring upstream `util1.c:sanitize_path()`.
pub mod sanitize_path;
/// Server-side protocol setup utilities.
pub mod setup;
/// Shared abstractions used by generator and receiver roles.
pub mod shared;
/// Symlink target safety analysis mirroring upstream `util1.c:unsafe_symlink()`.
pub mod symlink_safety;
/// RAII guard for temporary file cleanup.
pub mod temp_guard;
/// Writer abstraction supporting plain and multiplex modes.
mod writer;

/// Bounded-concurrency parallel I/O using tokio `spawn_blocking` + `Semaphore`.
mod parallel_io;

/// Batched acknowledgments for reduced network overhead.
pub mod ack_batcher;
/// Adaptive buffer sizing based on file size.
pub mod adaptive_buffer;
/// Buffer size constants mirroring upstream rsync.
pub mod constants;
/// Disk commit thread for decoupled network/disk I/O.
pub mod disk_commit;
/// Memory-mapped file abstraction for basis file access.
pub mod map_file;
/// Request pipelining for reduced latency in file transfers.
pub mod pipeline;
/// Progress reporting for server-side transfer operations.
pub mod progress;
/// Reusable buffer for delta token data.
pub mod token_buffer;
/// Strategy-based reader for plain and compressed delta token formats.
pub mod token_reader;
/// Transfer operation helpers for pipelined requests.
pub mod transfer_ops;

pub use self::adaptive_buffer::{
    AdaptiveTokenBuffer, HUGE_BUFFER_SIZE, HUGE_FILE_THRESHOLD, LARGE_BUFFER_SIZE,
    MEDIUM_BUFFER_SIZE, MEDIUM_FILE_THRESHOLD, SMALL_BUFFER_SIZE, SMALL_FILE_THRESHOLD,
    adaptive_buffer_size, adaptive_token_capacity, adaptive_writer_capacity,
};
pub use self::config::{
    FileSelectionConfig, ReferenceDirectory, ReferenceDirectoryKind, ServerConfig,
};
pub use self::delta_config::DeltaGeneratorConfig;
pub use self::flags::{InfoFlags, ParseFlagError, ParsedServerFlags};
pub use self::generator::{
    GeneratorContext, GeneratorStats, generate_delta_from_signature, io_error_flags,
};
pub use self::handshake::{HandshakeResult, perform_handshake, perform_legacy_handshake};
pub use self::receiver::{ReceiverContext, SumHead, TransferStats};
pub use self::role::ServerRole;
pub use self::shared::{ChecksumFactory, TransferDeadline};
pub use self::writer::{CountingWriter, MsgInfoSender};
pub use ack_batcher::{
    AckBatcher, AckBatcherConfig, AckBatcherStats, AckEntry, AckStatus, DEFAULT_BATCH_SIZE,
    DEFAULT_BATCH_TIMEOUT_MS, MAX_BATCH_SIZE, MAX_BATCH_TIMEOUT_MS, MIN_BATCH_SIZE,
};
pub use pipeline::{
    DEFAULT_PIPELINE_WINDOW, MAX_PIPELINE_WINDOW, MIN_PIPELINE_WINDOW, PendingTransfer,
    PipelineConfig, PipelineState,
};
pub use progress::{TransferProgressCallback, TransferProgressEvent};

/// Batch recording configuration for protocol stream teeing.
///
/// When provided to `run_server_with_handshake`, enables post-demux (reader) or
/// pre-mux (writer) protocol stream recording to a batch file. The transfer crate
/// has no dependency on the batch crate - this struct uses generic trait objects.
///
/// upstream: `io.c:start_write_batch()` activates `write_batch_monitor_in` (receiver)
/// or `write_batch_monitor_out` (sender) after writing the batch header.
pub struct BatchRecording {
    /// Callback invoked after `setup_protocol` with negotiated values.
    ///
    /// Receives (protocol_version, compat_flags, checksum_seed) so the caller can
    /// write the batch header with the correct negotiated values.
    pub on_setup_complete:
        Box<dyn FnOnce(i32, Option<protocol::CompatibilityFlags>, i32) -> io::Result<()> + Send>,
    /// Recorder for the multiplex layer. Receives demuxed data (reads) or
    /// pre-mux data (writes) depending on `is_sender`.
    pub recorder: Arc<Mutex<dyn Write + Send>>,
    /// True when the local side is the sender (tee outgoing data via writer).
    /// False for receiver (tee incoming data via reader).
    pub is_sender: bool,
}

#[cfg(test)]
mod tests;

/// Statistics returned from server execution, tagged by role.
///
/// After [`run_server_stdio`] or [`run_server_with_handshake`] completes,
/// the caller can inspect this enum to obtain role-specific transfer metrics.
#[derive(Debug, Clone)]
pub enum ServerStats {
    /// Statistics from a receiver transfer (files received, bytes transferred, etc.).
    Receiver(TransferStats),
    /// Statistics from a generator transfer (files sent, bytes transferred, etc.).
    Generator(GeneratorStats),
}

/// Result type for the top-level server entry points.
///
/// On success, contains [`ServerStats`] with role-specific transfer metrics.
/// On failure, contains the [`io::Error`] that caused the transfer to abort.
pub type ServerResult = io::Result<ServerStats>;

/// Determines whether the output stream should use multiplexed framing.
///
/// Upstream rsync activates multiplex output differently depending on
/// the execution context:
///
/// - **Server mode** (`--server`): always for protocol >= 23 (main.c:1247-1248).
/// - **Client sender** (push): always for protocol >= 30 (main.c:1300-1301).
/// - **Client receiver** (pull): when `need_messages_from_generator` is set
///   (main.c:1344-1347). Upstream sets this unconditionally for protocol >= 30
///   (compat.c:776), so the client always activates multiplex output for pull.
fn requires_multiplex_output(
    client_mode: bool,
    _role: ServerRole,
    protocol: protocol::ProtocolVersion,
    _compat_flags: Option<protocol::CompatibilityFlags>,
) -> bool {
    if client_mode {
        // upstream: both sender (main.c:1300) and receiver (main.c:1344)
        // activate multiplex output when need_messages_from_generator is set,
        // which is unconditional for protocol >= 30 (compat.c:776).
        protocol.supports_generator_messages()
    } else {
        protocol.supports_multiplex_io()
    }
}

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
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout, progress), fields(role = ?config.role)))]
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    progress: Option<&mut dyn TransferProgressCallback>,
) -> ServerResult {
    // Perform protocol handshake
    let handshake = perform_handshake(stdin, stdout)?;
    run_server_with_handshake(config, handshake, stdin, stdout, progress, None)
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
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout, progress, batch), fields(role = ?config.role, protocol = %handshake.protocol)))]
pub fn run_server_with_handshake<W: Write>(
    mut config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
    progress: Option<&mut dyn TransferProgressCallback>,
    batch: Option<BatchRecording>,
) -> ServerResult {
    // upstream: setup_protocol() skips binary exchange when remote_protocol != 0
    // (already set by @RSYNCD greeting or SSH handshake).
    let buffered_data = std::mem::take(&mut handshake.buffered);

    // Daemon mode: discard buffered data from handshake reader. The vstring
    // negotiation follows compat flags exchange, so any buffered bytes predate
    // the client's knowledge of whether to send vstrings.
    let mut chained_stdin: Box<dyn std::io::Read> =
        if handshake.client_args.is_some() && !buffered_data.is_empty() {
            Box::new(stdin)
        } else {
            let buffered = std::io::Cursor::new(buffered_data);
            Box::new(buffered.chain(stdin))
        };

    // upstream: main.c:1245 start_server() → setup_protocol(f_out, f_in)
    // Performs compat flags exchange + capability negotiation in RAW mode,
    // before multiplex activation.
    let is_server = !config.connection.client_mode;

    // upstream: daemon mode uses unidirectional negotiation (server sends,
    // client reads silently); SSH mode uses bidirectional exchange.
    let is_daemon_mode = config.connection.client_mode || handshake.client_args.is_some();

    // upstream: compat.c — do_compression is set by the -z short option.
    // Only check compact flag strings (single-dash args like "-avz"), not
    // long-form args like "--size-only" which contain 'z' but don't mean compression.
    let do_compression = if config.connection.client_mode {
        config.flags.compress
    } else if let Some(args) = handshake.client_args.as_deref() {
        args.iter()
            .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains('z'))
    } else {
        false
    };

    // Compute allow_inc_recurse matching upstream compat.c:161-179.
    // Requires recursive mode and not qsort. For receivers, also disallows
    // delete and prune_empty_dirs (which need the complete file list upfront).
    let allow_inc_recurse = config.flags.recursive
        && !config.qsort
        && (config.role == ServerRole::Generator
            || (!config.flags.delete && !config.flags.prune_empty_dirs));

    let setup_config = setup::ProtocolSetupConfig {
        protocol: handshake.protocol,
        skip_compat_exchange: handshake.compat_exchanged,
        client_args: handshake.client_args.as_deref(),
        is_server,
        is_daemon_mode,
        do_compression,
        checksum_seed: config.checksum_seed,
        allow_inc_recurse,
    };
    let setup_result = setup::setup_protocol(&mut stdout, &mut chained_stdin, &setup_config)?;

    handshake.negotiated_algorithms = setup_result.negotiated_algorithms;
    handshake.compat_flags = setup_result.compat_flags;
    handshake.checksum_seed = setup_result.checksum_seed;

    // upstream: compat.c:777-778 - apply CF_INPLACE_PARTIAL_DIR after compat exchange.
    // When the server advertises this flag and a partial directory is configured,
    // enable per-file inplace for partial-dir basis files.
    // upstream: receiver.c:797 - one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR
    if let Some(flags) = setup_result.compat_flags {
        if flags.contains(protocol::CompatibilityFlags::INPLACE_PARTIAL_DIR)
            && config.has_partial_dir
        {
            config.write.inplace_partial = true;
        }

        // upstream: compat.c:780-785 - when acting as client, detect that the
        // remote daemon lacks xattr support (no CF_AVOID_XATTR_OPTIM in its compat
        // flags means it was built without SUPPORT_XATTRS). Gracefully disable
        // xattr preservation and warn the user instead of failing mid-transfer.
        if config.connection.client_mode
            && config.flags.xattrs
            && !flags.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
        {
            eprintln!(
                "WARNING: remote daemon does not support extended attributes - disabling xattr preservation {}{}",
                role_trailer::error_location!(),
                role_trailer::receiver()
            );
            config.flags.xattrs = false;
        }

        // Same pattern for ACLs: upstream sets CF_AVOID_XATTR_OPTIM only when
        // SUPPORT_XATTRS is compiled in. A daemon without ACL support won't
        // advertise the corresponding compat flag. Since upstream rsync has no
        // dedicated ACL compat flag, ACL rejection is handled via the daemon's
        // "refuse options" mechanism instead of compat-flag detection.
    }

    // Flush raw-mode data before wrapping in multiplexed writer.
    stdout.flush()?;

    // upstream: io.c iobuf.in is 32KB circular; BufReader serves the same role,
    // batching small reads (4-byte multiplex headers) into fewer recvfrom syscalls.
    let reader =
        reader::ServerReader::new_plain(io::BufReader::with_capacity(64 * 1024, chained_stdin));
    // MultiplexWriter provides 64KB buffering (matching upstream iobuf_out).
    let mut writer = writer::ServerWriter::new_plain(stdout);

    let mplex_out = requires_multiplex_output(
        config.connection.client_mode,
        config.role,
        handshake.protocol,
        setup_result.compat_flags,
    );
    if mplex_out {
        writer = writer.activate_multiplex()?;
    }

    // upstream: exclude.c:1650 — am_sender && !receiver_wants_list skips sending.
    // Push mode applies exclusion locally in the generator; only delete/prune
    // needs the filter list on the wire.
    let receiver_wants_filter_list = config.flags.delete || config.flags.prune_empty_dirs;

    // upstream: main.c:1258 — daemon sender always calls recv_filter_list(f_in).
    let should_send_filter_list = if config.connection.client_mode {
        match config.role {
            ServerRole::Generator => receiver_wants_filter_list,
            ServerRole::Receiver => true,
        }
    } else {
        false
    };

    if should_send_filter_list {
        protocol::filters::write_filter_list(
            &mut writer,
            &config.connection.filter_rules,
            handshake.protocol,
        )?;
        writer.flush()?;
    }

    // upstream: main.c:1354-1356 — after sending filter list, forward
    // pre-read --files-from data to the remote daemon's generator so it
    // can build the file list from the forwarded filenames.
    // This applies only in client-mode pull (Receiver), where the daemon's
    // generator reads filenames from the protocol stream.
    if config.connection.client_mode && config.role == ServerRole::Receiver {
        if let Some(data) = config.connection.files_from_data.take() {
            writer.write_all(&data)?;
            writer.flush()?;
        }
    }

    // upstream: main.c:1249-1250 — server sends MSG_IO_TIMEOUT to client.
    if !config.connection.client_mode
        && let Some(timeout_secs) = handshake.io_timeout
        && handshake.protocol.supports_extended_goodbye()
    {
        use protocol::MessageCode;
        let timeout_bytes = (timeout_secs as i32).to_le_bytes();
        writer.send_message(MessageCode::IoTimeout, &timeout_bytes)?;
    }

    // upstream: io.c:start_write_batch() - activate batch recording after setup
    // but before file list data flows. The callback writes the batch header with
    // negotiated protocol values, then the recorder is attached at the multiplex layer.
    let mut chained_reader = reader;
    if let Some(batch_recording) = batch {
        (batch_recording.on_setup_complete)(
            i32::from(handshake.protocol),
            setup_result.compat_flags,
            handshake.checksum_seed,
        )?;

        if batch_recording.is_sender {
            writer.set_batch_recorder(batch_recording.recorder)?;
        } else {
            chained_reader.set_batch_recorder(batch_recording.recorder);
        }
    }

    // Input multiplex activation deferred to each role after reading filter list.

    match config.role {
        ServerRole::Receiver => {
            let mut ctx = ReceiverContext::new(&handshake, config);
            // upstream: io.c:859 — stats.total_written tracking
            let mut counting_writer = writer::CountingWriter::new(&mut writer);
            let mut stats = ctx.run(chained_reader, &mut counting_writer, progress)?;
            stats.bytes_sent = counting_writer.bytes_written();

            Ok(ServerStats::Receiver(stats))
        }
        ServerRole::Generator => {
            // Convert OsString args to PathBuf for file walking
            let mut paths = Vec::with_capacity(config.args.len());
            paths.extend(config.args.iter().map(std::path::PathBuf::from));

            let mut ctx = GeneratorContext::new(&handshake, config);
            // Pass reader by value - GeneratorContext::run now takes ownership and activates multiplex internally
            let stats = ctx.run(chained_reader, &mut writer, &paths, progress)?;

            Ok(ServerStats::Generator(stats))
        }
    }
}
