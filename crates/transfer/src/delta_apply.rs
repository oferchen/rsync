//! crates/core/src/server/delta_apply.rs
//!
//! Delta application for file transfer.
//!
//! This module encapsulates the logic for applying delta data received from a sender
//! to reconstruct files. It mirrors upstream rsync's `receive_data()` function from
//! `receiver.c:240`.
//!
//! # Upstream Reference
//!
//! - `receiver.c:240` - `receive_data()` - Main delta application loop
//! - `receiver.c:315` - Token processing loop (literal vs block reference)
//! - `receiver.c:374-382` - Sparse file finalization
//! - `receiver.c:408` - File checksum verification

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
use engine::signature::FileSignature;
use protocol::{ChecksumAlgorithm, CompatibilityFlags, NegotiationResult, ProtocolVersion};

// ============================================================================
// Sparse Write State - Tracks pending zeros for hole creation
// ============================================================================

/// State tracker for sparse file writing.
///
/// Tracks pending runs of zeros that should become holes in the output file
/// rather than being written as data. Mirrors upstream rsync's `write_sparse()`
/// behavior in `fileio.c`.
#[derive(Debug, Default)]
pub struct SparseWriteState {
    pending_zeros: u64,
}

impl SparseWriteState {
    /// Creates a new sparse write state.
    #[must_use]
    pub const fn new() -> Self {
        Self { pending_zeros: 0 }
    }

    /// Adds zero bytes to the pending run.
    #[inline]
    pub const fn accumulate(&mut self, count: usize) {
        self.pending_zeros = self.pending_zeros.saturating_add(count as u64);
    }

    /// Returns the number of pending zero bytes.
    #[must_use]
    pub const fn pending(&self) -> u64 {
        self.pending_zeros
    }

    /// Flushes pending zeros by seeking forward, creating a hole.
    pub fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
        }

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
    pub fn write<W: Write + Seek>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        const CHUNK_SIZE: usize = 1024;
        let mut offset = 0;

        while offset < data.len() {
            let end = (offset + CHUNK_SIZE).min(data.len());
            let chunk = &data[offset..end];

            let leading_zeros = chunk.iter().take_while(|&&b| b == 0).count();
            self.accumulate(leading_zeros);

            if leading_zeros == chunk.len() {
                offset = end;
                continue;
            }

            let tail = &chunk[leading_zeros..];
            let trailing_zeros = tail.iter().rev().take_while(|&&b| b == 0).count();
            let data_start = offset + leading_zeros;
            let data_end = end - trailing_zeros;

            if data_end > data_start {
                self.flush(writer)?;
                writer.write_all(&data[data_start..data_end])?;
            }

            self.pending_zeros = trailing_zeros as u64;
            offset = end;
        }

        Ok(data.len())
    }

    /// Finalizes sparse writing and returns final position.
    pub fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        if self.pending_zeros > 0 {
            let skip = self.pending_zeros.saturating_sub(1);
            if skip > 0 {
                let mut remaining = skip;
                while remaining > 0 {
                    let step = remaining.min(i64::MAX as u64);
                    writer.seek(SeekFrom::Current(step as i64))?;
                    remaining -= step;
                }
            }
            writer.write_all(&[0])?;
            self.pending_zeros = 0;
        }
        writer.stream_position()
    }
}

// ============================================================================
// Checksum Verifier - Enum-based dispatch (no heap allocation)
// ============================================================================

/// Checksum verifier for delta transfer integrity verification.
///
/// Uses enum dispatch for zero-allocation runtime algorithm selection.
/// Mirrors upstream rsync's checksum verification in `receiver.c`.
pub enum ChecksumVerifier {
    /// MD4 checksum (legacy, protocol < 30).
    Md4(Md4),
    /// MD5 checksum (protocol 30+ default).
    Md5(Md5),
    /// SHA1 checksum.
    Sha1(Sha1),
    /// XXH64 checksum (fast non-cryptographic).
    Xxh64(Xxh64),
    /// XXH3 64-bit checksum (fastest non-cryptographic).
    Xxh3(Xxh3),
    /// XXH3 128-bit checksum.
    Xxh128(Xxh3_128),
}

impl ChecksumVerifier {
    /// Creates a verifier based on negotiated parameters.
    #[must_use]
    pub fn new(
        negotiated: Option<&NegotiationResult>,
        protocol: ProtocolVersion,
        _seed: i32,
        _compat_flags: Option<&CompatibilityFlags>,
    ) -> Self {
        negotiated
            .map(|n| Self::for_algorithm(n.checksum))
            .unwrap_or_else(|| {
                if protocol.as_u8() >= 30 {
                    Self::Md5(Md5::new())
                } else {
                    Self::Md4(Md4::new())
                }
            })
    }

    /// Creates a verifier for a specific algorithm.
    #[must_use]
    pub fn for_algorithm(algorithm: ChecksumAlgorithm) -> Self {
        match algorithm {
            ChecksumAlgorithm::None | ChecksumAlgorithm::MD4 => Self::Md4(Md4::new()),
            ChecksumAlgorithm::MD5 => Self::Md5(Md5::new()),
            ChecksumAlgorithm::SHA1 => Self::Sha1(Sha1::new()),
            ChecksumAlgorithm::XXH64 => Self::Xxh64(Xxh64::with_seed(0)),
            ChecksumAlgorithm::XXH3 => Self::Xxh3(Xxh3::with_seed(0)),
            ChecksumAlgorithm::XXH128 => Self::Xxh128(Xxh3_128::with_seed(0)),
        }
    }

    /// Updates the hasher with data.
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md4(h) => h.update(data),
            Self::Md5(h) => h.update(data),
            Self::Sha1(h) => h.update(data),
            Self::Xxh64(h) => h.update(data),
            Self::Xxh3(h) => h.update(data),
            Self::Xxh128(h) => h.update(data),
        }
    }

    /// Returns the digest length for the current algorithm.
    #[must_use]
    pub const fn digest_len(&self) -> usize {
        match self {
            Self::Md4(_) | Self::Md5(_) | Self::Xxh128(_) => 16,
            Self::Sha1(_) => 20,
            Self::Xxh64(_) | Self::Xxh3(_) => 8,
        }
    }

    /// Finalizes and returns the digest as bytes.
    #[must_use]
    pub fn finalize(self) -> Vec<u8> {
        match self {
            Self::Md4(h) => h.finalize().as_ref().to_vec(),
            Self::Md5(h) => h.finalize().as_ref().to_vec(),
            Self::Sha1(h) => h.finalize().as_ref().to_vec(),
            Self::Xxh64(h) => h.finalize().as_ref().to_vec(),
            Self::Xxh3(h) => h.finalize().as_ref().to_vec(),
            Self::Xxh128(h) => h.finalize().as_ref().to_vec(),
        }
    }
}

// ============================================================================
// Delta Application Types
// ============================================================================

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
}

/// Applies delta data to reconstruct a file.
///
/// Mirrors upstream's `receive_data()` function structure.
pub struct DeltaApplicator<'a> {
    output: File,
    sparse_state: Option<SparseWriteState>,
    checksum_verifier: ChecksumVerifier,
    basis_signature: Option<&'a FileSignature>,
    basis_path: Option<&'a Path>,
    stats: DeltaApplyResult,
}

impl<'a> DeltaApplicator<'a> {
    /// Creates a new delta applicator.
    pub fn new(
        output: File,
        config: &DeltaApplyConfig,
        checksum_verifier: ChecksumVerifier,
        basis_signature: Option<&'a FileSignature>,
        basis_path: Option<&'a Path>,
    ) -> Self {
        Self {
            output,
            sparse_state: config.sparse.then(SparseWriteState::new),
            checksum_verifier,
            basis_signature,
            basis_path,
            stats: DeltaApplyResult::default(),
        }
    }

    /// Applies literal data.
    pub fn apply_literal(&mut self, data: &[u8]) -> io::Result<()> {
        self.checksum_verifier.update(data);

        if let Some(ref mut sparse) = self.sparse_state {
            sparse.write(&mut self.output, data)?;
        } else {
            self.output.write_all(data)?;
        }

        self.stats.bytes_written += data.len() as u64;
        self.stats.literal_bytes += data.len() as u64;
        Ok(())
    }

    /// Applies a block reference by copying from basis file.
    pub fn apply_block_ref(&mut self, block_idx: usize) -> io::Result<()> {
        let (Some(signature), Some(basis_path)) = (&self.basis_signature, &self.basis_path) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("block reference {block_idx} without basis file"),
            ));
        };

        let layout = signature.layout();
        let block_len = layout.block_length().get() as u64;
        let offset = block_idx as u64 * block_len;
        let block_count = layout.block_count() as usize;

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

        let mut basis_file = File::open(basis_path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to open basis file {basis_path:?}: {e}"),
            )
        })?;
        basis_file.seek(SeekFrom::Start(offset))?;

        let mut block_data = vec![0u8; bytes_to_copy];
        basis_file.read_exact(&mut block_data)?;

        self.checksum_verifier.update(&block_data);

        if let Some(ref mut sparse) = self.sparse_state {
            sparse.write(&mut self.output, &block_data)?;
        } else {
            self.output.write_all(&block_data)?;
        }

        self.stats.bytes_written += bytes_to_copy as u64;
        self.stats.matched_bytes += bytes_to_copy as u64;
        Ok(())
    }

    /// Reads and applies a single token from the reader.
    ///
    /// Returns `Ok(true)` if more tokens expected, `Ok(false)` at end.
    pub fn apply_token<R: Read>(&mut self, reader: &mut R) -> io::Result<bool> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let token = i32::from_le_bytes(buf);

        match token.cmp(&0) {
            std::cmp::Ordering::Equal => Ok(false),
            std::cmp::Ordering::Greater => {
                let mut data = vec![0u8; token as usize];
                reader.read_exact(&mut data)?;
                self.apply_literal(&data)?;
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

        self.output.sync_all()?;

        let expected_len = self.checksum_verifier.digest_len();
        let mut expected = vec![0u8; expected_len];
        reader.read_exact(&mut expected)?;

        let computed = self.checksum_verifier.finalize();
        let cmp_len = computed.len().min(expected.len());

        if computed[..cmp_len] != expected[..cmp_len] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "checksum verification failed: expected {:02x?}, got {:02x?}",
                    &expected[..cmp_len],
                    &computed[..cmp_len]
                ),
            ));
        }

        Ok(self.stats)
    }
}

/// Reads all delta tokens and applies them.
pub fn apply_delta_stream<R: Read>(
    reader: &mut R,
    applicator: &mut DeltaApplicator<'_>,
) -> io::Result<()> {
    while applicator.apply_token(reader)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn sparse_state_new() {
        let state = SparseWriteState::new();
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn sparse_state_accumulate() {
        let mut state = SparseWriteState::new();
        state.accumulate(100);
        assert_eq!(state.pending(), 100);
        state.accumulate(50);
        assert_eq!(state.pending(), 150);
    }

    #[test]
    fn sparse_state_flush_empty() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        state.flush(&mut cursor).unwrap();
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn sparse_state_flush_with_pending() {
        let mut state = SparseWriteState::new();
        state.accumulate(100);
        let mut cursor = Cursor::new(vec![0u8; 200]);
        state.flush(&mut cursor).unwrap();
        assert_eq!(cursor.position(), 100);
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn sparse_state_write_non_zero() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        assert_eq!(state.write(&mut cursor, b"hello").unwrap(), 5);
    }

    #[test]
    fn sparse_state_write_zeros_accumulates() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 100]);
        assert_eq!(state.write(&mut cursor, &[0u8; 50]).unwrap(), 50);
        assert!(state.pending() > 0);
    }

    #[test]
    fn sparse_state_finish() {
        let mut state = SparseWriteState::new();
        state.accumulate(10);
        let mut cursor = Cursor::new(vec![0u8; 100]);
        assert_eq!(state.finish(&mut cursor).unwrap(), 10);
    }

    #[test]
    fn verifier_digest_lengths() {
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD4).digest_len(),
            16
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5).digest_len(),
            16
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::SHA1).digest_len(),
            20
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH64).digest_len(),
            8
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH3).digest_len(),
            8
        );
        assert_eq!(
            ChecksumVerifier::for_algorithm(ChecksumAlgorithm::XXH128).digest_len(),
            16
        );
    }

    #[test]
    fn verifier_update_and_finalize() {
        let mut v = ChecksumVerifier::for_algorithm(ChecksumAlgorithm::MD5);
        v.update(b"hello");
        v.update(b" world");
        assert_eq!(v.finalize().len(), 16);
    }

    #[test]
    fn verifier_protocol_defaults() {
        let v29 = ChecksumVerifier::new(None, ProtocolVersion::try_from(29u8).unwrap(), 0, None);
        assert_eq!(v29.digest_len(), 16); // MD4

        let v30 = ChecksumVerifier::new(None, ProtocolVersion::try_from(30u8).unwrap(), 0, None);
        assert_eq!(v30.digest_len(), 16); // MD5
    }

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
}
