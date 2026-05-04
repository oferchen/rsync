#![deny(unsafe_code)]

//! Numeric formatting, width/alignment, and humanized unit helpers.

use std::fmt::Write as FmtWrite;

use crate::frontend::out_format::tokens::{
    HumanizeMode, MAX_PLACEHOLDER_WIDTH, PlaceholderAlignment, PlaceholderFormat,
};

/// Formats a numeric value according to the humanize mode in the placeholder format.
pub(super) fn format_numeric_value(value: i64, format: &PlaceholderFormat) -> String {
    match format.humanize() {
        HumanizeMode::None => value.to_string(),
        HumanizeMode::Separator => format_with_separator(value),
        HumanizeMode::DecimalUnits => {
            format_with_units(value, 1000).unwrap_or_else(|| format_with_separator(value))
        }
        HumanizeMode::BinaryUnits => {
            format_with_units(value, 1024).unwrap_or_else(|| format_with_separator(value))
        }
    }
}

/// Formats a value with SI or binary unit suffixes (K, M, G, T, P).
///
/// Returns `None` when the absolute value is below the base threshold.
fn format_with_units(value: i64, base: i64) -> Option<String> {
    if value.abs() < base {
        return None;
    }

    let mut magnitude = value as f64 / base as f64;
    let negative = magnitude.is_sign_negative();
    if negative {
        magnitude = -magnitude;
    }

    const UNITS: [char; 5] = ['K', 'M', 'G', 'T', 'P'];
    let mut units = 'P';
    for (index, candidate) in UNITS.iter().enumerate() {
        units = *candidate;
        if magnitude < base as f64 || index == UNITS.len() - 1 {
            break;
        }
        magnitude /= base as f64;
    }

    if negative {
        magnitude = -magnitude;
    }

    Some(format!("{magnitude:.2}{units}"))
}

/// Formats a number with comma separators between groups of three digits.
fn format_with_separator(value: i64) -> String {
    let separator = ',';
    let mut magnitude = if value < 0 {
        -(value as i128)
    } else {
        value as i128
    };

    if magnitude == 0 {
        return "0".to_owned();
    }

    let mut groups = Vec::new();
    while magnitude > 0 {
        groups.push((magnitude % 1000) as i16);
        magnitude /= 1000;
    }

    let mut rendered = String::new();
    if value < 0 {
        rendered.push('-');
    }

    if let Some(last) = groups.pop() {
        rendered.push_str(&last.to_string());
    }

    for group in groups.iter().rev() {
        rendered.push(separator);
        // write! to String is infallible
        let _ = write!(rendered, "{group:03}");
    }

    rendered
}

/// Applies width and alignment formatting to a rendered placeholder value.
pub(super) fn apply_placeholder_format(mut value: String, format: &PlaceholderFormat) -> String {
    if let Some(width) = format.width() {
        let capped_width = width.min(MAX_PLACEHOLDER_WIDTH);
        let len = value.chars().count();
        if len < capped_width {
            let padding = " ".repeat(capped_width - len);
            if format.align() == PlaceholderAlignment::Left {
                value.push_str(&padding);
            } else {
                value.insert_str(0, &padding);
            }
        }
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_with_separator_zero() {
        assert_eq!(format_with_separator(0), "0");
    }

    #[test]
    fn format_with_separator_small() {
        assert_eq!(format_with_separator(1), "1");
        assert_eq!(format_with_separator(999), "999");
    }

    #[test]
    fn format_with_separator_thousands() {
        assert_eq!(format_with_separator(1000), "1,000");
        assert_eq!(format_with_separator(1234), "1,234");
        assert_eq!(format_with_separator(999999), "999,999");
    }

    #[test]
    fn format_with_separator_millions() {
        assert_eq!(format_with_separator(1_000_000), "1,000,000");
        assert_eq!(format_with_separator(1_234_567), "1,234,567");
    }

    #[test]
    fn format_with_separator_billions() {
        assert_eq!(format_with_separator(1_000_000_000), "1,000,000,000");
    }

    #[test]
    fn format_with_separator_negative() {
        assert_eq!(format_with_separator(-1), "-1");
        assert_eq!(format_with_separator(-999), "-999");
        assert_eq!(format_with_separator(-1000), "-1,000");
        assert_eq!(format_with_separator(-1_234_567), "-1,234,567");
    }

    #[test]
    fn format_with_units_below_base() {
        assert_eq!(format_with_units(999, 1000), None);
        assert_eq!(format_with_units(1023, 1024), None);
    }

    #[test]
    fn format_with_units_decimal_kilo() {
        assert_eq!(format_with_units(1000, 1000), Some("1.00K".to_owned()));
        assert_eq!(format_with_units(1500, 1000), Some("1.50K".to_owned()));
        assert_eq!(
            format_with_units(999_999, 1000),
            Some("1000.00K".to_owned())
        );
    }

    #[test]
    fn format_with_units_binary_kilo() {
        assert_eq!(format_with_units(1024, 1024), Some("1.00K".to_owned()));
        assert_eq!(format_with_units(1536, 1024), Some("1.50K".to_owned()));
    }

    #[test]
    fn format_with_units_decimal_mega() {
        assert_eq!(format_with_units(1_000_000, 1000), Some("1.00M".to_owned()));
        assert_eq!(format_with_units(2_500_000, 1000), Some("2.50M".to_owned()));
    }

    #[test]
    fn format_with_units_binary_mega() {
        assert_eq!(format_with_units(1_048_576, 1024), Some("1.00M".to_owned()));
    }

    #[test]
    fn format_with_units_giga() {
        assert_eq!(
            format_with_units(1_000_000_000, 1000),
            Some("1.00G".to_owned())
        );
        assert_eq!(
            format_with_units(1_073_741_824, 1024),
            Some("1.00G".to_owned())
        );
    }

    #[test]
    fn format_with_units_tera() {
        assert_eq!(
            format_with_units(1_000_000_000_000, 1000),
            Some("1.00T".to_owned())
        );
    }

    #[test]
    fn format_with_units_negative() {
        assert_eq!(format_with_units(-1000, 1000), Some("-1.00K".to_owned()));
        assert_eq!(
            format_with_units(-1_000_000, 1000),
            Some("-1.00M".to_owned())
        );
    }

    #[test]
    fn apply_placeholder_format_no_width() {
        let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_right_align() {
        let format =
            PlaceholderFormat::new(Some(10), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(
            apply_placeholder_format("test".to_owned(), &format),
            "      test"
        );
    }

    #[test]
    fn apply_placeholder_format_left_align() {
        let format =
            PlaceholderFormat::new(Some(10), PlaceholderAlignment::Left, HumanizeMode::None);
        assert_eq!(
            apply_placeholder_format("test".to_owned(), &format),
            "test      "
        );
    }

    #[test]
    fn apply_placeholder_format_exact_width() {
        let format =
            PlaceholderFormat::new(Some(4), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_exceed_width() {
        let format =
            PlaceholderFormat::new(Some(2), PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(apply_placeholder_format("test".to_owned(), &format), "test");
    }

    #[test]
    fn apply_placeholder_format_max_width_capped() {
        // Width is capped to MAX_PLACEHOLDER_WIDTH
        let format = PlaceholderFormat::new(
            Some(MAX_PLACEHOLDER_WIDTH + 100),
            PlaceholderAlignment::Right,
            HumanizeMode::None,
        );
        let result = apply_placeholder_format("x".to_owned(), &format);
        assert_eq!(result.len(), MAX_PLACEHOLDER_WIDTH);
    }

    #[test]
    fn format_numeric_value_plain() {
        let format = PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::None);
        assert_eq!(format_numeric_value(12345, &format), "12345");
    }

    #[test]
    fn format_numeric_value_with_separator() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::Separator);
        assert_eq!(format_numeric_value(1234567, &format), "1,234,567");
    }

    #[test]
    fn format_numeric_value_decimal_units() {
        let format = PlaceholderFormat::new(
            None,
            PlaceholderAlignment::Right,
            HumanizeMode::DecimalUnits,
        );
        assert_eq!(format_numeric_value(1000, &format), "1.00K");
        assert_eq!(format_numeric_value(999, &format), "999");
    }

    #[test]
    fn format_numeric_value_binary_units() {
        let format =
            PlaceholderFormat::new(None, PlaceholderAlignment::Right, HumanizeMode::BinaryUnits);
        assert_eq!(format_numeric_value(1024, &format), "1.00K");
        assert_eq!(format_numeric_value(1023, &format), "1,023");
    }
}
