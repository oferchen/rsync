use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

use super::{LEGACY_DAEMON_PREFIX, malformed_legacy_greeting};

/// Detailed representation of a legacy ASCII daemon greeting.
///
/// Legacy daemons announce their protocol support via lines such as
/// `@RSYNCD: 31.0 md4 md5`. Besides the major protocol number the banner may
/// contain a fractional component (known as the "subprotocol") and an optional
/// digest list used for challenge/response authentication. Upstream rsync
/// retains all of this metadata during negotiation so the Rust implementation
/// mirrors that structure to avoid lossy parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyDaemonGreeting<'a> {
    protocol: ProtocolVersion,
    advertised_protocol: u32,
    subprotocol: Option<u32>,
    digest_list: Option<&'a str>,
}

impl<'a> LegacyDaemonGreeting<'a> {
    /// Returns the negotiated protocol version after clamping unsupported
    /// advertisements to the newest supported release.
    #[must_use]
    pub const fn protocol(self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns the protocol number advertised by the peer before clamping.
    ///
    /// Future peers may announce versions newer than we support. Upstream rsync
    /// still records the advertised value, so the helper exposes it for higher
    /// layers that mirror that behaviour.
    #[must_use]
    pub const fn advertised_protocol(self) -> u32 {
        self.advertised_protocol
    }

    /// Returns the parsed subprotocol value or zero when it was absent.
    #[must_use]
    pub const fn subprotocol(self) -> u32 {
        match self.subprotocol {
            Some(value) => value,
            None => 0,
        }
    }

    /// Reports whether the greeting explicitly supplied a subprotocol suffix.
    #[must_use]
    pub const fn has_subprotocol(self) -> bool {
        self.subprotocol.is_some()
    }

    /// Returns the digest list announced by the daemon, if any.
    #[must_use]
    pub const fn digest_list(self) -> Option<&'a str> {
        self.digest_list
    }
}

/// Parses a legacy ASCII daemon greeting of the form `@RSYNCD: <version>`.
///
/// This convenience wrapper retains the historical API by returning only the
/// negotiated [`ProtocolVersion`]. Callers that need access to the advertised
/// protocol number, subprotocol suffix, or digest list should use
/// [`parse_legacy_daemon_greeting_details`].
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    parse_legacy_daemon_greeting_details(line).map(LegacyDaemonGreeting::protocol)
}

/// Parses a legacy daemon greeting and returns a structured representation.
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting_details(
    line: &str,
) -> Result<LegacyDaemonGreeting<'_>, NegotiationError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let malformed = || malformed_legacy_greeting(trimmed);

    let after_prefix = trimmed
        .strip_prefix(LEGACY_DAEMON_PREFIX)
        .ok_or_else(malformed)?;

    let mut remainder = after_prefix.trim_start();
    if remainder.is_empty() {
        return Err(malformed());
    }

    let digits_len = ascii_digit_prefix_len(remainder);
    if digits_len == 0 {
        return Err(malformed());
    }

    let digits = &remainder[..digits_len];
    let advertised_protocol = parse_ascii_digits_to_u32(digits);
    remainder = &remainder[digits_len..];

    let mut subprotocol = None;
    loop {
        let trimmed_remainder = remainder.trim_start_matches(char::is_whitespace);
        let had_leading_whitespace = trimmed_remainder.len() != remainder.len();

        if trimmed_remainder.is_empty() {
            remainder = trimmed_remainder;
            break;
        }

        if let Some(after_dot) = trimmed_remainder.strip_prefix('.') {
            let fractional_len = ascii_digit_prefix_len(after_dot);
            if fractional_len == 0 {
                return Err(malformed());
            }

            let fractional_digits = &after_dot[..fractional_len];
            subprotocol = Some(parse_ascii_digits_to_u32(fractional_digits));
            remainder = &after_dot[fractional_len..];
            continue;
        }

        if !had_leading_whitespace {
            return Err(malformed());
        }

        remainder = trimmed_remainder;
        break;
    }

    if advertised_protocol >= 31 && subprotocol.is_none() {
        return Err(malformed());
    }

    let digest_list = remainder.trim();
    let digest_list = if digest_list.is_empty() {
        None
    } else {
        Some(digest_list)
    };

    let negotiated = advertised_protocol.min(u32::from(u8::MAX)) as u8;
    let protocol = ProtocolVersion::from_peer_advertisement(negotiated)?;

    Ok(LegacyDaemonGreeting {
        protocol,
        advertised_protocol,
        subprotocol,
        digest_list,
    })
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
    fn parses_legacy_daemon_greeting_details_with_digest_list() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0 md4 md5\n")
            .expect("digest list should parse");

        assert_eq!(
            greeting.protocol(),
            ProtocolVersion::from_supported(31).unwrap()
        );
        assert_eq!(greeting.advertised_protocol(), 31);
        assert!(greeting.has_subprotocol());
        assert_eq!(greeting.subprotocol(), 0);
        assert_eq!(greeting.digest_list(), Some("md4 md5"));
    }

    #[test]
    fn greeting_details_accepts_trailing_whitespace_in_digest_list() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 31.0   md4   md5  \r\n")
            .expect("digest list should tolerate padding");

        assert_eq!(greeting.digest_list(), Some("md4   md5"));
    }

    #[test]
    fn greeting_details_records_absence_of_subprotocol_for_old_versions() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 29\n")
            .expect("old protocols may omit subprotocol");

        assert_eq!(greeting.protocol().as_u8(), 29);
        assert!(!greeting.has_subprotocol());
        assert_eq!(greeting.subprotocol(), 0);
    }

    #[test]
    fn greeting_details_rejects_missing_subprotocol_for_newer_versions() {
        let err = parse_legacy_daemon_greeting_details("@RSYNCD: 31\n").unwrap_err();
        assert!(matches!(
            err,
            NegotiationError::MalformedLegacyGreeting { .. }
        ));
    }

    #[test]
    fn greeting_details_clamps_future_versions_but_retains_advertisement() {
        let greeting = parse_legacy_daemon_greeting_details("@RSYNCD: 999.1\n")
            .expect("future versions clamp");

        assert_eq!(greeting.protocol(), ProtocolVersion::NEWEST);
        assert_eq!(greeting.advertised_protocol(), 999);
        assert_eq!(greeting.subprotocol(), 1);
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
