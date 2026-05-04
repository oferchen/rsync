//! Byte-count and file-size formatting with optional human-readable suffixes.

use core::client::HumanReadableMode;

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

pub(crate) fn format_list_size(size: u64, human_readable: HumanReadableMode) -> String {
    let value = format_size(size, human_readable);
    format!("{value:>15}")
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
    fn format_list_size_pads_to_15() {
        let result = format_list_size(123, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 15);
        assert!(result.trim_start().starts_with("123"));
    }

    #[test]
    fn format_list_size_zero_pads_correctly() {
        let result = format_list_size(0, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 15);
        assert_eq!(result.trim(), "0");
        // Should be right-aligned: leading spaces then "0"
        assert!(result.ends_with('0'));
    }

    #[test]
    fn format_list_size_large_value_with_separators() {
        let result = format_list_size(1_234_567, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 15);
        assert!(
            result.contains("1,234,567"),
            "large value should have thousands separators: {result:?}"
        );
    }

    #[test]
    fn format_list_size_very_large_value() {
        let result = format_list_size(1_234_567_890_123, HumanReadableMode::Disabled);
        assert!(
            result.contains("1,234,567,890,123"),
            "very large value should be formatted with separators: {result:?}"
        );
    }

    #[test]
    fn format_list_size_human_readable_small() {
        // Values under 1000 should show plain digits
        let result = format_list_size(500, HumanReadableMode::Enabled);
        assert_eq!(result.len(), 15);
        assert_eq!(result.trim(), "500");
    }

    #[test]
    fn format_list_size_human_readable_kilo() {
        let result = format_list_size(1_500, HumanReadableMode::Enabled);
        assert_eq!(result.len(), 15);
        assert!(
            result.contains("1.50K"),
            "1500 bytes should show as 1.50K in human-readable: {result:?}"
        );
    }

    #[test]
    fn format_list_size_human_readable_mega() {
        let result = format_list_size(2_500_000, HumanReadableMode::Enabled);
        assert_eq!(result.len(), 15);
        assert!(
            result.contains("2.50M"),
            "2.5M bytes should show as 2.50M in human-readable: {result:?}"
        );
    }

    #[test]
    fn format_list_size_is_right_aligned() {
        let small = format_list_size(1, HumanReadableMode::Disabled);
        let large = format_list_size(1_000_000, HumanReadableMode::Disabled);

        assert_eq!(small.len(), 15);
        assert_eq!(large.len(), 15);

        let small_spaces = small.len() - small.trim_start().len();
        let large_spaces = large.len() - large.trim_start().len();
        assert!(
            small_spaces > large_spaces,
            "smaller value should have more leading spaces: small={small_spaces}, large={large_spaces}"
        );
    }
}
