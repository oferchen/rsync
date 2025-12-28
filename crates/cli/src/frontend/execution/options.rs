use std::ffi::OsStr;
use std::num::{IntErrorKind, NonZeroU32, NonZeroU64};
use std::str::FromStr;

use core::{
    client::{
        FEATURE_UNAVAILABLE_EXIT_CODE, HumanReadableMode, IconvParseError, IconvSetting,
        PROTOCOL_INCOMPATIBLE_EXIT_CODE, TransferTimeout,
    },
    message::{Message, Role},
    rsync_error,
};
use protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};

pub(crate) fn parse_protocol_version_arg(value: &OsStr) -> Result<ProtocolVersion, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match ProtocolVersion::from_str(text.as_ref()) {
        Ok(version) => Ok(version),
        Err(error) => {
            let supported = supported_protocols_list();
            // Mirror upstream: syntax errors (non-numeric) use exit code 1,
            // protocol incompatibility (numeric but out of range) uses exit code 2
            let (exit_code, detail) = match error.kind() {
                ParseProtocolVersionErrorKind::Empty => (
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    "protocol value must not be empty".to_owned(),
                ),
                ParseProtocolVersionErrorKind::InvalidDigit => (
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    "protocol version must be an unsigned integer".to_owned(),
                ),
                ParseProtocolVersionErrorKind::Negative => (
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    "protocol version cannot be negative".to_owned(),
                ),
                ParseProtocolVersionErrorKind::Overflow => (
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    "protocol version value exceeds 255".to_owned(),
                ),
                ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                    (
                        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
                        format!(
                            "protocol version {value} is outside the supported range {oldest}-{newest}"
                        ),
                    )
                }
            };
            use std::fmt::Write;

            let mut full_detail = detail;
            if !full_detail.is_empty() {
                full_detail.push_str("; ");
            }
            // write! to String is infallible
            let _ = write!(full_detail, "supported protocols are {supported}");

            Err(rsync_error!(
                exit_code,
                format!("invalid protocol version '{display}': {full_detail}")
            )
            .with_role(Role::Client))
        }
    }
}

const fn supported_protocols_list() -> &'static str {
    ProtocolVersion::supported_protocol_numbers_display()
}

pub(crate) fn parse_timeout_argument(value: &OsStr) -> Result<TransferTimeout, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "timeout value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid timeout '{}': timeout must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(0) => Ok(TransferTimeout::Disabled),
        Ok(value) => Ok(TransferTimeout::Seconds(
            NonZeroU64::new(value).expect("non-zero ensured"),
        )),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "timeout must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "timeout value exceeds the supported range"
                }
                IntErrorKind::Empty => "timeout value must not be empty",
                _ => "timeout value is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid timeout '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
}

pub(crate) fn parse_human_readable_level(value: &OsStr) -> Result<HumanReadableMode, clap::Error> {
    let text = value.to_string_lossy();
    HumanReadableMode::parse(text.as_ref())
        .map_err(|error| clap::Error::raw(clap::error::ErrorKind::InvalidValue, error.to_string()))
}

pub(crate) fn parse_max_delete_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--max-delete value must not be empty").with_role(Role::Client));
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --max-delete '{}': deletion limit must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "deletion limit must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "deletion limit exceeds the supported range"
                }
                IntErrorKind::Empty => "--max-delete value must not be empty",
                _ => "deletion limit is invalid",
            };
            Err(
                rsync_error!(1, format!("invalid --max-delete '{}': {}", display, detail))
                    .with_role(Role::Client),
            )
        }
    }
}

pub(crate) fn parse_checksum_seed_argument(value: &OsStr) -> Result<u32, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--checksum-seed value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    normalized.parse::<u32>().map_err(|_| {
        rsync_error!(
            1,
            format!(
                "invalid --checksum-seed value '{}': must be between 0 and {}",
                display,
                u32::MAX
            )
        )
        .with_role(Role::Client)
    })
}

pub(crate) fn resolve_iconv_setting(
    spec: Option<&OsStr>,
    disable: bool,
) -> Result<IconvSetting, Message> {
    if let Some(value) = spec {
        let text = value.to_string_lossy();
        match IconvSetting::parse(text.as_ref()) {
            Ok(setting) => Ok(setting),
            Err(error) => {
                let detail = match error {
                    IconvParseError::EmptySpecification => {
                        "--iconv value must not be empty".to_owned()
                    }
                    IconvParseError::MissingLocalCharset => {
                        "--iconv specification is missing the local charset".to_owned()
                    }
                    IconvParseError::MissingRemoteCharset => {
                        "--iconv specification is missing the remote charset".to_owned()
                    }
                };
                Err(rsync_error!(1, detail).with_role(Role::Client))
            }
        }
    } else if disable {
        Ok(IconvSetting::Disabled)
    } else {
        Ok(IconvSetting::Unspecified)
    }
}

pub(crate) fn parse_modify_window_argument(value: &OsStr) -> Result<u64, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    if trimmed.is_empty() {
        return Err(
            rsync_error!(1, "--modify-window value must not be empty").with_role(Role::Client)
        );
    }

    if trimmed.starts_with('-') {
        return Err(rsync_error!(
            1,
            format!(
                "invalid --modify-window '{}': window must be non-negative",
                display
            )
        )
        .with_role(Role::Client));
    }

    let normalized = trimmed.strip_prefix('+').unwrap_or(trimmed);

    match normalized.parse::<u64>() {
        Ok(value) => Ok(value),
        Err(error) => {
            let detail = match error.kind() {
                IntErrorKind::InvalidDigit => "window must be an unsigned integer",
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    "window exceeds the supported range"
                }
                IntErrorKind::Empty => "--modify-window value must not be empty",
                _ => "window is invalid",
            };
            Err(rsync_error!(
                1,
                format!("invalid --modify-window '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SizeParseError {
    Empty,
    Negative,
    Invalid,
    TooLarge,
}

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
                "invalid {flag} '{display}': expected a size with an optional K/M/G/T/P/E suffix"
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

fn parse_size_spec(text: &str) -> Result<u64, SizeParseError> {
    if text.is_empty() {
        return Err(SizeParseError::Empty);
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
        return Err(SizeParseError::Empty);
    }

    if negative {
        return Err(SizeParseError::Negative);
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
        return Err(SizeParseError::Invalid);
    }

    let (integer_part, fractional_part, denominator) =
        parse_decimal_components_for_size(numeric_part)?;

    let (exponent, mut remainder_after_suffix) = if remainder.is_empty() {
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
                'e' => 6,
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
    if value > u64::MAX as u128 {
        return Err(SizeParseError::TooLarge);
    }

    Ok(value as u64)
}

fn parse_decimal_components_for_size(text: &str) -> Result<(u128, u128, u128), SizeParseError> {
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

pub(crate) fn pow_u128_for_size(base: u32, exponent: u32) -> Result<u128, SizeParseError> {
    u128::from(base)
        .checked_pow(exponent)
        .ok_or(SizeParseError::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    // --- parse_size_spec tests ---

    #[test]
    fn parse_size_spec_empty() {
        assert_eq!(parse_size_spec(""), Err(SizeParseError::Empty));
    }

    #[test]
    fn parse_size_spec_just_sign() {
        assert_eq!(parse_size_spec("+"), Err(SizeParseError::Empty));
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
    fn parse_size_spec_positive_prefix() {
        assert_eq!(parse_size_spec("+100"), Ok(100));
        assert_eq!(parse_size_spec("+1K"), Ok(1024));
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
    fn parse_size_spec_exbibytes() {
        assert_eq!(parse_size_spec("1E"), Ok(1024u64.pow(6)));
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

    // --- pow_u128_for_size tests ---

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

    // --- SizeParseError tests ---

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

    // --- parse_timeout_argument tests ---

    #[test]
    fn parse_timeout_argument_zero() {
        let result = parse_timeout_argument(&os("0")).unwrap();
        assert_eq!(result, TransferTimeout::Disabled);
    }

    #[test]
    fn parse_timeout_argument_positive() {
        let result = parse_timeout_argument(&os("30")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 30));
    }

    #[test]
    fn parse_timeout_argument_with_plus() {
        let result = parse_timeout_argument(&os("+60")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 60));
    }

    #[test]
    fn parse_timeout_argument_empty() {
        let result = parse_timeout_argument(&os(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_negative() {
        let result = parse_timeout_argument(&os("-10"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_invalid() {
        let result = parse_timeout_argument(&os("abc"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_timeout_argument_whitespace() {
        let result = parse_timeout_argument(&os("  30  ")).unwrap();
        assert!(matches!(result, TransferTimeout::Seconds(n) if n.get() == 30));
    }

    // --- parse_max_delete_argument tests ---

    #[test]
    fn parse_max_delete_argument_zero() {
        assert_eq!(parse_max_delete_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_max_delete_argument_positive() {
        assert_eq!(parse_max_delete_argument(&os("100")).unwrap(), 100);
    }

    #[test]
    fn parse_max_delete_argument_with_plus() {
        assert_eq!(parse_max_delete_argument(&os("+50")).unwrap(), 50);
    }

    #[test]
    fn parse_max_delete_argument_empty() {
        assert!(parse_max_delete_argument(&os("")).is_err());
    }

    #[test]
    fn parse_max_delete_argument_negative() {
        assert!(parse_max_delete_argument(&os("-10")).is_err());
    }

    #[test]
    fn parse_max_delete_argument_invalid() {
        assert!(parse_max_delete_argument(&os("xyz")).is_err());
    }

    // --- parse_checksum_seed_argument tests ---

    #[test]
    fn parse_checksum_seed_argument_zero() {
        assert_eq!(parse_checksum_seed_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_checksum_seed_argument_positive() {
        assert_eq!(parse_checksum_seed_argument(&os("12345")).unwrap(), 12345);
    }

    #[test]
    fn parse_checksum_seed_argument_with_plus() {
        assert_eq!(parse_checksum_seed_argument(&os("+999")).unwrap(), 999);
    }

    #[test]
    fn parse_checksum_seed_argument_empty() {
        assert!(parse_checksum_seed_argument(&os("")).is_err());
    }

    #[test]
    fn parse_checksum_seed_argument_negative() {
        assert!(parse_checksum_seed_argument(&os("-1")).is_err());
    }

    #[test]
    fn parse_checksum_seed_argument_invalid() {
        assert!(parse_checksum_seed_argument(&os("abc")).is_err());
    }

    // --- parse_modify_window_argument tests ---

    #[test]
    fn parse_modify_window_argument_zero() {
        assert_eq!(parse_modify_window_argument(&os("0")).unwrap(), 0);
    }

    #[test]
    fn parse_modify_window_argument_positive() {
        assert_eq!(parse_modify_window_argument(&os("2")).unwrap(), 2);
    }

    #[test]
    fn parse_modify_window_argument_with_plus() {
        assert_eq!(parse_modify_window_argument(&os("+5")).unwrap(), 5);
    }

    #[test]
    fn parse_modify_window_argument_empty() {
        assert!(parse_modify_window_argument(&os("")).is_err());
    }

    #[test]
    fn parse_modify_window_argument_negative() {
        assert!(parse_modify_window_argument(&os("-1")).is_err());
    }

    #[test]
    fn parse_modify_window_argument_invalid() {
        assert!(parse_modify_window_argument(&os("foo")).is_err());
    }

    // --- parse_size_limit_argument tests ---

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

    // --- parse_block_size_argument tests ---

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

    // --- resolve_iconv_setting tests ---

    #[test]
    fn resolve_iconv_setting_none_not_disabled() {
        let result = resolve_iconv_setting(None, false).unwrap();
        assert_eq!(result, IconvSetting::Unspecified);
    }

    #[test]
    fn resolve_iconv_setting_none_disabled() {
        let result = resolve_iconv_setting(None, true).unwrap();
        assert_eq!(result, IconvSetting::Disabled);
    }

    #[test]
    fn resolve_iconv_setting_valid_spec() {
        let result = resolve_iconv_setting(Some(&os("UTF-8")), false).unwrap();
        assert_eq!(
            result,
            IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: None,
            }
        );
    }

    #[test]
    fn resolve_iconv_setting_both_charsets() {
        let result = resolve_iconv_setting(Some(&os("UTF-8,ISO-8859-1")), false).unwrap();
        assert_eq!(
            result,
            IconvSetting::Explicit {
                local: "UTF-8".to_owned(),
                remote: Some("ISO-8859-1".to_owned()),
            }
        );
    }

    #[test]
    fn resolve_iconv_setting_empty() {
        let result = resolve_iconv_setting(Some(&os("")), false);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_iconv_setting_locale_default() {
        let result = resolve_iconv_setting(Some(&os(".")), false).unwrap();
        assert_eq!(result, IconvSetting::LocaleDefault);
    }

    // --- parse_human_readable_level tests ---

    #[test]
    fn parse_human_readable_level_zero() {
        let result = parse_human_readable_level(&os("0")).unwrap();
        assert_eq!(result, HumanReadableMode::Disabled);
    }

    #[test]
    fn parse_human_readable_level_one() {
        let result = parse_human_readable_level(&os("1")).unwrap();
        assert_eq!(result, HumanReadableMode::Enabled);
    }

    #[test]
    fn parse_human_readable_level_two() {
        let result = parse_human_readable_level(&os("2")).unwrap();
        assert_eq!(result, HumanReadableMode::Combined);
    }

    #[test]
    fn parse_human_readable_level_invalid() {
        let result = parse_human_readable_level(&os("invalid"));
        assert!(result.is_err());
    }

    // --- parse_decimal_components_for_size tests ---

    #[test]
    fn parse_decimal_components_integer_only() {
        let (integer, fraction, denominator) = parse_decimal_components_for_size("123").unwrap();
        assert_eq!(integer, 123);
        assert_eq!(fraction, 0);
        assert_eq!(denominator, 1);
    }

    #[test]
    fn parse_decimal_components_with_fraction() {
        let (integer, fraction, denominator) = parse_decimal_components_for_size("1.5").unwrap();
        assert_eq!(integer, 1);
        assert_eq!(fraction, 5);
        assert_eq!(denominator, 10);
    }

    #[test]
    fn parse_decimal_components_with_comma() {
        let (integer, fraction, denominator) = parse_decimal_components_for_size("2,25").unwrap();
        assert_eq!(integer, 2);
        assert_eq!(fraction, 25);
        assert_eq!(denominator, 100);
    }

    #[test]
    fn parse_decimal_components_zero_fraction() {
        let (integer, fraction, denominator) = parse_decimal_components_for_size("10.0").unwrap();
        assert_eq!(integer, 10);
        assert_eq!(fraction, 0);
        assert_eq!(denominator, 10);
    }

    #[test]
    fn parse_decimal_components_multiple_decimal_points() {
        let result = parse_decimal_components_for_size("1.2.3");
        assert!(result.is_err());
    }
}
