#![deny(unsafe_code)]

//! # Overview
//!
//! The `bandwidth` module centralises parsing logic for rsync's `--bwlimit`
//! option. Both the client and daemon front-ends accept user supplied bandwidth
//! limits using the same syntax as upstream rsync. Sharing the parsing logic in
//! this module ensures consistent rounding, validation, and diagnostics across
//! binaries while keeping the core representation (`NonZeroU64` bytes per
//! second) independent from command-line formatting concerns.
//!
//! # Design
//!
//! Parsing is exposed via [`parse_bandwidth_argument`], which accepts a textual
//! rate specification and returns either an optional byte-per-second limit or a
//! [`BandwidthParseError`]. The function mirrors upstream semantics:
//!
//! - Values may include decimal fractions together with binary (`KiB`, `MiB`,
//!   ...) or decimal (`KB`, `MB`, ...) suffixes. The default unit is kibibytes
//!   per second when no suffix is supplied.
//! - A value of `0` disables the limiter (represented as `None`). Non-zero
//!   values below 512 bytes per second are rejected.
//! - The parser rounds to the nearest multiple of 1024 bytes per second,
//!   matching rsync's rounding rules.
//!
//! Higher layers can convert successful parses into
//! [`rsync_core::client::BandwidthLimit`] instances when needed.
//!
//! # Examples
//!
//! ```
//! use rsync_core::bandwidth::{parse_bandwidth_argument, BandwidthParseError};
//! use std::num::NonZeroU64;
//!
//! let limit = parse_bandwidth_argument("8M").expect("valid limit");
//! assert_eq!(limit, NonZeroU64::new(8 * 1024 * 1024));
//!
//! let unlimited = parse_bandwidth_argument("0").expect("unlimited allowed");
//! assert_eq!(unlimited, None);
//!
//! let error = parse_bandwidth_argument("12foo").unwrap_err();
//! assert_eq!(error, BandwidthParseError::Invalid);
//! ```
//!
//! # See also
//!
//! - [`crate::client::BandwidthLimit`] for the runtime representation used by
//!   the client orchestration code.

use std::num::NonZeroU64;

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
///
/// The function mirrors upstream rsync's behaviour. `Ok(None)` denotes an
/// unlimited transfer rate (users may specify `0` for this effect). Successful
/// parses return the rounded byte-per-second limit as [`NonZeroU64`].
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    if text.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = text.len();

    for (index, ch) in text.char_indices() {
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

    let numeric_part = &text[..numeric_end];
    let remainder = &text[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(BandwidthParseError::Invalid);
    }

    let normalized_numeric = numeric_part.replace(',', ".");
    let numeric_value: f64 = normalized_numeric
        .parse()
        .map_err(|_| BandwidthParseError::Invalid)?;

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

    let mut base: f64 = 1024.0;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000.0;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(BandwidthParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    base = 1024.0;
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(BandwidthParseError::Invalid);
                }
            }
            b'+' | b'-' => {}
            _ => return Err(BandwidthParseError::Invalid),
        }
    }

    let mut adjust = 0.0f64;
    if !remainder_after_suffix.is_empty() {
        if remainder_after_suffix == "+1" && numeric_end > 0 {
            adjust = 1.0;
            remainder_after_suffix = "";
        } else if remainder_after_suffix == "-1" && numeric_end > 0 {
            adjust = -1.0;
            remainder_after_suffix = "";
        }
    }

    if !remainder_after_suffix.is_empty() {
        return Err(BandwidthParseError::Invalid);
    }

    let scale = match repetitions {
        0 => 1.0,
        reps => base.powi(reps as i32),
    };

    let mut size = numeric_value * scale;
    if !size.is_finite() {
        return Err(BandwidthParseError::TooLarge);
    }
    size += adjust;
    if !size.is_finite() {
        return Err(BandwidthParseError::TooLarge);
    }

    let truncated = size.trunc();
    if truncated < 0.0 || truncated > u128::MAX as f64 {
        return Err(BandwidthParseError::TooLarge);
    }

    let bytes = truncated as u128;

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

#[cfg(test)]
mod tests {
    use super::{BandwidthParseError, parse_bandwidth_argument};
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
    fn parse_bandwidth_rejects_overflow() {
        let error = parse_bandwidth_argument("999999999999999999999999999999P").unwrap_err();
        assert_eq!(error, BandwidthParseError::TooLarge);
    }
}
