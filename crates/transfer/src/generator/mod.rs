#![deny(unsafe_code)]
//! Server-side Generator role implementation.
//!
//! When the native server operates as a Generator (sender), it:
//! 1. Walks the local filesystem to build a file list
//! 2. Sends the file list to the client (receiver)
//! 3. Receives signatures from the client for existing files
//! 4. Generates and sends deltas for each file
//!
//! # Upstream Reference
//!
//! - `generator.c` - Upstream generator role implementation
//! - `flist.c` - File list building and transmission
//! - `match.c` - Block matching and delta generation
//!
//! This module mirrors upstream rsync's generator behavior while leveraging
//! Rust's type system for safety.
//!
//! # Implementation Guide
//!
//! For comprehensive documentation on how the generator works within the delta transfer
//! algorithm, see the [`crate::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::receiver`] - The receiver role that applies deltas from the generator
//! - [`engine::delta::DeltaGenerator`] - Core delta generation engine
//! - [`engine::delta::DeltaSignatureIndex`] - Fast block lookup for delta generation
//! - [`engine::signature`] - Signature reconstruction from wire format
//! - [`protocol::wire`] - Wire format for signatures and deltas

mod delta;
mod file_list;
mod filters;
mod item_flags;
pub(crate) mod itemize;
mod protocol_io;
#[cfg(test)]
mod tests;
mod transfer;

use std::path::PathBuf;
use std::time::Instant;

use protocol::codec::NdxCodecEnum;
use protocol::flist::FileEntry;
use protocol::idlist::IdList;
use protocol::stats::DeleteStats;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use crate::role_trailer::error_location;

use super::config::ServerConfig;
use super::handshake::HandshakeResult;

pub use self::delta::generate_delta_from_signature;
pub use self::item_flags::ItemFlags;
pub use self::protocol_io::{calculate_duration_ms, read_signature_blocks};

/// I/O error flags for file list building and transfer.
///
/// Bitfield constants OR'd together to track error categories. Propagated to the
/// client summary and mapped to rsync exit codes via [`to_exit_code`].
///
/// # Upstream Reference
///
/// - `rsync.h:168-170` - `IOERR_GENERAL`, `IOERR_VANISHED`, `IOERR_DEL_LIMIT`
pub mod io_error_flags {
    /// General I/O error occurred during file operations.
    /// Must be 1 for backward compatibility with upstream rsync.
    pub const IOERR_GENERAL: i32 = 1 << 0;
    /// A file or directory vanished (was deleted) during the transfer.
    pub const IOERR_VANISHED: i32 = 1 << 1;
    /// Delete limit was exceeded during --delete operations.
    pub const IOERR_DEL_LIMIT: i32 = 1 << 2;

    /// Converts an accumulated `io_error` bitfield into the corresponding rsync
    /// exit code.
    ///
    /// Mirrors upstream `log.c` — `log_exit()` which maps the io_error flags to
    /// `RERR_*` exit codes. Returns 0 when no error bits are set.
    ///
    /// # Exit code mapping
    ///
    /// | Condition | Code | Upstream constant |
    /// |-----------|------|-------------------|
    /// | `IOERR_DEL_LIMIT` set | 25 | `RERR_DEL_LIMIT` |
    /// | `IOERR_VANISHED` set (only) | 24 | `RERR_VANISHED` |
    /// | `IOERR_GENERAL` set | 23 | `RERR_PARTIAL` |
    /// | No bits set | 0 | success |
    #[must_use]
    pub const fn to_exit_code(io_error: i32) -> i32 {
        if io_error & IOERR_DEL_LIMIT != 0 {
            25 // RERR_DEL_LIMIT
        } else if io_error & IOERR_GENERAL != 0 {
            23 // RERR_PARTIAL
        } else if io_error & IOERR_VANISHED != 0 {
            24 // RERR_VANISHED
        } else {
            0
        }
    }
}

/// A pending file list sub-segment for incremental recursion sending.
///
/// References entries in `GeneratorContext::file_list` by range rather than
/// storing cloned entries, avoiding double allocation.
#[derive(Debug)]
struct PendingSegment {
    /// Global NDX of the parent directory.
    parent_dir_ndx: i32,
    /// Start index into `GeneratorContext::file_list`.
    flist_start: usize,
    /// Number of entries in this segment.
    count: usize,
}

/// Classification of a directory's children for incremental recursion.
///
/// Groups the original file list indices belonging to a single directory
/// segment, along with the directory's NDX used as parent reference when
/// sending the segment over the wire.
#[derive(Debug)]
struct SegmentClassification {
    /// Directory NDX assigned to this directory.
    dir_ndx: usize,
    /// Original file list indices of entries belonging to this directory.
    child_indices: Vec<usize>,
}

/// Timing and byte-count statistics collected during the transfer.
#[derive(Debug)]
struct TransferTiming {
    /// When file list building started (for flist_buildtime statistic).
    flist_build_start: Option<Instant>,
    /// When file list building ended (for flist_buildtime statistic).
    flist_build_end: Option<Instant>,
    /// When file list transfer started (for flist_xfertime statistic).
    flist_xfer_start: Option<Instant>,
    /// When file list transfer ended (for flist_xfertime statistic).
    flist_xfer_end: Option<Instant>,
    /// Total bytes read from network during transfer (for total_read statistic).
    total_bytes_read: u64,
}

impl TransferTiming {
    /// Creates a new timing tracker with no recorded timestamps.
    fn new() -> Self {
        Self {
            flist_build_start: None,
            flist_build_end: None,
            flist_xfer_start: None,
            flist_xfer_end: None,
            total_bytes_read: 0,
        }
    }
}

/// Mutable state for INC_RECURSE segmented file list sending.
#[derive(Debug)]
struct IncrementalState {
    /// Pending file list segments for incremental recursion (INC_RECURSE).
    ///
    /// When INC_RECURSE is negotiated, the initial `send_file_list()` sends
    /// only top-level entries. Remaining per-directory segments are queued here
    /// and drained by `send_extra_file_lists()` during the transfer loop.
    pending_segments: Vec<PendingSegment>,
    /// Whether all incremental file list segments have been sent.
    flist_eof_sent: bool,
    /// Cached file list writer for compression state continuity across sub-lists.
    ///
    /// Upstream rsync uses `static` variables in `send_file_entry()` that persist
    /// across `send_file_list()` calls. This field preserves the same state
    /// (prev_name, prev_mode, prev_uid, prev_gid) between `send_file_list()`
    /// and `send_extra_file_lists()`.
    flist_writer_cache: Option<protocol::flist::FileListWriter>,
    /// Number of entries in the initial segment when INC_RECURSE is active.
    ///
    /// When set, `send_file_list()` only sends the first `initial_segment_count`
    /// entries. The remaining entries are sent via `send_extra_file_lists()`.
    initial_segment_count: Option<usize>,
    /// Segment boundary table for mapping wire NDX values to flat array indices.
    ///
    /// With INC_RECURSE, the sender sends segmented file lists with +1 gaps
    /// between segments (upstream `flist.c:2931`). When the receiver sends
    /// wire NDX values back, this table maps them to flat array indices.
    /// Each entry is `(flat_start, ndx_start)`.
    ///
    /// Without INC_RECURSE, this contains a single entry `(0, 0)`.
    ndx_segments: Vec<(usize, i32)>,
}

impl IncrementalState {
    /// Creates initial state with `ndx_start` derived from INC_RECURSE negotiation.
    fn new(initial_ndx_start: i32) -> Self {
        Self {
            pending_segments: Vec::new(),
            flist_eof_sent: false,
            flist_writer_cache: None,
            initial_segment_count: None,
            ndx_segments: vec![(0, initial_ndx_start)],
        }
    }
}

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
    protocol: ProtocolVersion,
    /// Server configuration.
    config: ServerConfig,
    /// List of files to send (contains relative paths for wire transmission).
    ///
    /// **Invariant**: `file_list` and `full_paths` must always have the same length.
    /// Use [`Self::push_file_item`] to add entries and [`Self::clear_file_list`] to clear.
    file_list: Vec<FileEntry>,
    /// Full filesystem paths for each file in `file_list` (parallel array).
    /// Used to open files for delta generation during transfer.
    ///
    /// **Invariant**: `file_list[i]` corresponds to `full_paths[i]` for all valid indices.
    full_paths: Vec<PathBuf>,
    /// Filter rules received from client.
    filters: Option<filters::FilterSet>,
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
    /// Timing and byte-count statistics for the transfer.
    timing: TransferTiming,
    /// Collected UID mappings for name-based ownership transfer.
    uid_list: IdList,
    /// Collected GID mappings for name-based ownership transfer.
    gid_list: IdList,
    /// I/O error flags accumulated during file list building and transfer.
    /// Uses [`io_error_flags`] constants (IOERR_GENERAL, IOERR_VANISHED, etc.).
    io_error: i32,
    /// Incremental recursion (INC_RECURSE) state for segmented file list sending.
    incremental: IncrementalState,
    /// Accumulated deletion statistics received via NDX_DEL_STATS messages.
    /// (upstream: main.c:238-247 `read_del_stats()`)
    delete_stats: DeleteStats,
}

impl GeneratorContext {
    /// Creates a new generator context from a completed handshake and server config.
    ///
    /// Initializes protocol state, INC_RECURSE NDX offset, and empty file list.
    /// Call [`build_file_list`](Self::build_file_list) to populate entries, then
    /// [`run`](Self::run) to execute the transfer.
    #[must_use]
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        // upstream: flist.c:2923 — ndx_start = inc_recurse ? 1 : 0
        let inc_recurse = handshake
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        let initial_ndx_start = if inc_recurse { 1 } else { 0 };

        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            full_paths: Vec::new(),
            filters: None,
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
            timing: TransferTiming::new(),
            uid_list: IdList::new(),
            gid_list: IdList::new(),
            io_error: 0,
            incremental: IncrementalState::new(initial_ndx_start),
            delete_stats: DeleteStats::new(),
        }
    }

    /// Converts a wire NDX value to a flat file list array index.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:424` — `i = ndx - cur_flist->ndx_start`
    fn wire_to_flat_ndx(&self, wire_ndx: i32) -> usize {
        for &(flat_start, ndx_start) in self.incremental.ndx_segments.iter().rev() {
            if wire_ndx >= ndx_start {
                return flat_start + (wire_ndx - ndx_start) as usize;
            }
        }
        wire_ndx as usize
    }

    /// Converts a flat file list array index to a wire NDX value.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2321` — `ndx = i + cur_flist->ndx_start`
    fn flat_to_wire_ndx(&self, flat_idx: usize) -> i32 {
        let seg_idx = self
            .incremental
            .ndx_segments
            .partition_point(|&(start, _)| start <= flat_idx)
            - 1;
        let (flat_start, ndx_start) = self.incremental.ndx_segments[seg_idx];
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
    fn inc_recurse(&self) -> bool {
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
    fn itemize_context(&self) -> itemize::ItemizeContext {
        itemize::ItemizeContext {
            preserve_mtimes: self.config.flags.times,
            receiver_symlink_times: self
                .compat_flags
                .is_some_and(|f| f.contains(CompatibilityFlags::SYMLINK_TIMES)),
        }
    }

    /// Creates a configured `FileListWriter` matching the current protocol and flags.
    fn build_flist_writer(&self) -> protocol::flist::FileListWriter {
        use super::shared::ChecksumFactory;

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
        .with_preserve_xattrs(self.config.flags.xattrs);

        // upstream: flist.c — always_checksum includes per-file checksums in the file list
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

    /// Returns the generated file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        debug_assert_eq!(
            self.file_list.len(),
            self.full_paths.len(),
            "file_list and full_paths must be kept in sync"
        );
        &self.file_list
    }

    /// Adds a file entry and its corresponding full path to the file list.
    ///
    /// This method maintains the invariant that `file_list` and `full_paths`
    /// have the same length and corresponding entries at each index.
    fn push_file_item(&mut self, entry: FileEntry, full_path: PathBuf) {
        debug_assert_eq!(
            self.file_list.len(),
            self.full_paths.len(),
            "file_list and full_paths must be kept in sync before push"
        );
        self.file_list.push(entry);
        self.full_paths.push(full_path);
    }

    /// Clears both the file list and full paths arrays.
    ///
    /// This method maintains the invariant that both arrays are cleared together.
    fn clear_file_list(&mut self) {
        self.file_list.clear();
        self.full_paths.clear();
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
    const fn should_activate_input_multiplex(&self) -> bool {
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
    fn record_io_error(&mut self, error: &std::io::Error) {
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
    const fn get_checksum_algorithm(&self) -> protocol::ChecksumAlgorithm {
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
    fn file_compression(&self, path: &std::path::Path) -> Option<protocol::CompressionAlgorithm> {
        let algo = self.negotiated_algorithms.map(|n| n.compression)?;
        if let Some(ref skip_list) = self.config.skip_compress {
            if skip_list.matches_path(path) {
                return None;
            }
        }
        Some(algo)
    }

    /// Opens a source file for reading, using io_uring for large files when available.
    ///
    /// Files at or above the io_uring read threshold (1 MB) use `reader_from_path`,
    /// which creates an io_uring-backed reader on Linux 5.6+ (respecting the
    /// `--no-io-uring` flag). Smaller files use a standard `BufReader` to avoid
    /// the overhead of creating an io_uring ring per file.
    ///
    /// This threshold-based dual-path mirrors the existing pattern used for
    /// parallel stat (PARALLEL_STAT_THRESHOLD) and adaptive buffer sizing.
    fn open_source_reader(
        &self,
        path: &std::path::Path,
        file_size: u64,
    ) -> std::io::Result<Box<dyn std::io::Read>> {
        use crate::adaptive_buffer::adaptive_buffer_size;

        // 1 MB threshold: io_uring ring creation has fixed overhead that only
        // pays off for larger reads where batched syscalls reduce total cost.
        const IO_URING_READ_THRESHOLD: u64 = 1024 * 1024;

        if file_size >= IO_URING_READ_THRESHOLD
            && self.config.write.io_uring_policy != fast_io::IoUringPolicy::Disabled
        {
            match fast_io::reader_from_path(path, self.config.write.io_uring_policy) {
                Ok(r) => return Ok(Box::new(r)),
                Err(_) => {
                    // Fall through to standard BufReader on io_uring failure
                }
            }
        }

        let f = std::fs::File::open(path)?;
        Ok(Box::new(std::io::BufReader::with_capacity(
            adaptive_buffer_size(file_size),
            f,
        )))
    }

    /// Validates that a file index is within bounds of the file list.
    fn validate_file_index(&self, ndx: usize) -> std::io::Result<()> {
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
}

/// Result from the transfer loop phase of the generator.
///
/// Contains statistics from processing file transfer requests.
#[derive(Debug, Clone)]
struct TransferLoopResult {
    /// Number of files actually transferred.
    files_transferred: usize,
    /// Total bytes sent during transfer.
    bytes_sent: u64,
    /// NDX read codec state carried over for the goodbye handshake.
    ndx_read_codec: NdxCodecEnum,
    /// NDX write codec state carried over for the goodbye handshake.
    ndx_write_codec: NdxCodecEnum,
}

/// Statistics from a generator (sender) transfer operation.
///
/// Returned inside [`crate::ServerStats::Generator`] after a successful send.
/// Contains file counts, byte totals, and file-list timing metrics.
///
/// # Upstream Reference
///
/// - `main.c:356-384` - `handle_stats()` sends/receives these statistics
/// - `sender.c:462` - `total_written` accumulated during `send_files()`
#[derive(Debug, Clone, Default)]
pub struct GeneratorStats {
    /// Number of files in the sent file list.
    pub files_listed: usize,
    /// Number of files actually transferred (delta or whole-file).
    pub files_transferred: usize,
    /// Total bytes sent to the receiver (delta data + literals).
    pub bytes_sent: u64,
    /// Total bytes read from the receiver (signatures, NDX requests).
    pub bytes_read: u64,
    /// File list build time in milliseconds (upstream: `stats.flist_buildtime`).
    pub flist_buildtime_ms: u64,
    /// File list transfer time in milliseconds (upstream: `stats.flist_xfertime`).
    pub flist_xfertime_ms: u64,
    /// Accumulated deletion statistics from the receiver via `NDX_DEL_STATS`.
    pub delete_stats: DeleteStats,
    /// Accumulated I/O error flags from file list building and transfer.
    ///
    /// Uses [`io_error_flags`] constants. When `IOERR_VANISHED` is set and
    /// `IOERR_GENERAL` is not, the exit code should be 24 (partial transfer
    /// due to vanished files). Propagated to the client summary so the
    /// process exit code reflects files that disappeared mid-transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1338-1345`: `log_exit()` maps `io_error` to `RERR_VANISHED` (24).
    pub io_error: i32,
}

/// Returns `true` when the I/O error indicates an early connection close.
///
/// During dry-run and at phase boundaries, the upstream daemon may close the
/// socket before the sender finishes the goodbye handshake. These error kinds
/// all represent "peer went away" rather than a protocol error.
///
/// # Upstream Reference
///
/// - `sender.c:225-232` - tolerant error handling for dry-run
/// - `main.c:875-906` - `read_final_goodbye()` with early close tolerance
fn is_early_close_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ConnectionAborted
    )
}
