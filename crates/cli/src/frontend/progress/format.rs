use std::time::{Duration, SystemTime};

use core::client::{
    ClientEntryKind, ClientEntryMetadata, ClientEvent, ClientEventKind, HumanReadableMode,
};
use time::OffsetDateTime;

use super::mode::NameOutputLevel;
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

pub(crate) fn format_list_size(size: u64, human_readable: HumanReadableMode) -> String {
    let value = format_size(size, human_readable);
    format!("{value:>15}")
}

/// Returns whether the provided event kind should be reflected in progress output.
pub(crate) const fn is_progress_event(kind: &ClientEventKind) -> bool {
    kind.is_progress()
}

/// Formats a byte count using thousands separators when human-readable formatting is disabled. When
/// enabled, the output uses decimal unit suffixes such as `K`, `M`, or `G` with two fractional
/// digits. Combined mode includes the exact decimal value in parentheses when the two representations
/// differ.
pub(crate) fn format_progress_bytes(bytes: u64, human_readable: HumanReadableMode) -> String {
    format_size(bytes, human_readable)
}

pub(crate) fn format_size(bytes: u64, human_readable: HumanReadableMode) -> String {
    let decimal = format_decimal_bytes(bytes);
    if !human_readable.is_enabled() {
        return decimal;
    }

    let human = format_human_bytes(bytes);
    if human_readable.includes_exact() && human != decimal {
        format!("{human} ({decimal})")
    } else {
        human
    }
}

pub(crate) fn format_decimal_bytes(bytes: u64) -> String {
    let mut digits = bytes.to_string();
    let mut groups = Vec::new();

    while digits.len() > 3 {
        let chunk = digits.split_off(digits.len() - 3);
        groups.push(chunk);
    }

    groups.push(digits);
    groups.reverse();
    groups.join(",")
}

pub(crate) fn format_human_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        return bytes.to_string();
    }

    const UNITS: &[(&str, f64)] = &[
        ("P", 1_000_000_000_000_000.0),
        ("T", 1_000_000_000_000.0),
        ("G", 1_000_000_000.0),
        ("M", 1_000_000.0),
        ("K", 1_000.0),
    ];

    let bytes_f64 = bytes as f64;
    for (suffix, threshold) in UNITS {
        if bytes_f64 >= *threshold {
            let value = bytes_f64 / *threshold;
            return format!("{value:.2}{suffix}");
        }
    }

    bytes.to_string()
}

pub(crate) fn format_summary_rate(rate: f64, human_readable: HumanReadableMode) -> String {
    let decimal = format!("{rate:.2}");
    if !human_readable.is_enabled() {
        return decimal;
    }

    let human = format_human_rate(rate);
    if human_readable.includes_exact() && human != decimal {
        format!("{human} ({decimal})")
    } else {
        human
    }
}

pub(crate) fn format_human_rate(rate: f64) -> String {
    if rate < 1_000.0 {
        return format!("{rate:.2}");
    }

    const UNITS: &[(&str, f64)] = &[
        ("P", 1_000_000_000_000_000.0),
        ("T", 1_000_000_000_000.0),
        ("G", 1_000_000_000.0),
        ("M", 1_000_000.0),
        ("K", 1_000.0),
    ];

    for (suffix, threshold) in UNITS {
        if rate >= *threshold {
            let value = rate / *threshold;
            return format!("{value:.2}{suffix}");
        }
    }

    format!("{rate:.2}")
}

pub(crate) struct VerboseRateDisplay {
    pub(crate) primary: (String, &'static str),
    pub(crate) secondary: Option<(String, &'static str)>,
}

pub(crate) fn format_verbose_rate(
    rate: f64,
    human_readable: HumanReadableMode,
) -> VerboseRateDisplay {
    let decimal = format_verbose_rate_decimal(rate);
    if !human_readable.is_enabled() {
        return VerboseRateDisplay {
            primary: decimal,
            secondary: None,
        };
    }

    let human = format_verbose_rate_human(rate);
    if human_readable.includes_exact() && (human.0 != decimal.0 || human.1 != decimal.1) {
        VerboseRateDisplay {
            primary: human,
            secondary: Some(decimal),
        }
    } else {
        VerboseRateDisplay {
            primary: human,
            secondary: None,
        }
    }
}

pub(crate) fn format_verbose_rate_decimal(rate: f64) -> (String, &'static str) {
    (format!("{rate:.1}"), "B/s")
}

pub(crate) fn format_verbose_rate_human(rate: f64) -> (String, &'static str) {
    const UNITS: &[(&str, f64)] = &[
        ("PB/s", 1_000_000_000_000_000.0),
        ("TB/s", 1_000_000_000_000.0),
        ("GB/s", 1_000_000_000.0),
        ("MB/s", 1_000_000.0),
        ("kB/s", 1_000.0),
    ];

    for (unit, threshold) in UNITS {
        if rate >= *threshold {
            let value = rate / *threshold;
            return (format!("{value:.2}"), *unit);
        }
    }

    (format!("{rate:.2}"), "B/s")
}

/// Formats a progress percentage, producing the upstream `??%` placeholder when totals are
/// unavailable.
pub(crate) fn format_progress_percent(bytes: u64, total: Option<u64>) -> String {
    match total {
        Some(total_bytes) if total_bytes > 0 => {
            let capped = bytes.min(total_bytes);
            let percent = (capped.saturating_mul(100)) / total_bytes;
            format!("{percent}%")
        }
        Some(_) => "100%".to_owned(),
        None => "??%".to_owned(),
    }
}

/// Formats a transfer rate in the `kB/s`, `MB/s`, or `GB/s` ranges.
pub(crate) fn format_progress_rate(
    bytes: u64,
    elapsed: Duration,
    human_readable: HumanReadableMode,
) -> String {
    if bytes == 0 || elapsed.is_zero() {
        return if human_readable.is_enabled() {
            "0.00B/s".to_owned()
        } else {
            "0.00kB/s".to_owned()
        };
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return if human_readable.is_enabled() {
            "0.00B/s".to_owned()
        } else {
            "0.00kB/s".to_owned()
        };
    }

    let rate = bytes as f64 / seconds;
    let decimal = format_progress_rate_decimal(rate);
    if !human_readable.is_enabled() {
        return decimal;
    }

    let human = format_progress_rate_human(rate);
    if human_readable.includes_exact() && human != decimal {
        format!("{human} ({decimal})")
    } else {
        human
    }
}

pub(crate) fn format_progress_rate_decimal(rate: f64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    if rate >= GIB {
        format!("{:.2}GB/s", rate / GIB)
    } else if rate >= MIB {
        format!("{:.2}MB/s", rate / MIB)
    } else {
        format!("{:.2}kB/s", rate / KIB)
    }
}

pub(crate) fn format_progress_rate_human(rate: f64) -> String {
    let display = format_verbose_rate_human(rate);
    format!("{}{}", display.0, display.1)
}

/// Formats an elapsed duration as `H:MM:SS`, matching rsync's progress output.
pub(crate) fn format_progress_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours}:{minutes:02}:{seconds:02}")
}

pub(crate) fn format_stat_categories(categories: &[(&str, u64)]) -> String {
    let parts: Vec<String> = categories
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| format!("{label}: {count}"))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

pub(crate) const fn event_matches_name_level(event: &ClientEvent, level: NameOutputLevel) -> bool {
    match level {
        NameOutputLevel::Disabled => false,
        NameOutputLevel::UpdatedOnly => matches!(
            event.kind(),
            ClientEventKind::DataCopied
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
        ClientEventKind::EntryDeleted => "deleted",
        ClientEventKind::SourceRemoved => "source removed",
    }
}

/// Computes the throughput in bytes per second for the provided measurements.
pub(crate) fn compute_rate(bytes: u64, elapsed: Duration) -> Option<f64> {
    if elapsed.is_zero() {
        return None;
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        None
    } else {
        Some(bytes as f64 / seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_decimal_bytes_small() {
        assert_eq!(format_decimal_bytes(0), "0");
        assert_eq!(format_decimal_bytes(999), "999");
    }

    #[test]
    fn format_decimal_bytes_thousands() {
        assert_eq!(format_decimal_bytes(1_000), "1,000");
        assert_eq!(format_decimal_bytes(12_345), "12,345");
    }

    #[test]
    fn format_decimal_bytes_millions() {
        assert_eq!(format_decimal_bytes(1_000_000), "1,000,000");
        assert_eq!(format_decimal_bytes(123_456_789), "123,456,789");
    }

    #[test]
    fn format_human_bytes_small() {
        assert_eq!(format_human_bytes(0), "0");
        assert_eq!(format_human_bytes(999), "999");
    }

    #[test]
    fn format_human_bytes_kilo() {
        assert_eq!(format_human_bytes(1_000), "1.00K");
        assert_eq!(format_human_bytes(1_500), "1.50K");
    }

    #[test]
    fn format_human_bytes_mega() {
        assert_eq!(format_human_bytes(1_000_000), "1.00M");
        assert_eq!(format_human_bytes(2_500_000), "2.50M");
    }

    #[test]
    fn format_human_bytes_giga() {
        assert_eq!(format_human_bytes(1_000_000_000), "1.00G");
    }

    #[test]
    fn format_human_bytes_tera() {
        assert_eq!(format_human_bytes(1_000_000_000_000), "1.00T");
    }

    #[test]
    fn format_progress_percent_with_total() {
        assert_eq!(format_progress_percent(50, Some(100)), "50%");
        assert_eq!(format_progress_percent(100, Some(100)), "100%");
        assert_eq!(format_progress_percent(0, Some(100)), "0%");
    }

    #[test]
    fn format_progress_percent_zero_total() {
        assert_eq!(format_progress_percent(0, Some(0)), "100%");
    }

    #[test]
    fn format_progress_percent_no_total() {
        assert_eq!(format_progress_percent(50, None), "??%");
    }

    #[test]
    fn format_progress_percent_capped_to_total() {
        assert_eq!(format_progress_percent(150, Some(100)), "100%");
    }

    #[test]
    fn format_progress_elapsed_zero() {
        assert_eq!(format_progress_elapsed(Duration::ZERO), "0:00:00");
    }

    #[test]
    fn format_progress_elapsed_seconds() {
        assert_eq!(format_progress_elapsed(Duration::from_secs(45)), "0:00:45");
    }

    #[test]
    fn format_progress_elapsed_minutes() {
        assert_eq!(format_progress_elapsed(Duration::from_secs(125)), "0:02:05");
    }

    #[test]
    fn format_progress_elapsed_hours() {
        assert_eq!(
            format_progress_elapsed(Duration::from_secs(3661)),
            "1:01:01"
        );
    }

    #[test]
    fn format_stat_categories_empty() {
        let categories: &[(&str, u64)] = &[];
        assert_eq!(format_stat_categories(categories), "");
    }

    #[test]
    fn format_stat_categories_all_zero() {
        let categories: &[(&str, u64)] = &[("files", 0), ("dirs", 0)];
        assert_eq!(format_stat_categories(categories), "");
    }

    #[test]
    fn format_stat_categories_some_nonzero() {
        let categories: &[(&str, u64)] = &[("files", 5), ("dirs", 0), ("symlinks", 3)];
        assert_eq!(
            format_stat_categories(categories),
            " (files: 5, symlinks: 3)"
        );
    }

    #[test]
    fn compute_rate_zero_elapsed() {
        assert_eq!(compute_rate(1000, Duration::ZERO), None);
    }

    #[test]
    fn compute_rate_valid() {
        let rate = compute_rate(1000, Duration::from_secs(1)).unwrap();
        assert!((rate - 1000.0).abs() < 0.001);
    }

    #[test]
    fn compute_rate_fractional() {
        let rate = compute_rate(500, Duration::from_millis(500)).unwrap();
        assert!((rate - 1000.0).abs() < 0.001);
    }

    #[test]
    fn format_human_rate_small() {
        assert_eq!(format_human_rate(500.0), "500.00");
    }

    #[test]
    fn format_human_rate_kilo() {
        assert_eq!(format_human_rate(1_500.0), "1.50K");
    }

    #[test]
    fn format_human_rate_mega() {
        assert_eq!(format_human_rate(2_500_000.0), "2.50M");
    }

    #[test]
    fn test_format_verbose_rate_decimal() {
        let (value, unit) = super::format_verbose_rate_decimal(1234.5);
        assert_eq!(value, "1234.5");
        assert_eq!(unit, "B/s");
    }

    #[test]
    fn format_verbose_rate_human_small() {
        let (value, unit) = format_verbose_rate_human(500.0);
        assert_eq!(value, "500.00");
        assert_eq!(unit, "B/s");
    }

    #[test]
    fn format_verbose_rate_human_kilo() {
        let (value, unit) = format_verbose_rate_human(1_500.0);
        assert_eq!(value, "1.50");
        assert_eq!(unit, "kB/s");
    }

    #[test]
    fn format_verbose_rate_human_mega() {
        let (value, unit) = format_verbose_rate_human(2_500_000.0);
        assert_eq!(value, "2.50");
        assert_eq!(unit, "MB/s");
    }

    #[test]
    fn format_list_size_pads_to_15() {
        let result = format_list_size(123, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 15);
        assert!(result.trim_start().starts_with("123"));
    }

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
