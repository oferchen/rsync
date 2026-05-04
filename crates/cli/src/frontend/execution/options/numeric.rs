//! Parsers for simple numeric command-line arguments.
//!
//! Handles `--timeout`, `--max-delete`, `--checksum-seed`, `--modify-window`,
//! and `--human-readable` options with upstream-compatible error messages.

use std::ffi::OsStr;
use std::num::{IntErrorKind, NonZeroU64};

use core::{
    client::{HumanReadableMode, TransferTimeout},
    message::{Message, Role},
    rsync_error,
};

/// Parses the `--timeout` argument into a `TransferTimeout`.
///
/// Accepts non-negative integers. Zero disables the timeout.
/// A leading `+` prefix is permitted and ignored.
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

/// Parses the `--human-readable` level argument.
///
/// Delegates to `HumanReadableMode::parse` and converts errors to `clap::Error`.
pub(crate) fn parse_human_readable_level(value: &OsStr) -> Result<HumanReadableMode, clap::Error> {
    let text = value.to_string_lossy();
    HumanReadableMode::parse(text.as_ref())
        .map_err(|error| clap::Error::raw(clap::error::ErrorKind::InvalidValue, error.to_string()))
}

/// Parses the `--max-delete` argument as a non-negative `u64` deletion limit.
///
/// A leading `+` prefix is permitted and ignored.
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

/// Parses the `--checksum-seed` argument as a non-negative `u32`.
///
/// A leading `+` prefix is permitted and ignored.
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

/// Parses the `--modify-window` argument as a non-negative `u64` seconds value.
///
/// A leading `+` prefix is permitted and ignored.
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
