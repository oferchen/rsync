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

/// Kind of writer paired with the [`DeltaApplicator`].
///
/// Drives the basis-file mapping policy: when the writer is io_uring-backed,
/// the basis file must be opened via [`BufferedMap`] (a sliding-window
/// `pread(2)` reader) rather than `mmap(2)`. Submitting an `mmap`-backed
/// pointer to an `io_uring` SQE has two failure modes:
///
/// 1. Cold-page faults are serviced under the SQE submission thread (or the
///    SQPOLL kernel thread when SQPOLL is enabled), turning a "free" zero-copy
///    write into a synchronous fault and stalling other in-flight SQEs on the
///    same poller.
/// 2. A concurrent truncation of the basis file raises `SIGBUS` while the
///    kernel is dereferencing the page on our behalf - recovery from
///    in-kernel `SIGBUS` is not signal-safe.
///
/// Upstream rsync deliberately avoids `mmap(2)` for basis files for the same
/// truncation reason - see `fileio.c:214-217` in upstream rsync 3.4.1.
///
/// See `docs/design/basis-file-io-policy.md` and audit
/// `docs/audits/mmap-iouring-co-usage.md` finding F1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BasisWriterKind {
    /// Standard buffered writer (or any writer not backed by io_uring).
    ///
    /// On Unix the basis file is opened with the adaptive strategy, which
    /// selects `mmap(2)` for files >= 1 MiB. This matches existing behaviour.
    #[default]
    Standard,
    /// io_uring-backed writer (e.g. `IoUringWriter` or `IoUringDiskBatch`).
    ///
    /// Forces the basis file onto [`BufferedMap`] regardless of size to keep
    /// `mmap`-backed pointers out of any io_uring submission queue entry.
    IoUring,
}

impl BasisWriterKind {
    /// Returns true if this writer is io_uring-backed.
    #[must_use]
    pub const fn is_io_uring(self) -> bool {
        matches!(self, Self::IoUring)
    }
}

/// Configuration for delta application.
#[derive(Debug, Clone, Default)]
pub struct DeltaApplyConfig {
    /// Enable sparse file optimization.
    pub sparse: bool,
    /// Writer kind paired with this applicator.
    ///
    /// Defaults to [`BasisWriterKind::Standard`]. Set to
    /// [`BasisWriterKind::IoUring`] when the destination writer is an
    /// io_uring-backed writer (e.g. `IoUringWriter` /
    /// `IoUringDiskBatch`); the applicator then opens the basis file
    /// via `BufferedMap` to avoid handing `mmap`-backed pointers to the
    /// ring. See [`BasisWriterKind`] for the rationale.
    pub writer_kind: BasisWriterKind,
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
/// - Uses `MapFile` with `BasisMapStrategy` for basis file access. The
///   exact strategy is policy-driven via [`DeltaApplyConfig::writer_kind`]:
///   - Unix, [`BasisWriterKind::Standard`]: `AdaptiveMapStrategy` -
///     files < 1MB use buffered I/O (256KB sliding window), files >= 1MB
///     use mmap for zero-copy access.
///   - Unix, [`BasisWriterKind::IoUring`]: forced to `BufferedMap` for all
///     sizes. Submitting an mmap-backed pointer to an io_uring SQE can
///     stall the SQPOLL kernel thread on cold-page faults and raises
///     `SIGBUS` on concurrent truncation. Mirrors upstream rsync's
///     deliberate avoidance of `mmap(2)` for basis files
///     (`fileio.c:214-217`). See `docs/design/basis-file-io-policy.md`.
///   - Non-Unix: `BufferedMap` - buffered I/O with 256KB sliding window
///     for all files.
/// - Uses `TokenBuffer` for literal data, reusing the same allocation across
///   all tokens to avoid per-token heap allocations.
pub struct DeltaApplicator<'a> {
    output: File,
    sparse_state: Option<SparseWriteState>,
    checksum_verifier: ChecksumVerifier,
    basis_signature: Option<&'a FileSignature>,
    /// Cached basis file mapper, opened once and reused for all block references.
    /// Uses `AdaptiveMapStrategy` on Unix, `BufferedMap` on Windows.
    basis_map: Option<MapFile<BasisMapStrategy>>,
    /// Reusable buffer for literal token data to avoid per-token allocations.
    token_buffer: TokenBuffer,
    stats: DeltaApplyResult,
}

impl<'a> DeltaApplicator<'a> {
    /// Creates a new delta applicator.
    ///
    /// If `basis_path` is provided, opens the file once and caches it for
    /// efficient block reference lookups. Basis-file mapping policy:
    ///
    /// - **Unix, standard writer**: `AdaptiveMapStrategy` - mmap for files
    ///   >= 1 MiB, buffered for smaller (existing behaviour).
    /// - **Unix, io_uring writer**: forces `BufferedMap` regardless of size.
    ///   Mmap'd basis pointers must never reach an io_uring SQE: cold-page
    ///   faults stall the SQPOLL kernel thread, and truncation by another
    ///   process raises `SIGBUS` inside the kernel SQE service path. Mirrors
    ///   upstream rsync's `fileio.c:214-217` rationale for using `read(2)`
    ///   instead of `mmap(2)` on basis files. See
    ///   `docs/design/basis-file-io-policy.md`.
    /// - **Non-Unix**: always `BufferedMap` (no mmap path is wired).
    pub fn new(
        output: File,
        config: &DeltaApplyConfig,
        checksum_verifier: ChecksumVerifier,
        basis_signature: Option<&'a FileSignature>,
        basis_path: Option<&'a Path>,
    ) -> io::Result<Self> {
        let basis_map = if let Some(path) = basis_path {
            #[cfg(unix)]
            let map = if config.writer_kind.is_io_uring() {
                // Avoid mmap when paired with io_uring (#1906, audit F1).
                MapFile::open_adaptive_buffered(path)
            } else {
                MapFile::open_adaptive(path)
            };
            #[cfg(not(unix))]
            let map = {
                // BufferedMap is the only basis strategy on non-Unix; the
                // io_uring path itself is Linux-only, so the writer_kind
                // signal is consumed indirectly via the cfg gate.
                let _ = config.writer_kind;
                MapFile::<BufferedMap>::open(path)
            };

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

    /// Returns true if a basis file is open and is using the mmap strategy.
    ///
    /// Used by tests to verify the policy decision in
    /// [`Self::new`]: an io_uring-backed writer must never produce a
    /// mmap-backed basis. See `docs/design/basis-file-io-policy.md`.
    #[must_use]
    pub fn basis_uses_mmap(&self) -> bool {
        #[cfg(unix)]
        {
            self.basis_map.as_ref().is_some_and(MapFile::is_mmap)
        }
        #[cfg(not(unix))]
        {
            // BufferedMap is the only basis strategy on non-Unix.
            false
        }
    }

    /// Returns true if a basis file is open. Used by tests.
    #[must_use]
    pub fn has_basis(&self) -> bool {
        self.basis_map.is_some()
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
    /// Uses the cached `MapFile` opened in [`Self::new`]. The mapping
    /// strategy depends on the configured [`BasisWriterKind`]:
    /// - Standard writer (Unix): adaptive - 256KB sliding window for files
    ///   < 1MB, zero-copy mmap for files >= 1MB.
    /// - io_uring writer: 256KB sliding window via `BufferedMap`,
    ///   regardless of size, to keep mmap pointers out of any io_uring SQE
    ///   (audit `docs/audits/mmap-iouring-co-usage.md` finding F1; upstream
    ///   `fileio.c:214-217`).
    /// - Non-Unix: 256KB sliding window for all files.
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

        // upstream: sum_end(char *sum) writes into caller-provided buffer.
        // sync_all() is not called - upstream rsync only fsyncs with --fsync.
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
    use tempfile::tempdir;

    #[test]
    fn delta_apply_config_default() {
        let config = DeltaApplyConfig::default();
        assert!(!config.sparse);
        assert_eq!(config.writer_kind, BasisWriterKind::Standard);
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
        let config = DeltaApplyConfig {
            sparse: true,
            writer_kind: BasisWriterKind::Standard,
        };
        assert!(config.sparse);
    }

    #[test]
    fn basis_writer_kind_io_uring_predicate() {
        assert!(BasisWriterKind::IoUring.is_io_uring());
        assert!(!BasisWriterKind::Standard.is_io_uring());
        assert_eq!(BasisWriterKind::default(), BasisWriterKind::Standard);
    }

    /// Creates a 2 MiB basis file and an output file, returning paths.
    /// 2 MiB is above `MMAP_THRESHOLD` (1 MiB) so the adaptive strategy
    /// would otherwise pick mmap on Unix.
    fn make_large_basis(dir: &tempfile::TempDir) -> (std::path::PathBuf, File) {
        let basis_path = dir.path().join("basis.bin");
        let out_path = dir.path().join("out.bin");

        let basis_bytes = vec![0xA5u8; 2 * 1024 * 1024];
        let mut basis_file = File::create(&basis_path).expect("create basis");
        basis_file.write_all(&basis_bytes).expect("write basis");
        basis_file.sync_all().ok();
        drop(basis_file);

        let out = File::create(out_path).expect("create out");
        (basis_path, out)
    }

    #[test]
    fn standard_writer_kind_uses_mmap_on_unix_for_large_basis() {
        let dir = tempdir().expect("tempdir");
        let (basis_path, out) = make_large_basis(&dir);
        let config = DeltaApplyConfig {
            sparse: false,
            writer_kind: BasisWriterKind::Standard,
        };
        let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
        let applicator =
            DeltaApplicator::new(out, &config, verifier, None, Some(basis_path.as_path()))
                .expect("construct applicator");
        assert!(applicator.has_basis());
        // On Unix, AdaptiveMapStrategy picks mmap for files >= 1 MiB.
        // On non-Unix only BufferedMap exists, so basis_uses_mmap() is
        // always false.
        #[cfg(unix)]
        assert!(
            applicator.basis_uses_mmap(),
            "standard writer + 2 MiB basis should pick mmap on Unix"
        );
        #[cfg(not(unix))]
        assert!(!applicator.basis_uses_mmap());
    }

    #[test]
    fn io_uring_writer_kind_forces_buffered_basis() {
        let dir = tempdir().expect("tempdir");
        let (basis_path, out) = make_large_basis(&dir);
        let config = DeltaApplyConfig {
            sparse: false,
            writer_kind: BasisWriterKind::IoUring,
        };
        let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
        let applicator =
            DeltaApplicator::new(out, &config, verifier, None, Some(basis_path.as_path()))
                .expect("construct applicator");
        assert!(applicator.has_basis());
        // The load-bearing invariant: io_uring writer => never mmap basis.
        // Submitting an mmap-backed pointer to an io_uring SQE either
        // stalls the SQPOLL kernel thread on cold-page faults or raises
        // SIGBUS on concurrent truncation (upstream fileio.c:214-217).
        assert!(
            !applicator.basis_uses_mmap(),
            "io_uring writer must force BufferedMap to keep mmap pointers \
             out of any io_uring SQE (audit F1, fileio.c:214-217)"
        );
    }

    #[test]
    fn applicator_without_basis_reports_no_mmap() {
        let dir = tempdir().expect("tempdir");
        let out = File::create(dir.path().join("out.bin")).expect("create out");
        let config = DeltaApplyConfig {
            sparse: false,
            writer_kind: BasisWriterKind::IoUring,
        };
        let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
        let applicator =
            DeltaApplicator::new(out, &config, verifier, None, None).expect("construct applicator");
        assert!(!applicator.has_basis());
        assert!(!applicator.basis_uses_mmap());
    }
}
