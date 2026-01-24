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

// ==================== Additional coverage tests for numeric parsing ====================

#[test]
fn pow_u128_handles_base_zero_various_exponents() {
    // 0^0 = 1 (mathematical convention in computing)
    assert_eq!(pow_u128(0, 0).unwrap(), 1);

    // 0^n = 0 for n > 0
    assert_eq!(pow_u128(0, 1).unwrap(), 0);
    assert_eq!(pow_u128(0, 10).unwrap(), 0);
}

#[test]
fn pow_u128_handles_base_two_powers() {
    // 2^0 = 1
    assert_eq!(pow_u128(2, 0).unwrap(), 1);
    // 2^1 = 2
    assert_eq!(pow_u128(2, 1).unwrap(), 2);
    // 2^10 = 1024
    assert_eq!(pow_u128(2, 10).unwrap(), 1024);
    // 2^20 = 1048576
    assert_eq!(pow_u128(2, 20).unwrap(), 1_048_576);
}

#[test]
fn pow_u128_handles_odd_and_even_exponents() {
    // Test odd exponent path (exp & 1 == 1)
    assert_eq!(pow_u128(3, 3).unwrap(), 27);
    // Test even exponent path (exp & 1 == 0)
    assert_eq!(pow_u128(3, 4).unwrap(), 81);
    // Test mixed
    assert_eq!(pow_u128(5, 5).unwrap(), 3125);
}

#[test]
fn pow_u128_near_overflow_boundary() {
    // Find a value that's near the overflow boundary
    // 2^127 is the largest power of 2 that fits in u128
    // 2^126 definitely fits
    let result = pow_u128(2, 126);
    assert!(result.is_ok());

    // 2^128 would overflow (u128::MAX is 2^128 - 1)
    // Actually the squaring step will overflow first
    let overflow = pow_u128(2, 200);
    assert!(overflow.is_err());
}

#[test]
fn parse_decimal_with_exponent_handles_large_positive_exponent() {
    // Large exponent values should parse correctly
    let (int, _frac, _denom, exp) = parse_decimal_with_exponent("1e100").unwrap();
    assert_eq!(int, 1);
    assert_eq!(exp, 100);
}

#[test]
fn parse_decimal_with_exponent_handles_large_negative_exponent() {
    let (int, _frac, _denom, exp) = parse_decimal_with_exponent("1e-100").unwrap();
    assert_eq!(int, 1);
    assert_eq!(exp, -100);
}

#[test]
fn parse_decimal_with_exponent_handles_zero_exponent() {
    let (int, _frac, _denom, exp) = parse_decimal_with_exponent("5e0").unwrap();
    assert_eq!(int, 5);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_handles_leading_zeros_in_exponent() {
    // "1e003" should parse as exponent 3
    let (int, _frac, _denom, exp) = parse_decimal_with_exponent("1e003").unwrap();
    assert_eq!(int, 1);
    assert_eq!(exp, 3);
}

#[test]
fn parse_decimal_with_exponent_integer_with_exponent_only() {
    // Integer with no fraction, positive exponent
    let (int, frac, denom, exp) = parse_decimal_with_exponent("42e5").unwrap();
    assert_eq!(int, 42);
    assert_eq!(frac, 0);
    assert_eq!(denom, 1);
    assert_eq!(exp, 5);
}

#[test]
fn parse_decimal_with_exponent_fractional_with_large_denominator() {
    // Many decimal places
    let (int, frac, denom, exp) = parse_decimal_with_exponent("1.123456789").unwrap();
    assert_eq!(int, 1);
    assert_eq!(frac, 123456789);
    assert_eq!(denom, 1_000_000_000);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_zero_integer_with_fraction() {
    // "0.5" - zero integer part
    let (int, frac, denom, exp) = parse_decimal_with_exponent("0.5").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 5);
    assert_eq!(denom, 10);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_zero_fraction() {
    // "5.0" - zero fraction
    let (int, frac, denom, exp) = parse_decimal_with_exponent("5.0").unwrap();
    assert_eq!(int, 5);
    assert_eq!(frac, 0);
    assert_eq!(denom, 10);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_empty_integer_with_comma() {
    // ",5" - empty integer part with comma separator
    let (int, frac, denom, exp) = parse_decimal_with_exponent(",5").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 5);
    assert_eq!(denom, 10);
    assert_eq!(exp, 0);
}
