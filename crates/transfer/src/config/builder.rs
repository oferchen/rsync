//! Builder pattern for constructing [`ServerConfig`](super::ServerConfig) with validation.
//!
//! All setter methods take `&mut self` and return `&mut Self` for chaining.
//! The [`build`](ServerConfigBuilder::build) method validates invariants before
//! returning the final configuration.
//!
//! # Example
//!
//! ```rust
//! use transfer::config::ServerConfigBuilder;
//! use transfer::ServerRole;
//!
//! let config = ServerConfigBuilder::new()
//!     .role(ServerRole::Receiver)
//!     .flag_string("-logDtpre.")
//!     .build()
//!     .expect("valid config");
//! ```

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::SystemTime;

use compress::zlib::CompressionLevel;
use engine::SkipCompressList;
use protocol::FilenameConverter;
use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::error::BuilderError;
use super::{
    ConnectionConfig, DeletionConfig, FileSelectionConfig, ReferenceDirectory, ServerConfig,
    WriteConfig,
};
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

/// Builder for constructing [`ServerConfig`] with validation at build time.
///
/// Uses `&mut self` setters that return `&mut Self` for ergonomic chaining.
/// Call [`build`](Self::build) to validate invariants and produce the config.
///
/// # Validation
///
/// The [`build`](Self::build) method checks:
/// - `--inplace` and `--delay-updates` are mutually exclusive
/// - `--append` and `--partial-dir` are mutually exclusive
/// - `min_file_size` must not exceed `max_file_size`
///
/// # Example
///
/// ```rust
/// use transfer::config::ServerConfigBuilder;
/// use transfer::ServerRole;
///
/// let config = ServerConfigBuilder::new()
///     .role(ServerRole::Generator)
///     .flag_string("-rv")
///     .args(vec![std::ffi::OsString::from("/path")])
///     .build()
///     .expect("valid config");
/// ```
#[derive(Clone, Debug)]
pub struct ServerConfigBuilder {
    role: ServerRole,
    protocol: ProtocolVersion,
    flag_string: String,
    flags: ParsedServerFlags,
    args: Vec<OsString>,
    connection: ConnectionConfig,
    reference_directories: Vec<ReferenceDirectory>,
    deletion: DeletionConfig,
    write: WriteConfig,
    checksum_seed: Option<u32>,
    checksum_choice: Option<protocol::ChecksumAlgorithm>,
    trust_sender: bool,
    stop_at: Option<SystemTime>,
    qsort: bool,
    has_partial_dir: bool,
    backup_dir: Option<String>,
    backup_suffix: Option<String>,
    daemon_filter_rules: Vec<FilterRuleWireFormat>,
    file_selection: FileSelectionConfig,
    do_stats: bool,
    temp_dir: Option<PathBuf>,
    skip_compress: Option<SkipCompressList>,
    fake_super: bool,
}

impl Default for ServerConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerConfigBuilder {
    /// Creates a new builder with default values.
    #[must_use]
    pub fn new() -> Self {
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
            has_partial_dir: false,
            backup_dir: None,
            backup_suffix: None,
            daemon_filter_rules: Vec::new(),
            file_selection: FileSelectionConfig::default(),
            do_stats: false,
            temp_dir: None,
            skip_compress: None,
            fake_super: false,
        }
    }

    /// Sets the server-side role (receiver or generator).
    pub fn role(&mut self, role: ServerRole) -> &mut Self {
        self.role = role;
        self
    }

    /// Sets the requested protocol version.
    pub fn protocol(&mut self, protocol: ProtocolVersion) -> &mut Self {
        self.protocol = protocol;
        self
    }

    /// Sets the raw compact flag string.
    pub fn flag_string(&mut self, flag_string: &str) -> &mut Self {
        self.flag_string = flag_string.to_owned();
        self
    }

    /// Sets the parsed server flags.
    pub fn flags(&mut self, flags: ParsedServerFlags) -> &mut Self {
        self.flags = flags;
        self
    }

    /// Sets the positional arguments passed to the server.
    pub fn args(&mut self, args: Vec<OsString>) -> &mut Self {
        self.args = args;
        self
    }

    /// Sets the full connection configuration.
    pub fn connection(&mut self, connection: ConnectionConfig) -> &mut Self {
        self.connection = connection;
        self
    }

    /// Enables or disables client mode.
    pub fn client_mode(&mut self, enabled: bool) -> &mut Self {
        self.connection.client_mode = enabled;
        self
    }

    /// Marks the transfer as occurring over a daemon connection.
    pub fn is_daemon_connection(&mut self, enabled: bool) -> &mut Self {
        self.connection.is_daemon_connection = enabled;
        self
    }

    /// Sets filter rules to send to a remote daemon.
    pub fn filter_rules(&mut self, rules: Vec<FilterRuleWireFormat>) -> &mut Self {
        self.connection.filter_rules = rules;
        self
    }

    /// Sets the filename encoding converter for `--iconv` support.
    pub fn iconv(&mut self, converter: Option<FilenameConverter>) -> &mut Self {
        self.connection.iconv = converter;
        self
    }

    /// Sets the optional compression level for zlib compression.
    pub fn compression_level(&mut self, level: Option<CompressionLevel>) -> &mut Self {
        self.connection.compression_level = level;
        self
    }

    /// Sets the explicit compression algorithm from `--compress-choice`.
    pub fn compress_choice(&mut self, algo: Option<protocol::CompressionAlgorithm>) -> &mut Self {
        self.connection.compress_choice = algo;
        self
    }

    /// Sets pre-read `--files-from` data for forwarding to a remote daemon.
    pub fn files_from_data(&mut self, data: Option<Vec<u8>>) -> &mut Self {
        self.connection.files_from_data = data;
        self
    }

    /// Sets the full deletion configuration.
    pub fn deletion(&mut self, deletion: DeletionConfig) -> &mut Self {
        self.deletion = deletion;
        self
    }

    /// Sets the maximum number of deletions allowed.
    pub fn max_delete(&mut self, max: Option<u64>) -> &mut Self {
        self.deletion.max_delete = max;
        self
    }

    /// Enables or disables deletion despite I/O errors.
    pub fn ignore_errors(&mut self, enabled: bool) -> &mut Self {
        self.deletion.ignore_errors = enabled;
        self
    }

    /// Enables or disables late (deferred) deletion.
    pub fn late_delete(&mut self, enabled: bool) -> &mut Self {
        self.deletion.late_delete = enabled;
        self
    }

    /// Sets the full write configuration.
    pub fn write(&mut self, write: WriteConfig) -> &mut Self {
        self.write = write;
        self
    }

    /// Enables or disables fsync after writing each file.
    pub fn fsync(&mut self, enabled: bool) -> &mut Self {
        self.write.fsync = enabled;
        self
    }

    /// Enables or disables in-place writes (`--inplace`).
    pub fn inplace(&mut self, enabled: bool) -> &mut Self {
        self.write.inplace = enabled;
        self
    }

    /// Enables or disables per-file inplace for partial-dir basis files.
    pub fn inplace_partial(&mut self, enabled: bool) -> &mut Self {
        self.write.inplace_partial = enabled;
        self
    }

    /// Enables or disables writing data to device files.
    pub fn write_devices(&mut self, enabled: bool) -> &mut Self {
        self.write.write_devices = enabled;
        self
    }

    /// Enables or disables delayed file updates (`--delay-updates`).
    pub fn delay_updates(&mut self, enabled: bool) -> &mut Self {
        self.write.delay_updates = enabled;
        self
    }

    /// Sets the io_uring usage policy.
    pub fn io_uring_policy(&mut self, policy: fast_io::IoUringPolicy) -> &mut Self {
        self.write.io_uring_policy = policy;
        self
    }

    /// Sets the reference directories for basis file lookup.
    pub fn reference_directories(&mut self, dirs: Vec<ReferenceDirectory>) -> &mut Self {
        self.reference_directories = dirs;
        self
    }

    /// Sets an optional user-specified checksum seed.
    pub fn checksum_seed(&mut self, seed: Option<u32>) -> &mut Self {
        self.checksum_seed = seed;
        self
    }

    /// Sets an optional checksum algorithm override.
    pub fn checksum_choice(&mut self, choice: Option<protocol::ChecksumAlgorithm>) -> &mut Self {
        self.checksum_choice = choice;
        self
    }

    /// Enables or disables `--trust-sender`.
    pub fn trust_sender(&mut self, enabled: bool) -> &mut Self {
        self.trust_sender = enabled;
        self
    }

    /// Sets an optional wall-clock deadline for the transfer.
    pub fn stop_at(&mut self, deadline: Option<SystemTime>) -> &mut Self {
        self.stop_at = deadline;
        self
    }

    /// Enables or disables unstable sort for file lists (`--qsort`).
    pub fn qsort(&mut self, enabled: bool) -> &mut Self {
        self.qsort = enabled;
        self
    }

    /// Sets whether `--partial-dir` is configured.
    pub fn has_partial_dir(&mut self, enabled: bool) -> &mut Self {
        self.has_partial_dir = enabled;
        self
    }

    /// Sets the backup directory path (`--backup-dir`).
    pub fn backup_dir(&mut self, dir: Option<String>) -> &mut Self {
        self.backup_dir = dir;
        self
    }

    /// Sets the backup file suffix (`--backup-suffix`).
    pub fn backup_suffix(&mut self, suffix: Option<String>) -> &mut Self {
        self.backup_suffix = suffix;
        self
    }

    /// Sets daemon-side filter rules from module configuration.
    pub fn daemon_filter_rules(&mut self, rules: Vec<FilterRuleWireFormat>) -> &mut Self {
        self.daemon_filter_rules = rules;
        self
    }

    /// Sets the full file selection configuration.
    pub fn file_selection(&mut self, config: FileSelectionConfig) -> &mut Self {
        self.file_selection = config;
        self
    }

    /// Sets the minimum file size filter.
    pub fn min_file_size(&mut self, size: Option<u64>) -> &mut Self {
        self.file_selection.min_file_size = size;
        self
    }

    /// Sets the maximum file size filter.
    pub fn max_file_size(&mut self, size: Option<u64>) -> &mut Self {
        self.file_selection.max_file_size = size;
        self
    }

    /// Enables or disables skipping files that already exist at the destination.
    pub fn ignore_existing(&mut self, enabled: bool) -> &mut Self {
        self.file_selection.ignore_existing = enabled;
        self
    }

    /// Enables or disables only updating existing files.
    pub fn existing_only(&mut self, enabled: bool) -> &mut Self {
        self.file_selection.existing_only = enabled;
        self
    }

    /// Enables or disables size-only comparison.
    pub fn size_only(&mut self, enabled: bool) -> &mut Self {
        self.file_selection.size_only = enabled;
        self
    }

    /// Sets the `--files-from` path for direct server-side reading.
    pub fn files_from_path(&mut self, path: Option<String>) -> &mut Self {
        self.file_selection.files_from_path = path;
        self
    }

    /// Enables or disables NUL-delimited `--files-from` input.
    pub fn from0(&mut self, enabled: bool) -> &mut Self {
        self.file_selection.from0 = enabled;
        self
    }

    /// Enables or disables detailed transfer statistics.
    pub fn do_stats(&mut self, enabled: bool) -> &mut Self {
        self.do_stats = enabled;
        self
    }

    /// Sets the temporary directory for receiving files.
    pub fn temp_dir(&mut self, dir: Option<PathBuf>) -> &mut Self {
        self.temp_dir = dir;
        self
    }

    /// Sets the file suffixes that should skip per-file compression.
    pub fn skip_compress(&mut self, list: Option<SkipCompressList>) -> &mut Self {
        self.skip_compress = list;
        self
    }

    /// Enables or disables daemon-side fake-super metadata storage.
    ///
    /// Sourced from the daemon module's `fake super = yes` directive in
    /// `rsyncd.conf(5)`. When true, ownership and special-file metadata are
    /// stored in the `user.rsync.%stat` xattr instead of being applied
    /// directly to inodes, allowing a non-root daemon receiver to preserve
    /// privileged metadata.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:1106-1107` - daemon `fake super = yes` demotes
    ///   `am_root` and forces `--fake-super` semantics on the receiver
    /// - `loadparm.c` - `fake super` module parameter
    pub fn fake_super(&mut self, enabled: bool) -> &mut Self {
        self.fake_super = enabled;
        self
    }

    /// Validates the builder configuration.
    fn validate(&self) -> Result<(), BuilderError> {
        // upstream: options.c:2934 - --inplace and --delay-updates are mutually exclusive
        if self.write.inplace && self.write.delay_updates {
            return Err(BuilderError::ConflictingOptions {
                option1: "--inplace",
                option2: "--delay-updates",
            });
        }

        // upstream: options.c - --append implies --inplace, which conflicts with --partial-dir
        if self.flags.append && self.has_partial_dir {
            return Err(BuilderError::ConflictingOptions {
                option1: "--append",
                option2: "--partial-dir",
            });
        }

        if let (Some(min), Some(max)) = (
            self.file_selection.min_file_size,
            self.file_selection.max_file_size,
        ) {
            if min > max {
                return Err(BuilderError::InvalidCombination {
                    message: format!("min_file_size ({min}) cannot exceed max_file_size ({max})"),
                });
            }
        }

        Ok(())
    }

    /// Validates the configuration and builds the [`ServerConfig`].
    ///
    /// # Errors
    ///
    /// Returns a [`BuilderError`] if:
    /// - `--inplace` and `--delay-updates` are both enabled
    /// - `--append` and `--partial-dir` are both enabled
    /// - `min_file_size` exceeds `max_file_size`
    pub fn build(&self) -> Result<ServerConfig, BuilderError> {
        self.validate()?;
        Ok(self.build_unchecked())
    }

    /// Builds the [`ServerConfig`] without validation.
    ///
    /// Useful when the configuration is known to be valid or when
    /// validation should be skipped for performance.
    #[must_use]
    pub fn build_unchecked(&self) -> ServerConfig {
        ServerConfig {
            role: self.role,
            protocol: self.protocol,
            flag_string: self.flag_string.clone(),
            flags: self.flags.clone(),
            args: self.args.clone(),
            connection: self.connection.clone(),
            reference_directories: self.reference_directories.clone(),
            deletion: self.deletion.clone(),
            write: self.write.clone(),
            checksum_seed: self.checksum_seed,
            checksum_choice: self.checksum_choice,
            trust_sender: self.trust_sender,
            stop_at: self.stop_at,
            qsort: self.qsort,
            has_partial_dir: self.has_partial_dir,
            backup_dir: self.backup_dir.clone(),
            backup_suffix: self.backup_suffix.clone(),
            daemon_filter_rules: self.daemon_filter_rules.clone(),
            file_selection: self.file_selection.clone(),
            do_stats: self.do_stats,
            temp_dir: self.temp_dir.clone(),
            skip_compress: self.skip_compress.clone(),
            fake_super: self.fake_super,
        }
    }
}

impl ServerConfig {
    /// Creates a new [`ServerConfigBuilder`] for constructing a server configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use transfer::ServerConfig;
    /// use transfer::ServerRole;
    ///
    /// let config = ServerConfig::builder()
    ///     .role(ServerRole::Generator)
    ///     .flag_string("-rv")
    ///     .build()
    ///     .expect("valid config");
    /// ```
    #[must_use]
    pub fn builder() -> ServerConfigBuilder {
        ServerConfigBuilder::new()
    }
}
