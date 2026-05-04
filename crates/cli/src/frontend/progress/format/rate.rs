//! Transfer rate formatting for summary, verbose, and progress output modes.

use std::time::Duration;

use core::client::HumanReadableMode;

/// Holds a primary rate display and an optional secondary (exact) display for combined mode.
pub(crate) struct VerboseRateDisplay {
    pub(crate) primary: (String, &'static str),
    pub(crate) secondary: Option<(String, &'static str)>,
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
        let (value, unit) = format_verbose_rate_decimal(1234.5);
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
}
