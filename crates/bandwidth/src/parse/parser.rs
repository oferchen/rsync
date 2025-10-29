use std::num::NonZeroU64;

use super::{
    components::BandwidthLimitComponents,
    error::BandwidthParseError,
    numeric::{parse_decimal_with_exponent, pow_u128},
};

/// Parses a `--bwlimit` style argument into an optional byte-per-second limit.
#[doc(alias = "--bwlimit")]
///
/// The function mirrors upstream rsync's behaviour. Inputs must match the
/// syntax accepted by `parse_size_arg()` which rejects embedded or surrounding
/// whitespace. `Ok(None)` denotes an unlimited transfer rate (users may specify
/// `0` for this effect). Successful parses return the rounded byte-per-second
/// limit as [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    if text
        .as_bytes()
        .iter()
        .all(|byte| byte.is_ascii_whitespace())
    {
        return Err(BandwidthParseError::Invalid);
    }

    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut negative = false;

    if let Some(&first) = bytes.first() {
        match first {
            b'+' => start = 1,
            b'-' => {
                negative = true;
                start = 1;
            }
            _ => {}
        }
    }

    if start == bytes.len() {
        return Err(BandwidthParseError::Invalid);
    }

    let unsigned = &text[start..];
    let unsigned_bytes = unsigned.as_bytes();
    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut exponent_seen = false;
    let mut exponent_digits_seen = false;
    let mut exponent_sign_allowed = false;
    let mut numeric_end = unsigned_bytes.len();

    for (index, &byte) in unsigned_bytes.iter().enumerate() {
        match byte {
            b'0'..=b'9' => {
                digits_seen = true;
                if exponent_seen {
                    exponent_digits_seen = true;
                    exponent_sign_allowed = false;
                }
                continue;
            }
            b'.' | b',' if !decimal_seen && !exponent_seen => {
                decimal_seen = true;
                continue;
            }
            b'e' | b'E' if digits_seen && !exponent_seen => {
                exponent_seen = true;
                exponent_sign_allowed = true;
                continue;
            }
            b'+' | b'-' if exponent_sign_allowed => {
                exponent_sign_allowed = false;
                continue;
            }
            _ => {
                numeric_end = index;
                break;
            }
        }
    }

    let numeric_part = &unsigned[..numeric_end];
    let remainder = &unsigned[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(BandwidthParseError::Invalid);
    }

    if exponent_seen && !exponent_digits_seen {
        return Err(BandwidthParseError::Invalid);
    }

    if exponent_sign_allowed {
        return Err(BandwidthParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator, decimal_exponent) =
        parse_decimal_with_exponent(numeric_part)?;

    let (suffix, mut remainder_after_suffix) =
        if remainder.is_empty() || remainder.starts_with('+') || remainder.starts_with('-') {
            (b'K', remainder)
        } else {
            let first = remainder.as_bytes()[0];
            if !first.is_ascii() || !remainder.is_char_boundary(1) {
                return Err(BandwidthParseError::Invalid);
            }
            (first, &remainder[1..])
        };

    let normalized_suffix = suffix.to_ascii_lowercase();
    let repetitions = match normalized_suffix {
        b'b' => 0,
        b'k' => 1,
        b'm' => 2,
        b'g' => 3,
        b't' => 4,
        b'p' => 5,
        _ => return Err(BandwidthParseError::Invalid),
    };

    let mut base: u32 = 1024;
    let mut alignment: u128 = if normalized_suffix == b'b' { 1 } else { 1024 };

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                alignment = 1000;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(BandwidthParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    base = 1024;
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(BandwidthParseError::Invalid);
                }
            }
            b'+' | b'-' => {}
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    let mut adjust = 0i8;
    if !remainder_after_suffix.is_empty() {
        let parse_adjust = |text: &[u8]| -> Option<i8> {
            match text {
                [b'+', b'1'] => Some(1),
                [b'-', b'1'] => Some(-1),
                _ => None,
            }
        };

        if let Some(delta) =
            parse_adjust(remainder_after_suffix.as_bytes()).filter(|_| numeric_end > 0)
        {
            adjust = delta;
            remainder_after_suffix = "";
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let scale = pow_u128(base, repetitions)?;

    let mut numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(BandwidthParseError::TooLarge)?;
    let mut denominator = denominator;

    if decimal_exponent > 0 {
        let factor = pow_u128(10, decimal_exponent.unsigned_abs())?;
        numerator = numerator
            .checked_mul(factor)
            .ok_or(BandwidthParseError::TooLarge)?;
    } else if decimal_exponent < 0 {
        let factor = pow_u128(10, decimal_exponent.unsigned_abs())?;
        denominator = denominator
            .checked_mul(factor)
            .ok_or(BandwidthParseError::TooLarge)?;
    }

    let product = numerator
        .checked_mul(scale)
        .ok_or(BandwidthParseError::TooLarge)?;

    let mut bytes = product / denominator;

    if adjust == -1 {
        if product >= denominator {
            bytes = bytes.checked_sub(1).ok_or(BandwidthParseError::TooLarge)?;
        } else {
            bytes = 0;
        }
    } else if adjust == 1 {
        bytes = bytes.checked_add(1).ok_or(BandwidthParseError::TooLarge)?;
    }

    if negative {
        return Err(BandwidthParseError::Invalid);
    }

    if bytes == 0 {
        return Ok(None);
    }

    if bytes < 512 {
        return Err(BandwidthParseError::TooSmall);
    }

    let rounded = bytes
        .checked_add(alignment / 2)
        .ok_or(BandwidthParseError::TooLarge)?
        / alignment;
    let rounded_bytes = rounded
        .checked_mul(alignment)
        .ok_or(BandwidthParseError::TooLarge)?;

    let bytes_u64 = u64::try_from(rounded_bytes).map_err(|_| BandwidthParseError::TooLarge)?;

    NonZeroU64::new(bytes_u64)
        .ok_or(BandwidthParseError::TooSmall)
        .map(Some)
}

/// Parses a bandwidth limit containing an optional burst component.
#[doc(alias = "--bwlimit")]
pub fn parse_bandwidth_limit(text: &str) -> Result<BandwidthLimitComponents, BandwidthParseError> {
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

    if trimmed.len() != text.len() {
        return Err(BandwidthParseError::Invalid);
    }

    if let Some((rate_text, burst_text)) = trimmed.split_once(':') {
        let rate = parse_bandwidth_argument(rate_text)?;
        if rate.is_none() {
            return Ok(BandwidthLimitComponents::new_internal(
                None, None, true, false,
            ));
        }

        let burst = parse_bandwidth_argument(burst_text)?;
        Ok(BandwidthLimitComponents::new_internal(
            rate, burst, true, true,
        ))
    } else {
        parse_bandwidth_argument(trimmed).map(|rate| match rate {
            Some(rate) => BandwidthLimitComponents::new(Some(rate), None),
            None => BandwidthLimitComponents::new_internal(None, None, true, false),
        })
    }
}
