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

proptest! {
    #[test]
    fn parse_round_trips_when_limit_is_multiple_of_1024(value in 1u64..1_000_000u64) {
        let text = format!("{value}K");
        let parsed = parse_bandwidth_argument(&text).expect("parse succeeds");
        let expected = NonZeroU64::new(value * 1024).expect("non-zero");
        prop_assert_eq!(parsed, Some(expected));
    }
}
