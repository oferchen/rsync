use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Statistics describing the outcome of a [`crate::local_copy::LocalCopyPlan`] execution.
///
/// The summary mirrors the high-level counters printed by upstream rsync's
/// `--stats` output: file/metadata operations and the aggregate payload size
/// transferred. Counts increase even in dry-run mode to reflect the actions
/// that would have been taken.
pub struct LocalCopySummary {
    regular_files_total: u64,
    regular_files_matched: u64,
    regular_files_ignored_existing: u64,
    regular_files_skipped_missing: u64,
    regular_files_skipped_newer: u64,
    directories_total: u64,
    symlinks_total: u64,
    devices_total: u64,
    fifos_total: u64,
    files_copied: u64,
    directories_created: u64,
    symlinks_copied: u64,
    hard_links_created: u64,
    devices_created: u64,
    fifos_created: u64,
    items_deleted: u64,
    sources_removed: u64,
    transferred_file_size: u64,
    bytes_copied: u64,
    matched_bytes: u64,
    bytes_sent: u64,
    bytes_received: u64,
    compressed_bytes: u64,
    compression_used: bool,
    total_source_bytes: u64,
    total_elapsed: Duration,
    bandwidth_sleep: Duration,
    file_list_size: u64,
    file_list_generation: Duration,
    file_list_transfer: Duration,
}

impl LocalCopySummary {
    /// Returns the number of regular files copied or updated.
    #[must_use]
    pub const fn files_copied(&self) -> u64 {
        self.files_copied
    }

    /// Returns the number of regular files encountered during the transfer.
    #[must_use]
    pub const fn regular_files_total(&self) -> u64 {
        self.regular_files_total
    }

    /// Returns the number of regular files that already matched the destination state.
    #[must_use]
    pub const fn regular_files_matched(&self) -> u64 {
        self.regular_files_matched
    }

    /// Returns the number of regular files skipped due to `--ignore-existing`.
    #[must_use]
    pub const fn regular_files_ignored_existing(&self) -> u64 {
        self.regular_files_ignored_existing
    }

    /// Returns the number of regular files skipped because the destination was newer.
    #[must_use]
    pub const fn regular_files_skipped_newer(&self) -> u64 {
        self.regular_files_skipped_newer
    }

    /// Returns the number of regular files skipped because the destination was absent and `--existing` was set.
    #[must_use]
    pub const fn regular_files_skipped_missing(&self) -> u64 {
        self.regular_files_skipped_missing
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub const fn directories_created(&self) -> u64 {
        self.directories_created
    }

    /// Returns the number of directories encountered in the source set.
    #[must_use]
    pub const fn directories_total(&self) -> u64 {
        self.directories_total
    }

    /// Returns the number of symbolic links copied.
    #[must_use]
    pub const fn symlinks_copied(&self) -> u64 {
        self.symlinks_copied
    }

    /// Returns the number of symbolic links encountered in the source set.
    #[must_use]
    pub const fn symlinks_total(&self) -> u64 {
        self.symlinks_total
    }

    /// Returns the number of hard links materialised.
    #[must_use]
    pub const fn hard_links_created(&self) -> u64 {
        self.hard_links_created
    }

    /// Returns the number of device nodes created.
    #[must_use]
    pub const fn devices_created(&self) -> u64 {
        self.devices_created
    }

    /// Returns the number of device nodes encountered in the source set.
    #[must_use]
    pub const fn devices_total(&self) -> u64 {
        self.devices_total
    }

    /// Returns the number of FIFOs created.
    #[must_use]
    pub const fn fifos_created(&self) -> u64 {
        self.fifos_created
    }

    /// Returns the number of FIFOs encountered in the source set.
    #[must_use]
    pub const fn fifos_total(&self) -> u64 {
        self.fifos_total
    }

    /// Returns the number of entries removed because of `--delete`.
    #[must_use]
    pub const fn items_deleted(&self) -> u64 {
        self.items_deleted
    }

    /// Returns the number of source entries removed due to `--remove-source-files`.
    #[must_use]
    pub const fn sources_removed(&self) -> u64 {
        self.sources_removed
    }

    /// Returns the aggregate number of literal bytes written for copied files.
    #[must_use]
    pub const fn bytes_copied(&self) -> u64 {
        self.bytes_copied
    }

    /// Returns the aggregate number of bytes that were reused from existing
    /// destination data instead of being rewritten.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.matched_bytes
    }

    /// Returns the aggregate number of bytes that were sent to the peer.
    #[must_use]
    pub const fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    /// Returns the aggregate number of bytes received during the transfer.
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Returns the aggregate size of files that were rewritten or created.
    #[must_use]
    pub const fn transferred_file_size(&self) -> u64 {
        self.transferred_file_size
    }

    /// Returns the aggregate number of compressed bytes that would be sent when compression is enabled.
    #[must_use]
    pub const fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    /// Reports whether compression was applied during the transfer.
    #[must_use]
    pub const fn compression_used(&self) -> bool {
        self.compression_used
    }

    /// Returns the aggregate size of all source files considered during the transfer.
    #[must_use]
    pub const fn total_source_bytes(&self) -> u64 {
        self.total_source_bytes
    }

    /// Returns the total elapsed time spent copying file payloads.
    #[must_use]
    pub const fn total_elapsed(&self) -> Duration {
        self.total_elapsed
    }

    /// Returns the cumulative duration spent sleeping due to `--bwlimit` pacing.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_sleep(&self) -> Duration {
        self.bandwidth_sleep
    }

    /// Returns the number of bytes that would be transmitted for the file list.
    #[must_use]
    pub const fn file_list_size(&self) -> u64 {
        self.file_list_size
    }

    pub(in crate::local_copy) fn record_file_list_entry(&mut self, entry_size: usize) {
        self.file_list_size = self.file_list_size.saturating_add(entry_size as u64);
    }

    /// Returns the time spent enumerating the file list.
    #[must_use]
    pub const fn file_list_generation_time(&self) -> Duration {
        self.file_list_generation
    }

    /// Returns the time spent sending the file list to a peer.
    #[must_use]
    pub const fn file_list_transfer_time(&self) -> Duration {
        self.file_list_transfer
    }

    pub(in crate::local_copy) fn record_file(
        &mut self,
        file_size: u64,
        literal_bytes: u64,
        compressed: Option<u64>,
    ) {
        self.files_copied = self.files_copied.saturating_add(1);
        self.transferred_file_size = self.transferred_file_size.saturating_add(file_size);
        self.bytes_copied = self.bytes_copied.saturating_add(literal_bytes);
        let matched = file_size.saturating_sub(literal_bytes);
        self.matched_bytes = self.matched_bytes.saturating_add(matched);
        let transmitted = compressed.unwrap_or(literal_bytes);
        self.bytes_sent = self.bytes_sent.saturating_add(transmitted);
        self.bytes_received = self.bytes_received.saturating_add(transmitted);
        if let Some(compressed_bytes) = compressed {
            self.compression_used = true;
            self.compressed_bytes = self.compressed_bytes.saturating_add(compressed_bytes);
        }
    }

    pub(in crate::local_copy) fn record_regular_file_total(&mut self) {
        self.regular_files_total = self.regular_files_total.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_regular_file_matched(&mut self) {
        self.regular_files_matched = self.regular_files_matched.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_regular_file_ignored_existing(&mut self) {
        self.regular_files_ignored_existing = self.regular_files_ignored_existing.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_regular_file_skipped_missing(&mut self) {
        self.regular_files_skipped_missing = self.regular_files_skipped_missing.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_regular_file_skipped_newer(&mut self) {
        self.regular_files_skipped_newer = self.regular_files_skipped_newer.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_total_bytes(&mut self, bytes: u64) {
        self.total_source_bytes = self.total_source_bytes.saturating_add(bytes);
    }

    pub(in crate::local_copy) fn record_elapsed(&mut self, elapsed: Duration) {
        self.total_elapsed = self.total_elapsed.saturating_add(elapsed);
    }

    pub(in crate::local_copy) fn record_bandwidth_sleep(&mut self, duration: Duration) {
        self.bandwidth_sleep = self.bandwidth_sleep.saturating_add(duration);
    }

    pub(in crate::local_copy) fn record_file_list_generation(&mut self, elapsed: Duration) {
        self.file_list_generation = self.file_list_generation.saturating_add(elapsed);
    }

    pub(in crate::local_copy) fn record_file_list_transfer(&mut self, elapsed: Duration) {
        self.file_list_transfer = self.file_list_transfer.saturating_add(elapsed);
    }

    pub(in crate::local_copy) fn record_directory(&mut self) {
        self.directories_created = self.directories_created.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_directory_total(&mut self) {
        self.directories_total = self.directories_total.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_symlink(&mut self) {
        self.symlinks_copied = self.symlinks_copied.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_symlink_total(&mut self) {
        self.symlinks_total = self.symlinks_total.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_hard_link(&mut self) {
        self.hard_links_created = self.hard_links_created.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_device(&mut self) {
        self.devices_created = self.devices_created.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_device_total(&mut self) {
        self.devices_total = self.devices_total.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_fifo(&mut self) {
        self.fifos_created = self.fifos_created.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_fifo_total(&mut self) {
        self.fifos_total = self.fifos_total.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_deletion(&mut self) {
        self.items_deleted = self.items_deleted.saturating_add(1);
    }

    pub(in crate::local_copy) fn record_source_removed(&mut self) {
        self.sources_removed = self.sources_removed.saturating_add(1);
    }
}
