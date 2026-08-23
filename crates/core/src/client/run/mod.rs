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

mod alt_basis;
mod batch;
mod filters;

use std::ffi::OsStr;
use std::path::{Path, PathBuf, is_separator};
use std::time::Duration;

#[cfg(feature = "tracing")]
use tracing::instrument;

use engine::local_copy::{
    FilterProgram, GlobalBufferPoolConfig, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
    init_global_buffer_pool,
};

use super::config::{BandwidthLimit, ClientConfig, DeleteMode};
use super::error::{ClientError, map_local_copy_error, missing_operands_error, validate_temp_dir};
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

/// The directory the receiver resolves relative operator paths against.
///
/// upstream: main.c:768-860 `get_local_name()` chdirs the receiver before any
/// transfer runs - into `dest_path` itself when the destination is (or is
/// created as) a directory (`change_dir` at main.c:765/823), and into the
/// destination's parent when a single file is being written (main.c:852). A
/// destination with no path component leaves the cwd alone (main.c:838).
///
/// oc never chdirs, so anything upstream resolves *after* that chdir has to be
/// joined onto this directory explicitly, or a relative value silently means
/// something different than it does upstream.
fn receiver_working_directory(dest: &Path) -> PathBuf {
    // A trailing separator names a directory even when it does not exist yet -
    // upstream creates it and chdirs in (main.c:804-823). Checking the lossy
    // rendering is safe for this predicate specifically: the separators are
    // ASCII, and lossy conversion never turns a non-UTF-8 byte into one, so a
    // path whose final byte is a separator still ends with one afterwards.
    if dest.is_dir() || dest.as_os_str().to_string_lossy().ends_with(is_separator) {
        return dest.to_path_buf();
    }
    match dest.parent() {
        // Upstream substitutes "/" when the destination is rooted (main.c:836).
        Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::from("."),
    }
}

#[cfg_attr(
    feature = "tracing",
    instrument(skip(config, observer), name = "client_internal")
)]
fn run_client_internal(
    mut config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    apply_max_alloc(&config);

    // upstream: main.c:1046-1061 do_recv() - the receiver validates --temp-dir
    // exists and is a directory before transferring. tmpdir is a receiver-only
    // option (options.c:2925 forwards it only when am_sender), so the check
    // fires only when the local process receives: a local copy or a pull (local
    // destination), never a push (remote destination).
    //
    // do_recv() runs AFTER get_local_name() has chdir'd the receiver into the
    // destination (main.c:765/823/852), so upstream stats - and later creates
    // its temp files under - a relative --temp-dir resolved against the
    // DESTINATION, not the process cwd. oc never chdirs, so the value is
    // anchored explicitly here, once, before the check: doing it later would
    // let the validation and the temp-file placement disagree about which
    // directory the operator named.
    if let Some(dest) = config
        .transfer_args()
        .last()
        .filter(|dest| !remote::operand_is_remote(dest))
    {
        let anchored = config
            .temp_directory()
            .filter(|temp_dir| temp_dir.is_relative())
            .map(|temp_dir| receiver_working_directory(Path::new(dest)).join(temp_dir));
        if let Some(anchored) = anchored {
            config.set_temp_directory(anchored);
        }
        if let Some(temp_dir) = config.temp_directory() {
            validate_temp_dir(temp_dir)?;
        }
    }

    let batch_writer = if let Some(batch_cfg) = config.batch_config() {
        if let Some(result) = batch::handle_batch_read(batch_cfg, &config) {
            return result;
        }
        Some(batch::create_batch_writer(batch_cfg)?)
    } else {
        None
    };

    // upstream: send_file_list() announces the walk with an FLOG-only
    // `rprintf(FLOG, "building file list\n")` (flist.c:2248), and the first
    // recv_file_list() mirrors it with `rprintf(FLOG, "receiving file list\n")`
    // (flist.c:2608). Both are unconditional (no verbosity gate): rwrite()
    // routes FLOG to the log file when one is active and discards it
    // otherwise (log.c:290-307), so the event is emitted here regardless of
    // `-v` and the sink decides its fate by consuming the FLOG code.
    logging::emit_info_coded(
        logging::InfoFlag::Flist,
        1,
        logging::LogCode::Log,
        if config.is_pull() {
            "receiving file list".to_owned()
        } else {
            "building file list".to_owned()
        },
    );

    // With the QUIC transport absent, a quic:// operand would otherwise fall
    // through to the subprocess ssh path below, which parses quic://host/module
    // as a host:path spec with host "quic" and fails with a confusing "could
    // not resolve hostname quic". Fail fast with an actionable diagnostic
    // instead. oc-specific: upstream rsync has no quic:// operand scheme.
    #[cfg(not(feature = "quic"))]
    {
        let has_quic_url = config
            .transfer_args()
            .iter()
            .any(|arg| remote::is_quic_url(&arg.to_string_lossy()));

        if has_quic_url {
            return Err(super::error::quic_url_requires_quic_feature());
        }
    }

    let has_daemon_url = config.transfer_args().iter().any(|arg| {
        let s = arg.to_string_lossy();
        if s.starts_with("rsync://") || s.contains("::") {
            return true;
        }
        // A `quic://` target speaks the same daemon protocol over QUIC, so it
        // routes to the daemon path (not the SSH path). Recognised only under
        // the `quic` feature; absent from a default build.
        #[cfg(feature = "quic")]
        if remote::is_quic_url(&s) {
            return true;
        }
        false
    });

    if has_daemon_url {
        // upstream: main.c:1593-1608 - when `-e`/`--rsh` is active with `::`,
        // the client spawns SSH with `rsync --server --daemon .` as the remote
        // command, then speaks the daemon protocol over the SSH pipes.
        let summary = if config.remote_shell().is_some() {
            remote::run_daemon_over_remote_shell(&config, observer, batch_writer.clone())?
        } else {
            remote::run_daemon_transfer(&config, observer, batch_writer.clone())?
        };

        // Flush the batch file and emit the replay script. The daemon path
        // previously returned early and relied on `BatchWriter` drop to flush,
        // which left the batch truncated (and, under load, raced the header
        // write into the recorder tee). The trailer itself is not written here:
        // a remote transfer records it inside the transfer layer, at the point
        // upstream's `handle_stats()` runs.
        if let Some(ref writer_arc) = batch_writer
            && let Some(batch_cfg) = config.batch_config()
        {
            batch::finalize_batch(writer_arc, batch_cfg, &config, &summary, false)?;
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
                    batch::finalize_batch(writer_arc, batch_cfg, &config, &summary, false)?;
                }

                return Ok(summary);
            }
        }

        // With the embedded SSH client absent, an ssh:// operand would otherwise
        // fall through to the subprocess ssh path below, which parses
        // ssh://user@host/path as a host:path spec with host "ssh" and fails
        // with a confusing "could not resolve hostname ssh". Fail fast with an
        // actionable diagnostic instead.
        // oc-specific: upstream rsync has no ssh:// operand scheme.
        #[cfg(not(feature = "embedded-ssh"))]
        {
            let has_ssh_url = config
                .transfer_args()
                .iter()
                .any(|arg| remote::is_ssh_url(&arg.to_string_lossy()));

            if has_ssh_url {
                return Err(super::error::ssh_url_requires_embedded_ssh());
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
            batch::finalize_batch(writer_arc, batch_cfg, &config, &summary, false)?;
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

    // upstream: main.c:760 validates destination directory access early,
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

    // upstream: main.c:1241 / main.c:1424 call `check_alt_basis_dirs()` once the
    // destination is known, so a stale `--link-dest` is reported rather than
    // silently costing the hard-link optimisation. Warn-only: the exit code is
    // untouched, matching upstream's FWARNING.
    alt_basis::check_alt_basis_dirs(config.reference_directories(), plan.destination());

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

    // upstream: main.c:1841-1842 - `--only-write-batch` forces dry_run=1 so
    // that the transfer runs (populating the batch file) without creating the
    // destination directory or writing any files.
    let mode = if config.dry_run() || config.list_only() || config.only_write_batch() {
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

    // upstream: receiver.c:674-676 - emit the progress2 end-of-transfer summary
    // line when the transfer moved no file data (a lone special/symlink or a
    // no-change run), which the per-file path never produces.
    if let Some(adapter) = handler_adapter.as_mut() {
        adapter.finalize();
    }

    // A local copy has no protocol stream to tee, so the synthesized batch has
    // no trailer yet: this is the one path that writes it here. upstream reaches
    // the same bytes through `handle_stats(-1)` + `read_final_goodbye()`, since
    // its local mode is an ordinary client sender over a socketpair.
    if let Some(ref writer_arc) = batch_writer
        && let Some(batch_cfg) = config.batch_config()
    {
        batch::finalize_batch(writer_arc, batch_cfg, &config, &summary, true)?;
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
    // upstream: options.c:2069-2072 - a max-alloc of 0 once meant SIZE_MAX and
    // so removed the ceiling; 3.5.0 refuses it (CVE-2026-53794). The CLI parser
    // rejects zero before it reaches here, and a library caller that supplies
    // it leaves the standing ceiling in place rather than lifting it, matching
    // `protocol::set_max_alloc`'s own contract for a zero argument.
    if limit == 0 {
        return;
    }
    let Ok(limit_usize) = usize::try_from(limit) else {
        // Configurations parsed via the CLI are bounded by `MAX_ALLOC_CEILING`
        // (u64::MAX / 4) so this branch is only reachable on 32-bit targets
        // when a programmatic builder supplies a 64-bit value larger than
        // the host's address space. Skipping the cap is safe; the pool
        // simply remains uncapped.
        return;
    };
    // upstream: options.c:1959-1965 rewrites the `max_alloc` global, which then
    // bounds every attacker-controlled wire allocation (util2.c:75), including
    // the xattr datum decoders. Publish it before any transfer decodes xattrs.
    protocol::set_max_alloc(limit_usize);
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
    // Map the CLI level (absent = upstream's CLVL_NOT_SPECIFIED) to the raw
    // do_compression_level, then defer to the shared init_compression_level
    // resolver so this path and the wire-negotiation path stay in lockstep.
    let raw = override_level.map_or(
        compress::algorithm::CLVL_NOT_SPECIFIED,
        compression_level_to_nstr,
    );
    algorithm.resolve_debug_level(raw)
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
        // upstream: token.c:73 - preserve zstd's negative "fast" levels.
        CompressionLevel::PreciseSigned(v) => v,
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

        // upstream: generator.c:304 delete_in_dir() - `--ignore-errors` lets the
        // delete pass run after a source-scan I/O error instead of skipping it
        // with a notice. The local-copy engine reads the flag from its own
        // options, so it has to be carried across this bridge exactly as the
        // remote path carries it onto ServerConfig.
        options
            .delete_excluded(config.delete_excluded())
            .ignore_errors(config.ignore_errors())
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
            .size_only(config.size_only())
            .ignore_times(config.ignore_times())
            .ignore_existing(config.ignore_existing())
            .existing_only(config.existing_only())
            .ignore_missing_args(config.ignore_missing_args())
            .delete_missing_args(config.delete_missing_args())
            .update(config.update())
            .with_modify_window(config.modify_window_setting())
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
            .drop_devices(config.drop_devices())
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

#[cfg(test)]
mod run_client_tests {
    use std::fs;

    use tempfile::tempdir;

    use super::run_client;
    use crate::client::config::{ClientConfig, FilterRuleSpec};

    /// Without the embedded SSH client compiled in, an `ssh://` operand has no
    /// transport. It must fail fast with the feature-unavailable exit code and
    /// an actionable message, never fall through to the subprocess ssh path
    /// where `ssh://user@host/path` misparses as host `ssh` and yields the
    /// confusing "could not resolve hostname ssh".
    #[cfg(not(feature = "embedded-ssh"))]
    #[test]
    fn run_client_rejects_ssh_url_without_embedded_ssh() {
        use crate::client::error::FEATURE_UNAVAILABLE_EXIT_CODE;

        let error = run_client(
            ClientConfig::builder()
                .transfer_args([
                    std::ffi::OsString::from("ssh://user@localhost/src"),
                    std::ffi::OsString::from("/tmp/oc-ssh-url-dest"),
                ])
                .build(),
        )
        .expect_err("ssh:// without embedded-ssh must error");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let msg = error.to_string();
        assert!(msg.contains("ssh:// URLs require the built-in SSH client"));
        assert!(msg.contains("embedded-ssh"));
        assert!(msg.contains("-e ssh"));
    }

    /// #7123: an `ssh://` operand combined with a remote shell (e.g. an implicit
    /// one from `RSYNC_RSH`) on a build without the built-in SSH client must
    /// still surface the "requires the embedded-ssh feature" diagnostic - the
    /// feature-availability error wins over the -e conflict (which is now gated
    /// on `embedded-ssh` and so cannot pre-empt it here).
    #[cfg(not(feature = "embedded-ssh"))]
    #[test]
    fn run_client_ssh_url_with_remote_shell_reports_missing_embedded_ssh() {
        use crate::client::error::FEATURE_UNAVAILABLE_EXIT_CODE;

        let error = run_client(
            ClientConfig::builder()
                .set_remote_shell(["ssh"])
                .transfer_args([
                    std::ffi::OsString::from("ssh://user@localhost/src"),
                    std::ffi::OsString::from("/tmp/oc-ssh-url-rsh-dest"),
                ])
                .build(),
        )
        .expect_err("ssh:// + remote shell without embedded-ssh must error");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let msg = error.to_string();
        assert!(
            msg.contains("ssh:// URLs require the built-in SSH client"),
            "expected the embedded-ssh diagnostic, got: {msg}"
        );
        // Must NOT be the -e conflict, which would blame an option the user may
        // never have typed (RSYNC_RSH) on a build that cannot do ssh:// anyway.
        assert!(!msg.contains("cannot be combined with an ssh:// URL operand"));
    }

    /// Without the QUIC transport compiled in, a `quic://` operand has no
    /// transport. It must fail fast with the feature-unavailable exit code and
    /// an actionable message, never fall through to the subprocess ssh path
    /// where `quic://host/module` misparses as host `quic` and yields the
    /// confusing "could not resolve hostname quic".
    #[cfg(not(feature = "quic"))]
    #[test]
    fn run_client_rejects_quic_url_without_quic() {
        use crate::client::error::FEATURE_UNAVAILABLE_EXIT_CODE;

        let error = run_client(
            ClientConfig::builder()
                .transfer_args([
                    std::ffi::OsString::from("quic://localhost/module"),
                    std::ffi::OsString::from("/tmp/oc-quic-url-dest"),
                ])
                .build(),
        )
        .expect_err("quic:// without the quic feature must error");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let msg = error.to_string();
        assert!(msg.contains("quic:// URLs require the QUIC transport"));
        assert!(msg.contains("'quic' feature"));
        assert!(msg.contains("rsync://"));
    }

    #[test]
    fn run_client_update_skips_newer_destination() {
        use filetime::{FileTime, set_file_times};

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-update.txt");
        let destination = tmp.path().join("dest-update.txt");
        fs::write(&source, b"fresh").expect("write source");
        fs::write(&destination, b"existing").expect("write destination");

        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let newer = FileTime::from_unix_time(1_700_000_100, 0);
        set_file_times(&source, older, older).expect("set source times");
        set_file_times(&destination, newer, newer).expect("set dest times");

        let summary = run_client(
            ClientConfig::builder()
                .transfer_args([
                    source.clone().into_os_string(),
                    destination.clone().into_os_string(),
                ])
                .update(true)
                .build(),
        )
        .expect("run client");

        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_skipped_newer(), 1);
        assert_eq!(
            fs::read(destination).expect("read destination"),
            b"existing"
        );
    }

    #[test]
    fn run_client_respects_filter_rules() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .extend_filter_rules([FilterRuleSpec::exclude("*.tmp".to_string())])
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert!(dest_root.join("source").join("keep.txt").exists());
        assert!(!dest_root.join("source").join("skip.tmp").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn run_client_filter_clear_resets_previous_rules() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .extend_filter_rules([
                FilterRuleSpec::exclude("*.tmp".to_string()),
                FilterRuleSpec::clear(),
                FilterRuleSpec::exclude("keep.txt".to_string()),
            ])
            .build();

        let summary = run_client(config).expect("copy succeeds");

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("skip.tmp").exists());
        assert!(!copied_root.join("keep.txt").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_copies_symbolic_link() {
        use std::os::unix::fs::symlink;

        use crate::client::{ClientEntryMetadata, ClientEventKind};

        let tmp = tempdir().expect("tempdir");
        let target_file = tmp.path().join("target.txt");
        fs::write(&target_file, b"symlink target").expect("write target");

        let source_link = tmp.path().join("source-link");
        symlink(&target_file, &source_link).expect("create source symlink");

        let destination_link = tmp.path().join("dest-link");
        let config = ClientConfig::builder()
            .transfer_args([source_link.clone(), destination_link.clone()])
            .links(true)
            .force_event_collection(true)
            .build();

        let summary = run_client(config).expect("link copy succeeds");

        let copied = fs::read_link(destination_link).expect("read copied link");
        assert_eq!(copied, target_file);
        assert_eq!(summary.symlinks_copied(), 1);

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
            .expect("symlink event present");
        let recorded_target = event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .expect("symlink target recorded");
        assert_eq!(recorded_target, target_file.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_file_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-metadata.txt");
        let destination = tmp.path().join("dest-metadata.txt");
        fs::write(&source, b"metadata").expect("write source");

        let mode = 0o640;
        fs::set_permissions(&source, PermissionsExt::from_mode(mode))
            .expect("set source permissions");
        let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
        set_file_times(&source, atime, mtime).expect("set source timestamps");

        let source_metadata = fs::metadata(&source).expect("source metadata");
        assert_eq!(source_metadata.permissions().mode() & 0o777, mode);
        let src_atime = FileTime::from_last_access_time(&source_metadata);
        let src_mtime = FileTime::from_last_modification_time(&source_metadata);
        assert_eq!(src_atime, atime);
        assert_eq!(src_mtime, mtime);

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .permissions(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        let dest_metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        // upstream: rsync.c:588-589 - without --atimes/-U the access time is left
        // unchanged (ATTRS_SKIP_ATIME); permissions and mtime are preserved.
        assert_ne!(dest_atime, atime, "atime must not be preserved without -U");
        assert_eq!(dest_mtime, mtime);
        assert_eq!(summary.files_copied(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_directory_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source-dir");
        fs::create_dir(&source_dir).expect("create source dir");

        let mode = 0o751;
        fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
            .expect("set directory permissions");
        let atime = FileTime::from_unix_time(1_700_010_000, 0);
        let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
        set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

        let destination_dir = tmp.path().join("dest-dir");
        let config = ClientConfig::builder()
            .transfer_args([source_dir.clone(), destination_dir.clone()])
            .permissions(true)
            .times(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(config.preserve_times());

        let summary = run_client(config).expect("directory copy succeeds");

        let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
        assert!(dest_metadata.is_dir());
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        // upstream: rsync.c:588-589 - directories always skip atime (S_ISDIR sets
        // ATTRS_SKIP_ATIME); permissions and mtime are preserved.
        assert_ne!(dest_atime, atime, "directory atime must never be preserved");
        assert_eq!(dest_mtime, mtime);
        assert!(summary.directories_created() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_updates_existing_directory_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source-tree");
        let source_nested = source_dir.join("nested");
        fs::create_dir_all(&source_nested).expect("create source tree");

        let source_mode = 0o745;
        fs::set_permissions(&source_nested, PermissionsExt::from_mode(source_mode))
            .expect("set source nested permissions");
        let source_atime = FileTime::from_unix_time(1_700_030_000, 1_000_000);
        let source_mtime = FileTime::from_unix_time(1_700_040_000, 2_000_000);
        set_file_times(&source_nested, source_atime, source_mtime)
            .expect("set source nested timestamps");

        let dest_root = tmp.path().join("dest-root");
        fs::create_dir(&dest_root).expect("create dest root");
        let dest_dir = dest_root.join("source-tree");
        let dest_nested = dest_dir.join("nested");
        fs::create_dir_all(&dest_nested).expect("pre-create destination tree");

        let dest_mode = 0o711;
        fs::set_permissions(&dest_nested, PermissionsExt::from_mode(dest_mode))
            .expect("set dest nested permissions");
        let dest_atime = FileTime::from_unix_time(1_600_000_000, 0);
        let dest_mtime = FileTime::from_unix_time(1_600_100_000, 0);
        set_file_times(&dest_nested, dest_atime, dest_mtime).expect("set dest nested timestamps");

        let config = ClientConfig::builder()
            .transfer_args([source_dir.clone(), dest_root.clone()])
            .permissions(true)
            .times(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(config.preserve_times());

        let _summary = run_client(config).expect("directory copy succeeds");

        let copied_nested = dest_root.join("source-tree").join("nested");
        let copied_metadata = fs::metadata(&copied_nested).expect("dest metadata");
        assert!(copied_metadata.is_dir());
        assert_eq!(copied_metadata.permissions().mode() & 0o777, source_mode);
        let copied_atime = FileTime::from_last_access_time(&copied_metadata);
        let copied_mtime = FileTime::from_last_modification_time(&copied_metadata);
        // upstream: rsync.c:588-589 - directories always skip atime; the existing
        // directory's permissions and mtime are updated to the source values.
        assert_ne!(
            copied_atime, source_atime,
            "directory atime must never be preserved"
        );
        assert_eq!(copied_mtime, source_mtime);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_sparse_copy_creates_holes() {
        use std::io::{Seek, SeekFrom, Write};
        use std::os::unix::fs::MetadataExt;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("sparse-source.bin");
        let mut source_file = fs::File::create(&source).expect("create source");
        source_file.write_all(&[0x11]).expect("write leading");
        source_file
            .seek(SeekFrom::Start(1024 * 1024))
            .expect("seek to hole");
        source_file.write_all(&[0x22]).expect("write middle");
        source_file
            .seek(SeekFrom::Start(4 * 1024 * 1024))
            .expect("seek to tail");
        source_file.write_all(&[0x33]).expect("write tail");
        source_file.set_len(6 * 1024 * 1024).expect("extend source");

        let dense_dest = tmp.path().join("dense.bin");
        let sparse_dest = tmp.path().join("sparse.bin");

        let dense_config = ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                dense_dest.clone().into_os_string(),
            ])
            .permissions(true)
            .times(true)
            .build();
        let summary = run_client(dense_config).expect("dense copy succeeds");
        assert!(summary.events().is_empty());

        let sparse_config = ClientConfig::builder()
            .transfer_args([
                source.into_os_string(),
                sparse_dest.clone().into_os_string(),
            ])
            .permissions(true)
            .times(true)
            .sparse(true)
            .build();
        let summary = run_client(sparse_config).expect("sparse copy succeeds");
        assert!(summary.events().is_empty());

        let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
        let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

        assert_eq!(dense_meta.len(), sparse_meta.len());

        let dense_blocks = dense_meta.blocks();
        let sparse_blocks = sparse_meta.blocks();

        // On filesystems with compression or automatic hole punching (APFS, btrfs, ZFS, etc.)
        // a "dense" write of zeros can already be stored efficiently. In that case the sparse
        // copy may use the same number of blocks as the dense copy. The portable guarantee
        // we care about is that a sparse copy never uses *more* blocks than a dense copy of
        // the same contents.
        assert!(
            sparse_blocks <= dense_blocks,
            "sparse copy must not use more blocks than dense copy (sparse={sparse_blocks}, dense={dense_blocks})",
        );
    }

    #[test]
    fn run_client_merges_directory_contents_when_trailing_separator_present() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        let file_path = nested.join("file.txt");
        fs::write(&file_path, b"contents").expect("write file");

        let dest_root = tmp.path().join("dest");
        let mut source_arg = source_root.clone().into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .build();

        let summary = run_client(config).expect("directory contents copy succeeds");

        assert!(dest_root.is_dir());
        assert!(dest_root.join("nested").is_dir());
        assert_eq!(
            fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
            b"contents"
        );
        assert!(!dest_root.join("source").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn sequential_runs_respect_ignore_times() {
        use std::time::Duration;

        use filetime::{FileTime, set_file_times};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("destination.txt");
        fs::write(&source, b"newdata").expect("write source");
        fs::write(&destination, b"olddata").expect("write destination");

        let timestamp = std::time::UNIX_EPOCH + Duration::from_secs(1_700_200_000);
        let filetime = FileTime::from_system_time(timestamp);
        set_file_times(&source, filetime, filetime).expect("source times");
        set_file_times(&destination, filetime, filetime).expect("dest times");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];

        let baseline_config = ClientConfig::builder()
            .transfer_args(operands.clone())
            .build();
        run_client(baseline_config).expect("baseline run");
        assert_eq!(fs::read(&destination).expect("read dest"), b"olddata");

        let ignore_config = ClientConfig::builder()
            .transfer_args(operands)
            .ignore_times(true)
            .build();
        run_client(ignore_config).expect("ignore run");
        assert_eq!(fs::read(&destination).expect("read dest"), b"newdata");
    }
}

#[cfg(test)]
mod local_copy_option_wiring_tests {
    use super::build_local_copy_options;
    use crate::client::config::{ClientConfig, TransferTimeout};
    use std::ffi::OsString;
    use std::num::NonZeroU64;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn local_copy_options_apply_explicit_timeout() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .timeout(timeout)
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(options.timeout(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn local_copy_options_apply_modify_window() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .modify_window(Some(3))
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(
            options.modify_window(),
            ::metadata::ModifyWindow::from_secs(3)
        );

        let default_config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();
        assert_eq!(
            build_local_copy_options(&default_config, None).modify_window(),
            ::metadata::ModifyWindow::ZERO
        );
    }

    #[test]
    fn local_copy_options_omit_timeout_when_unset() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let options = build_local_copy_options(&config, None);
        assert!(options.timeout().is_none());
    }

    #[test]
    fn local_copy_options_delay_updates_enable_partial_transfers() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delay_updates(true)
            .build();

        let enabled_options = build_local_copy_options(&enabled, None);
        assert!(enabled_options.delay_updates_enabled());
        assert!(enabled_options.partial_enabled());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let disabled_options = build_local_copy_options(&disabled, None);
        assert!(!disabled_options.delay_updates_enabled());
        assert!(!disabled_options.partial_enabled());
    }

    #[test]
    fn local_copy_options_honour_temp_directory_setting() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .temp_directory(Some(PathBuf::from(".staging")))
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(options.temp_directory_path(), Some(Path::new(".staging")));

        let default_config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(
            build_local_copy_options(&default_config, None)
                .temp_directory_path()
                .is_none()
        );
    }

    /// `--ignore-errors` lets the delete pass run despite a general I/O error
    /// (upstream: generator.c:304). The local-copy engine reads it from
    /// `LocalCopyOptions`, so it has to be carried across this bridge - without
    /// it the engine sees the default `false` and prints the upstream skip
    /// notice on a run where upstream stays silent and deletes.
    ///
    /// The engine-side setter has an extensive test file of its own, every case
    /// of which passed while nothing in production ever called it. This asserts
    /// the WIRING, which is the part that was missing.
    #[test]
    fn local_copy_options_carry_ignore_errors() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete(true)
            .ignore_errors(true)
            .build();

        assert!(config.ignore_errors(), "client config must hold the flag");
        assert!(
            build_local_copy_options(&config, None).ignore_errors_enabled(),
            "--ignore-errors must reach the local-copy engine"
        );
    }

    /// ⚠ Weaker than its sibling in `remote::flags`, which destructures
    /// `DeletionConfig` exhaustively so the compiler rejects a newly added field
    /// until someone decides how it crosses the bridge. `LocalCopyOptions` keeps
    /// its fields private to `engine`, so this side can only assert an explicit
    /// list - a new option added there will not fail here on its own.
    ///
    /// CLASS GUARD: the deletion group must survive this bridge as a whole.
    /// `ignore_errors` was carried by neither of oc's two config bridges, so
    /// this asserts every deletion option together rather than one field.
    #[test]
    fn local_copy_options_carry_every_deletion_option() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete(true)
            .delete_excluded(true)
            .ignore_errors(true)
            .max_delete(Some(7))
            .build();

        let options = build_local_copy_options(&config, None);
        assert!(options.delete_extraneous(), "delete");
        assert!(options.delete_excluded_enabled(), "delete_excluded");
        assert!(options.ignore_errors_enabled(), "ignore_errors");
        assert_eq!(options.max_deletion_limit(), Some(7), "max_deletions");
    }

    #[test]
    fn local_copy_options_respect_one_file_system_setting() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .one_file_system(1)
            .build();

        let enabled_options = build_local_copy_options(&enabled, None);
        assert!(enabled.one_file_system());
        assert_eq!(enabled.one_file_system_level(), 1);
        assert!(enabled_options.one_file_system_enabled());
        assert_eq!(enabled_options.one_file_system_level(), 1);

        let double = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .one_file_system(2)
            .build();

        let double_options = build_local_copy_options(&double, None);
        assert!(double.one_file_system());
        assert_eq!(double.one_file_system_level(), 2);
        assert!(double_options.one_file_system_enabled());
        assert_eq!(double_options.one_file_system_level(), 2);

        let default = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let default_options = build_local_copy_options(&default, None);
        assert!(!default.one_file_system());
        assert_eq!(default.one_file_system_level(), 0);
        assert!(!default_options.one_file_system_enabled());
        assert_eq!(default_options.one_file_system_level(), 0);
    }
}

#[cfg(test)]
mod temp_dir_anchor_tests {
    use super::{receiver_working_directory, run_client_with_observer};
    use crate::client::config::ClientConfig;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// A staging name unlikely to exist in whatever directory the test runner
    /// happens to be started from. The whole point of these cases is which
    /// directory a relative `--temp-dir` resolves against, so a name that also
    /// existed in the cwd would let the cwd-relative behaviour pass too.
    const STAGE: &str = "oc-temp-dir-anchor-stage";

    fn tree_with_stage_under(dest_stage: bool) -> TempDir {
        let base = TempDir::new().expect("tempdir");
        let src = base.path().join("src");
        let dest = base.path().join("dest");
        fs::create_dir(&src).expect("src");
        fs::create_dir(&dest).expect("dest");
        fs::write(src.join("f0"), b"PAYLOAD-DATA\n").expect("payload");
        if dest_stage {
            fs::create_dir(dest.join(STAGE)).expect("stage");
        }
        base
    }

    fn run_with_relative_temp_dir(base: &Path) -> Result<(), crate::client::ClientError> {
        let config = ClientConfig::builder()
            .transfer_args([
                OsString::from(base.join("src").join("").as_os_str()),
                OsString::from(base.join("dest").join("").as_os_str()),
            ])
            .temp_directory(Some(STAGE))
            .build();
        run_client_with_observer(config, None).map(|_| ())
    }

    /// upstream: `do_recv()` stats `tmpdir` (main.c:1046-1061) only after
    /// `get_local_name()` has chdir'd the receiver into the destination
    /// (main.c:765/823/852), so a relative `--temp-dir` names a directory
    /// under the DESTINATION. oc has no chdir; before the anchoring fix it
    /// resolved the value against the process cwd and died with
    /// "The temp-dir does not exist".
    #[test]
    fn relative_temp_dir_resolves_against_the_destination() {
        let base = tree_with_stage_under(true);
        let result = run_with_relative_temp_dir(base.path());
        assert!(
            result.is_ok(),
            "a relative --temp-dir naming a directory under the destination \
             must be accepted, got: {:?}",
            result.err()
        );
        assert_eq!(
            fs::read(base.path().join("dest").join("f0")).expect("copied file"),
            b"PAYLOAD-DATA\n"
        );
    }

    /// Non-vacuity companion for the case above: anchoring the value at the
    /// destination must not be mistaken for dropping upstream's existence
    /// check. With no `STAGE` directory anywhere, the run still fails.
    #[test]
    fn relative_temp_dir_absent_from_the_destination_is_still_rejected() {
        let base = tree_with_stage_under(false);
        let result = run_with_relative_temp_dir(base.path());
        let error = result.expect_err("a missing temp-dir must still be refused");
        assert!(
            error.to_string().contains("temp-dir does not exist"),
            "expected upstream's missing-temp-dir diagnostic, got: {error}"
        );
    }

    #[test]
    fn working_directory_is_the_destination_when_it_is_a_directory() {
        let base = TempDir::new().expect("tempdir");
        let dest = base.path().join("dest");
        fs::create_dir(&dest).expect("dest");
        assert_eq!(receiver_working_directory(&dest), dest);
    }

    /// upstream main.c:852 chdirs to the parent when a single file is written.
    #[test]
    fn working_directory_is_the_parent_for_a_file_destination() {
        let base = TempDir::new().expect("tempdir");
        let dest = base.path().join("dest");
        fs::create_dir(&dest).expect("dest");
        let file = dest.join("f0");
        fs::write(&file, b"x").expect("file");
        assert_eq!(receiver_working_directory(&file), dest);
    }

    /// A trailing separator names a directory even before it exists: upstream
    /// creates it and chdirs in (main.c:804-823).
    #[test]
    fn working_directory_honours_a_trailing_separator_on_a_missing_directory() {
        let base = TempDir::new().expect("tempdir");
        let absent = base.path().join("not-yet").join("");
        assert_eq!(receiver_working_directory(&absent), absent);
    }

    /// upstream main.c:838 returns without chdir'ing when the destination has
    /// no path component, leaving relative values on the process cwd.
    #[test]
    fn working_directory_is_the_cwd_for_a_bare_destination_name() {
        assert_eq!(
            receiver_working_directory(Path::new("f0")),
            PathBuf::from(".")
        );
    }
}
