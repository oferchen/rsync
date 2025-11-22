#![deny(unsafe_code)]

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::ServerRole;

/// Aggregates the parsed server invocation into a structured configuration.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ServerConfig {
    /// The negotiated role for this server invocation.
    pub role: ServerRole,
    /// Desired protocol version prior to negotiation.
    pub protocol: ProtocolVersion,
    /// Raw flag string supplied by the client.
    pub flag_string: String,
    /// Positional arguments following the flag string and optional placeholder.
    pub args: Vec<OsString>,
}

impl ServerConfig {
    /// Builds a [`ServerConfig`] from the compact flag string and positional
    /// arguments supplied to the `--server` entry point.
    ///
    /// The caller is responsible for performing any additional decoding of the
    /// compact flag string into structured options.
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

    #[test]
    fn rejects_empty_flag_string() {
        let err = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "   ".to_string(),
            vec![],
        )
        .expect_err("empty flag string should be rejected");

        assert_eq!(err, "missing rsync server flag string");
    }

    #[test]
    fn preserves_inputs() {
        let args = vec![OsString::from("src"), OsString::from("dest")];
        let cfg = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-logDtpre.iLsfxC".to_string(),
            args.clone(),
        )
        .expect("valid config");

        assert_eq!(cfg.role, ServerRole::Generator);
        assert_eq!(cfg.protocol, ProtocolVersion::NEWEST);
        assert_eq!(cfg.flag_string, "-logDtpre.iLsfxC");
        assert_eq!(cfg.args, args);
    }
}
