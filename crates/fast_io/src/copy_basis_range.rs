//! Offset-aware basis-to-destination range copy via platform-optimized I/O.
//!
//! This module is the IUD-10 fast path for the delta-apply COPY-token branch:
//! when the receiver replays a `COPY {index, len}` token, the contents at
//! `basis[basis_off..basis_off+len]` must end up at
//! `dest[dest_off..dest_off+len]`.
//!
//! Platform implementations:
//!
//! - **Linux 4.5+**: `copy_file_range(2)` performs a zero-copy in-kernel
//!   transfer without bouncing bytes through userspace. On supporting
//!   filesystems, reflink/server-side copy acceleration is available.
//! - **Windows**: `ReadFile`/`WriteFile` with `OVERLAPPED` offset structs
//!   performs a kernel-buffered copy without moving the file pointers,
//!   matching the `copy_file_range` contract.
//! - **Other**: Returns `Ok(0)` so callers fall back to `map_ptr` + write.
//!
//! Upstream rsync 3.4.1 itself does not use `copy_file_range(2)` in the
//! receiver - it walks `map_ptr` and `write(2)`. This is an oc-rsync
//! optimization that produces byte-identical output; it is invisible on the
//! wire and changes no protocol behavior.
//!
//! The wrapper is conservative: any failure on the first iteration collapses
//! to `Ok(0)` so callers fall back to the existing read-then-write path
//! without surfacing platform-specific errors. Only failures after a partial
//! copy has succeeded are propagated, because at that point the destination
//! is in a state the caller must reconcile.
//!
//! # Bounded I/O contract
//!
//! Every iteration submits at most 256 KB (Windows) or `i64::MAX` bytes
//! (Linux) and the loop terminates on three conditions: requested `len`
//! reached, EOF on the source, or an error. There is no unbounded retry;
//! the caller's `len` bounds total time.

use std::fs::File;
use std::io;

/// Minimum range size for which `copy_file_range` amortizes its syscall cost.
///
/// Smaller ranges are cheaper to satisfy with a single `pread`/`write` pair
/// than to round-trip through the syscall. The receiver's typical block size
/// is well above this threshold, so the dispatch is rarely declined on real
/// transfers; the threshold exists to keep tiny tail blocks fast.
pub const COPY_BASIS_RANGE_MIN_BYTES: usize = 4 * 1024;

/// Copies `len` bytes from `basis[basis_off..]` into `dest[dest_off..]` using
/// a platform-optimized path, or returns `Ok(0)` on unsupported platforms so
/// callers fall back to read+write.
///
/// # Returns
///
/// - `Ok(n)` where `n == len` on full success.
/// - `Ok(n)` where `0 < n < len` if EOF on the basis was reached before
///   the range was satisfied; the caller must reconcile the short copy
///   (rare with a correctly sized basis).
/// - `Ok(0)` when the platform copy path is unavailable or any first-iteration
///   error occurs. The caller must fall back to the read+write path; the
///   destination is untouched.
///
/// # Errors
///
/// Returns `Err` only when an error occurs **after** a partial copy has
/// already succeeded. In that case the destination has been partially
/// written and the caller cannot transparently fall back.
///
/// # Bounded I/O
///
/// Caps each iteration at 256 KB (Windows) or `i64::MAX` (Linux) and
/// terminates on completion, EOF, or error - never hangs. Source and
/// destination file offsets are NOT advanced (Linux uses explicit `loff_t*`
/// pointers; Windows uses `OVERLAPPED` offset fields).
///
/// # Platform support
///
/// - **Linux 4.5+** same filesystem, 5.3+ cross filesystem.
/// - **Windows**: `ReadFile`/`WriteFile` with `OVERLAPPED` offsets.
/// - **Other**: returns `Ok(0)` immediately, no I/O issued.
#[inline]
pub fn copy_basis_range(
    basis: &File,
    basis_off: u64,
    dest: &File,
    dest_off: u64,
    len: usize,
) -> io::Result<usize> {
    imp::copy_basis_range(basis, basis_off, dest, dest_off, len)
}

/// Returns `true` when the platform has a usable optimized basis-range copy.
///
/// On Linux, probes for `copy_file_range(2)` and caches the result via a
/// process-wide `OnceLock`. On Windows, returns `true` unconditionally
/// (the `ReadFile`/`WriteFile` path is always available). On other
/// platforms, returns `false`.
#[must_use]
#[inline]
pub fn copy_file_range_supported() -> bool {
    imp::copy_file_range_supported()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::sync::OnceLock;

    static CFR_SUPPORTED: OnceLock<bool> = OnceLock::new();

    pub(super) fn copy_file_range_supported() -> bool {
        if let Some(cached) = CFR_SUPPORTED.get().copied() {
            return cached;
        }
        let result = probe();
        let _ = CFR_SUPPORTED.set(result);
        CFR_SUPPORTED.get().copied().unwrap_or(result)
    }

    fn probe() -> bool {
        // Issue a zero-byte copy_file_range against /dev/null in both
        // directions. A return of 0 with no error or any errno other than
        // ENOSYS means the syscall reached the kernel and is available;
        // ENOSYS means the kernel pre-dates Linux 4.5.
        let Ok(src) = File::open("/dev/null") else {
            return false;
        };
        let Ok(dst) = File::options().write(true).open("/dev/null") else {
            return false;
        };

        let mut src_off: libc::loff_t = 0;
        let mut dst_off: libc::loff_t = 0;
        // SAFETY: both fds are valid for the duration of this call (owned by
        // the local File values), the offset pointers are valid &mut local
        // variables, len=0 is a no-op probe documented to return 0 on
        // supported kernels. No memory is read or written.
        #[allow(unsafe_code)]
        let ret = unsafe {
            libc::copy_file_range(
                src.as_raw_fd(),
                &mut src_off as *mut libc::loff_t,
                dst.as_raw_fd(),
                &mut dst_off as *mut libc::loff_t,
                0,
                0,
            )
        };

        if ret >= 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS)
    }

    pub(super) fn copy_basis_range(
        basis: &File,
        basis_off: u64,
        dest: &File,
        dest_off: u64,
        len: usize,
    ) -> io::Result<usize> {
        if len == 0 {
            return Ok(0);
        }
        if !copy_file_range_supported() {
            return Ok(0);
        }

        let basis_fd = basis.as_raw_fd();
        let dest_fd = dest.as_raw_fd();

        let mut total: usize = 0;
        let mut src_off: libc::loff_t = match libc::loff_t::try_from(basis_off) {
            Ok(v) => v,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "basis_off exceeds loff_t",
                ));
            }
        };
        let mut dst_off: libc::loff_t = match libc::loff_t::try_from(dest_off) {
            Ok(v) => v,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "dest_off exceeds loff_t",
                ));
            }
        };

        while total < len {
            let remaining = len - total;
            let chunk = remaining.min(i64::MAX as usize);

            // SAFETY: both fds are valid for the call (borrowed from &File),
            // the loff_t pointers reference valid local mutables, and the
            // syscall does not retain any pointer past return. The kernel
            // advances *src_off and *dst_off in place by the number of bytes
            // copied.
            #[allow(unsafe_code)]
            let ret = unsafe {
                libc::copy_file_range(
                    basis_fd,
                    &mut src_off as *mut libc::loff_t,
                    dest_fd,
                    &mut dst_off as *mut libc::loff_t,
                    chunk,
                    0,
                )
            };

            if ret < 0 {
                let err = io::Error::last_os_error();
                if total == 0 {
                    // First iteration: caller falls back to read+write.
                    // EXDEV, EOPNOTSUPP, EINVAL (overlapping ranges, special
                    // files), and EXDEV-like errors all collapse to Ok(0).
                    return Ok(0);
                }
                return Err(err);
            }

            if ret == 0 {
                // EOF on basis before len was satisfied; short copy.
                break;
            }

            total += ret as usize;
        }

        Ok(total)
    }
}

/// Windows implementation using `ReadFile`/`WriteFile` with `OVERLAPPED`.
///
/// The `OVERLAPPED` struct specifies the file offset for each I/O operation
/// without moving the file pointer, matching the `copy_file_range` contract.
/// A 256 KB buffer keeps syscall count low while avoiding excessive stack or
/// heap pressure.
#[cfg(target_os = "windows")]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;

    /// Per-iteration buffer size for the `ReadFile`/`WriteFile` copy loop.
    const COPY_BUF_SIZE: usize = 256 * 1024;

    pub(super) fn copy_file_range_supported() -> bool {
        true
    }

    /// Splits a `u64` offset into the `Offset` (low 32 bits) and
    /// `OffsetHigh` (high 32 bits) fields used by `OVERLAPPED`.
    #[inline]
    fn offset_parts(off: u64) -> (u32, u32) {
        (off as u32, (off >> 32) as u32)
    }

    #[allow(unsafe_code)]
    pub(super) fn copy_basis_range(
        basis: &File,
        basis_off: u64,
        dest: &File,
        dest_off: u64,
        len: usize,
    ) -> io::Result<usize> {
        if len == 0 {
            return Ok(0);
        }

        let basis_handle = basis.as_raw_handle() as isize;
        let dest_handle = dest.as_raw_handle() as isize;

        let mut buf = vec![0u8; COPY_BUF_SIZE];
        let mut total: usize = 0;
        let mut src_pos = basis_off;
        let mut dst_pos = dest_off;

        while total < len {
            let remaining = len - total;
            let chunk = remaining.min(COPY_BUF_SIZE);

            let (src_lo, src_hi) = offset_parts(src_pos);
            let mut read_overlapped = windows_sys::Win32::System::IO::OVERLAPPED {
                Internal: 0,
                InternalHigh: 0,
                Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0 {
                    Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0_0 {
                        Offset: src_lo,
                        OffsetHigh: src_hi,
                    },
                },
                hEvent: 0,
            };

            let mut bytes_read: u32 = 0;
            // SAFETY: basis_handle is a valid file handle borrowed from &File.
            // buf is a valid mutable buffer with at least `chunk` bytes.
            // read_overlapped is a properly initialized OVERLAPPED struct.
            // bytes_read is a valid output pointer. The call completes
            // synchronously because the file was not opened with
            // FILE_FLAG_OVERLAPPED.
            let read_ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::ReadFile(
                    basis_handle,
                    buf.as_mut_ptr().cast(),
                    chunk as u32,
                    &mut bytes_read,
                    &mut read_overlapped,
                )
            };

            if read_ok == 0 {
                let err = io::Error::last_os_error();
                if total == 0 {
                    return Ok(0);
                }
                return Err(err);
            }

            if bytes_read == 0 {
                break; // EOF
            }

            let n = bytes_read as usize;

            let (dst_lo, dst_hi) = offset_parts(dst_pos);
            let mut write_overlapped = windows_sys::Win32::System::IO::OVERLAPPED {
                Internal: 0,
                InternalHigh: 0,
                Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0 {
                    Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0_0 {
                        Offset: dst_lo,
                        OffsetHigh: dst_hi,
                    },
                },
                hEvent: 0,
            };

            let mut total_written: usize = 0;
            while total_written < n {
                let write_chunk = n - total_written;
                let write_pos = dst_pos + total_written as u64;
                let (wlo, whi) = offset_parts(write_pos);
                write_overlapped.Anonymous.Anonymous.Offset = wlo;
                write_overlapped.Anonymous.Anonymous.OffsetHigh = whi;

                let mut bytes_written: u32 = 0;
                // SAFETY: dest_handle is a valid file handle borrowed from
                // &File. The buffer slice is valid for `write_chunk` bytes
                // starting at offset `total_written`. write_overlapped is
                // properly initialized. The call completes synchronously.
                let write_ok = unsafe {
                    windows_sys::Win32::Storage::FileSystem::WriteFile(
                        dest_handle,
                        buf[total_written..].as_ptr().cast(),
                        write_chunk as u32,
                        &mut bytes_written,
                        &mut write_overlapped,
                    )
                };

                if write_ok == 0 {
                    let err = io::Error::last_os_error();
                    if total == 0 && total_written == 0 {
                        return Ok(0);
                    }
                    return Err(err);
                }

                if bytes_written == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "WriteFile returned 0 bytes written",
                    ));
                }

                total_written += bytes_written as usize;
            }

            src_pos += n as u64;
            dst_pos += n as u64;
            total += n;
        }

        Ok(total)
    }
}

/// Stub for platforms without a specialized basis-range copy.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod imp {
    use std::fs::File;
    use std::io;

    pub(super) fn copy_file_range_supported() -> bool {
        false
    }

    pub(super) fn copy_basis_range(
        _basis: &File,
        _basis_off: u64,
        _dest: &File,
        _dest_off: u64,
        _len: usize,
    ) -> io::Result<usize> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    #[cfg(target_os = "linux")]
    use std::io::{Read, Seek, SeekFrom};
    use tempfile::tempdir;

    fn make_basis(dir: &std::path::Path, name: &str, payload: &[u8]) -> File {
        let path = dir.join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(payload).unwrap();
        f.sync_all().ok();
        File::open(&path).unwrap()
    }

    fn make_dest(dir: &std::path::Path, name: &str) -> File {
        let path = dir.join(name);
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    fn read_all(dest: &mut File) -> Vec<u8> {
        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = Vec::new();
        dest.read_to_end(&mut buf).unwrap();
        buf
    }

    #[test]
    fn empty_len_returns_zero_without_syscall() {
        let dir = tempdir().unwrap();
        let basis = make_basis(dir.path(), "b", b"abc");
        let dest = make_dest(dir.path(), "d");
        let copied = copy_basis_range(&basis, 0, &dest, 0, 0).unwrap();
        assert_eq!(copied, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_fs_copy_produces_byte_identical_output() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, payload.len()).unwrap();
        // On kernels without copy_file_range, copied == 0 and the caller
        // would fall back. On Linux 4.5+ with a tmpfs/ext4-backed tempdir,
        // expect a full or partial in-kernel copy.
        if copied == 0 {
            // Old kernel or restricted FS - production path falls back; the
            // wrapper contract is satisfied.
            return;
        }
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(out, payload);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn offset_copy_extracts_correct_window() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..8 * 1024).map(|i| (i % 211) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let window = 4 * 1024;
        let copied = copy_basis_range(&basis, 1024, &dest, 0, window).unwrap();
        if copied == 0 {
            return;
        }
        assert_eq!(copied, window);

        let out = read_all(&mut dest);
        assert_eq!(out, &payload[1024..1024 + window]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dest_offset_writes_at_correct_position() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..4 * 1024).map(|i| (i % 199) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");
        // Pre-fill destination with a sentinel so we can verify the write
        // landed at the requested offset and did not clobber earlier bytes.
        dest.write_all(&vec![0xFFu8; 2048]).unwrap();
        dest.sync_all().ok();

        let copied = copy_basis_range(&basis, 0, &dest, 2048, payload.len()).unwrap();
        if copied == 0 {
            return;
        }
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(&out[..2048], &vec![0xFFu8; 2048]);
        assert_eq!(&out[2048..], payload.as_slice());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn short_copy_when_basis_eof_returns_partial_count() {
        // Request more than the basis contains; the kernel returns 0 once it
        // hits EOF mid-loop. The wrapper exits the loop and reports the
        // bytes that did make it through - the receiver's checksum verifier
        // (when sequential) will then surface the mismatch.
        let dir = tempdir().unwrap();
        let payload = vec![0xA5u8; 2048];
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, 8192).unwrap();
        if copied == 0 {
            return;
        }
        assert!(copied <= payload.len());
        assert!(copied >= 1);

        let out = read_all(&mut dest);
        assert_eq!(&out[..copied], &payload[..copied]);
    }

    #[cfg(target_os = "windows")]
    fn read_all(dest: &mut File) -> Vec<u8> {
        use std::io::{Read, Seek, SeekFrom};
        dest.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = Vec::new();
        dest.read_to_end(&mut buf).unwrap();
        buf
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_copy_produces_byte_identical_output() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, payload.len()).unwrap();
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(out, payload);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_offset_copy_extracts_correct_window() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..8 * 1024).map(|i| (i % 211) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let window = 4 * 1024;
        let copied = copy_basis_range(&basis, 1024, &dest, 0, window).unwrap();
        assert_eq!(copied, window);

        let out = read_all(&mut dest);
        assert_eq!(out, &payload[1024..1024 + window]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_dest_offset_writes_at_correct_position() {
        let dir = tempdir().unwrap();
        let payload: Vec<u8> = (0..4 * 1024).map(|i| (i % 199) as u8).collect();
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");
        dest.write_all(&vec![0xFFu8; 2048]).unwrap();
        dest.sync_all().ok();

        let copied = copy_basis_range(&basis, 0, &dest, 2048, payload.len()).unwrap();
        assert_eq!(copied, payload.len());

        let out = read_all(&mut dest);
        assert_eq!(&out[..2048], &vec![0xFFu8; 2048]);
        assert_eq!(&out[2048..], payload.as_slice());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_short_copy_when_basis_eof() {
        let dir = tempdir().unwrap();
        let payload = vec![0xA5u8; 2048];
        let basis = make_basis(dir.path(), "basis.bin", &payload);
        let mut dest = make_dest(dir.path(), "dest.bin");

        let copied = copy_basis_range(&basis, 0, &dest, 0, 8192).unwrap();
        assert!(copied <= payload.len());
        assert!(copied >= 1);

        let out = read_all(&mut dest);
        assert_eq!(&out[..copied], &payload[..copied]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_supported_returns_true() {
        assert!(copy_file_range_supported());
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    #[test]
    fn unsupported_platform_returns_zero() {
        let dir = tempdir().unwrap();
        let basis = make_basis(dir.path(), "b", b"abcd");
        let dest = make_dest(dir.path(), "d");
        let copied = copy_basis_range(&basis, 0, &dest, 0, 4).unwrap();
        assert_eq!(copied, 0);
        assert!(!copy_file_range_supported());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_probe_is_stable_across_calls() {
        let a = copy_file_range_supported();
        let b = copy_file_range_supported();
        assert_eq!(a, b);
    }
}
