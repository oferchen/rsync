//! Protocol version argument parsing.

use std::ffi::OsStr;

use core::{
    client::{FEATURE_UNAVAILABLE_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE},
    message::{Message, Role},
    rsync_error,
};
use protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
use std::str::FromStr;

/// Parses the `--protocol` command-line argument into a validated `ProtocolVersion`.
///
/// Returns user-friendly errors matching upstream rsync diagnostics:
/// - Exit code 1 for syntax errors (non-numeric input)
/// - Exit code 2 for protocol incompatibility (valid number, unsupported range)
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
