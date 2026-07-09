//! Bandwidth limit argument parsing compatible with upstream rsync.
//!
//! The parsers mirror the `--bwlimit` syntax accepted by upstream rsync's
//! `util2.c:parse_size_arg()`. Supported features include binary and decimal
//! suffixes (`K`, `M`, `G`, `T`, `P`, `B`, `iB`), fractional values with
//! dot or comma separators, scientific notation, leading `+`/`-` signs,
//! and optional `+1`/`-1` adjustment modifiers. A colon-separated burst
//! component (`RATE:BURST`) is also handled for daemon configurations.

use std::num::NonZeroU64;

use thiserror::Error;

mod components;

pub use components::BandwidthLimitComponents;

use crate::size_arg::{SizeArgError, parse_size_arg};

/// Errors returned when parsing a bandwidth limit fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum BandwidthParseError {
    /// The argument did not follow rsync's recognised syntax.
    #[error("invalid bandwidth limit syntax")]
    Invalid,
    /// The requested rate was too small (less than 512 bytes per second).
    #[error("bandwidth limit is below the minimum of 512 bytes per second")]
    TooSmall,
    /// The requested rate overflowed the supported range.
    #[error("bandwidth limit exceeds the supported range")]
    TooLarge,
}

impl From<SizeArgError> for BandwidthParseError {
    fn from(error: SizeArgError) -> Self {
        match error {
            SizeArgError::Invalid => BandwidthParseError::Invalid,
            SizeArgError::TooLarge => BandwidthParseError::TooLarge,
        }
    }
}

/// Parses a `--bwlimit` style argument into an optional byte-per-second limit.
///
/// The function mirrors upstream rsync's behaviour. Inputs must match the
/// syntax accepted by `parse_size_arg()` which rejects embedded or surrounding
/// whitespace. `Ok(None)` denotes an unlimited transfer rate (users may specify
/// `0` for this effect). Successful parses return the rounded byte-per-second
/// limit as [`NonZeroU64`].
// upstream: options.c:parse_size_arg() - suffix/multiplier/adjust parsing
#[doc(alias = "--bwlimit")]
pub fn parse_bandwidth_argument(text: &str) -> Result<Option<NonZeroU64>, BandwidthParseError> {
    if text.as_bytes().iter().all(u8::is_ascii_whitespace) {
        return Err(BandwidthParseError::Invalid);
    }

    // oc's --bwlimit historically accepts a leading '+'/'-'; a negative rate is
    // rejected once the magnitude is known. Everything after the sign is a plain
    // size argument with a default suffix of 'K', matching upstream's
    // `parse_size_arg(bwlimit_arg, 'K', ...)`.
    let mut start = 0usize;
    let mut negative = false;
    if let Some(&first) = text.as_bytes().first() {
        match first {
            b'+' => start = 1,
            b'-' => {
                negative = true;
                start = 1;
            }
            _ => {}
        }
    }

    if start == text.len() {
        return Err(BandwidthParseError::Invalid);
    }

    let parsed = parse_size_arg(&text[start..], b'K').map_err(BandwidthParseError::from)?;

    if negative {
        return Err(BandwidthParseError::Invalid);
    }

    let bytes = parsed.bytes;
    if bytes == 0 {
        return Ok(None);
    }

    if bytes < 512 {
        return Err(BandwidthParseError::TooSmall);
    }

    // upstream rounds bwlimit to whole KiB via `(size + 512) / 1024`; oc rounds
    // to the granularity implied by the suffix (bytes/decimal/binary).
    let alignment = parsed.unit;
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
///
/// Accepts the `RATE[:BURST]` syntax used in daemon module configuration.
/// Both the rate and burst segments follow the same suffix and multiplier
/// rules as [`parse_bandwidth_argument`]. A rate of `0` produces an
/// unlimited configuration. Surrounding whitespace is rejected to match
/// upstream rsync's strict parsing.
// upstream: options.c:server_options() - bwlimit with optional burst
#[doc(alias = "--bwlimit")]
pub fn parse_bandwidth_limit(text: &str) -> Result<BandwidthLimitComponents, BandwidthParseError> {
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());

    if trimmed.len() != text.len() {
        return Err(BandwidthParseError::Invalid);
    }

    if let Some((rate_text, burst_text)) = trimmed.split_once(':') {
        let rate = parse_bandwidth_argument(rate_text)?;
        if rate.is_none() {
            return Ok(BandwidthLimitComponents::with_internal_flags(
                None, None, true, false,
            ));
        }

        let burst = parse_bandwidth_argument(burst_text)?;
        Ok(BandwidthLimitComponents::with_internal_flags(
            rate, burst, true, true,
        ))
    } else {
        parse_bandwidth_argument(trimmed).map(|rate| match rate {
            Some(rate) => BandwidthLimitComponents::new(Some(rate), None),
            None => BandwidthLimitComponents::with_internal_flags(None, None, true, false),
        })
    }
}

#[cfg(test)]
mod tests;
