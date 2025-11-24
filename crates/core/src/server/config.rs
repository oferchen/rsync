#![deny(unsafe_code)]
//! Server configuration derived from the compact flag string and trailing arguments.

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::role::ServerRole;

/// Configuration supplied to the server entry point.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ServerConfig {
    /// Server-side role negotiated via `--server` / `--sender`.
    pub role: ServerRole,
    /// Requested protocol version; capped during handshake.
    pub protocol: ProtocolVersion,
    /// Raw compact flag string provided by the client.
    pub flag_string: String,
    /// Remaining positional arguments passed to the server.
    pub args: Vec<OsString>,
}

impl ServerConfig {
    /// Builds a [`ServerConfig`] from the compact flag string and positional arguments.
    ///
    /// The parser mirrors upstream rsync expectations by rejecting empty flag strings
    /// so obvious misuse surfaces before any protocol negotiation occurs.
    pub fn from_flag_string_and_args(
        role: ServerRole,
        flag_string: String,
        args: Vec<OsString>,
    ) -> Result<Self, String> {
        if flag_string.trim().is_empty() {
            return Err("missing rsync server flag string".to_string());
        }

        Ok(Self {
            role,
            protocol: ProtocolVersion::NEWEST,
            flag_string,
            args,
        })
    }
}
