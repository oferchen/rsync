//! Delta application logic for file transfer.
//!
//! Contains the `DeltaApplicator` that applies delta data received from a sender
//! to reconstruct files. Mirrors upstream rsync's `receive_data()` function from
//! `receiver.c:305`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
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
use crate::token_reader::{DeltaToken, LiteralData, TokenReader};

/// Same-fs detection cache for the IUD-10 `copy_file_range` fast path.
///
/// Resolved lazily on the first COPY token (so we don't pay a `metadata()`
/// syscall when the receiver never produces a COPY large enough to dispatch)
/// and cached for the remainder of the file - both fds are stable for the
/// lifetime of `DeltaApplicator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SameFsCache {
    Unresolved,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    SameFs,
    DifferentFs,
}

/// Per-applicator gate for the REFLINK-4 `FICLONERANGE` partial-clone path.
///
/// The first eligible COPY token attempts a clone; subsequent calls only
/// retry while the filesystem is still believed to support reflinks. Once a
/// clone returns `Ok(false)` we mark the path `Declined` and stop issuing
/// further ioctls so the receiver does not pay one ENOTSUP per COPY token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReflinkRangeCache {
    /// No FICLONERANGE attempt has been made yet.
    Unresolved,
    /// At least one clone succeeded - keep trying eligible ranges.
    Supported,
    /// First attempt returned `Ok(false)` (filesystem rejected the clone,
    /// alignment mismatch, or non-Linux platform). Skip further attempts.
    Declined,
}

/// Minimum filesystem block size assumed for `FICLONERANGE` alignment checks.
///
/// Btrfs, XFS, and bcachefs all use a 4 KiB block size in default
/// configurations. Configurations with larger blocks (16 KiB / 64 KiB) will
/// still attempt the clone; on alignment mismatch the kernel returns
/// `EINVAL`, the wrapper translates to `Ok(false)`, and the cache marks the
/// path `Declined` for the rest of the file. The 4 KiB floor is the
/// conservative cut-off: any range aligned to a smaller boundary cannot
/// satisfy any real-world CoW filesystem.
const REFLINK_BLOCK_ALIGNMENT: u64 = 4096;

/// Basis-file mapping strategy: adaptive (mmap above 1 MiB) on Unix,
/// buffered sliding window elsewhere.
#[cfg(unix)]
type BasisMapStrategy = AdaptiveMapStrategy;
#[cfg(not(unix))]
type BasisMapStrategy = BufferedMap;

/// Kind of writer paired with the [`DeltaApplicator`].
///
/// Drives the basis-file mapping policy: when the writer is io_uring-backed,
/// the basis file must be opened via [`crate::map_file::BufferedMap`] (a sliding-window
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
    /// Forces the basis file onto [`crate::map_file::BufferedMap`] regardless of size to keep
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
    /// Copy-on-write policy for the `FICLONERANGE` partial-reflink fast path.
    ///
    /// Defaults to [`fast_io::CowPolicy::Auto`]. When set to
    /// [`fast_io::CowPolicy::Disabled`] (the `--no-cow` flag) the applicator
    /// skips `FICLONERANGE` entirely and falls through to `copy_file_range(2)`
    /// or the read+write path, producing byte-identical output without
    /// sharing extents.
    pub cow_policy: fast_io::CowPolicy,
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
    /// Final output position after [`DeltaApplicator::finish`].
    ///
    /// When sparse mode is active this is the position returned by
    /// [`SparseWriteState::finish`] (the materialized file length including
    /// the trailing seek+1-byte hole terminator). When sparse mode is off it
    /// is `None`; the byte count is already tracked by `bytes_written`.
    ///
    /// Mirrors the post-finish size check in the live receiver path
    /// (`receiver/transfer/sync.rs:276-289`), which compares this position
    /// against the file-list entry size.
    pub final_pos: Option<u64>,
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
    /// Cached basis file mapper, opened once and reused for all block
    /// references. Strategy resolved per [`BasisWriterKind`] and target OS.
    basis_map: Option<MapFile<BasisMapStrategy>>,
    /// Reusable buffer for literal token data to avoid per-token allocations.
    token_buffer: TokenBuffer,
    /// Cached same-filesystem decision for the IUD-10 `copy_file_range`
    /// fast path. Resolved on first eligible COPY token, then reused.
    same_fs: SameFsCache,
    /// Cached gate for the REFLINK-4 `FICLONERANGE` partial-clone fast path.
    /// Marks the filesystem as `Declined` after the first `Ok(false)` so the
    /// receiver does not pay one `ENOTSUP` ioctl per COPY token on
    /// non-reflink filesystems.
    reflink_range: ReflinkRangeCache,
    /// Copy-on-write policy. When `Disabled`, the `FICLONERANGE` partial
    /// clone path is skipped entirely (mirrors the `--no-cow` flag).
    cow_policy: fast_io::CowPolicy,
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
            same_fs: SameFsCache::Unresolved,
            reflink_range: ReflinkRangeCache::Unresolved,
            cow_policy: config.cow_policy,
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

        debug_log!(
            Deltasum,
            3,
            "recv block ref idx={} basis_offset={} len={} output_offset={}",
            block_idx,
            offset,
            bytes_to_copy,
            self.stats.bytes_written
        );

        // IUD-10 fast path: when no checksum is being accumulated (CSUM_NONE)
        // and no sparse rewrite is in flight, hand the basis-to-dest copy off
        // to `copy_file_range(2)` instead of bouncing bytes through userspace.
        // upstream rsync 3.4.1 does not use copy_file_range; this is an
        // oc-rsync-only optimization that produces byte-identical output.
        if Self::should_try_kernel_copy(
            &self.checksum_verifier,
            self.sparse_state.as_ref(),
            bytes_to_copy,
        ) {
            if let Some(basis_file) = basis_map.buffered_basis_file() {
                let dest_off = self.stats.bytes_written;

                // REFLINK-4: try FICLONERANGE first when the basis and
                // destination ranges are block-aligned and large enough to
                // amortize the ioctl. Success is metadata-only - no bytes
                // traverse userspace or the kernel page cache. Failure
                // (`Ok(false)`) falls through to `copy_file_range(2)`.
                if Self::try_clone_basis_range(
                    basis_file,
                    offset,
                    &self.output,
                    dest_off,
                    bytes_to_copy,
                    self.cow_policy,
                    &mut self.reflink_range,
                    &mut self.same_fs,
                )? {
                    self.output
                        .seek(SeekFrom::Start(dest_off + bytes_to_copy as u64))?;
                    self.stats.bytes_written += bytes_to_copy as u64;
                    self.stats.matched_bytes += bytes_to_copy as u64;
                    self.stats.block_tokens += 1;
                    return Ok(());
                }

                let dispatched = Self::try_copy_basis_range(
                    basis_file,
                    offset,
                    &self.output,
                    dest_off,
                    bytes_to_copy,
                    &mut self.same_fs,
                )?;
                if dispatched == bytes_to_copy {
                    // Kernel honoured the full range. copy_file_range does
                    // not advance the destination file position, so seek
                    // forward to keep subsequent literal writes contiguous.
                    self.output
                        .seek(SeekFrom::Start(dest_off + dispatched as u64))?;
                    self.stats.bytes_written += dispatched as u64;
                    self.stats.matched_bytes += dispatched as u64;
                    self.stats.block_tokens += 1;
                    return Ok(());
                }
                // Partial or zero dispatch: fall through to read+write. The
                // destination position has not been touched (no seek issued).
            }
        }

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

    /// Predicate gating the IUD-10 `copy_file_range` fast path.
    ///
    /// Returns `true` when in-kernel copy is safe to attempt:
    /// - the checksum verifier is `None` (no per-byte digest to maintain),
    /// - no sparse rewrite is active (sparse needs zero-run scanning),
    /// - the COPY range is at least `COPY_BASIS_RANGE_MIN_BYTES` so the
    ///   syscall overhead pays off.
    #[inline]
    fn should_try_kernel_copy(
        verifier: &ChecksumVerifier,
        sparse: Option<&SparseWriteState>,
        bytes_to_copy: usize,
    ) -> bool {
        if !verifier.is_noop() {
            return false;
        }
        if sparse.is_some() {
            return false;
        }
        bytes_to_copy >= fast_io::COPY_BASIS_RANGE_MIN_BYTES
    }

    /// Resolves same-filesystem placement (cached) and dispatches the kernel
    /// copy on the first call when the basis and destination share an `st_dev`.
    ///
    /// Returns the byte count actually written by the kernel. `Ok(0)` is the
    /// caller's signal to fall back to read+write; the wrapper guarantees the
    /// destination has not been mutated in that case.
    fn try_copy_basis_range(
        basis_file: &File,
        basis_off: u64,
        dest_file: &File,
        dest_off: u64,
        bytes_to_copy: usize,
        cache: &mut SameFsCache,
    ) -> io::Result<usize> {
        if *cache == SameFsCache::Unresolved {
            *cache = Self::resolve_same_fs(basis_file, dest_file);
        }
        if *cache == SameFsCache::DifferentFs {
            return Ok(0);
        }
        fast_io::copy_basis_range(basis_file, basis_off, dest_file, dest_off, bytes_to_copy)
    }

    /// Attempts a `FICLONERANGE` partial reflink for a COPY token.
    ///
    /// Returns `Ok(true)` when the kernel cloned the full range (metadata-only,
    /// zero data transferred), `Ok(false)` when the platform, filesystem,
    /// alignment, or per-applicator gate disqualifies the call. The
    /// destination is untouched on `Ok(false)` and the caller falls through to
    /// `copy_file_range(2)`.
    ///
    /// The gating rules are conservative:
    ///
    /// - The [`fast_io::CowPolicy`] must not be `Disabled`. `--no-cow`
    ///   suppresses every reflink attempt and falls straight through to
    ///   `copy_file_range(2)`.
    /// - The cache must not be in the `Declined` state. After one negative
    ///   result we assume the filesystem does not support reflinks and stop
    ///   retrying.
    /// - The COPY range must be at least [`fast_io::CLONE_FILE_RANGE_MIN_BYTES`]
    ///   so the metadata-transaction cost is amortized.
    /// - Both file offsets and the length must be multiples of
    ///   [`REFLINK_BLOCK_ALIGNMENT`]. `FICLONERANGE` rejects unaligned
    ///   requests with `EINVAL`; checking up-front avoids burning one ioctl
    ///   per misaligned token.
    /// - The basis and destination must share an `st_dev`. `FICLONERANGE`
    ///   only reflinks within a single filesystem and fails with `EXDEV`
    ///   across mounts; the shared [`SameFsCache`] (also consulted by the
    ///   sibling `copy_file_range` path) lets us skip the doomed ioctl after
    ///   resolving the device pair once per file.
    fn try_clone_basis_range(
        basis_file: &File,
        basis_off: u64,
        dest_file: &File,
        dest_off: u64,
        bytes_to_copy: usize,
        cow_policy: fast_io::CowPolicy,
        cache: &mut ReflinkRangeCache,
        same_fs: &mut SameFsCache,
    ) -> io::Result<bool> {
        // `--no-cow`: never attempt a reflink. Fall through to
        // copy_file_range(2); leave the cache untouched so toggling the
        // policy back on within the same applicator can still clone.
        if cow_policy == fast_io::CowPolicy::Disabled {
            return Ok(false);
        }
        if *cache == ReflinkRangeCache::Declined {
            return Ok(false);
        }
        let len = bytes_to_copy as u64;
        if len < fast_io::CLONE_FILE_RANGE_MIN_BYTES {
            return Ok(false);
        }
        if basis_off % REFLINK_BLOCK_ALIGNMENT != 0
            || dest_off % REFLINK_BLOCK_ALIGNMENT != 0
            || len % REFLINK_BLOCK_ALIGNMENT != 0
        {
            return Ok(false);
        }
        if *same_fs == SameFsCache::Unresolved {
            *same_fs = Self::resolve_same_fs(basis_file, dest_file);
        }
        if *same_fs == SameFsCache::DifferentFs {
            return Ok(false);
        }
        let cloned =
            fast_io::try_clone_file_range(basis_file, basis_off, dest_file, dest_off, len)?;
        *cache = if cloned {
            ReflinkRangeCache::Supported
        } else {
            ReflinkRangeCache::Declined
        };
        Ok(cloned)
    }

    /// Computes the same-filesystem decision once per file.
    ///
    /// On Linux, checks `st_dev` to ensure `copy_file_range` can operate
    /// within a single filesystem (required on kernel < 5.3). On Windows,
    /// the `ReadFile`/`WriteFile` + `OVERLAPPED` path works regardless of
    /// volume, so we always return `SameFs` to enable the fast path. On
    /// other platforms the fast path is unavailable, so the result is
    /// `DifferentFs`.
    fn resolve_same_fs(basis: &File, dest: &File) -> SameFsCache {
        #[cfg(target_os = "linux")]
        {
            // REFLINK-1: shared st_dev comparison so the delta-apply
            // FICLONERANGE gate and the whole-file FICLONE gate agree.
            match fast_io::same_fs::files_same_device(basis, dest) {
                Some(true) => SameFsCache::SameFs,
                _ => SameFsCache::DifferentFs,
            }
        }
        #[cfg(target_os = "windows")]
        {
            // The Windows copy_basis_range implementation uses
            // ReadFile/WriteFile with OVERLAPPED offsets, which works
            // cross-volume. No same-filesystem constraint.
            let _ = (basis, dest);
            SameFsCache::SameFs
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = (basis, dest);
            SameFsCache::DifferentFs
        }
    }

    /// Reads and applies a single token from the wire via the shared
    /// [`TokenReader`].
    ///
    /// Returns `Ok(true)` if more tokens are expected, `Ok(false)` at the
    /// `End` marker. The `token_reader` handles BOTH the plain 4-byte-LE
    /// framing AND the compressed (`-z`) DEFLATED_DATA framing, exactly like
    /// the live receiver loop in `receiver/transfer/sync.rs:518-634`. After
    /// every block reference the basis bytes are fed back into the
    /// decompressor dictionary via [`TokenReader::see_token`] so the inflate
    /// state stays synchronized with the sender's deflate state - the critical
    /// step that plain raw-`i32` reads omit (upstream: `token.c:685`
    /// `see_deflate_token()`; mirrors `sync.rs:629`).
    ///
    /// Uses the applicator's reusable `TokenBuffer` for `Pending` literal
    /// reads to avoid per-token heap allocations. The live path additionally
    /// attempts a zero-copy `try_borrow_exact` on its concrete `ServerReader`;
    /// that is a pure I/O optimization with byte-identical output, so the
    /// generic `R: Read` path here always reads into the buffer.
    pub fn apply_token<R: Read>(
        &mut self,
        reader: &mut R,
        token_reader: &mut TokenReader,
    ) -> io::Result<bool> {
        match token_reader.read_token(reader)? {
            DeltaToken::End => {
                debug_log!(
                    Deltasum,
                    2,
                    "recv data complete, final offset={}",
                    self.stats.bytes_written
                );
                Ok(false)
            }
            DeltaToken::Literal(LiteralData::Ready(data)) => {
                self.apply_literal_bytes(&data)?;
                Ok(true)
            }
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                self.token_buffer.resize_for(len);
                reader.read_exact(self.token_buffer.as_mut_slice())?;
                // SAFETY: split the borrow so the literal write does not alias
                // the buffer mutably; `apply_literal_bytes` only reads.
                let len = self.token_buffer.len();
                self.apply_literal_from_buffer(len)?;
                Ok(true)
            }
            DeltaToken::BlockRef(block_idx) => {
                self.apply_block_ref(block_idx)?;
                // upstream: token.c:685 see_deflate_token() - keep the inflate
                // dictionary synced with the sender after every block match.
                // Mirrors the live receiver loop (sync.rs:629). Only the
                // compressed reader needs the bytes; for the plain reader
                // see_token is a no-op, so we avoid the extra basis map there.
                if token_reader.is_compressed() {
                    self.feed_see_token(block_idx, token_reader)?;
                }
                Ok(true)
            }
        }
    }

    /// Writes an already-materialized literal slice (the compressed-token
    /// `Ready` payload), feeding the checksum verifier and bumping counters.
    fn apply_literal_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        let len = data.len();
        debug_log!(
            Deltasum,
            3,
            "recv literal data len={} offset={}",
            len,
            self.stats.bytes_written
        );
        self.checksum_verifier.update(data);
        if let Some(ref mut sparse) = self.sparse_state {
            sparse.write(&mut self.output, data)?;
        } else {
            self.output.write_all(data)?;
        }
        self.stats.bytes_written += len as u64;
        self.stats.literal_bytes += len as u64;
        self.stats.literal_tokens += 1;
        Ok(())
    }

    /// Writes the first `len` bytes of the reusable token buffer as a literal.
    ///
    /// Kept separate from [`Self::apply_literal_bytes`] so the buffer borrow
    /// does not collide with the `&mut self.output` / `&mut self.sparse_state`
    /// borrows inside the write.
    fn apply_literal_from_buffer(&mut self, len: usize) -> io::Result<()> {
        debug_log!(
            Deltasum,
            3,
            "recv literal data len={} offset={}",
            len,
            self.stats.bytes_written
        );
        // Borrow the verifier and output disjointly from the token buffer.
        let Self {
            token_buffer,
            checksum_verifier,
            sparse_state,
            output,
            stats,
            ..
        } = self;
        let data = &token_buffer.as_slice()[..len];
        checksum_verifier.update(data);
        if let Some(sparse) = sparse_state.as_mut() {
            sparse.write(output, data)?;
        } else {
            output.write_all(data)?;
        }
        stats.bytes_written += len as u64;
        stats.literal_bytes += len as u64;
        stats.literal_tokens += 1;
        Ok(())
    }

    /// Maps the basis bytes for `block_idx` and feeds them into the
    /// decompressor dictionary via [`TokenReader::see_token`].
    ///
    /// Mirrors the live receiver's `token_reader.see_token(block_data)` call
    /// (sync.rs:629). The block index has already been bounds-checked by
    /// [`Self::apply_block_ref`], so a missing basis here is an internal
    /// invariant violation surfaced as `InvalidData`.
    fn feed_see_token(
        &mut self,
        block_idx: usize,
        token_reader: &mut TokenReader,
    ) -> io::Result<()> {
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
        let block_data = basis_map.map_ptr(offset, bytes_to_copy)?;
        token_reader.see_token(block_data)
    }

    /// Finalizes delta application with checksum verification.
    ///
    /// When `expected_size` is `Some` and sparse mode is active, also verifies
    /// the materialized file length matches, mirroring the live receiver's
    /// post-finish size check (`receiver/transfer/sync.rs:276-289`). The
    /// sparse final position is recorded in
    /// [`DeltaApplyResult::final_pos`] regardless.
    ///
    /// Returns the reconstructed output [`File`] alongside the result so the
    /// caller can fsync and commit the very handle that received the data -
    /// the live receiver path requires this handle for `--fsync` before the
    /// temp-file rename.
    pub fn finish<R: Read>(
        mut self,
        reader: &mut R,
        expected_size: Option<u64>,
    ) -> io::Result<(File, DeltaApplyResult)> {
        if let Some(ref mut sparse) = self.sparse_state {
            let final_pos = sparse.finish(&mut self.output)?;
            // upstream: fileio.c:43 sparse_end() - ftruncate to the logical
            // length (leaving the trailing region a hole) and punch any
            // in-basis zero runs, instead of materializing a trailing byte.
            self.output.set_len(final_pos)?;
            for (pos, len) in sparse.take_holes() {
                fast_io::punch_hole(&mut self.output, pos, len)?;
            }
            self.stats.final_pos = Some(final_pos);
            if let Some(expected) = expected_size {
                if final_pos != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "sparse file size mismatch: expected {expected} bytes, \
                             got {final_pos} bytes"
                        ),
                    ));
                }
            }
        }

        // upstream: sum_end(char *sum) writes into caller-provided buffer.
        // sync_all() is not called - upstream rsync only fsyncs with --fsync.
        let expected_len = self.checksum_verifier.digest_len();
        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        reader.read_exact(&mut expected[..expected_len])?;
        // upstream: receiver.c:516-517 DEBUG_GTE(DELTASUM, 2)
        debug_log!(Deltasum, 2, "got file_sum");

        let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let computed_len = self.checksum_verifier.finalize_into(&mut computed);

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

        // upstream: receiver.c:305 receive_data() emits the same summary line.
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

        Ok((self.output, self.stats))
    }
}

/// Reads all delta tokens and applies them.
///
/// The `token_reader` selects the wire framing (plain 4-byte-LE or compressed
/// DEFLATED_DATA) and is threaded through every token so the compressed
/// inflate dictionary stays synchronized via `see_token` after each block
/// match. Mirrors the live receiver loop in
/// `receiver/transfer/sync.rs:518-634`.
pub fn apply_delta_stream<R: Read>(
    reader: &mut R,
    applicator: &mut DeltaApplicator<'_>,
    token_reader: &mut TokenReader,
) -> io::Result<()> {
    // upstream: receiver.c:305 receive_data() logs the same start marker.
    debug_log!(Deltasum, 2, "recv delta stream start");

    while applicator.apply_token(reader, token_reader)? {}
    Ok(())
}

/// Drains a file's delta stream with no output and no basis, then consumes the
/// trailing whole-file checksum.
///
/// This is the receiver's DISCARD path: the temp file could not be created
/// (e.g. a read-only destination directory yielded `EACCES`), yet the sender
/// has already committed to streaming an ordinary delta for this file. The
/// bytes MUST be read off the wire in full or the protocol desyncs and every
/// subsequent file (and the goodbye handshake) is corrupted. Nothing is
/// written and no basis is mapped.
///
/// Token handling mirrors upstream `receive_data()` when `fd == -1` and
/// `mapbuf == NULL`:
///
/// - Literal tokens: the data bytes are read off the wire and dropped without
///   writing (`receiver.c:407` `write_file` is guarded by `fd != -1`). On the
///   compressed path the decoder returns the already-decompressed payload, so
///   reading the token keeps the inflate stream in sync.
/// - Block-match tokens: absorbed benignly by advancing the notional offset,
///   with NO basis read and NO `see_token` dictionary feed - exactly the
///   `if (!mapbuf) { ...; offset += len; continue; }` branch at
///   `receiver.c:444-451`. That branch runs BEFORE the `if (mapbuf)` guard that
///   would otherwise call `see_token`/`sum_update` (`receiver.c:461-466`), so
///   the discard path never feeds the dictionary either. A pre-fix upstream
///   version dereferenced `full_fname(fname)` with `fname == NULL` here and
///   crashed the receiver on an otherwise normal transfer (the
///   "nulldereference" the upstream test guards); we simply absorb the match.
///
/// After the `End` token, the sender always writes the whole-file checksum
/// (`xfer_sum_len` bytes); upstream's `receive_data()` reads it unconditionally
/// at `receiver.c:515` regardless of `fd`. `checksum_len` MUST equal the
/// negotiated digest length ([`ChecksumVerifier::digest_len`]).
///
/// # Upstream Reference
///
/// - `receiver.c:524-527` - `discard_receive_data()` calls
///   `receive_data(f_in, NULL, -1, 0, NULL, -1, file, 0)`.
/// - `receiver.c:999-1006` - `open_tmpfile` failure -> `discard_receive_data`
///   + `continue` (no propagation out of the receive loop).
/// - `receiver.c:444-451` - block-match-with-no-basis absorb (`offset += len`).
/// - `receiver.c:515` - trailing `read_buf(f_in, sender_file_sum, xfer_sum_len)`.
pub fn discard_delta_stream<R: Read>(
    reader: &mut R,
    token_reader: &mut TokenReader,
    checksum_len: usize,
) -> io::Result<()> {
    debug_log!(Deltasum, 2, "recv delta stream discard start");

    let mut scratch = Vec::new();
    loop {
        match token_reader.read_token(reader)? {
            DeltaToken::End => break,
            // Compressed literal: decoder already consumed + decompressed the
            // wire bytes; drop the payload.
            DeltaToken::Literal(LiteralData::Ready(_)) => {}
            // Plain literal: read the raw bytes off the wire and drop them.
            // upstream: receiver.c:407 write_file is skipped when fd == -1.
            DeltaToken::Literal(LiteralData::Pending(len)) => {
                if scratch.len() < len {
                    scratch.resize(len, 0);
                }
                reader.read_exact(&mut scratch[..len])?;
            }
            // Block match with no basis: absorb without reading a basis block
            // and without feeding see_token. upstream: receiver.c:444-451.
            DeltaToken::BlockRef(_) => {}
        }
    }

    // upstream: receiver.c:515 - the sender always trails the delta with the
    // whole-file checksum; consume it so the stream stays aligned for the next
    // NDX / goodbye. On the discard path there is nothing to verify against.
    let mut sink = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    reader.read_exact(&mut sink[..checksum_len])?;
    // upstream: receiver.c:516-517 DEBUG_GTE(DELTASUM, 2)
    debug_log!(Deltasum, 2, "got file_sum");

    debug_log!(Deltasum, 2, "recv delta stream discard complete");
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
            cow_policy: fast_io::CowPolicy::Auto,
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
            cow_policy: fast_io::CowPolicy::Auto,
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
            cow_policy: fast_io::CowPolicy::Auto,
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
            cow_policy: fast_io::CowPolicy::Auto,
        };
        let verifier = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
        let applicator =
            DeltaApplicator::new(out, &config, verifier, None, None).expect("construct applicator");
        assert!(!applicator.has_basis());
        assert!(!applicator.basis_uses_mmap());
    }

    #[test]
    fn should_try_kernel_copy_requires_noop_verifier() {
        // CSUM_NONE -> fast path eligible.
        let none = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
        assert!(DeltaApplicator::should_try_kernel_copy(
            &none,
            None,
            fast_io::COPY_BASIS_RANGE_MIN_BYTES,
        ));
        // Any active digest -> declined; the fast path cannot feed the
        // verifier without re-reading the bytes it just elided.
        let md5 = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::MD5);
        assert!(!DeltaApplicator::should_try_kernel_copy(
            &md5,
            None,
            fast_io::COPY_BASIS_RANGE_MIN_BYTES,
        ));
    }

    #[test]
    fn should_try_kernel_copy_declines_when_sparse() {
        let none = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
        let sparse = SparseWriteState::new();
        // Sparse needs zero-run scanning; cannot delegate to kernel copy.
        assert!(!DeltaApplicator::should_try_kernel_copy(
            &none,
            Some(&sparse),
            fast_io::COPY_BASIS_RANGE_MIN_BYTES,
        ));
    }

    #[test]
    fn should_try_kernel_copy_declines_below_min_bytes() {
        let none = ChecksumVerifier::for_algorithm(protocol::ChecksumAlgorithm::None);
        // Tiny ranges hit the syscall-cost wall before they amortize.
        assert!(!DeltaApplicator::should_try_kernel_copy(
            &none,
            None,
            fast_io::COPY_BASIS_RANGE_MIN_BYTES - 1,
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn resolve_same_fs_marks_two_files_in_same_tempdir() {
        // Both files in the same tempdir live on the same filesystem under
        // every supported test runner; the resolver must agree.
        let dir = tempdir().expect("tempdir");
        let a = File::create(dir.path().join("a")).expect("create a");
        let b = File::create(dir.path().join("b")).expect("create b");
        assert_eq!(
            DeltaApplicator::resolve_same_fs(&a, &b),
            SameFsCache::SameFs,
        );
    }

    /// REFLINK-4: range below `CLONE_FILE_RANGE_MIN_BYTES` declines without
    /// touching the filesystem so tail blocks do not burn an ioctl.
    #[test]
    fn try_clone_basis_range_declines_below_threshold() {
        let dir = tempdir().expect("tempdir");
        let basis = File::create(dir.path().join("basis")).expect("basis");
        let dest = File::create(dir.path().join("dest")).expect("dest");
        let mut cache = ReflinkRangeCache::Unresolved;
        let mut same_fs = SameFsCache::Unresolved;
        let result = DeltaApplicator::try_clone_basis_range(
            &basis,
            0,
            &dest,
            0,
            (fast_io::CLONE_FILE_RANGE_MIN_BYTES - 1) as usize,
            fast_io::CowPolicy::Auto,
            &mut cache,
            &mut same_fs,
        )
        .expect("declines silently");
        assert!(!result);
        // Predicate must NOT poison the cache when it declines purely on
        // size grounds - a subsequent large eligible token should still try.
        assert_eq!(cache, ReflinkRangeCache::Unresolved);
        // The size check runs before the same-fs probe, so no device lookup
        // should have happened.
        assert_eq!(same_fs, SameFsCache::Unresolved);
    }

    /// REFLINK-4: misaligned basis offset declines without an ioctl.
    /// `FICLONERANGE` rejects unaligned ranges with EINVAL; the predicate
    /// front-runs that check.
    #[test]
    fn try_clone_basis_range_declines_on_unaligned_basis_offset() {
        let dir = tempdir().expect("tempdir");
        let basis = File::create(dir.path().join("basis")).expect("basis");
        let dest = File::create(dir.path().join("dest")).expect("dest");
        let mut cache = ReflinkRangeCache::Unresolved;
        let mut same_fs = SameFsCache::Unresolved;
        let result = DeltaApplicator::try_clone_basis_range(
            &basis,
            1,
            &dest,
            0,
            REFLINK_BLOCK_ALIGNMENT as usize * 8,
            fast_io::CowPolicy::Auto,
            &mut cache,
            &mut same_fs,
        )
        .expect("declines silently");
        assert!(!result);
        assert_eq!(cache, ReflinkRangeCache::Unresolved);
        assert_eq!(same_fs, SameFsCache::Unresolved);
    }

    /// REFLINK-4: misaligned destination offset declines without an ioctl.
    #[test]
    fn try_clone_basis_range_declines_on_unaligned_dest_offset() {
        let dir = tempdir().expect("tempdir");
        let basis = File::create(dir.path().join("basis")).expect("basis");
        let dest = File::create(dir.path().join("dest")).expect("dest");
        let mut cache = ReflinkRangeCache::Unresolved;
        let mut same_fs = SameFsCache::Unresolved;
        let result = DeltaApplicator::try_clone_basis_range(
            &basis,
            0,
            &dest,
            1024,
            REFLINK_BLOCK_ALIGNMENT as usize * 8,
            fast_io::CowPolicy::Auto,
            &mut cache,
            &mut same_fs,
        )
        .expect("declines silently");
        assert!(!result);
        assert_eq!(cache, ReflinkRangeCache::Unresolved);
        assert_eq!(same_fs, SameFsCache::Unresolved);
    }

    /// REFLINK-4: once the cache is `Declined` the predicate short-circuits
    /// even for aligned, large enough ranges. This is the "one ioctl per
    /// file" invariant that keeps non-reflink filesystems from paying a
    /// per-token cost.
    #[test]
    fn try_clone_basis_range_short_circuits_when_declined() {
        let dir = tempdir().expect("tempdir");
        let basis = File::create(dir.path().join("basis")).expect("basis");
        let dest = File::create(dir.path().join("dest")).expect("dest");
        let mut cache = ReflinkRangeCache::Declined;
        let mut same_fs = SameFsCache::Unresolved;
        let result = DeltaApplicator::try_clone_basis_range(
            &basis,
            0,
            &dest,
            0,
            REFLINK_BLOCK_ALIGNMENT as usize * 16,
            fast_io::CowPolicy::Auto,
            &mut cache,
            &mut same_fs,
        )
        .expect("declines silently");
        assert!(!result);
        assert_eq!(cache, ReflinkRangeCache::Declined);
    }

    /// REFLINK-3: a cross-filesystem basis/destination pair declines without
    /// issuing the `FICLONERANGE` ioctl. The range here is aligned and large
    /// enough to clear the size and alignment gates, so the decline can only
    /// come from the same-fs guard. The reflink cache stays `Unresolved` -
    /// the device mismatch is not a filesystem capability verdict, so a later
    /// same-fs token must still be allowed to try.
    #[test]
    fn try_clone_basis_range_declines_on_different_filesystem() {
        let dir = tempdir().expect("tempdir");
        let basis = File::create(dir.path().join("basis")).expect("basis");
        let dest = File::create(dir.path().join("dest")).expect("dest");
        let mut cache = ReflinkRangeCache::Unresolved;
        let mut same_fs = SameFsCache::DifferentFs;
        let result = DeltaApplicator::try_clone_basis_range(
            &basis,
            0,
            &dest,
            0,
            REFLINK_BLOCK_ALIGNMENT as usize * 16,
            fast_io::CowPolicy::Auto,
            &mut cache,
            &mut same_fs,
        )
        .expect("declines silently");
        assert!(!result);
        assert_eq!(cache, ReflinkRangeCache::Unresolved);
        assert_eq!(same_fs, SameFsCache::DifferentFs);
    }

    /// Encodes a plain (uncompressed) delta: literal token, match token, End,
    /// then the trailing whole-file checksum. Mirrors the sender's wire layout
    /// (`encode_plain` + `append_checksum` in the equivalence test).
    fn plain_delta_with_match(digest_len: usize) -> Vec<u8> {
        let mut wire = Vec::new();
        // Literal token: positive length, then that many data bytes.
        let literal = b"abc";
        wire.extend_from_slice(&(literal.len() as i32).to_le_bytes());
        wire.extend_from_slice(literal);
        // Block-match token for basis index 0: encoded as -(idx + 1).
        let match_idx: i32 = 0;
        wire.extend_from_slice(&(-(match_idx + 1)).to_le_bytes());
        // End marker.
        wire.extend_from_slice(&0_i32.to_le_bytes());
        // Trailing whole-file checksum (arbitrary bytes; discard never verifies).
        wire.extend(std::iter::repeat_n(0xEE_u8, digest_len));
        wire
    }

    /// The discard path MUST consume the ENTIRE per-file frame - every delta
    /// token AND the trailing checksum. If a single byte is left behind, the
    /// next NDX read parses leftover delta bytes as a frame header and the
    /// whole session desyncs (upstream: `discard_receive_data` at
    /// receiver.c:524 exists precisely to keep the stream aligned when the
    /// receiver never writes the file). This test pins that WHY: after a
    /// discard the reader is positioned exactly at end-of-frame, with no
    /// trailing bytes and no error - even when the delta contains a
    /// block-match token with no basis (receiver.c:444-451).
    #[test]
    fn discard_drains_plain_delta_with_match_token_to_exact_end() {
        let digest_len = 16;
        let frame = plain_delta_with_match(digest_len);
        // A sentinel NDX follows the frame; discard must NOT touch it.
        let mut wire = frame.clone();
        let sentinel = 0x1234_5678_u32.to_le_bytes();
        wire.extend_from_slice(&sentinel);

        let mut cursor = io::Cursor::new(wire);
        let mut token_reader = TokenReader::new(None).expect("plain reader");

        discard_delta_stream(&mut cursor, &mut token_reader, digest_len).expect("drains cleanly");

        // The cursor is positioned exactly after the frame: the next 4 bytes
        // are the untouched sentinel NDX, proving no desync.
        assert_eq!(cursor.position(), frame.len() as u64);
        let mut next = [0u8; 4];
        cursor.read_exact(&mut next).expect("sentinel intact");
        assert_eq!(next, sentinel);
    }

    /// A truncated frame (delta drained but trailing checksum missing) must
    /// surface as an error, not silently succeed - a short read here means the
    /// peer died mid-frame and the caller must abort rather than proceed to the
    /// next NDX on a broken stream.
    #[test]
    fn discard_errors_when_trailing_checksum_is_missing() {
        let digest_len = 16;
        let mut wire = Vec::new();
        let match_idx: i32 = 0;
        wire.extend_from_slice(&(-(match_idx + 1)).to_le_bytes()); // match token
        wire.extend_from_slice(&0_i32.to_le_bytes()); // End
        // Deliberately omit the trailing checksum bytes.

        let mut cursor = io::Cursor::new(wire);
        let mut token_reader = TokenReader::new(None).expect("plain reader");

        let err = discard_delta_stream(&mut cursor, &mut token_reader, digest_len)
            .expect_err("truncated frame must error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// End-to-end intent of the temp-create-fail path: after the receiver
    /// drains a discarded delta it records IOERR_GENERAL, which MUST map to
    /// exit 23 (RERR_PARTIAL) - matching upstream's
    /// FERROR_XFER -> got_xfer_error -> _exit(RERR_PARTIAL) (log.c:311,
    /// main.c:1630). This pins WHY the receiver sets the flag rather than
    /// aborting: the transfer is partial, not fatal (exit 12), and the drained
    /// stream keeps every subsequent file intact.
    #[test]
    fn discarded_file_maps_to_partial_transfer_exit_23() {
        use crate::generator::io_error_flags::{IOERR_GENERAL, to_exit_code};

        let digest_len = 16;
        let frame = plain_delta_with_match(digest_len);
        let mut cursor = io::Cursor::new(frame.clone());
        let mut token_reader = TokenReader::new(None).expect("plain reader");

        // Draining must succeed (no crash, no propagated open error).
        discard_delta_stream(&mut cursor, &mut token_reader, digest_len).expect("drains cleanly");
        assert_eq!(cursor.position(), frame.len() as u64);

        // The receiver ORs IOERR_GENERAL for the failed file; that bit is the
        // exit-code signal.
        let io_error = IOERR_GENERAL;
        assert_eq!(
            to_exit_code(io_error),
            23,
            "temp-create failure must yield RERR_PARTIAL (exit 23), not fatal (12)"
        );
    }
}
