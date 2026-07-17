#![deny(unsafe_code)]

use std::fs::File;
use std::io::{self, Write};

use core::{
    client::{
        ClientConfig, ClientProgressObserver, ClientSummary, HumanReadableMode,
        run_client_with_observer,
    },
    message::Message,
};
use logging::{InfoFlag, info_gte};
use logging_sink::MessageSink;

use crate::frontend::{
    out_format::{OutFormat, OutFormatContext},
    progress::{
        LiveProgress, NameOutputLevel, ProgressMode, ProgressOutputConfig, StderrMode,
        emit_transfer_summary,
    },
};

use super::messages::emit_message_with_fallback;
use super::with_output_writer;

/// Configuration for writing transfer output to a log file.
pub(crate) struct LogFileConfig {
    pub(crate) file: File,
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
    // upstream: flist.c:2251 emits "sending incremental file list" only when
    // `inc_recurse && INFO_GTE(FLIST, 1) && !am_server`. compat.c:172 disables
    // inc_recurse when `!recurse`, so mirror the recursion gate (single-file
    // `-v` transfers get no banner); additionally require the FLIST info
    // category so `--info=flist0` suppresses the banner even at `-v`, matching
    // upstream's per-category gate rather than a raw verbose-level check.
    let emit_flist_banner = config.recursive() && info_gte(InfoFlag::Flist, 1);
    // Capture the preserve-links state before `config` is consumed so the
    // `--list-only` renderer knows whether to append the ` -> <target>` arrow
    // to symlink rows (upstream: generator.c:1183 gates it on preserve_links).
    let preserve_links = config.links();

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
                .with_emit_unchanged(emit_unchanged)
                .with_itemize_repeated(itemize_repeated)
                .with_eight_bit_output(eight_bit_output)
                .with_preserve_links(preserve_links);
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
                    verbosity,
                    stats_level,
                    list_only,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    is_sender,
                    itemize_repeated,
                    show_atimes,
                    show_crtimes,
                    eight_bit_output,
                    preserve_links,
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
    verbosity: u8,
    stats_level: u8,
    list_only: bool,
    name_level: NameOutputLevel,
    name_overridden: bool,
    human_readable_mode: HumanReadableMode,
    /// Whether the local client is the sender. Threaded through so the
    /// itemize direction arrow matches upstream `log.c:701-704`.
    is_sender: bool,
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
}

/// Writes the transfer summary to the configured log file.
fn emit_log_output(params: EmitLogOutputParams<'_>) -> io::Result<()> {
    let EmitLogOutputParams {
        summary,
        log,
        verbosity,
        stats_level,
        list_only,
        name_level,
        name_overridden,
        human_readable_mode,
        is_sender,
        itemize_repeated,
        show_atimes,
        show_crtimes,
        eight_bit_output,
        preserve_links,
    } = params;
    // upstream: generator.c:582-583 - mirror the `INFO_GTE(NAME, 2)` arm of
    // the itemize emit gate in the log-file renderer so `-vv` / `--info=name2`
    // surfaces unchanged entries in the log alongside stdout. The `-ii`
    // (`stdout_format_has_i > 1`) arm forces them independently of `-vv`.
    let emit_unchanged = matches!(name_level, NameOutputLevel::UpdatedAndUnchanged);
    let context = OutFormatContext::with_is_sender(is_sender)
        .with_emit_unchanged(emit_unchanged)
        .with_itemize_repeated(itemize_repeated)
        .with_eight_bit_output(eight_bit_output)
        .with_preserve_links(preserve_links);
    // The log file already carries the parallel "building file list" line via
    // logging::info_log!(Flist, 1, ...); the FCLIENT "sending incremental file
    // list" banner is for stdout only (upstream: flist.c:2248 vs 2252).
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
        // The `Copy method` line is a stdout-only `--info=copy` nicety; keep it
        // out of the log file.
        false,
        show_atimes,
        show_crtimes,
        eight_bit_output,
        &mut log.file,
    )?;
    log.file.flush()
}
