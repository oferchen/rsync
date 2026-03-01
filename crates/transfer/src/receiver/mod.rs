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

mod flist;

use flist::FailedDirectories;
pub use flist::IncrementalFileListReceiver;

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};

use logging::info_log;
use std::num::NonZeroU8;
use std::path::{Component, Path, PathBuf};

/// Default checksum length for delta verification (16 bytes = 128 bits).
///
/// This matches upstream rsync's default MD5 digest length and provides
/// sufficient collision resistance for file integrity verification.
const DEFAULT_CHECKSUM_LENGTH: NonZeroU8 = NonZeroU8::new(16).unwrap();

/// Minimum candidate count to justify rayon thread-pool overhead for
/// parallel stat() calls in the quick-check phase. Below this threshold,
/// sequential iteration is faster.
const PARALLEL_STAT_THRESHOLD: usize = 64;

use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::filters::read_filter_list;
use protocol::flist::{FileEntry, FileListReader};
use protocol::idlist::IdList;
#[cfg(test)]
use protocol::wire::DeltaOp;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use super::adaptive_buffer::adaptive_writer_capacity;
use super::delta_apply::{ChecksumVerifier, SparseWriteState};
use super::map_file::MapFile;
use super::token_buffer::TokenBuffer;

#[cfg(test)]
use engine::delta::{DeltaScript, DeltaToken};
use engine::delta::{SignatureLayoutParams, calculate_signature_layout};
use engine::fuzzy::FuzzyMatcher;
use engine::signature::{FileSignature, generate_file_signature};

use super::config::{ReferenceDirectory, ServerConfig};
use super::handshake::HandshakeResult;
use super::pipeline::{PipelineConfig, PipelineState};
use super::shared::ChecksumFactory;
#[cfg(test)]
use super::temp_guard::TempFileGuard;
use super::temp_guard::open_tmpfile;
use super::transfer_ops::{
    RequestConfig, ResponseContext, process_file_response_streaming, send_file_request,
};

use metadata::{MetadataOptions, apply_metadata_from_file_entry, apply_metadata_with_cached_stat};

/// Pure-function quick-check: compares destination stat against source entry.
///
/// Returns `Some(metadata)` when the destination already matches (skip transfer),
/// `None` when the file needs transfer. Thread-safe for use with `rayon::par_iter`.
///
/// Mirrors upstream `generator.c:617 quick_check_ok()` for `FT_REG`.
fn quick_check_ok_stateless(
    entry: &FileEntry,
    dest_dir: &Path,
    preserve_times: bool,
) -> Option<fs::Metadata> {
    if !preserve_times {
        return None;
    }
    let file_path = dest_dir.join(entry.path());
    let dest_meta = fs::metadata(&file_path).ok()?;
    if dest_meta.len() != entry.size() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if dest_meta.mtime() == entry.mtime() {
            Some(dest_meta)
        } else {
            None
        }
    }
    #[cfg(not(unix))]
    {
        let mtime_matches = dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| d.as_secs() as i64 == entry.mtime());
        if mtime_matches { Some(dest_meta) } else { None }
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
    pub(super) protocol: ProtocolVersion,
    /// Server configuration.
    pub(super) config: ServerConfig,
    /// List of files to receive.
    pub(super) file_list: Vec<FileEntry>,
    /// Negotiated checksum and compression algorithms from Protocol 30+ capability negotiation.
    /// None for protocols < 30 or when negotiation was skipped.
    pub(super) negotiated_algorithms: Option<NegotiationResult>,
    /// Compatibility flags exchanged during protocol setup.
    ///
    /// Controls protocol-specific behaviors like incremental recursion (`INC_RECURSE`),
    /// checksum seed ordering (`CHECKSUM_SEED_FIX`), and file list encoding (`VARINT_FLIST_FLAGS`).
    /// None for protocols < 30 or when compat exchange was skipped.
    pub(super) compat_flags: Option<CompatibilityFlags>,
    /// Checksum seed for XXHash algorithms.
    pub(super) checksum_seed: i32,
    /// Segment boundary table for mapping flat array indices to wire NDX values.
    ///
    /// With INC_RECURSE, each segment has `ndx_start = prev_ndx_start + prev_used + 1`.
    /// Each entry is `(flat_start, ndx_start)`.
    /// Without INC_RECURSE, contains a single entry `(0, 0)`.
    ///
    /// upstream: flist.c:2931 — `flist->ndx_start = prev->ndx_start + prev->used + 1`
    pub(super) ndx_segments: Vec<(usize, i32)>,
    /// Cached file list reader for compression state continuity across sub-lists.
    ///
    /// Upstream rsync uses `static` variables in `recv_file_entry()` that persist
    /// across `recv_file_list()` calls. This field preserves the same state
    /// (prev_name, prev_mode, prev_uid, prev_gid) between `receive_file_list()`
    /// and `receive_extra_file_lists()`.
    pub(super) flist_reader_cache: Option<FileListReader>,
    /// UID mappings from remote to local IDs.
    pub(super) uid_list: IdList,
    /// GID mappings from remote to local IDs.
    pub(super) gid_list: IdList,
}

impl ReceiverContext {
    /// Creates a new receiver context from handshake result and config.
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
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
            ndx_segments: vec![(0, initial_ndx_start)],
            flist_reader_cache: None,
            uid_list: IdList::new(),
            gid_list: IdList::new(),
        }
    }

    /// Converts a flat file list array index to a wire NDX value.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2321` — `ndx = i + cur_flist->ndx_start`
    fn flat_to_wire_ndx(&self, flat_idx: usize) -> i32 {
        let seg_idx = self
            .ndx_segments
            .partition_point(|&(start, _)| start <= flat_idx)
            - 1;
        let (flat_start, ndx_start) = self.ndx_segments[seg_idx];
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

    /// Builds a [`BasisFileConfig`] for a single file, pulling shared state from `self`.
    fn build_basis_file_config<'a>(
        &'a self,
        file_path: &'a Path,
        dest_dir: &'a Path,
        relative_path: &'a Path,
        target_size: u64,
        checksum_length: NonZeroU8,
        checksum_algorithm: engine::signature::SignatureAlgorithm,
    ) -> BasisFileConfig<'a> {
        BasisFileConfig {
            file_path,
            dest_dir,
            relative_path,
            target_size,
            fuzzy_enabled: self.config.flags.fuzzy,
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
    #[must_use]
    pub const fn compat_flags(&self) -> Option<protocol::CompatibilityFlags> {
        self.compat_flags
    }

    /// Returns the received file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
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
        !self.config.client_mode && receiver_wants_list
    }

    /// Sanitizes the received file list by removing entries with unsafe paths.
    ///
    /// When `trust_sender` is false, the receiver validates each entry to prevent
    /// directory traversal attacks from a malicious sender:
    ///
    /// - Entries with absolute paths are rejected (unless `--relative` is active)
    /// - Entries containing `..` path components are rejected
    /// - Symlink entries pointing outside the transfer tree are rejected
    ///
    /// Rejected entries are removed from the file list and warnings are emitted.
    /// Returns the number of entries removed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:757`: `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
    /// - `options.c:2595`: `trust_sender_args = trust_sender_filter = 1`
    fn sanitize_file_list(&mut self) -> usize {
        if self.config.trust_sender {
            return 0;
        }

        let relative_paths = self.config.flags.relative;
        let original_len = self.file_list.len();

        self.file_list.retain(|entry| {
            let path = entry.path();

            // Check for absolute paths (reject unless --relative is active).
            // upstream: flist.c:757 `!relative_paths && *thisname == '/'`
            if !relative_paths && path.has_root() {
                info_log!(
                    Misc,
                    1,
                    "ERROR: rejecting file-list entry with absolute path from sender: {}",
                    path.display()
                );
                return false;
            }

            // Check for `..` path components (always rejected).
            // upstream: flist.c:757 `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS) < 0`
            if path_contains_dot_dot(path) {
                info_log!(
                    Misc,
                    1,
                    "ERROR: rejecting file-list entry with \"..\" component from sender: {}",
                    path.display()
                );
                return false;
            }

            // Check symlink targets for safety (similar to --safe-links).
            // Reject symlinks that point outside the transfer tree.
            if entry.is_symlink() {
                if let Some(target) = entry.link_target() {
                    if !symlink_target_is_safe_for_transfer(target, path) {
                        info_log!(
                            Misc,
                            1,
                            "ERROR: rejecting symlink with unsafe target from sender: {} -> {}",
                            path.display(),
                            target.display()
                        );
                        return false;
                    }
                }
            }

            true
        });

        original_len - self.file_list.len()
    }

    /// Creates directories from the file list, applying metadata in parallel.
    ///
    /// Two-phase approach: directory creation is sequential (cheap, respects
    /// parent-child ordering), metadata application (`chown`/`chmod`/`utimes`)
    /// runs in parallel via rayon when above [`PARALLEL_STAT_THRESHOLD`].
    ///
    /// Returns a list of metadata errors encountered (path, error message).
    fn create_directories(
        &self,
        dest_dir: &std::path::Path,
        metadata_opts: &MetadataOptions,
    ) -> io::Result<Vec<(PathBuf, String)>> {
        use rayon::prelude::*;

        let dir_entries: Vec<(&FileEntry, PathBuf)> = self
            .file_list
            .iter()
            .filter(|e| e.is_dir())
            .map(|entry| {
                let relative_path = entry.path();
                let dir_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(relative_path)
                };
                (entry, dir_path)
            })
            .collect();

        for (_, dir_path) in &dir_entries {
            if !dir_path.exists() {
                fs::create_dir_all(dir_path)?;
            }
        }

        if dir_entries.len() >= PARALLEL_STAT_THRESHOLD {
            Ok(dir_entries
                .par_iter()
                .filter_map(|(entry, dir_path)| {
                    apply_metadata_from_file_entry(dir_path, entry, metadata_opts)
                        .err()
                        .map(|e| (dir_path.clone(), e.to_string()))
                })
                .collect())
        } else {
            let mut errors = Vec::new();
            for (entry, dir_path) in &dir_entries {
                if let Err(e) = apply_metadata_from_file_entry(dir_path, entry, metadata_opts) {
                    errors.push((dir_path.clone(), e.to_string()));
                }
            }
            Ok(errors)
        }
    }

    /// Builds the list of files that need transfer, applying quick-check to skip
    /// unchanged files and respecting size bounds and failed directory tracking.
    ///
    /// Performs stat() calls in parallel (via rayon) when the candidate count
    /// exceeds [`PARALLEL_STAT_THRESHOLD`], falling back to sequential iteration
    /// for small lists where thread-pool overhead would dominate.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1809` — `quick_check_ok()` for `FT_REG`
    fn build_files_to_transfer<'a>(
        &'a self,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        failed_dirs: Option<&FailedDirectories>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        stats: &mut TransferStats,
    ) -> Vec<(usize, &'a FileEntry)> {
        use rayon::prelude::*;

        // Phase A: Filter candidates (cheap, in-memory checks only).
        let candidates: Vec<(usize, &FileEntry)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file())
            .filter(|(_, e)| {
                let sz = e.size();
                self.config.min_file_size.is_none_or(|m| sz >= m)
                    && self.config.max_file_size.is_none_or(|m| sz <= m)
            })
            .filter(|(_, e)| {
                if let Some(fd) = failed_dirs {
                    if let Some(failed_parent) = fd.failed_ancestor(e.name()) {
                        if self.config.flags.verbose && self.config.client_mode {
                            info_log!(
                                Skip,
                                1,
                                "skipping {} (parent {} failed)",
                                e.name(),
                                failed_parent
                            );
                        }
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                true
            })
            .collect();

        let preserve_times = self.config.flags.times;

        // Phase B: Stat each candidate to determine quick-check status.
        // Parallel when above threshold, sequential otherwise.
        if candidates.len() >= PARALLEL_STAT_THRESHOLD {
            let results: Vec<_> = candidates
                .par_iter()
                .map(|&(idx, entry)| {
                    let meta = quick_check_ok_stateless(entry, dest_dir, preserve_times);
                    (idx, entry, meta)
                })
                .collect();

            let mut files_to_transfer = Vec::with_capacity(results.len());
            for (idx, entry, meta) in results {
                if let Some(cached_meta) = meta {
                    let file_path = dest_dir.join(entry.path());
                    if let Err(e) = apply_metadata_with_cached_stat(
                        &file_path,
                        entry,
                        metadata_opts,
                        Some(cached_meta),
                    ) {
                        metadata_errors.push((file_path, e.to_string()));
                    }
                } else {
                    files_to_transfer.push((idx, entry));
                }
            }
            files_to_transfer
        } else {
            let mut files_to_transfer = Vec::with_capacity(candidates.len());
            for (idx, entry) in candidates {
                if let Some(cached_meta) = quick_check_ok_stateless(entry, dest_dir, preserve_times)
                {
                    let file_path = dest_dir.join(entry.path());
                    if let Err(e) = apply_metadata_with_cached_stat(
                        &file_path,
                        entry,
                        metadata_opts,
                        Some(cached_meta),
                    ) {
                        metadata_errors.push((file_path, e.to_string()));
                    }
                } else {
                    files_to_transfer.push((idx, entry));
                }
            }
            files_to_transfer
        }
    }

    /// Creates a single directory during incremental processing.
    ///
    /// On success, returns `Ok(true)`. On failure or skip, marks the directory
    /// as failed and returns `Ok(false)`. Only returns `Err` for unrecoverable errors.
    fn create_directory_incremental(
        &self,
        dest_dir: &std::path::Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
    ) -> io::Result<bool> {
        let relative_path = entry.path();
        let dir_path = if relative_path.as_os_str() == "." {
            dest_dir.to_path_buf()
        } else {
            dest_dir.join(relative_path)
        };

        // Check if parent is under a failed directory
        if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
            if self.config.flags.verbose && self.config.client_mode {
                info_log!(
                    Skip,
                    1,
                    "skipping directory {} (parent {} failed)",
                    entry.name(),
                    failed_parent
                );
            }
            failed_dirs.mark_failed(entry.name());
            return Ok(false);
        }

        // Try to create the directory
        if !dir_path.exists() {
            if let Err(e) = fs::create_dir_all(&dir_path) {
                if self.config.flags.verbose && self.config.client_mode {
                    info_log!(
                        Misc,
                        1,
                        "failed to create directory {}: {}",
                        dir_path.display(),
                        e
                    );
                }
                failed_dirs.mark_failed(entry.name());
                return Ok(false);
            }
        }

        // Apply metadata (non-fatal errors)
        if let Err(e) = apply_metadata_from_file_entry(&dir_path, entry, metadata_opts) {
            if self.config.flags.verbose && self.config.client_mode {
                info_log!(
                    Misc,
                    1,
                    "warning: metadata error for {}: {}",
                    dir_path.display(),
                    e
                );
            }
        }

        if self.config.flags.verbose && self.config.client_mode {
            if relative_path.as_os_str() == "." {
                info_log!(Name, 1, "./");
            } else {
                info_log!(Name, 1, "{}/", relative_path.display());
            }
        }

        Ok(true)
    }

    /// Exchanges NDX_DONE messages for phase transitions.
    ///
    /// After sending all file requests, exchanges NDX_DONEs with the sender
    /// for multi-phase protocol (protocol >= 29 has 2 phases).
    fn exchange_phase_done<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<()> {
        // Exchange NDX_DONE markers to complete the transfer phases.
        //
        // The sender's send_files() loop (sender.c:225-462) reads NDX_DONE from
        // us and either frees a file list segment or transitions to the next phase.
        // After breaking out of the loop, it writes a final NDX_DONE (sender.c:462).
        //
        // With INC_RECURSE, the sender maintains a linked list of file list segments
        // (first_flist). Each NDX_DONE we send causes the sender to free one segment.
        // When all segments are freed (first_flist becomes NULL), subsequent NDX_DONEs
        // trigger phase transitions (++phase). The sender breaks when phase > max_phase.
        //
        // Without INC_RECURSE, there are no segments to free — all NDX_DONEs go
        // directly to phase transitions.
        //
        // upstream: sender.c:236-258 — NDX_DONE handling in send_files()
        // upstream: sender.c:462 — final NDX_DONE after loop exit
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        let max_phase: i32 = if self.protocol.supports_multi_phase() {
            2
        } else {
            1
        };

        if inc_recurse {
            // Send one NDX_DONE per file list segment. The sender frees one
            // segment per NDX_DONE and echoes each back:
            //   - Segments 0..N-2: frees segment, first_flist non-NULL → echo
            //   - Segment N-1 (last): frees segment, first_flist NULL →
            //     falls through to phase transition (++phase → 1) → echo
            //
            // upstream: sender.c:242-250 — segment freeing with echo
            let num_segments = self.ndx_segments.len();
            for _ in 0..num_segments {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "segment completion")?;
            }
            // Sender is now at phase 1 (incremented during last segment free).

            // Send remaining phase transitions. The sender is at phase 1 after
            // the segment completions, so we need phases 2..=max_phase (each
            // echoed) plus one final NDX_DONE that triggers break (no echo).
            //
            // upstream: sender.c:252-257 — phase transition with echo
            for _phase in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }

            // Final NDX_DONE that causes sender to break (++phase > max_phase).
            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        } else {
            // Without INC_RECURSE: all NDX_DONEs are phase transitions.
            // Phases 1..=max_phase each get an echo; the last (> max_phase) breaks.
            let mut phase: i32 = 0;
            loop {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                phase += 1;
                if phase > max_phase {
                    break;
                }
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }
        }

        // Read the final NDX_DONE that the sender writes after exiting its
        // send_files() loop (sender.c:462: `write_ndx(f_out, NDX_DONE)`).
        self.read_expected_ndx_done(ndx_read_codec, reader, "sender final")?;

        Ok(())
    }

    /// Reads an NDX and validates it is NDX_DONE (-1).
    fn read_expected_ndx_done<R: Read>(
        &self,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        reader: &mut R,
        context: &str,
    ) -> io::Result<()> {
        let ndx = ndx_read_codec.read_ndx(reader)?;
        if ndx != -1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected NDX_DONE (-1) from sender during {context}, got {ndx}"),
            ));
        }
        Ok(())
    }

    /// Handles the goodbye handshake at end of transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:875-907` - `read_final_goodbye()`
    fn handle_goodbye<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<()> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        // Send goodbye NDX_DONE
        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        // For protocol >= 31, sender echoes NDX_DONE and expects another
        if self.protocol.supports_extended_goodbye() {
            // Read echoed NDX_DONE from sender
            let goodbye_echo = ndx_read_codec.read_ndx(reader)?;
            if goodbye_echo != -1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected goodbye NDX_DONE echo (-1) from sender, got {goodbye_echo}"),
                ));
            }

            // Send final goodbye NDX_DONE
            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        }

        Ok(())
    }

    /// Receives transfer statistics from the sender.
    ///
    /// The sender transmits statistics after the transfer loop completes but before
    /// the goodbye handshake. We need to consume these to keep the protocol in sync.
    ///
    /// # Wire Format
    ///
    /// - total_read: i64 (sender's bytes read)
    /// - total_written: i64 (sender's bytes written)
    /// - total_size: i64 (total file size)
    /// - For protocol 29+: flist_buildtime: i64 (file list build time in ms)
    /// - For protocol 29+: flist_xfertime: i64 (file list transfer time in ms)
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1102-1130` - `read_final_stats()`
    fn receive_stats<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<SenderStats> {
        let stats_codec = create_protocol_codec(self.protocol.as_u8());

        let total_read = stats_codec.read_stat(reader)? as u64;
        let total_written = stats_codec.read_stat(reader)? as u64;
        let total_size = stats_codec.read_stat(reader)? as u64;

        let (flist_buildtime_ms, flist_xfertime_ms) = if self.protocol.supports_flist_times() {
            let buildtime = stats_codec.read_stat(reader)? as u64;
            let xfertime = stats_codec.read_stat(reader)? as u64;
            (Some(buildtime), Some(xfertime))
        } else {
            (None, None)
        };

        Ok(SenderStats {
            total_read,
            total_written,
            total_size,
            flist_buildtime_ms,
            flist_xfertime_ms,
        })
    }

    /// Runs the receiver role to completion.
    ///
    /// This orchestrates the full receive operation:
    /// 1. Receive file list
    /// 2. For each file: generate signature, receive delta, apply
    /// 3. Set final metadata
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use core::server::{ReceiverContext, ServerConfig, HandshakeResult};
    /// use core::server::role::ServerRole;
    /// use core::server::flags::ParsedServerFlags;
    /// use core::server::reader::ServerReader;
    /// use protocol::ProtocolVersion;
    /// use std::io::{stdin, stdout};
    /// use std::ffi::OsString;
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let handshake = HandshakeResult {
    ///     protocol: ProtocolVersion::try_from(32u8).unwrap(),
    ///     buffered: Vec::new(),
    ///     compat_exchanged: false,
    /// };
    ///
    /// let config = ServerConfig {
    ///     role: ServerRole::Receiver,
    ///     protocol: ProtocolVersion::try_from(32u8).unwrap(),
    ///     flag_string: "-a".to_string(),
    ///     flags: ParsedServerFlags::default(),
    ///     args: vec![OsString::from(".")],
    /// };
    ///
    /// let mut ctx = ReceiverContext::new(&handshake, config);
    ///
    /// // Run receiver role with stdin/stdout
    /// let reader = ServerReader::Plain(stdin().lock());
    /// let stats = ctx.run(reader, &mut stdout().lock())?;
    /// info_log!(Stats, 1, "Transferred {} files ({} bytes)",
    ///           stats.files_transferred, stats.bytes_received);
    /// # Ok(())
    /// # }
    /// ```
    pub fn run<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
        progress: Option<&mut dyn super::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        // Use pipelined transfer by default for improved performance.
        // When incremental-flist feature is enabled, use incremental mode
        // which provides failed directory tracking and better error recovery.
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(reader, writer, PipelineConfig::default(), progress)
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            let _ = progress;
            self.run_pipelined(reader, writer, PipelineConfig::default())
        }
    }

    /// Runs the receiver with synchronous (non-pipelined) transfer.
    ///
    /// This method is kept for compatibility and testing purposes.
    /// For production use, prefer the default `run()` which uses pipelining.
    pub fn run_sync<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length,
            checksum_algorithm,
        } = setup;

        // Transfer loop: for each file, generate signature, receive delta, apply
        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories from file list
        let mut metadata_errors = self.create_directories(&dest_dir, &metadata_opts)?;

        // Transfer loop: iterate through file list and request each file from sender
        // The receiver (generator side) drives the transfer by sending file indices
        // to the sender, which responds with delta data.
        //
        // Mirrors upstream recv_generator() which:
        // 1. Iterates through file list
        // 2. For each file to transfer: sends ndx, then signature
        // 3. Waits for sender to send delta
        //
        // Use NdxCodec Strategy pattern for protocol-version-aware NDX encoding.
        // The codec handles both legacy (4-byte LE) and modern (delta) formats,
        // and maintains its own prev_positive state for delta encoding.
        //
        // We need separate codecs for write and read because:
        // 1. The receiver writes NDX to request files from sender
        // 2. The sender writes NDX back to confirm which file it's sending
        // Each side maintains its own delta encoding state independently.
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // Reusable per-file resources — created once, reset between files
        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let mut token_buffer = TokenBuffer::with_default_capacity();

        let deadline = super::shared::TransferDeadline::from_system_time(self.config.stop_at);

        for (file_idx, file_entry) in self.file_list.iter().enumerate() {
            // Check deadline at file boundary before starting next file.
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    break;
                }
            }

            let relative_path = file_entry.path();

            // Compute actual file path
            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };

            // Skip non-regular files (directories, symlinks, devices, etc.)
            if !file_entry.is_file() {
                if file_entry.is_dir() && self.config.flags.verbose && self.config.client_mode {
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
                continue;
            }

            // Skip files outside the configured size range.
            let file_size = file_entry.size();
            if let Some(min_limit) = self.config.min_file_size {
                if file_size < min_limit {
                    continue;
                }
            }
            if let Some(max_limit) = self.config.max_file_size {
                if file_size > max_limit {
                    continue;
                }
            }

            // upstream: rsync.c:674
            if self.config.flags.verbose && self.config.client_mode {
                info_log!(Name, 1, "{}", relative_path.display());
            }

            // Convert flat index to wire NDX using segment boundary table.
            // upstream: generator.c:2321 — ndx = i + cur_flist->ndx_start
            let ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, ndx)?;

            // For protocol >= 29, sender expects iflags after NDX
            if self.protocol.supports_iflags() {
                writer.write_all(&SenderAttrs::ITEM_TRANSFER.to_le_bytes())?;
            }

            // Generate signature if basis file exists
            let basis_config = self.build_basis_file_config(
                &file_path,
                &dest_dir,
                relative_path,
                file_entry.size(),
                checksum_length,
                checksum_algorithm,
            );
            let basis_result = find_basis_file_with_config(&basis_config);
            let signature_opt = basis_result.signature;
            let basis_path_opt = basis_result.basis_path;

            // Send sum_head (signature header) — upstream write_sum_head()
            let sum_head = match signature_opt {
                Some(ref signature) => SumHead::from_signature(signature),
                None => SumHead::empty(),
            };
            sum_head.write(&mut *writer)?;

            if let Some(ref signature) = signature_opt {
                write_signature_blocks(&mut *writer, signature, sum_head.s2length)?;
            }
            writer.flush()?;

            // Read sender attributes (echoed NDX + iflags)
            let (echoed_ndx, _sender_attrs) =
                SenderAttrs::read_with_codec(reader, &mut ndx_read_codec)?;

            debug_assert_eq!(
                echoed_ndx, ndx,
                "sender echoed NDX {echoed_ndx} but we requested {ndx}"
            );

            // Read sum_head echoed by sender (we don't use it, but must consume it)
            let _echoed_sum_head = SumHead::read(reader)?;

            // Apply delta to reconstruct file
            // upstream: receiver.c open_tmpfile() → do_mkstemp() with ".filename.XXXXXX"
            let (file, mut temp_guard) = open_tmpfile(&file_path, None)?;
            let target_size = file_entry.size();
            let writer_capacity = adaptive_writer_capacity(target_size);
            let mut output = std::io::BufWriter::with_capacity(writer_capacity, file);
            let mut total_bytes: u64 = 0;

            let use_sparse = self.config.flags.sparse;
            let mut sparse_state = if use_sparse {
                Some(SparseWriteState::default())
            } else {
                None
            };

            // MapFile: Cache basis file with 256KB sliding window
            let mut basis_map = if let Some(ref path) = basis_path_opt {
                Some(MapFile::open(path).map_err(|e| {
                    io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
                })?)
            } else {
                None
            };

            // Read tokens in a loop
            loop {
                let mut token_buf = [0u8; 4];
                reader.read_exact(&mut token_buf)?;
                let token = i32::from_le_bytes(token_buf);

                if token == 0 {
                    // End of file — verify checksum using stack buffers.
                    // Use mem::replace to reset the verifier for the next file.
                    let checksum_len = checksum_verifier.digest_len();
                    let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                    reader.read_exact(&mut expected_buf[..checksum_len])?;

                    let algo = checksum_verifier.algorithm();
                    let old_verifier = std::mem::replace(
                        &mut checksum_verifier,
                        ChecksumVerifier::for_algorithm(algo),
                    );
                    let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                    let computed_len = old_verifier.finalize_into(&mut computed);
                    if computed_len != checksum_len {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "checksum length mismatch for {file_path:?}: expected {checksum_len} bytes, got {computed_len} bytes",
                            ),
                        ));
                    }
                    if computed[..computed_len] != expected_buf[..checksum_len] {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "checksum verification failed for {file_path:?}: expected {:02x?}, got {:02x?}",
                                &expected_buf[..checksum_len],
                                &computed[..computed_len]
                            ),
                        ));
                    }
                    break;
                } else if token > 0 {
                    // Literal data — try zero-copy from multiplex frame buffer,
                    // falling back to TokenBuffer when data spans frame boundaries.
                    let len = token as usize;

                    if let Some(data) = reader.try_borrow_exact(len)? {
                        if let Some(ref mut sparse) = sparse_state {
                            sparse.write(&mut output, data)?;
                        } else {
                            output.write_all(data)?;
                        }
                        checksum_verifier.update(data);
                    } else {
                        token_buffer.resize_for(len);
                        reader.read_exact(token_buffer.as_mut_slice())?;
                        let data = token_buffer.as_slice();
                        if let Some(ref mut sparse) = sparse_state {
                            sparse.write(&mut output, data)?;
                        } else {
                            output.write_all(data)?;
                        }
                        checksum_verifier.update(data);
                    }
                    total_bytes += len as u64;
                } else {
                    // Negative: block reference = -(token+1)
                    // For new files (no basis), this shouldn't happen
                    let block_idx = -(token + 1) as usize;
                    if let (Some(sig), Some(basis_map)) = (&signature_opt, basis_map.as_mut()) {
                        // We have a basis file - copy the block using cached MapFile
                        // Mirrors upstream receiver.c receive_data() block copy logic
                        let layout = sig.layout();
                        let block_count = layout.block_count() as usize;

                        // Validate block index bounds (upstream receiver.c doesn't send invalid indices)
                        if block_idx >= block_count {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "block index {block_idx} out of bounds (file has {block_count} blocks)"
                                ),
                            ));
                        }

                        let block_len = layout.block_length().get() as u64;
                        let offset = block_idx as u64 * block_len;

                        // Calculate actual bytes to copy for this block
                        // Last block may be shorter (remainder)
                        let bytes_to_copy = if block_idx == block_count.saturating_sub(1) {
                            // Last block uses remainder size
                            let remainder = layout.remainder();
                            if remainder > 0 {
                                remainder as usize
                            } else {
                                block_len as usize
                            }
                        } else {
                            block_len as usize
                        };

                        // Use cached MapFile with 256KB sliding window
                        // This avoids ~23,000 open/seek/read syscalls for a typical 16MB file
                        let block_data = basis_map.map_ptr(offset, bytes_to_copy)?;

                        // Use sparse writing if enabled
                        if let Some(ref mut sparse) = sparse_state {
                            sparse.write(&mut output, block_data)?;
                        } else {
                            output.write_all(block_data)?;
                        }

                        // Update checksum with copied data
                        checksum_verifier.update(block_data);

                        total_bytes += bytes_to_copy as u64;
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("block reference {block_idx} without basis file"),
                        ));
                    }
                }
            }

            // Finalize sparse writing if active
            // This ensures trailing zeros are handled (extends file to correct size)
            if let Some(ref mut sparse) = sparse_state {
                let final_pos = sparse.finish(&mut output)?;
                // Validate file size matches expected size from sender
                let expected_size = file_entry.size();
                if final_pos != expected_size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "sparse file size mismatch for {file_path:?}: \
                             expected {expected_size} bytes, got {final_pos} bytes"
                        ),
                    ));
                }
            }

            // Step 5b: Fsync if requested (optional durability guarantee)
            // Upstream rsync only fsyncs when --fsync flag is explicitly set (do_fsync=0 default).
            // The atomic rename still provides crash consistency - data is flushed when
            // the kernel closes the file or needs the buffers.
            // Flush BufWriter and get inner file for sync_all
            let file = output.into_inner().map_err(|e| {
                io::Error::other(format!(
                    "failed to flush output buffer for {file_path:?}: {e}"
                ))
            })?;
            if self.config.fsync {
                file.sync_all().map_err(|e| {
                    io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}"))
                })?;
            }
            drop(file); // Ensure file is closed before rename

            // Atomic rename (crash-safe)
            fs::rename(temp_guard.path(), &file_path)?;
            temp_guard.keep(); // Success! Keep the file (now renamed)

            // Step 6: Apply metadata from FileEntry (best-effort)
            if let Err(meta_err) =
                apply_metadata_from_file_entry(&file_path, file_entry, &metadata_opts)
            {
                // Collect error for final report - metadata failure shouldn't abort transfer
                metadata_errors.push((file_path.clone(), meta_err.to_string()));
            }

            // Step 7: Track stats
            bytes_received += total_bytes;
            files_transferred += 1;
        }

        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0,
            total_source_bytes,
            metadata_errors,
            io_error: 0,
            error_count: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            redo_count: 0,
        })
    }

    /// Runs the pipelined receiver transfer loop.
    ///
    /// This method implements request pipelining to reduce latency overhead.
    /// Instead of waiting for each file's response before requesting the next,
    /// it sends multiple requests ahead and processes responses as they arrive.
    ///
    /// # Performance Impact
    ///
    /// With 92,437 files and 0.5ms network latency:
    /// - Synchronous: 46+ seconds latency overhead
    /// - Pipelined (window=64): ~0.7 seconds latency overhead
    ///
    /// # Arguments
    ///
    /// * `reader` - Input stream from sender
    /// * `writer` - Output stream to sender
    /// * `pipeline_config` - Configuration for pipeline window size
    ///
    /// # Protocol Compatibility
    ///
    /// The pipelined receiver is fully compatible with upstream rsync daemons.
    /// The protocol requires in-order response processing which is preserved.
    pub fn run_pipelined<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        // Batch directory creation
        let mut metadata_errors = self.create_directories(&setup.dest_dir, &setup.metadata_opts)?;

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            ..Default::default()
        };
        let files_to_transfer = self.build_files_to_transfer(
            &setup.dest_dir,
            &setup.metadata_opts,
            None,
            &mut metadata_errors,
            &mut stats,
        );

        // Run pipelined transfer with decoupled network/disk I/O (phase 1)
        let total_files = files_to_transfer.len();
        let redo_config = pipeline_config.clone();
        let mut no_progress: Option<&mut dyn super::TransferProgressCallback> = None;
        let (mut files_transferred, mut bytes_received, redo_indices) = self
            .run_pipeline_loop_decoupled(
                reader,
                writer,
                pipeline_config,
                &setup,
                files_to_transfer,
                &mut metadata_errors,
                false,
                total_files,
                &mut no_progress,
            )?;

        // Phase 2: redo pass for files that failed checksum verification.
        // upstream: receiver.c:580-587 — phase transition, then re-receive redo'd files
        // upstream: generator.c:2160-2199 — generator re-sends with SUM_LENGTH, no basis
        let redo_count = redo_indices.len();
        if !redo_indices.is_empty() {
            let redo_files: Vec<(usize, &FileEntry)> = redo_indices
                .iter()
                .filter_map(|&idx| self.file_list.get(idx).map(|entry| (idx, entry)))
                .collect();

            let (redo_transferred, redo_bytes, _) = self.run_pipeline_loop_decoupled(
                reader,
                writer,
                redo_config,
                &setup,
                redo_files,
                &mut metadata_errors,
                true,
                total_files,
                &mut no_progress,
            )?;

            files_transferred += redo_transferred;
            bytes_received += redo_bytes;
        }

        // Print verbose directories
        for file_entry in &self.file_list {
            if file_entry.is_dir() && self.config.flags.verbose && self.config.client_mode {
                let relative_path = file_entry.path();
                if relative_path.as_os_str() == "." {
                    info_log!(Name, 1, "./");
                } else {
                    info_log!(Name, 1, "{}/", relative_path.display());
                }
            }
        }

        // Finalize handshake
        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.total_source_bytes = total_source_bytes;
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;

        Ok(stats)
    }

    /// Runs the receiver with incremental directory creation and failed-dir tracking.
    ///
    /// Unlike [`Self::run_pipelined`], this method creates directories incrementally
    /// as they appear in the file list, tracking failures and skipping children
    /// of failed directories.
    ///
    /// # Benefits
    ///
    /// - Failed directory tracking: Skip children of directories that fail to create
    /// - Better error recovery: Continue with unaffected subtrees
    /// - Detailed statistics: Track directories created, failed, and files skipped
    ///
    /// # Note
    ///
    /// File list is received completely before transfer begins (protocol
    /// requirement - rsync uses same connection for list and transfer data).
    pub fn run_pipelined_incremental<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        mut progress: Option<&mut dyn super::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        // Incremental directory creation with failure tracking
        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            ..Default::default()
        };
        let mut failed_dirs = FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                if self.create_directory_incremental(
                    &setup.dest_dir,
                    file_entry,
                    &setup.metadata_opts,
                    &mut failed_dirs,
                )? {
                    stats.directories_created += 1;
                } else {
                    stats.directories_failed += 1;
                }
            }
        }

        let files_to_transfer = self.build_files_to_transfer(
            &setup.dest_dir,
            &setup.metadata_opts,
            Some(&failed_dirs),
            &mut metadata_errors,
            &mut stats,
        );

        // Run pipelined transfer with decoupled network/disk I/O (phase 1)
        let total_files = files_to_transfer.len();
        let redo_config = pipeline_config.clone();
        let (mut files_transferred, mut bytes_received, redo_indices) = self
            .run_pipeline_loop_decoupled(
                reader,
                writer,
                pipeline_config,
                &setup,
                files_to_transfer,
                &mut metadata_errors,
                false,
                total_files,
                &mut progress,
            )?;

        // Phase 2: redo pass for files that failed checksum verification.
        let redo_count = redo_indices.len();
        if !redo_indices.is_empty() {
            let redo_files: Vec<(usize, &FileEntry)> = redo_indices
                .iter()
                .filter_map(|&idx| self.file_list.get(idx).map(|entry| (idx, entry)))
                .collect();

            let (redo_transferred, redo_bytes, _) = self.run_pipeline_loop_decoupled(
                reader,
                writer,
                redo_config,
                &setup,
                redo_files,
                &mut metadata_errors,
                true,
                total_files,
                &mut progress,
            )?;

            files_transferred += redo_transferred;
            bytes_received += redo_bytes;
        }

        // Finalize
        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;

        self.finalize_transfer(reader, writer)?;

        Ok(stats)
    }

    /// Common setup for both pipelined transfer modes.
    ///
    /// Activates multiplex, reads filter list, prints verbose header,
    /// receives the file list, and builds shared configuration.
    /// Returns the (possibly activated) reader, file count, and setup values.
    fn setup_transfer<R: Read>(
        &mut self,
        reader: super::reader::ServerReader<R>,
    ) -> io::Result<(super::reader::ServerReader<R>, usize, PipelineSetup)> {
        let mut reader = if self.protocol.uses_binary_negotiation() {
            reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?
        } else {
            reader
        };

        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        if self.config.flags.verbose && self.config.client_mode {
            info_log!(Flist, 1, "receiving incremental file list");
        }

        let file_count = self.receive_file_list(&mut reader)?;

        // Receive incremental file list segments (INC_RECURSE).
        // The sender sends all sub-lists immediately after the initial file list,
        // before entering the transfer loop. Entries are appended to self.file_list.
        let extra_count = self.receive_extra_file_lists(&mut reader)?;
        let file_count = file_count + extra_count;

        // Validate received file list for path safety (--trust-sender enforcement).
        // Removes entries with absolute paths, `..` components, or unsafe symlink
        // targets. This runs after sorting so the file list indices are stable.
        let removed = self.sanitize_file_list();
        let file_count = file_count - removed;

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = DEFAULT_CHECKSUM_LENGTH;

        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_atimes(self.config.flags.atimes)
            .preserve_crtimes(self.config.flags.crtimes)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        Ok((
            reader,
            file_count,
            PipelineSetup {
                dest_dir,
                metadata_opts,
                checksum_length,
                checksum_algorithm,
            },
        ))
    }

    /// Pipelined transfer loop with decoupled network/disk I/O.
    ///
    /// Streams delta data through a bounded channel to a dedicated disk commit
    /// thread. The network thread never blocks on disk I/O, and the disk thread
    /// never blocks on network reads — achieving overlap similar to upstream
    /// rsync's `fork()` model.
    /// Returns (files_transferred, bytes, redo_indices).
    ///
    /// The `redo_indices` vector contains file list indices for files whose
    /// whole-file checksum verification failed during this pass. The caller
    /// should retransmit these files in a redo pass with empty basis.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:554-984` — `recv_files()` main loop
    /// - `receiver.c:970-974` — `send_msg_int(MSG_REDO, ndx)` on checksum failure
    #[allow(clippy::too_many_arguments)]
    fn run_pipeline_loop_decoupled<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        setup: &PipelineSetup,
        files_to_transfer: Vec<(usize, &FileEntry)>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        is_redo_pass: bool,
        total_files: usize,
        progress: &mut Option<&mut dyn super::TransferProgressCallback>,
    ) -> io::Result<(usize, u64, Vec<usize>)> {
        use crate::disk_commit::DiskCommitConfig;
        use crate::pipeline::receiver::PipelinedReceiver;
        use crate::shared::TransferDeadline;

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.supports_iflags(),
            checksum_length: setup.checksum_length,
            checksum_algorithm: setup.checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.fsync,

            write_devices: self.config.write_devices,
            inplace: self.config.inplace,
            io_uring_policy: self.config.io_uring_policy,
        };

        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        // (file_list_index, dest_path, file_entry) — index needed for redo tracking
        let mut pending_files_info: VecDeque<(usize, PathBuf, &FileEntry)> =
            VecDeque::with_capacity(pipeline.window_size());
        let mut files_transferred = 0usize;
        let mut bytes_received = 0u64;

        // Reusable per-file resources
        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        // Spawn the disk commit thread with metadata options so it applies
        // mtime/perms/ownership immediately after rename — mirroring upstream
        // finish_transfer() → set_file_attrs() in receiver.c.
        let disk_config = DiskCommitConfig {
            do_fsync: self.config.fsync,
            metadata_opts: Some(setup.metadata_opts.clone()),
            ..DiskCommitConfig::default()
        };
        let mut pipelined_receiver = PipelinedReceiver::new(disk_config);
        if is_redo_pass {
            // In redo pass, checksum failures are hard errors (not re-queued).
            let _ = pipelined_receiver.take_redo_indices();
        }

        let result = (|| -> io::Result<(usize, u64, Vec<usize>)> {
            loop {
                // Check deadline at file boundary before requesting more files.
                // Files already in-flight will finish; we just stop sending new requests.
                // Mirrors upstream rsync's --stop-at which finishes the current file
                // before exiting gracefully.
                if let Some(ref dl) = deadline {
                    if dl.is_reached() {
                        break;
                    }
                }

                // Fill the pipeline with requests
                while pipeline.can_send() {
                    if let Some((file_idx, file_entry)) = file_iter.next() {
                        let relative_path = file_entry.path();
                        let file_path = if relative_path.as_os_str() == "." {
                            setup.dest_dir.clone()
                        } else {
                            setup.dest_dir.join(relative_path)
                        };

                        if self.config.flags.verbose && self.config.client_mode {
                            info_log!(Name, 1, "{}", relative_path.display());
                        }

                        // In redo pass, use empty basis to force whole-file transfer.
                        // upstream: generator.c:2163 — csum_length = SUM_LENGTH for redo
                        // upstream: generator.c:2170 — size_only = -size_only (negated)
                        let (sig, basis) = if is_redo_pass {
                            (None, None)
                        } else {
                            let basis_config = self.build_basis_file_config(
                                &file_path,
                                &setup.dest_dir,
                                relative_path,
                                file_entry.size(),
                                setup.checksum_length,
                                setup.checksum_algorithm,
                            );
                            let basis_result = find_basis_file_with_config(&basis_config);
                            (basis_result.signature, basis_result.basis_path)
                        };

                        let pending = send_file_request(
                            writer,
                            &mut ndx_write_codec,
                            self.flat_to_wire_ndx(file_idx),
                            file_path.clone(),
                            sig,
                            basis,
                            file_entry.size(),
                            &request_config,
                        )?;

                        pipeline.push(pending);
                        pending_files_info.push_back((file_idx, file_path, file_entry));
                    } else {
                        break;
                    }
                }

                if pipeline.is_empty() {
                    break;
                }

                // Process one response — streams chunks to disk thread.
                let pending = pipeline.pop().expect("pipeline not empty");
                let (file_idx, file_path, file_entry) =
                    pending_files_info.pop_front().expect("pipeline not empty");

                let response_ctx = ResponseContext {
                    config: &request_config,
                };

                let result = process_file_response_streaming(
                    reader,
                    &mut ndx_read_codec,
                    pending,
                    &response_ctx,
                    &mut checksum_verifier,
                    pipelined_receiver.file_sender(),
                    pipelined_receiver.buf_return_rx(),
                    0,
                    Some(file_entry.clone()),
                )?;

                pipelined_receiver.note_commit_sent(
                    result.expected_checksum,
                    result.checksum_len,
                    file_path.clone(),
                    file_idx,
                );

                // Non-blocking: collect any ready disk results to detect early errors.
                let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_ready_results()?;
                bytes_received += disk_bytes;
                metadata_errors.extend(disk_meta_errors);

                bytes_received += result.total_bytes;
                files_transferred += 1;

                if let Some(cb) = progress.as_mut() {
                    let event = super::TransferProgressEvent {
                        path: file_entry.path(),
                        file_bytes: result.total_bytes,
                        total_file_bytes: Some(file_entry.size()),
                        files_done: files_transferred,
                        total_files,
                    };
                    cb.on_file_transferred(&event);
                }
            }

            // Drain all remaining disk results — blocks until every file is
            // committed (flushed + renamed + metadata applied).
            let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_all_results()?;
            bytes_received += disk_bytes;
            metadata_errors.extend(disk_meta_errors);

            let redo_indices = pipelined_receiver.take_redo_indices();

            Ok((files_transferred, bytes_received, redo_indices))
        })();

        // Graceful shutdown regardless of success or failure.
        let _ = pipelined_receiver.shutdown();

        result
    }

    /// Exchange phase transitions, receive stats, and handle goodbye handshake.
    fn finalize_transfer<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // Only read stats in client mode: daemon sender writes stats over the wire,
        // but in server mode the client sender returns without writing stats.
        if self.config.client_mode {
            let _sender_stats = self.receive_stats(reader)?;
        }

        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(())
    }
}

/// Shared configuration produced by [`ReceiverContext::setup_transfer`].
struct PipelineSetup {
    dest_dir: PathBuf,
    metadata_opts: MetadataOptions,
    checksum_length: NonZeroU8,
    checksum_algorithm: engine::signature::SignatureAlgorithm,
}

/// Statistics from a receiver transfer operation.
///
/// Returned inside [`crate::ServerStats::Receiver`] after a successful receive.
/// Contains file counts, byte totals, metadata error records, and incremental-mode
/// statistics.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// Number of files in the received file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes received from the sender (file data, deltas, etc.).
    pub bytes_received: u64,
    /// Total bytes sent to the sender (signatures, file indices, etc.).
    ///
    /// This tracks data sent back during the transfer, such as signature blocks
    /// for delta generation and file index requests. Mirrors upstream rsync's
    /// `stats.total_written` tracking in io.c:859.
    pub bytes_sent: u64,
    /// Total size of all source files in the file list.
    ///
    /// This is the sum of all file sizes from the received file list,
    /// used to calculate speedup ratio (total_size / bytes_transferred).
    pub total_source_bytes: u64,
    /// Metadata errors encountered (path, error message).
    pub metadata_errors: Vec<(PathBuf, String)>,
    /// Accumulated I/O error flags from the sender's file list trailer.
    ///
    /// This bitfield uses the constants from [`super::io_error_flags`] and is
    /// propagated to the client summary so the exit code reflects any I/O
    /// errors that occurred during the transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2518`: `write_int(f, ignore_errors ? 0 : io_error);`
    pub io_error: i32,
    /// Number of `MSG_ERROR` messages received from the remote sender.
    ///
    /// When the sender encounters per-file errors it sends `MSG_ERROR` frames
    /// that the receiver tallies here. A non-zero count causes the exit code
    /// to report a partial transfer (`RERR_PARTIAL`, exit 23).
    pub error_count: u32,

    // Incremental mode statistics
    /// Total entries received from wire (incremental mode).
    pub entries_received: u64,
    /// Directories successfully created (incremental mode).
    pub directories_created: u64,
    /// Directories that failed to create (incremental mode).
    pub directories_failed: u64,
    /// Files skipped due to failed parent directory (incremental mode).
    pub files_skipped: u64,

    /// Number of files that were retransmitted due to checksum verification failure.
    ///
    /// Mirrors upstream rsync's redo mechanism where files that fail whole-file
    /// checksum after delta application are re-requested with an empty basis
    /// (whole-file transfer) in phase 2.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:970-974` — `send_msg_int(MSG_REDO, ndx)` queues for redo
    /// - `generator.c:2160-2199` — generator processes redo queue in phase 2
    pub redo_count: usize,
}

/// Statistics received from the remote sender after transfer completion.
///
/// The sender transmits these statistics over the wire after the transfer
/// loop finishes but before the goodbye handshake. The receiver uses them
/// to compute the speedup ratio displayed in `--stats` output.
#[derive(Debug, Clone, Default)]
pub struct SenderStats {
    /// Total bytes read by the sender during transfer.
    pub total_read: u64,
    /// Total bytes written by the sender during transfer.
    pub total_written: u64,
    /// Total size of all source files.
    pub total_size: u64,
    /// File list build time in milliseconds (protocol 29+).
    pub flist_buildtime_ms: Option<u64>,
    /// File list transfer time in milliseconds (protocol 29+).
    pub flist_xfertime_ms: Option<u64>,
}

/// Signature header for delta transfer.
///
/// Represents the `sum_head` structure from upstream rsync's rsync.h.
/// Contains metadata about the signature blocks that follow.
///
/// # Wire Format
///
/// All fields are transmitted as 32-bit little-endian integers:
/// - `count`: Number of signature blocks
/// - `blength`: Block length in bytes
/// - `s2length`: Strong sum (checksum) length in bytes
/// - `remainder`: Size of the final partial block (0 if file is block-aligned)
///
/// # Upstream Reference
///
/// - `rsync.h:200` - `struct sum_struct` definition
/// - `match.c:380` - `write_sum_head()` sends the header
/// - `sender.c:120` - `read_sum_head()` receives the header
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SumHead {
    /// Number of signature blocks.
    pub count: u32,
    /// Block length in bytes.
    pub blength: u32,
    /// Strong sum (checksum) length in bytes.
    pub s2length: u32,
    /// Size of the final partial block (0 if block-aligned).
    pub remainder: u32,
}

impl SumHead {
    /// Creates a new `SumHead` with the specified parameters.
    #[must_use]
    pub const fn new(count: u32, blength: u32, s2length: u32, remainder: u32) -> Self {
        Self {
            count,
            blength,
            s2length,
            remainder,
        }
    }

    /// Creates an empty `SumHead` (no basis file, requests whole-file transfer).
    ///
    /// When count=0, the sender knows to transmit the entire file as literal data.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            count: 0,
            blength: 0,
            s2length: 0,
            remainder: 0,
        }
    }

    /// Creates a `SumHead` from a file signature.
    #[must_use]
    pub const fn from_signature(signature: &FileSignature) -> Self {
        let layout = signature.layout();
        Self {
            count: layout.block_count() as u32,
            blength: layout.block_length().get(),
            s2length: layout.strong_sum_length().get() as u32,
            remainder: layout.remainder(),
        }
    }

    /// Returns true if this represents a whole-file transfer (no basis).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Writes the sum_head to the wire in rsync format.
    ///
    /// All four fields are written as 32-bit little-endian integers.
    pub fn write<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&(self.count as i32).to_le_bytes())?;
        writer.write_all(&(self.blength as i32).to_le_bytes())?;
        writer.write_all(&(self.s2length as i32).to_le_bytes())?;
        writer.write_all(&(self.remainder as i32).to_le_bytes())?;
        Ok(())
    }

    /// Reads a sum_head from the wire in rsync format.
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;
        Ok(Self {
            count: i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as u32,
            blength: i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as u32,
            s2length: i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]) as u32,
            remainder: i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as u32,
        })
    }
}

/// Attributes echoed back by the sender after receiving a file request.
///
/// After the receiver sends NDX + iflags + sum_head, the sender echoes back
/// its own NDX + iflags, potentially with additional fields based on flags.
///
/// # Upstream Reference
///
/// - `sender.c:180` - `write_ndx_and_attrs()` sends these
/// - `rsync.c:383` - `read_ndx_and_attrs()` reads them
#[derive(Debug, Clone, Default)]
pub struct SenderAttrs {
    /// Item flags indicating transfer mode.
    pub iflags: u16,
    /// Optional basis file type (if `ITEM_BASIS_TYPE_FOLLOWS` set).
    ///
    /// When present, indicates which basis file the generator selected for
    /// the delta transfer. See `FnameCmpType` for the possible values.
    pub fnamecmp_type: Option<protocol::FnameCmpType>,
    /// Optional alternate basis name (if `ITEM_XNAME_FOLLOWS` set).
    pub xname: Option<Vec<u8>>,
}

impl SenderAttrs {
    /// Item flag indicating file data will be transferred.
    pub const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
    /// Item flag indicating basis type follows.
    pub const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11; // 0x0800
    /// Item flag indicating alternate name follows.
    pub const ITEM_XNAME_FOLLOWS: u16 = 1 << 12; // 0x1000

    /// Reads sender attributes from the wire using an NDX codec.
    ///
    /// The sender echoes back NDX + iflags after receiving a file request.
    /// Protocol 30+ uses variable-length delta-encoded NDX values, which
    /// require the codec to maintain state across calls.
    ///
    /// # Arguments
    ///
    /// * `reader` - The input stream to read from
    /// * `ndx_codec` - The NDX codec for protocol-aware decoding (must match sender's state)
    ///
    /// # Protocol Details
    ///
    /// - Protocol >= 30: Uses delta-encoded NDX (1-5 bytes depending on value)
    /// - Protocol < 30: Uses 4-byte little-endian NDX
    /// - Protocol >= 29: Reads 2-byte iflags after NDX
    /// - Protocol < 29: Uses default ITEM_TRANSFER flags
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:2289-2318` - `read_ndx()` for NDX decoding
    /// - `rsync.c:383` - `read_ndx_and_attrs()` reads NDX + iflags
    pub fn read_with_codec<R: Read>(
        reader: &mut R,
        ndx_codec: &mut impl NdxCodec,
    ) -> io::Result<(i32, Self)> {
        // Read NDX using protocol-aware codec (handles delta encoding for protocol 30+)
        let ndx = ndx_codec.read_ndx(reader)?;

        let protocol_version = ndx_codec.protocol_version();

        // For protocol >= 29, read iflags (shortint = 2 bytes LE)
        let iflags = if protocol_version >= 29 {
            let mut iflags_buf = [0u8; 2];
            reader.read_exact(&mut iflags_buf)?;
            u16::from_le_bytes(iflags_buf)
        } else {
            Self::ITEM_TRANSFER // Default for older protocols
        };

        // Read optional fields based on iflags
        let fnamecmp_type = if iflags & Self::ITEM_BASIS_TYPE_FOLLOWS != 0 {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(protocol::FnameCmpType::from_wire(byte[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid fnamecmp type: 0x{:02X}", byte[0]),
                )
            })?)
        } else {
            None
        };

        let xname = if iflags & Self::ITEM_XNAME_FOLLOWS != 0 {
            // Read vstring: upstream io.c:1944-1960 read_vstring()
            // Format: first byte is length; if bit 7 set, length = (byte & 0x7F) * 256 + next_byte
            let mut len_byte = [0u8; 1];
            reader.read_exact(&mut len_byte)?;
            let xname_len = if len_byte[0] & 0x80 != 0 {
                let mut second_byte = [0u8; 1];
                reader.read_exact(&mut second_byte)?;
                ((len_byte[0] & 0x7F) as usize) * 256 + second_byte[0] as usize
            } else {
                len_byte[0] as usize
            };
            // Upstream MAXPATHLEN is typically 4096; reject excessively long names
            const MAX_XNAME_LEN: usize = 4096;
            if xname_len > MAX_XNAME_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("xname length {xname_len} exceeds maximum {MAX_XNAME_LEN}"),
                ));
            }
            if xname_len > 0 {
                let mut xname_buf = vec![0u8; xname_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        Ok((
            ndx,
            Self {
                iflags,
                fnamecmp_type,
                xname,
            },
        ))
    }

    /// Reads sender attributes from the wire (legacy method for tests).
    ///
    /// **Deprecated**: Use [`read_with_codec`] for proper protocol 30+ support.
    /// This method only reads a single byte for NDX, which is incorrect for
    /// protocol 30+ that uses variable-length delta encoding.
    ///
    /// # Arguments
    ///
    /// * `reader` - The input stream to read from
    /// * `protocol_version` - The negotiated protocol version
    #[cfg(test)]
    pub fn read<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Self> {
        // Legacy implementation: read single byte for NDX (only valid for tests
        // with protocol < 30 or first NDX where delta=1 fits in one byte)
        let mut ndx_byte = [0u8; 1];
        let n = reader.read(&mut ndx_byte)?;
        if n != 1 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "failed to read NDX byte from sender",
            ));
        }

        // For protocol >= 29, read iflags (shortint = 2 bytes LE)
        let iflags = if protocol_version >= 29 {
            let mut iflags_buf = [0u8; 2];
            reader.read_exact(&mut iflags_buf)?;
            u16::from_le_bytes(iflags_buf)
        } else {
            Self::ITEM_TRANSFER // Default for older protocols
        };

        // Read optional fields based on iflags
        let fnamecmp_type = if iflags & Self::ITEM_BASIS_TYPE_FOLLOWS != 0 {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(protocol::FnameCmpType::from_wire(byte[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid fnamecmp type: 0x{:02X}", byte[0]),
                )
            })?)
        } else {
            None
        };

        let xname = if iflags & Self::ITEM_XNAME_FOLLOWS != 0 {
            let mut len_byte = [0u8; 1];
            reader.read_exact(&mut len_byte)?;
            let xname_len = if len_byte[0] & 0x80 != 0 {
                let mut second_byte = [0u8; 1];
                reader.read_exact(&mut second_byte)?;
                ((len_byte[0] & 0x7F) as usize) * 256 + second_byte[0] as usize
            } else {
                len_byte[0] as usize
            };
            const MAX_XNAME_LEN: usize = 4096;
            if xname_len > MAX_XNAME_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("xname length {xname_len} exceeds maximum {MAX_XNAME_LEN}"),
                ));
            }
            if xname_len > 0 {
                let mut xname_buf = vec![0u8; xname_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            iflags,
            fnamecmp_type,
            xname,
        })
    }
}

/// Result of searching for a basis file via [`find_basis_file_with_config`].
///
/// Contains both the generated signature and the path to the basis file
/// that was used. When no basis is found, both fields are `None`; use
/// [`is_empty`](Self::is_empty) to check.
#[derive(Debug)]
pub struct BasisFileResult {
    /// The generated signature (None if no basis found).
    pub signature: Option<FileSignature>,
    /// Path to the basis file used (None if no basis found).
    pub basis_path: Option<PathBuf>,
}

impl BasisFileResult {
    /// Empty result when no basis file is found.
    const EMPTY: Self = Self {
        signature: None,
        basis_path: None,
    };

    /// Returns true if no basis file was found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.signature.is_none()
    }
}

/// Configuration for basis file search and signature generation.
///
/// Passed to [`find_basis_file_with_config`] to control where to look for
/// a basis file (exact match, reference directories, fuzzy match) and how
/// to generate its signature (protocol version, checksum algorithm, length).
#[derive(Debug)]
pub struct BasisFileConfig<'a> {
    /// Target file path in destination.
    pub file_path: &'a std::path::Path,
    /// Destination directory base.
    pub dest_dir: &'a std::path::Path,
    /// Relative path from destination root.
    pub relative_path: &'a std::path::Path,
    /// Expected size of the target file.
    pub target_size: u64,
    /// Whether to try fuzzy matching.
    pub fuzzy_enabled: bool,
    /// List of reference directories to check.
    pub reference_directories: &'a [ReferenceDirectory],
    /// Protocol version for signature generation.
    pub protocol: ProtocolVersion,
    /// Checksum truncation length.
    pub checksum_length: NonZeroU8,
    /// Algorithm for strong checksums.
    pub checksum_algorithm: engine::signature::SignatureAlgorithm,
    /// When true, skip basis file search entirely (upstream `--whole-file`).
    pub whole_file: bool,
}

/// Returns `true` if any component of the path is `..`.
///
/// This mirrors upstream rsync's `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
/// check that rejects paths containing parent-directory references, preventing
/// directory traversal attacks from a malicious sender.
///
/// # Upstream Reference
///
/// - `util1.c`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
fn path_contains_dot_dot(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// Determines whether a symlink target is safe within the transfer tree.
///
/// A symlink target is considered unsafe if:
/// - It is an absolute path
/// - It is empty
/// - It would escape the transfer directory via `..` traversal
///
/// The safety check evaluates whether the symlink, when resolved relative
/// to its location in the transfer tree, would point outside the tree root.
///
/// # Upstream Reference
///
/// - `util1.c`: `unsafe_symlink()` — returns 1 if unsafe, 0 if safe
fn symlink_target_is_safe_for_transfer(target: &Path, link_path: &Path) -> bool {
    // Reject empty targets and absolute symlinks.
    // upstream: util1.c `if (!dest || !*dest || *dest == '/') return 1;`
    if target.as_os_str().is_empty() || target.has_root() {
        return false;
    }

    // Count the directory depth of the link within the transfer tree.
    // The last component is the symlink name itself, not a directory level.
    let mut depth: i64 = 0;
    for component in link_path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => depth = 0,
            _ => {}
        }
    }
    // Exclude the symlink filename from the depth budget.
    depth = (depth - 1).max(0);

    // Walk the target components, tracking whether `..` escapes the tree.
    for component in target.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}

/// Tries to find a basis file in the reference directories.
///
/// Iterates through reference directories in order, checking if the relative
/// path exists in each one. Returns the first match found.
///
/// # Upstream Reference
///
/// - `generator.c:1400` - Reference directory basis file lookup
fn try_reference_directories(
    relative_path: &std::path::Path,
    reference_directories: &[ReferenceDirectory],
) -> Option<(fs::File, u64, PathBuf)> {
    for ref_dir in reference_directories {
        let candidate = ref_dir.path.join(relative_path);
        if let Ok(file) = fs::File::open(&candidate) {
            if let Ok(meta) = file.metadata() {
                if meta.is_file() {
                    return Some((file, meta.len(), candidate));
                }
            }
        }
    }
    None
}

/// Opens a file and returns it with metadata.
///
/// Returns the file handle, size, and path if successful.
fn try_open_file(path: &std::path::Path) -> Option<(fs::File, u64, PathBuf)> {
    let file = fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    Some((file, size, path.to_path_buf()))
}

/// Attempts fuzzy matching to find a similar basis file.
///
/// # Upstream Reference
///
/// - `generator.c:1580` - Fuzzy matching via `find_fuzzy_basis()`
fn try_fuzzy_match(
    relative_path: &std::path::Path,
    dest_dir: &std::path::Path,
    target_size: u64,
) -> Option<(fs::File, u64, PathBuf)> {
    let target_name = relative_path.file_name()?;
    let fuzzy_matcher = FuzzyMatcher::new();
    let fuzzy_match = fuzzy_matcher.find_fuzzy_basis(target_name, dest_dir, target_size)?;
    try_open_file(&fuzzy_match.path)
}

/// Configuration for generating a signature from a basis file.
///
/// This parameter object encapsulates the signature-related configuration
/// needed to generate a file signature, reducing parameter count and improving
/// maintainability.
#[derive(Debug, Clone, Copy)]
struct SignatureGenerationConfig {
    /// Protocol version for signature layout calculation.
    protocol: ProtocolVersion,
    /// Checksum truncation length.
    checksum_length: NonZeroU8,
    /// Algorithm for strong checksums.
    checksum_algorithm: engine::signature::SignatureAlgorithm,
}

impl SignatureGenerationConfig {
    /// Extracts signature generation config from a BasisFileConfig.
    fn from_basis_config(config: &BasisFileConfig<'_>) -> Self {
        Self {
            protocol: config.protocol,
            checksum_length: config.checksum_length,
            checksum_algorithm: config.checksum_algorithm,
        }
    }
}

/// Generates a signature for the given basis file.
///
/// # Arguments
///
/// * `basis_file` - The file to generate a signature for
/// * `basis_size` - The size of the basis file in bytes
/// * `basis_path` - The path to the basis file
/// * `config` - Signature generation configuration
///
/// # Returns
///
/// A `BasisFileResult` containing the signature and path, or empty if generation fails.
fn generate_basis_signature(
    basis_file: fs::File,
    basis_size: u64,
    basis_path: PathBuf,
    config: SignatureGenerationConfig,
) -> BasisFileResult {
    let params =
        SignatureLayoutParams::new(basis_size, None, config.protocol, config.checksum_length);

    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return BasisFileResult::EMPTY,
    };

    match generate_file_signature(basis_file, layout, config.checksum_algorithm) {
        Ok(sig) => BasisFileResult {
            signature: Some(sig),
            basis_path: Some(basis_path),
        },
        Err(_) => BasisFileResult::EMPTY,
    }
}

/// Finds a basis file for delta transfer using the provided configuration.
///
/// Search order:
/// 1. Exact file at destination path
/// 2. Reference directories (in order provided)
/// 3. Fuzzy matching in destination directory (if enabled)
///
/// # Upstream Reference
///
/// - `generator.c:1450` - Basis file selection in `recv_generator()`
/// - `generator.c:1580` - Fuzzy matching via `find_fuzzy_basis()`
/// - `generator.c:1400` - Reference directory checking
pub fn find_basis_file_with_config(config: &BasisFileConfig<'_>) -> BasisFileResult {
    // Upstream `generator.c:1949`: when `whole_file` is set, no basis file
    // is used — the entire file is sent as literals.
    if config.whole_file {
        return BasisFileResult::EMPTY;
    }

    // Try sources in priority order: exact match → reference dirs → fuzzy
    let basis = try_open_file(config.file_path)
        .or_else(|| try_reference_directories(config.relative_path, config.reference_directories))
        .or_else(|| {
            if config.fuzzy_enabled {
                try_fuzzy_match(config.relative_path, config.dest_dir, config.target_size)
            } else {
                None
            }
        });

    let Some((file, size, path)) = basis else {
        return BasisFileResult::EMPTY;
    };

    let sig_config = SignatureGenerationConfig::from_basis_config(config);
    generate_basis_signature(file, size, path, sig_config)
}

/// Writes signature blocks to the wire.
///
/// After writing sum_head, this sends each block's rolling sum and strong sum.
///
/// # Upstream Reference
///
/// - `match.c:395` - Signature block transmission
pub fn write_signature_blocks<W: Write + ?Sized>(
    writer: &mut W,
    signature: &FileSignature,
    s2length: u32,
) -> io::Result<()> {
    let mut sum_buf = vec![0u8; s2length as usize];
    for block in signature.blocks() {
        // Write rolling_sum as int32 LE
        writer.write_all(&(block.rolling().value() as i32).to_le_bytes())?;

        // Write strong_sum, truncated or padded to s2length
        let strong_bytes = block.strong();
        sum_buf.fill(0);
        let copy_len = std::cmp::min(strong_bytes.len(), s2length as usize);
        sum_buf[..copy_len].copy_from_slice(&strong_bytes[..copy_len]);
        writer.write_all(&sum_buf)?;
    }
    Ok(())
}

// Helper functions for delta transfer (test-only)

/// Applies a delta script to create a new file (whole-file transfer, no basis).
///
/// All tokens must be Literal; Copy operations indicate a protocol error.
#[cfg(test)]
fn apply_whole_file_delta(path: &std::path::Path, script: &DeltaScript) -> io::Result<()> {
    let mut output = fs::File::create(path)?;

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(data) => {
                output.write_all(data)?;
            }
            DeltaToken::Copy { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Copy operation in whole-file transfer (no basis exists)",
                ));
            }
        }
    }

    output.sync_all()?;
    Ok(())
}

/// Converts wire protocol delta operations to engine delta script.
#[cfg(test)]
fn wire_delta_to_script(ops: Vec<DeltaOp>) -> DeltaScript {
    let mut tokens = Vec::with_capacity(ops.len());
    let mut total_bytes = 0u64;
    let mut literal_bytes = 0u64;

    for op in ops {
        match op {
            DeltaOp::Literal(data) => {
                let len = data.len() as u64;
                total_bytes += len;
                literal_bytes += len;
                tokens.push(DeltaToken::Literal(data));
            }
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                total_bytes += length as u64;
                tokens.push(DeltaToken::Copy {
                    index: block_index as u64,
                    len: length as usize,
                });
            }
        }
    }

    DeltaScript::new(tokens, total_bytes, literal_bytes)
}

#[cfg(test)]
mod tests {
    use super::super::error::{
        DeltaFatalError, DeltaRecoverableError, DeltaTransferError, categorize_io_error,
    };
    use super::super::flags::ParsedServerFlags;
    use super::super::role::ServerRole;
    use super::*;
    use protocol::ChecksumAlgorithm;
    use std::ffi::OsString;
    use std::io::Cursor;

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags::default(),
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
            ignore_errors: false,
            fsync: false,

            io_uring_policy: fast_io::IoUringPolicy::Auto,
            checksum_seed: None,
            is_daemon_connection: false,
            checksum_choice: None,
            write_devices: false,
            trust_sender: false,
            stop_at: None,
            qsort: false,
            min_file_size: None,
            max_file_size: None,
            files_from_path: None,
            from0: false,
            inplace: false,
        }
    }

    fn test_handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,           // Test mode doesn't need client args
            io_timeout: None,            // Test mode doesn't configure I/O timeouts
            negotiated_algorithms: None, // Test mode uses defaults
            compat_flags: None,          // Test mode uses defaults
            checksum_seed: 0,            // Test mode uses dummy seed
        }
    }

    #[test]
    fn receiver_context_creation() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        assert_eq!(ctx.protocol().as_u8(), 32);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn receiver_empty_file_list() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Empty file list (just the end marker)
        let data = [0u8];
        let mut cursor = Cursor::new(&data[..]);

        let count = ctx.receive_file_list(&mut cursor).unwrap();
        assert_eq!(count, 0);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn receiver_single_file() {
        use protocol::flist::{FileEntry, FileListWriter};

        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Create a proper file list using FileListWriter for protocol 32
        let mut data = Vec::new();
        let mut writer = FileListWriter::new(handshake.protocol);

        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.write_entry(&mut data, &entry).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let mut cursor = Cursor::new(&data[..]);
        let count = ctx.receive_file_list(&mut cursor).unwrap();

        assert_eq!(count, 1);
        assert_eq!(ctx.file_list().len(), 1);
        assert_eq!(ctx.file_list()[0].name(), "test.txt");
    }

    #[test]
    fn wire_delta_to_script_converts_literals() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![
            DeltaOp::Literal(vec![1, 2, 3, 4]),
            DeltaOp::Literal(vec![5, 6, 7, 8]),
        ];

        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 2);
        assert_eq!(script.total_bytes(), 8);
        assert_eq!(script.literal_bytes(), 8);

        match &script.tokens()[0] {
            DeltaToken::Literal(data) => assert_eq!(data, &vec![1, 2, 3, 4]),
            _ => panic!("expected literal token"),
        }
    }

    #[test]
    fn wire_delta_to_script_converts_copy_operations() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(vec![9, 10]),
            DeltaOp::Copy {
                block_index: 1,
                length: 512,
            },
        ];

        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 3);
        assert_eq!(script.total_bytes(), 1024 + 2 + 512);
        assert_eq!(script.literal_bytes(), 2);

        match &script.tokens()[0] {
            DeltaToken::Copy { index, len } => {
                assert_eq!(*index, 0);
                assert_eq!(*len, 1024);
            }
            _ => panic!("expected copy token"),
        }
    }

    #[test]
    fn apply_whole_file_delta_accepts_only_literals() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Create a delta script with only literals
        let tokens = vec![
            DeltaToken::Literal(b"Hello, ".to_vec()),
            DeltaToken::Literal(b"world!".to_vec()),
        ];
        let script = DeltaScript::new(tokens, 13, 13);

        apply_whole_file_delta(&output_path, &script).unwrap();

        let result = std::fs::read(&output_path).unwrap();
        assert_eq!(result, b"Hello, world!");
    }

    #[test]
    fn apply_whole_file_delta_rejects_copy_operations() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Create a delta script with a copy operation (invalid for whole-file transfer)
        let tokens = vec![
            DeltaToken::Literal(b"data".to_vec()),
            DeltaToken::Copy {
                index: 0,
                len: 1024,
            },
        ];
        let script = DeltaScript::new(tokens, 1028, 4);

        let result = apply_whole_file_delta(&output_path, &script);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn temp_file_guard_cleans_up_on_drop() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().join("test.tmp");

        // Create temp file
        std::fs::write(&temp_path, b"test data").unwrap();
        assert!(temp_path.exists());

        {
            let _guard = TempFileGuard::new(temp_path.clone());
            // Guard goes out of scope here, should delete file
        }

        // File should be deleted
        assert!(!temp_path.exists());
    }

    #[test]
    fn temp_file_guard_keeps_file_when_marked() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path().join("test.tmp");

        // Create temp file
        std::fs::write(&temp_path, b"test data").unwrap();
        assert!(temp_path.exists());

        {
            let mut guard = TempFileGuard::new(temp_path.clone());
            guard.keep(); // Mark as successful
            // Guard goes out of scope here
        }

        // File should still exist
        assert!(temp_path.exists());
    }

    #[test]
    fn error_categorization_disk_full_is_fatal() {
        use std::path::Path;

        let err = io::Error::from(io::ErrorKind::StorageFull);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "write");

        match categorized {
            DeltaTransferError::Fatal(DeltaFatalError::DiskFull { path: p, .. }) => {
                assert_eq!(p, path);
            }
            _ => panic!("Expected fatal disk full error"),
        }
    }

    #[test]
    fn error_categorization_permission_denied_is_recoverable() {
        use std::path::Path;

        let err = io::Error::from(io::ErrorKind::PermissionDenied);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "open");

        match categorized {
            DeltaTransferError::Recoverable(DeltaRecoverableError::PermissionDenied {
                path: p,
                operation: op,
            }) => {
                assert_eq!(p, path);
                assert_eq!(op, "open");
            }
            _ => panic!("Expected recoverable permission denied error"),
        }
    }

    #[test]
    fn error_categorization_not_found_is_recoverable() {
        use std::path::Path;

        let err = io::Error::from(io::ErrorKind::NotFound);
        let path = Path::new("/tmp/test.txt");

        let categorized = categorize_io_error(err, path, "open");

        match categorized {
            DeltaTransferError::Recoverable(DeltaRecoverableError::FileNotFound { path: p }) => {
                assert_eq!(p, path);
            }
            _ => panic!("Expected recoverable file not found error"),
        }
    }

    #[test]
    fn transfer_stats_tracks_metadata_errors() {
        let mut stats = TransferStats::default();

        assert_eq!(stats.metadata_errors.len(), 0);

        // Simulate collecting metadata errors
        stats.metadata_errors.push((
            PathBuf::from("/tmp/file1.txt"),
            "Permission denied".to_owned(),
        ));
        stats.metadata_errors.push((
            PathBuf::from("/tmp/file2.txt"),
            "Operation not permitted".to_owned(),
        ));

        assert_eq!(stats.metadata_errors.len(), 2);
        assert_eq!(stats.metadata_errors[0].0, PathBuf::from("/tmp/file1.txt"));
        assert_eq!(stats.metadata_errors[0].1, "Permission denied");
    }

    #[test]
    fn checksum_verifier_md4_for_legacy_protocol() {
        // Protocol < 30 defaults to MD4
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        let mut verifier = ChecksumVerifier::new(None, protocol, 0, None);

        verifier.update(b"test data");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

        // MD4 produces 16 bytes
        assert_eq!(verifier.finalize_into(&mut buf), 16);
    }

    #[test]
    fn checksum_verifier_md5_for_modern_protocol() {
        // Protocol >= 30 without negotiation defaults to MD5
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut verifier = ChecksumVerifier::new(None, protocol, 12345, None);

        verifier.update(b"test data");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

        // MD5 produces 16 bytes
        assert_eq!(verifier.finalize_into(&mut buf), 16);
    }

    #[test]
    fn checksum_verifier_xxh3_with_negotiation() {
        use protocol::CompressionAlgorithm;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let negotiated = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH3,
            compression: CompressionAlgorithm::None,
        };

        let mut verifier = ChecksumVerifier::new(Some(&negotiated), protocol, 9999, None);

        verifier.update(b"test data");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

        // XXH3 produces 8 bytes (64-bit)
        assert_eq!(verifier.finalize_into(&mut buf), 8);
    }

    #[test]
    fn checksum_verifier_sha1_with_negotiation() {
        use protocol::CompressionAlgorithm;

        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let negotiated = NegotiationResult {
            checksum: ChecksumAlgorithm::SHA1,
            compression: CompressionAlgorithm::None,
        };

        let mut verifier = ChecksumVerifier::new(Some(&negotiated), protocol, 0, None);

        verifier.update(b"test data");
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

        // SHA1 produces 20 bytes
        assert_eq!(verifier.finalize_into(&mut buf), 20);
    }

    #[test]
    fn checksum_verifier_incremental_update() {
        // Test that incremental updates produce same result as single update
        let protocol = ProtocolVersion::try_from(28u8).unwrap();

        let mut verifier1 = ChecksumVerifier::new(None, protocol, 0, None);
        verifier1.update(b"hello ");
        verifier1.update(b"world");
        let mut buf1 = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len1 = verifier1.finalize_into(&mut buf1);

        let mut verifier2 = ChecksumVerifier::new(None, protocol, 0, None);
        verifier2.update(b"hello world");
        let mut buf2 = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len2 = verifier2.finalize_into(&mut buf2);

        assert_eq!(buf1[..len1], buf2[..len2]);
    }

    #[test]
    fn sparse_write_state_writes_nonzero_data() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Write non-zero data
        let data = b"hello world";
        sparse.write(&mut output, data).unwrap();
        sparse.finish(&mut output).unwrap();

        // Should write the data directly
        assert_eq!(output.get_ref(), data);
    }

    #[test]
    fn sparse_write_state_skips_zero_runs() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Write zeros followed by data
        let zeros = [0u8; 4096];
        let data = b"test";
        sparse.write(&mut output, &zeros).unwrap();
        sparse.write(&mut output, data).unwrap();
        sparse.finish(&mut output).unwrap();

        // Output should be mostly zeros (sparse seek) followed by "test"
        // The file position should be at zeros.len() + data.len()
        let result = output.into_inner();
        assert_eq!(result.len(), 4096 + 4);
        // Last 4 bytes should be "test"
        assert_eq!(&result[4096..], b"test");
    }

    #[test]
    fn sparse_write_state_handles_trailing_zeros() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Write data followed by zeros
        let data = b"test";
        let zeros = [0u8; 1024];
        sparse.write(&mut output, data).unwrap();
        sparse.write(&mut output, &zeros).unwrap();
        sparse.finish(&mut output).unwrap();

        // File should be extended to correct size
        let result = output.into_inner();
        assert_eq!(result.len(), 4 + 1024);
        assert_eq!(&result[..4], b"test");
    }

    #[test]
    fn sparse_write_state_mixed_data_and_zeros() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Interleaved data and zeros
        sparse.write(&mut output, b"AAA").unwrap();
        sparse.write(&mut output, &[0u8; 100]).unwrap();
        sparse.write(&mut output, b"BBB").unwrap();
        sparse.finish(&mut output).unwrap();

        let result = output.into_inner();
        assert_eq!(result.len(), 3 + 100 + 3);
        assert_eq!(&result[..3], b"AAA");
        assert_eq!(&result[103..], b"BBB");
    }

    #[test]
    fn sparse_write_state_empty_write() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Empty write should be a no-op
        let n = sparse.write(&mut output, &[]).unwrap();
        assert_eq!(n, 0);

        sparse.finish(&mut output).unwrap();
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn sparse_write_state_accumulate_pending_zeros() {
        let mut sparse = SparseWriteState::default();

        sparse.accumulate(100);
        assert_eq!(sparse.pending(), 100);

        sparse.accumulate(50);
        assert_eq!(sparse.pending(), 150);
    }

    #[test]
    fn sum_head_new_creates_with_correct_values() {
        let sum_head = SumHead::new(100, 1024, 16, 512);
        assert_eq!(sum_head.count, 100);
        assert_eq!(sum_head.blength, 1024);
        assert_eq!(sum_head.s2length, 16);
        assert_eq!(sum_head.remainder, 512);
    }

    #[test]
    fn sum_head_empty_creates_zero_values() {
        let sum_head = SumHead::empty();
        assert_eq!(sum_head.count, 0);
        assert_eq!(sum_head.blength, 0);
        assert_eq!(sum_head.s2length, 0);
        assert_eq!(sum_head.remainder, 0);
        assert!(sum_head.is_empty());
    }

    #[test]
    fn sum_head_default_is_empty() {
        let sum_head = SumHead::default();
        assert!(sum_head.is_empty());
        assert_eq!(sum_head, SumHead::empty());
    }

    #[test]
    fn sum_head_is_empty_false_for_nonzero_count() {
        let sum_head = SumHead::new(1, 1024, 16, 0);
        assert!(!sum_head.is_empty());
    }

    #[test]
    fn sum_head_write_produces_correct_wire_format() {
        let sum_head = SumHead::new(10, 700, 16, 100);
        let mut output = Vec::new();
        sum_head.write(&mut output).unwrap();

        assert_eq!(output.len(), 16);
        // All values as 32-bit little-endian
        assert_eq!(
            i32::from_le_bytes([output[0], output[1], output[2], output[3]]),
            10
        );
        assert_eq!(
            i32::from_le_bytes([output[4], output[5], output[6], output[7]]),
            700
        );
        assert_eq!(
            i32::from_le_bytes([output[8], output[9], output[10], output[11]]),
            16
        );
        assert_eq!(
            i32::from_le_bytes([output[12], output[13], output[14], output[15]]),
            100
        );
    }

    #[test]
    fn sum_head_read_parses_wire_format() {
        // Prepare wire data: count=5, blength=512, s2length=16, remainder=128
        let mut data = Vec::new();
        data.extend_from_slice(&5i32.to_le_bytes());
        data.extend_from_slice(&512i32.to_le_bytes());
        data.extend_from_slice(&16i32.to_le_bytes());
        data.extend_from_slice(&128i32.to_le_bytes());

        let sum_head = SumHead::read(&mut Cursor::new(data)).unwrap();

        assert_eq!(sum_head.count, 5);
        assert_eq!(sum_head.blength, 512);
        assert_eq!(sum_head.s2length, 16);
        assert_eq!(sum_head.remainder, 128);
    }

    #[test]
    fn sum_head_round_trip() {
        let original = SumHead::new(100, 1024, 20, 256);

        let mut buf = Vec::new();
        original.write(&mut buf).unwrap();

        let decoded = SumHead::read(&mut Cursor::new(buf)).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn sum_head_read_insufficient_data() {
        // Only 8 bytes instead of 16
        let data = vec![0u8; 8];
        let result = SumHead::read(&mut Cursor::new(data));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sender_attrs_read_protocol_28_returns_default_iflags() {
        // Protocol 28 just reads the NDX byte, no iflags
        let data = vec![0x05u8]; // NDX byte only
        let attrs = SenderAttrs::read(&mut Cursor::new(data), 28).unwrap();

        assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
        assert!(attrs.fnamecmp_type.is_none());
        assert!(attrs.xname.is_none());
    }

    #[test]
    fn sender_attrs_read_protocol_29_parses_iflags() {
        // NDX byte + iflags (0x8000 = ITEM_TRANSFER)
        let mut data = vec![0x05u8]; // NDX byte
        data.extend_from_slice(&0x8000u16.to_le_bytes()); // iflags

        let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

        assert_eq!(attrs.iflags, 0x8000);
        assert!(attrs.fnamecmp_type.is_none());
        assert!(attrs.xname.is_none());
    }

    #[test]
    fn sender_attrs_read_with_basis_type() {
        // NDX byte + iflags (0x8800 = ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS) + fnamecmp_type
        let mut data = vec![0x05u8]; // NDX byte
        data.extend_from_slice(&0x8800u16.to_le_bytes()); // iflags with BASIS_TYPE_FOLLOWS
        data.push(0x02); // fnamecmp_type = BasisDir(2)

        let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

        assert_eq!(attrs.iflags, 0x8800);
        assert_eq!(
            attrs.fnamecmp_type,
            Some(protocol::FnameCmpType::BasisDir(2))
        );
        assert!(attrs.xname.is_none());
    }

    #[test]
    fn sender_attrs_read_with_short_xname() {
        // NDX byte + iflags (0x9000 = ITEM_TRANSFER | ITEM_XNAME_FOLLOWS) + xname
        let mut data = vec![0x05u8]; // NDX byte
        data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
        data.push(0x04); // xname length (short form)
        data.extend_from_slice(b"test"); // xname content

        let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

        assert_eq!(attrs.iflags, 0x9000);
        assert!(attrs.fnamecmp_type.is_none());
        assert_eq!(attrs.xname, Some(b"test".to_vec()));
    }

    #[test]
    fn sender_attrs_read_with_long_xname() {
        // NDX + iflags + xname with extended length (> 127 bytes requires 2-byte length)
        let mut data = vec![0x05u8]; // NDX byte
        data.extend_from_slice(&0x9000u16.to_le_bytes()); // iflags with XNAME_FOLLOWS
        // Length 300 = 0x80 | (300 / 256) = 0x81, then 300 % 256 = 44
        data.push(0x81); // High byte: 0x80 flag + 1
        data.push(0x2C); // Low byte: 44 (1*256 + 44 = 300)
        data.extend(vec![b'x'; 300]); // xname content (300 'x' characters)

        let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

        assert_eq!(attrs.iflags, 0x9000);
        assert!(attrs.fnamecmp_type.is_none());
        assert_eq!(attrs.xname.as_ref().unwrap().len(), 300);
    }

    #[test]
    fn sender_attrs_read_empty_returns_eof_error() {
        let data: Vec<u8> = vec![];
        let result = SenderAttrs::read(&mut Cursor::new(data), 29);

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sender_attrs_constants_match_upstream() {
        // Verify our constants match upstream rsync.h values
        assert_eq!(SenderAttrs::ITEM_TRANSFER, 0x8000);
        assert_eq!(SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
        assert_eq!(SenderAttrs::ITEM_XNAME_FOLLOWS, 0x1000);
    }

    #[test]
    fn sender_attrs_read_with_codec_protocol_30_delta_encoded() {
        use protocol::codec::{NdxCodec, create_ndx_codec};

        // Simulate sender encoding NDX 0 for protocol 30+
        // With prev_positive=-1, ndx=0, diff=1, encoded as single byte 0x01
        let mut sender_codec = create_ndx_codec(31);
        let mut wire_data = Vec::new();
        sender_codec.write_ndx(&mut wire_data, 0).unwrap();
        // Add iflags (ITEM_TRANSFER = 0x8000)
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

        // Receiver reads with its own codec
        let mut receiver_codec = create_ndx_codec(31);
        let mut cursor = Cursor::new(&wire_data);
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

        assert_eq!(ndx, 0);
        assert_eq!(attrs.iflags, 0x8000);
    }

    #[test]
    fn sender_attrs_read_with_codec_protocol_30_sequential_indices() {
        use protocol::codec::{NdxCodec, create_ndx_codec};

        // Simulate sender sending sequential indices 0, 1, 2
        let mut sender_codec = create_ndx_codec(31);
        let mut wire_data = Vec::new();
        for ndx in 0..3 {
            sender_codec.write_ndx(&mut wire_data, ndx).unwrap();
            wire_data.extend_from_slice(&0x8000u16.to_le_bytes());
        }

        // Receiver reads all three
        let mut receiver_codec = create_ndx_codec(31);
        let mut cursor = Cursor::new(&wire_data);

        for expected_ndx in 0..3 {
            let (ndx, attrs) =
                SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();
            assert_eq!(ndx, expected_ndx, "expected NDX {expected_ndx}");
            assert_eq!(attrs.iflags, 0x8000);
        }
    }

    #[test]
    fn sender_attrs_read_with_codec_legacy_protocol_29() {
        use protocol::codec::{NdxCodec, create_ndx_codec};

        // Protocol 29 uses 4-byte LE NDX
        let mut sender_codec = create_ndx_codec(29);
        let mut wire_data = Vec::new();
        sender_codec.write_ndx(&mut wire_data, 42).unwrap();
        // Add iflags
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

        let mut receiver_codec = create_ndx_codec(29);
        let mut cursor = Cursor::new(&wire_data);
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

        assert_eq!(ndx, 42);
        assert_eq!(attrs.iflags, 0x8000);
    }

    #[test]
    fn sender_attrs_read_with_codec_protocol_28_no_iflags() {
        use protocol::codec::{NdxCodec, create_ndx_codec};

        // Protocol 28: 4-byte LE NDX, no iflags
        let mut sender_codec = create_ndx_codec(28);
        let mut wire_data = Vec::new();
        sender_codec.write_ndx(&mut wire_data, 5).unwrap();
        // No iflags for protocol < 29

        let mut receiver_codec = create_ndx_codec(28);
        let mut cursor = Cursor::new(&wire_data);
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

        assert_eq!(ndx, 5);
        // Default iflags for protocol < 29
        assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
    }

    #[test]
    fn sender_attrs_read_with_codec_large_index() {
        use protocol::codec::{NdxCodec, create_ndx_codec};

        // Test with a large index that requires extended encoding in protocol 30+
        let large_index = 50000;

        let mut sender_codec = create_ndx_codec(31);
        let mut wire_data = Vec::new();
        sender_codec.write_ndx(&mut wire_data, large_index).unwrap();
        wire_data.extend_from_slice(&0x8000u16.to_le_bytes());

        let mut receiver_codec = create_ndx_codec(31);
        let mut cursor = Cursor::new(&wire_data);
        let (ndx, attrs) = SenderAttrs::read_with_codec(&mut cursor, &mut receiver_codec).unwrap();

        assert_eq!(ndx, large_index);
        assert_eq!(attrs.iflags, 0x8000);
    }

    #[test]
    fn basis_file_result_is_empty_when_no_signature() {
        let result = BasisFileResult {
            signature: None,
            basis_path: None,
        };
        assert!(result.is_empty());
    }

    #[test]
    fn basis_file_result_is_not_empty_when_has_signature() {
        use engine::delta::SignatureLayout;
        use engine::signature::FileSignature;
        use std::num::NonZeroU32;

        // Create a minimal signature
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(512).unwrap(),
            0,
            0,
            DEFAULT_CHECKSUM_LENGTH,
        );
        let signature = FileSignature::from_raw_parts(layout, vec![], 0);

        let result = BasisFileResult {
            signature: Some(signature),
            basis_path: Some(PathBuf::from("/tmp/basis")),
        };
        assert!(!result.is_empty());
    }

    #[test]
    fn try_reference_directories_finds_file_in_first_directory() {
        use super::ReferenceDirectory;
        use crate::config::ReferenceDirectoryKind;
        use tempfile::tempdir;

        // Create two reference directories
        let ref_dir1 = tempdir().unwrap();
        let ref_dir2 = tempdir().unwrap();

        // Create a file in the first reference directory
        let test_file = ref_dir1.path().join("subdir/test.txt");
        fs::create_dir_all(test_file.parent().unwrap()).unwrap();
        fs::write(&test_file, b"test content from ref1").unwrap();

        let ref_dirs = vec![
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Compare,
                path: ref_dir1.path().to_path_buf(),
            },
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Link,
                path: ref_dir2.path().to_path_buf(),
            },
        ];

        let relative_path = std::path::Path::new("subdir/test.txt");
        let result = super::try_reference_directories(relative_path, &ref_dirs);

        assert!(result.is_some());
        let (_, size, path) = result.unwrap();
        assert_eq!(size, 22); // "test content from ref1".len()
        assert_eq!(path, test_file);
    }

    #[test]
    fn try_reference_directories_finds_file_in_second_directory() {
        use super::ReferenceDirectory;
        use crate::config::ReferenceDirectoryKind;
        use tempfile::tempdir;

        // Create two reference directories
        let ref_dir1 = tempdir().unwrap();
        let ref_dir2 = tempdir().unwrap();

        // Create a file only in the second reference directory
        let test_file = ref_dir2.path().join("test.txt");
        fs::write(&test_file, b"test content from ref2").unwrap();

        let ref_dirs = vec![
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Compare,
                path: ref_dir1.path().to_path_buf(),
            },
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Copy,
                path: ref_dir2.path().to_path_buf(),
            },
        ];

        let relative_path = std::path::Path::new("test.txt");
        let result = super::try_reference_directories(relative_path, &ref_dirs);

        assert!(result.is_some());
        let (_, size, path) = result.unwrap();
        assert_eq!(size, 22); // "test content from ref2".len()
        assert_eq!(path, test_file);
    }

    #[test]
    fn try_reference_directories_returns_none_when_not_found() {
        use super::ReferenceDirectory;
        use crate::config::ReferenceDirectoryKind;
        use tempfile::tempdir;

        let ref_dir = tempdir().unwrap();

        let ref_dirs = vec![ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref_dir.path().to_path_buf(),
        }];

        let relative_path = std::path::Path::new("nonexistent.txt");
        let result = super::try_reference_directories(relative_path, &ref_dirs);

        assert!(result.is_none());
    }

    #[test]
    fn try_reference_directories_empty_list_returns_none() {
        let ref_dirs: Vec<super::ReferenceDirectory> = vec![];
        let relative_path = std::path::Path::new("test.txt");
        let result = super::try_reference_directories(relative_path, &ref_dirs);

        assert!(result.is_none());
    }

    /// Creates test config with specific flags for ID list tests.
    fn config_with_flags(owner: bool, group: bool, numeric_ids: bool) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags {
                owner,
                group,
                numeric_ids,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
            ignore_errors: false,
            fsync: false,

            io_uring_policy: fast_io::IoUringPolicy::Auto,
            checksum_seed: None,
            is_daemon_connection: false,
            checksum_choice: None,
            write_devices: false,
            trust_sender: false,
            stop_at: None,
            qsort: false,
            min_file_size: None,
            max_file_size: None,
            files_from_path: None,
            from0: false,
            inplace: false,
        }
    }

    #[test]
    fn receive_id_lists_skips_when_numeric_ids_true() {
        let handshake = test_handshake();
        let config = config_with_flags(true, true, true);
        let mut ctx = ReceiverContext::new(&handshake, config);

        // With numeric_ids=true, no data should be read even with owner/group set
        let data: &[u8] = &[];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        // Cursor position unchanged - nothing read
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn receive_id_lists_reads_uid_list_when_owner_set() {
        let handshake = test_handshake();
        let config = config_with_flags(true, false, false);
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Empty UID list: varint 0 terminator only
        let data: &[u8] = &[0];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 1);
    }

    #[test]
    fn receive_id_lists_reads_gid_list_when_group_set() {
        let handshake = test_handshake();
        let config = config_with_flags(false, true, false);
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Empty GID list: varint 0 terminator only
        let data: &[u8] = &[0];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 1);
    }

    #[test]
    fn receive_id_lists_reads_both_when_owner_and_group_set() {
        let handshake = test_handshake();
        let config = config_with_flags(true, true, false);
        let mut ctx = ReceiverContext::new(&handshake, config);

        // Both lists: two varint 0 terminators
        let data: &[u8] = &[0, 0];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 2);
    }

    #[test]
    fn receive_id_lists_skips_both_when_neither_flag_set() {
        let handshake = test_handshake();
        let config = config_with_flags(false, false, false);
        let mut ctx = ReceiverContext::new(&handshake, config);

        let data: &[u8] = &[];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn apply_whole_file_delta_handles_empty_literals() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Empty delta script (no tokens)
        let script = DeltaScript::new(vec![], 0, 0);

        apply_whole_file_delta(&output_path, &script).unwrap();

        let result = std::fs::read(&output_path).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn apply_whole_file_delta_handles_large_literal() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Large literal (64KB)
        let large_data: Vec<u8> = (0..65536).map(|i| (i % 256) as u8).collect();
        let tokens = vec![DeltaToken::Literal(large_data.clone())];
        let script = DeltaScript::new(tokens, 65536, 65536);

        apply_whole_file_delta(&output_path, &script).unwrap();

        let result = std::fs::read(&output_path).unwrap();
        assert_eq!(result, large_data);
    }

    #[test]
    fn apply_whole_file_delta_concatenates_multiple_literals() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("output.txt");

        // Multiple small literals
        let tokens = vec![
            DeltaToken::Literal(b"part1_".to_vec()),
            DeltaToken::Literal(b"part2_".to_vec()),
            DeltaToken::Literal(b"part3".to_vec()),
        ];
        let script = DeltaScript::new(tokens, 17, 17);

        apply_whole_file_delta(&output_path, &script).unwrap();

        let result = std::fs::read(&output_path).unwrap();
        assert_eq!(result, b"part1_part2_part3");
    }

    #[test]
    fn wire_delta_to_script_handles_empty_input() {
        let wire_ops: Vec<protocol::wire::DeltaOp> = vec![];
        let script = wire_delta_to_script(wire_ops);

        assert!(script.is_empty());
        assert_eq!(script.total_bytes(), 0);
        assert_eq!(script.literal_bytes(), 0);
    }

    #[test]
    fn wire_delta_to_script_handles_zero_length_literal() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![DeltaOp::Literal(vec![])];
        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 1);
        assert_eq!(script.total_bytes(), 0);
    }

    #[test]
    fn wire_delta_to_script_handles_zero_length_copy() {
        use protocol::wire::DeltaOp;

        let wire_ops = vec![DeltaOp::Copy {
            block_index: 0,
            length: 0,
        }];
        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 1);
        assert_eq!(script.total_bytes(), 0);
        assert_eq!(script.copy_bytes(), 0);
    }

    #[test]
    fn wire_delta_to_script_mixed_operations() {
        use protocol::wire::DeltaOp;

        // Simulate typical rsync delta: copy unchanged block, insert literal, copy another block
        let wire_ops = vec![
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(vec![0xAB; 128]),
            DeltaOp::Copy {
                block_index: 2,
                length: 512,
            },
            DeltaOp::Literal(vec![0xCD; 64]),
        ];

        let script = wire_delta_to_script(wire_ops);

        assert_eq!(script.tokens().len(), 4);
        assert_eq!(script.total_bytes(), 1024 + 128 + 512 + 64);
        assert_eq!(script.literal_bytes(), 128 + 64);
        assert_eq!(script.copy_bytes(), 1024 + 512);
    }

    #[test]
    fn checksum_verifier_empty_data_produces_valid_digest() {
        let protocol = ProtocolVersion::try_from(28u8).unwrap();
        let verifier = ChecksumVerifier::new(None, protocol, 0, None);

        // No updates, just finalize
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];

        // MD4 produces 16 bytes even for empty input
        assert_eq!(verifier.finalize_into(&mut buf), 16);
    }

    #[test]
    fn checksum_verifier_digest_len_returns_correct_size() {
        use protocol::CompressionAlgorithm;

        // MD4 (protocol < 30)
        let protocol28 = ProtocolVersion::try_from(28u8).unwrap();
        let verifier28 = ChecksumVerifier::new(None, protocol28, 0, None);
        assert_eq!(verifier28.digest_len(), 16);

        // MD5 (protocol >= 30)
        let protocol32 = ProtocolVersion::try_from(32u8).unwrap();
        let verifier32 = ChecksumVerifier::new(None, protocol32, 0, None);
        assert_eq!(verifier32.digest_len(), 16);

        // XXH3 (negotiated)
        let negotiated = NegotiationResult {
            checksum: ChecksumAlgorithm::XXH3,
            compression: CompressionAlgorithm::None,
        };
        let verifier_xxh3 = ChecksumVerifier::new(Some(&negotiated), protocol32, 0, None);
        assert_eq!(verifier_xxh3.digest_len(), 8);

        // SHA1 (negotiated)
        let negotiated_sha1 = NegotiationResult {
            checksum: ChecksumAlgorithm::SHA1,
            compression: CompressionAlgorithm::None,
        };
        let verifier_sha1 = ChecksumVerifier::new(Some(&negotiated_sha1), protocol32, 0, None);
        assert_eq!(verifier_sha1.digest_len(), 20);
    }

    #[test]
    fn sparse_write_state_multiple_zero_runs_accumulate() {
        let mut sparse = SparseWriteState::default();

        // Accumulate multiple zero runs
        sparse.accumulate(100);
        sparse.accumulate(200);
        sparse.accumulate(300);

        assert_eq!(sparse.pending(), 600);
    }

    #[test]
    fn sparse_write_state_write_flushes_pending_zeros() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Accumulate zeros then write data
        sparse.accumulate(1024);
        sparse.write(&mut output, b"data").unwrap();
        sparse.finish(&mut output).unwrap();

        let result = output.into_inner();
        // File should be 1024 zeros + "data"
        assert_eq!(result.len(), 1028);
        assert_eq!(&result[1024..], b"data");
    }

    #[test]
    fn sparse_write_state_finish_handles_only_zeros() {
        let mut output = Cursor::new(Vec::new());
        let mut sparse = SparseWriteState::default();

        // Only zeros, no data
        sparse.accumulate(4096);
        sparse.finish(&mut output).unwrap();

        let result = output.into_inner();
        // File should extend to 4096 bytes of zeros
        assert_eq!(result.len(), 4096);
        assert!(result.iter().all(|&b| b == 0));
    }

    #[test]
    fn incremental_receiver_reads_entries() {
        // Create test data with a simple file list
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        // Add a directory and a file
        let dir = FileEntry::new_directory("testdir".into(), 0o755);
        let file = FileEntry::new_file("testdir/file.txt".into(), 100, 0o644);

        writer.write_entry(&mut data, &dir).unwrap();
        writer.write_entry(&mut data, &file).unwrap();
        writer.write_end(&mut data, None).unwrap();

        // Create handshake and config
        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        // Create incremental receiver
        let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

        // First entry should be the directory (it has no parent dependency)
        let entry1 = receiver.next_ready().unwrap().unwrap();
        assert!(entry1.is_dir());
        assert_eq!(entry1.name(), "testdir");

        // Second entry should be the file (parent dir now exists)
        let entry2 = receiver.next_ready().unwrap().unwrap();
        assert!(entry2.is_file());
        assert_eq!(entry2.name(), "testdir/file.txt");

        // No more entries
        assert!(receiver.next_ready().unwrap().is_none());
        assert!(receiver.is_empty());
        assert_eq!(receiver.entries_read(), 2);
    }

    #[test]
    fn incremental_receiver_handles_empty_list() {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let writer = protocol::flist::FileListWriter::new(protocol);
        writer.write_end(&mut data, None).unwrap();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

        assert!(receiver.next_ready().unwrap().is_none());
        assert!(receiver.is_empty());
        assert_eq!(receiver.entries_read(), 0);
    }

    #[test]
    fn incremental_receiver_collect_sorted() {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        // Add entries in random order
        let file1 = FileEntry::new_file("z_file.txt".into(), 50, 0o644);
        let file2 = FileEntry::new_file("a_file.txt".into(), 100, 0o644);
        let dir = FileEntry::new_directory("m_dir".into(), 0o755);

        writer.write_entry(&mut data, &file1).unwrap();
        writer.write_entry(&mut data, &file2).unwrap();
        writer.write_entry(&mut data, &dir).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

        // collect_sorted should return entries in sorted order
        let entries = receiver.collect_sorted().unwrap();
        assert_eq!(entries.len(), 3);

        // Files should come before directories at the same level
        assert_eq!(entries[0].name(), "a_file.txt");
        assert_eq!(entries[1].name(), "z_file.txt");
        assert_eq!(entries[2].name(), "m_dir");
    }

    #[test]
    fn incremental_receiver_iterator_interface() {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        let file = FileEntry::new_file("test.txt".into(), 100, 0o644);
        writer.write_entry(&mut data, &file).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

        // Use iterator interface
        let entries: Vec<_> = receiver.collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), "test.txt");
    }

    #[test]
    fn incremental_receiver_mark_directory_created() {
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut data = Vec::new();
        let mut writer = protocol::flist::FileListWriter::new(protocol);

        // Add only a nested file (no directory entry)
        let file = FileEntry::new_file("existing/nested.txt".into(), 100, 0o644);
        writer.write_entry(&mut data, &file).unwrap();
        writer.write_end(&mut data, None).unwrap();

        let handshake = test_handshake();
        let config = test_config();
        let ctx = ReceiverContext::new(&handshake, config);

        let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(&data[..]));

        // Mark the parent directory as already created
        receiver.mark_directory_created("existing");

        // Now the nested file should be immediately ready
        let entry = receiver.next_ready().unwrap().unwrap();
        assert_eq!(entry.name(), "existing/nested.txt");
    }

    #[test]
    fn transfer_stats_has_incremental_fields() {
        let stats = TransferStats {
            files_listed: 0,
            files_transferred: 0,
            bytes_received: 0,
            bytes_sent: 0,
            total_source_bytes: 0,
            metadata_errors: vec![],
            io_error: 0,
            error_count: 0,
            entries_received: 100,
            directories_created: 10,
            directories_failed: 2,
            files_skipped: 5,
            redo_count: 0,
        };

        assert_eq!(stats.entries_received, 100);
        assert_eq!(stats.directories_created, 10);
        assert_eq!(stats.directories_failed, 2);
        assert_eq!(stats.files_skipped, 5);
    }

    mod incremental_receiver_tests {
        use super::*;

        /// Helper: create wire-encoded file list data from entries.
        fn encode_entries(entries: &[FileEntry]) -> Vec<u8> {
            let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
            let mut data = Vec::new();
            let mut writer = protocol::flist::FileListWriter::new(protocol);

            for entry in entries {
                writer.write_entry(&mut data, entry).unwrap();
            }
            writer.write_end(&mut data, None).unwrap();

            data
        }

        /// Helper: create an `IncrementalFileListReceiver` from raw wire data.
        fn make_receiver(
            data: Vec<u8>,
        ) -> super::super::IncrementalFileListReceiver<Cursor<Vec<u8>>> {
            let handshake = test_handshake();
            let config = test_config();
            let ctx = ReceiverContext::new(&handshake, config);
            ctx.incremental_file_list_receiver(Cursor::new(data))
        }

        #[test]
        fn try_read_one_returns_false_when_finished() {
            // Create a receiver that's already marked as finished
            let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
            let flist_reader = protocol::flist::FileListReader::new(protocol);

            // Empty data - will hit EOF immediately
            let empty_data: Vec<u8> = vec![0]; // Single zero byte = end of list marker
            let source = Cursor::new(empty_data);

            let incremental = protocol::flist::IncrementalFileList::new();

            let mut receiver = super::super::IncrementalFileListReceiver {
                flist_reader,
                source,
                incremental,
                finished_reading: true, // Already finished
                entries_read: 0,
                use_qsort: false,
            };

            // Should return false since already finished
            assert!(!receiver.try_read_one().unwrap());
        }

        #[test]
        fn try_read_one_on_empty_list_returns_false() {
            // An empty file list (only the end-of-list marker) should
            // cause try_read_one to hit EOF and return false.
            let data = encode_entries(&[]);
            let mut receiver = make_receiver(data);

            assert!(!receiver.try_read_one().unwrap());
            assert!(receiver.is_finished_reading());
            assert_eq!(receiver.entries_read(), 0);
        }

        #[test]
        fn try_read_one_reads_single_entry() {
            let file = FileEntry::new_file("hello.txt".into(), 42, 0o644);
            let data = encode_entries(&[file]);
            let mut receiver = make_receiver(data);

            // First call reads one entry
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 1);
            assert_eq!(receiver.ready_count(), 1);
            assert!(!receiver.is_finished_reading());

            // The entry should be available via pop / next_ready
            let entry = receiver.next_ready().unwrap().unwrap();
            assert_eq!(entry.name(), "hello.txt");
            assert_eq!(entry.size(), 42);
        }

        #[test]
        fn try_read_one_reads_entries_one_at_a_time() {
            let entries = vec![
                FileEntry::new_file("a.txt".into(), 10, 0o644),
                FileEntry::new_file("b.txt".into(), 20, 0o644),
                FileEntry::new_file("c.txt".into(), 30, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read one at a time
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 1);
            assert_eq!(receiver.ready_count(), 1);

            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 2);
            assert_eq!(receiver.ready_count(), 2);

            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 3);
            assert_eq!(receiver.ready_count(), 3);

            // Next call hits end-of-list
            assert!(!receiver.try_read_one().unwrap());
            assert!(receiver.is_finished_reading());

            // All three entries should be ready
            let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
                .map(|e| e.name().to_string())
                .collect();
            assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
        }

        #[test]
        fn try_read_one_after_eof_is_idempotent() {
            let data = encode_entries(&[FileEntry::new_file("only.txt".into(), 1, 0o644)]);
            let mut receiver = make_receiver(data);

            // Read the single entry
            assert!(receiver.try_read_one().unwrap());
            // Hit EOF
            assert!(!receiver.try_read_one().unwrap());
            // Subsequent calls continue to return false
            assert!(!receiver.try_read_one().unwrap());
            assert!(!receiver.try_read_one().unwrap());
            assert!(receiver.is_finished_reading());
        }

        #[test]
        fn try_read_one_child_before_parent_stays_pending() {
            // Child file arrives before its parent directory.
            // try_read_one should add it to pending, not ready.
            let entries = vec![
                FileEntry::new_file("subdir/child.txt".into(), 100, 0o644),
                FileEntry::new_directory("subdir".into(), 0o755),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read child first - goes to pending since "subdir" doesn't exist
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 1);
            assert_eq!(receiver.ready_count(), 0);
            assert_eq!(receiver.pending_count(), 1);

            // Read parent directory - should release child too
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 2);
            assert_eq!(receiver.ready_count(), 2); // dir + file
            assert_eq!(receiver.pending_count(), 0);
        }

        #[test]
        fn try_read_one_with_pre_marked_directory() {
            // Mark a directory as created before reading. A child entry
            // should become immediately ready.
            let entries = vec![FileEntry::new_file("existing/file.txt".into(), 50, 0o644)];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            receiver.mark_directory_created("existing");

            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 1);
            assert_eq!(receiver.pending_count(), 0);

            let entry = receiver.next_ready().unwrap().unwrap();
            assert_eq!(entry.name(), "existing/file.txt");
        }

        #[test]
        fn try_read_one_deeply_nested_out_of_order() {
            // Push entries in reverse depth order, then verify resolution.
            let entries = vec![
                FileEntry::new_file("a/b/c/deep.txt".into(), 1, 0o644),
                FileEntry::new_directory("a/b/c".into(), 0o755),
                FileEntry::new_directory("a/b".into(), 0o755),
                FileEntry::new_directory("a".into(), 0o755),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read deep file - pending (no ancestors)
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 0);
            assert_eq!(receiver.pending_count(), 1);

            // Read "a/b/c" - pending (parent "a/b" missing)
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 0);
            assert_eq!(receiver.pending_count(), 2);

            // Read "a/b" - pending (parent "a" missing)
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 0);
            assert_eq!(receiver.pending_count(), 3);

            // Read "a" - cascading release: a -> a/b -> a/b/c -> deep.txt
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 4);
            assert_eq!(receiver.pending_count(), 0);
        }

        #[test]
        fn try_read_one_interleaved_with_next_ready() {
            let entries = vec![
                FileEntry::new_file("first.txt".into(), 1, 0o644),
                FileEntry::new_file("second.txt".into(), 2, 0o644),
                FileEntry::new_file("third.txt".into(), 3, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read one, consume it, read next
            assert!(receiver.try_read_one().unwrap());
            let e1 = receiver.next_ready().unwrap().unwrap();
            assert_eq!(e1.name(), "first.txt");
            assert_eq!(receiver.ready_count(), 0);

            assert!(receiver.try_read_one().unwrap());
            let e2 = receiver.next_ready().unwrap().unwrap();
            assert_eq!(e2.name(), "second.txt");

            assert!(receiver.try_read_one().unwrap());
            let e3 = receiver.next_ready().unwrap().unwrap();
            assert_eq!(e3.name(), "third.txt");

            // No more
            assert!(!receiver.try_read_one().unwrap());
            assert!(receiver.next_ready().unwrap().is_none());
        }

        #[test]
        fn try_read_one_interleaved_with_drain_ready() {
            let entries = vec![
                FileEntry::new_file("x.txt".into(), 1, 0o644),
                FileEntry::new_file("y.txt".into(), 2, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read both entries
            assert!(receiver.try_read_one().unwrap());
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 2);

            // Drain all at once
            let drained = receiver.drain_ready();
            assert_eq!(drained.len(), 2);
            assert_eq!(drained[0].name(), "x.txt");
            assert_eq!(drained[1].name(), "y.txt");
            assert_eq!(receiver.ready_count(), 0);

            // EOF
            assert!(!receiver.try_read_one().unwrap());
        }

        #[test]
        fn try_read_one_directory_and_children() {
            let entries = vec![
                FileEntry::new_directory("mydir".into(), 0o755),
                FileEntry::new_file("mydir/alpha.txt".into(), 10, 0o644),
                FileEntry::new_file("mydir/beta.txt".into(), 20, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read directory
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 1);

            // Read children - they should be immediately ready since parent exists
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 2);

            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.ready_count(), 3);

            // Verify order
            let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
                .map(|e| e.name().to_string())
                .collect();
            assert_eq!(names, vec!["mydir", "mydir/alpha.txt", "mydir/beta.txt"]);
        }

        #[test]
        fn try_read_one_is_empty_tracks_state_correctly() {
            let entries = vec![FileEntry::new_file("f.txt".into(), 1, 0o644)];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Not empty initially (haven't read yet, not finished)
            assert!(!receiver.is_finished_reading());

            // Read the entry
            assert!(receiver.try_read_one().unwrap());
            // Not empty: still has a ready entry
            assert!(!receiver.is_empty());

            // Hit EOF
            assert!(!receiver.try_read_one().unwrap());
            // Still not empty: one ready entry remains
            assert!(!receiver.is_empty());

            // Consume the entry
            receiver.next_ready().unwrap();
            // Now truly empty
            assert!(receiver.is_empty());
        }

        #[test]
        fn try_read_one_reads_symlink_entry() {
            let handshake = test_handshake();
            let mut config = test_config();
            config.flags.links = true;
            let ctx = ReceiverContext::new(&handshake, config);

            // Encode a symlink entry with links preserved
            let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
            let mut data = Vec::new();
            let mut writer = protocol::flist::FileListWriter::new(protocol);
            writer = writer.with_preserve_links(true);

            let symlink = FileEntry::new_symlink("link.txt".into(), "/target".into());
            writer.write_entry(&mut data, &symlink).unwrap();
            writer.write_end(&mut data, None).unwrap();

            let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(data));

            assert!(receiver.try_read_one().unwrap());
            let entry = receiver.next_ready().unwrap().unwrap();
            assert!(entry.is_symlink());
            assert_eq!(entry.name(), "link.txt");
        }

        #[test]
        fn try_read_one_increments_entries_read() {
            let entries = vec![
                FileEntry::new_file("one.txt".into(), 1, 0o644),
                FileEntry::new_file("two.txt".into(), 2, 0o644),
                FileEntry::new_file("three.txt".into(), 3, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            assert_eq!(receiver.entries_read(), 0);

            receiver.try_read_one().unwrap();
            assert_eq!(receiver.entries_read(), 1);

            receiver.try_read_one().unwrap();
            assert_eq!(receiver.entries_read(), 2);

            receiver.try_read_one().unwrap();
            assert_eq!(receiver.entries_read(), 3);

            // EOF does not increment
            receiver.try_read_one().unwrap();
            assert_eq!(receiver.entries_read(), 3);
        }

        #[test]
        fn try_read_one_partial_then_collect_sorted() {
            let entries = vec![
                FileEntry::new_file("z.txt".into(), 1, 0o644),
                FileEntry::new_file("a.txt".into(), 2, 0o644),
                FileEntry::new_file("m.txt".into(), 3, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read one entry via try_read_one
            assert!(receiver.try_read_one().unwrap());
            // Consume it so it doesn't appear in collect_sorted's drain
            let first = receiver.next_ready().unwrap().unwrap();
            assert_eq!(first.name(), "z.txt");

            // Now collect the remaining entries sorted
            let sorted = receiver.collect_sorted().unwrap();
            assert_eq!(sorted.len(), 2);
            // "a.txt" should come before "m.txt" after sorting
            assert_eq!(sorted[0].name(), "a.txt");
            assert_eq!(sorted[1].name(), "m.txt");
        }

        #[test]
        fn mark_finished_prevents_further_reads() {
            let entries = vec![
                FileEntry::new_file("a.txt".into(), 1, 0o644),
                FileEntry::new_file("b.txt".into(), 2, 0o644),
            ];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            // Read one entry
            assert!(receiver.try_read_one().unwrap());
            assert_eq!(receiver.entries_read(), 1);

            // Mark as finished (simulating error recovery)
            receiver.mark_finished();

            // try_read_one should now return false even though data remains
            assert!(!receiver.try_read_one().unwrap());
            assert!(receiver.is_finished_reading());
            assert_eq!(receiver.entries_read(), 1);
        }

        #[test]
        fn try_read_one_stats_are_accessible() {
            let entries = vec![FileEntry::new_file("stat_test.txt".into(), 999, 0o644)];
            let data = encode_entries(&entries);
            let mut receiver = make_receiver(data);

            assert!(receiver.try_read_one().unwrap());
            // Stats should reflect one regular file read
            let stats = receiver.stats();
            assert_eq!(stats.num_files, 1);
            assert_eq!(stats.total_size, 999);
        }
    }

    #[test]
    fn run_pipelined_incremental_compiles() {
        // This test just verifies the method signature is correct
        // Full integration tests will be in Task 8
        fn _check_signature<R: Read, W: Write + ?Sized>(
            ctx: &mut ReceiverContext,
            reader: super::super::reader::ServerReader<R>,
            writer: &mut W,
        ) {
            let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default(), None);
        }
    }

    mod create_directory_incremental_tests {
        use super::*;
        use tempfile::TempDir;

        #[test]
        fn creates_directory_successfully() {
            let temp = TempDir::new().unwrap();
            let dest = temp.path();

            let entry = FileEntry::new_directory("subdir".into(), 0o755);
            let opts = MetadataOptions::default();
            let mut failed = super::super::FailedDirectories::new();

            let handshake = test_handshake();
            let config = test_config();
            let ctx = ReceiverContext::new(&handshake, config);

            let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed);

            assert!(result.is_ok());
            assert!(result.unwrap()); // Returns true for success
            assert!(dest.join("subdir").exists());
            assert_eq!(failed.count(), 0);
        }

        #[test]
        fn skips_child_of_failed_parent() {
            let temp = TempDir::new().unwrap();
            let dest = temp.path();

            let entry = FileEntry::new_directory("failed_parent/child".into(), 0o755);
            let opts = MetadataOptions::default();
            let mut failed = super::super::FailedDirectories::new();
            failed.mark_failed("failed_parent");

            let handshake = test_handshake();
            let config = test_config();
            let ctx = ReceiverContext::new(&handshake, config);

            let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed);

            assert!(result.is_ok());
            assert!(!result.unwrap()); // Returns false for skipped
            assert!(!dest.join("failed_parent/child").exists());
            assert_eq!(failed.count(), 2); // Parent + child marked as failed
        }
    }

    mod failed_directories_tests {
        use super::super::FailedDirectories;

        #[test]
        fn failed_directories_empty_has_no_ancestors() {
            let failed = FailedDirectories::new();
            assert!(failed.failed_ancestor("any/path/file.txt").is_none());
        }

        #[test]
        fn failed_directories_marks_and_finds_exact() {
            let mut failed = FailedDirectories::new();
            failed.mark_failed("foo/bar");
            assert!(failed.failed_ancestor("foo/bar").is_some());
        }

        #[test]
        fn failed_directories_finds_child_of_failed() {
            let mut failed = FailedDirectories::new();
            failed.mark_failed("foo/bar");
            assert_eq!(
                failed.failed_ancestor("foo/bar/baz/file.txt"),
                Some("foo/bar")
            );
        }

        #[test]
        fn failed_directories_does_not_match_sibling() {
            let mut failed = FailedDirectories::new();
            failed.mark_failed("foo/bar");
            assert!(failed.failed_ancestor("foo/other/file.txt").is_none());
        }

        #[test]
        fn failed_directories_counts_failures() {
            let mut failed = FailedDirectories::new();
            assert_eq!(failed.count(), 0);
            failed.mark_failed("a");
            failed.mark_failed("b");
            assert_eq!(failed.count(), 2);
        }
    }

    #[cfg(feature = "incremental-flist")]
    mod incremental_mode_tests {
        use super::*;
        use tempfile::TempDir;

        #[test]
        fn failed_directories_skips_nested_children() {
            let mut failed = super::super::FailedDirectories::new();
            failed.mark_failed("a/b");

            // Direct child
            assert!(failed.failed_ancestor("a/b/file.txt").is_some());
            // Nested child
            assert!(failed.failed_ancestor("a/b/c/d/file.txt").is_some());
            // Sibling - not affected
            assert!(failed.failed_ancestor("a/c/file.txt").is_none());
            // Parent - not affected
            assert!(failed.failed_ancestor("a/file.txt").is_none());
        }

        #[test]
        fn failed_directories_handles_root_level() {
            let mut failed = super::super::FailedDirectories::new();
            failed.mark_failed("toplevel");

            assert!(failed.failed_ancestor("toplevel/sub/file.txt").is_some());
            assert!(failed.failed_ancestor("other/file.txt").is_none());
        }

        #[test]
        fn stats_tracks_incremental_fields() {
            let stats = TransferStats {
                entries_received: 100,
                directories_created: 20,
                directories_failed: 2,
                files_skipped: 10,
                files_transferred: 68,
                ..Default::default()
            };

            // Verify consistency
            assert_eq!(
                stats.directories_created + stats.directories_failed,
                22 // total directories
            );
        }

        #[test]
        fn create_directory_incremental_nested() {
            let temp = TempDir::new().unwrap();
            let dest = temp.path();

            // Create nested directory
            let entry = FileEntry::new_directory("a/b/c".into(), 0o755);
            let opts = MetadataOptions::default();
            let mut failed = super::super::FailedDirectories::new();

            let handshake = test_handshake();
            let config = test_config();
            let ctx = ReceiverContext::new(&handshake, config);

            let result = ctx.create_directory_incremental(dest, &entry, &opts, &mut failed);

            assert!(result.is_ok());
            assert!(result.unwrap());
            assert!(dest.join("a/b/c").exists());
        }

        #[test]
        fn failed_directories_propagates_to_deeply_nested() {
            let mut failed = super::super::FailedDirectories::new();
            failed.mark_failed("level1");

            // All descendants should be affected
            assert!(failed.failed_ancestor("level1/level2").is_some());
            assert!(failed.failed_ancestor("level1/level2/level3").is_some());
            assert!(
                failed
                    .failed_ancestor("level1/level2/level3/file.txt")
                    .is_some()
            );
        }

        #[test]
        fn transfer_stats_default_values() {
            let stats = TransferStats::default();

            assert_eq!(stats.entries_received, 0);
            assert_eq!(stats.directories_created, 0);
            assert_eq!(stats.directories_failed, 0);
            assert_eq!(stats.files_skipped, 0);
            assert_eq!(stats.files_transferred, 0);
            assert_eq!(stats.bytes_received, 0);
        }
    }
}
