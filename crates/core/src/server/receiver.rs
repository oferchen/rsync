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
use std::io::{self, Read, Write};
use std::num::NonZeroU8;
use std::path::PathBuf;

use checksums::strong::Md5Seed;
use protocol::filters::read_filter_list;
use protocol::flist::{FileEntry, FileListReader};
use protocol::wire::DeltaOp;
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

use engine::delta::{DeltaScript, DeltaToken, SignatureLayoutParams, calculate_signature_layout};
use engine::signature::{FileSignature, SignatureAlgorithm, generate_file_signature};

use super::config::ServerConfig;
use super::handshake::HandshakeResult;
use super::temp_guard::TempFileGuard;

use metadata::{MetadataOptions, apply_metadata_from_file_entry};

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
        let receiver_wants_list = self.config.flags.delete; // TODO: add prune_empty_dirs support
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

        // Select checksum algorithm: use negotiated algorithm if available,
        // otherwise fall back to protocol-based defaults (matches upstream rsync)
        let checksum_algorithm = if let Some(ref negotiated) = self.negotiated_algorithms {
            // Use negotiated algorithm from Protocol 30+ capability negotiation
            checksum_algorithm_to_signature(
                negotiated.checksum,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            )
        } else if self.protocol.as_u8() >= 30 {
            // Protocol 30+ default: MD5 (when negotiation was skipped)
            // Legacy seed ordering when no compat flags exchanged
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::legacy(self.checksum_seed),
            }
        } else {
            // Protocol < 30: MD4 (historical)
            SignatureAlgorithm::Md4
        };
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
        // NDX encoding (write_ndx/read_ndx in io.c):
        // - 0x00 = NDX_DONE (-1)
        // - 0xFF = negative number prefix (other negative values)
        // - 0xFE = extended encoding (large deltas)
        // - Other bytes: delta from previous positive index
        //   (e.g., for sequential files: prev=-1, send 1 for idx 0, then 1 for each next)
        let mut prev_positive_ndx: i32 = -1; // Track last positive index sent

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

            // Send file index using NDX delta encoding
            // Delta = current_index - prev_positive_ndx
            let ndx = file_idx as i32;
            let delta = ndx - prev_positive_ndx;
            prev_positive_ndx = ndx;

            // For small deltas (1-253), send as single byte
            // For larger deltas, would need 0xFE prefix + 2 or 4 bytes
            if (1..=253).contains(&delta) {
                writer.write_all(&[delta as u8])?;
            } else if delta == 0 {
                // Zero delta: 0xFE + 2-byte value
                writer.write_all(&[0xFE, 0x00, 0x00])?;
            } else {
                // For larger values, use extended encoding
                // 0xFE + 2-byte delta (or 4-byte if high bit set)
                let delta_u16 = delta as u16;
                writer.write_all(&[0xFE])?;
                writer.write_all(&delta_u16.to_le_bytes())?;
            }

            // For protocol >= 29, sender expects iflags after NDX
            // ITEM_TRANSFER (0x8000) tells sender to read sum_head and send delta
            // See upstream read_ndx_and_attrs() in rsync.c:383
            if self.protocol.as_u8() >= 29 {
                const ITEM_TRANSFER: u16 = 1 << 15; // 0x8000
                writer.write_all(&ITEM_TRANSFER.to_le_bytes())?;
            }
            writer.flush()?;

            // Step 1 & 2: Generate signature if basis file exists
            let signature_opt: Option<FileSignature> = 'sig: {
                let basis_file = match fs::File::open(&file_path) {
                    Ok(f) => f,
                    Err(_) => break 'sig None,
                };

                let file_size = match basis_file.metadata() {
                    Ok(meta) => meta.len(),
                    Err(_) => break 'sig None,
                };

                let params = SignatureLayoutParams::new(
                    file_size,
                    None, // Use default block size heuristic
                    self.protocol,
                    checksum_length,
                );

                let layout = match calculate_signature_layout(params) {
                    Ok(layout) => layout,
                    Err(_) => break 'sig None,
                };

                generate_file_signature(basis_file, layout, checksum_algorithm).ok()
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
                    // TODO: Optionally verify checksum matches computed hash
                    break;
                } else if token > 0 {
                    // Literal data: token bytes follow
                    let mut data = vec![0u8; token as usize];
                    reader.read_exact(&mut data)?;
                    output.write_all(&data)?;
                    total_bytes += token as u64;
                } else {
                    // Negative: block reference = -(token+1)
                    // For new files (no basis), this shouldn't happen
                    let block_idx = -(token + 1) as usize;
                    if let Some(ref sig) = signature_opt {
                        // We have a basis file - copy the block
                        let layout = sig.layout();
                        let block_len = layout.block_length().get() as u64;
                        let _offset = block_idx as u64 * block_len;
                        // Read from basis file at offset and write to output
                        // For now, return error as we don't have basis file mapped
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            format!("block copy not yet implemented (block {block_idx})"),
                        ));
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("block reference {block_idx} without basis file"),
                        ));
                    }
                }
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

        // Send NDX_DONE (wire value 0x00) to signal end of transfer
        // This tells the sender we don't want any more files
        writer.write_all(&[0x00])?;
        writer.flush()?;

        // Report metadata errors summary if any occurred
        // (metadata_errors tracking remains for potential logging)

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
}
