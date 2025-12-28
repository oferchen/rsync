//! Delta Transfer Implementation Guide
//!
//! This module documents the rsync delta transfer algorithm implementation in the Rust server.
//! It covers the complete flow from file list exchange through signature generation, delta
//! creation, delta application, and metadata preservation.
//!
//! # Overview
//!
//! The rsync protocol uses a three-role model for efficient file synchronization:
//!
//! 1. **Sender (Generator)**: Generates file list and delta operations
//! 2. **Receiver**: Generates signatures and applies deltas
//! 3. **Client**: Coordinates the transfer (may be sender or receiver)
//!
//! ## Implementation Locations
//!
//! - Generator role: [`crate::generator`]
//! - Receiver role: [`crate::receiver`]
//! - Metadata application: `metadata::apply_metadata_from_file_entry`
//! - Delta engine: [`engine::delta`]
//! - Wire protocol: [`protocol::wire`]
//!
//! # Data Flow
//!
//! ```text
//! Generator (Sender)                    Receiver
//! ------------------                    --------
//!
//! 1. Walk filesystem
//! 2. Build file list ──────────────────> Receive file list
//!
//!                                       3. For each file:
//!                                          Generate signature from basis
//!                     <────────────────── Send signature
//!
//! 4. For each file:
//!    Generate delta from signature
//!    Send delta ───────────────────────> Receive delta
//!                                        Apply delta to reconstruct file
//!                                        Apply metadata (perms, times, owner)
//! ```
//!
//! # Component Documentation
//!
//! ## 1. Receiver Signature Generation
//!
//! The receiver generates rolling and strong checksums for existing basis files.
//!
//! **Implementation**: [`crate::receiver::ReceiverContext::run`]
//!
//! ```rust,ignore
//! # use engine::signature::{SignatureLayoutParams, calculate_signature_layout};
//! # use engine::signature::{generate_file_signature, SignatureAlgorithm};
//! # use std::fs;
//! # use protocol::ProtocolVersion;
//! # fn example() -> std::io::Result<()> {
//! # let basis_path = std::path::Path::new("file.txt");
//! # let protocol = ProtocolVersion::try_from(32u8).unwrap();
//! # let checksum_length = 16;
//! // Check if basis file exists
//! let basis_file = match fs::File::open(basis_path) {
//!     Ok(f) => f,
//!     Err(_) => {
//!         // No basis exists - request whole file transfer
//!         return Ok(());
//!     }
//! };
//!
//! let file_size = basis_file.metadata()?.len();
//!
//! // Calculate block layout using rsync's square-root heuristic
//! let params = SignatureLayoutParams::new(
//!     file_size,
//!     None, // Use default block size heuristic
//!     protocol,
//!     checksum_length,
//! );
//!
//! let layout = calculate_signature_layout(params)?;
//!
//! // Generate signature using MD5 for strong checksums (protocol >= 30)
//! let signature = generate_file_signature(
//!     basis_file,
//!     layout,
//!     SignatureAlgorithm::Md5
//! )?;
//!
//! // Send signature to generator via wire protocol
//! // wire::write_signature(writer, ...)?;
//! # Ok(())
//! # }
//! ```
//!
//! **Key Points**:
//! - Uses [`engine::delta::calculate_signature_layout`] for block size heuristics
//! - MD5 for strong checksums (16 bytes) on protocol 30+
//! - Falls back to whole-file transfer if basis doesn't exist
//! - Returns signature with rolling sums (Adler-32 style) and strong sums (MD5)
//!
//! **Wire Format** (sent to generator):
//! ```text
//! Block count (varint)
//! Block length (varint)
//! Strong sum length (varint)
//! For each block:
//!   Rolling sum (4 bytes LE)
//!   Strong sum (variable length, typically 16 bytes for MD5)
//! ```
//!
//! ## 2. Generator Delta Generation
//!
//! The generator receives signatures and generates delta operations (literals vs copy references).
//!
//! **Implementation**: [`crate::generator::GeneratorContext`]
//!
//! ```rust,ignore
//! # use engine::delta::{DeltaGenerator, DeltaSignatureIndex};
//! # use engine::signature::{FileSignature, SignatureAlgorithm};
//! # use std::fs;
//! # fn example(signature: FileSignature, source_path: &std::path::Path) -> std::io::Result<()> {
//! // Open source file
//! let source = fs::File::open(source_path)?;
//!
//! // Create signature index for O(1) block lookups
//! let index = DeltaSignatureIndex::from_signature(
//!     &signature,
//!     SignatureAlgorithm::Md5
//! )?;
//!
//! // Generate delta using signature index
//! let generator = DeltaGenerator::new();
//! let delta_script = generator.generate(source, &index)?;
//!
//! // Convert engine delta script to wire format and send
//! // let wire_ops = script_to_wire_delta(delta_script);
//! // wire::write_delta(writer, &wire_ops)?;
//! # Ok(())
//! # }
//! ```
//!
//! **Key Points**:
//! - Creates [`engine::delta::DeltaSignatureIndex`] for O(1) block lookups
//! - Uses [`engine::delta::DeltaGenerator`] to create delta script
//! - Converts engine format to wire format for transmission
//!
//! **Wire Format** (sent to receiver):
//! ```text
//! Operation count (varint)
//! For each operation:
//!   Op code (1 byte): 0x00 = Literal, 0x01 = Copy
//!
//!   For Literal:
//!     Length (varint)
//!     Data bytes
//!
//!   For Copy:
//!     Block index (varint)
//!     Length (varint)
//! ```
//!
//! ## 3. Receiver Delta Application
//!
//! The receiver applies delta operations to reconstruct files atomically.
//!
//! **Implementation**: [`crate::receiver::ReceiverContext::run`]
//!
//! ```rust,ignore
//! # use engine::delta::{apply_delta, DeltaSignatureIndex};
//! # use engine::signature::{FileSignature, SignatureAlgorithm};
//! # use std::fs;
//! # fn example(
//! #     basis_path: &std::path::Path,
//! #     signature: FileSignature,
//! #     delta_script: engine::delta::DeltaScript
//! # ) -> std::io::Result<()> {
//! // Atomic file reconstruction using temp file
//! let temp_path = basis_path.with_extension("oc-rsync.tmp");
//!
//! // Create signature index
//! let index = DeltaSignatureIndex::from_signature(
//!     &signature,
//!     SignatureAlgorithm::Md5
//! )?;
//!
//! // Open basis for reading
//! let basis = fs::File::open(basis_path)?;
//! let mut output = fs::File::create(&temp_path)?;
//!
//! // Apply the delta
//! apply_delta(basis, &mut output, &index, &delta_script)?;
//! output.sync_all()?;
//!
//! // Atomic rename (crash-safe)
//! fs::rename(&temp_path, basis_path)?;
//! # Ok(())
//! # }
//! ```
//!
//! **Key Points**:
//! - Uses temp file for atomic operations (crash safety)
//! - [`engine::delta::apply_delta`] handles copy operations by reading from basis
//! - `sync_all()` before rename ensures durability
//!
//! ## 4. Metadata Preservation
//!
//! After file reconstruction, metadata is applied from the wire protocol [`protocol::flist::FileEntry`].
//!
//! **Implementation**: `metadata::apply_metadata_from_file_entry`
//!
//! See the function documentation for detailed examples of applying permissions, timestamps,
//! and ownership with nanosecond precision.
//!
//! # Adding New Functionality
//!
//! ## Example: Add Progress Reporting
//!
//! To add progress callbacks during delta application:
//!
//! ```rust,ignore
//! # use std::path::PathBuf;
//! /// Extended transfer statistics with progress tracking
//! #[derive(Debug, Clone, Default)]
//! pub struct TransferStats {
//!     pub files_listed: usize,
//!     pub files_transferred: usize,
//!     pub bytes_received: u64,
//!
//!     // NEW: Add progress fields
//!     pub bytes_matched: u64,       // Bytes copied from basis
//!     pub bytes_literal: u64,       // Bytes sent over wire
//!     pub current_file: Option<PathBuf>,
//! }
//!
//! /// Receiver context with progress callback
//! pub struct ReceiverWithProgress<F: Fn(&TransferStats)> {
//!     progress_callback: Option<F>,
//!     stats: TransferStats,
//! }
//!
//! impl<F: Fn(&TransferStats)> ReceiverWithProgress<F> {
//!     pub fn with_progress(callback: F) -> Self {
//!         Self {
//!             progress_callback: Some(callback),
//!             stats: TransferStats::default(),
//!         }
//!     }
//!
//!     fn report_progress(&self) {
//!         if let Some(ref callback) = self.progress_callback {
//!             callback(&self.stats);
//!         }
//!     }
//! }
//! ```
//!
//! ## Example: Add Compression Support
//!
//! To compress delta operations before sending:
//!
//! ```rust,ignore
//! # use std::io::{Read, Write};
//! # fn example<W: Write>(writer: W, compression_level: u32) -> std::io::Result<()> {
//! use compress::ZlibEncoder;
//!
//! let mut compressor = ZlibEncoder::new(writer, compression_level);
//! // write_delta(&mut compressor, &wire_ops)?;
//! compressor.finish()?;
//! # Ok(())
//! # }
//! ```
//!
//! # Testing Strategy
//!
//! ## Unit Tests
//!
//! Test helper functions in isolation. See [`crate::receiver`] tests for examples:
//!
//! ```rust
//! # use protocol::wire::DeltaOp;
//! # fn wire_delta_to_script(ops: Vec<DeltaOp>) -> engine::delta::DeltaScript {
//! #     unimplemented!()
//! # }
//! #[test]
//! fn wire_delta_to_script_converts_literals() {
//!     let wire_ops = vec![
//!         DeltaOp::Literal(vec![1, 2, 3, 4]),
//!         DeltaOp::Literal(vec![5, 6, 7, 8]),
//!     ];
//!
//!     let script = wire_delta_to_script(wire_ops);
//!
//!     assert_eq!(script.tokens().len(), 2);
//!     assert_eq!(script.total_bytes(), 8);
//!     assert_eq!(script.literal_bytes(), 8);
//! }
//! ```
//!
//! ## Integration Tests
//!
//! Test end-to-end transfers via CLI. See `tests/integration_server_delta.rs`:
//!
//! ```rust,ignore
//! # struct TestDir;
//! # impl TestDir {
//! #     fn new() -> std::io::Result<Self> { Ok(Self) }
//! # }
//! # struct RsyncCommand;
//! # impl RsyncCommand {
//! #     fn new() -> Self { Self }
//! #     fn args(&mut self, _: &[&str]) -> &mut Self { self }
//! #     fn assert_success(&mut self) {}
//! # }
//! #[test]
//! fn delta_transfer_with_modified_middle() {
//!     let test_dir = TestDir::new().expect("create test dir");
//!
//!     // Create source and basis files with different content
//!     // ...
//!
//!     // Run delta transfer
//!     let mut cmd = RsyncCommand::new();
//!     // cmd.args(&["src", "dest"]);
//!     cmd.assert_success();
//!
//!     // Verify reconstructed file matches source exactly
//!     // assert_eq!(fs::read(&dest_file).unwrap(), src_content);
//! }
//! ```
//!
//! # Debugging Tips
//!
//! ## Enable Trace Logging
//!
//! ```bash
//! export RUST_LOG=core::server=debug
//! cargo run -- <rsync args>
//! ```
//!
//! ## Inspect Wire Protocol
//!
//! Use binary diff tools to compare signatures/deltas:
//! ```bash
//! xxd basis_signature.bin > basis.hex
//! xxd expected_signature.bin > expected.hex
//! diff -u basis.hex expected.hex
//! ```
//!
//! ## Verify Rolling Checksum
//!
//! ```rust,ignore
//! # use checksums::RollingDigest;
//! # fn example(block_data: &[u8]) {
//! let mut rolling = RollingDigest::new();
//! rolling.update(block_data);
//! let sum = rolling.value();  // Should match signature
//! # }
//! ```
//!
//! # Performance Considerations
//!
//! ## Block Size Heuristics
//!
//! Rsync uses square-root-of-filesize heuristic:
//! - Small files (< 4KB): Whole-file transfer (no delta)
//! - Medium files: Block size ≈ √(filesize)
//! - Large files: Capped at max block size (64KB default)
//!
//! Implementation: [`engine::delta::calculate_signature_layout`]
//!
//! ## Memory Usage
//!
//! - Signatures stored in memory (one `SignatureBlock` per block)
//! - Delta script tokens buffered before application
//! - For very large files (> 1GB), consider streaming approaches
//!
//! ## SIMD Acceleration
//!
//! Rolling checksums use SIMD when available:
//! - AVX2 on x86_64 (8x 32-bit lanes)
//! - NEON on aarch64 (4x 32-bit lanes)
//! - Scalar fallback for other architectures
//!
//! Implementation: `checksums::rolling`
//!
//! # Common Patterns
//!
//! ## Atomic File Operations
//!
//! Always use temp file + rename pattern for crash safety:
//! ```rust,ignore
//! # use std::fs;
//! # fn example(final_path: &std::path::Path) -> std::io::Result<()> {
//! let temp_path = final_path.with_extension("oc-rsync.tmp");
//! let mut output = fs::File::create(&temp_path)?;
//!
//! // ... write data ...
//! output.sync_all()?;
//!
//! fs::rename(&temp_path, final_path)?;  // Atomic on same filesystem
//! # Ok(())
//! # }
//! ```
//!
//! ## Wire Protocol Trait Object Reborrowing
//!
//! Handle `?Sized` trait bounds using double mutable reborrow:
//! ```rust,ignore
//! # use std::io::{Read, Write};
//! # fn write_signature<W: Write + ?Sized>(_: &mut W) {}
//! # fn read_delta<R: Read + ?Sized>(_: &mut R) {}
//! # fn example<R: Read + ?Sized, W: Write + ?Sized>(reader: &mut R, writer: &mut W) {
//! write_signature(&mut &mut *writer);
//! read_delta(&mut &mut *reader);
//! # }
//! ```
//!
//! ## Best-Effort Metadata Application
//!
//! Never abort transfers due to metadata failures:
//! ```rust,ignore
//! # use std::path::Path;
//! # fn apply_metadata(_: &Path) -> Result<(), std::io::Error> { Ok(()) }
//! # fn example(path: &Path) {
//! if let Err(err) = apply_metadata(path) {
//!     eprintln!("[receiver] Warning: metadata failure: {}", err);
//!     // Continue with transfer
//! }
//! # }
//! ```
//!
//! # References
//!
//! - **Upstream rsync**: <https://github.com/RsyncProject/rsync>
//! - **Protocol spec**: `target/interop/upstream-src/rsync-3.4.1/csprotocol.txt`
//! - **Engine documentation**: [`engine::delta`]
//! - **Wire format**: [`protocol::wire`]
//! - **Metadata**: `metadata::apply`
//!
//! # Implementation Status
//!
//! **Complete** (as of 2025-12-09):
//! - ✅ Signature generation from basis files
//! - ✅ Delta generation with block matching
//! - ✅ Delta application with atomic file operations
//! - ✅ Metadata preservation (permissions, timestamps, ownership)
//! - ✅ Wire protocol integration (protocol 32)
//! - ✅ SIMD acceleration (AVX2/NEON)
//! - ✅ Comprehensive test coverage (3,228 tests)
//!
//! **Deferred**:
//! - ⏳ Advanced error handling (ENOSPC, permission errors, cleanup)
//! - ⏳ Protocol version compatibility testing (28-31)
//! - ⏳ Performance profiling and optimization
//! - ⏳ Sparse file detection and handling
//! - ⏳ Progress reporting callbacks
