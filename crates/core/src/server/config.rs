#![deny(unsafe_code)]

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::ServerRole;

/// Configuration derived from a parsed `--server` invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerConfig {
    pub(super) role: ServerRole,
    pub(super) protocol: ProtocolVersion,
    pub(super) flag_string: String,
    pub(super) args: Vec<OsString>,
}

impl ServerConfig {
    /// Builds a [`ServerConfig`] after performing basic validation of the flag string.
    pub fn from_flag_string_and_args(
        role: ServerRole,
        flag_string: String,
        args: Vec<OsString>,
    ) -> Result<Self, String> {
        if flag_string.is_empty() {
            return Err("missing rsync server flag string".to_string());
        }

        Ok(Self {
            role,
            protocol: ProtocolVersion::NEWEST,
            flag_string,
            args,
        })
    }

    #[must_use]
    /// Returns the server role negotiated for this invocation.
    pub const fn role(&self) -> ServerRole {
        self.role
    }

    #[must_use]
    /// Returns the protocol version requested by the client.
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }
}
