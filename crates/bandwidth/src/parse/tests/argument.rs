use super::{BandwidthParseError, NonZeroU64, parse_bandwidth_argument};
use proptest::prelude::*;

#[test]
fn parse_bandwidth_accepts_binary_units() {
    let limit = parse_bandwidth_argument("12M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(12 * 1024 * 1024));
}

#[test]
fn parse_bandwidth_accepts_decimal_units() {
    let limit = parse_bandwidth_argument("12MB").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(12_000_000));
}

#[test]
fn parse_bandwidth_accepts_explicit_byte_suffix() {
    let limit = parse_bandwidth_argument("512b").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(512));

    let uppercase = parse_bandwidth_argument("512B").expect("parse succeeds");
    assert_eq!(uppercase, limit);

    let too_small = parse_bandwidth_argument("10b").unwrap_err();
    assert_eq!(too_small, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_rejects_space_between_value_and_suffix() {
    let error = parse_bandwidth_argument("1 M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_numeric_separators() {
    for text in ["1_000K", "2M_", "4G__1", "1e3_", "_1K"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid, "input: {text}");
    }
}

#[test]
fn parse_bandwidth_accepts_iec_suffixes() {
    let limit = parse_bandwidth_argument("1MiB").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1_048_576));
}

#[test]
fn parse_bandwidth_accepts_trailing_decimal_point() {
    let limit = parse_bandwidth_argument("1.").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_accepts_exponent_notation() {
    let bytes = parse_bandwidth_argument("1e3b").expect("parse succeeds");
    assert_eq!(bytes, NonZeroU64::new(1_000));

    let kibibytes = parse_bandwidth_argument("2.5e2K").expect("parse succeeds");
    assert_eq!(kibibytes, NonZeroU64::new(256_000));

    let decimal = parse_bandwidth_argument("1e3MB").expect("parse succeeds");
    assert_eq!(decimal, NonZeroU64::new(1_000_000_000));
}

#[test]
fn parse_bandwidth_accepts_large_unit_suffixes() {
    let gibibytes = parse_bandwidth_argument("1G").expect("parse succeeds");
    assert_eq!(gibibytes, NonZeroU64::new(1_024u64.pow(3)));

    let tebibytes = parse_bandwidth_argument("2TiB").expect("parse succeeds");
    assert_eq!(tebibytes, NonZeroU64::new(2 * 1_024u64.pow(4)));

    let pebibytes = parse_bandwidth_argument("3P").expect("parse succeeds");
    assert_eq!(pebibytes, NonZeroU64::new(3 * 1_024u64.pow(5)));
}

#[test]
fn parse_bandwidth_accepts_zero_for_unlimited() {
    assert_eq!(parse_bandwidth_argument("0").expect("parse"), None);
}

#[test]
fn parse_bandwidth_rejects_small_values() {
    let error = parse_bandwidth_argument("0.25K").unwrap_err();
    assert_eq!(error, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_rejects_invalid_suffix() {
    let error = parse_bandwidth_argument("10Q").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_non_ascii_characters() {
    for text in ["10µ", "\u{FF11}\u{FF12}M"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_handles_fractional_values() {
    let limit = parse_bandwidth_argument("0.5M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(512 * 1024));
}

#[test]
fn parse_bandwidth_accepts_leading_decimal_without_integer_part() {
    let limit = parse_bandwidth_argument(".5M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(512 * 1024));
}

#[test]
fn parse_bandwidth_accepts_leading_plus_sign() {
    let limit = parse_bandwidth_argument("+1M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1_048_576));
}

#[test]
fn parse_bandwidth_honours_postfix_adjustments_for_byte_suffix() {
    let incremented = parse_bandwidth_argument("600b+1").expect("parse succeeds");
    assert_eq!(incremented, NonZeroU64::new(601));

    let decremented = parse_bandwidth_argument("600b-1").expect("parse succeeds");
    assert_eq!(decremented, NonZeroU64::new(599));
}

#[test]
fn parse_bandwidth_rejects_missing_digits_after_sign() {
    for text in ["+", "-", " + "] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_accepts_comma_fraction_separator() {
    let limit = parse_bandwidth_argument("0,5M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(512 * 1024));
}

#[test]
fn parse_bandwidth_rejects_surrounding_whitespace() {
    let error = parse_bandwidth_argument("\t 2M \n").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_accepts_positive_adjustment() {
    let limit = parse_bandwidth_argument("1K+1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_rejects_whitespace_before_adjustment() {
    let error = parse_bandwidth_argument("1K +1").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_whitespace_within_adjustment() {
    for text in ["1K+ 1", "1K + 1", "1K- 1"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_rejects_incomplete_iec_suffix() {
    for text in ["1Ki", "1Mi", "1Mi+", "1Mi-", "1Mi:"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_accepts_scientific_notation_without_suffix() {
    let limit = parse_bandwidth_argument("1e3").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1_024_000));

    let uppercase = parse_bandwidth_argument("1E3").expect("parse succeeds");
    assert_eq!(uppercase, limit);
}

#[test]
fn parse_bandwidth_accepts_scientific_notation_with_suffix() {
    let limit = parse_bandwidth_argument("2.5e2M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(262_144_000));
}

#[test]
fn parse_bandwidth_accepts_negative_scientific_notation() {
    let limit = parse_bandwidth_argument("1e-1M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(104_448));
}

#[test]
fn parse_bandwidth_rejects_non_unit_adjustment_value() {
    let error = parse_bandwidth_argument("1K+ 2").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_honours_negative_adjustment_for_small_values() {
    let limit = parse_bandwidth_argument("0.001M-1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(0x400));
}

#[test]
fn parse_bandwidth_negative_adjustment_can_trigger_too_small() {
    let error = parse_bandwidth_argument("0.0001M-1").unwrap_err();
    assert_eq!(error, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_rejects_trailing_data_after_adjustment() {
    let error = parse_bandwidth_argument("1K+1extra").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_adjustments_other_than_one() {
    for text in ["1K+2", "1K-2", "1M+3"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_rejects_incomplete_exponent() {
    for text in ["1e", "1e+", "1E-", "1e "] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_rejects_negative_values() {
    let error = parse_bandwidth_argument("-1M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn bandwidth_parse_error_display_matches_expected_messages() {
    assert_eq!(
        BandwidthParseError::Invalid.to_string(),
        "invalid bandwidth limit syntax"
    );
    assert_eq!(
        BandwidthParseError::TooSmall.to_string(),
        "bandwidth limit is below the minimum of 512 bytes per second"
    );
    assert_eq!(
        BandwidthParseError::TooLarge.to_string(),
        "bandwidth limit exceeds the supported range"
    );
}

#[test]
fn parse_bandwidth_rejects_overflow() {
    let error = parse_bandwidth_argument("999999999999999999999999999999P").unwrap_err();
    assert_eq!(error, BandwidthParseError::TooLarge);
}

#[test]
fn parse_bandwidth_rejects_excessive_exponent() {
    let error = parse_bandwidth_argument("1e2000M").unwrap_err();
    assert_eq!(error, BandwidthParseError::TooLarge);
}

// ==================== Additional edge case tests ====================

#[test]
fn parse_bandwidth_rejects_empty_string() {
    let error = parse_bandwidth_argument("").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_whitespace_only() {
    for text in [" ", "\t", "\n", "  \t\n  "] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

#[test]
fn parse_bandwidth_boundary_minimum_value() {
    // 512 bytes is the minimum allowed
    let at_minimum = parse_bandwidth_argument("512b").expect("parse succeeds");
    assert_eq!(at_minimum, NonZeroU64::new(512));

    // 511 bytes should be rejected
    let below_minimum = parse_bandwidth_argument("511b").unwrap_err();
    assert_eq!(below_minimum, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_boundary_maximum_u64() {
    // Values near u64::MAX should work or fail gracefully
    let large = parse_bandwidth_argument("16383P").expect("parse succeeds");
    assert!(large.is_some());

    // Values that overflow u64 should be rejected
    let overflow = parse_bandwidth_argument("16384P").unwrap_err();
    assert_eq!(overflow, BandwidthParseError::TooLarge);
}

#[test]
fn parse_bandwidth_accepts_mixed_case_suffix() {
    // Binary suffixes (case-insensitive for single letter)
    let lower_k = parse_bandwidth_argument("1k").expect("parse succeeds");
    let upper_k = parse_bandwidth_argument("1K").expect("parse succeeds");
    assert_eq!(lower_k, upper_k);

    let lower_m = parse_bandwidth_argument("1m").expect("parse succeeds");
    let upper_m = parse_bandwidth_argument("1M").expect("parse succeeds");
    assert_eq!(lower_m, upper_m);
}

#[test]
fn parse_bandwidth_accepts_decimal_suffix_case_variations() {
    // Decimal suffixes: KB, Kb, kB, kb should all be 1000-based
    let upper = parse_bandwidth_argument("1KB").expect("parse succeeds");
    let mixed1 = parse_bandwidth_argument("1Kb").expect("parse succeeds");
    let mixed2 = parse_bandwidth_argument("1kB").expect("parse succeeds");
    let lower = parse_bandwidth_argument("1kb").expect("parse succeeds");

    // All should equal 1000 bytes
    let expected = NonZeroU64::new(1000);
    assert_eq!(upper, expected);
    assert_eq!(mixed1, expected);
    assert_eq!(mixed2, expected);
    assert_eq!(lower, expected);
}

#[test]
fn parse_bandwidth_rejects_multiple_decimal_points() {
    let error = parse_bandwidth_argument("1.2.3M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_multiple_commas() {
    let error = parse_bandwidth_argument("1,2,3M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_mixed_decimal_separators() {
    let error = parse_bandwidth_argument("1.2,3M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_handles_very_small_fractions() {
    // Very small fractions: 0.0005M = 524.288 bytes, rounds to 1024 (1K boundary)
    let tiny = parse_bandwidth_argument("0.0005M").expect("parse succeeds");
    assert_eq!(tiny, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_rejects_only_decimal_point() {
    let error = parse_bandwidth_argument(".").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_only_suffix() {
    for text in ["K", "M", "G", "MB", "KiB"] {
        let error = parse_bandwidth_argument(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid, "input: {text}");
    }
}

proptest! {
    #[test]
    fn parse_round_trips_when_limit_is_multiple_of_1024(value in 1u64..1_000_000u64) {
        let text = format!("{value}K");
        let parsed = parse_bandwidth_argument(&text).expect("parse succeeds");
        let expected = NonZeroU64::new(value * 1024).expect("non-zero");
        prop_assert_eq!(parsed, Some(expected));
    }
}

// ==================== Additional coverage tests ====================

#[test]
fn parse_bandwidth_handles_adjustment_when_product_less_than_denominator() {
    // Test the case where product < denominator with -1 adjustment
    // This triggers the bytes = 0 path on line 221
    // 0.0001K = 0.1024 bytes, rounds to 0, with -1 adjustment -> 0 bytes -> unlimited (None)
    // Values below 512 that round to 0 become unlimited
    let result = parse_bandwidth_argument("0.0001K-1");
    // This becomes unlimited (None) because the value rounds to 0
    assert_eq!(result.expect("should parse"), None);
}

#[test]
fn parse_bandwidth_handles_very_small_value_with_minus_one_adjustment() {
    // Another test case for product < denominator with -1 adjust
    // When the value is so small that product < denominator, bytes becomes 0
    // 0.00001M = 0.01048576 bytes -> too small (below 512 minimum)
    // The -1 adjustment happens before the minimum check, so this is TooSmall
    let error = parse_bandwidth_argument("0.00001M-1").unwrap_err();
    assert_eq!(error, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_exponent_sign_at_end_rejected() {
    // Test exponent_sign_allowed still true at end of number
    // This tests line 113-114: if exponent_sign_allowed { return Err }
    // Input like "1e+" where + is the sign but no digits follow
    let error = parse_bandwidth_argument("1e+").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_bandwidth_argument("1e-").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_empty_first_byte_fallthrough() {
    // Test the default case in first byte match (line 55-57)
    // When first character is a digit, we fall through to the _ => {} case
    let result = parse_bandwidth_argument("1K").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_fractional_with_adjustment_edge_cases() {
    // Test adjustment with fractional values near minimum
    // 0.5K = 512 bytes, but gets rounded to 1024 due to 1024-byte alignment
    let at_half = parse_bandwidth_argument("0.5K").expect("parse succeeds");
    assert_eq!(at_half, NonZeroU64::new(1024));

    // 512b = 512 bytes exactly at minimum (byte suffix has 1-byte alignment)
    let at_min = parse_bandwidth_argument("512b").expect("parse succeeds");
    assert_eq!(at_min, NonZeroU64::new(512));

    // 512b-1 = 511 bytes, below minimum
    let below_min = parse_bandwidth_argument("512b-1").unwrap_err();
    assert_eq!(below_min, BandwidthParseError::TooSmall);
}

#[test]
fn parse_bandwidth_handles_all_large_unit_suffixes_with_values() {
    // Ensure all unit suffixes work with various values
    // Test case-insensitivity for all suffixes
    for (suffix_lower, suffix_upper) in [('g', 'G'), ('t', 'T'), ('p', 'P')] {
        let lower = parse_bandwidth_argument(&format!("1{suffix_lower}"));
        let upper = parse_bandwidth_argument(&format!("1{suffix_upper}"));
        assert_eq!(lower, upper, "Case insensitivity failed for {suffix_lower}/{suffix_upper}");
    }
}

#[test]
fn parse_bandwidth_handles_decimal_b_variants() {
    // Test decimal base variants (b suffix after K/M/G etc)
    // GB = 1000^3 bytes
    let gb = parse_bandwidth_argument("1GB").expect("parse succeeds");
    assert_eq!(gb, NonZeroU64::new(1_000_000_000));

    // TB = 1000^4 bytes
    let tb = parse_bandwidth_argument("1TB").expect("parse succeeds");
    assert_eq!(tb, NonZeroU64::new(1_000_000_000_000));
}

#[test]
fn parse_bandwidth_rejects_invalid_iec_middle_char() {
    // Test incomplete IEC suffix where second char is not 'b' or 'B'
    let error = parse_bandwidth_argument("1KiX").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_accepts_all_iec_variants() {
    // Test all IEC suffix variants (KiB, MiB, GiB, TiB, PiB)
    let kib = parse_bandwidth_argument("1KiB").expect("parse succeeds");
    assert_eq!(kib, NonZeroU64::new(1024));

    let mib = parse_bandwidth_argument("1MiB").expect("parse succeeds");
    assert_eq!(mib, NonZeroU64::new(1024 * 1024));

    let gib = parse_bandwidth_argument("1GiB").expect("parse succeeds");
    assert_eq!(gib, NonZeroU64::new(1024u64.pow(3)));

    let tib = parse_bandwidth_argument("1TiB").expect("parse succeeds");
    assert_eq!(tib, NonZeroU64::new(1024u64.pow(4)));
}

#[test]
fn parse_bandwidth_rejects_only_exponent() {
    // Just "e" or "E" without a number
    let error = parse_bandwidth_argument("e").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_bandwidth_argument("E").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_accepts_very_long_fractional_part() {
    // Test with many decimal places
    let result = parse_bandwidth_argument("1.123456789M").expect("parse succeeds");
    assert!(result.is_some());
}

#[test]
fn parse_bandwidth_handles_zero_with_suffix() {
    // Zero with various suffixes should all be unlimited
    assert_eq!(parse_bandwidth_argument("0K").expect("parse"), None);
    assert_eq!(parse_bandwidth_argument("0M").expect("parse"), None);
    assert_eq!(parse_bandwidth_argument("0G").expect("parse"), None);
    assert_eq!(parse_bandwidth_argument("0b").expect("parse"), None);
}

#[test]
fn parse_bandwidth_handles_zero_fractional() {
    // 0.0 should be unlimited
    assert_eq!(parse_bandwidth_argument("0.0M").expect("parse"), None);
    assert_eq!(parse_bandwidth_argument("0.00K").expect("parse"), None);
}

#[test]
fn parse_bandwidth_handles_exponent_with_explicit_positive_sign() {
    // Test e+N format
    let result = parse_bandwidth_argument("1e+3K").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(1024000));
}

#[test]
fn parse_bandwidth_rejects_double_exponent() {
    // Multiple exponents should fail
    let error = parse_bandwidth_argument("1e2e3K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_accepts_exponent_after_trailing_decimal() {
    // "1.e2K" is valid: 1.0 * 10^2 * 1024 = 100 * 1024 = 102400 bytes
    let result = parse_bandwidth_argument("1.e2K").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(102400));
}

#[test]
fn parse_bandwidth_handles_adjustment_boundary_at_minimum() {
    // 513b-1 = 512b, exactly at minimum
    let result = parse_bandwidth_argument("513b-1").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(512));
}

#[test]
fn parse_bandwidth_adjustment_overflow_at_max() {
    // Very large value + adjustment could overflow
    // 16383P is near max, +1 should still work
    let result = parse_bandwidth_argument("16383P");
    assert!(result.is_ok());
}

#[test]
fn parse_bandwidth_rejects_exponent_marker_followed_by_non_digit_non_sign() {
    // Test line 113-114: exponent_sign_allowed is still true when we hit a non-digit/non-sign
    // "1eK" -> e is exponent marker, K is suffix, but no exponent digits
    // This triggers the exponent_sign_allowed check
    let error = parse_bandwidth_argument("1eK").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_with_digit_first_character() {
    // Test line 55-57: first character is a digit (falls through default case)
    let result = parse_bandwidth_argument("5K").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(5120));
}

#[test]
fn parse_bandwidth_with_decimal_first_character() {
    // Another test for line 55-57: first character is a decimal point
    // .5K = 512 bytes, but with 1024-byte alignment, rounds to 1024
    let result = parse_bandwidth_argument(".5K").expect("parse succeeds");
    assert_eq!(result, NonZeroU64::new(1024));
}
