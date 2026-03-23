//! io_uring configuration, kernel detection, and availability caching.
//!
//! Kernel version detection uses `uname(2)` to parse the release string and
//! requires >= 5.6. The result is cached in process-wide atomics so that
//! subsequent calls to [`is_io_uring_available`] are a single relaxed load.

use std::ffi::CStr;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use io_uring::IoUring as RawIoUring;

/// Minimum kernel version required for io_uring.
///
/// Linux 5.6 introduced `io_uring_setup(2)` with support for all opcodes this
/// crate uses: `IORING_OP_READ`, `IORING_OP_WRITE`, `IORING_OP_SEND`,
/// `IORING_REGISTER_FILES`, and `IORING_SETUP_SQPOLL`. Earlier kernels (5.1-5.5)
/// had partial io_uring support but lacked critical features.
const MIN_KERNEL_VERSION: (u32, u32) = (5, 6);

/// Cached result of io_uring availability check.
static IO_URING_AVAILABLE: AtomicBool = AtomicBool::new(false);
static IO_URING_CHECKED: AtomicBool = AtomicBool::new(false);

/// Parses kernel version from uname release string (e.g., "5.15.0-generic").
pub(super) fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Gets the kernel release string using libc uname.
fn get_kernel_release() -> Option<String> {
    unsafe {
        let mut utsname: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut utsname) != 0 {
            return None;
        }
        let release = CStr::from_ptr(utsname.release.as_ptr());
        release.to_str().ok().map(String::from)
    }
}

/// Checks if the current kernel supports io_uring.
///
/// Returns `true` if all of the following hold:
///
/// 1. Running on Linux
/// 2. Kernel version is 5.6 or later (parsed from `uname().release`)
/// 3. `io_uring_setup(2)` succeeds - not blocked by seccomp or container runtime
///
/// The result is cached after the first call. Subsequent calls are a single
/// atomic load with `Relaxed` ordering (sub-nanosecond).
#[must_use]
pub fn is_io_uring_available() -> bool {
    // Fast path: use cached result
    if IO_URING_CHECKED.load(Ordering::Relaxed) {
        return IO_URING_AVAILABLE.load(Ordering::Relaxed);
    }

    let available = check_io_uring_available();
    IO_URING_AVAILABLE.store(available, Ordering::Relaxed);
    IO_URING_CHECKED.store(true, Ordering::Relaxed);
    available
}

fn check_io_uring_available() -> bool {
    // Check kernel version
    let release = match get_kernel_release() {
        Some(r) => r,
        None => return false,
    };

    let version = match parse_kernel_version(&release) {
        Some(v) => v,
        None => return false,
    };

    if version < MIN_KERNEL_VERSION {
        return false;
    }

    // Try to create a small io_uring instance to verify it's not blocked
    RawIoUring::new(4).is_ok()
}

/// Configuration for io_uring instances.
///
/// Controls ring size, buffer dimensions, and optional kernel features.
/// All features require Linux 5.6+. The defaults (64 SQ entries, 64 KB buffers,
/// fd registration enabled, SQPOLL disabled) are tuned for general rsync
/// workloads. Use [`for_large_files`](Self::for_large_files) or
/// [`for_small_files`](Self::for_small_files) for specialized workloads.
#[derive(Debug, Clone)]
pub struct IoUringConfig {
    /// Number of submission queue entries (must be power of 2).
    pub sq_entries: u32,
    /// Size of read/write buffers.
    pub buffer_size: usize,
    /// Whether to use direct I/O (O_DIRECT).
    pub direct_io: bool,
    /// Whether to register the file descriptor with io_uring.
    ///
    /// When enabled, the fd is registered via `IORING_REGISTER_FILES` at open
    /// time, eliminating per-op file table lookups in the kernel. This saves
    /// ~50ns per SQE on high-fd-count processes.
    pub register_files: bool,
    /// Whether to enable kernel-side SQ polling (`IORING_SETUP_SQPOLL`).
    ///
    /// When enabled, a kernel thread continuously polls the submission queue,
    /// eliminating the `io_uring_enter` syscall on submit. Requires elevated
    /// privileges or `CAP_SYS_NICE` on most kernels. Falls back to normal
    /// submission if setup fails.
    pub sqpoll: bool,
    /// Idle timeout (ms) for the SQPOLL kernel thread before it goes to sleep.
    /// Only relevant when `sqpoll` is true. Default: 1000ms.
    pub sqpoll_idle_ms: u32,
}

impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            sq_entries: 64,
            buffer_size: 64 * 1024, // 64 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }
}

impl IoUringConfig {
    /// Creates a config optimized for large file transfers.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            sq_entries: 256,
            buffer_size: 256 * 1024, // 256 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }

    /// Creates a config optimized for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            sq_entries: 128,
            buffer_size: 16 * 1024, // 16 KB
            direct_io: false,
            register_files: true,
            sqpoll: false,
            sqpoll_idle_ms: 1000,
        }
    }

    /// Builds an `IoUring` instance from this config.
    ///
    /// Tries SQPOLL first if requested; falls back to a plain ring on
    /// `EPERM` / `ENOMEM`. This two-step approach means callers can
    /// optimistically request SQPOLL without needing privilege checks
    /// upfront - the fallback is transparent.
    pub(crate) fn build_ring(&self) -> io::Result<RawIoUring> {
        if self.sqpoll {
            let mut builder = io_uring::IoUring::builder();
            builder.setup_sqpoll(self.sqpoll_idle_ms);
            match builder.build(self.sq_entries) {
                Ok(ring) => return Ok(ring),
                Err(_) => {
                    // SQPOLL requires privileges — fall through to normal ring
                }
            }
        }
        RawIoUring::new(self.sq_entries)
            .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
    }
}
