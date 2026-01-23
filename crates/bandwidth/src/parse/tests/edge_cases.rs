//! Comprehensive edge case tests for bandwidth parsing.
//!
//! This module tests all edge cases for the bandwidth parsing logic:
//! - Size suffixes (K, M, G, T, P with 1024-based multipliers)
//! - Rate formats including optional /s suffix
//! - Edge cases: zero, negative, overflow, empty strings
//! - Invalid format handling and error messages
//! - Whitespace handling
//! - Case sensitivity for suffixes

use super::{BandwidthParseError, NonZeroU64, parse_bandwidth_argument, parse_bandwidth_limit};

// ==================== Size Suffix Tests (1024-based like rsync) ====================

mod size_suffixes {
    use super::*;

    #[test]
    fn kilobyte_suffix_uses_1024_multiplier() {
        // K suffix should multiply by 1024 (binary kilobyte)
        let result = parse_bandwidth_argument("1K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));

        let result = parse_bandwidth_argument("10K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(10 * 1024));

        let result = parse_bandwidth_argument("100K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(100 * 1024));
    }

    #[test]
    fn megabyte_suffix_uses_1024_squared_multiplier() {
        // M suffix should multiply by 1024^2 (binary megabyte)
        let result = parse_bandwidth_argument("1M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024 * 1024));

        let result = parse_bandwidth_argument("5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(5 * 1024 * 1024));
    }

    #[test]
    fn gigabyte_suffix_uses_1024_cubed_multiplier() {
        // G suffix should multiply by 1024^3 (binary gigabyte)
        let result = parse_bandwidth_argument("1G").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024u64.pow(3)));

        let result = parse_bandwidth_argument("2G").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(2 * 1024u64.pow(3)));
    }

    #[test]
    fn terabyte_suffix_uses_1024_to_fourth_multiplier() {
        // T suffix should multiply by 1024^4 (binary terabyte)
        let result = parse_bandwidth_argument("1T").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024u64.pow(4)));
    }

    #[test]
    fn petabyte_suffix_uses_1024_to_fifth_multiplier() {
        // P suffix should multiply by 1024^5 (binary petabyte)
        let result = parse_bandwidth_argument("1P").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024u64.pow(5)));
    }

    #[test]
    fn byte_suffix_uses_no_multiplier() {
        // B/b suffix means raw bytes (no multiplier)
        let result = parse_bandwidth_argument("1024b").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));

        let result = parse_bandwidth_argument("512B").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512));
    }

    #[test]
    fn decimal_suffixes_use_1000_multiplier() {
        // KB/MB/GB suffixes use 1000-based (decimal) multipliers
        let kb = parse_bandwidth_argument("1KB").expect("parse succeeds");
        assert_eq!(kb, NonZeroU64::new(1000));

        let mb = parse_bandwidth_argument("1MB").expect("parse succeeds");
        assert_eq!(mb, NonZeroU64::new(1_000_000));

        let gb = parse_bandwidth_argument("1GB").expect("parse succeeds");
        assert_eq!(gb, NonZeroU64::new(1_000_000_000));
    }

    #[test]
    fn iec_suffixes_use_1024_multiplier() {
        // KiB/MiB/GiB suffixes explicitly use 1024-based (IEC) multipliers
        let kib = parse_bandwidth_argument("1KiB").expect("parse succeeds");
        assert_eq!(kib, NonZeroU64::new(1024));

        let mib = parse_bandwidth_argument("1MiB").expect("parse succeeds");
        assert_eq!(mib, NonZeroU64::new(1024 * 1024));

        let gib = parse_bandwidth_argument("1GiB").expect("parse succeeds");
        assert_eq!(gib, NonZeroU64::new(1024u64.pow(3)));
    }

    #[test]
    fn no_suffix_defaults_to_kilobytes() {
        // Without a suffix, rsync interprets the value as kilobytes
        let result = parse_bandwidth_argument("1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));

        let result = parse_bandwidth_argument("100").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(100 * 1024));
    }
}

// ==================== Rate Format Tests ====================

mod rate_formats {
    use super::*;

    #[test]
    fn simple_rate_formats() {
        // Standard rate formats
        let result = parse_bandwidth_argument("100K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(100 * 1024));

        let result = parse_bandwidth_argument("1M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024 * 1024));

        let result = parse_bandwidth_argument("500K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(500 * 1024));
    }

    #[test]
    fn fractional_rate_formats() {
        // Fractional values with suffixes
        let result = parse_bandwidth_argument("0.5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512 * 1024));

        // 1.5K = 1536 bytes, but rounds to 2048 due to 1024-byte alignment
        let result = parse_bandwidth_argument("1.5K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(2048));

        let result = parse_bandwidth_argument("2.25M").expect("parse succeeds");
        // 2.25 * 1024 * 1024 = 2359296
        assert_eq!(result, NonZeroU64::new(2359296));
    }

    #[test]
    fn rate_with_adjustment_suffix() {
        // rsync supports +1/-1 adjustments for fine-grained control
        let result = parse_bandwidth_argument("1K+1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024)); // Rounded, adjustment applied

        let result = parse_bandwidth_argument("1K-1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024)); // Rounded, adjustment applied

        // Byte suffix allows exact adjustment
        let result = parse_bandwidth_argument("600b+1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(601));

        let result = parse_bandwidth_argument("600b-1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(599));
    }

    #[test]
    fn rate_with_burst_component() {
        // Rate with burst: "rate:burst" format
        let components = parse_bandwidth_limit("1M:64K").expect("parse succeeds");
        assert_eq!(components.rate(), NonZeroU64::new(1024 * 1024));
        assert_eq!(components.burst(), NonZeroU64::new(64 * 1024));
        assert!(components.burst_specified());
    }

    #[test]
    fn rate_without_burst_component() {
        // Rate without burst
        let components = parse_bandwidth_limit("1M").expect("parse succeeds");
        assert_eq!(components.rate(), NonZeroU64::new(1024 * 1024));
        assert!(components.burst().is_none());
        assert!(!components.burst_specified());
    }
}

// ==================== Zero Value Tests ====================

mod zero_value {
    use super::*;

    #[test]
    fn zero_means_unlimited() {
        // Zero disables bandwidth limiting
        let result = parse_bandwidth_argument("0").expect("parse succeeds");
        assert!(result.is_none());
    }

    #[test]
    fn zero_with_suffix_means_unlimited() {
        let result = parse_bandwidth_argument("0K").expect("parse succeeds");
        assert!(result.is_none());

        let result = parse_bandwidth_argument("0M").expect("parse succeeds");
        assert!(result.is_none());

        let result = parse_bandwidth_argument("0b").expect("parse succeeds");
        assert!(result.is_none());
    }

    #[test]
    fn zero_in_limit_component_marks_as_specified() {
        let components = parse_bandwidth_limit("0").expect("parse succeeds");
        assert!(components.is_unlimited());
        assert!(components.limit_specified());
    }

    #[test]
    fn zero_rate_ignores_burst() {
        // When rate is zero, burst is ignored
        let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
        assert!(components.is_unlimited());
        assert!(components.burst().is_none());
        assert!(components.limit_specified());
    }

    #[test]
    fn zero_burst_is_valid() {
        // Zero burst with valid rate
        let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
        assert_eq!(components.rate(), NonZeroU64::new(1024 * 1024));
        assert!(components.burst().is_none());
        assert!(components.burst_specified());
    }

    #[test]
    fn zero_point_zero_means_unlimited() {
        let result = parse_bandwidth_argument("0.0").expect("parse succeeds");
        assert!(result.is_none());

        let result = parse_bandwidth_argument("0.0M").expect("parse succeeds");
        assert!(result.is_none());
    }
}

// ==================== Negative Value Tests ====================

mod negative_values {
    use super::*;

    #[test]
    fn negative_values_are_rejected() {
        let error = parse_bandwidth_argument("-1").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("-1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("-100K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn negative_zero_is_rejected() {
        let error = parse_bandwidth_argument("-0").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn negative_fractional_values_are_rejected() {
        let error = parse_bandwidth_argument("-0.5M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn negative_burst_is_rejected() {
        let error = parse_bandwidth_limit("1M:-64K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn negative_exponent_is_allowed() {
        // Negative exponent is valid (scientific notation)
        let result = parse_bandwidth_argument("1e-1M").expect("parse succeeds");
        assert!(result.is_some());
    }
}

// ==================== Overflow Tests ====================

mod overflow {
    use super::*;

    #[test]
    fn very_large_values_overflow() {
        let error = parse_bandwidth_argument("999999999999999999999999999P").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }

    #[test]
    fn excessive_exponent_overflows() {
        let error = parse_bandwidth_argument("1e2000M").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }

    #[test]
    fn maximum_valid_petabytes() {
        // 16383P is near the u64 maximum
        let result = parse_bandwidth_argument("16383P").expect("parse succeeds");
        assert!(result.is_some());
    }

    #[test]
    fn overflow_at_petabyte_boundary() {
        // 16384P overflows u64
        let error = parse_bandwidth_argument("16384P").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }

    #[test]
    fn large_multiplications_overflow() {
        // Large base number with large suffix
        let error = parse_bandwidth_argument("9999999999999999999999G").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }

    #[test]
    fn repeated_large_digits_overflow() {
        let huge = "9".repeat(50);
        let error = parse_bandwidth_argument(&huge).unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }
}

// ==================== Empty and Whitespace Tests ====================

mod empty_and_whitespace {
    use super::*;

    #[test]
    fn empty_string_is_rejected() {
        let error = parse_bandwidth_argument("").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn whitespace_only_is_rejected() {
        let error = parse_bandwidth_argument(" ").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("\t").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("\n").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("  \t\n  ").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn leading_whitespace_is_rejected() {
        let error = parse_bandwidth_argument(" 1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("\t1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn trailing_whitespace_is_rejected() {
        let error = parse_bandwidth_argument("1M ").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1M\n").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn surrounding_whitespace_is_rejected() {
        let error = parse_bandwidth_argument(" 1M ").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("\t 2M \n").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn internal_whitespace_is_rejected() {
        let error = parse_bandwidth_argument("1 M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1\tM").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("100 K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn whitespace_around_burst_separator_is_rejected() {
        let error = parse_bandwidth_limit("1M : 64K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_limit("1M :64K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_limit("1M: 64K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn whitespace_around_adjustment_is_rejected() {
        let error = parse_bandwidth_argument("1K +1").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1K+ 1").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1K - 1").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn limit_with_surrounding_whitespace_is_rejected() {
        let error = parse_bandwidth_limit(" 1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_limit("1M ").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

// ==================== Invalid Format Tests ====================

mod invalid_formats {
    use super::*;

    #[test]
    fn only_suffix_is_rejected() {
        let error = parse_bandwidth_argument("K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("G").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("MB").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("KiB").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn invalid_suffix_is_rejected() {
        let error = parse_bandwidth_argument("10Q").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("10X").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("10Z").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn incomplete_iec_suffix_is_rejected() {
        let error = parse_bandwidth_argument("1Ki").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1Mi").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1Gi").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn multiple_decimal_points_rejected() {
        let error = parse_bandwidth_argument("1.2.3M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn multiple_commas_rejected() {
        let error = parse_bandwidth_argument("1,2,3M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn mixed_decimal_separators_rejected() {
        let error = parse_bandwidth_argument("1.2,3M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1,2.3M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn only_decimal_point_rejected() {
        let error = parse_bandwidth_argument(".").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument(",").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn incomplete_exponent_rejected() {
        let error = parse_bandwidth_argument("1e").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1E").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1e+").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1e-").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn numeric_separators_rejected() {
        let error = parse_bandwidth_argument("1_000K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("2M_").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("_1K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn non_ascii_characters_rejected() {
        let error = parse_bandwidth_argument("10\u{00B5}").unwrap_err(); // micro sign
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("\u{FF11}\u{FF12}M").unwrap_err(); // fullwidth digits
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn missing_burst_value_rejected() {
        let error = parse_bandwidth_limit("1M:").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn invalid_burst_rejected() {
        let error = parse_bandwidth_limit("1M:abc").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn trailing_garbage_rejected() {
        let error = parse_bandwidth_argument("1M extra").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_limit("1M extra").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn adjustment_values_other_than_one_rejected() {
        let error = parse_bandwidth_argument("1K+2").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1K-2").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1M+10").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn trailing_data_after_adjustment_rejected() {
        let error = parse_bandwidth_argument("1K+1extra").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("1K-1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn sign_only_rejected() {
        let error = parse_bandwidth_argument("+").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);

        let error = parse_bandwidth_argument("-").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}

// ==================== Case Sensitivity Tests ====================

mod case_sensitivity {
    use super::*;

    #[test]
    fn single_letter_suffix_case_insensitive() {
        // K/k should be equivalent
        let lower = parse_bandwidth_argument("1k").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1K").expect("parse succeeds");
        assert_eq!(lower, upper);
        assert_eq!(lower, NonZeroU64::new(1024));
    }

    #[test]
    fn megabyte_suffix_case_insensitive() {
        let lower = parse_bandwidth_argument("1m").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1M").expect("parse succeeds");
        assert_eq!(lower, upper);
        assert_eq!(lower, NonZeroU64::new(1024 * 1024));
    }

    #[test]
    fn gigabyte_suffix_case_insensitive() {
        let lower = parse_bandwidth_argument("1g").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1G").expect("parse succeeds");
        assert_eq!(lower, upper);
    }

    #[test]
    fn terabyte_suffix_case_insensitive() {
        let lower = parse_bandwidth_argument("1t").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1T").expect("parse succeeds");
        assert_eq!(lower, upper);
    }

    #[test]
    fn petabyte_suffix_case_insensitive() {
        let lower = parse_bandwidth_argument("1p").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1P").expect("parse succeeds");
        assert_eq!(lower, upper);
    }

    #[test]
    fn byte_suffix_case_insensitive() {
        let lower = parse_bandwidth_argument("512b").expect("parse succeeds");
        let upper = parse_bandwidth_argument("512B").expect("parse succeeds");
        assert_eq!(lower, upper);
        assert_eq!(lower, NonZeroU64::new(512));
    }

    #[test]
    fn decimal_suffix_all_case_variations() {
        // KB, Kb, kB, kb all work
        let kb_upper = parse_bandwidth_argument("1KB").expect("parse succeeds");
        let kb_mixed1 = parse_bandwidth_argument("1Kb").expect("parse succeeds");
        let kb_mixed2 = parse_bandwidth_argument("1kB").expect("parse succeeds");
        let kb_lower = parse_bandwidth_argument("1kb").expect("parse succeeds");

        assert_eq!(kb_upper, NonZeroU64::new(1000));
        assert_eq!(kb_mixed1, NonZeroU64::new(1000));
        assert_eq!(kb_mixed2, NonZeroU64::new(1000));
        assert_eq!(kb_lower, NonZeroU64::new(1000));
    }

    #[test]
    fn iec_suffix_case_variations() {
        // KiB, Kib, kIB, kib, etc.
        let upper = parse_bandwidth_argument("1KiB").expect("parse succeeds");
        let mixed = parse_bandwidth_argument("1kIb").expect("parse succeeds");
        let lower = parse_bandwidth_argument("1kib").expect("parse succeeds");

        assert_eq!(upper, NonZeroU64::new(1024));
        assert_eq!(mixed, NonZeroU64::new(1024));
        assert_eq!(lower, NonZeroU64::new(1024));
    }

    #[test]
    fn exponent_notation_case_insensitive() {
        let lower = parse_bandwidth_argument("1e3").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1E3").expect("parse succeeds");
        assert_eq!(lower, upper);
    }
}

// ==================== Error Message Tests ====================

mod error_messages {
    use super::*;

    #[test]
    fn invalid_error_has_correct_message() {
        assert_eq!(
            BandwidthParseError::Invalid.to_string(),
            "invalid bandwidth limit syntax"
        );
    }

    #[test]
    fn too_small_error_has_correct_message() {
        assert_eq!(
            BandwidthParseError::TooSmall.to_string(),
            "bandwidth limit is below the minimum of 512 bytes per second"
        );
    }

    #[test]
    fn too_large_error_has_correct_message() {
        assert_eq!(
            BandwidthParseError::TooLarge.to_string(),
            "bandwidth limit exceeds the supported range"
        );
    }

    #[test]
    fn errors_are_eq_comparable() {
        assert_eq!(BandwidthParseError::Invalid, BandwidthParseError::Invalid);
        assert_ne!(BandwidthParseError::Invalid, BandwidthParseError::TooSmall);
        assert_ne!(BandwidthParseError::TooSmall, BandwidthParseError::TooLarge);
    }

    #[test]
    fn errors_are_debug_printable() {
        let debug = format!("{:?}", BandwidthParseError::Invalid);
        assert!(debug.contains("Invalid"));

        let debug = format!("{:?}", BandwidthParseError::TooSmall);
        assert!(debug.contains("TooSmall"));

        let debug = format!("{:?}", BandwidthParseError::TooLarge);
        assert!(debug.contains("TooLarge"));
    }
}

// ==================== Minimum Value Tests ====================

mod minimum_values {
    use super::*;

    #[test]
    fn minimum_512_bytes_is_valid() {
        let result = parse_bandwidth_argument("512b").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512));
    }

    #[test]
    fn below_minimum_511_bytes_is_rejected() {
        let error = parse_bandwidth_argument("511b").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }

    #[test]
    fn small_fractional_values_rejected() {
        // 0.25K = 256 bytes < 512 minimum
        let error = parse_bandwidth_argument("0.25K").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }

    #[test]
    fn very_small_byte_values_rejected() {
        let error = parse_bandwidth_argument("10b").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);

        let error = parse_bandwidth_argument("100b").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }

    #[test]
    fn negative_adjustment_can_trigger_too_small() {
        // 0.0001M = ~104 bytes, -1 makes it even smaller
        let error = parse_bandwidth_argument("0.0001M-1").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooSmall);
    }
}

// ==================== Scientific Notation Tests ====================

mod scientific_notation {
    use super::*;

    #[test]
    fn basic_positive_exponent() {
        // 1e3 = 1000, default unit is K, so 1000K = 1,024,000 bytes
        let result = parse_bandwidth_argument("1e3").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1_024_000));
    }

    #[test]
    fn exponent_with_suffix() {
        // 1e3b = 1000 bytes
        let result = parse_bandwidth_argument("1e3b").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1000));
    }

    #[test]
    fn fractional_with_exponent() {
        // 2.5e2K = 250K = 256,000 bytes
        let result = parse_bandwidth_argument("2.5e2K").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(256_000));
    }

    #[test]
    fn negative_exponent() {
        // 1e-1M = 0.1M = ~104,857 bytes
        let result = parse_bandwidth_argument("1e-1M").expect("parse succeeds");
        assert!(result.is_some());
    }

    #[test]
    fn explicit_positive_exponent_sign() {
        // 1e+3 should work the same as 1e3
        let with_sign = parse_bandwidth_argument("1e+3").expect("parse succeeds");
        let without_sign = parse_bandwidth_argument("1e3").expect("parse succeeds");
        assert_eq!(with_sign, without_sign);
    }

    #[test]
    fn decimal_with_exponent_and_suffix() {
        // 1e3MB = 1000MB = 1,000,000,000 bytes
        let result = parse_bandwidth_argument("1e3MB").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1_000_000_000));
    }

    #[test]
    fn uppercase_e_in_exponent() {
        let lower = parse_bandwidth_argument("1e3").expect("parse succeeds");
        let upper = parse_bandwidth_argument("1E3").expect("parse succeeds");
        assert_eq!(lower, upper);
    }
}

// ==================== Decimal Separator Tests ====================

mod decimal_separators {
    use super::*;

    #[test]
    fn period_as_decimal_separator() {
        let result = parse_bandwidth_argument("0.5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn comma_as_decimal_separator() {
        let result = parse_bandwidth_argument("0,5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn leading_decimal_without_integer() {
        // .5M = 0.5M
        let result = parse_bandwidth_argument(".5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512 * 1024));
    }

    #[test]
    fn trailing_decimal_without_fraction() {
        // 1. = 1.0
        let result = parse_bandwidth_argument("1.").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));
    }

    #[test]
    fn many_decimal_places() {
        // Test precision handling
        let result = parse_bandwidth_argument("1.123456789M").expect("parse succeeds");
        assert!(result.is_some());
    }
}

// ==================== Leading Sign Tests ====================

mod leading_signs {
    use super::*;

    #[test]
    fn leading_plus_is_accepted() {
        let result = parse_bandwidth_argument("+1M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1_048_576));
    }

    #[test]
    fn leading_minus_is_rejected() {
        let error = parse_bandwidth_argument("-1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn plus_zero_is_unlimited() {
        let result = parse_bandwidth_argument("+0").expect("parse succeeds");
        assert!(result.is_none());
    }

    #[test]
    fn plus_with_fractional() {
        let result = parse_bandwidth_argument("+0.5M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(512 * 1024));
    }
}

// ==================== Rounding Behavior Tests ====================

mod rounding {
    use super::*;

    #[test]
    fn values_are_rounded_to_alignment() {
        // Values get rounded to nearest 1024 for K/M/G suffixes
        // 0.0005M = ~524 bytes, rounds up to 1024
        let result = parse_bandwidth_argument("0.0005M").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));
    }

    #[test]
    fn byte_suffix_no_rounding() {
        // Byte suffix values are not rounded
        let result = parse_bandwidth_argument("513b").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(513));
    }

    #[test]
    fn adjustment_applied_after_rounding() {
        // For non-byte suffixes, adjustment has limited effect due to rounding
        let without_adj = parse_bandwidth_argument("1K").expect("parse succeeds");
        let with_adj = parse_bandwidth_argument("1K+1").expect("parse succeeds");
        // Both round to 1024
        assert_eq!(without_adj, with_adj);
    }
}

// ==================== Burst Component Tests ====================

mod burst_component {
    use super::*;

    #[test]
    fn burst_component_with_various_suffixes() {
        let components = parse_bandwidth_limit("1M:32K").expect("parse succeeds");
        assert_eq!(components.burst(), NonZeroU64::new(32 * 1024));

        let components = parse_bandwidth_limit("1G:1M").expect("parse succeeds");
        assert_eq!(components.burst(), NonZeroU64::new(1024 * 1024));

        let components = parse_bandwidth_limit("100K:8K").expect("parse succeeds");
        assert_eq!(components.burst(), NonZeroU64::new(8 * 1024));
    }

    #[test]
    fn burst_with_decimal_values() {
        // 0.5K = 512 bytes, but rounds to 1024 due to alignment
        let components = parse_bandwidth_limit("1M:0.5K").expect("parse succeeds");
        assert_eq!(components.burst(), NonZeroU64::new(1024));
    }

    #[test]
    fn multiple_colons_rejected() {
        // Only one colon allowed
        let error = parse_bandwidth_limit("1M:64K:32K").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn burst_inherits_limit_specified() {
        let components = parse_bandwidth_limit("1M:64K").expect("parse succeeds");
        assert!(components.limit_specified());
        assert!(components.burst_specified());
    }
}

// ==================== Default Unit Behavior Tests ====================

mod default_unit {
    use super::*;

    #[test]
    fn bare_number_defaults_to_kilobytes() {
        // rsync interprets bare numbers as kilobytes
        let result = parse_bandwidth_argument("1").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));

        let result = parse_bandwidth_argument("100").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(100 * 1024));

        let result = parse_bandwidth_argument("1000").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1000 * 1024));
    }

    #[test]
    fn bare_fractional_defaults_to_kilobytes() {
        // 0.5 = 0.5K = 512 bytes, rounds to 1024 due to alignment
        let result = parse_bandwidth_argument("0.5").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(1024));

        // 1.5K = 1536 bytes, rounds to 2048 due to alignment
        let result = parse_bandwidth_argument("1.5").expect("parse succeeds");
        assert_eq!(result, NonZeroU64::new(2048));
    }

    #[test]
    fn bare_number_with_adjustment_defaults_to_kilobytes() {
        // Adjustment applies after kilobyte interpretation
        let result = parse_bandwidth_argument("1+1").expect("parse succeeds");
        // 1K = 1024, +1 = 1025, but rounds to 1024
        assert_eq!(result, NonZeroU64::new(1024));
    }
}
