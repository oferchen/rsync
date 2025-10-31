use std::ffi::OsStr;
use std::num::{IntErrorKind, NonZeroU64};
use std::str::FromStr;

use rsync_core::{
    client::{HumanReadableMode, TransferTimeout},
    message::{Message, Role},
    rsync_error,
};
use rsync_protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};

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
            let mut detail = match error.kind() {
                ParseProtocolVersionErrorKind::Empty => {
                    "protocol value must not be empty".to_string()
                }
                ParseProtocolVersionErrorKind::InvalidDigit => {
                    "protocol version must be an unsigned integer".to_string()
                }
                ParseProtocolVersionErrorKind::Negative => {
                    "protocol version cannot be negative".to_string()
                }
                ParseProtocolVersionErrorKind::Overflow => {
                    "protocol version value exceeds 255".to_string()
                }
                ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                    let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                    format!(
                        "protocol version {} is outside the supported range {}-{}",
                        value, oldest, newest
                    )
                }
            };
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!("supported protocols are {}", supported));

            Err(rsync_error!(
                1,
                format!("invalid protocol version '{}': {}", display, detail)
            )
            .with_role(Role::Client))
        }
    }
}

fn supported_protocols_list() -> &'static str {
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
