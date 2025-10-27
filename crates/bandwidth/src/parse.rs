use std::error::Error;
use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use crate::limiter::BandwidthLimiter;

/// Parsed `--bwlimit` components consisting of an optional rate and burst size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimitComponents {
    rate: Option<NonZeroU64>,
    burst: Option<NonZeroU64>,
    burst_specified: bool,
}

impl BandwidthLimitComponents {
    /// Constructs a new component set from the provided parts.
    ///
    /// When the rate is `None` the combination represents an unlimited
    /// configuration. Upstream rsync ignores any burst component in that case,
    /// so the helper mirrors that behaviour by discarding the supplied burst.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>, burst: Option<NonZeroU64>) -> Self {
        Self::new_with_specified(rate, burst, burst.is_some())
    }

    /// Returns a component set that disables throttling.
    ///
    /// Upstream rsync treats `--bwlimit=0` as unlimited, ignoring any optional
    /// burst parameter. Providing an explicit constructor avoids sprinkling
    /// `None` pairs throughout the codebase while making the intent of
    /// "unlimited" limits clear at the call site. The helper is `const` so it
    /// can be used in static initialisers and default values.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new(None, None)
    }

    /// Constructs a new component set and records whether the burst component
    /// was explicitly supplied.
    #[must_use]
    pub const fn new_with_specified(
        rate: Option<NonZeroU64>,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
    ) -> Self {
        let effective_rate = rate;
        let effective_burst = if effective_rate.is_some() {
            burst
        } else {
            None
        };
        let effective_specified = if effective_rate.is_some() {
            burst_specified
        } else {
            false
        };

        Self {
            rate: effective_rate,
            burst: effective_burst,
            burst_specified: effective_specified,
        }
    }

    /// Returns the configured byte-per-second rate, if any.
    #[must_use]
    pub const fn rate(&self) -> Option<NonZeroU64> {
        self.rate
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst(&self) -> Option<NonZeroU64> {
        self.burst
    }

    /// Indicates whether a burst component was explicitly specified.
    #[must_use]
    pub const fn burst_specified(&self) -> bool {
        self.burst_specified
    }

    /// Indicates whether the limit disables throttling.
    #[must_use]
    pub const fn is_unlimited(self) -> bool {
        self.rate.is_none()
    }

    /// Converts the parsed components into a [`BandwidthLimiter`].
    ///
    /// When the rate component is absent (representing an unlimited
    /// configuration), the method returns `None`. Otherwise the limiter mirrors
    /// upstream rsync's token bucket by honouring the optional burst size.
    #[must_use]
    pub fn to_limiter(&self) -> Option<BandwidthLimiter> {
        self.rate()
            .map(|rate| BandwidthLimiter::with_burst(rate, self.burst()))
    }

    /// Consumes the components and constructs a [`BandwidthLimiter`].
    ///
    /// The behaviour matches [`Self::to_limiter`]; the by-value variant avoids
    /// cloning when the caller wishes to move ownership of the parsed
    /// components.
    #[must_use]
    pub fn into_limiter(self) -> Option<BandwidthLimiter> {
        self.rate
            .map(|rate| BandwidthLimiter::with_burst(rate, self.burst))
    }
}

impl FromStr for BandwidthLimitComponents {
    type Err = BandwidthParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse_bandwidth_limit(text)
    }
}

impl Default for BandwidthLimitComponents {
    fn default() -> Self {
        Self::unlimited()
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

impl fmt::Display for BandwidthParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            BandwidthParseError::Invalid => "invalid bandwidth limit syntax",
            BandwidthParseError::TooSmall => {
                "bandwidth limit is below the minimum of 512 bytes per second"
            }
            BandwidthParseError::TooLarge => "bandwidth limit exceeds the supported range",
        };

        f.write_str(description)
    }
}

impl Error for BandwidthParseError {}

/// Parses a `--bwlimit` style argument into an optional byte-per-second limit.
#[doc(alias = "--bwlimit")]
///
/// The function mirrors upstream rsync's behaviour. Inputs must match the
/// syntax accepted by `parse_size_arg()` which rejects embedded or surrounding
/// whitespace. `Ok(None)` denotes an unlimited transfer rate (users may specify
/// `0` for this effect). Successful parses return the rounded byte-per-second
/// limit as [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    if text.chars().all(|ch| ch.is_ascii_whitespace()) {
        return Err(BandwidthParseError::Invalid);
    }

    let mut unsigned = text;
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
    let mut exponent_seen = false;
    let mut exponent_digits_seen = false;
    let mut exponent_sign_allowed = false;
    let mut numeric_end = unsigned.len();

    for (index, ch) in unsigned.char_indices() {
        if ch.is_ascii_digit() {
            digits_seen = true;
            if exponent_seen {
                exponent_digits_seen = true;
                exponent_sign_allowed = false;
            }
            continue;
        }

        if (ch == '.' || ch == ',') && !decimal_seen && !exponent_seen {
            decimal_seen = true;
            continue;
        }

        if matches!(ch, 'e' | 'E') && digits_seen && !exponent_seen {
            exponent_seen = true;
            exponent_sign_allowed = true;
            continue;
        }

        if (ch == '+' || ch == '-') && exponent_sign_allowed {
            exponent_sign_allowed = false;
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
            return Ok(BandwidthLimitComponents::unlimited());
        }

        let burst = parse_bandwidth_argument(burst_text)?;
        Ok(BandwidthLimitComponents::new_with_specified(
            rate, burst, true,
        ))
    } else {
        parse_bandwidth_argument(trimmed).map(|rate| BandwidthLimitComponents::new(rate, None))
    }
}

fn parse_decimal_with_exponent(text: &str) -> Result<(u128, u128, u128, i32), BandwidthParseError> {
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

fn parse_decimal_mantissa(text: &str) -> Result<(u128, u128, u128), BandwidthParseError> {
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

#[cfg(test)]
mod tests;
