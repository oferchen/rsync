//! Socket option type definitions.

use std::io;
use std::net::TcpStream;

use fast_io::set_socket_int_option;

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
/// `setsockopt` call via the safe `fast_io::set_socket_int_option` wrapper.
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

    /// Applies this option to the provided stream (Unix).
    #[cfg(not(target_family = "windows"))]
    pub(super) fn apply(&self, stream: &TcpStream) -> io::Result<()> {
        match self.kind {
            SocketOptionKind::Bool { level, option } | SocketOptionKind::Int { level, option } => {
                let value = self.explicit_value.unwrap_or(1);
                set_socket_int_option(stream, level, option, value)
            }
            SocketOptionKind::On {
                level,
                option,
                value,
            } => set_socket_int_option(stream, level, option, value),
        }
    }

    /// Applies this option to the provided stream (Windows).
    ///
    /// IPTOS_* options are not available on Windows, so only `Bool`/`Int`
    /// variants are reachable.
    #[cfg(target_family = "windows")]
    pub(super) fn apply(&self, stream: &TcpStream) -> io::Result<()> {
        match self.kind {
            SocketOptionKind::Bool { level, option } | SocketOptionKind::Int { level, option } => {
                let value = self.explicit_value.unwrap_or(1);
                set_socket_int_option(stream, level, option, value)
            }
        }
    }
}
