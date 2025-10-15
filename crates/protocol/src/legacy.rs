use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

/// Legacy daemon greeting prefix used by rsync versions that speak the ASCII
/// banner negotiation path.
pub(crate) const LEGACY_DAEMON_PREFIX: &str = "@RSYNCD:";
pub(crate) const LEGACY_DAEMON_PREFIX_LEN: usize = LEGACY_DAEMON_PREFIX.len();

/// Parses a legacy ASCII daemon greeting of the form `@RSYNCD: <version>`.
///
/// Upstream rsync emits greetings such as `@RSYNCD: 31.0`. The Rust
/// implementation accepts optional fractional suffixes (e.g. `.0`) but only the
/// integer component participates in protocol negotiation. Any trailing carriage
/// returns or line feeds are ignored.
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let malformed = || malformed_legacy_greeting(trimmed);

    let after_prefix = trimmed
        .strip_prefix(LEGACY_DAEMON_PREFIX)
        .ok_or_else(malformed)?;

    let remainder = after_prefix.trim_start();
    if remainder.is_empty() {
        return Err(malformed());
    }

    let digits_len = ascii_digit_prefix_len(remainder);
    let digits = &remainder[..digits_len];
    if digits.is_empty() {
        return Err(malformed());
    }

    let mut rest = &remainder[digits_len..];
    loop {
        rest = rest.trim_start_matches(char::is_whitespace);

        if rest.is_empty() {
            break;
        }

        if let Some(after_dot) = rest.strip_prefix('.') {
            let fractional_len = ascii_digit_prefix_len(after_dot);
            if fractional_len == 0 {
                return Err(malformed());
            }

            rest = &after_dot[fractional_len..];
            continue;
        }

        return Err(malformed());
    }

    let parsed_version = parse_ascii_digits_to_u32(digits);
    let version = parsed_version.min(u32::from(u8::MAX)) as u8;

    ProtocolVersion::from_peer_advertisement(version)
}

/// Returns the length of the leading ASCII-digit run within `input`.
fn ascii_digit_prefix_len(input: &str) -> usize {
    input
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// Parses a string consisting solely of ASCII digits into a `u32`, saturating on
/// overflow.
fn parse_ascii_digits_to_u32(digits: &str) -> u32 {
    let mut value: u32 = 0;

    for &byte in digits.as_bytes() {
        debug_assert!(byte.is_ascii_digit());
        let digit = u32::from(byte - b'0');
        value = value.saturating_mul(10);
        value = value.saturating_add(digit);
    }

    value
}

/// Constructs a [`NegotiationError::MalformedLegacyGreeting`] for `trimmed` input.
fn malformed_legacy_greeting(trimmed: &str) -> NegotiationError {
    NegotiationError::MalformedLegacyGreeting {
        input: trimmed.to_owned(),
    }
}

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
            input: String::from_utf8_lossy(line).into_owned(),
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
            input: String::from_utf8_lossy(line).into_owned(),
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
            input: String::from_utf8_lossy(line).into_owned(),
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
            input: String::from_utf8_lossy(line).into_owned(),
        }),
    }
}

fn parse_prefixed_payload<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    trimmed.strip_prefix(prefix).map(|rest| rest.trim())
}

/// Formats the legacy ASCII daemon greeting used by pre-protocol-30 peers.
///
/// Upstream daemons send a line such as `@RSYNCD: 32.0\n` when speaking to
/// older clients. The Rust implementation mirrors that exact layout so callers
/// can emit byte-identical banners during negotiation and round-trip the value
/// through [`parse_legacy_daemon_greeting`].
#[must_use]
pub fn format_legacy_daemon_greeting(version: ProtocolVersion) -> String {
    let mut banner = String::with_capacity(16);
    banner.push_str(LEGACY_DAEMON_PREFIX);
    banner.push(' ');
    let digits = version.as_u8().to_string();
    banner.push_str(&digits);
    banner.push_str(".0\n");
    banner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_daemon_greeting_with_minor_version() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 31.0\r\n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 31);
    }

    #[test]
    fn parses_legacy_daemon_greeting_without_space_after_prefix() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD:31.0\n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 31);
    }

    #[test]
    fn parses_legacy_daemon_greeting_with_whitespace_before_fractional() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 32   .0   \n").expect("valid greeting");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parses_legacy_daemon_greeting_without_fractional_suffix() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30\n").expect("fractional optional");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn parses_greeting_with_trailing_whitespace() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 30.0   \n").expect("valid greeting");
        assert_eq!(parsed.as_u8(), 30);
    }

    #[test]
    fn parses_legacy_greeting_from_bytes() {
        let parsed =
            parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 29.0\r\n").expect("valid byte greeting");
        assert_eq!(parsed.as_u8(), 29);
    }

    #[test]
    fn rejects_non_utf8_legacy_greetings() {
        let err = parse_legacy_daemon_greeting_bytes(b"@RSYNCD: 31.0\xff").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_unsupported_version() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 27.0").unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn clamps_future_versions_in_legacy_greeting() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 40.1\n").expect("must clamp");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parses_large_future_version_numbers_by_clamping() {
        let parsed = parse_legacy_daemon_greeting("@RSYNCD: 999999999999.0\n").expect("must clamp");
        assert_eq!(parsed, ProtocolVersion::NEWEST);
    }

    #[test]
    fn parse_ascii_digits_to_u32_saturates_on_overflow() {
        let digits = "999999999999999999999999999999";
        assert_eq!(parse_ascii_digits_to_u32(digits), u32::MAX);
    }

    #[test]
    fn rejects_greeting_with_missing_prefix() {
        let err = parse_legacy_daemon_greeting("RSYNCD 32").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_without_version_digits() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: .0").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_fractional_without_digits() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 31.\n").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn rejects_greeting_with_non_numeric_suffix() {
        let err = parse_legacy_daemon_greeting("@RSYNCD: 31.0beta").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn formats_legacy_daemon_greeting_for_newest_protocol() {
        let rendered = format_legacy_daemon_greeting(ProtocolVersion::NEWEST);
        assert_eq!(rendered, "@RSYNCD: 32.0\n");
    }

    #[test]
    fn formatted_legacy_greeting_round_trips_through_parser() {
        let version = ProtocolVersion::try_from(29).expect("valid version");
        let rendered = format_legacy_daemon_greeting(version);
        let parsed = parse_legacy_daemon_greeting(&rendered).expect("parseable banner");
        assert_eq!(parsed, version);
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
    fn rejects_non_utf8_legacy_daemon_message_bytes() {
        let err = parse_legacy_daemon_message_bytes(b"@RSYNCD: AUTHREQD\xff").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn parses_legacy_error_message_and_trims_payload() {
        let payload =
            parse_legacy_error_message("@ERROR: access denied\r\n").expect("error payload");
        assert_eq!(payload, "access denied");
    }

    #[test]
    fn parses_legacy_error_message_bytes() {
        let payload = parse_legacy_error_message_bytes(b"@ERROR: access denied\n").expect("parse");
        assert_eq!(payload, Some("access denied"));
    }

    #[test]
    fn rejects_non_utf8_legacy_error_message_bytes() {
        let err = parse_legacy_error_message_bytes(b"@ERROR: denied\xff").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn parses_legacy_warning_message_and_trims_payload() {
        let payload =
            parse_legacy_warning_message("@WARNING: will retry  \n").expect("warning payload");
        assert_eq!(payload, "will retry");
    }

    #[test]
    fn parses_legacy_warning_message_bytes() {
        let payload =
            parse_legacy_warning_message_bytes(b"@WARNING: watch out\r\n").expect("parse");
        assert_eq!(payload, Some("watch out"));
    }
}
