//! Transfer summary and event tracking.
//!
//! This module provides comprehensive statistics and event logs for completed
//! client transfers. The [`ClientSummary`] structure aggregates counters for
//! files copied, bytes transferred, and time spent, while the [`ClientEvent`]
//! type describes individual file-level actions taken during the transfer.
//!
//! These types enable post-transfer analysis, test assertions, and status
//! displays that mirror the output of upstream rsync's `--stats` and
//! `--itemize-changes` flags.
//!
//! # Upstream Reference
//!
//! - `main.c:output_summary()` - Stats display after transfer
//! - `log.c:log_item()` - Per-file itemize output
//!
//! # Examples
//!
//! ```ignore
//! use core::client::{ClientConfig, run_client};
//!
//! let config = ClientConfig::builder()
//!     .transfer_args(["source/", "dest/"])
//!     .build();
//!
//! let summary = run_client(config)?;
//! println!("Files copied: {}", summary.files_copied());
//! println!("Bytes transferred: {}", summary.bytes_copied());
//! println!("Total elapsed: {:?}", summary.total_elapsed());
//!
//! for event in summary.events() {
//!     println!("{:?} {}", event.kind(), event.relative_path().display());
//! }
//! ```

mod event;
mod metadata;

pub use self::event::{ClientEvent, ClientEventKind};
pub use self::metadata::{ClientEntryKind, ClientEntryMetadata};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use engine::local_copy::{LocalCopyReport, LocalCopySummary};

/// Summary of the work performed by a client transfer.
#[derive(Clone, Debug, Default)]
pub struct ClientSummary {
    stats: LocalCopySummary,
    events: Vec<ClientEvent>,
    /// Optional exit code derived from server-side I/O error flags.
    ///
    /// When set, indicates the transfer completed with I/O errors that should
    /// be reflected in the process exit code. Maps to upstream rsync exit codes
    /// such as `RERR_PARTIAL` (23), `RERR_VANISHED` (24), or
    /// `RERR_DEL_LIMIT` (25).
    io_error_exit_code: Option<i32>,
}

impl ClientSummary {
    pub(crate) fn from_report(report: LocalCopyReport) -> Self {
        let stats = *report.summary();
        let destination_root: Arc<Path> = Arc::from(report.destination_root());
        let events = report
            .records()
            .iter()
            .map(|record| ClientEvent::from_record(record, Arc::clone(&destination_root)))
            .collect();
        Self {
            stats,
            events,
            io_error_exit_code: None,
        }
    }

    #[allow(clippy::large_types_passed_by_value)] // REASON: constructor takes ownership
    pub(crate) const fn from_summary(summary: LocalCopySummary) -> Self {
        Self {
            stats: summary,
            events: Vec::new(),
            io_error_exit_code: None,
        }
    }

    /// Returns the list of recorded transfer actions.
    #[must_use]
    pub fn events(&self) -> &[ClientEvent] {
        &self.events
    }

    /// Consumes the summary and returns the recorded actions.
    #[must_use]
    pub fn into_events(self) -> Vec<ClientEvent> {
        self.events
    }

    /// Returns the number of regular files copied or updated during the transfer.
    #[must_use]
    pub const fn files_copied(&self) -> u64 {
        self.stats.files_copied()
    }

    /// Returns the number of regular files encountered in the source set.
    #[must_use]
    pub const fn regular_files_total(&self) -> u64 {
        self.stats.regular_files_total()
    }

    /// Returns the number of regular files that were already up-to-date.
    #[must_use]
    pub const fn regular_files_matched(&self) -> u64 {
        self.stats.regular_files_matched()
    }

    /// Returns the number of regular files skipped due to `--ignore-existing`.
    #[must_use]
    pub const fn regular_files_ignored_existing(&self) -> u64 {
        self.stats.regular_files_ignored_existing()
    }

    /// Returns the number of regular files skipped because the destination was absent and `--existing` was requested.
    #[must_use]
    #[doc(alias = "--existing")]
    pub const fn regular_files_skipped_missing(&self) -> u64 {
        self.stats.regular_files_skipped_missing()
    }

    /// Returns the number of regular files skipped because the destination was newer.
    #[must_use]
    pub const fn regular_files_skipped_newer(&self) -> u64 {
        self.stats.regular_files_skipped_newer()
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub const fn directories_created(&self) -> u64 {
        self.stats.directories_created()
    }

    /// Returns the number of directories encountered in the source set.
    #[must_use]
    pub const fn directories_total(&self) -> u64 {
        self.stats.directories_total()
    }

    /// Returns the number of symbolic links copied during the transfer.
    #[must_use]
    pub const fn symlinks_copied(&self) -> u64 {
        self.stats.symlinks_copied()
    }

    /// Returns the number of symbolic links encountered in the source set.
    #[must_use]
    pub const fn symlinks_total(&self) -> u64 {
        self.stats.symlinks_total()
    }

    /// Returns the number of hard links materialised during the transfer.
    #[must_use]
    pub const fn hard_links_created(&self) -> u64 {
        self.stats.hard_links_created()
    }

    /// Returns the number of device nodes created during the transfer.
    #[must_use]
    pub const fn devices_created(&self) -> u64 {
        self.stats.devices_created()
    }

    /// Returns the number of device nodes encountered in the source set.
    #[must_use]
    pub const fn devices_total(&self) -> u64 {
        self.stats.devices_total()
    }

    /// Returns the number of FIFOs created during the transfer.
    #[must_use]
    pub const fn fifos_created(&self) -> u64 {
        self.stats.fifos_created()
    }

    /// Returns the number of FIFOs encountered in the source set.
    #[must_use]
    pub const fn fifos_total(&self) -> u64 {
        self.stats.fifos_total()
    }

    /// Returns the number of extraneous entries removed due to `--delete`.
    #[must_use]
    pub const fn items_deleted(&self) -> u64 {
        self.stats.items_deleted()
    }

    /// Returns the aggregate number of bytes copied.
    #[must_use]
    pub const fn bytes_copied(&self) -> u64 {
        self.stats.bytes_copied()
    }

    /// Returns the aggregate number of bytes reused from the destination instead of being
    /// rewritten during the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn matched_bytes(&self) -> u64 {
        self.stats.matched_bytes()
    }

    /// Returns the aggregate number of bytes received during the transfer.
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.stats.bytes_received()
    }

    /// Returns the aggregate number of bytes sent during the transfer.
    #[must_use]
    pub const fn bytes_sent(&self) -> u64 {
        self.stats.bytes_sent()
    }

    /// Returns the aggregate size of files that were rewritten or created.
    #[must_use]
    pub const fn transferred_file_size(&self) -> u64 {
        self.stats.transferred_file_size()
    }

    /// Returns the number of bytes that would be sent after applying compression.
    pub const fn compressed_bytes(&self) -> Option<u64> {
        if self.stats.compression_used() {
            Some(self.stats.compressed_bytes())
        } else {
            None
        }
    }

    /// Reports whether compression participated in the transfer.
    #[must_use]
    pub const fn compression_used(&self) -> bool {
        self.stats.compression_used()
    }

    /// Returns the number of source entries removed due to `--remove-source-files`.
    #[must_use]
    pub const fn sources_removed(&self) -> u64 {
        self.stats.sources_removed()
    }

    /// Returns the aggregate size of all source files considered during the transfer.
    #[must_use]
    pub const fn total_source_bytes(&self) -> u64 {
        self.stats.total_source_bytes()
    }

    /// Returns the total elapsed time spent transferring file payloads.
    #[must_use]
    pub const fn total_elapsed(&self) -> Duration {
        self.stats.total_elapsed()
    }

    /// Returns the cumulative duration spent sleeping due to bandwidth throttling.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_sleep(&self) -> Duration {
        self.stats.bandwidth_sleep()
    }

    /// Returns the number of bytes that would be transmitted for the file list.
    #[must_use]
    pub const fn file_list_size(&self) -> u64 {
        self.stats.file_list_size()
    }

    /// Returns the duration spent generating the in-memory file list.
    #[must_use]
    pub const fn file_list_generation_time(&self) -> Duration {
        self.stats.file_list_generation_time()
    }

    /// Returns the duration spent transmitting the file list to the peer.
    #[must_use]
    pub const fn file_list_transfer_time(&self) -> Duration {
        self.stats.file_list_transfer_time()
    }

    /// Returns the I/O error exit code when the transfer finished with errors.
    ///
    /// Maps to upstream rsync exit codes such as `RERR_PARTIAL` (23),
    /// `RERR_VANISHED` (24), or `RERR_DEL_LIMIT` (25). Returns `None`
    /// when the transfer completed without I/O errors.
    pub const fn io_error_exit_code(&self) -> Option<i32> {
        self.io_error_exit_code
    }

    /// Records an exit code derived from server-side I/O error flags.
    ///
    /// Called after a daemon transfer completes to propagate `io_error`
    /// bitfield values into the process exit code.
    pub(crate) fn set_io_error_exit_code(&mut self, code: i32) {
        self.io_error_exit_code = Some(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_summary_default_has_empty_events() {
        let summary = ClientSummary::default();
        assert!(summary.events().is_empty());
    }

    #[test]
    fn client_summary_default_has_zero_counts() {
        let summary = ClientSummary::default();
        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_total(), 0);
        assert_eq!(summary.directories_created(), 0);
        assert_eq!(summary.bytes_copied(), 0);
    }

    #[test]
    fn client_summary_into_events_consumes_self() {
        let summary = ClientSummary::default();
        let events = summary.into_events();
        assert!(events.is_empty());
    }

    #[test]
    fn client_summary_regular_files_matched() {
        let summary = ClientSummary::default();
        assert_eq!(summary.regular_files_matched(), 0);
    }

    #[test]
    fn client_summary_regular_files_ignored_existing() {
        let summary = ClientSummary::default();
        assert_eq!(summary.regular_files_ignored_existing(), 0);
    }

    #[test]
    fn client_summary_regular_files_skipped_missing() {
        let summary = ClientSummary::default();
        assert_eq!(summary.regular_files_skipped_missing(), 0);
    }

    #[test]
    fn client_summary_regular_files_skipped_newer() {
        let summary = ClientSummary::default();
        assert_eq!(summary.regular_files_skipped_newer(), 0);
    }

    #[test]
    fn client_summary_directories_total() {
        let summary = ClientSummary::default();
        assert_eq!(summary.directories_total(), 0);
    }

    #[test]
    fn client_summary_symlinks_copied() {
        let summary = ClientSummary::default();
        assert_eq!(summary.symlinks_copied(), 0);
    }

    #[test]
    fn client_summary_symlinks_total() {
        let summary = ClientSummary::default();
        assert_eq!(summary.symlinks_total(), 0);
    }

    #[test]
    fn client_summary_hard_links_created() {
        let summary = ClientSummary::default();
        assert_eq!(summary.hard_links_created(), 0);
    }

    #[test]
    fn client_summary_devices_created() {
        let summary = ClientSummary::default();
        assert_eq!(summary.devices_created(), 0);
    }

    #[test]
    fn client_summary_devices_total() {
        let summary = ClientSummary::default();
        assert_eq!(summary.devices_total(), 0);
    }

    #[test]
    fn client_summary_fifos_created() {
        let summary = ClientSummary::default();
        assert_eq!(summary.fifos_created(), 0);
    }

    #[test]
    fn client_summary_fifos_total() {
        let summary = ClientSummary::default();
        assert_eq!(summary.fifos_total(), 0);
    }

    #[test]
    fn client_summary_items_deleted() {
        let summary = ClientSummary::default();
        assert_eq!(summary.items_deleted(), 0);
    }

    #[test]
    fn client_summary_matched_bytes() {
        let summary = ClientSummary::default();
        assert_eq!(summary.matched_bytes(), 0);
    }

    #[test]
    fn client_summary_bytes_received() {
        let summary = ClientSummary::default();
        assert_eq!(summary.bytes_received(), 0);
    }

    #[test]
    fn client_summary_bytes_sent() {
        let summary = ClientSummary::default();
        assert_eq!(summary.bytes_sent(), 0);
    }

    #[test]
    fn client_summary_transferred_file_size() {
        let summary = ClientSummary::default();
        assert_eq!(summary.transferred_file_size(), 0);
    }

    #[test]
    fn client_summary_compressed_bytes_none_when_not_used() {
        let summary = ClientSummary::default();
        assert!(summary.compressed_bytes().is_none());
    }

    #[test]
    fn client_summary_compression_used() {
        let summary = ClientSummary::default();
        assert!(!summary.compression_used());
    }

    #[test]
    fn client_summary_sources_removed() {
        let summary = ClientSummary::default();
        assert_eq!(summary.sources_removed(), 0);
    }

    #[test]
    fn client_summary_total_source_bytes() {
        let summary = ClientSummary::default();
        assert_eq!(summary.total_source_bytes(), 0);
    }

    #[test]
    fn client_summary_total_elapsed() {
        let summary = ClientSummary::default();
        assert_eq!(summary.total_elapsed(), Duration::ZERO);
    }

    #[test]
    fn client_summary_bandwidth_sleep() {
        let summary = ClientSummary::default();
        assert_eq!(summary.bandwidth_sleep(), Duration::ZERO);
    }

    #[test]
    fn client_summary_file_list_size() {
        let summary = ClientSummary::default();
        assert_eq!(summary.file_list_size(), 0);
    }

    #[test]
    fn client_summary_file_list_generation_time() {
        let summary = ClientSummary::default();
        assert_eq!(summary.file_list_generation_time(), Duration::ZERO);
    }

    #[test]
    fn client_summary_file_list_transfer_time() {
        let summary = ClientSummary::default();
        assert_eq!(summary.file_list_transfer_time(), Duration::ZERO);
    }
}
