use super::{BandwidthParseError, parse_decimal_with_exponent, pow_u128};

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
