//! Result mapping: server statistics to client summary, SSH child exit-status
//! to rsync exit codes, and stderr context formatting.
//!
//! These helpers translate the outcome of an SSH transfer (the server stats,
//! the remote child's exit status, and any captured stderr) into the
//! client-facing summary and error surfaces, mirroring upstream
//! `main.c:wait_process_with_flush()` and `log.c:log_exit()`.

use std::time::Duration;

use super::super::super::summary::ClientSummary;
use crate::exit_code::ExitCode;

/// Converts server-side statistics to a client summary.
///
/// Maps the statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. Uses the available server statistics
/// (files listed, files transferred, and bytes sent/received) to create a
/// LocalCopySummary with the most relevant fields populated. The elapsed time
/// is used to calculate the transfer rate (bytes/sec) shown in the summary output.
pub(in crate::client::remote) fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;
    use transfer::io_error_flags;

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            // SSH-pull: local side ran the receiver and its `--delete` sweep.
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
                transfer_stats.literal_data,
                transfer_stats.matched_data,
                transfer_stats.delete_stats,
                transfer_stats.created_stats,
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            // SSH-push: local side ran the sender/generator; the remote
            // receiver reported its delete counters via `NDX_DEL_STATS`.
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_read,
                generator_stats.bytes_sent,
                generator_stats.total_size,
                elapsed,
                generator_stats.literal_data,
                generator_stats.matched_data,
                generator_stats.delete_stats,
                generator_stats.created_stats,
            );
            (s, generator_stats.io_error, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // upstream: log.c log_exit() - convert io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR - treat as RERR_PARTIAL.
        summary.set_io_error_exit_code(23);
    }

    summary
}

/// Maps an SSH child process exit status to an rsync exit code.
///
/// Mirrors upstream rsync's `wait_process_with_flush()` in `main.c:194`:
/// - Normal exit: `WEXITSTATUS(status)` is propagated raw. Known RERR_*
///   codes keep their named variant; any other status (for example a remote
///   rsync exit 42 or an SSH connection failure exit 255) round-trips
///   verbatim via [`ExitCode::Other`].
/// - Killed by signal with a core dump: `RERR_CRASHED` (15).
/// - Killed by signal without a core dump: `RERR_TERMINATED` (16).
/// - `waitpid()` failure / did not exit normally: `RERR_WAITCHILD` (21).
///
/// The resulting raw i32 flows unchanged into the worst-wins comparison
/// (cleanup.c:150) and ultimately to `process::exit`.
///
/// upstream: main.c:194 `wait_process_with_flush()`; cleanup.c:150.
pub(in crate::client::remote) fn map_child_exit_status(
    status: std::process::ExitStatus,
) -> ExitCode {
    if status.success() {
        return ExitCode::Ok;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // upstream: main.c:208-217 - a child that did not exit normally maps to
        // RERR_CRASHED (core dump) or RERR_TERMINATED (killed by signal).
        if status.signal().is_some() {
            return if status.core_dumped() {
                ExitCode::Crashed
            } else {
                ExitCode::Terminated
            };
        }
    }

    match status.code() {
        // upstream: main.c:221 - WEXITSTATUS is returned raw, never collapsed.
        Some(code) => ExitCode::from_raw(code),
        // upstream: main.c:206/218 - a failed waitpid() maps to RERR_WAITCHILD.
        None => ExitCode::WaitChild,
    }
}

/// Formats captured SSH stderr output as a suffix for error messages.
///
/// Returns an empty string when `stderr_bytes` is empty. Otherwise returns
/// a newline-separated block prefixed with "SSH stderr:" that gives the user
/// visibility into what the remote process wrote to stderr before exiting.
/// The output is trimmed to remove trailing whitespace.
pub(in crate::client::remote) fn format_stderr_context(stderr_bytes: &[u8]) -> String {
    if stderr_bytes.is_empty() {
        return String::new();
    }
    let text = String::from_utf8_lossy(stderr_bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    format!("\nSSH stderr:\n{trimmed}")
}
