//! Socket option type definitions.

use std::io;

/// Classifies a socket option by how its value is interpreted.
#[derive(Clone, Copy)]
pub(super) enum SocketOptionKind {
    /// Boolean toggle (0 or 1), e.g. `SO_KEEPALIVE`.
    Bool {
        level: libc::c_int,
        option: libc::c_int,
    },
    /// Integer value, e.g. `SO_SNDBUF=65536`.
    Int {
        level: libc::c_int,
        option: libc::c_int,
    },
    /// Fixed-value option (Unix only), e.g. `IPTOS_LOWDELAY`.
    #[cfg(not(target_family = "windows"))]
    On {
        level: libc::c_int,
        option: libc::c_int,
        value: libc::c_int,
    },
}

/// A single parsed socket option ready to be applied.
///
/// Decouples parsing from execution: `apply` performs the actual
/// `setsockopt` call via the safe `fast_io::set_socket_int_option_raw`
/// wrapper. It runs on the pre-connect `socket2::Socket` (see
/// `connect::connect_with_optional_bind`) rather than a connected
/// `TcpStream` - upstream: socket.c:279 `set_socket_options(s, sockopts)`
/// runs before `connect(s, ...)` at socket.c:280 so options that affect the
/// SYN (e.g. `SO_SNDBUF`/`SO_RCVBUF` window scaling) take effect.
pub(super) struct ParsedSocketOption {
    pub(super) kind: SocketOptionKind,
    pub(super) explicit_value: Option<libc::c_int>,
    pub(super) name: &'static str,
}

impl ParsedSocketOption {
    /// Returns the option name for error reporting.
    pub(super) const fn name(&self) -> &'static str {
        self.name
    }

    /// Applies this option to the provided socket (Unix).
    #[cfg(not(target_family = "windows"))]
    pub(super) fn apply(&self, socket: &socket2::Socket) -> io::Result<()> {
        use std::os::fd::AsRawFd;

        let fd = socket.as_raw_fd();
        match self.kind {
            SocketOptionKind::Bool { level, option } | SocketOptionKind::Int { level, option } => {
                let value = self.explicit_value.unwrap_or(1);
                fast_io::set_socket_int_option_raw(fd, level, option, value)
            }
            SocketOptionKind::On {
                level,
                option,
                value,
            } => fast_io::set_socket_int_option_raw(fd, level, option, value),
        }
    }

    /// Applies this option to the provided socket (Windows).
    ///
    /// IPTOS_* options are not available on Windows, so only `Bool`/`Int`
    /// variants are reachable.
    #[cfg(target_family = "windows")]
    pub(super) fn apply(&self, socket: &socket2::Socket) -> io::Result<()> {
        use std::os::windows::io::AsRawSocket;

        let raw = socket.as_raw_socket();
        match self.kind {
            SocketOptionKind::Bool { level, option } | SocketOptionKind::Int { level, option } => {
                let value = self.explicit_value.unwrap_or(1);
                fast_io::set_socket_int_option_raw(raw, level, option, value)
            }
        }
    }
}
