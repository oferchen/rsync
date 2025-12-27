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
//! algorithm, see the [`crate::server::delta_transfer`] module documentation.
//!
//! # Related Components
//!
//! - [`crate::server::generator`] - The generator role that sends deltas to the receiver
//! - [`engine::delta`] - Delta generation and application engine
//! - [`engine::signature`] - Signature generation utilities
//! - [`metadata::apply_metadata_from_file_entry`] - Metadata preservation
//! - [`protocol::wire`] - Wire format for signatures and deltas

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use protocol::filters::read_filter_list;

/// State tracker for sparse file writing.
///
/// Tracks pending runs of zeros that should become holes in the output file
/// rather than being written as data. Mirrors upstream rsync's write_sparse()
/// behavior in fileio.c.
#[derive(Default)]
struct SparseWriteState {
    /// Number of pending zero bytes to skip (becomes a hole).
    pending_zeros: u64,
}

impl SparseWriteState {
    /// Adds additional zero bytes to the pending run.
    fn accumulate(&mut self, additional: usize) {
        self.pending_zeros = self.pending_zeros.saturating_add(additional as u64);
    }

    /// Flushes pending zeros by seeking forward, creating a hole.
    fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
        }

        // Seek forward past the zeros to create a hole
        let mut remaining = self.pending_zeros;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer.seek(SeekFrom::Current(step as i64))?;
            remaining -= step;
        }

        self.pending_zeros = 0;
        Ok(())
    }

    /// Writes data with sparse optimization.
    ///
    /// Zero runs are tracked and become holes; non-zero data is written normally.
    fn write<W: Write + Seek>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut offset = 0;
        const CHUNK_SIZE: usize = 1024;

        while offset < data.len() {
            let segment_end = (offset + CHUNK_SIZE).min(data.len());
            let segment = &data[offset..segment_end];

            // Count leading zeros
            let leading = segment.iter().take_while(|&&b| b == 0).count();
            self.accumulate(leading);

            if leading == segment.len() {
                offset = segment_end;
                continue;
            }

            // Count trailing zeros
            let data_part = &segment[leading..];
            let trailing = data_part.iter().rev().take_while(|&&b| b == 0).count();
            let data_start = offset + leading;
            let data_end = segment_end - trailing;

            if data_end > data_start {
                // Flush pending zeros (creates hole)
                self.flush(writer)?;
                // Write non-zero data
                writer.write_all(&data[data_start..data_end])?;
            }

            // Trailing zeros become pending for next iteration
            self.pending_zeros = trailing as u64;
            offset = segment_end;
        }

        Ok(data.len())
    }

    /// Finalizes sparse writing and returns final position.
    ///
    /// If there are pending zeros at the end, we need to either:
    /// - Write a single zero byte at the last position (creating the hole), or
    /// - Just seek and let truncation handle it
    fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        if self.pending_zeros > 0 {
            // Seek to final position minus 1
            let final_offset = self.pending_zeros.saturating_sub(1);
            if final_offset > 0 {
                let mut remaining = final_offset;
                while remaining > 0 {
                    let step = remaining.min(i64::MAX as u64);
                    writer.seek(SeekFrom::Current(step as i64))?;
                    remaining -= step;
                }
            }
            // Write a single zero byte to extend the file to the correct size
            if self.pending_zeros > 0 {
                writer.write_all(&[0])?;
            }
            self.pending_zeros = 0;
        }

        writer.stream_position()
    }
}

/// Checksum verifier for delta transfer integrity verification.
///
/// This enum wraps the different strong checksum hashers to allow
/// runtime selection based on negotiated protocol parameters.
/// Mirrors upstream rsync's file checksum verification in receiver.c.
enum ChecksumVerifier {
    /// MD4 checksum (legacy, protocol < 30).
    Md4(Md4),
    /// MD5 checksum (protocol 30+).
    Md5(Md5),
    /// SHA1 checksum.
    Sha1(Sha1),
    /// XXH64 checksum.
    Xxh64(Xxh64),
    /// XXH3 (64-bit) checksum.
    Xxh3(Xxh3),
    /// XXH3 (128-bit) checksum.
    Xxh3_128(Xxh3_128),
}

impl ChecksumVerifier {
    /// Creates a new checksum verifier based on the negotiated algorithm.
    fn new(
        negotiated: Option<&NegotiationResult>,
        protocol: ProtocolVersion,
        _seed: i32,
        _compat_flags: Option<&CompatibilityFlags>,
    ) -> Self {
        if let Some(neg) = negotiated {
            match neg.checksum {
                ChecksumAlgorithm::None | ChecksumAlgorithm::MD4 => {
                    ChecksumVerifier::Md4(Md4::new())
                }
                ChecksumAlgorithm::MD5 => {
                    // Upstream checksum.c sum_init() for MD5: just md5_begin()
                    // The seed is NOT used for file transfer checksums.
                    // (Seed is only used for block checksums in get_checksum2)
                    ChecksumVerifier::Md5(Md5::new())
                }
                ChecksumAlgorithm::SHA1 => ChecksumVerifier::Sha1(Sha1::new()),
                // Upstream sum_init() uses 0 for XXH seeds, not checksum_seed
                ChecksumAlgorithm::XXH64 => ChecksumVerifier::Xxh64(Xxh64::with_seed(0)),
                ChecksumAlgorithm::XXH3 => ChecksumVerifier::Xxh3(Xxh3::with_seed(0)),
                ChecksumAlgorithm::XXH128 => ChecksumVerifier::Xxh3_128(Xxh3_128::with_seed(0)),
            }
        } else if protocol.as_u8() >= 30 {
            // Protocol 30+ default: MD5 (no seed for file transfer checksums)
            ChecksumVerifier::Md5(Md5::new())
        } else {
            // Protocol < 30: MD4
            ChecksumVerifier::Md4(Md4::new())
        }
    }

    /// Updates the hasher with data.
    fn update(&mut self, data: &[u8]) {
        match self {
            ChecksumVerifier::Md4(h) => h.update(data),
            ChecksumVerifier::Md5(h) => h.update(data),
            ChecksumVerifier::Sha1(h) => h.update(data),
            ChecksumVerifier::Xxh64(h) => h.update(data),
            ChecksumVerifier::Xxh3(h) => h.update(data),
            ChecksumVerifier::Xxh3_128(h) => h.update(data),
        }
    }

    /// Finalizes the hasher and returns the digest as bytes.
    fn finalize(self) -> Vec<u8> {
        match self {
            ChecksumVerifier::Md4(h) => h.finalize().as_ref().to_vec(),
            ChecksumVerifier::Md5(h) => h.finalize().as_ref().to_vec(),
            ChecksumVerifier::Sha1(h) => h.finalize().as_ref().to_vec(),
            ChecksumVerifier::Xxh64(h) => h.finalize().as_ref().to_vec(),
            ChecksumVerifier::Xxh3(h) => h.finalize().as_ref().to_vec(),
            ChecksumVerifier::Xxh3_128(h) => h.finalize().as_ref().to_vec(),
        }
    }
}
use protocol::codec::{NdxCodec, create_ndx_codec};
use protocol::flist::{FileEntry, FileListReader, sort_file_list};
use protocol::wire::DeltaOp;
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::delta::{DeltaScript, DeltaToken, SignatureLayoutParams, calculate_signature_layout};
use engine::fuzzy::FuzzyMatcher;
use engine::signature::{FileSignature, generate_file_signature};

use super::config::ServerConfig;
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
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
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

    /// Reads UID/GID lists from the sender.
    ///
    /// Mirrors upstream `recv_id_list()` in uidlist.c:460.
    /// Each list is varint-terminated: reads (id, name_len, name) tuples until id=0.
    ///
    /// IMPORTANT: Both sender and receiver must use the same flags for ID lists.
    /// The sender only sends ID lists if preserve_uid/gid is set on THEIR side,
    /// which should match the client's flags since the daemon parses the client's
    /// flag string.
    ///
    /// For now, we consume but don't store the mappings (no ownership preservation yet).
    fn receive_id_lists<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<()> {
        // Read UID list if preserve_uid is set (owner flag)
        // Upstream condition: (preserve_uid || preserve_acls) && numeric_ids <= 0
        // Note: numeric_ids is not implemented yet, assume false (0)
        if self.config.flags.owner {
            self.read_one_id_list(reader)?;
        }

        // Read GID list if preserve_gid is set (group flag)
        // Upstream condition: (preserve_gid || preserve_acls) && numeric_ids <= 0
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

        // Read filter list from sender (multiplexed for protocol >= 30)
        // This mirrors upstream recv_filter_list() at exclude.c:1672-1687.
        //
        // CRITICAL: The filter list is ONLY read when `receiver_wants_list` is true.
        // From upstream exclude.c:1680:
        //   if (!local_server && (am_sender || receiver_wants_list)) {
        //       while ((len = read_int(f_in)) != 0) { ... }
        //   }
        //
        // Where receiver_wants_list = prune_empty_dirs || (delete_mode && ...)
        //
        // For a daemon receiver (am_sender=0), the filter list is only read when:
        // - prune_empty_dirs is enabled, OR
        // - delete_mode is enabled
        //
        // If neither flag is set, NO filter list is sent by the client and we
        // must NOT try to read one, or we'll block waiting for data that never comes.
        //
        // In client mode (daemon client), skip reading filter list because:
        // - The client already sent filter list to the daemon
        // - The daemon (as generator) already consumed it
        // - There's no filter list coming back on this stream
        let receiver_wants_list = self.config.flags.delete || self.config.flags.prune_empty_dirs;
        if !self.config.client_mode && receiver_wants_list {
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
        let mut metadata_errors = Vec::new();

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
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        // First pass: create directories from file list
        // Directories don't go through delta transfer
        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                let relative_path = file_entry.path();
                let dir_path = if relative_path.as_os_str() == "." {
                    dest_dir.clone()
                } else {
                    dest_dir.join(relative_path)
                };
                if !dir_path.exists() {
                    fs::create_dir_all(&dir_path)?;
                }
                if let Err(meta_err) =
                    apply_metadata_from_file_entry(&dir_path, file_entry, metadata_opts.clone())
                {
                    metadata_errors.push((dir_path.to_path_buf(), meta_err.to_string()));
                }
            }
        }

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
            // If exact file not found and fuzzy matching is enabled, try to find
            // a similar file to use as basis for delta transfer.
            // We track the basis path for block copying during delta application.
            let (signature_opt, basis_path_opt): (Option<FileSignature>, Option<PathBuf>) = 'sig: {
                // Try to open the exact file first
                let (basis_file, basis_size, basis_path) = match fs::File::open(&file_path) {
                    Ok(f) => {
                        let size = match f.metadata() {
                            Ok(meta) => meta.len(),
                            Err(_) => break 'sig (None, None),
                        };
                        (f, size, file_path.clone())
                    }
                    Err(_) => {
                        // Exact file not found - try fuzzy matching if enabled
                        if !self.config.flags.fuzzy {
                            break 'sig (None, None);
                        }

                        // Use FuzzyMatcher to find a similar file in dest_dir
                        let fuzzy_matcher = FuzzyMatcher::new();
                        let target_name = match relative_path.file_name() {
                            Some(name) => name,
                            None => break 'sig (None, None),
                        };
                        let target_size = file_entry.size();

                        let fuzzy_match = match fuzzy_matcher.find_fuzzy_basis(
                            target_name,
                            &dest_dir,
                            target_size,
                        ) {
                            Some(m) => m,
                            None => break 'sig (None, None),
                        };

                        // Open the fuzzy-matched file as basis
                        let fuzzy_path = fuzzy_match.path.clone();
                        let fuzzy_file = match fs::File::open(&fuzzy_path) {
                            Ok(f) => f,
                            Err(_) => break 'sig (None, None),
                        };
                        let fuzzy_size = match fuzzy_file.metadata() {
                            Ok(meta) => meta.len(),
                            Err(_) => break 'sig (None, None),
                        };
                        (fuzzy_file, fuzzy_size, fuzzy_path)
                    }
                };

                let params = SignatureLayoutParams::new(
                    basis_size,
                    None, // Use default block size heuristic
                    self.protocol,
                    checksum_length,
                );

                let layout = match calculate_signature_layout(params) {
                    Ok(layout) => layout,
                    Err(_) => break 'sig (None, None),
                };

                match generate_file_signature(basis_file, layout, checksum_algorithm) {
                    Ok(sig) => (Some(sig), Some(basis_path)),
                    Err(_) => (None, None),
                }
            };

            // Step 3: Send sum_head (signature header) matching upstream wire format
            // upstream write_sum_head() sends: count, blength, s2length (proto>=27), remainder
            // All as 32-bit little-endian integers (write_int)
            if let Some(ref signature) = signature_opt {
                let sig_layout = signature.layout();
                let count = sig_layout.block_count() as u32;
                let blength = sig_layout.block_length().get();
                let s2length = sig_layout.strong_sum_length().get() as u32;
                let remainder = sig_layout.remainder();

                // Write sum_head: count, blength, s2length, remainder (all int32 LE)
                writer.write_all(&(count as i32).to_le_bytes())?;
                writer.write_all(&(blength as i32).to_le_bytes())?;
                writer.write_all(&(s2length as i32).to_le_bytes())?;
                writer.write_all(&(remainder as i32).to_le_bytes())?;

                // Write each block: rolling_sum (int32 LE) + strong_sum (s2length bytes)
                for block in signature.blocks() {
                    writer.write_all(&(block.rolling().value() as i32).to_le_bytes())?;
                    let strong_bytes = block.strong();
                    // Truncate or pad to s2length
                    let mut sum_buf = vec![0u8; s2length as usize];
                    let copy_len = std::cmp::min(strong_bytes.len(), s2length as usize);
                    sum_buf[..copy_len].copy_from_slice(&strong_bytes[..copy_len]);
                    writer.write_all(&sum_buf)?;
                }
            } else {
                // No basis, request whole file: send sum_head with count=0
                // count=0, blength=0, s2length=0, remainder=0
                writer.write_all(&0i32.to_le_bytes())?; // count
                writer.write_all(&0i32.to_le_bytes())?; // blength
                writer.write_all(&0i32.to_le_bytes())?; // s2length
                writer.write_all(&0i32.to_le_bytes())?; // remainder
            }
            writer.flush()?;

            // Step 4: Read ndx_and_attrs from sender
            // The sender echoes back: ndx (delta encoded), iflags (shortint for proto>=29)
            // Then possibly: fnamecmp_type, xname depending on flags
            // See upstream write_ndx_and_attrs() in sender.c:180

            // Read echoed NDX from sender (delta encoded)
            let mut ndx_byte = [0u8; 1];
            let n = reader.read(&mut ndx_byte)?;
            if n != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to read NDX byte",
                ));
            }

            // For protocol >= 29, read iflags (shortint = 2 bytes LE)
            let iflags = if self.protocol.as_u8() >= 29 {
                let mut iflags_buf = [0u8; 2];
                reader.read_exact(&mut iflags_buf)?;
                u16::from_le_bytes(iflags_buf)
            } else {
                0x8000 // ITEM_TRANSFER | ITEM_MISSING_DATA for older protocols
            };

            // Read optional fields based on iflags
            const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11; // 0x0800
            const ITEM_XNAME_FOLLOWS: u16 = 1 << 12; // 0x1000

            if iflags & ITEM_BASIS_TYPE_FOLLOWS != 0 {
                // Skip fnamecmp_type byte
                let mut fnamecmp_type = [0u8; 1];
                reader.read_exact(&mut fnamecmp_type)?;
            }

            if iflags & ITEM_XNAME_FOLLOWS != 0 {
                // Read vstring (xname): upstream io.c:1944-1960 read_vstring()
                // Format: first byte is length; if bit 7 set, length = (byte & 0x7F) * 256 + next_byte
                // Then read that many bytes of string data
                let mut len_byte = [0u8; 1];
                reader.read_exact(&mut len_byte)?;
                let xname_len = if len_byte[0] & 0x80 != 0 {
                    let mut second_byte = [0u8; 1];
                    reader.read_exact(&mut second_byte)?;
                    ((len_byte[0] & 0x7F) as usize) * 256 + second_byte[0] as usize
                } else {
                    len_byte[0] as usize
                };
                // Skip the xname string bytes
                if xname_len > 0 {
                    let mut xname_buf = vec![0u8; xname_len];
                    reader.read_exact(&mut xname_buf)?;
                }
            }

            // Read sum_head echoed by sender (16 bytes: count, blength, s2length, remainder)
            // We read but don't use these values since we already know the signature layout
            let mut sum_head_buf = [0u8; 16];
            reader.read_exact(&mut sum_head_buf)?;

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
                    // Length depends on negotiated checksum algorithm:
                    // - MD4/MD5/XXH128: 16 bytes
                    // - SHA1: 20 bytes
                    // - XXH64/XXH3: 8 bytes
                    let checksum_len = match &self.negotiated_algorithms {
                        Some(negotiated) => match negotiated.checksum {
                            ChecksumAlgorithm::SHA1 => 20,
                            ChecksumAlgorithm::XXH64 | ChecksumAlgorithm::XXH3 => 8,
                            _ => 16, // MD4, MD5, XXH128, None
                        },
                        None => 16, // Default to 16 for legacy protocols
                    };
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
                apply_metadata_from_file_entry(&file_path, file_entry, metadata_opts.clone())
            {
                // Collect error for final report - metadata failure shouldn't abort transfer
                metadata_errors.push((file_path.to_path_buf(), meta_err.to_string()));
            }

            // Step 7: Track stats
            bytes_received += total_bytes;
            files_transferred += 1;
        }

        // Phase handling: after sending all file requests, we need to exchange NDX_DONEs
        // with the sender for multi-phase protocol (protocol >= 29 has 2 phases).
        //
        // Upstream sender.c lines 236-258 and receiver.c lines 554-588:
        // - Phase 1: Normal file transfer (just completed above)
        // - Phase 2: Redo phase for files that failed verification
        // - After each phase, both sides exchange NDX_DONE
        //
        // Flow for each phase transition:
        // 1. We (receiver/generator) send NDX_DONE (no more files for this phase)
        // 2. Sender receives NDX_DONE, increments phase, echoes NDX_DONE
        // 3. We receive echoed NDX_DONE, increment phase
        // 4. If phase <= max_phase: send NDX_DONE for next phase, repeat
        // 5. If phase > max_phase: exit loop
        //
        // Sender also sends final NDX_DONE after loop (sender.c line 462)
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };
        let mut phase: i32 = 0;

        // Create separate NDX codec for reading (needs its own state for delta decoding)
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        loop {
            // Send NDX_DONE to signal end of current phase
            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;

            phase += 1;

            if phase > max_phase {
                // All phases complete, exit
                break;
            }

            // Read echoed NDX_DONE from sender
            // Upstream sender.c line 256: write_ndx(f_out, NDX_DONE)
            let ndx = ndx_read_codec.read_ndx(reader)?;
            if ndx != -1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "expected NDX_DONE (-1) from sender during phase transition, got {ndx}"
                    ),
                ));
            }

            // Continue to next phase (redo phase - no files to redo for now)
        }

        // Read final NDX_DONE from sender (sender.c line 462: write_ndx after loop)
        let final_ndx = ndx_read_codec.read_ndx(reader)?;
        if final_ndx != -1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected final NDX_DONE (-1) from sender, got {final_ndx}"),
            ));
        }

        // Goodbye exchange: After send_files() returns, sender calls read_final_goodbye()
        // which reads more NDX_DONEs from us.
        //
        // Upstream main.c read_final_goodbye() (lines 875-907):
        // - For protocol < 29: read_int() and expect NDX_DONE
        // - For protocol >= 29: read_ndx_and_attrs() and expect NDX_DONE
        // - For protocol >= 31: if NDX_DONE, echo NDX_DONE back, then read another NDX_DONE
        //
        // This comes from the generator side in do_recv() (main.c lines 1117-1121):
        // if (protocol_version >= 24) { write_ndx(f_out, NDX_DONE); }
        if self.protocol.as_u8() >= 24 {
            // Send goodbye NDX_DONE that sender reads in read_final_goodbye()
            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;

            // For protocol >= 31, sender echoes NDX_DONE and expects another
            if self.protocol.as_u8() >= 31 {
                // Read echoed NDX_DONE from sender
                let goodbye_echo = ndx_read_codec.read_ndx(reader)?;
                if goodbye_echo != -1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "expected goodbye NDX_DONE echo (-1) from sender, got {goodbye_echo}"
                        ),
                    ));
                }

                // Send final goodbye NDX_DONE
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
            }
        }

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
    use std::ffi::OsString;
    use std::io::Cursor;

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
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
            "Permission denied".to_string(),
        ));
        stats.metadata_errors.push((
            PathBuf::from("/tmp/file2.txt"),
            "Operation not permitted".to_string(),
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
        assert_eq!(sparse.pending_zeros, 100);

        sparse.accumulate(50);
        assert_eq!(sparse.pending_zeros, 150);
    }
}
