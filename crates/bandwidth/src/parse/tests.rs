use super::{
    BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit,
    pow_u128,
};
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
    assert_eq!(limit, NonZeroU64::new(12_000_256));
}

#[test]
fn parse_bandwidth_accepts_space_between_value_and_suffix() {
    let limit = parse_bandwidth_argument("1 M").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1_048_576));
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
}

#[test]
fn parse_bandwidth_limit_accepts_unlimited_rate() {
    let components: BandwidthLimitComponents = "0".parse().expect("parse succeeds");
    assert!(components.is_unlimited());
}

#[test]
fn parse_bandwidth_limit_rejects_invalid_burst() {
    let error = parse_bandwidth_limit("1M:abc").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_limit_zero_rate_disables_burst() {
    let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
    assert_eq!(components, BandwidthLimitComponents::unlimited());
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
    let limited = parse_bandwidth_limit("1M").expect("parse succeeds");
    assert!(!limited.is_unlimited());
}

#[test]
fn bandwidth_limit_components_unlimited_matches_default() {
    let unlimited = BandwidthLimitComponents::unlimited();
    assert!(unlimited.is_unlimited());
    assert_eq!(unlimited, BandwidthLimitComponents::default());
    assert!(!unlimited.burst_specified());
}

#[test]
fn parse_bandwidth_limit_accepts_zero_burst() {
    let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
    assert_eq!(
        components,
        BandwidthLimitComponents::new_with_specified(NonZeroU64::new(1_048_576), None, true,)
    );
    assert!(components.burst_specified());
}

#[test]
fn parse_bandwidth_trims_surrounding_whitespace() {
    let limit = parse_bandwidth_argument("\t 2M \n").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(2_097_152));
}

#[test]
fn components_into_limiter_respects_rate_and_burst() {
    let components = BandwidthLimitComponents::new(NonZeroU64::new(1024), NonZeroU64::new(4096));
    let limiter = components.into_limiter().expect("limiter");
    assert_eq!(limiter.limit_bytes().get(), 1024);
    assert_eq!(limiter.burst_bytes().map(NonZeroU64::get), Some(4096));
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
fn parse_bandwidth_accepts_whitespace_before_adjustment() {
    let limit = parse_bandwidth_argument("1K +1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_accepts_whitespace_within_adjustment() {
    let limit = parse_bandwidth_argument("1K+ 1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));

    let limit = parse_bandwidth_argument("1K + 1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));

    let limit = parse_bandwidth_argument("1K- 1").expect("parse succeeds");
    assert_eq!(limit, NonZeroU64::new(1024));
}

#[test]
fn parse_bandwidth_rejects_non_unit_adjustment_value() {
    let error = parse_bandwidth_argument("1K+ 2").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_multiple_decimal_separators() {
    let error = parse_bandwidth_argument("1.2.3").unwrap_err();
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
fn parse_bandwidth_rejects_negative_values() {
    let error = parse_bandwidth_argument("-1M").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_bandwidth_rejects_overflow() {
    let error = parse_bandwidth_argument("999999999999999999999999999999P").unwrap_err();
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
        let text = format!("{}K", value);
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
