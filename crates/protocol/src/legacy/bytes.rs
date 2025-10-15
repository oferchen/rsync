use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

use super::{
    greeting::parse_legacy_daemon_greeting,
    lines::{
        LegacyDaemonMessage, parse_legacy_daemon_message, parse_legacy_error_message,
        parse_legacy_warning_message,
    },
    lossy_trimmed_input,
};

/// Parses a byte-oriented legacy daemon message by validating UTF-8 and then
/// delegating to [`parse_legacy_daemon_message`].
///
/// The byte-level variant mirrors [`parse_legacy_daemon_greeting_bytes`]
/// so transports that accumulate raw network buffers can request a structured
/// classification without first materializing an owned string. Invalid UTF-8
/// sequences are rejected with [`NegotiationError::MalformedLegacyGreeting`]
/// containing a lossy rendering of the offending bytes, matching the
/// diagnostics emitted by upstream rsync when echoing unexpected banners.
pub fn parse_legacy_daemon_message_bytes(
    line: &[u8],
) -> Result<LegacyDaemonMessage<'_>, NegotiationError> {
    match core::str::from_utf8(line) {
        Ok(text) => parse_legacy_daemon_message(text),
        Err(_) => Err(NegotiationError::MalformedLegacyGreeting {
            input: lossy_trimmed_input(line),
        }),
    }
}

/// Parses a byte-oriented legacy daemon error line of the form `@ERROR: ...`.
///
/// Invalid UTF-8 input is rejected with
/// [`NegotiationError::MalformedLegacyGreeting`], mirroring
/// [`parse_legacy_daemon_message_bytes`].
pub fn parse_legacy_error_message_bytes(line: &[u8]) -> Result<Option<&str>, NegotiationError> {
    match core::str::from_utf8(line) {
        Ok(text) => Ok(parse_legacy_error_message(text)),
        Err(_) => Err(NegotiationError::MalformedLegacyGreeting {
            input: lossy_trimmed_input(line),
        }),
    }
}

/// Parses a byte-oriented legacy daemon warning line of the form `@WARNING: ...`.
///
/// Invalid UTF-8 input is rejected with the same diagnostics as
/// [`parse_legacy_error_message_bytes`].
pub fn parse_legacy_warning_message_bytes(line: &[u8]) -> Result<Option<&str>, NegotiationError> {
    match core::str::from_utf8(line) {
        Ok(text) => Ok(parse_legacy_warning_message(text)),
        Err(_) => Err(NegotiationError::MalformedLegacyGreeting {
            input: lossy_trimmed_input(line),
        }),
    }
}

/// Parses a byte-oriented legacy daemon greeting by first validating UTF-8 and
/// then delegating to [`parse_legacy_daemon_greeting`].
///
/// Legacy clients and daemons exchange greetings as ASCII byte streams. The
/// Rust implementation keeps the byte-oriented helper separate so higher level
/// transports can operate directly on buffers received from the network without
/// performing lossy conversions. Invalid UTF-8 sequences are rejected with a
/// [`NegotiationError::MalformedLegacyGreeting`] that captures the lossy string
/// representation for diagnostics, mirroring upstream behavior where the raw
/// greeting is echoed back to the user.
pub fn parse_legacy_daemon_greeting_bytes(
    line: &[u8],
) -> Result<ProtocolVersion, NegotiationError> {
    match core::str::from_utf8(line) {
        Ok(text) => parse_legacy_daemon_greeting(text),
        Err(_) => Err(NegotiationError::MalformedLegacyGreeting {
            input: lossy_trimmed_input(line),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::ProtocolVersion;

    #[test]
    fn parse_legacy_daemon_message_bytes_round_trips() {
        let message =
            parse_legacy_daemon_message_bytes(b"@RSYNCD: AUTHREQD module\r\n").expect("keyword");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthRequired {
                module: Some("module"),
            }
        );
    }

    #[test]
    fn rejects_non_utf8_legacy_daemon_message_bytes() {
        let err = parse_legacy_daemon_message_bytes(b"@RSYNCD: AUTHREQD\xff\r\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@RSYNCD: AUTHREQD\u{fffd}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parses_legacy_error_message_bytes() {
        let payload = parse_legacy_error_message_bytes(b"@ERROR: access denied\n").expect("parse");
        assert_eq!(payload, Some("access denied"));
    }

    #[test]
    fn parse_legacy_error_message_bytes_returns_none_for_unrecognized_prefix() {
        let payload = parse_legacy_error_message_bytes(b"something else\n").expect("parse");
        assert_eq!(payload, None);
    }

    #[test]
    fn parse_legacy_error_message_bytes_allows_empty_payload() {
        let payload = parse_legacy_error_message_bytes(b"@ERROR:\r\n").expect("parse");
        assert_eq!(payload, Some(""));
    }

    #[test]
    fn rejects_non_utf8_legacy_error_message_bytes() {
        let err = parse_legacy_error_message_bytes(b"@ERROR: denied\xff\r\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@ERROR: denied\u{fffd}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parses_legacy_warning_message_bytes() {
        let payload =
            parse_legacy_warning_message_bytes(b"@WARNING: watch out\r\n").expect("parse");
        assert_eq!(payload, Some("watch out"));
    }

    #[test]
    fn parse_legacy_warning_message_bytes_returns_none_for_unrecognized_prefix() {
        let payload = parse_legacy_warning_message_bytes(b"another prefix\n").expect("parse");
        assert_eq!(payload, None);
    }

    #[test]
    fn parse_legacy_warning_message_bytes_allows_empty_payload() {
        let payload = parse_legacy_warning_message_bytes(b"@WARNING:\n").expect("parse");
        assert_eq!(payload, Some(""));
    }

    #[test]
    fn rejects_non_utf8_legacy_warning_message_bytes() {
        let err = parse_legacy_warning_message_bytes(b"@WARNING: caution\xff\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@WARNING: caution\u{fffd}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parses_legacy_greeting_from_bytes() {
        let parsed =
            parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 29.0\r\n").expect("valid byte greeting");
        assert_eq!(parsed.as_u8(), 29);
    }

    #[test]
    fn parse_legacy_daemon_message_bytes_routes_version_banner() {
        let message =
            parse_legacy_daemon_message_bytes(b"@RSYNCD: 30.0\n").expect("version banner");
        assert_eq!(
            message,
            LegacyDaemonMessage::Version(ProtocolVersion::new_const(30))
        );
    }

    #[test]
    fn rejects_non_utf8_legacy_greetings() {
        let err = parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 31.0\xff\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@RSYNCD: 31.0\u{fffd}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
