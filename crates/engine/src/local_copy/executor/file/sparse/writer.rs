//! Sparse file writer decorator for transparent zero-region interception.
//!
//! Provides a [`SparseWriter`] decorator that wraps any `Write + Seek` writer
//! and intercepts write calls to scan for zero regions. Detected zero runs are
//! converted to seeks (creating filesystem holes) instead of writing zeroes,
//! producing sparse files on supported filesystems.
//!
//! upstream: fileio.c:write_sparse() - sparse write with seek-past-zeros

use std::io::{self, Seek, SeekFrom, Write};

use super::{leading_zero_run, trailing_zero_run};

/// Strategy for scanning data buffers to identify zero regions.
///
/// Controls how [`SparseWriter`] detects runs of zero bytes. The choice affects
/// throughput for different data patterns and alignment characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZeroScanStrategy {
    /// SIMD-accelerated scanning via `fast_io::zero_detect`.
    ///
    /// Uses AVX2 (32 bytes/iter), SSE2/NEON (16 bytes/iter), or scalar `u128`
    /// (16 bytes/iter) depending on platform. Best for large, aligned buffers.
    #[default]
    Simd,
    /// Byte-level scanning. Simpler and suitable for small or unaligned buffers
    /// where SIMD setup overhead would dominate.
    ByteLevel,
}

/// Statistics tracked by [`SparseWriter`] during sparse writes.
///
/// Provides insight into how effectively the sparse writer is converting zero
/// runs into holes, useful for diagnostics and performance tuning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SparseWriteStats {
    /// Total bytes written to the inner writer (non-zero data).
    pub bytes_written: u64,
    /// Total bytes skipped via seeking (zero regions converted to holes).
    pub bytes_seeked: u64,
    /// Number of distinct zero runs detected and converted to seeks.
    pub zero_runs_detected: u64,
}

/// Sparse file writer decorator that wraps any `Write + Seek` writer.
///
/// Intercepts [`Write::write`] calls and scans for contiguous zero-byte regions.
/// When a zero run is detected, it is accumulated and later flushed as a seek
/// (creating a filesystem hole) rather than written as data. Non-zero data is
/// delegated to the inner writer.
///
/// The decorator maintains the single-seek-per-zero-run invariant: consecutive
/// zero bytes are accumulated across multiple write calls and flushed as a
/// single seek when non-zero data arrives or at finalization.
///
/// # Zero-detection strategies
///
/// - [`ZeroScanStrategy::Simd`] (default) - SIMD-accelerated via `fast_io`
/// - [`ZeroScanStrategy::ByteLevel`] - byte-by-byte scanning
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::Write;
/// use engine::SparseWriter;
///
/// let file = File::create("output.bin").unwrap();
/// let mut writer = SparseWriter::new(file);
///
/// // Write data - zeros become holes, non-zero data is written normally
/// writer.write_all(b"hello").unwrap();
/// writer.write_all(&[0u8; 10000]).unwrap(); // Accumulated as pending seek
/// writer.write_all(b"world").unwrap();       // Flushes pending seek, then writes
///
/// let stats = writer.stats();
/// writer.finish().unwrap();
/// ```
pub struct SparseWriter<W> {
    inner: W,
    /// Accumulated zero bytes pending flush as a seek.
    pending_zeros: u64,
    /// Chunk size for zero-run scanning within write buffers.
    chunk_size: usize,
    /// Zero-detection strategy.
    scan_strategy: ZeroScanStrategy,
    /// Write statistics.
    stats: SparseWriteStats,
}

/// Default scan window for sparse detection, matching upstream rsync's
/// `SPARSE_WRITE_SIZE` (1 KB) so interior zero runs are punched as upstream does.
const DEFAULT_CHUNK_SIZE: usize = super::SPARSE_WRITE_SIZE;

impl<W: Write + Seek> SparseWriter<W> {
    /// Creates a new sparse writer wrapping the given writer.
    ///
    /// Uses SIMD-accelerated zero detection and the default scan window (1 KB).
    #[must_use]
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            pending_zeros: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            scan_strategy: ZeroScanStrategy::default(),
            stats: SparseWriteStats::default(),
        }
    }

    /// Creates a sparse writer with a specific zero-detection strategy.
    #[must_use]
    pub fn with_strategy(inner: W, strategy: ZeroScanStrategy) -> Self {
        Self {
            inner,
            pending_zeros: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            scan_strategy: strategy,
            stats: SparseWriteStats::default(),
        }
    }

    /// Returns a snapshot of the current write statistics.
    #[must_use]
    pub const fn stats(&self) -> SparseWriteStats {
        self.stats
    }

    /// Returns a reference to the inner writer.
    pub const fn inner(&self) -> &W {
        &self.inner
    }

    /// Returns a mutable reference to the inner writer.
    ///
    /// Use with care - direct writes bypass sparse detection.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consumes the sparse writer and returns the inner writer.
    ///
    /// Any pending zero run is discarded. Call [`finish`](Self::finish) first
    /// to flush pending zeros.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Flushes any pending zero run by seeking forward in the inner writer.
    ///
    /// This maintains the single-seek-per-zero-run invariant: all accumulated
    /// zeros are flushed as one seek operation.
    fn flush_pending_zeros(&mut self) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
        }

        let mut remaining = self.pending_zeros;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            self.inner.seek(SeekFrom::Current(step as i64))?;
            remaining -= step;
        }

        self.stats.bytes_seeked = self.stats.bytes_seeked.saturating_add(self.pending_zeros);
        self.pending_zeros = 0;
        Ok(())
    }

    /// Returns the leading zero count using the configured scan strategy.
    fn leading_zeros(&self, data: &[u8]) -> usize {
        match self.scan_strategy {
            ZeroScanStrategy::Simd => leading_zero_run(data),
            ZeroScanStrategy::ByteLevel => leading_zero_run_byte_level(data),
        }
    }

    /// Returns the trailing zero count using the configured scan strategy.
    fn trailing_zeros(&self, data: &[u8]) -> usize {
        match self.scan_strategy {
            ZeroScanStrategy::Simd => trailing_zero_run(data),
            ZeroScanStrategy::ByteLevel => trailing_zero_run_byte_level(data),
        }
    }

    /// Writes a chunk of data with sparse zero-run detection.
    ///
    /// Processes the buffer in `chunk_size` segments, detecting leading and
    /// trailing zero runs in each segment. Zero runs are accumulated rather
    /// than written, and flushed as seeks when non-zero data follows.
    fn write_sparse(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut offset = 0;

        while offset < buf.len() {
            let segment_end = (offset + self.chunk_size).min(buf.len());
            let segment = &buf[offset..segment_end];

            let leading = self.leading_zeros(segment);
            self.pending_zeros = self.pending_zeros.saturating_add(leading as u64);

            if leading == segment.len() {
                offset = segment_end;
                continue;
            }

            let trailing = self.trailing_zeros(&segment[leading..]);
            let data_start = offset + leading;
            let data_end = segment_end - trailing;

            if data_end > data_start {
                // Count zero runs: a new zero run was detected if pending > 0
                // before this flush (meaning we accumulated some zeros that are
                // now being flushed).
                if self.pending_zeros > 0 {
                    self.stats.zero_runs_detected = self.stats.zero_runs_detected.saturating_add(1);
                }
                self.flush_pending_zeros()?;
                self.inner.write_all(&buf[data_start..data_end])?;
                let written_len = (data_end - data_start) as u64;
                self.stats.bytes_written = self.stats.bytes_written.saturating_add(written_len);
            }

            self.pending_zeros = trailing as u64;
            offset = segment_end;
        }

        Ok(buf.len())
    }

    /// Finalizes sparse writing by flushing any remaining pending zeros.
    ///
    /// Returns the final stream position after all pending zeros have been
    /// flushed. The caller should use this position to set the file length
    /// (via `set_len`) to materialize trailing holes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if seeking or querying the stream position fails.
    pub fn finish(mut self) -> io::Result<(W, SparseWriteStats)> {
        if self.pending_zeros > 0 {
            self.stats.zero_runs_detected = self.stats.zero_runs_detected.saturating_add(1);
        }
        self.flush_pending_zeros()?;
        let stats = self.stats;
        Ok((self.inner, stats))
    }

    /// Finalizes and returns the final file position.
    ///
    /// Flushes pending zeros and returns the stream position for `set_len`.
    pub fn finish_and_position(mut self) -> io::Result<(W, u64, SparseWriteStats)> {
        if self.pending_zeros > 0 {
            self.stats.zero_runs_detected = self.stats.zero_runs_detected.saturating_add(1);
        }
        self.flush_pending_zeros()?;
        let pos = self.inner.stream_position()?;
        let stats = self.stats;
        Ok((self.inner, pos, stats))
    }
}

impl<W: Write + Seek> Write for SparseWriter<W> {
    /// Writes data with sparse zero-run interception.
    ///
    /// Zero regions are accumulated as pending seeks rather than written to the
    /// inner writer. Non-zero regions trigger a flush of any pending seeks
    /// followed by the actual write. Always reports the full buffer length as
    /// consumed, matching upstream rsync's `write_sparse()` semantics.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_sparse(buf)
    }

    /// Flushes the inner writer.
    ///
    /// Note: this does not flush pending zeros - they are flushed when non-zero
    /// data arrives or at finalization via [`finish`](Self::finish).
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Write + Seek> Seek for SparseWriter<W> {
    /// Seeks in the underlying writer after flushing any pending zeros.
    ///
    /// Pending zeros must be flushed before a seek to maintain correct file
    /// position tracking.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.pending_zeros > 0 {
            self.stats.zero_runs_detected = self.stats.zero_runs_detected.saturating_add(1);
        }
        self.flush_pending_zeros()?;
        self.inner.seek(pos)
    }
}

/// Byte-level leading zero run detection.
///
/// Simple byte-by-byte scan for environments where SIMD overhead is not
/// justified (small buffers, testing).
fn leading_zero_run_byte_level(data: &[u8]) -> usize {
    data.iter().take_while(|&&b| b == 0).count()
}

/// Byte-level trailing zero run detection.
fn trailing_zero_run_byte_level(data: &[u8]) -> usize {
    data.iter().rev().take_while(|&&b| b == 0).count()
}
