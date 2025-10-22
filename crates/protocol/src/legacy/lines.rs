use crate::error::NegotiationError;
use crate::version::ProtocolVersion;
use core::fmt::{self, Write as FmtWrite};

use super::{
    LEGACY_DAEMON_PREFIX, greeting::parse_legacy_daemon_greeting, malformed_legacy_greeting,
};

/// Classification of legacy ASCII daemon lines that share the `@RSYNCD:` prefix.
///
/// Legacy rsync clients and daemons exchange several non-version banners during
/// the ASCII-based negotiation path. These lines reuse the same prefix as the
/// version greeting, so higher level code benefits from a typed representation
/// to avoid stringly-typed comparisons while still mirroring upstream behavior.
#[doc(alias = "@RSYNCD")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LegacyDaemonMessage<'a> {
    /// A protocol version announcement such as `@RSYNCD: 30.0`.
    Version(ProtocolVersion),
    /// Confirmation that the negotiation can proceed (`@RSYNCD: OK`).
    Ok,
    /// Notification that the daemon is closing the legacy exchange
    /// (`@RSYNCD: EXIT`).
    Exit,
    /// Capability advertisement emitted by legacy daemons (`@RSYNCD: CAP …`).
    #[doc(alias = "@RSYNCD: CAP")]
    Capabilities {
        /// Raw capability string advertised by the daemon with ASCII
        /// whitespace trimmed from both ends.
        flags: &'a str,
    },
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
    /// Authentication challenge emitted after [`LegacyDaemonMessage::AuthRequired`].
    ///
    /// Some deployments stage authentication across two banners: the
    /// `AUTHREQD` keyword advertises that credentials are required and a
    /// follow-up `@RSYNCD: AUTH <challenge>` supplies the base64 challenge.
    /// Modern rsync versions typically inline the challenge inside the
    /// `AUTHREQD` response, but tolerating both styles ensures parity with
    /// legacy daemons still using the split handshake.
    AuthChallenge {
        /// Base64-encoded challenge supplied by the daemon.
        challenge: &'a str,
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
#[doc(alias = "@RSYNCD")]
#[must_use = "the parsed legacy daemon message must be handled"]
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
                if rest.is_empty() {
                    return Ok(LegacyDaemonMessage::AuthRequired { module: None });
                }

                if !rest
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    return Ok(LegacyDaemonMessage::Other(payload));
                }

                let module = rest.trim();
                let module = if module.is_empty() {
                    None
                } else {
                    Some(module)
                };
                return Ok(LegacyDaemonMessage::AuthRequired { module });
            }

            const AUTH_KEYWORD: &str = "AUTH";
            if let Some(rest) = payload.strip_prefix(AUTH_KEYWORD) {
                if rest.is_empty()
                    || !rest
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    return Ok(LegacyDaemonMessage::Other(payload));
                }

                let challenge = rest.trim();
                if challenge.is_empty() {
                    return Ok(LegacyDaemonMessage::Other(payload));
                }

                return Ok(LegacyDaemonMessage::AuthChallenge { challenge });
            }

            const CAP_KEYWORD: &str = "CAP";
            if let Some(rest) = payload.strip_prefix(CAP_KEYWORD) {
                if rest.is_empty()
                    || !rest
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    return Ok(LegacyDaemonMessage::Other(payload));
                }

                let flags = rest.trim();
                if flags.is_empty() {
                    return Ok(LegacyDaemonMessage::Other(payload));
                }

                return Ok(LegacyDaemonMessage::Capabilities { flags });
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
#[doc(alias = "@ERROR")]
#[must_use]
pub fn parse_legacy_error_message(line: &str) -> Option<&str> {
    parse_prefixed_payload(line, "@ERROR:")
}

/// Parses a legacy daemon warning line of the form `@WARNING: ...`.
///
/// The returned payload mirrors [`parse_legacy_error_message`], enabling higher
/// layers to surface warning text emitted by older daemons without guessing the
/// exact formatting nuances.
#[doc(alias = "@WARNING")]
#[must_use]
pub fn parse_legacy_warning_message(line: &str) -> Option<&str> {
    parse_prefixed_payload(line, "@WARNING:")
}

#[must_use]
fn parse_prefixed_payload<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    trimmed.strip_prefix(prefix).map(|rest| rest.trim())
}

fn write_prefixed_payload<W: FmtWrite>(writer: &mut W, payload: &str) -> fmt::Result {
    writer.write_str(LEGACY_DAEMON_PREFIX)?;
    if !payload.is_empty() {
        writer.write_char(' ')?;
        writer.write_str(payload)?;
    }
    writer.write_char('\n')
}

fn write_prefixed_keyword<W: FmtWrite>(
    writer: &mut W,
    keyword: &str,
    value: Option<&str>,
) -> fmt::Result {
    writer.write_str(LEGACY_DAEMON_PREFIX)?;
    writer.write_char(' ')?;
    writer.write_str(keyword)?;

    if let Some(rest) = value {
        if !rest.is_empty() {
            writer.write_char(' ')?;
            writer.write_str(rest)?;
        }
    }

    writer.write_char('\n')
}

/// Writes a canonical legacy daemon message into the supplied [`fmt::Write`] sink.
///
/// The helper mirrors upstream formatting for `@RSYNCD:` responses while
/// normalising whitespace. Version announcements reuse
/// [`write_legacy_daemon_greeting`](super::greeting::write_legacy_daemon_greeting)
/// so the protocol number is rendered with the canonical fractional suffix and
/// newline terminator. Other keywords emit a single space between the prefix
/// and payload, trimming trailing whitespace captured during parsing and
/// collapsing consecutive ASCII whitespace sequences inside capability banners
/// to match the formatting relayed by upstream rsync.
///
/// # Examples
///
/// Render a legacy daemon acknowledgment:
///
/// ```
/// use rsync_protocol::{format_legacy_daemon_message, LegacyDaemonMessage};
///
/// let rendered = format_legacy_daemon_message(LegacyDaemonMessage::Ok);
/// assert_eq!(rendered, "@RSYNCD: OK\n");
/// ```
///
/// Canonicalise a legacy capability banner:
///
/// ```
/// use rsync_protocol::{
///     format_legacy_daemon_message, LegacyDaemonMessage, parse_legacy_daemon_message,
/// };
///
/// let parsed = parse_legacy_daemon_message("@RSYNCD: CAP   0x1f  0x2\r\n")?;
/// let rendered = format_legacy_daemon_message(parsed);
///
/// assert_eq!(rendered, "@RSYNCD: CAP 0x1f 0x2\n");
/// # Ok::<_, rsync_protocol::NegotiationError>(())
/// ```
#[must_use = "callers typically forward the formatted message to the daemon or logs"]
pub fn write_legacy_daemon_message<W: FmtWrite>(
    writer: &mut W,
    message: LegacyDaemonMessage<'_>,
) -> fmt::Result {
    use super::greeting::write_legacy_daemon_greeting;

    match message {
        LegacyDaemonMessage::Version(version) => write_legacy_daemon_greeting(writer, version),
        LegacyDaemonMessage::Ok => write_prefixed_keyword(writer, "OK", None),
        LegacyDaemonMessage::Exit => write_prefixed_keyword(writer, "EXIT", None),
        LegacyDaemonMessage::Capabilities { flags } => {
            writer.write_str(LEGACY_DAEMON_PREFIX)?;
            writer.write_str(" CAP")?;

            let mut tokens = flags.split_ascii_whitespace();
            if let Some(first) = tokens.next() {
                writer.write_char(' ')?;
                writer.write_str(first)?;
                for token in tokens {
                    writer.write_char(' ')?;
                    writer.write_str(token)?;
                }
            }

            writer.write_char('\n')
        }
        LegacyDaemonMessage::AuthRequired { module } => {
            write_prefixed_keyword(writer, "AUTHREQD", module)
        }
        LegacyDaemonMessage::AuthChallenge { challenge } => {
            write_prefixed_keyword(writer, "AUTH", Some(challenge))
        }
        LegacyDaemonMessage::Other(payload) => {
            let normalized = payload.trim_end_matches(|ch: char| ch.is_ascii_whitespace());
            write_prefixed_payload(writer, normalized)
        }
    }
}

/// Formats a legacy daemon message into an owned [`String`].
///
/// This is a convenience wrapper around [`write_legacy_daemon_message`] for
/// call sites that prefer an owned allocation. The returned string always ends
/// with a newline to match upstream framing.
#[must_use]
pub fn format_legacy_daemon_message(message: LegacyDaemonMessage<'_>) -> String {
    let mut rendered = String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 32);
    write_legacy_daemon_message(&mut rendered, message).expect("String implements fmt::Write");
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn assert_copy<T: Copy>() {}
    fn assert_hash<T: Hash>() {}

    fn hash_value<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn legacy_daemon_message_supports_copy_and_hash() {
        assert_copy::<LegacyDaemonMessage<'static>>();
        assert_hash::<LegacyDaemonMessage<'static>>();

        let sample = LegacyDaemonMessage::AuthRequired {
            module: Some("module"),
        };
        let copied = sample;

        assert_eq!(sample, copied);
        assert_eq!(hash_value(&sample), hash_value(&copied));
    }

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
    fn parse_legacy_daemon_message_accepts_auth_challenge_keyword() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTH abc123\n").expect("keyword");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthChallenge {
                challenge: "abc123",
            }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_rejects_auth_without_payload() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTH\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Other("AUTH"));
    }

    #[test]
    fn parse_legacy_daemon_message_rejects_lowercase_prefix() {
        let err = parse_legacy_daemon_message("@rsyncd: OK\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@rsyncd: OK");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn write_legacy_daemon_message_formats_version_branch() {
        let message = LegacyDaemonMessage::Version(ProtocolVersion::from_supported(31).unwrap());
        let rendered = format_legacy_daemon_message(message);
        assert_eq!(rendered, "@RSYNCD: 31.0\n");
    }

    #[test]
    fn write_legacy_daemon_message_formats_keywords() {
        assert_eq!(
            format_legacy_daemon_message(LegacyDaemonMessage::Ok),
            "@RSYNCD: OK\n"
        );
        assert_eq!(
            format_legacy_daemon_message(LegacyDaemonMessage::Exit),
            "@RSYNCD: EXIT\n"
        );
        assert_eq!(
            format_legacy_daemon_message(LegacyDaemonMessage::AuthChallenge {
                challenge: "abc123",
            }),
            "@RSYNCD: AUTH abc123\n"
        );
    }

    #[test]
    fn write_legacy_daemon_message_formats_capabilities() {
        let message = LegacyDaemonMessage::Capabilities { flags: "0x1f 0x2" };
        let rendered = format_legacy_daemon_message(message);
        assert_eq!(rendered, "@RSYNCD: CAP 0x1f 0x2\n");
    }

    #[test]
    fn write_legacy_daemon_message_formats_auth_requests() {
        let without_module = LegacyDaemonMessage::AuthRequired { module: None };
        assert_eq!(
            format_legacy_daemon_message(without_module),
            "@RSYNCD: AUTHREQD\n"
        );

        let with_module = LegacyDaemonMessage::AuthRequired {
            module: Some("module"),
        };
        assert_eq!(
            format_legacy_daemon_message(with_module),
            "@RSYNCD: AUTHREQD module\n"
        );
    }

    #[test]
    fn write_legacy_daemon_message_normalises_other_payloads() {
        let parsed =
            parse_legacy_daemon_message("@RSYNCD: EXTRA   \t \r\n").expect("message should parse");
        assert_eq!(format_legacy_daemon_message(parsed), "@RSYNCD: EXTRA\n");
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
    fn parse_legacy_daemon_message_preserves_internal_whitespace_in_module_name() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD  module name\t\r\n")
            .expect("keyword with extra whitespace");
        assert_eq!(
            message,
            LegacyDaemonMessage::AuthRequired {
                module: Some("module name"),
            }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_authreqd_without_module() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQD\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::AuthRequired { module: None });
    }

    #[test]
    fn parse_legacy_daemon_message_requires_delimiter_after_authreqd_keyword() {
        let message = parse_legacy_daemon_message("@RSYNCD: AUTHREQDmodule\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Other("AUTHREQDmodule"));
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
    fn parse_legacy_daemon_message_accepts_capabilities_keyword() {
        let message = parse_legacy_daemon_message("@RSYNCD: CAP 0x1f 0x2\n").expect("keyword");
        assert_eq!(
            message,
            LegacyDaemonMessage::Capabilities { flags: "0x1f 0x2" }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_accepts_capabilities_with_extra_whitespace() {
        let message = parse_legacy_daemon_message("@RSYNCD: CAP\t capabilities list  \r\n")
            .expect("keyword with padding");
        assert_eq!(
            message,
            LegacyDaemonMessage::Capabilities {
                flags: "capabilities list",
            }
        );
    }

    #[test]
    fn parse_legacy_daemon_message_rejects_capabilities_without_payload() {
        let message = parse_legacy_daemon_message("@RSYNCD: CAP\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Other("CAP"));
    }

    #[test]
    fn parse_legacy_daemon_message_rejects_capabilities_without_delimiter() {
        let message = parse_legacy_daemon_message("@RSYNCD: CAPpayload\n").expect("keyword");
        assert_eq!(message, LegacyDaemonMessage::Other("CAPpayload"));
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
    fn parse_legacy_daemon_message_tolerates_leading_whitespace_before_version_digits() {
        let message =
            parse_legacy_daemon_message("@RSYNCD:    29.0  \r\n").expect("version with padding");
        assert_eq!(
            message,
            LegacyDaemonMessage::Version(ProtocolVersion::new_const(29))
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
    fn parse_legacy_daemon_message_rejects_empty_payload() {
        let err = parse_legacy_daemon_message("@RSYNCD:\n").unwrap_err();
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
    fn parse_legacy_error_message_allows_empty_payload() {
        let payload = parse_legacy_error_message("@ERROR:\n").expect("empty payload");
        assert_eq!(payload, "");
    }

    #[test]
    fn parse_legacy_error_message_returns_none_for_unrecognized_prefix() {
        assert!(parse_legacy_error_message("something else\n").is_none());
    }

    #[test]
    fn parses_legacy_warning_message_and_trims_payload() {
        let payload =
            parse_legacy_warning_message("@WARNING: will retry  \n").expect("warning payload");
        assert_eq!(payload, "will retry");
    }

    #[test]
    fn parse_legacy_warning_message_allows_empty_payload() {
        let payload = parse_legacy_warning_message("@WARNING:\r\n").expect("empty payload");
        assert_eq!(payload, "");
    }

    #[test]
    fn parse_legacy_warning_message_returns_none_for_unrecognized_prefix() {
        assert!(parse_legacy_warning_message("something else\n").is_none());
    }
}
