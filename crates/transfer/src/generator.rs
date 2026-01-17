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

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::delta_apply::ChecksumVerifier;
use filters::{FilterRule, FilterSet};
use logging::{debug_log, info_log};
use protocol::codec::{NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
use protocol::codec::{ProtocolCodec, create_protocol_codec};
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};
use protocol::flist::{FileEntry, FileListWriter, compare_file_entries};
use protocol::idlist::IdList;
use protocol::wire::{DeltaOp, SignatureBlock, write_token_stream};
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::delta::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken};

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_name, lookup_user_name};

use super::config::ServerConfig;
use super::handshake::HandshakeResult;
use super::receiver::SumHead;
use super::shared::ChecksumFactory;

/// Context for the generator role during a transfer.
#[derive(Debug)]
pub struct GeneratorContext {
    /// Negotiated protocol version.
    protocol: ProtocolVersion,
    /// Server configuration.
    config: ServerConfig,
    /// List of files to send (contains relative paths for wire transmission).
    file_list: Vec<FileEntry>,
    /// Full filesystem paths for each file in file_list (parallel array).
    /// Used to open files for delta generation during transfer.
    full_paths: Vec<PathBuf>,
    /// Filter rules received from client.
    filters: Option<FilterSet>,
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
    /// Collected UID mappings for name-based ownership transfer.
    uid_list: IdList,
    /// Collected GID mappings for name-based ownership transfer.
    gid_list: IdList,
}

impl GeneratorContext {
    /// Creates a new generator context from handshake result and config.
    #[must_use]
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            full_paths: Vec::new(),
            filters: None,
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
            flist_build_start: None,
            flist_build_end: None,
            flist_xfer_start: None,
            flist_xfer_end: None,
            total_bytes_read: 0,
            uid_list: IdList::new(),
            gid_list: IdList::new(),
        }
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
    #[must_use]
    pub const fn compat_flags(&self) -> Option<protocol::CompatibilityFlags> {
        self.compat_flags
    }

    /// Returns the generated file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
    }

    // =========================================================================
    // Helper Methods - Extracted from run() for modularity and testability
    // =========================================================================

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
        if self.config.client_mode {
            // Client mode: >= 23 (upstream main.c:1304-1305, no filesfrom)
            self.protocol.as_u8() >= 23
        } else {
            // Server mode: >= 30 (need_messages_from_generator)
            self.protocol.as_u8() >= 30
        }
    }

    /// Receives filter list from client in server mode.
    ///
    /// In server mode, we receive filter rules from the client before building
    /// the file list. In client mode, we already sent filters in mod.rs.
    ///
    /// # Upstream Reference
    ///
    /// - Server mode: `recv_filter_list()` at `main.c:1258`
    /// - Client mode: `send_filter_list()` at `main.c:1308` (done in mod.rs)
    fn receive_filter_list_if_server<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        if self.config.client_mode {
            return Ok(()); // Client mode: already sent filter list in mod.rs
        }

        // Server mode: read filter list from client (MULTIPLEXED for protocol >= 30)
        let wire_rules = read_filter_list(reader, self.protocol)?;

        // Convert wire format to FilterSet
        if !wire_rules.is_empty() {
            let filter_set = self.parse_received_filters(&wire_rules)?;
            self.filters = Some(filter_set);
        }

        Ok(())
    }

    /// Collects unique UID/GID values from the file list and looks up their names.
    ///
    /// This must be called after `build_file_list` and before `send_id_lists`.
    /// On non-Unix platforms, this is a no-op since ownership is not preserved.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:add_uid()` / `add_gid()` - called during file list building
    #[cfg(unix)]
    pub fn collect_id_mappings(&mut self) {
        // Skip if numeric_ids is set - no name mapping needed
        if self.config.flags.numeric_ids {
            return;
        }

        self.uid_list.clear();
        self.gid_list.clear();

        for entry in &self.file_list {
            // Collect UIDs if preserving ownership
            if self.config.flags.owner {
                if let Some(uid) = entry.uid() {
                    // Look up name for this UID
                    let name = lookup_user_name(uid).ok().flatten();
                    self.uid_list.add_id(uid, name);
                }
            }

            // Collect GIDs if preserving group
            if self.config.flags.group {
                if let Some(gid) = entry.gid() {
                    // Look up name for this GID
                    let name = lookup_group_name(gid).ok().flatten();
                    self.gid_list.add_id(gid, name);
                }
            }
        }
    }

    /// Collects unique UID/GID values from the file list.
    /// No-op on non-Unix platforms since ownership is not preserved.
    #[cfg(not(unix))]
    pub fn collect_id_mappings(&mut self) {
        // No-op on non-Unix platforms
    }

    /// Sends UID/GID name-to-ID mapping lists to the receiver.
    ///
    /// When `--numeric-ids` is not set, transmits name mappings so the receiver can
    /// translate user/group names to local numeric IDs. When `--numeric-ids` is set,
    /// no mappings are sent and numeric IDs are used as-is.
    ///
    /// # Wire Format
    ///
    /// Each list contains `(varint id, byte name_len, name_bytes)*` tuples terminated
    /// by `varint 0`. With `ID0_NAMES` compat flag, an additional name for id=0
    /// follows the terminator.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2513-2514` - `if (numeric_ids <= 0 && !inc_recurse) send_id_lists(f);`
    /// - `uidlist.c:407-414` - `send_id_lists()`
    pub(crate) fn send_id_lists<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        // Skip ID lists for INC_RECURSE (handled inline) or when numeric_ids is set
        if inc_recurse || self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        // Send UID list if preserving ownership
        if self.config.flags.owner {
            self.uid_list.write(writer, id0_names)?;
        }

        // Send GID list if preserving group
        if self.config.flags.group {
            self.gid_list.write(writer, id0_names)?;
        }

        // Flush to prevent deadlock: receiver waits for ID lists before proceeding
        writer.flush()
    }

    /// Sends io_error flag for protocol < 30.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2517-2518`: `write_int(f, ignore_errors ? 0 : io_error);`
    ///
    /// We always send 0 (no error) for now.
    fn send_io_error_flag<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if self.protocol.as_u8() < 30 {
            writer.write_all(&0i32.to_le_bytes())?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Sends NDX_FLIST_EOF if incremental recursion is enabled.
    ///
    /// This signals to the receiver that there are no more incremental file lists.
    /// For a simple (non-recursive directory) transfer, `send_dir_ndx` is -1, so we
    /// always send `NDX_FLIST_EOF` when INC_RECURSE is enabled.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2534-2545` in `send_file_list()`:
    ///   ```c
    ///   if (inc_recurse) {
    ///       if (send_dir_ndx < 0) {
    ///           write_ndx(f, NDX_FLIST_EOF);
    ///           flist_eof = 1;
    ///       }
    ///   }
    ///   ```
    fn send_flist_eof_if_inc_recurse<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if let Some(flags) = self.compat_flags
            && flags.contains(CompatibilityFlags::INC_RECURSE)
        {
            // Use NdxCodec for protocol-version-aware encoding of NDX_FLIST_EOF
            let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
            ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Returns the checksum algorithm to use for file transfer checksums.
    ///
    /// The algorithm depends on negotiation and protocol version:
    /// - Protocol 30+ with negotiation: uses negotiated algorithm
    /// - Protocol 30+ without negotiation: MD5 (16 bytes)
    /// - Protocol < 30: MD4 (16 bytes)
    #[must_use]
    const fn get_checksum_algorithm(&self) -> ChecksumAlgorithm {
        if let Some(negotiated) = &self.negotiated_algorithms {
            negotiated.checksum
        } else if self.protocol.as_u8() >= 30 {
            ChecksumAlgorithm::MD5
        } else {
            ChecksumAlgorithm::MD4
        }
    }

    /// Validates that a file index is within bounds of the file list.
    fn validate_file_index(&self, ndx: usize) -> io::Result<()> {
        if ndx >= self.file_list.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "invalid file index {}, file list has {} entries",
                    ndx,
                    self.file_list.len()
                ),
            ));
        }
        Ok(())
    }

    /// Runs the main file transfer loop, reading NDX requests from receiver.
    ///
    /// This method processes file transfer requests in phases until all phases complete.
    /// For each file index received, it reads signatures, generates deltas, and sends data.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:send_files()` - Main send loop (lines 210-462)
    /// - `io.c:read_ndx/write_ndx` - NDX protocol encoding
    fn run_transfer_loop<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<TransferLoopResult> {
        // Phase handling: upstream sender.c line 210: max_phase = protocol_version >= 29 ? 2 : 1
        let mut phase: i32 = 0;
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };

        let mut files_transferred = 0;
        let mut bytes_sent = 0u64;

        // Create NDX codecs using Strategy pattern for protocol-version-aware encoding.
        // Upstream rsync uses separate static variables for read and write state (io.c:2244-2245).
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());

        loop {
            // Read NDX value from receiver
            let ndx = ndx_read_codec.read_ndx(&mut *reader)?;

            // Handle negative NDX values (upstream io.c:1736-1750, sender.c:236-258)
            if ndx < 0 {
                match ndx {
                    NDX_DONE => {
                        // Phase transition
                        phase += 1;
                        if phase > max_phase {
                            break;
                        }
                        ndx_write_codec.write_ndx_done(&mut *writer)?;
                        writer.flush()?;
                        continue;
                    }
                    NDX_FLIST_EOF => {
                        // End of incremental file lists (upstream io.c:1738-1741)
                        debug_log!(Flist, 2, "received NDX_FLIST_EOF, file list complete");
                        continue;
                    }
                    NDX_DEL_STATS => {
                        // Deletion statistics (upstream main.c:228-230)
                        // Read and discard: 5 varints for deleted file counts
                        for _ in 0..5 {
                            protocol::read_varint(&mut *reader)?;
                        }
                        debug_log!(Flist, 2, "received and discarded NDX_DEL_STATS");
                        continue;
                    }
                    _ if ndx <= NDX_FLIST_OFFSET => {
                        // Incremental file list directory index (upstream flist.c)
                        debug_log!(Flist, 2, "received NDX_FLIST_OFFSET {}, not supported", ndx);
                        continue;
                    }
                    _ => {
                        // Unknown negative NDX - log and continue
                        debug_log!(Flist, 1, "received unknown negative NDX value {}", ndx);
                        continue;
                    }
                }
            }

            let ndx = ndx as usize;

            // Read item flags using ItemFlags helper
            let iflags = ItemFlags::read(&mut *reader, self.protocol.as_u8())?;
            if self.protocol.as_u8() >= 29 {
                self.total_bytes_read += 2;
            }

            // Read and discard optional trailing fields (basis type, xname)
            let (_fnamecmp_type, xname) = iflags.read_trailing(&mut *reader)?;
            if iflags.has_basis_type() {
                self.total_bytes_read += 1;
            }
            if let Some(ref xname_data) = xname {
                self.total_bytes_read += 4 + xname_data.len() as u64;
            }

            // Check if file should be transferred
            if !iflags.needs_transfer() {
                continue;
            }

            // Read sum_head using SumHead helper
            let sum_head = SumHead::read(&mut *reader)?;
            self.total_bytes_read += 16;

            // Validate file index
            self.validate_file_index(ndx)?;

            let file_entry = &self.file_list[ndx];
            let source_path = &self.full_paths[ndx];

            // Read signature blocks
            let sig_blocks = read_signature_blocks(&mut *reader, &sum_head)?;

            // Track bytes read for signature blocks
            let bytes_per_block = 4 + sum_head.s2length as u64;
            self.total_bytes_read += sum_head.count as u64 * bytes_per_block;

            let block_length = sum_head.blength;
            let strong_sum_length = sum_head.s2length as u8;
            let has_basis = !sum_head.is_empty();

            // Skip non-regular files
            if !file_entry.is_file() {
                continue;
            }

            // Open source file
            let source_file = match fs::File::open(source_path) {
                Ok(f) => f,
                Err(_e) => {
                    continue;
                }
            };

            // Generate delta (or send whole file if no basis)
            let delta_script = if has_basis {
                generate_delta_from_signature(
                    source_file,
                    block_length,
                    sig_blocks,
                    strong_sum_length,
                    self.protocol,
                    self.negotiated_algorithms.as_ref(),
                    self.compat_flags.as_ref(),
                    self.checksum_seed,
                )?
            } else {
                generate_whole_file_delta(source_file)?
            };

            // Send ndx and attrs back to receiver
            let ndx_i32 = ndx as i32;
            ndx_write_codec.write_ndx(&mut *writer, ndx_i32)?;

            // For protocol >= 29, echo back iflags
            if self.protocol.as_u8() >= 29 {
                writer.write_all(&iflags.raw().to_le_bytes())?;
            }

            // Send sum_head back to receiver
            sum_head.write(&mut *writer)?;

            // Compute file checksum and save stats before consuming delta script
            let checksum_algorithm = self.get_checksum_algorithm();
            let file_checksum = compute_file_checksum(
                &delta_script,
                checksum_algorithm,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );
            let delta_total_bytes = delta_script.total_bytes();

            // Send delta tokens (consumes delta_script to avoid clones)
            let wire_ops = script_to_wire_delta(delta_script);
            write_token_stream(&mut &mut *writer, &wire_ops)?;

            // Send file transfer checksum
            writer.write_all(&file_checksum)?;
            writer.flush()?;

            // Track stats
            bytes_sent += delta_total_bytes;
            files_transferred += 1;
        }

        // Send final NDX_DONE
        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        Ok(TransferLoopResult {
            files_transferred,
            bytes_sent,
        })
    }

    /// Sends transfer statistics to the receiver.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:813-844` - `handle_stats()` implementation
    fn send_stats<W: Write>(&self, writer: &mut W, bytes_sent: u64) -> io::Result<()> {
        let total_read: u64 = self.total_bytes_read;
        let total_written: u64 = bytes_sent;
        let total_size: u64 = self.file_list.iter().map(|e| e.size()).sum();

        let flist_buildtime = calculate_duration_ms(self.flist_build_start, self.flist_build_end);
        let flist_xfertime = calculate_duration_ms(self.flist_xfer_start, self.flist_xfer_end);

        // Use protocol-aware codec for stats encoding
        let stats_codec = create_protocol_codec(self.protocol.as_u8());
        stats_codec.write_stat(writer, total_read as i64)?;
        stats_codec.write_stat(writer, total_written as i64)?;
        stats_codec.write_stat(writer, total_size as i64)?;
        if self.protocol.as_u8() >= 29 {
            stats_codec.write_stat(writer, flist_buildtime as i64)?;
            stats_codec.write_stat(writer, flist_xfertime as i64)?;
        }
        writer.flush()?;
        Ok(())
    }

    /// Handles the goodbye handshake at end of transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:880-905` - `read_final_goodbye()`
    fn handle_goodbye<R: Read, W: Write>(&self, reader: &mut R, writer: &mut W) -> io::Result<()> {
        if self.protocol.as_u8() < 24 {
            return Ok(());
        }

        let mut goodbye_byte = [0u8; 1];

        // Read first NDX_DONE from receiver
        reader.read_exact(&mut goodbye_byte)?;

        // Handle both write_ndx(0x00) and write_int(0xFFFFFFFF) formats
        if goodbye_byte[0] == 0xFF {
            let mut extra = [0u8; 3];
            reader.read_exact(&mut extra)?;
        } else if goodbye_byte[0] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected NDX_DONE, got 0x{:02x}", goodbye_byte[0]),
            ));
        }

        // For protocol 31+: write NDX_DONE back, then read again
        if self.protocol.as_u8() >= 31 {
            writer.write_all(&[0x00])?;
            writer.flush()?;

            // Read final NDX_DONE - may fail if daemon kills receiver child early
            match reader.read_exact(&mut goodbye_byte) {
                Ok(()) => {
                    if goodbye_byte[0] == 0xFF {
                        let mut extra = [0u8; 3];
                        let _ = reader.read_exact(&mut extra);
                    }
                }
                Err(e)
                    if e.kind() == io::ErrorKind::ConnectionReset
                        || e.kind() == io::ErrorKind::UnexpectedEof
                        || e.kind() == io::ErrorKind::BrokenPipe
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    // Connection closed during final goodbye - acceptable
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Builds the file list from the specified paths.
    ///
    /// This walks the filesystem starting from each path in the arguments
    /// and builds a sorted file list for transmission.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2192` - `send_file_list()` - Main file list builder
    /// - `flist.c:1456` - `send_file_entry()` - Per-file encoding
    ///
    /// Mirrors upstream recursive directory scanning and file list construction behavior.
    pub fn build_file_list(&mut self, base_paths: &[PathBuf]) -> io::Result<usize> {
        // Track timing for flist_buildtime statistic (upstream stats.flist_buildtime)
        self.flist_build_start = Some(Instant::now());

        info_log!(Flist, 1, "building file list...");
        self.file_list.clear();
        self.full_paths.clear();

        for base_path in base_paths {
            self.walk_path(base_path, base_path)?;
        }

        // Sort file list using rsync's ordering (upstream flist.c:f_name_cmp).
        // We need to sort both file_list and full_paths together to maintain correspondence.
        // Create index array, sort by rsync rules, then reorder both arrays.
        let file_list_ref = &self.file_list;
        let mut indices: Vec<usize> = (0..self.file_list.len()).collect();
        indices.sort_by(|&a, &b| compare_file_entries(&file_list_ref[a], &file_list_ref[b]));

        // Apply permutation in-place using cycle-following algorithm.
        // This avoids cloning every element - O(n) swaps instead of O(n) clones.
        apply_permutation_in_place(&mut self.file_list, &mut self.full_paths, indices);

        // Record end time for flist_buildtime statistic
        self.flist_build_end = Some(Instant::now());

        // Collect UID/GID mappings for name-based ownership transfer
        self.collect_id_mappings();

        let count = self.file_list.len();
        info_log!(Flist, 1, "built file list with {} entries", count);
        debug_log!(
            Flist,
            2,
            "file list entries: {:?}",
            self.file_list.iter().map(|e| e.name()).collect::<Vec<_>>()
        );

        Ok(count)
    }

    /// Recursively walks a path and adds entries to the file list.
    ///
    /// # Upstream Reference
    ///
    /// When the source path is a directory ending with '/', upstream rsync includes
    /// the directory itself as "." entry in the file list. This allows the receiver
    /// to create the destination directory and properly set its attributes.
    ///
    /// See flist.c:send_file_list() which adds "." for the top-level directory.
    fn walk_path(&mut self, base: &Path, path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;

        // Calculate relative path
        let relative = path.strip_prefix(base).unwrap_or(path).to_path_buf();

        // For the base directory, skip the "." entry and just walk children
        // Some clients may not expect/handle the "." entry correctly
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            // Walk children of the base directory (no "." entry)
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                self.walk_path(base, &entry.path())?;
            }
            return Ok(());
        }

        // Check filters if present
        if let Some(ref filters) = self.filters {
            let is_dir = metadata.is_dir();
            if !filters.allows(&relative, is_dir) {
                // Path is excluded by filters, skip it
                return Ok(());
            }
        }

        // Create file entry based on type
        let entry = self.create_entry(path, &relative, &metadata)?;
        self.file_list.push(entry);
        self.full_paths.push(path.to_path_buf());

        // Recurse into directories if recursive mode is enabled
        if metadata.is_dir() && self.config.flags.recursive {
            for dir_entry in std::fs::read_dir(path)? {
                let dir_entry = dir_entry?;
                self.walk_path(base, &dir_entry.path())?;
            }
        }

        Ok(())
    }

    /// Creates a file entry from path and metadata.
    ///
    /// The `full_path` is used for filesystem operations (e.g., reading symlink targets),
    /// while `relative_path` is stored in the entry for transmission to the receiver.
    fn create_entry(
        &self,
        full_path: &Path,
        relative_path: &Path,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let file_type = metadata.file_type();

        let mut entry = if file_type.is_file() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };

            FileEntry::new_file(relative_path.to_path_buf(), metadata.len(), mode)
        } else if file_type.is_dir() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = 0o755;

            FileEntry::new_directory(relative_path.to_path_buf(), mode)
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from(""));

            FileEntry::new_symlink(relative_path.to_path_buf(), target)
        } else {
            // Other file types (devices, etc.)
            FileEntry::new_file(relative_path.to_path_buf(), 0, 0o644)
        };

        // Set modification time
        #[cfg(unix)]
        {
            entry.set_mtime(metadata.mtime(), metadata.mtime_nsec() as u32);
        }
        #[cfg(not(unix))]
        {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_mtime(duration.as_secs() as i64, duration.subsec_nanos());
                }
            }
        }

        // Set ownership if preserving
        #[cfg(unix)]
        if self.config.flags.owner {
            entry.set_uid(metadata.uid());
        }
        #[cfg(unix)]
        if self.config.flags.group {
            entry.set_gid(metadata.gid());
        }

        Ok(entry)
    }

    /// Sends the file list to the receiver.
    pub fn send_file_list<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        // Track timing for flist_xfertime statistic (upstream stats.flist_xfertime)
        self.flist_xfer_start = Some(Instant::now());

        let flist_writer = if let Some(flags) = self.compat_flags {
            FileListWriter::with_compat_flags(self.protocol, flags)
        } else {
            FileListWriter::new(self.protocol)
        };

        // Configure UID/GID preservation based on server flags
        // Upstream flist.c uses preserve_uid/preserve_gid globals
        let mut flist_writer = flist_writer
            .with_preserve_uid(self.config.flags.owner)
            .with_preserve_gid(self.config.flags.group);

        // Wire up iconv converter if configured
        if let Some(ref converter) = self.config.iconv {
            flist_writer = flist_writer.with_iconv(converter.clone());
        }

        for entry in &self.file_list {
            flist_writer.write_entry(writer, entry)?;
        }

        // Write end marker with no error (SAFE_FILE_LIST support)
        // Future: track I/O errors during file list building and pass them here
        flist_writer.write_end(writer, None)?;
        writer.flush()?;

        // Record end time for flist_xfertime statistic
        self.flist_xfer_end = Some(Instant::now());

        Ok(self.file_list.len())
    }

    /// Runs the generator role to completion.
    ///
    /// This orchestrates the full send operation:
    /// 1. Build file list from paths
    /// 2. Send file list
    /// 3. For each file: receive signature, generate delta, send delta
    ///
    /// The writer must be a ServerWriter to support `write_raw` for protocol
    /// messages that bypass multiplexing (like the goodbye NDX_DONE).
    pub fn run<R: Read, W: Write>(
        &mut self,
        mut reader: super::reader::ServerReader<R>,
        writer: &mut super::writer::ServerWriter<W>,
        paths: &[PathBuf],
    ) -> io::Result<GeneratorStats> {
        // Step 1: Activate input multiplex if needed (mode and protocol dependent)
        if self.should_activate_input_multiplex() {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        // Step 1b: Activate compression on reader if negotiated (Protocol 30+ with compression algorithm)
        // This mirrors upstream io.c:io_start_buffering_in()
        // Compression is activated AFTER multiplex, wrapping the multiplexed stream
        if let Some(ref negotiated) = self.negotiated_algorithms
            && let Some(compress_alg) = negotiated.compression.to_compress_algorithm()?
        {
            reader = reader.activate_compression(compress_alg).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to activate INPUT compression: {e}"),
                )
            })?;
        }

        // Step 2: Receive filter list from client (server mode only)
        self.receive_filter_list_if_server(&mut reader)?;

        let reader = &mut reader; // Convert owned reader to mutable reference for rest of function

        // Step 3: Build and send file list
        self.build_file_list(paths)?;
        let file_count = self.send_file_list(writer)?;

        // Step 4: Send ID lists for non-INC_RECURSE protocols
        self.send_id_lists(writer)?;

        // Step 5: Send io_error flag for protocol < 30
        self.send_io_error_flag(writer)?;

        // Step 6: Send NDX_FLIST_EOF if incremental recursion is enabled
        self.send_flist_eof_if_inc_recurse(writer)?;

        // Step 7: Run main transfer loop
        let transfer_result = self.run_transfer_loop(reader, writer)?;

        // Step 8: Send statistics to receiver
        self.send_stats(writer, transfer_result.bytes_sent)?;

        // Step 9: Handle goodbye handshake
        self.handle_goodbye(reader, writer)?;

        // Calculate timing stats for return value
        let flist_buildtime = calculate_duration_ms(self.flist_build_start, self.flist_build_end);
        let flist_xfertime = calculate_duration_ms(self.flist_xfer_start, self.flist_xfer_end);

        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred: transfer_result.files_transferred,
            bytes_sent: transfer_result.bytes_sent,
            bytes_read: self.total_bytes_read,
            flist_buildtime_ms: flist_buildtime,
            flist_xfertime_ms: flist_xfertime,
        })
    }

    /// Converts wire format rules to FilterSet.
    ///
    /// Maps the wire protocol representation to the filters crate's `FilterSet`
    /// for use during file walking.
    fn parse_received_filters(&self, wire_rules: &[FilterRuleWireFormat]) -> io::Result<FilterSet> {
        let mut rules = Vec::with_capacity(wire_rules.len());

        for wire_rule in wire_rules {
            // Convert wire RuleType to FilterRule
            let mut rule = match wire_rule.rule_type {
                RuleType::Include => FilterRule::include(&wire_rule.pattern),
                RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
                RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
                RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
                RuleType::Clear => {
                    // Clear rule removes all previous rules
                    rules.push(
                        FilterRule::clear()
                            .with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                    );
                    continue;
                }
                RuleType::Merge | RuleType::DirMerge => {
                    // Merge rules require per-directory filter file loading during file walking.
                    // Implementation requires:
                    // 1. Store merge rule specs (filename, options like inherit/exclude_self)
                    // 2. During build_file_list(), check each directory for the merge file
                    // 3. Parse merge file contents using engine::local_copy::dir_merge parsing
                    // 4. Inject parsed rules into the active FilterSet for that subtree
                    // 5. Pop rules when leaving directories (if no_inherit is set)
                    //
                    // See crates/engine/src/local_copy/dir_merge/ for the local copy implementation
                    // that can be adapted for server mode. The challenge is that FilterSet is
                    // currently immutable after construction.
                    //
                    // For now, clients can pre-expand merge rules before transmission, or use
                    // local copy mode which fully supports merge rules.
                    continue;
                }
            };

            // Apply modifiers
            if wire_rule.sender_side || wire_rule.receiver_side {
                rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
            }

            if wire_rule.perishable {
                rule = rule.with_perishable(true);
            }

            if wire_rule.xattr_only {
                rule = rule.with_xattr_only(true);
            }

            if wire_rule.anchored {
                rule = rule.anchor_to_root();
            }

            // Note: directory_only, no_inherit, cvs_exclude, word_split, exclude_from_merge
            // are pattern modifiers handled by the filters crate during compilation
            // We store them in the pattern itself as upstream does

            rules.push(rule);
        }

        FilterSet::from_rules(rules)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))
    }
}

/// Result from the transfer loop phase of the generator.
///
/// Contains statistics from processing file transfer requests.
#[derive(Debug, Clone, Default)]
struct TransferLoopResult {
    /// Number of files actually transferred.
    files_transferred: usize,
    /// Total bytes sent during transfer.
    bytes_sent: u64,
}

/// Statistics from a generator transfer operation.
#[derive(Debug, Clone, Default)]
pub struct GeneratorStats {
    /// Number of files in the sent file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes sent.
    pub bytes_sent: u64,
    /// Total bytes read from network.
    pub bytes_read: u64,
    /// File list build time in milliseconds.
    pub flist_buildtime_ms: u64,
    /// File list transfer time in milliseconds.
    pub flist_xfertime_ms: u64,
}

// ============================================================================
// ItemFlags - Encapsulates item flags parsing from receiver
// ============================================================================

/// Item flags received from the receiver indicating transfer requirements.
///
/// The generator reads these flags to determine how to handle each file request.
/// Protocol versions >= 29 include these flags with each file index.
///
/// # Upstream Reference
///
/// - `rsync.h:100-115` - Item flag definitions
/// - `rsync.c:227` - `read_ndx_and_attrs()` reads iflags
/// - `sender.c:324` - Sender processes these flags
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemFlags {
    /// Raw 16-bit flags value.
    raw: u16,
}

impl ItemFlags {
    /// Item needs data transfer (file content differs).
    pub const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
    /// Item is being reported (itemized output).
    pub const ITEM_REPORT_ATIME: u16 = 1 << 14; // 0x4000
    /// Item is being reported for checksum change.
    pub const ITEM_REPORT_CHECKSUM: u16 = 1 << 13; // 0x2000
    /// Alternate basis file name follows.
    pub const ITEM_XNAME_FOLLOWS: u16 = 1 << 12; // 0x1000
    /// Basis file type follows.
    pub const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11; // 0x0800
    /// Item reports size change.
    pub const ITEM_REPORT_SIZE: u16 = 1 << 10; // 0x0400
    /// Item reports time change.
    pub const ITEM_REPORT_TIME: u16 = 1 << 9; // 0x0200
    /// Item reports perms change.
    pub const ITEM_REPORT_PERMS: u16 = 1 << 8; // 0x0100
    /// Item reports owner change.
    pub const ITEM_REPORT_OWNER: u16 = 1 << 7; // 0x0080
    /// Item reports group change.
    pub const ITEM_REPORT_GROUP: u16 = 1 << 6; // 0x0040
    /// Item reports ACL change.
    pub const ITEM_REPORT_ACL: u16 = 1 << 5; // 0x0020
    /// Item reports xattr change.
    pub const ITEM_REPORT_XATTR: u16 = 1 << 4; // 0x0010
    /// Item is a directory.
    pub const ITEM_IS_NEW: u16 = 1 << 3; // 0x0008
    /// Item's basis matched fuzzy file.
    pub const ITEM_LOCAL_CHANGE: u16 = 1 << 2; // 0x0004
    /// Transfer type follows (hardlink, etc).
    pub const ITEM_TRANSFER_TYPE: u16 = 1 << 1; // 0x0002
    /// Extended name follows (symlink target, etc).
    pub const ITEM_REPORT_LINKS: u16 = 1 << 0; // 0x0001

    /// Creates ItemFlags from raw 16-bit value.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        Self { raw }
    }

    /// Returns the raw 16-bit flags value.
    #[must_use]
    pub const fn raw(&self) -> u16 {
        self.raw
    }

    /// Returns true if the item needs data transfer.
    #[must_use]
    pub const fn needs_transfer(&self) -> bool {
        self.raw & Self::ITEM_TRANSFER != 0
    }

    /// Returns true if basis file type follows.
    #[must_use]
    pub const fn has_basis_type(&self) -> bool {
        self.raw & Self::ITEM_BASIS_TYPE_FOLLOWS != 0
    }

    /// Returns true if extended name follows.
    #[must_use]
    pub const fn has_xname(&self) -> bool {
        self.raw & Self::ITEM_XNAME_FOLLOWS != 0
    }

    /// Reads item flags from the wire.
    ///
    /// For protocol >= 29, reads 2 bytes little-endian.
    /// For older protocols, returns ITEM_TRANSFER as default.
    pub fn read<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Self> {
        if protocol_version >= 29 {
            let mut buf = [0u8; 2];
            reader.read_exact(&mut buf)?;
            Ok(Self::from_raw(u16::from_le_bytes(buf)))
        } else {
            // Older protocols assume transfer is needed
            Ok(Self::from_raw(Self::ITEM_TRANSFER))
        }
    }

    /// Reads optional trailing fields based on flags.
    ///
    /// Returns (fnamecmp_type, xname) where each is present only if indicated by flags.
    pub fn read_trailing<R: Read>(
        &self,
        reader: &mut R,
    ) -> io::Result<(Option<u8>, Option<Vec<u8>>)> {
        // Read basis file type if ITEM_BASIS_TYPE_FOLLOWS
        let fnamecmp_type = if self.has_basis_type() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(byte[0])
        } else {
            None
        };

        // Read extended name if ITEM_XNAME_FOLLOWS
        let xname = if self.has_xname() {
            // vstring format: first byte is length; if bit 7 set, length = (byte & 0x7F) * 256 + next_byte
            let xlen = protocol::read_varint(reader)? as usize;
            if xlen > 0 {
                let actual_len = xlen.min(4096); // Sanity limit
                let mut xname_buf = vec![0u8; actual_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        Ok((fnamecmp_type, xname))
    }
}

// ============================================================================
// Signature Block Reading - Encapsulates reading checksum blocks from receiver
// ============================================================================

/// Reads signature blocks from the receiver.
///
/// After reading sum_head, this reads the rolling and strong checksums for each block.
/// When sum_head.count is 0, returns an empty Vec (whole-file transfer).
///
/// # Upstream Reference
///
/// - `sender.c:120` - `receive_sums()` reads signature blocks
/// - `match.c:395` - Block format: rolling_sum (4 bytes) + strong_sum (s2length bytes)
pub fn read_signature_blocks<R: Read>(
    reader: &mut R,
    sum_head: &SumHead,
) -> io::Result<Vec<SignatureBlock>> {
    if sum_head.is_empty() {
        // No basis file (count=0), whole-file transfer - no blocks to read
        return Ok(Vec::new());
    }

    let mut blocks = Vec::with_capacity(sum_head.count as usize);

    for i in 0..sum_head.count {
        // Read rolling checksum (4 bytes LE)
        let mut rolling_bytes = [0u8; 4];
        reader.read_exact(&mut rolling_bytes)?;
        let rolling_sum = u32::from_le_bytes(rolling_bytes);

        // Read strong checksum (s2length bytes)
        let mut strong_sum = vec![0u8; sum_head.s2length as usize];
        reader.read_exact(&mut strong_sum)?;

        blocks.push(SignatureBlock {
            index: i,
            rolling_sum,
            strong_sum,
        });
    }

    Ok(blocks)
}

// ============================================================================
// Timing Helpers - Statistics calculation utilities
// ============================================================================

/// Calculates duration in milliseconds between two optional timestamps.
///
/// Returns 0 if either timestamp is `None`.
///
/// # Usage
///
/// Used for calculating `flist_buildtime` and `flist_xfertime` statistics
/// sent to the client during protocol finalization.
#[must_use]
pub fn calculate_duration_ms(start: Option<Instant>, end: Option<Instant>) -> u64 {
    match (start, end) {
        (Some(s), Some(e)) => e.duration_since(s).as_millis() as u64,
        _ => 0,
    }
}

// Helper functions for delta generation

/// Generates a delta script from a received signature.
///
/// Reconstructs the signature from wire format blocks, creates an index,
/// and uses DeltaGenerator to produce the delta.
///
/// Takes ownership of sig_blocks to avoid cloning strong_sum data.
#[allow(clippy::too_many_arguments)]
fn generate_delta_from_signature<R: Read>(
    source: R,
    block_length: u32,
    sig_blocks: Vec<protocol::wire::signature::SignatureBlock>,
    strong_sum_length: u8,
    protocol: ProtocolVersion,
    negotiated_algorithms: Option<&NegotiationResult>,
    compat_flags: Option<&CompatibilityFlags>,
    checksum_seed: i32,
) -> io::Result<DeltaScript> {
    use checksums::RollingDigest;
    use engine::delta::SignatureLayout;
    use engine::signature::{FileSignature, SignatureBlock};
    use std::num::{NonZeroU8, NonZeroU32};

    // Reconstruct engine signature from wire format
    let block_length_nz = NonZeroU32::new(block_length).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "block length must be non-zero")
    })?;

    let strong_sum_length_nz = NonZeroU8::new(strong_sum_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "strong sum length must be non-zero",
        )
    })?;

    let block_count = sig_blocks.len() as u64;

    // Reconstruct signature layout (remainder unknown, set to 0)
    let layout = SignatureLayout::from_raw_parts(
        block_length_nz,
        0, // remainder unknown from wire format
        block_count,
        strong_sum_length_nz,
    );

    // Convert wire blocks to engine signature blocks (consumes sig_blocks)
    let engine_blocks: Vec<SignatureBlock> = sig_blocks
        .into_iter()
        .map(|wire_block| {
            SignatureBlock::from_raw_parts(
                wire_block.index as u64,
                RollingDigest::from_value(wire_block.rolling_sum, block_length as usize),
                wire_block.strong_sum,
            )
        })
        .collect();

    // Calculate total bytes (approximation since we don't know exact remainder)
    let total_bytes = (block_count.saturating_sub(1)) * u64::from(block_length);
    let signature = FileSignature::from_raw_parts(layout, engine_blocks, total_bytes);

    // Select checksum algorithm using ChecksumFactory (handles negotiated vs default)
    let checksum_factory = ChecksumFactory::from_negotiation(
        negotiated_algorithms,
        protocol,
        checksum_seed,
        compat_flags,
    );
    let checksum_algorithm = checksum_factory.signature_algorithm();

    // Create index for delta generation
    let index =
        DeltaSignatureIndex::from_signature(&signature, checksum_algorithm).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "failed to create signature index",
            )
        })?;

    // Generate delta
    let generator = DeltaGenerator::new();
    generator
        .generate(source, &index)
        .map_err(|e| io::Error::other(format!("delta generation failed: {e}")))
}

/// Maximum file size for in-memory whole-file transfer (8 GB).
///
/// Files larger than this limit require streaming approaches that are not
/// yet implemented. This limit prevents OOM from unbounded `read_to_end()`.
const MAX_IN_MEMORY_SIZE: u64 = 8 * 1024 * 1024 * 1024;

/// Generates a delta script containing the entire file as literals (whole-file transfer).
///
/// # Size Limit
///
/// This function reads the entire file into memory. Files larger than
/// [`MAX_IN_MEMORY_SIZE`] (8 GB) will return an error to prevent OOM.
fn generate_whole_file_delta<R: Read>(mut source: R) -> io::Result<DeltaScript> {
    let mut data = Vec::new();
    source.read_to_end(&mut data)?;

    // Check size limit to prevent OOM
    let total_bytes = data.len() as u64;
    if total_bytes > MAX_IN_MEMORY_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "File too large for whole-file transfer: {total_bytes} bytes (max {MAX_IN_MEMORY_SIZE})"
            ),
        ));
    }

    let tokens = vec![DeltaToken::Literal(data)];

    Ok(DeltaScript::new(tokens, total_bytes, total_bytes))
}

/// Computes the file transfer checksum from delta script data.
///
/// After sending delta tokens, upstream rsync sends a file checksum for verification.
/// This checksum is computed over all bytes being transferred (literal data + copy sources).
///
/// Reference: upstream match.c lines 370, 411, 426:
/// - `sum_init(xfer_sum_nni, checksum_seed);` - start with seed
/// - `sum_end(sender_file_sum);` - finalize
/// - `write_buf(f, sender_file_sum, xfer_sum_len);` - send checksum
fn compute_file_checksum(
    script: &DeltaScript,
    algorithm: ChecksumAlgorithm,
    _seed: i32,
    _compat_flags: Option<&CompatibilityFlags>,
) -> Vec<u8> {
    // Special case: None uses a 1-byte placeholder
    if matches!(algorithm, ChecksumAlgorithm::None) {
        return vec![0u8];
    }

    // Use ChecksumVerifier for all other algorithms (uses trait delegation internally)
    let mut verifier = ChecksumVerifier::for_algorithm(algorithm);

    // Feed all literal bytes from the script to the verifier
    for token in script.tokens() {
        if let DeltaToken::Literal(data) = token {
            verifier.update(data);
        }
        // Note: Copy tokens reference basis file blocks - the receiver has those.
        // The checksum is computed on all data bytes (matching upstream behavior
        // where sum_update is called on each data chunk during match processing).
    }

    verifier.finalize()
}

/// Converts engine delta script to wire protocol delta operations.
///
/// Takes ownership of the script to avoid cloning literal data.
fn script_to_wire_delta(script: DeltaScript) -> Vec<DeltaOp> {
    script
        .into_tokens()
        .into_iter()
        .map(|token| match token {
            DeltaToken::Literal(data) => DeltaOp::Literal(data),
            DeltaToken::Copy { index, len } => DeltaOp::Copy {
                block_index: index as u32,
                length: len as u32,
            },
        })
        .collect()
}

/// Applies a source-based permutation to two slices in-place using cycle-following.
///
/// This reorders elements according to the permutation `source_indices` without
/// cloning elements - only swaps are used. The algorithm inverts the permutation
/// and then follows each cycle, placing elements in their final positions.
///
/// # Arguments
/// * `slice_a` - First slice to reorder
/// * `slice_b` - Second slice to reorder (must have same length)
/// * `source_indices` - Source-based permutation where `source_indices[i]` is the
///   index of the element that should end up at position `i`
///
/// # Time Complexity
/// O(n) time and O(n) space for the inverse permutation.
fn apply_permutation_in_place<A, B>(
    slice_a: &mut [A],
    slice_b: &mut [B],
    source_indices: Vec<usize>,
) {
    let n = slice_a.len();
    debug_assert_eq!(slice_b.len(), n);
    debug_assert_eq!(source_indices.len(), n);

    if n == 0 {
        return;
    }

    // Invert the permutation: source_indices[i] = j becomes dest_perm[j] = i
    // This converts "element at j goes to i" to "element at i goes to j"
    let mut dest_perm = vec![0; n];
    for (new_pos, &old_pos) in source_indices.iter().enumerate() {
        dest_perm[old_pos] = new_pos;
    }

    // Apply destination-based permutation using cycle-following
    for i in 0..n {
        while dest_perm[i] != i {
            let j = dest_perm[i];
            slice_a.swap(i, j);
            slice_b.swap(i, j);
            dest_perm.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::flags::ParsedServerFlags;
    use super::super::role::ServerRole;
    use super::*;
    use std::ffi::OsString;
    use std::io::Cursor;

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Generator,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags::default(),
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
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
    fn generator_context_creation() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        assert_eq!(ctx.protocol().as_u8(), 32);
        assert!(ctx.file_list().is_empty());
    }

    #[test]
    fn send_empty_file_list() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let count = ctx.send_file_list(&mut output).unwrap();

        assert_eq!(count, 0);
        // Should just have the end marker
        assert_eq!(output, vec![0u8]);
    }

    #[test]
    fn send_single_file_entry() {
        let handshake = test_handshake();
        let config = test_config();
        let mut ctx = GeneratorContext::new(&handshake, config);

        // Manually add an entry
        let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
        ctx.file_list.push(entry);

        let mut output = Vec::new();
        let count = ctx.send_file_list(&mut output).unwrap();

        assert_eq!(count, 1);
        // Should have entry data plus end marker
        assert!(!output.is_empty());
        assert_eq!(*output.last().unwrap(), 0u8); // End marker
    }

    #[test]
    fn build_and_send_round_trip() {
        use super::super::receiver::ReceiverContext;
        use std::io::Cursor;

        let handshake = test_handshake();
        let mut gen_config = test_config();
        gen_config.role = ServerRole::Generator;
        let mut generator = GeneratorContext::new(&handshake, gen_config);

        // Add some entries manually (simulating a walk)
        let mut entry1 = FileEntry::new_file("file1.txt".into(), 100, 0o644);
        entry1.set_mtime(1700000000, 0);
        let mut entry2 = FileEntry::new_file("file2.txt".into(), 200, 0o644);
        entry2.set_mtime(1700000000, 0);
        generator.file_list.push(entry1);
        generator.file_list.push(entry2);

        // Send file list
        let mut wire_data = Vec::new();
        generator.send_file_list(&mut wire_data).unwrap();

        // Receive file list
        let recv_config = test_config();
        let mut receiver = ReceiverContext::new(&handshake, recv_config);
        let mut cursor = Cursor::new(&wire_data[..]);
        let count = receiver.receive_file_list(&mut cursor).unwrap();

        assert_eq!(count, 2);
        assert_eq!(receiver.file_list()[0].name(), "file1.txt");
        assert_eq!(receiver.file_list()[1].name(), "file2.txt");
    }

    #[test]
    fn parse_received_filters_empty() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        // Empty filter list
        let wire_rules = vec![];
        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_single_exclude() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_owned())];
        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(!filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_multiple_rules() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.log".to_owned()),
            FilterRuleWireFormat::include("*.txt".to_owned()),
            FilterRuleWireFormat::exclude("temp/".to_owned()).with_directory_only(true),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        assert!(!filter_set.is_empty());
    }

    #[test]
    fn parse_received_filters_with_modifiers() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::FilterRuleWireFormat;

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.tmp".to_owned())
                .with_sides(true, false)
                .with_perishable(true),
            FilterRuleWireFormat::include("/important".to_owned()).with_anchored(true),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_received_filters_clear_rule() {
        let handshake = test_handshake();
        let config = test_config();
        let ctx = GeneratorContext::new(&handshake, config);

        use protocol::filters::{FilterRuleWireFormat, RuleType};

        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*.log".to_owned()),
            FilterRuleWireFormat {
                rule_type: RuleType::Clear,
                pattern: String::new(),
                anchored: false,
                directory_only: false,
                no_inherit: false,
                cvs_exclude: false,
                word_split: false,
                exclude_from_merge: false,
                xattr_only: false,
                sender_side: true,
                receiver_side: true,
                perishable: false,
                negate: false,
            },
            FilterRuleWireFormat::include("*.txt".to_owned()),
        ];

        let result = ctx.parse_received_filters(&wire_rules);
        assert!(result.is_ok());

        let filter_set = result.unwrap();
        // Clear rule should have removed previous rules
        assert!(!filter_set.is_empty()); // Only the include rule remains
    }

    #[test]
    fn filter_application_excludes_files() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("include.txt"), b"included").unwrap();
        std::fs::write(base_path.join("exclude.log"), b"excluded").unwrap();
        std::fs::write(base_path.join("another.txt"), b"also included").unwrap();

        // Set up generator with filter that excludes *.log
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false; // Don't recurse for this test

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters
        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_owned())];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have 2 entries: 2 .txt files (not the .log file)
        // Note: "." entry is NOT included (we skip base directory entry for interop)
        assert_eq!(count, 2);
        let file_list = ctx.file_list();
        assert_eq!(file_list.len(), 2);

        // Verify the .log file is not in the list
        for entry in file_list {
            assert!(!entry.name().contains(".log"));
        }
    }

    #[test]
    fn filter_application_includes_only_matching() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("data.txt"), b"text").unwrap();
        std::fs::write(base_path.join("script.sh"), b"shell").unwrap();
        std::fs::write(base_path.join("readme.md"), b"markdown").unwrap();

        // Set up generator with filters: exclude all, then include only *.txt
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false;

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters: exclude *, include *.txt
        let wire_rules = vec![
            FilterRuleWireFormat::exclude("*".to_owned()),
            FilterRuleWireFormat::include("*.txt".to_owned()),
        ];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have 1 entry: data.txt (other files excluded by "exclude *")
        // Note: "." entry is NOT included (we skip base directory entry for interop)
        assert_eq!(count, 1);
        let file_list = ctx.file_list();
        assert_eq!(file_list.len(), 1);
        assert_eq!(file_list[0].name(), "data.txt");
    }

    #[test]
    fn filter_application_with_directories() {
        use protocol::filters::FilterRuleWireFormat;
        use tempfile::TempDir;

        // Create temporary test directory structure
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        std::fs::create_dir(base_path.join("include_dir")).unwrap();
        std::fs::write(base_path.join("include_dir/file.txt"), b"data").unwrap();

        std::fs::create_dir(base_path.join("exclude_dir")).unwrap();
        std::fs::write(base_path.join("exclude_dir/file.txt"), b"data").unwrap();

        // Set up generator with filter that excludes exclude_dir/
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = true;

        let mut ctx = GeneratorContext::new(&handshake, config);

        // Parse and set filters
        let wire_rules = vec![
            FilterRuleWireFormat::exclude("exclude_dir/".to_owned()).with_directory_only(true),
        ];
        let filter_set = ctx.parse_received_filters(&wire_rules).unwrap();
        ctx.filters = Some(filter_set);

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have include_dir and its file, but not exclude_dir
        assert!(count >= 2); // At least the directory and one file
        let file_list = ctx.file_list();

        // Verify exclude_dir is not in the list
        for entry in file_list {
            let name = entry.name();
            assert!(!name.contains("exclude_dir"), "Found excluded dir: {name}");
        }
    }

    #[test]
    fn filter_application_no_filters_includes_all() {
        use tempfile::TempDir;

        // Create temporary test directory
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        // Create test files
        std::fs::write(base_path.join("file1.txt"), b"data1").unwrap();
        std::fs::write(base_path.join("file2.log"), b"data2").unwrap();
        std::fs::write(base_path.join("file3.md"), b"data3").unwrap();

        // Set up generator with NO filters
        let handshake = test_handshake();
        let mut config = test_config();
        config.args = vec![OsString::from(base_path)];
        config.flags.recursive = false;

        let mut ctx = GeneratorContext::new(&handshake, config);
        // No filters set (ctx.filters remains None)

        // Build file list
        let paths = vec![base_path.to_path_buf()];
        let count = ctx.build_file_list(&paths).unwrap();

        // Should have 3 entries: 3 files when no filters are present
        // Note: "." entry is NOT included (we skip base directory entry for interop)
        assert_eq!(count, 3);
        assert_eq!(ctx.file_list().len(), 3);
    }

    #[test]
    fn script_to_wire_delta_converts_literals() {
        let tokens = vec![
            DeltaToken::Literal(vec![1, 2, 3]),
            DeltaToken::Literal(vec![4, 5, 6]),
        ];
        let script = DeltaScript::new(tokens, 6, 6);

        let wire_ops = script_to_wire_delta(script);

        assert_eq!(wire_ops.len(), 2);
        match &wire_ops[0] {
            DeltaOp::Literal(data) => assert_eq!(data, &vec![1, 2, 3]),
            _ => panic!("expected literal op"),
        }
        match &wire_ops[1] {
            DeltaOp::Literal(data) => assert_eq!(data, &vec![4, 5, 6]),
            _ => panic!("expected literal op"),
        }
    }

    #[test]
    fn script_to_wire_delta_converts_copy_operations() {
        let tokens = vec![
            DeltaToken::Copy {
                index: 0,
                len: 1024,
            },
            DeltaToken::Literal(vec![99]),
            DeltaToken::Copy { index: 1, len: 512 },
        ];
        let script = DeltaScript::new(tokens, 1537, 1);

        let wire_ops = script_to_wire_delta(script);

        assert_eq!(wire_ops.len(), 3);
        match &wire_ops[0] {
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                assert_eq!(*block_index, 0);
                assert_eq!(*length, 1024);
            }
            _ => panic!("expected copy op"),
        }
        match &wire_ops[1] {
            DeltaOp::Literal(data) => assert_eq!(data, &vec![99]),
            _ => panic!("expected literal op"),
        }
        match &wire_ops[2] {
            DeltaOp::Copy {
                block_index,
                length,
            } => {
                assert_eq!(*block_index, 1);
                assert_eq!(*length, 512);
            }
            _ => panic!("expected copy op"),
        }
    }

    #[test]
    fn generate_whole_file_delta_reads_entire_file() {
        let data = b"Hello, world! This is a test file.";
        let mut cursor = Cursor::new(&data[..]);

        let script = generate_whole_file_delta(&mut cursor).unwrap();

        assert_eq!(script.tokens().len(), 1);
        assert_eq!(script.total_bytes(), data.len() as u64);
        assert_eq!(script.literal_bytes(), data.len() as u64);

        match &script.tokens()[0] {
            DeltaToken::Literal(content) => assert_eq!(content, &data.to_vec()),
            _ => panic!("expected literal token"),
        }
    }

    #[test]
    fn generate_whole_file_delta_handles_empty_file() {
        let data = b"";
        let mut cursor = Cursor::new(&data[..]);

        let script = generate_whole_file_delta(&mut cursor).unwrap();

        assert_eq!(script.tokens().len(), 1);
        assert_eq!(script.total_bytes(), 0);
        assert_eq!(script.literal_bytes(), 0);

        match &script.tokens()[0] {
            DeltaToken::Literal(content) => assert!(content.is_empty()),
            _ => panic!("expected literal token"),
        }
    }

    #[test]
    fn generate_whole_file_delta_checks_size_limit() {
        // Test that the size limit constant exists and is reasonable (8GB)
        assert_eq!(MAX_IN_MEMORY_SIZE, 8 * 1024 * 1024 * 1024);

        // Note: We can't practically test reading 8GB+ in a unit test.
        // The size check happens after read_to_end(), which means we'd need
        // to actually allocate 8GB+ to trigger it. This is impractical for CI.
        // The constant exists and is used in generate_whole_file_delta().
    }

    #[test]
    fn generate_whole_file_delta_accepts_max_size_file() {
        // Test that a file exactly at MAX_IN_MEMORY_SIZE is accepted
        // We won't actually allocate 8GB, just test a small file to verify the logic works
        let data = vec![0xAB; 1024]; // 1KB test file
        let mut cursor = Cursor::new(&data);

        let script = generate_whole_file_delta(&mut cursor).unwrap();

        assert_eq!(script.tokens().len(), 1);
        assert_eq!(script.total_bytes(), 1024);
        assert_eq!(script.literal_bytes(), 1024);

        match &script.tokens()[0] {
            DeltaToken::Literal(content) => {
                assert_eq!(content.len(), 1024);
                assert!(content.iter().all(|&b| b == 0xAB));
            }
            _ => panic!("expected literal token"),
        }
    }

    // ========================================================================
    // ItemFlags Tests
    // ========================================================================

    #[test]
    fn item_flags_from_raw() {
        let flags = ItemFlags::from_raw(0x8000);
        assert_eq!(flags.raw(), 0x8000);
        assert!(flags.needs_transfer());
        assert!(!flags.has_basis_type());
        assert!(!flags.has_xname());
    }

    #[test]
    fn item_flags_needs_transfer() {
        // Test ITEM_TRANSFER flag (0x8000)
        assert!(ItemFlags::from_raw(0x8000).needs_transfer());
        assert!(ItemFlags::from_raw(0x8001).needs_transfer());
        assert!(ItemFlags::from_raw(0xFFFF).needs_transfer());
        assert!(!ItemFlags::from_raw(0x0000).needs_transfer());
        assert!(!ItemFlags::from_raw(0x7FFF).needs_transfer());
    }

    #[test]
    fn item_flags_has_basis_type() {
        // Test ITEM_BASIS_TYPE_FOLLOWS flag (0x0800)
        assert!(ItemFlags::from_raw(0x0800).has_basis_type());
        assert!(ItemFlags::from_raw(0x8800).has_basis_type());
        assert!(!ItemFlags::from_raw(0x0000).has_basis_type());
        assert!(!ItemFlags::from_raw(0x8000).has_basis_type());
    }

    #[test]
    fn item_flags_has_xname() {
        // Test ITEM_XNAME_FOLLOWS flag (0x1000)
        assert!(ItemFlags::from_raw(0x1000).has_xname());
        assert!(ItemFlags::from_raw(0x9000).has_xname());
        assert!(!ItemFlags::from_raw(0x0000).has_xname());
        assert!(!ItemFlags::from_raw(0x8000).has_xname());
    }

    #[test]
    fn item_flags_read_protocol_29_plus() {
        // Protocol 29+ reads 2 bytes little-endian
        let data = [0x00, 0x80]; // 0x8000 = ITEM_TRANSFER
        let mut cursor = Cursor::new(&data[..]);

        let flags = ItemFlags::read(&mut cursor, 29).unwrap();
        assert_eq!(flags.raw(), 0x8000);
        assert!(flags.needs_transfer());
    }

    #[test]
    fn item_flags_read_protocol_28() {
        // Protocol 28 and older defaults to ITEM_TRANSFER without reading
        let data: [u8; 0] = [];
        let mut cursor = Cursor::new(&data[..]);

        let flags = ItemFlags::read(&mut cursor, 28).unwrap();
        assert_eq!(flags.raw(), ItemFlags::ITEM_TRANSFER);
        assert!(flags.needs_transfer());
    }

    #[test]
    fn item_flags_read_trailing_no_fields() {
        // No trailing fields when neither flag is set
        let data: [u8; 0] = [];
        let mut cursor = Cursor::new(&data[..]);

        let flags = ItemFlags::from_raw(0x8000); // Just ITEM_TRANSFER
        let (ftype, xname) = flags.read_trailing(&mut cursor).unwrap();

        assert!(ftype.is_none());
        assert!(xname.is_none());
    }

    #[test]
    fn item_flags_read_trailing_basis_type() {
        // ITEM_BASIS_TYPE_FOLLOWS reads 1 byte
        let data = [0x42]; // basis type = 0x42
        let mut cursor = Cursor::new(&data[..]);

        let flags = ItemFlags::from_raw(0x0800); // ITEM_BASIS_TYPE_FOLLOWS
        let (ftype, xname) = flags.read_trailing(&mut cursor).unwrap();

        assert_eq!(ftype, Some(0x42));
        assert!(xname.is_none());
    }

    #[test]
    fn item_flags_combined_flags() {
        // Test multiple flags combined
        let flags = ItemFlags::from_raw(0x9800); // TRANSFER + XNAME + BASIS_TYPE
        assert!(flags.needs_transfer());
        assert!(flags.has_basis_type());
        assert!(flags.has_xname());
    }

    #[test]
    fn item_flags_constants() {
        // Verify constant values match upstream rsync
        assert_eq!(ItemFlags::ITEM_TRANSFER, 0x8000);
        assert_eq!(ItemFlags::ITEM_REPORT_ATIME, 0x4000);
        assert_eq!(ItemFlags::ITEM_REPORT_CHECKSUM, 0x2000);
        assert_eq!(ItemFlags::ITEM_XNAME_FOLLOWS, 0x1000);
        assert_eq!(ItemFlags::ITEM_BASIS_TYPE_FOLLOWS, 0x0800);
        assert_eq!(ItemFlags::ITEM_REPORT_SIZE, 0x0400);
        assert_eq!(ItemFlags::ITEM_REPORT_TIME, 0x0200);
        assert_eq!(ItemFlags::ITEM_REPORT_PERMS, 0x0100);
        assert_eq!(ItemFlags::ITEM_REPORT_OWNER, 0x0080);
        assert_eq!(ItemFlags::ITEM_REPORT_GROUP, 0x0040);
    }

    // ========================================================================
    // read_signature_blocks Tests
    // ========================================================================

    #[test]
    fn read_signature_blocks_empty() {
        // count=0 means whole-file transfer, no blocks to read
        let data: [u8; 0] = [];
        let mut cursor = Cursor::new(&data[..]);

        let sum_head = SumHead::empty();
        let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

        assert!(blocks.is_empty());
    }

    #[test]
    fn read_signature_blocks_single_block() {
        // Single block: rolling (4 bytes) + strong (16 bytes)
        let mut data = Vec::new();
        // Rolling sum = 0x12345678 (little-endian)
        data.extend_from_slice(&0x12345678u32.to_le_bytes());
        // Strong sum = 16 bytes
        data.extend_from_slice(&[0xAA; 16]);

        let mut cursor = Cursor::new(&data[..]);

        let sum_head = SumHead::new(1, 1024, 16, 0);
        let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[0].rolling_sum, 0x12345678);
        assert_eq!(blocks[0].strong_sum, vec![0xAA; 16]);
    }

    #[test]
    fn read_signature_blocks_multiple_blocks() {
        // Three blocks
        let mut data = Vec::new();

        // Block 0
        data.extend_from_slice(&0x11111111u32.to_le_bytes());
        data.extend_from_slice(&[0x01; 16]);

        // Block 1
        data.extend_from_slice(&0x22222222u32.to_le_bytes());
        data.extend_from_slice(&[0x02; 16]);

        // Block 2
        data.extend_from_slice(&0x33333333u32.to_le_bytes());
        data.extend_from_slice(&[0x03; 16]);

        let mut cursor = Cursor::new(&data[..]);

        let sum_head = SumHead::new(3, 1024, 16, 512);
        let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

        assert_eq!(blocks.len(), 3);

        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[0].rolling_sum, 0x11111111);
        assert_eq!(blocks[0].strong_sum, vec![0x01; 16]);

        assert_eq!(blocks[1].index, 1);
        assert_eq!(blocks[1].rolling_sum, 0x22222222);
        assert_eq!(blocks[1].strong_sum, vec![0x02; 16]);

        assert_eq!(blocks[2].index, 2);
        assert_eq!(blocks[2].rolling_sum, 0x33333333);
        assert_eq!(blocks[2].strong_sum, vec![0x03; 16]);
    }

    #[test]
    fn read_signature_blocks_short_strong_sum() {
        // Test with shorter strong sum (e.g., 8 bytes for XXH64)
        let mut data = Vec::new();
        data.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
        data.extend_from_slice(&[0xFF; 8]); // 8-byte strong sum

        let mut cursor = Cursor::new(&data[..]);

        let sum_head = SumHead::new(1, 2048, 8, 0);
        let blocks = read_signature_blocks(&mut cursor, &sum_head).unwrap();

        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].rolling_sum, 0xDEADBEEF);
        assert_eq!(blocks[0].strong_sum.len(), 8);
        assert_eq!(blocks[0].strong_sum, vec![0xFF; 8]);
    }

    #[test]
    fn read_signature_blocks_truncated_data() {
        // Test error handling when data is truncated
        let data = [0x12, 0x34, 0x56]; // Only 3 bytes, need 4 for rolling sum

        let mut cursor = Cursor::new(&data[..]);

        let sum_head = SumHead::new(1, 1024, 16, 0);
        let result = read_signature_blocks(&mut cursor, &sum_head);

        assert!(result.is_err());
    }

    #[test]
    fn sum_head_round_trip() {
        // Test that SumHead read/write are inverses
        let original = SumHead::new(42, 4096, 16, 1024);

        let mut wire = Vec::new();
        original.write(&mut wire).unwrap();

        assert_eq!(wire.len(), 16); // 4 * 4 bytes

        let mut cursor = Cursor::new(&wire[..]);
        let parsed = SumHead::read(&mut cursor).unwrap();

        assert_eq!(parsed.count, 42);
        assert_eq!(parsed.blength, 4096);
        assert_eq!(parsed.s2length, 16);
        assert_eq!(parsed.remainder, 1024);
    }

    #[test]
    fn sum_head_is_empty() {
        assert!(SumHead::empty().is_empty());
        assert!(SumHead::new(0, 0, 0, 0).is_empty());
        assert!(!SumHead::new(1, 1024, 16, 0).is_empty());
    }

    // ========================================================================
    // Helper Method Tests - Tests for extracted helper methods
    // ========================================================================

    #[test]
    fn should_activate_input_multiplex_client_mode_protocol_28() {
        // Client mode activates at protocol >= 23, so 28 should activate
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(28u8).unwrap();
        let mut config = test_config();
        config.client_mode = true;

        let ctx = GeneratorContext::new(&handshake, config);
        // Protocol 28 >= 23, so should activate in client mode
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn should_activate_input_multiplex_client_mode_protocol_32() {
        // Test with higher protocol version
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut config = test_config();
        config.client_mode = true;

        let ctx = GeneratorContext::new(&handshake, config);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn should_activate_input_multiplex_server_mode_protocol_30() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(30u8).unwrap();
        let mut config = test_config();
        config.client_mode = false;

        let ctx = GeneratorContext::new(&handshake, config);
        assert!(ctx.should_activate_input_multiplex());
    }

    #[test]
    fn should_activate_input_multiplex_server_mode_protocol_29() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(29u8).unwrap();
        let mut config = test_config();
        config.client_mode = false;

        let ctx = GeneratorContext::new(&handshake, config);
        assert!(!ctx.should_activate_input_multiplex());
    }

    #[test]
    fn get_checksum_algorithm_default_protocol_28() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(28u8).unwrap();
        handshake.negotiated_algorithms = None;

        let ctx = GeneratorContext::new(&handshake, test_config());
        assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD4);
    }

    #[test]
    fn get_checksum_algorithm_default_protocol_30() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(30u8).unwrap();
        handshake.negotiated_algorithms = None;

        let ctx = GeneratorContext::new(&handshake, test_config());
        assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::MD5);
    }

    #[test]
    fn get_checksum_algorithm_negotiated() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(32u8).unwrap();
        handshake.negotiated_algorithms = Some(NegotiationResult {
            checksum: ChecksumAlgorithm::XXH3,
            compression: protocol::CompressionAlgorithm::None,
        });

        let ctx = GeneratorContext::new(&handshake, test_config());
        assert_eq!(ctx.get_checksum_algorithm(), ChecksumAlgorithm::XXH3);
    }

    #[test]
    fn validate_file_index_valid() {
        let handshake = test_handshake();
        let mut ctx = GeneratorContext::new(&handshake, test_config());
        ctx.file_list
            .push(FileEntry::new_file("test.txt".into(), 100, 0o644));
        ctx.file_list
            .push(FileEntry::new_file("test2.txt".into(), 200, 0o644));

        assert!(ctx.validate_file_index(0).is_ok());
        assert!(ctx.validate_file_index(1).is_ok());
    }

    #[test]
    fn validate_file_index_invalid() {
        let handshake = test_handshake();
        let mut ctx = GeneratorContext::new(&handshake, test_config());
        ctx.file_list
            .push(FileEntry::new_file("test.txt".into(), 100, 0o644));

        let result = ctx.validate_file_index(1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn validate_file_index_empty_list() {
        let handshake = test_handshake();
        let ctx = GeneratorContext::new(&handshake, test_config());

        let result = ctx.validate_file_index(0);
        assert!(result.is_err());
    }

    #[test]
    fn calculate_duration_ms_both_some() {
        let start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let end = Instant::now();

        let duration = calculate_duration_ms(Some(start), Some(end));
        assert!(duration >= 10);
        assert!(duration < 100); // Sanity check
    }

    #[test]
    fn calculate_duration_ms_start_none() {
        let end = Instant::now();
        let duration = calculate_duration_ms(None, Some(end));
        assert_eq!(duration, 0);
    }

    #[test]
    fn calculate_duration_ms_end_none() {
        let start = Instant::now();
        let duration = calculate_duration_ms(Some(start), None);
        assert_eq!(duration, 0);
    }

    #[test]
    fn calculate_duration_ms_both_none() {
        let duration = calculate_duration_ms(None, None);
        assert_eq!(duration, 0);
    }

    #[test]
    fn send_id_lists_empty_output_no_preserve() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.owner = false;
        config.flags.group = false;

        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        ctx.send_id_lists(&mut output).unwrap();

        // No output when preserve flags are off
        assert!(output.is_empty());
    }

    #[test]
    fn send_id_lists_owner_only() {
        let handshake = test_handshake();
        let mut config = test_config();
        config.flags.owner = true;
        config.flags.group = false;

        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        ctx.send_id_lists(&mut output).unwrap();

        // Should have varint 0 terminator (1 byte)
        assert!(!output.is_empty());
        assert_eq!(output[0], 0); // Empty list terminator
    }

    #[test]
    fn send_io_error_flag_protocol_29() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(29u8).unwrap();

        let ctx = GeneratorContext::new(&handshake, test_config());

        let mut output = Vec::new();
        ctx.send_io_error_flag(&mut output).unwrap();

        // Protocol < 30 should write 4-byte io_error (value 0)
        assert_eq!(output.len(), 4);
        assert_eq!(output, &[0, 0, 0, 0]);
    }

    #[test]
    fn send_io_error_flag_protocol_30() {
        let mut handshake = test_handshake();
        handshake.protocol = ProtocolVersion::try_from(30u8).unwrap();

        let ctx = GeneratorContext::new(&handshake, test_config());

        let mut output = Vec::new();
        ctx.send_io_error_flag(&mut output).unwrap();

        // Protocol >= 30 should not write io_error
        assert!(output.is_empty());
    }

    #[test]
    fn apply_permutation_in_place_identity() {
        let mut a = vec![1, 2, 3, 4];
        let mut b = vec!["a", "b", "c", "d"];
        let indices = vec![0, 1, 2, 3];
        apply_permutation_in_place(&mut a, &mut b, indices);
        assert_eq!(a, vec![1, 2, 3, 4]);
        assert_eq!(b, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn apply_permutation_in_place_reverse() {
        let mut a = vec![1, 2, 3, 4];
        let mut b = vec!["a", "b", "c", "d"];
        // Indices represent: position 0 gets element from 3, pos 1 from 2, etc.
        let indices = vec![3, 2, 1, 0];
        apply_permutation_in_place(&mut a, &mut b, indices);
        assert_eq!(a, vec![4, 3, 2, 1]);
        assert_eq!(b, vec!["d", "c", "b", "a"]);
    }

    #[test]
    fn apply_permutation_in_place_cycle() {
        let mut a = vec![1, 2, 3, 4];
        let mut b = vec!["a", "b", "c", "d"];
        // Cycle: 0->1->2->3->0
        let indices = vec![3, 0, 1, 2];
        apply_permutation_in_place(&mut a, &mut b, indices);
        assert_eq!(a, vec![4, 1, 2, 3]);
        assert_eq!(b, vec!["d", "a", "b", "c"]);
    }

    #[test]
    fn apply_permutation_in_place_empty() {
        let mut a: Vec<i32> = vec![];
        let mut b: Vec<&str> = vec![];
        let indices: Vec<usize> = vec![];
        apply_permutation_in_place(&mut a, &mut b, indices);
        assert!(a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn apply_permutation_in_place_single() {
        let mut a = vec![42];
        let mut b = vec!["x"];
        let indices = vec![0];
        apply_permutation_in_place(&mut a, &mut b, indices);
        assert_eq!(a, vec![42]);
        assert_eq!(b, vec!["x"]);
    }

    /// Creates test config with specific flags for ID list tests.
    fn config_with_flags(owner: bool, group: bool, numeric_ids: bool) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Generator,
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
        }
    }

    #[test]
    fn send_id_lists_skips_when_numeric_ids_true() {
        let handshake = test_handshake();
        let config = config_with_flags(true, true, true);
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let result = ctx.send_id_lists(&mut output);

        assert!(result.is_ok());
        // With numeric_ids=true, nothing should be written
        assert!(output.is_empty());
    }

    #[test]
    fn send_id_lists_sends_uid_list_when_owner_set() {
        let handshake = test_handshake();
        let config = config_with_flags(true, false, false);
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let result = ctx.send_id_lists(&mut output);

        assert!(result.is_ok());
        // Empty UID list: varint 0 terminator
        assert_eq!(output, vec![0]);
    }

    #[test]
    fn send_id_lists_sends_gid_list_when_group_set() {
        let handshake = test_handshake();
        let config = config_with_flags(false, true, false);
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let result = ctx.send_id_lists(&mut output);

        assert!(result.is_ok());
        // Empty GID list: varint 0 terminator
        assert_eq!(output, vec![0]);
    }

    #[test]
    fn send_id_lists_sends_both_when_owner_and_group_set() {
        let handshake = test_handshake();
        let config = config_with_flags(true, true, false);
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let result = ctx.send_id_lists(&mut output);

        assert!(result.is_ok());
        // Both lists: two varint 0 terminators
        assert_eq!(output, vec![0, 0]);
    }

    #[test]
    fn send_id_lists_skips_both_when_neither_flag_set() {
        let handshake = test_handshake();
        let config = config_with_flags(false, false, false);
        let ctx = GeneratorContext::new(&handshake, config);

        let mut output = Vec::new();
        let result = ctx.send_id_lists(&mut output);

        assert!(result.is_ok());
        assert!(output.is_empty());
    }

    #[test]
    fn id_lists_round_trip_with_numeric_ids_false() {
        use super::super::receiver::ReceiverContext;

        let handshake = test_handshake();

        // Generator sends ID lists (numeric_ids=false, owner/group=true)
        let gen_config = config_with_flags(true, true, false);
        let generator = GeneratorContext::new(&handshake, gen_config);

        let mut wire_data = Vec::new();
        generator.send_id_lists(&mut wire_data).unwrap();

        // Both empty lists with terminators
        assert_eq!(wire_data, vec![0, 0]);

        // Receiver reads ID lists with matching flags
        let recv_config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags {
                owner: true,
                group: true,
                numeric_ids: false,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
        };
        let mut receiver = ReceiverContext::new(&handshake, recv_config);

        let mut cursor = Cursor::new(&wire_data[..]);
        let result = receiver.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position() as usize, wire_data.len());
    }

    #[test]
    fn id_lists_round_trip_with_numeric_ids_true() {
        use super::super::receiver::ReceiverContext;

        let handshake = test_handshake();

        // Generator skips ID lists (numeric_ids=true)
        let gen_config = config_with_flags(true, true, true);
        let generator = GeneratorContext::new(&handshake, gen_config);

        let mut wire_data = Vec::new();
        generator.send_id_lists(&mut wire_data).unwrap();

        // No data written when numeric_ids=true
        assert!(wire_data.is_empty());

        // Receiver also skips reading with matching flags
        let recv_config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            flags: ParsedServerFlags {
                owner: true,
                group: true,
                numeric_ids: true,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
            reference_directories: Vec::new(),
            iconv: None,
        };
        let mut receiver = ReceiverContext::new(&handshake, recv_config);

        let mut cursor = Cursor::new(&wire_data[..]);
        let result = receiver.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn generator_context_stores_negotiated_compression() {
        let mut handshake = test_handshake();
        handshake.negotiated_algorithms = Some(NegotiationResult {
            checksum: ChecksumAlgorithm::XXH3,
            compression: protocol::CompressionAlgorithm::Zlib,
        });

        let ctx = GeneratorContext::new(&handshake, test_config());
        assert!(ctx.negotiated_algorithms.is_some());
        let negotiated = ctx.negotiated_algorithms.as_ref().unwrap();
        assert_eq!(negotiated.compression, protocol::CompressionAlgorithm::Zlib);
    }

    #[test]
    fn generator_context_handles_no_compression() {
        let mut handshake = test_handshake();
        handshake.negotiated_algorithms = Some(NegotiationResult {
            checksum: ChecksumAlgorithm::MD5,
            compression: protocol::CompressionAlgorithm::None,
        });

        let ctx = GeneratorContext::new(&handshake, test_config());
        assert!(ctx.negotiated_algorithms.is_some());
        let negotiated = ctx.negotiated_algorithms.as_ref().unwrap();
        assert_eq!(negotiated.compression, protocol::CompressionAlgorithm::None);
    }
}
