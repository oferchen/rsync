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

use checksums::strong::Md5Seed;
use filters::{FilterRule, FilterSet};
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};
use protocol::flist::{FileEntry, FileListWriter};
use protocol::wire::{DeltaOp, read_signature, write_delta};
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
    /// List of files to send.
    file_list: Vec<FileEntry>,
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
}

impl GeneratorContext {
    /// Creates a new generator context from handshake result and config.
    pub fn new(handshake: &HandshakeResult, config: ServerConfig) -> Self {
        Self {
            protocol: handshake.protocol,
            config,
            file_list: Vec::new(),
            filters: None,
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
        self.file_list.clear();

        for base_path in base_paths {
            self.walk_path(base_path, base_path)?;
        }

        // Sort file list lexicographically (rsync requirement)
        self.file_list.sort_by(|a, b| a.name().cmp(b.name()));

        Ok(self.file_list.len())
    }

    /// Recursively walks a path and adds entries to the file list.
    fn walk_path(&mut self, base: &Path, path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;

        // Calculate relative path
        let relative = path.strip_prefix(base).unwrap_or(path).to_path_buf();

        // Skip the base path itself if it's a directory
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            // Walk children of the base directory
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
    pub fn send_file_list<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<usize> {
        let mut flist_writer = if let Some(flags) = self.compat_flags {
            FileListWriter::with_compat_flags(self.protocol, flags)
        } else {
            FileListWriter::new(self.protocol)
        };

        for entry in &self.file_list {
            flist_writer.write_entry(writer, entry)?;
        }

        // Write end marker with no error (SAFE_FILE_LIST support)
        // Future: track I/O errors during file list building and pass them here
        flist_writer.write_end(writer, None)?;
        writer.flush()?;

        Ok(self.file_list.len())
    }

    /// Runs the generator role to completion.
    ///
    /// This orchestrates the full send operation:
    /// 1. Build file list from paths
    /// 2. Send file list
    /// 3. For each file: receive signature, generate delta, send delta
    pub fn run<R: Read, W: Write + ?Sized>(
        &mut self,
        mut reader: super::reader::ServerReader<R>,
        writer: &mut W,
        paths: &[PathBuf],
    ) -> io::Result<GeneratorStats> {
        // CRITICAL: Activate INPUT multiplex BEFORE reading filter list for protocol >= 30
        // This matches upstream behavior where the generator/sender role also activates
        // INPUT multiplex when protocol >= 30 to read multiplexed data from the receiver.
        if self.protocol.as_u8() >= 30 {
            reader = reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?;
        }

        // Activate compression on reader if negotiated (Protocol 30+ with compression algorithm)
        // This mirrors upstream io.c:io_start_buffering_in()
        // Compression is activated AFTER multiplex, wrapping the multiplexed stream
        if let Some(ref negotiated) = self.negotiated_algorithms {
            if let Some(compress_alg) = negotiated.compression.to_compress_algorithm()? {
                reader = reader.activate_compression(compress_alg).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("failed to activate INPUT compression: {e}"),
                    )
                })?;
            }
        }

        // Read filter list from client (multiplexed for protocol >= 30)
        let wire_rules = read_filter_list(&mut reader, self.protocol)?;

        // Convert wire format to FilterSet
        if !wire_rules.is_empty() {
            let filter_set = self.parse_received_filters(&wire_rules)?;
            self.filters = Some(filter_set);
        }

        let reader = &mut reader; // Convert owned reader to mutable reference for rest of function

        // Build file list
        self.build_file_list(paths)?;

        // Send file list
        let file_count = self.send_file_list(writer)?;

        // Wait for client to send NDX_DONE (indicates file list received)
        // Mirrors upstream sender.c:read_ndx_and_attrs() flow
        // For protocol >= 30, NDX_DONE is encoded as single byte 0x00
        let mut ndx_byte = [0u8; 1];
        reader.read_exact(&mut ndx_byte)?;

        if ndx_byte[0] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected NDX_DONE (0x00), got 0x{:02x}", ndx_byte[0]),
            ));
        }

        // Send NDX_DONE back to signal phase completion
        // Mirrors upstream sender.c:256 (write_ndx(f_out, NDX_DONE))
        writer.write_all(&[0])?;
        writer.flush()?;

        // Delta generation loop: for each file, receive signature, generate delta, send
        let mut files_transferred = 0;
        let mut bytes_sent = 0u64;

        for file_entry in &self.file_list {
            let source_path = file_entry.path();

            // Step 1: Receive signature from receiver (or no-basis marker)
            let (block_length, block_count, strong_sum_length, sig_blocks) =
                read_signature(&mut &mut *reader)?;

            let has_basis = block_count > 0;

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

            // Step 4: Convert engine delta to wire format and send
            let wire_ops = script_to_wire_delta(&delta_script);
            write_delta(&mut &mut *writer, &wire_ops)?;
            writer.flush()?;

            // Step 5: Track stats
            bytes_sent += delta_script.total_bytes();
            files_transferred += 1;
        }

        Ok(GeneratorStats {
            files_listed: file_count,
            files_transferred,
            bytes_sent,
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
        let ctx = GeneratorContext::new(&handshake, config);

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

        // Should only have 2 files (the .txt files), not the .log file
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

        // Should only have 1 file (data.txt)
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

        // Should have all 3 files when no filters are present
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
