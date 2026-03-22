//! Client transfer event types and action mapping.
//!
//! Maps engine-level `LocalCopyAction` values to user-facing
//! [`ClientEventKind`] variants. The event kinds correspond to the
//! itemize change indicators emitted by upstream `log.c:log_item()`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use engine::local_copy::{LocalCopyAction, LocalCopyChangeSet, LocalCopyRecord};

use super::metadata::{ClientEntryKind, ClientEntryMetadata};

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
    destination_root: Arc<Path>,
    destination_path: PathBuf,
    change_set: LocalCopyChangeSet,
}

impl ClientEvent {
    pub(crate) fn from_record(record: &LocalCopyRecord, destination_root: Arc<Path>) -> Self {
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
        destination_root: Arc<Path>,
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
    pub const fn kind(&self) -> &ClientEventKind {
        &self.kind
    }

    /// Returns the number of bytes transferred as part of this event.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this event, when known.
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this event.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata associated with the event, when available.
    pub const fn metadata(&self) -> Option<&ClientEntryMetadata> {
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

    /// Constructs a [`ClientEvent`] for testing purposes.
    ///
    /// Exposed so downstream crates can build events for format-rendering tests
    /// without needing to run a full transfer pipeline.
    #[doc(hidden)]
    pub fn for_test(
        relative_path: PathBuf,
        kind: ClientEventKind,
        created: bool,
        metadata: Option<ClientEntryMetadata>,
        change_set: LocalCopyChangeSet,
    ) -> Self {
        let destination_root: Arc<Path> = Arc::from(Path::new("/tmp"));
        let destination_path = destination_root.join(&relative_path);
        Self {
            relative_path,
            kind,
            bytes_transferred: 0,
            total_bytes: None,
            elapsed: Duration::ZERO,
            metadata,
            created,
            destination_root,
            destination_path,
            change_set,
        }
    }

    /// Constructs a [`ClientEntryMetadata`] for testing purposes.
    #[doc(hidden)]
    pub fn test_metadata(kind: ClientEntryKind) -> ClientEntryMetadata {
        ClientEntryMetadata::for_test(kind)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_event_kind_is_progress_returns_true_for_data_copied() {
        assert!(ClientEventKind::DataCopied.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_true_for_metadata_reused() {
        assert!(ClientEventKind::MetadataReused.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_true_for_hard_link() {
        assert!(ClientEventKind::HardLink.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_true_for_symlink_copied() {
        assert!(ClientEventKind::SymlinkCopied.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_true_for_fifo_copied() {
        assert!(ClientEventKind::FifoCopied.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_true_for_device_copied() {
        assert!(ClientEventKind::DeviceCopied.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_false_for_skipped() {
        assert!(!ClientEventKind::SkippedExisting.is_progress());
        assert!(!ClientEventKind::SkippedMissingDestination.is_progress());
        assert!(!ClientEventKind::SkippedNewerDestination.is_progress());
        assert!(!ClientEventKind::SkippedNonRegular.is_progress());
        assert!(!ClientEventKind::SkippedDirectory.is_progress());
    }

    #[test]
    fn client_event_kind_is_progress_returns_false_for_deleted() {
        assert!(!ClientEventKind::EntryDeleted.is_progress());
        assert!(!ClientEventKind::SourceRemoved.is_progress());
    }

    #[test]
    fn resolve_destination_path_joins_components() {
        let root = Path::new("/dest");
        let relative = Path::new("subdir/file.txt");
        let result = ClientEvent::resolve_destination_path(root, relative);
        assert_eq!(result, PathBuf::from("/dest/subdir/file.txt"));
    }

    #[test]
    fn resolve_destination_path_matches_root_when_filename_matches() {
        let root = Path::new("/dest/file.txt");
        let relative = Path::new("file.txt");
        let result = ClientEvent::resolve_destination_path(root, relative);
        assert!(
            result == PathBuf::from("/dest/file.txt")
                || result == PathBuf::from("/dest/file.txt/file.txt")
        );
    }
}
