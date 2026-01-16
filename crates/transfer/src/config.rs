#![deny(unsafe_code)]
//! Server configuration derived from the compact flag string and trailing arguments.

use std::ffi::OsString;
use std::path::PathBuf;

use compress::zlib::CompressionLevel;
use protocol::FilenameConverter;
use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::flags::ParsedServerFlags;
use super::role::ServerRole;

/// Identifies how a reference directory should be used when looking for basis files.
///
/// This mirrors the engine crate's `ReferenceDirectoryKind` for use in remote transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// `--compare-dest`: Skip creating destination when reference matches.
    Compare,
    /// `--copy-dest`: Copy from reference when it matches.
    Copy,
    /// `--link-dest`: Hard-link to reference when it matches.
    Link,
}

/// A reference directory used for basis file lookup during delta transfers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    /// The kind of reference directory operation.
    pub kind: ReferenceDirectoryKind,
    /// The path to the reference directory.
    pub path: PathBuf,
}

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
    /// Reference directories for basis file lookup (`--compare-dest`, `--copy-dest`, `--link-dest`).
    ///
    /// These directories are searched in order when looking for basis files during delta
    /// transfers. Each directory can be used for comparison, copying, or hard-linking
    /// depending on its kind.
    pub reference_directories: Vec<ReferenceDirectory>,
    /// Optional filename encoding converter for `--iconv` support.
    ///
    /// When set, filenames are converted between local and remote character encodings
    /// during file list transmission. This is used when the local and remote systems
    /// use different filename encodings.
    pub iconv: Option<FilenameConverter>,
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
            return Err("missing rsync server flag string".to_owned());
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
            reference_directories: Vec::new(),
            iconv: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn from_flag_string_and_args_with_valid_flags() {
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-vr".to_owned(),
            vec![OsString::from("/path/to/file")],
        );
        assert!(result.is_ok());
        let config = result.unwrap();
        assert_eq!(config.role, ServerRole::Generator);
        assert_eq!(config.flag_string, "-vr");
        assert_eq!(config.args.len(), 1);
    }

    #[test]
    fn from_flag_string_and_args_rejects_empty_without_args() {
        let result =
            ServerConfig::from_flag_string_and_args(ServerRole::Receiver, "".to_owned(), vec![]);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("missing rsync server flag string")
        );
    }

    #[test]
    fn from_flag_string_and_args_allows_empty_with_args() {
        // Daemon mode uses empty flag strings with module paths
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "".to_owned(),
            vec![OsString::from("module/path")],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn from_flag_string_and_args_allows_whitespace_only_with_args() {
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "   ".to_owned(),
            vec![OsString::from("path")],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn from_flag_string_and_args_sets_default_protocol() {
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        );
        let config = result.unwrap();
        assert_eq!(config.protocol, ProtocolVersion::NEWEST);
    }

    #[test]
    fn from_flag_string_and_args_sets_defaults_for_optional_fields() {
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        );
        let config = result.unwrap();
        assert!(config.compression_level.is_none());
        assert!(!config.client_mode);
        assert!(config.filter_rules.is_empty());
    }

    #[test]
    fn from_flag_string_and_args_with_multiple_args() {
        let result = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "-rv".to_owned(),
            vec![
                OsString::from("/path/one"),
                OsString::from("/path/two"),
                OsString::from("/path/three"),
            ],
        );
        let config = result.unwrap();
        assert_eq!(config.args.len(), 3);
    }

    #[test]
    fn server_config_clone() {
        let config = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-vr".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        let cloned = config.clone();
        assert_eq!(config, cloned);
    }

    #[test]
    fn server_config_debug_includes_struct_name() {
        let config = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        let debug = format!("{config:?}");
        assert!(debug.contains("ServerConfig"));
    }

    #[test]
    fn server_config_equality() {
        let config1 = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        let config2 = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        assert_eq!(config1, config2);
    }

    #[test]
    fn server_config_inequality_on_role() {
        let config1 = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        let config2 = ServerConfig::from_flag_string_and_args(
            ServerRole::Receiver,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        assert_ne!(config1, config2);
    }

    #[test]
    fn server_config_inequality_on_flags() {
        let config1 = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-r".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        let config2 = ServerConfig::from_flag_string_and_args(
            ServerRole::Generator,
            "-rv".to_owned(),
            vec![OsString::from("/path")],
        )
        .unwrap();
        assert_ne!(config1, config2);
    }
}
