use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{LocalCopyAction, LocalCopyChangeSet, LocalCopyMetadata, LocalCopyProgress};

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
    change_set: LocalCopyChangeSet,
    hardlink_uptodate: bool,
    is_directory: bool,
}

impl LocalCopyRecord {
    /// Creates a new [`LocalCopyRecord`].
    pub(in crate::local_copy) const fn new(
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
            change_set: LocalCopyChangeSet::new(),
            hardlink_uptodate: false,
            is_directory: false,
        }
    }

    /// Marks whether this record describes a directory entry.
    ///
    /// Carried so the CLI renderer can append the upstream `%n` trailing
    /// slash for directory rows (e.g. `*deleting sub/`) even when no metadata
    /// snapshot is attached, as is the case for `EntryDeleted` records.
    #[must_use]
    pub(in crate::local_copy) const fn with_directory(mut self, is_directory: bool) -> Self {
        self.is_directory = is_directory;
        self
    }

    /// Marks whether the record corresponds to the creation of a new destination entry.
    #[must_use]
    pub(in crate::local_copy) const fn with_creation(mut self, created: bool) -> Self {
        self.created = created;
        self
    }

    /// Marks a `HardLink` record whose destination already shared the source
    /// group leader's inode. Used by the verbose renderer to emit the
    /// upstream `"is uptodate"` notice (upstream: hlink.c:218-224, gated by
    /// `INFO_GTE(NAME, 2) && maybe_ATTRS_REPORT`).
    #[must_use]
    pub(in crate::local_copy) const fn with_hardlink_uptodate(mut self, uptodate: bool) -> Self {
        self.hardlink_uptodate = uptodate;
        self
    }

    /// Returns the relative path affected by this record.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the action performed by this record.
    #[must_use]
    pub const fn action(&self) -> &LocalCopyAction {
        &self.action
    }

    /// Returns the number of bytes transferred for this record.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this record, when known.
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent performing the action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata snapshot associated with this record, when available.
    pub const fn metadata(&self) -> Option<&LocalCopyMetadata> {
        self.metadata.as_ref()
    }

    /// Returns whether the record corresponds to a newly created destination entry.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// Returns whether this record describes a directory entry.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        self.is_directory
    }

    /// Returns whether this record describes a hardlink whose destination
    /// already shared the source group leader's inode (upstream:
    /// hlink.c:218-224).
    #[must_use]
    pub const fn is_hardlink_uptodate(&self) -> bool {
        self.hardlink_uptodate
    }

    /// Returns the change flags associated with this record.
    #[must_use]
    pub const fn change_set(&self) -> LocalCopyChangeSet {
        self.change_set
    }

    /// Associates a change-set with the record.
    #[must_use]
    pub(in crate::local_copy) const fn with_change_set(
        mut self,
        change_set: LocalCopyChangeSet,
    ) -> Self {
        self.change_set = change_set;
        self
    }

    /// Consumes the record and returns its constituent fields.
    ///
    /// Enables callers to move heap-allocated fields (notably `relative_path`
    /// and `metadata`) instead of cloning them.
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        PathBuf,
        LocalCopyAction,
        u64,
        Option<u64>,
        Duration,
        Option<LocalCopyMetadata>,
        bool,
        LocalCopyChangeSet,
        bool,
        bool,
    ) {
        (
            self.relative_path,
            self.action,
            self.bytes_transferred,
            self.total_bytes,
            self.elapsed,
            self.metadata,
            self.created,
            self.change_set,
            self.hardlink_uptodate,
            self.is_directory,
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> LocalCopyRecord {
        LocalCopyRecord::new(
            PathBuf::from("test/file.txt"),
            LocalCopyAction::DataCopied,
            1024,
            Some(2048),
            Duration::from_millis(100),
            None,
        )
    }

    #[test]
    fn record_relative_path_returns_path() {
        let record = test_record();
        assert_eq!(record.relative_path(), Path::new("test/file.txt"));
    }

    #[test]
    fn record_action_returns_action() {
        let record = test_record();
        assert_eq!(record.action(), &LocalCopyAction::DataCopied);
    }

    #[test]
    fn record_bytes_transferred_returns_value() {
        let record = test_record();
        assert_eq!(record.bytes_transferred(), 1024);
    }

    #[test]
    fn record_total_bytes_returns_some_when_set() {
        let record = test_record();
        assert_eq!(record.total_bytes(), Some(2048));
    }

    #[test]
    fn record_total_bytes_returns_none_when_unset() {
        let record = LocalCopyRecord::new(
            PathBuf::from("file"),
            LocalCopyAction::DataCopied,
            0,
            None,
            Duration::ZERO,
            None,
        );
        assert!(record.total_bytes().is_none());
    }

    #[test]
    fn record_elapsed_returns_duration() {
        let record = test_record();
        assert_eq!(record.elapsed(), Duration::from_millis(100));
    }

    #[test]
    fn record_metadata_returns_none_when_unset() {
        let record = test_record();
        assert!(record.metadata().is_none());
    }

    #[test]
    fn record_was_created_default_false() {
        let record = test_record();
        assert!(!record.was_created());
    }

    #[test]
    fn record_with_creation_true() {
        let record = test_record().with_creation(true);
        assert!(record.was_created());
    }

    #[test]
    fn record_with_creation_false() {
        let record = test_record().with_creation(false);
        assert!(!record.was_created());
    }

    #[test]
    fn record_change_set_default_is_empty() {
        let record = test_record();
        let change_set = record.change_set();
        assert_eq!(change_set, LocalCopyChangeSet::new());
    }

    #[test]
    fn record_with_change_set_updates_change_set() {
        let change_set = LocalCopyChangeSet::new().with_checksum_changed(true);
        let record = test_record().with_change_set(change_set);
        assert!(record.change_set().checksum_changed());
    }

    #[test]
    fn record_clone_produces_equal_paths() {
        let record = test_record();
        let cloned = record.clone();
        assert_eq!(cloned.relative_path(), record.relative_path());
        assert_eq!(cloned.action(), record.action());
        assert_eq!(cloned.bytes_transferred(), record.bytes_transferred());
    }

    #[test]
    fn record_debug_contains_path() {
        let record = test_record();
        let debug = format!("{record:?}");
        assert!(debug.contains("test/file.txt"));
    }

    #[test]
    fn record_handler_closure_receives_record() {
        let mut received = None;
        let mut handler = |r: LocalCopyRecord| {
            received = Some(r);
        };

        let record = test_record();
        handler.handle(record);

        assert!(received.is_some());
        let rec = received.unwrap();
        assert_eq!(rec.relative_path(), Path::new("test/file.txt"));
    }

    #[test]
    fn record_with_zero_bytes() {
        let record = LocalCopyRecord::new(
            PathBuf::from("empty"),
            LocalCopyAction::MetadataReused,
            0,
            Some(0),
            Duration::ZERO,
            None,
        );
        assert_eq!(record.bytes_transferred(), 0);
        assert_eq!(record.total_bytes(), Some(0));
    }

    #[test]
    fn record_different_actions() {
        let actions = [
            LocalCopyAction::DataCopied,
            LocalCopyAction::MetadataReused,
            LocalCopyAction::HardLink,
            LocalCopyAction::SymlinkCopied,
            LocalCopyAction::DirectoryCreated,
        ];

        for action in actions {
            let record = LocalCopyRecord::new(
                PathBuf::from("file"),
                action.clone(),
                0,
                None,
                Duration::ZERO,
                None,
            );
            assert_eq!(record.action(), &action);
        }
    }

    #[test]
    fn into_parts_moves_relative_path() {
        let record = test_record().with_creation(true);
        let (
            path,
            action,
            bytes,
            total,
            elapsed,
            meta,
            created,
            change_set,
            hardlink_uptodate,
            is_directory,
        ) = record.into_parts();
        assert_eq!(path, Path::new("test/file.txt"));
        assert_eq!(action, LocalCopyAction::DataCopied);
        assert_eq!(bytes, 1024);
        assert_eq!(total, Some(2048));
        assert_eq!(elapsed, Duration::from_millis(100));
        assert!(meta.is_none());
        assert!(created);
        assert!(!hardlink_uptodate);
        assert!(!is_directory);
        assert_eq!(change_set, LocalCopyChangeSet::new());
    }

    #[test]
    fn record_hardlink_uptodate_default_false() {
        let record = test_record();
        assert!(!record.is_hardlink_uptodate());
    }

    #[test]
    fn record_with_hardlink_uptodate_true() {
        let record = test_record().with_hardlink_uptodate(true);
        assert!(record.is_hardlink_uptodate());
    }
}
