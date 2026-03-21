//! Option name resolution and value parsing.
//!
//! Maps user-facing option name strings (e.g. `"SO_KEEPALIVE"`) to their
//! corresponding `SocketOptionKind`, and provides a minimal numeric parser
//! for option values.

use super::consts;
#[cfg(not(target_family = "windows"))]
use super::consts::{IPTOS_LOWDELAY, IPTOS_THROUGHPUT};
use super::types::SocketOptionKind;

/// Resolves an option name to its `SocketOptionKind`.
///
/// Returns `None` for unrecognized names.
// upstream: socket.c:set_socket_options()
pub(super) fn lookup_socket_option(name: &str) -> Option<SocketOptionKind> {
    match name {
        "SO_KEEPALIVE" => Some(SocketOptionKind::Bool {
            level: consts::SOL_SOCKET,
            option: consts::SO_KEEPALIVE,
        }),
        "SO_REUSEADDR" => Some(SocketOptionKind::Bool {
            level: consts::SOL_SOCKET,
            option: consts::SO_REUSEADDR,
        }),
        #[cfg(any(target_family = "unix", target_os = "windows"))]
        "SO_BROADCAST" => Some(SocketOptionKind::Bool {
            level: consts::SOL_SOCKET,
            option: consts::SO_BROADCAST,
        }),
        "SO_SNDBUF" => Some(SocketOptionKind::Int {
            level: consts::SOL_SOCKET,
            option: consts::SO_SNDBUF,
        }),
        "SO_RCVBUF" => Some(SocketOptionKind::Int {
            level: consts::SOL_SOCKET,
            option: consts::SO_RCVBUF,
        }),
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "solaris",
            target_os = "illumos",
            target_os = "aix",
            target_os = "haiku",
            target_os = "redox",
            target_os = "fuchsia",
            target_os = "nto",
            target_os = "vxworks",
            target_os = "hurd",
            target_os = "cygwin"
        ))]
        "SO_SNDLOWAT" => Some(SocketOptionKind::Int {
            level: libc::SOL_SOCKET,
            option: libc::SO_SNDLOWAT,
        }),
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "solaris",
            target_os = "illumos",
            target_os = "aix",
            target_os = "haiku",
            target_os = "redox",
            target_os = "fuchsia",
            target_os = "nto",
            target_os = "vxworks",
            target_os = "hurd",
            target_os = "cygwin"
        ))]
        "SO_RCVLOWAT" => Some(SocketOptionKind::Int {
            level: libc::SOL_SOCKET,
            option: libc::SO_RCVLOWAT,
        }),
        "SO_SNDTIMEO" => Some(SocketOptionKind::Int {
            level: consts::SOL_SOCKET,
            option: consts::SO_SNDTIMEO,
        }),
        "SO_RCVTIMEO" => Some(SocketOptionKind::Int {
            level: consts::SOL_SOCKET,
            option: consts::SO_RCVTIMEO,
        }),
        "TCP_NODELAY" => Some(SocketOptionKind::Bool {
            level: consts::IPPROTO_TCP,
            option: consts::TCP_NODELAY,
        }),
        #[cfg(not(target_family = "windows"))]
        "IPTOS_LOWDELAY" => Some(SocketOptionKind::On {
            level: libc::IPPROTO_IP,
            option: libc::IP_TOS,
            value: IPTOS_LOWDELAY,
        }),
        #[cfg(not(target_family = "windows"))]
        "IPTOS_THROUGHPUT" => Some(SocketOptionKind::On {
            level: libc::IPPROTO_IP,
            option: libc::IP_TOS,
            value: IPTOS_THROUGHPUT,
        }),
        _ => None,
    }
}

/// Minimal, allocation-free numeric parser for socket option values.
///
/// Stricter than `libc::atoi`-style parsing. Clamps into `i32` range.
pub(super) fn parse_socket_option_value(raw: &str) -> libc::c_int {
    let mut bytes = raw.trim_start().as_bytes().iter().copied();

    let mut sign = 1i64;
    let mut value: i64 = 0;
    let mut digits_consumed = false;

    if let Some(first) = bytes.next() {
        match first {
            b'+' => {}
            b'-' => sign = -1,
            b'0'..=b'9' => {
                digits_consumed = true;
                value = i64::from(first - b'0');
            }
            _ => return 0,
        }
    } else {
        return 0;
    }

    if !digits_consumed {
        if let Some(byte) = bytes.by_ref().next() {
            match byte {
                b'0'..=b'9' => {
                    digits_consumed = true;
                    value = i64::from(byte - b'0');
                }
                _ => return 0,
            }
        } else {
            return 0;
        }
    }

    if digits_consumed {
        for byte in bytes {
            match byte {
                b'0'..=b'9' => {
                    value = value
                        .saturating_mul(10)
                        .saturating_add(i64::from(byte - b'0'));
                }
                _ => break,
            }
        }
    }

    let signed = value.saturating_mul(sign);
    let clamped = signed
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
        .try_into()
        .unwrap_or_else(|_| {
            if signed.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            }
        });
    clamped as libc::c_int
}

/// Interns well-known option names into static string slices so error paths
/// can safely hold references without allocating.
pub(super) fn intern_name(name: &str) -> &'static str {
    match name {
        "SO_KEEPALIVE" => "SO_KEEPALIVE",
        "SO_REUSEADDR" => "SO_REUSEADDR",
        "SO_BROADCAST" => "SO_BROADCAST",
        "SO_SNDBUF" => "SO_SNDBUF",
        "SO_RCVBUF" => "SO_RCVBUF",
        "SO_SNDLOWAT" => "SO_SNDLOWAT",
        "SO_RCVLOWAT" => "SO_RCVLOWAT",
        "SO_SNDTIMEO" => "SO_SNDTIMEO",
        "SO_RCVTIMEO" => "SO_RCVTIMEO",
        "TCP_NODELAY" => "TCP_NODELAY",
        "IPTOS_LOWDELAY" => "IPTOS_LOWDELAY",
        "IPTOS_THROUGHPUT" => "IPTOS_THROUGHPUT",
        other => {
            debug_assert!(false, "unexpected socket option '{other}'");
            ""
        }
    }
}
