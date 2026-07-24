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
