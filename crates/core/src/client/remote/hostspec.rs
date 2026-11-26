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

/// `URL_PREFIX` from `rsync.h`, matched case-insensitively.
const URL_PREFIX: &[u8] = b"rsync://";

/// A source operand split at its hostspec: the `[USER@]HOST` part (IPv6
/// brackets elided) and the byte offset where the path starts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Hostspec {
    pub(crate) host: String,
    pub(crate) path_start: usize,
}

/// upstream: options.c:3322 `parse_hostspec()`. `port` is `Some` only for the
/// body of an `rsync://` URL, where running out of string or meeting a `/`
/// ends the host and a `:PORT` is parsed into it.
fn parse_hostspec(s: &[u8], mut port: Option<&mut i32>) -> Option<Hostspec> {
    let url = port.is_some();
    let mut host_start = 0;
    let mut hostlen = 0;
    let mut userlen = 0;
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
        && s.len() >= URL_PREFIX.len()
        && s[..URL_PREFIX.len()].eq_ignore_ascii_case(URL_PREFIX)
        && let Some(spec) = parse_hostspec(&s[URL_PREFIX.len()..], Some(port))
    {
        if *port == 0 {
            *port = -1;
        }
        return Some(Hostspec {
            host: spec.host,
            path_start: URL_PREFIX.len() + spec.path_start,
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
            host: spec.host,
            path_start: spec.path_start + 1,
        });
    }
    if let Some(port) = port {
        *port = 0;
    }
    Some(spec)
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
        host: spec.host,
        path_start: QUIC_PREFIX.len() + spec.path_start,
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
}
