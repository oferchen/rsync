//! Execution context and helper types for local filesystem copies.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use super::ActiveCompressor;
use super::filter_program::{
    ExcludeIfPresentLayers, ExcludeIfPresentStack, FilterContext, FilterProgram, FilterSegment,
    FilterSegmentLayers, FilterSegmentStack, directory_has_marker,
};

#[cfg(all(unix, feature = "acl"))]
use super::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
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
use ::metadata::{MetadataOptions, apply_file_metadata_with_options};
use bandwidth::{BandwidthLimitComponents, BandwidthLimiter};
use checksums::RollingChecksum;
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use filters::FilterRule;

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
    stop_deadline: Option<Instant>,
    stop_at: Option<SystemTime>,
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

    #[cfg(all(unix, feature = "xattr"))]
    preserve_xattrs: bool,

    #[cfg(all(unix, feature = "acl"))]
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
        #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
        #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
    ) -> Self {
        Self {
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            file_type,
            destination_previously_existed,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(unix, feature = "acl"))]
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
    #[cfg(all(unix, feature = "xattr"))]
    preserve_xattrs: bool,
    #[cfg(all(unix, feature = "acl"))]
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
        #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
        #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
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
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(unix, feature = "acl"))]
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

include!("context_impl/state.rs");
include!("context_impl/options.rs");
include!("context_impl/transfer.rs");
include!("context_impl/delta_transfer.rs");
include!("context_impl/reporting.rs");

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
