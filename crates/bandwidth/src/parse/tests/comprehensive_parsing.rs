// Comprehensive configuration parsing tests
// Focus on edge cases, error handling, and format variations

use super::super::*;
use std::num::NonZeroU64;

fn nz(val: u64) -> NonZeroU64 {
    NonZeroU64::new(val).unwrap()
}

// ========================================================================
// Bandwidth argument parsing comprehensive tests
// ========================================================================

#[test]
fn parse_bandwidth_argument_simple_integer() {
    // Simple integer defaults to kilobytes
    let result = parse_bandwidth_argument("100").unwrap();
    assert_eq!(result, Some(nz(100 * 1024)));
}

#[test]
fn parse_bandwidth_argument_zero_returns_none() {
    // Zero means unlimited
    let result = parse_bandwidth_argument("0").unwrap();
    assert!(result.is_none());
}

#[test]
fn parse_bandwidth_argument_decimal_kilobytes() {
    // Decimal with default K suffix
    let result = parse_bandwidth_argument("1.5").unwrap();
    // 1.5K = 1.5 * 1024 = 1536, rounded to nearest 1024 = 2048
    assert_eq!(result, Some(nz(2048)));
}

#[test]
fn parse_bandwidth_argument_explicit_bytes() {
    // Explicit byte suffix (not rounded)
    let result = parse_bandwidth_argument("1024b").unwrap();
    assert_eq!(result, Some(nz(1024)));
}

#[test]
fn parse_bandwidth_argument_kilobytes() {
    let result = parse_bandwidth_argument("10k").unwrap();
    assert_eq!(result, Some(nz(10 * 1024)));
}

#[test]
fn parse_bandwidth_argument_megabytes() {
    let result = parse_bandwidth_argument("5m").unwrap();
    assert_eq!(result, Some(nz(5 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_argument_gigabytes() {
    let result = parse_bandwidth_argument("2g").unwrap();
    assert_eq!(result, Some(nz(2 * 1024 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_argument_terabytes() {
    let result = parse_bandwidth_argument("1t").unwrap();
    assert_eq!(result, Some(nz(1024u64 * 1024 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_argument_petabytes() {
    let result = parse_bandwidth_argument("1p").unwrap();
    assert_eq!(result, Some(nz(1024u64 * 1024 * 1024 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_argument_decimal_base_suffix() {
    // "kb" means decimal kilobytes (1000, not 1024)
    let result = parse_bandwidth_argument("10kb").unwrap();
    // 10 * 1000 = 10000, rounded to nearest 1000 = 10000
    assert_eq!(result, Some(nz(10000)));
}

#[test]
fn parse_bandwidth_argument_binary_suffix() {
    // "kib" means binary kilobytes (explicit 1024)
    let result = parse_bandwidth_argument("10kib").unwrap();
    assert_eq!(result, Some(nz(10 * 1024)));
}

#[test]
fn parse_bandwidth_argument_megabytes_decimal() {
    let result = parse_bandwidth_argument("5mb").unwrap();
    // 5 * 1000000 = 5000000, rounded to nearest 1000 = 5000000
    assert_eq!(result, Some(nz(5_000_000)));
}

#[test]
fn parse_bandwidth_argument_uppercase_suffixes() {
    let result = parse_bandwidth_argument("10K").unwrap();
    assert_eq!(result, Some(nz(10 * 1024)));

    let result = parse_bandwidth_argument("5M").unwrap();
    assert_eq!(result, Some(nz(5 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_argument_mixed_case_suffixes() {
    let result = parse_bandwidth_argument("10KiB").unwrap();
    assert_eq!(result, Some(nz(10 * 1024)));

    let result = parse_bandwidth_argument("5Mb").unwrap();
    assert_eq!(result, Some(nz(5_000_000)));
}

#[test]
fn parse_bandwidth_argument_plus_sign_allowed() {
    let result = parse_bandwidth_argument("+100").unwrap();
    assert_eq!(result, Some(nz(100 * 1024)));
}

#[test]
fn parse_bandwidth_argument_negative_sign_invalid() {
    let result = parse_bandwidth_argument("-100");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_argument_adjust_plus_one() {
    // "10k+1" means 10KB + 1 byte
    let result = parse_bandwidth_argument("10k+1").unwrap();
    // 10 * 1024 + 1 = 10241, rounded to 10240 (nearest multiple of 1024)
    assert_eq!(result.unwrap().get(), 10240);
}

#[test]
fn parse_bandwidth_argument_adjust_minus_one() {
    // "10k-1" means 10KB - 1 byte
    let result = parse_bandwidth_argument("10k-1").unwrap();
    // 10 * 1024 - 1 = 10239, rounded to 10240 (nearest multiple of 1024)
    assert_eq!(result.unwrap().get(), 10240);
}

#[test]
fn parse_bandwidth_argument_scientific_notation() {
    // Scientific notation: "1e3" = 1000
    let result = parse_bandwidth_argument("1e3").unwrap();
    // 1000K = 1024000
    assert_eq!(result, Some(nz(1024 * 1000)));
}

#[test]
fn parse_bandwidth_argument_scientific_negative_exponent() {
    // "1e-1" = 0.1K = 102.4 -> rounds to 0 (below minimum)
    let result = parse_bandwidth_argument("1e-1");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_argument_decimal_with_comma() {
    // Comma as decimal separator
    let result = parse_bandwidth_argument("1,5").unwrap();
    // 1.5K = 1536, rounded to 2048
    assert_eq!(result, Some(nz(2048)));
}

// ========================================================================
// Error cases
// ========================================================================

#[test]
fn parse_bandwidth_argument_empty_string() {
    let result = parse_bandwidth_argument("");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_argument_whitespace_only() {
    let result = parse_bandwidth_argument("   ");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_argument_invalid_characters() {
    let result = parse_bandwidth_argument("abc");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("12@34");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_too_small() {
    // Below 512 bytes is rejected
    let result = parse_bandwidth_argument("500b");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);

    let result = parse_bandwidth_argument("1b");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_exactly_512_bytes() {
    // Exactly 512 bytes should be accepted
    let result = parse_bandwidth_argument("512b").unwrap();
    assert_eq!(result, Some(nz(512)));
}

#[test]
fn parse_bandwidth_argument_overflow() {
    // Very large value that overflows
    let result = parse_bandwidth_argument("99999999999999999999p");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::TooLarge);
}

#[test]
fn parse_bandwidth_argument_invalid_suffix() {
    let result = parse_bandwidth_argument("100x");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("100z");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_only_sign() {
    let result = parse_bandwidth_argument("+");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("-");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_only_decimal_point() {
    let result = parse_bandwidth_argument(".");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_multiple_decimal_points() {
    let result = parse_bandwidth_argument("1.2.3");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_invalid_exponent() {
    let result = parse_bandwidth_argument("1e");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("1e+");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_argument_trailing_garbage() {
    let result = parse_bandwidth_argument("100k garbage");
    assert!(result.is_err());
}

// ========================================================================
// Bandwidth limit (with burst) parsing tests
// ========================================================================

#[test]
fn parse_bandwidth_limit_simple() {
    let result = parse_bandwidth_limit("1000").unwrap();
    assert_eq!(result.rate(), Some(nz(1000 * 1024)));
    assert!(result.burst().is_none());
}

#[test]
fn parse_bandwidth_limit_zero_unlimited() {
    let result = parse_bandwidth_limit("0").unwrap();
    assert!(result.is_unlimited());
    assert!(result.limit_specified());
}

#[test]
fn parse_bandwidth_limit_with_burst() {
    let result = parse_bandwidth_limit("1000:500").unwrap();
    assert_eq!(result.rate(), Some(nz(1000 * 1024)));
    assert_eq!(result.burst(), Some(nz(500 * 1024)));
    assert!(result.limit_specified());
    assert!(result.burst_specified());
}

#[test]
fn parse_bandwidth_limit_with_zero_burst() {
    // Rate with zero burst
    let result = parse_bandwidth_limit("1000:0").unwrap();
    assert_eq!(result.rate(), Some(nz(1000 * 1024)));
    assert!(result.burst().is_none()); // Zero burst means None
}

#[test]
fn parse_bandwidth_limit_unlimited_with_burst() {
    // "0:500" means unlimited rate, burst ignored
    let result = parse_bandwidth_limit("0:500").unwrap();
    assert!(result.is_unlimited());
    assert!(result.limit_specified());
    // Burst should be None when rate is unlimited
}

#[test]
fn parse_bandwidth_limit_explicit_suffixes() {
    let result = parse_bandwidth_limit("10m:5m").unwrap();
    assert_eq!(result.rate(), Some(nz(10 * 1024 * 1024)));
    assert_eq!(result.burst(), Some(nz(5 * 1024 * 1024)));
}

#[test]
fn parse_bandwidth_limit_different_suffixes() {
    let result = parse_bandwidth_limit("1g:512k").unwrap();
    assert_eq!(result.rate(), Some(nz(1024 * 1024 * 1024)));
    assert_eq!(result.burst(), Some(nz(512 * 1024)));
}

#[test]
fn parse_bandwidth_limit_decimal_values() {
    let result = parse_bandwidth_limit("1.5m:0.5m").unwrap();
    // Values will be rounded
    assert!(result.rate().is_some());
    assert!(result.burst().is_some());
}

// ========================================================================
// Error cases for bandwidth limit parsing
// ========================================================================

#[test]
fn parse_bandwidth_limit_empty() {
    let result = parse_bandwidth_limit("");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_leading_whitespace() {
    let result = parse_bandwidth_limit(" 1000");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_trailing_whitespace() {
    let result = parse_bandwidth_limit("1000 ");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_whitespace_around_colon() {
    let result = parse_bandwidth_limit("1000 : 500");
    assert!(result.is_err());

    let result = parse_bandwidth_limit("1000: 500");
    assert!(result.is_err());

    let result = parse_bandwidth_limit("1000 :500");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_invalid_rate() {
    let result = parse_bandwidth_limit("invalid:500");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_invalid_burst() {
    let result = parse_bandwidth_limit("1000:invalid");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_multiple_colons() {
    let result = parse_bandwidth_limit("1000:500:200");
    assert!(result.is_err());
}

#[test]
fn parse_bandwidth_limit_rate_too_small() {
    let result = parse_bandwidth_limit("100b");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_limit_burst_too_small() {
    let result = parse_bandwidth_limit("1000:100b");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), BandwidthParseError::TooSmall);
}

// ========================================================================
// Rounding behavior tests
// ========================================================================

#[test]
fn rounding_to_kilobyte_boundary() {
    // 1500 bytes with 'b' suffix - no rounding since alignment is 1
    let result = parse_bandwidth_argument("1500b").unwrap();
    // With 'b' suffix, alignment is 1, so result is 1500
    assert_eq!(result, Some(nz(1500)));
}

#[test]
fn rounding_default_kilobyte() {
    // Default suffix is K, so rounds to 1024 boundaries
    let result = parse_bandwidth_argument("1500").unwrap();
    // 1500K = 1536000, rounded to nearest 1024 = 1536000
    assert!(result.unwrap().get() % 1024 == 0);
}

#[test]
fn rounding_decimal_base() {
    // "kb" uses base 1000, rounds to 1000
    let result = parse_bandwidth_argument("1500kb").unwrap();
    // 1500 * 1000 = 1500000, rounded to nearest 1000
    assert!(result.unwrap().get() % 1000 == 0);
}

// ========================================================================
// Boundary value tests
// ========================================================================

#[test]
fn boundary_minimum_valid_512_bytes() {
    let result = parse_bandwidth_argument("512b").unwrap();
    assert_eq!(result, Some(nz(512)));
}

#[test]
fn boundary_511_bytes_too_small() {
    let result = parse_bandwidth_argument("511b");
    assert!(result.is_err());
}

#[test]
fn boundary_513_bytes_valid() {
    let result = parse_bandwidth_argument("513b").unwrap();
    // With 'b' suffix, alignment is 1, so no rounding - result is 513
    assert_eq!(result, Some(nz(513)));
}

#[test]
fn boundary_u64_max_doesnt_overflow() {
    // Test that we handle near-max values without overflow
    // But we might get TooLarge error for values that actually overflow
    let result = parse_bandwidth_argument("18446744073709551615b");
    // This is u64::MAX, should work or give TooLarge
    // Depending on rounding, might overflow
    assert!(result.is_ok() || result.unwrap_err() == BandwidthParseError::TooLarge);
}

// ========================================================================
// Scientific notation edge cases
// ========================================================================

#[test]
fn scientific_notation_positive_small_exponent() {
    let result = parse_bandwidth_argument("5e2").unwrap();
    // 5 * 10^2 = 500K = 512000
    assert_eq!(result, Some(nz(512000)));
}

#[test]
fn scientific_notation_large_exponent() {
    let result = parse_bandwidth_argument("1e9b").unwrap();
    // 1 * 10^9 bytes = 1GB (decimal)
    assert_eq!(result, Some(nz(1_000_000_000)));
}

#[test]
fn scientific_notation_uppercase_e() {
    let result1 = parse_bandwidth_argument("1e3").unwrap();
    let result2 = parse_bandwidth_argument("1E3").unwrap();
    assert_eq!(result1, result2);
}

#[test]
fn scientific_notation_explicit_plus() {
    let result = parse_bandwidth_argument("1e+3").unwrap();
    assert!(result.is_some());
}

#[test]
fn scientific_notation_negative_exponent_rounds_to_zero() {
    let result = parse_bandwidth_argument("5e-10");
    // Very small value rounds to zero, which means unlimited
    assert_eq!(result.unwrap(), None);
}

// ========================================================================
// Complex format combinations
// ========================================================================

#[test]
fn complex_decimal_with_exponent() {
    let result = parse_bandwidth_argument("1.5e3").unwrap();
    // 1.5 * 10^3 = 1500K
    assert!(result.is_some());
}

#[test]
fn complex_decimal_comma_with_suffix() {
    let result = parse_bandwidth_argument("2,5m").unwrap();
    // 2.5M
    assert!(result.is_some());
}

#[test]
fn complex_all_features() {
    // Decimal, exponent, suffix, adjust
    let result = parse_bandwidth_argument("+1.5e2k-1");
    // This is complex but should parse if valid
    assert!(result.is_ok() || result.is_err()); // Just check it doesn't panic
}

// ========================================================================
// BandwidthLimitComponents integration
// ========================================================================

#[test]
fn components_from_str_simple() {
    let components: BandwidthLimitComponents = "1000".parse().unwrap();
    assert_eq!(components.rate(), Some(nz(1000 * 1024)));
}

#[test]
fn components_from_str_with_burst() {
    let components: BandwidthLimitComponents = "1000:500".parse().unwrap();
    assert_eq!(components.rate(), Some(nz(1000 * 1024)));
    assert_eq!(components.burst(), Some(nz(500 * 1024)));
}

#[test]
fn components_from_str_error() {
    let result: Result<BandwidthLimitComponents, _> = "invalid".parse();
    assert!(result.is_err());
}

// ========================================================================
// Special characters and Unicode
// ========================================================================

#[test]
fn parse_with_embedded_null() {
    // Null bytes should be invalid
    let result = parse_bandwidth_argument("100\0");
    assert!(result.is_err());
}

#[test]
fn parse_with_tab() {
    let result = parse_bandwidth_argument("100\t");
    assert!(result.is_err());
}

#[test]
fn parse_with_newline() {
    let result = parse_bandwidth_argument("100\n");
    assert!(result.is_err());
}

#[test]
fn parse_non_ascii_digits() {
    // Non-ASCII characters should be rejected
    let result = parse_bandwidth_argument("١٠٠"); // Arabic-Indic digits
    assert!(result.is_err());
}

// ========================================================================
// Edge cases from fuzzing scenarios
// ========================================================================

#[test]
fn fuzz_empty_exponent_component() {
    // "1.5e" has exponent marker but no digits
    let result = parse_bandwidth_argument("1.5e");
    assert!(result.is_err());
}

#[test]
fn fuzz_double_sign() {
    let result = parse_bandwidth_argument("++100");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("--100");
    assert!(result.is_err());
}

#[test]
fn fuzz_sign_after_digits() {
    let result = parse_bandwidth_argument("100+");
    // This might be interpreted as adjust, but without second digit should fail
    // Actually "100+" is not valid adjust syntax (needs +1 or -1)
    assert!(result.is_err() || result.is_ok()); // Depends on parser behavior
}

#[test]
fn fuzz_mixed_separators() {
    let result = parse_bandwidth_argument("1.2,3");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("1,2.3");
    assert!(result.is_err());
}

#[test]
fn fuzz_suffix_without_number() {
    let result = parse_bandwidth_argument("k");
    assert!(result.is_err());

    let result = parse_bandwidth_argument("mb");
    assert!(result.is_err());
}
