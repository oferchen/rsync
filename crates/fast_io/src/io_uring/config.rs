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

/// Whether SQPOLL was requested but fell back to regular submission.
///
/// Set to `true` the first time `build_ring()` attempts SQPOLL and it fails
/// (typically `EPERM` because the process lacks `CAP_SYS_NICE`). Callers
/// can query this via [`sqpoll_fell_back`] for diagnostics or `--version` output.
static SQPOLL_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Returns `true` if SQPOLL was requested but setup failed.
///
/// When `IoUringConfig::sqpoll` is `true` but the kernel rejects the request
/// (usually `EPERM` due to missing `CAP_SYS_NICE`), `build_ring()` transparently
/// falls back to a regular io_uring ring. This function reports whether that
/// fallback occurred, enabling diagnostic output like:
///
/// ```text
/// io_uring SQPOLL requires CAP_SYS_NICE, fell back to regular submission
/// ```
///
/// Returns `false` if SQPOLL was never requested or if it succeeded.
#[must_use]
pub fn sqpoll_fell_back() -> bool {
    SQPOLL_FALLBACK.load(Ordering::Relaxed)
}

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

/// Public accessors for kernel version detection used by `--version` output.
pub mod config_detail {
    /// Parses kernel version from uname release string (e.g., "5.15.0-generic").
    #[must_use]
    pub fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
        super::parse_kernel_version(release)
    }

    /// Returns the kernel release string from `uname(2)`.
    #[must_use]
    pub fn get_kernel_release_string() -> Option<String> {
        super::get_kernel_release()
    }

    /// Returns a human-readable reason for io_uring availability or unavailability.
    ///
    /// Probes the kernel version and attempts to create a minimal io_uring
    /// instance, returning a log-friendly string describing the result.
    #[must_use]
    pub fn io_uring_availability_reason() -> String {
        super::check_io_uring_reason().reason()
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
    matches!(
        check_io_uring_reason(),
        IoUringProbeResult::Available { .. }
    )
}

/// Result of probing io_uring availability with the specific reason.
#[derive(Debug, Clone)]
pub(crate) enum IoUringProbeResult {
    /// io_uring is available on this kernel.
    Available {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
    },
    /// Could not read the kernel release string from uname(2).
    NoKernelRelease,
    /// Kernel release string could not be parsed into major.minor.
    UnparsableVersion,
    /// Kernel version is below the 5.6 minimum.
    KernelTooOld {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
    },
    /// Kernel version is sufficient but io_uring_setup(2) failed - likely
    /// blocked by seccomp, container runtime, or permission restrictions.
    SyscallBlocked {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
    },
}

impl IoUringProbeResult {
    /// Returns a human-readable reason string suitable for log output.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Available { major, minor } => {
                format!("io_uring available (kernel {major}.{minor})")
            }
            Self::NoKernelRelease => {
                "io_uring unavailable: could not read kernel version".to_string()
            }
            Self::UnparsableVersion => {
                "io_uring unavailable: could not parse kernel version".to_string()
            }
            Self::KernelTooOld { major, minor } => {
                format!("io_uring unavailable: kernel {major}.{minor} is below minimum 5.6")
            }
            Self::SyscallBlocked { major, minor } => {
                format!(
                    "io_uring unavailable: io_uring_setup(2) blocked on kernel {major}.{minor} \
                     (seccomp, container, or permission restriction)"
                )
            }
        }
    }
}

/// Probes io_uring availability and returns the detailed result.
pub(crate) fn check_io_uring_reason() -> IoUringProbeResult {
    let release = match get_kernel_release() {
        Some(r) => r,
        None => return IoUringProbeResult::NoKernelRelease,
    };

    let (major, minor) = match parse_kernel_version(&release) {
        Some(v) => v,
        None => return IoUringProbeResult::UnparsableVersion,
    };

    if (major, minor) < MIN_KERNEL_VERSION {
        return IoUringProbeResult::KernelTooOld { major, minor };
    }

    if RawIoUring::new(4).is_ok() {
        IoUringProbeResult::Available { major, minor }
    } else {
        IoUringProbeResult::SyscallBlocked { major, minor }
    }
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
    /// Whether to register fixed buffers for `READ_FIXED`/`WRITE_FIXED` operations.
    ///
    /// When enabled, a [`RegisteredBufferGroup`](super::RegisteredBufferGroup) is
    /// created alongside the ring, pinning page-aligned buffers in kernel memory.
    /// This eliminates per-SQE `get_user_pages()` overhead - a significant win for
    /// high-throughput sequential I/O.
    ///
    /// Falls back silently to regular `Read`/`Write` opcodes if registration fails.
    pub register_buffers: bool,
    /// Number of fixed buffers to register. Only relevant when `register_buffers`
    /// is true. Capped at 1024 by the kernel. Default: 8.
    pub registered_buffer_count: usize,
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
            register_buffers: true,
            registered_buffer_count: 8,
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
            register_buffers: true,
            registered_buffer_count: 16,
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
            register_buffers: true,
            registered_buffer_count: 8,
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
                    // SQPOLL requires CAP_SYS_NICE on most kernels. Record
                    // the fallback so callers can surface it in diagnostics.
                    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
                }
            }
        }
        RawIoUring::new(self.sq_entries)
            .map_err(|e| io::Error::other(format!("io_uring init failed: {e}")))
    }
}
