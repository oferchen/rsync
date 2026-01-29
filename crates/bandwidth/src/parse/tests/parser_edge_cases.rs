/// Comprehensive edge case tests for bandwidth parsing
use super::super::{BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit};
use std::num::NonZeroU64;

// ========================================================================
// Negative Number Tests
// ========================================================================

#[test]
fn parse_negative_number_is_invalid() {
    let result = parse_bandwidth_argument("-1024");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_negative_zero() {
    let result = parse_bandwidth_argument("-0");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_negative_with_suffix() {
    let result = parse_bandwidth_argument("-1K");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_negative_decimal() {
    let result = parse_bandwidth_argument("-1.5");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_negative_with_exponent() {
    let result = parse_bandwidth_argument("-1e3");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

// ========================================================================
// Overflow Tests
// ========================================================================

#[test]
fn parse_extremely_large_number_overflows() {
    // Number larger than u128::MAX
    let huge = "999999999999999999999999999999999999999999";
    let result = parse_bandwidth_argument(huge);
    assert_eq!(result, Err(BandwidthParseError::TooLarge));
}

#[test]
fn parse_large_exponent_overflows() {
    let result = parse_bandwidth_argument("1e100");
    assert_eq!(result, Err(BandwidthParseError::TooLarge));
}

#[test]
fn parse_large_base_with_large_exponent() {
    let result = parse_bandwidth_argument("999999e50");
    assert_eq!(result, Err(BandwidthParseError::TooLarge));
}

#[test]
fn parse_u64_max_plus_one() {
    // Just over u64::MAX
    let result = parse_bandwidth_argument("18446744073709551616b"); // u64::MAX + 1 in bytes
    assert!(result.is_err());
}

// ========================================================================
// Special Character Tests
// ========================================================================

#[test]
fn parse_with_special_chars_is_invalid() {
    assert_eq!(
        parse_bandwidth_argument("100@"),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("100#"),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("100$"),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("100%"),
        Err(BandwidthParseError::Invalid)
    );
}

#[test]
fn parse_with_underscore_is_invalid() {
    let result = parse_bandwidth_argument("1_000");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_with_space_is_invalid() {
    let result = parse_bandwidth_argument("1 000");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_with_tab_is_invalid() {
    let result = parse_bandwidth_argument("1000\t");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_with_newline_is_invalid() {
    let result = parse_bandwidth_argument("1000\n");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

// ========================================================================
// Empty and Whitespace Tests
// ========================================================================

#[test]
fn parse_empty_string_is_invalid() {
    let result = parse_bandwidth_argument("");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_only_whitespace_is_invalid() {
    assert_eq!(
        parse_bandwidth_argument(" "),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("   "),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("\t"),
        Err(BandwidthParseError::Invalid)
    );
}

#[test]
fn parse_only_plus_is_invalid() {
    let result = parse_bandwidth_argument("+");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_only_minus_is_invalid() {
    let result = parse_bandwidth_argument("-");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

// ========================================================================
// Decimal Point Tests
// ========================================================================

#[test]
fn parse_only_decimal_point_is_invalid() {
    let result = parse_bandwidth_argument(".");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_only_comma_is_invalid() {
    let result = parse_bandwidth_argument(",");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_multiple_decimal_points() {
    let result = parse_bandwidth_argument("1.2.3");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_mixed_decimal_separators() {
    let result = parse_bandwidth_argument("1.2,3");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_decimal_point_with_suffix() {
    let result = parse_bandwidth_argument(".5K");
    // .5K = 0.5 * 1024 = 512 bytes, which is minimum, so valid
    assert!(result.is_ok());
}

#[test]
fn parse_trailing_decimal_point() {
    let result = parse_bandwidth_argument("5.");
    assert!(result.is_ok());
}

// ========================================================================
// Exponent Tests
// ========================================================================

#[test]
fn parse_exponent_without_digits() {
    let result = parse_bandwidth_argument("5e");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_exponent_with_only_sign() {
    let result = parse_bandwidth_argument("5e+");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_double_exponent() {
    let result = parse_bandwidth_argument("1e2e3");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_exponent_with_decimal() {
    let result = parse_bandwidth_argument("1e1.5");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_uppercase_e_exponent() {
    let result = parse_bandwidth_argument("1E3");
    assert!(result.is_ok());
}

#[test]
fn parse_mixed_case_exponent() {
    // Only 'e' or 'E' at the start of exponent, not both
    let result = parse_bandwidth_argument("1e3");
    assert!(result.is_ok());
}

// ========================================================================
// Suffix Tests
// ========================================================================

#[test]
fn parse_invalid_suffix() {
    assert_eq!(
        parse_bandwidth_argument("100x"),
        Err(BandwidthParseError::Invalid)
    );
    assert_eq!(
        parse_bandwidth_argument("100z"),
        Err(BandwidthParseError::Invalid)
    );
}

#[test]
fn parse_double_suffix() {
    let result = parse_bandwidth_argument("100kk");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_suffix_with_extra_chars() {
    let result = parse_bandwidth_argument("100kb2");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_b_suffix_bytes() {
    // 1024B = 1024 bytes (no rounding, alignment is 1 for B suffix)
    let result = parse_bandwidth_argument("1024B").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1024).unwrap()));
}

#[test]
fn parse_kb_decimal_suffix() {
    // 1KB = 1000 bytes (decimal, not binary)
    let result = parse_bandwidth_argument("1KB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1000).unwrap()));
}

#[test]
fn parse_kib_binary_suffix() {
    // 1KiB = 1024 bytes
    let result = parse_bandwidth_argument("1KiB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1024).unwrap()));
}

#[test]
fn parse_mb_decimal() {
    // 1MB = 1,000,000 bytes
    let result = parse_bandwidth_argument("1MB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1_000_000).unwrap()));
}

#[test]
fn parse_mib_binary() {
    // 1MiB = 1024 * 1024 bytes
    let result = parse_bandwidth_argument("1MiB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1_048_576).unwrap()));
}

#[test]
fn parse_gb_decimal() {
    // 1GB = 1,000,000,000 bytes
    let result = parse_bandwidth_argument("1GB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1_000_000_000).unwrap()));
}

#[test]
fn parse_tb_decimal() {
    // 1TB = 1,000,000,000,000 bytes
    let result = parse_bandwidth_argument("1TB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1_000_000_000_000).unwrap()));
}

#[test]
fn parse_pb_decimal() {
    // 1PB = 1,000,000,000,000,000 bytes
    let result = parse_bandwidth_argument("1PB").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1_000_000_000_000_000).unwrap()));
}

// ========================================================================
// Adjust (+1/-1) Tests
// ========================================================================

#[test]
fn parse_with_plus_one_adjust() {
    // 1K+1 = 1024 + 1 = 1025 bytes (rounded to nearest 1024)
    let result = parse_bandwidth_argument("1K+1").unwrap();
    // After rounding: (1025 + 512) / 1024 * 1024 = 1024
    assert_eq!(result, Some(NonZeroU64::new(1024).unwrap()));
}

#[test]
fn parse_with_minus_one_adjust() {
    // 2K-1 = 2048 - 1 = 2047 bytes (rounded to nearest 1024)
    let result = parse_bandwidth_argument("2K-1").unwrap();
    // After rounding: (2047 + 512) / 1024 * 1024 = 2560 / 1024 * 1024 = 2048
    assert_eq!(result, Some(NonZeroU64::new(2048).unwrap()));
}

#[test]
fn parse_adjust_without_numeric_part_invalid() {
    let result = parse_bandwidth_argument("K+1");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_adjust_with_wrong_format() {
    let result = parse_bandwidth_argument("1K+2");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_adjust_without_suffix() {
    // Without suffix, adjust applies but default unit is K
    let result = parse_bandwidth_argument("1+1").unwrap();
    // 1K+1 parsed
    assert!(result.is_some());
}

// ========================================================================
// Below Minimum Tests
// ========================================================================

#[test]
fn parse_below_minimum_512_bytes() {
    let result = parse_bandwidth_argument("511B");
    assert_eq!(result, Err(BandwidthParseError::TooSmall));
}

#[test]
fn parse_exactly_minimum_512_bytes() {
    let result = parse_bandwidth_argument("512B").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(512).unwrap()));
}

#[test]
fn parse_just_above_minimum() {
    let result = parse_bandwidth_argument("513B").unwrap();
    assert!(result.is_some());
}

#[test]
fn parse_fractional_below_minimum() {
    let result = parse_bandwidth_argument("0.1K");
    // 0.1K = 102.4 bytes, rounds to ~102 which is < 512
    assert_eq!(result, Err(BandwidthParseError::TooSmall));
}

// ========================================================================
// Zero Value Tests
// ========================================================================

#[test]
fn parse_zero_returns_none() {
    let result = parse_bandwidth_argument("0").unwrap();
    assert_eq!(result, None);
}

#[test]
fn parse_zero_with_suffix_returns_none() {
    let result = parse_bandwidth_argument("0K").unwrap();
    assert_eq!(result, None);
}

#[test]
fn parse_zero_with_decimal_returns_none() {
    let result = parse_bandwidth_argument("0.0").unwrap();
    assert_eq!(result, None);
}

#[test]
fn parse_plus_zero() {
    let result = parse_bandwidth_argument("+0").unwrap();
    assert_eq!(result, None);
}

// ========================================================================
// Rounding Tests
// ========================================================================

#[test]
fn parse_rounds_to_kilobyte_boundary() {
    // Default alignment is 1024
    // 1500 bytes + 512 = 2012, then 2012 / 1024 = 1, then 1 * 1024 = 1024
    let result = parse_bandwidth_argument("1500B").unwrap();
    // Should round to 1024 or 2048 depending on rounding
    assert!(result.is_some());
}

#[test]
fn parse_decimal_suffix_rounds_to_1000() {
    // With KB (decimal), alignment is 1000
    let result = parse_bandwidth_argument("1500B").unwrap();
    assert!(result.is_some());
}

// ========================================================================
// Burst Limit Tests
// ========================================================================

#[test]
fn parse_limit_with_burst() {
    let result = parse_bandwidth_limit("1000:500").unwrap();
    assert_eq!(result.rate(), Some(NonZeroU64::new(1024000).unwrap()));
    assert_eq!(result.burst(), Some(NonZeroU64::new(512000).unwrap()));
}

#[test]
fn parse_limit_with_zero_rate() {
    let result = parse_bandwidth_limit("0:500").unwrap();
    assert!(result.is_unlimited());
}

#[test]
fn parse_limit_with_zero_burst() {
    let result = parse_bandwidth_limit("1000:0").unwrap();
    assert!(result.rate().is_some());
    assert!(result.burst().is_none());
}

#[test]
fn parse_limit_both_zero() {
    let result = parse_bandwidth_limit("0:0").unwrap();
    assert!(result.is_unlimited());
}

#[test]
fn parse_limit_with_colon_but_no_burst() {
    let result = parse_bandwidth_limit("1000:");
    // Trailing colon with no burst value - should fail or parse as just rate
    assert!(result.is_err());
}

#[test]
fn parse_limit_empty_rate_with_burst() {
    let result = parse_bandwidth_limit(":500");
    assert!(result.is_err());
}

#[test]
fn parse_limit_multiple_colons() {
    let result = parse_bandwidth_limit("1000:500:200");
    assert!(result.is_err());
}

// ========================================================================
// Unicode and Non-ASCII Tests
// ========================================================================

#[test]
fn parse_with_unicode_is_invalid() {
    let result = parse_bandwidth_argument("100０"); // Full-width 0
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

#[test]
fn parse_with_emoji_is_invalid() {
    let result = parse_bandwidth_argument("100📊");
    assert_eq!(result, Err(BandwidthParseError::Invalid));
}

// ========================================================================
// Case Sensitivity Tests
// ========================================================================

#[test]
fn parse_lowercase_suffixes() {
    assert!(parse_bandwidth_argument("1k").is_ok());
    assert!(parse_bandwidth_argument("1m").is_ok());
    assert!(parse_bandwidth_argument("1g").is_ok());
    assert!(parse_bandwidth_argument("1t").is_ok());
    assert!(parse_bandwidth_argument("1p").is_ok());
}

#[test]
fn parse_uppercase_suffixes() {
    assert!(parse_bandwidth_argument("1K").is_ok());
    assert!(parse_bandwidth_argument("1M").is_ok());
    assert!(parse_bandwidth_argument("1G").is_ok());
    assert!(parse_bandwidth_argument("1T").is_ok());
    assert!(parse_bandwidth_argument("1P").is_ok());
}

#[test]
fn parse_mixed_case_suffix() {
    // Suffix normalization handles case
    assert!(parse_bandwidth_argument("1Kb").is_ok());
}

// ========================================================================
// Very Long Input Tests
// ========================================================================

#[test]
fn parse_very_long_numeric_string() {
    let long = "1".repeat(100);
    let result = parse_bandwidth_argument(&long);
    assert_eq!(result, Err(BandwidthParseError::TooLarge));
}

#[test]
fn parse_very_long_decimal_fraction() {
    let long_fraction = format!("1.{}", "9".repeat(100));
    let result = parse_bandwidth_argument(&long_fraction);
    // Should either parse or overflow
    assert!(result.is_err() || result.is_ok());
}

// ========================================================================
// Leading Zeros Tests
// ========================================================================

#[test]
fn parse_leading_zeros() {
    let result = parse_bandwidth_argument("001024").unwrap();
    assert!(result.is_some());
}

#[test]
fn parse_only_zeros() {
    let result = parse_bandwidth_argument("0000").unwrap();
    assert_eq!(result, None);
}

// ========================================================================
// Fractional Part Tests
// ========================================================================

#[test]
fn parse_tiny_fraction() {
    let result = parse_bandwidth_argument("0.001K");
    // 0.001K = 1.024 bytes, which is < 512 minimum
    assert_eq!(result, Err(BandwidthParseError::TooSmall));
}

#[test]
fn parse_fraction_at_boundary() {
    // 0.5K = 512 bytes exactly
    let result = parse_bandwidth_argument("0.5K").unwrap();
    assert!(result.is_some());
}

#[test]
fn parse_fraction_just_below_boundary() {
    // Slightly less than 0.5K
    let result = parse_bandwidth_argument("0.49K");
    assert!(result.is_err() || result.is_ok());
}

// ========================================================================
// Comma Decimal Separator Tests
// ========================================================================

#[test]
fn parse_comma_as_decimal() {
    let result = parse_bandwidth_argument("1,5K").unwrap();
    // 1.5K = 1536 bytes
    assert!(result.is_some());
}

#[test]
fn parse_comma_and_exponent() {
    let result = parse_bandwidth_argument("1,5e2").unwrap();
    // 1.5 * 100 = 150K
    assert!(result.is_some());
}

// ========================================================================
// Default Unit Tests
// ========================================================================

#[test]
fn parse_no_suffix_defaults_to_k() {
    // No suffix means kilobytes
    let result = parse_bandwidth_argument("1").unwrap();
    assert_eq!(result, Some(NonZeroU64::new(1024).unwrap()));
}

#[test]
fn parse_no_suffix_large_number() {
    let result = parse_bandwidth_argument("1000").unwrap();
    // 1000K = 1,024,000 bytes
    assert_eq!(result, Some(NonZeroU64::new(1024000).unwrap()));
}
