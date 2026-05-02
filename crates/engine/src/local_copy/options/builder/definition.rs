//! Builder struct definition, constructor, and presets.

use std::ffi::OsString;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ::metadata::{ChmodModifiers, CopyAsIds, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use fast_io::{DefaultPlatformCopy, PlatformCopy};
use filters::FilterSet;
use protocol::iconv::FilenameConverter;

use crate::batch::BatchWriter;
use crate::local_copy::filter_program::FilterProgram;
use crate::local_copy::options::types::{DeleteTiming, LinkDestEntry, ReferenceDirectory};
use crate::local_copy::skip_compress::SkipCompressList;
use crate::signature::SignatureAlgorithm;

/// Builder for constructing [`LocalCopyOptions`](crate::local_copy::LocalCopyOptions) with validation.
///
/// # Example
///
/// ```rust
/// use engine::local_copy::LocalCopyOptions;
///
/// // Basic usage
/// let options = LocalCopyOptions::builder()
///     .recursive(true)
///     .build()
///     .expect("valid options");
///
/// // Archive mode preset
/// let archive_options = LocalCopyOptions::builder()
///     .archive()
///     .build()
///     .expect("valid archive options");
/// ```
#[derive(Clone, Debug)]
pub struct LocalCopyOptionsBuilder {
    pub(super) delete: bool,
    pub(super) delete_timing: DeleteTiming,
    pub(super) delete_excluded: bool,
    pub(super) delete_missing_args: bool,
    pub(super) max_deletions: Option<u64>,

    pub(super) min_file_size: Option<u64>,
    pub(super) max_file_size: Option<u64>,

    pub(super) block_size_override: Option<NonZeroU32>,
    pub(super) remove_source_files: bool,
    pub(super) preallocate: bool,
    pub(super) fsync: bool,
    pub(super) bandwidth_limit: Option<NonZeroU64>,
    pub(super) bandwidth_burst: Option<NonZeroU64>,

    pub(super) compress: bool,
    pub(super) compression_algorithm: CompressionAlgorithm,
    pub(super) compression_level_override: Option<CompressionLevel>,
    pub(super) compression_level: CompressionLevel,
    pub(super) skip_compress: SkipCompressList,

    pub(super) open_noatime: bool,
    pub(super) whole_file: Option<bool>,
    pub(super) copy_links: bool,
    pub(super) preserve_symlinks: bool,
    pub(super) copy_dirlinks: bool,
    pub(super) copy_unsafe_links: bool,
    pub(super) keep_dirlinks: bool,
    pub(super) safe_links: bool,
    pub(super) munge_links: bool,

    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_executability: bool,
    pub(super) preserve_permissions: bool,
    pub(super) preserve_times: bool,
    pub(super) preserve_atimes: bool,
    pub(super) preserve_crtimes: bool,
    pub(super) omit_link_times: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) copy_as: Option<CopyAsIds>,
    pub(super) omit_dir_times: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) preserve_acls: bool,

    pub(super) filters: Option<FilterSet>,
    pub(super) filter_program: Option<FilterProgram>,
    /// See [`LocalCopyOptions`](crate::local_copy::LocalCopyOptions)::iconv.
    pub(super) iconv: Option<FilenameConverter>,

    pub(super) numeric_ids: bool,
    pub(super) sparse: bool,
    pub(super) checksum: bool,
    pub(super) checksum_algorithm: SignatureAlgorithm,
    pub(super) checksum_seed: Option<u32>,
    pub(super) size_only: bool,
    pub(super) ignore_times: bool,
    pub(super) ignore_existing: bool,
    pub(super) existing_only: bool,
    pub(super) ignore_missing_args: bool,
    pub(super) update: bool,
    pub(super) modify_window: Duration,

    pub(super) partial: bool,
    pub(super) partial_dir: Option<PathBuf>,
    pub(super) temp_dir: Option<PathBuf>,
    pub(super) delay_updates: bool,
    pub(super) inplace: bool,
    pub(super) append: bool,
    pub(super) append_verify: bool,
    pub(super) collect_events: bool,

    pub(super) preserve_hard_links: bool,

    pub(super) relative_paths: bool,
    pub(super) one_file_system: u8,
    pub(super) recursive: bool,
    pub(super) dirs: bool,
    pub(super) devices: bool,
    pub(super) copy_devices_as_files: bool,
    pub(super) specials: bool,
    pub(super) force_replacements: bool,
    pub(super) implied_dirs: bool,
    pub(super) mkpath: bool,
    pub(super) prune_empty_dirs: bool,

    pub(super) timeout: Option<Duration>,
    pub(super) contimeout: Option<Duration>,
    pub(super) stop_at: Option<SystemTime>,

    #[cfg(all(unix, feature = "xattr"))]
    pub(super) preserve_xattrs: bool,
    #[cfg(all(unix, feature = "xattr"))]
    pub(super) preserve_nfsv4_acls: bool,

    pub(super) backup: bool,
    pub(super) backup_dir: Option<PathBuf>,
    pub(super) backup_suffix: OsString,

    pub(super) link_dests: Vec<LinkDestEntry>,
    pub(super) reference_directories: Vec<ReferenceDirectory>,

    pub(super) chmod: Option<ChmodModifiers>,
    pub(super) user_mapping: Option<UserMapping>,
    pub(super) group_mapping: Option<GroupMapping>,

    pub(super) batch_writer: Option<Arc<Mutex<BatchWriter>>>,

    pub(super) super_mode: Option<bool>,
    pub(super) fake_super: bool,

    pub(super) ignore_errors: bool,

    pub(super) log_file: Option<PathBuf>,
    pub(super) log_file_format: Option<String>,

    pub(super) platform_copy: Arc<dyn PlatformCopy>,
}

impl Default for LocalCopyOptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalCopyOptionsBuilder {
    /// Creates a new builder with default values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            delete: false,
            delete_timing: DeleteTiming::default(),
            delete_excluded: false,
            delete_missing_args: false,
            max_deletions: None,
            min_file_size: None,
            max_file_size: None,
            block_size_override: None,
            remove_source_files: false,
            preallocate: false,
            fsync: false,
            bandwidth_limit: None,
            bandwidth_burst: None,
            compress: false,
            compression_algorithm: CompressionAlgorithm::default_algorithm(),
            compression_level_override: None,
            compression_level: CompressionLevel::Default,
            skip_compress: SkipCompressList::default(),
            open_noatime: false,
            whole_file: None,
            copy_links: false,
            preserve_symlinks: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            munge_links: false,
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: false,
            preserve_times: false,
            preserve_atimes: false,
            preserve_crtimes: false,
            owner_override: None,
            group_override: None,
            copy_as: None,
            omit_dir_times: false,
            omit_link_times: false,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: false,
            filters: None,
            filter_program: None,
            iconv: None,
            numeric_ids: false,
            sparse: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::none(),
            },
            checksum_seed: None,
            size_only: false,
            ignore_times: false,
            ignore_existing: false,
            existing_only: false,
            ignore_missing_args: false,
            update: false,
            modify_window: Duration::ZERO,
            partial: false,
            partial_dir: None,
            temp_dir: None,
            delay_updates: false,
            inplace: false,
            append: false,
            append_verify: false,
            collect_events: false,
            preserve_hard_links: false,
            relative_paths: false,
            one_file_system: 0,
            recursive: true,
            dirs: false,
            devices: false,
            copy_devices_as_files: false,
            specials: false,
            force_replacements: false,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
            timeout: None,
            contimeout: None,
            stop_at: None,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs: false,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_nfsv4_acls: false,
            backup: false,
            backup_dir: None,
            backup_suffix: OsString::from("~"),
            link_dests: Vec::new(),
            reference_directories: Vec::new(),
            chmod: None,
            user_mapping: None,
            group_mapping: None,
            batch_writer: None,
            super_mode: None,
            fake_super: false,
            ignore_errors: false,
            log_file: None,
            log_file_format: None,
            platform_copy: Arc::new(DefaultPlatformCopy::new()),
        }
    }

    /// Applies the archive preset equivalent to rsync's `-a` flag.
    ///
    /// Enables recursive traversal, symlink preservation, permission/timestamp/group/owner
    /// preservation, and device/special file handling.
    #[must_use]
    pub fn archive(mut self) -> Self {
        self.recursive = true;
        self.preserve_symlinks = true;
        self.preserve_permissions = true;
        self.preserve_times = true;
        self.preserve_group = true;
        self.preserve_owner = true;
        self.devices = true;
        self.specials = true;
        self
    }

    /// Applies settings for a sync operation that mirrors source to destination.
    ///
    /// Enables archive preset plus deletion of extraneous files.
    #[must_use]
    pub fn sync(self) -> Self {
        self.archive().delete(true)
    }

    /// Applies settings optimized for backup operations.
    ///
    /// Enables archive preset plus hard link preservation and partial file handling.
    #[must_use]
    pub fn backup_preset(self) -> Self {
        self.archive().hard_links(true).partial(true)
    }
}
