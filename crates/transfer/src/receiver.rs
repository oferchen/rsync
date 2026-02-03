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

use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;

/// Default checksum length for delta verification (16 bytes = 128 bits).
///
/// This matches upstream rsync's default MD5 digest length and provides
/// sufficient collision resistance for file integrity verification.
const DEFAULT_CHECKSUM_LENGTH: NonZeroU8 = NonZeroU8::new(16).unwrap();

use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::filters::read_filter_list;
use protocol::flist::{FileEntry, FileListReader, IncrementalFileListBuilder, sort_file_list};
use protocol::idlist::IdList;
#[cfg(test)]
use protocol::wire::DeltaOp;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_by_name, lookup_user_by_name};

use super::adaptive_buffer::{adaptive_token_capacity, adaptive_writer_capacity};
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
use super::temp_guard::TempFileGuard;
use super::transfer_ops::{RequestConfig, ResponseContext, process_file_response, send_file_request};

use metadata::{MetadataOptions, apply_metadata_from_file_entry};

/// Context for the receiver role during a transfer.
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
    /// UID mappings from remote to local IDs.
    uid_list: IdList,
    /// GID mappings from remote to local IDs.
    gid_list: IdList,
}

impl ReceiverContext {
    /// Creates a new receiver context from handshake result and config.
    #[must_use]
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
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

    /// Returns the received file list.
    #[must_use]
    pub fn file_list(&self) -> &[FileEntry] {
        &self.file_list
    }

    /// Receives the file list from the sender.
    ///
    /// The file list is sent by the client in the rsync wire format with
    /// path compression and conditional fields based on flags.
    ///
    /// If the sender transmits an I/O error marker (SAFE_FILE_LIST mode), this
    /// method propagates the error up to the caller for handling. The caller should
    /// decide whether to continue or abort based on the error severity and context.
    ///
    /// After the file list entries, this also consumes the UID/GID lists that follow
    /// (unless using incremental recursion). See upstream `recv_id_list()` in uidlist.c.
    pub fn receive_file_list<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<usize> {
        let mut flist_reader = if let Some(flags) = self.compat_flags {
            FileListReader::with_compat_flags(self.protocol, flags)
        } else {
            FileListReader::new(self.protocol)
        }
        // Wire up preserve flags from server config.
        // These MUST match what the sender is sending - if sender uses flags like -o/-g/-l,
        // corresponding data is included in the file list and we must consume it.
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_acls(self.config.flags.acls)
        .with_preserve_xattrs(self.config.flags.xattrs);

        // Wire up iconv converter if configured
        if let Some(ref converter) = self.config.iconv {
            flist_reader = flist_reader.with_iconv(converter.clone());
        }
        let mut count = 0;

        // Read entries until end marker or error
        // If SAFE_FILE_LIST is enabled, sender may transmit I/O error marker
        while let Some(entry) = flist_reader.read_entry(reader)? {
            self.file_list.push(entry);
            count += 1;
        }

        // Read ID lists (UID/GID mappings) after file list
        // Upstream: recv_id_list() is called when !inc_recurse
        // See flist.c:2726-2727 and uidlist.c:460
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));
        if !inc_recurse {
            self.receive_id_lists(reader)?;
        }

        // Sort file list to match sender's sorted order.
        // Upstream: flist_sort_and_clean() is called after recv_id_list()
        // See flist.c:2736 - both sides must sort to ensure matching NDX indices.
        sort_file_list(&mut self.file_list);

        Ok(count)
    }

    /// Creates an incremental file list receiver for streaming processing.
    ///
    /// Instead of waiting for the complete file list before processing, this
    /// method returns an [`IncrementalFileListReceiver`] that yields entries
    /// as they arrive from the sender, with proper dependency tracking.
    ///
    /// # Benefits
    ///
    /// - Reduced startup latency: Transfers begin as soon as first entries arrive
    /// - Better memory efficiency: Don't need entire list in memory before starting
    /// - Improved progress feedback: Users see activity immediately
    ///
    /// # Dependency Tracking
    ///
    /// The incremental receiver tracks parent directory dependencies. Entries are
    /// only yielded when their parent directory has been processed, ensuring:
    ///
    /// 1. Directories are created before their contents
    /// 2. Nested directories are created in order
    /// 3. Files can be transferred immediately once their parent exists
    pub fn incremental_file_list_receiver<R: Read>(
        &self,
        reader: R,
    ) -> IncrementalFileListReceiver<R> {
        let mut flist_reader = if let Some(flags) = self.compat_flags {
            FileListReader::with_compat_flags(self.protocol, flags)
        } else {
            FileListReader::new(self.protocol)
        }
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group)
        .with_preserve_links(self.config.flags.links)
        .with_preserve_devices(self.config.flags.devices)
        .with_preserve_hard_links(self.config.flags.hard_links)
        .with_preserve_acls(self.config.flags.acls)
        .with_preserve_xattrs(self.config.flags.xattrs);

        if let Some(ref converter) = self.config.iconv {
            flist_reader = flist_reader.with_iconv(converter.clone());
        }

        // Build incremental processor with pre-existing destination directories
        let incremental = IncrementalFileListBuilder::new()
            .incremental_recursion(self.config.flags.incremental_recursion)
            .build();

        IncrementalFileListReceiver {
            flist_reader,
            source: reader,
            incremental,
            finished_reading: false,
            entries_read: 0,
        }
    }

    /// Reads UID/GID name-to-ID mapping lists from the sender.
    ///
    /// When `--numeric-ids` is not set, the sender transmits name mappings so the
    /// receiver can translate remote user/group names to local numeric IDs. When
    /// `--numeric-ids` is set, no mappings are sent and numeric IDs are used as-is.
    ///
    /// # Wire Format
    ///
    /// Each list contains `(varint id, byte name_len, name_bytes)*` tuples terminated
    /// by `varint 0`. With `ID0_NAMES` compat flag, an additional name for id=0
    /// follows the terminator.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(unix)]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        // Skip ID lists when numeric_ids is set (upstream: numeric_ids <= 0)
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // Read UID list if preserving ownership
        if self.config.flags.owner {
            self.uid_list
                .read(reader, id0_names, protocol_version, |name| {
                    lookup_user_by_name(name).ok().flatten()
                })?;
        }

        // Read GID list if preserving group
        if self.config.flags.group {
            self.gid_list
                .read(reader, id0_names, protocol_version, |name| {
                    lookup_group_by_name(name).ok().flatten()
                })?;
        }

        Ok(())
    }

    /// Reads UID/GID name-to-ID mapping lists from the sender (non-Unix platforms).
    ///
    /// On non-Unix platforms (e.g., Windows), this reads the ID lists from the wire
    /// but does not resolve user/group names to local IDs since the platform lacks
    /// the POSIX user database. All name lookups return `None`, causing ownership
    /// to fall back to numeric IDs.
    ///
    /// # Platform Behavior
    ///
    /// This matches upstream rsync behavior where platforms without user/group
    /// databases effectively operate as if `--numeric-ids` was specified.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(not(unix))]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        if self.config.flags.owner {
            self.uid_list
                .read(reader, id0_names, protocol_version, |_| None)?;
        }

        if self.config.flags.group {
            self.gid_list
                .read(reader, id0_names, protocol_version, |_| None)?;
        }

        Ok(())
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
        !self.config.client_mode && receiver_wants_list
    }

    /// Creates directories from the file list.
    ///
    /// Iterates through the file list and creates all directories first.
    /// Returns a list of metadata errors encountered (path, error message).
    fn create_directories(
        &self,
        dest_dir: &std::path::Path,
        metadata_opts: &MetadataOptions,
    ) -> io::Result<Vec<(PathBuf, String)>> {
        let mut metadata_errors = Vec::new();

        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                let relative_path = file_entry.path();
                let dir_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(relative_path)
                };
                if !dir_path.exists() {
                    fs::create_dir_all(&dir_path)?;
                }
                if let Err(meta_err) =
                    apply_metadata_from_file_entry(&dir_path, file_entry, metadata_opts)
                {
                    metadata_errors.push((dir_path.clone(), meta_err.to_string()));
                }
            }
        }

        Ok(metadata_errors)
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
                eprintln!(
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
                    eprintln!("failed to create directory {}: {}", dir_path.display(), e);
                }
                failed_dirs.mark_failed(entry.name());
                return Ok(false);
            }
        }

        // Apply metadata (non-fatal errors)
        if let Err(e) = apply_metadata_from_file_entry(&dir_path, entry, metadata_opts) {
            if self.config.flags.verbose && self.config.client_mode {
                eprintln!("warning: metadata error for {}: {}", dir_path.display(), e);
            }
            // Don't mark as failed - directory exists, just metadata issue
        }

        // Verbose output
        if self.config.flags.verbose && self.config.client_mode {
            if relative_path.as_os_str() == "." {
                eprintln!("./");
            } else {
                eprintln!("{}/", relative_path.display());
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
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };
        let mut phase: i32 = 0;

        loop {
            // Send NDX_DONE to signal end of current phase
            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;

            phase += 1;

            if phase > max_phase {
                break;
            }

            // Read echoed NDX_DONE from sender
            let ndx = ndx_read_codec.read_ndx(reader)?;
            if ndx != -1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "expected NDX_DONE (-1) from sender during phase transition, got {ndx}"
                    ),
                ));
            }
        }

        // Read final NDX_DONE from sender
        let final_ndx = ndx_read_codec.read_ndx(reader)?;
        if final_ndx != -1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected final NDX_DONE (-1) from sender, got {final_ndx}"),
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
        if self.protocol.as_u8() < 24 {
            return Ok(());
        }

        // Send goodbye NDX_DONE
        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        // For protocol >= 31, sender echoes NDX_DONE and expects another
        if self.protocol.as_u8() >= 31 {
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

        let (flist_buildtime_ms, flist_xfertime_ms) = if self.protocol.as_u8() >= 29 {
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
    /// eprintln!("Transferred {} files ({} bytes)",
    ///           stats.files_transferred, stats.bytes_received);
    /// # Ok(())
    /// # }
    /// ```
    pub fn run<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: super::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        // Use pipelined transfer by default for improved performance.
        // When incremental-flist feature is enabled, use incremental mode
        // which provides failed directory tracking and better error recovery.
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(reader, writer, PipelineConfig::default())
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            self.run_pipelined(reader, writer, PipelineConfig::default())
        }
    }

    /// Runs the receiver with synchronous (non-pipelined) transfer.
    ///
    /// This method is kept for compatibility and testing purposes.
    /// For production use, prefer the default `run()` which uses pipelining.
    pub fn run_sync<R: Read, W: Write + ?Sized>(
        &mut self,
        mut reader: super::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        // CRITICAL: Activate INPUT multiplex BEFORE reading filter list for protocol >= 30.
        // This matches upstream do_server_recv() at main.c:1167 which calls io_start_multiplex_in()
        // BEFORE calling recv_filter_list() at line 1171.
        // The client sends ALL data (including filter list) as multiplexed MSG_DATA frames for protocol >= 30.
        if self.protocol.as_u8() >= 30 {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        // NOTE: Compression is NOT applied at the stream level.
        // Upstream rsync uses token-level compression (send_deflated_token/recv_deflated_token)
        // only during the delta transfer phase. Filter list and file list are plain data.

        // Read filter list from sender if appropriate
        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        let reader = &mut reader; // Convert owned reader to mutable reference for rest of function

        // Print verbose message before receiving file list (mirrors upstream flist.c:2571-2572)
        // INFO_GTE(FLIST, 1) && !am_server - when verbose and acting as client
        if self.config.flags.verbose && self.config.client_mode {
            eprintln!("receiving incremental file list");
        }

        // Receive file list from sender
        let file_count = self.receive_file_list(reader)?;
        let _ = file_count; // Suppress unused warning (file list stored in self.file_list)

        // NOTE: Do NOT send NDX_DONE here!
        // The receiver/generator should immediately start sending file indices
        // for files it wants. NDX_DONE is sent at the END of the transfer phase.

        // Transfer loop: for each file, generate signature, receive delta, apply
        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // Select checksum algorithm using ChecksumFactory (handles negotiated vs default)
        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = DEFAULT_CHECKSUM_LENGTH;

        // Build metadata options from server config flags
        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        // Extract destination directory from config args
        // For receiver, args[0] is the destination path where files should be written
        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

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

        for (file_idx, file_entry) in self.file_list.iter().enumerate() {
            let relative_path = file_entry.path();

            // Compute actual file path
            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };

            // Skip non-regular files (directories, symlinks, devices, etc.)
            // Only regular files are transferred via delta transfer protocol.
            // Symlinks have their targets stored in the file list entry itself.
            // Devices/specials just need metadata, not content transfer.
            if !file_entry.is_file() {
                // Output directory name with trailing slash in verbose mode
                if file_entry.is_dir() && self.config.flags.verbose && self.config.client_mode {
                    if relative_path.as_os_str() == "." {
                        eprintln!("./");
                    } else {
                        eprintln!("{}/", relative_path.display());
                    }
                }
                continue;
            }

            // Output file name in verbose mode (mirrors upstream rsync.c:674)
            if self.config.flags.verbose && self.config.client_mode {
                eprintln!("{}", relative_path.display());
            }

            // Send file index using NDX encoding via NdxCodec Strategy pattern.
            // The codec handles protocol-version-aware encoding automatically.
            let ndx = file_idx as i32;
            ndx_write_codec.write_ndx(&mut *writer, ndx)?;

            // For protocol >= 29, sender expects iflags after NDX
            // ITEM_TRANSFER (0x8000) tells sender to read sum_head and send delta
            // See upstream read_ndx_and_attrs() in rsync.c:383
            if self.protocol.as_u8() >= 29 {
                const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
                writer.write_all(&ITEM_TRANSFER.to_le_bytes())?;
            }
            // Note: No flush here - we batch NDX+iflags with sum_head+signature
            // and flush once after sending the complete request.

            // Step 1 & 2: Generate signature if basis file exists
            // Uses find_basis_file_with_config() to encapsulate exact match, reference directories, and fuzzy logic.
            let basis_config = BasisFileConfig {
                file_path: &file_path,
                dest_dir: &dest_dir,
                relative_path,
                target_size: file_entry.size(),
                fuzzy_enabled: self.config.flags.fuzzy,
                reference_directories: &self.config.reference_directories,
                protocol: self.protocol,
                checksum_length,
                checksum_algorithm,
            };
            let basis_result = find_basis_file_with_config(&basis_config);
            let signature_opt = basis_result.signature;
            let basis_path_opt = basis_result.basis_path;

            // Step 3: Send sum_head (signature header) using SumHead struct
            // Upstream write_sum_head() sends: count, blength, s2length, remainder
            let sum_head = match signature_opt {
                Some(ref signature) => SumHead::from_signature(signature),
                None => SumHead::empty(),
            };
            sum_head.write(&mut *writer)?;

            // Write signature blocks if we have a basis file
            if let Some(ref signature) = signature_opt {
                write_signature_blocks(&mut *writer, signature, sum_head.s2length)?;
            }
            writer.flush()?;

            // Step 4: Read sender attributes using SenderAttrs helper
            // The sender echoes back: ndx, iflags, and optional fields.
            // Uses NdxCodec to properly decode variable-length NDX for protocol 30+.
            let (echoed_ndx, _sender_attrs) =
                SenderAttrs::read_with_codec(reader, &mut ndx_read_codec)?;

            // Verify the sender echoed back the correct file index
            debug_assert_eq!(
                echoed_ndx, ndx,
                "sender echoed NDX {echoed_ndx} but we requested {ndx}"
            );

            // Read sum_head echoed by sender (we don't use it, but must consume it)
            let _echoed_sum_head = SumHead::read(reader)?;

            // Step 5: Apply delta to reconstruct file
            let temp_path = file_path.with_extension("oc-rsync.tmp");
            let mut temp_guard = TempFileGuard::new(temp_path.clone());
            // Use BufWriter with adaptive capacity based on file size:
            // - Small files (< 64KB): 4KB buffer to avoid wasted memory
            // - Medium files (64KB - 1MB): 64KB buffer for balanced performance
            // - Large files (> 1MB): 256KB buffer to maximize throughput
            let target_size = file_entry.size();
            let file = fs::File::create(&temp_path)?;
            let writer_capacity = adaptive_writer_capacity(target_size);
            let mut output = std::io::BufWriter::with_capacity(writer_capacity, file);
            let mut total_bytes: u64 = 0;

            // Sparse file support: track zero runs to create holes
            // Mirrors upstream rsync's write_sparse() in fileio.c
            let use_sparse = self.config.flags.sparse;
            let mut sparse_state = if use_sparse {
                Some(SparseWriteState::default())
            } else {
                None
            };

            // Create checksum verifier for integrity verification
            // Mirrors upstream rsync's file checksum calculation during delta application
            let mut checksum_verifier = ChecksumVerifier::new(
                self.negotiated_algorithms.as_ref(),
                self.protocol,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );

            // Performance optimizations:
            // 1. MapFile: Cache basis file with 256KB sliding window to avoid
            //    repeated open/seek/read syscalls for each block reference.
            //    For a typical 16MB file with 700-byte blocks, this prevents ~23,000 syscalls.
            // 2. TokenBuffer: Adaptive initial capacity based on file size to
            //    avoid both memory waste and unnecessary reallocation.
            let mut basis_map = if let Some(ref path) = basis_path_opt {
                Some(MapFile::open(path).map_err(|e| {
                    io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
                })?)
            } else {
                None
            };
            let token_capacity = adaptive_token_capacity(target_size);
            let mut token_buffer = TokenBuffer::with_capacity(token_capacity);

            // Read tokens in a loop
            loop {
                let mut token_buf = [0u8; 4];
                reader.read_exact(&mut token_buf)?;
                let token = i32::from_le_bytes(token_buf);

                if token == 0 {
                    // End of file delta tokens
                    // Read file checksum from sender - upstream receiver.c:408
                    // The sender sends xfer_sum_len bytes after all delta tokens.
                    // Use digest_len() from ChecksumVerifier to get the correct length.
                    let checksum_len = checksum_verifier.digest_len();
                    let mut file_checksum = vec![0u8; checksum_len];
                    reader.read_exact(&mut file_checksum)?;

                    // Verify checksum matches computed hash
                    // Upstream receiver.c:440-457 - verification after delta application
                    let computed = checksum_verifier.finalize();
                    // Require exact length match for security - truncated checksums are invalid
                    if computed.len() != file_checksum.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "checksum length mismatch for {:?}: expected {} bytes, got {} bytes",
                                file_path,
                                checksum_len,
                                computed.len()
                            ),
                        ));
                    }
                    if computed != file_checksum {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "checksum verification failed for {file_path:?}: expected {file_checksum:02x?}, got {computed:02x?}"
                            ),
                        ));
                    }
                    break;
                } else if token > 0 {
                    // Literal data: token bytes follow
                    // Reuse TokenBuffer to avoid per-token allocation
                    let len = token as usize;
                    token_buffer.resize_for(len);
                    reader.read_exact(token_buffer.as_mut_slice())?;
                    let data = token_buffer.as_slice();
                    // Use sparse writing if enabled
                    if let Some(ref mut sparse) = sparse_state {
                        sparse.write(&mut output, data)?;
                    } else {
                        output.write_all(data)?;
                    }
                    // Update checksum with literal data
                    checksum_verifier.update(data);
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
            fs::rename(&temp_path, &file_path)?;
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

        // Create separate NDX codec for reading (needs its own state for delta decoding)
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // Exchange phase transitions with sender
        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // Receive transfer statistics from sender
        // The sender sends stats after the transfer loop but before goodbye.
        let _sender_stats = self.receive_stats(reader)?;

        // Handle goodbye handshake
        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // Calculate total source bytes from file list (mirrors upstream stats.total_size)
        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0, // Set by caller after run() via CountingWriter
            total_source_bytes,
            metadata_errors,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
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
        mut reader: super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        // CRITICAL: Activate INPUT multiplex BEFORE reading filter list for protocol >= 30.
        if self.protocol.as_u8() >= 30 {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        // Read filter list from sender if appropriate
        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        let reader = &mut reader;

        // Print verbose message before receiving file list
        if self.config.flags.verbose && self.config.client_mode {
            eprintln!("receiving incremental file list");
        }

        // Receive file list from sender
        let file_count = self.receive_file_list(reader)?;

        // Setup for transfer
        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

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
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        // First pass: create directories from file list
        let mut metadata_errors = self.create_directories(&dest_dir, &metadata_opts)?;

        // Setup codecs
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // Create request config
        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.as_u8() >= 29,
            checksum_length,
            checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.fsync,
        };

        // Initialize pipeline state
        let mut pipeline = PipelineState::new(pipeline_config);

        // Build list of files to transfer (filter out non-regular files)
        let files_to_transfer: Vec<(usize, &FileEntry)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_file())
            .collect();

        let mut file_iter = files_to_transfer.into_iter();
        let mut pending_files_info: Vec<(PathBuf, &FileEntry)> =
            Vec::with_capacity(pipeline.window_size());

        // Pipelined transfer loop
        loop {
            // Phase 1: Fill the pipeline with requests
            while pipeline.can_send() {
                if let Some((file_idx, file_entry)) = file_iter.next() {
                    let relative_path = file_entry.path();
                    let file_path = if relative_path.as_os_str() == "." {
                        dest_dir.clone()
                    } else {
                        dest_dir.join(relative_path)
                    };

                    // Verbose output
                    if self.config.flags.verbose && self.config.client_mode {
                        eprintln!("{}", relative_path.display());
                    }

                    // Find basis file and generate signature
                    let basis_config = BasisFileConfig {
                        file_path: &file_path,
                        dest_dir: &dest_dir,
                        relative_path,
                        target_size: file_entry.size(),
                        fuzzy_enabled: self.config.flags.fuzzy,
                        reference_directories: &self.config.reference_directories,
                        protocol: self.protocol,
                        checksum_length,
                        checksum_algorithm,
                    };
                    let basis_result = find_basis_file_with_config(&basis_config);

                    // Send request
                    let pending = send_file_request(
                        writer,
                        &mut ndx_write_codec,
                        file_idx as i32,
                        file_path.clone(),
                        basis_result.signature,
                        basis_result.basis_path,
                        file_entry.size(),
                        &request_config,
                    )?;

                    // Track pending transfer
                    pipeline.push(pending);
                    pending_files_info.push((file_path, file_entry));
                } else {
                    // No more files to request
                    break;
                }
            }

            // Phase 2: Process responses if pipeline has outstanding requests
            if pipeline.is_empty() {
                break; // Done with all transfers
            }

            // Process one response
            let pending = pipeline.pop().expect("pipeline not empty");
            let (file_path, file_entry) = pending_files_info.remove(0);

            let response_ctx = ResponseContext {
                config: &request_config,
            };

            let total_bytes = process_file_response(reader, &mut ndx_read_codec, pending, &response_ctx)?;

            // Apply metadata
            if let Err(meta_err) =
                apply_metadata_from_file_entry(&file_path, file_entry, &metadata_opts)
            {
                metadata_errors.push((file_path, meta_err.to_string()));
            }

            // Track stats
            bytes_received += total_bytes;
            files_transferred += 1;
        }

        // Print verbose directories that were skipped
        for file_entry in &self.file_list {
            if file_entry.is_dir() && self.config.flags.verbose && self.config.client_mode {
                let relative_path = file_entry.path();
                if relative_path.as_os_str() == "." {
                    eprintln!("./");
                } else {
                    eprintln!("{}/", relative_path.display());
                }
            }
        }

        // Exchange phase transitions with sender
        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // Receive transfer statistics from sender
        let _sender_stats = self.receive_stats(reader)?;

        // Handle goodbye handshake
        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        // Calculate total source bytes from file list
        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0,
            total_source_bytes,
            metadata_errors,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
        })
    }

    /// Runs the receiver with incremental directory creation and failed-dir tracking.
    ///
    /// Unlike [`run_pipelined`], this method creates directories incrementally
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
        mut reader: super::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        // Phase 1: Setup
        if self.protocol.as_u8() >= 30 {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        if self.config.flags.verbose && self.config.client_mode {
            eprintln!("receiving incremental file list");
        }

        // Phase 2: Receive file list
        let file_count = self.receive_file_list(&mut reader)?;

        // Statistics tracking
        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            ..Default::default()
        };

        // Setup checksum and metadata
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
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        // Phase 3: Incremental directory creation with failure tracking
        let mut failed_dirs = FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                if self.create_directory_incremental(
                    &dest_dir,
                    file_entry,
                    &metadata_opts,
                    &mut failed_dirs,
                )? {
                    stats.directories_created += 1;
                } else {
                    stats.directories_failed += 1;
                }
            }
        }

        // Phase 4: Build file transfer list, skipping children of failed dirs
        let mut files_to_transfer: Vec<(usize, &FileEntry)> = Vec::new();
        for (idx, entry) in self.file_list.iter().enumerate() {
            if entry.is_file() {
                if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
                    if self.config.flags.verbose && self.config.client_mode {
                        eprintln!(
                            "skipping {} (parent {} failed)",
                            entry.name(),
                            failed_parent
                        );
                    }
                    stats.files_skipped += 1;
                } else {
                    files_to_transfer.push((idx, entry));
                }
            }
        }

        // Setup codecs
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.as_u8() >= 29,
            checksum_length,
            checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.fsync,
        };

        // Phase 5: Pipelined file transfer
        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        let mut pending_files_info: Vec<(PathBuf, &FileEntry)> =
            Vec::with_capacity(pipeline.window_size());

        loop {
            // Fill pipeline
            while pipeline.can_send() {
                if let Some((file_idx, file_entry)) = file_iter.next() {
                    let relative_path = file_entry.path();
                    let file_path = if relative_path.as_os_str() == "." {
                        dest_dir.clone()
                    } else {
                        dest_dir.join(relative_path)
                    };

                    if self.config.flags.verbose && self.config.client_mode {
                        eprintln!("{}", relative_path.display());
                    }

                    let basis_config = BasisFileConfig {
                        file_path: &file_path,
                        dest_dir: &dest_dir,
                        relative_path,
                        target_size: file_entry.size(),
                        fuzzy_enabled: self.config.flags.fuzzy,
                        reference_directories: &self.config.reference_directories,
                        protocol: self.protocol,
                        checksum_length,
                        checksum_algorithm,
                    };
                    let basis_result = find_basis_file_with_config(&basis_config);

                    let pending = send_file_request(
                        writer,
                        &mut ndx_write_codec,
                        file_idx as i32,
                        file_path.clone(),
                        basis_result.signature,
                        basis_result.basis_path,
                        file_entry.size(),
                        &request_config,
                    )?;

                    pipeline.push(pending);
                    pending_files_info.push((file_path, file_entry));
                } else {
                    break;
                }
            }

            if pipeline.is_empty() {
                break;
            }

            // Process one response
            let pending = pipeline.pop().expect("pipeline not empty");
            let (file_path, file_entry) = pending_files_info.remove(0);

            let response_ctx = ResponseContext {
                config: &request_config,
            };

            let total_bytes =
                process_file_response(&mut reader, &mut ndx_read_codec, pending, &response_ctx)?;

            if let Err(meta_err) =
                apply_metadata_from_file_entry(&file_path, file_entry, &metadata_opts)
            {
                metadata_errors.push((file_path, meta_err.to_string()));
            }

            stats.bytes_received += total_bytes;
            stats.files_transferred += 1;
        }

        // Phase 6: Finalization
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();
        stats.metadata_errors = metadata_errors;

        self.exchange_phase_done(&mut reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;
        let _sender_stats = self.receive_stats(&mut reader)?;
        self.handle_goodbye(&mut reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(stats)
    }
}

// ============================================================================
// Incremental File List Receiver
// ============================================================================

/// Streaming receiver for incremental file list processing.
///
/// This struct wraps a [`FileListReader`] and provides iterator-like access
/// to file entries as they become available from the wire. It handles parent
// ============================================================================
// Failed Directory Tracking
// ============================================================================

/// Tracks directories that failed to create.
///
/// Children of failed directories are skipped during incremental processing.
#[derive(Debug, Default)]
struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: std::collections::HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories.
    #[cfg(test)]
    fn count(&self) -> usize {
        self.paths.len()
    }
}

// ============================================================================
// Incremental File List Receiver
// ============================================================================

/// directory dependencies automatically, ensuring directories are yielded
/// before their contents.
///
/// # Benefits
///
/// - **Reduced latency**: Start processing as soon as first entries arrive
/// - **Lower memory**: Don't need full list in memory before starting
/// - **Better UX**: Users see progress immediately
///
/// # Dependency Tracking
///
/// Entries are only yielded when their parent directory has been processed.
/// If entries arrive out of order (child before parent), the child is held
/// until its parent arrives.
pub struct IncrementalFileListReceiver<R> {
    /// Wire format reader for file entries.
    flist_reader: FileListReader,
    /// Data source (network stream).
    source: R,
    /// Incremental processor tracking dependencies.
    incremental: protocol::flist::IncrementalFileList,
    /// Whether we've finished reading from the wire.
    finished_reading: bool,
    /// Number of entries read from the wire.
    entries_read: usize,
}

impl<R: Read> IncrementalFileListReceiver<R> {
    /// Returns the next entry that is ready for processing.
    ///
    /// An entry is "ready" when its parent directory has already been yielded.
    /// This method may need to read additional entries from the wire to find
    /// one whose parent is available.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(entry))` - An entry ready for processing
    /// - `Ok(None)` - No more entries (end of list reached and all processed)
    /// - `Err(e)` - An I/O or protocol error occurred
    pub fn next_ready(&mut self) -> io::Result<Option<FileEntry>> {
        // First check if we have any ready entries
        if let Some(entry) = self.incremental.pop() {
            return Ok(Some(entry));
        }

        // If we've finished reading, nothing more to yield
        if self.finished_reading {
            return Ok(None);
        }

        // Read entries until we get one that's ready or hit end of list
        loop {
            match self.flist_reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.entries_read += 1;
                    self.incremental.push(entry);

                    // Check if this or any other entry is now ready
                    if let Some(ready) = self.incremental.pop() {
                        return Ok(Some(ready));
                    }
                    // No entry ready yet, keep reading
                }
                None => {
                    // End of file list
                    self.finished_reading = true;
                    // Return any remaining ready entry
                    return Ok(self.incremental.pop());
                }
            }
        }
    }

    /// Drains all entries that are currently ready for processing.
    ///
    /// This is useful for batch processing multiple ready entries at once.
    /// Returns an empty vector if no entries are currently ready.
    pub fn drain_ready(&mut self) -> Vec<FileEntry> {
        self.incremental.drain_ready()
    }

    /// Returns the number of entries ready for immediate processing.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.incremental.ready_count()
    }

    /// Returns the number of entries waiting for their parent directory.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.incremental.pending_count()
    }

    /// Returns the total number of entries read from the wire.
    #[must_use]
    pub const fn entries_read(&self) -> usize {
        self.entries_read
    }

    /// Returns `true` if all entries have been read from the wire.
    #[must_use]
    pub const fn is_finished_reading(&self) -> bool {
        self.finished_reading
    }

    /// Returns `true` if there are no more entries to yield.
    ///
    /// This is `true` when reading is complete and all ready entries have been consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.finished_reading && self.incremental.is_empty()
    }

    /// Marks a directory as already created (for pre-existing destinations).
    ///
    /// Call this for destination directories that exist before the transfer.
    /// This allows child entries to become ready immediately.
    pub fn mark_directory_created(&mut self, path: &str) {
        self.incremental.mark_directory_created(path);
    }

    /// Attempts to read one entry from the wire without blocking on ready queue.
    ///
    /// Returns `Ok(true)` if an entry was read and added to the incremental
    /// processor, `Ok(false)` if at EOF or already finished reading.
    ///
    /// Unlike [`next_ready`], this method does not wait for an entry to become
    /// ready. It simply reads from the wire and adds to the dependency tracker.
    pub fn try_read_one(&mut self) -> io::Result<bool> {
        if self.finished_reading {
            return Ok(false);
        }

        match self.flist_reader.read_entry(&mut self.source)? {
            Some(entry) => {
                self.entries_read += 1;
                self.incremental.push(entry);
                Ok(true)
            }
            None => {
                self.finished_reading = true;
                Ok(false)
            }
        }
    }

    /// Marks reading as finished (for error recovery).
    pub fn mark_finished(&mut self) {
        self.finished_reading = true;
    }

    /// Reads all remaining entries and returns them as a sorted vector.
    ///
    /// This method consumes the receiver and returns entries suitable for
    /// traditional batch processing. Use this when you need the complete
    /// sorted list for NDX indexing.
    ///
    /// # Note
    ///
    /// This method provides a fallback to traditional batch processing.
    /// For truly incremental processing, use [`next_ready`] instead.
    pub fn collect_sorted(mut self) -> io::Result<Vec<FileEntry>> {
        let mut entries = Vec::new();

        // Drain any already-ready entries
        entries.extend(self.incremental.drain_ready());

        // Read remaining entries
        while !self.finished_reading {
            match self.flist_reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.entries_read += 1;
                    entries.push(entry);
                }
                None => {
                    self.finished_reading = true;
                }
            }
        }

        // Drain any pending entries (they may have become orphans)
        entries.extend(self.incremental.drain_ready());

        // Sort to match sender's order for NDX indexing
        sort_file_list(&mut entries);

        Ok(entries)
    }

    /// Returns the file list statistics from the reader.
    #[must_use]
    pub const fn stats(&self) -> &protocol::flist::FileListStats {
        self.flist_reader.stats()
    }
}

impl<R: Read> Iterator for IncrementalFileListReceiver<R> {
    type Item = io::Result<FileEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_ready() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// Statistics from a receiver transfer operation.
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

    // Incremental mode statistics
    /// Total entries received from wire (incremental mode).
    pub entries_received: u64,
    /// Directories successfully created (incremental mode).
    pub directories_created: u64,
    /// Directories that failed to create (incremental mode).
    pub directories_failed: u64,
    /// Files skipped due to failed parent directory (incremental mode).
    pub files_skipped: u64,
}

/// Statistics received from the sender after transfer completion.
///
/// The sender transmits these statistics after the transfer loop but before
/// the goodbye handshake.
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

// ============================================================================
// Signature Header (SumHead) - Encapsulates rsync's sum_head structure
// ============================================================================

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

// ============================================================================
// Sender Attributes - Encapsulates attributes echoed back by the sender
// ============================================================================

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
    pub fnamecmp_type: Option<u8>,
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
            Some(byte[0])
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
            Some(byte[0])
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

// ============================================================================
// Basis File Finder - Encapsulates exact match and fuzzy matching logic
// ============================================================================

/// Result of searching for a basis file.
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

/// Generates a signature for the given basis file.
fn generate_basis_signature(
    basis_file: fs::File,
    basis_size: u64,
    basis_path: PathBuf,
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
    checksum_algorithm: engine::signature::SignatureAlgorithm,
) -> BasisFileResult {
    let params = SignatureLayoutParams::new(basis_size, None, protocol, checksum_length);

    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return BasisFileResult::EMPTY,
    };

    match generate_file_signature(basis_file, layout, checksum_algorithm) {
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

    generate_basis_signature(
        file,
        size,
        path,
        config.protocol,
        config.checksum_length,
        config.checksum_algorithm,
    )
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
    for block in signature.blocks() {
        // Write rolling_sum as int32 LE
        writer.write_all(&(block.rolling().value() as i32).to_le_bytes())?;

        // Write strong_sum, truncated or padded to s2length
        let strong_bytes = block.strong();
        let mut sum_buf = vec![0u8; s2length as usize];
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
        let digest = verifier.finalize();

        // MD4 produces 16 bytes
        assert_eq!(digest.len(), 16);
    }

    #[test]
    fn checksum_verifier_md5_for_modern_protocol() {
        // Protocol >= 30 without negotiation defaults to MD5
        let protocol = ProtocolVersion::try_from(32u8).unwrap();
        let mut verifier = ChecksumVerifier::new(None, protocol, 12345, None);

        verifier.update(b"test data");
        let digest = verifier.finalize();

        // MD5 produces 16 bytes
        assert_eq!(digest.len(), 16);
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
        let digest = verifier.finalize();

        // XXH3 produces 8 bytes (64-bit)
        assert_eq!(digest.len(), 8);
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
        let digest = verifier.finalize();

        // SHA1 produces 20 bytes
        assert_eq!(digest.len(), 20);
    }

    #[test]
    fn checksum_verifier_incremental_update() {
        // Test that incremental updates produce same result as single update
        let protocol = ProtocolVersion::try_from(28u8).unwrap();

        let mut verifier1 = ChecksumVerifier::new(None, protocol, 0, None);
        verifier1.update(b"hello ");
        verifier1.update(b"world");
        let digest1 = verifier1.finalize();

        let mut verifier2 = ChecksumVerifier::new(None, protocol, 0, None);
        verifier2.update(b"hello world");
        let digest2 = verifier2.finalize();

        assert_eq!(digest1, digest2);
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

    // ============================================================================
    // SumHead tests
    // ============================================================================

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

    // ============================================================================
    // SenderAttrs tests
    // ============================================================================

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
        data.push(0x02); // fnamecmp_type

        let attrs = SenderAttrs::read(&mut Cursor::new(data), 29).unwrap();

        assert_eq!(attrs.iflags, 0x8800);
        assert_eq!(attrs.fnamecmp_type, Some(0x02));
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

    // ============================================================================
    // BasisFileResult tests
    // ============================================================================

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

    // ============================================================================
    // Delta apply edge case tests
    // ============================================================================

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
        let digest = verifier.finalize();

        // MD4 produces 16 bytes even for empty input
        assert_eq!(digest.len(), 16);
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

    // ============================================================================
    // IncrementalFileListReceiver tests
    // ============================================================================

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
            // New fields
            entries_received: 100,
            directories_created: 10,
            directories_failed: 2,
            files_skipped: 5,
        };

        assert_eq!(stats.entries_received, 100);
        assert_eq!(stats.directories_created, 10);
        assert_eq!(stats.directories_failed, 2);
        assert_eq!(stats.files_skipped, 5);
    }

    mod incremental_receiver_tests {
        use super::*;

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
            };

            // Should return false since already finished
            assert!(!receiver.try_read_one().unwrap());
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
            let _ = ctx.run_pipelined_incremental(reader, writer, PipelineConfig::default());
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
            assert_eq!(failed.failed_ancestor("foo/bar/baz/file.txt"), Some("foo/bar"));
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
            let mut stats = TransferStats::default();

            stats.entries_received = 100;
            stats.directories_created = 20;
            stats.directories_failed = 2;
            stats.files_skipped = 10;
            stats.files_transferred = 68;

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
            assert!(failed.failed_ancestor("level1/level2/level3/file.txt").is_some());
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
