#![deny(unsafe_code)]
//! Server configuration derived from the compact flag string and trailing arguments.

use std::ffi::OsString;
use std::time::SystemTime;

use compress::zlib::CompressionLevel;
use protocol::FilenameConverter;
use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::flags::ParsedServerFlags;
use super::role::ServerRole;

/// Reference directory types for remote transfers.
pub use engine::{ReferenceDirectory, ReferenceDirectoryKind};

/// File write behavior configuration.
///
/// Controls how the receiver writes transferred data to disk.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WriteConfig {
    /// Call fsync() after writing each file (`--fsync`).
    pub fsync: bool,
    /// Write directly to destination without temp-file + rename (`--inplace`).
    pub inplace: bool,
    /// Write data to device files instead of creating with mknod (`--write-devices`).
    pub write_devices: bool,
    /// Policy controlling io_uring usage for file I/O.
    pub io_uring_policy: fast_io::IoUringPolicy,
}

impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            fsync: false,
            inplace: false,
            write_devices: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
        }
    }
}

/// Deletion behavior configuration.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct DeletionConfig {
    /// Maximum number of deletions allowed (`--max-delete=NUM`).
    pub max_delete: Option<u64>,
    /// Delete files even if there are I/O errors (`--ignore-errors`).
    pub ignore_errors: bool,
}

/// Connection and protocol context configuration.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct ConnectionConfig {
    /// When true, indicates client-side operation (daemon client mode).
    pub client_mode: bool,
    /// Indicates the transfer is over a daemon (rsync://) connection.
    pub is_daemon_connection: bool,
    /// Filter rules to send to remote daemon (client_mode only).
    pub filter_rules: Vec<FilterRuleWireFormat>,
    /// Optional filename encoding converter for `--iconv` support.
    pub iconv: Option<FilenameConverter>,
    /// Optional compression level for zlib compression (0-9).
    pub compression_level: Option<CompressionLevel>,
}

/// File selection and filtering options.
///
/// Controls which files are candidates for transfer based on size,
/// existence at destination, and external file lists.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct FileSelectionConfig {
    /// Minimum file size in bytes. Files smaller than this are skipped.
    pub min_file_size: Option<u64>,
    /// Maximum file size in bytes. Files larger than this are skipped.
    pub max_file_size: Option<u64>,
    /// Skip updating files that already exist at the destination (`--ignore-existing`).
    pub ignore_existing: bool,
    /// Skip creating new files - only update existing files (`--existing`).
    pub existing_only: bool,
    /// Compare only file sizes, ignoring modification times (`--size-only`).
    pub size_only: bool,
    /// Path for `--files-from` when the server reads the file list directly.
    pub files_from_path: Option<String>,
    /// Use NUL bytes as delimiters for `--files-from` input (`--from0`).
    pub from0: bool,
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
    /// Connection and protocol context configuration.
    pub connection: ConnectionConfig,
    /// Reference directories for basis file lookup (`--compare-dest`, `--copy-dest`, `--link-dest`).
    pub reference_directories: Vec<ReferenceDirectory>,
    /// Deletion behavior configuration.
    pub deletion: DeletionConfig,
    /// File write behavior configuration.
    pub write: WriteConfig,
    /// Optional user-specified checksum seed from `--checksum-seed=NUM`.
    ///
    /// When `Some(seed)`, the server uses this fixed seed instead of generating
    /// a random one. This makes transfers reproducible (useful for testing/debugging).
    ///
    /// When `None`, the server generates a seed from current time XOR PID
    /// (matching upstream rsync's default behavior).
    ///
    /// A value of `0` means "use current time" in upstream rsync, which is
    /// equivalent to `None` (the default random seed generation).
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:835`: `--checksum-seed=NUM`
    /// - `compat.c:750`: `checksum_seed = (int32)time(NULL);` (default)
    pub checksum_seed: Option<u32>,
    /// Optional checksum algorithm override from `--checksum-choice`.
    ///
    /// When set, forces the negotiated checksum algorithm for the transfer
    /// protocol instead of using automatic negotiation. Propagated from
    /// the client configuration to ensure both sides agree on the algorithm.
    pub checksum_choice: Option<protocol::ChecksumAlgorithm>,
    /// Disables sender path safety checks when true (`--trust-sender`).
    ///
    /// When false (default), the receiver validates file list entries from the
    /// sender to prevent directory traversal attacks:
    /// - Rejects entries with absolute paths (when not using `--relative`)
    /// - Rejects entries containing `..` path components
    ///
    /// When true, these checks are skipped. This flag is purely receiver-side
    /// and does not affect the wire protocol.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:757`: `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
    /// - `options.c:797`: `--trust-sender` option definition
    /// - `options.c:2493`: trust_sender logic for args and filter
    pub trust_sender: bool,
    /// Optional wall-clock deadline for the transfer (`--stop-at` / `--stop-after`).
    ///
    /// When set, the transfer stops gracefully at the next file boundary after
    /// the deadline has passed. The current file finishes before stopping.
    /// This mirrors upstream rsync's `--stop-at` / `--stop-after` / `--time-limit`
    /// behavior.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c`: `stop_at_utime` global checked in transfer loop
    /// - `io.c`: deadline checked during I/O operations
    pub stop_at: Option<SystemTime>,
    /// Use unstable sort (qsort) instead of stable merge sort for file lists.
    ///
    /// When true, uses `sort_unstable_by` which corresponds to upstream rsync's
    /// `--qsort` flag that selects the C library `qsort()` instead of the default
    /// merge sort. The unstable sort may be faster but does not preserve relative
    /// order of equal elements.
    ///
    /// Default is false (stable merge sort).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2991`: `if (use_qsort) qsort(...); else merge_sort(...);`
    /// - `options.c`: `--qsort` flag definition
    pub qsort: bool,
    /// File selection and filtering configuration.
    pub file_selection: FileSelectionConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::NEWEST,
            flag_string: String::new(),
            flags: ParsedServerFlags::default(),
            args: Vec::new(),
            connection: ConnectionConfig::default(),
            reference_directories: Vec::new(),
            deletion: DeletionConfig::default(),
            write: WriteConfig::default(),
            checksum_seed: None,
            checksum_choice: None,
            trust_sender: false,
            stop_at: None,
            qsort: false,
            file_selection: FileSelectionConfig::default(),
        }
    }
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
            flag_string,
            flags,
            args,
            ..Self::default()
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
        assert!(config.connection.compression_level.is_none());
        assert!(!config.connection.client_mode);
        assert!(config.connection.filter_rules.is_empty());
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
