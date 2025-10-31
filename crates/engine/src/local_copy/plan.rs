use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(test)]
pub(crate) use super::filter_program::FilterOutcome;

use super::{
    CopyOutcome, DestinationSpec, LocalCopyArgumentError, LocalCopyError, LocalCopyOptions,
    SourceSpec, copy_sources, is_fifo, operand_is_remote,
};

/// Plan describing a local filesystem copy.
///
/// Instances are constructed from CLI-style operands using
/// [`LocalCopyPlan::from_operands`]. Execution copies regular files, directories,
/// and symbolic links while preserving permissions, timestamps, and
/// optional ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCopyPlan {
    pub(super) sources: Vec<SourceSpec>,
    pub(super) destination: DestinationSpec,
}

impl LocalCopyPlan {
    /// Constructs a plan from CLI-style operands.
    ///
    /// The operands must contain at least one source and a destination. A
    /// trailing path separator on a source operand mirrors upstream rsync's
    /// behaviour of copying the directory *contents* rather than the directory
    /// itself. Remote operands such as `host::module`, `host:/path`, or
    /// `rsync://server/module` are rejected with
    /// [`LocalCopyArgumentError::RemoteOperandUnsupported`] so callers receive a
    /// deterministic diagnostic explaining that this build only supports local
    /// filesystem copies.
    ///
    /// # Errors
    ///
    /// Returns [`LocalCopyErrorKind::MissingSourceOperands`] when fewer than two
    /// operands are supplied. Empty operands and invalid destination states are
    /// reported via [`LocalCopyErrorKind::InvalidArgument`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_engine::local_copy::LocalCopyPlan;
    /// use std::ffi::OsString;
    ///
    /// let operands = vec![OsString::from("src"), OsString::from("dst")];
    /// let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
    /// assert_eq!(plan.sources().len(), 1);
    /// assert_eq!(plan.destination(), std::path::Path::new("dst"));
    /// ```
    pub fn from_operands(operands: &[OsString]) -> Result<Self, LocalCopyError> {
        if operands.len() < 2 {
            return Err(LocalCopyError::missing_operands());
        }

        let sources: Vec<SourceSpec> = operands[..operands.len() - 1]
            .iter()
            .map(SourceSpec::from_operand)
            .collect::<Result<_, _>>()?;

        if sources.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptySourceOperand,
            ));
        }

        let destination_operand = &operands[operands.len() - 1];
        if destination_operand.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptyDestinationOperand,
            ));
        }

        if operand_is_remote(destination_operand.as_os_str()) {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::RemoteOperandUnsupported,
            ));
        }

        let destination = DestinationSpec::from_operand(destination_operand);

        Ok(Self {
            sources,
            destination,
        })
    }

    /// Returns the planned source operands.
    #[must_use]
    pub fn sources(&self) -> &[SourceSpec] {
        &self.sources
    }

    /// Returns the planned destination path.
    #[must_use]
    pub fn destination(&self) -> &Path {
        self.destination.path()
    }

    /// Executes the planned copy.
    ///
    /// # Errors
    ///
    /// Reports [`LocalCopyError`] variants when operand validation fails or I/O
    /// operations encounter errors.
    pub fn execute(&self) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
    }

    /// Executes the planned copy using the requested execution mode.
    ///
    /// When [`LocalCopyExecution::DryRun`] is selected the filesystem is left
    /// untouched while operand validation and readability checks still occur.
    pub fn execute_with(
        &self,
        mode: LocalCopyExecution,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(mode, LocalCopyOptions::default())
    }

    /// Executes the planned copy with additional behavioural options.
    pub fn execute_with_options(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options_and_handler(mode, options, None)
    }

    /// Executes the planned copy and returns a detailed report of performed actions.
    pub fn execute_with_report(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        self.execute_with_report_and_handler(mode, options, None)
    }

    /// Executes the planned copy while routing records to the supplied handler.
    pub fn execute_with_options_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(CopyOutcome::into_summary)
    }

    /// Executes the planned copy, returning a detailed report and notifying the handler.
    pub fn execute_with_report_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(|outcome| {
            let (_summary, report) = outcome.into_summary_and_report();
            report
        })
    }
}

/// Describes how a [`LocalCopyPlan`] should be executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyExecution {
    /// Perform the copy and mutate the destination filesystem.
    Apply,
    /// Validate the copy without mutating the destination tree.
    DryRun,
}

impl LocalCopyExecution {
    pub(super) const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}

/// Describes an action performed while executing a [`LocalCopyPlan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCopyAction {
    /// File data was copied into place.
    DataCopied,
    /// An existing destination file already matched the source.
    MetadataReused,
    /// A hard link was created pointing at a previously copied destination.
    HardLink,
    /// A symbolic link was recreated.
    SymlinkCopied,
    /// A FIFO node was recreated.
    FifoCopied,
    /// A character or block device was recreated.
    DeviceCopied,
    /// A directory was created.
    DirectoryCreated,
    /// An existing destination file was left untouched due to `--ignore-existing`.
    SkippedExisting,
    /// An existing destination file was newer than the source and left untouched.
    SkippedNewerDestination,
    /// A non-regular file was skipped because support was disabled.
    SkippedNonRegular,
    /// A symbolic link was skipped because it was deemed unsafe by `--safe-links`.
    SkippedUnsafeSymlink,
    /// A directory was skipped because it resides on a different filesystem.
    SkippedMountPoint,
    /// An entry was removed due to `--delete`.
    EntryDeleted,
    /// A source entry was removed after a successful transfer.
    SourceRemoved,
}

/// Record describing a single filesystem action performed during local copy execution.
#[derive(Clone, Debug)]
pub struct LocalCopyRecord {
    relative_path: PathBuf,
    action: LocalCopyAction,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
    metadata: Option<LocalCopyMetadata>,
    created: bool,
}

impl LocalCopyRecord {
    /// Creates a new [`LocalCopyRecord`].
    pub(super) fn new(
        relative_path: PathBuf,
        action: LocalCopyAction,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
        metadata: Option<LocalCopyMetadata>,
    ) -> Self {
        Self {
            relative_path,
            action,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata,
            created: false,
        }
    }

    /// Marks whether the record corresponds to the creation of a new destination entry.
    #[must_use]
    pub(super) fn with_creation(mut self, created: bool) -> Self {
        self.created = created;
        self
    }

    /// Returns the relative path affected by this record.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the action performed by this record.
    #[must_use]
    pub fn action(&self) -> &LocalCopyAction {
        &self.action
    }

    /// Returns the number of bytes transferred for this record.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this record, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent performing the action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata snapshot associated with this record, when available.
    #[must_use]
    pub fn metadata(&self) -> Option<&LocalCopyMetadata> {
        self.metadata.as_ref()
    }

    /// Returns whether the record corresponds to a newly created destination entry.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }
}

/// File type captured for [`LocalCopyMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyFileKind {
    /// Regular file entry.
    File,
    /// Directory entry.
    Directory,
    /// Symbolic link entry.
    Symlink,
    /// FIFO entry.
    Fifo,
    /// Character device entry.
    CharDevice,
    /// Block device entry.
    BlockDevice,
    /// Unix domain socket entry.
    Socket,
    /// Unknown or platform specific entry.
    Other,
}

impl LocalCopyFileKind {
    fn from_file_type(file_type: &fs::FileType) -> Self {
        if file_type.is_dir() {
            return Self::Directory;
        }
        if file_type.is_symlink() {
            return Self::Symlink;
        }
        if file_type.is_file() {
            return Self::File;
        }
        if is_fifo(file_type) {
            return Self::Fifo;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            if file_type.is_char_device() {
                return Self::CharDevice;
            }
            if file_type.is_block_device() {
                return Self::BlockDevice;
            }
            if file_type.is_socket() {
                return Self::Socket;
            }
        }
        Self::Other
    }

    /// Returns whether the kind represents a directory.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// Metadata snapshot recorded for events emitted by [`LocalCopyRecord`].
#[derive(Clone, Debug)]
pub struct LocalCopyMetadata {
    kind: LocalCopyFileKind,
    len: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl LocalCopyMetadata {
    pub(super) fn from_metadata(metadata: &fs::Metadata, symlink_target: Option<PathBuf>) -> Self {
        let file_type = metadata.file_type();
        let kind = LocalCopyFileKind::from_file_type(&file_type);
        let len = metadata.len();
        let modified = metadata.modified().ok();

        #[cfg(unix)]
        let (mode, uid, gid, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(metadata.mode()),
                Some(metadata.uid()),
                Some(metadata.gid()),
                Some(metadata.nlink()),
            )
        };

        #[cfg(not(unix))]
        let (mode, uid, gid, nlink) = (None, None, None, None);

        let target = if matches!(kind, LocalCopyFileKind::Symlink) {
            symlink_target
        } else {
            None
        };

        Self {
            kind,
            len,
            modified,
            mode,
            uid,
            gid,
            nlink,
            symlink_target: target,
        }
    }

    /// Returns the entry kind associated with the metadata.
    #[must_use]
    pub const fn kind(&self) -> LocalCopyFileKind {
        self.kind
    }

    /// Returns the entry length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns whether the metadata describes an empty entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the recorded modification time, when available.
    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Returns the Unix permission bits when available.
    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the numeric owner identifier when available.
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the numeric group identifier when available.
    #[must_use]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the hard link count when available.
    #[must_use]
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the metadata describes a symlink.
    #[must_use]
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}

/// Snapshot describing in-flight progress for a transfer action.
#[derive(Clone, Copy, Debug)]
pub struct LocalCopyProgress<'a> {
    relative_path: &'a Path,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
}

impl<'a> LocalCopyProgress<'a> {
    /// Creates a new [`LocalCopyProgress`] snapshot.
    #[must_use]
    pub const fn new(
        relative_path: &'a Path,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
    ) -> Self {
        Self {
            relative_path,
            bytes_transferred,
            total_bytes,
            elapsed,
        }
    }

    /// Returns the path associated with the progress snapshot.
    #[must_use]
    pub const fn relative_path(&self) -> &'a Path {
        self.relative_path
    }

    /// Returns the number of bytes transferred so far.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this action, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// Report returned after executing a [`LocalCopyPlan`] with event collection enabled.
#[derive(Clone, Debug, Default)]
pub struct LocalCopyReport {
    summary: LocalCopySummary,
    records: Vec<LocalCopyRecord>,
    destination_root: PathBuf,
}

impl LocalCopyReport {
    pub(super) fn new(
        summary: LocalCopySummary,
        records: Vec<LocalCopyRecord>,
        destination_root: PathBuf,
    ) -> Self {
        Self {
            summary,
            records,
            destination_root,
        }
    }

    /// Returns the high-level summary collected during execution.
    #[must_use]
    pub const fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    /// Consumes the report and returns the aggregated summary.
    #[must_use]
    pub fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    /// Returns the list of records captured during execution.
    #[must_use]
    pub fn records(&self) -> &[LocalCopyRecord] {
        &self.records
    }

    /// Consumes the report and returns the recorded events.
    #[must_use]
    pub fn into_records(self) -> Vec<LocalCopyRecord> {
        self.records
    }

    /// Returns the destination root path used during execution.
    #[must_use]
    pub fn destination_root(&self) -> &Path {
        &self.destination_root
    }
}

/// Observer invoked for each [`LocalCopyRecord`] emitted during execution.
pub trait LocalCopyRecordHandler {
    /// Handles a newly produced [`LocalCopyRecord`].
    fn handle(&mut self, record: LocalCopyRecord);

    /// Handles an in-flight progress update for the current action.
    fn handle_progress(&mut self, _progress: LocalCopyProgress<'_>) {}
}

impl<F> LocalCopyRecordHandler for F
where
    F: FnMut(LocalCopyRecord),
{
    fn handle(&mut self, record: LocalCopyRecord) {
        self(record);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Statistics describing the outcome of a [`LocalCopyPlan`] execution.
///
/// The summary mirrors the high-level counters printed by upstream rsync's
/// `--stats` output: file/metadata operations and the aggregate payload size
/// transferred. Counts increase even in dry-run mode to reflect the actions
/// that would have been taken.
pub struct LocalCopySummary {
    regular_files_total: u64,
    regular_files_matched: u64,
    regular_files_ignored_existing: u64,
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

    pub(super) fn record_file(
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

    pub(super) fn record_regular_file_total(&mut self) {
        self.regular_files_total = self.regular_files_total.saturating_add(1);
    }

    pub(super) fn record_regular_file_matched(&mut self) {
        self.regular_files_matched = self.regular_files_matched.saturating_add(1);
    }

    pub(super) fn record_regular_file_ignored_existing(&mut self) {
        self.regular_files_ignored_existing = self.regular_files_ignored_existing.saturating_add(1);
    }

    pub(super) fn record_regular_file_skipped_newer(&mut self) {
        self.regular_files_skipped_newer = self.regular_files_skipped_newer.saturating_add(1);
    }

    pub(super) fn record_total_bytes(&mut self, bytes: u64) {
        self.total_source_bytes = self.total_source_bytes.saturating_add(bytes);
    }

    pub(super) fn record_elapsed(&mut self, elapsed: Duration) {
        self.total_elapsed = self.total_elapsed.saturating_add(elapsed);
    }

    pub(super) fn record_bandwidth_sleep(&mut self, duration: Duration) {
        self.bandwidth_sleep = self.bandwidth_sleep.saturating_add(duration);
    }

    pub(super) fn record_file_list_generation(&mut self, elapsed: Duration) {
        self.file_list_generation = self.file_list_generation.saturating_add(elapsed);
    }

    pub(super) fn record_file_list_transfer(&mut self, elapsed: Duration) {
        self.file_list_transfer = self.file_list_transfer.saturating_add(elapsed);
    }

    pub(super) fn record_directory(&mut self) {
        self.directories_created = self.directories_created.saturating_add(1);
    }

    pub(super) fn record_directory_total(&mut self) {
        self.directories_total = self.directories_total.saturating_add(1);
    }

    pub(super) fn record_symlink(&mut self) {
        self.symlinks_copied = self.symlinks_copied.saturating_add(1);
    }

    pub(super) fn record_symlink_total(&mut self) {
        self.symlinks_total = self.symlinks_total.saturating_add(1);
    }

    pub(super) fn record_hard_link(&mut self) {
        self.hard_links_created = self.hard_links_created.saturating_add(1);
    }

    pub(super) fn record_device(&mut self) {
        self.devices_created = self.devices_created.saturating_add(1);
    }

    pub(super) fn record_device_total(&mut self) {
        self.devices_total = self.devices_total.saturating_add(1);
    }

    pub(super) fn record_fifo(&mut self) {
        self.fifos_created = self.fifos_created.saturating_add(1);
    }

    pub(super) fn record_fifo_total(&mut self) {
        self.fifos_total = self.fifos_total.saturating_add(1);
    }

    pub(super) fn record_deletion(&mut self) {
        self.items_deleted = self.items_deleted.saturating_add(1);
    }

    pub(super) fn record_source_removed(&mut self) {
        self.sources_removed = self.sources_removed.saturating_add(1);
    }
}
