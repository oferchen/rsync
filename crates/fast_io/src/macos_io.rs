//! macOS-optimized file writer using `F_NOCACHE` and `writev`.
//!
//! For large sequential writes, macOS's `F_NOCACHE` flag (via `fcntl`) bypasses
//! the unified buffer cache, reducing memory pressure and avoiding double-buffering
//! when the application already manages its own write buffers. Unlike Linux's
//! `O_DIRECT`, `F_NOCACHE` has no alignment requirements - the kernel handles
//! misaligned I/O transparently.
//!
//! The `writev` scatter-gather syscall reduces per-write syscall overhead by
//! batching multiple buffers into a single kernel transition. This is beneficial
//! when the caller accumulates several small buffers before flushing.
//!
//! # Size threshold
//!
//! `F_NOCACHE` is only applied to files above [`F_NOCACHE_THRESHOLD`] (1 MB).
//! For smaller files, the buffer cache is beneficial because the data fits in
//! memory and may be re-read soon. Above the threshold, sequential write patterns
//! dominate and cache bypass prevents evicting hot pages.
//!
//! # Fallback
//!
//! On non-macOS platforms, the public API compiles to standard buffered I/O
//! via the stub at the bottom of this file. All types and functions are present
//! on every platform so callers avoid `#[cfg]` branching.

use std::io::{self, Write};
use std::path::Path;

/// Minimum file size (in bytes) at which `F_NOCACHE` is applied.
///
/// Files below this threshold use standard buffered I/O. The 1 MB boundary
/// balances cache-bypass benefits for large sequential writes against the
/// cache hit advantage for small files that may be re-read.
pub const F_NOCACHE_THRESHOLD: u64 = 1024 * 1024;

/// Maximum number of `iovec` entries passed to a single `writev` call.
///
/// The POSIX `IOV_MAX` on macOS is 1024; we use a conservative value that
/// avoids stack overflow from large iovec arrays while still batching
/// enough buffers to amortize syscall overhead.
pub const MAX_IOV_COUNT: usize = 64;

/// macOS-optimized file writer.
///
/// Combines two optimizations for large sequential file writes:
///
/// - **`F_NOCACHE`** - set via `fcntl(fd, F_NOCACHE, 1)` on files whose
///   expected size exceeds [`F_NOCACHE_THRESHOLD`]. Bypasses the unified
///   buffer cache to reduce memory pressure for streaming workloads.
///
/// - **`writev`** - scatter-gather write that flushes multiple accumulated
///   buffers in a single syscall, reducing transition overhead compared to
///   individual `write` calls.
///
/// For files below the threshold, the writer delegates to standard buffered
/// I/O with no additional overhead.
#[cfg(target_os = "macos")]
pub struct MacosWriter {
    file: std::fs::File,
    bytes_written: u64,
    nocache_enabled: bool,
    /// Accumulated buffers waiting for a `writev` flush.
    pending: Vec<Vec<u8>>,
    /// Total bytes across all pending buffers.
    pending_bytes: usize,
    /// Flush threshold - when pending_bytes exceeds this, auto-flush via writev.
    flush_threshold: usize,
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
impl MacosWriter {
    /// Creates a new file for writing with optional `F_NOCACHE` optimization.
    ///
    /// When `size_hint` exceeds [`F_NOCACHE_THRESHOLD`], sets `F_NOCACHE` on
    /// the file descriptor to bypass the buffer cache. The file is created
    /// (or truncated) at the given path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created. An `fcntl` failure for
    /// `F_NOCACHE` is logged but does not fail the open - the writer falls
    /// back to cached I/O silently.
    pub fn create(path: &Path, size_hint: u64) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let nocache_enabled = if size_hint >= F_NOCACHE_THRESHOLD {
            Self::try_set_nocache(&file)
        } else {
            false
        };
        Ok(Self {
            file,
            bytes_written: 0,
            nocache_enabled,
            pending: Vec::new(),
            pending_bytes: 0,
            flush_threshold: 256 * 1024,
        })
    }

    /// Wraps an existing file handle with optional `F_NOCACHE`.
    ///
    /// The `size_hint` controls whether `F_NOCACHE` is applied. Use this
    /// when the file is already open (e.g., from a temp-file strategy).
    pub fn from_file(file: std::fs::File, size_hint: u64) -> Self {
        let nocache_enabled = if size_hint >= F_NOCACHE_THRESHOLD {
            Self::try_set_nocache(&file)
        } else {
            false
        };
        Self {
            file,
            bytes_written: 0,
            nocache_enabled,
            pending: Vec::new(),
            pending_bytes: 0,
            flush_threshold: 256 * 1024,
        }
    }

    /// Returns whether `F_NOCACHE` was successfully applied to this writer.
    #[must_use]
    pub fn is_nocache_enabled(&self) -> bool {
        self.nocache_enabled
    }

    /// Returns the total number of bytes written so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Flushes pending buffers and fsyncs the underlying file.
    ///
    /// Drains any accumulated `writev` buffers and then calls `sync_all` on
    /// the file descriptor so the data is persisted to durable storage.
    pub fn sync(&mut self) -> io::Result<()> {
        self.flush_writev()?;
        self.file.sync_all()
    }

    /// Attempts to set `F_NOCACHE` on the file descriptor.
    ///
    /// Returns `true` on success, `false` if `fcntl` fails (e.g., on a
    /// filesystem that does not support the flag).
    fn try_set_nocache(file: &std::fs::File) -> bool {
        use std::os::unix::io::AsRawFd;

        let fd = file.as_raw_fd();
        // SAFETY: `fd` is a valid open file descriptor owned by `file`.
        // `F_NOCACHE` is a macOS-specific fcntl command that accepts an int
        // argument (1 to enable, 0 to disable). It cannot corrupt state;
        // failure returns -1 with errno set.
        let ret = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
        ret != -1
    }

    /// Flushes all pending buffers using `writev` for scatter-gather I/O.
    ///
    /// Batches up to [`MAX_IOV_COUNT`] buffers per `writev` call. If there
    /// are more pending buffers, multiple `writev` calls are issued.
    fn flush_writev(&mut self) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        if self.pending.is_empty() {
            return Ok(());
        }

        let fd = self.file.as_raw_fd();
        let mut offset = 0;

        while offset < self.pending.len() {
            let end = (offset + MAX_IOV_COUNT).min(self.pending.len());
            let chunk = &self.pending[offset..end];

            let iovecs: Vec<libc::iovec> = chunk
                .iter()
                .map(|buf| libc::iovec {
                    iov_base: buf.as_ptr() as *mut libc::c_void,
                    iov_len: buf.len(),
                })
                .collect();

            // SAFETY: `fd` is a valid open file descriptor. `iovecs`
            // points to valid `iovec` structs whose `iov_base` pointers
            // reference live `Vec<u8>` buffers in `self.pending`. The buffers
            // outlive this call because they are not dropped until after the
            // loop. `iovcnt` is within `IOV_MAX`.
            let written = unsafe { libc::writev(fd, iovecs.as_ptr(), iovecs.len() as libc::c_int) };

            if written < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    // Retry this batch on interrupt.
                    continue;
                }
                // Clear pending on error to avoid double-flush from Drop.
                self.pending.clear();
                self.pending_bytes = 0;
                return Err(err);
            }

            let written = written as usize;
            let batch_total: usize = chunk.iter().map(|b| b.len()).sum();

            // If writev was partial, collect the remaining bytes and write
            // them via plain write(2). This is rare - writev on macOS
            // typically completes fully for reasonable buffer sizes.
            if written < batch_total {
                let mut skip = written;
                let mut remainder = Vec::with_capacity(batch_total - written);
                for buf in chunk {
                    if skip >= buf.len() {
                        skip -= buf.len();
                        continue;
                    }
                    remainder.extend_from_slice(&buf[skip..]);
                    skip = 0;
                }
                write_all_to_fd(fd, &remainder)?;
            }

            offset = end;
        }

        self.pending.clear();
        self.pending_bytes = 0;
        Ok(())
    }
}

/// Writes all bytes to a raw fd using `write(2)`, retrying on `EINTR`.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn write_all_to_fd(fd: libc::c_int, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: `fd` is a valid open file descriptor passed by the caller.
        // `buf` is a valid byte slice that outlives the call.
        let written = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if written < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        buf = &buf[written as usize..];
    }
    Ok(())
}

#[cfg(target_os = "macos")]
impl Write for MacosWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        self.pending.push(buf.to_vec());
        self.pending_bytes += buf.len();
        self.bytes_written += buf.len() as u64;

        if self.pending_bytes >= self.flush_threshold {
            self.flush_writev()?;
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_writev()
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacosWriter {
    fn drop(&mut self) {
        // Best-effort flush on drop; errors are silently discarded.
        let _ = self.flush_writev();
    }
}

/// Writes multiple buffers to a file descriptor using `writev`.
///
/// This is a standalone convenience function for callers that have a set of
/// buffers ready to write in a single syscall. For repeated writes, prefer
/// [`MacosWriter`] which accumulates buffers and auto-flushes.
///
/// Returns the total number of bytes written.
///
/// # Errors
///
/// Returns an I/O error if `writev` fails.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn writev_buffers(file: &std::fs::File, buffers: &[&[u8]]) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;

    if buffers.is_empty() {
        return Ok(0);
    }

    let fd = file.as_raw_fd();
    let iovecs: Vec<libc::iovec> = buffers
        .iter()
        .map(|buf| libc::iovec {
            iov_base: buf.as_ptr() as *mut libc::c_void,
            iov_len: buf.len(),
        })
        .collect();

    let total_expected: usize = buffers.iter().map(|b| b.len()).sum();
    let mut total_written: usize = 0;

    // Single writev call; caller can retry if partial.
    // SAFETY: `fd` is a valid open file descriptor borrowed from `file`.
    // `iovecs` contains valid pointers to the caller's `buffers` slices
    // which outlive this function call. The iovec count is within bounds.
    let written = unsafe { libc::writev(fd, iovecs.as_ptr(), iovecs.len() as libc::c_int) };

    if written < 0 {
        return Err(io::Error::last_os_error());
    }

    total_written += written as usize;

    // If partial, write remaining bytes via standard Write trait.
    if total_written < total_expected {
        let mut remaining = total_written;
        for buf in buffers {
            if remaining >= buf.len() {
                remaining -= buf.len();
                continue;
            }
            use std::io::Write;
            let mut file_ref = file;
            file_ref.write_all(&buf[remaining..])?;
            total_written += buf[remaining..].len();
            remaining = 0;
        }
    }

    Ok(total_written)
}

/// Sets `F_NOCACHE` on a file descriptor to bypass the buffer cache.
///
/// Returns `true` if the flag was successfully set, `false` otherwise.
/// This is a standalone utility for callers that manage their own file
/// handles but want the cache-bypass optimization for large writes.
///
/// # Platform support
///
/// - **macOS**: Calls `fcntl(fd, F_NOCACHE, 1)`.
/// - **Other platforms**: Always returns `false`.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn set_nocache(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;

    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open file descriptor. F_NOCACHE with argument 1
    // is a well-defined macOS fcntl operation.
    let ret = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
    ret != -1
}

/// Applies the macOS sequential-read advisory to a source file descriptor.
///
/// For large files read end-to-end (rsync's sender / local-copy path), the
/// unified buffer cache adds no benefit: each byte is read once and the data
/// will not be revisited soon. Without a hint, a single tree-wide transfer
/// can evict every other working set from the cache. macOS exposes
/// `fcntl(fd, F_NOCACHE, 1)` for exactly this case - it is the equivalent of
/// Linux's `posix_fadvise(POSIX_FADV_DONTNEED)` / `POSIX_FADV_NOREUSE`.
///
/// `F_RDADVISE` is intentionally **not** invoked here. That fcntl prefetches
/// a specific extent range (`struct radvisory { ra_offset, ra_count }`) and
/// is appropriate when the caller knows precisely which bytes will be read.
/// rsync's delta algorithm seeks unpredictably through the basis file, so a
/// blanket `F_RDADVISE` could waste prefetch bandwidth. Callers that already
/// know they will stream the file end-to-end can drive prefetch separately.
///
/// The hint is applied only when `size_hint >= F_NOCACHE_THRESHOLD`. Smaller
/// files fit comfortably in the cache and may legitimately be re-read.
///
/// Returns `true` when the hint was applied. Returns `false` when the file
/// is below the threshold, when the platform is not macOS, or when the
/// `fcntl` call fails (e.g., on a filesystem that rejects the flag). Failure
/// is silent because the hint is advisory.
///
/// # Reference
///
/// Apple's `fcntl(2)` man page documents `F_NOCACHE`:
/// "Turns data caching off/on. A non-zero value in arg turns data caching
///  off. A value of zero in arg turns data caching on."
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn apply_sequential_read_hint(file: &std::fs::File, size_hint: u64) -> bool {
    use std::os::unix::io::AsRawFd;

    if size_hint < F_NOCACHE_THRESHOLD {
        return false;
    }
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open file descriptor borrowed from `file`.
    // `F_NOCACHE` with argument 1 is well-defined on macOS and cannot
    // corrupt the descriptor or kernel state; failure returns -1 with errno
    // set, which we surface as `false`.
    let ret = unsafe { libc::fcntl(fd, libc::F_NOCACHE, 1) };
    ret != -1
}

/// Stub for non-macOS platforms. Always returns `false`.
#[cfg(not(target_os = "macos"))]
pub fn apply_sequential_read_hint(_file: &std::fs::File, _size_hint: u64) -> bool {
    false
}

/// Opens `path` read-only for a sequential single-pass scan.
///
/// On Windows the sequential-access caching hint
/// (`FILE_FLAG_SEQUENTIAL_SCAN`) is a `CreateFile`-time flag that cannot be
/// set on an already-open handle, so it must be applied here at open time;
/// it tells the cache manager to bias read-ahead for forward streaming and
/// to evict pages eagerly behind the read point. On every other platform
/// this is a plain [`std::fs::File::open`] - macOS applies its post-open
/// `F_NOCACHE` hint separately via [`apply_sequential_read_hint`], and Linux
/// has no equivalent open-time flag.
///
/// # Errors
///
/// Returns any error from opening the file.
#[cfg(windows)]
pub fn open_sequential_read(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(crate::win_path::to_extended_path(path))
}

/// Opens `path` read-only (non-Windows: a plain open; the read-ahead hint, if
/// any, is applied post-open by [`apply_sequential_read_hint`]).
///
/// # Errors
///
/// Returns any error from opening the file.
#[cfg(not(windows))]
pub fn open_sequential_read(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Queries whether `F_NOCACHE` is currently set on a file descriptor.
///
/// macOS does not provide a direct query API for `F_NOCACHE` - the `fcntl`
/// command is set-only. This function always returns `false`. Use
/// [`MacosWriter::is_nocache_enabled`] to check the tracked state instead.
#[cfg(target_os = "macos")]
pub fn is_nocache_set(_file: &std::fs::File) -> bool {
    // F_NOCACHE is a set-only fcntl on macOS; there is no corresponding
    // query command. The MacosWriter tracks this state internally.
    false
}

/// Stub `MacosWriter` for non-macOS platforms.
///
/// Delegates entirely to standard buffered I/O. All macOS-specific methods
/// are no-ops or return safe defaults so cross-platform code compiles
/// without `#[cfg]` branching at call sites.
#[cfg(not(target_os = "macos"))]
pub struct MacosWriter {
    inner: std::io::BufWriter<std::fs::File>,
    bytes_written: u64,
}

#[cfg(not(target_os = "macos"))]
impl MacosWriter {
    /// Creates a file for writing using standard buffered I/O.
    ///
    /// The `size_hint` is ignored on non-macOS platforms.
    pub fn create(path: &Path, _size_hint: u64) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        Ok(Self {
            inner: std::io::BufWriter::new(file),
            bytes_written: 0,
        })
    }

    /// Wraps an existing file handle using standard buffered I/O.
    pub fn from_file(file: std::fs::File, _size_hint: u64) -> Self {
        Self {
            inner: std::io::BufWriter::new(file),
            bytes_written: 0,
        }
    }

    /// Always returns `false` on non-macOS platforms.
    #[must_use]
    pub fn is_nocache_enabled(&self) -> bool {
        false
    }

    /// Returns the total number of bytes written so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Flushes buffered data and fsyncs the underlying file.
    pub fn sync(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        self.inner.get_ref().sync_all()
    }
}

#[cfg(not(target_os = "macos"))]
impl Write for MacosWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Stub `writev_buffers` for non-macOS platforms.
///
/// Falls back to sequential `write_all` calls for each buffer.
#[cfg(not(target_os = "macos"))]
pub fn writev_buffers(file: &std::fs::File, buffers: &[&[u8]]) -> io::Result<usize> {
    use std::io::Write;

    let mut total = 0;
    let mut file_ref = file;
    for buf in buffers {
        file_ref.write_all(buf)?;
        total += buf.len();
    }
    Ok(total)
}

/// Stub for non-macOS platforms. Always returns `false`.
#[cfg(not(target_os = "macos"))]
pub fn set_nocache(_file: &std::fs::File) -> bool {
    false
}

/// Stub for non-macOS platforms. Always returns `false`.
#[cfg(not(target_os = "macos"))]
pub fn is_nocache_set(_file: &std::fs::File) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn small_file_skips_nocache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.bin");

        let writer = MacosWriter::create(&path, 512).unwrap();

        #[cfg(target_os = "macos")]
        assert!(!writer.is_nocache_enabled());

        #[cfg(not(target_os = "macos"))]
        assert!(!writer.is_nocache_enabled());

        drop(writer);
    }

    #[test]
    fn large_file_enables_nocache_on_macos() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.bin");

        let writer = MacosWriter::create(&path, 2 * 1024 * 1024).unwrap();

        #[cfg(target_os = "macos")]
        assert!(writer.is_nocache_enabled());

        #[cfg(not(target_os = "macos"))]
        assert!(!writer.is_nocache_enabled());

        drop(writer);
    }

    #[test]
    fn threshold_boundary_below() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary_below.bin");

        let writer = MacosWriter::create(&path, F_NOCACHE_THRESHOLD - 1).unwrap();

        #[cfg(target_os = "macos")]
        assert!(!writer.is_nocache_enabled());

        #[cfg(not(target_os = "macos"))]
        assert!(!writer.is_nocache_enabled());

        drop(writer);
    }

    #[test]
    fn threshold_boundary_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary_exact.bin");

        let writer = MacosWriter::create(&path, F_NOCACHE_THRESHOLD).unwrap();

        #[cfg(target_os = "macos")]
        assert!(writer.is_nocache_enabled());

        #[cfg(not(target_os = "macos"))]
        assert!(!writer.is_nocache_enabled());

        drop(writer);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let data = b"hello, macos optimized writer!";

        {
            let mut writer = MacosWriter::create(&path, 0).unwrap();
            writer.write_all(data).unwrap();
            writer.flush().unwrap();
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, data);
    }

    #[test]
    fn write_large_payload_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large_roundtrip.bin");
        let data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 3) % 256) as u8).collect();

        {
            let mut writer = MacosWriter::create(&path, data.len() as u64).unwrap();
            writer.write_all(&data).unwrap();
            writer.flush().unwrap();
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content.len(), data.len());
        assert_eq!(content, data);
    }

    #[test]
    fn write_above_threshold_with_nocache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nocache_write.bin");
        let size = (F_NOCACHE_THRESHOLD + 4096) as usize;
        let data: Vec<u8> = (0..size).map(|i| ((i * 7 + 11) % 256) as u8).collect();

        {
            let mut writer = MacosWriter::create(&path, size as u64).unwrap();
            writer.write_all(&data).unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), size as u64);
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content.len(), size);
        assert_eq!(content, data);
    }

    #[test]
    fn multiple_small_writes_accumulate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi_write.bin");

        {
            let mut writer = MacosWriter::create(&path, 0).unwrap();
            for i in 0u8..100 {
                writer.write_all(&[i]).unwrap();
            }
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 100);
        }

        let content = std::fs::read(&path).unwrap();
        let expected: Vec<u8> = (0u8..100).collect();
        assert_eq!(content, expected);
    }

    #[test]
    fn bytes_written_tracks_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracking.bin");

        let mut writer = MacosWriter::create(&path, 0).unwrap();
        assert_eq!(writer.bytes_written(), 0);

        writer.write_all(b"hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);

        writer.write_all(b" world").unwrap();
        assert_eq!(writer.bytes_written(), 11);

        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 11);
    }

    #[test]
    fn from_file_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("from_file.bin");

        let file = std::fs::File::create(&path).unwrap();
        let mut writer = MacosWriter::from_file(file, 0);
        writer.write_all(b"from file").unwrap();
        writer.flush().unwrap();

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, b"from file");
    }

    #[test]
    fn from_file_large_enables_nocache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("from_file_large.bin");

        let file = std::fs::File::create(&path).unwrap();
        let writer = MacosWriter::from_file(file, 2 * 1024 * 1024);

        #[cfg(target_os = "macos")]
        assert!(writer.is_nocache_enabled());

        #[cfg(not(target_os = "macos"))]
        assert!(!writer.is_nocache_enabled());

        drop(writer);
    }

    #[test]
    fn empty_write_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");

        let mut writer = MacosWriter::create(&path, 0).unwrap();
        writer.write_all(b"").unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 0);

        let content = std::fs::read(&path).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn flush_on_drop_writes_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop_flush.bin");

        {
            let mut writer = MacosWriter::create(&path, 0).unwrap();
            writer.write_all(b"drop me").unwrap();
            // No explicit flush - drop should handle it
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, b"drop me");
    }

    #[test]
    fn writev_buffers_single_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writev_single.bin");

        let file = std::fs::File::create(&path).unwrap();
        let data = b"single buffer";
        let written = writev_buffers(&file, &[data.as_slice()]).unwrap();
        assert_eq!(written, data.len());

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, data);
    }

    #[test]
    fn writev_buffers_multiple_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writev_multi.bin");

        let file = std::fs::File::create(&path).unwrap();
        let bufs: Vec<&[u8]> = vec![b"hello", b" ", b"world", b"!"];
        let written = writev_buffers(&file, &bufs).unwrap();
        assert_eq!(written, 12);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, b"hello world!");
    }

    #[test]
    fn writev_buffers_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writev_empty.bin");

        let file = std::fs::File::create(&path).unwrap();
        let written = writev_buffers(&file, &[]).unwrap();
        assert_eq!(written, 0);
    }

    #[test]
    fn set_nocache_returns_expected_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nocache_test.bin");

        let file = std::fs::File::create(&path).unwrap();
        let result = set_nocache(&file);

        #[cfg(target_os = "macos")]
        assert!(result);

        #[cfg(not(target_os = "macos"))]
        assert!(!result);
    }

    #[test]
    fn is_nocache_set_returns_expected_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nocache_query.bin");

        let file = std::fs::File::create(&path).unwrap();
        let result = is_nocache_set(&file);

        // F_NOCACHE is not directly queryable on macOS, so this always
        // returns false. The writer tracks state internally.
        assert!(!result);
    }

    #[test]
    fn create_invalid_path_returns_error() {
        let result = MacosWriter::create(Path::new("/nonexistent/dir/file.txt"), 0);
        assert!(result.is_err());
    }

    #[test]
    fn f_nocache_threshold_is_one_megabyte() {
        assert_eq!(F_NOCACHE_THRESHOLD, 1024 * 1024);
    }

    #[test]
    fn max_iov_count_is_reasonable() {
        assert_eq!(MAX_IOV_COUNT, 64);
    }

    #[test]
    fn write_chunks_exceed_flush_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exceed_threshold.bin");

        // Write enough data to trigger auto-flush (default 256 KB threshold)
        let chunk: Vec<u8> = (0..64 * 1024).map(|i| (i % 256) as u8).collect();
        {
            let mut writer = MacosWriter::create(&path, 0).unwrap();
            for _ in 0..8 {
                writer.write_all(&chunk).unwrap();
            }
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 8 * 64 * 1024);
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content.len(), 8 * 64 * 1024);
        for (i, actual_chunk) in content.chunks(64 * 1024).enumerate() {
            assert_eq!(actual_chunk, &chunk[..], "chunk {i} mismatch");
        }
    }

    #[test]
    fn writev_buffers_large_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writev_large.bin");

        let file = std::fs::File::create(&path).unwrap();
        let buf1: Vec<u8> = (0..32768).map(|i| (i % 256) as u8).collect();
        let buf2: Vec<u8> = (0..32768).map(|i| ((i + 1) % 256) as u8).collect();
        let bufs: Vec<&[u8]> = vec![&buf1, &buf2];

        let written = writev_buffers(&file, &bufs).unwrap();
        assert_eq!(written, 65536);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content[..32768], &buf1[..]);
        assert_eq!(&content[32768..], &buf2[..]);
    }

    #[test]
    fn multiple_flush_calls_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi_flush.bin");

        let mut writer = MacosWriter::create(&path, 0).unwrap();
        writer.write_all(b"data").unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, b"data");
    }

    #[test]
    fn apply_sequential_read_hint_below_threshold_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small_source.bin");
        std::fs::write(&path, b"tiny").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        // Below the threshold the hint is intentionally not applied,
        // regardless of platform.
        assert!(!apply_sequential_read_hint(&file, 512));
    }

    #[test]
    fn apply_sequential_read_hint_large_file_matches_platform() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large_source.bin");
        std::fs::write(&path, b"placeholder").unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let applied = apply_sequential_read_hint(&file, F_NOCACHE_THRESHOLD);

        // On macOS the fcntl call should succeed for a regular file on a
        // local filesystem. On every other platform the helper is a stub
        // and must return false.
        #[cfg(target_os = "macos")]
        assert!(applied);

        #[cfg(not(target_os = "macos"))]
        assert!(!applied);
    }

    #[test]
    fn write_interleaved_with_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interleaved.bin");

        let mut writer = MacosWriter::create(&path, 0).unwrap();
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();
        writer.write_all(b"second").unwrap();
        writer.flush().unwrap();
        writer.write_all(b"third").unwrap();
        writer.flush().unwrap();

        assert_eq!(writer.bytes_written(), 16);

        drop(writer);
        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content, b"firstsecondthird");
    }

    #[test]
    fn open_sequential_read_returns_readable_file() {
        // The sequential-scan open must behave as a normal read-only open on
        // every platform: the Windows variant only adds a caching hint, never
        // changes the bytes returned. (The Windows flag path itself is
        // exercised by the Windows CI matrix.)
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seq.bin");
        std::fs::write(&path, b"sequential-scan-payload").unwrap();

        let mut file = open_sequential_read(&path).expect("open_sequential_read");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read back");
        assert_eq!(buf, b"sequential-scan-payload");
    }
}
