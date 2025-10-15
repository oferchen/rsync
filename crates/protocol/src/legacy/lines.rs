use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

use super::{
    LEGACY_DAEMON_PREFIX, greeting::parse_legacy_daemon_greeting, malformed_legacy_greeting,
};

/// Classification of legacy ASCII daemon lines that share the `@RSYNCD:` prefix.
///
/// Legacy rsync clients and daemons exchange several non-version banners during
/// the ASCII-based negotiation path. These lines reuse the same prefix as the
/// version greeting, so higher level code benefits from a typed representation
/// to avoid stringly-typed comparisons while still mirroring upstream behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LegacyDaemonMessage<'a> {
    /// A protocol version announcement such as `@RSYNCD: 30.0`.
    Version(ProtocolVersion),
    /// Confirmation that the negotiation can proceed (`@RSYNCD: OK`).
    Ok,
    /// Notification that the daemon is closing the legacy exchange
    /// (`@RSYNCD: EXIT`).
    Exit,
    /// The daemon requires authentication before continuing. Upstream rsync
    /// includes the requested module name after the keyword (e.g.
    /// `@RSYNCD: AUTHREQD module`). The module is optional in practice because
    /// older daemons sometimes omit it when the request has not yet selected a
    /// module. The parser therefore surfaces it as an optional borrowed
    /// substring.
    AuthRequired {
        /// Optional module name provided by the daemon.
        module: Option<&'a str>,
    },
    /// Any other keyword line the daemon may send. This variant is intentionally
    /// permissive to avoid guessing the full matrix of legacy extensions while
    /// still allowing higher layers to perform equality checks if needed.
    Other(&'a str),
}

/// Parses a legacy daemon line that begins with `@RSYNCD:` into a structured
/// representation.
///
/// The helper accepts and normalizes trailing carriage returns or line feeds.
/// When the payload begins with digits, the function delegates to
/// [`parse_legacy_daemon_greeting`] to preserve the exact validation rules used
/// for version announcements. Recognized keywords are mapped to dedicated
/// variants and all remaining inputs yield [`LegacyDaemonMessage::Other`],
/// allowing callers to gracefully handle extensions without guessing upstream's
/// future strings.
pub fn parse_legacy_daemon_message(
    line: &str,
) -> Result<LegacyDaemonMessage<'_>, NegotiationError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let after_prefix = trimmed
        .strip_prefix(LEGACY_DAEMON_PREFIX)
        .ok_or_else(|| malformed_legacy_greeting(trimmed))?;

    let payload = after_prefix.trim_start();
    if payload.is_empty() {
        return Err(malformed_legacy_greeting(trimmed));
    }

    let payload_for_match = payload.trim_end();

    if payload_for_match
        .as_bytes()
        .first()
        .copied()
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        return parse_legacy_daemon_greeting(trimmed).map(LegacyDaemonMessage::Version);
    }

    match payload_for_match {
        "OK" => Ok(LegacyDaemonMessage::Ok),
        "EXIT" => Ok(LegacyDaemonMessage::Exit),
        payload => {
            const AUTHREQD_KEYWORD: &str = "AUTHREQD";
            if let Some(rest) = payload.strip_prefix(AUTHREQD_KEYWORD) {
                let module = rest.trim_start();
                let module = if module.is_empty() {
                    None
                } else {
                    Some(module)
                };
                return Ok(LegacyDaemonMessage::AuthRequired { module });
            }

            Ok(LegacyDaemonMessage::Other(payload))
        }
    }
}

/// Parses a legacy daemon error line of the form `@ERROR: ...`.
///
/// Legacy rsync daemons sometimes terminate the ASCII negotiation path with an
/// explicit error banner rather than the regular `@RSYNCD:` responses. The
/// payload following `@ERROR:` is returned with surrounding ASCII whitespace
/// removed, allowing callers to surface the daemon's diagnostic verbatim while
/// still matching upstream trimming behavior.
#[must_use]
pub fn parse_legacy_error_message(line: &str) -> Option<&str> {
    parse_prefixed_payload(line, "@ERROR:")
}

/// Parses a legacy daemon warning line of the form `@WARNING: ...`.
///
/// The returned payload mirrors [`parse_legacy_error_message`], enabling higher
/// layers to surface warning text emitted by older daemons without guessing the
/// exact formatting nuances.
#[must_use]
pub fn parse_legacy_warning_message(line: &str) -> Option<&str> {
    parse_prefixed_payload(line, "@WARNING:")
}

#[must_use]
fn parse_prefixed_payload<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    trimmed.strip_prefix(prefix).map(|rest| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_legacy_daemon_message_accepts_ok_keyword() {
        let message = parse_legacy_daemon_message("@RSYNCD: OK\r\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Ok);
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_ok_with_trailing_whitespace() {
        let message =
            parse_legacy_daemon_message("@RSYNCD: OK   \r\n").expect("keyword with padding");
        assert_eq!(message, LegacyDaemonMessage::Ok);
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_exit_keyword() {
        let message = parse_legacy_daemon_message("@RSYNCD: EXIT\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Exit);
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_exit_with_trailing_whitespace() {
        let message =
            parse_legacy_daemon_message("@RSYNCD: EXIT   \n").expect("keyword with padding");
        assert_eq!(message, LegacyDaemonMessage::Exit);
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_authreqd_with_module() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD sample\n").expect("keyword");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthRequired {
                module: Some("sample"),
            }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_authreqd_without_module() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::AuthRequired { module: None });
    }

    #[test]
    fn parse_legacy_daemon_message_treats_whitespace_only_module_as_none() {
        let message =
            parse_legacy_daemon_message("@RSYNCD: AUTHREQD    \n").expect("keyword with padding");
        assert_eq!(message, LegacyDaemonMessage::AuthRequired { module: None });
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_authreqd_with_trailing_whitespace() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD module   \n")
            .expect("keyword with padding");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthRequired {
                module: Some("module"),
            }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_classifies_unknown_keywords() {
        let message = parse_legacy_daemon_message("@RSYNCD: SOMETHING\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Other("SOMETHING"));
    }

    #[test]
    fn parse_legacy_daemon_message_routes_version_to_existing_parser() {
        let message = parse_legacy_daemon_message("@RSYNCD: 30.0\n").expect("version");
        assert_eq!(
            message,
            LegacyDaemonMessage::Version(ProtocolVersion::new_const(30))
        );
    }

    #[test]
    fn parse_legacy_daemon_message_rejects_missing_prefix() {
        let err = parse_legacy_daemon_message("RSYNCD: AUTHREQD module\n").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_authreqd_with_trailing_tabs() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD\tmodule\t\n")
            .expect("keyword with tabs");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthRequired {
                module: Some("module"),
            }
        );
    }

    #[test]
    fn parses_legacy_error_message_and_trims_payload() {
        let payload =
            parse_legacy_error_message("@ERROR: access denied\r\n").expect("error payload");
        assert_eq!(payload, "access denied");
    }

    #[test]
    fn parses_legacy_warning_message_and_trims_payload() {
        let payload =
            parse_legacy_warning_message("@WARNING: will retry  \n").expect("warning payload");
        assert_eq!(payload, "will retry");
    }
}
