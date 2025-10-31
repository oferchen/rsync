#![deny(unsafe_code)]

use std::io::{self, Write};

use rsync_core::{
    client::{
        ClientConfig, ClientOutcome, ClientProgressObserver, HumanReadableMode, RemoteFallbackArgs,
        RemoteFallbackContext, run_client_or_fallback,
    },
    message::Message,
};
use rsync_logging::MessageSink;

use crate::frontend::{
    out_format::OutFormatContext,
    progress::{LiveProgress, NameOutputLevel, ProgressMode, emit_transfer_summary},
};

use super::messages::emit_message_with_fallback;
use super::with_output_writer;

pub(crate) struct TransferExecutionInputs<'a> {
    pub(crate) config: ClientConfig,
    pub(crate) fallback_args: Option<RemoteFallbackArgs>,
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
}

/// Drives the client transfer, handling optional fallback execution and final summaries.
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
        fallback_args,
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
    } = inputs;

    if let Some(args) = fallback_args {
        let outcome = {
            let mut stderr_writer = stderr.writer_mut();
            run_client_or_fallback(
                config,
                None,
                Some(RemoteFallbackContext::new(stdout, &mut stderr_writer, args)),
            )
        };

        return match outcome {
            Ok(ClientOutcome::Fallback(summary)) => summary.exit_code(),
            Ok(ClientOutcome::Local(_)) => {
                unreachable!("local outcome returned without fallback context")
            }
            Err(error) => {
                let message = error.message();
                let fallback = message.to_string();
                emit_message_with_fallback(message, &fallback, stderr);
                error.exit_code()
            }
        };
    }

    let mut live_progress = requested_progress_mode.map(|mode| {
        with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
            LiveProgress::new(writer, mode, human_readable_mode)
        })
    });

    let result = {
        let observer = live_progress
            .as_mut()
            .map(|observer| observer as &mut dyn ClientProgressObserver);
        run_client_or_fallback::<io::Sink, io::Sink>(config, observer, None)
    };

    match result {
        Ok(ClientOutcome::Local(summary)) => {
            let summary = *summary;
            let progress_rendered_live = live_progress.as_ref().is_some_and(LiveProgress::rendered);
            let suppress_updated_only_totals = itemize_changes && !stats && verbosity == 0;

            if let Some(observer) = live_progress {
                if let Err(error) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {error}")
                    });
                }
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
            0
        }
        Ok(ClientOutcome::Fallback(_)) => {
            unreachable!("fallback outcome returned without fallback args")
        }
        Err(error) => {
            if let Some(observer) = live_progress {
                if let Err(err) = observer.finish() {
                    let _ = with_output_writer(stdout, stderr, msgs_to_stderr, |writer| {
                        writeln!(writer, "warning: failed to render progress output: {err}")
                    });
                }
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
