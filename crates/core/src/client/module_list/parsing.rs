use super::super::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, daemon_error};
use super::DaemonAddress;

pub(crate) fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }

    let (candidate, remainder) = text.split_at(prefix.len());
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(remainder)
    } else {
        None
    }
}

pub(crate) fn parse_bracketed_host(
    host: &str,
    default_port: u16,
) -> Result<(String, u16), ClientError> {
    let (addr, remainder) = host.split_once(']').ok_or_else(|| {
        daemon_error(
            "invalid bracketed daemon host",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let decoded = decode_host_component(addr)?;

    if remainder.is_empty() {
        return Ok((decoded, default_port));
    }

    let port = remainder
        .strip_prefix(':')
        .ok_or_else(|| {
            daemon_error(
                "invalid bracketed daemon host",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            )
        })?
        .parse::<u16>()
        .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;

    Ok((decoded, port))
}

pub(crate) fn decode_host_component(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_percent_encoding_error,
        invalid_host_utf8_error,
    )
}

pub(crate) fn decode_daemon_username(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_username_percent_encoding_error,
        invalid_username_utf8_error,
    )
}

pub(crate) fn decode_percent_component(
    input: &str,
    invalid_percent: fn() -> ClientError,
    invalid_utf8: fn() -> ClientError,
) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let zone_fallback = input[..index].contains(':');

            if index + 2 >= bytes.len() {
                if zone_fallback {
                    decoded.push(bytes[index]);
                    index += 1;
                    continue;
                }
                return Err(invalid_percent());
            }

            let hi = hex_value(bytes[index + 1]);
            let lo = hex_value(bytes[index + 2]);

            match (hi, lo) {
                (Some(hi), Some(lo)) => {
                    decoded.push((hi << 4) | lo);
                    index += 3;
                    continue;
                }
                _ if zone_fallback => {
                    decoded.push(bytes[index]);
                    index += 1;
                    continue;
                }
                _ => {
                    return Err(invalid_percent());
                }
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| invalid_utf8())
}

pub(crate) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cold]
pub(crate) fn invalid_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon host",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

#[cold]
pub(crate) fn invalid_host_utf8_error() -> ClientError {
    daemon_error(
        "daemon host contains invalid UTF-8",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

#[cold]
pub(crate) fn invalid_username_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon username",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

#[cold]
pub(crate) fn invalid_username_utf8_error() -> ClientError {
    daemon_error(
        "daemon username contains invalid UTF-8",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

pub(crate) fn split_host_port(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind(':')?;
    Some((&input[..idx], &input[idx + 1..]))
}

pub(crate) fn split_daemon_username(input: &str) -> Result<(Option<&str>, &str), ClientError> {
    if let Some((username, remainder)) = input.split_once('@') {
        if username.is_empty() {
            return Err(daemon_error(
                "daemon username must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok((Some(username), remainder))
    } else {
        Ok((None, input))
    }
}

pub(crate) fn split_daemon_host_module(input: &str) -> Result<Option<(&str, &str)>, ClientError> {
    if !input.contains('[') {
        let segments = input.split("::");
        if segments.clone().count() > 2 {
            return Err(daemon_error(
                "IPv6 daemon addresses must be enclosed in brackets",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let mut in_brackets = false;
    let mut previous_colon = None;

    for (idx, ch) in input.char_indices() {
        match ch {
            '[' => {
                in_brackets = true;
                previous_colon = None;
            }
            ']' => {
                in_brackets = false;
                previous_colon = None;
            }
            ':' if !in_brackets => {
                if let Some(prev) = previous_colon.filter(|prev| *prev + 1 == idx) {
                    let host = &input[..prev];
                    if !host.contains('[') {
                        let colon_count = host.chars().filter(|&ch| ch == ':').count();
                        if colon_count > 1 {
                            return Err(daemon_error(
                                "IPv6 daemon addresses must be enclosed in brackets",
                                FEATURE_UNAVAILABLE_EXIT_CODE,
                            ));
                        }
                    }
                    let module = &input[idx + 1..];
                    return Ok(Some((host, module)));
                }
                previous_colon = Some(idx);
            }
            _ => {
                previous_colon = None;
            }
        }
    }

    Ok(None)
}

pub(crate) struct ParsedDaemonTarget {
    pub(crate) address: DaemonAddress,
    pub(crate) username: Option<String>,
}

pub(crate) fn parse_host_port(
    input: &str,
    default_port: u16,
) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_HOST: &str = "localhost";

    let (username, input) = split_daemon_username(input)?;
    let username = username.map(decode_daemon_username).transpose()?;

    if input.is_empty() {
        let address = DaemonAddress::new(DEFAULT_HOST.to_string(), default_port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, default_port)?;
        let address = DaemonAddress::new(address, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some((host, port)) = split_host_port(input) {
        let host_contains_colon = host.contains(':');
        let port_is_digits = !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit());

        if port_is_digits {
            if host_contains_colon {
                return Err(daemon_error(
                    "IPv6 daemon addresses must be enclosed in brackets",
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                ));
            }

            let port = port
                .parse::<u16>()
                .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;
            let host = decode_host_component(host)?;
            let address = DaemonAddress::new(host, port)?;
            return Ok(ParsedDaemonTarget { address, username });
        }

        if !host_contains_colon {
            return Err(daemon_error(
                "invalid daemon port",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let host = decode_host_component(input)?;
    let address = DaemonAddress::new(host, default_port)?;
    Ok(ParsedDaemonTarget { address, username })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prefix_ignore_ascii_case_matches_exact() {
        let result = strip_prefix_ignore_ascii_case("@ERROR: message", "@ERROR");
        assert_eq!(result, Some(": message"));
    }

    #[test]
    fn strip_prefix_ignore_ascii_case_matches_lowercase() {
        let result = strip_prefix_ignore_ascii_case("@error: message", "@ERROR");
        assert_eq!(result, Some(": message"));
    }

    #[test]
    fn strip_prefix_ignore_ascii_case_matches_mixed_case() {
        let result = strip_prefix_ignore_ascii_case("@ErRoR: message", "@ERROR");
        assert_eq!(result, Some(": message"));
    }

    #[test]
    fn strip_prefix_ignore_ascii_case_returns_none_for_mismatch() {
        let result = strip_prefix_ignore_ascii_case("@RSYNCD: 31", "@ERROR");
        assert!(result.is_none());
    }

    #[test]
    fn strip_prefix_ignore_ascii_case_returns_none_for_short_text() {
        let result = strip_prefix_ignore_ascii_case("@E", "@ERROR");
        assert!(result.is_none());
    }

    #[test]
    fn hex_value_parses_digits() {
        assert_eq!(hex_value(b'0'), Some(0));
        assert_eq!(hex_value(b'5'), Some(5));
        assert_eq!(hex_value(b'9'), Some(9));
    }

    #[test]
    fn hex_value_parses_lowercase_hex() {
        assert_eq!(hex_value(b'a'), Some(10));
        assert_eq!(hex_value(b'c'), Some(12));
        assert_eq!(hex_value(b'f'), Some(15));
    }

    #[test]
    fn hex_value_parses_uppercase_hex() {
        assert_eq!(hex_value(b'A'), Some(10));
        assert_eq!(hex_value(b'C'), Some(12));
        assert_eq!(hex_value(b'F'), Some(15));
    }

    #[test]
    fn hex_value_returns_none_for_invalid() {
        assert!(hex_value(b'g').is_none());
        assert!(hex_value(b'G').is_none());
        assert!(hex_value(b'z').is_none());
        assert!(hex_value(b' ').is_none());
        assert!(hex_value(b'%').is_none());
    }

    #[test]
    fn split_host_port_splits_on_last_colon() {
        let result = split_host_port("localhost:8873");
        assert_eq!(result, Some(("localhost", "8873")));
    }

    #[test]
    fn split_host_port_handles_ipv6() {
        let result = split_host_port("::1:8873");
        assert_eq!(result, Some(("::1", "8873")));
    }

    #[test]
    fn split_host_port_returns_none_without_colon() {
        let result = split_host_port("localhost");
        assert!(result.is_none());
    }

    #[test]
    fn split_daemon_username_extracts_username() {
        let result = split_daemon_username("user@host").expect("split");
        assert_eq!(result, (Some("user"), "host"));
    }

    #[test]
    fn split_daemon_username_returns_none_without_at() {
        let result = split_daemon_username("host").expect("split");
        assert_eq!(result, (None, "host"));
    }

    #[test]
    fn split_daemon_username_rejects_empty_username() {
        let result = split_daemon_username("@host");
        assert!(result.is_err());
    }

    #[test]
    fn decode_host_component_passes_through_plain_text() {
        let result = decode_host_component("localhost").expect("decode");
        assert_eq!(result, "localhost");
    }

    #[test]
    fn decode_host_component_decodes_percent_encoding() {
        let result = decode_host_component("hello%20world").expect("decode");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn decode_host_component_decodes_multiple_percent_encodings() {
        let result = decode_host_component("%41%42%43").expect("decode");
        assert_eq!(result, "ABC");
    }

    #[test]
    fn decode_daemon_username_passes_through_plain_text() {
        let result = decode_daemon_username("testuser").expect("decode");
        assert_eq!(result, "testuser");
    }

    #[test]
    fn decode_daemon_username_decodes_percent_encoding() {
        let result = decode_daemon_username("user%40domain").expect("decode");
        assert_eq!(result, "user@domain");
    }

    #[test]
    fn parse_bracketed_host_extracts_ipv6() {
        let result = parse_bracketed_host("::1]", 873).expect("parse");
        assert_eq!(result, ("::1".to_string(), 873));
    }

    #[test]
    fn parse_bracketed_host_extracts_ipv6_with_port() {
        let result = parse_bracketed_host("::1]:8873", 873).expect("parse");
        assert_eq!(result, ("::1".to_string(), 8873));
    }

    #[test]
    fn parse_bracketed_host_rejects_missing_bracket() {
        let result = parse_bracketed_host("::1", 873);
        assert!(result.is_err());
    }

    #[test]
    fn parse_bracketed_host_rejects_text_after_bracket() {
        let result = parse_bracketed_host("::1]garbage", 873);
        assert!(result.is_err());
    }

    #[test]
    fn split_daemon_host_module_splits_on_double_colon() {
        let result = split_daemon_host_module("host::module").expect("split");
        assert_eq!(result, Some(("host", "module")));
    }

    #[test]
    fn split_daemon_host_module_returns_none_without_double_colon() {
        let result = split_daemon_host_module("host:port").expect("split");
        assert!(result.is_none());
    }

    #[test]
    fn split_daemon_host_module_handles_bracketed_ipv6() {
        let result = split_daemon_host_module("[::1]::module").expect("split");
        assert_eq!(result, Some(("[::1]", "module")));
    }

    #[test]
    fn parse_host_port_parses_simple_host() {
        let result = parse_host_port("localhost", 873).expect("parse");
        assert_eq!(result.address.port(), 873);
        assert!(result.username.is_none());
    }

    #[test]
    fn parse_host_port_parses_host_with_port() {
        let result = parse_host_port("localhost:8873", 873).expect("parse");
        assert_eq!(result.address.port(), 8873);
    }

    #[test]
    fn parse_host_port_parses_with_username() {
        let result = parse_host_port("user@localhost", 873).expect("parse");
        assert_eq!(result.username, Some("user".to_string()));
    }

    #[test]
    fn parse_host_port_parses_bracketed_ipv6() {
        let result = parse_host_port("[::1]", 873).expect("parse");
        assert_eq!(result.address.port(), 873);
    }

    #[test]
    fn parse_host_port_uses_default_for_empty_input() {
        let result = parse_host_port("", 873).expect("parse");
        assert_eq!(result.address.port(), 873);
    }
}
