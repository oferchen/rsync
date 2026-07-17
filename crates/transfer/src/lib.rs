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
//! signature generation and delta application with network I/O.
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
    HandshakeResult, IoTimeoutReapply, perform_handshake, perform_handshake_with_max,
    perform_legacy_handshake, perform_server_handshake,
};
pub use self::reader::RemoteExitError;
pub use self::receiver::{ListOnlyEntry, ReceiverContext, SumHead, TransferStats};
pub use self::role::ServerRole;
pub use self::shared::{ChecksumFactory, TransferDeadline};
pub use self::temp_cleanup::cleanup_stale_temp_files;
pub use self::writer::{CountingWriter, MsgInfoSender, ServerWriter, shutdown_send_side};
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
        // upstream: token.c:73 - preserve zstd's negative "fast" levels.
        CompressionLevel::PreciseSigned(v) => v,
    }
}

/// Reports whether the receiver wants the transfer's filter list on the wire.
///
/// Upstream computes this identical predicate in both `send_filter_list()` (the
/// client that transmits the list) and `recv_filter_list()` (the peer that reads
/// it); keeping it in one place guarantees the two ends stay in lockstep, so a
/// list is never half-sent. `--prune-empty-dirs` always needs the list;
/// otherwise it is needed only for `--delete`, and then suppressed under
/// `--delete-excluded` on a legacy peer because protocol < 29 cannot encode the
/// sender-side modifier that keeps excluded entries deletable.
///
/// # Upstream Reference
///
/// `exclude.c:1647-1648` / `exclude.c:1676-1677` -
/// `receiver_wants_list = prune_empty_dirs || (delete_mode && (!delete_excluded || protocol_version >= 29))`
pub(crate) const fn receiver_wants_filter_list(
    prune_empty_dirs: bool,
    delete_mode: bool,
    delete_excluded: bool,
    protocol: protocol::ProtocolVersion,
) -> bool {
    prune_empty_dirs || (delete_mode && (!delete_excluded || protocol.as_u8() >= 29))
}

/// Reports whether a single wire filter rule is transmitted to the peer.
///
/// Mirrors the elision performed by upstream `send_rules()`: a rule restricted to
/// the local side is applied here and never reaches the peer, and under
/// `--delete-excluded` a no-prefixes per-directory merge (`:-`/`:+`) is elided
/// from a push (the sender applies it locally) while still crossing the wire on a
/// pull. Explicitly-sided merges (`:s-`/`:r-`) carry their side flag and are
/// handled by the side-local check, so the no-prefix branch never adds a spurious
/// `s` modifier - upstream `add_rule()` deliberately spares per-directory merges
/// from the implicit sender-side flip.
///
/// CVS rules produced by `-C`/`--cvs-exclude` follow a separate gate: upstream's
/// `send_filter_list()` adds them to the transmitted list only on a sending
/// client (`am_sender`), and adds the `:C` per-directory merge only when the
/// negotiated protocol is >= 29; on a receiving client they are appended after
/// `send_rules()` and never cross the wire. The `-C` flag is forwarded to the
/// peer in argv, so the peer regenerates them locally - transmitting them on a
/// pull would be redundant and non-upstream, and emitting `:C` on a legacy peer
/// would abort with "filter rules are too modern".
///
/// # Upstream Reference
///
/// `exclude.c:1605-1612` (`send_rules`); `exclude.c:1330-1332` (`add_rule`);
/// `exclude.c:1652-1668` (`send_filter_list`, the CVS role/protocol gate).
fn wire_rule_crosses_wire(
    rule: &protocol::filters::FilterRuleWireFormat,
    client_is_sender: bool,
    delete_excluded: bool,
    protocol: protocol::ProtocolVersion,
) -> bool {
    if rule.cvs_origin {
        // upstream: exclude.c:1652 send_filter_list() - `if (cvs_exclude && am_sender)`
        // adds the `-C` rules before send_rules(); a receiving client adds them
        // only afterwards (exclude.c:1663-1668), so they never cross the wire.
        if !client_is_sender {
            return false;
        }
        // upstream: exclude.c:1653 send_filter_list() - the `:C` per-directory
        // merge is added to the transmitted list only when protocol_version >= 29;
        // on a legacy peer it is kept local (exclude.c:1664) instead.
        if matches!(rule.rule_type, protocol::filters::RuleType::DirMerge)
            && protocol.uses_old_prefixes()
        {
            return false;
        }
    }
    let side_local = if client_is_sender {
        rule.sender_side
    } else {
        rule.receiver_side
    };
    if side_local {
        return false;
    }
    if client_is_sender
        && delete_excluded
        && matches!(rule.rule_type, protocol::filters::RuleType::DirMerge)
        && rule.no_prefixes
        && !rule.sender_side
        && !rule.receiver_side
    {
        return false;
    }
    true
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
    // upstream: compat.c:600-602 - the non-local server reconciles the client's
    // pre-release subprotocol (carried in its `-e` capability string) before it
    // writes its protocol version. For a stock release peer this is a no-op and
    // the version exchange is byte-identical to a plain `perform_handshake`.
    let handshake = perform_server_handshake(stdin, stdout, &config.flag_string)?;
    run_server_with_handshake(config, handshake, stdin, stdout, progress, None, None)
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
pub fn run_server_with_handshake<W: Write>(
    config: ServerConfig,
    handshake: HandshakeResult,
    stdin: &mut dyn Read,
    stdout: W,
    progress: Option<&mut dyn TransferProgressCallback>,
    batch: Option<BatchRecording>,
    itemize: Option<&mut dyn ItemizeCallback>,
) -> ServerResult {
    // Default entry: no daemon-advertised I/O-timeout adoption. Only the client
    // receiver of a daemon transfer supplies a re-apply hook, via
    // `run_server_with_handshake_adopting`.
    run_server_with_handshake_adopting(
        config,
        handshake,
        stdin,
        stdout,
        ServerTransferHooks {
            progress,
            batch,
            itemize,
            io_timeout_reapply: None,
        },
    )
}

/// Optional side-channels for a server transfer.
///
/// Groups the per-transfer callbacks and hooks so the entry point stays within
/// a reasonable argument count: live progress, batch recording, the push
/// itemize callback, and the client-receiver I/O-timeout re-apply hook.
#[derive(Default)]
pub struct ServerTransferHooks<'p, 'i> {
    /// Live per-file progress callback.
    pub progress: Option<&'p mut dyn TransferProgressCallback>,
    /// Batch recording sink for `--write-batch` / `--only-write-batch`.
    pub batch: Option<BatchRecording>,
    /// Push itemize callback (client-as-sender itemized output).
    pub itemize: Option<&'i mut dyn ItemizeCallback>,
    /// Re-applies a daemon-advertised `MSG_IO_TIMEOUT` to the live socket.
    /// `Some` only on the daemon-pull (client receiver) path.
    /// upstream: io.c:1551-1561 `read_a_msg()` case `MSG_IO_TIMEOUT`.
    pub io_timeout_reapply: Option<IoTimeoutReapply>,
}

/// Runs a server transfer that may adopt a daemon-advertised `MSG_IO_TIMEOUT`.
///
/// Identical to [`run_server_with_handshake`] but takes an optional
/// `io_timeout_reapply` hook (via [`ServerTransferHooks`]). When the local side
/// is the client receiver of a daemon transfer
/// (`config.connection.client_mode && role == Receiver`) and a hook is supplied,
/// the demultiplexer adopts a daemon-advertised timeout and re-applies it to the
/// live socket, mirroring upstream `io.c:1551-1561`. Every other caller passes
/// `None` through the thin wrapper above, so the default path is unchanged and
/// wire-identical.
#[cfg_attr(feature = "tracing", instrument(skip(stdin, stdout, hooks), fields(role = ?config.role, protocol = %handshake.protocol)))]
pub fn run_server_with_handshake_adopting<W: Write>(
    mut config: ServerConfig,
    mut handshake: HandshakeResult,
    stdin: &mut dyn Read,
    mut stdout: W,
    hooks: ServerTransferHooks<'_, '_>,
) -> ServerResult {
    let ServerTransferHooks {
        progress,
        batch,
        itemize,
        io_timeout_reapply,
    } = hooks;
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
        // upstream: compat.c:751-753 - abort when --crtimes is requested but the
        // negotiated peer lacks CF_VARINT_FLIST_FLAGS (rsync < 3.2.0).
        preserve_crtimes: config.flags.crtimes,
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
    let mut reader =
        reader::ServerReader::new_plain(io::BufReader::with_capacity(64 * 1024, counting_stdin));

    // upstream: io.c:1551-1561 - only the client receiver adopts a
    // daemon-advertised MSG_IO_TIMEOUT (`am_server || am_generator` treat it as
    // an invalid message). The re-apply hook is supplied only on the daemon-pull
    // path, so its presence plus the client-receiver role gates adoption exactly
    // as upstream does. The client's own --timeout is the current value the
    // adoption test compares against (upstream io.c:1556 `!io_timeout || io_timeout > val`).
    if let Some(reapply) = io_timeout_reapply {
        if config.connection.client_mode && config.role == crate::role::ServerRole::Receiver {
            reader
                .enable_io_timeout_adoption(handshake.io_timeout.map(|secs| secs as u32), reapply);
        }
    }

    // upstream: log.c:870-874 - the client (a push client is the sender/generator
    // role) renders each MSG_DELETED the remote receiver forwards, gating on its
    // own info=del / itemize verbosity. Capture the gate now, on the setup
    // thread where the verbosity thread-local is valid, so the read loop never
    // depends on its own thread's state. `-v` sets INFO_GTE(DEL, 1)
    // (options.c:251), so verbose_level covers the common case even if the
    // read loop runs elsewhere; --info=del without -v is caught by info_gte.
    if config.connection.client_mode && config.role == crate::role::ServerRole::Generator {
        reader.enable_deleted_render(reader::DeletedRender {
            itemize: config.flags.info_flags.itemize,
            show_plain: config.flags.verbose_level >= 1
                || logging::info_gte(logging::InfoFlag::Del, 1),
        });
    }

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

    // upstream: io.c:set_io_timeout() derives allowed_lull = (io_timeout + 1) / 2
    // (io.c:1151). Once configured, the generator/sender loop emits an empty
    // MSG_DATA keepalive during an I/O lull so the peer's timeout does not fire.
    // Without --timeout there is no lull tracking and the wire stays identical.
    if let Some(timeout_secs) = handshake.io_timeout {
        // upstream: (io_timeout + 1) / 2, i.e. ceil(io_timeout / 2).
        let allowed_lull = std::time::Duration::from_secs(timeout_secs.div_ceil(2));
        writer.set_allowed_lull(Some(allowed_lull));
    }

    // upstream: exclude.c:1647-1648 send_filter_list() -
    //   receiver_wants_list = prune_empty_dirs
    //       || (delete_mode && (!delete_excluded || protocol_version >= 29));
    // Push mode applies exclusion locally in the generator; the receiver only
    // needs the filter list for --prune-empty-dirs or --delete. Under
    // --delete-excluded on a legacy peer (protocol < 29) the list is suppressed:
    // the pre-29 wire cannot encode the sender-side modifier that keeps excluded
    // entries deletable, so upstream neither sends nor reads it there.
    let receiver_wants_filter_list = receiver_wants_filter_list(
        config.flags.prune_empty_dirs,
        config.flags.delete,
        config.deletion.delete_excluded,
        handshake.protocol,
    );

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
        // to the local side only (and, under --delete-excluded, any no-prefix
        // per-directory merge on a push) so the peer never sees it - see
        // wire_rule_crosses_wire(). The local deletion chain reads
        // `config.connection.filter_rules` directly (receiver/transfer/setup), so
        // this elision changes only the bytes placed on the wire.
        let client_is_sender = config.role == ServerRole::Generator;
        let wire_rules: Vec<protocol::filters::FilterRuleWireFormat> = config
            .connection
            .filter_rules
            .iter()
            .filter(|rule| {
                wire_rule_crosses_wire(
                    rule,
                    client_is_sender,
                    config.deletion.delete_excluded,
                    handshake.protocol,
                )
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
