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
    progress::{LiveProgress, NameOutputLevel, ProgressMode, emit_transfer_summary},
};

use super::messages::emit_message_with_fallback;
use super::with_output_writer;

/// Configuration for writing transfer output to a log file.
pub(crate) struct LogFileConfig {
    pub(crate) file: File,
    pub(crate) format: OutFormat,
}

pub(crate) struct TransferExecutionInputs<'a> {
    pub(crate) config: ClientConfig,
    pub(crate) msgs_to_stderr: bool,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) human_readable_mode: HumanReadableMode,
    pub(crate) itemize_changes: bool,
    pub(crate) stats: bool,
    pub(crate) verbosity: u8,
    pub(crate) list_only: bool,
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
        progress_mode: requested_progress_mode,
        human_readable_mode,
        itemize_changes,
        stats,
        verbosity,
        list_only,
        out_format_template,
        name_level,
        name_overridden,
        log_file,
    } = inputs;

    let mut live_progress = requested_progress_mode.map(|mode| {
        with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
            LiveProgress::new(writer, mode, human_readable_mode)
        })
    });

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_with_observer(config, observer)
    };

    match result {
        Ok(summary) => {
            // let summary = summary;
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);
            let suppress_updated_only_totals = itemize_changes && !stats && verbosity == 0;

            if let Some(observer) = live_progress
                && let Err(error) = observer.finish()
            {
                let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                    writeln!(writer, "warning: failed to render progress output: {error}")
                });
            }

            let out_format_context = OutFormatContext::default();
            if let Err(error) = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                emit_transfer_summary(
                    &summary,
                    verbosity,
                    requested_progress_mode,
                    stats,
                    progress_rendered_live,
                    list_only,
                    out_format_template,
                    &out_format_context,
                    name_level,
                    name_overridden,
                    human_readable_mode,
                    suppress_updated_only_totals,
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
                    stats,
                    list_only,
                    name_level,
                    name_overridden,
                    human_readable_mode,
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

struct EmitLogOutputParams<'a> {
    summary: &'a ClientSummary,
    log: &'a mut LogFileConfig,
    verbosity: u8,
    stats: bool,
    list_only: bool,
    name_level: NameOutputLevel,
    name_overridden: bool,
    human_readable_mode: HumanReadableMode,
}

fn emit_log_output(params: EmitLogOutputParams<'_>) -> io::Result<()> {
    let EmitLogOutputParams {
        summary,
        log,
        verbosity,
        stats,
        list_only,
        name_level,
        name_overridden,
        human_readable_mode,
    } = params;
    let context = OutFormatContext::default();
    emit_transfer_summary(
        summary,
        verbosity,
        None,
        stats,
        false,
        list_only,
        Some(&log.format),
        &context,
        name_level,
        name_overridden,
        human_readable_mode,
        false,
        &mut log.file,
    )?;
    log.file.flush()
}
