//! Protocol version argument parsing.

use std::ffi::OsStr;

use core::{
    client::{FEATURE_UNAVAILABLE_EXIT_CODE, PROTOCOL_INCOMPATIBLE_EXIT_CODE},
    message::{Message, Role},
    rsync_error,
};
use protocol::{ParseProtocolVersionErrorKind, ProtocolVersion};
use std::str::FromStr;

/// Lowest protocol version upstream rsync accepts on the command line.
///
/// upstream: rsync.h:147 `MIN_PROTOCOL_VERSION 20`. `setup_protocol`
/// (compat.c:629) rejects anything below this with `RERR_PROTOCOL`.
const UPSTREAM_MIN_PROTOCOL: u8 = 20;
/// Highest protocol version upstream rsync accepts on the command line.
///
/// upstream: rsync.h:114 `PROTOCOL_VERSION 32`. `setup_protocol`
/// (compat.c:634) rejects anything above this with `RERR_PROTOCOL`.
const UPSTREAM_MAX_PROTOCOL: u8 = 32;

/// Classification of a validated `--protocol` argument.
///
/// Upstream accepts protocol `20..=32` on the command line, but this build only
/// speaks `28..=32` over the wire. Values in `20..=27` are valid per upstream
/// and are meaningful only for a local copy, where no protocol is ever
/// negotiated (the local-copy executor never touches the wire).
#[derive(Debug)]
pub(crate) enum ProtocolArg {
    /// A wire-capable protocol version in `28..=32`.
    Supported(ProtocolVersion),
    /// An upstream-valid version in `20..=27`, below this build's wire floor.
    LegacyLocalOnly(u8),
}

/// Parses and classifies the `--protocol` command-line argument.
///
/// Mirrors upstream rsync's command-line bounds (`setup_protocol`,
/// compat.c:629-637): values outside `20..=32` are protocol errors (exit 4 in
/// this build is reserved for unsupported actions; a bad protocol number is
/// `RERR_PROTOCOL` = 2). Non-numeric input is a syntax error (exit 1).
///
/// Returns user-friendly errors matching upstream rsync diagnostics:
/// - Exit code 1 for syntax errors (non-numeric input).
/// - Exit code 2 for a numeric value outside upstream's `20..=32` range.
pub(crate) fn parse_protocol_version_arg(value: &OsStr) -> Result<ProtocolArg, Message> {
    let text = value.to_string_lossy();
    let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
    let display = if trimmed.is_empty() {
        text.as_ref()
    } else {
        trimmed
    };

    match ProtocolVersion::from_str(text.as_ref()) {
        // `from_str` only succeeds for this build's supported range (28..=32).
        Ok(version) => Ok(ProtocolArg::Supported(version)),
        Err(error) => match error.kind() {
            // Non-numeric input is a usage error: exit 1 (RERR_SYNTAX).
            ParseProtocolVersionErrorKind::Empty => Err(syntax_error(
                display,
                "protocol value must not be empty".to_owned(),
            )),
            ParseProtocolVersionErrorKind::InvalidDigit => Err(syntax_error(
                display,
                "protocol version must be an unsigned integer".to_owned(),
            )),
            ParseProtocolVersionErrorKind::Negative => Err(syntax_error(
                display,
                "protocol version cannot be negative".to_owned(),
            )),
            // A value larger than u8 is necessarily above the upstream ceiling.
            ParseProtocolVersionErrorKind::Overflow => Err(ceiling_error(display)),
            ParseProtocolVersionErrorKind::UnsupportedRange(raw) => {
                if raw < UPSTREAM_MIN_PROTOCOL {
                    Err(floor_error(display))
                } else if raw > UPSTREAM_MAX_PROTOCOL {
                    Err(ceiling_error(display))
                } else {
                    // 20..=27: valid upstream, below this build's wire floor.
                    Ok(ProtocolArg::LegacyLocalOnly(raw))
                }
            }
        },
    }
}

/// Builds a syntax (exit 1) diagnostic for a non-numeric `--protocol` value.
fn syntax_error(display: &str, detail: String) -> Message {
    let supported = supported_protocols_list();
    rsync_error!(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        format!(
            "invalid protocol version '{display}': {detail}; supported protocols are {supported}"
        )
    )
    .with_role(Role::Client)
}

/// Builds the exit-2 diagnostic for a value below upstream's minimum (20).
///
/// upstream: compat.c:630 "--protocol must be at least 20 on the %s."
fn floor_error(display: &str) -> Message {
    rsync_error!(
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
        format!(
            "invalid protocol version '{display}': --protocol must be at least {UPSTREAM_MIN_PROTOCOL} on the client"
        )
    )
    .with_role(Role::Client)
}

/// Builds the exit-2 diagnostic for a value above upstream's maximum (32).
///
/// upstream: compat.c:635 "--protocol must be no more than 32 on the %s."
fn ceiling_error(display: &str) -> Message {
    rsync_error!(
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
        format!(
            "invalid protocol version '{display}': --protocol must be no more than {UPSTREAM_MAX_PROTOCOL} on the client"
        )
    )
    .with_role(Role::Client)
}

/// Builds the exit-2 diagnostic for a legacy protocol requested over the wire.
///
/// upstream accepts 20..=27 and would negotiate it, but this build speaks only
/// 28..=32 over the wire, so a remote transfer at a legacy version is refused.
pub(crate) fn legacy_remote_rejection(raw: u8) -> Message {
    let supported = supported_protocols_list();
    rsync_error!(
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
        format!(
            "protocol version {raw} is not supported over the wire by this build (supported protocols are {supported}); it is accepted only for a local copy"
        )
    )
    .with_role(Role::Client)
}

const fn supported_protocols_list() -> &'static str {
    ProtocolVersion::supported_protocol_numbers_display()
}
