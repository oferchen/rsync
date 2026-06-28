//! Uncached bulk file writer that lands literal chunks via `pwritev2(2)` with
//! the `RWF_DONTCACHE` flag, so large transfers do not evict the page-cache
//! working set.
//!
//! # Why
//!
//! A streaming receiver that writes many gigabytes through `write(2)` fills the
//! page cache with pages it will never read again, evicting the resident
//! working set of the rest of the system. `RWF_DONTCACHE` (Linux 6.14+) asks
//! the kernel to drop those pages from the cache after the write completes,
//! keeping bulk I/O from polluting the cache while still using the page cache
//! for the write itself (unlike `O_DIRECT`, no alignment constraints apply).
//!
//! # When to use
//!
//! The dontcache path is a transparent optimisation: it never changes the bytes
//! landed on disk, only their cache residency. It is most valuable for transfers
//! whose total size exceeds available RAM. On kernels older than 6.14, or on
//! filesystems that reject the flag, [`DontcacheFileWriter::write_chunk`] falls
//! back to a buffered `write(2)` and remembers the rejection so subsequent
//! chunks skip the failing syscall.
//!
//! # Feature gate
//!
//! The full implementation is compiled only on `cfg(all(target_os = "linux",
//! feature = "dontcache"))`. On every other configuration the module exposes a
//! stub type whose constructor and `write_chunk` return
//! [`std::io::ErrorKind::Unsupported`], so callers can write
//! platform-independent code that compiles everywhere. The feature is
//! default-off until eviction/throughput benchmarks on the `>RAM` workload
//! justify promotion.

use std::fs::File;
use std::io;

#[cfg(all(target_os = "linux", feature = "dontcache"))]
use std::os::fd::{AsRawFd, RawFd};

/// Writer that lands userspace chunks into a destination file via
/// `pwritev2(2)` with `RWF_DONTCACHE`, falling back to a buffered write when
/// the kernel or filesystem rejects the flag.
///
/// Construct one per destination file. The first chunk that the kernel rejects
/// with `EINVAL`/`ENOTSUP`/`EOPNOTSUPP`/`ENOSYS` flips the writer to the
/// buffered path for the remainder of the file, so an unsupported kernel pays
/// at most one failed syscall per file.
#[cfg(all(target_os = "linux", feature = "dontcache"))]
pub struct DontcacheFileWriter {
    file: File,
    dest_fd: RawFd,
    dontcache_ok: bool,
}

#[cfg(all(target_os = "linux", feature = "dontcache"))]
impl DontcacheFileWriter {
    /// Wraps `file` so writes attempt the `RWF_DONTCACHE` path.
    ///
    /// # Errors
    ///
    /// Never fails today; returns `io::Result` to keep the constructor
    /// signature symmetric with other `fast_io` writers and allow future
    /// validation without a breaking change.
    pub fn new(file: File) -> io::Result<Self> {
        let dest_fd = file.as_raw_fd();
        Ok(Self {
            file,
            dest_fd,
            dontcache_ok: true,
        })
    }

    /// Returns a reference to the destination file.
    #[must_use]
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Returns the destination file descriptor.
    #[must_use]
    pub fn dest_fd(&self) -> RawFd {
        self.dest_fd
    }

    /// Returns whether the `RWF_DONTCACHE` path is still active (i.e. the
    /// kernel has not yet rejected the flag for this file).
    #[must_use]
    pub fn dontcache_active(&self) -> bool {
        self.dontcache_ok
    }

    /// Consumes the writer and returns the destination file for any further
    /// fsync, truncate, or rename step.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Writes `chunk` to the destination file, taking the `RWF_DONTCACHE` path
    /// when it is still active and falling back to a buffered write otherwise.
    ///
    /// On the first kernel rejection of the flag the writer switches to the
    /// buffered path permanently for this file. Short `pwritev2` transfers have
    /// their tail completed via the buffered path so the caller always observes
    /// a full write.
    ///
    /// # Errors
    ///
    /// Returns the error from either path. Flag-rejection errnos
    /// (`EINVAL`/`ENOTSUP`/`EOPNOTSUPP`/`ENOSYS`) are handled internally by
    /// switching to the buffered path; any other write error propagates.
    pub fn write_chunk(&mut self, chunk: &[u8]) -> io::Result<usize> {
        use std::io::Write;

        if chunk.is_empty() {
            return Ok(0);
        }

        if self.dontcache_ok {
            match dontcache_write(self.dest_fd, chunk) {
                Ok(n) if n == chunk.len() => return Ok(n),
                Ok(n) => {
                    // Short write: land the tail via the buffered path so the
                    // caller observes a full write. pwritev2(offset=-1) and
                    // write(2) share the kernel file offset, so this continues
                    // exactly where pwritev2 stopped.
                    self.file.write_all(&chunk[n..])?;
                    return Ok(chunk.len());
                }
                // On Linux ENOTSUP and EOPNOTSUPP share the same value, so only
                // ENOTSUP is listed to avoid an unreachable match arm.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EINVAL) | Some(libc::ENOTSUP) | Some(libc::ENOSYS)
                    ) =>
                {
                    // Kernel or filesystem does not support RWF_DONTCACHE.
                    // Disable it for the rest of this file and fall through.
                    self.dontcache_ok = false;
                }
                Err(e) => return Err(e),
            }
        }

        self.file.write_all(chunk)?;
        Ok(chunk.len())
    }
}

/// Issues a single `pwritev2(fd, iov, 1, -1, RWF_DONTCACHE)` for `chunk`.
///
/// An offset of `-1` writes at and advances the current file offset, matching
/// `write(2)` semantics, so this composes with buffered writes on the same fd.
#[cfg(all(target_os = "linux", feature = "dontcache"))]
#[allow(unsafe_code)]
fn dontcache_write(fd: RawFd, chunk: &[u8]) -> io::Result<usize> {
    let iov = libc::iovec {
        iov_base: chunk.as_ptr() as *mut libc::c_void,
        iov_len: chunk.len(),
    };
    // SAFETY: `fd` is a valid open file descriptor owned by the caller's File
    // for the duration of this call. `iov` points to `chunk`, which is a live
    // immutable slice of `iov_len` bytes; pwritev2 only reads from it. iovcnt
    // is 1 to match the single iovec. offset -1 selects the current file
    // offset (write(2) semantics). No memory is written by this call.
    let ret = unsafe { libc::pwritev2(fd, &iov as *const libc::iovec, 1, -1, libc::RWF_DONTCACHE) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret as usize)
}

/// Fills `dst` from `offset` using positioned `preadv2(RWF_DONTCACHE)` reads so
/// the bytes are dropped from the page cache after the read.
///
/// Returns `Ok(true)` once `dst` is fully filled, `Ok(false)` if the very first
/// read is rejected because the kernel or filesystem does not support
/// `RWF_DONTCACHE` (the caller then falls back to a buffered read), and `Err`
/// for a genuine I/O error or an unexpected short read after some bytes landed.
/// Uses positioned reads, so the file's seek offset is left untouched.
///
/// Mirrors [`dontcache_write`] on the read side.
#[cfg(all(target_os = "linux", feature = "dontcache"))]
#[allow(unsafe_code)]
pub fn dontcache_read_exact(file: &File, offset: u64, dst: &mut [u8]) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    let mut filled = 0usize;
    while filled < dst.len() {
        let iov = libc::iovec {
            iov_base: dst[filled..].as_mut_ptr() as *mut libc::c_void,
            iov_len: dst.len() - filled,
        };
        // SAFETY: `fd` is a valid open descriptor owned by `file` for the call.
        // `iov` points to `dst[filled..]`, a unique mutable slice of exactly
        // `iov_len` bytes that preadv2 only writes into; iovcnt 1 matches the
        // single iovec. The positioned offset leaves the file cursor untouched.
        let ret = unsafe {
            libc::preadv2(
                fd,
                &iov as *const libc::iovec,
                1,
                (offset + filled as u64) as libc::off_t,
                libc::RWF_DONTCACHE,
            )
        };
        if ret < 0 {
            let e = io::Error::last_os_error();
            // A first-read rejection means the flag is unsupported on this
            // mount; signal a clean fallback. Once bytes have landed (a flag
            // accepted then a later genuine error), surface the error.
            if filled == 0
                && matches!(
                    e.raw_os_error(),
                    Some(libc::EINVAL) | Some(libc::ENOTSUP) | Some(libc::ENOSYS)
                )
            {
                return Ok(false);
            }
            return Err(e);
        }
        if ret == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "preadv2 returned 0 before the basis window was filled",
            ));
        }
        filled += ret as usize;
    }
    Ok(true)
}

/// Returns whether the running kernel advertises `RWF_DONTCACHE` (Linux 6.14+).
///
/// The kernel release is read once via `uname(2)` and the verdict is cached for
/// the lifetime of the process. A `true` result only means the version gate is
/// met; per-file filesystem rejection is still handled by
/// [`DontcacheFileWriter`]'s `dontcache_ok` fallback. Callers use this as a
/// cheap up-front "is the flag worth attempting" check before selecting the
/// writer, mirroring `is_io_uring_available`.
#[cfg(all(target_os = "linux", feature = "dontcache"))]
#[must_use]
pub fn dontcache_supported() -> bool {
    use crate::kernel_version::{DontcacheRequirement, VersionRequirement, parse_kernel_version};
    use std::sync::OnceLock;

    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        kernel_release()
            .as_deref()
            .and_then(parse_kernel_version)
            .is_some_and(|kv| DontcacheRequirement.is_supported(&kv))
    })
}

/// Reads the kernel release string via `uname(2)` (e.g. `"6.14.0-generic"`).
#[cfg(all(target_os = "linux", feature = "dontcache"))]
#[allow(unsafe_code)]
fn kernel_release() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: `utsname` is zero-initialised then fully populated by `uname`,
    // which is sound for the POD struct. On success `release` is a
    // NUL-terminated byte array that stays valid for the `CStr` borrow below.
    unsafe {
        let mut utsname: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut utsname) != 0 {
            return None;
        }
        CStr::from_ptr(utsname.release.as_ptr())
            .to_str()
            .ok()
            .map(String::from)
    }
}

/// Stub: `RWF_DONTCACHE` exists only on Linux 6.14+ with the `dontcache`
/// feature, so every other configuration reports it unsupported.
#[cfg(not(all(target_os = "linux", feature = "dontcache")))]
#[must_use]
pub fn dontcache_supported() -> bool {
    false
}

/// Stub for non-Linux or when the `dontcache` feature is off: always reports
/// that the caller should fall back to a buffered read.
#[cfg(not(all(target_os = "linux", feature = "dontcache")))]
pub fn dontcache_read_exact(_file: &File, _offset: u64, _dst: &mut [u8]) -> io::Result<bool> {
    Ok(false)
}

/// Stub for non-Linux platforms or when the `dontcache` feature is disabled.
///
/// Every constructor and write method returns
/// [`std::io::ErrorKind::Unsupported`], allowing callers to compile a single
/// code path everywhere and probe availability at runtime.
#[cfg(not(all(target_os = "linux", feature = "dontcache")))]
pub struct DontcacheFileWriter {
    _private: (),
}

#[cfg(not(all(target_os = "linux", feature = "dontcache")))]
impl DontcacheFileWriter {
    /// Stub: always returns [`io::ErrorKind::Unsupported`].
    pub fn new(_file: File) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "dontcache writer requires Linux and the `dontcache` cargo feature",
        ))
    }

    /// Stub: always reports the dontcache path inactive.
    #[must_use]
    pub fn dontcache_active(&self) -> bool {
        false
    }

    /// Stub: always returns [`io::ErrorKind::Unsupported`].
    pub fn write_chunk(&mut self, _chunk: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "dontcache writer requires Linux and the `dontcache` cargo feature",
        ))
    }
}

#[cfg(all(test, target_os = "linux", feature = "dontcache"))]
mod linux_tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Read;

    fn read_back(path: &std::path::Path) -> Vec<u8> {
        let mut buf = Vec::new();
        File::open(path)
            .expect("reopen output")
            .read_to_end(&mut buf)
            .expect("read output");
        buf
    }

    fn create(path: &std::path::Path) -> File {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .expect("create file")
    }

    #[test]
    fn writes_one_mib_chunk_byte_equal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dontcache_1mib.bin");
        let mut writer = DontcacheFileWriter::new(create(&path)).expect("writer");
        let chunk = vec![0xABu8; 1024 * 1024];
        let n = writer.write_chunk(&chunk).expect("write_chunk 1 MiB");
        assert_eq!(n, chunk.len());
        drop(writer);

        let actual = read_back(&path);
        assert_eq!(actual, chunk);
    }

    #[test]
    fn multiple_chunks_concatenate_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dontcache_multi.bin");
        let mut writer = DontcacheFileWriter::new(create(&path)).expect("writer");

        let a = vec![0x11u8; 200 * 1024];
        let b = vec![0x22u8; 64];
        let c = vec![0x33u8; 512 * 1024];
        writer.write_chunk(&a).expect("a");
        writer.write_chunk(&b).expect("b");
        writer.write_chunk(&c).expect("c");
        drop(writer);

        let mut expected = Vec::new();
        expected.extend_from_slice(&a);
        expected.extend_from_slice(&b);
        expected.extend_from_slice(&c);
        assert_eq!(read_back(&path), expected);
    }

    #[test]
    fn empty_chunk_is_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dontcache_empty.bin");
        let mut writer = DontcacheFileWriter::new(create(&path)).expect("writer");
        assert_eq!(writer.write_chunk(&[]).expect("empty"), 0);
        drop(writer);
        assert!(read_back(&path).is_empty());
    }

    #[test]
    fn dontcache_supported_matches_live_kernel_and_is_cached() {
        use crate::kernel_version::{
            DontcacheRequirement, VersionRequirement, parse_kernel_version,
        };

        // The probe must agree with the version gate computed from this host's
        // live `uname(2)` release, so the result is correct on both new and
        // old kernels without hard-coding a version into the test.
        let expected = super::kernel_release()
            .as_deref()
            .and_then(parse_kernel_version)
            .is_some_and(|kv| DontcacheRequirement.is_supported(&kv));
        assert_eq!(dontcache_supported(), expected);
        // OnceLock caches the verdict: a repeat call returns the same value.
        assert_eq!(dontcache_supported(), expected);
    }
}

#[cfg(all(test, not(all(target_os = "linux", feature = "dontcache"))))]
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

        let err = match DontcacheFileWriter::new(file) {
            Ok(_) => panic!("stub should fail"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
