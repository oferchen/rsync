#![deny(unsafe_code)]
//! Server configuration derived from the compact flag string and trailing arguments.

use std::ffi::OsString;

use compress::zlib::CompressionLevel;
use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::flags::ParsedServerFlags;
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
    /// Parsed transfer options from the flag string.
    pub flags: ParsedServerFlags,
    /// Remaining positional arguments passed to the server.
    pub args: Vec<OsString>,
    /// Optional compression level for zlib compression (0-9).
    /// When None, defaults to level 6 (upstream default).
    /// Sourced from daemon configuration or environment.
    pub compression_level: Option<CompressionLevel>,
    /// When true, indicates client-side operation (daemon client mode).
    ///
    /// In client mode:
    /// - Filter list is SENT to the remote server, not read from it
    /// - The contexts skip reading filter list since the client already sent it
    ///
    /// This is used when connecting to a daemon as a client, where our code
    /// sends the filter list to the daemon (which reads it), and then runs
    /// server contexts locally that should not try to read filter list again.
    pub client_mode: bool,
    /// Filter rules to send to remote daemon (client_mode only).
    ///
    /// When `client_mode` is true, these rules are sent to the daemon after
    /// multiplex activation and before the transfer begins. The daemon uses
    /// these rules to filter file list generation.
    ///
    /// This is empty for normal server mode (where we receive filter list).
    pub filter_rules: Vec<FilterRuleWireFormat>,
}

impl ServerConfig {
    /// Builds a [`ServerConfig`] from the compact flag string and positional arguments.
    ///
    /// The parser accepts empty flag strings when positional arguments are provided,
    /// as daemon mode uses this pattern with module paths as arguments. Empty flag
    /// strings without arguments are rejected as obvious misuse.
    pub fn from_flag_string_and_args(
        role: ServerRole,
        flag_string: String,
        args: Vec<OsString>,
    ) -> Result<Self, String> {
        if flag_string.trim().is_empty() && args.is_empty() {
            return Err("missing rsync server flag string".to_string());
        }

        let flags = if flag_string.trim().is_empty() {
            ParsedServerFlags::default()
        } else {
            ParsedServerFlags::parse(&flag_string)
                .map_err(|e| format!("invalid flag string: {e}"))?
        };

        Ok(Self {
            role,
            protocol: ProtocolVersion::NEWEST,
            flag_string,
            flags,
            args,
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
        })
    }
}
