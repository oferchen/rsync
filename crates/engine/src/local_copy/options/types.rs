use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use rsync_compress::zlib::CompressionLevel;
use rsync_filters::FilterSet;
use rsync_meta::ChmodModifiers;

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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    pub(super) kind: ReferenceDirectoryKind,
    pub(super) path: PathBuf,
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
    pub(super) max_deletions: Option<u64>,
    pub(super) min_file_size: Option<u64>,
    pub(super) max_file_size: Option<u64>,
    pub(super) remove_source_files: bool,
    pub(super) preallocate: bool,
    pub(super) bandwidth_limit: Option<NonZeroU64>,
    pub(super) bandwidth_burst: Option<NonZeroU64>,
    pub(super) compress: bool,
    pub(super) compression_level_override: Option<CompressionLevel>,
    pub(super) compression_level: CompressionLevel,
    pub(super) skip_compress: SkipCompressList,
    pub(super) whole_file: bool,
    pub(super) copy_links: bool,
    pub(super) copy_dirlinks: bool,
    pub(super) copy_unsafe_links: bool,
    pub(super) keep_dirlinks: bool,
    pub(super) safe_links: bool,
    pub(super) preserve_owner: bool,
    pub(super) preserve_group: bool,
    pub(super) preserve_permissions: bool,
    pub(super) preserve_times: bool,
    pub(super) omit_link_times: bool,
    pub(super) owner_override: Option<u32>,
    pub(super) group_override: Option<u32>,
    pub(super) omit_dir_times: bool,
    #[cfg(feature = "acl")]
    pub(super) preserve_acls: bool,
    pub(super) filters: Option<FilterSet>,
    pub(super) filter_program: Option<FilterProgram>,
    pub(super) numeric_ids: bool,
    pub(super) sparse: bool,
    pub(super) checksum: bool,
    pub(super) checksum_algorithm: SignatureAlgorithm,
    pub(super) size_only: bool,
    pub(super) ignore_existing: bool,
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
    pub(super) one_file_system: bool,
    pub(super) devices: bool,
    pub(super) specials: bool,
    pub(super) implied_dirs: bool,
    pub(super) mkpath: bool,
    pub(super) prune_empty_dirs: bool,
    pub(super) timeout: Option<Duration>,
    #[cfg(feature = "xattr")]
    pub(super) preserve_xattrs: bool,
    pub(super) backup: bool,
    pub(super) backup_dir: Option<PathBuf>,
    pub(super) backup_suffix: OsString,
    pub(super) link_dests: Vec<LinkDestEntry>,
    pub(super) reference_directories: Vec<ReferenceDirectory>,
    pub(super) chmod: Option<ChmodModifiers>,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
    #[must_use]
    pub fn new() -> Self {
        Self {
            delete: false,
            delete_timing: DeleteTiming::default(),
            delete_excluded: false,
            max_deletions: None,
            min_file_size: None,
            max_file_size: None,
            remove_source_files: false,
            preallocate: false,
            bandwidth_limit: None,
            bandwidth_burst: None,
            compress: false,
            compression_level_override: None,
            compression_level: CompressionLevel::Default,
            skip_compress: SkipCompressList::default(),
            whole_file: true,
            copy_links: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            owner_override: None,
            group_override: None,
            omit_dir_times: false,
            omit_link_times: false,
            #[cfg(feature = "acl")]
            preserve_acls: false,
            filters: None,
            filter_program: None,
            numeric_ids: false,
            sparse: false,
            checksum: false,
            checksum_algorithm: SignatureAlgorithm::Md5,
            size_only: false,
            ignore_existing: false,
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
            devices: false,
            specials: false,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
            timeout: None,
            #[cfg(feature = "xattr")]
            preserve_xattrs: false,
            backup: false,
            backup_dir: None,
            backup_suffix: OsString::from("~"),
            link_dests: Vec::new(),
            reference_directories: Vec::new(),
            chmod: None,
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
