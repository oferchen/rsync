//! Client event classification and description helpers.

use core::client::{ClientEvent, ClientEventKind};

use super::super::mode::NameOutputLevel;

/// Returns whether the provided event kind should be reflected in progress output.
pub(crate) const fn is_progress_event(kind: &ClientEventKind) -> bool {
    kind.is_progress()
}

/// Reports whether an event should print its name at the given output level:
/// `UpdatedOnly` (`-v`) lists changed entries, `UpdatedAndUnchanged` (`-vv`)
/// also lists reused ones, and `Disabled` lists none.
pub(crate) const fn event_matches_name_level(event: &ClientEvent, level: NameOutputLevel) -> bool {
    match level {
        NameOutputLevel::Disabled => false,
        NameOutputLevel::UpdatedOnly => matches!(
            event.kind(),
            ClientEventKind::DataCopied
                | ClientEventKind::ReferenceCopied
                | ClientEventKind::HardLink
                | ClientEventKind::SymlinkCopied
                | ClientEventKind::FifoCopied
                | ClientEventKind::DeviceCopied
                | ClientEventKind::DirectoryCreated
                | ClientEventKind::SourceRemoved
        ),
        NameOutputLevel::UpdatedAndUnchanged => matches!(
            event.kind(),
            ClientEventKind::DataCopied
                | ClientEventKind::ReferenceCopied
                | ClientEventKind::MetadataReused
                | ClientEventKind::HardLink
                | ClientEventKind::SymlinkCopied
                | ClientEventKind::FifoCopied
                | ClientEventKind::DeviceCopied
                | ClientEventKind::DirectoryCreated
                | ClientEventKind::SourceRemoved
        ),
    }
}

/// Maps an event kind to a human-readable description.
pub(crate) const fn describe_event_kind(kind: &ClientEventKind) -> &'static str {
    match kind {
        ClientEventKind::DataCopied => "copied",
        ClientEventKind::ReferenceCopied => "copied from reference",
        ClientEventKind::MetadataReused => "metadata reused",
        ClientEventKind::HardLink => "hard link",
        ClientEventKind::SymlinkCopied => "symlink",
        ClientEventKind::FifoCopied => "fifo",
        ClientEventKind::DeviceCopied => "device",
        ClientEventKind::DirectoryCreated => "directory",
        ClientEventKind::SkippedExisting => "skipped existing file",
        ClientEventKind::SkippedMissingDestination => "skipped missing destination",
        ClientEventKind::SkippedNonRegular => "skipped non-regular file",
        ClientEventKind::SkippedDirectory => "skipped directory (no recursion)",
        ClientEventKind::SkippedUnsafeSymlink => "skipped unsafe symlink",
        ClientEventKind::SkippedMountPoint => "skipped mount point",
        ClientEventKind::SkippedNewerDestination => "skipped newer destination file",
        ClientEventKind::SkippedOverMaxSize => "skipped file over max-size",
        ClientEventKind::SkippedUnderMinSize => "skipped file under min-size",
        ClientEventKind::EntryDeleted => "deleted",
        ClientEventKind::SourceRemoved => "source removed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_event_kind_data_copied() {
        assert_eq!(describe_event_kind(&ClientEventKind::DataCopied), "copied");
    }

    #[test]
    fn describe_event_kind_metadata_reused() {
        assert_eq!(
            describe_event_kind(&ClientEventKind::MetadataReused),
            "metadata reused"
        );
    }

    #[test]
    fn describe_event_kind_deleted() {
        assert_eq!(
            describe_event_kind(&ClientEventKind::EntryDeleted),
            "deleted"
        );
    }
}
