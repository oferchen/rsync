//! Server configuration derived from the compact flag string and trailing arguments.

mod builder;
mod error;

pub use builder::ServerConfigBuilder;
pub use error::BuilderError;

use std::ffi::OsString;
use std::time::SystemTime;

use compress::zlib::CompressionLevel;
use engine::SkipCompressList;
use metadata::ChmodModifiers;
use protocol::FilenameConverter;
use protocol::ProtocolVersion;
use protocol::filters::FilterRuleWireFormat;

use super::flags::ParsedServerFlags;
use super::role::ServerRole;

/// Reference directory types for remote transfers.
pub use engine::{ReferenceDirectory, ReferenceDirectoryKind};

/// File write behavior configuration for the receiver.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WriteConfig {
    /// Call fsync() after writing each file (`--fsync`).
    pub fsync: bool,
    /// Write directly to destination without temp-file + rename (`--inplace`).
    pub inplace: bool,
    /// Per-file inplace for partial-dir basis files (CF_INPLACE_PARTIAL_DIR).
    ///
    /// When true, files whose basis comes from `--partial-dir` are written
    /// in-place (directly to the partial file) instead of using temp+rename.
    /// Other files still use the safe temp+rename path.
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c:777-778`: `if (compat_flags & CF_INPLACE_PARTIAL_DIR) inplace_partial = 1;`
    /// - `receiver.c:797`: `one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR;`
    pub inplace_partial: bool,
    /// Write data to device files instead of creating with mknod (`--write-devices`).
    pub write_devices: bool,
    /// Delay file updates until the end of the transfer (`--delay-updates`).
    ///
    /// When enabled, files are first written to a temporary holding directory
    /// and then moved into place at the end of the transfer. This makes the
    /// update more atomic from the perspective of other processes reading the
    /// destination tree.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2934-2935`: `--delay-updates` option handling
    /// - `receiver.c`: deferred rename sweep at end of transfer
    pub delay_updates: bool,
    /// Enable AIMD adaptive concurrency for the concurrent delta pipeline.
    ///
    /// When true, the work queue uses an additive-increase / multiplicative-
    /// decrease limiter to adapt in-flight concurrency to disk and I/O
    /// pressure. Disabled by default (opt-in via `--adaptive-concurrency`).
    pub adaptive_concurrency: bool,
    /// Policy controlling io_uring usage for file I/O.
    pub io_uring_policy: fast_io::IoUringPolicy,
    /// Optional override for the io_uring submission queue depth (`--io-uring-depth=N`).
    ///
    /// `None` keeps the upstream default
    /// ([`fast_io::IoUringConfig::sq_entries`], 64). `Some(n)` overrides the
    /// default with a power-of-two value previously validated via
    /// [`fast_io::validate_io_uring_depth`].
    pub io_uring_depth: Option<u32>,
    /// Policy controlling I/O-level zero-copy syscalls (`sendfile`, `splice`,
    /// `copy_file_range`, io_uring `SEND_ZC`).
    ///
    /// Orthogonal to filesystem-level reflink cloning. When set to
    /// [`fast_io::ZeroCopyPolicy::Disabled`], the receiver routes through
    /// portable `read`/`write` loops and the sender skips `sendfile(2)`.
    pub zero_copy_policy: fast_io::ZeroCopyPolicy,
    /// Open source files with `O_NOATIME` (`--open-noatime`).
    ///
    /// Linux/Android only; ignored on other targets where `O_NOATIME` is
    /// not defined. Mirrors upstream `do_open` / `do_open_nofollow`
    /// (rsync 3.4.2 propagates `O_NOATIME` through both paths).
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:228` - `do_open()` ORs `O_NOATIME` into flags.
    /// - `syscall.c:687` - `do_open_nofollow()` (added in 3.4.2).
    pub open_noatime: bool,
}

impl Default for WriteConfig {
    fn default() -> Self {
        Self {
            fsync: false,
            inplace: false,
            inplace_partial: false,
            write_devices: false,
            delay_updates: false,
            adaptive_concurrency: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            io_uring_depth: None,
            zero_copy_policy: fast_io::ZeroCopyPolicy::Auto,
            open_noatime: false,
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
    /// Whether deletions are deferred until after the transfer completes.
    ///
    /// True when the delete mode is `--delete-delay` or `--delete-after`.
    /// Controls timing of NDX_DEL_STATS in the goodbye phase.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:124`: `#define EARLY_DELETE_DONE_MSG() (!(delete_during == 2 || delete_after))`
    pub late_delete: bool,
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
    /// Explicit compression algorithm from `--compress-choice`.
    ///
    /// When set, bypasses vstring compression negotiation and uses this
    /// algorithm directly. Both sides must agree (the client sends
    /// `--compress-choice=ALGO` to the server, which sets the same field).
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c:543`: compression vstrings skipped when compress_choice is set
    /// - `options.c:2800-2805`: `--compress-choice=ALGO` sent as long-form arg
    pub compress_choice: Option<protocol::CompressionAlgorithm>,
    /// Worker thread count for zstd's `ZSTD_c_nbWorkers` (`--compress-threads=N`).
    ///
    /// `None` keeps zstd single-threaded, matching upstream's
    /// `do_compression_threads = 0` default. Only meaningful when zstd is the
    /// negotiated compression algorithm.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:89`: `do_compression_threads` global
    /// - `token.c:701`: `ZSTD_CCtx_setParameter(.., ZSTD_c_nbWorkers, ..)`
    pub compression_threads: Option<std::num::NonZeroU8>,
    /// Pre-read `--files-from` data for forwarding to a remote daemon.
    ///
    /// When the client has `--files-from` pointing to stdin or a local file,
    /// the data is read upfront and stored here. During a pull transfer the
    /// client forwards this data to the daemon's generator over the protocol
    /// stream so it can build its file list from the forwarded filenames.
    ///
    /// The data is in the wire format produced by
    /// [`protocol::forward_files_from`]: NUL-separated filenames terminated
    /// by a double-NUL sentinel.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:forward_filesfrom_data()` - forwards local file to socket
    /// - `main.c:1354-1356` - `start_filesfrom_forwarding(filesfrom_fd)`
    pub files_from_data: Option<Vec<u8>>,
}

/// File selection and filtering options for transfer candidates.
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
    /// Silently skip source entries that do not exist (`--ignore-missing-args`).
    ///
    /// When true and a top-level source path (or `--files-from` entry) returns
    /// ENOENT during file list building, the entry is silently omitted from the
    /// file list with no error or warning.
    ///
    /// Mutually exclusive with `delete_missing_args` - if both are set,
    /// `delete_missing_args` takes precedence (upstream: `missing_args == 2`).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_file_list()` - `missing_args == 1`
    /// - `options.c:817` - `--ignore-missing-args`
    pub ignore_missing_args: bool,
    /// Delete destination entries whose source has vanished (`--delete-missing-args`).
    ///
    /// When true and a top-level source path (or `--files-from` entry) returns
    /// ENOENT during file list building, a mode-0 sentinel entry is emitted into
    /// the file list. The receiver interprets this sentinel as an instruction to
    /// delete the corresponding destination path.
    ///
    /// Takes precedence over `ignore_missing_args` when both are set.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_file_list()` - `missing_args == 2`
    /// - `options.c:818` - `--delete-missing-args`
    pub delete_missing_args: bool,
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
    /// Whether `--partial-dir` is configured on the client.
    ///
    /// Used after compat flag negotiation to apply `CF_INPLACE_PARTIAL_DIR`:
    /// when the server advertises this flag and a partial directory is configured,
    /// the receiver uses in-place writes for partial-dir basis files.
    ///
    /// # Upstream Reference
    ///
    /// - `compat.c:777-778`: `if (compat_flags & CF_INPLACE_PARTIAL_DIR) inplace_partial = 1;`
    /// - `receiver.c:797`: `one_inplace = inplace_partial && fnamecmp_type == FNAMECMP_PARTIAL_DIR;`
    pub has_partial_dir: bool,
    /// Directory path for storing partial files on interrupt (`--partial-dir=DIR`).
    ///
    /// When set, interrupted transfers move the incomplete temp file into this
    /// directory instead of deleting it. On subsequent transfers, the receiver
    /// checks this directory for a basis file to resume from.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:partial_dir` - `--partial-dir=DIR` option
    /// - `cleanup.c:handle_partial_dir()` - moves temp to partial-dir on interrupt
    pub partial_dir: Option<std::path::PathBuf>,
    /// Backup directory path (long-form `--backup-dir=DIR`).
    ///
    /// When set with `--backup`, displaced files are placed in this directory
    /// hierarchy instead of alongside the destination files.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2854-2870`: `--backup-dir=DIR` server option
    pub backup_dir: Option<String>,
    /// Backup file suffix (long-form `--backup-suffix=SUFFIX`).
    ///
    /// Overrides the default `~` suffix for backup files. When `--backup-dir`
    /// is set, the default suffix is empty.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2871-2876`: `--backup-suffix=SUFFIX` server option
    pub backup_suffix: Option<String>,
    /// Daemon-side filter rules from module configuration.
    ///
    /// These rules are enforced by the daemon regardless of what the client sends.
    /// Built from the module's `filter`, `exclude`, `include`, `exclude_from`,
    /// and `include_from` parameters in `rsyncd.conf`.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:rsync_module()` - builds `daemon_filter_list` from
    ///   `lp_filter()`, `lp_include()`, `lp_exclude()`, `lp_include_from()`,
    ///   `lp_exclude_from()` before the transfer starts.
    pub daemon_filter_rules: Vec<FilterRuleWireFormat>,
    /// File selection and filtering configuration.
    pub file_selection: FileSelectionConfig,
    /// Whether `--stats` was requested, enabling detailed transfer statistics.
    ///
    /// Maps to upstream's `INFO_GTE(STATS, 2)` condition. When true, deletion
    /// statistics (NDX_DEL_STATS) are sent during the goodbye phase.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2046-2048`: `do_stats` sets `info_levels[INFO_STATS]` to 2+
    /// - `generator.c:2377,2422`: `INFO_GTE(STATS, 2)` gates `write_del_stats()`
    pub do_stats: bool,
    /// Temporary directory for receiving files before final placement.
    ///
    /// When set, temporary files are created in this directory instead of alongside
    /// the destination file. After successful transfer, the temp file is renamed
    /// to its final location. This is useful when the destination is on a slow or
    /// network-mounted filesystem.
    ///
    /// Sources: client `--temp-dir` argument, daemon `temp dir` module parameter.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2907-2909`: `--temp-dir` server option
    /// - `receiver.c:766`: `open_tmpfile()` uses `tmpdir` when set
    /// - `loadparm.c`: `temp dir` daemon module parameter
    pub temp_dir: Option<std::path::PathBuf>,
    /// File suffixes that should skip per-file compression.
    ///
    /// When compression is enabled, files whose extension matches a suffix in
    /// this list are sent uncompressed. This is populated from the daemon's
    /// `dont compress` module parameter or the client's `--skip-compress` option.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:do_compression` - per-file compression decision
    /// - `loadparm.c` - `dont compress` daemon parameter
    pub skip_compress: Option<SkipCompressList>,
    /// When true, store privileged metadata (uid/gid, devices, special files) in
    /// the `user.rsync.%stat` xattr instead of applying it directly to inodes.
    ///
    /// Sourced from the daemon module's `fake super = yes` directive in
    /// `rsyncd.conf(5)`. Lets a non-root daemon receive ownership and special-file
    /// metadata from a privileged client by recording it in xattrs that a later
    /// privileged restore can replay.
    ///
    /// Mirrors upstream's behaviour where a daemon module with `fake super = yes`
    /// demotes the receiver's `am_root` to the fake-super marker (`-1`) and
    /// rewrites a client's `--fake-super` so the daemon honours the directive
    /// even when the client did not request it. The directive is purely
    /// daemon-config-driven; this flag is set by the daemon when constructing
    /// [`ServerConfig`] and is never populated from a client `--fake-super` arg.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:1106-1107` - daemon `fake super = yes` demotes
    ///   `am_root` and forces `--fake-super` semantics on the receiver
    /// - `loadparm.c` - `fake super` module parameter
    /// - `rsync.c:set_file_attrs()` - fake-super stores ownership in xattrs
    pub fake_super: bool,
    /// Daemon module `incoming chmod` modifiers applied to received files.
    ///
    /// Parsed from the module's `incoming chmod = SPEC` directive in
    /// `rsyncd.conf`. When set, the receiver rewrites the destination mode
    /// according to the chmod-spec clauses before finalising on-disk
    /// permissions. Push transfers (client to daemon) consult this value;
    /// pull transfers ignore it.
    ///
    /// Daemon-config-driven; never populated from a client `--chmod` flag.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:rsync_module()` - `parse_chmod(lp_xxx_chmod(i), &daemon_chmod_modes)`
    /// - `loadparm.c` - `incoming chmod` module parameter
    /// - `generator.c` / `receiver.c` - `daemon_chmod_modes` applied to incoming entries
    pub daemon_incoming_chmod: Option<ChmodModifiers>,
    /// Daemon module `outgoing chmod` modifiers applied to sent files.
    ///
    /// Parsed from the module's `outgoing chmod = SPEC` directive in
    /// `rsyncd.conf`. When set, the sender rewrites the mode emitted on the
    /// wire for each file list entry according to the chmod-spec clauses.
    /// Pull transfers (daemon to client) consult this value; push transfers
    /// ignore it.
    ///
    /// Daemon-config-driven; never populated from a client `--chmod` flag.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:rsync_module()` - `parse_chmod(lp_xxx_chmod(i), &daemon_chmod_modes)`
    /// - `loadparm.c` - `outgoing chmod` module parameter
    /// - `flist.c:make_file()` - `daemon_chmod_modes` applied during flist build
    pub daemon_outgoing_chmod: Option<ChmodModifiers>,
    /// When true, munge symlink targets with the `/rsyncd-munged/` prefix.
    ///
    /// Sourced from the daemon module's `munge symlinks` directive (or its
    /// `!use_chroot` auto default). Mirrors upstream's `munge_symlinks`
    /// global. Propagated into [`ParsedServerFlags::munge_symlinks`] when the
    /// server config is consumed so the sender (strip on `readlink()`) and
    /// receiver (prepend on `symlink()` write) apply the bidirectional
    /// transform.
    ///
    /// # Upstream Reference
    ///
    /// - `clientserver.c:992-1004` - daemon sets `munge_symlinks` from
    ///   `lp_munge_symlinks()`.
    /// - `flist.c:222-226` - sender strips the prefix.
    /// - `flist.c:1122-1126` - receiver prepends the prefix.
    pub munge_symlinks: bool,
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
            has_partial_dir: false,
            partial_dir: None,
            backup_dir: None,
            backup_suffix: None,
            daemon_filter_rules: Vec::new(),
            file_selection: FileSelectionConfig::default(),
            do_stats: false,
            temp_dir: None,
            skip_compress: None,
            fake_super: false,
            daemon_incoming_chmod: None,
            daemon_outgoing_chmod: None,
            munge_symlinks: false,
        }
    }
}

impl ServerConfig {
    /// Returns the effective backup suffix, matching upstream rsync defaults.
    ///
    /// When `backup_suffix` is explicitly set, returns that value. Otherwise,
    /// returns `""` if `backup_dir` is set, or `"~"` as the default.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2278-2279`: `backup_suffix = backup_dir ? "" : BACKUP_SUFFIX`
    pub fn effective_backup_suffix(&self) -> &str {
        match self.backup_suffix.as_deref() {
            Some(s) => s,
            None => {
                if self.backup_dir.is_some() {
                    ""
                } else {
                    "~"
                }
            }
        }
    }

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
mod builder_tests;

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

    #[test]
    fn effective_backup_suffix_defaults_to_tilde() {
        let config = ServerConfig::default();
        assert_eq!(config.effective_backup_suffix(), "~");
    }

    #[test]
    fn effective_backup_suffix_empty_when_backup_dir_set() {
        // upstream: options.c:2278-2279 - backup_suffix = backup_dir ? "" : BACKUP_SUFFIX
        let config = ServerConfig {
            backup_dir: Some(".backups".to_owned()),
            ..Default::default()
        };
        assert_eq!(config.effective_backup_suffix(), "");
    }

    #[test]
    fn effective_backup_suffix_uses_explicit_suffix() {
        let config = ServerConfig {
            backup_suffix: Some(".bak".to_owned()),
            ..Default::default()
        };
        assert_eq!(config.effective_backup_suffix(), ".bak");
    }

    #[test]
    fn effective_backup_suffix_explicit_overrides_backup_dir_default() {
        // When both --backup-dir and --suffix are set, --suffix wins
        let config = ServerConfig {
            backup_dir: Some(".backups".to_owned()),
            backup_suffix: Some(".old".to_owned()),
            ..Default::default()
        };
        assert_eq!(config.effective_backup_suffix(), ".old");
    }

    #[test]
    fn effective_backup_suffix_explicit_empty_with_no_backup_dir() {
        let config = ServerConfig {
            backup_suffix: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(config.effective_backup_suffix(), "");
    }
}
