//! Transfer rate formatting for summary, verbose, and progress output modes.

use std::time::Duration;

use core::client::HumanReadableMode;

pub(crate) fn format_summary_rate(rate: f64, human_readable: HumanReadableMode) -> String {
    if !human_readable.is_enabled() {
        return format!("{rate:.2}");
    }

    // upstream: main.c:464 formats the bytes/sec rate with human_num, the same
    // do_big_num path as the byte counters, so the rate honours the level's base.
    format_human_rate(rate, human_readable.unit_base())
}

pub(crate) fn format_human_rate(rate: f64, base: f64) -> String {
    if rate < base {
        return format!("{rate:.2}");
    }

    let units = [
        ("P", base.powi(5)),
        ("T", base.powi(4)),
        ("G", base.powi(3)),
        ("M", base.powi(2)),
        ("K", base),
    ];

    for (suffix, threshold) in units {
        if rate >= threshold {
            let value = rate / threshold;
            return format!("{value:.2}{suffix}");
        }
    }

    format!("{rate:.2}")
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
    if !human_readable.is_enabled() {
        return format_progress_rate_decimal(rate);
    }

    format_progress_rate_human(rate)
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

/// Formats a pre-computed transfer rate (bytes/sec) for the progress line.
///
/// Unlike [`format_progress_rate`] which computes the rate from cumulative
/// bytes and elapsed time, this function accepts a rate value directly.
/// This supports the sliding-window rate used in progress2 mode, where
/// the rate comes from the [`RemainingTimeEstimator::window_rate`] method
/// rather than a simple bytes/elapsed division.
///
/// upstream: progress.c:108-116 rprint_progress - rate unit selection
pub(crate) fn format_progress_rate_from_value(
    rate: f64,
    human_readable: HumanReadableMode,
) -> String {
    if rate <= 0.0 {
        return if human_readable.is_enabled() {
            "0.00B/s".to_owned()
        } else {
            "0.00kB/s".to_owned()
        };
    }

    if !human_readable.is_enabled() {
        return format_progress_rate_decimal(rate);
    }

    format_progress_rate_human(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_human_rate_small() {
        assert_eq!(format_human_rate(500.0, 1000.0), "500.00");
    }

    #[test]
    fn format_human_rate_kilo() {
        assert_eq!(format_human_rate(1_500.0, 1000.0), "1.50K");
    }

    #[test]
    fn format_human_rate_mega() {
        assert_eq!(format_human_rate(2_500_000.0, 1000.0), "2.50M");
    }

    #[test]
    fn format_human_rate_base_1024() {
        // -hh divides the rate by 1024, matching upstream human_num.
        assert_eq!(format_human_rate(1_048_576.0, 1024.0), "1.00M");
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

    /// Upstream `rprint_progress` (progress.c:108-116) caps its unit scaling
    /// at `GB/s` and never substitutes a sentinel for the rate field. The
    /// numeric value is printed verbatim with the `%7.2f` format, growing
    /// the field width when the value exceeds 7 chars. We mirror this:
    /// extreme rates stay in `GB/s` units with no placeholder substitution.
    /// upstream: progress.c:108-116 rprint_progress
    #[test]
    fn progress_rate_has_no_overflow_sentinel() {
        let huge = 1_000_000_000_000.0_f64; // 1 TB/s
        let rendered = format_progress_rate_decimal(huge);
        assert!(
            rendered.ends_with("GB/s"),
            "extreme rates must remain in GB/s units: {rendered}"
        );
        assert!(!rendered.contains("??"), "no sentinel for rate: {rendered}");
    }

    /// Verifies the three upstream rate tiers (`kB/s`, `MB/s`, `GB/s`) using
    /// base-1024 scaling. upstream: progress.c:108-116 rprint_progress.
    #[test]
    fn progress_rate_unit_tiers_mirror_upstream() {
        // Below 1024 B/s prints in kB/s with the fractional value.
        let kb = format_progress_rate_decimal(512.0);
        assert!(kb.ends_with("kB/s"), "{kb}");
        // 1 MiB/s prints in MB/s.
        let mb = format_progress_rate_decimal(1024.0 * 1024.0);
        assert!(mb.ends_with("MB/s"), "{mb}");
        // 1 GiB/s prints in GB/s.
        let gb = format_progress_rate_decimal(1024.0 * 1024.0 * 1024.0);
        assert!(gb.ends_with("GB/s"), "{gb}");
    }

    /// `format_progress_rate_from_value` accepts a pre-computed rate instead of
    /// deriving it from bytes/elapsed, supporting the sliding-window rate used
    /// in progress2 mode.
    #[test]
    fn format_progress_rate_from_value_zero() {
        let result = format_progress_rate_from_value(0.0, HumanReadableMode::Disabled);
        assert_eq!(result, "0.00kB/s");
    }

    #[test]
    fn format_progress_rate_from_value_negative() {
        let result = format_progress_rate_from_value(-1.0, HumanReadableMode::Disabled);
        assert_eq!(result, "0.00kB/s");
    }

    #[test]
    fn format_progress_rate_from_value_kb_range() {
        let result = format_progress_rate_from_value(512.0, HumanReadableMode::Disabled);
        assert!(result.ends_with("kB/s"), "expected kB/s: {result}");
    }

    #[test]
    fn format_progress_rate_from_value_mb_range() {
        let result =
            format_progress_rate_from_value(2.0 * 1024.0 * 1024.0, HumanReadableMode::Disabled);
        assert!(result.ends_with("MB/s"), "expected MB/s: {result}");
    }

    #[test]
    fn format_progress_rate_from_value_gb_range() {
        let result = format_progress_rate_from_value(
            2.0 * 1024.0 * 1024.0 * 1024.0,
            HumanReadableMode::Disabled,
        );
        assert!(result.ends_with("GB/s"), "expected GB/s: {result}");
    }
}
