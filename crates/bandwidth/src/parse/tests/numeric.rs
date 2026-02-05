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

// ==================== Additional pow_u128 edge cases ====================

#[test]
fn pow_u128_base_one_any_exponent() {
    // 1^n = 1 for all n
    for exp in [0, 1, 10, 100, 1000] {
        assert_eq!(pow_u128(1, exp).unwrap(), 1);
    }
}

#[test]
fn pow_u128_exponent_one_any_base() {
    // n^1 = n for all n
    for base in [0, 1, 10, 100, 1000, u32::MAX] {
        assert_eq!(pow_u128(base, 1).unwrap(), u128::from(base));
    }
}

#[test]
fn pow_u128_large_base_small_exponent() {
    // Large base with small exponent should work
    assert_eq!(pow_u128(u32::MAX, 1).unwrap(), u128::from(u32::MAX));
    assert_eq!(
        pow_u128(u32::MAX, 2).unwrap(),
        u128::from(u32::MAX) * u128::from(u32::MAX)
    );
}

#[test]
fn pow_u128_overflow_at_specific_boundary() {
    // Find boundary where overflow occurs
    // 2^127 fits, 2^128 would be > u128::MAX
    assert!(pow_u128(2, 127).is_ok());
}

#[test]
fn pow_u128_ten_powers() {
    // Common use case: powers of 10
    assert_eq!(pow_u128(10, 0).unwrap(), 1);
    assert_eq!(pow_u128(10, 1).unwrap(), 10);
    assert_eq!(pow_u128(10, 2).unwrap(), 100);
    assert_eq!(pow_u128(10, 9).unwrap(), 1_000_000_000);
    assert_eq!(pow_u128(10, 18).unwrap(), 1_000_000_000_000_000_000);
    assert_eq!(
        pow_u128(10, 38).unwrap(),
        100_000_000_000_000_000_000_000_000_000_000_000_000
    );
}

#[test]
fn pow_u128_overflow_with_ten() {
    // 10^39 would overflow u128
    let result = pow_u128(10, 39);
    assert!(result.is_err());
}

// ==================== parse_decimal_with_exponent comprehensive tests ====================

#[test]
fn parse_decimal_with_exponent_pure_integer() {
    let (int, frac, denom, exp) = parse_decimal_with_exponent("42").unwrap();
    assert_eq!(int, 42);
    assert_eq!(frac, 0);
    assert_eq!(denom, 1);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_pure_fraction() {
    // ".123" - just a fraction
    let (int, frac, denom, exp) = parse_decimal_with_exponent(".123").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 123);
    assert_eq!(denom, 1000);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_trailing_decimal() {
    // "42." - integer with trailing decimal
    let (int, frac, denom, exp) = parse_decimal_with_exponent("42.").unwrap();
    assert_eq!(int, 42);
    assert_eq!(frac, 0);
    assert_eq!(denom, 1); // No fractional digits
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_leading_zeros_in_fraction() {
    // "1.0025" - leading zeros in fraction
    let (int, frac, denom, exp) = parse_decimal_with_exponent("1.0025").unwrap();
    assert_eq!(int, 1);
    assert_eq!(frac, 25);
    assert_eq!(denom, 10000);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_large_integer_part() {
    // Large integer that fits in u128
    let (int, frac, denom, exp) = parse_decimal_with_exponent("123456789012345").unwrap();
    assert_eq!(int, 123_456_789_012_345);
    assert_eq!(frac, 0);
    assert_eq!(denom, 1);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_both_parts_large() {
    let (int, frac, denom, exp) = parse_decimal_with_exponent("999999.999999").unwrap();
    assert_eq!(int, 999999);
    assert_eq!(frac, 999999);
    assert_eq!(denom, 1_000_000);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_exponent_boundary_values() {
    // Maximum reasonable positive exponent
    let (_, _, _, exp) = parse_decimal_with_exponent("1e999").unwrap();
    assert_eq!(exp, 999);

    // Maximum reasonable negative exponent
    let (_, _, _, exp) = parse_decimal_with_exponent("1e-999").unwrap();
    assert_eq!(exp, -999);
}

#[test]
fn parse_decimal_with_exponent_combined_complex() {
    // Complex: fraction + positive exponent
    let (int, frac, denom, exp) = parse_decimal_with_exponent("1.5e10").unwrap();
    assert_eq!(int, 1);
    assert_eq!(frac, 5);
    assert_eq!(denom, 10);
    assert_eq!(exp, 10);

    // Complex: fraction + negative exponent
    let (int, frac, denom, exp) = parse_decimal_with_exponent("2.75e-5").unwrap();
    assert_eq!(int, 2);
    assert_eq!(frac, 75);
    assert_eq!(denom, 100);
    assert_eq!(exp, -5);
}

#[test]
fn parse_decimal_with_exponent_explicit_positive_sign() {
    // "1e+5" - explicit positive sign in exponent
    let (int, _frac, _denom, exp) = parse_decimal_with_exponent("1e+5").unwrap();
    assert_eq!(int, 1);
    assert_eq!(exp, 5);
}

#[test]
fn parse_decimal_with_exponent_zero_value() {
    let (int, frac, denom, exp) = parse_decimal_with_exponent("0").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 0);
    assert_eq!(denom, 1);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_zero_with_fraction() {
    let (int, frac, denom, exp) = parse_decimal_with_exponent("0.0").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 0);
    assert_eq!(denom, 10);
    assert_eq!(exp, 0);
}

#[test]
fn parse_decimal_with_exponent_zero_with_exponent() {
    let (int, frac, _denom, exp) = parse_decimal_with_exponent("0e100").unwrap();
    assert_eq!(int, 0);
    assert_eq!(frac, 0);
    assert_eq!(exp, 100);
}

// ==================== Error case coverage ====================

#[test]
fn parse_decimal_with_exponent_rejects_double_exponent() {
    let error = parse_decimal_with_exponent("1e5e3").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_decimal_with_exponent_rejects_mixed_separators() {
    let error = parse_decimal_with_exponent("1.5,3").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_decimal_with_exponent("1,5.3").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_decimal_with_exponent_rejects_sign_only() {
    let error = parse_decimal_with_exponent("+").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_decimal_with_exponent("-").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_decimal_with_exponent_rejects_multiple_signs() {
    let error = parse_decimal_with_exponent("++1").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_decimal_with_exponent("+-1").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}

#[test]
fn parse_decimal_with_exponent_rejects_double_sign_in_exponent() {
    let error = parse_decimal_with_exponent("1e++5").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);

    let error = parse_decimal_with_exponent("1e--5").unwrap_err();
    assert_eq!(error, BandwidthParseError::Invalid);
}
