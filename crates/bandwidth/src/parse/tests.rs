use super::{
    BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit,
    parse_decimal_with_exponent, pow_u128,
};
use crate::limiter::{BandwidthLimiter, LimiterChange};
use proptest::prelude::*;
use std::num::NonZeroU64;

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
fn parse_bandwidth_limit_accepts_burst_component() {
    let components = parse_bandwidth_limit("1M:64K").expect("parse succeeds");
    assert_eq!(
        components,
        BandwidthLimitComponents::new(NonZeroU64::new(1_048_576), NonZeroU64::new(64 * 1024),)
    );
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_accepts_unlimited_rate() {
    let components: BandwidthLimitComponents = "0".parse().expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_limit_rejects_invalid_burst() {
    let error = parse_bandwidth_limit("1M:abc").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_negative_burst() {
    let error = parse_bandwidth_limit("1M:-64K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_zero_rate_disables_burst() {
    let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
    assert!(components.burst().is_none());
}

#[test]
fn components_discard_burst_for_unlimited_rate() {
    let burst = NonZeroU64::new(4096);
    let components = BandwidthLimitComponents::new(None, burst);
    assert!(components.is_unlimited());
    assert_eq!(components.burst(), None);
}

#[test]
fn parse_bandwidth_limit_reports_unlimited_state() {
    let components = parse_bandwidth_limit("0").expect("parse succeeds");
    assert!(components.is_unlimited());
    assert!(components.limit_specified());
    let limited = parse_bandwidth_limit("1M").expect("parse succeeds");
    assert!(!limited.is_unlimited());
    assert!(limited.limit_specified());
}

#[test]
fn bandwidth_limit_components_unlimited_matches_default() {
    let unlimited = BandwidthLimitComponents::unlimited();
    assert!(unlimited.is_unlimited());
    assert_eq!(unlimited, BandwidthLimitComponents::default());
    assert!(!unlimited.burst_specified());
    assert!(!unlimited.limit_specified());
}

#[test]
fn parse_bandwidth_limit_accepts_zero_burst() {
    let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
    assert_eq!(
        components,
        BandwidthLimitComponents::new_with_specified(NonZeroU64::new(1_048_576), None, true,)
    );
    assert!(components.burst_specified());
    assert!(components.limit_specified());
}

#[test]
fn parse_bandwidth_rejects_surrounding_whitespace() {
    let error = parse_bandwidth_argument("\t 2M \n").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_surrounding_whitespace() {
    let error = parse_bandwidth_limit(" 1M ").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn components_into_limiter_respects_rate_and_burst() {
    let components = BandwidthLimitComponents::new(NonZeroU64::new(1024), NonZeroU64::new(4096));
    let limiter = components.into_limiter().expect("limiter");
    assert_eq!(limiter.limit_bytes().get(), 1024);
    assert_eq!(limiter.burst_bytes().map(NonZeroU64::get), Some(4096));
}

#[test]
fn constrained_by_prefers_stricter_limit_and_preserves_burst() {
    let client = parse_bandwidth_limit("8M:64K").expect("client components");
    let module = parse_bandwidth_limit("2M").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(2 * 1024 * 1024));
    assert_eq!(combined.burst().map(NonZeroU64::get), Some(64 * 1024));
    assert!(combined.burst_specified());
    assert!(combined.limit_specified());
}

#[test]
fn constrained_by_initialises_limiter_when_unlimited() {
    let client = BandwidthLimitComponents::unlimited();
    let module = parse_bandwidth_limit("1M:32K").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(1024 * 1024));
    assert_eq!(combined.burst().map(NonZeroU64::get), Some(32 * 1024));
    assert!(combined.burst_specified());
    assert!(combined.limit_specified());
}

#[test]
fn constrained_by_respects_module_disable() {
    let client = parse_bandwidth_limit("4M:48K").expect("client components");
    let module = parse_bandwidth_limit("0").expect("module components");

    let combined = client.constrained_by(&module);

    assert!(combined.is_unlimited());
    assert!(combined.limit_specified());
    assert!(!combined.burst_specified());
    assert!(combined.burst().is_none());
}

#[test]
fn constrained_by_overrides_burst_when_specified() {
    let client = parse_bandwidth_limit("3M:16K").expect("client components");
    let module = parse_bandwidth_limit("5M:0").expect("module components");

    let combined = client.constrained_by(&module);

    assert_eq!(combined.rate().map(NonZeroU64::get), Some(3 * 1024 * 1024));
    assert!(combined.burst().is_none());
    assert!(combined.burst_specified());
}

#[test]
fn components_apply_to_limiter_disables_when_explicitly_unlimited() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024).expect("limit"),
    ));

    let components = BandwidthLimitComponents::new_with_flags(None, None, true, false);
    let change = components.apply_to_limiter(&mut limiter);

    assert_eq!(change, LimiterChange::Disabled);
    assert!(limiter.is_none());
}

#[test]
fn components_apply_to_limiter_updates_burst_with_unlimited_override() {
    let limit = NonZeroU64::new(4 * 1024 * 1024).expect("limit");
    let mut limiter = Some(BandwidthLimiter::new(limit));
    let burst = NonZeroU64::new(256 * 1024).expect("burst");

    let components = BandwidthLimitComponents::new_with_flags(None, Some(burst), false, true);
    let change = components.apply_to_limiter(&mut limiter);

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter should remain active");
    assert_eq!(limiter.limit_bytes(), limit);
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn new_with_flags_retains_unlimited_burst_specification() {
    let burst = NonZeroU64::new(128 * 1024).expect("burst");
    let components = BandwidthLimitComponents::new_with_flags(None, Some(burst), false, true);

    assert!(!components.limit_specified());
    assert!(components.burst_specified());
    assert_eq!(components.burst(), Some(burst));
    assert!(components.is_unlimited());
}

#[test]
fn new_with_flags_forces_limit_specified_when_rate_present() {
    let limit = NonZeroU64::new(2048).expect("limit");
    let components = BandwidthLimitComponents::new_with_flags(Some(limit), None, false, false);

    assert!(components.limit_specified());
    assert_eq!(components.rate(), Some(limit));
    assert!(components.burst().is_none());
}

#[test]
fn new_with_flags_tracks_explicit_burst_clearing_with_rate() {
    let limit = NonZeroU64::new(4096).expect("limit");
    let components = BandwidthLimitComponents::new_with_flags(Some(limit), None, false, true);

    assert!(components.limit_specified());
    assert!(components.burst_specified());
    assert!(components.burst().is_none());
}

#[test]
fn components_into_limiter_returns_none_when_unlimited() {
    let components = BandwidthLimitComponents::new(None, NonZeroU64::new(4096));
    assert!(components.into_limiter().is_none());
}

#[test]
fn parse_bandwidth_limit_records_explicit_burst_presence() {
    let specified = parse_bandwidth_limit("2M:128K").expect("parse succeeds");
    assert!(specified.burst_specified());
    assert_eq!(specified.burst(), NonZeroU64::new(128 * 1024));

    let unspecified = parse_bandwidth_limit("2M").expect("parse succeeds");
    assert!(!unspecified.burst_specified());
    assert!(unspecified.burst().is_none());
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
fn parse_bandwidth_limit_rejects_whitespace_around_burst_separator() {
    let error = parse_bandwidth_limit("4M : 256K").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_rejects_trailing_garbage() {
    let error = parse_bandwidth_limit("1M extra").unwrap_err();
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

#[test]
fn parse_bandwidth_limit_rejects_missing_burst_value() {
    let error = parse_bandwidth_limit("1M:").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
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

#[test]
fn pow_u128_matches_checked_pow_for_supported_inputs() {
    let base = 1024u32;
    for exponent in 0..=5u32 {
        let expected = u128::from(base).checked_pow(exponent).expect("no overflow");
        assert_eq!(
            pow_u128(base, exponent).expect("computation succeeds"),
            expected
        );
    }
}

#[test]
fn pow_u128_reports_overflow() {
    let overflow = pow_u128(u32::MAX, 5);
    assert_eq!(overflow, Err(BandwidthParseError::TooLarge));
}

#[test]
fn parse_decimal_with_exponent_parses_integer_and_fraction_components() {
    let (integer, fraction, denominator, exponent) =
        parse_decimal_with_exponent("123.45").expect("parse succeeds");

    assert_eq!(integer, 123);
    assert_eq!(fraction, 45);
    assert_eq!(denominator, 100);
    assert_eq!(exponent, 0);
}

#[test]
fn parse_decimal_with_exponent_accepts_comma_separator_and_scientific_notation() {
    let (integer, fraction, denominator, exponent) =
        parse_decimal_with_exponent("7,89e3").expect("parse succeeds");

    assert_eq!(integer, 7);
    assert_eq!(fraction, 89);
    assert_eq!(denominator, 100);
    assert_eq!(exponent, 3);
}

#[test]
fn parse_decimal_with_exponent_supports_negative_exponents() {
    let (_, _, _, exponent) = parse_decimal_with_exponent("10E-2").expect("parse succeeds");
    assert_eq!(exponent, -2);
}

#[test]
fn parse_decimal_with_exponent_rejects_repeated_decimal_markers() {
    let error = parse_decimal_with_exponent("1.2.3").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_decimal_with_exponent_rejects_missing_exponent_digits() {
    for text in ["10e", "5E+", "2e-"] {
        let error = parse_decimal_with_exponent(text).unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }
}
