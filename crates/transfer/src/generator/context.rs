//! `GeneratorContext` definition and inherent methods shared across the
//! generator role's submodules.
//!
//! The context holds protocol state, configuration, the running file list,
//! filter chain, accumulated statistics, and incremental-recursion state.
//! Construction-time setup happens in [`GeneratorContext::new`]; the full send
//! workflow is driven by the `transfer` submodule via `GeneratorContext::run`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use ::filters::FilterChain;
use protocol::flist::{DualFileList, FileEntry};
use protocol::idlist::IdList;
use protocol::stats::DeleteStats;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use crate::role_trailer::error_location;

use super::diagnostics::{NDX_CONVERT_CALLS, NDX_CONVERT_CMPS, partition_point_depth};
use super::io_error_flags;
use super::segments::IncrementalState;
use super::timing::TransferTiming;
use super::{itemize, open_source};
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::transfer_state::TransferPipeline;

/// Concrete source-file descriptor surfaced by `open_source_unbuffered`.
///
/// NSV-1: `RawFd` on Unix so the daemon SERVE path can later engage a zero-copy
/// file->socket sender. `!()` collapses to unit on non-unix where no fd exists;
/// the value is always `None` there.
#[cfg(unix)]
pub(crate) type SourceFd = std::os::fd::RawFd;
/// Non-unix placeholder for the source descriptor (never populated).
#[cfg(not(unix))]
pub(crate) type SourceFd = ();

/// Context for the generator role during a transfer.
///
/// Holds protocol state, configuration, file list, and filter rules needed
/// to drive the send loop. Created via [`GeneratorContext::new`] from a
/// completed [`HandshakeResult`] and [`ServerConfig`], then executed with
/// [`GeneratorContext::run`].
///
/// See the [module-level documentation](crate::generator) for the full send workflow.
#[derive(Debug)]
pub struct GeneratorContext {
    /// Negotiated protocol version.
    pub(crate) protocol: ProtocolVersion,
    /// Server configuration.
    pub(crate) config: ServerConfig,
    /// List of files to send (contains relative paths for wire transmission).
    ///
    /// **Invariant**: `file_list` and `source_bases` must always have the same length.
    /// Use [`Self::push_file_item`] to add entries and [`Self::clear_file_list`] to clear.
    pub(crate) file_list: DualFileList,
    /// Interned source-directory base for each file in `file_list` (parallel array).
    ///
    /// The on-disk path used to open a source file for delta generation is
    /// `base.join(entry.name())` (see [`Self::reconstruct_source_path`]). Storing
    /// only the base - which is identical for every entry produced by a single
    /// source argument - lets all those entries share one `Arc<Path>` allocation
    /// instead of each owning a full `PathBuf` that redundantly re-stores the
    /// entry's relative name. At scale this removes the per-file path bytes (and
    /// their allocator overhead) from the sender's resident set; the name bytes
    /// already live in the `FileEntry`.
    ///
    /// **Invariant**: `file_list[i]` corresponds to `source_bases[i]` for all
    /// valid indices, and `reconstruct_source_path(i)` reproduces the exact
    /// on-disk path the walk recorded for entry `i`.
    pub(crate) source_bases: Vec<Arc<Path>>,
    /// One-entry interning cache for [`Self::push_file_item`].
    ///
    /// Consecutive entries from the same source argument derive the identical
    /// base, so caching the last `Arc<Path>` collapses them onto a single shared
    /// allocation without the overhead of a full interning map.
    last_source_base: Option<Arc<Path>>,
    /// Per-directory scoped filter chain for file list building and deletion.
    ///
    /// Combines global filter rules (from command-line or wire) with per-directory
    /// merge files (`.rsync-filter`). During `walk_path()`, the chain pushes/pops
    /// scoped rules as directories are entered and left.
    ///
    /// # Upstream Reference
    ///
    /// - `exclude.c:push_local_filters()` / `pop_local_filters()`
    pub(crate) filter_chain: FilterChain,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub(crate) negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    ///
    /// Controls protocol-specific behaviors like incremental recursion (`INC_RECURSE`),
    /// checksum seed ordering (`CHECKSUM_SEED_FIX`), and file list encoding (`VARINT_FLIST_FLAGS`).
    /// None for protocols < 30 or when compat exchange was skipped.
    pub(crate) compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    pub(crate) checksum_seed: i32,
    /// Timing and byte-count statistics for the transfer.
    pub(crate) timing: TransferTiming,
    /// Collected UID mappings for name-based ownership transfer.
    pub(crate) uid_list: IdList,
    /// Collected GID mappings for name-based ownership transfer.
    pub(crate) gid_list: IdList,
    /// I/O error flags accumulated during file list building and transfer.
    /// Uses [`io_error_flags`] constants (IOERR_GENERAL, IOERR_VANISHED, etc.).
    pub(crate) io_error: i32,
    /// Incremental recursion (INC_RECURSE) state for segmented file list sending.
    pub(crate) incremental: IncrementalState,
    /// Accumulated deletion statistics received via NDX_DEL_STATS messages.
    /// (upstream: main.c:238-247 `read_del_stats()`)
    pub(crate) delete_stats: DeleteStats,
    /// Per-operation thresholds for switching between sequential and parallel execution.
    ///
    /// Different operations have different overhead profiles: CPU-bound signature
    /// computation benefits from parallelism at lower counts than I/O-bound stat calls.
    pub(crate) parallel_thresholds: crate::parallel_io::ParallelThresholds,
    /// Transfer pipeline FSM tracking the current protocol phase.
    ///
    /// Enforces the linear phase progression through the transfer lifecycle.
    /// Initialized at `FilterExchange` by `run_server_with_handshake` and
    /// advanced through `FileListTransfer`, `DeltaTransfer`, `Finalization`,
    /// and `Complete` as the generator progresses.
    pub(crate) pipeline: TransferPipeline,
}

impl GeneratorContext {
    /// Creates a new generator context from a completed handshake and server config.
    ///
    /// Initializes protocol state, INC_RECURSE NDX offset, and empty file list.
    /// Call [`build_file_list`](Self::build_file_list) to populate entries, then
    /// [`run`](Self::run) to execute the transfer.
    /// The `pipeline` parameter carries the transfer FSM state from the
    /// orchestration layer. It should be at `FilterExchange` when the
    /// generator is created.
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

        // upstream: compat.c:653-655 - legacy (protocol < 30) peers verify the
        // append prefix, so promote plain `--append` to `--append-verify`.
        let mut config = config;
        config.promote_append_mode_for_protocol(handshake.protocol);

        Self {
            protocol: handshake.protocol,
            config,
            file_list: DualFileList::new(),
            source_bases: Vec::new(),
            last_source_base: None,
            filter_chain: FilterChain::empty(),
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
            timing: TransferTiming::new(),
            uid_list: IdList::new(),
            gid_list: IdList::new(),
            io_error: 0,
            incremental: IncrementalState::new(initial_ndx_start),
            delete_stats: DeleteStats::new(),
            parallel_thresholds: crate::parallel_io::ParallelThresholds::default(),
            pipeline,
        }
    }

    /// Creates a generator context for unit testing with a default pipeline.
    ///
    /// The pipeline is initialized at `FilterExchange`, matching the state
    /// when a real `run_server_with_handshake` dispatches to the generator.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new_for_test(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        let mut pipeline = TransferPipeline::new(crate::role::ServerRole::Generator);
        pipeline
            .advance_to(crate::transfer_state::TransferPhase::FilterExchange)
            .expect("test pipeline advance");
        Self::new(handshake, config, pipeline)
    }

    /// Converts a wire NDX value to a flat file list array index.
    ///
    /// Uses `partition_point` for O(log n) lookup, matching `flat_to_wire_ndx`.
    ///
    /// Updates the `NDX_CONVERT_CALLS` / `NDX_CONVERT_CMPS` counters used
    /// for INC_RECURSE diagnostic I4 (#2199).
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:424` - `i = ndx - cur_flist->ndx_start`
    pub(crate) fn wire_to_flat_ndx(&self, wire_ndx: i32) -> usize {
        let segments = &self.incremental.ndx_segments;
        NDX_CONVERT_CALLS.fetch_add(1, Ordering::Relaxed);
        NDX_CONVERT_CMPS.fetch_add(partition_point_depth(segments.len()), Ordering::Relaxed);
        let seg_idx = segments
            .partition_point(|&(_, ndx_start)| ndx_start <= wire_ndx)
            .saturating_sub(1);
        let (flat_start, ndx_start) = segments[seg_idx];
        flat_start + (wire_ndx - ndx_start) as usize
    }

    /// Converts a flat file list array index to a wire NDX value.
    ///
    /// Updates the `NDX_CONVERT_CALLS` / `NDX_CONVERT_CMPS` counters used
    /// for INC_RECURSE diagnostic I4 (#2199).
    ///
    /// Only used in tests after the NDX echo-back fix - the transfer loop now
    /// preserves the original wire NDX instead of round-tripping through this
    /// function, which avoids corrupting INC_RECURSE gap NDX values.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2321` - `ndx = i + cur_flist->ndx_start`
    #[cfg(test)]
    pub(crate) fn flat_to_wire_ndx(&self, flat_idx: usize) -> i32 {
        let segments = &self.incremental.ndx_segments;
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

    /// Returns the negotiated compatibility flags.
    ///
    /// Returns `None` for protocols < 30 or when compat exchange was skipped.
    /// The flags control protocol-specific behaviors like incremental recursion,
    /// checksum seed ordering, and file list encoding.
    pub const fn compat_flags(&self) -> Option<protocol::CompatibilityFlags> {
        self.compat_flags
    }

    /// Returns `true` when `INC_RECURSE` compat flag is negotiated.
    pub(crate) fn inc_recurse(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE))
    }

    /// Builds the display context for itemize time-position rendering.
    ///
    /// Captures `preserve_mtimes` (from `--times` flag) and `receiver_symlink_times`
    /// (from `CF_SYMLINK_TIMES` compat flag) so `format_iflags` can correctly
    /// distinguish `t` from `T` at position 4.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:708-710` - symlink time: `T` when `!preserve_mtimes || !receiver_symlink_times`
    /// - `log.c:716-717` - non-symlink time: `T` when `!preserve_mtimes`
    pub(crate) fn itemize_context(&self) -> itemize::ItemizeContext {
        itemize::ItemizeContext {
            preserve_mtimes: self.config.flags.times,
            receiver_symlink_times: self
                .compat_flags
                .is_some_and(|f| f.contains(CompatibilityFlags::SYMLINK_TIMES)),
        }
    }

    /// Creates a configured `FileListWriter` matching the current protocol and flags.
    pub(crate) fn build_flist_writer(&self) -> protocol::flist::FileListWriter {
        use crate::shared::ChecksumFactory;

        // upstream: acls.c:597 - ACL entry names are written only under
        // incremental recursion and when names (not numeric ids) are used.
        // Without inc_recurse the receiver remaps ACL ids through the id-list
        // (match_acl_ids), so no per-entry name is needed on the wire.
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(protocol::CompatibilityFlags::INC_RECURSE));
        // upstream: acls.c:716 - `if (inc_recurse && !numeric_ids)`; per-entry
        // ACL names travel only when names are active (`numeric_ids == 0`).
        let acl_send_names = inc_recurse && self.config.flags.numeric_ids.is_off();

        let mut writer = if let Some(flags) = self.compat_flags {
            protocol::flist::FileListWriter::with_compat_flags(self.protocol, flags)
        } else {
            protocol::flist::FileListWriter::new(self.protocol)
        }
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_specials(self.config.flags.specials)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_atimes(self.config.flags.atimes)
        .with_preserve_crtimes(self.config.flags.crtimes)
        .with_preserve_acls(self.config.flags.acls)
        .with_acl_send_names(acl_send_names)
        // upstream: flist.c:481-482,491-492 - inline XMIT_*_NAME_FOLLOWS owner
        // names are emitted only under incremental recursion. Without it the
        // names travel solely in the trailing id-list (send_id_lists), so
        // gating inline emission on inc_recurse keeps the flist bytes identical
        // to upstream for non-incremental transfers.
        .with_name_follows(inc_recurse)
        .with_preserve_xattrs(self.config.flags.xattrs)
        .with_checksum_seed(self.checksum_seed);

        // upstream: flist.c - always_checksum includes per-file checksums in the file list
        if self.config.flags.checksum {
            let factory = ChecksumFactory::from_negotiation(
                self.negotiated_algorithms.as_ref(),
                self.protocol,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );
            writer = writer.with_always_checksum(factory.digest_length());
        }

        if let Some(ref converter) = self.config.connection.iconv {
            writer = writer.with_iconv(converter.clone());
        }
        writer
    }

    /// Returns a reference to the filter chain for external use.
    ///
    /// The receiver may need the filter chain for deletion filtering.
    #[must_use]
    pub fn filter_chain(&self) -> &FilterChain {
        &self.filter_chain
    }

    /// Returns the generated file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        debug_assert_eq!(
            self.file_list.len(),
            self.source_bases.len(),
            "file_list and source_bases must be kept in sync"
        );
        self.file_list.as_slice()
    }

    /// Adds a file entry and its corresponding on-disk source path to the list.
    ///
    /// `full_path` is the exact path the walk recorded for the entry (used to
    /// open the source for delta generation). Rather than storing it verbatim,
    /// this derives and interns the source-directory base so that
    /// `base.join(entry.name())` reproduces `full_path` byte-for-byte (see
    /// [`reconstruct_source_path`](Self::reconstruct_source_path)). Entries from
    /// one source argument share a single base allocation.
    ///
    /// This method maintains the invariant that `file_list` and `source_bases`
    /// have the same length and corresponding entries at each index.
    pub(crate) fn push_file_item(&mut self, entry: FileEntry, full_path: PathBuf) {
        debug_assert_eq!(
            self.file_list.len(),
            self.source_bases.len(),
            "file_list and source_bases must be kept in sync before push"
        );
        let base = self.intern_source_base(&full_path, entry.path());
        self.file_list.push(entry);
        self.source_bases.push(base);
    }

    /// Derives the source base for `full_path`/`name` and interns it against the
    /// one-entry cache, returning a shared `Arc<Path>`.
    fn intern_source_base(&mut self, full_path: &Path, name: &Path) -> Arc<Path> {
        let base = derive_source_base(full_path, name);
        match &self.last_source_base {
            Some(cached) if cached.as_ref() == base.as_path() => Arc::clone(cached),
            _ => {
                let arc: Arc<Path> = Arc::from(base.as_path());
                self.last_source_base = Some(Arc::clone(&arc));
                arc
            }
        }
    }

    /// Reconstructs the on-disk source path recorded for entry `ndx`.
    ///
    /// Inverts [`push_file_item`](Self::push_file_item): the returned path equals
    /// the `full_path` originally passed in, byte-for-byte, for every non-
    /// degenerate source (single or multiple positional args, `--files-from`
    /// `/./` anchors, daemon module roots, and arbitrarily nested names).
    pub(crate) fn reconstruct_source_path(&self, ndx: usize) -> PathBuf {
        join_source_path(&self.source_bases[ndx], self.file_list[ndx].path())
    }

    /// Clears both the file list and the source-base array.
    ///
    /// This method maintains the invariant that both arrays are cleared together
    /// and resets the interning cache so a fresh build starts clean.
    pub(crate) fn clear_file_list(&mut self) {
        self.file_list = DualFileList::new();
        self.source_bases.clear();
        self.last_source_base = None;
    }

    /// Determines if input multiplex should be activated based on mode and protocol.
    ///
    /// The activation threshold differs by mode:
    ///
    /// **Server mode** (daemon sender - `main.c:1252-1257` `start_server am_sender`):
    /// - For protocol >= 30, `need_messages_from_generator = 1` (compat.c:776)
    /// - `if (need_messages_from_generator) io_start_multiplex_in(f_in);`
    ///
    /// **Client mode** (client pushing to daemon - `main.c:1304-1305` `client_run am_sender`):
    /// - `if (protocol_version >= 31 || (!filesfrom_host && protocol_version >= 23))`
    /// - We don't support filesfrom, so this simplifies to >= 23
    #[must_use]
    pub(crate) const fn should_activate_input_multiplex(&self) -> bool {
        if self.config.connection.client_mode {
            // Client mode: >= 23 (upstream main.c:1304-1305, no filesfrom)
            self.protocol.supports_multiplex_io()
        } else {
            // Server mode: >= 30 (need_messages_from_generator)
            self.protocol.supports_generator_messages()
        }
    }

    /// Adds an I/O error flag to the accumulated error state.
    ///
    /// Use constants from [`io_error_flags`] module (IOERR_GENERAL, IOERR_VANISHED, etc.).
    ///
    /// # Example
    ///
    /// ```ignore
    /// ctx.add_io_error(io_error_flags::IOERR_GENERAL);
    /// ```
    pub fn add_io_error(&mut self, flag: i32) {
        self.io_error |= flag;
    }

    /// Records an I/O error, distinguishing between vanished files and general errors.
    ///
    /// This is a convenience wrapper around [`Self::add_io_error`] that maps
    /// `NotFound` errors to `IOERR_VANISHED` and all other errors to `IOERR_GENERAL`.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors upstream rsync's error handling where ENOENT indicates a vanished
    /// file (race condition during scan) vs other I/O errors.
    pub(crate) fn record_io_error(&mut self, error: &std::io::Error) {
        if error.kind() == std::io::ErrorKind::NotFound {
            self.add_io_error(io_error_flags::IOERR_VANISHED);
        } else {
            self.add_io_error(io_error_flags::IOERR_GENERAL);
        }
    }

    /// Returns the current I/O error flags.
    #[must_use]
    pub const fn io_error(&self) -> i32 {
        self.io_error
    }

    /// Returns the checksum algorithm to use for file transfer checksums.
    ///
    /// The algorithm depends on negotiation and protocol version:
    /// - Protocol 30+ with negotiation: uses negotiated algorithm
    /// - Protocol 30+ without negotiation: MD5 (16 bytes)
    /// - Protocol < 30: MD4 (16 bytes)
    #[must_use]
    pub(crate) const fn get_checksum_algorithm(&self) -> protocol::ChecksumAlgorithm {
        if let Some(negotiated) = &self.negotiated_algorithms {
            negotiated.checksum
        } else if self.protocol.uses_varint_encoding() {
            protocol::ChecksumAlgorithm::MD5
        } else {
            protocol::ChecksumAlgorithm::MD4
        }
    }

    /// Returns the per-file compression algorithm, respecting the skip-compress list.
    ///
    /// When compression is negotiated but the file's extension matches the
    /// skip-compress list, returns `None` to send the file uncompressed.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:do_compression` - checks `dont_compress_re` regex per file
    /// - `loadparm.c` - `dont compress` daemon parameter populates the regex
    pub(crate) fn file_compression(
        &self,
        path: &std::path::Path,
    ) -> Option<protocol::CompressionAlgorithm> {
        let algo = self.negotiated_algorithms.map(|n| n.compression)?;
        if let Some(ref skip_list) = self.config.skip_compress {
            if skip_list.matches_path(path) {
                return None;
            }
        }
        Some(algo)
    }

    /// Opens a source file for reading with `BufReader` buffering.
    ///
    /// Suitable for callers that perform many small or random reads (e.g., the
    /// delta generator's rolling-checksum scan). For callers that manage their
    /// own large read buffer, use [`open_source_unbuffered`](Self::open_source_unbuffered)
    /// to avoid an extra copy layer.
    ///
    /// Files at or above the io_uring read threshold (1 MB) use `reader_from_path`,
    /// which creates an io_uring-backed reader on Linux 5.6+ (respecting the
    /// `--no-io-uring` flag) or an IOCP overlapped reader on Windows, each with
    /// transparent std fallback. Smaller files use a standard `BufReader` to
    /// avoid the overhead of creating a ring/completion port per file.
    ///
    /// When `--open-noatime` is in effect the io_uring fast path is bypassed
    /// because `IoUringReader::open` does not accept custom open flags;
    /// matching upstream `do_open` semantics is the user-requested invariant.
    ///
    /// # Upstream Reference
    ///
    /// - `syscall.c:228 do_open` / `syscall.c:687 do_open_nofollow` (3.4.2
    ///   propagates `O_NOATIME` through both paths).
    pub(crate) fn open_source_reader(
        &self,
        path: &std::path::Path,
        file_size: u64,
    ) -> std::io::Result<Box<dyn std::io::Read>> {
        use crate::adaptive_buffer::adaptive_buffer_size;

        // 1 MB threshold: io_uring ring creation has fixed overhead that only
        // pays off for larger reads where batched syscalls reduce total cost.
        const IO_URING_READ_THRESHOLD: u64 = 1024 * 1024;

        let use_noatime = self.config.write.open_noatime;

        // upstream: sender.c streams a `--copy-devices` device through the plain
        // read path. Block/char devices report `st_size == 0`, so the io_uring
        // fast path (which sizes reads from the fd's stat length) yields EOF at 0
        // bytes and short-reads the stream. Force buffered I/O for devices so the
        // full `file_size` bytes reach the wire.
        if !use_noatime
            && file_size >= IO_URING_READ_THRESHOLD
            && self.config.write.io_uring_policy != fast_io::IoUringPolicy::Disabled
            && !self.source_is_copy_device(path)
        {
            // Windows gets IOCP where Linux gets io_uring, std elsewhere. The
            // IOCP reader mirrors the disk-commit writer's dispatch: runtime
            // `is_iocp_available()` detection with transparent std buffered
            // fallback (see fast_io iocp::file_factory::reader_from_path and
            // disk_commit/config.rs iocp_policy).
            #[cfg(target_os = "windows")]
            {
                if let Ok(r) = fast_io::iocp_reader_from_path(path, fast_io::IocpPolicy::Auto) {
                    return Ok(Box::new(r));
                }
                // Fall through to standard BufReader on IOCP failure.
            }
            #[cfg(not(target_os = "windows"))]
            {
                match fast_io::reader_from_path_with_depth(
                    path,
                    self.config.write.io_uring_policy,
                    self.config.write.io_uring_depth,
                ) {
                    Ok(r) => return Ok(Box::new(r)),
                    Err(_) => {
                        // Fall through to standard BufReader on io_uring failure
                    }
                }
            }
        }

        let f = open_source::open_source_with_noatime(path, use_noatime)?;
        Ok(Box::new(std::io::BufReader::with_capacity(
            adaptive_buffer_size(file_size),
            f,
        )))
    }

    /// Returns whether `--copy-devices` is active and `path` is a block/char
    /// device, i.e. a source that must be read through the plain buffered path
    /// rather than the size-from-stat io_uring/mmap fast paths.
    ///
    /// The `symlink_metadata` probe runs only when `--copy-devices` is set, so
    /// regular transfers pay no extra stat.
    #[cfg(unix)]
    pub(crate) fn source_is_copy_device(&self, path: &std::path::Path) -> bool {
        use std::os::unix::fs::FileTypeExt;
        self.config.flags.copy_devices
            && std::fs::symlink_metadata(path).is_ok_and(|m| {
                let ft = m.file_type();
                ft.is_block_device() || ft.is_char_device()
            })
    }

    /// Non-Unix stub: devices are not a distinct source kind, so nothing forces
    /// the buffered path.
    #[cfg(not(unix))]
    pub(crate) fn source_is_copy_device(&self, _path: &std::path::Path) -> bool {
        false
    }

    /// Opens a source file without intermediate buffering.
    ///
    /// Identical to [`open_source_reader`](Self::open_source_reader) except the
    /// fallback path returns the raw `File` instead of wrapping it in a
    /// `BufReader`. Callers that manage their own read buffer (e.g.,
    /// `stream_whole_file_transfer` with its 256 KB staging buffer and
    /// `read_exact` calls) should use this to avoid an extra memcpy per byte
    /// through the `BufReader`'s internal buffer.
    ///
    /// The io_uring fast path is unchanged - it already returns a reader with
    /// its own internal buffering strategy.
    ///
    /// # NSV-1
    ///
    /// Returns the reader together with the concrete source `RawFd` when the
    /// fallback path opens a plain `File` (Unix only). The io_uring fast path
    /// yields `None` for the fd because its reader owns the descriptor behind an
    /// abstraction; a later zero-copy SERVE gate applies only to the plain-file
    /// case. The fd is purely informational here - the byte stream is identical.
    pub(crate) fn open_source_unbuffered(
        &self,
        path: &std::path::Path,
        file_size: u64,
    ) -> std::io::Result<(Box<dyn std::io::Read>, Option<SourceFd>)> {
        // 1 MB threshold: io_uring ring creation has fixed overhead that only
        // pays off for larger reads where batched syscalls reduce total cost.
        const IO_URING_READ_THRESHOLD: u64 = 1024 * 1024;

        let use_noatime = self.config.write.open_noatime;

        // upstream: `--copy-devices` streams through the plain read path; a
        // device's stat size is 0, so the io_uring fast path would short-read.
        // See `source_is_copy_device` / `open_source_reader`.
        if !use_noatime
            && file_size >= IO_URING_READ_THRESHOLD
            && self.config.write.io_uring_policy != fast_io::IoUringPolicy::Disabled
            && !self.source_is_copy_device(path)
        {
            // Windows gets IOCP where Linux gets io_uring, std elsewhere. The
            // IOCP reader mirrors the disk-commit writer's dispatch: runtime
            // `is_iocp_available()` detection with transparent std buffered
            // fallback (see fast_io iocp::file_factory::reader_from_path and
            // disk_commit/config.rs iocp_policy).
            #[cfg(target_os = "windows")]
            {
                if let Ok(r) = fast_io::iocp_reader_from_path(path, fast_io::IocpPolicy::Auto) {
                    return Ok((Box::new(r), None));
                }
                // Fall through to raw File on IOCP failure.
            }
            #[cfg(not(target_os = "windows"))]
            {
                match fast_io::reader_from_path_with_depth(
                    path,
                    self.config.write.io_uring_policy,
                    self.config.write.io_uring_depth,
                ) {
                    Ok(r) => return Ok((Box::new(r), None)),
                    Err(_) => {
                        // Fall through to raw File on io_uring failure
                    }
                }
            }
        }

        let f = open_source::open_source_with_noatime(path, use_noatime)?;
        #[cfg(unix)]
        let src_fd = Some(std::os::fd::AsRawFd::as_raw_fd(&f));
        #[cfg(not(unix))]
        let src_fd = None;
        Ok((Box::new(f), src_fd))
    }

    /// Returns the upstream `missing_args` mode for ENOENT handling.
    ///
    /// Maps the two boolean flags to the upstream integer convention:
    /// - `0` (default): emit `link_stat ... failed` warning, set IOERR_GENERAL
    /// - `1` (`--ignore-missing-args`): silently skip the entry
    /// - `2` (`--delete-missing-args`): emit mode-0 sentinel for receiver deletion
    ///
    /// `delete_missing_args` takes precedence when both are set.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_file_list()` - `missing_args` variable (0/1/2)
    pub(crate) fn missing_args_mode(&self) -> u8 {
        if self.config.file_selection.delete_missing_args {
            2
        } else if self.config.file_selection.ignore_missing_args {
            1
        } else {
            0
        }
    }

    /// Validates that a file index is within bounds of the file list.
    pub(crate) fn validate_file_index(&self, ndx: usize) -> std::io::Result<()> {
        if ndx >= self.file_list.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "invalid file index {}, file list has {} entries {}{}",
                    ndx,
                    self.file_list.len(),
                    error_location!(),
                    crate::role_trailer::sender()
                ),
            ));
        }
        Ok(())
    }

    /// Reclaims heap data from the oldest unreclaimed INC_RECURSE segment.
    ///
    /// Frees PathBuf, dirname Arc, and extras Box allocations for all entries
    /// in the segment while keeping entries in place so NDX-based indexing
    /// remains valid. Advances `first_segment_idx` to the next segment.
    ///
    /// No-op when there is only one segment remaining (the current segment
    /// must not be reclaimed while the transfer loop may still access it)
    /// or when all segments have already been reclaimed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2945 flist_free()` - frees completed file list segments
    /// - `sender.c:244` - `flist_free(first_flist)` in sender transfer loop
    pub(crate) fn reclaim_oldest_segment(&mut self) {
        let segments = &self.incremental.ndx_segments;
        let first = self.incremental.first_segment_idx;

        // Must have at least 2 segments to reclaim (keep the current one).
        if first + 1 >= segments.len() {
            return;
        }

        let start = segments[first].0;
        let end = segments[first + 1].0;

        logging::debug_log!(
            Flist,
            2,
            "reclaiming segment {} entries [{start}..{end})",
            first
        );

        self.file_list.reclaim_segment(start, end);
        // Also reclaim the parallel source_bases entries. Point them at a single
        // shared empty Arc so dropping the reclaimed slots releases the last
        // reference to the segment's source base(s).
        let empty: Arc<Path> = Arc::from(Path::new(""));
        for base in &mut self.source_bases[start..end] {
            *base = Arc::clone(&empty);
        }
        self.incremental.first_segment_idx += 1;
    }
}

/// Joins an interned source base with an entry's relative name to reproduce the
/// on-disk path the walk recorded.
///
/// The `.` transfer-root entry stores its full directory path as the base (its
/// relative name is `.`), so it is returned unchanged rather than gaining a
/// trailing `/.`. Every other entry pushes the name's *components* onto the
/// base, so the platform separator is used uniformly: the wire name is
/// `/`-separated even on Windows (the flist sorts on `/`), while the base
/// carries native separators. Extending by component - never concatenating raw
/// path strings - keeps the result natively separated on every platform and
/// avoids any `/`-vs-`\` byte surgery.
fn join_source_path(base: &Path, name: &Path) -> PathBuf {
    if name == Path::new(".") {
        return base.to_path_buf();
    }
    let mut path = base.to_path_buf();
    path.extend(name.components());
    path
}

/// Derives the interned base for a `(full_path, name)` pair such that
/// [`join_source_path`]`(base, name)` reproduces `full_path`.
///
/// The base is `full_path` with the `name`'s components stripped from its tail -
/// the constant source-argument directory shared by every entry of a source.
/// [`Path::ends_with`] compares whole path components, so a `/`-separated wire
/// name matches a natively separated on-disk path on any platform; the strip is
/// done entirely through [`std::path::Components`], never through byte or string
/// suffix operations (which would mis-handle `/` vs `\` on Windows). For a
/// degenerate operand whose recorded name is not a component suffix of the
/// on-disk path (e.g. a trailing-slash source that resolved to a non-directory),
/// the parent directory is used instead - reconstruction then opens the same
/// inode, matching the pre-existing behaviour for such malformed operands.
fn derive_source_base(full: &Path, name: &Path) -> PathBuf {
    if name == Path::new(".") {
        return full.to_path_buf();
    }
    if full.ends_with(name) {
        let keep = full.components().count() - name.components().count();
        return full.components().take(keep).collect();
    }
    full.parent()
        .map_or_else(|| full.to_path_buf(), Path::to_path_buf)
}

#[cfg(all(test, unix))]
mod source_base_tests {
    //! Round-trip proof that interning a source base and reconstructing
    //! `base.join(name)` reproduces the exact on-disk path the walk recorded,
    //! for every case that made naive "reconstruct from entry alone" fail:
    //! multiple positional sources with distinct bases, `--files-from` `/./`
    //! anchors, daemon module roots, and arbitrarily nested names. Unix-gated
    //! so the byte-for-byte assertions use forward-slash literals; the derive
    //! itself is separator-agnostic (it strips/re-joins native components).
    use super::{derive_source_base, join_source_path};
    use std::path::Path;

    fn round_trips(full: &str, name: &str) {
        let full_p = Path::new(full);
        let base = derive_source_base(full_p, Path::new(name));
        let rebuilt = join_source_path(&base, Path::new(name));
        assert_eq!(
            rebuilt.as_os_str().as_encoded_bytes(),
            full_p.as_os_str().as_encoded_bytes(),
            "reconstructed path for name {name:?} under {full:?} diverged: got {rebuilt:?}"
        );
    }

    #[test]
    fn single_source_nested_names_round_trip() {
        // rsync /srv/data/ dst : one base, arbitrarily nested wire names.
        round_trips("/srv/data/file.txt", "file.txt");
        round_trips("/srv/data/sub/file.txt", "sub/file.txt");
        round_trips("/srv/data/a/b/c/d/e.bin", "a/b/c/d/e.bin");
    }

    #[test]
    fn multi_source_distinct_bases_round_trip() {
        // rsync /a/foo /b/bar dst : each positional keeps its own base, and the
        // wire name (basename) alone cannot recover it - the stored base must.
        round_trips("/a/foo", "foo");
        round_trips("/b/bar", "bar");
        // Two sources whose basenames collide but live under different bases.
        round_trips("/one/data/x/y", "x/y");
        round_trips("/two/data/x/y", "x/y");
    }

    #[test]
    fn files_from_dot_anchor_round_trip() {
        // --files-from with `from/./dir/sub` : base is everything before /./,
        // the wire name is everything after.
        round_trips("/export/from/dir/sub", "dir/sub");
        round_trips("/export/from/dir/sub/leaf.dat", "dir/sub/leaf.dat");
    }

    #[test]
    fn absolute_and_bare_bases_round_trip() {
        // Absolute source with `/` base (receiver strips the leading slash).
        round_trips("/foo/bar", "foo/bar");
        // Bare relative basename: empty base, name == full.
        round_trips("file", "file");
        round_trips("rel/dir/file", "rel/dir/file");
    }

    #[test]
    fn dot_transfer_root_entry_round_trips() {
        // The "." transfer-root entry stores its full directory as the base and
        // must not gain a trailing "/.".
        let base = derive_source_base(Path::new("/srv/data"), Path::new("."));
        assert_eq!(base, Path::new("/srv/data"));
        assert_eq!(
            join_source_path(&base, Path::new(".")),
            Path::new("/srv/data")
        );
    }

    #[test]
    fn same_base_derives_identical_value_for_sharing() {
        // Every entry of one source derives the identical base value, which is
        // what lets the one-entry interning cache collapse them onto a single
        // Arc allocation.
        let a = derive_source_base(Path::new("/srv/data/x/y"), Path::new("x/y"));
        let b = derive_source_base(Path::new("/srv/data/z"), Path::new("z"));
        let c = derive_source_base(
            Path::new("/srv/data/deep/nested/f"),
            Path::new("deep/nested/f"),
        );
        assert_eq!(a, Path::new("/srv/data"));
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn degenerate_trailing_slash_file_reconstructs_same_inode() {
        // A trailing-slash source that resolved to a non-directory: the trailing
        // slash sits after the name and is dropped by component handling, so the
        // reconstruction loses it but opens the same inode.
        let base = derive_source_base(Path::new("/srv/mod/file/"), Path::new("file"));
        assert_eq!(base, Path::new("/srv/mod"));
        assert_eq!(
            join_source_path(&base, Path::new("file")),
            Path::new("/srv/mod/file")
        );
    }
}

#[cfg(all(test, windows))]
mod source_base_windows_tests {
    //! Windows regression coverage: the wire name is `/`-separated (the flist
    //! sorts on `/`) while the on-disk path uses `\`. A byte- or string-based
    //! suffix strip mis-derives the base here, so this must run in Windows CI.
    use super::{derive_source_base, join_source_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn forward_slash_wire_name_over_backslash_path_round_trips() {
        // Exact shape of the failing push: name `dir/file`, on-disk `C:\src\dir\file`.
        let full = Path::new(r"C:\src\dir\file");
        let name = Path::new("dir/file");
        let base = derive_source_base(full, name);
        assert_eq!(base, Path::new(r"C:\src"));
        let rebuilt = join_source_path(&base, name);
        // Reconstructed natively and byte-for-byte equal to the original path.
        assert_eq!(rebuilt, PathBuf::from(r"C:\src\dir\file"));
        assert_eq!(
            rebuilt.as_os_str().as_encoded_bytes(),
            full.as_os_str().as_encoded_bytes(),
        );
    }

    #[test]
    fn deep_forward_slash_name_round_trips() {
        let full = Path::new(r"C:\a\b\c\d\e.bin");
        let name = Path::new("b/c/d/e.bin");
        let base = derive_source_base(full, name);
        assert_eq!(base, Path::new(r"C:\a"));
        assert_eq!(
            join_source_path(&base, name),
            PathBuf::from(r"C:\a\b\c\d\e.bin"),
        );
    }

    #[test]
    fn top_level_backslash_name_round_trips() {
        // A top-level entry: on-disk `\` name, wire `/` (single component here).
        let full = Path::new(r"C:\srv\module\foo");
        let name = Path::new("foo");
        let base = derive_source_base(full, name);
        assert_eq!(base, Path::new(r"C:\srv\module"));
        assert_eq!(
            join_source_path(&base, name),
            PathBuf::from(r"C:\srv\module\foo"),
        );
    }
}
