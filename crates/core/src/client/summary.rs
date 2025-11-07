use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use oc_rsync_engine::local_copy::{
    LocalCopyAction, LocalCopyChangeSet, LocalCopyFileKind, LocalCopyMetadata, LocalCopyRecord,
    LocalCopyReport, LocalCopySummary,
};

/// Summary of the work performed by a client transfer.
#[derive(Clone, Debug, Default)]
pub struct ClientSummary {
    stats: LocalCopySummary,
    events: Vec<ClientEvent>,
}

impl ClientSummary {
    pub(crate) fn from_report(report: LocalCopyReport) -> Self {
        let stats = *report.summary();
        let destination_root = Arc::new(report.destination_root().to_path_buf());
        let events = report
            .records()
            .iter()
            .map(|record| ClientEvent::from_record(record, Arc::clone(&destination_root)))
            .collect();
        Self { stats, events }
    }

    pub(crate) fn from_summary(summary: LocalCopySummary) -> Self {
        Self {
            stats: summary,
            events: Vec::new(),
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
    pub fn files_copied(&self) -> u64 {
        self.stats.files_copied()
    }

    /// Returns the number of regular files encountered in the source set.
    #[must_use]
    pub fn regular_files_total(&self) -> u64 {
        self.stats.regular_files_total()
    }

    /// Returns the number of regular files that were already up-to-date.
    #[must_use]
    pub fn regular_files_matched(&self) -> u64 {
        self.stats.regular_files_matched()
    }

    /// Returns the number of regular files skipped due to `--ignore-existing`.
    #[must_use]
    pub fn regular_files_ignored_existing(&self) -> u64 {
        self.stats.regular_files_ignored_existing()
    }

    /// Returns the number of regular files skipped because the destination was absent and `--existing` was requested.
    #[must_use]
    #[doc(alias = "--existing")]
    pub fn regular_files_skipped_missing(&self) -> u64 {
        self.stats.regular_files_skipped_missing()
    }

    /// Returns the number of regular files skipped because the destination was newer.
    #[must_use]
    pub fn regular_files_skipped_newer(&self) -> u64 {
        self.stats.regular_files_skipped_newer()
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub fn directories_created(&self) -> u64 {
        self.stats.directories_created()
    }

    /// Returns the number of directories encountered in the source set.
    #[must_use]
    pub fn directories_total(&self) -> u64 {
        self.stats.directories_total()
    }

    /// Returns the number of symbolic links copied during the transfer.
    #[must_use]
    pub fn symlinks_copied(&self) -> u64 {
        self.stats.symlinks_copied()
    }

    /// Returns the number of symbolic links encountered in the source set.
    #[must_use]
    pub fn symlinks_total(&self) -> u64 {
        self.stats.symlinks_total()
    }

    /// Returns the number of hard links materialised during the transfer.
    #[must_use]
    pub fn hard_links_created(&self) -> u64 {
        self.stats.hard_links_created()
    }

    /// Returns the number of device nodes created during the transfer.
    #[must_use]
    pub fn devices_created(&self) -> u64 {
        self.stats.devices_created()
    }

    /// Returns the number of device nodes encountered in the source set.
    #[must_use]
    pub fn devices_total(&self) -> u64 {
        self.stats.devices_total()
    }

    /// Returns the number of FIFOs created during the transfer.
    #[must_use]
    pub fn fifos_created(&self) -> u64 {
        self.stats.fifos_created()
    }

    /// Returns the number of FIFOs encountered in the source set.
    #[must_use]
    pub fn fifos_total(&self) -> u64 {
        self.stats.fifos_total()
    }

    /// Returns the number of extraneous entries removed due to `--delete`.
    #[must_use]
    pub fn items_deleted(&self) -> u64 {
        self.stats.items_deleted()
    }

    /// Returns the aggregate number of bytes copied.
    #[must_use]
    pub fn bytes_copied(&self) -> u64 {
        self.stats.bytes_copied()
    }

    /// Returns the aggregate number of bytes reused from the destination instead of being
    /// rewritten during the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub fn matched_bytes(&self) -> u64 {
        self.stats.matched_bytes()
    }

    /// Returns the aggregate number of bytes received during the transfer.
    #[must_use]
    pub fn bytes_received(&self) -> u64 {
        self.stats.bytes_received()
    }

    /// Returns the aggregate number of bytes sent during the transfer.
    #[must_use]
    pub fn bytes_sent(&self) -> u64 {
        self.stats.bytes_sent()
    }

    /// Returns the aggregate size of files that were rewritten or created.
    #[must_use]
    pub fn transferred_file_size(&self) -> u64 {
        self.stats.transferred_file_size()
    }

    /// Returns the number of bytes that would be sent after applying compression.
    #[must_use]
    pub fn compressed_bytes(&self) -> Option<u64> {
        if self.stats.compression_used() {
            Some(self.stats.compressed_bytes())
        } else {
            None
        }
    }

    /// Reports whether compression participated in the transfer.
    #[must_use]
    pub fn compression_used(&self) -> bool {
        self.stats.compression_used()
    }

    /// Returns the number of source entries removed due to `--remove-source-files`.
    #[must_use]
    pub fn sources_removed(&self) -> u64 {
        self.stats.sources_removed()
    }

    /// Returns the aggregate size of all source files considered during the transfer.
    #[must_use]
    pub fn total_source_bytes(&self) -> u64 {
        self.stats.total_source_bytes()
    }

    /// Returns the total elapsed time spent transferring file payloads.
    #[must_use]
    pub fn total_elapsed(&self) -> Duration {
        self.stats.total_elapsed()
    }

    /// Returns the cumulative duration spent sleeping due to bandwidth throttling.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub fn bandwidth_sleep(&self) -> Duration {
        self.stats.bandwidth_sleep()
    }

    /// Returns the number of bytes that would be transmitted for the file list.
    #[must_use]
    pub fn file_list_size(&self) -> u64 {
        self.stats.file_list_size()
    }

    /// Returns the duration spent generating the in-memory file list.
    #[must_use]
    pub fn file_list_generation_time(&self) -> Duration {
        self.stats.file_list_generation_time()
    }

    /// Returns the duration spent transmitting the file list to the peer.
    #[must_use]
    pub fn file_list_transfer_time(&self) -> Duration {
        self.stats.file_list_transfer_time()
    }
}

/// Describes a transfer action performed by the local copy engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientEventKind {
    /// File data was copied into place.
    DataCopied,
    /// The destination already matched the source and metadata was reused.
    MetadataReused,
    /// A hard link was created to a previously copied destination file.
    HardLink,
    /// A symbolic link was recreated.
    SymlinkCopied,
    /// A FIFO node was recreated.
    FifoCopied,
    /// A device node was recreated.
    DeviceCopied,
    /// A directory was created during traversal.
    DirectoryCreated,
    /// An existing destination file was left untouched due to `--ignore-existing`.
    SkippedExisting,
    /// A destination entry was not created because it was absent and `--existing` was enabled.
    SkippedMissingDestination,
    /// An existing destination file was left untouched because it is newer.
    SkippedNewerDestination,
    /// A non-regular entry was skipped because support was disabled.
    SkippedNonRegular,
    /// A directory was skipped because recursion was disabled.
    SkippedDirectory,
    /// A symbolic link was skipped because it was deemed unsafe.
    SkippedUnsafeSymlink,
    /// A directory was skipped to honour `--one-file-system`.
    SkippedMountPoint,
    /// An entry was deleted due to `--delete`.
    EntryDeleted,
    /// A source entry was removed after a successful transfer.
    SourceRemoved,
}

impl ClientEventKind {
    /// Returns whether the event kind represents progress-worthy activity.
    pub const fn is_progress(&self) -> bool {
        matches!(
            self,
            Self::DataCopied
                | Self::MetadataReused
                | Self::HardLink
                | Self::SymlinkCopied
                | Self::FifoCopied
                | Self::DeviceCopied
        )
    }
}

/// Event describing a single action performed during a client transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientEvent {
    relative_path: PathBuf,
    kind: ClientEventKind,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
    metadata: Option<ClientEntryMetadata>,
    created: bool,
    destination_root: Arc<PathBuf>,
    destination_path: PathBuf,
    change_set: LocalCopyChangeSet,
}

impl ClientEvent {
    pub(crate) fn from_record(record: &LocalCopyRecord, destination_root: Arc<PathBuf>) -> Self {
        let kind = match record.action() {
            LocalCopyAction::DataCopied => ClientEventKind::DataCopied,
            LocalCopyAction::MetadataReused => ClientEventKind::MetadataReused,
            LocalCopyAction::HardLink => ClientEventKind::HardLink,
            LocalCopyAction::SymlinkCopied => ClientEventKind::SymlinkCopied,
            LocalCopyAction::FifoCopied => ClientEventKind::FifoCopied,
            LocalCopyAction::DeviceCopied => ClientEventKind::DeviceCopied,
            LocalCopyAction::DirectoryCreated => ClientEventKind::DirectoryCreated,
            LocalCopyAction::SkippedExisting => ClientEventKind::SkippedExisting,
            LocalCopyAction::SkippedMissingDestination => {
                ClientEventKind::SkippedMissingDestination
            }
            LocalCopyAction::SkippedNewerDestination => ClientEventKind::SkippedNewerDestination,
            LocalCopyAction::SkippedNonRegular => ClientEventKind::SkippedNonRegular,
            LocalCopyAction::SkippedDirectory => ClientEventKind::SkippedDirectory,
            LocalCopyAction::SkippedUnsafeSymlink => ClientEventKind::SkippedUnsafeSymlink,
            LocalCopyAction::SkippedMountPoint => ClientEventKind::SkippedMountPoint,
            LocalCopyAction::EntryDeleted => ClientEventKind::EntryDeleted,
            LocalCopyAction::SourceRemoved => ClientEventKind::SourceRemoved,
        };
        let created = match record.action() {
            LocalCopyAction::DataCopied => record.was_created(),
            LocalCopyAction::DirectoryCreated
            | LocalCopyAction::SymlinkCopied
            | LocalCopyAction::FifoCopied
            | LocalCopyAction::DeviceCopied
            | LocalCopyAction::HardLink => true,
            LocalCopyAction::MetadataReused
            | LocalCopyAction::SkippedExisting
            | LocalCopyAction::SkippedMissingDestination
            | LocalCopyAction::SkippedNewerDestination
            | LocalCopyAction::SkippedNonRegular
            | LocalCopyAction::SkippedDirectory
            | LocalCopyAction::SkippedUnsafeSymlink
            | LocalCopyAction::SkippedMountPoint
            | LocalCopyAction::EntryDeleted
            | LocalCopyAction::SourceRemoved => false,
        };
        let destination_path =
            Self::resolve_destination_path(&destination_root, record.relative_path());
        Self {
            relative_path: record.relative_path().to_path_buf(),
            kind,
            bytes_transferred: record.bytes_transferred(),
            total_bytes: record.total_bytes(),
            elapsed: record.elapsed(),
            metadata: record
                .metadata()
                .map(ClientEntryMetadata::from_local_copy_metadata),
            created,
            destination_root,
            destination_path,
            change_set: record.change_set(),
        }
    }

    pub(crate) fn from_progress(
        relative: &Path,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
        destination_root: Arc<PathBuf>,
    ) -> Self {
        let destination_path = Self::resolve_destination_path(&destination_root, relative);
        Self {
            relative_path: relative.to_path_buf(),
            kind: ClientEventKind::DataCopied,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata: None,
            created: false,
            destination_root,
            destination_path,
            change_set: LocalCopyChangeSet::new(),
        }
    }

    /// Returns the relative path affected by this event.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the action recorded by this event.
    #[must_use]
    pub fn kind(&self) -> &ClientEventKind {
        &self.kind
    }

    /// Returns the number of bytes transferred as part of this event.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this event, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this event.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata associated with the event, when available.
    #[must_use]
    pub fn metadata(&self) -> Option<&ClientEntryMetadata> {
        self.metadata.as_ref()
    }

    /// Returns whether the event corresponds to the creation of a new destination entry.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// Returns the change flags associated with this event.
    #[must_use]
    pub const fn change_set(&self) -> LocalCopyChangeSet {
        self.change_set
    }

    /// Returns the root directory of the destination tree.
    #[must_use]
    pub fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    /// Returns the absolute destination path associated with this event.
    #[must_use]
    pub fn destination_path(&self) -> PathBuf {
        self.destination_path.clone()
    }

    /// Resolves a destination path for the provided relative component under the supplied root.
    ///
    /// Exposed for testing so unit suites can assert how the summary logic expands candidate
    /// destinations without executing a full transfer.
    #[doc(hidden)]
    pub fn resolve_destination_path(destination_root: &Path, relative: &Path) -> PathBuf {
        let candidate = destination_root.join(relative);
        if candidate.exists() {
            return candidate;
        }

        if destination_root
            .file_name()
            .is_some_and(|file_name| relative == Path::new(file_name))
        {
            return destination_root.to_path_buf();
        }

        candidate
    }
}

/// Kind of entry described by [`ClientEntryMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientEntryKind {
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
    /// Entry of an unknown or platform-specific type.
    Other,
}

impl ClientEntryKind {
    /// Returns whether the metadata describes a directory entry.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Returns whether the metadata describes a symbolic link entry.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// Metadata snapshot describing an entry affected by a client event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEntryMetadata {
    kind: ClientEntryKind,
    length: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl ClientEntryMetadata {
    pub(crate) fn from_local_copy_metadata(metadata: &LocalCopyMetadata) -> Self {
        Self {
            kind: match metadata.kind() {
                LocalCopyFileKind::File => ClientEntryKind::File,
                LocalCopyFileKind::Directory => ClientEntryKind::Directory,
                LocalCopyFileKind::Symlink => ClientEntryKind::Symlink,
                LocalCopyFileKind::Fifo => ClientEntryKind::Fifo,
                LocalCopyFileKind::CharDevice => ClientEntryKind::CharDevice,
                LocalCopyFileKind::BlockDevice => ClientEntryKind::BlockDevice,
                LocalCopyFileKind::Socket => ClientEntryKind::Socket,
                LocalCopyFileKind::Other => ClientEntryKind::Other,
            },
            length: metadata.len(),
            modified: metadata.modified(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            nlink: metadata.nlink(),
            symlink_target: metadata.symlink_target().map(Path::to_path_buf),
        }
    }

    /// Returns the kind of entry represented by this metadata snapshot.
    #[must_use]
    pub const fn kind(&self) -> ClientEntryKind {
        self.kind
    }

    /// Returns the logical length of the entry in bytes.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the recorded modification timestamp, when available.
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

    /// Returns the recorded link count when available.
    #[must_use]
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the entry represents a symlink.
    #[must_use]
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}
