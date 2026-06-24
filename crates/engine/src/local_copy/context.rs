//! Execution context and helper types for local filesystem copies.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use std::sync::Arc;

use super::ActiveCompressor;
use super::buffer_pool::{BufferPool, global_buffer_pool};
use super::deferred_sync::{DeferredSync, SyncStrategy};
use super::filter_program::{
    ExcludeIfPresentLayers, ExcludeIfPresentRule, ExcludeIfPresentStack, FilterContext,
    FilterOutcome, FilterProgram, FilterSegment, FilterSegmentLayers, FilterSegmentStack,
    directory_has_marker,
};

#[cfg(all(any(unix, windows), feature = "acl"))]
use super::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use super::sync_nfsv4_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use super::sync_xattrs_if_requested;

use super::{
    CopyComparison, DeleteTiming, DestinationWriteGuard, HardLinkTracker, LocalCopyAction,
    LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyExecution,
    LocalCopyMetadata, LocalCopyOptions, LocalCopyProgress, LocalCopyRecord,
    LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary, NestedDirMerge, ReferenceDirectory,
    SparseWriteState, SparseWriter, compute_backup_path, copy_entry_to_backup,
    delete_extraneous_entries, filter_program_local_error, follow_symlink_metadata,
    load_dir_merge_rules_recursive, map_metadata_error, remove_source_entry_if_requested,
    resolve_dir_merge_path, should_skip_copy, symlink_target_is_safe, trace_make_backup_copy,
    trace_make_backup_rename, trace_make_backup_symlink, write_sparse_chunk,
};
use crate::delta::DeltaSignatureIndex;
use crate::signature::SignatureBlock;
use ::metadata::{MetadataOptions, apply_file_metadata_with_options};
use bandwidth::{BandwidthLimitComponents, BandwidthLimiter};
use checksums::RollingChecksum;
use compress::algorithm::CompressionAlgorithm;
use compress::strategy::adaptive_level::AdaptiveLevelController;
use compress::zlib::CompressionLevel;
use filters::FilterRule;
use logging::info_log;
use protocol::flist::FileListWriter;

/// Aggregated result of a local copy operation, containing the transfer
/// summary, optional per-file event records, and the destination root path.
pub(crate) struct CopyOutcome {
    summary: LocalCopySummary,
    events: Option<Vec<LocalCopyRecord>>,
    destination_root: PathBuf,
}

impl CopyOutcome {
    /// Consumes the outcome and returns only the transfer summary.
    pub(super) fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    /// Consumes the outcome and returns both the summary and a detailed report.
    pub(super) fn into_summary_and_report(self) -> (LocalCopySummary, LocalCopyReport) {
        let summary = self.summary;
        let records = self.events.unwrap_or_default();
        (
            summary,
            LocalCopyReport::new(summary, records, self.destination_root),
        )
    }
}

/// Mutable execution context threaded through every stage of a local copy.
///
/// Holds transfer options, progress tracking, filter state, deferred
/// operations, and the buffer pool. Created once per `local_copy()` call
/// and consumed to produce a [`CopyOutcome`].
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
    /// Per-directory stack of dynamic `dir-merge` rules registered while
    /// loading parent per-directory merge files.
    ///
    /// upstream: exclude.c:1419-1428 - `dir-merge .filt2` inside `bar/.filt`
    /// registers `.filt2` for lookup in every subdirectory entered beneath
    /// `bar/`. Each stack frame matches one `enter_directory_for_path` call:
    /// the frame's `active_rules` is the cumulative set inherited from the
    /// parent frame plus any rules newly registered while processing the
    /// current directory's merge files. `loaded_segments` and
    /// `loaded_markers` are the segments / markers produced by looking up
    /// the active rules against the current directory. The
    /// [`DirectoryFilterGuard`] pops the frame on drop.
    dynamic_dir_merge_stack: Rc<RefCell<Vec<DynamicDirMergeFrame>>>,
    deferred_ops: DeferredOperationQueue,
    timeout: Option<Duration>,
    stop_deadline: Option<Instant>,
    stop_at: Option<SystemTime>,
    last_progress: Instant,
    destination_root: PathBuf,
    /// Number of leading path components in `relative` that represent the
    /// transfer root name (e.g. the source directory name when copying
    /// without a trailing slash).  These components inflate the depth
    /// visible to `symlink_target_is_safe` and must be excluded when
    /// computing the safety-relative path for `--safe-links` /
    /// `--copy-unsafe-links`.
    safety_depth_offset: usize,
    /// Whether to use the buffer pool for I/O operations (runtime toggle).
    /// When `true`, buffers are acquired from the shared pool for reuse.
    /// When `false`, a fresh `Vec` is allocated for each transfer.
    use_buffer_pool: bool,
    /// Shared buffer pool for file I/O operations.
    buffer_pool: Arc<BufferPool>,
    /// Deferred filesystem sync manager.
    deferred_sync: DeferredSync,
    /// Cache of prefetched file checksums for parallel checksum mode.
    checksum_cache: Option<super::executor::ChecksumCache>,
    /// Tracks whether any I/O errors occurred during the transfer.
    /// When set to `true` and `--ignore-errors` is not enabled, deletions
    /// are suppressed to prevent data loss.
    io_errors_occurred: bool,
    /// `true` when the active plan carries more than one source operand.
    /// Used to switch `--delete-during` to a deferred sweep so the per-source
    /// keep lists can be merged before any extraneous unlink fires; upstream
    /// achieves the same result by sharing a single flist across sources.
    multi_source: bool,
    /// Cache of parent directories whose existence has been verified.
    /// Eliminates redundant `statx` syscalls when many files share the
    /// same parent (e.g. 10K files in one directory → 1 stat instead of 10K).
    verified_parents: HashSet<PathBuf>,
    /// Protocol flist encoder for batch mode.
    ///
    /// When batch mode is active, file entries are encoded using the protocol
    /// wire format (same as network transfers) so the batch file body matches
    /// upstream rsync's raw stream tee. The writer maintains cross-entry
    /// compression state (name prefix sharing, same-mode flags, etc.).
    batch_flist_writer: Option<FileListWriter>,
    /// Per-file delta data accumulator for the current file being captured.
    /// Reset at each `begin_batch_file_delta()`, contents moved to
    /// `batch_delta_entries` at `finalize_batch_file_delta()`.
    ///
    /// Contains iflags + sum_head + tokens + checksum (but NOT the NDX,
    /// which is written at flush time after sort-order mapping).
    batch_delta_buf: Option<io::Cursor<Vec<u8>>>,
    /// Completed per-file delta entries: (traversal_index, data).
    /// Data contains iflags + sum_head + tokens + checksum without NDX.
    /// NDX is computed from sort-order mapping at flush time.
    batch_delta_entries: Vec<(i32, Vec<u8>)>,
    /// Sort metadata for each flist entry in traversal order: (name_bytes, is_dir).
    /// Used to compute the traversal→sorted index mapping that upstream's
    /// `flist_sort_and_clean()` produces after reading the batch flist.
    batch_entry_sort_data: Vec<(Vec<u8>, bool)>,
    /// Traversal index of the current file being delta-captured.
    batch_current_delta_idx: i32,
    /// Zero-based file-list index counter for batch NDX framing.
    /// Incremented in `capture_batch_file_entry()` for every entry (dirs,
    /// files, symlinks, etc.) to match the upstream flist numbering.
    batch_flist_index: i32,
    /// NDX codec for writing file indices to the batch delta stream.
    /// Persists across files to maintain delta-encoding state (proto >= 30).
    batch_ndx_codec: Option<protocol::codec::NdxCodecEnum>,
    /// Reusable buffer for directory enumeration in `read_directory_entries_sorted`.
    ///
    /// Cleared and refilled at each directory level, avoiding a fresh heap
    /// allocation for the intermediate `(OsString, PathBuf)` collection per
    /// directory during recursive traversal.
    readdir_buf: Vec<(OsString, PathBuf)>,
    /// Adaptive compression level controller that adjusts compression level
    /// between files based on observed compression ratios.
    adaptive_level: Option<AdaptiveLevelController>,
}

/// Path and type context for metadata finalization.
///
/// Groups the source path, optional relative path, file type, and whether the
/// destination previously existed - all describing the "where and what" of the
/// entry being finalized. Extracted from [`FinalizeMetadataParams`] to reduce
/// parameter count.
pub(crate) struct MetadataPathContext<'a> {
    pub(crate) source: &'a Path,
    pub(crate) relative: Option<&'a Path>,
    pub(crate) file_type: fs::FileType,
    pub(crate) destination_previously_existed: bool,
}

/// Parameters for the metadata-and-finalize step after writing a file.
///
/// Bundles the source metadata, option flags, and platform-specific handles
/// needed by [`CopyContext::apply_metadata_and_finalize`].
pub(crate) struct FinalizeMetadataParams<'a> {
    metadata: &'a fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    path_context: MetadataPathContext<'a>,

    /// Pre-transfer destination metadata captured before the temp-file
    /// rename. Mirrors upstream's `stat_mode` argument to `dest_mode()`:
    /// `Some(meta)` when the destination existed at transfer start;
    /// `None` for a brand-new destination. Used by
    /// [`::metadata::apply_dest_mode_pre_transfer`] to reproduce the
    /// upstream `rsync.c:954-965` chmod-on-rename loop.
    pre_transfer_meta: Option<&'a fs::Metadata>,

    #[cfg(unix)]
    fd: Option<std::os::fd::BorrowedFd<'a>>,

    #[cfg(all(unix, feature = "xattr"))]
    preserve_xattrs: bool,

    #[cfg(all(any(unix, windows), feature = "acl"))]
    preserve_acls: bool,
}

impl<'a> FinalizeMetadataParams<'a> {
    pub(crate) const fn new(
        metadata: &'a fs::Metadata,
        metadata_options: MetadataOptions,
        mode: LocalCopyExecution,
        path_context: MetadataPathContext<'a>,
        #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
        #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
    ) -> Self {
        Self {
            metadata,
            metadata_options,
            mode,
            path_context,
            pre_transfer_meta: None,
            #[cfg(unix)]
            fd: None,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        }
    }

    /// Attach an open file descriptor for fd-based metadata operations.
    #[cfg(unix)]
    pub(crate) const fn with_fd(mut self, fd: std::os::fd::BorrowedFd<'a>) -> Self {
        self.fd = Some(fd);
        self
    }

    /// Attach the pre-transfer destination metadata so the apply path can
    /// reproduce upstream `dest_mode()` semantics for the chmod-on-rename
    /// loop.
    pub(crate) const fn with_pre_transfer_meta(
        mut self,
        pre_transfer_meta: Option<&'a fs::Metadata>,
    ) -> Self {
        self.pre_transfer_meta = pre_transfer_meta;
        self
    }
}

/// Byte-level statistics from a single file copy (literal and optional
/// compressed sizes).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FileCopyOutcome {
    literal_bytes: u64,
    compressed_bytes: Option<u64>,
}

impl FileCopyOutcome {
    /// Creates a new outcome with the given literal and optional compressed byte counts.
    const fn new(literal_bytes: u64, compressed_bytes: Option<u64>) -> Self {
        Self {
            literal_bytes,
            compressed_bytes,
        }
    }

    /// Returns the number of literal (unmatched) bytes transferred.
    pub(crate) const fn literal_bytes(self) -> u64 {
        self.literal_bytes
    }

    /// Returns the compressed byte count, if compression was used.
    pub(crate) const fn compressed_bytes(self) -> Option<u64> {
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
    const fn new(descriptor: &'a SignatureBlock, canonical_length: usize) -> Self {
        Self {
            descriptor,
            canonical_length,
        }
    }

    /// Returns the matched [`SignatureBlock`].
    const fn descriptor(&self) -> &'a SignatureBlock {
        self.descriptor
    }

    /// Calculates the byte offset of the block within the destination file.
    const fn offset(&self) -> u64 {
        self.descriptor
            .index()
            .saturating_mul(self.canonical_length as u64)
    }
}

/// Groups deferred filesystem operations that execute after file content transfers.
///
/// Three tiers of deferred work:
/// 1. Deletions - directory cleanup with exclusion lists
/// 2. Updates - metadata/permission application to finalized files
/// 3. Created entries - tracking for RAII rollback on timeout errors
#[derive(Default)]
pub(crate) struct DeferredOperationQueue {
    /// Pending directory deletions with keep-lists.
    pub(crate) deletions: Vec<DeferredDeletion>,
    /// Pending metadata/permission updates for transferred files.
    pub(crate) updates: Vec<DeferredUpdate>,
    /// Staging directories (`.~tmp~`) created by `--delay-updates` for cleanup.
    pub(crate) delay_staging_dirs: HashSet<PathBuf>,
    /// Newly created paths tracked for rollback on timeout errors.
    pub(crate) created_entries: Vec<CreatedEntry>,
}

/// A directory deletion deferred until after the transfer phase completes.
pub(crate) struct DeferredDeletion {
    pub(crate) destination: PathBuf,
    pub(crate) relative: Option<PathBuf>,
    pub(crate) keep: Vec<OsString>,
}

/// Owned path and type context for deferred metadata finalization.
///
/// Owned equivalent of [`MetadataPathContext`] for storing in [`DeferredUpdate`]
/// where the paths must outlive the original references.
pub(crate) struct OwnedPathContext {
    pub(crate) source: PathBuf,
    pub(crate) relative: Option<PathBuf>,
    pub(crate) file_type: fs::FileType,
    pub(crate) destination_previously_existed: bool,
}

/// A file update deferred for `--delay-updates` mode, holding the write guard
/// and metadata needed to commit the staged file to its final location.
pub(crate) struct DeferredUpdate {
    guard: DestinationWriteGuard,
    metadata: fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    path_context: OwnedPathContext,
    destination: PathBuf,
    #[cfg(all(unix, feature = "xattr"))]
    preserve_xattrs: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))]
    preserve_acls: bool,
}

impl DeferredUpdate {
    pub(crate) const fn new(
        guard: DestinationWriteGuard,
        metadata: fs::Metadata,
        metadata_options: MetadataOptions,
        mode: LocalCopyExecution,
        path_context: OwnedPathContext,
        destination: PathBuf,
        #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
        #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
    ) -> Self {
        Self {
            guard,
            metadata,
            metadata_options,
            mode,
            path_context,
            destination,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        }
    }
}

/// A filesystem entry created during the transfer, tracked for rollback on
/// timeout or error.
#[derive(Clone, Debug)]
pub(crate) struct CreatedEntry {
    pub(crate) path: PathBuf,
    pub(crate) kind: CreatedEntryKind,
}

/// The type of filesystem entry created during a transfer.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CreatedEntryKind {
    File,
    Directory,
    Symlink,
    Fifo,
    // REASON: device nodes are only created on Unix; non-Unix receivers skip
    // device-node creation entirely (WIND-2), so this variant is never
    // constructed there.
    #[cfg_attr(not(unix), allow(dead_code))]
    Device,
    HardLink,
}

/// Strategy used to place an existing destination into the backup
/// location. Mirrors upstream rsync's `make_backup` success branches
/// (RENAME / COPY / SYMLINK / DEVICE / HLINK) for `--debug=BACKUP`
/// reporting; oc-rsync's local-copy executor exercises only RENAME,
/// COPY (cross-device fallback for regular files), and SYMLINK
/// (cross-device fallback for symlinks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackupStrategy {
    Rename,
    Copy,
    Symlink,
}

include!("context_impl/state.rs");
include!("context_impl/options/metadata.rs");
include!("context_impl/options/transfer.rs");
include!("context_impl/options/dirs.rs");
include!("context_impl/options/filter.rs");
include!("context_impl/options/batch.rs");
include!("context_impl/transfer.rs");
include!("context_impl/delta_transfer.rs");
include!("context_impl/reporting.rs");

/// Outcome of loading one nested `dir-merge` file in a directory: the compiled
/// rule segment (absent when the file had no concrete rules), any
/// `exclude-if-present` markers, and any further nested dir-merge rules it
/// registered.
struct LoadedNestedDirMerge {
    segment: Option<FilterSegment>,
    markers: Vec<ExcludeIfPresentRule>,
    nested: Vec<NestedDirMerge>,
}

/// Rewrites an anchored per-dir merge rule so its leading `/` anchor binds to
/// the merge file's directory (`relative_dir`) rather than the transfer root.
///
/// upstream: exclude.c:200-207 `add_rule` - for a rule loaded via
/// `parse_filter_file` with `XFLG_ANCHORED2ABS`, a leading-`/` pattern gets the
/// directory prefix `dirbuf + module_dirlen` (length
/// `dirbuf_len - module_dirlen - 1`) prepended, so `- /file1` in `foo/.filt`
/// matches `foo/file1`. Unanchored patterns and rules with no directory context
/// are returned unchanged.
fn anchor_dir_merge_rule(rule: FilterRule, relative_dir: Option<&Path>) -> FilterRule {
    let Some(dir) = relative_dir else {
        return rule;
    };
    if dir.as_os_str().is_empty() {
        return rule;
    }
    let pattern = rule.pattern();
    let Some(rest) = pattern.strip_prefix('/') else {
        return rule;
    };
    // `dir` is the transfer-root-relative directory of the merge file; build the
    // anchored pattern `/<dir>/<rest>` using forward slashes (filter patterns are
    // always `/`-delimited regardless of platform separator).
    let dir_str = dir.to_string_lossy().replace('\\', "/");
    let dir_str = dir_str.trim_matches('/');
    let new_pattern = if rest.is_empty() {
        format!("/{dir_str}")
    } else {
        format!("/{dir_str}/{rest}")
    };
    rule.with_pattern(new_pattern)
}

/// A filter segment loaded from a runtime-registered `dir-merge` file, paired
/// with whether the originating rule inherits into subdirectories.
#[derive(Clone, Debug)]
pub(crate) struct LoadedDynamicSegment {
    /// Compiled rules loaded from the merge file in this directory.
    pub(crate) segment: FilterSegment,
    /// `true` unless the dir-merge rule carried the `n` modifier
    /// (`FILTRULE_NO_INHERIT`); inheritable segments propagate to child frames.
    pub(crate) inherit: bool,
}

/// Per-directory frame for runtime-registered `dir-merge` rules.
///
/// upstream: exclude.c:1419-1428 - tracks the cumulative set of nested
/// `dir-merge` directives that are active at the current traversal depth
/// (`active_rules`), and the segments / markers produced by looking those
/// rules up against the directory being entered (`loaded_segments` and
/// `loaded_markers`). The frame is pushed by `enter_directory_for_path` and
/// popped by [`DirectoryFilterGuard`].
#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicDirMergeFrame {
    /// All dynamic dir-merge rules active at this depth, inherited from the
    /// parent frame plus rules newly registered while loading this
    /// directory's merge files.
    pub(crate) active_rules: Vec<NestedDirMerge>,
    /// Filter segments loaded by resolving `active_rules` against the
    /// current directory's filesystem entries, each tagged with whether the
    /// originating dir-merge rule inherits into subdirectories.
    ///
    /// upstream: exclude.c:801 `push_local_filters` sets `lp->tail = NULL` so
    /// rules loaded at an ancestor depth stay in `lp->head` and continue to
    /// match descendants. The frame's loaded segments therefore accumulate the
    /// inheritable segments of every ancestor; segments whose rule carried the
    /// `n` modifier (`FILTRULE_NO_INHERIT`) are flagged non-inheritable and are
    /// dropped from child frames.
    pub(crate) loaded_segments: Vec<LoadedDynamicSegment>,
    /// `exclude-if-present` markers loaded from the same files.
    pub(crate) loaded_markers: Vec<ExcludeIfPresentRule>,
}

/// Shared references to the layered filter stacks, used by
/// [`DirectoryFilterGuard`] to push and pop per-directory filter rules.
#[derive(Clone)]
struct DirectoryFilterHandles {
    layers: Rc<RefCell<FilterSegmentLayers>>,
    marker_layers: Rc<RefCell<ExcludeIfPresentLayers>>,
    ephemeral: Rc<RefCell<FilterSegmentStack>>,
    marker_ephemeral: Rc<RefCell<ExcludeIfPresentStack>>,
    dynamic: Rc<RefCell<Vec<DynamicDirMergeFrame>>>,
}

/// Whole-stack snapshot of the source-visible per-directory filter state.
///
/// Captured before a destination-side deletion scan loads its `dir-merge`
/// files and restored verbatim when the scan's guard drops, so the
/// destination load can never perturb the source-side filter evaluation.
///
/// upstream: delete.c:65-115 `delete_in_dir()` runs the destination
/// per-directory filter push/pop (`push_local_filters`/`pop_local_filters`)
/// against a separate `delete_filt` chain seeded from `change_local_filter_dir`;
/// the receiver's destination scan never mutates the sender's `filter_list`.
/// We mirror that isolation by snapshotting and restoring the source-visible
/// stacks around the destination scan rather than relying on a per-index pop,
/// which cannot balance the nested `dir-merge` directives a destination merge
/// file may register (exclude.c:801 `push_local_filters` seeds `lp->head` from
/// rules an arbitrary number of indices deep).
struct FilterStateSnapshot {
    layers: FilterSegmentLayers,
    marker_layers: ExcludeIfPresentLayers,
    ephemeral: FilterSegmentStack,
    marker_ephemeral: ExcludeIfPresentStack,
    dynamic: Vec<DynamicDirMergeFrame>,
}

/// RAII guard that reverts per-directory filter rules when dropped.
///
/// Pushing dir-merge rules into the layered filter stacks yields this guard.
/// On drop, all rules pushed for the directory are popped, restoring the
/// filter state to what it was before entering the directory.
///
/// When `restore` is set (destination-deletion scans), the guard restores the
/// captured [`FilterStateSnapshot`] wholesale instead of running the per-index
/// pop logic, guaranteeing source-side state is left untouched.
pub(crate) struct DirectoryFilterGuard {
    handles: DirectoryFilterHandles,
    indices: Vec<usize>,
    marker_counts: Vec<(usize, usize)>,
    ephemeral_active: bool,
    dynamic_active: bool,
    excluded: bool,
    restore: Option<FilterStateSnapshot>,
}

impl DirectoryFilterGuard {
    const fn new(
        handles: DirectoryFilterHandles,
        indices: Vec<usize>,
        marker_counts: Vec<(usize, usize)>,
        ephemeral_active: bool,
        dynamic_active: bool,
        excluded: bool,
    ) -> Self {
        Self {
            handles,
            indices,
            marker_counts,
            ephemeral_active,
            dynamic_active,
            excluded,
            restore: None,
        }
    }

    /// Returns `true` if the directory was excluded by a filter rule.
    pub(crate) const fn is_excluded(&self) -> bool {
        self.excluded
    }

    /// Arms the guard to restore the captured source-visible filter state on
    /// drop instead of running the per-index pop logic. Used by the
    /// destination-deletion path so the destination merge load is isolated from
    /// source-side evaluation.
    fn arm_restore(&mut self, snapshot: FilterStateSnapshot) {
        self.restore = Some(snapshot);
    }
}

impl Drop for DirectoryFilterGuard {
    fn drop(&mut self) {
        if let Some(snapshot) = self.restore.take() {
            *self.handles.layers.borrow_mut() = snapshot.layers;
            *self.handles.marker_layers.borrow_mut() = snapshot.marker_layers;
            *self.handles.ephemeral.borrow_mut() = snapshot.ephemeral;
            *self.handles.marker_ephemeral.borrow_mut() = snapshot.marker_ephemeral;
            *self.handles.dynamic.borrow_mut() = snapshot.dynamic;
            return;
        }

        if self.dynamic_active {
            let mut stack = self.handles.dynamic.borrow_mut();
            stack.pop();
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The XMP invariant: parallelising the per-entry deletion matcher must not
    /// change WHICH entries are deleted nor the ORDER they are emitted - only
    /// WHERE the pure `allows_deletion` decision is computed. This test feeds an
    /// identical candidate set through serial and rayon `par_iter` evaluation of
    /// the same immutable [`DeletionFilterSnapshot`] (built from a nested
    /// dir-merge fixture: a global `- *.deep` exclude plus a dynamic `- secret`
    /// segment) and asserts the decision SET and the name-sorted emission ORDER
    /// are byte-for-byte identical. If they ever diverge, the wire output would
    /// no longer match upstream rsync, so this test must fail.
    #[test]
    fn deletion_snapshot_parallel_matches_serial_set_and_order() {
        use crate::local_copy::filter_program::FilterProgramEntry;
        use filters::FilterRule;
        use rayon::prelude::*;
        use std::path::Path;

        let program = FilterProgram::new([FilterProgramEntry::Rule(FilterRule::exclude("*.deep"))])
            .expect("filter program builds");
        let mut dynamic_segment = FilterSegment::default();
        dynamic_segment
            .push_rule(FilterRule::exclude("secret"))
            .expect("dynamic rule compiles");

        let snapshot = DeletionFilterSnapshot {
            program: Some(program),
            layers: FilterSegmentLayers::new(),
            ephemeral_last: None,
            dynamic_loaded_segments: vec![LoadedDynamicSegment {
                segment: dynamic_segment,
                inherit: true,
            }],
            filter_set: None,
            delete_excluded: false,
        };

        // A wide directory: `*.deep` and `secret` are filter-protected (must NOT
        // delete), everything else is deletable. The mix guarantees both
        // outcomes are exercised so the test can fail if either path regresses.
        let candidates: Vec<(String, bool)> = (0..200)
            .map(|i| {
                let name = if i % 3 == 0 {
                    format!("f{i}.deep")
                } else if i % 7 == 0 {
                    "secret".to_string()
                } else {
                    format!("f{i}.txt")
                };
                (name, false)
            })
            .collect();

        let decide = |name: &str, is_dir: bool| snapshot.allows_deletion(Path::new(name), is_dir);

        let serial: Vec<bool> = candidates
            .iter()
            .map(|(name, is_dir)| decide(name, *is_dir))
            .collect();
        let parallel: Vec<bool> = candidates
            .par_iter()
            .map(|(name, is_dir)| decide(name, *is_dir))
            .collect();

        // Decision SET parity: par_iter preserves index order, so the aligned
        // decision vectors must be identical element-for-element.
        assert_eq!(serial, parallel, "parallel decisions diverged from serial");

        // Sanity: the fixture must produce both outcomes, otherwise the parity
        // assertion above could pass vacuously.
        assert!(serial.iter().any(|&d| d), "expected some deletable entries");
        assert!(
            serial.iter().any(|&d| !d),
            "expected some protected entries"
        );
        assert!(!decide("secret", false), "dynamic - secret must protect");
        assert!(!decide("f0.deep", false), "global - *.deep must protect");
        assert!(decide("f1.txt", false), "unmatched entry must be deletable");

        // Emission ORDER parity: the plan emits deletable names name-sorted.
        // Derive that order from both decision vectors; they must match.
        let emission = |decisions: &[bool]| -> Vec<String> {
            let mut names: Vec<String> = candidates
                .iter()
                .zip(decisions)
                .filter_map(|((name, _), &keep)| keep.then(|| name.clone()))
                .collect();
            names.sort_unstable();
            names
        };
        assert_eq!(
            emission(&serial),
            emission(&parallel),
            "parallel emission order diverged from serial"
        );
    }

    #[test]
    fn file_copy_outcome_new_stores_values() {
        let outcome = FileCopyOutcome::new(1000, Some(500));
        assert_eq!(outcome.literal_bytes(), 1000);
        assert_eq!(outcome.compressed_bytes(), Some(500));
    }

    #[test]
    fn file_copy_outcome_new_without_compression() {
        let outcome = FileCopyOutcome::new(2000, None);
        assert_eq!(outcome.literal_bytes(), 2000);
        assert!(outcome.compressed_bytes().is_none());
    }

    #[test]
    fn file_copy_outcome_zero_bytes() {
        let outcome = FileCopyOutcome::new(0, Some(0));
        assert_eq!(outcome.literal_bytes(), 0);
        assert_eq!(outcome.compressed_bytes(), Some(0));
    }

    #[test]
    fn file_copy_outcome_default_is_zero() {
        let outcome = FileCopyOutcome::default();
        assert_eq!(outcome.literal_bytes(), 0);
        assert!(outcome.compressed_bytes().is_none());
    }

    #[test]
    fn file_copy_outcome_clone() {
        let outcome = FileCopyOutcome::new(100, Some(50));
        let cloned = outcome;
        assert_eq!(cloned.literal_bytes(), 100);
        assert_eq!(cloned.compressed_bytes(), Some(50));
    }

    #[test]
    fn file_copy_outcome_debug_format() {
        let outcome = FileCopyOutcome::new(100, None);
        let debug = format!("{outcome:?}");
        assert!(debug.contains("FileCopyOutcome"));
        assert!(debug.contains("100"));
    }

    #[test]
    fn file_copy_outcome_eq() {
        let a = FileCopyOutcome::new(100, Some(50));
        let b = FileCopyOutcome::new(100, Some(50));
        let c = FileCopyOutcome::new(100, Some(60));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn created_entry_kind_file_debug() {
        let kind = CreatedEntryKind::File;
        let debug = format!("{kind:?}");
        assert!(debug.contains("File"));
    }

    #[test]
    fn created_entry_kind_directory_debug() {
        let kind = CreatedEntryKind::Directory;
        let debug = format!("{kind:?}");
        assert!(debug.contains("Directory"));
    }

    #[test]
    fn created_entry_kind_symlink_debug() {
        let kind = CreatedEntryKind::Symlink;
        let debug = format!("{kind:?}");
        assert!(debug.contains("Symlink"));
    }

    #[test]
    fn created_entry_kind_fifo_debug() {
        let kind = CreatedEntryKind::Fifo;
        let debug = format!("{kind:?}");
        assert!(debug.contains("Fifo"));
    }

    #[test]
    fn created_entry_kind_device_debug() {
        let kind = CreatedEntryKind::Device;
        let debug = format!("{kind:?}");
        assert!(debug.contains("Device"));
    }

    #[test]
    fn created_entry_kind_hard_link_debug() {
        let kind = CreatedEntryKind::HardLink;
        let debug = format!("{kind:?}");
        assert!(debug.contains("HardLink"));
    }

    #[test]
    fn created_entry_kind_clone() {
        let kind = CreatedEntryKind::File;
        let cloned = kind;
        assert!(matches!(cloned, CreatedEntryKind::File));
    }

    #[test]
    fn created_entry_kind_copy() {
        let kind = CreatedEntryKind::Directory;
        let copied = kind;
        // Original still usable after copy
        assert!(matches!(kind, CreatedEntryKind::Directory));
        assert!(matches!(copied, CreatedEntryKind::Directory));
    }

    #[test]
    fn created_entry_debug_contains_path() {
        let entry = CreatedEntry {
            path: PathBuf::from("/test/path"),
            kind: CreatedEntryKind::File,
        };
        let debug = format!("{entry:?}");
        assert!(debug.contains("CreatedEntry"));
        assert!(debug.contains("/test/path"));
        assert!(debug.contains("File"));
    }

    #[test]
    fn created_entry_clone() {
        let entry = CreatedEntry {
            path: PathBuf::from("/some/path"),
            kind: CreatedEntryKind::Symlink,
        };
        let cloned = entry;
        assert_eq!(cloned.path, PathBuf::from("/some/path"));
        assert!(matches!(cloned.kind, CreatedEntryKind::Symlink));
    }

    #[test]
    fn deferred_deletion_creation() {
        let deletion = DeferredDeletion {
            destination: PathBuf::from("/dest"),
            relative: Some(PathBuf::from("rel")),
            keep: vec![OsString::from("file1"), OsString::from("file2")],
        };
        assert_eq!(deletion.destination, PathBuf::from("/dest"));
        assert_eq!(deletion.relative, Some(PathBuf::from("rel")));
        assert_eq!(deletion.keep.len(), 2);
    }

    #[test]
    fn deferred_deletion_no_relative() {
        let deletion = DeferredDeletion {
            destination: PathBuf::from("/dest"),
            relative: None,
            keep: Vec::new(),
        };
        assert!(deletion.relative.is_none());
        assert!(deletion.keep.is_empty());
    }
}
