//! Size specification parsing for arguments with optional unit suffixes.
//!
//! Handles `--block-size`, `--max-size`, `--min-size`, and `--max-alloc` arguments.
//! Supports binary (K/M/G/T/P/E = powers of 1024) and decimal (KB/MB/GB = powers of 1000)
//! suffixes, as well as explicit binary suffixes (KiB/MiB/GiB).
//! Mirrors upstream rsync's size parsing behavior.

use std::ffi::OsStr;
use std::num::NonZeroU32;

use core::{
    message::{Message, Role},
    rsync_error,
};

/// Error variants for size specification parsing.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SizeParseError {
    /// Input string is empty or contains only a sign character.
    Empty,
    /// Input is a negative number.
    Negative,
    /// Input has invalid format or unrecognized suffix.
    Invalid,
    /// Parsed value exceeds representable range.
    TooLarge,
}

/// Parses a size argument with an optional unit suffix (K/M/G/T/P/E).
///
/// The `flag` parameter is used in error messages (e.g. `"--max-size"`).
pub(crate) fn parse_size_limit_argument(value: &OsStr, flag: &str) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match parse_size_spec(trimmed) {
        Ok(limit) => Ok(limit),
        Err(SizeParseError::Empty) => {
            Err(rsync_error!(1, format!("{flag} value must not be empty")).with_role(Role::Client))
        }
        Err(SizeParseError::Negative) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size must be non-negative")
        )
        .with_role(Role::Client)),
        Err(SizeParseError::Invalid) => Err(rsync_error!(
            1,
            format!(
                "invalid {flag} '{display}': expected a size with an optional K/M/G/T/P suffix"
            )
        )
        .with_role(Role::Client)),
        Err(SizeParseError::TooLarge) => Err(rsync_error!(
            1,
            format!("invalid {flag} '{display}': size exceeds the supported range")
        )
        .with_role(Role::Client)),
    }
}

/// Sanity ceiling for `--max-alloc`.
///
/// Limits the parsed value to at most one quarter of `u64::MAX` so the cap
/// can be safely converted to `usize` and added to outstanding-byte counters
/// without risking arithmetic overflow on 64-bit platforms.
pub(crate) const MAX_ALLOC_CEILING: u64 = u64::MAX / 4;

/// Parses the `--max-alloc` argument as a non-zero byte count with a sanity ceiling.
///
/// Mirrors upstream rsync's `parse_size_arg("max-alloc", ...)` accepting K, M,
/// G, T, P, E suffixes, while rejecting:
///
/// - empty input,
/// - negative values,
/// - zero (unlimited semantics are not exposed by oc-rsync),
/// - values above [`MAX_ALLOC_CEILING`].
///
/// # Errors
///
/// Returns a `Message` with role [`Role::Client`] and exit code 1 on any
/// rejection, matching upstream's diagnostic style.
pub(crate) fn parse_max_alloc_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    let limit = parse_size_limit_argument(value, "--max-alloc")?;

    if limit == 0 {
        return Err(rsync_error!(
            1,
            format!("invalid --max-alloc '{display}': size must be greater than zero")
        )
        .with_role(Role::Client));
    }

    if limit > MAX_ALLOC_CEILING {
        return Err(rsync_error!(
            1,
            format!("invalid --max-alloc '{display}': size exceeds the supported range")
        )
        .with_role(Role::Client));
    }

    Ok(limit)
}

/// Parses the `--block-size` argument as a positive `NonZeroU32` with optional unit suffix.
pub(crate) fn parse_block_size_argument(value: &OsStr) -> Result<NonZeroU32, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    let limit = parse_size_limit_argument(value, "--block-size")?;
    if limit == 0 {
        return Err(rsync_error!(
            1,
            format!("invalid --block-size '{display}': size must be positive")
        )
        .with_role(Role::Client));
    }

    let block_size = u32::try_from(limit).map_err(|_| {
        rsync_error!(
            1,
            format!("invalid --block-size '{display}': size exceeds the supported 32-bit range")
        )
        .with_role(Role::Client)
    })?;

    NonZeroU32::new(block_size).ok_or_else(|| {
        rsync_error!(
            1,
            format!("invalid --block-size '{display}': size must be positive")
        )
        .with_role(Role::Client)
    })
}

/// Computes `base^exponent` as `u128`, returning `TooLarge` on overflow.
pub(crate) fn pow_u128_for_size(base: u32, exponent: u32) -> Result<u128, SizeParseError> {
    u128::from(base)
        .checked_pow(exponent)
        .ok_or(SizeParseError::TooLarge)
}

/// Parses a size specification string into a byte count.
///
/// Mirrors upstream rsync's `options.c:parse_size_arg()` exactly. Supports:
/// - Plain integers: `"1024"` -> 1024
/// - Fractional values with `.` or `,`: `"1.5K"` -> 1536
/// - Binary suffixes (powers of 1024): K, M, G, T, P
/// - Decimal suffixes (powers of 1000): KB, MB, GB, TB, PB
/// - Explicit binary suffixes: KiB, MiB, GiB, TiB, PiB
/// - Byte suffix: B (no scaling)
/// - A single trailing `+1`/`-1` byte adjustment: `"1K-1"` -> 1023
///
/// A leading `+` is rejected and there is no exa (`E`) suffix, matching
/// upstream's suffix switch which stops at `p`/`P`.
fn parse_size_spec(text: &str) -> Result<u64, SizeParseError> {
    if text.is_empty() {
        return Err(SizeParseError::Empty);
    }

    // upstream: options.c:parse_size_arg() never strips a leading '+', so
    // "+100" is rejected. A leading '-' is a negative size, which we reject
    // with a dedicated diagnostic rather than a generic parse error.
    let unsigned = match text.strip_prefix('-') {
        Some("") => return Err(SizeParseError::Empty),
        Some(_) => return Err(SizeParseError::Negative),
        None => text,
    };

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
        return Err(SizeParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) = parse_decimal_components(numeric_part)?;

    // upstream: options.c:parse_size_arg() suffix switch stops at 'p' (no exa)
    // and, because it excludes '+'/'-' from the suffix, treats a trailing sign
    // as "no suffix" so the default byte scaling applies before the adjustment.
    let (exponent, mut remainder_after_suffix) =
        if remainder.is_empty() || remainder.starts_with(['+', '-']) {
            (0u32, remainder)
        } else {
            let mut chars = remainder.chars();
            let ch = chars.next().unwrap();
            (
                match ch.to_ascii_lowercase() {
                    'b' => 0,
                    'k' => 1,
                    'm' => 2,
                    'g' => 3,
                    't' => 4,
                    'p' => 5,
                    _ => return Err(SizeParseError::Invalid),
                },
                chars.as_str(),
            )
        };

    let mut base = 1024u32;

    if !remainder_after_suffix.is_empty() {
        let bytes = remainder_after_suffix.as_bytes();
        match bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                remainder_after_suffix = &remainder_after_suffix[1..];
            }
            b'i' | b'I' => {
                if bytes.len() < 2 {
                    return Err(SizeParseError::Invalid);
                }
                if matches!(bytes[1], b'b' | b'B') {
                    remainder_after_suffix = &remainder_after_suffix[2..];
                } else {
                    return Err(SizeParseError::Invalid);
                }
            }
            _ => {}
        }
    }

    // upstream: options.c:parse_size_arg() honours exactly one trailing
    // "+1"/"-1" byte adjustment after the suffix, e.g. "1K-1" == 1023. Any
    // other trailing text (including "+2" or "-10") is invalid.
    let adjust: i8 = match remainder_after_suffix.as_bytes() {
        [b'+', b'1'] => {
            remainder_after_suffix = "";
            1
        }
        [b'-', b'1'] => {
            remainder_after_suffix = "";
            -1
        }
        _ => 0,
    };

    if !remainder_after_suffix.is_empty() {
        return Err(SizeParseError::Invalid);
    }

    let scale = pow_u128_for_size(base, exponent)?;

    let numerator = integer_part
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fractional_part))
        .ok_or(SizeParseError::TooLarge)?;
    let product = numerator
        .checked_mul(scale)
        .ok_or(SizeParseError::TooLarge)?;

    let value = product / denominator;
    // upstream rejects a resulting negative size (`0-1` reports "too large").
    let value = match adjust {
        1 => value.checked_add(1).ok_or(SizeParseError::TooLarge)?,
        -1 => value.checked_sub(1).ok_or(SizeParseError::TooLarge)?,
        _ => value,
    };
    if value > u64::MAX as u128 {
        return Err(SizeParseError::TooLarge);
    }

    Ok(value as u64)
}

/// Splits a decimal number string into integer, fractional, and denominator components.
///
/// For `"1.5"`: returns `(1, 5, 10)` so the value is `1 + 5/10`.
/// Supports both `.` and `,` as decimal separators.
fn parse_decimal_components(text: &str) -> Result<(u128, u128, u128), SizeParseError> {
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
                        .ok_or(SizeParseError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeParseError::TooLarge)?;
                }
            }
            '.' | ',' => {
                if saw_decimal {
                    return Err(SizeParseError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(SizeParseError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn parse_size_spec_empty() {
        assert_eq!(parse_size_spec(""), Err(SizeParseError::Empty));
    }

    #[test]
    fn parse_size_spec_just_sign() {
        // upstream: options.c:parse_size_arg() rejects a bare leading '+'.
        assert_eq!(parse_size_spec("+"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("-"), Err(SizeParseError::Empty));
    }

    #[test]
    fn parse_size_spec_negative() {
        assert_eq!(parse_size_spec("-100"), Err(SizeParseError::Negative));
        assert_eq!(parse_size_spec("-1K"), Err(SizeParseError::Negative));
    }

    #[test]
    fn parse_size_spec_plain_number() {
        assert_eq!(parse_size_spec("0"), Ok(0));
        assert_eq!(parse_size_spec("1"), Ok(1));
        assert_eq!(parse_size_spec("100"), Ok(100));
        assert_eq!(parse_size_spec("12345"), Ok(12345));
    }

    #[test]
    fn parse_size_spec_leading_plus_rejected() {
        // upstream rsync rejects a leading '+' for size args: `--max-size=+100
        // is invalid`. Only bare digits (optionally with a suffix) are valid.
        assert_eq!(parse_size_spec("+100"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("+1K"), Err(SizeParseError::Invalid));
    }

    #[test]
    fn parse_size_spec_trailing_adjustment() {
        // upstream: a single trailing "+1"/"-1" adjusts the byte count so that
        // "--max-size=1K-1" (1023) or "1K+1" (1025) can target a boundary.
        assert_eq!(parse_size_spec("1K-1"), Ok(1023));
        assert_eq!(parse_size_spec("1K+1"), Ok(1025));
        assert_eq!(parse_size_spec("1-1"), Ok(0));
        assert_eq!(parse_size_spec("1KB-1"), Ok(999));
        assert_eq!(parse_size_spec("1.5K-1"), Ok(1535));
    }

    #[test]
    fn parse_size_spec_rejects_non_unit_adjustment() {
        // Only exactly "+1"/"-1" is accepted; anything else is invalid, and a
        // "-1" that would drive the size negative is rejected too.
        assert_eq!(parse_size_spec("1K-2"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1K+2"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1K-10"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1K-0"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1K+0"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1K-1x"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("0-1"), Err(SizeParseError::TooLarge));
    }

    #[test]
    fn parse_size_spec_kibibytes() {
        assert_eq!(parse_size_spec("1K"), Ok(1024));
        assert_eq!(parse_size_spec("1k"), Ok(1024));
        assert_eq!(parse_size_spec("2K"), Ok(2048));
        assert_eq!(parse_size_spec("10K"), Ok(10240));
    }

    #[test]
    fn parse_size_spec_kilobytes_decimal() {
        assert_eq!(parse_size_spec("1KB"), Ok(1000));
        assert_eq!(parse_size_spec("1Kb"), Ok(1000));
        assert_eq!(parse_size_spec("2KB"), Ok(2000));
    }

    #[test]
    fn parse_size_spec_kilobytes_binary_explicit() {
        assert_eq!(parse_size_spec("1KiB"), Ok(1024));
        assert_eq!(parse_size_spec("1kib"), Ok(1024));
    }

    #[test]
    fn parse_size_spec_mebibytes() {
        assert_eq!(parse_size_spec("1M"), Ok(1024 * 1024));
        assert_eq!(parse_size_spec("1m"), Ok(1024 * 1024));
    }

    #[test]
    fn parse_size_spec_megabytes_decimal() {
        assert_eq!(parse_size_spec("1MB"), Ok(1000 * 1000));
    }

    #[test]
    fn parse_size_spec_gibibytes() {
        assert_eq!(parse_size_spec("1G"), Ok(1024 * 1024 * 1024));
    }

    #[test]
    fn parse_size_spec_gigabytes_decimal() {
        assert_eq!(parse_size_spec("1GB"), Ok(1000 * 1000 * 1000));
    }

    #[test]
    fn parse_size_spec_tebibytes() {
        assert_eq!(parse_size_spec("1T"), Ok(1024u64.pow(4)));
    }

    #[test]
    fn parse_size_spec_pebibytes() {
        assert_eq!(parse_size_spec("1P"), Ok(1024u64.pow(5)));
    }

    #[test]
    fn parse_size_spec_exa_suffix_rejected() {
        // upstream's suffix switch stops at 'p'/'P'; there is no exa suffix.
        assert_eq!(parse_size_spec("1E"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("1e"), Err(SizeParseError::Invalid));
    }

    #[test]
    fn parse_size_spec_bytes_suffix() {
        assert_eq!(parse_size_spec("100B"), Ok(100));
        assert_eq!(parse_size_spec("100b"), Ok(100));
    }

    #[test]
    fn parse_size_spec_fractional() {
        assert_eq!(parse_size_spec("1.5K"), Ok(1536));
        assert_eq!(parse_size_spec("2.5M"), Ok(2621440));
    }

    #[test]
    fn parse_size_spec_fractional_comma() {
        assert_eq!(parse_size_spec("1,5K"), Ok(1536));
    }

    #[test]
    fn parse_size_spec_invalid_suffix() {
        assert_eq!(parse_size_spec("100X"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("100Q"), Err(SizeParseError::Invalid));
    }

    #[test]
    fn parse_size_spec_invalid_format() {
        assert_eq!(parse_size_spec("abc"), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec("."), Err(SizeParseError::Invalid));
        assert_eq!(parse_size_spec(","), Err(SizeParseError::Invalid));
    }

    #[test]
    fn parse_size_spec_incomplete_binary_suffix() {
        assert_eq!(parse_size_spec("1Ki"), Err(SizeParseError::Invalid));
    }

    #[test]
    fn pow_u128_for_size_zero_exponent() {
        assert_eq!(pow_u128_for_size(1024, 0), Ok(1));
        assert_eq!(pow_u128_for_size(1000, 0), Ok(1));
    }

    #[test]
    fn pow_u128_for_size_one_exponent() {
        assert_eq!(pow_u128_for_size(1024, 1), Ok(1024));
        assert_eq!(pow_u128_for_size(1000, 1), Ok(1000));
    }

    #[test]
    fn pow_u128_for_size_small_exponents() {
        assert_eq!(pow_u128_for_size(1024, 2), Ok(1_048_576));
        assert_eq!(pow_u128_for_size(1000, 3), Ok(1_000_000_000));
    }

    #[test]
    fn size_parse_error_eq() {
        assert_eq!(SizeParseError::Empty, SizeParseError::Empty);
        assert_eq!(SizeParseError::Negative, SizeParseError::Negative);
        assert_eq!(SizeParseError::Invalid, SizeParseError::Invalid);
        assert_eq!(SizeParseError::TooLarge, SizeParseError::TooLarge);
    }

    #[test]
    fn size_parse_error_ne() {
        assert_ne!(SizeParseError::Empty, SizeParseError::Negative);
        assert_ne!(SizeParseError::Invalid, SizeParseError::TooLarge);
    }

    #[test]
    fn size_parse_error_clone() {
        let err = SizeParseError::Empty;
        let cloned = err;
        assert_eq!(err, cloned);
    }

    #[test]
    fn parse_size_limit_argument_valid() {
        assert_eq!(
            parse_size_limit_argument(&os("1K"), "--max-size").unwrap(),
            1024
        );
        assert_eq!(
            parse_size_limit_argument(&os("1M"), "--max-size").unwrap(),
            1024 * 1024
        );
    }

    #[test]
    fn parse_size_limit_argument_empty() {
        assert!(parse_size_limit_argument(&os(""), "--max-size").is_err());
    }

    #[test]
    fn parse_size_limit_argument_negative() {
        assert!(parse_size_limit_argument(&os("-1K"), "--max-size").is_err());
    }

    #[test]
    fn parse_size_limit_argument_invalid() {
        assert!(parse_size_limit_argument(&os("abc"), "--max-size").is_err());
    }

    #[test]
    fn parse_max_alloc_bytes() {
        assert_eq!(
            parse_size_limit_argument(&os("1048576"), "--max-alloc").unwrap(),
            1_048_576
        );
    }

    #[test]
    fn parse_max_alloc_kilobytes() {
        assert_eq!(
            parse_size_limit_argument(&os("512K"), "--max-alloc").unwrap(),
            512 * 1024
        );
    }

    #[test]
    fn parse_max_alloc_megabytes() {
        assert_eq!(
            parse_size_limit_argument(&os("256M"), "--max-alloc").unwrap(),
            256 * 1024 * 1024
        );
    }

    #[test]
    fn parse_max_alloc_gigabytes() {
        assert_eq!(
            parse_size_limit_argument(&os("2G"), "--max-alloc").unwrap(),
            2 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn parse_max_alloc_terabytes() {
        assert_eq!(
            parse_size_limit_argument(&os("1T"), "--max-alloc").unwrap(),
            1024u64.pow(4)
        );
    }

    #[test]
    fn parse_max_alloc_zero() {
        assert_eq!(
            parse_size_limit_argument(&os("0"), "--max-alloc").unwrap(),
            0
        );
    }

    #[test]
    fn parse_max_alloc_fractional() {
        assert_eq!(
            parse_size_limit_argument(&os("1.5G"), "--max-alloc").unwrap(),
            1_610_612_736
        );
    }

    #[test]
    fn parse_max_alloc_empty() {
        assert!(parse_size_limit_argument(&os(""), "--max-alloc").is_err());
    }

    #[test]
    fn parse_max_alloc_negative() {
        assert!(parse_size_limit_argument(&os("-1M"), "--max-alloc").is_err());
    }

    #[test]
    fn parse_max_alloc_invalid_suffix() {
        assert!(parse_size_limit_argument(&os("100X"), "--max-alloc").is_err());
    }

    #[test]
    fn parse_max_alloc_non_numeric() {
        assert!(parse_size_limit_argument(&os("abc"), "--max-alloc").is_err());
    }

    #[test]
    fn parse_max_alloc_error_mentions_flag_name() {
        let err = parse_size_limit_argument(&os("garbage"), "--max-alloc").unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("--max-alloc"),
            "error should mention --max-alloc, got: {rendered}"
        );
    }

    #[test]
    fn parse_max_alloc_argument_valid_gigabyte() {
        assert_eq!(
            parse_max_alloc_argument(&os("1G")).unwrap(),
            1024 * 1024 * 1024
        );
    }

    #[test]
    fn parse_max_alloc_argument_valid_megabyte() {
        assert_eq!(
            parse_max_alloc_argument(&os("512M")).unwrap(),
            512 * 1024 * 1024
        );
    }

    #[test]
    fn parse_max_alloc_argument_valid_kilobyte() {
        assert_eq!(parse_max_alloc_argument(&os("1024K")).unwrap(), 1024 * 1024);
    }

    #[test]
    fn parse_max_alloc_argument_valid_plain_bytes() {
        assert_eq!(parse_max_alloc_argument(&os("1024")).unwrap(), 1024);
    }

    #[test]
    fn parse_max_alloc_argument_rejects_zero() {
        let err = parse_max_alloc_argument(&os("0")).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("must be greater than zero"),
            "expected zero-rejection error, got: {rendered}"
        );
    }

    #[test]
    fn parse_max_alloc_argument_rejects_invalid() {
        assert!(parse_max_alloc_argument(&os("garbage")).is_err());
        assert!(parse_max_alloc_argument(&os("100X")).is_err());
        assert!(parse_max_alloc_argument(&os("")).is_err());
        assert!(parse_max_alloc_argument(&os("-1G")).is_err());
    }

    #[test]
    fn parse_max_alloc_argument_rejects_above_ceiling() {
        // u64::MAX expressed as bytes overflows the ceiling.
        let value = format!("{}", u64::MAX);
        let err = parse_max_alloc_argument(&os(&value)).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains("exceeds the supported range"),
            "expected range error, got: {rendered}"
        );
    }

    #[test]
    fn parse_max_alloc_argument_accepts_ceiling() {
        let value = format!("{MAX_ALLOC_CEILING}");
        assert_eq!(
            parse_max_alloc_argument(&os(&value)).unwrap(),
            MAX_ALLOC_CEILING
        );
    }

    #[test]
    fn parse_block_size_argument_valid() {
        let result = parse_block_size_argument(&os("1K")).unwrap();
        assert_eq!(result.get(), 1024);
    }

    #[test]
    fn parse_block_size_argument_small() {
        let result = parse_block_size_argument(&os("512")).unwrap();
        assert_eq!(result.get(), 512);
    }

    #[test]
    fn parse_block_size_argument_zero() {
        assert!(parse_block_size_argument(&os("0")).is_err());
    }

    #[test]
    fn parse_block_size_argument_empty() {
        assert!(parse_block_size_argument(&os("")).is_err());
    }

    #[test]
    fn parse_block_size_argument_negative() {
        assert!(parse_block_size_argument(&os("-1")).is_err());
    }

    #[test]
    fn parse_decimal_components_integer_only() {
        let (integer, fraction, denominator) = parse_decimal_components("123").unwrap();
        assert_eq!(integer, 123);
        assert_eq!(fraction, 0);
        assert_eq!(denominator, 1);
    }

    #[test]
    fn parse_decimal_components_with_fraction() {
        let (integer, fraction, denominator) = parse_decimal_components("1.5").unwrap();
        assert_eq!(integer, 1);
        assert_eq!(fraction, 5);
        assert_eq!(denominator, 10);
    }

    #[test]
    fn parse_decimal_components_with_comma() {
        let (integer, fraction, denominator) = parse_decimal_components("2,25").unwrap();
        assert_eq!(integer, 2);
        assert_eq!(fraction, 25);
        assert_eq!(denominator, 100);
    }

    #[test]
    fn parse_decimal_components_zero_fraction() {
        let (integer, fraction, denominator) = parse_decimal_components("10.0").unwrap();
        assert_eq!(integer, 10);
        assert_eq!(fraction, 0);
        assert_eq!(denominator, 10);
    }

    #[test]
    fn parse_decimal_components_multiple_decimal_points() {
        let result = parse_decimal_components("1.2.3");
        assert!(result.is_err());
    }
}
