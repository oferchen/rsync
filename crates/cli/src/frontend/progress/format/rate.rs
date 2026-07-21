//! Transfer rate formatting for summary, verbose, and progress output modes.

use std::time::Duration;

use core::client::HumanReadableMode;

/// Formats the bytes/sec field of the transfer summary trailer: raw decimals
/// under `--no-h`, thousands-grouped at the default level, or unit-suffixed
/// under `-h`/`-hh`.
pub(crate) fn format_summary_rate(rate: f64, human_readable: HumanReadableMode) -> String {
    if !human_readable.is_enabled() {
        // upstream: main.c:418 formats the rate with human_dnum(rate, 2), i.e.
        // do_big_dnum(rate, human_readable, 2). do_big_num (lib/compat.c:62)
        // inserts the thousands separator only when human_flag != 0, so level 0
        // (`--no-h`) renders raw ("1509.61") while the default level 1 groups
        // ("1,509.61"). Grouping the rate at level 0 diverges from upstream.
        return if human_readable.uses_separators() {
            crate::stats_format::format_speed(rate)
        } else {
            format!("{rate:.2}")
        };
    }

    // upstream: main.c:418 human_dnum uses the same do_big_num unit path as the
    // byte counters, so the rate honours the level's base under `-h`/`-hh`.
    format_human_rate(rate, human_readable.unit_base())
}

/// Formats a rate with a `K`/`M`/`G`/`T`/`P` suffix and two decimals. `base` is
/// 1000 for `-h` or 1024 for `-hh`; rates below `base` render as plain decimals.
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

/// Formats a transfer rate in the `kB/s`, `MB/s`, or `GB/s` ranges.
///
/// upstream: progress.c:108-116 rprint_progress - the progress rate column is
/// always scaled with base-1024 divisors (kB/s / MB/s / GB/s) and never honours
/// `-h`/`-hh`; the human-readable modes affect only the byte counters, so the
/// mode is intentionally ignored here.
pub(crate) fn format_progress_rate(
    bytes: u64,
    elapsed: Duration,
    _human_readable: HumanReadableMode,
) -> String {
    let seconds = elapsed.as_secs_f64();
    let rate = if bytes == 0 || seconds <= 0.0 {
        0.0
    } else {
        bytes as f64 / seconds
    };
    format_progress_rate_decimal(rate)
}

/// Formats a bytes/sec rate for the progress line into `kB/s`, `MB/s`, or
/// `GB/s`, using base-1024 divisors and capping at the `GB/s` tier.
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
    _human_readable: HumanReadableMode,
) -> String {
    // upstream: progress.c:108-116 - always base-1024, never `-h`/`-hh`.
    format_progress_rate_decimal(rate.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_human_rate_small() {
        assert_eq!(format_human_rate(500.0, 1000.0), "500.00");
    }

    #[test]
    fn summary_rate_groups_thousands_without_human_readable() {
        // upstream output_summary() uses comma_dnum for the bytes/sec field, so
        // the default (non -h) summary trailer must be thousands-grouped, the
        // same as oc-rsync's own --stats path.
        assert_eq!(
            format_summary_rate(1_234_567.89, HumanReadableMode::Grouped),
            "1,234,567.89"
        );
        assert_eq!(
            format_summary_rate(512.0, HumanReadableMode::Grouped),
            "512.00"
        );
    }

    #[test]
    fn summary_rate_raw_under_no_h() {
        // upstream: --no-h sets human_readable = 0, so main.c:418's human_dnum
        // passes human_flag 0 and do_big_num inserts no separator. A rate that
        // crosses 1000 must therefore render raw, not grouped, matching the real
        // binary's "sent X bytes  received Y bytes  1509.61 bytes/sec" trailer.
        assert_eq!(
            format_summary_rate(1_509.61, HumanReadableMode::Raw),
            "1509.61"
        );
        assert_eq!(
            format_summary_rate(1_234_567.89, HumanReadableMode::Raw),
            "1234567.89"
        );
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

    /// Progress rate always uses base-1024 units regardless of `-h`/`-hh`.
    /// upstream: progress.c:108-116 rprint_progress never consults human_num.
    #[test]
    fn progress_rate_ignores_human_readable_mode() {
        // 2 MiB/s must render as `2.00MB/s` in every human-readable mode -
        // never the base-1000 `2.10MB/s` or a `B/s`/`TB/s` unit.
        let bytes = 2 * 1024 * 1024;
        for mode in [
            HumanReadableMode::Grouped,
            HumanReadableMode::DecimalUnits,
            HumanReadableMode::BinaryUnits,
        ] {
            assert_eq!(
                format_progress_rate(bytes, Duration::from_secs(1), mode),
                "2.00MB/s",
                "progress rate must stay base-1024 under {mode:?}"
            );
            assert_eq!(
                format_progress_rate_from_value(bytes as f64, mode),
                "2.00MB/s",
                "from_value progress rate must stay base-1024 under {mode:?}"
            );
        }
    }

    /// A zero rate always renders `0.00kB/s`, never `0.00B/s`.
    /// upstream: progress.c:113-116 - a zero rate falls into the `kB/s` tier.
    #[test]
    fn progress_rate_zero_is_kb_per_sec_in_every_mode() {
        for mode in [
            HumanReadableMode::Grouped,
            HumanReadableMode::DecimalUnits,
            HumanReadableMode::BinaryUnits,
        ] {
            assert_eq!(
                format_progress_rate(0, Duration::from_secs(1), mode),
                "0.00kB/s"
            );
            assert_eq!(format_progress_rate_from_value(0.0, mode), "0.00kB/s");
        }
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
        let result = format_progress_rate_from_value(0.0, HumanReadableMode::Grouped);
        assert_eq!(result, "0.00kB/s");
    }

    #[test]
    fn format_progress_rate_from_value_negative() {
        let result = format_progress_rate_from_value(-1.0, HumanReadableMode::Grouped);
        assert_eq!(result, "0.00kB/s");
    }

    #[test]
    fn format_progress_rate_from_value_kb_range() {
        let result = format_progress_rate_from_value(512.0, HumanReadableMode::Grouped);
        assert!(result.ends_with("kB/s"), "expected kB/s: {result}");
    }

    #[test]
    fn format_progress_rate_from_value_mb_range() {
        let result =
            format_progress_rate_from_value(2.0 * 1024.0 * 1024.0, HumanReadableMode::Grouped);
        assert!(result.ends_with("MB/s"), "expected MB/s: {result}");
    }

    #[test]
    fn format_progress_rate_from_value_gb_range() {
        let result = format_progress_rate_from_value(
            2.0 * 1024.0 * 1024.0 * 1024.0,
            HumanReadableMode::Grouped,
        );
        assert!(result.ends_with("GB/s"), "expected GB/s: {result}");
    }
}
