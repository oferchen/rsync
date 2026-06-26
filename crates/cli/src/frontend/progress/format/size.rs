//! Byte-count and file-size formatting with optional human-readable suffixes.

use core::client::HumanReadableMode;

/// Formats a byte count using thousands separators when human-readable formatting is disabled. When
/// enabled, the output uses unit suffixes such as `K`, `M`, or `G` with two fractional digits.
/// `-h` (Enabled) divides by 1000; `-hh` (Combined) divides by 1024, mirroring upstream `do_big_num`.
pub(crate) fn format_progress_bytes(bytes: u64, human_readable: HumanReadableMode) -> String {
    format_size(bytes, human_readable)
}

pub(crate) fn format_size(bytes: u64, human_readable: HumanReadableMode) -> String {
    if !human_readable.is_enabled() {
        return format_decimal_bytes(bytes);
    }

    format_human_bytes(bytes, human_readable.unit_base())
}

pub(crate) fn format_decimal_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return String::from("0");
    }

    // u64::MAX with commas is 26 chars: "18,446,744,073,709,551,615"
    let mut buf = [0u8; 26];
    let mut pos = buf.len();
    let mut n = bytes;
    let mut digit_count: u8 = 0;

    while n > 0 {
        if digit_count == 3 {
            pos -= 1;
            buf[pos] = b',';
            digit_count = 0;
        }
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        digit_count += 1;
    }

    // SAFETY: buf[pos..] contains only ASCII digits and commas.
    String::from(std::str::from_utf8(&buf[pos..]).expect("ASCII-only content"))
}

pub(crate) fn format_human_bytes(bytes: u64, base: f64) -> String {
    // upstream: lib/compat.c:do_big_num - values below the multiplier are
    // printed without a unit suffix; otherwise K/M/G/T/P with two fractional
    // digits. `base` is 1000 for `-h` and 1024 for `-hh` (compat.c:183).
    let bytes_f64 = bytes as f64;
    if bytes_f64 < base {
        return bytes.to_string();
    }

    let units = [
        ("P", base.powi(5)),
        ("T", base.powi(4)),
        ("G", base.powi(3)),
        ("M", base.powi(2)),
        ("K", base),
    ];

    for (suffix, threshold) in units {
        if bytes_f64 >= threshold {
            let value = bytes_f64 / threshold;
            return format!("{value:.2}{suffix}");
        }
    }

    bytes.to_string()
}

/// Width of the size column in `--list-only` output.
///
/// upstream: generator.c:1159 `list_file_entry` - `size_width = human_readable
/// ? 14 : 11`. `human_readable` defaults to 1 (options.c:111), so every oc
/// rendering mode (all of which print thousands-separated values like upstream's
/// default `human_num`) uses the 14-wide field; the 11-wide field only applies
/// to upstream's `--no-human-readable`, which oc does not distinctly model.
pub(crate) const LIST_SIZE_WIDTH: usize = 14;

pub(crate) fn format_list_size(size: u64, human_readable: HumanReadableMode) -> String {
    let value = format_size(size, human_readable);
    format!("{value:>LIST_SIZE_WIDTH$}")
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
    fn format_decimal_bytes_edge_cases() {
        assert_eq!(format_decimal_bytes(1), "1");
        assert_eq!(format_decimal_bytes(10), "10");
        assert_eq!(format_decimal_bytes(100), "100");
        assert_eq!(format_decimal_bytes(1_000), "1,000");
        assert_eq!(format_decimal_bytes(10_000), "10,000");
        assert_eq!(format_decimal_bytes(100_000), "100,000");
        assert_eq!(format_decimal_bytes(1_000_000_000), "1,000,000,000");
        assert_eq!(format_decimal_bytes(u64::MAX), "18,446,744,073,709,551,615");
    }

    // upstream: lib/compat.c:183 - `-h` (level 2) uses base 1000.
    const BASE_1000: f64 = 1000.0;
    // upstream: lib/compat.c:183 - `-hh` (level 3) uses base 1024.
    const BASE_1024: f64 = 1024.0;

    #[test]
    fn format_human_bytes_small() {
        assert_eq!(format_human_bytes(0, BASE_1000), "0");
        assert_eq!(format_human_bytes(999, BASE_1000), "999");
    }

    #[test]
    fn format_human_bytes_kilo() {
        assert_eq!(format_human_bytes(1_000, BASE_1000), "1.00K");
        assert_eq!(format_human_bytes(1_500, BASE_1000), "1.50K");
    }

    #[test]
    fn format_human_bytes_mega() {
        assert_eq!(format_human_bytes(1_000_000, BASE_1000), "1.00M");
        assert_eq!(format_human_bytes(2_500_000, BASE_1000), "2.50M");
    }

    #[test]
    fn format_human_bytes_giga() {
        assert_eq!(format_human_bytes(1_000_000_000, BASE_1000), "1.00G");
    }

    #[test]
    fn format_human_bytes_tera() {
        assert_eq!(format_human_bytes(1_000_000_000_000, BASE_1000), "1.00T");
    }

    #[test]
    fn format_human_bytes_base_1024() {
        // -hh divides by 1024: 2,201,503 bytes -> 2.10M (not 2.20M at base 1000).
        assert_eq!(format_human_bytes(2_201_503, BASE_1024), "2.10M");
        assert_eq!(format_human_bytes(1_024, BASE_1024), "1.00K");
        assert_eq!(format_human_bytes(1_048_576, BASE_1024), "1.00M");
    }

    #[test]
    fn format_list_size_pads_to_14() {
        // upstream: generator.c:1159 size_width = 14 (human_readable defaults to 1).
        let result = format_list_size(123, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 14);
        assert!(result.trim_start().starts_with("123"));
    }

    #[test]
    fn format_list_size_zero_pads_correctly() {
        let result = format_list_size(0, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 14);
        assert_eq!(result.trim(), "0");
        // Should be right-aligned: leading spaces then "0"
        assert!(result.ends_with('0'));
    }

    #[test]
    fn format_list_size_large_value_with_separators() {
        let result = format_list_size(1_234_567, HumanReadableMode::Disabled);
        assert_eq!(result.len(), 14);
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
        assert_eq!(result.len(), 14);
        assert_eq!(result.trim(), "500");
    }

    #[test]
    fn format_list_size_human_readable_kilo() {
        let result = format_list_size(1_500, HumanReadableMode::Enabled);
        assert_eq!(result.len(), 14);
        assert!(
            result.contains("1.50K"),
            "1500 bytes should show as 1.50K in human-readable: {result:?}"
        );
    }

    #[test]
    fn format_list_size_human_readable_mega() {
        let result = format_list_size(2_500_000, HumanReadableMode::Enabled);
        assert_eq!(result.len(), 14);
        assert!(
            result.contains("2.50M"),
            "2.5M bytes should show as 2.50M in human-readable: {result:?}"
        );
    }

    #[test]
    fn format_list_size_is_right_aligned() {
        let small = format_list_size(1, HumanReadableMode::Disabled);
        let large = format_list_size(1_000_000, HumanReadableMode::Disabled);

        assert_eq!(small.len(), 14);
        assert_eq!(large.len(), 14);

        let small_spaces = small.len() - small.trim_start().len();
        let large_spaces = large.len() - large.trim_start().len();
        assert!(
            small_spaces > large_spaces,
            "smaller value should have more leading spaces: small={small_spaces}, large={large_spaces}"
        );
    }
}
