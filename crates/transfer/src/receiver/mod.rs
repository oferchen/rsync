#![deny(unsafe_code)]
//! Server-side Receiver role implementation.
//!
//! When the native server operates as a Receiver, it:
//! 1. Receives the file list from the client (sender)
//! 2. Generates signatures for existing local files
//! 3. Receives delta data and applies it to create/update files
//! 4. Sets metadata (permissions, times, ownership) on received files
//!
//! # Upstream Reference
//!
//! - `receiver.c:340` - `receive_data()` - Delta application logic
//! - `receiver.c:720` - `recv_files()` - Main file reception loop
//! - `generator.c:1450` - `recv_generator()` - Signature generation
//!
//! Mirrors upstream rsync's receiver behavior with block-by-block delta
//! application and atomic file updates via temporary files.
//!
//! # Implementation Guide
//!
//! For comprehensive documentation on how the receiver works within the delta transfer
//! algorithm, see the [`crate::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::generator`] - The generator role that sends deltas to the receiver
//! - [`engine::delta`] - Delta generation and application engine
//! - [`engine::signature`] - Signature generation utilities
//! - [`metadata::apply_metadata_from_file_entry`] - Metadata preservation
//! - [`protocol::wire`] - Wire format for signatures and deltas

mod basis;
mod directory;
#[cfg(feature = "flat-flist")]
pub mod entry_accessor;
mod file_list;
mod quick_check;
mod stats;
#[cfg(test)]
mod tests;
mod transfer;
mod wire;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use filters::{FilterChain, FilterSet};
use protocol::acl::AclCache;
use protocol::filters::FilterRuleWireFormat;
use protocol::flist::{FileEntry, FileListReader};
use protocol::idlist::IdList;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::HardlinkApplyTracker;
use engine::delete::DeleteContext;

use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::shared::ChecksumFactory;
use crate::transfer_state::TransferPipeline;

pub use self::basis::{BasisFileConfig, BasisFileResult, find_basis_file_with_config};
pub use self::file_list::IncrementalFileListReceiver;
pub use self::stats::{SenderStats, TransferStats};
pub use self::wire::{
    SenderAttrs, SumHead, apply_xattr_abbreviation_values, write_signature_blocks,
    write_xattr_request,
};

/// Phase 1 checksum length (2 bytes) - reduced signature overhead.
///
/// Upstream rsync uses `SHORT_SUM_LENGTH` (2) during phase 1 to reduce
/// signature wire size. The `derive_strong_sum_length()` heuristic computes
/// a dynamic length between 2-16 bytes based on file and block sizes.
///
/// (upstream: generator.c:2157 `csum_length = SHORT_SUM_LENGTH`)
const PHASE1_CHECKSUM_LENGTH: NonZeroU8 =
    NonZeroU8::new(signature::block_size::SHORT_SUM_LENGTH).unwrap();

/// Phase 2 redo checksum length (16 bytes) - full collision resistance.
///
/// Upstream rsync switches to `SUM_LENGTH` (16) for phase 2 redo to ensure
/// maximum integrity after a whole-file checksum failure.
///
/// (upstream: generator.c:2163 `csum_length = SUM_LENGTH`)
const REDO_CHECKSUM_LENGTH: NonZeroU8 =
    NonZeroU8::new(signature::block_size::MAX_SUM_LENGTH).unwrap();

pub(crate) use crate::parallel_io::ParallelThresholds;

use signature;

/// Total invocations of [`ReceiverContext::flat_to_wire_ndx`] across all
/// receiver transfers in this process. Diagnostic counter for receiver-side
/// INC_RECURSE (#2199, I4) - quantifies how often the wire/flat conversion
/// hot path fires per transfer relative to the file count.
///
/// Sampled at end-of-transfer in the receiver finalize path via
/// [`ndx_convert_totals`] and emitted via `tracing::debug!`.
static NDX_CONVERT_CALLS: AtomicU64 = AtomicU64::new(0);

/// Cumulative `partition_point` comparison depth (approximated as
/// `floor(log2(len)) + 1`) summed across every NDX conversion call.
/// Diagnostic counter for receiver-side INC_RECURSE (#2199, I4) - lets
/// operators see when the segment table grows large enough for the
/// binary-search cost to matter.
static NDX_CONVERT_CMPS: AtomicU64 = AtomicU64::new(0);

/// Approximate number of comparisons a binary search performs on a sorted
/// slice of length `len`. Returns `floor(log2(len)) + 1`, matching the worst
/// case of `[T]::partition_point` on the segment table.
fn partition_point_depth(len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    u64::from((len as u64).ilog2()) + 1
}

/// Snapshot of the global NDX conversion counters.
///
/// Returns `(call_count, cumulative_partition_point_depth)`. Used by the
/// receiver finalize path to emit an end-of-transfer diagnostic line and by
/// unit tests that assert the counters monotonically grow.
#[must_use]
pub fn ndx_convert_totals() -> (u64, u64) {
    (
        NDX_CONVERT_CALLS.load(Ordering::Relaxed),
        NDX_CONVERT_CMPS.load(Ordering::Relaxed),
    )
}

/// Reports whether a destination operand was written with a trailing path
/// separator.
///
/// Upstream rsync inspects the raw `dest_path` argument (`main.c:724-725`)
/// after a final `strrchr('/')` to decide whether the operand ends with a
/// directory marker. The detection is byte-level on Unix and matches either
/// `'/'` or `'\\'` on Windows so paths produced by either separator convention
/// are honored.
pub(in crate::receiver) fn dest_arg_has_trailing_slash(arg: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        arg.as_bytes().last() == Some(&b'/')
    }
    #[cfg(windows)]
    {
        let bytes = arg.as_encoded_bytes();
        matches!(bytes.last(), Some(b'/') | Some(b'\\'))
    }
    #[cfg(not(any(unix, windows)))]
    {
        arg.to_string_lossy().ends_with('/')
    }
}

/// Creates the destination root directory when the transfer needs one.
///
/// Mirrors upstream `main.c:778-792` (`get_local_name()`): when the receiver
/// is about to write more than one file, or the destination operand carries a
/// trailing slash, the root must exist as a directory before per-entry mkdir
/// dispatch. The local-mode receiver gets this for free via the file-list-
/// driven implicit mkdir, but the `--server` path never created the root, so
/// the alt-dest upstream interop test that runs over remote-shell failed when
/// the destination did not already exist.
///
/// Returns `Ok(true)` when a new directory was created, `Ok(false)` when the
/// pre-flight was a no-op (already exists, single-file transfer without
/// trailing slash, or `dry_run`).
///
/// # Symlink refusal
///
/// The existence check uses `symlink_metadata()` (lstat) rather than
/// `metadata()` (stat) so a symlink at `dest_root` is detected directly
/// instead of being silently resolved. When `dest_root` is itself a symlink -
/// broken or otherwise - the helper refuses with `InvalidInput` instead of
/// proceeding. A broken symlink would fall through the stat-based check
/// (NotFound at the target) and let `create_dir_all` resolve through the
/// symlink, materializing the directory outside the intended location; a
/// dangling-link-class containment bypass analogous to the SEC-1 TOCTOU
/// family. An existing-symlink dest is equally suspect because every
/// per-entry write would land at the symlink target, sidestepping the
/// daemon's `module.path` containment. Operators that genuinely need to
/// receive into a symlinked location must materialize the real directory
/// themselves; oc-rsync never auto-creates through a symlink.
///
/// # Upstream Reference
///
/// - `main.c:778-792` - `get_local_name()` pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
/// - `main.c:794-796` - sets `FLAG_DIR_CREATED` on the first flist entry when
///   its basename is `.` (deferred follow-up; oc-rsync's delete path does
///   not currently consume that flag).
pub fn ensure_dest_root_exists(
    dest_root: &Path,
    file_total: usize,
    trailing_slash: bool,
    dry_run: bool,
) -> io::Result<bool> {
    if dry_run {
        return Ok(false);
    }
    if file_total <= 1 && !trailing_slash {
        return Ok(false);
    }
    // lstat instead of stat so a symlink at `dest_root` is observed directly.
    // Upstream `main.c:778-792` does not pre-flight a symlinked dest_path
    // (`do_stat` follows the link and either reports the target dir or
    // returns ENOENT, after which `do_mkdir` resolves through the symlink).
    // We refuse instead: auto-creating through an attacker-planted symlink
    // would resolve to whatever the link points at, defeating the
    // module-root containment that the SEC-1 `*at` chain enforces for every
    // subsequent per-entry write.
    match dest_root.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing to create destination root '{}' through a symlink: \
                 auto-creating at the symlink target would bypass module \
                 containment (UTS-2 follow-up to PR #5567)",
                dest_root.display(),
            ),
        )),
        Ok(_) => Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dest_root).map(|()| true)
        }
        Err(err) => Err(err),
    }
}

/// Context for the receiver role during a transfer.
///
/// Holds protocol state, configuration, and the file list needed to drive
/// the receive loop. Created via [`ReceiverContext::new`] from a completed
/// [`HandshakeResult`] and [`ServerConfig`], then executed with [`ReceiverContext::run`].
///
/// See the [module-level documentation](crate::receiver) for the full receive workflow.
#[derive(Debug)]
pub struct ReceiverContext {
    /// Negotiated protocol version.
    protocol: ProtocolVersion,
    /// Server configuration.
    config: ServerConfig,
    /// List of files to receive.
    file_list: Vec<FileEntry>,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    ///
    /// Controls protocol-specific behaviors like incremental recursion (`INC_RECURSE`),
    /// checksum seed ordering (`CHECKSUM_SEED_FIX`), and file list encoding (`VARINT_FLIST_FLAGS`).
    /// None for protocols < 30 or when compat exchange was skipped.
    compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    checksum_seed: i32,
    /// Segment boundary table for mapping flat array indices to wire NDX values.
    ///
    /// With INC_RECURSE, each segment has `ndx_start = prev_ndx_start + prev_used + 1`.
    /// Each entry is `(flat_start, ndx_start)`.
    /// Without INC_RECURSE, contains a single entry `(0, 0)`.
    ///
    /// upstream: flist.c:2931 - `flist->ndx_start = prev->ndx_start + prev->used + 1`
    ndx_segments: Vec<(usize, i32)>,
    /// Index into `ndx_segments` of the oldest unreclaimed segment.
    ///
    /// Advances by one each time a completed segment is reclaimed via
    /// `reclaim_oldest_segment()`. Mirrors upstream's `first_flist`
    /// pointer which advances as segments are freed by `flist_free()`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:101` - `first_flist` pointer
    /// - `receiver.c:573` - `flist_free(first_flist)` advances `first_flist`
    first_segment_idx: usize,
    /// Cached file list reader for compression state continuity across sub-lists.
    ///
    /// Upstream rsync uses `static` variables in `recv_file_entry()` that persist
    /// across `recv_file_list()` calls. This field preserves the same state
    /// (prev_name, prev_mode, prev_uid, prev_gid) between `receive_file_list()`
    /// and `receive_extra_file_lists()`.
    flist_reader_cache: Option<FileListReader>,
    /// UID mappings from remote to local IDs.
    uid_list: IdList,
    /// GID mappings from remote to local IDs.
    gid_list: IdList,
    /// Compiled daemon-side filter rules from rsyncd.conf module configuration.
    ///
    /// Built from `ServerConfig::daemon_filter_rules` at construction time.
    /// Used to reject daemon-excluded files before accepting transfers and
    /// to prepend server-side rules to the client filter chain for deletion.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:599-604` - `check_filter(&daemon_filter_list, ...)` rejects
    ///   excluded files before accepting transfer data
    /// - `flist.c:254-272` - `path_is_daemon_excluded()` checks each path
    ///   component against the daemon filter list
    daemon_filter_set: Option<FilterSet>,
    /// Per-directory scoped filter chain for deletion protection.
    ///
    /// Used by `delete_extraneous_files()` to check `allows_deletion()` before
    /// removing destination files not present in the sender's file list. Rules
    /// include global protect/risk rules and per-directory merge file rules.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` - `is_excluded()` check before deletion
    filter_chain: FilterChain,
    /// Tracker for hardlink leader/follower relationships during file apply.
    ///
    /// Records committed leader paths so followers can be hard-linked to them.
    /// Initialized when `--hard-links` is active; `None` otherwise.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:finish_hard_link()` - links deferred followers after leader commit
    /// - `hlink.c:hard_link_check()` - defers follower when leader in-progress
    hardlink_tracker: Option<HardlinkApplyTracker>,
    /// Persistent hardlink group state across INC_RECURSE segments.
    ///
    /// Maps gnum (hardlink group number) to whether it has been seen. Populated
    /// during `receive_file_list` and carried across `receive_extra_file_lists`
    /// so that cross-directory hardlink followers are not incorrectly promoted
    /// to leaders when their leader was received in a previous segment.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:match_gnums()` - `prior_hlinks` hashtable persists across segments
    prior_hlinks: HashMap<u32, bool>,
    /// Accumulated I/O error flags from the sender's file list for protocol < 30.
    ///
    /// For protocol < 30, the sender writes a 4-byte LE io_error flag after the
    /// id lists (upstream: flist.c:2517-2518). Protocol >= 30 uses MSG_IO_ERROR
    /// or SAFE_FILE_LIST instead.
    flist_io_error: i32,
    /// Per-operation thresholds for switching between sequential and parallel execution.
    ///
    /// Different operations have different overhead profiles: CPU-bound signature
    /// computation benefits from parallelism at lower counts than I/O-bound stat calls.
    parallel_thresholds: ParallelThresholds,
    /// Optional handle into the parallel-deterministic-delete pipeline.
    ///
    /// When `Some`, the receiver publishes a [`engine::delete::DeletePlan`]
    /// for every INC_RECURSE segment via
    /// [`DeleteContext::observe_segment_for_delete`]. The plans accumulate
    /// in the shared [`engine::delete::DeletePlanMap`] for the (not-yet-
    /// active) emitter to drain. When `None`, the receiver behaves
    /// identically to the legacy batched-sweep path; nothing in the
    /// segment loop calls into the delete pipeline.
    ///
    /// This is wired by task DDP-B3 (#2257) and consumed by the emitter
    /// wiring in tasks DDP-E1-E5.
    delete_ctx: Option<Arc<DeleteContext>>,
    /// Transfer pipeline FSM tracking the current protocol phase.
    ///
    /// Enforces the linear phase progression through the transfer lifecycle.
    /// Initialized at `FilterExchange` by `run_server_with_handshake` and
    /// advanced through `FileListTransfer`, `DeltaTransfer`, `Finalization`,
    /// and `Complete` as the receiver progresses.
    pipeline: TransferPipeline,
}

impl ReceiverContext {
    /// Creates a new receiver context from a completed handshake and server config.
    ///
    /// Initializes protocol state, INC_RECURSE NDX offset, and empty file list.
    /// Compiles daemon filter rules from `ServerConfig::daemon_filter_rules` into
    /// a `FilterSet` for per-file exclusion checking during transfer.
    /// Execute the transfer via [`run`](Self::run).
    ///
    /// The `pipeline` parameter carries the transfer FSM state from the
    /// orchestration layer. It should be at `FilterExchange` when the
    /// receiver is created.
    #[must_use]
    pub fn new(
        handshake: &HandshakeResult,
        config: ServerConfig,
        pipeline: TransferPipeline,
    ) -> Self {
        // upstream: flist.c:2923 - ndx_start = inc_recurse ? 1 : 0
        let inc_recurse = handshake
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        let initial_ndx_start = if inc_recurse { 1 } else { 0 };

        let hardlink_tracker = if config.flags.hard_links {
            Some(HardlinkApplyTracker::new())
        } else {
            None
        };

        // upstream: clientserver.c:874-893 - daemon_filter_list is built from
        // module filter/exclude/include directives and used by all roles.
        let daemon_filter_set = compile_daemon_filter_set(&config.daemon_filter_rules);

        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
            ndx_segments: vec![(0, initial_ndx_start)],
            first_segment_idx: 0,
            flist_reader_cache: None,
            uid_list: IdList::new(),
            gid_list: IdList::new(),
            daemon_filter_set,
            filter_chain: FilterChain::empty(),
            hardlink_tracker,
            prior_hlinks: HashMap::new(),
            flist_io_error: 0,
            parallel_thresholds: ParallelThresholds::default(),
            delete_ctx: None,
            pipeline,
        }
    }

    /// Creates a receiver context for unit testing with a default pipeline.
    ///
    /// The pipeline is initialized at `FilterExchange`, matching the state
    /// when a real `run_server_with_handshake` dispatches to the receiver.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new_for_test(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        let mut pipeline = TransferPipeline::new(crate::role::ServerRole::Receiver);
        pipeline
            .advance_to(crate::transfer_state::TransferPhase::FilterExchange)
            .expect("test pipeline advance");
        Self::new(handshake, config, pipeline)
    }

    /// Attaches a [`DeleteContext`] to the receiver.
    ///
    /// When set, the receiver's per-segment hook publishes one
    /// [`engine::delete::DeletePlan`] per INC_RECURSE segment into the
    /// context's shared [`engine::delete::DeletePlanMap`]. Plans
    /// accumulate for later consumption by the emitter (tasks
    /// DDP-E1-E5); the legacy batched-sweep path remains active and
    /// continues to drive observable deletions until the emitter takes
    /// over.
    ///
    /// Pass `None` to detach the context. Must be called before
    /// [`run`](Self::run) - the context is consumed on each segment.
    pub fn set_delete_context(&mut self, ctx: Option<Arc<DeleteContext>>) {
        self.delete_ctx = ctx;
    }

    /// Returns a clone of the current [`DeleteContext`] handle, if any.
    #[must_use]
    pub fn delete_context(&self) -> Option<Arc<DeleteContext>> {
        self.delete_ctx.as_ref().map(Arc::clone)
    }

    /// Converts a wire NDX value to a flat file list array index.
    ///
    /// Inverse of [`Self::flat_to_wire_ndx`]. Walks the segment table
    /// (`ndx_segments`) to find the segment owning `wire_ndx` and
    /// computes the flat offset within it. Returns `None` when the wire
    /// NDX falls outside every segment's range (for example NDX 0 under
    /// INC_RECURSE, which is reserved).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2931` - segment table layout used to do the inverse
    ///   mapping.
    pub(in crate::receiver) fn wire_to_flat_ndx(&self, wire_ndx: i32) -> Option<usize> {
        let segments = &self.ndx_segments;
        // Find the segment whose `ndx_start` is <= wire_ndx.
        let seg_idx = segments
            .partition_point(|&(_, ns)| ns <= wire_ndx)
            .checked_sub(1)?;
        let (flat_start, ndx_start) = segments[seg_idx];
        if wire_ndx < ndx_start {
            return None;
        }
        let offset = (wire_ndx - ndx_start) as usize;
        let flat_idx = flat_start + offset;
        // Bound by the next segment's flat_start (or file_list len for
        // the last segment), so we never return an index past the end
        // of the segment we located.
        let seg_end = segments
            .get(seg_idx + 1)
            .map(|&(start, _)| start)
            .unwrap_or(self.file_list.len());
        if flat_idx >= seg_end {
            return None;
        }
        Some(flat_idx)
    }

    /// Converts a flat file list array index to a wire NDX value.
    ///
    /// Updates the [`NDX_CONVERT_CALLS`] / [`NDX_CONVERT_CMPS`] counters used
    /// for INC_RECURSE diagnostic I4 (#2199).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2321` - `ndx = i + cur_flist->ndx_start`
    pub(in crate::receiver) fn flat_to_wire_ndx(&self, flat_idx: usize) -> i32 {
        let segments = &self.ndx_segments;
        NDX_CONVERT_CALLS.fetch_add(1, Ordering::Relaxed);
        NDX_CONVERT_CMPS.fetch_add(partition_point_depth(segments.len()), Ordering::Relaxed);
        let seg_idx = segments.partition_point(|&(start, _)| start <= flat_idx) - 1;
        let (flat_start, ndx_start) = segments[seg_idx];
        ndx_start + (flat_idx - flat_start) as i32
    }

    /// Returns the negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a reference to the server configuration.
    #[must_use]
    pub const fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Returns the file-list checksum algorithm based on negotiation and protocol.
    ///
    /// upstream: checksum.c - `file_sum_nni` selects algorithm for file checksums
    #[must_use]
    const fn get_checksum_algorithm(&self) -> protocol::ChecksumAlgorithm {
        if let Some(negotiated) = &self.negotiated_algorithms {
            negotiated.checksum
        } else if self.protocol.uses_varint_encoding() {
            protocol::ChecksumAlgorithm::MD5
        } else {
            protocol::ChecksumAlgorithm::MD4
        }
    }

    /// Builds a [`BasisFileConfig`] for a single file, pulling shared state from `self`.
    fn build_basis_file_config<'a>(
        &'a self,
        file_path: &'a std::path::Path,
        dest_dir: &'a std::path::Path,
        relative_path: &'a std::path::Path,
        target_size: u64,
        checksum_length: NonZeroU8,
        checksum_algorithm: signature::SignatureAlgorithm,
    ) -> BasisFileConfig<'a> {
        BasisFileConfig {
            file_path,
            dest_dir,
            relative_path,
            target_size,
            fuzzy_level: self.config.flags.fuzzy_level,
            reference_directories: &self.config.reference_directories,
            protocol: self.protocol,
            checksum_length,
            checksum_algorithm,
            whole_file: self.config.flags.whole_file,
        }
    }

    /// Returns the negotiated compatibility flags.
    ///
    /// Returns `None` for protocols < 30 or when compat exchange was skipped.
    /// The flags control protocol-specific behaviors like incremental recursion,
    /// checksum seed ordering, and file list encoding.
    pub const fn compat_flags(&self) -> Option<protocol::CompatibilityFlags> {
        self.compat_flags
    }

    /// Returns the received file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
    }

    /// Creates a configured `FileListReader` matching the current protocol and flags.
    fn build_flist_reader(&self) -> FileListReader {
        let mut reader = if let Some(flags) = self.compat_flags {
            FileListReader::with_compat_flags(self.protocol, flags)
        } else {
            FileListReader::new(self.protocol)
        }
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_specials(self.config.flags.specials)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_acls(self.config.flags.acls)
        .with_preserve_xattrs(self.config.flags.xattrs)
        .with_preserve_atimes(self.config.flags.atimes)
        .with_relative_paths(self.config.flags.relative);

        // upstream: flist.c - always_checksum includes per-file checksums in the file list
        if self.config.flags.checksum {
            let factory = ChecksumFactory::from_negotiation(
                self.negotiated_algorithms.as_ref(),
                self.protocol,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );
            reader = reader.with_always_checksum(factory.digest_length());
        }

        if let Some(ref converter) = self.config.connection.iconv {
            reader = reader.with_iconv(converter.clone());
        }

        reader
    }

    /// Returns true when iconv is active and would transcode filenames,
    /// indicating the receiver must keep its NDX-addressed file list in
    /// sender wire-emit order rather than re-sorting on local-charset bytes.
    ///
    /// Mirrors upstream's `need_unsorted_flist = 1` flag, which `options.c`
    /// sets whenever `iconv_opt` resolves to an actual conversion. An
    /// identity converter (same local/remote encoding) leaves bytes
    /// untouched, so the sort/lookup order cannot diverge and the reorder
    /// stays enabled - matching upstream's check that nulls out `iconv_opt`
    /// when it is `"-"` before setting `need_unsorted_flist = 1`.
    ///
    /// # Upstream Reference
    ///
    /// - `options.c:2051-2056` - `need_unsorted_flist = 1` when `iconv_opt`
    /// - `flist.c:2496-2498` - "both sides keep an unsorted file-list array
    ///   because the names will differ on the sending and receiving sides"
    /// - `flist.c:2149-2153` - allocates a separate `flist->sorted[]`
    ///   pointer array so `flist->files[]` stays in scan order
    pub(in crate::receiver) fn iconv_reorder_suppressed(&self) -> bool {
        self.config
            .connection
            .iconv
            .as_ref()
            .is_some_and(|converter| !converter.is_identity())
    }

    /// Translates a remote UID to a local UID using the received mappings.
    ///
    /// Returns the mapped local UID if a mapping exists, otherwise returns the
    /// remote UID unchanged (falling back to numeric ID).
    #[inline]
    #[must_use]
    pub fn match_uid(&self, remote_uid: u32) -> u32 {
        self.uid_list.match_id(remote_uid)
    }

    /// Translates a remote GID to a local GID using the received mappings.
    ///
    /// Returns the mapped local GID if a mapping exists, otherwise returns the
    /// remote GID unchanged (falling back to numeric ID).
    #[inline]
    #[must_use]
    pub fn match_gid(&self, remote_gid: u32) -> u32 {
        self.gid_list.match_id(remote_gid)
    }

    /// Resolves the xattr list for a file entry from the cached `FileListReader`.
    ///
    /// Returns `None` if xattrs are not being preserved, if the file entry has no
    /// xattr index, or if the cache lookup fails. The returned `XattrList` is
    /// cloned from the cache for use by the disk commit thread.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `xattrs.c:set_xattr()` which looks up `F_XATTR(file)` in the
    /// global xattr list cache `rsync_xal_l`.
    fn resolve_xattr_list(
        &self,
        entry: &protocol::flist::FileEntry,
    ) -> Option<protocol::xattr::XattrList> {
        if !self.config.flags.xattrs {
            return None;
        }
        let ndx = entry.xattr_ndx()?;
        let reader = self.flist_reader_cache.as_ref()?;
        reader.xattr_cache().get(ndx as usize).cloned()
    }

    /// Determines if input multiplex should be activated based on mode and protocol.
    ///
    /// The activation threshold differs by mode:
    ///
    /// **Client mode** (daemon pull - `main.c:1342-1343` `client_run !am_sender`):
    /// - `if (protocol_version >= 23) io_start_multiplex_in(f_in);`
    ///
    /// **Server mode** (daemon/SSH receiver - `main.c:1167-1168` `do_recv`):
    /// - `if (protocol_version >= 30) io_start_multiplex_in(f_in);`
    /// - Protocol < 30 uses `io_start_buffering_in()` instead (no multiplex).
    #[must_use]
    pub(crate) const fn should_activate_input_multiplex(&self) -> bool {
        if self.config.connection.client_mode {
            // Client mode: >= 23 (upstream main.c:1342-1343)
            self.protocol.supports_multiplex_io()
        } else {
            // Server mode: >= 30 (upstream main.c:1167-1168)
            self.protocol.uses_binary_negotiation()
        }
    }

    /// Determines if filter list should be read from sender.
    ///
    /// For a daemon receiver, the filter list is only read when:
    /// - `prune_empty_dirs` is enabled, OR
    /// - `delete_mode` is enabled
    ///
    /// In client mode, skip reading because the client already sent filters to the daemon.
    #[must_use]
    const fn should_read_filter_list(&self) -> bool {
        let receiver_wants_list = self.config.flags.delete || self.config.flags.prune_empty_dirs;
        !self.config.connection.client_mode && receiver_wants_list
    }

    /// Sets the per-directory filter chain for deletion filtering.
    ///
    /// Called after receiving the filter list from the sender, before the
    /// deletion pass. The chain is used by `delete_extraneous_files()`.
    pub fn set_filter_chain(&mut self, chain: FilterChain) {
        self.filter_chain = chain;
    }

    /// Returns a reference to the per-directory filter chain.
    #[must_use]
    pub fn filter_chain(&self) -> &FilterChain {
        &self.filter_chain
    }

    /// Returns the compiled daemon filter set, if any rules were configured.
    ///
    /// Used by `build_files_to_transfer()` to reject daemon-excluded files
    /// before accepting transfer data.
    pub fn daemon_filter_set(&self) -> Option<&FilterSet> {
        self.daemon_filter_set.as_ref()
    }

    /// Returns whether itemize emission should be active.
    ///
    /// MSG_INFO itemize frames are only emitted when:
    /// - Running in server mode (daemon or SSH) - not client mode
    /// - The client requested `--itemize-changes` (`-i`)
    #[must_use]
    const fn should_emit_itemize(&self) -> bool {
        !self.config.connection.client_mode && self.config.flags.info_flags.itemize
    }

    /// Builds the display context for itemize time-position rendering.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:708-710` - symlink time: `T` when `!preserve_mtimes || !receiver_symlink_times`
    /// - `log.c:716-717` - non-symlink time: `T` when `!preserve_mtimes`
    fn itemize_context(&self) -> crate::generator::itemize::ItemizeContext {
        crate::generator::itemize::ItemizeContext {
            preserve_mtimes: self.config.flags.times,
            receiver_symlink_times: self
                .compat_flags
                .is_some_and(|f| f.contains(protocol::CompatibilityFlags::SYMLINK_TIMES)),
        }
    }

    /// Emits a MSG_INFO frame with itemize output for a file entry.
    ///
    /// Formats the itemize string (`"%i %n%L\n"`) and sends it as a MSG_INFO
    /// multiplexed message. Uses `is_sender: false` since the daemon is receiving
    /// files (producing `>` direction indicator).
    ///
    /// Suppresses output when `iflags` has no significant flags set (the file is
    /// completely unchanged), matching upstream's gate in `itemize()` at
    /// `generator.c:574-576`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:574-576` - `iflags & (SIGNIFICANT_ITEM_FLAGS|ITEM_REPORT_XATTR)`
    /// - `generator.c:2260` - `itemize()` in receiver's generator context
    /// - `log.c:330-340` - `rwrite()` converts to `send_msg(MSG_INFO)` when `am_server`
    fn emit_itemize<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        writer: &mut W,
        iflags: &crate::generator::ItemFlags,
        entry: &protocol::flist::FileEntry,
    ) -> std::io::Result<()> {
        if !self.should_emit_itemize() {
            return Ok(());
        }
        // upstream: generator.c:574-576 - only emit when significant flags are
        // set. When iflags == 0 (file is completely up-to-date), no line is
        // produced. INFO_GTE(NAME, 2) and stdout_format_has_i > 1 gates are
        // not applicable on the server side (the client controls display).
        if !iflags.has_significant_flags() {
            return Ok(());
        }
        let ctx = self.itemize_context();
        let line = crate::generator::itemize::format_itemize_line(iflags, entry, false, &ctx);
        writer.send_msg_info(line.as_bytes())
    }

    /// Reclaims heap data from the oldest unreclaimed INC_RECURSE segment.
    ///
    /// Frees PathBuf, dirname Arc, and extras Box allocations for all entries
    /// in the segment while keeping entries in place so NDX-based indexing
    /// remains valid. Advances `first_segment_idx` to the next segment.
    ///
    /// No-op when there is only one segment remaining or when all segments
    /// have already been reclaimed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2945 flist_free()` - frees completed file list segments
    /// - `receiver.c:573` - `flist_free(first_flist)` in receiver transfer loop
    pub(in crate::receiver) fn reclaim_oldest_segment(&mut self) {
        let first = self.first_segment_idx;

        // Must have at least 2 segments to reclaim (keep the current one).
        if first + 1 >= self.ndx_segments.len() {
            return;
        }

        let start = self.ndx_segments[first].0;
        let end = self.ndx_segments[first + 1].0;

        logging::debug_log!(
            Flist,
            2,
            "reclaiming segment {} entries [{start}..{end})",
            first
        );

        for entry in &mut self.file_list[start..end] {
            entry.reclaim_heap_data();
        }
        self.first_segment_idx += 1;
    }
}

/// Shared configuration produced by [`ReceiverContext::setup_transfer`].
///
/// Groups the checksum, metadata, and ACL state that is common to all
/// transfer modes (sync, pipelined, incremental). Passed to the pipeline
/// loop and the redo pass.
struct PipelineSetup {
    dest_dir: PathBuf,
    metadata_opts: metadata::MetadataOptions,
    checksum_length: NonZeroU8,
    checksum_algorithm: signature::SignatureAlgorithm,
    /// ACL cache populated during flist reception. Shared with the disk commit
    /// thread via `Arc` so cached ACLs can be applied after file metadata.
    /// `None` when `--acls` is not active.
    acl_cache: Option<Arc<AclCache>>,
    /// Parent-dirfd carrier rooted at the destination tree.
    ///
    /// Opened once via [`fast_io::secure_open_dir`] when `setup_transfer`
    /// resolves the destination path. Threaded through the receiver
    /// pipeline so the SEC-1.f-j cutover sites can replace path-based
    /// syscalls with their `*at` siblings without re-walking the path
    /// through the kernel. This PR (SEC-1.e) wires the carrier through
    /// to [`ReceiverContext`] but does not migrate any syscalls; the
    /// existing path-based code paths continue to be the active code.
    ///
    /// `None` on Unix when the destination root cannot be opened (for
    /// example because it does not yet exist - the receiver will create
    /// it later and the carrier stays absent for the duration of the
    /// transfer). `None` on Windows where the carrier is not used
    /// (handle-based NTFS APIs, see SEC-1.l audit).
    #[cfg(unix)]
    sandbox: Option<Arc<fast_io::DirSandbox>>,
}

/// Applies ACLs from the receiver's ACL cache to a destination file.
///
/// Looks up the file entry's `acl_ndx` and optional `def_acl_ndx` in the cache
/// and applies the corresponding ACL to `destination`. No-op when `acl_cache`
/// is `None` or the entry has no ACL index.
///
/// # Upstream Reference
///
/// Mirrors upstream `set_file_attrs()` in receiver.c which calls `set_acl()`
/// after setting permissions, times, and ownership.
fn apply_acls_from_receiver_cache(
    destination: &std::path::Path,
    entry: &FileEntry,
    acl_cache: Option<&AclCache>,
    follow_symlinks: bool,
) -> Result<(), metadata::MetadataError> {
    let cache = match acl_cache {
        Some(c) => c,
        None => return Ok(()),
    };
    let access_ndx = match entry.acl_ndx() {
        Some(ndx) => ndx,
        None => return Ok(()),
    };
    metadata::apply_acls_from_cache(
        destination,
        cache,
        access_ndx,
        entry.def_acl_ndx(),
        follow_symlinks,
        Some(entry.mode()),
    )
}

/// Compiles daemon filter rules from wire format into a `FilterSet`.
///
/// Returns `Some(filter_set)` when rules are present, `None` when empty.
/// Used by the receiver to reject daemon-excluded files before accepting
/// transfer data, mirroring upstream `check_filter(&daemon_filter_list, ...)`
/// in `receiver.c:599-604`.
///
/// # Upstream Reference
///
/// - `clientserver.c:874-893` - daemon filter list is built from module
///   filter/exclude/include/exclude_from/include_from directives
/// - `receiver.c:599-604` - per-file check against daemon_filter_list
fn compile_daemon_filter_set(rules: &[FilterRuleWireFormat]) -> Option<FilterSet> {
    use filters::FilterRule;
    use protocol::filters::RuleType;

    if rules.is_empty() {
        return None;
    }

    let filter_rules: Vec<FilterRule> = rules
        .iter()
        .filter_map(|wire_rule| {
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(&wire_rule.pattern),
                RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
                RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
                RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
                RuleType::Clear | RuleType::DirMerge | RuleType::Merge => return None,
            };

            if wire_rule.sender_side || wire_rule.receiver_side {
                rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
            }
            if wire_rule.perishable {
                rule = rule.with_perishable(true);
            }
            if wire_rule.anchored {
                rule = rule.anchor_to_root();
            }

            Some(rule)
        })
        .collect();

    if filter_rules.is_empty() {
        return None;
    }

    FilterSet::from_rules(filter_rules).ok()
}
