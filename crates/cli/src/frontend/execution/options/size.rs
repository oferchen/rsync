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
/// Maps an empty value to `0`, mirroring what upstream's `parse_size_arg` does
/// with an empty string.
///
/// upstream: options.c:1178-1181 - the digit scan leaves `arg` on the string
/// terminator, so the suffix switch takes `def_suf` and `strtod("")` yields 0.
///
/// Applied PER OPTION rather than inside the shared string parser, because
/// whether the resulting 0 is legal is decided by that option's own
/// `min_value`: legal for `--block-size`, `--min-size` and `--max-size`
/// (min 0 - options.c:1808, :1809, :1815), and for `--bwlimit` and
/// `--max-alloc` (`unlimited_0`, :1821, :2073). Folding the rule into
/// `parse_size_spec` would silently start accepting an empty value for an
/// option whose minimum excludes 0.
///
/// Measured against rsync 3.5.0: an empty value behaves exactly like `=0` on
/// each option - and for `--max-size` that EXCLUDES every non-empty file
/// rather than meaning "no limit", so this is not a "fall back to the
/// default" rule.
pub(crate) fn empty_size_means_zero(value: &OsStr) -> &OsStr {
    if value
        .to_string_lossy()
        .trim_matches(|ch: char| ch.is_ascii_whitespace())
        .is_empty()
    {
        return OsStr::new("0");
    }
    value
}

pub(crate) fn parse_size_limit_argument(value: &OsStr, flag: &str) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    parse_size_spec(trimmed).map_err(|error| size_parse_error(flag, display, error))
}

/// Renders a size parse failure as the client diagnostic for `flag`.
fn size_parse_error(flag: &str, display: &str, error: SizeParseError) -> Message {
    let text = match error {
        SizeParseError::Empty => format!("{flag} value must not be empty"),
        SizeParseError::Negative => {
            format!("invalid {flag} '{display}': size must be non-negative")
        }
        SizeParseError::Invalid => {
            format!("invalid {flag} '{display}': expected a size with an optional K/M/G/T/P suffix")
        }
        SizeParseError::TooLarge => {
            format!("invalid {flag} '{display}': size exceeds the supported range")
        }
    };
    rsync_error!(1, text).with_role(Role::Client)
}

// The `--max-alloc` value rules and their constants live in
// `protocol::max_alloc`, because the daemon's client-argv parser applies the
// same rules to a peer-forwarded value; see `validate_max_alloc` there.

/// Upper bound for `--block-size` at protocol >= 30.
///
/// upstream: rsync.h:161 `#define MAX_BLOCK_SIZE ((int32)1 << 17)` (131072),
/// enforced by options.c:1698-1701 `parse_size_arg(arg, 'b', "block-size", 0,
/// max_blength, False)`.
const MAX_BLOCK_SIZE: u64 = 1 << 17;

/// Parses the `--max-alloc` argument as a byte ceiling.
///
/// Mirrors upstream rsync's `parse_size_arg(arg, 'B', "max-alloc", 1024*1024,
/// -1, True)` and the resolution of 0 that follows it in `parse_arguments()`
/// (options.c:2072-2086):
///
/// - `0` means the largest limit this build supports and resolves to
///   `protocol::max_alloc::SIZE_ARG_MAX`.
/// - A non-zero value below 1 MiB is rejected ("too small").
/// - A value that reaches `SIZE_ARG_MAX`, or overflows while being scaled, is
///   rejected ("too large").
/// - Empty, negative, and non-numeric input is rejected.
///
/// # Errors
///
/// Returns a `Message` with role [`Role::Client`] and exit code 1 on any
/// rejection, matching upstream's diagnostic style.
pub(crate) fn parse_max_alloc_argument(value: &OsStr) -> Result<u64, Message> {
    let value = empty_size_means_zero(value);
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    let limit = match parse_size_spec(trimmed) {
        Ok(limit) => limit,
        // upstream: options.c:1211-1216 - a value that overflows while being
        // scaled is "too large", the same verdict as one past the ceiling, so
        // hand the shared rule a value it rejects on that ground.
        Err(SizeParseError::TooLarge) => u64::MAX,
        Err(error) => return Err(size_parse_error("--max-alloc", display, error)),
    };

    // The zero / too-small / too-large rules are NOT restated here: the daemon
    // applies the identical block to a peer-forwarded `--max-alloc`, and
    // upstream runs one `parse_arguments()` body on both ends
    // (options.c:2067-2086). This call site only adapts the shared owner's text
    // into the client-role `Message` shape.
    ::protocol::max_alloc::validate_max_alloc(limit, display)
        .map_err(|text| rsync_error!(1, text).with_role(Role::Client))
}

/// Parses the `--block-size` argument into an optional override.
///
/// Mirrors upstream rsync's `parse_size_arg(arg, 'b', "block-size", 0,
/// MAX_BLOCK_SIZE, False)` (options.c:1698-1701):
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

    let limit = parse_size_limit_argument(empty_size_means_zero(value), "--block-size")?;

    // upstream: options.c:1698-1701 - min_value 0 accepts `--block-size=0`,
    // which stores block_size = 0 and later falls back to the default.
    if limit == 0 {
        return Ok(None);
    }

    // upstream: options.c:1698-1701,1116-1119 - a value above MAX_BLOCK_SIZE is
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
    // The rule's owner; these tests assert against ITS constants, so a change
    // there cannot leave a stale copy asserting the old bound here.
    use ::protocol::max_alloc::SIZE_ARG_MAX;
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
        // 1024K == 1 MiB, exactly the upstream minimum (options.c:1966).
        assert_eq!(parse_max_alloc_argument(&os("1024K")).unwrap(), 1024 * 1024);
    }

    #[test]
    fn parse_max_alloc_argument_resolves_zero_to_the_ceiling() {
        // upstream: options.c:2085-2086 - every spelling of zero means "the
        // largest limit this build supports", SIZE_MAX/2, which keeps the
        // my_alloc() ceiling bounded.
        for value in ["0", "0B", "0K", "0.0M"] {
            assert_eq!(
                parse_max_alloc_argument(&os(value)).unwrap(),
                SIZE_ARG_MAX,
                "--max-alloc={value}"
            );
        }
    }

    #[test]
    fn parse_max_alloc_argument_rejects_below_one_mib() {
        // upstream: options.c:2073 - parse_size_arg min value is 1 MiB, so a
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
        assert!(parse_max_alloc_argument(&os("-1G")).is_err());
    }

    #[test]
    fn parse_max_alloc_argument_rejects_the_ceiling() {
        // upstream: options.c:1221-1227 - reaching SIZE_ARG_MAX is "too large".
        let value = format!("{SIZE_ARG_MAX}");
        let err = parse_max_alloc_argument(&os(&value)).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("--max-alloc={value} is too large")),
            "expected upstream's too-large text, got: {err}"
        );
    }

    #[test]
    fn parse_max_alloc_argument_accepts_just_below_the_ceiling() {
        // The largest value the double comparison lets through: the ceiling
        // less one ulp of f64 at that magnitude.
        let below = SIZE_ARG_MAX - 2048;
        assert_eq!(
            parse_max_alloc_argument(&os(&below.to_string())).unwrap(),
            below
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
        // upstream: options.c:1698-1701 - `--block-size=0` passes the min_value
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
        // upstream: options.c:1698-1701 - a value above MAX_BLOCK_SIZE is "too
        // large (max: 128.00K)".
        let err = parse_block_size_argument(&os("200000")).unwrap_err();
        assert!(
            err.to_string().contains("is too large (max: 128.00K)"),
            "expected too-large error, got: {err}"
        );
    }

    /// An empty value resolves to 0, which for `--block-size` means "no
    /// override, use the default" - the same `Ok(None)` that `=0` yields.
    ///
    /// This previously asserted a rejection, pinning oc's divergence: upstream
    /// accepts the empty spelling (options.c:1178-1181 + :1802, min 0).
    #[test]
    fn parse_block_size_argument_empty_resolves_like_zero() {
        assert_eq!(
            parse_block_size_argument(&os("")).expect("empty is accepted"),
            parse_block_size_argument(&os("0")).expect("zero is accepted"),
        );
        assert!(
            parse_block_size_argument(&os(""))
                .expect("empty is accepted")
                .is_none()
        );
    }

    #[test]
    fn parse_block_size_argument_negative() {
        assert!(parse_block_size_argument(&os("-1")).is_err());
    }
}
