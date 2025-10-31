//! Execution context and helper types for local filesystem copies.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::filter_program::{
    ExcludeIfPresentLayers, ExcludeIfPresentStack, FilterContext, FilterProgram, FilterSegment,
    FilterSegmentLayers, FilterSegmentStack, directory_has_marker,
};
#[cfg(feature = "acl")]
use super::sync_acls_if_requested;
#[cfg(feature = "xattr")]
use super::sync_xattrs_if_requested;
use super::{
    CopyComparison, DeleteTiming, DestinationWriteGuard, HardLinkTracker, LocalCopyAction,
    LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyExecution,
    LocalCopyMetadata, LocalCopyOptions, LocalCopyProgress, LocalCopyRecord,
    LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary, ReferenceDirectory,
    compute_backup_path, copy_entry_to_backup, delete_extraneous_entries,
    filter_program_local_error, follow_symlink_metadata, load_dir_merge_rules_recursive,
    map_metadata_error, remove_source_entry_if_requested, resolve_dir_merge_path, should_skip_copy,
    write_sparse_chunk,
};
use crate::delta::DeltaSignatureIndex;
use crate::signature::SignatureBlock;
use rsync_bandwidth::{BandwidthLimitComponents, BandwidthLimiter};
use rsync_checksums::RollingChecksum;
use rsync_compress::zlib::{CompressionLevel, CountingZlibEncoder};
use rsync_filters::FilterRule;
use rsync_meta::{MetadataOptions, apply_file_metadata_with_options};

pub(crate) struct CopyOutcome {
    summary: LocalCopySummary,
    events: Option<Vec<LocalCopyRecord>>,
    destination_root: PathBuf,
}

impl CopyOutcome {
    pub(super) fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    pub(super) fn into_summary_and_report(self) -> (LocalCopySummary, LocalCopyReport) {
        let summary = self.summary;
        let records = self.events.unwrap_or_default();
        (
            summary,
            LocalCopyReport::new(summary, records, self.destination_root),
        )
    }
}

pub(crate) struct CopyContext<'a> {
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    hard_links: HardLinkTracker,
    limiter: Option<BandwidthLimiter>,
    summary: LocalCopySummary,
    events: Option<Vec<LocalCopyRecord>>,
    filter_program: Option<FilterProgram>,
    dir_merge_layers: Rc<RefCell<FilterSegmentLayers>>,
    dir_merge_marker_layers: Rc<RefCell<ExcludeIfPresentLayers>>,
    observer: Option<&'a mut dyn LocalCopyRecordHandler>,
    dir_merge_ephemeral: Rc<RefCell<FilterSegmentStack>>,
    dir_merge_marker_ephemeral: Rc<RefCell<ExcludeIfPresentStack>>,
    deferred_deletions: Vec<DeferredDeletion>,
    deferred_updates: Vec<DeferredUpdate>,
    timeout: Option<Duration>,
    last_progress: Instant,
    created_entries: Vec<CreatedEntry>,
    destination_root: PathBuf,
}

pub(crate) struct FinalizeMetadataParams<'a> {
    metadata: &'a fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    source: &'a Path,
    relative: Option<&'a Path>,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
}

impl<'a> FinalizeMetadataParams<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        metadata: &'a fs::Metadata,
        metadata_options: MetadataOptions,
        mode: LocalCopyExecution,
        source: &'a Path,
        relative: Option<&'a Path>,
        file_type: fs::FileType,
        destination_previously_existed: bool,
        #[cfg(feature = "xattr")] preserve_xattrs: bool,
        #[cfg(feature = "acl")] preserve_acls: bool,
    ) -> Self {
        Self {
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FileCopyOutcome {
    literal_bytes: u64,
    compressed_bytes: Option<u64>,
}

impl FileCopyOutcome {
    fn new(literal_bytes: u64, compressed_bytes: Option<u64>) -> Self {
        Self {
            literal_bytes,
            compressed_bytes,
        }
    }

    pub(crate) fn literal_bytes(self) -> u64 {
        self.literal_bytes
    }

    pub(crate) fn compressed_bytes(self) -> Option<u64> {
        self.compressed_bytes
    }
}

/// Describes a block matched against the existing destination during delta copy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MatchedBlock<'a> {
    descriptor: &'a SignatureBlock,
    canonical_length: usize,
}

impl<'a> MatchedBlock<'a> {
    /// Creates a matched block descriptor from a [`SignatureBlock`] and its canonical length.
    fn new(descriptor: &'a SignatureBlock, canonical_length: usize) -> Self {
        Self {
            descriptor,
            canonical_length,
        }
    }

    /// Returns the matched [`SignatureBlock`].
    fn descriptor(&self) -> &'a SignatureBlock {
        self.descriptor
    }

    /// Calculates the byte offset of the block within the destination file.
    fn offset(&self) -> u64 {
        self.descriptor
            .index()
            .saturating_mul(self.canonical_length as u64)
    }
}

struct DeferredDeletion {
    destination: PathBuf,
    relative: Option<PathBuf>,
    keep: Vec<OsString>,
}

pub(crate) struct DeferredUpdate {
    guard: DestinationWriteGuard,
    metadata: fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    source: PathBuf,
    relative: Option<PathBuf>,
    destination: PathBuf,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
}

impl DeferredUpdate {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        guard: DestinationWriteGuard,
        metadata: fs::Metadata,
        metadata_options: MetadataOptions,
        mode: LocalCopyExecution,
        source: PathBuf,
        relative: Option<PathBuf>,
        destination: PathBuf,
        file_type: fs::FileType,
        destination_previously_existed: bool,
        #[cfg(feature = "xattr")] preserve_xattrs: bool,
        #[cfg(feature = "acl")] preserve_acls: bool,
    ) -> Self {
        Self {
            guard,
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            destination,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        }
    }
}

#[derive(Clone, Debug)]
struct CreatedEntry {
    path: PathBuf,
    kind: CreatedEntryKind,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CreatedEntryKind {
    File,
    Directory,
    Symlink,
    Fifo,
    Device,
    HardLink,
}

impl<'a> CopyContext<'a> {
    pub(super) fn new(
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        observer: Option<&'a mut dyn LocalCopyRecordHandler>,
        destination_root: PathBuf,
    ) -> Self {
        let burst = options.bandwidth_burst_bytes();
        let limiter =
            BandwidthLimitComponents::new(options.bandwidth_limit_bytes(), burst).into_limiter();
        let collect_events = options.events_enabled();
        let filter_program = options.filter_program().cloned();
        let dir_merge_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_marker_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_ephemeral = Vec::new();
        let dir_merge_marker_ephemeral = Vec::new();
        let timeout = options.timeout();
        Self {
            mode,
            options,
            hard_links: HardLinkTracker::new(),
            limiter,
            summary: LocalCopySummary::default(),
            events: if collect_events {
                Some(Vec::new())
            } else {
                None
            },
            filter_program,
            dir_merge_layers: Rc::new(RefCell::new(dir_merge_layers)),
            dir_merge_marker_layers: Rc::new(RefCell::new(dir_merge_marker_layers)),
            observer,
            dir_merge_ephemeral: Rc::new(RefCell::new(dir_merge_ephemeral)),
            dir_merge_marker_ephemeral: Rc::new(RefCell::new(dir_merge_marker_ephemeral)),
            deferred_deletions: Vec::new(),
            deferred_updates: Vec::new(),
            timeout,
            last_progress: Instant::now(),
            created_entries: Vec::new(),
            destination_root,
        }
    }

    pub(super) fn register_progress(&mut self) {
        self.last_progress = Instant::now();
    }

    pub(super) fn enforce_timeout(&mut self) -> Result<(), LocalCopyError> {
        if let Some(limit) = self.timeout {
            if self.last_progress.elapsed() > limit {
                return Err(LocalCopyError::timeout(limit));
            }
        }
        Ok(())
    }

    pub(super) fn mode(&self) -> LocalCopyExecution {
        self.mode
    }

    pub(super) fn options(&self) -> &LocalCopyOptions {
        &self.options
    }

    pub(super) fn one_file_system_enabled(&self) -> bool {
        self.options.one_file_system_enabled()
    }

    pub(super) fn record_hard_link(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if self.options.hard_links_enabled() {
            self.hard_links.record(metadata, destination);
        }
    }

    pub(super) fn existing_hard_link_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        if self.options.hard_links_enabled() {
            self.hard_links.existing_target(metadata)
        } else {
            None
        }
    }

    pub(super) fn delay_updates_enabled(&self) -> bool {
        self.options.delay_updates_enabled()
    }

    pub(super) fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    pub(super) fn apply_metadata_and_finalize(
        &mut self,
        destination: &Path,
        params: FinalizeMetadataParams<'_>,
    ) -> Result<(), LocalCopyError> {
        let FinalizeMetadataParams {
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        } = params;
        self.register_created_path(
            destination,
            CreatedEntryKind::File,
            destination_previously_existed,
        );
        apply_file_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
        #[cfg(feature = "xattr")]
        {
            sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
        }
        #[cfg(feature = "acl")]
        {
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
        }
        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = mode;
        self.record_hard_link(metadata, destination);
        remove_source_entry_if_requested(self, source, relative, file_type)?;
        Ok(())
    }

    pub(super) fn link_dest_target(
        &self,
        relative: &Path,
        source: &Path,
        metadata: &fs::Metadata,
        metadata_options: &MetadataOptions,
        size_only: bool,
        checksum: bool,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest candidate",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_file() {
                continue;
            }

            if should_skip_copy(CopyComparison {
                source_path: source,
                source: metadata,
                destination_path: candidate.as_path(),
                destination: &candidate_metadata,
                options: metadata_options,
                size_only,
                checksum,
                checksum_algorithm: self.options.checksum_algorithm(),
                modify_window: self.options.modify_window(),
            }) {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    pub(super) fn reference_directories(&self) -> &[ReferenceDirectory] {
        self.options.reference_directories()
    }

    pub(super) fn register_deferred_update(&mut self, update: DeferredUpdate) {
        let metadata = update.metadata.clone();
        let destination = update.destination.clone();
        self.record_hard_link(&metadata, destination.as_path());
        self.deferred_updates.push(update);
    }

    pub(super) fn commit_deferred_update_for(
        &mut self,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if let Some(index) = self
            .deferred_updates
            .iter()
            .position(|update| update.destination.as_path() == destination)
        {
            let update = self.deferred_updates.swap_remove(index);
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    pub(super) fn flush_deferred_updates(&mut self) -> Result<(), LocalCopyError> {
        if self.deferred_updates.is_empty() {
            return Ok(());
        }

        let updates = std::mem::take(&mut self.deferred_updates);
        for update in updates {
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    pub(super) fn backup_existing_entry(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
        file_type: fs::FileType,
    ) -> Result<(), LocalCopyError> {
        if !self.options.backup_enabled() || self.mode.is_dry_run() {
            return Ok(());
        }

        if file_type.is_dir() {
            return Ok(());
        }

        let backup_path = compute_backup_path(
            self.destination_root(),
            destination,
            relative,
            self.options.backup_directory(),
            self.options.backup_suffix(),
        );

        if let Some(parent) = backup_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create backup directory", parent.to_path_buf(), error)
                })?;
            }
        }

        match fs::rename(destination, &backup_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if let Err(remove_error) = fs::remove_file(&backup_path) {
                    if remove_error.kind() != io::ErrorKind::NotFound {
                        return Err(LocalCopyError::io(
                            "remove existing backup",
                            backup_path.clone(),
                            remove_error,
                        ));
                    }
                }
                fs::rename(destination, &backup_path).map_err(|rename_error| {
                    LocalCopyError::io("create backup", backup_path.clone(), rename_error)
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                copy_entry_to_backup(destination, &backup_path, file_type)?;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create backup",
                    backup_path.clone(),
                    error,
                ));
            }
        }

        Ok(())
    }

    pub(super) fn finalize_deferred_update(
        &mut self,
        update: DeferredUpdate,
    ) -> Result<(), LocalCopyError> {
        let DeferredUpdate {
            guard,
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            destination,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        } = update;

        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = &source;

        guard.commit()?;

        self.apply_metadata_and_finalize(
            destination.as_path(),
            FinalizeMetadataParams {
                metadata: &metadata,
                metadata_options,
                mode,
                source: source.as_path(),
                relative: relative.as_deref(),
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            },
        )
    }

    pub(super) fn delete_timing(&self) -> Option<DeleteTiming> {
        self.options.delete_timing()
    }

    pub(super) fn min_file_size_limit(&self) -> Option<u64> {
        self.options.min_file_size_limit()
    }

    pub(super) fn max_file_size_limit(&self) -> Option<u64> {
        self.options.max_file_size_limit()
    }

    pub(super) fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .numeric_ids(self.options.numeric_ids_enabled())
            .with_owner_override(self.options.owner_override())
            .with_group_override(self.options.group_override())
            .with_chmod(self.options.chmod().cloned())
    }

    pub(super) fn copy_links_enabled(&self) -> bool {
        self.options.copy_links_enabled()
    }

    pub(super) fn copy_unsafe_links_enabled(&self) -> bool {
        self.options.copy_unsafe_links_enabled()
    }

    pub(super) fn safe_links_enabled(&self) -> bool {
        self.options.safe_links_enabled()
    }

    pub(super) fn copy_dirlinks_enabled(&self) -> bool {
        self.options.copy_dirlinks_enabled()
    }

    pub(super) fn keep_dirlinks_enabled(&self) -> bool {
        self.options.keep_dirlinks_enabled()
    }

    pub(super) fn whole_file_enabled(&self) -> bool {
        self.options.whole_file_enabled()
    }

    pub(super) fn sparse_enabled(&self) -> bool {
        self.options.sparse_enabled()
    }

    pub(super) fn append_enabled(&self) -> bool {
        self.options.append_enabled()
    }

    pub(super) fn append_verify_enabled(&self) -> bool {
        self.options.append_verify_enabled()
    }

    pub(super) fn preallocate_enabled(&self) -> bool {
        self.options.preallocate_enabled()
    }

    pub(super) fn devices_enabled(&self) -> bool {
        self.options.devices_enabled()
    }

    pub(super) fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    #[cfg(feature = "acl")]
    pub(super) fn acls_enabled(&self) -> bool {
        self.options.acls_enabled()
    }

    pub(super) fn relative_paths_enabled(&self) -> bool {
        self.options.relative_paths_enabled()
    }

    pub(super) fn implied_dirs_enabled(&self) -> bool {
        self.options.implied_dirs_enabled()
    }

    pub(super) fn mkpath_enabled(&self) -> bool {
        self.options.mkpath_enabled()
    }

    pub(super) fn prune_empty_dirs_enabled(&self) -> bool {
        self.options.prune_empty_dirs_enabled()
    }

    pub(super) fn omit_dir_times_enabled(&self) -> bool {
        self.options.omit_dir_times_enabled()
    }

    pub(super) fn omit_link_times_enabled(&self) -> bool {
        self.options.omit_link_times_enabled()
    }

    pub(super) fn prepare_parent_directory(&mut self, parent: &Path) -> Result<(), LocalCopyError> {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }

        let allow_creation = self.implied_dirs_enabled() || self.mkpath_enabled();
        let keep_dirlinks = self.keep_dirlinks_enabled();

        if self.mode.is_dry_run() {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        follow_symlink_metadata(parent).and_then(|metadata| {
                            if metadata.file_type().is_dir() {
                                Ok(())
                            } else {
                                Err(LocalCopyError::invalid_argument(
                                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                                ))
                            }
                        })
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if allow_creation {
                        Ok(())
                    } else {
                        Err(LocalCopyError::io(
                            "create parent directory",
                            parent.to_path_buf(),
                            error,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "inspect existing destination",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else if allow_creation {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(parent).map_err(|error| {
                        LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                    })?;
                    self.register_progress();
                    Ok(())
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        }
    }

    pub(super) fn remove_source_files_enabled(&self) -> bool {
        self.options.remove_source_files_enabled()
    }

    pub(super) fn compress_enabled(&self) -> bool {
        self.options.compress_enabled()
    }

    pub(super) fn should_compress(&self, relative: &Path) -> bool {
        self.compress_enabled() && !self.options.should_skip_compress(relative)
    }

    pub(super) fn compression_level(&self) -> CompressionLevel {
        self.options.compression_level()
    }

    pub(super) fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    pub(super) fn size_only_enabled(&self) -> bool {
        self.options.size_only_enabled()
    }

    pub(super) fn ignore_existing_enabled(&self) -> bool {
        self.options.ignore_existing_enabled()
    }

    pub(super) fn ignore_missing_args_enabled(&self) -> bool {
        self.options.ignore_missing_args_enabled()
    }

    pub(super) fn update_enabled(&self) -> bool {
        self.options.update_enabled()
    }

    pub(super) fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    pub(super) fn partial_directory_path(&self) -> Option<&Path> {
        self.options.partial_directory_path()
    }

    pub(super) fn temp_directory_path(&self) -> Option<&Path> {
        self.options.temp_directory_path()
    }

    pub(super) fn inplace_enabled(&self) -> bool {
        self.options.inplace_enabled()
    }

    #[cfg(feature = "xattr")]
    pub(super) fn xattrs_enabled(&self) -> bool {
        self.options.preserve_xattrs()
    }

    pub(super) fn allows(&self, relative: &Path, is_dir: bool) -> bool {
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            program
                .evaluate(
                    relative,
                    is_dir,
                    layers.as_slice(),
                    temp_layers,
                    FilterContext::Transfer,
                )
                .allows_transfer()
        } else if let Some(filters) = self.options.filter_set() {
            filters.allows(relative, is_dir)
        } else {
            true
        }
    }

    pub(super) fn allows_deletion(&self, relative: &Path, is_dir: bool) -> bool {
        let delete_excluded = self.options.delete_excluded_enabled();
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            let outcome = program.evaluate(
                relative,
                is_dir,
                layers.as_slice(),
                temp_layers,
                FilterContext::Deletion,
            );
            if delete_excluded {
                outcome.allows_deletion_when_excluded_removed()
            } else {
                outcome.allows_deletion()
            }
        } else if let Some(filters) = self.options.filter_set() {
            if delete_excluded {
                filters.allows_deletion_when_excluded_removed(relative, is_dir)
            } else {
                filters.allows_deletion(relative, is_dir)
            }
        } else {
            true
        }
    }

    pub(super) fn enter_directory(
        &self,
        source: &Path,
    ) -> Result<DirectoryFilterGuard, LocalCopyError> {
        let Some(program) = &self.filter_program else {
            let handles = DirectoryFilterHandles {
                layers: Rc::clone(&self.dir_merge_layers),
                marker_layers: Rc::clone(&self.dir_merge_marker_layers),
                ephemeral: Rc::clone(&self.dir_merge_ephemeral),
                marker_ephemeral: Rc::clone(&self.dir_merge_marker_ephemeral),
            };
            return Ok(DirectoryFilterGuard::new(
                handles,
                Vec::new(),
                Vec::new(),
                false,
                false,
            ));
        };

        let mut added_indices = Vec::new();
        let mut marker_counts = Vec::new();
        let mut layers = self.dir_merge_layers.borrow_mut();
        let mut marker_layers = self.dir_merge_marker_layers.borrow_mut();
        let mut ephemeral_stack = self.dir_merge_ephemeral.borrow_mut();
        let mut marker_ephemeral_stack = self.dir_merge_marker_ephemeral.borrow_mut();
        ephemeral_stack.push(Vec::new());
        marker_ephemeral_stack.push(Vec::new());

        for (index, rule) in program.dir_merge_rules().iter().enumerate() {
            let candidate = resolve_dir_merge_path(source, rule.pattern());

            let metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(LocalCopyError::io(
                        "inspect filter file",
                        candidate.clone(),
                        error,
                    ));
                }
            };

            if !metadata.is_file() {
                continue;
            }

            let mut visited = Vec::new();
            let mut entries = match load_dir_merge_rules_recursive(
                candidate.as_path(),
                rule.options(),
                &mut visited,
            ) {
                Ok(entries) => entries,
                Err(error) => {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(error);
                }
            };

            let mut segment = FilterSegment::default();
            for compiled in entries.rules.drain(..) {
                if let Err(error) = segment.push_rule(compiled) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            if rule.options().excludes_self() {
                let pattern = rule.pattern().to_string_lossy().into_owned();
                if let Err(error) = segment.push_rule(FilterRule::exclude(pattern)) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            let has_segment = !segment.is_empty();
            let markers = entries.exclude_if_present;
            if !has_segment && markers.is_empty() {
                continue;
            }

            if rule.options().inherit_rules() {
                if has_segment {
                    layers[index].push(segment);
                    added_indices.push(index);
                }
                if !markers.is_empty() {
                    let count = markers.len();
                    marker_layers[index].extend(markers.into_iter());
                    marker_counts.push((index, count));
                }
            } else {
                if has_segment {
                    if let Some(current) = ephemeral_stack.last_mut() {
                        current.push((index, segment));
                    }
                }
                if !markers.is_empty() {
                    if let Some(current) = marker_ephemeral_stack.last_mut() {
                        current.push((index, markers));
                    }
                }
            }
        }

        drop(layers);
        drop(marker_layers);
        drop(ephemeral_stack);
        drop(marker_ephemeral_stack);

        let excluded = self.directory_excluded(source, program)?;

        let handles = DirectoryFilterHandles {
            layers: Rc::clone(&self.dir_merge_layers),
            marker_layers: Rc::clone(&self.dir_merge_marker_layers),
            ephemeral: Rc::clone(&self.dir_merge_ephemeral),
            marker_ephemeral: Rc::clone(&self.dir_merge_marker_ephemeral),
        };
        Ok(DirectoryFilterGuard::new(
            handles,
            added_indices,
            marker_counts,
            true,
            excluded,
        ))
    }

    pub(super) fn directory_excluded(
        &self,
        directory: &Path,
        program: &FilterProgram,
    ) -> Result<bool, LocalCopyError> {
        if program.should_exclude_directory(directory)? {
            return Ok(true);
        }

        {
            let layers = self.dir_merge_marker_layers.borrow();
            for rules in layers.iter() {
                if directory_has_marker(rules, directory)? {
                    return Ok(true);
                }
            }
        }

        {
            let stack = self.dir_merge_marker_ephemeral.borrow();
            if let Some(entries) = stack.last() {
                for (_, rules) in entries.iter() {
                    if directory_has_marker(rules, directory)? {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    pub(super) fn summary_mut(&mut self) -> &mut LocalCopySummary {
        &mut self.summary
    }

    pub(super) fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    pub(super) fn record(&mut self, record: LocalCopyRecord) {
        if let Some(observer) = &mut self.observer {
            observer.handle(record.clone());
        }
        if let Some(events) = &mut self.events {
            events.push(record);
        }
    }

    pub(super) fn notify_progress(
        &mut self,
        relative: &Path,
        total_bytes: Option<u64>,
        transferred: u64,
        elapsed: Duration,
    ) {
        self.register_progress();
        if self.observer.is_none() {
            return;
        }

        if let Some(observer) = &mut self.observer {
            observer.handle_progress(LocalCopyProgress::new(
                relative,
                transferred,
                total_bytes,
                elapsed,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_file_contents(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        delta: Option<&DeltaSignatureIndex>,
        total_size: u64,
        initial_bytes: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        if let Some(index) = delta {
            return self.copy_file_contents_with_delta(
                reader,
                writer,
                buffer,
                sparse,
                compress,
                source,
                destination,
                relative,
                index,
                total_size,
                initial_bytes,
                start,
            );
        }

        let mut total_bytes: u64 = 0;
        let mut literal_bytes: u64 = 0;
        let mut compressor = if compress {
            Some(CountingZlibEncoder::new(self.compression_level()))
        } else {
            None
        };
        let mut compressed_progress: u64 = 0;

        loop {
            self.enforce_timeout()?;
            let chunk_len = if let Some(limiter) = self.limiter.as_ref() {
                limiter.recommended_read_size(buffer.len())
            } else {
                buffer.len()
            };

            let read = reader
                .read(&mut buffer[..chunk_len])
                .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
            if read == 0 {
                break;
            }

            let written = if sparse {
                write_sparse_chunk(writer, &buffer[..read], destination)?
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
                read
            };

            self.register_progress();

            let mut compressed_delta = None;
            if let Some(encoder) = compressor.as_mut() {
                encoder.write(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("compress file", source.to_path_buf(), error)
                })?;
                let total = encoder.bytes_written();
                let delta = total.saturating_sub(compressed_progress);
                compressed_progress = total;
                compressed_delta = Some(delta);
            }

            if let Some(sleep) = if let Some(limiter) = self.limiter.as_mut() {
                if let Some(delta) = compressed_delta {
                    if delta > 0 {
                        let bounded = delta.min(usize::MAX as u64) as usize;
                        Some(limiter.register(bounded))
                    } else {
                        None
                    }
                } else {
                    Some(limiter.register(read))
                }
            } else {
                None
            } {
                self.summary.record_bandwidth_sleep(sleep.requested());
            }

            total_bytes = total_bytes.saturating_add(read as u64);
            literal_bytes = literal_bytes.saturating_add(written as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            let final_len = initial_bytes.saturating_add(total_bytes);
            writer.set_len(final_len).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            self.register_progress();
            if let Some(sleep) = if let Some(limiter) = self.limiter.as_mut() {
                let delta = compressed_total.saturating_sub(compressed_progress);
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    Some(limiter.register(bounded))
                } else {
                    None
                }
            } else {
                None
            } {
                self.summary.record_bandwidth_sleep(sleep.requested());
            }
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_file_contents_with_delta(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        index: &DeltaSignatureIndex,
        total_size: u64,
        initial_bytes: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        let mut destination_reader = fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;
        let mut compressor = if compress {
            Some(CountingZlibEncoder::new(self.compression_level()))
        } else {
            None
        };
        let mut compressed_progress = 0u64;
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;
        let mut window: VecDeque<u8> = VecDeque::with_capacity(index.block_length());
        let mut pending_literals = Vec::with_capacity(index.block_length());
        let mut scratch = Vec::with_capacity(index.block_length());
        let mut rolling = RollingChecksum::new();
        let mut outgoing: Option<u8> = None;
        let mut read_buffer = vec![0u8; buffer.len().max(index.block_length())];
        let mut buffer_len = 0usize;
        let mut buffer_pos = 0usize;

        loop {
            self.enforce_timeout()?;
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut read_buffer).map_err(|error| {
                    LocalCopyError::io("copy file", source.to_path_buf(), error)
                })?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = read_buffer[buffer_pos];
            buffer_pos += 1;

            window.push_back(byte);
            if let Some(outgoing_byte) = outgoing.take() {
                debug_assert!(window.len() <= index.block_length());
                rolling.roll_many(&[outgoing_byte], &[byte]).map_err(|_| {
                    LocalCopyError::invalid_argument(LocalCopyArgumentError::UnsupportedFileType)
                })?;
            } else {
                rolling.update(&[byte]);
            }

            if window.len() < index.block_length() {
                continue;
            }

            let digest = rolling.digest();
            if let Some(block_index) = index.find_match_window(digest, &window, &mut scratch) {
                if !pending_literals.is_empty() {
                    let flushed_len = pending_literals.len();
                    let flushed = self.flush_literal_chunk(
                        writer,
                        pending_literals.as_slice(),
                        sparse,
                        compressor.as_mut(),
                        &mut compressed_progress,
                        source,
                        destination,
                    )?;
                    literal_bytes = literal_bytes.saturating_add(flushed as u64);
                    total_bytes = total_bytes.saturating_add(flushed_len as u64);
                    let progressed = initial_bytes.saturating_add(total_bytes);
                    self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
                    pending_literals.clear();
                }

                let block = index.block(block_index);
                let block_len = block.len();
                let matched = MatchedBlock::new(block, index.block_length());
                self.copy_matched_block(
                    &mut destination_reader,
                    writer,
                    buffer,
                    destination,
                    matched,
                    sparse,
                )?;
                total_bytes = total_bytes.saturating_add(block_len as u64);
                let progressed = initial_bytes.saturating_add(total_bytes);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
                window.clear();
                rolling.reset();
                outgoing = None;
                continue;
            }

            if let Some(front) = window.pop_front() {
                pending_literals.push(front);
                outgoing = Some(front);
            }
        }

        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            let flushed_len = pending_literals.len();
            let flushed = self.flush_literal_chunk(
                writer,
                pending_literals.as_slice(),
                sparse,
                compressor.as_mut(),
                &mut compressed_progress,
                source,
                destination,
            )?;
            total_bytes = total_bytes.saturating_add(flushed_len as u64);
            literal_bytes = literal_bytes.saturating_add(flushed as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            let final_len = initial_bytes.saturating_add(total_bytes);
            writer.set_len(final_len).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            if let Some(limiter) = self.limiter.as_mut() {
                let delta = compressed_total.saturating_sub(compressed_progress);
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    let sleep = limiter.register(bounded);
                    self.summary.record_bandwidth_sleep(sleep.requested());
                }
            }
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn flush_literal_chunk(
        &mut self,
        writer: &mut fs::File,
        chunk: &[u8],
        sparse: bool,
        compressor: Option<&mut CountingZlibEncoder>,
        compressed_progress: &mut u64,
        source: &Path,
        destination: &Path,
    ) -> Result<usize, LocalCopyError> {
        if chunk.is_empty() {
            return Ok(0);
        }
        self.enforce_timeout()?;
        let written = if sparse {
            write_sparse_chunk(writer, chunk, destination)?
        } else {
            writer.write_all(chunk).map_err(|error| {
                LocalCopyError::io("copy file", destination.to_path_buf(), error)
            })?;
            chunk.len()
        };

        let mut sleep_recorded = None;
        if let Some(encoder) = compressor {
            encoder.write(chunk).map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            let total = encoder.bytes_written();
            let delta = total.saturating_sub(*compressed_progress);
            *compressed_progress = total;
            if let Some(limiter) = self.limiter.as_mut() {
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    sleep_recorded = Some(limiter.register(bounded));
                }
            }
        } else if let Some(limiter) = self.limiter.as_mut() {
            sleep_recorded = Some(limiter.register(chunk.len()));
        }

        if let Some(sleep) = sleep_recorded {
            self.summary.record_bandwidth_sleep(sleep.requested());
        }

        Ok(written)
    }

    pub(super) fn copy_matched_block(
        &mut self,
        existing: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        destination: &Path,
        matched: MatchedBlock<'_>,
        sparse: bool,
    ) -> Result<(), LocalCopyError> {
        let offset = matched.offset();
        existing.seek(SeekFrom::Start(offset)).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;

        let mut remaining = matched.descriptor().len();
        while remaining > 0 {
            self.enforce_timeout()?;
            let chunk_len = remaining.min(buffer.len());
            let read = existing.read(&mut buffer[..chunk_len]).map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            if read == 0 {
                let eof = io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading existing block",
                );
                return Err(LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    eof,
                ));
            }

            if sparse {
                let _ = write_sparse_chunk(writer, &buffer[..read], destination)?;
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
            }

            remaining -= read;
        }

        Ok(())
    }

    pub(super) fn record_skipped_non_regular(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedNonRegular,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    pub(super) fn record_skipped_mount_point(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedMountPoint,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    pub(super) fn record_skipped_unsafe_symlink(
        &mut self,
        relative: Option<&Path>,
        metadata: &fs::Metadata,
        target: PathBuf,
    ) {
        if let Some(path) = relative {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedUnsafeSymlink,
                0,
                None,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
    }

    pub(super) fn record_file_list_generation(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_generation(elapsed);
        }
    }

    #[allow(dead_code)]
    pub(super) fn record_file_list_transfer(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_transfer(elapsed);
        }
    }

    pub(super) fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            summary: self.summary,
            events: self.events,
            destination_root: self.destination_root,
        }
    }

    pub(super) fn defer_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        keep: Vec<OsString>,
    ) {
        self.deferred_deletions.push(DeferredDeletion {
            destination,
            relative,
            keep,
        });
    }

    pub(super) fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        let pending = std::mem::take(&mut self.deferred_deletions);
        for entry in pending {
            self.enforce_timeout()?;
            let relative = entry.relative.as_deref();
            delete_extraneous_entries(self, entry.destination.as_path(), relative, &entry.keep)?;
        }
        Ok(())
    }

    pub(super) fn register_created_path(
        &mut self,
        path: &Path,
        kind: CreatedEntryKind,
        existed_before: bool,
    ) {
        if self.mode.is_dry_run() || existed_before {
            return;
        }
        self.created_entries.push(CreatedEntry {
            path: path.to_path_buf(),
            kind,
        });
    }

    pub(crate) fn last_created_entry_path(&self) -> Option<&Path> {
        self.created_entries
            .last()
            .map(|entry| entry.path.as_path())
    }

    pub(crate) fn pop_last_created_entry(&mut self) {
        self.created_entries.pop();
    }

    pub(super) fn rollback_on_error(&mut self, error: &LocalCopyError) {
        if matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }) {
            self.rollback_created_entries();
        }
    }

    pub(super) fn rollback_created_entries(&mut self) {
        while let Some(entry) = self.created_entries.pop() {
            match entry.kind {
                CreatedEntryKind::Directory => {
                    let _ = fs::remove_dir(&entry.path);
                }
                CreatedEntryKind::File
                | CreatedEntryKind::Symlink
                | CreatedEntryKind::Fifo
                | CreatedEntryKind::Device
                | CreatedEntryKind::HardLink => {
                    let _ = fs::remove_file(&entry.path);
                }
            }
        }
    }
}

#[derive(Clone)]
struct DirectoryFilterHandles {
    layers: Rc<RefCell<FilterSegmentLayers>>,
    marker_layers: Rc<RefCell<ExcludeIfPresentLayers>>,
    ephemeral: Rc<RefCell<FilterSegmentStack>>,
    marker_ephemeral: Rc<RefCell<ExcludeIfPresentStack>>,
}

pub(crate) struct DirectoryFilterGuard {
    handles: DirectoryFilterHandles,
    indices: Vec<usize>,
    marker_counts: Vec<(usize, usize)>,
    ephemeral_active: bool,
    excluded: bool,
}

impl DirectoryFilterGuard {
    fn new(
        handles: DirectoryFilterHandles,
        indices: Vec<usize>,
        marker_counts: Vec<(usize, usize)>,
        ephemeral_active: bool,
        excluded: bool,
    ) -> Self {
        Self {
            handles,
            indices,
            marker_counts,
            ephemeral_active,
            excluded,
        }
    }

    pub(crate) fn is_excluded(&self) -> bool {
        self.excluded
    }
}

impl Drop for DirectoryFilterGuard {
    fn drop(&mut self) {
        if self.ephemeral_active {
            let mut stack = self.handles.ephemeral.borrow_mut();
            stack.pop();
            let mut marker_stack = self.handles.marker_ephemeral.borrow_mut();
            marker_stack.pop();
        }

        if !self.marker_counts.is_empty() {
            let mut marker_layers = self.handles.marker_layers.borrow_mut();
            for (index, count) in self.marker_counts.drain(..).rev() {
                if let Some(layer) = marker_layers.get_mut(index) {
                    for _ in 0..count {
                        layer.pop();
                    }
                }
            }
        }

        if !self.indices.is_empty() {
            let mut layers = self.handles.layers.borrow_mut();
            for index in self.indices.drain(..).rev() {
                if let Some(layer) = layers.get_mut(index) {
                    layer.pop();
                }
            }
        }
    }
}
