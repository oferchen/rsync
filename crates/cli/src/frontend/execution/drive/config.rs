#![deny(unsafe_code)]

use std::ffi::OsString;
use std::num::{NonZeroU8, NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::time::SystemTime;

use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use core::client::{
    AddressMode, BandwidthLimit, BatchConfig, ClientConfig, ClientConfigBuilder,
    CompressionSetting, DeleteMode, FilesFromSource, IconvSetting, SkipCompressList,
    StrongChecksumChoice, TcpFastOpenMode, TransferTimeout,
};
use rsync_io::ssh;

use crate::frontend::progress::{NameOutputLevel, ProgressMode};
use crate::platform::{gid_t, uid_t};

/// All inputs required to assemble the base [`ClientConfig`] before filters are applied.
pub(crate) struct ConfigInputs {
    pub(crate) transfer_operands: Vec<OsString>,
    /// Optional `--protocol=N` ceiling that caps the negotiated protocol version
    /// for remote (SSH and daemon) transfers. `None` uses the default ceiling.
    pub(crate) desired_protocol: Option<protocol::ProtocolVersion>,
    pub(crate) address_mode: AddressMode,
    pub(crate) connect_program: Option<OsString>,
    pub(crate) bind_address: Option<core::client::BindAddress>,
    pub(crate) sockopts: Option<OsString>,
    pub(crate) tcp_fastopen: TcpFastOpenMode,
    pub(crate) blocking_io: Option<bool>,
    pub(crate) dry_run: bool,
    pub(crate) list_only: bool,
    /// Whether `--list-only` was passed explicitly (upstream `list_only > 1`).
    pub(crate) list_only_arg: bool,
    /// Whether `-q` / `--quiet` was passed (upstream `quiet`).
    pub(crate) quiet: bool,
    /// Tri-state `--msgs2stderr` / `--no-msgs2stderr` (upstream `msgs2stderr`).
    pub(crate) msgs2stderr: Option<bool>,
    pub(crate) recursive: bool,
    pub(crate) dirs: Option<bool>,
    pub(crate) delete_mode: DeleteMode,
    pub(crate) delete_excluded: bool,
    pub(crate) delete_missing_args: bool,
    pub(crate) ignore_errors: bool,
    pub(crate) max_delete_limit: Option<u64>,
    pub(crate) min_size_limit: Option<u64>,
    pub(crate) max_size_limit: Option<u64>,
    pub(crate) block_size_override: Option<NonZeroU32>,
    pub(crate) rayon_threads: Option<NonZeroUsize>,
    pub(crate) tokio_threads: Option<NonZeroUsize>,
    pub(crate) max_alloc: Option<u64>,
    pub(crate) backup: bool,
    pub(crate) backup_dir: Option<PathBuf>,
    pub(crate) backup_suffix: Option<OsString>,
    pub(crate) bandwidth_limit: Option<BandwidthLimit>,
    pub(crate) compression_setting: CompressionSetting,
    pub(crate) compress: bool,
    pub(crate) compression_level_override: Option<compress::zlib::CompressionLevel>,
    pub(crate) compression_algorithm: Option<CompressionAlgorithm>,
    /// Raw `--compress-choice` name preserved for the `--debug=NSTR` summary.
    pub(crate) compress_choice_name: Option<String>,
    pub(crate) compression_threads: Option<NonZeroU8>,
    pub(crate) open_noatime: bool,
    pub(crate) owner: bool,
    pub(crate) owner_override: Option<uid_t>,
    pub(crate) group: bool,
    pub(crate) group_override: Option<gid_t>,
    pub(crate) copy_as: Option<OsString>,
    pub(crate) chmod_modifiers: Option<ChmodModifiers>,
    pub(crate) user_mapping: Option<UserMapping>,
    pub(crate) group_mapping: Option<GroupMapping>,
    pub(crate) executability: bool,
    pub(crate) permissions: bool,
    pub(crate) fake_super: bool,
    /// Explicit `--super` request (upstream `am_root > 1`), forwarded on push.
    pub(crate) super_user: bool,
    pub(crate) times: bool,
    /// Access-time preservation level (0 = off, 1 = `-U`, 2 = `-UU`).
    pub(crate) atimes: u8,
    pub(crate) crtimes: bool,
    pub(crate) modify_window_setting: Option<i64>,
    pub(crate) omit_dir_times: bool,
    pub(crate) omit_link_times: bool,
    pub(crate) devices: bool,
    pub(crate) copy_devices: bool,
    pub(crate) write_devices: bool,
    pub(crate) specials: bool,
    pub(crate) force_replacements: bool,
    pub(crate) checksum: bool,
    pub(crate) checksum_seed: Option<u32>,
    pub(crate) size_only: bool,
    pub(crate) ignore_times: bool,
    pub(crate) ignore_existing: bool,
    pub(crate) existing_only: bool,
    pub(crate) ignore_missing_args: bool,
    pub(crate) update: bool,
    pub(crate) numeric_ids: bool,
    pub(crate) hard_links: bool,
    pub(crate) links: bool,
    pub(crate) sparse: bool,
    pub(crate) sparse_detect: engine::SparseDetectStrategy,
    pub(crate) copy_links: bool,
    pub(crate) copy_dirlinks: bool,
    pub(crate) copy_unsafe_links: bool,
    pub(crate) keep_dirlinks: bool,
    pub(crate) safe_links: bool,
    pub(crate) munge_links: bool,
    pub(crate) trust_sender: bool,
    pub(crate) fuzzy_level: u8,
    pub(crate) relative_paths: bool,
    pub(crate) one_file_system: u8,
    pub(crate) implied_dirs: bool,
    pub(crate) human_readable: bool,
    pub(crate) mkpath: bool,
    pub(crate) prune_empty_dirs: bool,
    pub(crate) qsort: bool,
    /// Resolved tri-state for `--inc-recursive` / `--no-inc-recursive`.
    ///
    /// `None` keeps the upstream default (advertise `'i'`); `Some(false)`
    /// suppresses it (`--no-inc-recursive`). Mirrors upstream
    /// `compat.c:720 set_allow_inc_recurse()`.
    pub(crate) inc_recursive_send: Option<bool>,
    pub(crate) verbosity: u8,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) stats: bool,
    pub(crate) debug_flags_list: Vec<OsString>,
    /// Explicit `--info` categories forwarded to a remote peer.
    pub(crate) info_flags_list: Vec<OsString>,
    pub(crate) partial: bool,
    pub(crate) preallocate: bool,
    pub(crate) fsync: bool,
    pub(crate) io_uring_policy: fast_io::IoUringPolicy,
    pub(crate) io_uring_depth: Option<u32>,
    pub(crate) zero_copy_policy: fast_io::ZeroCopyPolicy,
    /// `--parallel-delta-scan` - opt-in, default-off local sender-side delta
    /// scan across multiple cores. Local-only; never forwarded to a peer.
    pub(crate) parallel_delta_scan: bool,
    pub(crate) cow_policy: fast_io::CowPolicy,
    pub(crate) partial_dir: Option<PathBuf>,
    pub(crate) temp_dir: Option<PathBuf>,
    pub(crate) delay_updates: bool,
    pub(crate) link_dests: Vec<PathBuf>,
    pub(crate) remove_source_files: bool,
    /// `--remove-sent-files` - deprecated alias; forwarded verbatim on the wire.
    /// upstream: options.c:2982-2985.
    pub(crate) remove_sent_files: bool,
    /// Resolved out-format string carries `%i`; forwards `--log-format=%i`.
    /// Mirrors upstream `stdout_format_has_i`, derived from the resolved format
    /// string rather than the `-i` flag. upstream: options.c:2345-2358,2772-2775.
    pub(crate) out_format_forwards_i: bool,
    /// A custom `--out-format` template was given, so remote per-file output is
    /// rendered client-side from collected events instead of the server line.
    pub(crate) render_out_format_locally: bool,
    /// Explicit `--out-format` / `--log-format` contains `%o` but not `%i`;
    /// forwards `--log-format=%o`. upstream: options.c:2776-2777.
    pub(crate) out_format_has_operation: bool,
    /// Explicit `--out-format` / `--log-format` was given with neither `%i` nor
    /// `%o`; forwards the placeholder `--log-format=X` for a non-verbose client.
    /// upstream: options.c:2778-2779.
    pub(crate) out_format_placeholder: bool,
    pub(crate) inplace: bool,
    pub(crate) append: bool,
    pub(crate) append_verify: bool,
    pub(crate) whole_file: Option<bool>,
    pub(crate) xxh64_dedup: bool,
    pub(crate) timeout: TransferTimeout,
    pub(crate) connect_timeout: TransferTimeout,
    pub(crate) stop_deadline: Option<SystemTime>,
    pub(crate) checksum_choice: Option<StrongChecksumChoice>,
    pub(crate) compare_destinations: Vec<OsString>,
    pub(crate) copy_destinations: Vec<OsString>,
    pub(crate) link_destinations: Vec<OsString>,
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(crate) preserve_acls: bool,
    /// Extended-attribute preservation level (0 = off, 1 = `-X`, 2 = `-XX`).
    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub(crate) xattrs: u8,
    pub(crate) skip_compress_list: Option<SkipCompressList>,
    /// Raw `--skip-compress` spec forwarded to the remote sender; `Some` only
    /// when explicitly set.
    pub(crate) skip_compress_spec: Option<String>,
    /// `-C` / `--cvs-exclude` request, forwarded to the peer as the `C` letter.
    pub(crate) cvs_exclude: bool,
    pub(crate) itemize_changes: bool,
    pub(crate) out_format_template: Option<crate::frontend::out_format::OutFormat>,
    pub(crate) log_file_template: Option<crate::frontend::out_format::OutFormat>,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) iconv: IconvSetting,
    pub(crate) remote_shell: Option<OsString>,
    pub(crate) rsync_path: Option<OsString>,
    pub(crate) early_input: Option<PathBuf>,
    pub(crate) prefer_aes_gcm: Option<bool>,
    pub(crate) protect_args: Option<bool>,
    pub(crate) old_args: Option<bool>,
    pub(crate) jump_hosts: Option<OsString>,
    pub(crate) batch_config: Option<BatchConfig>,
    pub(crate) no_motd: bool,
    pub(crate) password_override: Option<Vec<u8>>,
    /// Extra options forwarded to the remote rsync process via `-M`.
    pub(crate) remote_options: Vec<OsString>,
    pub(crate) daemon_params: Vec<String>,
    pub(crate) files_from: FilesFromSource,
    pub(crate) from0: bool,
    /// CLI override for the reorder-buffer spill directory.
    ///
    /// `Some` replaces both the env-var value and the `SpillPolicy::dir`
    /// default. Precedence is **CLI > env > defaults**, applied via
    /// `engine::SpillPolicy::apply_cli_overrides`.
    pub(crate) spill_dir: Option<PathBuf>,
    /// CLI override for the reorder-buffer spill byte threshold.
    ///
    /// `Some` replaces both the env-var value and the
    /// `SpillPolicy::threshold_bytes` default. Precedence is
    /// **CLI > env > defaults**, applied via
    /// `engine::SpillPolicy::apply_cli_overrides`.
    pub(crate) spill_threshold_bytes: Option<u64>,
    /// CLI flag to disable disk-based spilling.
    ///
    /// When `true`, sets `SpillPolicy::in_memory_only` to `true`. Takes
    /// precedence over `OC_RSYNC_NO_SPILL`. Applied via
    /// `engine::SpillPolicy::apply_cli_overrides`.
    pub(crate) no_spill: bool,
}

/// Builds the base [`ClientConfigBuilder`] from the provided inputs.
pub(crate) fn build_base_config(mut inputs: ConfigInputs) -> ClientConfigBuilder {
    let mut builder = ClientConfig::builder()
        .transfer_args(std::mem::take(&mut inputs.transfer_operands))
        .protocol_version(inputs.desired_protocol)
        .address_mode(inputs.address_mode)
        .connect_program(inputs.connect_program.clone())
        .bind_address(inputs.bind_address.clone())
        .sockopts(inputs.sockopts.clone())
        .tcp_fastopen(inputs.tcp_fastopen)
        .blocking_io(inputs.blocking_io)
        .dry_run(inputs.dry_run)
        .list_only(inputs.list_only)
        .list_only_arg(inputs.list_only_arg)
        .quiet(inputs.quiet)
        .msgs2stderr(inputs.msgs2stderr)
        .recursive(inputs.recursive)
        // upstream: options.c:2199-2203 - `else if (recurse) xfer_dirs = 1;
        // else if (xfer_dirs < 0) xfer_dirs = list_only ? 1 : 0;`. When neither
        // -r nor an explicit -d/--no-dirs is given, --list-only still transfers
        // (lists) a bare directory operand's own entry.
        .dirs(if inputs.recursive {
            true
        } else {
            inputs.dirs.unwrap_or(inputs.list_only)
        })
        // upstream: options.c:2215-2217 - delete mode is enabled only by an
        // explicit `--delete*` or `--delete-excluded`. `--max-delete` merely caps
        // the count (options.c:2182-2185) and must never enable deletion.
        .delete(inputs.delete_mode.is_enabled() || inputs.delete_excluded)
        .delete_excluded(inputs.delete_excluded)
        .delete_missing_args(inputs.delete_missing_args)
        .ignore_errors(inputs.ignore_errors)
        .max_delete(inputs.max_delete_limit)
        .min_file_size(inputs.min_size_limit)
        .max_file_size(inputs.max_size_limit)
        .block_size_override(inputs.block_size_override)
        .rayon_threads(inputs.rayon_threads)
        .tokio_threads(inputs.tokio_threads)
        .max_alloc(inputs.max_alloc)
        .backup(inputs.backup)
        .backup_directory(inputs.backup_dir.clone())
        .backup_suffix(inputs.backup_suffix.clone())
        .bandwidth_limit(inputs.bandwidth_limit.take())
        .compression_setting(inputs.compression_setting)
        .compress(inputs.compress)
        .compression_level(inputs.compression_level_override)
        .compression_threads(inputs.compression_threads)
        .open_noatime(inputs.open_noatime)
        .owner(inputs.owner)
        .owner_override(inputs.owner_override)
        .group(inputs.group)
        .group_override(inputs.group_override)
        .copy_as(inputs.copy_as.clone())
        .chmod(inputs.chmod_modifiers.clone())
        .user_mapping(inputs.user_mapping.clone())
        .group_mapping(inputs.group_mapping.clone())
        .executability(inputs.executability)
        .permissions(inputs.permissions)
        .fake_super(inputs.fake_super)
        .super_user(inputs.super_user)
        .times(inputs.times)
        .atimes(inputs.atimes)
        .crtimes(inputs.crtimes)
        .modify_window(inputs.modify_window_setting)
        .omit_dir_times(inputs.omit_dir_times)
        .omit_link_times(inputs.omit_link_times)
        .devices(inputs.devices)
        .copy_devices(inputs.copy_devices)
        .write_devices(inputs.write_devices)
        .specials(inputs.specials)
        .force_replacements(inputs.force_replacements)
        .checksum(inputs.checksum)
        .checksum_seed(inputs.checksum_seed)
        .size_only(inputs.size_only)
        .ignore_times(inputs.ignore_times)
        .ignore_existing(inputs.ignore_existing)
        .existing_only(inputs.existing_only)
        .ignore_missing_args(inputs.ignore_missing_args)
        .update(inputs.update)
        .numeric_ids(inputs.numeric_ids)
        .hard_links(inputs.hard_links)
        .links(inputs.links)
        .sparse(inputs.sparse)
        .sparse_detect(inputs.sparse_detect)
        .fuzzy_level(inputs.fuzzy_level)
        .copy_links(inputs.copy_links)
        .copy_dirlinks(inputs.copy_dirlinks)
        .copy_unsafe_links(inputs.copy_unsafe_links)
        .keep_dirlinks(inputs.keep_dirlinks)
        .safe_links(inputs.safe_links)
        .munge_links(inputs.munge_links)
        .trust_sender(inputs.trust_sender)
        .relative_paths(inputs.relative_paths)
        .one_file_system(inputs.one_file_system)
        .implied_dirs(inputs.implied_dirs)
        .human_readable(inputs.human_readable)
        .mkpath(inputs.mkpath)
        .prune_empty_dirs(inputs.prune_empty_dirs)
        .qsort(inputs.qsort);
    // Only override the builder's upstream default when the user supplied
    // `--inc-recursive` or `--no-inc-recursive`. Mirrors upstream
    // `compat.c:720 set_allow_inc_recurse()`.
    if let Some(value) = inputs.inc_recursive_send {
        builder = builder.inc_recursive_send(value);
    }
    builder = builder
        .verbosity(inputs.verbosity)
        .progress(inputs.progress_mode.is_some())
        .stats(inputs.stats)
        .debug_flags(inputs.debug_flags_list.clone())
        .info_flags(inputs.info_flags_list.clone())
        .partial(inputs.partial)
        .preallocate(inputs.preallocate)
        .fsync(inputs.fsync)
        .io_uring_policy(inputs.io_uring_policy)
        .io_uring_depth(inputs.io_uring_depth)
        .zero_copy_policy(inputs.zero_copy_policy)
        .parallel_delta_scan(inputs.parallel_delta_scan)
        .cow_policy(inputs.cow_policy)
        .partial_directory(inputs.partial_dir.clone())
        .temp_directory(inputs.temp_dir.clone())
        .delay_updates(inputs.delay_updates)
        .extend_link_dests(inputs.link_dests.clone())
        .remove_source_files(inputs.remove_source_files)
        .remove_sent_files(inputs.remove_sent_files)
        .out_format_forwards_i(inputs.out_format_forwards_i)
        .render_out_format_locally(inputs.render_out_format_locally)
        .out_format_has_operation(inputs.out_format_has_operation)
        .out_format_placeholder(inputs.out_format_placeholder)
        .inplace(inputs.inplace)
        .append(inputs.append)
        .append_verify(inputs.append_verify)
        .whole_file_option(inputs.whole_file)
        .xxh64_dedup(inputs.xxh64_dedup)
        .timeout(inputs.timeout)
        .connect_timeout(inputs.connect_timeout)
        .stop_at(inputs.stop_deadline)
        .iconv(inputs.iconv.clone());

    if let Some(ref shell_spec) = inputs.remote_shell {
        match ssh::parse_remote_shell(shell_spec) {
            Ok(args) => {
                builder = builder.set_remote_shell(args);
            }
            Err(_e) => {}
        }
    }

    if let Some(ref path) = inputs.rsync_path {
        builder = builder.set_rsync_path(path.clone());
    }

    builder = builder.early_input(inputs.early_input.clone());
    builder = builder
        .prefer_aes_gcm(inputs.prefer_aes_gcm)
        .protect_args(inputs.protect_args)
        .old_args(inputs.old_args)
        .set_jump_hosts(inputs.jump_hosts.clone());

    if let Some(batch_cfg) = inputs.batch_config {
        builder = builder.batch_config(Some(batch_cfg));
    }

    if let Some(algorithm) = inputs.compression_algorithm {
        builder = builder.compression_algorithm(algorithm);
    }

    if inputs.compress_choice_name.is_some() {
        builder = builder.compress_choice_name(inputs.compress_choice_name.take());
    }

    if let Some(choice) = inputs.checksum_choice {
        builder = builder.checksum_choice(choice);
    }

    for path in &inputs.compare_destinations {
        builder = builder.compare_destination(PathBuf::from(path));
    }

    for path in &inputs.copy_destinations {
        builder = builder.copy_destination(PathBuf::from(path));
    }

    for path in &inputs.link_destinations {
        builder = builder.link_destination(PathBuf::from(path));
    }

    #[cfg(all(any(unix, windows), feature = "acl"))]
    {
        builder = builder.acls(inputs.preserve_acls);
    }

    #[cfg(all(any(unix, windows), feature = "xattr"))]
    {
        builder = builder.xattrs(inputs.xattrs);
    }

    if let Some(list) = inputs.skip_compress_list.take() {
        builder = builder.skip_compress(list);
    }

    if let Some(spec) = inputs.skip_compress_spec.take() {
        builder = builder.skip_compress_spec(Some(spec));
    }

    builder = builder.cvs_exclude(inputs.cvs_exclude);

    builder = match inputs.delete_mode {
        DeleteMode::Before => builder.delete_before(true),
        DeleteMode::After => builder.delete_after(true),
        DeleteMode::Delay => builder.delete_delay(true),
        DeleteMode::During | DeleteMode::DuringDefault | DeleteMode::Disabled => builder,
    };

    builder = builder.itemize_changes(inputs.itemize_changes);
    // upstream: generator.c:575-576 - emit itemize rows for unchanged entries
    // when `-ii` / `--info=name2` / `-vv` raised the level. The parser folds
    // all three into `NameOutputLevel::UpdatedAndUnchanged`.
    builder = builder.itemize_unchanged(matches!(
        inputs.name_level,
        NameOutputLevel::UpdatedAndUnchanged
    ));

    let force_event_collection = inputs.itemize_changes
        || inputs.out_format_template.is_some()
        || inputs.log_file_template.is_some()
        || !matches!(inputs.name_level, NameOutputLevel::Disabled);

    builder = builder.files_from(inputs.files_from).from0(inputs.from0);

    builder = builder
        .spill_dir(inputs.spill_dir)
        .spill_threshold_bytes(inputs.spill_threshold_bytes)
        .no_spill(inputs.no_spill);

    builder
        .force_event_collection(force_event_collection)
        .no_motd(inputs.no_motd)
        .password_override(inputs.password_override)
        .remote_options(inputs.remote_options)
        .daemon_params(inputs.daemon_params)
}
