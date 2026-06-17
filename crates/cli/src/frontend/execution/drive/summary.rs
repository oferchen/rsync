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
    pub(crate) stats_level: u8,
    pub(crate) verbosity: u8,
    pub(crate) list_only: bool,
    pub(crate) dry_run: bool,
    pub(crate) out_format_template: Option<&'a crate::frontend::out_format::OutFormat>,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) name_overridden: bool,
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
        stats_level,
        verbosity,
        list_only,
        dry_run,
        out_format_template,
        name_level,
        name_overridden,
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
    // inc_recurse is on, which compat.c:172 disables when `!recurse`. Mirror
    // that gate so single-file (`-v`) transfers don't get a spurious banner.
    let recursive = config.recursive();

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

            let out_format_context = OutFormatContext::with_is_sender(is_sender);
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    requested_progress_mode,
                    stats_level,
                    progress_rendered_live,
                    list_only,
                    dry_run,
                    out_format_template,
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    suppress_updated_only_totals,
                    recursive,
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
                })
            {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(writer, "warning: failed to append to log file: {error}")
                });
            }
            0
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
    } = params;
    let context = OutFormatContext::with_is_sender(is_sender);
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
        false,
        Some(&log.format),
        &context,
        name_level,
        name_overridden,
        human_readable_mode,
        false,
        false,
        &mut log.file,
    )?;
    log.file.flush()
}
