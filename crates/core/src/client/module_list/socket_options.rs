use std::ffi::OsStr;
use std::io;
use std::net::TcpStream;

use super::super::error::invalid_argument_error;
use crate::client::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, SOCKET_IO_EXIT_CODE};
use crate::message::Role;
use crate::rsync_error;

// RFC 1349 class selectors are consistent across Unix targets, but libc does
// not expose them uniformly (for example, Apple platforms omit the aliases), so
// we provide portable definitions when the `libc` crate does not.
#[cfg(not(target_family = "windows"))]
const IPTOS_LOWDELAY: libc::c_int = 0x10;

#[cfg(not(target_family = "windows"))]
const IPTOS_THROUGHPUT: libc::c_int = 0x08;

/// Platform-specific socket constants.
///
/// On Unix, forwards directly to `libc`.
/// On Windows, provides Winsock-compatible numeric values.
///
/// This is a small adapter/facade so the rest of the module stays platform-neutral.
mod socket_consts {
    #[cfg(not(target_family = "windows"))]
    pub const SOL_SOCKET: libc::c_int = libc::SOL_SOCKET;
    #[cfg(target_family = "windows")]
    pub const SOL_SOCKET: libc::c_int = 0xFFFF;

    #[cfg(not(target_family = "windows"))]
    pub const SO_KEEPALIVE: libc::c_int = libc::SO_KEEPALIVE;
    #[cfg(target_family = "windows")]
    pub const SO_KEEPALIVE: libc::c_int = 0x0008;

    #[cfg(not(target_family = "windows"))]
    pub const SO_REUSEADDR: libc::c_int = libc::SO_REUSEADDR;
    #[cfg(target_family = "windows")]
    pub const SO_REUSEADDR: libc::c_int = 0x0004;

    #[cfg(not(target_family = "windows"))]
    pub const SO_BROADCAST: libc::c_int = libc::SO_BROADCAST;
    #[cfg(target_family = "windows")]
    pub const SO_BROADCAST: libc::c_int = 0x0020;

    #[cfg(not(target_family = "windows"))]
    pub const SO_SNDBUF: libc::c_int = libc::SO_SNDBUF;
    #[cfg(target_family = "windows")]
    pub const SO_SNDBUF: libc::c_int = 0x1001;

    #[cfg(not(target_family = "windows"))]
    pub const SO_RCVBUF: libc::c_int = libc::SO_RCVBUF;
    #[cfg(target_family = "windows")]
    pub const SO_RCVBUF: libc::c_int = 0x1002;

    #[cfg(not(target_family = "windows"))]
    pub const SO_SNDTIMEO: libc::c_int = libc::SO_SNDTIMEO;
    #[cfg(target_family = "windows")]
    pub const SO_SNDTIMEO: libc::c_int = 0x1005;

    #[cfg(not(target_family = "windows"))]
    pub const SO_RCVTIMEO: libc::c_int = libc::SO_RCVTIMEO;
    #[cfg(target_family = "windows")]
    pub const SO_RCVTIMEO: libc::c_int = 0x1006;

    #[cfg(not(target_family = "windows"))]
    pub const IPPROTO_TCP: libc::c_int = libc::IPPROTO_TCP;
    #[cfg(target_family = "windows")]
    pub const IPPROTO_TCP: libc::c_int = 6;

    #[cfg(not(target_family = "windows"))]
    pub const TCP_NODELAY: libc::c_int = libc::TCP_NODELAY;
    #[cfg(target_family = "windows")]
    pub const TCP_NODELAY: libc::c_int = 0x0001;

    // setsockopt() returns -1 on error; on Winsock this is SOCKET_ERROR == -1.
    // Only needed on Windows; on Unix we just compare against -1 directly.
    #[cfg(windows)]
    pub const SOCKET_ERROR: libc::c_int = -1;
}

#[derive(Clone, Copy)]
enum SocketOptionKind {
    Bool {
        level: libc::c_int,
        option: libc::c_int,
    },
    Int {
        level: libc::c_int,
        option: libc::c_int,
    },
    On {
        level: libc::c_int,
        option: libc::c_int,
        value: libc::c_int,
    },
}

struct ParsedSocketOption {
    kind: SocketOptionKind,
    explicit_value: Option<libc::c_int>,
    name: &'static str,
}

impl ParsedSocketOption {
    /// Applies this parsed option to the provided stream.
    ///
    /// Small command-style helper to keep `apply_socket_options` focused on
    /// parsing/orchestration rather than low-level details.
    fn apply(&self, stream: &TcpStream) -> io::Result<()> {
        match self.kind {
            SocketOptionKind::Bool { level, option }
            | SocketOptionKind::Int { level, option } => {
                let value = self.explicit_value.unwrap_or(libc::c_int::from(1));
                set_socket_option_int(stream, level, option, value)
            }
            SocketOptionKind::On {
                level,
                option,
                value,
            } => set_socket_option_int(stream, level, option, value),
        }
    }
}

/// Applies the caller-provided socket options to the supplied TCP stream.
pub(crate) fn apply_socket_options(stream: &TcpStream, options: &OsStr) -> Result<(), ClientError> {
    let list = options.to_string_lossy();

    if list.trim().is_empty() {
        return Ok(());
    }

    let mut parsed = Vec::new();

    for token in list
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .filter(|token| !token.is_empty())
    {
        let (name, value) = match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        };

        let descriptor = lookup_socket_option(name).ok_or_else(|| unknown_option(name))?;

        match descriptor {
            SocketOptionKind::On { .. } if value.is_some() => {
                return Err(option_disallows_value(name));
            }
            SocketOptionKind::On { .. } => {
                parsed.push(ParsedSocketOption {
                    kind: descriptor,
                    explicit_value: None,
                    name: intern_name(name),
                });
            }
            SocketOptionKind::Bool { .. } | SocketOptionKind::Int { .. } => {
                let parsed_value = value
                    .map(parse_socket_option_value)
                    .unwrap_or(libc::c_int::from(1));
                parsed.push(ParsedSocketOption {
                    kind: descriptor,
                    explicit_value: Some(parsed_value),
                    name: intern_name(name),
                });
            }
        }
    }

    for option in parsed {
        option
            .apply(stream)
            .map_err(|error| socket_option_error(option.name, error))?;
    }

    Ok(())
}

fn lookup_socket_option(name: &str) -> Option<SocketOptionKind> {
    match name {
        "SO_KEEPALIVE" => Some(SocketOptionKind::Bool {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_KEEPALIVE,
        }),
        "SO_REUSEADDR" => Some(SocketOptionKind::Bool {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_REUSEADDR,
        }),
        #[cfg(any(target_family = "unix", target_os = "windows"))]
        "SO_BROADCAST" => Some(SocketOptionKind::Bool {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_BROADCAST,
        }),
        "SO_SNDBUF" => Some(SocketOptionKind::Int {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_SNDBUF,
        }),
        "SO_RCVBUF" => Some(SocketOptionKind::Int {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_RCVBUF,
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
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_SNDTIMEO,
        }),
        "SO_RCVTIMEO" => Some(SocketOptionKind::Int {
            level: socket_consts::SOL_SOCKET,
            option: socket_consts::SO_RCVTIMEO,
        }),
        "TCP_NODELAY" => Some(SocketOptionKind::Bool {
            level: socket_consts::IPPROTO_TCP,
            option: socket_consts::TCP_NODELAY,
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

fn parse_socket_option_value(raw: &str) -> libc::c_int {
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

fn socket_option_error(name: &str, error: io::Error) -> ClientError {
    let rendered = format!("failed to set socket option {name}: {error}");
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, rendered).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn unknown_option(name: &str) -> ClientError {
    invalid_argument_error(
        &format!("Unknown socket option {name}"),
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn option_disallows_value(name: &str) -> ClientError {
    invalid_argument_error(
        &format!("syntax error -- {name} does not take a value"),
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn intern_name(name: &str) -> &'static str {
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

#[cfg(unix)]
#[allow(unsafe_code)]
fn set_socket_option_int(
    stream: &TcpStream,
    level: libc::c_int,
    option: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let raw = stream.as_raw_fd();
    let ret = unsafe {
        libc::setsockopt(
            raw,
            level,
            option,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };

    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn set_socket_option_int(
    stream: &TcpStream,
    level: libc::c_int,
    option: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    let raw = stream.as_raw_socket();
    let ret = unsafe {
        libc::setsockopt(
            raw as libc::SOCKET,
            level,
            option,
            &value as *const libc::c_int as *const libc::c_char,
            std::mem::size_of::<libc::c_int>() as libc::c_int,
        )
    };

    if ret == socket_consts::SOCKET_ERROR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
