//! Delta application logic for file transfer.
//!
//! Contains the `DeltaApplicator` that applies delta data received from a sender
//! to reconstruct files. Mirrors upstream rsync's `receive_data()` function from
//! `receiver.c:240`.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use engine::signature::FileSignature;
use logging::debug_log;

use super::checksum::ChecksumVerifier;
use super::sparse::SparseWriteState;
#[cfg(unix)]
use crate::map_file::AdaptiveMapStrategy;
#[cfg(not(unix))]
use crate::map_file::BufferedMap;
use crate::map_file::MapFile;
use crate::token_buffer::TokenBuffer;

// The strategy type used for basis file mapping
#[cfg(unix)]
type BasisMapStrategy = AdaptiveMapStrategy;
#[cfg(not(unix))]
type BasisMapStrategy = BufferedMap;

/// Configuration for delta application.
#[derive(Debug, Clone, Default)]
pub struct DeltaApplyConfig {
    /// Enable sparse file optimization.
    pub sparse: bool,
}

/// Result of applying a delta to a file.
#[derive(Debug, Clone, Default)]
pub struct DeltaApplyResult {
    /// Total bytes written to the output file.
    pub bytes_written: u64,
    /// Number of literal bytes received.
    pub literal_bytes: u64,
    /// Number of bytes copied from basis file.
    pub matched_bytes: u64,
    /// Number of literal tokens processed.
    pub literal_tokens: u64,
    /// Number of block reference tokens processed.
    pub block_tokens: u64,
}

/// Applies delta data to reconstruct a file.
///
/// Mirrors upstream's `receive_data()` function structure.
///
/// # Performance Optimizations
///
/// - Uses `MapFile` with `BasisMapStrategy` for basis file access:
///   - Unix: `AdaptiveMapStrategy` - files < 1MB use buffered I/O with 256KB sliding window,
///     files >= 1MB use memory-mapped for zero-copy access
///   - Non-Unix: `BufferedMap` - buffered I/O with 256KB sliding window for all files
/// - Uses `TokenBuffer` for literal data, reusing the same allocation across
///   all tokens to avoid per-token heap allocations.
pub struct DeltaApplicator<'a> {
    output: File,
    sparse_state: Option<SparseWriteState>,
    checksum_verifier: ChecksumVerifier,
    basis_signature: Option<&'a FileSignature>,
    /// Cached basis file mapper - opened once and reused for all block refs
    /// Uses BasisMapStrategy: adaptive on Unix, buffered on Windows
    basis_map: Option<MapFile<BasisMapStrategy>>,
    /// Reusable buffer for literal token data
    token_buffer: TokenBuffer,
    stats: DeltaApplyResult,
}

impl<'a> DeltaApplicator<'a> {
    /// Creates a new delta applicator.
    ///
    /// If `basis_path` is provided, opens the file once and caches it for
    /// efficient block reference lookups. Uses `BasisMapStrategy` which:
    /// - On Unix: automatically selects mmap for files >= 1MB, buffered I/O for smaller
    /// - On non-Unix: uses buffered I/O for all files
    pub fn new(
        output: File,
        config: &DeltaApplyConfig,
        checksum_verifier: ChecksumVerifier,
        basis_signature: Option<&'a FileSignature>,
        basis_path: Option<&'a Path>,
    ) -> io::Result<Self> {
        // Open basis file once if provided - cached for all block references
        // Uses BasisMapStrategy: adaptive on Unix, buffered on Windows
        let basis_map = if let Some(path) = basis_path {
            #[cfg(unix)]
            let map = MapFile::open_adaptive(path);
            #[cfg(not(unix))]
            let map = MapFile::<BufferedMap>::open(path);

            Some(map.map_err(|e| {
                io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
            })?)
        } else {
            None
        };

        Ok(Self {
            output,
            sparse_state: config.sparse.then(SparseWriteState::new),
            checksum_verifier,
            basis_signature,
            basis_map,
            token_buffer: TokenBuffer::with_default_capacity(),
            stats: DeltaApplyResult::default(),
        })
    }

    /// Applies literal data.
    pub fn apply_literal(&mut self, data: &[u8]) -> io::Result<()> {
        // DEBUG_DELTASUM level 3: Log literal token details
        debug_log!(
            Deltasum,
            3,
            "recv literal data len={} offset={}",
            data.len(),
            self.stats.bytes_written
        );

        self.checksum_verifier.update(data);

        if let Some(ref mut sparse) = self.sparse_state {
            sparse.write(&mut self.output, data)?;
        } else {
            self.output.write_all(data)?;
        }

        self.stats.bytes_written += data.len() as u64;
        self.stats.literal_bytes += data.len() as u64;
        self.stats.literal_tokens += 1;
        Ok(())
    }

    /// Applies a block reference by copying from basis file.
    ///
    /// Uses cached `MapFile` with `AdaptiveMapStrategy` for efficient access:
    /// - Small files (< 1MB): 256KB sliding window buffer
    /// - Large files (>= 1MB): Zero-copy memory-mapped access
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No basis file is available
    /// - The block index is out of bounds (>= block count)
    pub fn apply_block_ref(&mut self, block_idx: usize) -> io::Result<()> {
        let (Some(signature), Some(basis_map)) = (&self.basis_signature, self.basis_map.as_mut())
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("block reference {block_idx} without basis file"),
            ));
        };

        let layout = signature.layout();
        let block_len = layout.block_length().get() as u64;
        let block_count = layout.block_count() as usize;

        // Validate block index bounds
        if block_idx >= block_count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("block reference {block_idx} out of bounds (block count: {block_count})"),
            ));
        }

        let offset = block_idx as u64 * block_len;

        let bytes_to_copy = if block_idx == block_count.saturating_sub(1) {
            let remainder = layout.remainder();
            if remainder > 0 {
                remainder as usize
            } else {
                block_len as usize
            }
        } else {
            block_len as usize
        };

        // DEBUG_DELTASUM level 3: Log block reference details
        debug_log!(
            Deltasum,
            3,
            "recv block ref idx={} basis_offset={} len={} output_offset={}",
            block_idx,
            offset,
            bytes_to_copy,
            self.stats.bytes_written
        );

        // Use cached MapFile - data stays in 256KB sliding window
        let block_data = basis_map.map_ptr(offset, bytes_to_copy)?;

        self.checksum_verifier.update(block_data);

        if let Some(ref mut sparse) = self.sparse_state {
            sparse.write(&mut self.output, block_data)?;
        } else {
            self.output.write_all(block_data)?;
        }

        self.stats.bytes_written += bytes_to_copy as u64;
        self.stats.matched_bytes += bytes_to_copy as u64;
        self.stats.block_tokens += 1;
        Ok(())
    }

    /// Reads and applies a single token from the reader.
    ///
    /// Returns `Ok(true)` if more tokens expected, `Ok(false)` at end.
    ///
    /// Uses reusable `TokenBuffer` to avoid per-token heap allocations,
    /// significantly reducing allocation overhead for token-heavy transfers.
    pub fn apply_token<R: Read>(&mut self, reader: &mut R) -> io::Result<bool> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let token = i32::from_le_bytes(buf);

        // DEBUG_DELTASUM level 4: Per-token tracking (very verbose)
        debug_log!(
            Deltasum,
            4,
            "recv token={} offset={}",
            token,
            self.stats.bytes_written
        );

        match token.cmp(&0) {
            std::cmp::Ordering::Equal => {
                // DEBUG_DELTASUM level 2: Token stream end marker
                debug_log!(
                    Deltasum,
                    2,
                    "recv data complete, final offset={}",
                    self.stats.bytes_written
                );
                Ok(false)
            }
            std::cmp::Ordering::Greater => {
                let len = token as usize;
                // Reuse TokenBuffer - grows but never shrinks
                self.token_buffer.resize_for(len);
                reader.read_exact(self.token_buffer.as_mut_slice())?;

                // DEBUG_DELTASUM level 3: Log literal token details
                debug_log!(
                    Deltasum,
                    3,
                    "recv literal data len={} offset={}",
                    len,
                    self.stats.bytes_written
                );

                // Apply literal data inline to avoid borrow conflict
                let data = self.token_buffer.as_slice();
                self.checksum_verifier.update(data);

                if let Some(ref mut sparse) = self.sparse_state {
                    sparse.write(&mut self.output, data)?;
                } else {
                    self.output.write_all(data)?;
                }

                self.stats.bytes_written += len as u64;
                self.stats.literal_bytes += len as u64;
                self.stats.literal_tokens += 1;
                Ok(true)
            }
            std::cmp::Ordering::Less => {
                let block_idx = -(token + 1) as usize;
                self.apply_block_ref(block_idx)?;
                Ok(true)
            }
        }
    }

    /// Finalizes delta application with checksum verification.
    pub fn finish<R: Read>(mut self, reader: &mut R) -> io::Result<DeltaApplyResult> {
        if let Some(ref mut sparse) = self.sparse_state {
            sparse.finish(&mut self.output)?;
        }

        // Note: We don't call sync_all() by default, matching upstream rsync behavior.
        // Upstream rsync only fsyncs when --fsync flag is explicitly set.

        // Read expected checksum into stack buffer - mirrors upstream sum_end(char *sum)
        // which writes into a caller-provided buffer, never allocating.
        let expected_len = self.checksum_verifier.digest_len();
        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        reader.read_exact(&mut expected[..expected_len])?;

        let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let computed_len = self.checksum_verifier.finalize_into(&mut computed);

        // DEBUG_DELTASUM level 3: Log checksum verification details
        debug_log!(
            Deltasum,
            3,
            "recv checksum verify expected={:02x?} computed={:02x?}",
            &expected[..computed_len.min(4)],
            &computed[..computed_len.min(4)]
        );

        if computed[..computed_len] != expected[..expected_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "checksum verification failed: expected {:02x?}, got {:02x?}",
                    &expected[..expected_len],
                    &computed[..computed_len]
                ),
            ));
        }

        // DEBUG_DELTASUM level 1: Basic summary (mirrors upstream receive_data)
        debug_log!(
            Deltasum,
            1,
            "recv: {} tokens ({} literal, {} block), {} bytes total ({} literal, {} matched)",
            self.stats.literal_tokens + self.stats.block_tokens,
            self.stats.literal_tokens,
            self.stats.block_tokens,
            self.stats.bytes_written,
            self.stats.literal_bytes,
            self.stats.matched_bytes
        );

        Ok(self.stats)
    }
}

/// Reads all delta tokens and applies them.
pub fn apply_delta_stream<R: Read>(
    reader: &mut R,
    applicator: &mut DeltaApplicator<'_>,
) -> io::Result<()> {
    // DEBUG_DELTASUM level 2: Log delta application start (mirrors upstream receive_data)
    debug_log!(Deltasum, 2, "recv delta stream start");

    while applicator.apply_token(reader)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_apply_config_default() {
        let config = DeltaApplyConfig::default();
        assert!(!config.sparse);
    }

    #[test]
    fn delta_apply_result_default() {
        let result = DeltaApplyResult::default();
        assert_eq!(result.bytes_written, 0);
        assert_eq!(result.literal_bytes, 0);
        assert_eq!(result.matched_bytes, 0);
    }

    #[test]
    fn delta_apply_config_sparse_enabled() {
        let config = DeltaApplyConfig { sparse: true };
        assert!(config.sparse);
    }
}
