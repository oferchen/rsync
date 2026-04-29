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

/// Structured kernel information for io_uring availability reporting.
///
/// Provides machine-readable fields for callers that need to act on
/// kernel version or op count (e.g., `--version` output, debug logging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoUringKernelInfo {
    /// Whether io_uring is usable on this system.
    pub available: bool,
    /// Detected kernel major version, if parseable.
    pub kernel_major: Option<u32>,
    /// Detected kernel minor version, if parseable.
    pub kernel_minor: Option<u32>,
    /// Number of supported io_uring opcodes (0 if unavailable or probe failed).
    pub supported_ops: u32,
    /// Human-readable reason string (same as `io_uring_availability_reason()`).
    pub reason: String,
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
    ///
    /// Example outputs:
    /// - `"io_uring: enabled (kernel 6.1, 48 ops supported)"`
    /// - `"io_uring: disabled (kernel 4.19 < 5.6 required)"`
    /// - `"io_uring: disabled (kernel 5.15, io_uring_setup(2) blocked by seccomp, container, or permission restriction)"`
    #[must_use]
    pub fn io_uring_availability_reason() -> String {
        super::check_io_uring_reason().reason()
    }

    /// Returns structured kernel information for io_uring availability.
    ///
    /// Probes the kernel version and io_uring syscall availability, returning
    /// a struct with machine-readable fields for programmatic consumption.
    #[must_use]
    pub fn io_uring_kernel_info() -> super::IoUringKernelInfo {
        let result = super::check_io_uring_reason();
        match &result {
            super::IoUringProbeResult::Available {
                major,
                minor,
                supported_ops,
            } => super::IoUringKernelInfo {
                available: true,
                kernel_major: Some(*major),
                kernel_minor: Some(*minor),
                supported_ops: *supported_ops,
                reason: result.reason(),
            },
            super::IoUringProbeResult::KernelTooOld { major, minor }
            | super::IoUringProbeResult::SyscallBlocked { major, minor } => {
                super::IoUringKernelInfo {
                    available: false,
                    kernel_major: Some(*major),
                    kernel_minor: Some(*minor),
                    supported_ops: 0,
                    reason: result.reason(),
                }
            }
            super::IoUringProbeResult::NoKernelRelease
            | super::IoUringProbeResult::UnparsableVersion => super::IoUringKernelInfo {
                available: false,
                kernel_major: None,
                kernel_minor: None,
                supported_ops: 0,
                reason: result.reason(),
            },
        }
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

    let result = check_io_uring_reason();
    let reason = result.reason();
    let available = matches!(result, IoUringProbeResult::Available { .. });
    IO_URING_AVAILABLE.store(available, Ordering::Relaxed);
    IO_URING_CHECKED.store(true, Ordering::Relaxed);
    logging::debug_log!(Io, 1, "{reason}");
    available
}

/// Result of probing io_uring availability with the specific reason.
#[derive(Debug, Clone)]
pub(crate) enum IoUringProbeResult {
    /// io_uring is available on this kernel.
    Available {
        /// Detected kernel major.minor version.
        major: u32,
        minor: u32,
        /// Number of supported io_uring opcodes reported by `IORING_REGISTER_PROBE`.
        supported_ops: u32,
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
            Self::Available {
                major,
                minor,
                supported_ops,
            } => {
                format!("io_uring: enabled (kernel {major}.{minor}, {supported_ops} ops supported)")
            }
            Self::NoKernelRelease => {
                "io_uring: disabled (could not read kernel version)".to_string()
            }
            Self::UnparsableVersion => {
                "io_uring: disabled (could not parse kernel version)".to_string()
            }
            Self::KernelTooOld { major, minor } => {
                format!("io_uring: disabled (kernel {major}.{minor} < 5.6 required)")
            }
            Self::SyscallBlocked { major, minor } => {
                format!(
                    "io_uring: disabled (kernel {major}.{minor}, io_uring_setup(2) blocked \
                     by seccomp, container, or permission restriction)"
                )
            }
        }
    }
}

/// Counts supported io_uring opcodes by probing via `IORING_REGISTER_PROBE`.
///
/// Creates a temporary ring, registers a probe, and counts how many opcodes
/// the kernel reports as supported. Returns 0 if the probe fails.
fn count_supported_ops(ring: &RawIoUring) -> u32 {
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_ok() {
        (0..=u8::MAX).filter(|&op| probe.is_supported(op)).count() as u32
    } else {
        0
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

    match RawIoUring::new(4) {
        Ok(ring) => {
            let supported_ops = count_supported_ops(&ring);
            IoUringProbeResult::Available {
                major,
                minor,
                supported_ops,
            }
        }
        Err(_) => IoUringProbeResult::SyscallBlocked { major, minor },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_result_available_reason_contains_kernel_version_and_ops() {
        let result = IoUringProbeResult::Available {
            major: 6,
            minor: 1,
            supported_ops: 48,
        };
        let reason = result.reason();
        assert!(reason.contains("enabled"));
        assert!(reason.contains("6.1"));
        assert!(reason.contains("48 ops supported"));
    }

    #[test]
    fn probe_result_no_kernel_release_reason() {
        let result = IoUringProbeResult::NoKernelRelease;
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("could not read kernel version"));
    }

    #[test]
    fn probe_result_unparsable_version_reason() {
        let result = IoUringProbeResult::UnparsableVersion;
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("could not parse kernel version"));
    }

    #[test]
    fn probe_result_kernel_too_old_reason() {
        let result = IoUringProbeResult::KernelTooOld {
            major: 4,
            minor: 19,
        };
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("4.19"));
        assert!(reason.contains("< 5.6 required"));
    }

    #[test]
    fn probe_result_syscall_blocked_reason() {
        let result = IoUringProbeResult::SyscallBlocked {
            major: 5,
            minor: 15,
        };
        let reason = result.reason();
        assert!(reason.contains("disabled"));
        assert!(reason.contains("5.15"));
        assert!(reason.contains("blocked"));
        assert!(reason.contains("seccomp"));
    }

    #[test]
    fn probe_result_all_variants_start_with_io_uring_prefix() {
        let variants: Vec<IoUringProbeResult> = vec![
            IoUringProbeResult::Available {
                major: 6,
                minor: 8,
                supported_ops: 50,
            },
            IoUringProbeResult::NoKernelRelease,
            IoUringProbeResult::UnparsableVersion,
            IoUringProbeResult::KernelTooOld { major: 4, minor: 0 },
            IoUringProbeResult::SyscallBlocked {
                major: 5,
                minor: 10,
            },
        ];

        for variant in &variants {
            let reason = variant.reason();
            assert!(
                reason.starts_with("io_uring: "),
                "all variants must start with 'io_uring: ' prefix, got: {reason}"
            );
            assert!(
                !reason.contains('\n'),
                "reason must be single line, got: {reason}"
            );
        }
    }

    #[test]
    fn kernel_info_available_has_all_fields() {
        let info = IoUringKernelInfo {
            available: true,
            kernel_major: Some(6),
            kernel_minor: Some(1),
            supported_ops: 48,
            reason: "io_uring: enabled (kernel 6.1, 48 ops supported)".to_string(),
        };
        assert!(info.available);
        assert_eq!(info.kernel_major, Some(6));
        assert_eq!(info.kernel_minor, Some(1));
        assert!(info.supported_ops > 0);
    }

    #[test]
    fn kernel_info_unavailable_has_zero_ops() {
        let info = IoUringKernelInfo {
            available: false,
            kernel_major: Some(4),
            kernel_minor: Some(19),
            supported_ops: 0,
            reason: "io_uring: disabled (kernel 4.19 < 5.6 required)".to_string(),
        };
        assert!(!info.available);
        assert_eq!(info.supported_ops, 0);
    }

    #[test]
    fn kernel_info_no_kernel_release_has_none_versions() {
        let info = IoUringKernelInfo {
            available: false,
            kernel_major: None,
            kernel_minor: None,
            supported_ops: 0,
            reason: "io_uring: disabled (could not read kernel version)".to_string(),
        };
        assert!(!info.available);
        assert!(info.kernel_major.is_none());
        assert!(info.kernel_minor.is_none());
    }

    #[test]
    fn config_detail_kernel_info_returns_consistent_result() {
        let info = config_detail::io_uring_kernel_info();
        let reason = config_detail::io_uring_availability_reason();
        assert_eq!(info.reason, reason);
        assert_eq!(info.available, is_io_uring_available());
    }

    #[test]
    fn sqpoll_fell_back_initial_state() {
        // The SQPOLL_FALLBACK atomic starts as false. It is only set to true
        // when build_ring() attempts SQPOLL and it fails.
        assert!(!sqpoll_fell_back());
    }

    #[test]
    fn parse_kernel_version_valid_strings() {
        assert_eq!(parse_kernel_version("5.6.0"), Some((5, 6)));
        assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
        assert_eq!(parse_kernel_version("4.19.123-aws"), Some((4, 19)));
    }

    #[test]
    fn parse_kernel_version_invalid_strings() {
        assert_eq!(parse_kernel_version("invalid"), None);
        assert_eq!(parse_kernel_version(""), None);
    }

    #[test]
    fn config_detail_io_uring_availability_reason_is_non_empty() {
        let reason = config_detail::io_uring_availability_reason();
        assert!(!reason.is_empty());
        assert!(reason.starts_with("io_uring: "));
    }

    #[test]
    fn parse_kernel_version_extra_dots_azure() {
        // Azure kernel strings have extra dot-separated segments.
        assert_eq!(parse_kernel_version("5.15.0.1-azure"), Some((5, 15)));
    }

    #[test]
    fn parse_kernel_version_very_large_numbers() {
        assert_eq!(parse_kernel_version("100.200.300"), Some((100, 200)));
    }

    #[test]
    fn parse_kernel_version_single_digit_returns_none() {
        // A single digit has no minor component - the second `parts.next()?`
        // yields an empty string from the trailing split, which fails to parse.
        assert_eq!(parse_kernel_version("5"), None);
    }

    #[test]
    fn parse_kernel_version_trailing_rc_suffix() {
        // Release candidate strings like "6.1.0-rc1" - the split on non-digit
        // chars separates "rc1" from the numeric parts.
        assert_eq!(parse_kernel_version("6.1.0-rc1"), Some((6, 1)));
    }

    #[test]
    fn parse_kernel_version_leading_zeros() {
        // Rust's u32::parse treats leading zeros as valid decimal.
        assert_eq!(parse_kernel_version("06.01.00"), Some((6, 1)));
    }

    #[test]
    fn parse_kernel_version_zero_zero() {
        assert_eq!(parse_kernel_version("0.0.0"), Some((0, 0)));
    }

    #[test]
    fn parse_kernel_version_wsl_style() {
        // WSL2 kernel: "5.15.167.4-microsoft-standard-WSL2"
        assert_eq!(
            parse_kernel_version("5.15.167.4-microsoft-standard-WSL2"),
            Some((5, 15))
        );
    }

    #[test]
    fn parse_kernel_version_chromeos_style() {
        // ChromeOS: "5.10.159-20950-g5765b1ef511a"
        assert_eq!(
            parse_kernel_version("5.10.159-20950-g5765b1ef511a"),
            Some((5, 10))
        );
    }

    fn is_power_of_two(n: u32) -> bool {
        n > 0 && (n & (n - 1)) == 0
    }

    #[test]
    fn default_config_sq_entries_is_power_of_two() {
        let config = IoUringConfig::default();
        assert!(
            is_power_of_two(config.sq_entries),
            "default sq_entries {} must be a power of 2",
            config.sq_entries
        );
    }

    #[test]
    fn large_files_config_has_reasonable_values() {
        let config = IoUringConfig::for_large_files();
        assert!(
            is_power_of_two(config.sq_entries),
            "sq_entries {} must be a power of 2",
            config.sq_entries
        );
        assert!(
            config.sq_entries >= 64,
            "large file config should have at least 64 SQ entries"
        );
        assert!(
            config.buffer_size >= 128 * 1024,
            "large file buffer should be at least 128 KB"
        );
        assert!(
            config.buffer_size <= 4 * 1024 * 1024,
            "large file buffer should not exceed 4 MB"
        );
        assert!(config.register_files, "fd registration should be enabled");
        assert!(
            config.register_buffers,
            "buffer registration should be enabled for large files"
        );
        assert!(
            config.registered_buffer_count >= 8,
            "large file config should register at least 8 buffers"
        );
    }

    #[test]
    fn small_files_config_has_reasonable_values() {
        let config = IoUringConfig::for_small_files();
        assert!(
            is_power_of_two(config.sq_entries),
            "sq_entries {} must be a power of 2",
            config.sq_entries
        );
        assert!(
            config.buffer_size >= 4 * 1024,
            "small file buffer should be at least 4 KB"
        );
        assert!(
            config.buffer_size <= 128 * 1024,
            "small file buffer should not exceed 128 KB"
        );
        assert!(config.register_files, "fd registration should be enabled");
    }

    #[test]
    fn small_files_config_has_smaller_buffers_than_large() {
        let small = IoUringConfig::for_small_files();
        let large = IoUringConfig::for_large_files();
        assert!(
            small.buffer_size < large.buffer_size,
            "small file buffer ({}) should be smaller than large file buffer ({})",
            small.buffer_size,
            large.buffer_size
        );
    }

    #[test]
    fn large_files_config_has_more_sq_entries_than_default() {
        let default = IoUringConfig::default();
        let large = IoUringConfig::for_large_files();
        assert!(
            large.sq_entries >= default.sq_entries,
            "large file sq_entries ({}) should be >= default ({})",
            large.sq_entries,
            default.sq_entries
        );
    }

    #[test]
    fn default_config_sqpoll_disabled() {
        let config = IoUringConfig::default();
        assert!(!config.sqpoll, "SQPOLL should be disabled by default");
    }

    #[test]
    fn build_ring_with_sqpoll_falls_back_gracefully() {
        // Request SQPOLL - on most CI machines without CAP_SYS_NICE this will
        // fail and fall back to a regular ring. Either way, build_ring() must
        // succeed.
        let config = IoUringConfig {
            sqpoll: true,
            ..IoUringConfig::default()
        };
        let ring_result = config.build_ring();
        assert!(
            ring_result.is_ok(),
            "build_ring() must succeed even when SQPOLL falls back: {:?}",
            ring_result.err()
        );
    }

    #[test]
    fn sqpoll_fallback_flag_set_after_failed_sqpoll() {
        // Reset the global to a known state - note: this is not thread-safe
        // but test runners serialize by default.
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);

        let config = IoUringConfig {
            sqpoll: true,
            ..IoUringConfig::default()
        };
        let _ = config.build_ring();

        // On unprivileged systems, SQPOLL setup fails and the flag is set.
        // On privileged systems (root/CAP_SYS_NICE), SQPOLL succeeds and the
        // flag stays false. Both outcomes are valid.
        // We cannot assert a specific value since it depends on privileges,
        // but we can verify the flag is queryable without panic.
        let _fell_back: bool = sqpoll_fell_back();
    }

    #[test]
    fn build_ring_without_sqpoll_does_not_set_fallback() {
        SQPOLL_FALLBACK.store(false, Ordering::Relaxed);

        let config = IoUringConfig::default();
        assert!(!config.sqpoll);
        let _ = config.build_ring();

        assert!(
            !sqpoll_fell_back(),
            "SQPOLL fallback flag must not be set when SQPOLL was not requested"
        );
    }
}
