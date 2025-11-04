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
