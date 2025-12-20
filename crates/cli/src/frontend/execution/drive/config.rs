#![deny(unsafe_code)]

use std::ffi::OsString;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::SystemTime;

use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use core::client::{
    AddressMode, BandwidthLimit, ClientConfig, ClientConfigBuilder, CompressionSetting, DeleteMode,
    IconvSetting, SkipCompressList, StrongChecksumChoice, TransferTimeout,
};
use engine::batch;
use transport::ssh;

use crate::frontend::progress::{NameOutputLevel, ProgressMode};
use crate::platform::{gid_t, uid_t};

/// All inputs required to assemble the base [`ClientConfig`] before filters are applied.
pub(crate) struct ConfigInputs {
    pub(crate) transfer_operands: Vec<OsString>,
    pub(crate) address_mode: AddressMode,
    pub(crate) connect_program: Option<OsString>,
    pub(crate) bind_address: Option<core::client::BindAddress>,
    pub(crate) sockopts: Option<OsString>,
    pub(crate) blocking_io: Option<bool>,
    pub(crate) dry_run: bool,
    pub(crate) list_only: bool,
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
    pub(crate) backup: bool,
    pub(crate) backup_dir: Option<PathBuf>,
    pub(crate) backup_suffix: Option<OsString>,
    pub(crate) bandwidth_limit: Option<BandwidthLimit>,
    pub(crate) compression_setting: CompressionSetting,
    pub(crate) compress: bool,
    pub(crate) compression_level_override: Option<compress::zlib::CompressionLevel>,
    pub(crate) compression_algorithm: Option<CompressionAlgorithm>,
    pub(crate) open_noatime: bool,
    pub(crate) owner: bool,
    pub(crate) owner_override: Option<uid_t>,
    pub(crate) group: bool,
    pub(crate) group_override: Option<gid_t>,
    pub(crate) chmod_modifiers: Option<ChmodModifiers>,
    pub(crate) user_mapping: Option<UserMapping>,
    pub(crate) group_mapping: Option<GroupMapping>,
    pub(crate) executability: bool,
    pub(crate) permissions: bool,
    pub(crate) fake_super: bool,
    pub(crate) times: bool,
    pub(crate) atimes: bool,
    pub(crate) crtimes: bool,
    pub(crate) modify_window_setting: Option<u64>,
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
    pub(crate) copy_links: bool,
    pub(crate) copy_dirlinks: bool,
    pub(crate) copy_unsafe_links: bool,
    pub(crate) keep_dirlinks: bool,
    pub(crate) safe_links: bool,
    pub(crate) fuzzy: bool,
    pub(crate) relative_paths: bool,
    pub(crate) one_file_system: bool,
    pub(crate) implied_dirs: bool,
    pub(crate) human_readable: bool,
    pub(crate) mkpath: bool,
    pub(crate) prune_empty_dirs: bool,
    pub(crate) verbosity: u8,
    pub(crate) progress_mode: Option<ProgressMode>,
    pub(crate) stats: bool,
    pub(crate) debug_flags_list: Vec<OsString>,
    pub(crate) partial: bool,
    pub(crate) preallocate: bool,
    pub(crate) fsync: bool,
    pub(crate) partial_dir: Option<PathBuf>,
    pub(crate) temp_dir: Option<PathBuf>,
    pub(crate) delay_updates: bool,
    pub(crate) link_dests: Vec<PathBuf>,
    pub(crate) remove_source_files: bool,
    pub(crate) inplace: bool,
    pub(crate) append: bool,
    pub(crate) append_verify: bool,
    pub(crate) whole_file: bool,
    pub(crate) force_fallback: bool,
    pub(crate) timeout: TransferTimeout,
    pub(crate) connect_timeout: TransferTimeout,
    pub(crate) stop_deadline: Option<SystemTime>,
    pub(crate) checksum_choice: Option<StrongChecksumChoice>,
    pub(crate) compare_destinations: Vec<OsString>,
    pub(crate) copy_destinations: Vec<OsString>,
    pub(crate) link_destinations: Vec<OsString>,
    #[cfg(feature = "acl")]
    pub(crate) preserve_acls: bool,
    #[cfg(feature = "xattr")]
    pub(crate) xattrs: bool,
    pub(crate) skip_compress_list: Option<SkipCompressList>,
    pub(crate) itemize_changes: bool,
    pub(crate) out_format_template: Option<crate::frontend::out_format::OutFormat>,
    pub(crate) log_file_template: Option<crate::frontend::out_format::OutFormat>,
    pub(crate) name_level: NameOutputLevel,
    pub(crate) iconv: IconvSetting,
    pub(crate) remote_shell: Option<OsString>,
    pub(crate) rsync_path: Option<OsString>,
    pub(crate) batch_config: Option<batch::BatchConfig>,
}

/// Builds the base [`ClientConfigBuilder`] from the provided inputs.
pub(crate) fn build_base_config(mut inputs: ConfigInputs) -> ClientConfigBuilder {
    let mut builder = ClientConfig::builder()
        .transfer_args(std::mem::take(&mut inputs.transfer_operands))
        .address_mode(inputs.address_mode)
        .connect_program(inputs.connect_program.clone())
        .bind_address(inputs.bind_address.clone())
        .sockopts(inputs.sockopts.clone())
        .blocking_io(inputs.blocking_io)
        .dry_run(inputs.dry_run)
        .list_only(inputs.list_only)
        .recursive(inputs.recursive)
        .dirs(if inputs.recursive {
            true
        } else {
            inputs.dirs.unwrap_or(false)
        })
        .delete(
            inputs.delete_mode.is_enabled()
                || inputs.delete_excluded
                || inputs.max_delete_limit.is_some(),
        )
        .delete_excluded(inputs.delete_excluded)
        .delete_missing_args(inputs.delete_missing_args)
        .ignore_errors(inputs.ignore_errors)
        .max_delete(inputs.max_delete_limit)
        .min_file_size(inputs.min_size_limit)
        .max_file_size(inputs.max_size_limit)
        .block_size_override(inputs.block_size_override)
        .backup(inputs.backup)
        .backup_directory(inputs.backup_dir.clone())
        .backup_suffix(inputs.backup_suffix.clone())
        .bandwidth_limit(inputs.bandwidth_limit.take())
        .compression_setting(inputs.compression_setting)
        .compress(inputs.compress)
        .compression_level(inputs.compression_level_override)
        .open_noatime(inputs.open_noatime)
        .owner(inputs.owner)
        .owner_override(inputs.owner_override)
        .group(inputs.group)
        .group_override(inputs.group_override)
        .chmod(inputs.chmod_modifiers.clone())
        .user_mapping(inputs.user_mapping.clone())
        .group_mapping(inputs.group_mapping.clone())
        .executability(inputs.executability)
        .permissions(inputs.permissions)
        .fake_super(inputs.fake_super)
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
        .fuzzy(inputs.fuzzy)
        .copy_links(inputs.copy_links)
        .copy_dirlinks(inputs.copy_dirlinks)
        .copy_unsafe_links(inputs.copy_unsafe_links)
        .keep_dirlinks(inputs.keep_dirlinks)
        .safe_links(inputs.safe_links)
        .relative_paths(inputs.relative_paths)
        .one_file_system(inputs.one_file_system)
        .implied_dirs(inputs.implied_dirs)
        .human_readable(inputs.human_readable)
        .mkpath(inputs.mkpath)
        .prune_empty_dirs(inputs.prune_empty_dirs)
        .verbosity(inputs.verbosity)
        .progress(inputs.progress_mode.is_some())
        .stats(inputs.stats)
        .debug_flags(inputs.debug_flags_list.clone())
        .partial(inputs.partial)
        .preallocate(inputs.preallocate)
        .fsync(inputs.fsync)
        .partial_directory(inputs.partial_dir.clone())
        .temp_directory(inputs.temp_dir.clone())
        .delay_updates(inputs.delay_updates)
        .extend_link_dests(inputs.link_dests.clone())
        .remove_source_files(inputs.remove_source_files)
        .inplace(inputs.inplace)
        .append(inputs.append)
        .append_verify(inputs.append_verify)
        .whole_file(inputs.whole_file)
        .force_fallback(inputs.force_fallback)
        .timeout(inputs.timeout)
        .connect_timeout(inputs.connect_timeout)
        .stop_at(inputs.stop_deadline)
        .iconv(inputs.iconv.clone());

    // Configure custom remote shell if specified
    if let Some(ref shell_spec) = inputs.remote_shell {
        match ssh::parse_remote_shell(shell_spec) {
            Ok(args) => {
                builder = builder.set_remote_shell(args);
            }
            Err(e) => {
                // Invalid remote shell specification - this should have been caught
                // during argument validation, but handle it gracefully here
                eprintln!("warning: invalid remote shell specification: {e}");
            }
        }
    }

    // Configure custom remote rsync path if specified
    if let Some(ref path) = inputs.rsync_path {
        builder = builder.set_rsync_path(path.clone());
    }

    // Configure batch mode if specified
    if let Some(batch_cfg) = inputs.batch_config {
        builder = builder.batch_config(Some(batch_cfg));
    }

    if let Some(algorithm) = inputs.compression_algorithm {
        builder = builder.compression_algorithm(algorithm);
    }

    if let Some(choice) = inputs.checksum_choice {
        builder = builder.checksum_choice(choice);
    }

    for path in inputs.compare_destinations.iter() {
        builder = builder.compare_destination(PathBuf::from(path));
    }

    for path in inputs.copy_destinations.iter() {
        builder = builder.copy_destination(PathBuf::from(path));
    }

    for path in inputs.link_destinations.iter() {
        builder = builder.link_destination(PathBuf::from(path));
    }

    #[cfg(feature = "acl")]
    {
        builder = builder.acls(inputs.preserve_acls);
    }

    #[cfg(feature = "xattr")]
    {
        builder = builder.xattrs(inputs.xattrs);
    }

    if let Some(list) = inputs.skip_compress_list.take() {
        builder = builder.skip_compress(list);
    }

    builder = match inputs.delete_mode {
        DeleteMode::Before => builder.delete_before(true),
        DeleteMode::After => builder.delete_after(true),
        DeleteMode::Delay => builder.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => builder,
    };

    let force_event_collection = inputs.itemize_changes
        || inputs.out_format_template.is_some()
        || inputs.log_file_template.is_some()
        || !matches!(inputs.name_level, NameOutputLevel::Disabled);

    builder.force_event_collection(force_event_collection)
}
