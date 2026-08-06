//! Result mapping: server statistics to client summary and SSH child
//! exit-status to rsync exit codes.
//!
//! These helpers translate the outcome of an SSH transfer (the server stats and
//! the remote child's exit status) into the client-facing summary and error
//! surfaces, mirroring upstream `main.c:wait_process_with_flush()` and
//! `log.c:log_exit()`. The remote child's stderr is streamed to our stderr in
//! real time by the SSH aux-channel drain, not formatted here.

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

    let (local_summary, io_error, got_xfer_error) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            // SSH-pull: local side ran the receiver and its `--delete` sweep.
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.transferred_file_size,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
                transfer_stats.literal_data,
                transfer_stats.matched_data,
                transfer_stats.flist_size,
                transfer_stats.delete_stats,
                transfer_stats.created_stats,
                engine::local_copy::FileTypeTotals {
                    dirs: transfer_stats.num_dirs,
                    symlinks: transfer_stats.num_symlinks,
                    devices: transfer_stats.num_devices,
                    specials: transfer_stats.num_specials,
                },
            );
            (s, transfer_stats.io_error, transfer_stats.got_xfer_error)
        }
        ServerStats::Generator(ref generator_stats) => {
            // SSH-push: local side ran the sender/generator; the remote
            // receiver reported its delete counters via `NDX_DEL_STATS`.
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.transferred_file_size,
                generator_stats.bytes_read,
                generator_stats.bytes_sent,
                generator_stats.total_size,
                elapsed,
                generator_stats.literal_data,
                generator_stats.matched_data,
                generator_stats.delete_stats,
                generator_stats.created_stats,
                engine::local_copy::FileTypeTotals {
                    dirs: generator_stats.num_dirs,
                    symlinks: generator_stats.num_symlinks,
                    devices: generator_stats.num_devices,
                    specials: generator_stats.num_specials,
                },
            );
            (s, generator_stats.io_error, generator_stats.got_xfer_error)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // upstream: log.c log_exit() - convert io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if got_xfer_error {
        // upstream: cleanup.c:217-218 - `io_error & IOERR_GENERAL ||
        // got_xfer_error` lifts a zero exit to RERR_PARTIAL. This is the only
        // arm a missing source argument reaches, since `flist.c:2431` withholds
        // IOERR_GENERAL for ENOENT.
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

#[cfg(all(test, windows))]
mod windows_exit_status_tests {
    use super::map_child_exit_status;
    use crate::exit_code::ExitCode;
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;

    // #202: On Windows the `#[cfg(unix)]` signal branch of `map_child_exit_status`
    // is compiled out, so every non-success child status is classified purely by
    // its exit code. These pins assert the upstream `main.c:221` contract that
    // `WEXITSTATUS` is propagated raw into the worst-wins comparison
    // (cleanup.c:150), driving the mapper directly (no live SSH needed).

    #[test]
    fn windows_exit_zero_maps_to_ok() {
        assert_eq!(map_child_exit_status(ExitStatus::from_raw(0)), ExitCode::Ok);
    }

    #[test]
    fn windows_command_not_found_127_maps_to_named_variant() {
        // An SSH "command not found" child exit (127) IS a known RERR_ code
        // (upstream errcode.h:64 RERR_CMD_NOTFOUND = 127), so it maps to the
        // named `ExitCode::CommandNotFound` - not `Other(127)`. Its numeric value
        // is still preserved (CommandNotFound -> 127), so it round-trips verbatim
        // into the worst-wins comparison; only the representation is named.
        let mapped = map_child_exit_status(ExitStatus::from_raw(127));
        assert_eq!(mapped, ExitCode::CommandNotFound);
        assert_eq!(mapped.as_i32(), 127);
    }

    #[test]
    fn windows_connection_failure_255_round_trips_verbatim() {
        // An SSH connection failure (255) likewise round-trips verbatim.
        assert_eq!(
            map_child_exit_status(ExitStatus::from_raw(255)),
            ExitCode::Other(255)
        );
    }

    #[test]
    fn windows_known_rerr_code_keeps_its_named_variant() {
        // A known RERR_* code (12 = `RERR_STREAMIO`) maps to its named variant,
        // not `ExitCode::Other`, but its numeric value is preserved so the
        // worst-wins comparison still sees 12.
        let mapped = map_child_exit_status(ExitStatus::from_raw(12));
        assert_eq!(mapped, ExitCode::from_raw(12));
        assert_eq!(mapped.as_i32(), 12);
        assert!(
            !matches!(mapped, ExitCode::Other(_)),
            "a known RERR_* code must keep its named variant, not fall to Other"
        );
    }
}
