use std::ffi::OsString;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ::metadata::{ChmodModifiers, GroupMapping, UserMapping};
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use filters::FilterSet;

use crate::batch::BatchWriter;
use crate::local_copy::filter_program::FilterProgram;
use crate::local_copy::skip_compress::SkipCompressList;
use crate::signature::SignatureAlgorithm;

/// Controls when deletion sweeps run relative to content transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteTiming {
    /// Remove extraneous entries before copying new content.
    Before,
    /// Remove extraneous entries as directories are processed.
    During,
    /// Record deletions during the walk and apply them after transfers finish.
    Delay,
    /// Remove extraneous entries after the full transfer completes.
    After,
}

impl DeleteTiming {
    pub(super) const fn default() -> Self {
        Self::During
    }
}

/// Identifies how a reference directory should be treated when evaluating
/// `--compare-dest`, `--copy-dest`, and `--link-dest` semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// Skip creating the destination entry when the reference file matches.
    Compare,
    /// Copy the payload from the reference directory when the file matches.
    Copy,
    /// Create a hard link to the reference directory when the file matches.
    Link,
}

/// Reference directory consulted during copy execution.
///
/// This represents a `--compare-dest`, `--copy-dest`, or `--link-dest` directory
/// used for basis file lookup during both local and remote transfers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    /// The kind of reference directory operation.
    pub kind: ReferenceDirectoryKind,
    /// The path to the reference directory.
    pub path: PathBuf,
}

impl ReferenceDirectory {
    /// Creates a new reference directory entry.
    #[must_use]
    pub fn new(kind: ReferenceDirectoryKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }

    /// Returns the reference directory kind.
    #[must_use]
    pub const fn kind(&self) -> ReferenceDirectoryKind {
        self.kind
    }

    /// Returns the base directory path associated with the entry.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Options that influence how a [`LocalCopyPlan`](crate::local_copy::LocalCopyPlan) is executed.
#[derive(Clone, Debug)]
pub struct LocalCopyOptions {
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
    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_executability: bool,
    pub(super) preserve_permissions: bool,
    pub(super) preserve_times: bool,
    pub(super) omit_link_times: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) omit_dir_times: bool,
    #[cfg(all(unix, feature = "acl"))]
    pub(super) preserve_acls: bool,
    pub(super) filters: Option<FilterSet>,
    pub(super) filter_program: Option<FilterProgram>,
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
    /// When `Some(true)`, the receiving side attempts super-user activities
    /// (preserving ownership, devices, specials) even when not running as root.
    /// When `Some(false)`, explicitly disables super-user attempts.
    /// When `None`, defers to the default (check effective UID).
    pub(super) super_mode: Option<bool>,
    /// When enabled, stores/restores privileged metadata via xattrs instead
    /// of actually requiring root privileges.
    pub(super) fake_super: bool,
    /// When enabled, `--delete` proceeds even when I/O errors occurred during
    /// the transfer. Without this flag, deletions are suppressed when any I/O
    /// errors are recorded, preventing data loss when the sender could not read
    /// all files.
    pub(super) ignore_errors: bool,
    /// Optional file path for logging transfer activity.
    pub(super) log_file: Option<PathBuf>,
    /// Optional format string for per-item log entries.
    pub(super) log_file_format: Option<String>,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
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
        }
    }
}

impl Default for LocalCopyOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LinkDestEntry {
    pub(super) path: PathBuf,
    pub(super) is_relative: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_timing_default_is_during() {
        assert_eq!(DeleteTiming::default(), DeleteTiming::During);
    }

    #[test]
    fn delete_timing_eq() {
        assert_eq!(DeleteTiming::Before, DeleteTiming::Before);
        assert_eq!(DeleteTiming::During, DeleteTiming::During);
        assert_eq!(DeleteTiming::Delay, DeleteTiming::Delay);
        assert_eq!(DeleteTiming::After, DeleteTiming::After);
        assert_ne!(DeleteTiming::Before, DeleteTiming::After);
    }

    #[test]
    fn reference_directory_kind_eq() {
        assert_eq!(
            ReferenceDirectoryKind::Compare,
            ReferenceDirectoryKind::Compare
        );
        assert_eq!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Copy);
        assert_eq!(ReferenceDirectoryKind::Link, ReferenceDirectoryKind::Link);
        assert_ne!(
            ReferenceDirectoryKind::Compare,
            ReferenceDirectoryKind::Link
        );
    }

    #[test]
    fn reference_directory_new_creates_entry() {
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/tmp/ref");
        assert_eq!(dir.kind(), ReferenceDirectoryKind::Compare);
        assert_eq!(dir.path().to_str().unwrap(), "/tmp/ref");
    }

    #[test]
    fn reference_directory_new_with_path_buf() {
        let path = PathBuf::from("/tmp/ref");
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Copy, path);
        assert_eq!(dir.kind(), ReferenceDirectoryKind::Copy);
    }

    #[test]
    fn reference_directory_clone() {
        let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Link, "/tmp/link");
        let cloned = dir.clone();
        assert_eq!(dir, cloned);
    }

    #[test]
    fn local_copy_options_new_has_defaults() {
        let opts = LocalCopyOptions::new();
        assert!(!opts.delete);
        assert_eq!(opts.delete_timing, DeleteTiming::During);
        assert!(!opts.delete_excluded);
        assert!(opts.max_deletions.is_none());
        assert!(!opts.preallocate);
        assert!(!opts.fsync);
        assert!(opts.whole_file.is_none());
        assert!(opts.recursive);
    }

    #[test]
    fn local_copy_options_default_same_as_new() {
        let new_opts = LocalCopyOptions::new();
        let default_opts = LocalCopyOptions::default();
        assert_eq!(new_opts.delete, default_opts.delete);
        assert_eq!(new_opts.recursive, default_opts.recursive);
    }

    #[test]
    fn local_copy_options_clone() {
        let opts = LocalCopyOptions::new();
        let cloned = opts.clone();
        assert_eq!(opts.delete, cloned.delete);
        assert_eq!(opts.recursive, cloned.recursive);
    }
}
