//! Canonical size-argument parser shared across every size-with-suffix option.
//!
//! Upstream rsync funnels `--bwlimit`, `--max-size`, `--min-size`,
//! `--block-size` and `--max-alloc` through a single `options.c:parse_size_arg()`
//! parameterised by a default suffix (`def_suf`): `'b'` for the byte-oriented
//! size limits and `'K'` for `--bwlimit`. This module is the equivalent single
//! source of truth: the `--bwlimit` parser in this crate and the CLI size-limit
//! handlers both call [`parse_size_arg`], each layering only its own
//! range/rounding/message policy on top.
//!
//! Scientific notation is intentionally rejected. Upstream scans digits and a
//! single decimal separator, then treats the next character as the suffix; a
//! trailing `e`/`E` therefore hits the suffix switch's default case and fails.

/// Error returned by [`parse_size_arg`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeArgError {
    /// Malformed input or an unrecognised suffix.
    Invalid,
    /// The value overflows the supported range, or a `-1` adjustment would
    /// drive it below zero (which upstream reports as "too large").
    TooLarge,
}

/// Successful decomposition of a size argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedSize {
    /// Byte count after suffix scaling and the optional `+1`/`-1` adjustment.
    pub bytes: u128,
    /// Unit granularity implied by the suffix: `1` for a byte suffix, `1000`
    /// for a decimal suffix (`KB`/`MB`/...), `1024` otherwise. Reported for
    /// callers that need the suffix granularity; the byte-oriented size limits
    /// and `--bwlimit` (which quantizes to whole KiB) do not consume it.
    pub unit: u128,
}

/// Checked integer exponentiation used to build the suffix multiplier.
// upstream: options.c:parse_size_arg() - `while (reps--) size *= mult`
pub fn pow_u128(base: u32, exponent: u32) -> Result<u128, SizeArgError> {
    let mut result = 1u128;
    let mut factor = u128::from(base);
    let mut exp = exponent;

    while exp > 0 {
        if exp & 1 == 1 {
            result = result.checked_mul(factor).ok_or(SizeArgError::TooLarge)?;
        }
        exp >>= 1;
        if exp > 0 {
            factor = factor.checked_mul(factor).ok_or(SizeArgError::TooLarge)?;
        }
    }

    Ok(result)
}

/// Parses a size argument with an optional unit suffix, mirroring upstream
/// rsync's `options.c:parse_size_arg()`.
///
/// `text` must already have any caller-specific leading sign stripped. The
/// grammar is an unsigned decimal mantissa (digits with at most one `.`/`,`
/// separator), an optional suffix (`B`/`K`/`M`/`G`/`T`/`P`, optionally trailed
/// by `B` for a decimal multiplier or `iB` for an explicit binary one), and a
/// single trailing `+1`/`-1` byte adjustment. `def_suf` supplies the suffix
/// used when none is present (`b'b'` for byte-oriented limits, `b'K'` for
/// `--bwlimit`).
///
/// # Errors
///
/// Returns [`SizeArgError::Invalid`] for malformed input or an unrecognised
/// suffix, and [`SizeArgError::TooLarge`] on overflow or a negative result.
pub fn parse_size_arg(text: &str, def_suf: u8) -> Result<ParsedSize, SizeArgError> {
    // upstream: for (arg = size_arg; isDigit(arg); arg++); one '.'/',' fraction.
    let bytes = text.as_bytes();
    let mut digits_seen = false;
    let mut decimal_seen = false;
    let mut numeric_end = bytes.len();

    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'0'..=b'9' => digits_seen = true,
            b'.' | b',' if !decimal_seen => decimal_seen = true,
            _ => {
                numeric_end = index;
                break;
            }
        }
    }

    let numeric_part = &text[..numeric_end];
    let remainder = &text[numeric_end..];

    if !digits_seen || numeric_part == "." || numeric_part == "," {
        return Err(SizeArgError::Invalid);
    }

    let (integer, fraction, denominator) = parse_decimal(numeric_part)?;

    // upstream: switch (*arg && *arg != '+' && *arg != '-' ? *arg++ : def_suf)
    let (suffix, mut rest) = if remainder.is_empty() || remainder.starts_with(['+', '-']) {
        (def_suf, remainder)
    } else {
        let first = remainder.as_bytes()[0];
        if !first.is_ascii() {
            return Err(SizeArgError::Invalid);
        }
        (first, &remainder[1..])
    };

    let normalized = suffix.to_ascii_lowercase();
    let reps = match normalized {
        b'b' => 0u32,
        b'k' => 1,
        b'm' => 2,
        b'g' => 3,
        b't' => 4,
        b'p' => 5,
        _ => return Err(SizeArgError::Invalid),
    };

    // upstream: 'b'/'B' -> mult 1000; end/'+'/'-' -> 1024; "ib" -> 1024.
    let mut base = 1024u32;
    let mut unit: u128 = if normalized == b'b' { 1 } else { 1024 };
    if !rest.is_empty() {
        let rest_bytes = rest.as_bytes();
        match rest_bytes[0] {
            b'b' | b'B' => {
                base = 1000;
                unit = 1000;
                rest = &rest[1..];
            }
            b'i' | b'I' => {
                if rest_bytes.len() < 2 || !matches!(rest_bytes[1], b'b' | b'B') {
                    return Err(SizeArgError::Invalid);
                }
                base = 1024;
                rest = &rest[2..];
            }
            b'+' | b'-' => {}
            _ => return Err(SizeArgError::Invalid),
        }
    }

    // upstream: (*arg == '+' || *arg == '-') && arg[1] == '1' && arg != size_arg.
    let adjust: i8 = match rest.as_bytes() {
        [b'+', b'1'] => {
            rest = "";
            1
        }
        [b'-', b'1'] => {
            rest = "";
            -1
        }
        _ => 0,
    };

    if !rest.is_empty() {
        return Err(SizeArgError::Invalid);
    }

    let scale = pow_u128(base, reps)?;
    let numerator = integer
        .checked_mul(denominator)
        .and_then(|value| value.checked_add(fraction))
        .ok_or(SizeArgError::TooLarge)?;
    let product = numerator.checked_mul(scale).ok_or(SizeArgError::TooLarge)?;
    let value = product / denominator;

    // upstream: size += atoi(arg); if (size < 0) reports "too large".
    let bytes = match adjust {
        1 => value.checked_add(1).ok_or(SizeArgError::TooLarge)?,
        -1 => value.checked_sub(1).ok_or(SizeArgError::TooLarge)?,
        _ => value,
    };

    Ok(ParsedSize { bytes, unit })
}

/// Splits a decimal mantissa into `(integer, fraction, denominator)` so the
/// value is `integer + fraction / denominator`. Accepts `.` or `,` as the
/// separator; rejects a second separator or any non-digit character.
fn parse_decimal(text: &str) -> Result<(u128, u128, u128), SizeArgError> {
    let mut integer = 0u128;
    let mut fraction = 0u128;
    let mut denominator = 1u128;
    let mut saw_decimal = false;

    for &byte in text.as_bytes() {
        match byte {
            b'0'..=b'9' => {
                let digit = u128::from(byte - b'0');
                if saw_decimal {
                    denominator = denominator.checked_mul(10).ok_or(SizeArgError::TooLarge)?;
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeArgError::TooLarge)?;
                } else {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|value| value.checked_add(digit))
                        .ok_or(SizeArgError::TooLarge)?;
                }
            }
            b'.' | b',' => {
                if saw_decimal {
                    return Err(SizeArgError::Invalid);
                }
                saw_decimal = true;
            }
            _ => return Err(SizeArgError::Invalid),
        }
    }

    Ok((integer, fraction, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(text: &str, def_suf: u8) -> Result<u128, SizeArgError> {
        parse_size_arg(text, def_suf).map(|parsed| parsed.bytes)
    }

    #[test]
    fn byte_default_matches_size_limit_semantics() {
        assert_eq!(bytes("0", b'b'), Ok(0));
        assert_eq!(bytes("1024", b'b'), Ok(1024));
        assert_eq!(bytes("1K", b'b'), Ok(1024));
        assert_eq!(bytes("1KB", b'b'), Ok(1000));
        assert_eq!(bytes("1KiB", b'b'), Ok(1024));
        assert_eq!(bytes("1M", b'b'), Ok(1024 * 1024));
        assert_eq!(bytes("1.5K", b'b'), Ok(1536));
        assert_eq!(bytes("1,5K", b'b'), Ok(1536));
        assert_eq!(bytes("1P", b'b'), Ok(1024u128.pow(5)));
    }

    #[test]
    fn kilo_default_matches_bwlimit_semantics() {
        assert_eq!(bytes("100", b'K'), Ok(102_400));
        assert_eq!(bytes("1", b'K'), Ok(1024));
        assert_eq!(bytes("1b", b'K'), Ok(1));
    }

    #[test]
    fn trailing_unit_adjustment() {
        assert_eq!(bytes("1K-1", b'b'), Ok(1023));
        assert_eq!(bytes("1K+1", b'b'), Ok(1025));
        assert_eq!(bytes("1-1", b'b'), Ok(0));
        assert_eq!(bytes("1KB-1", b'b'), Ok(999));
    }

    #[test]
    fn negative_result_reports_too_large() {
        assert_eq!(bytes("0-1", b'b'), Err(SizeArgError::TooLarge));
        assert_eq!(bytes("0-1", b'K'), Err(SizeArgError::TooLarge));
    }

    #[test]
    fn scientific_notation_is_rejected() {
        // upstream's suffix switch treats a trailing 'e'/'E' as an unknown
        // suffix and fails; there is no scientific-notation support.
        for text in ["1e3", "1e3K", "2.5e2K", "1e-1M", "1E3", "1.e2K"] {
            assert_eq!(bytes(text, b'K'), Err(SizeArgError::Invalid), "{text}");
        }
    }

    #[test]
    fn unrecognised_suffix_and_exa_are_rejected() {
        assert_eq!(bytes("1E", b'b'), Err(SizeArgError::Invalid));
        assert_eq!(bytes("100X", b'b'), Err(SizeArgError::Invalid));
        assert_eq!(bytes("1Ki", b'b'), Err(SizeArgError::Invalid));
    }

    #[test]
    fn empty_and_signed_prefixes_are_invalid() {
        assert_eq!(bytes("", b'b'), Err(SizeArgError::Invalid));
        assert_eq!(bytes("+100", b'b'), Err(SizeArgError::Invalid));
        assert_eq!(bytes(".", b'b'), Err(SizeArgError::Invalid));
        assert_eq!(bytes(",", b'b'), Err(SizeArgError::Invalid));
    }

    #[test]
    fn unit_reports_suffix_granularity() {
        assert_eq!(parse_size_arg("100", b'K').unwrap().unit, 1024);
        assert_eq!(parse_size_arg("1b", b'K').unwrap().unit, 1);
        assert_eq!(parse_size_arg("1KB", b'K').unwrap().unit, 1000);
        assert_eq!(parse_size_arg("1KiB", b'K').unwrap().unit, 1024);
    }

    #[test]
    fn pow_u128_examples() {
        assert_eq!(pow_u128(1024, 0), Ok(1));
        assert_eq!(pow_u128(1024, 2), Ok(1_048_576));
        assert_eq!(pow_u128(1000, 3), Ok(1_000_000_000));
        assert_eq!(pow_u128(1024, 100), Err(SizeArgError::TooLarge));
    }
}
