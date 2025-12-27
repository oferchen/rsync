/// Describes an action performed while executing a [`crate::local_copy::LocalCopyPlan`].
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
    /// A new destination entry was not created due to `--existing`.
    SkippedMissingDestination,
    /// An existing destination file was newer than the source and left untouched.
    SkippedNewerDestination,
    /// A non-regular file was skipped because support was disabled.
    SkippedNonRegular,
    /// A directory was skipped because recursion was disabled.
    SkippedDirectory,
    /// A symbolic link was skipped because it was deemed unsafe by `--safe-links`.
    SkippedUnsafeSymlink,
    /// A directory was skipped because it resides on a different filesystem.
    SkippedMountPoint,
    /// An entry was removed due to `--delete`.
    EntryDeleted,
    /// A source entry was removed after a successful transfer.
    SourceRemoved,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_clone() {
        let action = LocalCopyAction::DataCopied;
        let cloned = action.clone();
        assert_eq!(action, cloned);
    }

    #[test]
    fn action_debug() {
        let action = LocalCopyAction::DataCopied;
        let debug = format!("{action:?}");
        assert!(debug.contains("DataCopied"));
    }

    #[test]
    fn action_eq() {
        assert_eq!(LocalCopyAction::DataCopied, LocalCopyAction::DataCopied);
        assert_ne!(LocalCopyAction::DataCopied, LocalCopyAction::MetadataReused);
    }

    #[test]
    fn all_action_variants_are_distinct() {
        let actions = [
            LocalCopyAction::DataCopied,
            LocalCopyAction::MetadataReused,
            LocalCopyAction::HardLink,
            LocalCopyAction::SymlinkCopied,
            LocalCopyAction::FifoCopied,
            LocalCopyAction::DeviceCopied,
            LocalCopyAction::DirectoryCreated,
            LocalCopyAction::SkippedExisting,
            LocalCopyAction::SkippedMissingDestination,
            LocalCopyAction::SkippedNewerDestination,
            LocalCopyAction::SkippedNonRegular,
            LocalCopyAction::SkippedDirectory,
            LocalCopyAction::SkippedUnsafeSymlink,
            LocalCopyAction::SkippedMountPoint,
            LocalCopyAction::EntryDeleted,
            LocalCopyAction::SourceRemoved,
        ];

        // Each action should be equal only to itself
        for (i, a) in actions.iter().enumerate() {
            for (j, b) in actions.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn action_debug_formats_all_variants() {
        let actions = [
            LocalCopyAction::DataCopied,
            LocalCopyAction::MetadataReused,
            LocalCopyAction::HardLink,
            LocalCopyAction::SymlinkCopied,
            LocalCopyAction::FifoCopied,
            LocalCopyAction::DeviceCopied,
            LocalCopyAction::DirectoryCreated,
            LocalCopyAction::SkippedExisting,
            LocalCopyAction::SkippedMissingDestination,
            LocalCopyAction::SkippedNewerDestination,
            LocalCopyAction::SkippedNonRegular,
            LocalCopyAction::SkippedDirectory,
            LocalCopyAction::SkippedUnsafeSymlink,
            LocalCopyAction::SkippedMountPoint,
            LocalCopyAction::EntryDeleted,
            LocalCopyAction::SourceRemoved,
        ];

        for action in &actions {
            let debug = format!("{action:?}");
            assert!(!debug.is_empty());
        }
    }
}
