use super::BandwidthParseError;
use memchr::memchr2;

pub(crate) fn parse_decimal_with_exponent(
    text: &str,
) -> Result<(u128, u128, u128, i32), BandwidthParseError> {
    let bytes = text.as_bytes();
    let (mantissa_text, exponent_text) = if let Some(position) = memchr2(b'e', b'E', bytes) {
        let mantissa = &text[..position];
        let exponent = &text[position + 1..];
        (mantissa, Some(exponent))
    } else {
        (text, None)
    };

    let (integer, fraction, denominator) = parse_decimal_mantissa(mantissa_text)?;

    let exponent = match exponent_text {
        Some(component) => {
            if component.is_empty() {
                return Err(BandwidthParseError::Invalid);
            }

            component
                .parse::<i32>()
                .map_err(|_| BandwidthParseError::Invalid)?
        }
        None => 0,
    };

    Ok((integer, fraction, denominator, exponent))
}

pub(crate) fn pow_u128(base: u32, exponent: u32) -> Result<u128, BandwidthParseError> {
    let mut result = 1u128;
    let mut factor = u128::from(base);
    let mut exp = exponent;

    while exp > 0 {
        if (exp & 1) == 1 {
            result = result
                .checked_mul(factor)
                .ok_or(BandwidthParseError::TooLarge)?;
        }

        exp >>= 1;
        if exp > 0 {
            factor = factor
                .checked_mul(factor)
                .ok_or(BandwidthParseError::TooLarge)?;
        }
    }

    Ok(result)
}

fn parse_decimal_mantissa(text: &str) -> Result<(u128, u128, u128), BandwidthParseError> {
    let bytes = text.as_bytes();

    if let Some(position) = memchr2(b'.', b',', bytes) {
        let (integer_bytes, fractional_with_sep) = bytes.split_at(position);
        let fractional_bytes = &fractional_with_sep[1..];

        if memchr2(b'.', b',', fractional_bytes).is_some() {
            return Err(BandwidthParseError::Invalid);
        }

        let integer = parse_digits(integer_bytes)?;
        let mut denominator = 1u128;
        let mut fraction = 0u128;

        for &byte in fractional_bytes {
            if !byte.is_ascii_digit() {
                return Err(BandwidthParseError::Invalid);
            }

            denominator = denominator
                .checked_mul(10)
                .ok_or(BandwidthParseError::TooLarge)?;
            fraction = fraction
                .checked_mul(10)
                .and_then(|value| value.checked_add(u128::from(byte - b'0')))
                .ok_or(BandwidthParseError::TooLarge)?;
        }

        return Ok((integer, fraction, denominator));
    }

    let integer = parse_digits(bytes)?;
    Ok((integer, 0, 1))
}

fn parse_digits(bytes: &[u8]) -> Result<u128, BandwidthParseError> {
    let mut value = 0u128;

    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(BandwidthParseError::Invalid);
        }

        value = value
            .checked_mul(10)
            .and_then(|acc| acc.checked_add(u128::from(byte - b'0')))
            .ok_or(BandwidthParseError::TooLarge)?;
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for pow_u128
    #[test]
    fn pow_u128_zero_exponent() {
        assert_eq!(pow_u128(10, 0).unwrap(), 1);
        assert_eq!(pow_u128(1024, 0).unwrap(), 1);
    }

    #[test]
    fn pow_u128_one_exponent() {
        assert_eq!(pow_u128(10, 1).unwrap(), 10);
        assert_eq!(pow_u128(1024, 1).unwrap(), 1024);
    }

    #[test]
    fn pow_u128_small_powers() {
        assert_eq!(pow_u128(2, 10).unwrap(), 1024);
        assert_eq!(pow_u128(10, 3).unwrap(), 1000);
        assert_eq!(pow_u128(1024, 2).unwrap(), 1024 * 1024);
    }

    #[test]
    fn pow_u128_larger_powers() {
        // 1024^5 = petabyte
        assert_eq!(pow_u128(1024, 5).unwrap(), 1024u128.pow(5));
    }

    #[test]
    fn pow_u128_overflow_returns_error() {
        // Very large exponents should overflow
        let result = pow_u128(1024, 100);
        assert!(result.is_err());
    }

    #[test]
    fn pow_u128_base_one() {
        // 1^n = 1 for any n
        assert_eq!(pow_u128(1, 100).unwrap(), 1);
    }

    #[test]
    fn pow_u128_base_zero_one() {
        // 0^1 = 0
        assert_eq!(pow_u128(0, 1).unwrap(), 0);
    }

    // Tests for parse_decimal_with_exponent
    #[test]
    fn parse_decimal_with_exponent_integer_only() {
        let (int, frac, denom, exp) = parse_decimal_with_exponent("12345").unwrap();
        assert_eq!(int, 12345);
        assert_eq!(frac, 0);
        assert_eq!(denom, 1);
        assert_eq!(exp, 0);
    }

    #[test]
    fn parse_decimal_with_exponent_with_fraction() {
        let (int, frac, denom, exp) = parse_decimal_with_exponent("12.345").unwrap();
        assert_eq!(int, 12);
        assert_eq!(frac, 345);
        assert_eq!(denom, 1000);
        assert_eq!(exp, 0);
    }

    #[test]
    fn parse_decimal_with_exponent_with_comma() {
        // Comma is also accepted as decimal separator
        let (int, frac, denom, exp) = parse_decimal_with_exponent("12,5").unwrap();
        assert_eq!(int, 12);
        assert_eq!(frac, 5);
        assert_eq!(denom, 10);
        assert_eq!(exp, 0);
    }

    #[test]
    fn parse_decimal_with_exponent_positive_exponent() {
        let (int, frac, denom, exp) = parse_decimal_with_exponent("1e3").unwrap();
        assert_eq!(int, 1);
        assert_eq!(frac, 0);
        assert_eq!(denom, 1);
        assert_eq!(exp, 3);
    }

    #[test]
    fn parse_decimal_with_exponent_negative_exponent() {
        let (int, frac, denom, exp) = parse_decimal_with_exponent("1E-3").unwrap();
        assert_eq!(int, 1);
        assert_eq!(frac, 0);
        assert_eq!(denom, 1);
        assert_eq!(exp, -3);
    }

    #[test]
    fn parse_decimal_with_exponent_full_form() {
        let (int, frac, denom, exp) = parse_decimal_with_exponent("1.5e2").unwrap();
        assert_eq!(int, 1);
        assert_eq!(frac, 5);
        assert_eq!(denom, 10);
        assert_eq!(exp, 2);
    }

    #[test]
    fn parse_decimal_with_exponent_uppercase_e() {
        let (_, _, _, exp) = parse_decimal_with_exponent("5E10").unwrap();
        assert_eq!(exp, 10);
    }

    #[test]
    fn parse_decimal_with_exponent_empty_exponent_fails() {
        // "1e" with no digits after e is invalid
        let result = parse_decimal_with_exponent("1e");
        assert!(result.is_err());
    }

    // Tests for parse_decimal_mantissa
    #[test]
    fn parse_decimal_mantissa_integer() {
        let (int, frac, denom) = parse_decimal_mantissa("123").unwrap();
        assert_eq!(int, 123);
        assert_eq!(frac, 0);
        assert_eq!(denom, 1);
    }

    #[test]
    fn parse_decimal_mantissa_with_decimal() {
        let (int, frac, denom) = parse_decimal_mantissa("3.14159").unwrap();
        assert_eq!(int, 3);
        assert_eq!(frac, 14159);
        assert_eq!(denom, 100000);
    }

    #[test]
    fn parse_decimal_mantissa_leading_zeros_in_fraction() {
        let (int, frac, denom) = parse_decimal_mantissa("1.01").unwrap();
        assert_eq!(int, 1);
        assert_eq!(frac, 1);
        assert_eq!(denom, 100);
    }

    #[test]
    fn parse_decimal_mantissa_comma_separator() {
        let (int, frac, denom) = parse_decimal_mantissa("2,5").unwrap();
        assert_eq!(int, 2);
        assert_eq!(frac, 5);
        assert_eq!(denom, 10);
    }

    #[test]
    fn parse_decimal_mantissa_multiple_separators_fails() {
        let result = parse_decimal_mantissa("1.2.3");
        assert!(result.is_err());
    }

    #[test]
    fn parse_decimal_mantissa_empty_integer_part() {
        // ".5" should parse with integer = 0
        let (int, frac, denom) = parse_decimal_mantissa(".5").unwrap();
        assert_eq!(int, 0);
        assert_eq!(frac, 5);
        assert_eq!(denom, 10);
    }

    #[test]
    fn parse_decimal_mantissa_empty_fraction_part() {
        // "5." should parse with fraction = 0
        let (int, frac, denom) = parse_decimal_mantissa("5.").unwrap();
        assert_eq!(int, 5);
        assert_eq!(frac, 0);
        assert_eq!(denom, 1);
    }

    // Tests for parse_digits
    #[test]
    fn parse_digits_valid() {
        assert_eq!(parse_digits(b"12345").unwrap(), 12345);
    }

    #[test]
    fn parse_digits_zero() {
        assert_eq!(parse_digits(b"0").unwrap(), 0);
    }

    #[test]
    fn parse_digits_empty() {
        assert_eq!(parse_digits(b"").unwrap(), 0);
    }

    #[test]
    fn parse_digits_leading_zeros() {
        assert_eq!(parse_digits(b"00123").unwrap(), 123);
    }

    #[test]
    fn parse_digits_invalid_char_fails() {
        let result = parse_digits(b"12a34");
        assert!(result.is_err());
    }

    #[test]
    fn parse_digits_large_number() {
        let result = parse_digits(b"12345678901234567890").unwrap();
        assert_eq!(result, 12345678901234567890u128);
    }

    #[test]
    fn parse_digits_very_large_overflows() {
        // u128::MAX is 39 digits, so 40+ should overflow
        let huge = "9".repeat(50);
        let result = parse_digits(huge.as_bytes());
        assert!(result.is_err());
    }
}
