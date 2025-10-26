use std::num::NonZeroU64;

/// Parsed `--bwlimit` components consisting of an optional rate and burst size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimitComponents {
    rate: Option<NonZeroU64>,
    burst: Option<NonZeroU64>,
}

impl BandwidthLimitComponents {
    /// Constructs a new component set from the provided parts.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>, burst: Option<NonZeroU64>) -> Self {
        Self { rate, burst }
    }

    /// Returns the configured byte-per-second rate, if any.
    #[must_use]
    pub const fn rate(self) -> Option<NonZeroU64> {
        self.rate
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst(self) -> Option<NonZeroU64> {
        self.burst
    }
}

/// Errors returned when parsing a bandwidth limit fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BandwidthParseError {
    /// The argument did not follow rsync's recognised syntax.
    Invalid,
    /// The requested rate was too small (less than 512 bytes per second).
    TooSmall,
    /// The requested rate overflowed the supported range.
    TooLarge,
}

/// Parses a `--bwlimit` style argument into an optional byte-per-second limit.
#[doc(alias = "--bwlimit")]
///
/// The function mirrors upstream rsync's behaviour. Leading and trailing ASCII
/// whitespace is ignored to match `strtod`'s parsing rules. `Ok(None)` denotes
/// an unlimited transfer rate (users may specify `0` for this effect).
/// Successful parses return the rounded byte-per-second limit as
/// [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

    if trimmed.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut unsigned = trimmed;
    let mut negative = false;

    if let Some(first) = unsigned.chars().next() {
        match first {
            '+' => {
                unsigned = &unsigned[first.len_utf8()..];
            }
            '-' => {
                negative = true;
                unsigned = &unsigned[first.len_utf8()..];
            }
            _ => {}
        }
    }

    if unsigned.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = unsigned.len();

    for (index, ch) in unsigned.char_indices() {
        if ch.is_ascii_digit() {
            digits_seen = true;
            continue;
        }

        if (ch == '.' || ch == ',') && !decimal_seen {
            decimal_seen = true;
            continue;
        }

        numeric_end = index;
        break;
    }

    let numeric_part = &unsigned[..numeric_end];
    let remainder = &unsigned[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(BandwidthParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) = parse_decimal_components(numeric_part)?;

    let (suffix, mut remainder_after_suffix) =
        if remainder.is_empty() || remainder.starts_with('+') || remainder.starts_with('-') {
            ('K', remainder)
        } else {
            let mut chars = remainder.chars();
            let ch = chars.next().unwrap();
            (ch, chars.as_str())
        };

    let repetitions = match suffix.to_ascii_lowercase() {
        'b' => 0,
        'k' => 1,
        'm' => 2,
        'g' => 3,
        't' => 4,
        'p' => 5,
        _ => return Err(BandwidthParseError::Invalid),
    };

    let mut base: u32 = 1024;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
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
        if remainder_after_suffix == "+1" && numeric_end > 0 {
            adjust = 1;
            remainder_after_suffix = "";
        } else if remainder_after_suffix == "-1" && numeric_end > 0 {
            adjust = -1;
            remainder_after_suffix = "";
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let scale = pow_u128(base, repetitions)?;

    let numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(BandwidthParseError::TooLarge)?;
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
        .checked_add(512)
        .ok_or(BandwidthParseError::TooLarge)?
        / 1024;
    let rounded_bytes = rounded
        .checked_mul(1024)
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

    if let Some((rate_text, burst_text)) = trimmed.split_once(':') {
        let rate = parse_bandwidth_argument(rate_text)?;
        if rate.is_none() {
            return Ok(BandwidthLimitComponents::new(None, None));
        }

        let burst = parse_bandwidth_argument(burst_text)?;
        Ok(BandwidthLimitComponents::new(rate, burst))
    } else {
        parse_bandwidth_argument(trimmed).map(|rate| BandwidthLimitComponents::new(rate, None))
    }
}

fn parse_decimal_components(text: &str) -> Result<(u128, u128, u128), BandwidthParseError> {
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

fn pow_u128(base: u32, exponent: u32) -> Result<u128, BandwidthParseError> {
    let mut acc = 1u128;
    for _ in 0..exponent {
        acc = acc
            .checked_mul(u128::from(base))
            .ok_or(BandwidthParseError::TooLarge)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::{
        BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument,
        parse_bandwidth_limit,
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
    fn parse_bandwidth_accepts_leading_plus_sign() {
        let limit = parse_bandwidth_argument("+1M").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1_048_576));
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
    fn parse_bandwidth_limit_zero_rate_disables_burst() {
        let components = parse_bandwidth_limit("0:128K").expect("parse succeeds");
        assert_eq!(components, BandwidthLimitComponents::new(None, None));
    }

    #[test]
    fn parse_bandwidth_limit_accepts_zero_burst() {
        let components = parse_bandwidth_limit("1M:0").expect("parse succeeds");
        assert_eq!(
            components,
            BandwidthLimitComponents::new(NonZeroU64::new(1_048_576), None)
        );
    }

    #[test]
    fn parse_bandwidth_trims_surrounding_whitespace() {
        let limit = parse_bandwidth_argument("\t 2M \n").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(2_097_152));
    }

    #[test]
    fn parse_bandwidth_accepts_positive_adjustment() {
        let limit = parse_bandwidth_argument("1K+1").expect("parse succeeds");
        assert_eq!(limit, NonZeroU64::new(1024));
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
    fn parse_bandwidth_rejects_negative_values() {
        let error = parse_bandwidth_argument("-1M").unwrap_err();
        assert_eq!(error, BandwidthParseError::Invalid);
    }

    #[test]
    fn parse_bandwidth_rejects_overflow() {
        let error = parse_bandwidth_argument("999999999999999999999999999999P").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
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
}
