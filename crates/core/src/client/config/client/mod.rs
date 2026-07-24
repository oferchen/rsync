use std::ffi::{OsStr, OsString};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ::metadata::{ChmodModifiers, GroupMapping, ModifyWindow, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use engine::SkipCompressList;

use super::builder::ClientConfigBuilder;
use super::{
    AddressMode, BandwidthLimit, BindAddress, CompressionSetting, DeleteMode, FilesFromSource,
    FilterRuleSpec, IconvSetting, ReferenceDirectory, StrongChecksumChoice, TcpFastOpenMode,
    TransferTimeout,
};

/// Configuration describing the requested client operation.
///
/// This structure encapsulates all transfer settings supplied by the caller,
/// from source and destination paths to preservation flags, filter rules, and
/// network options. Instances are typically constructed via [`ClientConfig::builder()`]
/// to enable incremental configuration through a fluent builder API.
///
/// The fields correspond to the option set parsed by upstream `options.c` and
/// transmitted to the remote side via `options.c:server_options()`.
///
/// # Examples
///
/// Basic local copy:
///
/// ```ignore
/// use core::client::ClientConfig;
///
/// let config = ClientConfig::builder()
///     .transfer_args(["source/", "dest/"])
///     .recursive(true)
///     .preserve_times(true)
///     .build();
/// ```
///
/// Transfer with deletion and compression:
///
/// ```ignore
/// use core::client::{ClientConfig, DeleteMode, CompressionSetting};
/// use compress::zlib::CompressionLevel;
///
/// let config = ClientConfig::builder()
///     .transfer_args(["source/", "dest/"])
///     .delete_mode(DeleteMode::Before)
///     .compression_setting(CompressionSetting::level(CompressionLevel::Default))
///     .build();
/// ```
///
/// # See Also
///
/// - [`ClientConfigBuilder`] for the builder interface
/// - [`run_client`](crate::client::run_client) for executing transfers with this configuration
pub struct ClientConfig {
    pub(super) transfer_args: Vec<OsString>,
    pub(super) dry_run: bool,
    pub(super) delete_mode: DeleteMode,
    pub(super) delete_excluded: bool,
    pub(super) delete_missing_args: bool,
    pub(super) ignore_errors: bool,
    pub(super) max_delete: Option<u64>,
    pub(super) recursive: bool,
    pub(super) dirs: bool,
    pub(super) min_file_size: Option<u64>,
    pub(super) max_file_size: Option<u64>,
    pub(super) block_size_override: Option<NonZeroU32>,
    pub(super) rayon_threads: Option<NonZeroUsize>,
    pub(super) tokio_threads: Option<NonZeroUsize>,
    pub(super) max_alloc: Option<u64>,
    pub(super) modify_window: Option<i64>,
    pub(super) remove_source_files: bool,
    /// Whether the user spelled the deprecated `--remove-sent-files` alias (and
    /// it was not overridden by a later `--remove-source-files`). Selects which
    /// spelling `server_options()` forwards on the wire.
    ///
    /// upstream: options.c:730 (option table sets `remove_source_files = 2`) and
    /// options.c:2982-2985 (emit `--remove-sent-files` when the value is 2).
    pub(super) remove_sent_files: bool,
    /// Whether the explicit `--out-format` / `--log-format` string contains the
    /// Whether the resolved out-format string carries the `%i` (itemize)
    /// directive. Mirrors upstream `stdout_format_has_i`, derived from the
    /// resolved format string (not the `-i` flag): an explicit `--out-format`
    /// without `%i` clears it even under `-i`, while `-i` alone installs the
    /// default `"%i %n%L"` format. Drives the `--log-format=%i` server arg.
    ///
    /// upstream: options.c:2345-2358 (`stdout_format_has_i`) and
    /// options.c:2772-2775 (emit `--log-format=%i`).
    pub(super) out_format_forwards_i: bool,
    /// A custom `--out-format` template was given, so remote per-file output is
    /// routed through the client's out-format renderer (collected as events)
    /// rather than printed as the server's default preformatted line.
    pub(super) render_out_format_locally: bool,
    /// `%o` (operation) directive but not `%i`. Drives the `--log-format=%o`
    /// server arg so the remote emits matching operation output.
    ///
    /// upstream: options.c:2375-2376 (`stdout_format_has_o_or_i`) and
    /// options.c:2776-2777 (emit `--log-format=%o`).
    pub(super) out_format_has_operation: bool,
    /// Whether an explicit `--out-format` / `--log-format` string was given that
    /// contains neither `%i` nor `%o`. Drives the placeholder `--log-format=X`
    /// server arg (further gated on the client not being verbose).
    ///
    /// upstream: options.c:2778-2779 (emit `--log-format=X` when `!verbose`).
    pub(super) out_format_placeholder: bool,
    pub(super) bandwidth_limit: Option<BandwidthLimit>,
    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_executability: bool,
    pub(super) preserve_permissions: bool,
    pub(super) fake_super: bool,
    pub(super) super_user: bool,
    pub(super) preserve_times: bool,
    /// Access-time preservation level: 0 = off, 1 = `-U`, 2 = `-UU`.
    pub(super) preserve_atimes: u8,
    pub(super) preserve_crtimes: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) copy_as: Option<OsString>,
    pub(super) chmod: Option<ChmodModifiers>,
    pub(super) user_mapping: Option<UserMapping>,
    pub(super) group_mapping: Option<GroupMapping>,
    pub(super) omit_dir_times: bool,
    pub(super) omit_link_times: bool,
    pub(super) compress: bool,
    pub(super) compression_algorithm: CompressionAlgorithm,
    /// Whether the user explicitly specified `--compress-choice`.
    ///
    /// Distinguishes "user chose zstd" from "zstd is the default."
    /// Required for correct forwarding to the remote peer - upstream
    /// `options.c:2818-2823` only sends `--compress-choice` / `--new-compress`
    /// / `--old-compress` when the user explicitly selected an algorithm.
    pub(super) explicit_compress_choice: bool,
    /// Raw `--compress-choice` name as typed by the user (e.g. `"zlibx"`).
    ///
    /// [`CompressionAlgorithm`] folds `zlibx` into `Zlib` because the two
    /// share the deflate codec, so the enum alone cannot reproduce upstream's
    /// verbatim `compress_choice` name in the `--debug=NSTR` summary. Upstream
    /// prints the user-supplied string (`compat.c:206-219`), so this preserves
    /// it. `None` means the algorithm was the negotiated/default choice, in
    /// which case the summary derives the name from the algorithm.
    pub(super) compress_choice_name: Option<String>,
    pub(super) compression_level: Option<CompressionLevel>,
    pub(super) compression_setting: CompressionSetting,
    /// Worker thread count requested via `--compress-threads=N` (zstd's
    /// `ZSTD_c_nbWorkers`). Propagated to both the local copy engine
    /// (`ActiveCompressor`) and the wire protocol token encoder
    /// (`CompressedTokenEncoder`) via `ServerConfig`.
    ///
    /// Upstream: `options.c:89 do_compression_threads`,
    /// `token.c:701 ZSTD_c_nbWorkers`.
    pub(super) compression_threads: Option<std::num::NonZeroU8>,
    pub(super) skip_compress: SkipCompressList,
    /// Raw `--skip-compress` spec forwarded verbatim to the remote sender.
    ///
    /// `Some` only when the suffix list was explicitly set (CLI or environment);
    /// the built-in default list forwards nothing, matching upstream's NULL
    /// `skip_compress` global (options.c:150).
    pub(super) skip_compress_spec: Option<String>,
    /// Whether `-C` / `--cvs-exclude` was requested (upstream `cvs_exclude`).
    pub(super) cvs_exclude: bool,
    pub(super) open_noatime: bool,
    pub(super) whole_file: Option<bool>,
    /// Internal-only xxh64 file-dedup heuristic toggle.
    ///
    /// Enabled via `--xxh64-dedup`. The receiver hashes both the source and
    /// the existing destination with xxh64 before computing a delta;
    /// matching digests bypass delta computation. The flag is local-only
    /// and never alters the wire protocol forwarded to the peer.
    pub(super) xxh64_dedup: bool,
    pub(super) checksum: bool,
    pub(super) checksum_choice: StrongChecksumChoice,
    pub(super) checksum_seed: Option<u32>,
    pub(super) size_only: bool,
    pub(super) ignore_times: bool,
    pub(super) ignore_existing: bool,
    pub(super) existing_only: bool,
    pub(super) ignore_missing_args: bool,
    pub(super) update: bool,
    pub(super) numeric_ids: bool,
    pub(super) preallocate: bool,
    pub(super) preserve_hard_links: bool,
    pub(super) preserve_symlinks: bool,
    pub(super) filter_rules: Vec<FilterRuleSpec>,
    pub(super) debug_flags: Vec<OsString>,
    pub(super) info_flags: Vec<OsString>,
    pub(super) sparse: bool,
    pub(super) sparse_detect: engine::SparseDetectStrategy,
    pub(super) fuzzy_level: u8,
    pub(super) copy_links: bool,
    pub(super) copy_dirlinks: bool,
    pub(super) copy_unsafe_links: bool,
    pub(super) keep_dirlinks: bool,
    pub(super) safe_links: bool,
    pub(super) munge_links: bool,
    pub(super) trust_sender: bool,
    pub(super) relative_paths: bool,
    pub(super) one_file_system: u8,
    pub(super) implied_dirs: bool,
    pub(super) mkpath: bool,
    pub(super) prune_empty_dirs: bool,
    pub(super) qsort: bool,
    pub(super) inc_recursive_send: bool,
    pub(super) verbosity: u8,
    pub(super) progress: bool,
    pub(super) stats: bool,
    pub(super) human_readable: bool,
    pub(super) partial: bool,
    pub(super) partial_dir: Option<PathBuf>,
    pub(super) temp_directory: Option<PathBuf>,
    pub(super) backup: bool,
    pub(super) backup_dir: Option<PathBuf>,
    pub(super) backup_suffix: Option<OsString>,
    pub(super) delay_updates: bool,
    pub(super) inplace: bool,
    pub(super) append: bool,
    pub(super) append_verify: bool,
    pub(super) force_replacements: bool,
    pub(super) fsync: bool,
    pub(super) io_uring_policy: fast_io::IoUringPolicy,
    pub(super) io_uring_depth: Option<u32>,
    pub(super) cow_policy: fast_io::CowPolicy,
    pub(super) zero_copy_policy: fast_io::ZeroCopyPolicy,
    pub(super) parallel_delta_scan: bool,
    pub(super) itemize_changes: bool,
    pub(super) itemize_unchanged: bool,
    pub(super) force_event_collection: bool,
    pub(super) preserve_devices: bool,
    pub(super) copy_devices: bool,
    pub(super) write_devices: bool,
    pub(super) preserve_specials: bool,
    pub(super) list_only: bool,
    /// Whether the user passed `--list-only` explicitly (upstream `list_only > 1`).
    ///
    /// Distinct from `list_only`, which is also set implicitly for a single
    /// source with no destination. Only the explicit form is forwarded to the
    /// remote as `--list-only` (upstream `options.c:2747`).
    pub(super) list_only_arg: bool,
    /// Whether `-q` / `--quiet` was passed (upstream `quiet`).
    pub(super) quiet: bool,
    /// Tri-state for `--msgs2stderr` / `--no-msgs2stderr` (upstream `msgs2stderr`).
    ///
    /// `None` is the default (upstream value 2); `Some(true)` is `--msgs2stderr`
    /// (value 1); `Some(false)` is `--no-msgs2stderr` (value 0).
    pub(super) msgs2stderr: Option<bool>,
    pub(super) address_mode: AddressMode,
    pub(super) timeout: TransferTimeout,
    pub(super) connect_timeout: TransferTimeout,
    pub(super) stop_at: Option<SystemTime>,
    pub(super) link_dest_paths: Vec<PathBuf>,
    pub(super) reference_directories: Vec<ReferenceDirectory>,
    pub(super) connect_program: Option<OsString>,
    pub(super) bind_address: Option<BindAddress>,
    pub(super) sockopts: Option<OsString>,
    pub(super) tcp_fastopen: TcpFastOpenMode,
    pub(super) blocking_io: Option<bool>,
    pub(super) iconv: IconvSetting,
    pub(super) remote_shell: Option<Vec<OsString>>,
    pub(super) rsync_path: Option<OsString>,
    pub(super) early_input: Option<PathBuf>,
    pub(super) prefer_aes_gcm: Option<bool>,
    pub(super) protect_args: Option<bool>,
    /// `--old-args` / `--no-old-args` - pre-3.0 argument passing.
    ///
    /// When `Some(true)`, filename arguments are passed unescaped to the remote
    /// shell, allowing space-separated paths to be split by `eval`.
    /// upstream: options.c - `old_style_args`, `RSYNC_OLD_ARGS` env var.
    pub(super) old_args: Option<bool>,
    pub(super) jump_hosts: Option<OsString>,
    pub(super) batch_config: Option<engine::batch::BatchConfig>,
    pub(super) files_from: FilesFromSource,
    pub(super) from0: bool,
    /// CLI override for the reorder-buffer spill directory.
    ///
    /// When `Some`, `engine::SpillPolicy::apply_cli_overrides` replaces
    /// the env-var (or default) directory. Precedence: **CLI > env >
    /// defaults**.
    pub(super) spill_dir: Option<PathBuf>,
    /// CLI override for the reorder-buffer spill byte threshold.
    ///
    /// When `Some`, `engine::SpillPolicy::apply_cli_overrides` replaces
    /// the env-var (or default) threshold. Precedence: **CLI > env >
    /// defaults**.
    pub(super) spill_threshold_bytes: Option<u64>,
    /// CLI flag to disable disk-based spilling.
    ///
    /// When `true`, `engine::SpillPolicy::apply_cli_overrides` sets
    /// `in_memory_only` to `true`. Precedence: **CLI > env > defaults**.
    pub(super) no_spill: bool,
    pub(super) no_motd: bool,
    /// Pre-loaded password override for daemon authentication.
    ///
    /// When `Some`, this password takes precedence over the `RSYNC_PASSWORD`
    /// environment variable during daemon handshake. Populated from
    /// `--password-command` or `--password-file` at the CLI layer.
    pub(super) password_override: Option<Vec<u8>>,
    /// Extra options forwarded to the remote rsync process via `-M` / `--remote-option`.
    ///
    /// Each entry is a complete option string (e.g. `--bwlimit=100`) appended
    /// verbatim to the server command line after all locally-derived arguments.
    /// upstream: `options.c:server_options()` appends `remote_options[]` at the
    /// end of the server argument vector.
    pub(super) remote_options: Vec<OsString>,
    pub(super) daemon_params: Vec<String>,
    pub(super) protocol_version: Option<protocol::ProtocolVersion>,
    #[cfg(feature = "embedded-ssh")]
    pub(super) embedded_ssh_config: Option<EmbeddedSshOptions>,
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) preserve_acls: bool,
    /// Extended-attribute preservation level: 0 = off, 1 = `-X`, 2 = `-XX`.
    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub(super) preserve_xattrs: u8,
}

/// Options for the embedded SSH transport (russh-based).
///
/// These fields override defaults from `SshConfig` when the transfer
/// uses `ssh://` URLs with the `embedded-ssh` feature enabled. Fields
/// left as `None` use `SshConfig::default()` values.
#[cfg(feature = "embedded-ssh")]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EmbeddedSshOptions {
    /// Cipher preference list overriding hardware-detected defaults.
    pub ciphers: Vec<String>,
    /// Connection timeout in seconds (overrides `SshConfig` default of 30s).
    pub connect_timeout_secs: Option<u64>,
    /// Keepalive interval in seconds. `Some(0)` disables keepalives.
    pub keepalive_interval_secs: Option<u64>,
    /// Identity file paths for key-based authentication.
    pub identity_files: Vec<std::path::PathBuf>,
    /// Whether to disable SSH agent authentication.
    pub no_agent: bool,
    /// Host key verification policy (`yes`, `no`, `ask`).
    pub strict_host_key_checking: Option<String>,
    /// Prefer IPv6 for DNS resolution.
    pub prefer_ipv6: bool,
    /// Port override (takes precedence over port in the URL).
    pub port: Option<u16>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            transfer_args: Vec::new(),
            dry_run: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            delete_missing_args: false,
            ignore_errors: false,
            max_delete: None,
            recursive: false,
            dirs: false,
            min_file_size: None,
            max_file_size: None,
            block_size_override: None,
            rayon_threads: None,
            tokio_threads: None,
            max_alloc: None,
            modify_window: None,
            remove_source_files: false,
            remove_sent_files: false,
            out_format_forwards_i: false,
            render_out_format_locally: false,
            out_format_has_operation: false,
            out_format_placeholder: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: false,
            fake_super: false,
            super_user: false,
            preserve_times: false,
            preserve_atimes: 0,
            preserve_crtimes: false,
            owner_override: None,
            group_override: None,
            copy_as: None,
            chmod: None,
            user_mapping: None,
            group_mapping: None,
            omit_dir_times: false,
            omit_link_times: false,
            compress: false,
            compression_algorithm: CompressionAlgorithm::default_algorithm(),
            explicit_compress_choice: false,
            compress_choice_name: None,
            compression_level: None,
            compression_setting: CompressionSetting::default(),
            compression_threads: None,
            skip_compress: SkipCompressList::default(),
            skip_compress_spec: None,
            cvs_exclude: false,
            open_noatime: false,
            whole_file: None,
            xxh64_dedup: false,
            checksum: false,
            checksum_choice: StrongChecksumChoice::default(),
            checksum_seed: None,
            size_only: false,
            ignore_times: false,
            ignore_existing: false,
            existing_only: false,
            ignore_missing_args: false,
            update: false,
            numeric_ids: false,
            preallocate: false,
            preserve_hard_links: false,
            preserve_symlinks: false,
            filter_rules: Vec::new(),
            debug_flags: Vec::new(),
            info_flags: Vec::new(),
            sparse: false,
            sparse_detect: engine::SparseDetectStrategy::Auto,
            fuzzy_level: 0,
            copy_links: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            munge_links: false,
            trust_sender: false,
            relative_paths: false,
            one_file_system: 0,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
            qsort: false,
            inc_recursive_send: true,
            verbosity: 0,
            progress: false,
            stats: false,
            human_readable: false,
            partial: false,
            partial_dir: None,
            temp_directory: None,
            backup: false,
            backup_dir: None,
            backup_suffix: None,
            delay_updates: false,
            inplace: false,
            append: false,
            append_verify: false,
            force_replacements: false,
            fsync: false,
            io_uring_policy: fast_io::IoUringPolicy::Auto,
            io_uring_depth: None,
            cow_policy: fast_io::CowPolicy::Auto,
            zero_copy_policy: fast_io::ZeroCopyPolicy::Auto,
            parallel_delta_scan: false,
            itemize_changes: false,
            itemize_unchanged: false,
            force_event_collection: false,
            preserve_devices: false,
            copy_devices: false,
            write_devices: false,
            preserve_specials: false,
            list_only: false,
            list_only_arg: false,
            quiet: false,
            msgs2stderr: None,
            address_mode: AddressMode::Default,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            stop_at: None,
            link_dest_paths: Vec::new(),
            reference_directories: Vec::new(),
            connect_program: None,
            bind_address: None,
            sockopts: None,
            tcp_fastopen: TcpFastOpenMode::Auto,
            blocking_io: None,
            iconv: IconvSetting::Unspecified,
            remote_shell: None,
            rsync_path: None,
            early_input: None,
            prefer_aes_gcm: None,
            protect_args: None,
            old_args: None,
            jump_hosts: None,
            batch_config: None,
            files_from: FilesFromSource::None,
            from0: false,
            spill_dir: None,
            spill_threshold_bytes: None,
            no_spill: false,
            no_motd: false,
            password_override: None,
            remote_options: Vec::new(),
            daemon_params: Vec::new(),
            protocol_version: None,
            #[cfg(feature = "embedded-ssh")]
            embedded_ssh_config: None,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: false,
            #[cfg(all(any(unix, windows), feature = "xattr"))]
            preserve_xattrs: 0,
        }
    }
}

impl ClientConfig {
    /// Creates a new [`ClientConfigBuilder`].
    #[must_use]
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default().recursive(true)
    }

    /// Returns the configured iconv setting.
    #[must_use]
    pub const fn iconv(&self) -> &IconvSetting {
        &self.iconv
    }

    /// Reports whether recursive traversal is enabled.
    #[must_use]
    #[doc(alias = "--recursive")]
    #[doc(alias = "-r")]
    pub const fn recursive(&self) -> bool {
        self.recursive
    }

    /// Reports whether symlinks should be copied as symlinks.
    #[must_use]
    #[doc(alias = "--links")]
    #[doc(alias = "-l")]
    pub const fn links(&self) -> bool {
        self.preserve_symlinks
    }

    /// Reports whether directory entries should be copied when recursion is disabled.
    ///
    /// Folds in `--files-from`: upstream forces `xfer_dirs = 1` whenever a
    /// files-from source is active and `--dirs`/`--no-dirs` was left unset, so
    /// the bare directories named in the list are transferred rather than
    /// skipped by the `!xfer_dirs` guard in `flist.c:2451`.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2190-2191` - `if (files_from) { if (xfer_dirs < 0) xfer_dirs = 1; }`
    #[must_use]
    #[doc(alias = "--dirs")]
    #[doc(alias = "-d")]
    pub const fn dirs(&self) -> bool {
        self.dirs || self.files_from.is_active()
    }
}

mod arguments;
mod deletion;
mod filters;
mod metadata;
mod network;
mod output;
mod partials;
mod paths;
mod performance;
mod preservation;
mod selection;
mod validation;
