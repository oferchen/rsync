use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsync_compress::zlib::CompressionLevel;
use rsync_engine::SkipCompressList;
use rsync_meta::ChmodModifiers;

use super::builder::ClientConfigBuilder;
use super::{
    AddressMode, BandwidthLimit, BindAddress, CompressionSetting, DeleteMode, FilterRuleSpec,
    ReferenceDirectory, StrongChecksumChoice, TransferTimeout,
};

/// Configuration describing the requested client operation.
pub struct ClientConfig {
    pub(super) transfer_args: Vec<OsString>,
    pub(super) dry_run: bool,
    pub(super) delete_mode: DeleteMode,
    pub(super) delete_excluded: bool,
    pub(super) max_delete: Option<u64>,
    pub(super) min_file_size: Option<u64>,
    pub(super) max_file_size: Option<u64>,
    pub(super) modify_window: Option<u64>,
    pub(super) remove_source_files: bool,
    pub(super) bandwidth_limit: Option<BandwidthLimit>,
    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_permissions: bool,
    pub(super) preserve_times: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) chmod: Option<ChmodModifiers>,
    pub(super) omit_dir_times: bool,
    pub(super) omit_link_times: bool,
    pub(super) compress: bool,
    pub(super) compression_level: Option<CompressionLevel>,
    pub(super) compression_setting: CompressionSetting,
    pub(super) skip_compress: SkipCompressList,
    pub(super) whole_file: bool,
    pub(super) checksum: bool,
    pub(super) checksum_choice: StrongChecksumChoice,
    pub(super) checksum_seed: Option<u32>,
    pub(super) size_only: bool,
    pub(super) ignore_existing: bool,
    pub(super) ignore_missing_args: bool,
    pub(super) update: bool,
    pub(super) numeric_ids: bool,
    pub(super) preallocate: bool,
    pub(super) preserve_hard_links: bool,
    pub(super) filter_rules: Vec<FilterRuleSpec>,
    pub(super) debug_flags: Vec<OsString>,
    pub(super) sparse: bool,
    pub(super) copy_links: bool,
    pub(super) copy_dirlinks: bool,
    pub(super) copy_unsafe_links: bool,
    pub(super) keep_dirlinks: bool,
    pub(super) safe_links: bool,
    pub(super) relative_paths: bool,
    pub(super) one_file_system: bool,
    pub(super) implied_dirs: bool,
    pub(super) mkpath: bool,
    pub(super) prune_empty_dirs: bool,
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
    pub(super) force_event_collection: bool,
    pub(super) preserve_devices: bool,
    pub(super) preserve_specials: bool,
    pub(super) list_only: bool,
    pub(super) address_mode: AddressMode,
    pub(super) timeout: TransferTimeout,
    pub(super) connect_timeout: TransferTimeout,
    pub(super) link_dest_paths: Vec<PathBuf>,
    pub(super) reference_directories: Vec<ReferenceDirectory>,
    pub(super) connect_program: Option<OsString>,
    pub(super) bind_address: Option<BindAddress>,
    #[cfg(feature = "acl")]
    pub(super) preserve_acls: bool,
    #[cfg(feature = "xattr")]
    pub(super) preserve_xattrs: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            transfer_args: Vec::new(),
            dry_run: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            max_delete: None,
            min_file_size: None,
            max_file_size: None,
            modify_window: None,
            remove_source_files: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            owner_override: None,
            group_override: None,
            chmod: None,
            omit_dir_times: false,
            omit_link_times: false,
            compress: false,
            compression_level: None,
            compression_setting: CompressionSetting::default(),
            skip_compress: SkipCompressList::default(),
            whole_file: true,
            checksum: false,
            checksum_choice: StrongChecksumChoice::default(),
            checksum_seed: None,
            size_only: false,
            ignore_existing: false,
            ignore_missing_args: false,
            update: false,
            numeric_ids: false,
            preallocate: false,
            preserve_hard_links: false,
            filter_rules: Vec::new(),
            debug_flags: Vec::new(),
            sparse: false,
            copy_links: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            relative_paths: false,
            one_file_system: false,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
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
            force_event_collection: false,
            preserve_devices: false,
            preserve_specials: false,
            list_only: false,
            address_mode: AddressMode::Default,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            link_dest_paths: Vec::new(),
            reference_directories: Vec::new(),
            connect_program: None,
            bind_address: None,
            #[cfg(feature = "acl")]
            preserve_acls: false,
            #[cfg(feature = "xattr")]
            preserve_xattrs: false,
        }
    }
}

impl ClientConfig {
    /// Creates a new [`ClientConfigBuilder`].
    #[must_use]
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
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
