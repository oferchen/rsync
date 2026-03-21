//! Server statistics to client summary conversion.
//!
//! Maps server-side transfer statistics into the client summary format
//! for display, including I/O error exit code propagation.

use std::time::Duration;

use crate::client::summary::ClientSummary;

/// Converts server-side statistics to a client summary.
///
/// Maps statistics returned by the server (receiver or generator) into the
/// format expected by the client summary. The elapsed time is used to calculate
/// the transfer rate (bytes/sec) shown in the summary output.
pub(super) fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;
    use transfer::io_error_flags;

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            let s = LocalCopySummary::from_receiver_stats(
                transfer_stats.files_listed,
                transfer_stats.files_transferred,
                transfer_stats.bytes_received,
                transfer_stats.bytes_sent,
                transfer_stats.total_source_bytes,
                elapsed,
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            let s = LocalCopySummary::from_generator_stats(
                generator_stats.files_listed,
                generator_stats.files_transferred,
                generator_stats.bytes_sent,
                elapsed,
            );
            (s, generator_stats.io_error, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);

    // upstream: log.c - log_exit() converts io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR - treat as partial transfer.
        summary.set_io_error_exit_code(23); // RERR_PARTIAL
    }

    summary
}
