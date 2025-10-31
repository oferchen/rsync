//! Configuration structures and helpers for local copy execution.

use std::ffi::{OsStr, OsString};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rsync_compress::zlib::CompressionLevel;
use rsync_filters::FilterSet;
use rsync_meta::ChmodModifiers;

use super::filter_program::FilterProgram;
use super::skip_compress::SkipCompressList;
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
    const fn default() -> Self {
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
    kind: ReferenceDirectoryKind,
    path: PathBuf,
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
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Options that influence how a [`LocalCopyPlan`] is executed.
#[derive(Clone, Debug)]
pub struct LocalCopyOptions {
    delete: bool,
    delete_timing: DeleteTiming,
    delete_excluded: bool,
    max_deletions: Option<u64>,
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,
    remove_source_files: bool,
    preallocate: bool,
    bandwidth_limit: Option<NonZeroU64>,
    bandwidth_burst: Option<NonZeroU64>,
    compress: bool,
    compression_level_override: Option<CompressionLevel>,
    compression_level: CompressionLevel,
    skip_compress: SkipCompressList,
    whole_file: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    omit_link_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    omit_dir_times: bool,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
    filters: Option<FilterSet>,
    filter_program: Option<FilterProgram>,
    numeric_ids: bool,
    sparse: bool,
    checksum: bool,
    checksum_algorithm: SignatureAlgorithm,
    size_only: bool,
    ignore_existing: bool,
    ignore_missing_args: bool,
    update: bool,
    modify_window: Duration,
    partial: bool,
    partial_dir: Option<PathBuf>,
    temp_dir: Option<PathBuf>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    collect_events: bool,
    preserve_hard_links: bool,
    relative_paths: bool,
    one_file_system: bool,
    devices: bool,
    specials: bool,
    implied_dirs: bool,
    mkpath: bool,
    prune_empty_dirs: bool,
    timeout: Option<Duration>,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
    backup: bool,
    backup_dir: Option<PathBuf>,
    backup_suffix: OsString,
    link_dests: Vec<LinkDestEntry>,
    reference_directories: Vec<ReferenceDirectory>,
    chmod: Option<ChmodModifiers>,
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

    /// Adds a directory that should be consulted when creating hard links for matching files.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn with_link_dest(mut self, path: PathBuf) -> Self {
        if !path.as_os_str().is_empty() {
            self.link_dests.push(LinkDestEntry::new(path));
        }
        self
    }

    /// Enables or disables creation of backups prior to overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    #[doc(alias = "-b")]
    pub const fn backup(mut self, enabled: bool) -> Self {
        self.backup = enabled;
        self
    }

    /// Configures the optional directory that should receive backup entries.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn with_backup_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.backup_dir = directory.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Overrides the suffix appended to backup file names.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn with_backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        match suffix {
            Some(value) => {
                self.backup_suffix = value.into();
                self.backup = true;
            }
            None => {
                self.backup_suffix = OsString::from("~");
            }
        }
        self
    }

    /// Extends the link-destination list with additional directories.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn extend_link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        for path in paths.into_iter() {
            let path = path.into();
            if !path.as_os_str().is_empty() {
                self.link_dests.push(LinkDestEntry::new(path));
            }
        }
        self
    }

    /// Enables or disables hard-link preservation for identical inodes.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Restricts traversal to a single filesystem when enabled.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = enabled;
        self
    }

    /// Returns `true` when the copy should remain on the source filesystem.
    #[must_use]
    pub const fn one_file_system_enabled(&self) -> bool {
        self.one_file_system
    }

    /// Returns `true` when hard-link preservation is enabled.
    #[must_use]
    pub const fn hard_links_enabled(&self) -> bool {
        self.preserve_hard_links
    }

    /// Configures chmod modifiers that should be applied after metadata preservation.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Returns the configured link-destination entries.
    #[must_use]
    pub(crate) fn link_dest_entries(&self) -> &[LinkDestEntry] {
        &self.link_dests
    }

    /// Requests that destination files absent from the source be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
        if delete {
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that extraneous destination files be removed after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete = true;
            self.delete_timing = DeleteTiming::After;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::After) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Queues deletions discovered during the walk and applies them after transfers finish.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete = true;
            self.delete_timing = DeleteTiming::Delay;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Delay) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete = true;
            self.delete_timing = DeleteTiming::Before;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Before) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed while processing directories.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        if self.delete {
            self.delete_timing = DeleteTiming::During;
        } else {
            self.delete = true;
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that excluded destination entries be removed during deletion sweeps.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Limits the number of deletions performed during a transfer.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_deletions(mut self, limit: Option<u64>) -> Self {
        self.max_deletions = limit;
        self
    }

    /// Applies a minimum size filter for regular files.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(mut self, limit: Option<u64>) -> Self {
        self.min_file_size = limit;
        self
    }

    /// Applies a maximum size filter for regular files.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(mut self, limit: Option<u64>) -> Self {
        self.max_file_size = limit;
        self
    }

    /// Requests that source files be removed after successful transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(mut self, remove: bool) -> Self {
        self.remove_source_files = remove;
        self
    }

    /// Requests that destination files be preallocated before writing begins.
    #[must_use]
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(mut self, preallocate: bool) -> Self {
        self.preallocate = preallocate;
        self
    }

    /// Applies an optional bandwidth limit expressed in bytes per second.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Applies an optional burst limit expressed in bytes per read.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_burst(mut self, burst: Option<NonZeroU64>) -> Self {
        self.bandwidth_burst = burst;
        self
    }

    /// Controls whether whole-file transfers are forced even when delta mode is requested.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(mut self, whole: bool) -> Self {
        self.whole_file = whole;
        self
    }

    /// Requests that symlinks be followed and copied as their referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(mut self, copy: bool) -> Self {
        self.copy_links = copy;
        self
    }

    /// Requests that unsafe symlinks be followed and copied as their referents.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(mut self, copy: bool) -> Self {
        self.copy_unsafe_links = copy;
        self
    }

    /// Skips symlinks whose targets escape the transfer root.
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(mut self, enabled: bool) -> Self {
        self.safe_links = enabled;
        self
    }

    /// Treats symlinks to directories as directories when traversing the source tree.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    pub const fn copy_dirlinks(mut self, copy: bool) -> Self {
        self.copy_dirlinks = copy;
        self
    }

    /// Keeps existing destination symlinks that point to directories.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(mut self, keep: bool) -> Self {
        self.keep_dirlinks = keep;
        self
    }

    /// Applies an inactivity timeout to the transfer.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Enables or disables compression during payload processing.
    #[must_use]
    #[doc(alias = "--compress")]
    pub const fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        if !compress {
            self.compression_level_override = None;
        }
        self
    }

    /// Applies an explicit compression level override for payload processing.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level_override(
        mut self,
        level: Option<CompressionLevel>,
    ) -> Self {
        self.compression_level_override = level;
        self
    }

    /// Sets the default compression level used when compression is enabled.
    #[must_use]
    pub const fn with_default_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level = level;
        self
    }

    /// Applies an explicit compression level override supplied by the user.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level_override = Some(level);
        self
    }

    /// Overrides the suffix list used to disable compression for specific files.
    #[must_use]
    pub fn with_skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }

    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Applies an explicit ownership override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Applies an explicit group override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Skips preserving directory modification times even when [`Self::times`] is enabled.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
        self
    }

    /// Controls whether symbolic link timestamps are preserved.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(mut self, omit: bool) -> Self {
        self.omit_link_times = omit;
        self
    }

    #[cfg(feature = "acl")]
    /// Requests that POSIX ACLs be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Applies a precompiled filter set to the execution.
    #[must_use]
    pub fn with_filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Applies a filter set using the legacy builder name for compatibility.
    #[must_use]
    pub fn filters(self, filters: Option<FilterSet>) -> Self {
        self.with_filters(filters)
    }

    /// Applies an external filter program configuration.
    #[must_use]
    pub fn with_filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
        self
    }

    /// Requests numeric UID/GID preservation.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Enables sparse file handling during copies.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Enables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }

    /// Selects the strong checksum algorithm used when verifying files.
    #[must_use]
    pub const fn with_checksum_algorithm(mut self, algorithm: SignatureAlgorithm) -> Self {
        self.checksum_algorithm = algorithm;
        self
    }

    /// Enables size-only change detection.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(mut self, size_only: bool) -> Self {
        self.size_only = size_only;
        self
    }

    /// Requests that existing destination files be skipped.
    #[must_use]
    #[doc(alias = "--ignore-existing")]
    pub const fn ignore_existing(mut self, ignore: bool) -> Self {
        self.ignore_existing = ignore;
        self
    }

    /// Requests that missing source arguments be ignored instead of causing an error.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(mut self, ignore: bool) -> Self {
        self.ignore_missing_args = ignore;
        self
    }

    /// Requests that newer destination files be preserved.
    #[must_use]
    #[doc(alias = "--update")]
    pub const fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Applies the modification time tolerance used when comparing files.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn with_modify_window(mut self, window: Duration) -> Self {
        self.modify_window = window;
        self
    }

    /// Requests that partial transfers leave temporary files.
    #[must_use]
    #[doc(alias = "--partial")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Selects the directory used for temporary files when staging updates.
    #[must_use]
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn with_temp_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.temp_dir = directory.map(Into::into);
        self
    }

    /// Requests that updated files be renamed into place after the transfer completes.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(mut self, delay: bool) -> Self {
        self.delay_updates = delay;
        if delay {
            self.partial = true;
        }
        self
    }

    /// Selects the directory used to retain partial files when transfers fail.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn with_partial_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.partial_dir = directory.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Appends a reference directory consulted for `--compare-dest`,
    /// `--copy-dest`, and `--link-dest` handling.
    #[must_use]
    pub fn push_reference_directory(mut self, reference: ReferenceDirectory) -> Self {
        self.reference_directories.push(reference);
        self
    }

    /// Extends the reference directory list with the provided entries.
    #[must_use]
    pub fn extend_reference_directories<I>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = ReferenceDirectory>,
    {
        self.reference_directories.extend(references);
        self
    }

    /// Requests in-place destination updates.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Enables appending to existing destination files when they are shorter than the source.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(mut self, append: bool) -> Self {
        self.append = append;
        if !append {
            self.append_verify = false;
        }
        self
    }

    /// Enables append-with-verification semantics.
    #[must_use]
    #[doc(alias = "--append-verify")]
    pub const fn append_verify(mut self, verify: bool) -> Self {
        if verify {
            self.append = true;
            self.append_verify = true;
        } else {
            self.append_verify = false;
        }
        self
    }

    /// Requests that relative source paths be preserved in the destination.
    #[must_use]
    #[doc(alias = "--relative")]
    pub const fn relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Controls whether parent directories implied by the source path are created.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = implied;
        self
    }

    /// Requests creation of missing destination path components prior to copying.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(mut self, mkpath: bool) -> Self {
        self.mkpath = mkpath;
        self
    }

    /// Prunes directories that would otherwise be empty after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
        self
    }

    /// Requests that device nodes be copied.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, devices: bool) -> Self {
        self.devices = devices;
        self
    }

    /// Requests that special files such as FIFOs be copied.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, specials: bool) -> Self {
        self.specials = specials;
        self
    }

    /// Enables collection of transfer events that describe work performed by the engine.
    #[must_use]
    pub const fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
        self
    }

    /// Requests that extended attributes be preserved when copying entries.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Reports whether extraneous destination files should be removed.
    #[must_use]
    pub const fn delete_extraneous(&self) -> bool {
        self.delete
    }

    /// Returns the configured maximum number of deletions, if any.
    #[must_use]
    pub const fn max_deletion_limit(&self) -> Option<u64> {
        self.max_deletions
    }

    /// Returns the minimum file size filter configured for the run.
    #[must_use]
    pub const fn min_file_size_limit(&self) -> Option<u64> {
        self.min_file_size
    }

    /// Returns the maximum file size filter configured for the run.
    #[must_use]
    pub const fn max_file_size_limit(&self) -> Option<u64> {
        self.max_file_size
    }

    /// Returns the configured deletion timing when deletion sweeps are enabled.
    #[must_use]
    pub const fn delete_timing(&self) -> Option<DeleteTiming> {
        if self.delete {
            Some(self.delete_timing)
        } else {
            None
        }
    }

    /// Reports whether deletions should occur before content transfers.
    #[must_use]
    pub const fn delete_before_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Before) && self.delete
    }

    /// Reports whether deletions should occur after transfers instead of immediately.
    #[must_use]
    pub const fn delete_after_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::After) && self.delete
    }

    /// Reports whether deletions are deferred until after transfers but determined during the walk.
    #[must_use]
    pub const fn delete_delay_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Delay) && self.delete
    }

    /// Reports whether deletions should occur while processing directory entries.
    #[must_use]
    pub const fn delete_during_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::During) && self.delete
    }

    /// Reports whether excluded paths should also be removed during deletion sweeps.
    #[must_use]
    pub const fn delete_excluded_enabled(&self) -> bool {
        self.delete_excluded
    }

    /// Reports whether source files should be removed after transfer.
    #[must_use]
    pub const fn remove_source_files_enabled(&self) -> bool {
        self.remove_source_files
    }

    /// Returns the configured bandwidth limit, if any, in bytes per second.
    #[must_use]
    pub const fn bandwidth_limit_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn bandwidth_burst_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_burst
    }

    /// Returns whether compression is enabled for payload handling.
    #[must_use]
    pub const fn compress_enabled(&self) -> bool {
        self.compress
    }

    /// Returns the configured compression level override, if any.
    #[must_use]
    pub const fn compression_level_override(&self) -> Option<CompressionLevel> {
        self.compression_level_override
    }

    /// Returns the compression level that should be used when compression is enabled.
    #[must_use]
    pub const fn compression_level(&self) -> CompressionLevel {
        match self.compression_level_override {
            Some(level) => level,
            None => self.compression_level,
        }
    }

    /// Reports whether whole-file transfers are requested.
    #[must_use]
    pub const fn whole_file_enabled(&self) -> bool {
        self.whole_file
    }

    /// Returns whether symlinks should be materialised as their referents.
    #[must_use]
    pub const fn copy_links_enabled(&self) -> bool {
        self.copy_links
    }

    /// Returns whether unsafe symlinks should be materialised as their referents.
    #[must_use]
    pub const fn copy_unsafe_links_enabled(&self) -> bool {
        self.copy_unsafe_links
    }

    /// Reports whether unsafe symlinks should be ignored.
    #[must_use]
    pub const fn safe_links_enabled(&self) -> bool {
        self.safe_links
    }

    /// Reports whether symlinks to directories should be followed as directories.
    #[must_use]
    pub const fn copy_dirlinks_enabled(&self) -> bool {
        self.copy_dirlinks
    }

    /// Reports whether existing destination directory symlinks should be preserved.
    #[must_use]
    pub const fn keep_dirlinks_enabled(&self) -> bool {
        self.keep_dirlinks
    }

    /// Returns the effective compression level when compression is enabled.
    #[must_use]
    pub const fn effective_compression_level(&self) -> Option<CompressionLevel> {
        if self.compress {
            Some(self.compression_level())
        } else {
            None
        }
    }

    /// Reports whether ownership preservation has been requested.
    #[must_use]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation has been requested.
    #[must_use]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Returns the configured group override, if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the configured chmod modifiers, if any.
    #[must_use]
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether directory modification times should be skipped during metadata preservation.
    #[must_use]
    pub const fn omit_dir_times_enabled(&self) -> bool {
        self.omit_dir_times
    }

    /// Returns whether symbolic link timestamps should be skipped.
    #[must_use]
    pub const fn omit_link_times_enabled(&self) -> bool {
        self.omit_link_times
    }

    #[cfg(feature = "acl")]
    /// Returns whether POSIX ACLs should be preserved.
    #[must_use]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    /// Returns the configured filter set, if any.
    #[must_use]
    pub fn filter_set(&self) -> Option<&FilterSet> {
        self.filters.as_ref()
    }

    /// Returns the configured filter program, if any.
    #[must_use]
    pub fn filter_program(&self) -> Option<&FilterProgram> {
        self.filter_program.as_ref()
    }

    /// Reports whether numeric UID/GID preservation has been requested.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Returns whether destination files are preallocated before writing.
    #[must_use]
    pub const fn preallocate_enabled(&self) -> bool {
        self.preallocate
    }

    #[cfg(feature = "acl")]
    /// Reports whether ACL preservation is enabled.
    #[must_use]
    pub const fn acls_enabled(&self) -> bool {
        self.preserve_acls
    }

    /// Reports whether checksum-based change detection has been requested.
    #[must_use]
    pub const fn checksum_enabled(&self) -> bool {
        self.checksum
    }

    /// Returns the strong checksum algorithm used for comparisons.
    #[must_use]
    pub const fn checksum_algorithm(&self) -> SignatureAlgorithm {
        self.checksum_algorithm
    }

    /// Returns the skip-compress list associated with the options.
    pub fn skip_compress(&self) -> &SkipCompressList {
        &self.skip_compress
    }

    /// Reports whether compression should be bypassed for `path`.
    pub fn should_skip_compress(&self, path: &Path) -> bool {
        self.skip_compress.matches_path(path)
    }

    /// Reports whether size-only change detection has been requested.
    #[must_use]
    pub const fn size_only_enabled(&self) -> bool {
        self.size_only
    }

    /// Reports whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing_enabled(&self) -> bool {
        self.ignore_existing
    }

    /// Reports whether missing source arguments should be ignored.
    #[must_use]
    pub const fn ignore_missing_args_enabled(&self) -> bool {
        self.ignore_missing_args
    }

    /// Reports whether newer destination files should be preserved.
    #[must_use]
    pub const fn update_enabled(&self) -> bool {
        self.update
    }

    /// Returns the modification time tolerance applied during comparisons.
    #[must_use]
    pub const fn modify_window(&self) -> Duration {
        self.modify_window
    }

    /// Reports whether sparse handling has been requested.
    #[must_use]
    pub const fn sparse_enabled(&self) -> bool {
        self.sparse
    }

    /// Reports whether copying of device nodes has been requested.
    #[must_use]
    pub const fn devices_enabled(&self) -> bool {
        self.devices
    }

    /// Reports whether copying of special files has been requested.
    #[must_use]
    pub const fn specials_enabled(&self) -> bool {
        self.specials
    }

    /// Reports whether relative path preservation has been requested.
    #[must_use]
    pub const fn relative_paths_enabled(&self) -> bool {
        self.relative_paths
    }

    /// Reports whether implied parent directories should be created automatically.
    #[must_use]
    pub const fn implied_dirs_enabled(&self) -> bool {
        self.implied_dirs
    }

    /// Reports whether `--mkpath` style directory creation is enabled.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath_enabled(&self) -> bool {
        self.mkpath
    }

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    pub const fn prune_empty_dirs_enabled(&self) -> bool {
        self.prune_empty_dirs
    }

    /// Reports whether partial transfer handling has been requested.
    #[must_use]
    pub const fn partial_enabled(&self) -> bool {
        self.partial || self.partial_dir.is_some()
    }

    /// Returns the configured partial directory when present.
    #[must_use]
    pub fn partial_directory_path(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Returns the configured temporary directory for staged updates when present.
    #[must_use]
    pub fn temp_directory_path(&self) -> Option<&Path> {
        self.temp_dir.as_deref()
    }

    /// Reports whether destination updates should be delayed until the end of the transfer.
    #[must_use]
    pub const fn delay_updates_enabled(&self) -> bool {
        self.delay_updates
    }

    /// Returns the ordered list of reference directories consulted during copy
    /// execution.
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether backups should be created before overwriting or deleting entries.
    #[must_use]
    pub const fn backup_enabled(&self) -> bool {
        self.backup
    }

    /// Returns the configured backup directory, if any.
    #[must_use]
    pub fn backup_directory(&self) -> Option<&Path> {
        self.backup_dir.as_deref()
    }

    /// Returns the suffix appended to backup file names.
    #[must_use]
    pub fn backup_suffix(&self) -> &OsStr {
        &self.backup_suffix
    }

    /// Reports whether in-place destination updates have been requested.
    #[must_use]
    pub const fn inplace_enabled(&self) -> bool {
        self.inplace
    }

    /// Returns `true` when appending to existing destinations is enabled.
    #[must_use]
    pub const fn append_enabled(&self) -> bool {
        self.append
    }

    /// Returns `true` when append verification is requested.
    #[must_use]
    pub const fn append_verify_enabled(&self) -> bool {
        self.append_verify
    }

    /// Reports whether the execution should record transfer events.
    #[must_use]
    pub const fn events_enabled(&self) -> bool {
        self.collect_events
    }

    /// Reports whether extended attribute preservation has been requested.
    #[cfg(feature = "xattr")]
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Returns the configured inactivity timeout, if any.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LinkDestEntry {
    path: PathBuf,
    is_relative: bool,
}

impl LinkDestEntry {
    pub(crate) fn new(path: PathBuf) -> Self {
        let is_relative = !path.is_absolute();
        Self { path, is_relative }
    }

    pub(crate) fn resolve(&self, destination_root: &Path, relative: &Path) -> PathBuf {
        let base = if self.is_relative {
            destination_root.join(&self.path)
        } else {
            self.path.clone()
        };
        base.join(relative)
    }
}

impl Default for LocalCopyOptions {
    fn default() -> Self {
        Self::new()
    }
}
