#![deny(unsafe_code)]

use std::fs::File;
use std::io::{self, Write};

use core::{
    client::{
        ClientConfig, ClientProgressObserver, ClientSummary, HumanReadableMode,
        StrongChecksumAlgorithm, run_client_with_observer,
    },
    message::Message,
};
use logging::{DebugFlag, InfoFlag, LogCode, debug_gte, info_gte};
use logging_sink::{MessageSink, logfile::LogFileWriter};

use crate::frontend::{
    out_format::{OutFormat, OutFormatContext},
    progress::{
        DeltaTransmissionState, DeltaTransmissionSummary, LiveProgress, NameOutputLevel,
        ProgressMode, ProgressOutputConfig, StderrMode, emit_transfer_summary,
    },
};

use super::messages::emit_message_with_fallback;
use super::with_output_writer;

/// Configuration for writing transfer output to a log file.
///
/// The handle is wrapped in a [`LogFileWriter`] so every line the renderers
/// produce is stamped with upstream's `"%Y/%m/%d %H:%M:%S [pid] "` prefix
/// (upstream: log.c:122-132 `logit()`).
pub(crate) struct LogFileConfig {
    pub(crate) file: LogFileWriter<File>,
    pub(crate) format: OutFormat,
}

/// Inputs for driving a client transfer and rendering its output.
pub(crate) struct TransferExecutionInputs<'a> {
    pub(crate) config: ClientConfig,
    pub(crate) msgs_to_stderr: bool,
    pub(crate) stderr_mode: StderrMode,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) progress_output_config: ProgressOutputConfig,
    pub(crate) human_readable_mode: HumanReadableMode,
    pub(crate) itemize_changes: bool,
    /// `-ii` (the `-i` flag repeated) - upstream `stdout_format_has_i > 1`.
    pub(crate) itemize_repeated: bool,
    pub(crate) stats_level: u8,
    pub(crate) verbosity: u8,
    pub(crate) list_only: bool,
    pub(crate) dry_run: bool,
    /// `--only-write-batch` (upstream `write_batch < 0`): appends the
    /// `" (BATCH ONLY)"` speedup suffix in the summary trailer.
    pub(crate) only_write_batch: bool,
    /// `--info=copy`: opt-in to the oc-rsync `Copy method` line that reports
    /// which local-copy I/O acceleration (clonefile/reflink/io_uring) ran.
    pub(crate) show_copy_method: bool,
    /// `-U`/`--atimes`: render the ATIME column in `--list-only` output
    /// (upstream: generator.c list_file_entry() atimes_ndx field).
    pub(crate) show_atimes: bool,
    /// `--crtimes`: render the CRTIME column in `--list-only` output
    /// (upstream: generator.c list_file_entry() crtimes_ndx field).
    pub(crate) show_crtimes: bool,
    pub(crate) out_format_template: Option<&'a crate::frontend::out_format::OutFormat>,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) name_overridden: bool,
    /// `--8-bit-output` / `-8`: pass high-bit characters through without
    /// octal escaping. When false (the default), non-printable bytes in
    /// filenames are escaped as `\#ooo` matching upstream log.c:filtered_fwrite.
    pub(crate) eight_bit_output: bool,
    pub(crate) log_file: Option<LogFileConfig>,
}

/// Drives the client transfer and final summaries.
pub(crate) fn execute_transfer<Out, Err>(
    stdout: &mut Out,
    stderr: &mut MessageSink<Err>,
    inputs: TransferExecutionInputs<'_>,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let TransferExecutionInputs {
        config,
        msgs_to_stderr,
        stderr_mode,
        progress_mode: requested_progress_mode,
        progress_output_config,
        human_readable_mode,
        itemize_changes,
        itemize_repeated,
        stats_level,
        verbosity,
        list_only,
        dry_run,
        only_write_batch,
        show_copy_method,
        show_atimes,
        show_crtimes,
        out_format_template,
        name_level,
        name_overridden,
        eight_bit_output,
        log_file,
    } = inputs;

    // `StderrMode::All` is handled by the caller setting `msgs_to_stderr = true`.
    // `Client` mode applies to remote transfers only (server-side message routing);
    // for local transfers it behaves identically to `Errors`.
    let _ = stderr_mode;

    let mut live_progress = requested_progress_mode.map(|mode| {
        with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
            LiveProgress::with_output_config(
                writer,
                mode,
                human_readable_mode,
                progress_output_config,
            )
        })
    });

    // Capture the sender role before `config` is consumed by the client driver.
    // Threaded into `OutFormatContext` so the itemize renderer picks the correct
    // direction arrow (upstream: log.c:701-704 - `<` for sender, `>` otherwise).
    let is_sender = config.is_local_sender();
    // upstream: log.c `%o` reports `recv` on the receiving client (a pull) and
    // `send` otherwise (a push or a local copy) - a different split than the
    // itemize arrow, so it needs its own signal rather than `is_sender`.
    let is_pull = config.is_pull();
    // upstream: log.c %U/%G render the numeric id only under -o/-g (uid_ndx/
    // gid_ndx); otherwise 0 / "DEFAULT".
    let preserve_owner = config.preserve_owner();
    let preserve_group = config.preserve_group();
    // upstream: flist.c:2251 emits "sending incremental file list" only when
    // `inc_recurse && INFO_GTE(FLIST, 1) && !am_server`. Mirror each gate:
    // - compat.c:172 disables inc_recurse when `!recurse`, so a single-file
    //   `-v` transfer gets no banner (`config.recursive()`);
    // - the FLIST info category gate lets `--info=flist0` suppress it even at
    //   `-v` (`info_gte(Flist, 1)`);
    // - `!am_server` means the sender prints it: the client is the sender on a
    //   push and a local copy, and the receiver on a pull (which separately
    //   prints "receiving incremental file list"), so suppress it on a pull
    //   (`!config.is_pull()`) to avoid printing both banners.
    //
    // A remote push prints the banner live at file-list-send time instead
    // (generator/transfer/orchestrator.rs `announce_incremental_flist`), ahead
    // of the per-file rows that stream straight to stdout during the transfer;
    // rendering it here again would both duplicate it and print it dead last.
    // Only a local copy - whose per-file rows are all rendered post-hoc from
    // the collected events - still gets the banner from this deferred path.
    let emit_flist_banner =
        config.recursive() && info_gte(InfoFlag::Flist, 1) && !config.is_pull() && !is_sender;
    // A pure-local copy (no remote operand): `is_local_sender()` reports false
    // for the local `local_server` case and `is_pull()` is false, so their
    // conjunction uniquely identifies the in-process local-copy path whose
    // generator/sender diagnostics oc renders post-hoc.
    let is_local_transfer = !config.is_pull() && !is_sender;
    // upstream: main.c:650-653 forces whole_file=1 for a local transfer unless
    // the user passed --[no-]whole-file; the local-copy default is therefore
    // whole-file. This decides the `delta-transmission` wording.
    let whole_file = config.whole_file();
    // upstream: generator.c:2291-2294 + match.c:439-446 - the delta-transmission
    // notice fires at DEBUG_GTE(FLIST, 1) and the match_report `total:` line at
    // DEBUG_GTE(DELTASUM, 1); both first activate at -vv and honour --debug
    // overrides. Only wire them for the local-copy path (see #170); network
    // transfers emit them live from the generator/sender.
    let delta_state = if whole_file {
        DeltaTransmissionState::Disabled
    } else {
        DeltaTransmissionState::Enabled
    };
    let delta_notice = DeltaTransmissionSummary {
        notice: (is_local_transfer && debug_gte(DebugFlag::Flist, 1)).then_some(delta_state),
        // upstream: match_report() (match.c:440) is gated on DEBUG_GTE(DELTASUM, 1)
        // alone - `--whole-file` does not suppress it, it just leaves the match
        // counters at zero. MEASURED against rsync 3.4.4: both `-rvv` and
        // `-rvv --no-whole-file` print the line.
        emit_total: is_local_transfer && debug_gte(DebugFlag::Deltasum, 1),
    };
    // Capture the preserve-links state before `config` is consumed so the
    // `--list-only` renderer knows whether to append the ` -> <target>` arrow
    // to symlink rows (upstream: generator.c:1183 gates it on preserve_links).
    let preserve_links = config.links();
    // Capture the negotiated checksum before `config` is consumed so the `%C`
    // renderer reports the negotiated algorithm's digest (upstream: log.c:687-690
    // selects `file_sum_nni` under `--checksum`, else `xfer_sum_nni`).
    let always_checksum = config.checksum();
    let full_checksum_algorithm = if always_checksum {
        config.checksum_choice().file()
    } else {
        config.checksum_choice().transfer()
    };

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_with_observer(config, observer)
    };

    match result {
        Ok(summary) => {
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);
            let suppress_updated_only_totals =
                itemize_changes && stats_level == 0 && verbosity == 0;

            if let Some(observer) = live_progress
                && let Err(error) = observer.finish()
            {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(writer, "warning: failed to render progress output: {error}")
                });
            }

            // upstream: generator.c:582-583 - `INFO_GTE(NAME, 2)` (i.e. `-vv`
            // or `--info=name2`) keeps emitting itemize lines for unchanged
            // entries. Thread the resolved name-level into the renderer so it
            // bypasses the empty-change-set suppression and surfaces all-dot
            // rows for unchanged dirs, files, and symlinks.
            let emit_unchanged = matches!(name_level, NameOutputLevel::UpdatedAndUnchanged);
            let out_format_context = OutFormatContext::with_is_sender(is_sender)
                .with_is_pull(is_pull)
                .with_id_preservation(preserve_owner, preserve_group)
                .with_emit_unchanged(emit_unchanged)
                .with_itemize_repeated(itemize_repeated)
                .with_eight_bit_output(eight_bit_output)
                .with_preserve_links(preserve_links)
                .with_full_checksum(full_checksum_algorithm, always_checksum);
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    requested_progress_mode,
                    stats_level,
                    progress_rendered_live,
                    list_only,
                    dry_run,
                    only_write_batch,
                    out_format_template,
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    suppress_updated_only_totals,
                    emit_flist_banner,
                    delta_notice,
                    show_copy_method,
                    show_atimes,
                    show_crtimes,
                    eight_bit_output,
                    writer,
                )
            }) {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(
                        writer,
                        "warning: failed to render transfer summary: {error}"
                    )
                });
            }

            if let Some(mut log) = log_file
                && let Err(error) = emit_log_output(EmitLogOutputParams {
                    summary: &summary,
                    log: &mut log,
                    flog_events: logging::drain_events_coded(LogCode::Log),
                    verbosity,
                    stats_level,
                    list_only,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    is_sender,
                    is_pull,
                    preserve_owner,
                    preserve_group,
                    itemize_repeated,
                    show_atimes,
                    show_crtimes,
                    eight_bit_output,
                    preserve_links,
                    full_checksum_algorithm,
                    always_checksum,
                })
            {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(writer, "warning: failed to append to log file: {error}")
                });
            }
            // upstream: log.c:log_exit() maps the accumulated io_error /
            // got_xfer_error into RERR_* exit codes. A transfer can complete
            // its summary yet still owe a non-zero code (e.g. a receiver that
            // discarded a file because its output mkstemp() failed reports exit
            // 23 via MSG_ERROR_XFER). Honour it here instead of forcing 0.
            summary.io_error_exit_code().unwrap_or(0)
        }
        Err(error) => {
            if let Some(observer) = live_progress
                && let Err(err) = observer.finish()
            {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(writer, "warning: failed to render progress output: {err}")
                });
            }

            let message: &Message = error.message();
            emit_message_with_fallback(
                message,
                "rsync error: client functionality is unavailable in this build (code 1)",
                stderr,
            );
            error.exit_code()
        }
    }
}

/// Parameters for writing transfer output to a log file.
struct EmitLogOutputParams<'a> {
    summary: &'a ClientSummary,
    log: &'a mut LogFileConfig,
    /// FLOG-classified diagnostic events consumed from the thread-local queue
    /// (e.g. `building file list`, flist.c:2248). These lines belong to the
    /// log file only (upstream: log.c:304-305).
    flog_events: Vec<logging::DiagnosticEvent>,
    verbosity: u8,
    stats_level: u8,
    list_only: bool,
    name_level: NameOutputLevel,
    name_overridden: bool,
    human_readable_mode: HumanReadableMode,
    /// Whether the local client is the sender. Threaded through so the
    /// itemize direction arrow matches upstream `log.c:701-704`.
    is_sender: bool,
    /// Whether the transfer is a pull; drives the `%o` operation word.
    is_pull: bool,
    /// `-o`/`-g` (upstream uid_ndx/gid_ndx): gate the numeric `%U`/`%G`.
    preserve_owner: bool,
    preserve_group: bool,
    /// `-ii` (the `-i` flag repeated) - upstream `stdout_format_has_i > 1`.
    /// Forces unchanged itemize rows in the log file as it does on stdout.
    itemize_repeated: bool,
    /// `-U`/`--atimes`: render the ATIME column in `--list-only` log output.
    show_atimes: bool,
    /// `--crtimes`: render the CRTIME column in `--list-only` log output.
    show_crtimes: bool,
    /// `--8-bit-output` / `-8`: pass high-bit characters through without
    /// octal escaping in log-file output.
    eight_bit_output: bool,
    /// `--links` / `-l`: whether symlink rows in `--list-only` log output get
    /// the ` -> <target>` arrow (upstream: generator.c:1183).
    preserve_links: bool,
    /// Negotiated `%C` checksum algorithm (upstream: log.c:687-690).
    full_checksum_algorithm: StrongChecksumAlgorithm,
    /// `--checksum` / `-c` (upstream `always_checksum`), gating `%C` for
    /// untransferred regular files.
    always_checksum: bool,
}

/// Writes the transfer summary to the configured log file.
fn emit_log_output(params: EmitLogOutputParams<'_>) -> io::Result<()> {
    let EmitLogOutputParams {
        summary,
        log,
        flog_events,
        verbosity,
        stats_level,
        list_only,
        name_level,
        name_overridden,
        human_readable_mode,
        is_sender,
        is_pull,
        preserve_owner,
        preserve_group,
        itemize_repeated,
        show_atimes,
        show_crtimes,
        eight_bit_output,
        preserve_links,
        full_checksum_algorithm,
        always_checksum,
    } = params;
    // upstream: generator.c:582-583 - mirror the `INFO_GTE(NAME, 2)` arm of
    // the itemize emit gate in the log-file renderer so `-vv` / `--info=name2`
    // surfaces unchanged entries in the log alongside stdout. The `-ii`
    // (`stdout_format_has_i > 1`) arm forces them independently of `-vv`.
    let emit_unchanged = matches!(name_level, NameOutputLevel::UpdatedAndUnchanged);
    let context = OutFormatContext::with_is_sender(is_sender)
        .with_is_pull(is_pull)
        .with_id_preservation(preserve_owner, preserve_group)
        .with_emit_unchanged(emit_unchanged)
        .with_itemize_repeated(itemize_repeated)
        .with_eight_bit_output(eight_bit_output)
        .with_preserve_links(preserve_links)
        .with_full_checksum(full_checksum_algorithm, always_checksum);
    // upstream: log.c:290-305 - FLOG messages are written to the log file
    // before any client-stream output and never reach stdout. The queue holds
    // them in emission order, so "building file list" (flist.c:2248) or
    // "receiving file list" (flist.c:2608) precedes the per-file lines.
    for event in &flog_events {
        let (logging::DiagnosticEvent::Info { message, .. }
        | logging::DiagnosticEvent::Debug { message, .. }) = event;
        writeln!(log.file, "{message}")?;
    }
    // The FCLIENT "sending incremental file list" banner is stdout only;
    // upstream's parallel "building file list" line (flist.c:2248) is an FLOG
    // log-file message consumed from the FLOG event queue above.
    emit_transfer_summary(
        summary,
        verbosity,
        None,
        stats_level,
        false,
        list_only,
        false, // dry_run
        false, // only_write_batch
        Some(&log.format),
        &context,
        name_level,
        name_overridden,
        human_readable_mode,
        false,
        false,
        // The delta-transmission notice and match_report `total:` line are
        // stdout-only diagnostics (upstream FINFO); the log file is fed from the
        // FLOG queue above, so suppress them here.
        DeltaTransmissionSummary::default(),
        // The `Copy method` line is a stdout-only `--info=copy` nicety; keep it
        // out of the log file.
        false,
        show_atimes,
        show_crtimes,
        eight_bit_output,
        &mut log.file,
    )?;
    // upstream: cleanup.c:222-226 gates log_exit() on
    // `logfile_name && (am_server || !INFO_GTE(STATS, 1))`, and log_exit()
    // writes the FLOG-only totals trailer
    // `sent %s bytes  received %s bytes  total size %s` via `big_num()`
    // (plain digits, inums.h:19-23) at log.c:894-899. A `-v`/`--stats` run
    // raises STATS >= 1 and mirrors the FINFO trailer into the log instead
    // (rendered by emit_transfer_summary above).
    if verbosity == 0 && stats_level == 0 {
        writeln!(
            log.file,
            "sent {} bytes  received {} bytes  total size {}",
            summary.bytes_sent(),
            summary.bytes_received(),
            summary.total_source_bytes(),
        )?;
    }
    log.file.flush()
}
