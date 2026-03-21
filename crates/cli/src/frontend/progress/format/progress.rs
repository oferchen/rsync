//! Progress bar display helpers - percentages, elapsed time, and stat categories.

use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
