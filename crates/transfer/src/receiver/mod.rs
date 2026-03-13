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
mod file_list;
mod quick_check;
mod stats;
#[cfg(test)]
mod tests;
mod transfer;
mod wire;

use std::num::NonZeroU8;
use std::path::PathBuf;

use protocol::flist::{FileEntry, FileListReader};
use protocol::idlist::IdList;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::shared::ChecksumFactory;

// Public re-exports
pub use self::basis::{BasisFileConfig, BasisFileResult, find_basis_file_with_config};
pub use self::file_list::IncrementalFileListReceiver;
pub use self::stats::{SenderStats, TransferStats};
pub use self::wire::{SenderAttrs, SumHead, write_signature_blocks};

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

/// Minimum candidate count to justify parallel I/O overhead for
/// stat() calls in the quick-check phase. Below this threshold,
/// sequential iteration is faster.
const PARALLEL_STAT_THRESHOLD: usize = 64;

use signature;

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
}

impl ReceiverContext {
    /// Creates a new receiver context from handshake result and config.
    #[must_use]
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        // upstream: flist.c:2923 - ndx_start = inc_recurse ? 1 : 0
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
    /// - `generator.c:2321` - `ndx = i + cur_flist->ndx_start`
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
    #[must_use]
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

    /// Returns whether itemize emission should be active.
    ///
    /// MSG_INFO itemize frames are only emitted when:
    /// - Running in server mode (daemon or SSH) - not client mode
    /// - The client requested `--itemize-changes` (`-i`)
    #[must_use]
    const fn should_emit_itemize(&self) -> bool {
        !self.config.connection.client_mode && self.config.flags.info_flags.itemize
    }

    /// Emits a MSG_INFO frame with itemize output for a file entry.
    ///
    /// Formats the itemize string (`"%i %n%L\n"`) and sends it as a MSG_INFO
    /// multiplexed message. Uses `is_sender: false` since the daemon is receiving
    /// files (producing `>` direction indicator).
    ///
    /// # Upstream Reference
    ///
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
        let line = crate::generator::itemize::format_itemize_line(iflags, entry, false);
        writer.send_msg_info(line.as_bytes())
    }
}

/// Shared configuration produced by [`ReceiverContext::setup_transfer`].
struct PipelineSetup {
    dest_dir: PathBuf,
    metadata_opts: metadata::MetadataOptions,
    checksum_length: NonZeroU8,
    checksum_algorithm: signature::SignatureAlgorithm,
}
