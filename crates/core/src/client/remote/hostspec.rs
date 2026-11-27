//! Byte-level port of upstream's operand hostspec parser.
//!
//! Used to validate every source of a remote pull and to strip each source's
//! hostspec before its path is sent to a daemon, exactly as upstream walks
//! `remote_argv` in `main.c:1588-1610`.
//!
//! # Upstream Reference
//!
//! - `options.c:3322-3370` `parse_hostspec()`
//! - `options.c:3379-3405` `check_for_hostspec()`

use std::ffi::{OsStr, OsString};
use std::net::Ipv6Addr;

/// `URL_PREFIX` from `rsync.h`, matched case-insensitively.
const URL_PREFIX: &[u8] = b"rsync://";

/// A source operand split at its hostspec: the `[USER@]HOST` part (IPv6
/// brackets elided) and the byte offset where the path starts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hostspec {
    pub(crate) host: String,
    pub(crate) path_start: usize,
    /// Length of the `USER@` prefix of `host`, 0 without one.
    pub(crate) user_len: usize,
    /// Whether the host was written as `[...]`.
    pub(crate) bracketed: bool,
}

/// upstream: options.c:3322 `parse_hostspec()`. `port` is `Some` only for the
/// body of an `rsync://` URL, where running out of string or meeting a `/`
/// ends the host and a `:PORT` is parsed into it.
fn parse_hostspec(s: &[u8], mut port: Option<&mut i32>) -> Option<Hostspec> {
    let url = port.is_some();
    let mut host_start = 0;
    let mut hostlen = 0;
    let mut userlen = 0;
    let mut bracketed = false;
    let mut i = 0;
    loop {
        let Some(&c) = s.get(i) else {
            if !url {
                return None;
            }
            if hostlen == 0 {
                hostlen = i - host_start;
            }
            break;
        };
        if c == b':' || c == b'/' {
            if hostlen == 0 {
                hostlen = i - host_start;
            }
            i += 1;
            if c == b'/' {
                if !url {
                    return None;
                }
            } else if let Some(port) = port.as_deref_mut() {
                let digits = s[i..].iter().take_while(|b| b.is_ascii_digit()).count();
                *port = atoi(&s[i..i + digits]);
                i += digits;
                if let Some(&next) = s.get(i) {
                    i += 1;
                    if next != b'/' {
                        return None;
                    }
                }
            }
            break;
        }
        if c == b'@' {
            userlen = i + 1;
            host_start = i + 1;
        } else if c == b'[' {
            if i != host_start {
                return None;
            }
            host_start += 1;
            bracketed = true;
            while s.get(i).is_some_and(|&b| b != b']' && b != b'/') {
                i += 1;
            }
            hostlen = i - host_start;
            let closed = s.get(i) == Some(&b']');
            let follows_ok = s.get(i + 1).is_none_or(|&b| b == b'/' || b == b':');
            if !closed || !follows_ok || hostlen == 0 {
                return None;
            }
        }
        i += 1;
    }
    let mut host = Vec::with_capacity(userlen + hostlen);
    host.extend_from_slice(&s[..userlen]);
    host.extend_from_slice(&s[host_start..host_start + hostlen]);
    Some(Hostspec {
        host: String::from_utf8_lossy(&host).into_owned(),
        path_start: i,
        user_len: userlen,
        bracketed,
    })
}

/// `atoi()` over a run of ASCII digits; an empty run is 0, like the C call on
/// a non-digit. Saturates instead of overflowing.
fn atoi(digits: &[u8]) -> i32 {
    digits.iter().fold(0i32, |acc, &d| {
        acc.saturating_mul(10).saturating_add(i32::from(d - b'0'))
    })
}

/// upstream: options.c:3379 `check_for_hostspec()`.
///
/// Recognises `HOST:PATH`, `HOST::PATH` and `rsync://HOST[:PORT]/PATH`. With
/// `port` given, a daemon spelling leaves it non-zero: an explicit URL port,
/// `-1` for "the default" when it was 0, or its earlier value otherwise; a
/// single-colon spelling sets it to 0. Returns `None` for a local path.
pub(crate) fn check_for_hostspec(s: &[u8], mut port: Option<&mut i32>) -> Option<Hostspec> {
    if let Some(port) = port.as_deref_mut()
        && has_url_prefix(s)
        && let Some(spec) = parse_hostspec(&s[URL_PREFIX.len()..], Some(port))
    {
        if *port == 0 {
            *port = -1;
        }
        return Some(Hostspec {
            path_start: URL_PREFIX.len() + spec.path_start,
            ..spec
        });
    }

    let spec = parse_hostspec(s, None)?;
    if s.get(spec.path_start) == Some(&b':') {
        if let Some(port) = port
            && *port == 0
        {
            *port = -1;
        }
        return Some(Hostspec {
            path_start: spec.path_start + 1,
            ..spec
        });
    }
    if let Some(port) = port {
        *port = 0;
    }
    Some(spec)
}

/// Whether `operand` names an rsync daemon: `HOST::PATH`, or any operand
/// with the `rsync://` prefix (matched case-insensitively).
///
/// upstream: main.c:1480 and :1527 pass the operand to `check_for_hostspec()`
/// with `&rsync_port`, and a non-zero port afterwards selects a daemon
/// connection (main.c:1501, :1535). The `[...]` of an IPv6 literal is skipped
/// by that parser, so the `::` inside `[::1]:path` never counts. An
/// `rsync://` URL whose authority it cannot split, such as a bare IPv6
/// address, falls back in 3.5.2 to `HOST:PATH` with the host `rsync`; oc keeps
/// it on the daemon path, whose address parser refuses it.
pub(crate) fn is_daemon_operand(operand: &OsStr) -> bool {
    let bytes = operand_bytes(operand);
    let mut port = 0;
    has_url_prefix(bytes) || (check_for_hostspec(bytes, Some(&mut port)).is_some() && port != 0)
}

/// Whether `s` starts with `URL_PREFIX`, ignoring case (options.c:3383).
fn has_url_prefix(s: &[u8]) -> bool {
    s.get(..URL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(URL_PREFIX))
}

/// Byte offsets of a `HOST::PATH` operand: the end of its `[USER@]HOST` text
/// as typed (brackets kept) and the start of its path. `None` for any other
/// spelling.
pub(crate) fn double_colon_split(operand: &OsStr) -> Option<(usize, usize)> {
    let bytes = operand_bytes(operand);
    let spec = check_for_hostspec(bytes, None)?;
    let host_end = spec.path_start.checked_sub(2)?;
    (bytes.get(host_end..spec.path_start) == Some(b"::".as_slice()))
        .then_some((host_end, spec.path_start))
}

/// Refusal text for brackets around anything but an IPv6 address.
pub(crate) const BRACKETED_NON_IPV6: &str =
    "Invalid remote host: only an IPv6 address may be enclosed in brackets.";

/// Refusal text for an IPv6 address written without brackets.
pub(crate) const UNBRACKETED_IPV6: &str = "IPv6 daemon addresses must be enclosed in brackets";

/// Checks a host token against the bracket rule of RFC 3986 section 3.2.2.
///
/// `host` is the token without its brackets; `bracketed` says whether it had
/// them. A bracketed token must be an IPv6 address, optionally followed by a
/// non-empty `%zone` that is kept verbatim (RFC 6874). An unbracketed token is
/// a host name or IPv4 address, neither of which can contain `:`.
///
/// Deliberate deviation: upstream's `parse_hostspec()` (options.c:3354-3360)
/// checks only the bracket structure, so 3.5.2 passes `[1.2.3.4]` or
/// `[example.com]` on with the brackets stripped.
///
/// # Errors
///
/// The refusal text, [`BRACKETED_NON_IPV6`] or [`UNBRACKETED_IPV6`].
pub(crate) fn check_host_literal(host: &str, bracketed: bool) -> Result<(), &'static str> {
    if !bracketed {
        return if host.contains(':') {
            Err(UNBRACKETED_IPV6)
        } else {
            Ok(())
        };
    }
    let (address, zone) = host.split_once('%').unwrap_or((host, "-"));
    if zone.is_empty() || address.parse::<Ipv6Addr>().is_err() {
        return Err(BRACKETED_NON_IPV6);
    }
    Ok(())
}

/// Applies [`check_host_literal`] to the host of a hostspec operand. A local
/// operand has no host and passes.
///
/// # Errors
///
/// The refusal text of [`check_host_literal`].
pub(crate) fn check_operand_host(operand: &OsStr) -> Result<(), &'static str> {
    let mut port = 0;
    match check_for_hostspec(operand_bytes(operand), Some(&mut port)) {
        Some(spec) => check_host_literal(&spec.host[spec.user_len..], spec.bracketed),
        None => Ok(()),
    }
}

/// Raw bytes of an operand: the verbatim filesystem bytes on Unix, the
/// WTF-8 encoding elsewhere.
pub(crate) fn operand_bytes(operand: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        operand.as_bytes()
    }
    #[cfg(not(unix))]
    {
        operand.as_encoded_bytes()
    }
}

/// The path part of a remote source, with its hostspec stripped the way
/// `main.c:1589` does (`remote_argv[i] = arg`). `None` for a local operand.
///
/// The bytes after the hostspec are kept verbatim, so a non-UTF-8 name
/// reaches the daemon unchanged.
pub(crate) fn source_path(operand: &OsStr) -> Option<OsString> {
    let bytes = operand_bytes(operand);
    let mut port = 0;
    let spec = quic_url_hostspec(bytes).or_else(|| check_for_hostspec(bytes, Some(&mut port)))?;
    let tail = &bytes[spec.path_start..];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(OsString::from_vec(tail.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Some(OsString::from(String::from_utf8_lossy(tail).into_owned()))
    }
}

/// oc's `quic://HOST[:PORT]/PATH` spelling (no upstream counterpart), split
/// with the same authority grammar as `rsync://`.
fn quic_url_hostspec(s: &[u8]) -> Option<Hostspec> {
    const QUIC_PREFIX: &[u8] = b"quic://";
    if s.len() < QUIC_PREFIX.len() || !s[..QUIC_PREFIX.len()].eq_ignore_ascii_case(QUIC_PREFIX) {
        return None;
    }
    let mut port = 0;
    let spec = parse_hostspec(&s[QUIC_PREFIX.len()..], Some(&mut port))?;
    Some(Hostspec {
        path_start: QUIC_PREFIX.len() + spec.path_start,
        ..spec
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(s: &str, start: i32) -> Option<(String, String, i32)> {
        let mut port = start;
        check_for_hostspec(s.as_bytes(), Some(&mut port))
            .map(|spec| (spec.host, s[spec.path_start..].to_owned(), port))
    }

    fn t(host: &str, path: &str, port: i32) -> Option<(String, String, i32)> {
        Some((host.to_owned(), path.to_owned(), port))
    }

    #[test]
    fn url_with_port_sets_the_port_and_strips_the_authority() {
        assert_eq!(check("rsync://h:8873/mod/I", 0), t("h", "mod/I", 8873));
    }

    #[test]
    fn url_without_port_asks_for_the_default_only_when_none_is_set() {
        assert_eq!(check("rsync://h/mod/I", 0), t("h", "mod/I", -1));
        // main.c:1588 carries dummy_port across args, so an earlier port stays.
        assert_eq!(check("rsync://h/mod/I", 8873), t("h", "mod/I", 8873));
    }

    #[test]
    fn double_colon_is_a_daemon_spec_and_keeps_an_earlier_port() {
        assert_eq!(check("h::mod/J", 0), t("h", "mod/J", -1));
        assert_eq!(check("h::mod/J", 8873), t("h", "mod/J", 8873));
    }

    #[test]
    fn single_colon_is_a_shell_spec_and_clears_the_port() {
        assert_eq!(check("h:/p/I", 8873), t("h", "/p/I", 0));
    }

    #[test]
    fn empty_hosts_parse_with_an_empty_name() {
        assert_eq!(check("::mod/J", 8873), t("", "mod/J", 8873));
        assert_eq!(check("rsync:///mod/J", 8873), t("", "mod/J", 8873));
    }

    #[test]
    fn user_and_ipv6_brackets_follow_parse_hostspec() {
        assert_eq!(check("u@h::mod", 0), t("u@h", "mod", -1));
        assert_eq!(check("rsync://u@[::1]:99/mod", 0), t("u@::1", "mod", 99));
        assert_eq!(check("[::1]:/p", 0), t("::1", "/p", 0));
    }

    #[test]
    fn local_paths_have_no_hostspec() {
        assert_eq!(check("/abs/path", 5), None);
        assert_eq!(check("rel/path", 5), None);
        assert_eq!(check("plain", 5), None);
    }

    #[test]
    fn source_path_strips_every_spelling_to_the_module_path() {
        assert_eq!(
            source_path(OsStr::new("rsync://h:1/mod/J")),
            Some("mod/J".into())
        );
        assert_eq!(source_path(OsStr::new("h::mod/J")), Some("mod/J".into()));
        assert_eq!(
            source_path(OsStr::new("quic://h:1/mod/J")),
            Some("mod/J".into())
        );
        assert_eq!(source_path(OsStr::new("local")), None);
    }

    /// The `::` inside an IPv6 literal is address text: only a `::` after the
    /// host, or an `rsync://` prefix in any case, makes a daemon operand
    /// (main.c:1480, :1501).
    #[test]
    fn daemon_operands_follow_check_for_hostspec() {
        for (operand, daemon) in [
            ("[::1]:Music", false),
            ("user@[::1]:Music", false),
            ("[::1]:Music/sub", false),
            ("[::1]:", false),
            ("[fda4:d471:fb49:cb00::3]:Music", false),
            ("host:a::b", false),
            ("/abs/a::b", false),
            ("[::1]::mod", true),
            ("user@[::1]::mod", true),
            ("host::mod", true),
            ("::mod", true),
            ("rsync://[::1]/mod", true),
            ("Rsync://h/m", true),
            ("rsync://::1/mod", true),
        ] {
            assert_eq!(is_daemon_operand(OsStr::new(operand)), daemon, "{operand}");
        }
    }

    /// The host of `HOST::PATH` keeps its brackets for the daemon-address
    /// parser, and the path starts after the `::` that ends the host.
    #[test]
    fn double_colon_split_ends_the_host_after_its_brackets() {
        for (operand, split) in [
            ("[::1]::mod", Some(("[::1]", "mod"))),
            ("u@[fe80::1%lo]::mod/p", Some(("u@[fe80::1%lo]", "mod/p"))),
            ("h::", Some(("h", ""))),
            ("::m", Some(("", "m"))),
            ("[::1]:p", None),
            ("h:/a::b", None),
            ("Rsync://h/m", None),
            ("local", None),
        ] {
            let got = double_colon_split(OsStr::new(operand))
                .map(|(end, start)| (&operand[..end], &operand[start..]));
            assert_eq!(got, split, "{operand}");
        }
    }

    /// RFC 3986 section 3.2.2: brackets hold an IPv6 address and nothing else,
    /// with an RFC 6874 zone kept verbatim; a host name or IPv4 address is
    /// never bracketed, and an IPv6 address always is.
    #[test]
    fn host_literals_follow_the_bracket_rule() {
        for (host, bracketed, verdict) in [
            ("::1", true, Ok(())),
            ("fda4:d471:fb49:cb00::3", true, Ok(())),
            ("::ffff:127.0.0.1", true, Ok(())),
            ("fe80::1%eth0", true, Ok(())),
            ("fe80::1%lo", true, Ok(())),
            ("1.2.3.4", true, Err(BRACKETED_NON_IPV6)),
            ("example.com", true, Err(BRACKETED_NON_IPV6)),
            ("garbage", true, Err(BRACKETED_NON_IPV6)),
            ("1:2:3", true, Err(BRACKETED_NON_IPV6)),
            ("fe80::1%", true, Err(BRACKETED_NON_IPV6)),
            ("%lo", true, Err(BRACKETED_NON_IPV6)),
            ("", true, Err(BRACKETED_NON_IPV6)),
            ("1.2.3.4", false, Ok(())),
            ("example.com", false, Ok(())),
            ("", false, Ok(())),
            ("::1", false, Err(UNBRACKETED_IPV6)),
            ("fe80::1%lo", false, Err(UNBRACKETED_IPV6)),
            ("fe80:", false, Err(UNBRACKETED_IPV6)),
        ] {
            assert_eq!(
                check_host_literal(host, bracketed),
                verdict,
                "{host:?} bracketed={bracketed}"
            );
        }
    }

    /// Every operand spelling reaches the bracket rule through its hostspec;
    /// a local path has no host to check.
    #[test]
    fn operand_hosts_are_checked_in_every_spelling() {
        for (operand, verdict) in [
            ("[::1]:p", Ok(())),
            ("user@[::1]:p", Ok(())),
            ("[::1]::mod", Ok(())),
            ("rsync://[::1]:8873/mod", Ok(())),
            ("1.2.3.4:p", Ok(())),
            ("host::mod", Ok(())),
            ("[1.2.3.4]:p", Err(BRACKETED_NON_IPV6)),
            ("user@[example.com]:p", Err(BRACKETED_NON_IPV6)),
            ("[garbage]::mod", Err(BRACKETED_NON_IPV6)),
            ("rsync://[1.2.3.4]/mod", Err(BRACKETED_NON_IPV6)),
            ("/local/[x]:p", Ok(())),
        ] {
            assert_eq!(
                check_operand_host(OsStr::new(operand)),
                verdict,
                "{operand}"
            );
        }
    }
}
