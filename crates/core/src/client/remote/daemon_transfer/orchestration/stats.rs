//! Server statistics to client summary conversion.
//!
//! Maps server-side transfer statistics into the client summary format
//! for display, including I/O error exit code propagation.

use std::time::Duration;

use crate::client::summary::{
    ClientEntryMetadata, ClientEvent, ClientSummary, ListOnlyEntryFields,
};

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

    // upstream: generator.c:1249 - in list-only mode the receiver captures every
    // flist entry's metadata instead of requesting file data; convert those into
    // metadata-bearing events so the client can render the listing.
    let list_only_events: Vec<ClientEvent> = match stats {
        ServerStats::Receiver(ref transfer_stats) => transfer_stats
            .list_only_entries
            .iter()
            .map(|entry| {
                let metadata = ClientEntryMetadata::from_list_only_entry(&ListOnlyEntryFields {
                    mode: entry.mode,
                    size: entry.size,
                    mtime: entry.mtime,
                    mtime_nsec: entry.mtime_nsec,
                    atime: entry.atime,
                    atime_nsec: entry.atime_nsec,
                    crtime: entry.crtime,
                    crtime_nsec: entry.crtime_nsec,
                    symlink_target: entry.symlink_target.clone(),
                    is_symlink: entry.is_symlink,
                });
                ClientEvent::from_list_only_entry(entry.path.clone(), metadata)
            })
            .collect(),
        ServerStats::Generator(_) => Vec::new(),
    };

    let (local_summary, io_error, error_count) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            // Daemon-pull: local side ran the receiver and its `--delete`
            // sweep. The per-type counters live on `delete_stats`.
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
            );
            (s, transfer_stats.io_error, transfer_stats.error_count)
        }
        ServerStats::Generator(ref generator_stats) => {
            // Daemon-upload: local side ran the sender/generator. The remote
            // receiver ran the `--delete` sweep and reported the per-type
            // counters via `NDX_DEL_STATS` during the goodbye phase
            // (see `GeneratorContext::handle_goodbye`).
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
            );
            (s, generator_stats.io_error, 0u32)
        }
    };

    let mut summary = ClientSummary::from_summary(local_summary);
    if !list_only_events.is_empty() {
        summary = summary.with_events(list_only_events);
    }

    // upstream: log.c - log_exit() converts io_error bitfield to RERR_* codes.
    let exit_code = io_error_flags::to_exit_code(io_error);
    if exit_code != 0 {
        summary.set_io_error_exit_code(exit_code);
    } else if error_count > 0 {
        // Remote sender reported errors via MSG_ERROR - treat as RERR_PARTIAL (23).
        summary.set_io_error_exit_code(23);
    }

    summary
}
