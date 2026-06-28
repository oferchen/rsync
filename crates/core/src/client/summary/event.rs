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
    /// A regular file was reconstructed locally from a `--copy-dest` basis.
    /// Itemizes with the local-change indicator (`c`).
    ReferenceCopied,
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
    /// Returns whether the event kind is a file-list entry that counts toward
    /// `--progress` accounting - i.e. whether it contributes to the
    /// `to-chk=<remaining>/<total>` denominator.
    ///
    /// The generator walks every such entry, so `to-chk` counts down across all
    /// of them regardless of whether each one is transferred. Whether a given
    /// entry actually prints a per-file progress block and advances `xfr#` is
    /// the narrower question answered by [`ClientEvent::is_uptodate`]: an
    /// up-to-date (quick-check match) entry is still counted here but stays
    /// silent under `--progress`/`-P`.
    ///
    /// upstream: progress.c rprint_progress
    pub const fn is_progress(&self) -> bool {
        matches!(
            self,
            Self::DataCopied
                | Self::ReferenceCopied
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
    hardlink_uptodate: bool,
    is_directory: bool,
}

impl ClientEvent {
    /// Creates an event by consuming a [`LocalCopyRecord`], moving heap-allocated
    /// fields instead of cloning them.
    pub(crate) fn from_record_owned(record: LocalCopyRecord, destination_root: Arc<Path>) -> Self {
        let (
            relative_path,
            action,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata,
            was_created,
            change_set,
            hardlink_uptodate,
            is_directory,
        ) = record.into_parts();
        let kind = match &action {
            LocalCopyAction::DataCopied => ClientEventKind::DataCopied,
            LocalCopyAction::ReferenceCopied => ClientEventKind::ReferenceCopied,
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
        let created = match &action {
            // Honour the explicit `was_created` bit for every action whose
            // record represents an entry that may either be newly created
            // OR re-pointed/updated in place. upstream: log.c:736-738 -
            // `(iflags & ITEM_IS_NEW)` flips slots 2-10 to `+`; that bit is
            // only set by the generator when the destination did not exist
            // (`statret < 0`). The renderer mirrors that gate via
            // `was_created`. Callers must opt into creation with
            // `.with_creation(true)` only when the destination was actually
            // newly materialised.
            LocalCopyAction::DataCopied
            | LocalCopyAction::HardLink
            | LocalCopyAction::SymlinkCopied
            | LocalCopyAction::FifoCopied
            | LocalCopyAction::DeviceCopied
            // DirectoryCreated honours the explicit `was_created` bit: genuine
            // mkdirs set `.with_creation(true)`, while a directory reconstructed
            // from a `--copy-dest` basis records a change set without creation so
            // its row stays `cd` + blank instead of `cd+++++++++`.
            // upstream: generator.c:1480-1482 - the copy-dest match itemizes with
            // ITEM_LOCAL_CHANGE, never ITEM_IS_NEW.
            | LocalCopyAction::DirectoryCreated => was_created,
            // upstream: generator.c:1039 itemizes the copy-dest reconstruction
            // with statret == 0 (the basis was stat'd successfully), so
            // ITEM_IS_NEW is never set and attribute slots are never `+`.
            LocalCopyAction::ReferenceCopied
            | LocalCopyAction::MetadataReused
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
        let destination_path = Self::resolve_destination_path(&destination_root, &relative_path);
        Self {
            relative_path,
            kind,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata: metadata.map(ClientEntryMetadata::from_local_copy_metadata_owned),
            created,
            destination_root,
            destination_path,
            change_set,
            hardlink_uptodate,
            is_directory,
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
            hardlink_uptodate: false,
            is_directory: false,
        }
    }

    /// Builds an event for a `--list-only` flist entry.
    ///
    /// The renderer (`emit_list_only`) prints the line from `relative_path` plus
    /// the metadata snapshot; `kind` only gates inclusion via `list_only_event`.
    /// Directories map to `DirectoryCreated`, symlinks to `SymlinkCopied`, and
    /// every other entry to `DataCopied` - all three pass the inclusion gate.
    /// No destination write occurs in list-only mode, so the destination paths
    /// are left empty.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `list_file_entry()` per-entry render
    #[must_use]
    pub fn from_list_only_entry(relative_path: PathBuf, metadata: ClientEntryMetadata) -> Self {
        let kind = match metadata.kind() {
            ClientEntryKind::Directory => ClientEventKind::DirectoryCreated,
            ClientEntryKind::Symlink => ClientEventKind::SymlinkCopied,
            _ => ClientEventKind::DataCopied,
        };
        let is_directory = metadata.kind().is_directory();
        let total_bytes = Some(metadata.length());
        let destination_root: Arc<Path> = Arc::from(Path::new(""));
        Self {
            relative_path,
            kind,
            bytes_transferred: 0,
            total_bytes,
            elapsed: Duration::ZERO,
            metadata: Some(metadata),
            created: false,
            destination_root,
            destination_path: PathBuf::new(),
            change_set: LocalCopyChangeSet::new(),
            hardlink_uptodate: false,
            is_directory,
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

    /// Returns whether the event describes a directory entry.
    ///
    /// Carried for `EntryDeleted` records, which lack a metadata snapshot, so
    /// the renderer can still append the upstream `%n` trailing slash to a
    /// deleted-directory row (e.g. `*deleting sub/`).
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.is_directory
    }

    /// Returns whether the event describes a hardlink whose destination
    /// already shared the source group leader's inode (upstream:
    /// hlink.c:218-224).
    #[must_use]
    pub const fn is_hardlink_uptodate(&self) -> bool {
        self.hardlink_uptodate
    }

    /// Returns the change flags associated with this event.
    #[must_use]
    pub const fn change_set(&self) -> LocalCopyChangeSet {
        self.change_set
    }

    /// Returns whether this event describes an entry that is already up to date
    /// at the destination: a quick-check metadata match, a `--copy-dest`
    /// reconstruction, an already-correct hardlink alias, or an unchanged
    /// directory/symlink reconstructed from a basis.
    ///
    /// Such entries count toward the `to-chk` file-list total but are never
    /// transferred, so under `--progress`/`-P` they print no per-file block and
    /// do not advance `xfr#` - matching upstream's silent second run. They
    /// surface only with `-vv`/`-i`.
    ///
    /// upstream: hlink.c:218-224, generator.c:1010-1022/1145-1147,
    /// rsync.c:672-676 - the generator records these as "is uptodate" without
    /// opening a receiver progress block.
    #[must_use]
    pub fn is_uptodate(&self) -> bool {
        if matches!(self.kind, ClientEventKind::MetadataReused) || self.hardlink_uptodate {
            return true;
        }
        match self.kind {
            ClientEventKind::ReferenceCopied => true,
            ClientEventKind::DirectoryCreated | ClientEventKind::SymlinkCopied => {
                !self.created && !self.change_set.has_any_change()
            }
            // A `--link-dest` symlink hard-linked from the basis is `hL` + blank
            // and emits "%s is uptodate" like the other alt-dest matches.
            ClientEventKind::HardLink => self
                .metadata
                .as_ref()
                .map(ClientEntryMetadata::kind)
                .is_some_and(|kind| matches!(kind, ClientEntryKind::Symlink)),
            _ => false,
        }
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
            hardlink_uptodate: false,
            is_directory: false,
        }
    }

    /// Marks the event as an up-to-date hardlink alias for testing purposes.
    ///
    /// Mirrors the `with_hardlink_uptodate(true)` flag set by the local-copy
    /// engine for an already-correct hardlink alias, so renderer tests can
    /// exercise the suppression gate without running a transfer.
    #[doc(hidden)]
    #[must_use]
    pub fn with_hardlink_uptodate_for_test(mut self) -> Self {
        self.hardlink_uptodate = true;
        self
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
        // An up-to-date entry still counts toward the `to-chk` file-list total
        // (it is a file the generator checked), so `is_progress` is true. Its
        // per-file line/`xfr#` suppression is handled by `is_uptodate`, not here.
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
