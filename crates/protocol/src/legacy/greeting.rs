use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

use super::{LEGACY_DAEMON_PREFIX, malformed_legacy_greeting};

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
    fn rejects_greeting_with_lowercase_prefix() {
        let err = parse_legacy_daemon_greeting("@rsyncd: 31.0\n").unwrap_err();
        match err {
            NegotiationError::MalformedLegacyGreeting { input } => {
                assert_eq!(input, "@rsyncd: 31.0");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn formats_legacy_daemon_greeting_for_newest_protocol() {
        let rendered = format_legacy_daemon_greeting(ProtocolVersion::NEWEST);
        assert_eq!(rendered, "@RSYNCD: 32.0\n");
    }

    #[test]
    fn formatted_legacy_greeting_round_trips_through_parser() {
        for &version in ProtocolVersion::supported_versions() {
            let rendered = format_legacy_daemon_greeting(version);
            let parsed = parse_legacy_daemon_greeting(&rendered)
                .unwrap_or_else(|err| panic!("failed to parse {rendered:?}: {err}"));
            assert_eq!(parsed, version);
        }
    }
}
