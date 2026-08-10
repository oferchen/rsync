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

/// Converts an [`InvalidTransition`] into an [`io::Error`].
///
/// Invalid FSM transitions indicate a logic error in the transfer orchestration -
/// the caller tried to advance through phases in the wrong order. Mapped to
/// `InvalidData` because the orchestration state is inconsistent.
pub(crate) fn fsm_error(err: transfer_state::InvalidTransition) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

/// Classifies a peer-disconnect error so post-goodbye flushes can tolerate it.
///
/// Re-exported from the generator role so the receiver role can share the
/// same predicate when flushing buffered NDX_DONE frames after the goodbye
/// handshake. Mirrors upstream's tolerant treatment of socket teardown after
/// the final exchange.
pub(crate) fn is_early_close_error(e: &io::Error) -> bool {
    generator::is_early_close_error(e)
}

mod compressed_reader;
mod compressed_writer;
pub mod config;
pub mod delta_apply;
pub mod delta_config;
pub mod delta_transfer;
pub mod error;
pub mod flags;
pub mod generator;
pub mod handshake;
mod reader;
pub mod receiver;
pub mod role;
pub(crate) mod role_trailer;
pub mod sanitize_path;
pub mod setup;
pub mod shared;
pub mod symlink_safety;
pub mod temp_cleanup;
pub mod temp_guard;
pub mod writer;

mod parallel_io;

pub mod delta_pipeline;

pub mod ack_batcher;
pub mod adaptive_buffer;
pub mod constants;
pub mod disk_commit;
pub mod map_file;
pub mod pipeline;
pub mod progress;
pub mod reorder_buffer;
pub mod token_buffer;
pub mod token_reader;
pub mod transfer_ops;
pub mod transfer_state;

pub use self::adaptive_buffer::{
    AdaptiveTokenBuffer, HUGE_BUFFER_SIZE, HUGE_FILE_THRESHOLD, LARGE_BUFFER_SIZE,
    MEDIUM_BUFFER_SIZE, MEDIUM_FILE_THRESHOLD, SMALL_BUFFER_SIZE, SMALL_FILE_THRESHOLD,
    adaptive_buffer_size, adaptive_token_capacity, adaptive_writer_capacity,
};
pub use self::config::{
    BuilderError, FileSelectionConfig, ReferenceDirectory, ReferenceDirectoryKind, ServerConfig,
    ServerConfigBuilder,
};
pub use self::delta_config::DeltaGeneratorConfig;
pub use self::flags::{InfoFlags, NumericIds, ParseFlagError, ParsedServerFlags};
pub use self::generator::{
    GeneratorContext, GeneratorStats, generate_delta_from_signature, io_error_flags,
};
pub use self::handshake::{
    HandshakeResult, perform_handshake, perform_handshake_with_max, perform_legacy_handshake,
};
pub use self::reader::RemoteExitError;
pub use self::receiver::{ListOnlyEntry, ReceiverContext, SumHead, TransferStats};
pub use self::role::ServerRole;
pub use self::shared::{ChecksumFactory, TransferDeadline};
pub use self::temp_cleanup::cleanup_stale_temp_files;
pub use self::writer::{CountingWriter, MsgInfoSender, ServerWriter, shutdown_send_side};
pub use ack_batcher::{
    AckBatcher, AckBatcherConfig, AckBatcherStats, AckEntry, AckStatus, DEFAULT_BATCH_SIZE,
    DEFAULT_BATCH_TIMEOUT_MS, MAX_BATCH_SIZE, MAX_BATCH_TIMEOUT_MS, MIN_BATCH_SIZE,
};
pub use delta_pipeline::{
    DEFAULT_PARALLEL_THRESHOLD, ParallelDeltaPipeline, ReceiverDeltaPipeline,
    SequentialDeltaPipeline, ThresholdDeltaPipeline,
};
pub use parallel_io::{
    DEFAULT_DELETION_THRESHOLD, DEFAULT_METADATA_THRESHOLD, DEFAULT_SIGNATURE_THRESHOLD,
    DEFAULT_STAT_THRESHOLD, ParallelOp, ParallelThresholds,
};
pub use pipeline::{
    DEFAULT_PIPELINE_WINDOW, MAX_PIPELINE_WINDOW, MIN_PIPELINE_WINDOW, PendingTransfer,
    PipelineConfig, PipelineState,
};
pub use progress::{ItemizeCallback, TransferProgressCallback, TransferProgressEvent};
pub use transfer_state::{InvalidTransition, TransferPhase, TransferPipeline};

// ASY-3: tokio-hosted server driver entry point. Re-exported so `core`'s
// session shim can call it without reaching into the private `pipeline` module.
// Default-off behind `tokio-transfer`; the threaded path never references it.
#[cfg(feature = "tokio-transfer")]
pub use pipeline::tokio_driver::run_server_with_handshake_on;

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

/// Maps a user-supplied [`compress::zlib::CompressionLevel`] to the raw i32
/// upstream stores in `do_compression_level` and renders in the NSTR compress
/// summary. upstream: token.c:init_compression_level() - zlib range 0..=9.
fn compression_level_to_i32(level: compress::zlib::CompressionLevel) -> i32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => i32::from(n.get()),
    }
}

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

/// Decides whether the local side may advertise INC_RECURSE in its compat
/// flags response. Mirrors upstream `compat.c:161-179 set_allow_inc_recurse`
/// with one local restriction: the receiver role never advertises INC_RECURSE
/// because `receive_extra_file_lists` drains the entire sub-list stream
/// upfront, which deadlocks against upstream's
/// `MIN_FILECNT_LOOKAHEAD`-throttled `send_extra_file_list` (sender.c:228-232)
/// on source trees larger than the lookahead window.
///
/// upstream: compat.c:161-179 set_allow_inc_recurse,
/// sender.c:228-232 (send_extra_file_list throttle),
/// io.c:1740-1760 (receiver's inline sub-list dispatch oc-rsync does not implement).
pub(crate) fn compute_allow_inc_recurse(recursive: bool, qsort: bool, role: ServerRole) -> bool {
    recursive && !qsort && role == ServerRole::Generator
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
    let handshake = perform_handshake(stdin, stdout)?;
    run_server_with_handshake(
        config,
        handshake,
        stdin,
        stdout,
        progress,
        None,
        None,
        #[cfg(feature = "async-bench")]
        None,
    )
}

/// BENCHMARK-ONLY receiver handoff for the gated `async-bench` daemon path.
///
/// Carries the multi-thread runtime handle the async receiver driver is driven
/// on plus the pre-split socket clone adopted as the async read half. It is
/// threaded into [`run_server_with_handshake`] only under the `async-bench`
/// feature and is never constructed in a default build, so the production
/// server path is unchanged. Activating the path additionally requires
/// `OC_RSYNC_ASYNC_BENCH=1` at runtime (checked by the daemon before this is
/// constructed). This does NOT satisfy the live-wiring rung: the synchronous
/// write leg still blocks, so it is not production-safe.
#[cfg(feature = "async-bench")]
#[derive(Debug)]
pub struct AsyncBenchReceiver<'a> {
    /// Multi-thread runtime handle (`>= 2` workers) the driver is
    /// `block_on`-driven on. A synchronous blocking write parks one worker while
    /// another polls the `.await` read, so the peer keeps draining and the
    /// driver does not self-deadlock the way a current-thread runtime would
    /// (asy-7 Blocker F).
    pub handle: &'a tokio::runtime::Handle,
    /// The socket clone (a dup'd fd of the transfer socket) adopted as the async
    /// read half. It is flipped non-blocking and handed to the tokio reactor
    /// inside the driver; the blocking write leg uses a separate clone.
    pub socket: std::net::TcpStream,
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
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout, progress, batch, itemize), fields(role = ?config.role, protocol = %handshake.protocol)))]
pub fn run_server_with_handshake<W: Write>(
    mut config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
    progress: Option<&mut dyn TransferProgressCallback>,
    batch: Option<BatchRecording>,
    itemize: Option<&mut dyn ItemizeCallback>,
    // BENCHMARK-ONLY (default-off): when `Some`, the receiver dispatch runs the
    // async driver over `async_bench.socket` instead of the threaded path. The
    // parameter only exists under the `async-bench` feature, so the default
    // build's signature and every production call site are unchanged.
    #[cfg(feature = "async-bench")] async_bench: Option<AsyncBenchReceiver<'_>>,
) -> ServerResult {
    // FSM: begin at Handshake (version exchange is already complete when this
    // function is called - either via run_server_stdio or daemon greeting).
    let mut pipeline = TransferPipeline::new(config.role);

    // upstream: setup_protocol() skips binary exchange when remote_protocol != 0
    // (already set by @RSYNCD greeting or SSH handshake).
    let buffered_data = std::mem::take(&mut handshake.buffered);

    // Chain any buffered data from the handshake BufReader ahead of raw stdin.
    // In daemon mode, the BufReader used for argument reading may buffer compat
    // exchange bytes (TCP coalescing). Discarding them breaks protocol negotiation.
    let mut chained_stdin: Box<dyn std::io::Read> = if buffered_data.is_empty() {
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

    // upstream: compat.c - do_compression is set by -z (short option) or by
    // --new-compress, --old-compress, --compress-choice=ALGO (long options).
    // upstream: options.c:2704 only puts 'z' in argstr for CPRES_ZLIB.
    // For zlibx, zstd, lz4, upstream sends long options instead.
    let (do_compression, compress_choice) = if config.connection.client_mode {
        // upstream: compat.c - client knows its own compress_choice from CLI args.
        // When --compress-choice is set, map it to its wire name; otherwise None.
        let choice = config.connection.compress_choice.map(|algo| algo.as_str());
        (config.flags.compress || choice.is_some(), choice)
    } else if let Some(args) = handshake.client_args.as_deref() {
        // Daemon-mode server: client args are the verbatim argv emitted by
        // upstream's server_options() before they were parsed into ServerConfig.
        // Scan them directly so the daemon path mirrors upstream's compat.c logic.

        // Check compact flag strings for 'z' (CPRES_ZLIB only).
        let has_z = args
            .iter()
            .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains('z'));

        // upstream: options.c:2800-2805 - long options for non-ZLIB compression:
        //   CPRES_ZLIBX → --new-compress
        //   CPRES_ZLIB with explicit choice → --old-compress
        //   other (zstd/lz4) → --compress-choice=ALGO
        let choice: Option<&str> = args.iter().find_map(|arg| {
            let s = arg.as_str();
            if s == "--new-compress" {
                Some("zlibx")
            } else if s == "--old-compress" {
                Some("zlib")
            } else {
                s.strip_prefix("--compress-choice=")
                    .or_else(|| s.strip_prefix("--zc="))
            }
        });

        (has_z || choice.is_some(), choice)
    } else {
        // SSH server mode: handshake.client_args is None because the CLI
        // frontend has already parsed the argv into ServerConfig. Recover the
        // compression intent from the parsed flag string (`config.flags.compress`
        // for `-z`) and the explicit `--compress-choice` / `--new-compress` /
        // `--old-compress` long options preserved on `config.connection`.
        // upstream: options.c:2696-2805 - server_options() emits `-z` in the
        // compact arg string for CPRES_ZLIB, and the long-form variants for
        // other algorithms. Both flow through to ServerConfig here.
        let choice = config.connection.compress_choice.map(|algo| algo.as_str());
        (config.flags.compress || choice.is_some(), choice)
    };

    // upstream: compat.c:543 - compression vstrings are only exchanged when
    // do_compression is active AND no explicit compress_choice was given.
    // When --compress-choice=ALGO is specified, both sides know the algorithm
    // and skip the vstring negotiation for compression.
    let compress_choice_algo = compress_choice
        .map(protocol::CompressionAlgorithm::parse)
        .transpose()
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid --compress-choice from client: {e}"),
            )
        })?;

    // Compute allow_inc_recurse matching upstream compat.c:161-179 with the
    // receiver-side restriction documented on `compute_allow_inc_recurse`.
    let allow_inc_recurse =
        compute_allow_inc_recurse(config.flags.recursive, config.qsort, config.role);

    // In SSH server mode (client_args is None), pass the compact flag string
    // so setup_protocol can extract the `-e.xxx` capability string from it.
    // upstream: compat.c:163-164 - `client_info = shell_cmd` for SSH mode.
    let flag_str_ref = if handshake.client_args.is_none() && is_server {
        Some(config.flag_string.as_str())
    } else {
        None
    };

    // upstream: options.c:88,767 - do_compression_level is CLVL_NOT_SPECIFIED
    // unless the user passed --compress-level=N. The NSTR compress summary
    // renders it verbatim, so map an absent override to CLVL_NOT_SPECIFIED.
    let compression_level = config
        .connection
        .compression_level
        .map_or(protocol::nstr::CLVL_NOT_SPECIFIED, compression_level_to_i32);

    let setup_config = setup::ProtocolSetupConfig {
        protocol: handshake.protocol,
        skip_compat_exchange: handshake.compat_exchanged,
        client_args: handshake.client_args.as_deref(),
        flag_string: flag_str_ref,
        is_server,
        is_daemon_mode,
        do_compression,
        compress_choice: compress_choice_algo,
        compression_level,
        checksum_choice: config.checksum_choice,
        checksum_seed: config.checksum_seed,
        allow_inc_recurse,
    };
    let setup_result = setup::setup_protocol(&mut stdout, &mut chained_stdin, &setup_config)?;

    // FSM: Handshake complete (setup_protocol exchanged compat flags, checksum
    // seed, and capability negotiation). Advance to FilterExchange.
    pipeline
        .advance_to(TransferPhase::FilterExchange)
        .map_err(fsm_error)?;

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

        // upstream: compat.c:722-723,747 - CF_AVOID_XATTR_OPTIM only signals
        // "avoid the xattr hardlink optimization" (want_xattr_optim) and is
        // gated on protocol_version >= 31. Its absence does NOT mean the peer
        // lacks xattr support: proto-30 peers (rsync 3.0.x) never define the
        // flag yet fully preserve xattrs, and the 'x' capability that drives it
        // is emitted unconditionally (options.c:3048). A remote genuinely built
        // without SUPPORT_XATTRS rejects -X at option-parse time instead. So we
        // must NOT disable xattr preservation here - doing so half-disabled the
        // sender and desynced the proto-30 flist ("xa index out of range").
    }

    // upstream: options.c:1842-1868 - when compiled without SUPPORT_ACLS or
    // SUPPORT_XATTRS, the server rejects -A/-X from the client. We mirror this
    // by clearing feature-gated flags and warning instead of hard-failing, so
    // the transfer proceeds without the unsupported metadata type.
    let cleared = config.flags.clear_unsupported_features();
    for feature in &cleared {
        let role_suffix = match config.role {
            ServerRole::Receiver => role_trailer::receiver(),
            ServerRole::Generator => role_trailer::generator(),
        };
        eprintln!(
            "WARNING: {feature} are not supported on this host - disabling preservation {}{}",
            role_trailer::error_location!(),
            role_suffix
        );
    }

    // Flush raw-mode data before wrapping in multiplexed writer.
    stdout.flush()?;

    // upstream: io.c iobuf.in is 32KB circular; BufReader serves the same role,
    // batching small reads (4-byte multiplex headers) into fewer recvfrom syscalls.
    // The CountingReader wraps the raw transport (below the multiplex demuxer and
    // token decompression) so the running total reflects compressed wire bytes,
    // matching upstream's `stats.total_read` (io.c:820).
    let counting_stdin = reader::CountingReader::new(chained_stdin);
    let bytes_received_counter = counting_stdin.counter();
    let reader =
        reader::ServerReader::new_plain(io::BufReader::with_capacity(64 * 1024, counting_stdin));
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

    // upstream: exclude.c:1650 - am_sender && !receiver_wants_list skips sending.
    // Push mode applies exclusion locally in the generator; only delete/prune
    // needs the filter list on the wire.
    let receiver_wants_filter_list = config.flags.delete || config.flags.prune_empty_dirs;

    // upstream: main.c:1258 - daemon sender always calls recv_filter_list(f_in).
    let should_send_filter_list = if config.connection.client_mode {
        match config.role {
            ServerRole::Generator => receiver_wants_filter_list,
            ServerRole::Receiver => true,
        }
    } else {
        false
    };

    if should_send_filter_list {
        // upstream: exclude.c:1605-1614 send_rules() elides any rule that applies
        // to the local side only, so the peer never sees it. On a pull (client is
        // the receiver) a receiver-side (`r`) rule is applied locally in the
        // deletion pass and must NOT reach the sender - otherwise the sender would
        // wrongly drop the matching file from the transfer (this is exactly the
        // upstream `elide == LOCAL_RULE -> continue` case). Symmetrically, on a
        // push (client is the sender) a sender-side (`s`) rule is applied locally
        // by the generator and must not reach the remote receiver, where it would
        // wrongly protect a matching destination file from --delete. A both-sided
        // rule (neither flag set) is always sent. The local deletion chain reads
        // `config.connection.filter_rules` directly (receiver/transfer/setup),
        // so this elision changes only the bytes placed on the wire.
        let client_is_sender = config.role == ServerRole::Generator;
        let wire_rules: Vec<protocol::filters::FilterRuleWireFormat> = config
            .connection
            .filter_rules
            .iter()
            .filter(|rule| {
                if client_is_sender {
                    !rule.sender_side
                } else {
                    !rule.receiver_side
                }
            })
            .cloned()
            .collect();
        protocol::filters::write_filter_list(&mut writer, &wire_rules, handshake.protocol)?;
        writer.flush()?;
    }

    // upstream: main.c:1354-1356 - after sending filter list, forward
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

    // upstream: main.c:1249-1250 - server sends MSG_IO_TIMEOUT to client.
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
            // BENCHMARK-ONLY: the protocol setup above already ran synchronously
            // on `stdin`/`writer`. Hand the wire-facing reads to the async
            // receiver driver over the pre-split socket clone, keeping the
            // request half on the blocking `writer` (a separate clone). Reachable
            // only with the `async-bench` feature AND a `Some(async_bench)` from
            // the daemon (which only supplies it when `OC_RSYNC_ASYNC_BENCH=1`).
            #[cfg(feature = "async-bench")]
            if let Some(bench) = async_bench {
                let mut ctx = ReceiverContext::new(&handshake, config, pipeline);
                let stats = bench
                    .handle
                    .block_on(ctx.run_receiver_async_bench(bench.socket, &mut writer))?;
                return Ok(ServerStats::Receiver(stats));
            }
            let mut ctx = ReceiverContext::new(&handshake, config, pipeline);
            // upstream: io.c:859 - stats.total_written tracking
            let mut counting_writer = writer::CountingWriter::new(&mut writer);
            let mut stats = ctx.run(chained_reader, &mut counting_writer, progress)?;
            stats.bytes_sent = counting_writer.bytes_written();
            // upstream: io.c:820 - stats.total_read counts raw bytes read off the
            // socket (mux frames + compressed tokens), below decompression. Source
            // bytes_received from the wire counter so --stats reports compressed
            // wire bytes, not the post-decompression literal byte total.
            stats.bytes_received =
                bytes_received_counter.load(std::sync::atomic::Ordering::Relaxed);

            Ok(ServerStats::Receiver(stats))
        }
        ServerRole::Generator => {
            let mut paths = Vec::with_capacity(config.args.len());
            paths.extend(config.args.iter().map(std::path::PathBuf::from));

            let mut ctx = GeneratorContext::new(&handshake, config, pipeline);
            let stats = ctx.run(chained_reader, &mut writer, &paths, progress, itemize)?;

            Ok(ServerStats::Generator(stats))
        }
    }
}
