use std::time::Duration;

/// I/O technology the local-copy executor used to materialise a whole-file
/// copy. Tracked so the `Copy method` stats line can report which kernel
/// acceleration ran. Upstream rsync has no equivalent - it always reconstructs
/// files from the wire - so this is oc-rsync-specific and only populated by the
/// local-copy fast paths, never by remote/protocol transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyMethodKind {
    /// macOS `clonefile` copy-on-write clone.
    Clonefile,
    /// Linux `FICLONE` copy-on-write reflink.
    Ficlone,
    /// Linux `copy_file_range` in-kernel copy.
    CopyFileRange,
    /// Windows ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` block clone.
    ReFsReflink,
    /// Windows `CopyFileExW`.
    CopyFileEx,
    /// Linux io_uring registered-buffer data write.
    IoUring,
    /// Portable userspace read/write loop (or delta reconstruction).
    Standard,
}

impl CopyMethodKind {
    /// Every variant, in display order. Indexed by `self as usize`.
    const ALL: [Self; 7] = [
        Self::Clonefile,
        Self::Ficlone,
        Self::CopyFileRange,
        Self::ReFsReflink,
        Self::CopyFileEx,
        Self::IoUring,
        Self::Standard,
    ];

    /// Human-readable label for the `Copy method` stats line. CoW mechanisms
    /// are annotated so the user can see no data was moved.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Clonefile => "clonefile (CoW)",
            Self::Ficlone => "FICLONE (CoW)",
            Self::CopyFileRange => "copy_file_range",
            Self::ReFsReflink => "ReFS reflink (CoW)",
            Self::CopyFileEx => "CopyFileExW",
            Self::IoUring => "io_uring",
            Self::Standard => "standard",
        }
    }

    /// Whether this method is a kernel acceleration (anything but the portable
    /// userspace path). Used to gate the stats line so a plain standard-only
    /// copy stays byte-identical to upstream's `--stats` output.
    #[must_use]
    pub const fn is_accelerated(self) -> bool {
        !matches!(self, Self::Standard)
    }

    /// Maps a `fast_io` platform-copy mechanism onto the tracked kind. The
    /// non-zero-copy `copyfile`/`StandardCopy` results fold into `Standard`.
    #[must_use]
    pub fn from_platform(method: fast_io::CopyMethod) -> Self {
        use fast_io::CopyMethod;
        match method {
            CopyMethod::Clonefile => Self::Clonefile,
            CopyMethod::Ficlone => Self::Ficlone,
            CopyMethod::CopyFileRange => Self::CopyFileRange,
            CopyMethod::ReFsReflink => Self::ReFsReflink,
            CopyMethod::CopyFileEx => Self::CopyFileEx,
            CopyMethod::Copyfile | CopyMethod::StandardCopy => Self::Standard,
        }
    }
}

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
    /// Wall-clock span of the whole transfer (not the sum of per-file copy
    /// durations in `total_elapsed`). Used for the upstream-style transfer rate.
    wall_clock_elapsed: Duration,
    bandwidth_sleep: Duration,
    file_list_size: u64,
    file_list_generation: Duration,
    file_list_transfer: Duration,
    destination_root_created: bool,
    // Per-method copy counts, indexed by `CopyMethodKind as usize`. Populated
    // only by the local-copy fast paths; drives the `Copy method` stats line.
    copy_methods: [u64; 7],
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

    /// Returns the wall-clock span of the whole transfer.
    ///
    /// Unlike [`Self::total_elapsed`] (the sum of per-file copy durations, which
    /// is ~0 for CoW/clonefile), this is a single span suitable for computing a
    /// transfer rate that mirrors upstream `main.c:418` `bytes_per_sec_human_dnum`.
    #[must_use]
    pub const fn wall_clock_elapsed(&self) -> Duration {
        self.wall_clock_elapsed
    }

    /// Records the whole-transfer wall-clock span. Call once at finalize.
    pub(in crate::local_copy) const fn record_wall_clock_elapsed(&mut self, elapsed: Duration) {
        self.wall_clock_elapsed = elapsed;
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

    pub(in crate::local_copy) const fn record_file_list_entry(&mut self, entry_size: usize) {
        self.file_list_size = self.file_list_size.saturating_add(entry_size as u64);
    }

    /// Records that one whole-file copy used the given I/O technology. Called
    /// by the local-copy fast paths alongside [`Self::record_file`].
    pub(in crate::local_copy) fn record_copy_method(&mut self, kind: CopyMethodKind) {
        let index = kind as usize;
        self.copy_methods[index] = self.copy_methods[index].saturating_add(1);
    }

    /// Returns the per-method copy breakdown as `(label, count)` pairs, in
    /// display order, omitting methods that were never used. Empty when no
    /// local-copy fast path ran (e.g. a remote/protocol transfer).
    #[must_use]
    pub fn copy_method_breakdown(&self) -> Vec<(&'static str, u64)> {
        CopyMethodKind::ALL
            .iter()
            .filter_map(|&kind| {
                let count = self.copy_methods[kind as usize];
                (count > 0).then_some((kind.label(), count))
            })
            .collect()
    }

    /// Whether any whole-file copy used a kernel acceleration (clonefile,
    /// reflink, copy_file_range, io_uring, ...). Used to gate the `Copy method`
    /// line so a standard-only copy keeps upstream-identical `--stats` output.
    #[must_use]
    pub fn used_copy_acceleration(&self) -> bool {
        CopyMethodKind::ALL
            .iter()
            .any(|&kind| kind.is_accelerated() && self.copy_methods[kind as usize] > 0)
    }

    /// Folds the file-list size into the `sent` byte total so a local copy
    /// reports the protocol-equivalent figure upstream prints.
    ///
    /// Upstream always runs the transfer protocol over a socketpair, even for a
    /// purely local copy, so its `Total bytes sent` (`total_written`) is
    /// dominated by the file list it serialises (plus any literal data tokens).
    /// The local-copy executor bypasses the wire entirely, so `bytes_sent` would
    /// otherwise report only the literal data - `0` on a no-change run. Folding
    /// the separately tracked file-list size in yields a comparable `sent`
    /// total (and a meaningful speedup) instead of `sent 0 bytes`.
    ///
    /// Resets the enumerated file-list size to zero for a local-copy summary.
    ///
    /// Call exactly once when finalising a local copy. Upstream rsync reports
    /// `File list size: 0` for local transfers, and its `Total bytes sent` is
    /// dominated by the file *data*, not the file list (verified against
    /// rsync 3.4.4: a 1 MiB local copy reports `sent 1,049,017` ~= the file
    /// size, not the path lengths). oc-rsync already counts the literal data in
    /// `bytes_sent` via `record_file`, so it must NOT fold the enumerated path
    /// lengths on top - doing so inflated `sent` ~2x. Zeroing here matches
    /// upstream's `File list size: 0` and leaves `bytes_sent` as the data-only
    /// figure. A local copy transmits nothing over a wire, so the residual gap
    /// versus upstream's socketpair framing bytes is irreducible and not
    /// synthesised.
    ///
    /// upstream: main.c output_summary (`File list size: 0` for local copies)
    pub fn clear_file_list_size(&mut self) {
        self.file_list_size = 0;
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

    /// Returns `true` when the transfer materialised the destination root directory.
    ///
    /// upstream: main.c:798-799 - `rprintf(FINFO, "created directory %s\n", dest_path)`
    /// gated on `INFO_GTE(NAME, 1) || stdout_format_has_i`. The CLI mirrors this
    /// gate to emit the notice ahead of the per-entry itemize lines so the
    /// upstream `testsuite/itemize.test` golden matches.
    #[must_use]
    pub const fn destination_root_created(&self) -> bool {
        self.destination_root_created
    }

    pub(in crate::local_copy) const fn mark_destination_root_created(&mut self) {
        self.destination_root_created = true;
    }

    /// Creates a summary from server-side receiver statistics.
    ///
    /// This constructor is used when the local side acted as the receiver in a pull transfer.
    /// It maps the available receiver statistics (files listed, files transferred, bytes received,
    /// bytes sent, total source bytes) to the corresponding LocalCopySummary fields.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // REASON: maps a wire-stats struct field-by-field
    pub fn from_receiver_stats(
        files_listed: usize,
        files_transferred: usize,
        bytes_received: u64,
        bytes_sent: u64,
        total_source_bytes: u64,
        elapsed: Duration,
        literal_data: u64,
        matched_data: u64,
        items_deleted: u64,
    ) -> Self {
        Self {
            regular_files_total: files_listed as u64,
            files_copied: files_transferred as u64,
            bytes_received,
            bytes_sent,
            bytes_copied: literal_data,
            matched_bytes: matched_data,
            total_source_bytes,
            items_deleted,
            total_elapsed: elapsed,
            wall_clock_elapsed: elapsed,
            ..Default::default()
        }
    }

    /// Creates a summary from server-side generator statistics.
    ///
    /// This constructor is used when the local side acted as the generator/sender in a push transfer.
    /// It maps the generator statistics (files listed/transferred, bytes sent/received, and the
    /// sender-accumulated matched/literal/total-size counters) to the corresponding
    /// LocalCopySummary fields, mirroring [`Self::from_receiver_stats`] for the opposite direction.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // REASON: maps a wire-stats struct field-by-field
    pub fn from_generator_stats(
        files_listed: usize,
        files_transferred: usize,
        bytes_received: u64,
        bytes_sent: u64,
        total_source_bytes: u64,
        elapsed: Duration,
        literal_data: u64,
        matched_data: u64,
        items_deleted: u64,
    ) -> Self {
        Self {
            regular_files_total: files_listed as u64,
            files_copied: files_transferred as u64,
            bytes_received,
            bytes_sent,
            bytes_copied: literal_data,
            matched_bytes: matched_data,
            total_source_bytes,
            items_deleted,
            total_elapsed: elapsed,
            wall_clock_elapsed: elapsed,
            ..Default::default()
        }
    }

    /// Creates a summary from proxy transfer statistics.
    ///
    /// This constructor is used for remote-to-remote transfers where the local side
    /// acts as a relay/proxy. It records the bytes relayed in each direction.
    #[must_use]
    pub const fn from_proxy_stats(bytes_source_to_dest: u64, bytes_dest_to_source: u64) -> Self {
        Self {
            bytes_sent: bytes_source_to_dest,
            bytes_received: bytes_dest_to_source,
            regular_files_total: 0,
            regular_files_matched: 0,
            regular_files_ignored_existing: 0,
            regular_files_skipped_missing: 0,
            regular_files_skipped_newer: 0,
            directories_total: 0,
            symlinks_total: 0,
            devices_total: 0,
            fifos_total: 0,
            files_copied: 0,
            directories_created: 0,
            symlinks_copied: 0,
            hard_links_created: 0,
            devices_created: 0,
            fifos_created: 0,
            items_deleted: 0,
            sources_removed: 0,
            transferred_file_size: 0,
            bytes_copied: 0,
            matched_bytes: 0,
            compressed_bytes: 0,
            compression_used: false,
            total_source_bytes: 0,
            total_elapsed: Duration::ZERO,
            wall_clock_elapsed: Duration::ZERO,
            bandwidth_sleep: Duration::ZERO,
            file_list_size: 0,
            file_list_generation: Duration::ZERO,
            file_list_transfer: Duration::ZERO,
            destination_root_created: false,
            copy_methods: [0; 7],
        }
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
        // A local copy emulates the protocol sender: it writes the file data
        // (counted as sent) but receives no data payload back. Counting the data
        // as received too would halve the reported speedup vs upstream, where a
        // first copy reads back only the generator's small replies (modeled as
        // 0 here). upstream: main.c output_summary - speedup = total_size /
        // (total_written + total_read), and a local sender's total_read is tiny.
        self.bytes_sent = self.bytes_sent.saturating_add(transmitted);
        if let Some(compressed_bytes) = compressed {
            self.compression_used = true;
            self.compressed_bytes = self.compressed_bytes.saturating_add(compressed_bytes);
        }
    }

    pub(in crate::local_copy) const fn record_regular_file_total(&mut self) {
        self.regular_files_total = self.regular_files_total.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_regular_file_matched(&mut self) {
        self.regular_files_matched = self.regular_files_matched.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_regular_file_ignored_existing(&mut self) {
        self.regular_files_ignored_existing = self.regular_files_ignored_existing.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_regular_file_skipped_missing(&mut self) {
        self.regular_files_skipped_missing = self.regular_files_skipped_missing.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_regular_file_skipped_newer(&mut self) {
        self.regular_files_skipped_newer = self.regular_files_skipped_newer.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_total_bytes(&mut self, bytes: u64) {
        self.total_source_bytes = self.total_source_bytes.saturating_add(bytes);
    }

    pub(in crate::local_copy) const fn record_elapsed(&mut self, elapsed: Duration) {
        self.total_elapsed = self.total_elapsed.saturating_add(elapsed);
    }

    pub(in crate::local_copy) const fn record_bandwidth_sleep(&mut self, duration: Duration) {
        self.bandwidth_sleep = self.bandwidth_sleep.saturating_add(duration);
    }

    pub(in crate::local_copy) const fn record_file_list_generation(&mut self, elapsed: Duration) {
        self.file_list_generation = self.file_list_generation.saturating_add(elapsed);
    }

    #[allow(dead_code)] // symmetric with record_file_list_generation
    pub(in crate::local_copy) const fn record_file_list_transfer(&mut self, elapsed: Duration) {
        self.file_list_transfer = self.file_list_transfer.saturating_add(elapsed);
    }

    pub(in crate::local_copy) const fn record_directory(&mut self) {
        self.directories_created = self.directories_created.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_directory_total(&mut self) {
        self.directories_total = self.directories_total.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_symlink(&mut self) {
        self.symlinks_copied = self.symlinks_copied.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_symlink_total(&mut self) {
        self.symlinks_total = self.symlinks_total.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_hard_link(&mut self) {
        self.hard_links_created = self.hard_links_created.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_device(&mut self) {
        self.devices_created = self.devices_created.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_device_total(&mut self) {
        self.devices_total = self.devices_total.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_fifo(&mut self) {
        self.fifos_created = self.fifos_created.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_fifo_total(&mut self) {
        self.fifos_total = self.fifos_total.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_deletion(&mut self) {
        self.items_deleted = self.items_deleted.saturating_add(1);
    }

    pub(in crate::local_copy) const fn record_source_removed(&mut self) {
        self.sources_removed = self.sources_removed.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_summary_has_zero_counts() {
        let summary = LocalCopySummary::default();
        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_total(), 0);
        assert_eq!(summary.directories_created(), 0);
        assert_eq!(summary.symlinks_copied(), 0);
        assert_eq!(summary.bytes_copied(), 0);
        assert_eq!(summary.total_elapsed(), Duration::ZERO);
    }

    #[test]
    fn from_receiver_stats_sets_fields() {
        let summary = LocalCopySummary::from_receiver_stats(
            100,
            50,
            1024,
            256,
            8192,
            Duration::from_secs(5),
            0,
            0,
            0,
        );
        assert_eq!(summary.regular_files_total(), 100);
        assert_eq!(summary.files_copied(), 50);
        assert_eq!(summary.bytes_received(), 1024);
        assert_eq!(summary.bytes_sent(), 256);
        assert_eq!(summary.total_source_bytes(), 8192);
        assert_eq!(summary.total_elapsed(), Duration::from_secs(5));
    }

    #[test]
    fn from_receiver_stats_with_delta_data() {
        let summary = LocalCopySummary::from_receiver_stats(
            10,
            5,
            2048,
            512,
            4096,
            Duration::from_secs(2),
            800,
            1200,
            0,
        );
        assert_eq!(summary.bytes_copied(), 800);
        assert_eq!(summary.matched_bytes(), 1200);
        assert_eq!(summary.files_copied(), 5);
        assert_eq!(summary.bytes_received(), 2048);
    }

    /// Mirrors the daemon-upload + `--delete` path: the daemon receiver
    /// sweeps the destination and sends `NDX_DEL_STATS` back to the client
    /// sender. The client's `LocalCopySummary` must surface the count so
    /// `--stats` renders "Number of deleted files: N" instead of zero.
    #[test]
    fn from_generator_stats_records_items_deleted() {
        let summary = LocalCopySummary::from_generator_stats(
            10,
            5,
            0,
            2048,
            0,
            Duration::from_secs(1),
            0,
            0,
            7,
        );
        assert_eq!(summary.items_deleted(), 7);
    }

    /// Mirrors the daemon-pull + `--delete` path: the local receiver
    /// performs the delete sweep and records the count locally; the
    /// summary must surface the same counter.
    #[test]
    fn from_receiver_stats_records_items_deleted() {
        let summary = LocalCopySummary::from_receiver_stats(
            10,
            5,
            2048,
            512,
            4096,
            Duration::from_secs(2),
            0,
            0,
            3,
        );
        assert_eq!(summary.items_deleted(), 3);
    }

    #[test]
    fn from_generator_stats_sets_fields() {
        let summary = LocalCopySummary::from_generator_stats(
            200,
            75,
            1793,
            2048,
            204_800,
            Duration::from_secs(3),
            2800,
            202_000,
            0,
        );
        assert_eq!(summary.regular_files_total(), 200);
        assert_eq!(summary.files_copied(), 75);
        assert_eq!(summary.bytes_sent(), 2048);
        // #477: the sender-accumulated literal_data must reach the summary
        // (previously dropped, so daemon/ssh-push --stats printed 0).
        assert_eq!(summary.bytes_copied(), 2800);
        assert_eq!(summary.total_elapsed(), Duration::from_secs(3));
    }

    #[test]
    fn record_file_increments_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_file(1000, 800, None);

        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.transferred_file_size(), 1000);
        assert_eq!(summary.bytes_copied(), 800);
        assert_eq!(summary.matched_bytes(), 200);
        assert_eq!(summary.bytes_sent(), 800);
        assert!(!summary.compression_used());
    }

    #[test]
    fn record_file_with_compression() {
        let mut summary = LocalCopySummary::default();
        summary.record_file(1000, 800, Some(400));

        assert_eq!(summary.bytes_copied(), 800);
        assert_eq!(summary.compressed_bytes(), 400);
        assert!(summary.compression_used());
        assert_eq!(summary.bytes_sent(), 400);
    }

    #[test]
    fn record_multiple_files_accumulates() {
        let mut summary = LocalCopySummary::default();
        summary.record_file(100, 80, None);
        summary.record_file(200, 150, None);

        assert_eq!(summary.files_copied(), 2);
        assert_eq!(summary.transferred_file_size(), 300);
        assert_eq!(summary.bytes_copied(), 230);
    }

    #[test]
    fn record_regular_file_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_regular_file_total();
        summary.record_regular_file_total();
        summary.record_regular_file_matched();
        summary.record_regular_file_ignored_existing();
        summary.record_regular_file_skipped_missing();
        summary.record_regular_file_skipped_newer();

        assert_eq!(summary.regular_files_total(), 2);
        assert_eq!(summary.regular_files_matched(), 1);
        assert_eq!(summary.regular_files_ignored_existing(), 1);
        assert_eq!(summary.regular_files_skipped_missing(), 1);
        assert_eq!(summary.regular_files_skipped_newer(), 1);
    }

    #[test]
    fn record_directory_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_directory_total();
        summary.record_directory_total();
        summary.record_directory();

        assert_eq!(summary.directories_total(), 2);
        assert_eq!(summary.directories_created(), 1);
    }

    #[test]
    fn record_symlink_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_symlink_total();
        summary.record_symlink();

        assert_eq!(summary.symlinks_total(), 1);
        assert_eq!(summary.symlinks_copied(), 1);
    }

    #[test]
    fn record_device_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_device_total();
        summary.record_device();

        assert_eq!(summary.devices_total(), 1);
        assert_eq!(summary.devices_created(), 1);
    }

    #[test]
    fn record_fifo_counters() {
        let mut summary = LocalCopySummary::default();
        summary.record_fifo_total();
        summary.record_fifo();

        assert_eq!(summary.fifos_total(), 1);
        assert_eq!(summary.fifos_created(), 1);
    }

    #[test]
    fn record_hard_link() {
        let mut summary = LocalCopySummary::default();
        summary.record_hard_link();
        summary.record_hard_link();

        assert_eq!(summary.hard_links_created(), 2);
    }

    #[test]
    fn record_deletion_and_source_removal() {
        let mut summary = LocalCopySummary::default();
        summary.record_deletion();
        summary.record_deletion();
        summary.record_source_removed();

        assert_eq!(summary.items_deleted(), 2);
        assert_eq!(summary.sources_removed(), 1);
    }

    #[test]
    fn record_elapsed_and_bandwidth_sleep() {
        let mut summary = LocalCopySummary::default();
        summary.record_elapsed(Duration::from_millis(100));
        summary.record_elapsed(Duration::from_millis(50));
        summary.record_bandwidth_sleep(Duration::from_millis(20));

        assert_eq!(summary.total_elapsed(), Duration::from_millis(150));
        assert_eq!(summary.bandwidth_sleep(), Duration::from_millis(20));
    }

    #[test]
    fn record_file_list_stats() {
        let mut summary = LocalCopySummary::default();
        summary.record_file_list_entry(100);
        summary.record_file_list_entry(200);
        summary.record_file_list_generation(Duration::from_millis(50));
        summary.record_file_list_transfer(Duration::from_millis(30));

        assert_eq!(summary.file_list_size(), 300);
        assert_eq!(
            summary.file_list_generation_time(),
            Duration::from_millis(50)
        );
        assert_eq!(summary.file_list_transfer_time(), Duration::from_millis(30));
    }

    #[test]
    fn record_total_bytes() {
        let mut summary = LocalCopySummary::default();
        summary.record_total_bytes(500);
        summary.record_total_bytes(300);

        assert_eq!(summary.total_source_bytes(), 800);
    }

    #[test]
    fn clear_file_list_size_zeroes_flist_and_keeps_sent() {
        // A local copy reports `File list size: 0` (upstream parity) and never
        // folds enumerated path lengths into `sent`. A no-change copy transmits
        // no data, so `bytes_sent` stays 0.
        let mut summary = LocalCopySummary::default();
        summary.record_file_list_entry(93);
        summary.record_file_list_entry(95);
        assert_eq!(summary.bytes_sent(), 0);
        assert_eq!(summary.file_list_size(), 188);

        summary.clear_file_list_size();

        assert_eq!(summary.bytes_sent(), 0);
        assert_eq!(summary.file_list_size(), 0);
    }

    #[test]
    fn copy_method_breakdown_counts_and_gates_acceleration() {
        let mut summary = LocalCopySummary::default();
        assert!(summary.copy_method_breakdown().is_empty());
        assert!(!summary.used_copy_acceleration());

        for _ in 0..400 {
            summary.record_copy_method(CopyMethodKind::Clonefile);
        }
        summary.record_copy_method(CopyMethodKind::Standard);
        summary.record_copy_method(CopyMethodKind::Standard);

        assert_eq!(
            summary.copy_method_breakdown(),
            vec![("clonefile (CoW)", 400), ("standard", 2)]
        );
        // A clone is an acceleration, so the gate trips even though some files
        // fell back to the standard path.
        assert!(summary.used_copy_acceleration());
    }

    #[test]
    fn copy_method_standard_only_does_not_gate_acceleration() {
        // A standard-only local copy must not trip the gate, so its `--stats`
        // output stays byte-identical to upstream (which has no Copy method line).
        let mut summary = LocalCopySummary::default();
        summary.record_copy_method(CopyMethodKind::Standard);
        assert_eq!(summary.copy_method_breakdown(), vec![("standard", 1)]);
        assert!(!summary.used_copy_acceleration());
    }

    #[test]
    fn copy_method_from_platform_maps_zero_copy_and_fallbacks() {
        use fast_io::CopyMethod;
        assert_eq!(
            CopyMethodKind::from_platform(CopyMethod::Clonefile),
            CopyMethodKind::Clonefile
        );
        assert_eq!(
            CopyMethodKind::from_platform(CopyMethod::CopyFileRange),
            CopyMethodKind::CopyFileRange
        );
        // Non-zero-copy platform results fold into the standard bucket.
        assert_eq!(
            CopyMethodKind::from_platform(CopyMethod::StandardCopy),
            CopyMethodKind::Standard
        );
        assert_eq!(
            CopyMethodKind::from_platform(CopyMethod::Copyfile),
            CopyMethodKind::Standard
        );
    }

    #[test]
    fn clear_file_list_size_keeps_literal_data() {
        // Clearing the flist size must leave already-counted literal data in
        // `bytes_sent` untouched - that data-only figure is what upstream's
        // `Total bytes sent` is built on for a local copy.
        let mut summary = LocalCopySummary::default();
        summary.record_file(1_000, 1_000, None);
        summary.record_file_list_entry(40);
        assert_eq!(summary.bytes_sent(), 1_000);

        summary.clear_file_list_size();

        assert_eq!(summary.bytes_sent(), 1_000);
        assert_eq!(summary.file_list_size(), 0);
    }
}
