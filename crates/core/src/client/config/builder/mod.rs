use std::ffi::OsString;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::SystemTime;

/// Generates chainable builder setter methods.
///
/// This macro reduces boilerplate for simple field setters that follow the
/// pattern of assigning a value and returning `self`. Each generated method
/// is marked `#[must_use]` and declared as `pub const fn`.
///
/// # Examples
///
/// Generate a simple setter with doc comment:
///
/// ```ignore
/// builder_setter! {
///     /// Sets the recursive flag.
///     recursive: bool,
/// }
/// // Expands to:
/// // /// Sets the recursive flag.
/// // #[must_use]
/// // pub const fn recursive(mut self, value: bool) -> Self {
/// //     self.recursive = value;
/// //     self
/// // }
/// ```
///
/// Generate multiple setters at once:
///
/// ```ignore
/// builder_setter! {
///     /// Sets the minimum file size to transfer.
///     min_file_size: Option<u64>,
///     /// Sets the maximum file size to transfer.
///     max_file_size: Option<u64>,
/// }
/// ```
#[macro_export]
macro_rules! builder_setter {
    // Single field with doc comments and attributes
    ($(#[$attr:meta])* $field:ident: $ty:ty) => {
        $(#[$attr])*
        #[must_use]
        pub const fn $field(mut self, value: $ty) -> Self {
            self.$field = value;
            self
        }
    };
    // Multiple fields with doc comments
    ($($(#[$attr:meta])* $field:ident: $ty:ty),+ $(,)?) => {
        $(
            builder_setter!($(#[$attr])* $field: $ty);
        )+
    };
}

use super::{
    AddressMode, BandwidthLimit, BindAddress, ClientConfig, CompressionSetting, DeleteMode,
    FilterRuleSpec, IconvSetting, ReferenceDirectory, ReferenceDirectoryKind, StrongChecksumChoice,
    TransferTimeout,
};
use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use engine::SkipCompressList;

/// Builder used to assemble a [`ClientConfig`].
///
/// This type provides a fluent interface for constructing [`ClientConfig`] instances
/// with incremental configuration of transfer settings. Methods are chainable and
/// return `self` to allow multiple options to be set in a single expression.
///
/// Create a builder via [`ClientConfig::builder()`].
///
/// # Examples
///
/// ```ignore
/// use core::client::ClientConfig;
///
/// let config = ClientConfig::builder()
///     .transfer_args(["src/", "dest/"])
///     .recursive(true)
///     .preserve_times(true)
///     .preserve_permissions(true)
///     .dry_run(false)
///     .build();
/// ```
///
/// Compression and bandwidth limiting:
///
/// ```ignore
/// use core::client::{ClientConfig, BandwidthLimit, CompressionSetting};
/// use compress::zlib::CompressionLevel;
/// use std::num::NonZeroU64;
///
/// let config = ClientConfig::builder()
///     .transfer_args(["large_file", "backup/"])
///     .compression_setting(CompressionSetting::level(CompressionLevel::Default))
///     .bandwidth_limit(Some(BandwidthLimit::from_bytes_per_second(
///         NonZeroU64::new(1024 * 1024).unwrap()
///     )))
///     .build();
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    delete_missing_args: bool,
    ignore_errors: bool,
    max_delete: Option<u64>,
    recursive: bool,
    dirs: bool,
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,
    block_size_override: Option<NonZeroU32>,
    max_alloc: Option<u64>,
    modify_window: Option<u64>,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_executability: bool,
    preserve_permissions: bool,
    fake_super: bool,
    preserve_times: bool,
    preserve_atimes: bool,
    preserve_crtimes: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    copy_as: Option<OsString>,
    chmod: Option<ChmodModifiers>,
    user_mapping: Option<UserMapping>,
    group_mapping: Option<GroupMapping>,
    omit_dir_times: bool,
    omit_link_times: bool,
    compress: bool,
    compression_algorithm: CompressionAlgorithm,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    skip_compress: SkipCompressList,
    open_noatime: bool,
    whole_file: Option<bool>,
    checksum: bool,
    checksum_choice: StrongChecksumChoice,
    checksum_seed: Option<u32>,
    size_only: bool,
    ignore_times: bool,
    ignore_existing: bool,
    existing_only: bool,
    ignore_missing_args: bool,
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    fsync: bool,
    preserve_hard_links: bool,
    preserve_symlinks: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    fuzzy: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,
    munge_links: bool,
    copy_devices: bool,
    write_devices: bool,
    relative_paths: bool,
    one_file_system: bool,
    implied_dirs: Option<bool>,
    mkpath: bool,
    prune_empty_dirs: bool,
    qsort: bool,
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
    force_replacements: bool,
    force_event_collection: bool,
    force_fallback: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    address_mode: AddressMode,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
    stop_deadline: Option<SystemTime>,
    link_dest_paths: Vec<PathBuf>,
    reference_directories: Vec<ReferenceDirectory>,
    connect_program: Option<OsString>,
    bind_address: Option<BindAddress>,
    sockopts: Option<OsString>,
    blocking_io: Option<bool>,
    iconv: IconvSetting,
    remote_shell: Option<Vec<OsString>>,
    rsync_path: Option<OsString>,
    early_input: Option<PathBuf>,
    batch_config: Option<engine::batch::BatchConfig>,
    no_motd: bool,
    #[cfg(all(unix, feature = "acl"))]
    preserve_acls: bool,
    #[cfg(all(unix, feature = "xattr"))]
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
            delete_missing_args: self.delete_missing_args,
            ignore_errors: self.ignore_errors,
            max_delete: self.max_delete,
            recursive: self.recursive,
            dirs: self.dirs,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            block_size_override: self.block_size_override,
            max_alloc: self.max_alloc,
            modify_window: self.modify_window,
            remove_source_files: self.remove_source_files,
            bandwidth_limit: self.bandwidth_limit,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_executability: self.preserve_executability,
            preserve_permissions: self.preserve_permissions,
            fake_super: self.fake_super,
            preserve_times: self.preserve_times,
            preserve_atimes: self.preserve_atimes,
            preserve_crtimes: self.preserve_crtimes,
            owner_override: self.owner_override,
            group_override: self.group_override,
            copy_as: self.copy_as,
            chmod: self.chmod,
            user_mapping: self.user_mapping,
            group_mapping: self.group_mapping,
            omit_dir_times: self.omit_dir_times,
            omit_link_times: self.omit_link_times,
            compress: self.compress,
            compression_algorithm: self.compression_algorithm,
            compression_level: self.compression_level,
            compression_setting: self.compression_setting,
            skip_compress: self.skip_compress,
            open_noatime: self.open_noatime,
            whole_file: self.whole_file.unwrap_or(true),
            checksum: self.checksum,
            checksum_choice: self.checksum_choice,
            checksum_seed: self.checksum_seed,
            size_only: self.size_only,
            ignore_times: self.ignore_times,
            ignore_existing: self.ignore_existing,
            existing_only: self.existing_only,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            numeric_ids: self.numeric_ids,
            preallocate: self.preallocate,
            fsync: self.fsync,
            preserve_hard_links: self.preserve_hard_links,
            preserve_symlinks: self.preserve_symlinks,
            filter_rules: self.filter_rules,
            debug_flags: self.debug_flags,
            sparse: self.sparse,
            fuzzy: self.fuzzy,
            copy_links: self.copy_links,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            munge_links: self.munge_links,
            copy_devices: self.copy_devices,
            write_devices: self.write_devices,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            implied_dirs: self.implied_dirs.unwrap_or(true),
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            qsort: self.qsort,
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
            force_replacements: self.force_replacements,
            force_event_collection: self.force_event_collection,
            force_fallback: self.force_fallback,
            preserve_devices: self.preserve_devices,
            preserve_specials: self.preserve_specials,
            list_only: self.list_only,
            address_mode: self.address_mode,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            stop_at: self.stop_deadline,
            link_dest_paths: self.link_dest_paths,
            reference_directories: self.reference_directories,
            connect_program: self.connect_program,
            bind_address: self.bind_address,
            sockopts: self.sockopts,
            blocking_io: self.blocking_io,
            iconv: self.iconv,
            remote_shell: self.remote_shell,
            rsync_path: self.rsync_path,
            early_input: self.early_input,
            batch_config: self.batch_config,
            no_motd: self.no_motd,
            #[cfg(all(unix, feature = "acl"))]
            preserve_acls: self.preserve_acls,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs: self.preserve_xattrs,
        }
    }
}

mod arguments;
mod deletion;
mod fallback;
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
