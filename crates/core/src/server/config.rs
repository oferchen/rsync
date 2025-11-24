#![deny(unsafe_code)]

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::role::ServerRole;

/// Configuration derived from the compact server flag string and arguments.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server role requested by the client.
    pub role: ServerRole,
    /// Protocol version to advertise during negotiation.
    pub protocol: ProtocolVersion,
    /// Compact flag string provided by the client.
    pub flag_string: String,
    /// Positional arguments supplied after the flag string.
    pub args: Vec<OsString>,
}

impl ServerConfig {
    /// Builds a [`ServerConfig`] after validating the compact flag string.
    ///
    /// The function mirrors upstream parsing by ensuring the flag string is
    /// present while deferring detailed flag decoding to later stages.
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
}
