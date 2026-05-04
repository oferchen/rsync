//! File-list output formatting - permissions, timestamps, and list-mode event filtering.

use std::time::SystemTime;

use core::client::{ClientEntryKind, ClientEntryMetadata, ClientEventKind};
use time::OffsetDateTime;

use crate::LIST_TIMESTAMP_FORMAT;

pub(crate) const fn list_only_event(kind: &ClientEventKind) -> bool {
    matches!(
        kind,
        ClientEventKind::DataCopied
            | ClientEventKind::MetadataReused
            | ClientEventKind::HardLink
            | ClientEventKind::SymlinkCopied
            | ClientEventKind::FifoCopied
            | ClientEventKind::DeviceCopied
            | ClientEventKind::DirectoryCreated
    )
}

pub(crate) fn format_list_permissions(metadata: &ClientEntryMetadata) -> String {
    let type_char = match metadata.kind() {
        ClientEntryKind::File => '-',
        ClientEntryKind::Directory => 'd',
        ClientEntryKind::Symlink => 'l',
        ClientEntryKind::Fifo => 'p',
        ClientEntryKind::CharDevice => 'c',
        ClientEntryKind::BlockDevice => 'b',
        ClientEntryKind::Socket => 's',
        ClientEntryKind::Other => '?',
    };

    let mut symbols = ['-'; 10];
    symbols[0] = type_char;

    if let Some(mode) = metadata.mode() {
        const PERMISSION_MASKS: [(usize, u32, char); 9] = [
            (1, 0o400, 'r'),
            (2, 0o200, 'w'),
            (3, 0o100, 'x'),
            (4, 0o040, 'r'),
            (5, 0o020, 'w'),
            (6, 0o010, 'x'),
            (7, 0o004, 'r'),
            (8, 0o002, 'w'),
            (9, 0o001, 'x'),
        ];

        for &(index, mask, ch) in &PERMISSION_MASKS {
            if mode & mask != 0 {
                symbols[index] = ch;
            }
        }

        if mode & 0o4000 != 0 {
            symbols[3] = match symbols[3] {
                'x' => 's',
                '-' => 'S',
                other => other,
            };
        }

        if mode & 0o2000 != 0 {
            symbols[6] = match symbols[6] {
                'x' => 's',
                '-' => 'S',
                other => other,
            };
        }

        if mode & 0o1000 != 0 {
            symbols[9] = match symbols[9] {
                'x' => 't',
                '-' => 'T',
                other => other,
            };
        }
    }

    symbols.iter().collect()
}

pub(crate) fn format_list_timestamp(modified: Option<SystemTime>) -> String {
    if let Some(time) = modified
        && let Ok(datetime) = OffsetDateTime::from(time).format(LIST_TIMESTAMP_FORMAT)
    {
        return datetime;
    }
    "1970/01/01 00:00:00".to_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn format_list_timestamp_none_returns_epoch() {
        let result = format_list_timestamp(None);
        assert_eq!(result, "1970/01/01 00:00:00");
    }

    #[test]
    fn format_list_timestamp_epoch_returns_epoch() {
        let result = format_list_timestamp(Some(SystemTime::UNIX_EPOCH));
        assert_eq!(result, "1970/01/01 00:00:00");
    }

    #[test]
    fn format_list_timestamp_has_correct_length() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let result = format_list_timestamp(Some(time));
        assert_eq!(
            result.len(),
            19,
            "timestamp should be 19 chars (YYYY/MM/DD HH:MM:SS): {result:?}"
        );
    }

    #[test]
    fn format_list_timestamp_has_correct_separators() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let result = format_list_timestamp(Some(time));

        assert_eq!(result.as_bytes()[4], b'/', "first separator is /");
        assert_eq!(result.as_bytes()[7], b'/', "second separator is /");
        assert_eq!(result.as_bytes()[10], b' ', "date/time separator is space");
        assert_eq!(result.as_bytes()[13], b':', "hour/minute separator is :");
        assert_eq!(result.as_bytes()[16], b':', "minute/second separator is :");
    }

    #[test]
    fn format_list_timestamp_year_month_day_are_digits() {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let result = format_list_timestamp(Some(time));

        assert!(
            result[0..4].chars().all(|c| c.is_ascii_digit()),
            "year should be digits"
        );
        assert!(
            result[5..7].chars().all(|c| c.is_ascii_digit()),
            "month should be digits"
        );
        assert!(
            result[8..10].chars().all(|c| c.is_ascii_digit()),
            "day should be digits"
        );
        assert!(
            result[11..13].chars().all(|c| c.is_ascii_digit()),
            "hours should be digits"
        );
        assert!(
            result[14..16].chars().all(|c| c.is_ascii_digit()),
            "minutes should be digits"
        );
        assert!(
            result[17..19].chars().all(|c| c.is_ascii_digit()),
            "seconds should be digits"
        );
    }

    #[test]
    fn list_only_event_includes_data_copied() {
        assert!(list_only_event(&ClientEventKind::DataCopied));
    }

    #[test]
    fn list_only_event_includes_metadata_reused() {
        assert!(list_only_event(&ClientEventKind::MetadataReused));
    }

    #[test]
    fn list_only_event_includes_directory_created() {
        assert!(list_only_event(&ClientEventKind::DirectoryCreated));
    }

    #[test]
    fn list_only_event_includes_symlink_copied() {
        assert!(list_only_event(&ClientEventKind::SymlinkCopied));
    }

    #[test]
    fn list_only_event_includes_hard_link() {
        assert!(list_only_event(&ClientEventKind::HardLink));
    }

    #[test]
    fn list_only_event_includes_fifo_copied() {
        assert!(list_only_event(&ClientEventKind::FifoCopied));
    }

    #[test]
    fn list_only_event_includes_device_copied() {
        assert!(list_only_event(&ClientEventKind::DeviceCopied));
    }

    #[test]
    fn list_only_event_excludes_skipped_kinds() {
        assert!(!list_only_event(&ClientEventKind::SkippedExisting));
        assert!(!list_only_event(&ClientEventKind::SkippedNonRegular));
        assert!(!list_only_event(&ClientEventKind::SkippedDirectory));
        assert!(!list_only_event(&ClientEventKind::SkippedUnsafeSymlink));
        assert!(!list_only_event(&ClientEventKind::SkippedMountPoint));
        assert!(!list_only_event(&ClientEventKind::SkippedNewerDestination));
        assert!(!list_only_event(
            &ClientEventKind::SkippedMissingDestination
        ));
    }

    #[test]
    fn list_only_event_excludes_deleted() {
        assert!(!list_only_event(&ClientEventKind::EntryDeleted));
    }

    #[test]
    fn list_only_event_excludes_source_removed() {
        assert!(!list_only_event(&ClientEventKind::SourceRemoved));
    }
}
