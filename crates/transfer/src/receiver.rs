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
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;

use protocol::codec::{NdxCodec, create_ndx_codec};
use protocol::filters::read_filter_list;
use protocol::flist::{FileEntry, FileListReader, sort_file_list};
use protocol::wire::DeltaOp;
use protocol::{CompatibilityFlags, NegotiationResult, ProtocolVersion};

use super::delta_apply::{ChecksumVerifier, SparseWriteState};

use engine::delta::{DeltaScript, DeltaToken, SignatureLayoutParams, calculate_signature_layout};
use engine::fuzzy::FuzzyMatcher;
use engine::signature::{FileSignature, generate_file_signature};

use super::config::{ReferenceDirectory, ServerConfig};
use super::handshake::HandshakeResult;
use super::shared::ChecksumFactory;
use super::temp_guard::TempFileGuard;

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
}

impl ReceiverContext {
    /// Creates a new receiver context from handshake result and config.
    pub const fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            negotiated_algorithms: handshake.negotiated_algorithms,
            compat_flags: handshake.compat_flags,
            checksum_seed: handshake.checksum_seed,
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
        // Wire up preserve_uid/preserve_gid from server config flags.
        // This MUST match what the sender is sending - if sender uses -o/-g,
        // uid/gid values are included in the file list and we must consume them.
        .with_preserve_uid(self.config.flags.owner)
        .with_preserve_gid(self.config.flags.group);

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
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<()> {
        // Skip ID lists when numeric_ids is set (upstream: numeric_ids <= 0)
        if self.config.flags.numeric_ids {
            return Ok(());
        }

        if self.config.flags.owner {
            self.read_one_id_list(reader)?;
        }

        if self.config.flags.group {
            self.read_one_id_list(reader)?;
        }

        Ok(())
    }

    /// Reads a single ID list (uid or gid).
    ///
    /// Format: (varint id, byte name_len, name bytes)* followed by varint 0 terminator.
    /// If ID0_NAMES compat flag is set, also reads name for id=0 after terminator.
    fn read_one_id_list<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<()> {
        // Read (id, name) pairs until id=0
        loop {
            let id = protocol::read_varint(reader)?;
            if id == 0 {
                break;
            }
            // Read name: 1 byte length, then that many bytes
            let mut len_buf = [0u8; 1];
            reader.read_exact(&mut len_buf)?;
            let name_len = len_buf[0] as usize;
            if name_len > 0 {
                let mut name_buf = vec![0u8; name_len];
                reader.read_exact(&mut name_buf)?;
            }
        }

        // If ID0_NAMES flag is set, also read name for id=0
        // Note: This is only used with modern rsync (3.2.0+) and when preserve_uid/gid is set
        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));
        if id0_names {
            // recv_user_name(f, 0) or recv_group_name(f, 0)
            let mut len_buf = [0u8; 1];
            reader.read_exact(&mut len_buf)?;
            let name_len = len_buf[0] as usize;
            if name_len > 0 {
                let mut name_buf = vec![0u8; name_len];
                reader.read_exact(&mut name_buf)?;
            }
        }

        Ok(())
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

        // Activate compression on reader if negotiated (Protocol 30+ with compression algorithm)
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

        // Read filter list from sender if appropriate
        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        let reader = &mut reader; // Convert owned reader to mutable reference for rest of function

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
        let checksum_length = NonZeroU8::new(16).expect("checksum length must be non-zero");

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
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());

        for (file_idx, file_entry) in self.file_list.iter().enumerate() {
            let relative_path = file_entry.path();

            // Compute actual file path
            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };

            // Skip directories (already handled above)
            if file_entry.is_dir() {
                continue;
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
            writer.flush()?;

            // Step 1 & 2: Generate signature if basis file exists
            // Uses find_basis_file() helper to encapsulate exact match, reference directories, and fuzzy logic.
            let basis_result = find_basis_file(
                &file_path,
                &dest_dir,
                relative_path,
                file_entry.size(),
                self.config.flags.fuzzy,
                &self.config.reference_directories,
                self.protocol,
                checksum_length,
                checksum_algorithm,
            );
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
            // The sender echoes back: ndx, iflags, and optional fields
            let _sender_attrs = SenderAttrs::read(reader, self.protocol.as_u8())?;

            // Read sum_head echoed by sender (we don't use it, but must consume it)
            let _echoed_sum_head = SumHead::read(reader)?;

            // Step 5: Apply delta to reconstruct file
            let temp_path = file_path.with_extension("oc-rsync.tmp");
            let mut temp_guard = TempFileGuard::new(temp_path.clone());
            let mut output = fs::File::create(&temp_path)?;
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
                    // Compare only up to the minimum of computed and received lengths
                    // (some algorithms may have truncated checksums)
                    let cmp_len = std::cmp::min(computed.len(), file_checksum.len());
                    if computed[..cmp_len] != file_checksum[..cmp_len] {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "checksum verification failed for {:?}: expected {:02x?}, got {:02x?}",
                                file_path,
                                &file_checksum[..cmp_len],
                                &computed[..cmp_len]
                            ),
                        ));
                    }
                    break;
                } else if token > 0 {
                    // Literal data: token bytes follow
                    let mut data = vec![0u8; token as usize];
                    reader.read_exact(&mut data)?;
                    // Use sparse writing if enabled
                    if let Some(ref mut sparse) = sparse_state {
                        sparse.write(&mut output, &data)?;
                    } else {
                        output.write_all(&data)?;
                    }
                    // Update checksum with literal data
                    checksum_verifier.update(&data);
                    total_bytes += token as u64;
                } else {
                    // Negative: block reference = -(token+1)
                    // For new files (no basis), this shouldn't happen
                    let block_idx = -(token + 1) as usize;
                    if let (Some(sig), Some(basis_path)) = (&signature_opt, &basis_path_opt) {
                        // We have a basis file - copy the block
                        // Mirrors upstream receiver.c receive_data() block copy logic
                        let layout = sig.layout();
                        let block_len = layout.block_length().get() as u64;
                        let offset = block_idx as u64 * block_len;

                        // Calculate actual bytes to copy for this block
                        // Last block may be shorter (remainder)
                        let block_count = layout.block_count() as usize;
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

                        // Open basis file and seek to block offset
                        let mut basis_file = fs::File::open(basis_path).map_err(|e| {
                            io::Error::new(
                                e.kind(),
                                format!("failed to open basis file {basis_path:?}: {e}"),
                            )
                        })?;
                        basis_file.seek(SeekFrom::Start(offset))?;

                        // Read block data and write to output
                        let mut block_data = vec![0u8; bytes_to_copy];
                        basis_file.read_exact(&mut block_data)?;
                        // Use sparse writing if enabled
                        if let Some(ref mut sparse) = sparse_state {
                            sparse.write(&mut output, &block_data)?;
                        } else {
                            output.write_all(&block_data)?;
                        }

                        // Update checksum with copied data
                        checksum_verifier.update(&block_data);

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
                sparse.finish(&mut output)?;
            }

            // Sync the output file
            output.sync_all()?;

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

        // Handle goodbye handshake
        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            metadata_errors,
        })
    }
}

/// Statistics from a receiver transfer operation.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// Number of files in the received file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes received.
    pub bytes_received: u64,
    /// Metadata errors encountered (path, error message).
    pub metadata_errors: Vec<(PathBuf, String)>,
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

    /// Reads sender attributes from the wire.
    ///
    /// For protocol >= 29, reads iflags and optional trailing fields.
    /// For older protocols, returns default ITEM_TRANSFER flags.
    pub fn read<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Self> {
        // Read initial NDX byte (we already know the NDX, just consume it)
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
    /// Returns true if no basis file was found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.signature.is_none()
    }
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

/// Finds a basis file for delta transfer.
///
/// Search order:
/// 1. Exact file at destination path
/// 2. Reference directories (in order provided)
/// 3. Fuzzy matching in destination directory (if enabled)
///
/// # Arguments
///
/// * `file_path` - Target file path in destination
/// * `dest_dir` - Destination directory base
/// * `relative_path` - Relative path from destination root
/// * `target_size` - Expected size of the target file
/// * `fuzzy_enabled` - Whether to try fuzzy matching
/// * `reference_directories` - List of reference directories to check
/// * `protocol` - Protocol version for signature generation
/// * `checksum_length` - Checksum truncation length
/// * `checksum_algorithm` - Algorithm for strong checksums
///
/// # Upstream Reference
///
/// - `generator.c:1450` - Basis file selection in `recv_generator()`
/// - `generator.c:1580` - Fuzzy matching via `find_fuzzy_basis()`
/// - `generator.c:1400` - Reference directory checking
#[allow(clippy::too_many_arguments)]
pub fn find_basis_file(
    file_path: &std::path::Path,
    dest_dir: &std::path::Path,
    relative_path: &std::path::Path,
    target_size: u64,
    fuzzy_enabled: bool,
    reference_directories: &[ReferenceDirectory],
    protocol: ProtocolVersion,
    checksum_length: NonZeroU8,
    checksum_algorithm: engine::signature::SignatureAlgorithm,
) -> BasisFileResult {
    // Try to open the exact file first
    let (basis_file, basis_size, basis_path) = if let Ok(f) = fs::File::open(file_path) {
        let size = match f.metadata() {
            Ok(meta) => meta.len(),
            Err(_) => {
                return BasisFileResult {
                    signature: None,
                    basis_path: None,
                };
            }
        };
        (f, size, file_path.to_path_buf())
    } else {
        // Exact file not found - try reference directories first
        let ref_result = try_reference_directories(relative_path, reference_directories);
        if let Some((file, size, path)) = ref_result {
            (file, size, path)
        } else {
            // Reference directories didn't yield a basis - try fuzzy matching if enabled
            if !fuzzy_enabled {
                return BasisFileResult {
                    signature: None,
                    basis_path: None,
                };
            }

            // Use FuzzyMatcher to find a similar file in dest_dir
            let fuzzy_matcher = FuzzyMatcher::new();
            let Some(target_name) = relative_path.file_name() else {
                return BasisFileResult {
                    signature: None,
                    basis_path: None,
                };
            };

            let Some(fuzzy_match) =
                fuzzy_matcher.find_fuzzy_basis(target_name, dest_dir, target_size)
            else {
                return BasisFileResult {
                    signature: None,
                    basis_path: None,
                };
            };

            // Open the fuzzy-matched file as basis
            let fuzzy_path = fuzzy_match.path;
            let fuzzy_file = match fs::File::open(&fuzzy_path) {
                Ok(f) => f,
                Err(_) => {
                    return BasisFileResult {
                        signature: None,
                        basis_path: None,
                    };
                }
            };
            let fuzzy_size = match fuzzy_file.metadata() {
                Ok(meta) => meta.len(),
                Err(_) => {
                    return BasisFileResult {
                        signature: None,
                        basis_path: None,
                    };
                }
            };
            (fuzzy_file, fuzzy_size, fuzzy_path)
        }
    };

    // Calculate signature layout
    let params = SignatureLayoutParams::new(
        basis_size,
        None, // Use default block size heuristic
        protocol,
        checksum_length,
    );

    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => {
            return BasisFileResult {
                signature: None,
                basis_path: None,
            };
        }
    };

    // Generate signature
    match generate_file_signature(basis_file, layout, checksum_algorithm) {
        Ok(sig) => BasisFileResult {
            signature: Some(sig),
            basis_path: Some(basis_path),
        },
        Err(_) => BasisFileResult {
            signature: None,
            basis_path: None,
        },
    }
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

// Helper functions for delta transfer

/// Applies a delta script to create a new file (whole-file transfer, no basis).
///
/// All tokens must be Literal; Copy operations indicate a protocol error.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
            NonZeroU8::new(16).unwrap(),
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
        }
    }

    #[test]
    fn receive_id_lists_skips_when_numeric_ids_true() {
        let handshake = test_handshake();
        let config = config_with_flags(true, true, true);
        let ctx = ReceiverContext::new(&handshake, config);

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
        let ctx = ReceiverContext::new(&handshake, config);

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
        let ctx = ReceiverContext::new(&handshake, config);

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
        let ctx = ReceiverContext::new(&handshake, config);

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
        let ctx = ReceiverContext::new(&handshake, config);

        let data: &[u8] = &[];
        let mut cursor = Cursor::new(data);
        let result = ctx.receive_id_lists(&mut cursor);

        assert!(result.is_ok());
        assert_eq!(cursor.position(), 0);
    }
}
