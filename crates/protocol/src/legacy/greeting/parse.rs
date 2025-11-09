use crate::error::NegotiationError;
use crate::version::ProtocolVersion;

use super::super::{LEGACY_DAEMON_PREFIX, malformed_legacy_greeting};
use super::types::{LegacyDaemonGreeting, LegacyDaemonGreetingOwned};

/// Parses a legacy ASCII daemon greeting of the form `@RSYNCD: <version>`.
///
/// This convenience wrapper retains the historical API by returning only the
/// negotiated [`ProtocolVersion`]. Callers that need access to the advertised
/// protocol number, subprotocol suffix, or digest list should use
/// [`parse_legacy_daemon_greeting_details`].
#[doc(alias = "@RSYNCD")]
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting(line: &str) -> Result<ProtocolVersion, NegotiationError> {
    parse_legacy_daemon_greeting_details(line).map(LegacyDaemonGreeting::protocol)
}

/// Parses a legacy ASCII daemon greeting and returns an owned representation.
///
/// Legacy negotiation helpers frequently need to retain the parsed metadata
/// beyond the lifetime of the buffer that backed the original line. This
/// wrapper mirrors [`parse_legacy_daemon_greeting_details`] but converts the
/// borrowed [`LegacyDaemonGreeting`] into the fully owned
/// [`LegacyDaemonGreetingOwned`], allowing callers to drop the input buffer
/// immediately after parsing.
///
/// # Examples
///
/// ```
/// use protocol::{parse_legacy_daemon_greeting_owned, ProtocolVersion};
///
/// let owned = parse_legacy_daemon_greeting_owned("@RSYNCD: 29\n")?;
/// assert_eq!(owned.protocol(), ProtocolVersion::from_supported(29).unwrap());
/// assert_eq!(owned.advertised_protocol(), 29);
/// assert!(!owned.has_subprotocol());
/// # Ok::<_, protocol::NegotiationError>(())
/// ```
#[doc(alias = "@RSYNCD")]
#[must_use = "legacy daemon greeting parsing errors must be handled"]
pub fn parse_legacy_daemon_greeting_owned(
    line: &str,
) -> Result<LegacyDaemonGreetingOwned, NegotiationError> {
    parse_legacy_daemon_greeting_details(line).map(Into::into)
}

/// Parses a legacy daemon greeting and returns a structured representation.
#[doc(alias = "@RSYNCD")]
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

    let protocol = ProtocolVersion::from_peer_advertisement(advertised_protocol)?;

    Ok(LegacyDaemonGreeting::new(
        protocol,
        advertised_protocol,
        subprotocol,
        digest_list,
    ))
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

#[cfg(test)]
mod tests {
    use super::{ascii_digit_prefix_len, parse_ascii_digits_to_u32};

    #[test]
    fn ascii_prefix_len_counts_digits_only() {
        assert_eq!(ascii_digit_prefix_len("123abc"), 3);
        assert_eq!(ascii_digit_prefix_len("abc"), 0);
        assert_eq!(ascii_digit_prefix_len(""), 0);
    }

    #[test]
    fn parse_digits_to_u32_saturates() {
        assert_eq!(parse_ascii_digits_to_u32("0"), 0);
        assert_eq!(parse_ascii_digits_to_u32("42"), 42);
        assert_eq!(parse_ascii_digits_to_u32("4294967296"), u32::MAX);
    }
}
