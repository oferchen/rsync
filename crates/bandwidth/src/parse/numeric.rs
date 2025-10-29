use super::BandwidthParseError;

pub(super) fn parse_decimal_with_exponent(
    text: &str,
) -> Result<(u128, u128, u128, i32), BandwidthParseError> {
    let (mantissa_text, exponent_text) = if let Some(position) = text.find(['e', 'E']) {
        let (mantissa, exponent) = text.split_at(position);
        let exponent = &exponent[1..];
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

pub(super) fn parse_decimal_mantissa(
    text: &str,
) -> Result<(u128, u128, u128), BandwidthParseError> {
    let mut integer = 0u128;
    let mut fraction = 0u128;
    let mut denominator = 1u128;
    let mut saw_decimal = false;

    for ch in text.chars() {
        match ch {
            '0'..='9' => {
                let digit = u128::from(ch as u8 - b'0');
                if saw_decimal {
                    denominator = denominator
                        .checked_mul(10)
                        .ok_or(BandwidthParseError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(BandwidthParseError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(BandwidthParseError::TooLarge)?;
                }
            }
            '.' | ',' => {
                if saw_decimal {
                    return Err(BandwidthParseError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
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
