//! Client transfer execution and orchestration.
//!
//! This module implements the primary entry points for executing file transfers,
//! including [`run_client`] and [`run_client_with_observer`]. These functions
//! coordinate local copies and remote transfers over SSH and rsync daemon
//! protocols, mirroring the dispatch logic in upstream `main.c:start_client()`.
//!
//! The orchestration layer handles:
//! - Configuration validation and argument parsing
//! - Progress tracking and event collection
//! - Filter rule compilation and application
//! - Batch mode file replay and recording
//! - Remote transfer role determination
//!
//! # Upstream Reference
//!
//! - `main.c:start_client()` - Top-level client dispatch
//! - `main.c:do_cmd()` - SSH fork/exec and role selection
//! - `main.c:read_batch()` - Batch file replay entry point
//! - `options.c` - Argument validation and server options building
//!
//! # Examples
//!
//! Basic local transfer:
//!
//! ```ignore
//! use core::client::{ClientConfig, run_client};
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["source/", "dest/"])
//!     .recursive(true)
//!     .build();
//!
//! let summary = run_client(config)?;
//! println!("Transferred {} files", summary.files_copied());
//! ```
//!
//! Transfer with progress reporting:
//!
//! ```ignore
//! use core::client::{ClientConfig, run_client_with_observer};
//!
//! let mut observer = |update| {
//!     println!("Progress: {}/{}", update.index(), update.total());
//! };
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["large_source/", "dest/"])
//!     .build();
//!
//! run_client_with_observer(config, Some(&mut observer))?;
//! ```

mod batch;
mod filters;

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::local_copy::{
    FilterProgram, GlobalBufferPoolConfig, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
    init_global_buffer_pool,
};

use super::config::{BandwidthLimit, ClientConfig, DeleteMode};
use super::error::{ClientError, map_local_copy_error, missing_operands_error};
use super::progress::{ClientProgressForwarder, ClientProgressObserver};
use super::remote;
use super::summary::ClientSummary;

/// Runs the client orchestration using the provided configuration.
///
/// Mirrors upstream `main.c:start_client()` by dispatching to the local copy
/// engine, SSH transport, or daemon protocol based on the operand format.
/// Both paths return a summary of the work performed.
///
/// # Arguments
///
/// * `config` - The client configuration specifying sources, destination,
///   and transfer options.
///
/// # Returns
///
/// Returns `Ok(ClientSummary)` on successful transfer with statistics about
/// files copied, bytes transferred, etc. Returns `Err(ClientError)` if the
/// transfer fails or configuration is invalid.
///
/// # Errors
///
/// Returns an error if:
/// - No transfer operands are provided (missing source or destination)
/// - The destination directory cannot be accessed due to permission denied
/// - Filter rules fail to compile due to invalid patterns
/// - The local copy engine fails during file transfer
/// - Remote SSH or daemon transfer fails
/// - Batch file operations fail (creation, header writing, or flushing)
///
/// # Examples
///
/// ```no_run
/// use core::client::{run_client, ClientConfig};
///
/// let config = ClientConfig::builder()
///     .transfer_args(vec!["source.txt", "dest.txt"])
///     .build();
///
/// let summary = run_client(config)?;
/// println!("Copied {} files", summary.files_copied());
/// # Ok::<(), core::client::ClientError>(())
/// ```
#[cfg_attr(feature = "tracing", instrument(skip(config)))]
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, None)
}

/// Runs the client orchestration while reporting progress events.
///
/// When an observer is supplied the transfer emits progress updates mirroring
/// the behaviour of upstream rsync's `--info=progress2`.
///
/// # Arguments
///
/// * `config` - The client configuration specifying sources, destination,
///   and transfer options.
/// * `observer` - Optional progress observer to receive transfer updates.
///   Pass `None` for no progress reporting.
///
/// # Returns
///
/// Returns `Ok(ClientSummary)` on successful transfer with statistics about
/// files copied, bytes transferred, etc. Returns `Err(ClientError)` if the
/// transfer fails or configuration is invalid.
///
/// # Errors
///
/// Returns an error if:
/// - No transfer operands are provided (missing source or destination)
/// - The destination directory cannot be accessed due to permission denied
/// - Filter rules fail to compile due to invalid patterns
/// - The local copy engine fails during file transfer
/// - Remote SSH or daemon transfer fails
/// - Batch file operations fail (creation, header writing, or flushing)
///
/// # Examples
///
/// ```no_run
/// use core::client::{run_client_with_observer, ClientConfig, ClientProgressUpdate};
///
/// struct MyObserver;
/// impl core::client::ClientProgressObserver for MyObserver {
///     fn on_update(&mut self, update: &ClientProgressUpdate) {
///         println!("Progress: {}/{}", update.index(), update.total());
///     }
/// }
///
/// let config = ClientConfig::builder()
///     .transfer_args(vec!["source/", "dest/"])
///     .build();
///
/// let mut observer = MyObserver;
/// let summary = run_client_with_observer(config, Some(&mut observer))?;
/// # Ok::<(), core::client::ClientError>(())
/// ```
#[cfg_attr(feature = "tracing", instrument(skip(config, observer)))]
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    run_client_internal(config, observer)
}

#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "client_internal")
)]
fn run_client_internal(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    apply_max_alloc(&config);

    let batch_writer = if let Some(batch_cfg) = config.batch_config() {
        if let Some(result) = batch::handle_batch_read(batch_cfg, &config) {
            return result;
        }
        Some(batch::create_batch_writer(batch_cfg)?)
    } else {
        None
    };

    let has_daemon_url = config.transfer_args().iter().any(|arg| {
        arg.to_string_lossy().starts_with("rsync://") || arg.to_string_lossy().contains("::")
    });

    if has_daemon_url {
        // upstream: main.c:1571-1586 - when `-e`/`--rsh` is active with `::`,
        // the client spawns SSH with `rsync --server --daemon .` as the remote
        // command, then speaks the daemon protocol over the SSH pipes.
        let summary = if config.remote_shell().is_some() {
            remote::run_daemon_over_remote_shell(&config, observer, batch_writer.clone())?
        } else {
            remote::run_daemon_transfer(&config, observer, batch_writer.clone())?
        };

        // upstream: main.c:374-383 - the client writes trailing batch stats and
        // the NDX_DONE terminator after a successful transfer. The SSH and
        // local-copy paths finalize here too; the daemon path previously
        // returned early and relied on `BatchWriter` drop to flush, which left
        // the batch file without its stats trailer (and, under load, races the
        // header write into the recorder tee).
        if let Some(ref writer_arc) = batch_writer
            && let Some(batch_cfg) = config.batch_config()
        {
            batch::finalize_batch(writer_arc, batch_cfg, &config, &summary)?;
        }

        return Ok(summary);
    }

    let has_remote = config
        .transfer_args()
        .iter()
        .any(|arg| remote::operand_is_remote(arg));

    if has_remote {
        // ssh:// operands dispatch to the embedded SSH transport instead of
        // spawning the system ssh binary when embedded-ssh is enabled.
        #[cfg(feature = "embedded-ssh")]
        {
            let has_ssh_url = config
                .transfer_args()
                .iter()
                .any(|arg| remote::is_ssh_url(&arg.to_string_lossy()));

            if has_ssh_url {
                let summary =
                    remote::run_embedded_ssh_transfer(&config, observer, batch_writer.clone())?;

                if let Some(ref writer_arc) = batch_writer
                    && let Some(batch_cfg) = config.batch_config()
                {
                    batch::finalize_batch(writer_arc, batch_cfg, &config, &summary)?;
                }

                return Ok(summary);
            }
        }

        // upstream parity: SSH transfers stay on the spawned-process path by
        // default. The async transport (#1805) is gated behind the
        // `async-ssh` cargo feature and only activated when the
        // `OC_RSYNC_ASYNC_SSH` env var is set, since the CLI flag is
        // tracked separately in #1806.
        #[cfg(feature = "async-ssh")]
        let summary = if remote::async_ssh_enabled() {
            remote::run_async_ssh_transfer(&config, observer, batch_writer.clone())?
        } else {
            remote::run_ssh_transfer(&config, observer, batch_writer.clone())?
        };
        #[cfg(not(feature = "async-ssh"))]
        let summary = remote::run_ssh_transfer(&config, observer, batch_writer.clone())?;

        if let Some(ref writer_arc) = batch_writer
            && let Some(batch_cfg) = config.batch_config()
        {
            batch::finalize_batch(writer_arc, batch_cfg, &config, &summary)?;
        }

        return Ok(summary);
    }

    // upstream: main.c:708 `get_local_name()` returns NULL when `list_only` is
    // set, so a local listing needs no destination operand. oc-rsync's local
    // plan always requires source+destination, so for `--list-only` with a
    // single source we synthesize a placeholder destination. List-only output
    // is rendered from the source flist in DryRun mode and never touches the
    // destination, so the placeholder is inert.
    let mut synthesized_operands: Option<Vec<std::ffi::OsString>> = None;
    if config.list_only() && config.transfer_args().len() == 1 {
        let mut operands = config.transfer_args().to_vec();
        operands.push(std::ffi::OsString::from("."));
        synthesized_operands = Some(operands);
    }
    let plan_operands = synthesized_operands
        .as_deref()
        .unwrap_or_else(|| config.transfer_args());

    let plan = match LocalCopyPlan::from_operands(plan_operands) {
        Ok(plan) => plan,
        Err(error) => return Err(map_local_copy_error(error)),
    };

    // upstream: main.c:751 validates destination directory access early,
    // returning FILE_SELECTION (3) for PermissionDenied instead of
    // PARTIAL_TRANSFER (23). Other errors (e.g. NotFound) proceed normally.
    use std::fs;
    let dest_to_check = if plan.destination().is_dir() {
        plan.destination()
    } else if let Some(parent) = plan.destination().parent() {
        parent
    } else {
        plan.destination()
    };

    if let Err(error) = fs::read_dir(dest_to_check) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            return Err(super::error::destination_access_error(dest_to_check, error));
        }
    }

    let filter_program =
        filters::compile_filter_program(config.filter_rules(), config.delete_excluded())?;
    let mut options = build_local_copy_options(&config, filter_program);

    // A local copy bypasses the wire, so the capability negotiator - the only
    // place trace_checksum_summary/trace_compress_summary fire on the wire path
    // - never runs. Upstream forks a real local_child server (main.c:649-654)
    // whose parse_checksum_choice/parse_compress_choice still emit the NSTR
    // summary lines, so reproduce them here from the resolved algorithms.
    // upstream: checksum.c:206-211, compat.c:213-219 (DEBUG_GTE(NSTR, 1) client).
    emit_local_copy_nstr_summaries(&config);

    let batch_writer_for_options = if let Some(ref writer) = batch_writer {
        batch::write_batch_header(writer, &config)?;
        Some(writer.clone())
    } else {
        None
    };

    if let Some(ref writer_arc) = batch_writer_for_options {
        options = options.batch_writer(Some(writer_arc.clone()));
    }

    // upstream: main.c:1817-1818 - `--only-write-batch` forces dry_run=1 so
    // that the transfer runs (populating the batch file) without creating the
    // destination directory or writing any files.
    let is_only_write_batch = config
        .batch_config()
        .is_some_and(|bc| !bc.should_transfer());

    let mode = if config.dry_run() || config.list_only() || is_only_write_batch {
        LocalCopyExecution::DryRun
    } else {
        LocalCopyExecution::Apply
    };

    let collect_events = config.collect_events();

    if collect_events {
        options = options.collect_events(true);
    }

    let mut handler_adapter = observer
        .map(|observer| ClientProgressForwarder::new(observer, &plan, options.clone()))
        .transpose()?;

    let summary = if collect_events {
        plan.execute_with_report_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(ClientSummary::from_report)
    } else {
        plan.execute_with_options_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(|mut summary| {
            // A local copy bypasses the wire protocol; `bytes_sent` holds only
            // the literal file data. Match upstream's `File list size: 0` for
            // local copies (mirrors `from_report`) instead of folding the
            // enumerated path lengths into `sent`. Covers `--stats` without
            // `-v`/`-P`.
            summary.clear_file_list_size();
            ClientSummary::from_summary(summary)
        })
    };

    let summary = summary.map_err(map_local_copy_error)?;

    if let Some(ref writer_arc) = batch_writer
        && let Some(batch_cfg) = config.batch_config()
    {
        batch::finalize_batch(writer_arc, batch_cfg, &config, &summary)?;
    }

    Ok(summary)
}

/// Applies the `--max-alloc` cap from the [`ClientConfig`] to the global
/// buffer pool.
///
/// Calls [`init_global_buffer_pool`] with [`GlobalBufferPoolConfig::default`]
/// adjusted to carry the requested byte budget. The first caller wins per
/// process: subsequent invocations and lazy initialisations are no-ops, so
/// the cap only takes effect when this runs before any subsystem has
/// acquired a buffer. That matches the lifetime of a typical CLI invocation
/// (one client per process). Library callers that already initialised the
/// pool retain whatever cap they chose.
///
/// The CLI flag drives the soft byte budget on pool retention rather than
/// the hard outstanding-memory cap: `--max-alloc` is meant to bound how
/// much memory the pool retains across calls, not to block transfers when
/// the budget is hit. When the pool retention budget is full, returning
/// buffers are deallocated and counted via the pool's overflow counter;
/// subsequent acquires allocate fresh outside the pool.
///
/// Mirrors upstream rsync `options.c:1943-1950`, where `max_alloc` is set
/// once during option processing and consumed by allocation paths thereafter.
fn apply_max_alloc(config: &ClientConfig) {
    let Some(limit) = config.max_alloc() else {
        return;
    };
    let Ok(limit_usize) = usize::try_from(limit) else {
        // Configurations parsed via the CLI are bounded by `MAX_ALLOC_CEILING`
        // (u64::MAX / 4) so this branch is only reachable on 32-bit targets
        // when a programmatic builder supplies a 64-bit value larger than
        // the host's address space. Skipping the cap is safe; the pool
        // simply remains uncapped.
        return;
    };
    if limit_usize == 0 {
        return;
    }
    let cfg = GlobalBufferPoolConfig {
        byte_budget: Some(limit_usize),
        ..GlobalBufferPoolConfig::default()
    };
    // `Err` means another caller (typically a library embedder) already
    // initialised the pool; their configuration wins to avoid silently
    // overriding their settings.
    let _ = init_global_buffer_pool(cfg);
}

/// Maps the engine's strong-checksum choice to the NSTR wire name upstream
/// prints in the `parse_checksum_choice` summary. Names match
/// `protocol::ChecksumAlgorithm::as_str` / upstream `valid_checksums_items[]`
/// (checksum.c:49-64).
const fn signature_checksum_nstr_name(
    algorithm: engine::signature::SignatureAlgorithm,
) -> &'static str {
    use engine::signature::SignatureAlgorithm;
    match algorithm {
        SignatureAlgorithm::Md4 | SignatureAlgorithm::Md4Seeded { .. } => "md4",
        SignatureAlgorithm::Md5 { .. } => "md5",
        SignatureAlgorithm::Sha1 => "sha1",
        SignatureAlgorithm::Xxh64 { .. } => "xxh64",
        SignatureAlgorithm::Xxh3 { .. } => "xxh3",
        SignatureAlgorithm::Xxh3_128 { .. } => "xxh128",
    }
}

/// Emits the `--debug=NSTR` checksum and compress summary lines for a local
/// copy, mirroring what upstream's forked `local_child` server prints via
/// `parse_checksum_choice` / `parse_compress_choice`.
///
/// Upstream's local transfer forks a child connected over a socketpair and runs
/// the full protocol, including `negotiate_the_strings()` with
/// `do_negotiated_strings` set (the local child sends the `v` capability). So
/// when the user did NOT force an algorithm, `valid_checksums.negotiated_nni`
/// is set and the summary renders the `" negotiated"` qualifier; an explicit
/// `--checksum-choice` / `--compress-choice` bypasses negotiation and the
/// qualifier stays blank. The oc local path performs no wire negotiation, so we
/// synthesize the same qualifier from whether a choice was forced. The trace
/// helpers self-gate on the NSTR debug level, so this is a no-op unless
/// `--debug=NSTR` is active.
///
/// upstream: checksum.c:206-211 (`"%s%s checksum: %s"`),
/// compat.c:213-219 (`"%s%s compress: %s (level %d)"`), both at
/// `DEBUG_GTE(NSTR, am_server ? 3 : 1)` - the client side is level 1.
fn emit_local_copy_nstr_summaries(config: &ClientConfig) {
    use protocol::nstr::{NstrSide, trace_checksum_summary, trace_compress_summary};

    // upstream: checksum.c:209 - the qualifier renders iff negotiated_nni is
    // set, which happens when no --checksum-choice forced the algorithm.
    let checksum_negotiated = config.checksum_protocol_override().is_none();
    trace_checksum_summary(
        NstrSide::Client,
        checksum_negotiated,
        signature_checksum_nstr_name(config.checksum_signature_algorithm()),
    );

    // upstream: compat.c:200-201 calls init_compression_level() before the
    // debug print whenever `do_compression != CPRES_NONE`, and compat.c:215
    // only emits the line when `do_compression != CPRES_NONE ||
    // do_compression_level != CLVL_NOT_SPECIFIED`. A local copy performs no
    // compression, so the line fires solely when the user enabled it.
    if !config.compress() {
        return;
    }

    let algorithm = config.compression_algorithm();
    // upstream prints the verbatim `compress_choice` string (compat.c:208-211,
    // 216-219); the algorithm enum folds `zlibx` onto `Zlib`, so prefer the
    // preserved raw name and fall back to the enum name for the default case.
    let name = config
        .compress_choice_name()
        .unwrap_or_else(|| algorithm.name());
    let level = resolve_nstr_compress_level(algorithm, config.compression_level());
    // upstream: compat.c:218 - the compress " negotiated" qualifier renders
    // when do_negotiated_strings selected the codec, i.e. when no explicit
    // --compress-choice (compress_choice_name) forced it.
    let compress_negotiated = config.compress_choice_name().is_none();
    trace_compress_summary(NstrSide::Client, compress_negotiated, name, level);
}

/// Resolves the compress level rendered in the `--debug=NSTR` summary,
/// mirroring upstream `token.c:init_compression_level()`.
///
/// upstream (`token.c:55-105`): when the user did not pass `--compress-level`
/// (`CLVL_NOT_SPECIFIED`), the level becomes the algorithm's `def_level` -
/// `6` for zlib/zlibx, `ZSTD_CLEVEL_DEFAULT` (3) for zstd, and `0` for lz4.
/// A user-supplied level is clamped into the algorithm's valid range. The
/// forked local-copy server runs this before the compat.c:216 print, so the
/// summary never shows the raw `CLVL_NOT_SPECIFIED` sentinel.
fn resolve_nstr_compress_level(
    algorithm: compress::algorithm::CompressionAlgorithm,
    override_level: Option<compress::zlib::CompressionLevel>,
) -> i32 {
    use compress::algorithm::{CompressionAlgorithm, ZLIB_DEFAULT_LEVEL};

    match algorithm {
        CompressionAlgorithm::Zlib => {
            // upstream: token.c:62-70 - zlib/zlibx range 1..=9, def_level 6.
            override_level
                .map_or(ZLIB_DEFAULT_LEVEL, compression_level_to_nstr)
                .clamp(1, 9)
        }
        #[cfg(feature = "zstd")]
        CompressionAlgorithm::Zstd => {
            // upstream: token.c:72-79 - def_level ZSTD_CLEVEL_DEFAULT (3).
            override_level.map_or(
                compress::algorithm::ZSTD_DEFAULT_LEVEL,
                compression_level_to_nstr,
            )
        }
        #[cfg(feature = "lz4")]
        CompressionAlgorithm::Lz4 => {
            // upstream: token.c:81-87 - lz4 def_level 0, min/max 0.
            0
        }
        // Feature-unification guard: another crate may enable `compress`'s
        // zstd/lz4 features (exposing those variants) while `core` is built
        // without them, removing the cfg-gated arms above. Fall back to the
        // zlib default so the match stays exhaustive in every feature combo.
        // Unreachable under the default build where those arms are present.
        #[allow(unreachable_patterns)]
        _ => override_level
            .map_or(ZLIB_DEFAULT_LEVEL, compression_level_to_nstr)
            .clamp(1, 9),
    }
}

/// Maps a user-supplied `--compress-level` to the raw i32 upstream renders in
/// the NSTR compress summary. upstream: token.c:init_compression_level().
fn compression_level_to_nstr(level: compress::zlib::CompressionLevel) -> i32 {
    use compress::zlib::CompressionLevel;
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 6,
        CompressionLevel::Best => 9,
        CompressionLevel::Precise(n) => i32::from(n.get()),
    }
}

/// Builder for [`LocalCopyOptions`] derived from a [`ClientConfig`] and
/// optional [`FilterProgram`].
///
/// This encapsulates the translation from CLI-facing configuration to
/// engine options using a Builder-style facade, keeping
/// `build_local_copy_options` small and testable.
///
/// The option mapping mirrors upstream `options.c:server_options()` which
/// translates CLI flags into the compact server argument format.
struct LocalCopyOptionsBuilder<'a> {
    config: &'a ClientConfig,
    filter_program: Option<FilterProgram>,
}

impl<'a> LocalCopyOptionsBuilder<'a> {
    const fn new(config: &'a ClientConfig, filter_program: Option<FilterProgram>) -> Self {
        Self {
            config,
            filter_program,
        }
    }

    fn build(self) -> LocalCopyOptions {
        let config = self.config;
        let mut options = LocalCopyOptions::default();

        options = self.apply_recursion_and_delete(options, config);
        options = self.apply_core_limits_and_bandwidth(options, config);
        options = self.apply_compression(options, config);
        options = self.apply_metadata_preservation(options, config);
        options = self.apply_behavioral_flags(options, config);
        options = self.apply_paths_and_backups(options, config);
        options = self.apply_time_and_timeout(options, config);
        options = self.apply_reference_directories(options, config);
        options = self.apply_iconv(options, config);
        options = Self::apply_cow_policy(options, config);
        options = self.apply_filter_program(options);
        options = Self::apply_zero_copy_policy(options, config);

        options
    }

    /// Swaps in [`fast_io::NoZeroCopyPlatformCopy`] when the user passed
    /// `--no-zero-copy`, forcing whole-file copies through the portable
    /// `std::fs::copy` fallback.
    ///
    /// `Auto` and `Enabled` leave the engine's default platform copy
    /// strategy in place so the platform fallback chain
    /// (`FICLONE`/`copy_file_range` on Linux, `clonefile`/`fcopyfile` on
    /// macOS, `CopyFileExW`/ReFS on Windows) remains active.
    fn apply_zero_copy_policy(
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        if matches!(config.zero_copy_policy(), fast_io::ZeroCopyPolicy::Disabled) {
            options.with_platform_copy(std::sync::Arc::new(fast_io::NoZeroCopyPlatformCopy::new()))
        } else {
            options
        }
    }

    /// Swaps the platform copy strategy when `--no-cow` / `--reflink=never`
    /// or `--reflink=always` is in effect.
    fn apply_cow_policy(options: LocalCopyOptions, config: &ClientConfig) -> LocalCopyOptions {
        match config.cow_policy() {
            fast_io::CowPolicy::Auto => options,
            fast_io::CowPolicy::Required => options
                .with_platform_copy(std::sync::Arc::new(fast_io::RequireCowPlatformCopy::new())),
            fast_io::CowPolicy::Disabled => {
                options.with_platform_copy(std::sync::Arc::new(fast_io::NoCowPlatformCopy::new()))
            }
        }
    }

    /// Resolves the user's `--iconv` request into a
    /// [`FilenameConverter`](protocol::iconv::FilenameConverter) and
    /// attaches it to the local-copy options.
    ///
    /// Local-copy must encode source-side bytes (`LOCAL`) directly into
    /// destination-side bytes (`REMOTE`) because no wire stage is
    /// present. This is the composition of the sender and receiver
    /// iconv contexts upstream rsync opens when both processes share an
    /// address space: see [`IconvSetting::resolve_local_copy_converter`]
    /// for the derivation against `rsync.c:118-140`. The SSH/daemon
    /// invocation builder already forwards the user's `--iconv=` string
    /// to the remote CLI verbatim, so the bare-`LOCAL` form keeps
    /// behaving as today on the wire.
    ///
    /// When `IconvSetting::Unspecified` or `IconvSetting::Disabled`,
    /// `resolve_local_copy_converter` returns `None`, leaving the
    /// engine's pass-through behaviour untouched. This mirrors upstream
    /// rsync's behaviour when `--iconv` is absent or `--no-iconv` is
    /// supplied.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:118-140` `setup_iconv()` - LOCAL/REMOTE split and
    ///   `ic_send`/`ic_recv` `iconv_open` calls.
    /// - `flist.c:1579-1603` `send_file_name()` sender filename transcode.
    /// - `flist.c:738-754` `recv_file_entry()` receiver filename transcode.
    /// - `options.c::recv_iconv_settings`
    /// - `compat.c:716-718`
    fn apply_iconv(&self, options: LocalCopyOptions, config: &ClientConfig) -> LocalCopyOptions {
        options.with_iconv(config.iconv().resolve_local_copy_converter())
    }

    const fn apply_recursion_and_delete(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options = options.recursive(config.recursive());

        if config.delete_mode().is_enabled() || config.delete_excluded() {
            options = options.delete(true);
        }

        options = match config.delete_mode() {
            DeleteMode::Before => options.delete_before(true),
            DeleteMode::After => options.delete_after(true),
            DeleteMode::Delay => options.delete_delay(true),
            DeleteMode::During | DeleteMode::DuringDefault | DeleteMode::Disabled => options,
        };

        options
            .delete_excluded(config.delete_excluded())
            .max_deletions(config.max_delete())
    }

    fn apply_core_limits_and_bandwidth(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .min_file_size(config.min_file_size())
            .max_file_size(config.max_file_size())
            .with_block_size_override(config.block_size_override())
            .remove_source_files(config.remove_source_files())
            .bandwidth_limit(
                config
                    .bandwidth_limit()
                    .map(BandwidthLimit::bytes_per_second),
            )
            .bandwidth_burst(
                config
                    .bandwidth_limit()
                    .and_then(BandwidthLimit::burst_bytes),
            )
    }

    fn apply_compression(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        // upstream: batch.c tees compressed wire bytes using zlib by default.
        // The batch header records do_compression but not which algorithm,
        // so upstream rsync cannot decode zstd/lz4 batch data; force zlib for
        // cross-tool interop.
        let algorithm = if config.batch_config().is_some_and(|bc| bc.is_write_mode()) {
            compress::algorithm::CompressionAlgorithm::Zlib
        } else {
            config.compression_algorithm()
        };
        options
            .with_compression_algorithm(algorithm)
            .with_default_compression_level(config.compression_setting().level_or_default())
            .with_skip_compress(config.skip_compress().clone())
            .compress(config.compress())
            .with_compression_level_override(config.compression_level())
            // upstream: options.c:89 do_compression_threads, plumbed into
            // ZSTD_c_nbWorkers by token.c:701 when zstd is selected.
            .with_compression_threads(config.compression_threads())
    }

    fn apply_metadata_preservation(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        let copy_as_ids = config
            .copy_as()
            .and_then(|spec| ::metadata::parse_copy_as_spec(spec).ok());

        options = options
            .with_stop_at(config.stop_at())
            .whole_file_option(config.whole_file_raw())
            .open_noatime(config.open_noatime())
            .owner(config.preserve_owner())
            .with_owner_override(config.owner_override())
            .group(config.preserve_group())
            .with_group_override(config.group_override())
            // upstream: options.c set_fake_super() -> am_root = -1; the local-copy
            // executor stores ownership/device/mode in the user.rsync.%stat xattr
            // instead of chown/mknod. Without this the flag was a silent no-op on
            // the local path and every fake-super round-trip lost its metadata.
            .fake_super(config.fake_super())
            .with_copy_as(copy_as_ids)
            .with_chmod(config.chmod().cloned())
            .executability(config.preserve_executability())
            .permissions(config.preserve_permissions())
            .times(config.preserve_times())
            .atimes(config.preserve_atimes())
            .crtimes(config.preserve_crtimes())
            .omit_dir_times(config.omit_dir_times())
            .omit_link_times(config.omit_link_times())
            .with_user_mapping(config.user_mapping().cloned())
            .with_group_mapping(config.group_mapping().cloned());

        #[cfg(all(any(unix, windows), feature = "acl"))]
        {
            options = options.acls(config.preserve_acls());
        }

        // `LocalCopyOptions::xattrs` is available on Unix and Windows (the
        // latter maps `-X` onto NTFS Alternate Data Streams); match the engine
        // crate's cfg so the flag reaches the local-copy executor on both.
        #[cfg(all(feature = "xattr", any(unix, windows)))]
        {
            options = options.xattrs(config.preserve_xattrs());
        }

        options
    }

    fn apply_behavioral_flags(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .itemize_active(config.itemize_changes())
            .checksum(config.checksum())
            .with_checksum_algorithm(config.checksum_signature_algorithm())
            .enable_xxh64_dedup(config.xxh64_dedup())
            .size_only(config.size_only())
            .ignore_times(config.ignore_times())
            .ignore_existing(config.ignore_existing())
            .existing_only(config.existing_only())
            .ignore_missing_args(config.ignore_missing_args())
            .delete_missing_args(config.delete_missing_args())
            .update(config.update())
            .with_modify_window(config.modify_window_duration())
            .numeric_ids(config.numeric_ids())
            .preallocate(config.preallocate())
            .fsync(config.fsync())
            .hard_links(config.preserve_hard_links())
            .links(config.links())
            .sparse(config.sparse())
            .sparse_detect_strategy(config.sparse_detect())
            .copy_links(config.copy_links())
            .copy_dirlinks(config.copy_dirlinks())
            .copy_devices_as_files(config.copy_devices())
            .copy_unsafe_links(config.copy_unsafe_links())
            .keep_dirlinks(config.keep_dirlinks())
            .safe_links(config.safe_links())
            .munge_links(config.munge_links())
            .devices(config.preserve_devices())
            .specials(config.preserve_specials())
            .with_one_file_system_level(config.one_file_system_level())
            .relative_paths(config.relative_paths())
            .dirs(config.dirs())
            .implied_dirs(config.implied_dirs())
            .mkpath(config.mkpath())
            .fuzzy_level(config.fuzzy_level())
            .prune_empty_dirs(config.prune_empty_dirs())
            .inplace(config.inplace())
            .append(config.append())
            .append_verify(config.append_verify())
            .partial(config.partial())
            .force_replacements(config.force_replacements())
            .list_only(config.list_only())
    }

    fn apply_paths_and_backups(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options
            .with_temp_directory(config.temp_directory().map(Path::to_path_buf))
            .backup(config.backup())
            .with_backup_directory(config.backup_directory().map(Path::to_path_buf))
            .with_backup_suffix(config.backup_suffix().map(OsStr::to_os_string))
            .with_partial_directory(config.partial_directory().map(Path::to_path_buf))
            .delay_updates(config.delay_updates())
            .extend_link_dests(config.link_dest_paths().iter().cloned())
    }

    fn apply_time_and_timeout(
        &self,
        options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        options.with_timeout(
            config
                .timeout()
                .as_seconds()
                .map(|seconds| Duration::from_secs(seconds.get())),
        )
    }

    fn apply_reference_directories(
        &self,
        mut options: LocalCopyOptions,
        config: &ClientConfig,
    ) -> LocalCopyOptions {
        if !config.reference_directories().is_empty() {
            options = options
                .extend_reference_directories(config.reference_directories().iter().cloned());
        }
        options
    }

    fn apply_filter_program(self, options: LocalCopyOptions) -> LocalCopyOptions {
        options.with_filter_program(self.filter_program)
    }
}

/// Builds [`LocalCopyOptions`] reflecting the provided client configuration and optional filter
/// program.
///
/// This helper mirrors the internal wiring used by [`run_client`](super::run_client) so that unit
/// tests can validate the translation layer without re-invoking the entire transfer engine.
#[doc(hidden)]
pub fn build_local_copy_options(
    config: &ClientConfig,
    filter_program: Option<FilterProgram>,
) -> LocalCopyOptions {
    LocalCopyOptionsBuilder::new(config, filter_program).build()
}

#[cfg(test)]
mod iconv_wiring_tests {
    use std::ffi::OsString;

    use super::build_local_copy_options;
    use crate::client::config::{ClientConfig, IconvSetting};

    fn config_with_iconv(setting: IconvSetting) -> ClientConfig {
        ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .iconv(setting)
            .build()
    }

    #[test]
    fn local_copy_options_iconv_unset_yields_none() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }

    #[test]
    fn local_copy_options_iconv_disabled_yields_none() {
        let config = config_with_iconv(IconvSetting::Disabled);
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }

    #[test]
    fn local_copy_options_iconv_locale_default_yields_some() {
        let config = config_with_iconv(IconvSetting::LocaleDefault);
        let options = build_local_copy_options(&config, None);
        let converter = options
            .iconv()
            .expect("locale-default iconv should produce a converter");
        assert!(converter.is_identity());
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn local_copy_options_iconv_explicit_yields_converter() {
        // upstream: rsync.c:118-140 - in local-copy mode the sender opens
        // ic_send=iconv_open(UTF8, LOCAL) and the receiver opens
        // ic_recv=iconv_open(REMOTE, UTF8). Composing both is equivalent
        // to a single LOCAL -> REMOTE converter, which is what the engine
        // applies to filenames on emit. The contract here is that the
        // engine receives a non-identity converter when LOCAL != REMOTE
        // so its iconv-aware path is wired in.
        let config = config_with_iconv(IconvSetting::Explicit {
            local: "UTF-8".to_owned(),
            remote: Some("ISO-8859-1".to_owned()),
        });
        let options = build_local_copy_options(&config, None);
        let converter = options
            .iconv()
            .expect("explicit iconv pair should produce a converter");
        assert!(!converter.is_identity());
        assert_eq!(converter.local_encoding_name(), "UTF-8");
    }

    #[test]
    fn local_copy_options_iconv_unsupported_charset_falls_back_to_none() {
        let config = config_with_iconv(IconvSetting::Explicit {
            local: "definitely-not-a-real-charset".to_owned(),
            remote: Some("also-fake".to_owned()),
        });
        let options = build_local_copy_options(&config, None);
        assert!(options.iconv().is_none());
    }
}

#[cfg(test)]
mod cow_policy_wiring_tests {
    use std::ffi::OsString;

    use super::build_local_copy_options;
    use crate::client::config::ClientConfig;

    fn config_with_cow(policy: fast_io::CowPolicy) -> ClientConfig {
        ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .cow_policy(policy)
            .build()
    }

    #[test]
    fn auto_policy_keeps_default_platform_copy() {
        let config = config_with_cow(fast_io::CowPolicy::Auto);
        let options = build_local_copy_options(&config, None);
        assert!(options.platform_copy().supports_reflink());
    }

    #[test]
    fn disabled_policy_installs_no_cow_strategy() {
        let config = config_with_cow(fast_io::CowPolicy::Disabled);
        let options = build_local_copy_options(&config, None);
        assert!(!options.platform_copy().supports_reflink());
        assert_eq!(
            options.platform_copy().preferred_method(0),
            fast_io::CopyMethod::StandardCopy
        );
        assert_eq!(
            options.platform_copy().preferred_method(1024 * 1024 * 1024),
            fast_io::CopyMethod::StandardCopy
        );
    }

    /// `--reflink=always` (`CowPolicy::Required`) must install the
    /// adapter that surfaces an error when the destination filesystem
    /// cannot honour the reflink request. The preferred method must
    /// match the platform reflink primitive so callers that inspect
    /// the trait surface observe the hard-required path.
    #[test]
    fn required_policy_installs_require_cow_strategy() {
        let config = config_with_cow(fast_io::CowPolicy::Required);
        let options = build_local_copy_options(&config, None);
        let expected = if cfg!(target_os = "linux") {
            fast_io::CopyMethod::Ficlone
        } else if cfg!(target_os = "macos") {
            fast_io::CopyMethod::Clonefile
        } else if cfg!(target_os = "windows") {
            fast_io::CopyMethod::ReFsReflink
        } else {
            fast_io::CopyMethod::StandardCopy
        };
        assert_eq!(options.platform_copy().preferred_method(0), expected);
        assert_eq!(
            options.platform_copy().preferred_method(1024 * 1024 * 1024),
            expected
        );
    }
}
