use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;

/// Describes a bind address specified via `--address`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindAddress {
    raw: OsString,
    socket: SocketAddr,
}

impl BindAddress {
    /// Creates a new bind address from the caller-provided specification.
    #[must_use]
    pub const fn new(raw: OsString, socket: SocketAddr) -> Self {
        Self { raw, socket }
    }

    /// Returns the raw command-line representation forwarded to the fallback binary.
    #[must_use]
    pub fn raw(&self) -> &OsStr {
        self.raw.as_os_str()
    }

    /// Returns the socket address (with port zero) used when binding local connections.
    #[must_use]
    pub const fn socket(&self) -> SocketAddr {
        self.socket
    }
}
