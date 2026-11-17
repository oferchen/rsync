//! Number and rate formatting helpers for rsync statistics output.
//!
//! These helpers produce the comma-grouped integers and fractional
//! statistics used by the `--stats` and summary output paths, matching
//! upstream rsync's `comma_num`/`comma_dnum` formatting exactly.

use core::client::HumanReadableMode;

/// Formats a number with thousands separators (commas).
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_number;
///
/// assert_eq!(format_number(0), "0");
/// assert_eq!(format_number(999), "999");
/// assert_eq!(format_number(1000), "1,000");
/// assert_eq!(format_number(1234), "1,234");
/// assert_eq!(format_number(1234567), "1,234,567");
/// ```
#[must_use]
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, ch) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
    }

    result
}

/// Formats a fractional statistic (speedup, seconds) as upstream's
/// `comma_dnum()` does: thousands-grouped at every enabled `--human-readable`
/// level and plain digits under `--no-h`, never with a unit suffix.
///
/// upstream: inums.h `comma_dnum()` is `do_big_dnum(dnum, human_readable != 0,
/// decimal_digits)`.
///
/// # Examples
///
/// ```
/// use cli::stats_format::comma_dnum;
/// use core::client::HumanReadableMode;
///
/// assert_eq!(comma_dnum(15.38, 2, HumanReadableMode::Grouped), "15.38");
/// assert_eq!(comma_dnum(26_548.67, 2, HumanReadableMode::BinaryUnits), "26,548.67");
/// assert_eq!(comma_dnum(26_548.67, 2, HumanReadableMode::Raw), "26548.67");
/// ```
#[must_use]
pub fn comma_dnum(value: f64, decimal_digits: usize, human_readable: HumanReadableMode) -> String {
    if human_readable.groups_counts() {
        logging::comma_dnum(value, decimal_digits)
    } else {
        format!("{value:.decimal_digits$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_zero() {
        assert_eq!(format_number(0), "0");
    }

    #[test]
    fn format_number_no_separator() {
        assert_eq!(format_number(1), "1");
        assert_eq!(format_number(99), "99");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_one_separator() {
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234), "1,234");
        assert_eq!(format_number(9999), "9,999");
    }

    #[test]
    fn format_number_two_separators() {
        assert_eq!(format_number(12345), "12,345");
        assert_eq!(format_number(123456), "123,456");
        assert_eq!(format_number(999999), "999,999");
    }

    #[test]
    fn format_number_three_separators() {
        assert_eq!(format_number(1234567), "1,234,567");
        assert_eq!(format_number(9999999), "9,999,999");
    }

    #[test]
    fn format_number_large() {
        assert_eq!(format_number(1234567890), "1,234,567,890");
        assert_eq!(format_number(9_999_999_999), "9,999,999,999");
    }

    /// upstream: inums.h `comma_dnum()` passes `human_readable != 0`, so the
    /// speedup groups at -h and -hh (no units) and only `--no-h` is ungrouped:
    /// 3.5.2 prints `speedup is 26548.67` under `--no-h`.
    #[test]
    fn comma_dnum_groups_at_every_enabled_level_only() {
        for mode in [
            HumanReadableMode::Grouped,
            HumanReadableMode::DecimalUnits,
            HumanReadableMode::BinaryUnits,
        ] {
            assert_eq!(comma_dnum(26_548.672, 2, mode), "26,548.67", "{mode:?}");
        }
        assert_eq!(
            comma_dnum(26_548.672, 2, HumanReadableMode::Raw),
            "26548.67"
        );
        assert_eq!(comma_dnum(1.5, 2, HumanReadableMode::Raw), "1.50");
        assert_eq!(
            comma_dnum(1234.5, 3, HumanReadableMode::Grouped),
            "1,234.500"
        );
    }
}
