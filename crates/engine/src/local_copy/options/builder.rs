//! Builder pattern for constructing [`LocalCopyOptions`].
//!
//! This module provides [`LocalCopyOptionsBuilder`], a fluent API for constructing
//! [`LocalCopyOptions`] with validation at build time.
//!
//! # Example
//!
//! ```rust
//! use engine::local_copy::LocalCopyOptions;
//!
//! let options = LocalCopyOptions::builder()
//!     .recursive(true)
//!     .preserve_times(true)
//!     .preserve_permissions(true)
//!     .delete(true)
//!     .build()
//!     .expect("valid options");
//! ```

use std::ffi::OsString;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use filters::FilterSet;

use super::types::{DeleteTiming, LinkDestEntry, LocalCopyOptions, ReferenceDirectory};
use crate::batch::BatchWriter;
use crate::local_copy::filter_program::FilterProgram;
use crate::local_copy::skip_compress::SkipCompressList;
use crate::signature::SignatureAlgorithm;

/// Errors that can occur when building [`LocalCopyOptions`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuilderError {
    /// Conflicting options were specified.
    ConflictingOptions {
        /// Description of the first conflicting option.
        option1: &'static str,
        /// Description of the second conflicting option.
        option2: &'static str,
    },
    /// An invalid combination of options was specified.
    InvalidCombination {
        /// Description of the invalid combination.
        message: String,
    },
    /// A required option is missing.
    MissingRequiredOption {
        /// Name of the missing option.
        option: &'static str,
    },
    /// An option value is out of range.
    ValueOutOfRange {
        /// Name of the option with invalid value.
        option: &'static str,
        /// Description of the valid range.
        range: String,
    },
}

impl std::fmt::Display for BuilderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConflictingOptions { option1, option2 } => {
                write!(f, "conflicting options: {option1} and {option2}")
            }
            Self::InvalidCombination { message } => {
                write!(f, "invalid option combination: {message}")
            }
            Self::MissingRequiredOption { option } => {
                write!(f, "missing required option: {option}")
            }
            Self::ValueOutOfRange { option, range } => {
                write!(f, "value out of range for {option}: expected {range}")
            }
        }
    }
}

impl std::error::Error for BuilderError {}

/// Builder for constructing [`LocalCopyOptions`] with validation.
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
    // Deletion options
    delete: bool,
    delete_timing: DeleteTiming,
    delete_excluded: bool,
    delete_missing_args: bool,
    max_deletions: Option<u64>,

    // Size limits
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,

    // Transfer options
    block_size_override: Option<NonZeroU32>,
    remove_source_files: bool,
    preallocate: bool,
    fsync: bool,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,

    // Compression options
    compress: bool,
    compression_algorithm: CompressionAlgorithm,
    compression_level_override: Option<CompressionLevel>,
    compression_level: CompressionLevel,
    skip_compress: SkipCompressList,

    // Path behavior options
    open_noatime: bool,
    whole_file: bool,
    copy_links: bool,
    preserve_symlinks: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,

    // Metadata preservation options
    preserve_owner: bool,
    preserve_group: bool,
    preserve_executability: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    omit_link_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    omit_dir_times: bool,
    #[cfg(all(unix, feature = "acl"))]
    preserve_acls: bool,

    // Filter options
    filters: Option<FilterSet>,
    filter_program: Option<FilterProgram>,

    // Integrity options
    numeric_ids: bool,
    sparse: bool,
    checksum: bool,
    checksum_algorithm: SignatureAlgorithm,
    size_only: bool,
    ignore_times: bool,
    ignore_existing: bool,
    existing_only: bool,
    ignore_missing_args: bool,
    update: bool,
    modify_window: Duration,

    // Staging options
    partial: bool,
    partial_dir: Option<PathBuf>,
    temp_dir: Option<PathBuf>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    collect_events: bool,

    // Link options
    preserve_hard_links: bool,

    // Path options
    relative_paths: bool,
    one_file_system: bool,
    recursive: bool,
    dirs: bool,
    devices: bool,
    copy_devices_as_files: bool,
    specials: bool,
    force_replacements: bool,
    implied_dirs: bool,
    mkpath: bool,
    prune_empty_dirs: bool,

    // Timeout options
    timeout: Option<Duration>,
    stop_at: Option<SystemTime>,

    // Extended attributes
    #[cfg(all(unix, feature = "xattr"))]
    preserve_xattrs: bool,
    #[cfg(all(unix, feature = "xattr"))]
    preserve_nfsv4_acls: bool,

    // Backup options
    backup: bool,
    backup_dir: Option<PathBuf>,
    backup_suffix: OsString,

    // Reference directories
    link_dests: Vec<LinkDestEntry>,
    reference_directories: Vec<ReferenceDirectory>,

    // Metadata modifiers
    chmod: Option<ChmodModifiers>,
    user_mapping: Option<UserMapping>,
    group_mapping: Option<GroupMapping>,

    // Batch mode
    batch_writer: Option<Arc<Mutex<BatchWriter>>>,
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
            whole_file: true,
            copy_links: false,
            preserve_symlinks: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            preserve_owner: false,
            preserve_group: false,
            preserve_executability: false,
            preserve_permissions: false,
            preserve_times: false,
            owner_override: None,
            group_override: None,
            omit_dir_times: false,
            omit_link_times: false,
            #[cfg(all(unix, feature = "acl"))]
            preserve_acls: false,
            filters: None,
            filter_program: None,
            numeric_ids: false,
            sparse: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::none(),
            },
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
            one_file_system: false,
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
        }
    }

    // ==================== Presets ====================

    /// Applies the archive preset equivalent to rsync's `-a` flag.
    ///
    /// This enables:
    /// - Recursive traversal
    /// - Symlink preservation
    /// - Permission preservation
    /// - Timestamp preservation
    /// - Group preservation
    /// - Owner preservation
    /// - Device preservation
    /// - Special file preservation
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
    /// This enables:
    /// - Archive preset
    /// - Delete extraneous files
    #[must_use]
    pub fn sync(self) -> Self {
        self.archive().delete(true)
    }

    /// Applies settings optimized for backup operations.
    ///
    /// This enables:
    /// - Archive preset
    /// - Hard link preservation
    /// - Partial file handling
    #[must_use]
    pub fn backup_preset(self) -> Self {
        self.archive().hard_links(true).partial(true)
    }

    // ==================== Deletion Options ====================

    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    pub fn delete(mut self, enabled: bool) -> Self {
        self.delete = enabled;
        if enabled {
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Sets the timing for deletion operations.
    #[must_use]
    pub fn delete_timing(mut self, timing: DeleteTiming) -> Self {
        self.delete_timing = timing;
        if !matches!(timing, DeleteTiming::During) {
            self.delete = true;
        }
        self
    }

    /// Enables deletion before transfer.
    #[must_use]
    pub fn delete_before(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::Before;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Before) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables deletion after transfer.
    #[must_use]
    pub fn delete_after(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::After;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::After) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables delayed deletion.
    #[must_use]
    pub fn delete_delay(mut self, enabled: bool) -> Self {
        if enabled {
            self.delete = true;
            self.delete_timing = DeleteTiming::Delay;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Delay) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Enables deletion during transfer.
    #[must_use]
    pub fn delete_during(mut self) -> Self {
        self.delete = true;
        self.delete_timing = DeleteTiming::During;
        self
    }

    /// Enables deletion of excluded files.
    #[must_use]
    pub fn delete_excluded(mut self, enabled: bool) -> Self {
        self.delete_excluded = enabled;
        self
    }

    /// Enables deletion of files corresponding to missing source arguments.
    #[must_use]
    pub fn delete_missing_args(mut self, enabled: bool) -> Self {
        self.delete_missing_args = enabled;
        self
    }

    /// Sets the maximum number of deletions allowed.
    #[must_use]
    pub fn max_deletions(mut self, limit: Option<u64>) -> Self {
        self.max_deletions = limit;
        self
    }

    // ==================== Size Limits ====================

    /// Sets the minimum file size for transfers.
    #[must_use]
    pub fn min_file_size(mut self, size: Option<u64>) -> Self {
        self.min_file_size = size;
        self
    }

    /// Sets the maximum file size for transfers.
    #[must_use]
    pub fn max_file_size(mut self, size: Option<u64>) -> Self {
        self.max_file_size = size;
        self
    }

    // ==================== Transfer Options ====================

    /// Sets the block size override for delta transfers.
    #[must_use]
    pub fn block_size(mut self, size: Option<NonZeroU32>) -> Self {
        self.block_size_override = size;
        self
    }

    /// Enables removal of source files after successful transfer.
    #[must_use]
    pub fn remove_source_files(mut self, enabled: bool) -> Self {
        self.remove_source_files = enabled;
        self
    }

    /// Enables preallocation of destination files.
    #[must_use]
    pub fn preallocate(mut self, enabled: bool) -> Self {
        self.preallocate = enabled;
        self
    }

    /// Enables fsync after file writes.
    #[must_use]
    pub fn fsync(mut self, enabled: bool) -> Self {
        self.fsync = enabled;
        self
    }

    /// Sets the bandwidth limit in bytes per second.
    #[must_use]
    pub fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Sets the bandwidth burst limit in bytes.
    #[must_use]
    pub fn bandwidth_burst(mut self, burst: Option<NonZeroU64>) -> Self {
        self.bandwidth_burst = burst;
        self
    }

    // ==================== Compression Options ====================

    /// Enables or disables compression.
    #[must_use]
    pub fn compress(mut self, enabled: bool) -> Self {
        self.compress = enabled;
        if !enabled {
            self.compression_level_override = None;
        }
        self
    }

    /// Sets the compression algorithm.
    #[must_use]
    pub fn compression_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
        self.compression_algorithm = algorithm;
        self
    }

    /// Sets the compression level.
    #[must_use]
    pub fn compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level = level;
        self
    }

    /// Sets the compression level override.
    #[must_use]
    pub fn compression_level_override(mut self, level: Option<CompressionLevel>) -> Self {
        self.compression_level_override = level;
        self
    }

    /// Sets the skip-compress list for file suffixes.
    #[must_use]
    pub fn skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }

    // ==================== Path Behavior Options ====================

    /// Enables opening files without updating access time.
    #[must_use]
    pub fn open_noatime(mut self, enabled: bool) -> Self {
        self.open_noatime = enabled;
        self
    }

    /// Enables whole-file transfer mode.
    #[must_use]
    pub fn whole_file(mut self, enabled: bool) -> Self {
        self.whole_file = enabled;
        self
    }

    /// Enables copying symlinks as their targets.
    #[must_use]
    pub fn copy_links(mut self, enabled: bool) -> Self {
        self.copy_links = enabled;
        self
    }

    /// Enables preserving symlinks as symlinks.
    #[must_use]
    pub fn preserve_symlinks(mut self, enabled: bool) -> Self {
        self.preserve_symlinks = enabled;
        self
    }

    /// Alias for `preserve_symlinks` for rsync compatibility.
    #[must_use]
    pub fn links(mut self, enabled: bool) -> Self {
        self.preserve_symlinks = enabled;
        self
    }

    /// Enables copying directory symlinks as directories.
    #[must_use]
    pub fn copy_dirlinks(mut self, enabled: bool) -> Self {
        self.copy_dirlinks = enabled;
        self
    }

    /// Enables copying unsafe symlinks.
    #[must_use]
    pub fn copy_unsafe_links(mut self, enabled: bool) -> Self {
        self.copy_unsafe_links = enabled;
        self
    }

    /// Enables keeping existing directory symlinks.
    #[must_use]
    pub fn keep_dirlinks(mut self, enabled: bool) -> Self {
        self.keep_dirlinks = enabled;
        self
    }

    /// Enables safe link mode.
    #[must_use]
    pub fn safe_links(mut self, enabled: bool) -> Self {
        self.safe_links = enabled;
        self
    }

    // ==================== Metadata Preservation Options ====================

    /// Enables owner preservation.
    #[must_use]
    pub fn preserve_owner(mut self, enabled: bool) -> Self {
        self.preserve_owner = enabled;
        self
    }

    /// Alias for `preserve_owner` for rsync compatibility.
    #[must_use]
    pub fn owner(mut self, enabled: bool) -> Self {
        self.preserve_owner = enabled;
        self
    }

    /// Enables group preservation.
    #[must_use]
    pub fn preserve_group(mut self, enabled: bool) -> Self {
        self.preserve_group = enabled;
        self
    }

    /// Alias for `preserve_group` for rsync compatibility.
    #[must_use]
    pub fn group(mut self, enabled: bool) -> Self {
        self.preserve_group = enabled;
        self
    }

    /// Enables executability preservation.
    #[must_use]
    pub fn preserve_executability(mut self, enabled: bool) -> Self {
        self.preserve_executability = enabled;
        self
    }

    /// Alias for `preserve_executability` for rsync compatibility.
    #[must_use]
    pub fn executability(mut self, enabled: bool) -> Self {
        self.preserve_executability = enabled;
        self
    }

    /// Enables permission preservation.
    #[must_use]
    pub fn preserve_permissions(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Alias for `preserve_permissions` for rsync compatibility.
    #[must_use]
    pub fn permissions(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Alias for `preserve_permissions` for rsync compatibility.
    #[must_use]
    pub fn perms(mut self, enabled: bool) -> Self {
        self.preserve_permissions = enabled;
        self
    }

    /// Enables timestamp preservation.
    #[must_use]
    pub fn preserve_times(mut self, enabled: bool) -> Self {
        self.preserve_times = enabled;
        self
    }

    /// Alias for `preserve_times` for rsync compatibility.
    #[must_use]
    pub fn times(mut self, enabled: bool) -> Self {
        self.preserve_times = enabled;
        self
    }

    /// Enables omitting link times from preservation.
    #[must_use]
    pub fn omit_link_times(mut self, enabled: bool) -> Self {
        self.omit_link_times = enabled;
        self
    }

    /// Sets the owner override.
    #[must_use]
    pub fn owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Sets the group override.
    #[must_use]
    pub fn group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Enables omitting directory times from preservation.
    #[must_use]
    pub fn omit_dir_times(mut self, enabled: bool) -> Self {
        self.omit_dir_times = enabled;
        self
    }

    /// Enables ACL preservation.
    #[cfg(all(unix, feature = "acl"))]
    #[must_use]
    pub fn preserve_acls(mut self, enabled: bool) -> Self {
        self.preserve_acls = enabled;
        self
    }

    /// Alias for `preserve_acls` for rsync compatibility.
    #[cfg(all(unix, feature = "acl"))]
    #[must_use]
    pub fn acls(mut self, enabled: bool) -> Self {
        self.preserve_acls = enabled;
        self
    }

    // ==================== Filter Options ====================

    /// Sets the filter set.
    #[must_use]
    pub fn filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Sets the filter program.
    #[must_use]
    pub fn filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
        self
    }

    // ==================== Integrity Options ====================

    /// Enables numeric ID handling.
    #[must_use]
    pub fn numeric_ids(mut self, enabled: bool) -> Self {
        self.numeric_ids = enabled;
        self
    }

    /// Enables sparse file handling.
    #[must_use]
    pub fn sparse(mut self, enabled: bool) -> Self {
        self.sparse = enabled;
        self
    }

    /// Enables checksum-based comparison.
    #[must_use]
    pub fn checksum(mut self, enabled: bool) -> Self {
        self.checksum = enabled;
        self
    }

    /// Sets the checksum algorithm.
    #[must_use]
    pub fn checksum_algorithm(mut self, algorithm: SignatureAlgorithm) -> Self {
        self.checksum_algorithm = algorithm;
        self
    }

    /// Enables size-only comparison.
    #[must_use]
    pub fn size_only(mut self, enabled: bool) -> Self {
        self.size_only = enabled;
        self
    }

    /// Enables ignore-times mode.
    #[must_use]
    pub fn ignore_times(mut self, enabled: bool) -> Self {
        self.ignore_times = enabled;
        self
    }

    /// Enables ignore-existing mode.
    #[must_use]
    pub fn ignore_existing(mut self, enabled: bool) -> Self {
        self.ignore_existing = enabled;
        self
    }

    /// Enables existing-only mode.
    #[must_use]
    pub fn existing_only(mut self, enabled: bool) -> Self {
        self.existing_only = enabled;
        self
    }

    /// Enables ignore-missing-args mode.
    #[must_use]
    pub fn ignore_missing_args(mut self, enabled: bool) -> Self {
        self.ignore_missing_args = enabled;
        self
    }

    /// Enables update mode.
    #[must_use]
    pub fn update(mut self, enabled: bool) -> Self {
        self.update = enabled;
        self
    }

    /// Sets the modification time window.
    #[must_use]
    pub fn modify_window(mut self, window: Duration) -> Self {
        self.modify_window = window;
        self
    }

    // ==================== Staging Options ====================

    /// Enables partial file handling.
    #[must_use]
    pub fn partial(mut self, enabled: bool) -> Self {
        self.partial = enabled;
        self
    }

    /// Sets the partial directory.
    #[must_use]
    pub fn partial_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.partial_dir = dir.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Sets the temp directory.
    #[must_use]
    pub fn temp_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.temp_dir = dir.map(Into::into);
        self
    }

    /// Enables delay-updates mode.
    #[must_use]
    pub fn delay_updates(mut self, enabled: bool) -> Self {
        self.delay_updates = enabled;
        if enabled {
            self.partial = true;
        }
        self
    }

    /// Enables inplace mode.
    #[must_use]
    pub fn inplace(mut self, enabled: bool) -> Self {
        self.inplace = enabled;
        self
    }

    /// Enables append mode.
    #[must_use]
    pub fn append(mut self, enabled: bool) -> Self {
        self.append = enabled;
        if !enabled {
            self.append_verify = false;
        }
        self
    }

    /// Enables append-verify mode.
    #[must_use]
    pub fn append_verify(mut self, enabled: bool) -> Self {
        if enabled {
            self.append = true;
            self.append_verify = true;
        } else {
            self.append_verify = false;
        }
        self
    }

    /// Enables event collection.
    #[must_use]
    pub fn collect_events(mut self, enabled: bool) -> Self {
        self.collect_events = enabled;
        self
    }

    // ==================== Link Options ====================

    /// Enables hard link preservation.
    #[must_use]
    pub fn hard_links(mut self, enabled: bool) -> Self {
        self.preserve_hard_links = enabled;
        self
    }

    /// Adds a link-dest directory.
    #[must_use]
    pub fn link_dest<P: Into<PathBuf>>(mut self, path: P) -> Self {
        let path = path.into();
        if !path.as_os_str().is_empty() {
            self.link_dests.push(LinkDestEntry::new(path));
        }
        self
    }

    /// Extends link-dest directories.
    #[must_use]
    pub fn link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        for path in paths {
            let path = path.into();
            if !path.as_os_str().is_empty() {
                self.link_dests.push(LinkDestEntry::new(path));
            }
        }
        self
    }

    /// Adds a reference directory.
    #[must_use]
    pub fn reference_directory(mut self, reference: ReferenceDirectory) -> Self {
        self.reference_directories.push(reference);
        self
    }

    /// Extends reference directories.
    #[must_use]
    pub fn reference_directories<I>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = ReferenceDirectory>,
    {
        self.reference_directories.extend(references);
        self
    }

    // ==================== Path Options ====================

    /// Enables relative path handling.
    #[must_use]
    pub fn relative_paths(mut self, enabled: bool) -> Self {
        self.relative_paths = enabled;
        self
    }

    /// Enables one-file-system mode.
    #[must_use]
    pub fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = enabled;
        self
    }

    /// Enables recursive mode.
    #[must_use]
    pub fn recursive(mut self, enabled: bool) -> Self {
        self.recursive = enabled;
        self
    }

    /// Enables dirs mode.
    #[must_use]
    pub fn dirs(mut self, enabled: bool) -> Self {
        self.dirs = enabled;
        self
    }

    /// Enables device handling.
    #[must_use]
    pub fn devices(mut self, enabled: bool) -> Self {
        self.devices = enabled;
        self
    }

    /// Enables copying devices as files.
    #[must_use]
    pub fn copy_devices_as_files(mut self, enabled: bool) -> Self {
        self.copy_devices_as_files = enabled;
        self
    }

    /// Enables special file handling.
    #[must_use]
    pub fn specials(mut self, enabled: bool) -> Self {
        self.specials = enabled;
        self
    }

    /// Enables force replacements.
    #[must_use]
    pub fn force_replacements(mut self, enabled: bool) -> Self {
        self.force_replacements = enabled;
        self
    }

    /// Enables implied directories.
    #[must_use]
    pub fn implied_dirs(mut self, enabled: bool) -> Self {
        self.implied_dirs = enabled;
        self
    }

    /// Enables mkpath mode.
    #[must_use]
    pub fn mkpath(mut self, enabled: bool) -> Self {
        self.mkpath = enabled;
        self
    }

    /// Enables prune-empty-dirs mode.
    #[must_use]
    pub fn prune_empty_dirs(mut self, enabled: bool) -> Self {
        self.prune_empty_dirs = enabled;
        self
    }

    // ==================== Timeout Options ====================

    /// Sets the timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the stop-at deadline.
    #[must_use]
    pub fn stop_at(mut self, deadline: Option<SystemTime>) -> Self {
        self.stop_at = deadline;
        self
    }

    // ==================== Extended Attributes ====================

    /// Enables extended attribute preservation.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn preserve_xattrs(mut self, enabled: bool) -> Self {
        self.preserve_xattrs = enabled;
        self
    }

    /// Alias for `preserve_xattrs` for rsync compatibility.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn xattrs(mut self, enabled: bool) -> Self {
        self.preserve_xattrs = enabled;
        self
    }

    /// Enables NFSv4 ACL preservation.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn preserve_nfsv4_acls(mut self, enabled: bool) -> Self {
        self.preserve_nfsv4_acls = enabled;
        self
    }

    /// Alias for `preserve_nfsv4_acls` for rsync compatibility.
    #[cfg(all(unix, feature = "xattr"))]
    #[must_use]
    pub fn nfsv4_acls(mut self, enabled: bool) -> Self {
        self.preserve_nfsv4_acls = enabled;
        self
    }

    // ==================== Backup Options ====================

    /// Enables backup mode.
    #[must_use]
    pub fn backup(mut self, enabled: bool) -> Self {
        self.backup = enabled;
        self
    }

    /// Sets the backup directory.
    #[must_use]
    pub fn backup_dir<P: Into<PathBuf>>(mut self, dir: Option<P>) -> Self {
        self.backup_dir = dir.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Sets the backup suffix.
    #[must_use]
    pub fn backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        match suffix {
            Some(s) => {
                self.backup_suffix = s.into();
                self.backup = true;
            }
            None => {
                self.backup_suffix = OsString::from("~");
            }
        }
        self
    }

    // ==================== Metadata Modifiers ====================

    /// Sets the chmod modifiers.
    #[must_use]
    pub fn chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Sets the user mapping.
    #[must_use]
    pub fn user_mapping(mut self, mapping: Option<UserMapping>) -> Self {
        self.user_mapping = mapping;
        self
    }

    /// Sets the group mapping.
    #[must_use]
    pub fn group_mapping(mut self, mapping: Option<GroupMapping>) -> Self {
        self.group_mapping = mapping;
        self
    }

    // ==================== Batch Mode ====================

    /// Sets the batch writer.
    #[must_use]
    pub fn batch_writer(mut self, writer: Option<Arc<Mutex<BatchWriter>>>) -> Self {
        self.batch_writer = writer;
        self
    }

    // ==================== Validation and Build ====================

    /// Validates the builder configuration and returns any errors.
    fn validate(&self) -> Result<(), BuilderError> {
        // Check for conflicting options

        // size_only and checksum are mutually exclusive
        if self.size_only && self.checksum {
            return Err(BuilderError::ConflictingOptions {
                option1: "size_only",
                option2: "checksum",
            });
        }

        // inplace and delay_updates are mutually exclusive
        if self.inplace && self.delay_updates {
            return Err(BuilderError::ConflictingOptions {
                option1: "inplace",
                option2: "delay_updates",
            });
        }

        // append requires inplace behavior semantically (rsync implements it this way)
        // But we don't flag this as an error since rsync allows it

        // ignore_existing and existing_only are conceptually opposite but not strictly conflicting
        // since ignore_existing skips updates and existing_only skips creates

        // Validate size limits
        if let (Some(min), Some(max)) = (self.min_file_size, self.max_file_size) {
            if min > max {
                return Err(BuilderError::InvalidCombination {
                    message: format!(
                        "min_file_size ({min}) cannot be greater than max_file_size ({max})"
                    ),
                });
            }
        }

        // copy_links and preserve_symlinks are mutually exclusive
        if self.copy_links && self.preserve_symlinks {
            return Err(BuilderError::ConflictingOptions {
                option1: "copy_links",
                option2: "preserve_symlinks",
            });
        }

        Ok(())
    }

    /// Builds the [`LocalCopyOptions`] with validation.
    ///
    /// # Errors
    ///
    /// Returns a [`BuilderError`] if the configuration is invalid.
    pub fn build(self) -> Result<LocalCopyOptions, BuilderError> {
        self.validate()?;

        Ok(LocalCopyOptions {
            delete: self.delete,
            delete_timing: self.delete_timing,
            delete_excluded: self.delete_excluded,
            delete_missing_args: self.delete_missing_args,
            max_deletions: self.max_deletions,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            block_size_override: self.block_size_override,
            remove_source_files: self.remove_source_files,
            preallocate: self.preallocate,
            fsync: self.fsync,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_burst: self.bandwidth_burst,
            compress: self.compress,
            compression_algorithm: self.compression_algorithm,
            compression_level_override: self.compression_level_override,
            compression_level: self.compression_level,
            skip_compress: self.skip_compress,
            open_noatime: self.open_noatime,
            whole_file: self.whole_file,
            copy_links: self.copy_links,
            preserve_symlinks: self.preserve_symlinks,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_executability: self.preserve_executability,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            omit_link_times: self.omit_link_times,
            owner_override: self.owner_override,
            group_override: self.group_override,
            omit_dir_times: self.omit_dir_times,
            #[cfg(all(unix, feature = "acl"))]
            preserve_acls: self.preserve_acls,
            filters: self.filters,
            filter_program: self.filter_program,
            numeric_ids: self.numeric_ids,
            sparse: self.sparse,
            checksum: self.checksum,
            checksum_algorithm: self.checksum_algorithm,
            size_only: self.size_only,
            ignore_times: self.ignore_times,
            ignore_existing: self.ignore_existing,
            existing_only: self.existing_only,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            modify_window: self.modify_window,
            partial: self.partial,
            partial_dir: self.partial_dir,
            temp_dir: self.temp_dir,
            delay_updates: self.delay_updates,
            inplace: self.inplace,
            append: self.append,
            append_verify: self.append_verify,
            collect_events: self.collect_events,
            preserve_hard_links: self.preserve_hard_links,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            recursive: self.recursive,
            dirs: self.dirs,
            devices: self.devices,
            copy_devices_as_files: self.copy_devices_as_files,
            specials: self.specials,
            force_replacements: self.force_replacements,
            implied_dirs: self.implied_dirs,
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            timeout: self.timeout,
            stop_at: self.stop_at,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs: self.preserve_xattrs,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_nfsv4_acls: self.preserve_nfsv4_acls,
            backup: self.backup,
            backup_dir: self.backup_dir,
            backup_suffix: self.backup_suffix,
            link_dests: self.link_dests,
            reference_directories: self.reference_directories,
            chmod: self.chmod,
            user_mapping: self.user_mapping,
            group_mapping: self.group_mapping,
            batch_writer: self.batch_writer,
        })
    }

    /// Builds the [`LocalCopyOptions`] without validation.
    ///
    /// This is useful when you know the configuration is valid or want
    /// to skip validation for performance reasons.
    #[must_use]
    pub fn build_unchecked(self) -> LocalCopyOptions {
        LocalCopyOptions {
            delete: self.delete,
            delete_timing: self.delete_timing,
            delete_excluded: self.delete_excluded,
            delete_missing_args: self.delete_missing_args,
            max_deletions: self.max_deletions,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            block_size_override: self.block_size_override,
            remove_source_files: self.remove_source_files,
            preallocate: self.preallocate,
            fsync: self.fsync,
            bandwidth_limit: self.bandwidth_limit,
            bandwidth_burst: self.bandwidth_burst,
            compress: self.compress,
            compression_algorithm: self.compression_algorithm,
            compression_level_override: self.compression_level_override,
            compression_level: self.compression_level,
            skip_compress: self.skip_compress,
            open_noatime: self.open_noatime,
            whole_file: self.whole_file,
            copy_links: self.copy_links,
            preserve_symlinks: self.preserve_symlinks,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_executability: self.preserve_executability,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            omit_link_times: self.omit_link_times,
            owner_override: self.owner_override,
            group_override: self.group_override,
            omit_dir_times: self.omit_dir_times,
            #[cfg(all(unix, feature = "acl"))]
            preserve_acls: self.preserve_acls,
            filters: self.filters,
            filter_program: self.filter_program,
            numeric_ids: self.numeric_ids,
            sparse: self.sparse,
            checksum: self.checksum,
            checksum_algorithm: self.checksum_algorithm,
            size_only: self.size_only,
            ignore_times: self.ignore_times,
            ignore_existing: self.ignore_existing,
            existing_only: self.existing_only,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            modify_window: self.modify_window,
            partial: self.partial,
            partial_dir: self.partial_dir,
            temp_dir: self.temp_dir,
            delay_updates: self.delay_updates,
            inplace: self.inplace,
            append: self.append,
            append_verify: self.append_verify,
            collect_events: self.collect_events,
            preserve_hard_links: self.preserve_hard_links,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            recursive: self.recursive,
            dirs: self.dirs,
            devices: self.devices,
            copy_devices_as_files: self.copy_devices_as_files,
            specials: self.specials,
            force_replacements: self.force_replacements,
            implied_dirs: self.implied_dirs,
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            timeout: self.timeout,
            stop_at: self.stop_at,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs: self.preserve_xattrs,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_nfsv4_acls: self.preserve_nfsv4_acls,
            backup: self.backup,
            backup_dir: self.backup_dir,
            backup_suffix: self.backup_suffix,
            link_dests: self.link_dests,
            reference_directories: self.reference_directories,
            chmod: self.chmod,
            user_mapping: self.user_mapping,
            group_mapping: self.group_mapping,
            batch_writer: self.batch_writer,
        }
    }
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptionsBuilder`] for constructing options.
    ///
    /// # Example
    ///
    /// ```rust
    /// use engine::local_copy::LocalCopyOptions;
    ///
    /// let options = LocalCopyOptions::builder()
    ///     .recursive(true)
    ///     .preserve_times(true)
    ///     .build()
    ///     .expect("valid options");
    /// ```
    #[must_use]
    pub fn builder() -> LocalCopyOptionsBuilder {
        LocalCopyOptionsBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod builder_creation {
        use super::*;

        #[test]
        fn new_creates_builder_with_defaults() {
            let builder = LocalCopyOptionsBuilder::new();
            let options = builder.build().expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.whole_file_enabled());
            assert!(options.implied_dirs_enabled());
            assert!(!options.delete_extraneous());
            assert!(!options.compress_enabled());
        }

        #[test]
        fn default_trait_matches_new() {
            let builder1 = LocalCopyOptionsBuilder::new();
            let builder2 = LocalCopyOptionsBuilder::default();

            let options1 = builder1.build().expect("valid options");
            let options2 = builder2.build().expect("valid options");

            assert_eq!(options1.recursive_enabled(), options2.recursive_enabled());
            assert_eq!(
                options1.delete_extraneous(),
                options2.delete_extraneous()
            );
        }

        #[test]
        fn builder_method_on_local_copy_options() {
            let options = LocalCopyOptions::builder().build().expect("valid options");
            assert!(options.recursive_enabled());
        }
    }

    mod presets {
        use super::*;

        #[test]
        fn archive_preset_enables_expected_options() {
            let options = LocalCopyOptionsBuilder::new()
                .archive()
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.links_enabled());
            assert!(options.preserve_permissions());
            assert!(options.preserve_times());
            assert!(options.preserve_group());
            assert!(options.preserve_owner());
            assert!(options.devices_enabled());
            assert!(options.specials_enabled());
        }

        #[test]
        fn sync_preset_enables_archive_and_delete() {
            let options = LocalCopyOptionsBuilder::new()
                .sync()
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.delete_extraneous());
        }

        #[test]
        fn backup_preset_enables_archive_and_extras() {
            let options = LocalCopyOptionsBuilder::new()
                .backup_preset()
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.hard_links_enabled());
            assert!(options.partial_enabled());
        }
    }

    mod deletion_options {
        use super::*;

        #[test]
        fn delete_enables_deletion() {
            let options = LocalCopyOptionsBuilder::new()
                .delete(true)
                .build()
                .expect("valid options");

            assert!(options.delete_extraneous());
            assert_eq!(options.delete_timing(), Some(DeleteTiming::During));
        }

        #[test]
        fn delete_before_sets_timing() {
            let options = LocalCopyOptionsBuilder::new()
                .delete_before(true)
                .build()
                .expect("valid options");

            assert!(options.delete_extraneous());
            assert_eq!(options.delete_timing(), Some(DeleteTiming::Before));
        }

        #[test]
        fn delete_after_sets_timing() {
            let options = LocalCopyOptionsBuilder::new()
                .delete_after(true)
                .build()
                .expect("valid options");

            assert!(options.delete_extraneous());
            assert_eq!(options.delete_timing(), Some(DeleteTiming::After));
        }

        #[test]
        fn delete_delay_sets_timing() {
            let options = LocalCopyOptionsBuilder::new()
                .delete_delay(true)
                .build()
                .expect("valid options");

            assert!(options.delete_extraneous());
            assert_eq!(options.delete_timing(), Some(DeleteTiming::Delay));
        }

        #[test]
        fn max_deletions_sets_limit() {
            let options = LocalCopyOptionsBuilder::new()
                .max_deletions(Some(100))
                .build()
                .expect("valid options");

            assert_eq!(options.max_deletion_limit(), Some(100));
        }
    }

    mod size_limits {
        use super::*;

        #[test]
        fn min_file_size_sets_limit() {
            let options = LocalCopyOptionsBuilder::new()
                .min_file_size(Some(1024))
                .build()
                .expect("valid options");

            assert_eq!(options.min_file_size_limit(), Some(1024));
        }

        #[test]
        fn max_file_size_sets_limit() {
            let options = LocalCopyOptionsBuilder::new()
                .max_file_size(Some(1_000_000))
                .build()
                .expect("valid options");

            assert_eq!(options.max_file_size_limit(), Some(1_000_000));
        }
    }

    mod transfer_options {
        use super::*;

        #[test]
        fn remove_source_files_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .remove_source_files(true)
                .build()
                .expect("valid options");

            assert!(options.remove_source_files_enabled());
        }

        #[test]
        fn preallocate_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .preallocate(true)
                .build()
                .expect("valid options");

            assert!(options.preallocate_enabled());
        }

        #[test]
        fn fsync_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .fsync(true)
                .build()
                .expect("valid options");

            assert!(options.fsync_enabled());
        }

        #[test]
        fn bandwidth_limit_sets_value() {
            let limit = NonZeroU64::new(1_000_000).unwrap();
            let options = LocalCopyOptionsBuilder::new()
                .bandwidth_limit(Some(limit))
                .build()
                .expect("valid options");

            assert_eq!(options.bandwidth_limit_bytes(), Some(limit));
        }
    }

    mod compression_options {
        use super::*;

        #[test]
        fn compress_enables_compression() {
            let options = LocalCopyOptionsBuilder::new()
                .compress(true)
                .build()
                .expect("valid options");

            assert!(options.compress_enabled());
        }

        #[test]
        fn compression_algorithm_sets_value() {
            let options = LocalCopyOptionsBuilder::new()
                .compression_algorithm(CompressionAlgorithm::Zstd)
                .build()
                .expect("valid options");

            assert_eq!(options.compression_algorithm(), CompressionAlgorithm::Zstd);
        }

        #[test]
        fn compression_level_sets_value() {
            let options = LocalCopyOptionsBuilder::new()
                .compression_level(CompressionLevel::Best)
                .build()
                .expect("valid options");

            assert_eq!(options.compression_level(), CompressionLevel::Best);
        }
    }

    mod metadata_options {
        use super::*;

        #[test]
        fn owner_preservation() {
            let options = LocalCopyOptionsBuilder::new()
                .preserve_owner(true)
                .build()
                .expect("valid options");

            assert!(options.preserve_owner());
        }

        #[test]
        fn group_preservation() {
            let options = LocalCopyOptionsBuilder::new()
                .preserve_group(true)
                .build()
                .expect("valid options");

            assert!(options.preserve_group());
        }

        #[test]
        fn permissions_preservation() {
            let options = LocalCopyOptionsBuilder::new()
                .preserve_permissions(true)
                .build()
                .expect("valid options");

            assert!(options.preserve_permissions());
        }

        #[test]
        fn times_preservation() {
            let options = LocalCopyOptionsBuilder::new()
                .preserve_times(true)
                .build()
                .expect("valid options");

            assert!(options.preserve_times());
        }

        #[test]
        fn alias_methods_work() {
            let options = LocalCopyOptionsBuilder::new()
                .owner(true)
                .group(true)
                .perms(true)
                .times(true)
                .build()
                .expect("valid options");

            assert!(options.preserve_owner());
            assert!(options.preserve_group());
            assert!(options.preserve_permissions());
            assert!(options.preserve_times());
        }
    }

    mod integrity_options {
        use super::*;

        #[test]
        fn checksum_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .checksum(true)
                .build()
                .expect("valid options");

            assert!(options.checksum_enabled());
        }

        #[test]
        fn size_only_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .size_only(true)
                .build()
                .expect("valid options");

            assert!(options.size_only_enabled());
        }

        #[test]
        fn ignore_times_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .ignore_times(true)
                .build()
                .expect("valid options");

            assert!(options.ignore_times_enabled());
        }

        #[test]
        fn modify_window_sets_value() {
            let window = Duration::from_secs(5);
            let options = LocalCopyOptionsBuilder::new()
                .modify_window(window)
                .build()
                .expect("valid options");

            assert_eq!(options.modify_window(), window);
        }
    }

    mod staging_options {
        use super::*;

        #[test]
        fn partial_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .partial(true)
                .build()
                .expect("valid options");

            assert!(options.partial_enabled());
        }

        #[test]
        fn partial_dir_enables_partial() {
            let options = LocalCopyOptionsBuilder::new()
                .partial_dir(Some("/tmp/partial"))
                .build()
                .expect("valid options");

            assert!(options.partial_enabled());
        }

        #[test]
        fn delay_updates_enables_partial() {
            let options = LocalCopyOptionsBuilder::new()
                .delay_updates(true)
                .build()
                .expect("valid options");

            assert!(options.delay_updates_enabled());
            assert!(options.partial_enabled());
        }

        #[test]
        fn inplace_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .inplace(true)
                .build()
                .expect("valid options");

            assert!(options.inplace_enabled());
        }

        #[test]
        fn append_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .append(true)
                .build()
                .expect("valid options");

            assert!(options.append_enabled());
        }

        #[test]
        fn append_verify_enables_append() {
            let options = LocalCopyOptionsBuilder::new()
                .append_verify(true)
                .build()
                .expect("valid options");

            assert!(options.append_enabled());
            assert!(options.append_verify_enabled());
        }
    }

    mod path_options {
        use super::*;

        #[test]
        fn recursive_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .recursive(true)
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
        }

        #[test]
        fn recursive_disables() {
            let options = LocalCopyOptionsBuilder::new()
                .recursive(false)
                .build()
                .expect("valid options");

            assert!(!options.recursive_enabled());
        }

        #[test]
        fn whole_file_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .whole_file(true)
                .build()
                .expect("valid options");

            assert!(options.whole_file_enabled());
        }

        #[test]
        fn copy_links_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .copy_links(true)
                .build()
                .expect("valid options");

            assert!(options.copy_links_enabled());
        }

        #[test]
        fn preserve_symlinks_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .preserve_symlinks(true)
                .build()
                .expect("valid options");

            assert!(options.links_enabled());
        }

        #[test]
        fn links_alias_works() {
            let options = LocalCopyOptionsBuilder::new()
                .links(true)
                .build()
                .expect("valid options");

            assert!(options.links_enabled());
        }

        #[test]
        fn one_file_system_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .one_file_system(true)
                .build()
                .expect("valid options");

            assert!(options.one_file_system_enabled());
        }
    }

    mod backup_options {
        use super::*;

        #[test]
        fn backup_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .backup(true)
                .build()
                .expect("valid options");

            assert!(options.backup_enabled());
        }

        #[test]
        fn backup_dir_enables_backup() {
            let options = LocalCopyOptionsBuilder::new()
                .backup_dir(Some("/tmp/backup"))
                .build()
                .expect("valid options");

            assert!(options.backup_enabled());
        }

        #[test]
        fn backup_suffix_enables_backup() {
            let options = LocalCopyOptionsBuilder::new()
                .backup_suffix(Some(".bak"))
                .build()
                .expect("valid options");

            assert!(options.backup_enabled());
        }
    }

    mod validation {
        use super::*;

        #[test]
        fn valid_configuration_passes() {
            let result = LocalCopyOptionsBuilder::new()
                .recursive(true)
                .preserve_times(true)
                .build();

            assert!(result.is_ok());
        }

        #[test]
        fn size_only_and_checksum_conflict() {
            let result = LocalCopyOptionsBuilder::new()
                .size_only(true)
                .checksum(true)
                .build();

            assert!(matches!(
                result,
                Err(BuilderError::ConflictingOptions {
                    option1: "size_only",
                    option2: "checksum"
                })
            ));
        }

        #[test]
        fn inplace_and_delay_updates_conflict() {
            let result = LocalCopyOptionsBuilder::new()
                .inplace(true)
                .delay_updates(true)
                .build();

            assert!(matches!(
                result,
                Err(BuilderError::ConflictingOptions {
                    option1: "inplace",
                    option2: "delay_updates"
                })
            ));
        }

        #[test]
        fn min_greater_than_max_file_size_fails() {
            let result = LocalCopyOptionsBuilder::new()
                .min_file_size(Some(1000))
                .max_file_size(Some(500))
                .build();

            assert!(matches!(result, Err(BuilderError::InvalidCombination { .. })));
        }

        #[test]
        fn copy_links_and_preserve_symlinks_conflict() {
            let result = LocalCopyOptionsBuilder::new()
                .copy_links(true)
                .preserve_symlinks(true)
                .build();

            assert!(matches!(
                result,
                Err(BuilderError::ConflictingOptions {
                    option1: "copy_links",
                    option2: "preserve_symlinks"
                })
            ));
        }

        #[test]
        fn build_unchecked_skips_validation() {
            // This would fail with build()
            let options = LocalCopyOptionsBuilder::new()
                .size_only(true)
                .checksum(true)
                .build_unchecked();

            assert!(options.size_only_enabled());
            assert!(options.checksum_enabled());
        }
    }

    mod builder_error {
        use super::*;

        #[test]
        fn conflicting_options_display() {
            let err = BuilderError::ConflictingOptions {
                option1: "foo",
                option2: "bar",
            };
            assert_eq!(err.to_string(), "conflicting options: foo and bar");
        }

        #[test]
        fn invalid_combination_display() {
            let err = BuilderError::InvalidCombination {
                message: "test message".to_string(),
            };
            assert_eq!(err.to_string(), "invalid option combination: test message");
        }

        #[test]
        fn missing_required_option_display() {
            let err = BuilderError::MissingRequiredOption { option: "test" };
            assert_eq!(err.to_string(), "missing required option: test");
        }

        #[test]
        fn value_out_of_range_display() {
            let err = BuilderError::ValueOutOfRange {
                option: "test",
                range: "0-100".to_string(),
            };
            assert_eq!(err.to_string(), "value out of range for test: expected 0-100");
        }

        #[test]
        fn builder_error_implements_error() {
            let err: Box<dyn std::error::Error> = Box::new(BuilderError::ConflictingOptions {
                option1: "a",
                option2: "b",
            });
            assert!(!err.to_string().is_empty());
        }
    }

    mod chaining {
        use super::*;

        #[test]
        fn multiple_options_can_be_chained() {
            let options = LocalCopyOptionsBuilder::new()
                .recursive(true)
                .preserve_times(true)
                .preserve_permissions(true)
                .delete(true)
                .compress(true)
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.preserve_times());
            assert!(options.preserve_permissions());
            assert!(options.delete_extraneous());
            assert!(options.compress_enabled());
        }

        #[test]
        fn preset_can_be_modified() {
            let options = LocalCopyOptionsBuilder::new()
                .archive()
                .delete(true)
                .compress(true)
                .build()
                .expect("valid options");

            assert!(options.recursive_enabled());
            assert!(options.delete_extraneous());
            assert!(options.compress_enabled());
        }
    }

    mod link_options {
        use super::*;

        #[test]
        fn hard_links_enables() {
            let options = LocalCopyOptionsBuilder::new()
                .hard_links(true)
                .build()
                .expect("valid options");

            assert!(options.hard_links_enabled());
        }

        #[test]
        fn link_dest_adds_entry() {
            let options = LocalCopyOptionsBuilder::new()
                .link_dest("/backup")
                .build()
                .expect("valid options");

            assert_eq!(options.link_dest_entries().len(), 1);
        }

        #[test]
        fn link_dests_adds_multiple() {
            let options = LocalCopyOptionsBuilder::new()
                .link_dests(["/backup1", "/backup2"])
                .build()
                .expect("valid options");

            assert_eq!(options.link_dest_entries().len(), 2);
        }
    }

    mod timeout_options {
        use super::*;

        #[test]
        fn timeout_sets_value() {
            let timeout = Duration::from_secs(60);
            let options = LocalCopyOptionsBuilder::new()
                .timeout(Some(timeout))
                .build()
                .expect("valid options");

            assert_eq!(options.timeout(), Some(timeout));
        }

        #[test]
        fn stop_at_sets_value() {
            let deadline = SystemTime::now();
            let options = LocalCopyOptionsBuilder::new()
                .stop_at(Some(deadline))
                .build()
                .expect("valid options");

            assert!(options.stop_at().is_some());
        }
    }
}
