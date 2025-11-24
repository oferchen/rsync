#![deny(unsafe_code)]

use std::ffi::OsString;

use protocol::ProtocolVersion;

use crate::server::role::ServerRole;

/// Structured configuration derived from a `--server` invocation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ServerConfig {
    /// Selected role for this invocation.
    pub role: ServerRole,
    /// Protocol version advertised by the server.
    pub protocol: ProtocolVersion,
    /// Raw flag string supplied by the client.
    pub flag_string: String,
    /// Trailing arguments forwarded by the client.
    pub args: Vec<OsString>,
}

impl ServerConfig {
    /// Builds a configuration from the raw flag string and trailing arguments supplied
    /// to the `--server` shim.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::role::ServerRole;

    #[test]
    fn rejects_empty_flag_string() {
        let error = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            String::new(),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(error, "missing rsync server flag string");
    }

    #[test]
    fn builds_config_with_defaults() {
        let args = vec![OsString::from("/tmp"), OsString::from("dest")];
        let config = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-logDtpre.iLsfxC".to_string(),
            args.clone(),
        )
        .expect("valid config");
        assert_eq!(config.role, ServerRole::Generator);
        assert_eq!(config.flag_string, "-logDtpre.iLsfxC");
        assert_eq!(config.args, args);
        assert_eq!(config.protocol, ProtocolVersion::NEWEST);
    }
}
