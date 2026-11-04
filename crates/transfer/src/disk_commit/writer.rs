//! Buffered writer with vectored I/O and direct-write bypass.
//!
//! Provides `ReusableBufWriter` which reuses an externally-owned buffer,
//! matching upstream rsync's static `wf_writeBuf` (fileio.c:262-266). Large
//! chunks bypass the buffer entirely via `write_all_vectored`.
//!
//! The [`Writer`] enum dispatches between `ReusableBufWriter`,
//! [`fast_io::IoUringDiskBatch`] (Linux 5.6+ with the `io_uring` feature),
//! and [`fast_io::IocpDiskBatch`] (Windows with the `iocp` feature) so the
//! disk-commit hot path can use batched async submissions when available
//! while preserving identical semantics for sparse mode and platforms
//! without a batched backend.

use std::fs;
use std::io::{self, IoSlice, Seek, Write};
use std::path::Path;

use crate::temp_guard::CommitOp;

/// Fixed write buffer size matching upstream's `wf_writeBufSize = WRITE_SIZE * 8`
/// (fileio.c:262-266). Upstream always uses 256 KB regardless of file size.
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
/// (fileio.c:262-266).
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

    /// Releases the file. Callers flush first; nothing buffered is written here.
    pub(super) fn into_file(self) -> fs::File {
        self.file
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
/// active file via `begin_file`. `Iocp` is the Windows analogue, borrowing
/// the persistent [`fast_io::IocpDiskBatch`]. `Macos` wraps the file in
/// [`fast_io::MacosWriter`], which pairs `F_NOCACHE` (for files above 1 MiB)
/// with `writev(2)` scatter-gather flushes.
///
/// Sparse mode requires `Seek`, which none of `IoUring`, `Iocp`, `Macos`, or
/// `MacosGcd` provide, so callers must select `Buffered` whenever `use_sparse`
/// is set or `append_offset` is non-zero.
pub(super) enum Writer<'a> {
    Buffered(ReusableBufWriter<'a>),
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    IoUring {
        batch: &'a mut fast_io::IoUringDiskBatch,
    },
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    Iocp {
        batch: &'a mut fast_io::IocpDiskBatch,
    },
    #[cfg(target_os = "macos")]
    Macos(fast_io::MacosWriter),
    /// Grand Central Dispatch (`dispatch_io`) async writer (macOS + the
    /// default-off `macos-gcd` feature). Selected in preference to `Macos`
    /// only for non-sparse, non-append writes; sparse/append need `Seek`,
    /// which the channel does not provide, so those fall back to `Buffered`.
    /// Each `write_chunk` submits one `dispatch_io_write` and blocks until it
    /// drains, so its byte content and fsync semantics match the buffered
    /// path. See `fast_io::GcdWriter`.
    #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
    MacosGcd(fast_io::GcdWriter),
    /// vmsplice zero-copy writer for already-demuxed literal chunks. Selected
    /// only when `io_uring` is not engaged (Linux + `vmsplice` feature, non-
    /// sparse, non-append). Per-chunk it falls back to a buffered write when
    /// the chunk is small or unaligned. See `fast_io::VmspliceFileWriter` and
    /// `docs/design/splice-vmsplice-zero-copy.md`.
    #[cfg(all(target_os = "linux", feature = "vmsplice"))]
    Vmsplice(fast_io::VmspliceFileWriter),
    /// Uncached bulk writer that lands literal chunks via
    /// `pwritev2(RWF_DONTCACHE)` (Linux + `dontcache` feature, non-sparse,
    /// non-append) so large transfers do not evict the page-cache working set.
    /// Per-chunk it falls back to a buffered write when the kernel or
    /// filesystem rejects the flag. See `fast_io::DontcacheFileWriter`.
    #[cfg(all(target_os = "linux", feature = "dontcache"))]
    Dontcache(fast_io::DontcacheFileWriter),
}

impl<'a> Writer<'a> {
    /// Returns a `Write + Seek` view of the buffered writer for sparse mode.
    ///
    /// Sparse writes require `Seek` to punch holes via `seek(Current(n))`,
    /// which neither the io_uring nor IOCP batch writer supports. Sparse
    /// mode therefore always uses the buffered variant; this accessor
    /// enforces that at the type level.
    pub(super) fn buffered_for_sparse(&mut self) -> &mut ReusableBufWriter<'a> {
        match self {
            Writer::Buffered(w) => w,
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { .. } => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
            #[cfg(all(target_os = "windows", feature = "iocp"))]
            Writer::Iocp { .. } => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
            #[cfg(target_os = "macos")]
            Writer::Macos(_) => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
            #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
            Writer::MacosGcd(_) => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
            #[cfg(all(target_os = "linux", feature = "vmsplice"))]
            Writer::Vmsplice(_) => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
            #[cfg(all(target_os = "linux", feature = "dontcache"))]
            Writer::Dontcache(_) => {
                debug_assert!(false, "sparse mode must select buffered writer");
                unreachable!("sparse mode must select buffered writer")
            }
        }
    }

    /// Seeks past `len` bytes that are already correct in the destination,
    /// instead of rewriting them (the `--inplace` matched-at-same-offset case).
    ///
    /// Mirrors upstream `skip_matched()` (`fileio.c:294-314`): flush any pending
    /// buffered bytes, then advance the file position by `len` via
    /// `lseek(SEEK_CUR)`. The buffered writer's [`Seek`] impl performs exactly
    /// that flush-then-seek. Only the buffered writer is used for in-place
    /// updates (`make_writer` forces it whenever `is_inplace` is set), because
    /// the batched backends submit at their own internally tracked offset and
    /// cannot honor an intervening seek; the other variants are therefore
    /// unreachable here.
    ///
    /// A failure is tagged [`CommitOp::Write`] against `file_path`, the
    /// destination name upstream's `write failed on` line reports. A failed
    /// seek also carries `offset`, the position the skip started from, for the
    /// `lseek returned %s, not %s` line `skip_matched()` prints before it
    /// (`fileio.c:307-310`, which reports that offset, not the target).
    pub(super) fn skip_matched(
        &mut self,
        len: u64,
        file_path: &Path,
        offset: u64,
    ) -> io::Result<()> {
        match self {
            Writer::Buffered(w) => {
                w.flush().map_err(|e| write_failed(file_path, e))?;
                w.seek(io::SeekFrom::Current(len as i64)).map_err(|e| {
                    crate::temp_guard::attach_commit_op_at(CommitOp::Write, file_path, offset, e)
                })?;
                Ok(())
            }
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { .. } => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
            #[cfg(all(target_os = "windows", feature = "iocp"))]
            Writer::Iocp { .. } => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
            #[cfg(target_os = "macos")]
            Writer::Macos(_) => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
            #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
            Writer::MacosGcd(_) => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
            #[cfg(all(target_os = "linux", feature = "vmsplice"))]
            Writer::Vmsplice(_) => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
            #[cfg(all(target_os = "linux", feature = "dontcache"))]
            Writer::Dontcache(_) => {
                debug_assert!(false, "in-place skip must select buffered writer");
                unreachable!("in-place skip must select buffered writer")
            }
        }
    }

    /// Writes the entire chunk, dispatching to the active variant.
    pub(super) fn write_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Writer::Buffered(w) => w.write_all(data),
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { batch } => batch.write_data(data),
            #[cfg(all(target_os = "windows", feature = "iocp"))]
            Writer::Iocp { batch } => batch.write_data(data),
            #[cfg(target_os = "macos")]
            Writer::Macos(w) => w.write_all(data),
            #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
            Writer::MacosGcd(w) => w.write_all(data),
            #[cfg(all(target_os = "linux", feature = "vmsplice"))]
            Writer::Vmsplice(w) => w.write_chunk(data).map(|_| ()),
            #[cfg(all(target_os = "linux", feature = "dontcache"))]
            Writer::Dontcache(w) => w.write_chunk(data).map(|_| ()),
        }
    }

    /// Flushes pending data and, when requested, fsyncs the underlying file.
    ///
    /// For the io_uring and IOCP variants this is a no-op: both flush and
    /// fsync are performed atomically by [`Self::finish`] via the batch's
    /// `commit_file(do_fsync)`, avoiding a redundant submission.
    pub(super) fn flush_and_sync(&mut self, do_fsync: bool, file_path: &Path) -> io::Result<()> {
        match self {
            Writer::Buffered(w) => {
                w.flush().map_err(|e| write_failed(file_path, e))?;
                if do_fsync {
                    w.sync().map_err(|e| fsync_failed(file_path, e))?;
                }
                Ok(())
            }
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { .. } => Ok(()),
            #[cfg(all(target_os = "windows", feature = "iocp"))]
            Writer::Iocp { .. } => Ok(()),
            #[cfg(target_os = "macos")]
            Writer::Macos(w) => {
                w.flush().map_err(|e| write_failed(file_path, e))?;
                if do_fsync {
                    w.sync().map_err(|e| fsync_failed(file_path, e))?;
                }
                Ok(())
            }
            #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
            Writer::MacosGcd(w) => {
                // Each `write_chunk` already drove its `dispatch_io_write` to
                // completion, so there is no userspace buffer to flush; the
                // non-fsync case is a no-op. The fsync path forces the
                // durability barrier on the channel-owned descriptor.
                if do_fsync {
                    w.sync_all().map_err(|e| fsync_failed(file_path, e))?;
                }
                Ok(())
            }
            #[cfg(all(target_os = "linux", feature = "vmsplice"))]
            Writer::Vmsplice(w) => {
                // vmsplice writes go straight to the file fd via the kernel,
                // so there is no userspace buffer to flush. The fsync, when
                // requested, syncs the file the writer owns.
                if do_fsync {
                    w.file()
                        .sync_all()
                        .map_err(|e| fsync_failed(file_path, e))?;
                }
                Ok(())
            }
            #[cfg(all(target_os = "linux", feature = "dontcache"))]
            Writer::Dontcache(w) => {
                // pwritev2 writes go straight to the file fd, so there is no
                // userspace buffer to flush. The fsync, when requested, syncs
                // the file the writer owns.
                if do_fsync {
                    w.file()
                        .sync_all()
                        .map_err(|e| fsync_failed(file_path, e))?;
                }
                Ok(())
            }
        }
    }

    /// Releases the writer and hands back the file it wrote, still open, so
    /// the caller can close it with [`close_received_file`].
    ///
    /// The batched variants first call `commit_file` to flush, optionally
    /// fsync, and detach the file from the batch. `None` only for the GCD
    /// writer, whose dispatch channel owns and closes the descriptor itself.
    #[cfg_attr(
        not(any(
            all(target_os = "linux", feature = "io_uring"),
            all(target_os = "windows", feature = "iocp"),
        )),
        allow(unused_variables)
    )]
    pub(super) fn finish(self, do_fsync: bool, file_path: &Path) -> io::Result<Option<fs::File>> {
        let file = match self {
            Writer::Buffered(w) => w.into_file(),
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            Writer::IoUring { batch } => {
                batch
                    .commit_file_staged(do_fsync)
                    .map_err(|failure| commit_failed(file_path, failure))?
                    .0
            }
            #[cfg(all(target_os = "windows", feature = "iocp"))]
            Writer::Iocp { batch } => {
                batch
                    .commit_file_staged(do_fsync)
                    .map_err(|failure| commit_failed(file_path, failure))?
                    .0
            }
            #[cfg(target_os = "macos")]
            Writer::Macos(w) => w.into_file()?,
            #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
            Writer::MacosGcd(_) => return Ok(None),
            #[cfg(all(target_os = "linux", feature = "vmsplice"))]
            Writer::Vmsplice(w) => w.into_file(),
            #[cfg(all(target_os = "linux", feature = "dontcache"))]
            Writer::Dontcache(w) => w.into_file(),
        };
        Ok(Some(file))
    }
}

/// Tags a failed write of received data, reported as upstream's
/// `write failed on %s` (`receiver.c:658-660`).
pub(super) fn write_failed(file_path: &Path, error: io::Error) -> io::Error {
    crate::temp_guard::attach_commit_op(CommitOp::Write, file_path, error)
}

/// Tags a failed `--fsync`, reported as upstream's `fsync failed on %s`
/// (`receiver.c:679-681`).
fn fsync_failed(file_path: &Path, error: io::Error) -> io::Error {
    crate::temp_guard::attach_commit_op(CommitOp::Fsync, file_path, error)
}

/// Tags a failed batched commit by the stage that failed: the drain of its
/// buffered writes or its fsync.
#[cfg(any(
    all(target_os = "linux", feature = "io_uring"),
    all(target_os = "windows", feature = "iocp"),
))]
fn commit_failed(file_path: &Path, failure: fast_io::CommitFailure) -> io::Error {
    match failure {
        fast_io::CommitFailure::Write(e) => write_failed(file_path, e),
        fast_io::CommitFailure::Fsync(e) => fsync_failed(file_path, e),
    }
}

/// Closes a received file and fails the transfer if `close(2)` reports an
/// error, tagged [`CommitOp::Close`](crate::temp_guard::CommitOp::Close) so it
/// is reported as upstream's `close failed on` and exits `RERR_FILEIO`.
///
/// `written` names the file that was written: the temp file, or the
/// destination itself for an in-place update (upstream's `fnametmp`).
///
/// upstream: receiver.c:1296-1300 - `if (close(fd2) < 0) { rsyserr(FERROR,
/// errno, "close failed on %s", full_fname(fnametmp));
/// exit_cleanup(RERR_FILEIO); }`.
pub(crate) fn close_received_file(file: Option<fs::File>, written: &Path) -> io::Result<()> {
    let Some(file) = file else {
        return Ok(());
    };
    close_seam(file).map_err(|e| {
        crate::temp_guard::attach_commit_op(crate::temp_guard::CommitOp::Close, written, e)
    })
}

#[cfg(not(test))]
fn close_seam(file: fs::File) -> io::Result<()> {
    fast_io::close_file(file)
}

/// Test build: honours a fault armed with [`close_fault::arm`] before
/// delegating to the real close.
#[cfg(test)]
fn close_seam(file: fs::File) -> io::Result<()> {
    match close_fault::take() {
        Some(error) => {
            drop(file);
            Err(error)
        }
        None => fast_io::close_file(file),
    }
}

/// Fault seam for the received-file close: no filesystem the test suite can
/// rely on makes `close(2)` fail on demand, so the next close is told to.
///
/// Process-wide rather than thread-local because the close runs on the disk
/// thread; nextest runs each test in its own process, so an armed fault
/// cannot leak into another test.
#[cfg(test)]
pub(crate) mod close_fault {
    use std::io;
    use std::sync::atomic::{AtomicI32, Ordering};

    static ERRNO: AtomicI32 = AtomicI32::new(0);

    /// Makes the next received-file close fail with `errno`. Armed only by the
    /// unix tests, which can name the errno and its text.
    #[cfg(unix)]
    pub(crate) fn arm(errno: i32) {
        ERRNO.store(errno, Ordering::SeqCst);
    }

    pub(super) fn take() -> Option<io::Error> {
        match ERRNO.swap(0, Ordering::SeqCst) {
            0 => None,
            errno => Some(io::Error::from_raw_os_error(errno)),
        }
    }
}

/// Each receive-path failure carries the operation upstream reports it as.
/// `/dev/full` fails every write with ENOSPC and `fsync(2)` with EINVAL, and a
/// pipe refuses `lseek(2)`, so each site fails for real.
#[cfg(all(test, target_os = "linux"))]
mod failure_site_tests {
    use super::*;

    fn dev_full() -> fs::File {
        fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open /dev/full")
    }

    fn op_of(error: &io::Error) -> (CommitOp, Option<u64>) {
        let (op, _) = crate::temp_guard::commit_op_failure(error).expect("tagged");
        (op, crate::temp_guard::commit_op_offset(error))
    }

    /// upstream: receiver.c:652-660 - a failed flush_write_file() is reported
    /// as the write it completes.
    #[test]
    fn a_failed_flush_is_a_write_failure() {
        let mut buf = Vec::with_capacity(WRITE_BUF_SIZE);
        let mut writer = Writer::Buffered(ReusableBufWriter::new(dev_full(), &mut buf));
        writer.write_chunk(b"payload").expect("buffered");

        let error = writer
            .flush_and_sync(false, Path::new("dest/a"))
            .expect_err("/dev/full refuses the flush");

        assert_eq!(op_of(&error), (CommitOp::Write, None));
        assert_eq!(
            error.raw_os_error(),
            None,
            "tagged errors keep the errno inside"
        );
        assert_eq!(
            logging::upstream_errno_text(&error),
            logging::upstream_errno_text(&io::Error::from_raw_os_error(28))
        );
    }

    /// The flush before an `--fsync` is still the write: upstream flushes at
    /// receiver.c:652 and syncs only later, at receiver.c:679.
    #[test]
    fn a_failed_flush_under_fsync_is_a_write_failure() {
        let mut buf = Vec::with_capacity(WRITE_BUF_SIZE);
        let mut writer = Writer::Buffered(ReusableBufWriter::new(dev_full(), &mut buf));
        writer.write_chunk(b"payload").expect("buffered");

        let error = writer
            .flush_and_sync(true, Path::new("dest/a"))
            .expect_err("/dev/full refuses the flush");

        assert_eq!(op_of(&error), (CommitOp::Write, None));
    }

    /// upstream: receiver.c:679-681 - `fsync failed on %s`.
    #[test]
    fn a_failed_fsync_is_an_fsync_failure() {
        let mut buf = Vec::with_capacity(WRITE_BUF_SIZE);
        let mut writer = Writer::Buffered(ReusableBufWriter::new(dev_full(), &mut buf));

        let error = writer
            .flush_and_sync(true, Path::new("dest/a"))
            .expect_err("/dev/full refuses fsync");

        assert_eq!(op_of(&error), (CommitOp::Fsync, None));
        assert_eq!(crate::error::rerr_for_io_error(&error), 11);
    }

    /// upstream: fileio.c:304-311 - a failed lseek past a matched block reports
    /// the offset it started from, then the write failure.
    #[test]
    fn a_failed_skip_seek_carries_its_offset() {
        let (_reader, pipe) = std::io::pipe().expect("pipe");
        let file = fs::File::from(std::os::fd::OwnedFd::from(pipe));
        let mut buf = Vec::with_capacity(WRITE_BUF_SIZE);
        let mut writer = Writer::Buffered(ReusableBufWriter::new(file, &mut buf));

        let error = writer
            .skip_matched(512, Path::new("dest/a"), 4096)
            .expect_err("a pipe cannot seek");

        assert_eq!(op_of(&error), (CommitOp::Write, Some(4096)));
    }

    /// A skip whose flush fails never reached the lseek, so it reports only the
    /// write (fileio.c:304-305 returns before the lseek).
    #[test]
    fn a_failed_skip_flush_carries_no_offset() {
        let mut buf = Vec::with_capacity(WRITE_BUF_SIZE);
        let mut writer = Writer::Buffered(ReusableBufWriter::new(dev_full(), &mut buf));
        writer.write_chunk(b"payload").expect("buffered");

        let error = writer
            .skip_matched(512, Path::new("dest/a"), 4096)
            .expect_err("/dev/full refuses the flush");

        assert_eq!(op_of(&error), (CommitOp::Write, None));
    }
}
