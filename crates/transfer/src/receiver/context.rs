//! [`ReceiverContext`] state and the bulk of its method surface.
//!
//! Extracted verbatim from the receiver hub. Holds protocol state, the
//! received file list, and the predicates/builders that drive the receive
//! loop. The itemize/info-line emission methods live in the sibling
//! [`super::itemize`] module.

use std::collections::HashMap;
use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use filters::{FilterChain, FilterSet};
use protocol::flist::{FileEntry, FileListReader};
use protocol::idlist::IdList;
use protocol::stats::DeleteStats;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::HardlinkApplyTracker;
use engine::delete::DeleteContext;

use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::shared::ChecksumFactory;
use crate::transfer_state::TransferPipeline;

use super::basis::BasisFileConfig;
use super::{
    NDX_CONVERT_CALLS, NDX_CONVERT_CMPS, ParallelThresholds, compile_daemon_filter_set,
    partition_point_depth,
};

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
    pub(in crate::receiver) protocol: ProtocolVersion,
    /// Server configuration.
    pub(in crate::receiver) config: ServerConfig,
    /// List of files to receive.
    pub(in crate::receiver) file_list: Vec<FileEntry>,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub(in crate::receiver) negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    ///
    /// Controls protocol-specific behaviors like incremental recursion (`INC_RECURSE`),
    /// checksum seed ordering (`CHECKSUM_SEED_FIX`), and file list encoding (`VARINT_FLIST_FLAGS`).
    /// None for protocols < 30 or when compat exchange was skipped.
    pub(in crate::receiver) compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    pub(in crate::receiver) checksum_seed: i32,
    /// Segment boundary table for mapping flat array indices to wire NDX values.
    ///
    /// With INC_RECURSE, each segment has `ndx_start = prev_ndx_start + prev_used + 1`.
    /// Each entry is `(flat_start, ndx_start)`.
    /// Without INC_RECURSE, contains a single entry `(0, 0)`.
    ///
    /// upstream: flist.c:2931 - `flist->ndx_start = prev->ndx_start + prev->used + 1`
    pub(in crate::receiver) ndx_segments: Vec<(usize, i32)>,
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
    pub(in crate::receiver) first_segment_idx: usize,
    /// Cached file list reader for compression state continuity across sub-lists.
    ///
    /// Upstream rsync uses `static` variables in `recv_file_entry()` that persist
    /// across `recv_file_list()` calls. This field preserves the same state
    /// (prev_name, prev_mode, prev_uid, prev_gid) between `receive_file_list()`
    /// and `receive_extra_file_lists()`.
    pub(in crate::receiver) flist_reader_cache: Option<FileListReader>,
    /// UID mappings from remote to local IDs.
    pub(in crate::receiver) uid_list: IdList,
    /// GID mappings from remote to local IDs.
    pub(in crate::receiver) gid_list: IdList,
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
    pub(in crate::receiver) daemon_filter_set: Option<FilterSet>,
    /// Per-directory scoped filter chain for deletion protection.
    ///
    /// Used by `delete_extraneous_files()` to check `allows_deletion()` before
    /// removing destination files not present in the sender's file list. Rules
    /// include global protect/risk rules and per-directory merge file rules.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` - `is_excluded()` check before deletion
    pub(in crate::receiver) filter_chain: FilterChain,
    /// Per-directory merge rules consulted exclusively by the `--delete` pass.
    ///
    /// Held separately from `filter_chain` because `filter_chain` is also read
    /// by the `--prune-empty-dirs` pass (`prune_empty_dirs_pass`); the deletion
    /// pass needs the dir-merge configs (`.rsync-filter`, `.filt`/`.filt2`) so
    /// it can reload each destination directory's per-directory merge files
    /// while scanning, but those rules must not perturb prune-empty-dirs.
    ///
    /// On a local-client pull the wire filter list is never received
    /// (`should_read_filter_list()` is false in client mode), so this is built
    /// in `setup_transfer` from the local CLI filter rules. On a daemon/server
    /// receiver it is cloned from the wire-populated `filter_chain`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` -> `change_local_filter_dir()` ->
    ///   `exclude.c:push_local_filters()` reloads dest-side per-dir merge files
    pub(in crate::receiver) deletion_filter_chain: FilterChain,
    /// Tracker for hardlink leader/follower relationships during file apply.
    ///
    /// Records committed leader paths so followers can be hard-linked to them.
    /// Initialized when `--hard-links` is active; `None` otherwise.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:finish_hard_link()` - links deferred followers after leader commit
    /// - `hlink.c:hard_link_check()` - defers follower when leader in-progress
    pub(in crate::receiver) hardlink_tracker: Option<HardlinkApplyTracker>,
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
    pub(in crate::receiver) prior_hlinks: HashMap<u32, bool>,
    /// Accumulated I/O error flags from the sender's file list for protocol < 30.
    ///
    /// For protocol < 30, the sender writes a 4-byte LE io_error flag after the
    /// id lists (upstream: flist.c:2517-2518). Protocol >= 30 uses MSG_IO_ERROR
    /// or SAFE_FILE_LIST instead.
    pub(in crate::receiver) flist_io_error: i32,
    /// Per-operation thresholds for switching between sequential and parallel execution.
    ///
    /// Different operations have different overhead profiles: CPU-bound signature
    /// computation benefits from parallelism at lower counts than I/O-bound stat calls.
    pub(in crate::receiver) parallel_thresholds: ParallelThresholds,
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
    pub(in crate::receiver) delete_ctx: Option<Arc<DeleteContext>>,
    /// Deletion stats produced by the receiver's pre-transfer `--delete` sweep.
    ///
    /// Populated by `delete_extraneous_files` from both `run_pipelined` and
    /// `run_pipelined_incremental`, then consumed by `handle_goodbye` to
    /// emit `NDX_DEL_STATS` during the goodbye phase. Mirrors upstream's
    /// daemon-recv fork where the generator (which performs the delete pass)
    /// is also the side that emits `write_del_stats(f_out)`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2393-2398` - early `write_del_stats` when `delete_mode || force_delete || read_batch`
    /// - `main.c:225-238` - `write_del_stats()` wire format
    pub(in crate::receiver) pending_del_stats: DeleteStats,
    /// Transfer pipeline FSM tracking the current protocol phase.
    ///
    /// Enforces the linear phase progression through the transfer lifecycle.
    /// Initialized at `FilterExchange` by `run_server_with_handshake` and
    /// advanced through `FileListTransfer`, `DeltaTransfer`, `Finalization`,
    /// and `Complete` as the receiver progresses.
    pub(in crate::receiver) pipeline: TransferPipeline,
    /// Whether `setup_transfer`'s pre-flight mkdir actually created the
    /// destination root directory this run.
    ///
    /// Mirrors upstream `main.c:794-796` which sets `FLAG_DIR_CREATED` on the
    /// first flist entry only when the receiver had to `do_mkdir()` the dest
    /// root. The generator's `itemize()` then ORs `ITEM_IS_NEW` for the root
    /// entry, emitting `cd+++++++++ ./`. When the dest root already existed
    /// (e.g. `up1/ -> up2/` where `up2` is present), the flag stays clear and
    /// the root reports a metadata-only row that the standard significance
    /// gate drops. `emit_itemize` reads this to decide whether to force the
    /// created-directory glyph for the root entry.
    pub(in crate::receiver) dest_root_created: bool,
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
            deletion_filter_chain: FilterChain::empty(),
            hardlink_tracker,
            prior_hlinks: HashMap::new(),
            flist_io_error: 0,
            parallel_thresholds: ParallelThresholds::default(),
            delete_ctx: None,
            pending_del_stats: DeleteStats::new(),
            pipeline,
            dest_root_created: false,
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

    /// Advances the pipeline FSM to `DeltaTransfer` for tests that need to
    /// exercise [`finalize_transfer`](Self::finalize_transfer) directly.
    ///
    /// Walks the FSM through `FileListTransfer -> DeltaTransfer` so the
    /// caller can drop straight into the post-transfer finalization sequence
    /// without having to run a real file-list or delta exchange.
    #[cfg(test)]
    pub(in crate::receiver) fn advance_pipeline_to_delta_transfer_for_test(&mut self) {
        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::FileListTransfer)
            .expect("test pipeline advance to FileListTransfer");
        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::DeltaTransfer)
            .expect("test pipeline advance to DeltaTransfer");
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
    pub(in crate::receiver) const fn get_checksum_algorithm(&self) -> protocol::ChecksumAlgorithm {
        if let Some(negotiated) = &self.negotiated_algorithms {
            negotiated.checksum
        } else if self.protocol.uses_varint_encoding() {
            protocol::ChecksumAlgorithm::MD5
        } else {
            protocol::ChecksumAlgorithm::MD4
        }
    }

    /// Builds a [`BasisFileConfig`] for a single file, pulling shared state from `self`.
    pub(in crate::receiver) fn build_basis_file_config<'a>(
        &'a self,
        file_path: &'a std::path::Path,
        dest_dir: &'a std::path::Path,
        relative_path: &'a std::path::Path,
        target_size: u64,
        target_mtime: i64,
        checksum_length: NonZeroU8,
        checksum_algorithm: signature::SignatureAlgorithm,
    ) -> BasisFileConfig<'a> {
        BasisFileConfig {
            file_path,
            dest_dir,
            relative_path,
            target_size,
            target_mtime,
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
    pub(in crate::receiver) fn build_flist_reader(&self) -> FileListReader {
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
    pub(in crate::receiver) fn resolve_xattr_list(
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
    pub(in crate::receiver) const fn should_read_filter_list(&self) -> bool {
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

    /// Sets the deletion-pass filter chain.
    ///
    /// Held separately from `filter_chain` so the `--delete` pass can reload
    /// per-directory merge files without perturbing `--prune-empty-dirs`. In
    /// production this is populated by `setup_transfer`; tests set it directly.
    pub fn set_deletion_filter_chain(&mut self, chain: FilterChain) {
        self.deletion_filter_chain = chain;
    }

    /// Returns the compiled daemon filter set, if any rules were configured.
    ///
    /// Used by `build_files_to_transfer()` to reject daemon-excluded files
    /// before accepting transfer data.
    pub fn daemon_filter_set(&self) -> Option<&FilterSet> {
        self.daemon_filter_set.as_ref()
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
