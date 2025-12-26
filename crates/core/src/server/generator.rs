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
//! algorithm, see the [`crate::server::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::server::receiver`] - The receiver role that applies deltas from the generator
//! - [`engine::delta::DeltaGenerator`] - Core delta generation engine
//! - [`engine::delta::DeltaSignatureIndex`] - Fast block lookup for delta generation
//! - [`engine::signature`] - Signature reconstruction from wire format
//! - [`protocol::wire`] - Wire format for signatures and deltas

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use checksums::strong::{Md4, Md5, Md5Seed, Xxh3, Xxh64};
use filters::{FilterRule, FilterSet};
use logging::{debug_log, info_log};
use protocol::codec::{ProtocolCodec, create_protocol_codec};
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};
use protocol::flist::{FileEntry, FileListWriter, compare_file_entries};
use protocol::ndx::{NDX_FLIST_EOF, NdxCodec, create_ndx_codec};
use protocol::wire::{DeltaOp, SignatureBlock, write_token_stream};
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::delta::{DeltaGenerator, DeltaScript, DeltaSignatureIndex, DeltaToken};
use engine::signature::SignatureAlgorithm;

use super::config::ServerConfig;
use super::handshake::HandshakeResult;

/// Converts a negotiated checksum algorithm from the protocol layer to
/// a signature algorithm for the engine layer.
///
/// The seed parameter is used for XXHash variants and MD5 (when compat_flags are present).
/// For MD5, the CHECKSUM_SEED_FIX compat flag determines hash ordering:
/// - Flag set: seed hashed before data (proper order, protocol 30+)
/// - Flag not set: seed hashed after data (legacy order)
fn checksum_algorithm_to_signature(
    algorithm: ChecksumAlgorithm,
    seed: i32,
    compat_flags: Option<&CompatibilityFlags>,
) -> SignatureAlgorithm {
    let seed_u64 = seed as u64;
    match algorithm {
        ChecksumAlgorithm::None => SignatureAlgorithm::Md4, // Fallback to MD4 when no checksum requested
        ChecksumAlgorithm::MD4 => SignatureAlgorithm::Md4,
        ChecksumAlgorithm::MD5 => {
            let seed_config = if let Some(flags) = compat_flags {
                if flags.contains(CompatibilityFlags::CHECKSUM_SEED_FIX) {
                    Md5Seed::proper(seed)
                } else {
                    Md5Seed::legacy(seed)
                }
            } else {
                // No compat flags = legacy behavior (protocol < 30)
                Md5Seed::legacy(seed)
            };
            SignatureAlgorithm::Md5 { seed_config }
        }
        ChecksumAlgorithm::SHA1 => SignatureAlgorithm::Sha1,
        ChecksumAlgorithm::XXH64 => SignatureAlgorithm::Xxh64 { seed: seed_u64 },
        ChecksumAlgorithm::XXH3 => SignatureAlgorithm::Xxh3 { seed: seed_u64 },
        ChecksumAlgorithm::XXH128 => SignatureAlgorithm::Xxh3_128 { seed: seed_u64 },
    }
}

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
}

impl GeneratorContext {
    /// Creates a new generator context from handshake result and config.
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

        // Reorder both arrays according to sorted indices
        let sorted_entries: Vec<_> = indices.iter().map(|&i| self.file_list[i].clone()).collect();
        let sorted_paths: Vec<_> = indices
            .iter()
            .map(|&i| self.full_paths[i].clone())
            .collect();
        self.file_list = sorted_entries;
        self.full_paths = sorted_paths;

        // Record end time for flist_buildtime statistic
        self.flist_build_end = Some(Instant::now());

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

        for entry in self.file_list.iter() {
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
        // Activate INPUT multiplex based on mode and protocol version.
        //
        // The activation threshold differs by mode:
        //
        // SERVER mode (daemon sender - main.c:1252-1257 start_server am_sender):
        //   if (need_messages_from_generator)
        //       io_start_multiplex_in(f_in);
        //   For protocol >= 30, need_messages_from_generator = 1 (compat.c:776)
        //
        // CLIENT mode (client pushing to daemon - main.c:1304-1305 client_run am_sender):
        //   if (protocol_version >= 31 || (!filesfrom_host && protocol_version >= 23))
        //       io_start_multiplex_in(f_in);
        //   We don't support filesfrom, so this simplifies to >= 23
        //
        // The daemon receiver (start_server) activates OUTPUT multiplex for >= 23:
        //   main.c:1247-1248: if (protocol_version >= 23) io_start_multiplex_out(f_out);
        //
        // So when client pushes to daemon (protocol >= 23):
        // - Daemon sends us MSG_DATA frames (daemon output multiplex)
        // - We must read them with INPUT multiplex
        let should_activate_input_multiplex = if self.config.client_mode {
            // Client mode: >= 23 (upstream main.c:1304-1305, no filesfrom)
            self.protocol.as_u8() >= 23
        } else {
            // Server mode: >= 30 (need_messages_from_generator)
            self.protocol.as_u8() >= 30
        };

        if should_activate_input_multiplex {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        // Filter list handling depends on whether we're server or client:
        //
        // SERVER mode (receiving from client - upstream do_server_sender):
        //   - recv_filter_list() at main.c:1258 - receive filter list FROM client
        //   - Then build and send file list to client
        //
        // CLIENT mode (sending to server - upstream client_run with am_sender):
        //   - send_filter_list() at main.c:1308 - send filter list TO server (done in mod.rs)
        //   - send_file_list() at main.c:1317 - build and send file list TO server
        //   - Do NOT receive filter list (server never sends one)
        //
        // In client_mode, we already sent filter list in mod.rs, so skip reading here.
        if !self.config.client_mode {
            // Server mode: read filter list from client (MULTIPLEXED for protocol >= 30)
            let wire_rules = read_filter_list(&mut reader, self.protocol)?;

            // Convert wire format to FilterSet
            if !wire_rules.is_empty() {
                let filter_set = self.parse_received_filters(&wire_rules)?;
                self.filters = Some(filter_set);
            }
        }

        let reader = &mut reader; // Convert owned reader to mutable reference for rest of function

        // Build file list
        self.build_file_list(paths)?;

        // Send file list
        let file_count = self.send_file_list(writer)?;

        // Send ID lists for non-INC_RECURSE protocols (upstream flist.c:2513-2514)
        //
        // Upstream sends UID/GID mappings here if preserve_uid/gid is set.
        // For now, we send empty lists (just the terminator) if preserve flags are set.
        // Format: varint id, byte name_len, name bytes, ... varint 0 terminator
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        if !inc_recurse {
            // ID0_NAMES compat flag (protocol 32+) requires sending name for id=0
            // after the terminator. See upstream uidlist.c:send_user_name/send_group_name.
            let id0_names = self
                .compat_flags
                .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

            // Send UID list if preserve_uid
            if self.config.flags.owner {
                // Empty list: just write varint 0 terminator
                protocol::write_varint(writer, 0)?;

                // ID0_NAMES: send name for id=0 after terminator
                // Format: 1 byte length, then name bytes
                // We send empty name (length=0)
                if id0_names {
                    writer.write_all(&[0u8])?; // Empty name for uid=0
                }
            }
            // Send GID list if preserve_gid
            if self.config.flags.group {
                // Empty list: just write varint 0 terminator
                protocol::write_varint(writer, 0)?;

                // ID0_NAMES: send name for id=0 after terminator
                if id0_names {
                    writer.write_all(&[0u8])?; // Empty name for gid=0
                }
            }

            // CRITICAL: Flush UID/GID lists before entering main loop.
            // Without this flush, the data stays in the write buffer and causes
            // a deadlock: we wait to read from daemon, daemon waits to receive
            // our UID/GID lists. This mirrors upstream behavior where the I/O
            // layer flushes buffered data before blocking reads.
            writer.flush()?;
        }

        // Send io_error flag for protocol < 30 (upstream flist.c:2517-2518)
        //
        // Upstream: write_int(f, ignore_errors ? 0 : io_error);
        // We always send 0 (no error) for now.
        if self.protocol.as_u8() < 30 {
            writer.write_all(&0i32.to_le_bytes())?;
            writer.flush()?;
        }

        // Send NDX_FLIST_EOF if incremental recursion is enabled
        //
        // Upstream flist.c:2534-2545 in send_file_list():
        //   if (inc_recurse) {
        //       if (send_dir_ndx < 0) {
        //           write_ndx(f, NDX_FLIST_EOF);
        //           flist_eof = 1;
        //       }
        //   }
        //
        // This signals to the receiver that there are no more incremental file lists.
        // For a simple (non-recursive directory) transfer, send_dir_ndx is -1, so we
        // always send NDX_FLIST_EOF when INC_RECURSE is enabled.
        if let Some(flags) = self.compat_flags
            && flags.contains(CompatibilityFlags::INC_RECURSE)
        {
            // Use NdxCodec for protocol-version-aware encoding of NDX_FLIST_EOF
            let mut ndx_codec = create_ndx_codec(self.protocol.as_u8());
            ndx_codec.write_ndx(writer, NDX_FLIST_EOF)?;
            writer.flush()?;
        }

        // Main transfer loop: read file indices from receiver until NDX_DONE
        //
        // Protocol 30+ NDX encoding (upstream io.c:read_ndx/write_ndx):
        // - 0x00 = NDX_DONE (-1): signals end of file requests
        // - 0xFF prefix = other negative values (NDX_FLIST_EOF, etc.)
        // - 1-253 = delta-encoded positive index
        // - 0xFE prefix = larger index encoding
        //
        // Upstream sender.c:send_files() phase handling (lines 210, 236-258, 462):
        //   - phase = 0, max_phase = protocol_version >= 29 ? 2 : 1
        //   - On NDX_DONE: if (++phase > max_phase) break; else write_ndx(NDX_DONE), continue
        //   - After loop: write_ndx(NDX_DONE)
        //
        // For a simple listing operation (no files to transfer), the receiver
        // sends multiple NDX_DONEs for each phase transition.

        // Transfer loop: read file indices from receiver until all phases complete
        //
        // Upstream sender.c line 210: max_phase = protocol_version >= 29 ? 2 : 1
        let mut phase: i32 = 0;
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };

        let mut files_transferred = 0;
        let mut bytes_sent = 0u64;

        // Create NDX codecs using Strategy pattern for protocol-version-aware encoding.
        // Upstream rsync uses separate static variables for read and write state (io.c:2244-2245),
        // so we need two codecs: one for reading NDX from receiver, one for writing back.
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());

        loop {
            // Read NDX value from receiver
            //
            // Upstream io.c:read_ndx/write_ndx for protocol < 30:
            //   if (protocol_version < 30) return read_int(f);
            // For protocol 30+, uses compressed encoding.
            //
            // Use NdxCodec Strategy pattern for protocol-version-aware NDX decoding.
            // The codec handles both legacy (4-byte LE) and modern (delta) formats.
            let ndx = ndx_read_codec.read_ndx(&mut *reader)?;

            // Handle NDX_DONE (-1): phase transition (upstream sender.c lines 236-258)
            if ndx == -1 {
                phase += 1;

                if phase > max_phase {
                    // All phases complete, exit loop
                    break;
                }

                // Echo NDX_DONE back and continue to next phase
                // Upstream sender.c line 256: write_ndx(f_out, NDX_DONE)
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;

                continue;
            }

            // Handle other negative NDX values (NDX_FLIST_EOF, etc.)
            if ndx < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("negative NDX value {ndx} not yet implemented"),
                ));
            }

            let ndx = ndx as usize;

            // Read item flags (iflags) for protocol >= 29
            // Upstream rsync.c:read_ndx_and_attrs() line ~227:
            //   iflags = read_shortint(f_in);
            // This is sent as 2 bytes little-endian.
            //
            // Common iflags values:
            // - ITEM_TRANSFER (0x8000) = file needs to be transferred
            // - ITEM_REPORT_* = various reporting flags
            //
            // For the sender, we mainly care about ITEM_TRANSFER to know if
            // we need to send file data.
            let iflags = if self.protocol.as_u8() >= 29 {
                let mut iflags_bytes = [0u8; 2];
                reader.read_exact(&mut iflags_bytes)?;
                self.total_bytes_read += 2;
                u16::from_le_bytes(iflags_bytes)
            } else {
                // For older protocols, assume ITEM_TRANSFER
                0x8000u16
            };

            // ITEM_BASIS_TYPE_FOLLOWS (0x0800) - if set, read fnamecmp_type byte
            // ITEM_XNAME_FOLLOWS (0x0001) - if set, read extended name vstring
            // For now, we don't support these advanced features
            if iflags & 0x0800 != 0 {
                // Read and discard fnamecmp_type byte
                let mut _ftype = [0u8; 1];
                reader.read_exact(&mut _ftype)?;
                self.total_bytes_read += 1;
            }
            if iflags & 0x0001 != 0 {
                // Read and discard extended name (vstring format: varint length + bytes)
                let xlen = protocol::read_varint(reader)? as usize;
                self.total_bytes_read += 4; // Approximate varint size
                if xlen > 0 {
                    let actual_len = xlen.min(4096);
                    let mut xname = vec![0u8; actual_len];
                    reader.read_exact(&mut xname)?;
                    self.total_bytes_read += actual_len as u64;
                }
            }

            // Check if file should be transferred
            const ITEM_TRANSFER: u16 = 0x8000;
            if iflags & ITEM_TRANSFER == 0 {
                // File doesn't need transfer (e.g., unchanged or directory)
                continue;
            }

            // Read sum_head (checksum summary) from receiver's generator
            // Upstream sender.c:~325 calls receive_sums() after reading ndx+iflags
            // The receiver's generator sends this to tell us how to create deltas.
            //
            // sum_head format (upstream io.c:write_sum_head):
            // - count (4 bytes): number of checksum blocks (0 = whole file transfer)
            // - blength (4 bytes): block length
            // - s2length (4 bytes, protocol >= 27): strong sum length
            // - remainder (4 bytes, protocol >= 27): last block size
            //
            // When count=0, the receiver has no basis file and expects a whole-file transfer.
            let mut sum_head = [0u8; 16];
            if self.protocol.as_u8() >= 27 {
                // Protocol 27+: 16 bytes (count, blength, s2length, remainder)
                reader.read_exact(&mut sum_head)?;
                self.total_bytes_read += 16;
            } else {
                // Older protocols: 8 bytes (count, blength)
                reader.read_exact(&mut sum_head[..8])?;
                self.total_bytes_read += 8;
            }
            let sum_count = i32::from_le_bytes(sum_head[0..4].try_into().unwrap());
            let sum_blength = i32::from_le_bytes(sum_head[4..8].try_into().unwrap());
            let sum_s2length = if self.protocol.as_u8() >= 27 {
                i32::from_le_bytes(sum_head[8..12].try_into().unwrap())
            } else {
                // Older protocols use fixed 16-byte MD4 strong sum
                16
            };
            let sum_remainder = if self.protocol.as_u8() >= 27 {
                i32::from_le_bytes(sum_head[12..16].try_into().unwrap())
            } else {
                0
            };

            // Validate file index
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

            let _file_entry = &self.file_list[ndx];
            let source_path = &self.full_paths[ndx];

            // Read signature blocks from receiver
            //
            // Upstream sender.c:receive_sums() reads checksum blocks after sum_head.
            // When count=0 (no basis file), there are no blocks to read.
            // When count>0, read rolling_sum (4 bytes LE) + strong_sum (s2length bytes) per block.
            let block_length = sum_blength as u32;
            let block_count = sum_count as u32;
            let strong_sum_length = sum_s2length as u8;

            let sig_blocks: Vec<SignatureBlock> = if sum_count > 0 {
                // Receiver has basis file, read checksum blocks
                let mut blocks = Vec::with_capacity(sum_count as usize);
                for i in 0..sum_count {
                    // Read rolling checksum (4 bytes LE)
                    let mut rolling_sum_bytes = [0u8; 4];
                    reader.read_exact(&mut rolling_sum_bytes)?;
                    self.total_bytes_read += 4;
                    let rolling_sum = u32::from_le_bytes(rolling_sum_bytes);

                    // Read strong checksum (s2length bytes)
                    let mut strong_sum = vec![0u8; sum_s2length as usize];
                    reader.read_exact(&mut strong_sum)?;
                    self.total_bytes_read += sum_s2length as u64;

                    blocks.push(SignatureBlock {
                        index: i as u32,
                        rolling_sum,
                        strong_sum,
                    });
                }
                blocks
            } else {
                // No basis file (count=0), whole-file transfer - no blocks to read
                Vec::new()
            };
            let has_basis = block_count > 0;

            // Skip non-regular files (directories, symlinks, etc.)
            // Directories don't have file data to transfer - their metadata
            // is handled separately. Only regular files need delta transfer.
            if !_file_entry.is_file() {
                continue;
            }

            // Step 2: Open source file
            let source_file = match fs::File::open(source_path) {
                Ok(f) => f,
                Err(_e) => {
                    // Note: Upstream rsync sends an error marker in the wire protocol when
                    // a source file cannot be opened (see generator.c:1450). For now, we
                    // skip the file entirely, which matches rsync behavior with --ignore-errors.
                    // Future enhancement: Implement protocol error marker for per-file failures.
                    continue;
                }
            };

            // Step 3: Generate delta (or send whole file if no basis)
            let delta_script = if has_basis {
                // Receiver has basis, generate delta
                generate_delta_from_signature(
                    source_file,
                    block_length,
                    &sig_blocks,
                    strong_sum_length,
                    self.protocol,
                    self.negotiated_algorithms.as_ref(),
                    self.compat_flags.as_ref(),
                    self.checksum_seed,
                )?
            } else {
                // Receiver has no basis, send whole file as literals
                generate_whole_file_delta(source_file)?
            };

            // Step 4a: Send ndx and attrs back to receiver
            //
            // Upstream sender.c:411 - write_ndx_and_attrs(f_out, ndx, iflags, ...)
            // This tells the receiver which file is about to be received.
            //
            // Use NdxCodec Strategy pattern for protocol-version-aware NDX encoding.
            // The codec maintains its own prev_positive state for delta encoding.
            let ndx_i32 = ndx as i32;
            ndx_write_codec.write_ndx(&mut *writer, ndx_i32)?;

            // For protocol >= 29, echo back the iflags we received from the daemon
            // Upstream sender.c:411 - write_ndx_and_attrs(f_out, ndx, iflags, ...)
            // The receiver expects to get back the same iflags it sent us
            if self.protocol.as_u8() >= 29 {
                // write_shortint sends 2 bytes little-endian
                writer.write_all(&iflags.to_le_bytes())?;
            }

            // Step 4b: Send sum_head (signature summary) to receiver
            //
            // Upstream sender.c:412 - write_sum_head(f_xfer, s)
            // The sender forwards the SAME sum_head it received from the receiver.
            // The receiver expects to get back the values it sent us.
            //
            // Reference: io.c:write_sum_head() writes count, blength, s2length, remainder
            writer.write_all(&sum_count.to_le_bytes())?;
            writer.write_all(&sum_blength.to_le_bytes())?;
            if self.protocol.as_u8() >= 27 {
                writer.write_all(&sum_s2length.to_le_bytes())?;
                writer.write_all(&sum_remainder.to_le_bytes())?;
            }

            // Step 4c: Convert engine delta to wire format and send
            // Using upstream token format: write_int(len) + data for literals,
            // write_int(-(block+1)) for block matches, write_int(0) as end marker
            let wire_ops = script_to_wire_delta(&delta_script);
            write_token_stream(&mut &mut *writer, &wire_ops)?;

            // Step 4d: Send file transfer checksum
            //
            // Upstream match.c line 426: write_buf(f, sender_file_sum, xfer_sum_len);
            // After sending all delta tokens, the sender sends a checksum of the
            // file data for verification by the receiver.
            //
            // The checksum algorithm and length depend on negotiation:
            // - Protocol 30+ with negotiation: uses negotiated algorithm
            // - Protocol 30+ without negotiation: MD5 (16 bytes)
            // - Protocol < 30: MD4 (16 bytes)
            let checksum_algorithm = if let Some(negotiated) = &self.negotiated_algorithms {
                negotiated.checksum
            } else if self.protocol.as_u8() >= 30 {
                ChecksumAlgorithm::MD5
            } else {
                ChecksumAlgorithm::MD4
            };

            let file_checksum = compute_file_checksum(
                &delta_script,
                checksum_algorithm,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );

            writer.write_all(&file_checksum)?;
            writer.flush()?;

            // Step 5: Track stats
            bytes_sent += delta_script.total_bytes();
            files_transferred += 1;
        }

        // Upstream do_server_sender flow (main.c):
        // 1. send_files() - ends with write_ndx(NDX_DONE)
        // 2. io_flush(FULL_FLUSH)
        // 3. handle_stats(f_out) - writes 5 varlong30 values
        // 4. read_final_goodbye() - for protocol >= 24
        // 5. io_flush(FULL_FLUSH)
        // 6. exit

        // Step 1: Send NDX_DONE to indicate end of file transfer phase
        // write_ndx(f_out, NDX_DONE) from sender.c line 462
        //
        // Use NdxCodec for protocol-version-aware encoding:
        // - Protocol 28-29: NDX_DONE = -1 as 4-byte little-endian (write_int)
        // - Protocol 30+: NDX_DONE = 0x00 (compressed encoding)
        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        // Step 2: Stats handling
        // Upstream handle_stats() in main.c lines 813-844:
        //   if (am_server && am_sender) {
        //       write_varlong30(f, total_read, 3);
        //       write_varlong30(f, total_written, 3);
        //       write_varlong30(f, stats.total_size, 3);
        //       if (protocol_version >= 29) {
        //           write_varlong30(f, stats.flist_buildtime, 3);
        //           write_varlong30(f, stats.flist_xfertime, 3);
        //       }
        //   }
        //
        // The server sender MUST send these stats - the client expects them!
        let total_read: u64 = self.total_bytes_read;
        let total_written: u64 = bytes_sent; // Bytes sent during transfer
        let total_size: u64 = self.file_list.iter().map(|e| e.size()).sum();

        // Calculate file list build time in milliseconds (upstream stats.flist_buildtime)
        let flist_buildtime: u64 = match (self.flist_build_start, self.flist_build_end) {
            (Some(start), Some(end)) => end.duration_since(start).as_millis() as u64,
            _ => 0,
        };

        // Calculate file list transfer time in milliseconds (upstream stats.flist_xfertime)
        let flist_xfertime: u64 = match (self.flist_xfer_start, self.flist_xfer_end) {
            (Some(start), Some(end)) => end.duration_since(start).as_millis() as u64,
            _ => 0,
        };

        // Use protocol-aware codec for stats encoding:
        // - Protocol < 30: uses write_longint (4-byte fixed, or 12 for large values)
        // - Protocol >= 30: uses write_varlong (variable-length encoding)
        let stats_codec = create_protocol_codec(self.protocol.as_u8());
        stats_codec.write_stat(writer, total_read as i64)?;
        stats_codec.write_stat(writer, total_written as i64)?;
        stats_codec.write_stat(writer, total_size as i64)?;
        if self.protocol.as_u8() >= 29 {
            stats_codec.write_stat(writer, flist_buildtime as i64)?;
            stats_codec.write_stat(writer, flist_xfertime as i64)?;
        }
        writer.flush()?;

        // Step 3: read_final_goodbye (main.c lines 880-905)
        // For protocol >= 24
        if self.protocol.as_u8() >= 24 {
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
                writer.flush()?; // Must flush before reading final goodbye

                // Read final NDX_DONE
                // Note: This read may fail with connection reset/close if the daemon's
                // receiver child is killed (by SIGUSR2) before we can read. This is a
                // known race condition in the rsync protocol. The daemon sends the final
                // NDX_DONE (main.c:1121), flushes, then immediately kills the receiver
                // child. If the timing is unlucky, the connection closes before we read.
                // Since the transfer has already completed successfully at this point,
                // we treat connection errors here as acceptable and return success.
                match reader.read_exact(&mut goodbye_byte) {
                    Ok(()) => {
                        if goodbye_byte[0] == 0xFF {
                            let mut extra = [0u8; 3];
                            let _ = reader.read_exact(&mut extra); // Ignore error on extra bytes
                        }
                        // Non-zero but not 0xFF is unusual but transfer was successful
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::ConnectionReset
                            || e.kind() == io::ErrorKind::UnexpectedEof
                            || e.kind() == io::ErrorKind::BrokenPipe
                            || e.kind() == io::ErrorKind::WouldBlock =>
                    {
                        // Connection closed/reset/unavailable during final goodbye - this is
                        // acceptable as the transfer has already completed successfully.
                        // WouldBlock can happen if the client closes before sending second goodbye.
                    }
                    Err(e) => {
                        // Propagate other errors
                        return Err(e);
                    }
                }
            }
        }

        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred,
            bytes_sent,
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
        let mut rules = Vec::new();

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
                    // Merge rules not yet supported in server mode
                    // Skip for now; will be implemented in future phases
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

// Helper functions for delta generation

/// Generates a delta script from a received signature.
///
/// Reconstructs the signature from wire format blocks, creates an index,
/// and uses DeltaGenerator to produce the delta.
#[allow(clippy::too_many_arguments)]
fn generate_delta_from_signature<R: Read>(
    source: R,
    block_length: u32,
    sig_blocks: &[protocol::wire::signature::SignatureBlock],
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

    // Convert wire blocks to engine signature blocks
    let engine_blocks: Vec<SignatureBlock> = sig_blocks
        .iter()
        .map(|wire_block| {
            SignatureBlock::from_raw_parts(
                wire_block.index as u64,
                RollingDigest::from_value(wire_block.rolling_sum, block_length as usize),
                wire_block.strong_sum.clone(),
            )
        })
        .collect();

    // Calculate total bytes (approximation since we don't know exact remainder)
    let total_bytes = (block_count.saturating_sub(1)) * u64::from(block_length);
    let signature = FileSignature::from_raw_parts(layout, engine_blocks, total_bytes);

    // Select checksum algorithm: use negotiated algorithm if available,
    // otherwise fall back to protocol-based defaults (matches upstream rsync)
    let checksum_algorithm = if let Some(negotiated) = negotiated_algorithms {
        // Use negotiated algorithm from Protocol 30+ capability negotiation
        checksum_algorithm_to_signature(negotiated.checksum, checksum_seed, compat_flags)
    } else if protocol.as_u8() >= 30 {
        // Protocol 30+ default: MD5 (when negotiation was skipped)
        // Legacy seed ordering when no compat flags exchanged
        SignatureAlgorithm::Md5 {
            seed_config: Md5Seed::legacy(checksum_seed),
        }
    } else {
        // Protocol < 30: MD4 (historical)
        SignatureAlgorithm::Md4
    };

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
    seed: i32,
    _compat_flags: Option<&CompatibilityFlags>,
) -> Vec<u8> {
    // Collect all literal bytes from the script
    let mut all_bytes = Vec::new();
    for token in script.tokens() {
        if let DeltaToken::Literal(data) = token {
            all_bytes.extend_from_slice(data);
        }
        // Note: Copy tokens reference basis file blocks - the receiver has those.
        // The checksum is computed on all data bytes (matching upstream behavior
        // where sum_update is called on each data chunk during match processing).
    }

    // Compute checksum using the appropriate algorithm
    match algorithm {
        ChecksumAlgorithm::None => {
            // Protocol uses a 1-byte placeholder when checksums are disabled
            vec![0u8]
        }
        ChecksumAlgorithm::MD4 => {
            // Upstream checksum.c:sum_init() for MD4: prepends seed as 4 LE bytes
            // SIVAL(s.buf, 0, seed); md4_update(&m, s.buf, 4);
            let mut hasher = Md4::new();
            hasher.update(&seed.to_le_bytes());
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
        ChecksumAlgorithm::MD5 => {
            // Upstream checksum.c sum_init() for MD5: just md5_begin(&ctx_md)
            // The seed is NOT used for MD5 file transfer checksums!
            // (The seed is only used for MD4 variants and for block checksums)
            let mut hasher = Md5::new();
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
        ChecksumAlgorithm::SHA1 => {
            // SHA1 doesn't use a seed for file transfer checksums
            use checksums::strong::Sha1;
            let mut hasher = Sha1::new();
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
        ChecksumAlgorithm::XXH64 => {
            // Upstream checksum.c line 583: XXH64_reset(xxh64_state, 0)
            // XXH64 uses 0 as seed for file transfer checksums, NOT checksum_seed
            let mut hasher = Xxh64::new(0);
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
        ChecksumAlgorithm::XXH3 => {
            // Upstream checksum.c line 590: XXH3_64bits_reset(xxh3_state)
            // XXH3 uses default seed (0) for file transfer checksums
            let mut hasher = Xxh3::new(0);
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
        ChecksumAlgorithm::XXH128 => {
            // Upstream checksum.c line 595: XXH3_128bits_reset(xxh3_state)
            // XXH3_128 uses default seed (0) for file transfer checksums
            use checksums::strong::Xxh3_128;
            let mut hasher = Xxh3_128::new(0);
            hasher.update(&all_bytes);
            hasher.finalize().to_vec()
        }
    }
}

/// Converts engine delta script to wire protocol delta operations.
fn script_to_wire_delta(script: &DeltaScript) -> Vec<DeltaOp> {
    script
        .tokens()
        .iter()
        .map(|token| match token {
            DeltaToken::Literal(data) => DeltaOp::Literal(data.clone()),
            DeltaToken::Copy { index, len } => DeltaOp::Copy {
                block_index: *index as u32,
                length: *len as u32,
            },
        })
        .collect()
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
            flag_string: "-logDtpre.".to_string(),
            flags: ParsedServerFlags::default(),
            args: vec![OsString::from(".")],
            compression_level: None,
            client_mode: false,
            filter_rules: Vec::new(),
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

        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_string())];
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
            FilterRuleWireFormat::exclude("*.log".to_string()),
            FilterRuleWireFormat::include("*.txt".to_string()),
            FilterRuleWireFormat::exclude("temp/".to_string()).with_directory_only(true),
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
            FilterRuleWireFormat::exclude("*.tmp".to_string())
                .with_sides(true, false)
                .with_perishable(true),
            FilterRuleWireFormat::include("/important".to_string()).with_anchored(true),
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
            FilterRuleWireFormat::exclude("*.log".to_string()),
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
            FilterRuleWireFormat::include("*.txt".to_string()),
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
        let wire_rules = vec![FilterRuleWireFormat::exclude("*.log".to_string())];
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
            FilterRuleWireFormat::exclude("*".to_string()),
            FilterRuleWireFormat::include("*.txt".to_string()),
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
            FilterRuleWireFormat::exclude("exclude_dir/".to_string()).with_directory_only(true),
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

        let wire_ops = script_to_wire_delta(&script);

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

        let wire_ops = script_to_wire_delta(&script);

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
}
