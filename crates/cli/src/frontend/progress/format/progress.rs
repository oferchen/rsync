//! Progress bar display helpers - percentages, elapsed time, and stat categories.

use core::client::HumanReadableMode;
use std::time::Duration;

/// Formats a progress percentage.
///
/// upstream: progress.c:128 rprint_progress computes `pct = ofs == size ? 100 :
/// (int)(100.0 * ofs / size)` and never emits a `??` placeholder for the
/// percent field (unlike the ETA field, which does). oc emits one progress line
/// per file at completion (`ofs == size`), so an unknown total resolves to
/// 100% rather than a sentinel.
pub(crate) fn format_progress_percent(bytes: u64, total: Option<u64>) -> String {
    match total {
        Some(total_bytes) if total_bytes > 0 => {
            let capped = bytes.min(total_bytes);
            let percent = (capped.saturating_mul(100)) / total_bytes;
            format!("{percent}%")
        }
        // total 0 (empty file, ofs == size) or unknown: upstream prints 100%.
        Some(_) | None => "100%".to_owned(),
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

/// Formats a parenthesized breakdown of non-zero stat categories (e.g.
/// ` (reg: 1,500, dir: 1)`), grouping each sub-count per the human-readable
/// level. Returns an empty string when every category is zero.
pub(crate) fn format_stat_categories(
    categories: &[(&str, u64)],
    human_readable: HumanReadableMode,
) -> String {
    // upstream: main.c output_itemized_counts comma_num()s each breakdown
    // sub-count too, e.g. `(reg: 1,500, dir: 1)` at level >= 1, `(reg: 1500,
    // dir: 1)` under --no-h (level 0).
    let parts: Vec<String> = categories
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(label, count)| {
            format!(
                "{label}: {}",
                super::size::format_count(*count, human_readable)
            )
        })
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
        // upstream progress.c:128 never emits `??` for the percent field; an
        // unknown total resolves to the completion value 100%, not a sentinel.
        assert_eq!(format_progress_percent(50, None), "100%");
        assert!(!format_progress_percent(50, None).contains("??"));
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
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::Grouped),
            ""
        );
    }

    #[test]
    fn format_stat_categories_all_zero() {
        let categories: &[(&str, u64)] = &[("files", 0), ("dirs", 0)];
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::Grouped),
            ""
        );
    }

    #[test]
    fn format_stat_categories_some_nonzero() {
        let categories: &[(&str, u64)] = &[("files", 5), ("dirs", 0), ("symlinks", 3)];
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::Grouped),
            " (files: 5, symlinks: 3)"
        );
    }

    #[test]
    fn format_stat_categories_groups_thousands_like_upstream() {
        // upstream comma_num()s each sub-count: `(reg: 1,500, dir: 1)`.
        let categories: &[(&str, u64)] = &[("reg", 1500), ("dir", 1)];
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::Grouped),
            " (reg: 1,500, dir: 1)"
        );
    }

    #[test]
    fn format_stat_categories_raw_level_zero_no_separators() {
        // upstream: --no-h => comma_num = do_big_num(x, 0, NULL) emits raw
        // digits in the breakdown too: `(reg: 1500, dir: 1)`, not `1,500`.
        let categories: &[(&str, u64)] = &[("reg", 1500), ("dir", 1)];
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::Raw),
            " (reg: 1500, dir: 1)"
        );
    }

    #[test]
    fn format_stat_categories_hh_groups_not_humanised() {
        // upstream: -hh keeps counts comma-grouped (comma_num passes
        // human_readable != 0 == 1), never K/M/G units: `(reg: 1,500)`.
        let categories: &[(&str, u64)] = &[("reg", 1500)];
        assert_eq!(
            format_stat_categories(categories, HumanReadableMode::BinaryUnits),
            " (reg: 1,500)"
        );
    }
}
