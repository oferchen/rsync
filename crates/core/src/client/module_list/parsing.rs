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

    let decoded = addr.to_owned();

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

// The daemon host and username are taken from the URL VERBATIM.
//
// upstream: options.c:3295 `check_for_hostspec()` parses `rsync://HOST:PORT/PATH`
// and never decodes anything - rsync 3.5.0 contains no percent-decoder at all,
// so `%` is an ordinary hostname byte. That is load-bearing rather than an
// omission: `socket.c:204-215 shell_unsafe_connect_host()` refuses a host
// beginning with `%` precisely because a nested fish would expand `%self`, and
// it can only refuse what reaches it. An IPv6 zone id (`fe80::1%eth0`) relies on
// the same literal treatment.
//
// oc previously decoded both components and rejected a lone `%` as a malformed
// escape unless the text before it contained `:`. That rejected `%self` before
// `RSYNC_CONNECT_PROG` validation could refuse it, and it silently rewrote any
// host or username containing a `%XX` sequence.

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
    let (username, input) = split_daemon_username(input)?;
    let username = username.map(str::to_owned);

    if input.is_empty() {
        // An empty authority stays empty. Upstream substitutes no default:
        // `rsync:///mod/` reaches getaddrinfo with an empty node and fails
        // there, and under RSYNC_CONNECT_PROG it reaches
        // shell_unsafe_connect_host (socket.c:204-215), which refuses it -
        // an empty argument survives a direct exec but vanishes when a
        // nested shell re-splits, shifting every later argument left.
        let address = DaemonAddress::new(String::new(), default_port);
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, default_port)?;
        let address = DaemonAddress::new(address, port);
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
            let host = host.to_owned();
            let address = DaemonAddress::new(host, port);
            return Ok(ParsedDaemonTarget { address, username });
        }

        if !host_contains_colon {
            return Err(daemon_error(
                "invalid daemon port",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let host = input.to_owned();
    let address = DaemonAddress::new(host, default_port);
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

    /// A `%XX` sequence in the host reaches the connector as typed.
    ///
    /// upstream: rsync 3.5.0 has no percent-decoder; `%` is an ordinary
    /// hostname byte. These assertions are the INVERSE of what oc recorded
    /// while it decoded - `hello%20world` used to become `hello world`.
    #[test]
    fn parse_host_port_keeps_percent_sequences_in_the_host_verbatim() {
        for host in ["hello%20world", "%41%42%43", "%self"] {
            let parsed = parse_host_port(host, 873).expect("parse");
            assert_eq!(
                parsed.address.host(),
                host,
                "host {host:?} must be verbatim"
            );
        }
    }

    /// `rsync:///mod/` names an empty host, and upstream substitutes nothing
    /// for it - it reaches getaddrinfo (which fails) or, under
    /// RSYNC_CONNECT_PROG, shell_unsafe_connect_host (which refuses it).
    /// oc used to swap in `localhost`, which both invented a target the
    /// operator never named and hid the empty host from the validator.
    #[test]
    fn parse_host_port_keeps_an_empty_host_empty() {
        let parsed = parse_host_port("", 873).expect("parse");
        assert_eq!(parsed.address.host(), "");
        assert_eq!(parsed.address.port(), 873);
    }

    /// Non-vacuity companion: the empty case above must not be passing
    /// because every host now comes back empty.
    #[test]
    fn parse_host_port_still_returns_a_named_host() {
        let parsed = parse_host_port("example.com", 873).expect("parse");
        assert_eq!(parsed.address.host(), "example.com");
    }

    /// The username is taken verbatim for the same reason, and from the same
    /// absence of a decoder upstream. `user%40domain` is NOT `user@domain`:
    /// the split already happened at the last `@`, so decoding one here would
    /// invent a second, later separator that upstream never sees.
    #[test]
    fn parse_host_port_keeps_percent_sequences_in_the_username_verbatim() {
        let parsed = parse_host_port("user%40domain@localhost", 873).expect("parse");
        assert_eq!(parsed.username.as_deref(), Some("user%40domain"));
        assert_eq!(parsed.address.host(), "localhost");
    }

    /// An IPv6 zone id keeps working, and now does so WITHOUT a special case.
    /// oc previously needed a `zone_fallback` that kept a bare `%` literal only
    /// when the text before it contained `:` - that branch existed solely to
    /// stop the decoder mangling this shape, and it is gone with the decoder.
    #[test]
    fn parse_host_port_keeps_an_ipv6_zone_id() {
        let parsed = parse_host_port("[fe80::1%eth0]", 873).expect("parse");
        assert_eq!(parsed.address.host(), "fe80::1%eth0");
    }

    /// The non-vacuity companion: a plain host is unaffected, so the three
    /// tests above are pinning the percent behaviour rather than a parser that
    /// stopped working.
    #[test]
    fn parse_host_port_still_parses_a_plain_host_and_port() {
        let parsed = parse_host_port("localhost:8873", 873).expect("parse");
        assert_eq!(parsed.address.host(), "localhost");
        assert_eq!(parsed.address.port(), 8873);
        assert!(parsed.username.is_none());
    }

    #[test]
    fn parse_bracketed_host_extracts_ipv6() {
        let result = parse_bracketed_host("::1]", 873).expect("parse");
        assert_eq!(result, ("::1".to_owned(), 873));
    }

    #[test]
    fn parse_bracketed_host_extracts_ipv6_with_port() {
        let result = parse_bracketed_host("::1]:8873", 873).expect("parse");
        assert_eq!(result, ("::1".to_owned(), 8873));
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
        assert_eq!(result.username, Some("user".to_owned()));
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
