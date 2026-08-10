//! Zero-copy file writer that pushes already-demuxed literal chunks into a
//! destination file via `vmsplice(2)` + `splice(2)`.
//!
//! This module is the disk-write tail of integration shape B in
//! `docs/design/splice-vmsplice-zero-copy.md`: a userspace `Vec<u8>` payload
//! has already been read off the multiplex layer and the caller wants to land
//! the bytes in the page cache without paying a userspace-to-kernel `memcpy`.
//!
//! # Layering
//!
//! ```text
//! caller buffer (&[u8]) -> vmsplice -> pipe (kernel) -> splice -> file_fd
//! ```
//!
//! A single [`crate::splice::SplicePipe`] is held per writer for the lifetime
//! of one file so the pipe pair is reused across chunks; this matches step 3
//! of the design doc's implementation sequencing and keeps the fd budget at
//! two for the pipe plus one for the destination file.
//!
//! # When to use
//!
//! Per the design doc, the vmsplice path only wins over a buffered `write(2)`
//! or io_uring `WRITE_FIXED` when:
//!
//! - `io_uring` is unavailable or disabled, and
//! - The chunk is large enough to clear the per-`vmsplice` pipe-buffer ceiling
//!   and amortise the two-syscall pair (>= 64 KiB), and
//! - The chunk pointer is page-aligned so the kernel can move whole pages by
//!   reference instead of falling back to an internal copy.
//!
//! On Linux with the `vmsplice` feature, [`VmspliceFileWriter::write_chunk`]
//! enforces those gates and falls back to `std::fs::File::write_all`
//! otherwise. Any kernel-side rejection (`ErrorKind::Unsupported`,
//! `InvalidInput` mapped from `EINVAL`) also drops to the fallback so
//! unsupported filesystems (tmpfs/FUSE/NFS) keep working.
//!
//! # Feature gate
//!
//! The full implementation is compiled only on `cfg(all(target_os = "linux",
//! feature = "vmsplice"))`. On every other configuration the module exposes a
//! stub type whose constructor and `write_chunk` return
//! [`std::io::ErrorKind::Unsupported`], so callers can write
//! platform-independent code that compiles everywhere. The feature is
//! default-off until benchmarks on the trigger workload justify promotion.

use std::fs::File;
use std::io;

#[cfg(all(target_os = "linux", feature = "vmsplice"))]
use std::os::fd::{AsRawFd, RawFd};

#[cfg(all(target_os = "linux", feature = "vmsplice"))]
use crate::splice::{DEFAULT_PIPE_CAPACITY, SplicePipe, is_splice_available};

/// Minimum chunk size at which the vmsplice path is attempted.
///
/// Below this threshold the two-syscall pair (`vmsplice` + `splice`) costs
/// more than a single buffered `write(2)`. 64 KiB matches the default Linux
/// pipe-buffer capacity and the [`crate::splice`] module's
/// `SPLICE_THRESHOLD`.
pub const VMSPLICE_MIN_CHUNK: usize = 64 * 1024;

/// Page size assumed for the alignment check.
///
/// 4 KiB matches every Linux target tier-1 architecture (`x86_64`, `aarch64`,
/// `i686`, `armv7`). On targets with a larger native page size the check is
/// conservative: a 16 KiB-aligned buffer is also 4 KiB-aligned, so it still
/// takes the fast path; a 4 KiB-aligned buffer on a 16 KiB-page kernel would
/// take the fast path and vmsplice would internally copy, which is correct
/// (no UB) just not zero-copy.
#[cfg(all(target_os = "linux", feature = "vmsplice"))]
const ASSUMED_PAGE_SIZE: usize = 4096;

/// Writer that vmsplices userspace chunks into a destination file via an
/// owned [`SplicePipe`].
///
/// Construct one per destination file; the pipe pair is reused across all
/// chunks written to that file. Drop the writer (or call [`Self::into_file`])
/// before renaming or fsyncing the underlying file - the pipe and the file
/// are independent fds, so neither operation depends on the other, but the
/// caller still owns ordering.
#[cfg(all(target_os = "linux", feature = "vmsplice"))]
pub struct VmspliceFileWriter {
    pipe: SplicePipe,
    file: File,
    dest_fd: RawFd,
}

#[cfg(all(target_os = "linux", feature = "vmsplice"))]
impl VmspliceFileWriter {
    /// Wraps `file` with a freshly created pipe of the default capacity.
    ///
    /// # Errors
    ///
    /// Returns the error from [`SplicePipe::with_capacity`] when the pipe
    /// pair cannot be created (typically an `RLIMIT_NOFILE` exhaustion).
    pub fn new(file: File) -> io::Result<Self> {
        let pipe = SplicePipe::with_capacity(DEFAULT_PIPE_CAPACITY)?;
        let dest_fd = file.as_raw_fd();
        Ok(Self {
            pipe,
            file,
            dest_fd,
        })
    }

    /// Returns the actual pipe buffer capacity in bytes.
    ///
    /// The kernel may have granted less than [`DEFAULT_PIPE_CAPACITY`] when
    /// the process lacks `CAP_SYS_RESOURCE` or `/proc/sys/fs/pipe-max-size`
    /// is lower.
    #[must_use]
    pub fn pipe_capacity(&self) -> usize {
        self.pipe.capacity()
    }

    /// Returns the destination file descriptor.
    #[must_use]
    pub fn dest_fd(&self) -> RawFd {
        self.dest_fd
    }

    /// Returns a reference to the destination file.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Consumes the writer and returns the destination file.
    ///
    /// The owned pipe is dropped; the file is returned to the caller for any
    /// further fsync, truncate, or rename step.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Writes `chunk` to the destination file, taking the vmsplice path when
    /// it is expected to be zero-copy and falling back to a buffered write
    /// otherwise.
    ///
    /// The vmsplice path is selected when **all** of the following hold:
    ///
    /// - The kernel reports `splice(2)` available
    ///   ([`is_splice_available`] returns `true`).
    /// - `chunk.len() >= VMSPLICE_MIN_CHUNK` (>= 64 KiB).
    /// - The chunk pointer is page-aligned.
    ///
    /// Any other case, plus any kernel-side rejection
    /// (`ErrorKind::Unsupported`, `EINVAL` mapped to `InvalidInput`), routes
    /// to `File::write_all`.
    ///
    /// # Errors
    ///
    /// Returns the error from either path. The vmsplice path falls back to
    /// the write path on `Unsupported`/`InvalidInput`; any other error from
    /// `vmsplice`/`splice` propagates directly.
    pub fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<usize> {
        use std::io::Write;

        if chunk.is_empty() {
            return Ok(0);
        }

        if Self::should_vmsplice(chunk) {
            match self.pipe.vmsplice_to_file(chunk, self.dest_fd) {
                Ok(n) if n == chunk.len() => return Ok(n),
                Ok(n) => {
                    // Short transfer (e.g. pipe-capacity short of chunk): write
                    // the tail via the fallback path so the caller observes a
                    // full write.
                    self.file.write_all(&chunk[n..])?;
                    return Ok(chunk.len());
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
                    ) =>
                {
                    // Fall through to the buffered write path.
                }
                Err(e) => return Err(e),
            }
        }

        self.file.write_all(chunk)?;
        Ok(chunk.len())
    }

    /// Returns whether `chunk` is eligible for the vmsplice fast path.
    fn should_vmsplice(chunk: &[u8]) -> bool {
        if chunk.len() < VMSPLICE_MIN_CHUNK {
            return false;
        }
        if (chunk.as_ptr() as usize) % ASSUMED_PAGE_SIZE != 0 {
            return false;
        }
        is_splice_available()
    }
}

/// Stub for non-Linux platforms or when the `vmsplice` feature is disabled.
///
/// Every constructor and write method returns
/// [`std::io::ErrorKind::Unsupported`], allowing callers to compile a single
/// code path everywhere and probe availability at runtime.
#[cfg(not(all(target_os = "linux", feature = "vmsplice")))]
pub struct VmspliceFileWriter {
    _private: (),
}

#[cfg(not(all(target_os = "linux", feature = "vmsplice")))]
impl VmspliceFileWriter {
    /// Stub: always returns [`io::ErrorKind::Unsupported`].
    pub fn new(_file: File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "vmsplice writer requires Linux and the `vmsplice` cargo feature",
        ))
    }

    /// Stub: returns 0.
    #[must_use]
    pub fn pipe_capacity(&self) -> usize {
        0
    }

    /// Stub: always returns [`io::ErrorKind::Unsupported`].
    pub fn write_chunk(&mut self, _chunk: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "vmsplice writer requires Linux and the `vmsplice` cargo feature",
        ))
    }
}

#[cfg(all(test, target_os = "linux", feature = "vmsplice"))]
mod linux_tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Read;

    /// Allocates a `Vec<u8>` whose backing pointer is page-aligned.
    ///
    /// `Vec<u8>` carries the default `u8` alignment (1), so the standard
    /// allocator is free to place the buffer anywhere. This helper allocates
    /// with an explicit page-aligned `Layout`, fills the bytes, and hands
    /// the allocation to a `Vec` via `from_raw_parts` so the test can pass
    /// the buffer to [`VmspliceFileWriter::write_chunk`] and have the
    /// allocator free it on drop with the matching layout.
    #[allow(unsafe_code)]
    fn page_aligned_vec(len: usize, fill: u8) -> Vec<u8> {
        assert!(len > 0, "page_aligned_vec requires non-zero length");
        let layout = std::alloc::Layout::from_size_align(len, ASSUMED_PAGE_SIZE)
            .expect("layout for page-aligned buffer");
        // SAFETY: layout has non-zero size and a valid power-of-two alignment.
        // The allocation is handed to a Vec via from_raw_parts with matching
        // capacity, so the global allocator will free it with the same layout
        // on drop.
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null(), "allocation failed");
        // SAFETY: ptr is non-null and the allocation is len bytes; write_bytes
        // initialises every byte. The resulting Vec owns the allocation with
        // capacity == len, matching what alloc returned. Vec<u8> uses the
        // global allocator and frees with the original layout when dropped.
        unsafe {
            std::ptr::write_bytes(ptr, fill, len);
            Vec::from_raw_parts(ptr, len, len)
        }
    }

    fn read_back(path: &std::path::Path) -> Vec<u8> {
        let mut buf = Vec::new();
        File::open(path)
            .expect("reopen output")
            .read_to_end(&mut buf)
            .expect("read output");
        buf
    }

    #[test]
    fn writes_one_mib_chunk_byte_equal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("vmsplice_1mib.bin");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create file");

        let mut writer = VmspliceFileWriter::new(file).expect("vmsplice writer");
        let chunk = page_aligned_vec(1024 * 1024, 0xAB);
        let n = writer.write_chunk(&chunk).expect("write_chunk 1 MiB");
        assert_eq!(n, chunk.len());
        drop(writer);

        let actual = read_back(&path);
        assert_eq!(actual.len(), chunk.len());
        assert_eq!(actual, chunk);
    }

    #[test]
    fn small_chunk_falls_back_to_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("vmsplice_4kib.bin");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create file");

        let mut writer = VmspliceFileWriter::new(file).expect("vmsplice writer");
        // 4 KiB is below VMSPLICE_MIN_CHUNK, so should_vmsplice returns false
        // and the fallback write path runs regardless of alignment.
        let chunk = vec![0x5Au8; 4096];
        assert!(!VmspliceFileWriter::should_vmsplice(&chunk));

        let n = writer.write_chunk(&chunk).expect("write_chunk 4 KiB");
        assert_eq!(n, chunk.len());
        drop(writer);

        let actual = read_back(&path);
        assert_eq!(actual, chunk);
    }

    #[test]
    fn unaligned_large_chunk_falls_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("vmsplice_unaligned.bin");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create file");

        let mut writer = VmspliceFileWriter::new(file).expect("vmsplice writer");
        // Build a backing buffer one page longer than the slice we hand to
        // the writer, then slice from offset 1 so the data pointer is
        // guaranteed misaligned to ASSUMED_PAGE_SIZE.
        let mut backing = page_aligned_vec(128 * 1024 + ASSUMED_PAGE_SIZE, 0u8);
        for (i, b) in backing.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let unaligned = &backing[1..1 + 128 * 1024];
        assert!(!VmspliceFileWriter::should_vmsplice(unaligned));

        let n = writer
            .write_chunk(unaligned)
            .expect("write_chunk unaligned");
        assert_eq!(n, unaligned.len());
        drop(writer);

        let actual = read_back(&path);
        assert_eq!(actual, unaligned);
    }

    #[test]
    fn empty_chunk_is_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("vmsplice_empty.bin");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create file");

        let mut writer = VmspliceFileWriter::new(file).expect("vmsplice writer");
        let n = writer.write_chunk(&[]).expect("write_chunk empty");
        assert_eq!(n, 0);
        drop(writer);

        let actual = read_back(&path);
        assert!(actual.is_empty());
    }
}

#[cfg(all(test, not(all(target_os = "linux", feature = "vmsplice"))))]
mod stub_tests {
    use super::*;
    use std::fs::OpenOptions;

    #[test]
    fn stub_constructor_returns_unsupported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("stub.bin");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .expect("create file");

        let err = match VmspliceFileWriter::new(file) {
            Ok(_) => panic!("stub should fail"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
