//! Number and rate formatting helpers for rsync statistics output.
//!
//! These helpers produce the comma-grouped integers and two-decimal
//! rate/speedup strings used by the `--stats` and summary output paths, matching
//! upstream rsync's `comma_num`/`comma_dnum` formatting exactly.

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

/// Formats a transfer speed with 2 decimal places.
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_speed;
///
/// assert_eq!(format_speed(0.0), "0.00");
/// assert_eq!(format_speed(1234.56), "1,234.56");
/// assert_eq!(format_speed(1234567.89), "1,234,567.89");
/// ```
#[must_use]
pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 0.0 {
        return "0.00".to_string();
    }

    let rounded = (bytes_per_sec * 100.0).round() / 100.0;
    let integer_part = rounded.floor() as u64;
    let fractional_part = ((rounded - integer_part as f64) * 100.0).round() as u64;

    format!("{}.{:02}", format_number(integer_part), fractional_part)
}

/// Formats a speedup ratio with 2 decimal places.
///
/// # Examples
///
/// ```
/// use cli::stats_format::format_speedup;
///
/// assert_eq!(format_speedup(0.0), "0.00");
/// assert_eq!(format_speedup(15.38), "15.38");
/// assert_eq!(format_speedup(1234.567), "1,234.57");
/// ```
#[must_use]
pub fn format_speedup(speedup: f64) -> String {
    if speedup < 0.0 {
        return "0.00".to_string();
    }

    let rounded = (speedup * 100.0).round() / 100.0;
    let integer_part = rounded.floor() as u64;
    let fractional_part = ((rounded - integer_part as f64) * 100.0).round() as u64;

    format!("{}.{:02}", format_number(integer_part), fractional_part)
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

    #[test]
    fn format_speed_zero() {
        assert_eq!(format_speed(0.0), "0.00");
    }

    #[test]
    fn format_speed_small() {
        assert_eq!(format_speed(1.5), "1.50");
        assert_eq!(format_speed(99.99), "99.99");
    }

    #[test]
    fn format_speed_with_separator() {
        assert_eq!(format_speed(1234.56), "1,234.56");
        assert_eq!(format_speed(26745.00), "26,745.00");
    }

    #[test]
    fn format_speed_large() {
        assert_eq!(format_speed(1234567.89), "1,234,567.89");
    }

    #[test]
    fn format_speed_rounds_correctly() {
        assert_eq!(format_speed(1234.567), "1,234.57");
        assert_eq!(format_speed(1234.564), "1,234.56");
    }

    #[test]
    fn format_speed_negative_becomes_zero() {
        assert_eq!(format_speed(-100.0), "0.00");
    }

    #[test]
    fn format_speedup_zero() {
        assert_eq!(format_speedup(0.0), "0.00");
    }

    #[test]
    fn format_speedup_small() {
        assert_eq!(format_speedup(1.5), "1.50");
        assert_eq!(format_speedup(15.38), "15.38");
    }

    #[test]
    fn format_speedup_large() {
        assert_eq!(format_speedup(1234.56), "1,234.56");
    }

    #[test]
    fn format_speedup_rounds_correctly() {
        assert_eq!(format_speedup(1234.567), "1,234.57");
        assert_eq!(format_speedup(1234.564), "1,234.56");
    }

    #[test]
    fn format_speedup_negative_becomes_zero() {
        assert_eq!(format_speedup(-100.0), "0.00");
    }
}
