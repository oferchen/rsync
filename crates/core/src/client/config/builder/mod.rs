use std::ffi::OsString;
use std::path::PathBuf;

use super::{
    AddressMode, BandwidthLimit, BindAddress, ClientConfig, CompressionSetting, DeleteMode,
    FilterRuleSpec, ReferenceDirectory, ReferenceDirectoryKind, StrongChecksumChoice,
    TransferTimeout,
};
use rsync_compress::zlib::CompressionLevel;
use rsync_engine::SkipCompressList;
use rsync_meta::ChmodModifiers;

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    max_delete: Option<u64>,
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,
    modify_window: Option<u64>,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    chmod: Option<ChmodModifiers>,
    omit_dir_times: bool,
    omit_link_times: bool,
    compress: bool,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    skip_compress: SkipCompressList,
    whole_file: Option<bool>,
    checksum: bool,
    checksum_choice: StrongChecksumChoice,
    checksum_seed: Option<u32>,
    size_only: bool,
    ignore_existing: bool,
    ignore_missing_args: bool,
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    preserve_hard_links: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,
    relative_paths: bool,
    one_file_system: bool,
    implied_dirs: Option<bool>,
    mkpath: bool,
    prune_empty_dirs: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    human_readable: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    temp_directory: Option<PathBuf>,
    backup: bool,
    backup_dir: Option<PathBuf>,
    backup_suffix: Option<OsString>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    force_event_collection: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    address_mode: AddressMode,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
    link_dest_paths: Vec<PathBuf>,
    reference_directories: Vec<ReferenceDirectory>,
    connect_program: Option<OsString>,
    bind_address: Option<BindAddress>,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
}

impl ClientConfigBuilder {
    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
            dry_run: self.dry_run,
            delete_mode: self.delete_mode,
            delete_excluded: self.delete_excluded,
            max_delete: self.max_delete,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            modify_window: self.modify_window,
            remove_source_files: self.remove_source_files,
            bandwidth_limit: self.bandwidth_limit,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            owner_override: self.owner_override,
            group_override: self.group_override,
            chmod: self.chmod,
            omit_dir_times: self.omit_dir_times,
            omit_link_times: self.omit_link_times,
            compress: self.compress,
            compression_level: self.compression_level,
            compression_setting: self.compression_setting,
            skip_compress: self.skip_compress,
            whole_file: self.whole_file.unwrap_or(true),
            checksum: self.checksum,
            checksum_choice: self.checksum_choice,
            checksum_seed: self.checksum_seed,
            size_only: self.size_only,
            ignore_existing: self.ignore_existing,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            numeric_ids: self.numeric_ids,
            preallocate: self.preallocate,
            preserve_hard_links: self.preserve_hard_links,
            filter_rules: self.filter_rules,
            debug_flags: self.debug_flags,
            sparse: self.sparse,
            copy_links: self.copy_links,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            implied_dirs: self.implied_dirs.unwrap_or(true),
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            verbosity: self.verbosity,
            progress: self.progress,
            stats: self.stats,
            human_readable: self.human_readable,
            partial: self.partial,
            partial_dir: self.partial_dir,
            temp_directory: self.temp_directory,
            backup: self.backup,
            backup_dir: self.backup_dir,
            backup_suffix: self.backup_suffix,
            delay_updates: self.delay_updates,
            inplace: self.inplace,
            append: self.append,
            append_verify: self.append_verify,
            force_event_collection: self.force_event_collection,
            preserve_devices: self.preserve_devices,
            preserve_specials: self.preserve_specials,
            list_only: self.list_only,
            address_mode: self.address_mode,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            link_dest_paths: self.link_dest_paths,
            reference_directories: self.reference_directories,
            connect_program: self.connect_program,
            bind_address: self.bind_address,
            #[cfg(feature = "acl")]
            preserve_acls: self.preserve_acls,
            #[cfg(feature = "xattr")]
            preserve_xattrs: self.preserve_xattrs,
        }
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
