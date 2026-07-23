//! Size specification parsing for arguments with optional unit suffixes.
//!
//! Handles `--block-size`, `--max-size`, `--min-size`, and `--max-alloc` arguments.
//! Supports binary (K/M/G/T/P = powers of 1024) and decimal (KB/MB/GB = powers of 1000)
//! suffixes, as well as explicit binary suffixes (KiB/MiB/GiB).
//! Mirrors upstream rsync's size parsing behavior.

use std::ffi::OsStr;
use std::num::NonZeroU32;

use bandwidth::{SizeArgError, parse_size_arg};
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

impl From<SizeArgError> for SizeParseError {
    fn from(error: SizeArgError) -> Self {
        match error {
            SizeArgError::Invalid => SizeParseError::Invalid,
            SizeArgError::TooLarge => SizeParseError::TooLarge,
        }
    }
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
/// without risking arithmetic overflow on 64-bit platforms. Upstream imposes
/// no upper bound; this guard is oc-specific overflow protection.
pub(crate) const MAX_ALLOC_CEILING: u64 = u64::MAX / 4;

/// Lower bound for a non-zero `--max-alloc`, mirroring upstream's
/// `parse_size_arg(arg, 'B', "max-alloc", 1024*1024, -1, True)` min value.
///
/// upstream: options.c:1960.
const MAX_ALLOC_MIN: u64 = 1024 * 1024;

/// Sentinel returned for `--max-alloc=0`, meaning "unlimited".
///
/// upstream: options.c:1966 `if (!max_alloc) max_alloc = SIZE_MAX;` - a parsed
/// value of zero disables the ceiling entirely. Callers resolve this to the
/// platform's `usize::MAX` before publishing it.
pub(crate) const MAX_ALLOC_UNLIMITED: u64 = 0;

/// Upper bound for `--block-size` at protocol >= 30.
///
/// upstream: rsync.h:161 `#define MAX_BLOCK_SIZE ((int32)1 << 17)` (131072),
/// enforced by options.c:1692-1695 `parse_size_arg(arg, 'b', "block-size", 0,
/// max_blength, False)`.
const MAX_BLOCK_SIZE: u64 = 1 << 17;

/// Parses the `--max-alloc` argument as a byte ceiling.
///
/// Mirrors upstream rsync's `parse_size_arg(arg, 'B', "max-alloc", 1024*1024,
/// -1, True)` (options.c:1960) followed by `if (!max_alloc) max_alloc =
/// SIZE_MAX;` (options.c:1966):
///
/// - `0` is accepted and returned verbatim ([`MAX_ALLOC_UNLIMITED`]); callers
///   resolve it to an unlimited ceiling.
/// - A non-zero value below 1 MiB is rejected ("too small").
/// - Empty, negative, and non-numeric input is rejected.
/// - Values above [`MAX_ALLOC_CEILING`] are rejected for overflow safety
///   (an oc-specific guard; upstream has no upper bound).
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

    // upstream: options.c:1966 - a zero value means unlimited (SIZE_MAX).
    if limit == MAX_ALLOC_UNLIMITED {
        return Ok(MAX_ALLOC_UNLIMITED);
    }

    // upstream: options.c:1960,1120-1127 - parse_size_arg rejects a non-zero
    // value below the 1 MiB minimum with "is too small (min: 1.00M or 0 for
    // unlimited)". do_big_num renders the constant 1 MiB minimum as "1.00M".
    if limit < MAX_ALLOC_MIN {
        return Err(rsync_error!(
            1,
            format!("--max-alloc={display} is too small (min: 1.00M or 0 for unlimited)")
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

/// Parses the `--block-size` argument into an optional override.
///
/// Mirrors upstream rsync's `parse_size_arg(arg, 'b', "block-size", 0,
/// MAX_BLOCK_SIZE, False)` (options.c:1692-1695):
///
/// - `0` is accepted and yields `None`, falling back to the negotiated default
///   block size (upstream stores `block_size = 0`, later replaced with the
///   computed default).
/// - A value above [`MAX_BLOCK_SIZE`] (131072 at protocol >= 30) is rejected
///   with "is too large (max: 128.00K)".
/// - Empty, negative, and non-numeric input is rejected.
pub(crate) fn parse_block_size_argument(value: &OsStr) -> Result<Option<NonZeroU32>, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    let limit = parse_size_limit_argument(value, "--block-size")?;

    // upstream: options.c:1692-1695 - min_value 0 accepts `--block-size=0`,
    // which stores block_size = 0 and later falls back to the default.
    if limit == 0 {
        return Ok(None);
    }

    // upstream: options.c:1692-1695,1116-1119 - a value above MAX_BLOCK_SIZE is
    // rejected with "is too large (max: ...)". do_big_num renders the constant
    // 131072 ceiling as "128.00K".
    if limit > MAX_BLOCK_SIZE {
        return Err(rsync_error!(
            1,
            format!("--block-size={display} is too large (max: 128.00K)")
        )
        .with_role(Role::Client));
    }

    let block_size = u32::try_from(limit).expect("value <= MAX_BLOCK_SIZE fits in u32");
    Ok(Some(
        NonZeroU32::new(block_size).expect("non-zero checked above"),
    ))
}

/// Parses a size specification string into a byte count.
///
/// Delegates the numeric-and-suffix grammar to the shared
/// [`bandwidth::parse_size_arg`] (upstream's single `options.c:parse_size_arg()`)
/// with the byte default suffix used by the size limits, and layers the CLI's
/// sign diagnostics and 64-bit narrowing on top. Supports plain integers,
/// fractional values (`.`/`,`), binary suffixes (K/M/G/T/P), decimal suffixes
/// (KB/MB/...), explicit binary suffixes (KiB/...), the byte suffix `B`, and a
/// single trailing `+1`/`-1` adjustment. A leading `+` is rejected and there is
/// no exa (`E`) suffix, matching upstream's suffix switch which stops at `P`.
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

    let parsed = parse_size_arg(unsigned, b'b').map_err(SizeParseError::from)?;
    u64::try_from(parsed.bytes).map_err(|_| SizeParseError::TooLarge)
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
        // 1024K == 1 MiB, exactly the upstream minimum (options.c:1960).
        assert_eq!(parse_max_alloc_argument(&os("1024K")).unwrap(), 1024 * 1024);
    }

    #[test]
    fn parse_max_alloc_argument_accepts_zero_as_unlimited() {
        // upstream: options.c:1966 `if (!max_alloc) max_alloc = SIZE_MAX;` - a
        // zero value is accepted and means unlimited, not an error.
        assert_eq!(
            parse_max_alloc_argument(&os("0")).unwrap(),
            MAX_ALLOC_UNLIMITED
        );
    }

    #[test]
    fn parse_max_alloc_argument_rejects_below_one_mib() {
        // upstream: options.c:1960 - parse_size_arg min value is 1 MiB, so a
        // non-zero value below it ("512K", 1024 bytes) is "too small".
        for value in ["1024", "512K"] {
            let err = parse_max_alloc_argument(&os(value)).unwrap_err();
            let rendered = err.to_string();
            assert!(
                rendered.contains("is too small (min: 1.00M or 0 for unlimited)"),
                "expected too-small error for {value}, got: {rendered}"
            );
        }
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
        let result = parse_block_size_argument(&os("1K")).unwrap().unwrap();
        assert_eq!(result.get(), 1024);
    }

    #[test]
    fn parse_block_size_argument_small() {
        let result = parse_block_size_argument(&os("512")).unwrap().unwrap();
        assert_eq!(result.get(), 512);
    }

    #[test]
    fn parse_block_size_argument_zero_falls_back_to_default() {
        // upstream: options.c:1692-1695 - `--block-size=0` passes the min_value
        // 0 check and stores block_size = 0, which falls back to the default.
        assert_eq!(parse_block_size_argument(&os("0")).unwrap(), None);
    }

    #[test]
    fn parse_block_size_argument_accepts_maximum() {
        // upstream: rsync.h:161 MAX_BLOCK_SIZE == 131072 is the inclusive cap.
        let result = parse_block_size_argument(&os("131072")).unwrap().unwrap();
        assert_eq!(result.get(), 131072);
    }

    #[test]
    fn parse_block_size_argument_rejects_above_maximum() {
        // upstream: options.c:1692-1695 - a value above MAX_BLOCK_SIZE is "too
        // large (max: 128.00K)".
        let err = parse_block_size_argument(&os("200000")).unwrap_err();
        assert!(
            err.to_string().contains("is too large (max: 128.00K)"),
            "expected too-large error, got: {err}"
        );
    }

    #[test]
    fn parse_block_size_argument_empty() {
        assert!(parse_block_size_argument(&os("")).is_err());
    }

    #[test]
    fn parse_block_size_argument_negative() {
        assert!(parse_block_size_argument(&os("-1")).is_err());
    }
}
