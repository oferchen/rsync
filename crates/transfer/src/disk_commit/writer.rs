//! Buffered writer with vectored I/O and direct-write bypass.
//!
//! Provides `ReusableBufWriter` which reuses an externally-owned buffer,
//! matching upstream rsync's static `wf_writeBuf` (fileio.c:161). Large
//! chunks bypass the buffer entirely via `write_all_vectored`.
//!
//! The [`Writer`] enum dispatches between `ReusableBufWriter` and
//! [`fast_io::IoUringDiskBatch`] so the disk-commit hot path can use batched
//! io_uring submissions when available (Linux 5.6+ with the `io_uring`
//! feature) while preserving identical semantics for sparse mode and
//! non-Linux platforms.

use std::fs;
use std::io::{self, IoSlice, Seek, Write};
use std::path::Path;

/// Fixed write buffer size matching upstream's `wf_writeBufSize = WRITE_SIZE * 8`
/// (fileio.c:161). Upstream always uses 256 KB regardless of file size.
pub(super) const WRITE_BUF_SIZE: usize = 256 * 1024;

/// Minimum chunk size for direct-to-file writes, bypassing the buffer.
///
/// Chunks at or above this size are written directly to the file descriptor,
/// eliminating one `memcpy` from the hot path. Smaller chunks are still
/// buffered to amortize syscall overhead for tiny delta tokens.
///
/// 8 KB balances syscall cost (~100-200 ns) against copy cost (~200-400 ns
/// for 8 KB in L1/L2 cache). Most rsync literal tokens are 32 KB+, so this
/// threshold catches the common case.
const DIRECT_WRITE_THRESHOLD: usize = 8 * 1024;

/// Writes two buffers as a single `writev` syscall, falling back to
/// sequential `write_all` if vectored I/O is unsupported.
fn write_all_vectored(file: &mut fs::File, first: &[u8], second: &[u8]) -> io::Result<()> {
    let total = first.len() + second.len();
    let mut written = 0usize;

    while written < first.len() {
        let bufs = [IoSlice::new(&first[written..]), IoSlice::new(second)];
        match file.write_vectored(&bufs) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_vectored returned 0",
                ));
            }
            Ok(n) => written += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    let second_offset = written - first.len();
    if second_offset < second.len() {
        file.write_all(&second[second_offset..])?;
    }

    debug_assert_eq!(
        first.len() + second.len(),
        total,
        "write_all_vectored size mismatch"
    );
    Ok(())
}

/// Buffered writer that reuses an externally-owned `Vec<u8>`, avoiding
/// per-file allocation. The buffer is allocated once in `disk_thread_main`
/// and cleared between files - matching upstream rsync's static `wf_writeBuf`
/// (fileio.c:161).
pub(super) struct ReusableBufWriter<'a> {
    file: fs::File,
    buf: &'a mut Vec<u8>,
}

impl<'a> ReusableBufWriter<'a> {
    /// Creates a new writer wrapping the given file with a reusable buffer.
    pub(super) fn new(file: fs::File, buf: &'a mut Vec<u8>) -> Self {
        buf.clear();
        Self { file, buf }
    }

    /// Flushes buffered data and calls `sync_all` on the underlying file.
    pub(super) fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        self.file.sync_all()
    }
}

impl Write for ReusableBufWriter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() >= DIRECT_WRITE_THRESHOLD {
            if !self.buf.is_empty() {
                // Combine buffered data and new chunk in a single writev
                // syscall, halving the write count for the common case of
                // small buffered data followed by a large literal token.
                write_all_vectored(&mut self.file, self.buf, data)?;
                self.buf.clear();
            } else {
                self.file.write_all(data)?;
            }
            return Ok(data.len());
        }

        if self.buf.len() + data.len() <= self.buf.capacity() {
            self.buf.extend_from_slice(data);
        } else {
            self.file.write_all(self.buf)?;
            self.buf.clear();
            self.buf.extend_from_slice(data);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.file.write_all(self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

impl Seek for ReusableBufWriter<'_> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.flush()?;
        self.file.seek(pos)
    }
}

/// Writer variant used by the disk-commit thread for a single file.
///
/// `Buffered` uses [`ReusableBufWriter`] backed by the thread's reusable
/// 256 KB buffer. `IoUring` borrows the disk thread's persistent
/// [`fast_io::IoUringDiskBatch`] which has already been registered with the
/// active file via `begin_file`.
///
/// Sparse mode requires `Seek`, which `IoUringDiskBatch` does not provide,
/// so callers must select `Buffered` whenever `use_sparse` is set.
pub(super) enum Writer<'a> {
    Buffered(ReusableBufWriter<'a>),
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    IoUring {
        batch: &'a mut fast_io::IoUringDiskBatch,
    },
}

impl<'a> Writer<'a> {
    /// Returns a `Write + Seek` view of the buffered writer for sparse mode.
    ///
    /// Sparse writes require `Seek` to punch holes via `seek(Current(n))`,
    /// which io_uring's batch writer does not support. Sparse mode therefore
    /// always uses the buffered variant; this accessor enforces that at the
    /// type level.
    pub(super) fn buffered_for_sparse(&mut self) -> &mut ReusableBufWriter<'a> {
        match self {
            Writer::Buffered(w) => w,
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { .. } => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
        }
    }

    /// Writes the entire chunk, dispatching to the active variant.
    pub(super) fn write_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Writer::Buffered(w) => w.write_all(data),
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { batch } => batch.write_data(data),
        }
    }

    /// Flushes pending data and, when requested, fsyncs the underlying file.
    ///
    /// For the io_uring variant this is a no-op: both flush and fsync are
    /// performed atomically by [`Self::finish`] via the batch's
    /// `commit_file(do_fsync)`, avoiding a redundant ring submission.
    pub(super) fn flush_and_sync(&mut self, do_fsync: bool, file_path: &Path) -> io::Result<()> {
        match self {
            Writer::Buffered(w) => {
                if do_fsync {
                    w.sync().map_err(|e| {
                        io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}"))
                    })
                } else {
                    w.flush().map_err(|e| {
                        io::Error::other(format!("flush failed for {file_path:?}: {e}"))
                    })
                }
            }
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { .. } => Ok(()),
        }
    }

    /// Releases the writer and, for io_uring, commits the active file (with
    /// optional fsync) so its file handle can be used for rename/truncate.
    ///
    /// The buffered variant simply drops, closing the file. The io_uring
    /// variant calls `commit_file` to flush, optionally fsync, and detach the
    /// file from the batch.
    pub(super) fn finish(self, do_fsync: bool, file_path: &Path) -> io::Result<()> {
        match self {
            Writer::Buffered(_) => Ok(()),
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { batch } => batch.commit_file(do_fsync).map(|_| ()).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("io_uring commit failed for {file_path:?}: {e}"),
                )
            }),
        }
    }
}
