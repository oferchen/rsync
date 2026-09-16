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
pub(crate) fn convert_server_stats_to_summary(
    stats: crate::server::ServerStats,
    elapsed: Duration,
) -> ClientSummary {
    use crate::server::ServerStats;
    use engine::local_copy::LocalCopySummary;

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

    let (local_summary, io_error, got_xfer_error) = match stats {
        ServerStats::Receiver(ref transfer_stats) => {
            // Daemon-pull: local side ran the receiver and its `--delete`
            // sweep. The per-type counters live on `delete_stats`.
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
            // Daemon-upload: local side ran the sender/generator. The remote
            // receiver ran the `--delete` sweep and reported the per-type
            // counters via `NDX_DEL_STATS` during the goodbye phase
            // (see `GeneratorContext::handle_goodbye`).
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
    if !list_only_events.is_empty() {
        summary = summary.with_events(list_only_events);
    }

    // The io_error->RERR_* rule (cleanup.c:210-218, incl. the `|| got_xfer_error`
    // lift to RERR_PARTIAL) has one owner in the exit funnel; both the daemon
    // and SSH stat converters route through it so the rule cannot drift.
    if let Some(exit_code) = crate::exit_code::io_error_exit_code(io_error, got_xfer_error) {
        summary.set_io_error_exit_code(exit_code);
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::convert_server_stats_to_summary;
    use crate::server::ServerStats;
    use std::time::Duration;
    use transfer::TransferStats;

    /// 432 dedup pin (daemon leg). A `got_xfer_error` with no io_error bit lifts
    /// a clean daemon transfer to RERR_PARTIAL through the shared funnel rule.
    /// If this converter's `|| got_xfer_error` arm is dropped or diverges from
    /// the SSH leg's, this turns RED - the two legs can no longer disagree
    /// silently.
    ///
    /// upstream: cleanup.c:217-218.
    #[test]
    fn got_xfer_error_lifts_a_clean_run_to_partial() {
        let stats = ServerStats::Receiver(TransferStats {
            got_xfer_error: true,
            ..TransferStats::default()
        });
        let summary = convert_server_stats_to_summary(stats, Duration::ZERO);
        assert_eq!(summary.io_error_exit_code(), Some(23));
    }

    /// Non-vacuity control: a clean run with neither an io_error bit nor
    /// got_xfer_error owes no exit code.
    #[test]
    fn a_clean_run_owes_no_exit_code() {
        let stats = ServerStats::Receiver(TransferStats::default());
        let summary = convert_server_stats_to_summary(stats, Duration::ZERO);
        assert_eq!(summary.io_error_exit_code(), None);
    }
}
