//! IOCP configuration, availability detection, and caching.
//!
//! Windows I/O Completion Ports are available on Windows Vista and later.
//! Availability is checked once and cached in a process-wide atomic.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Availability check states.
const NOT_CHECKED: u8 = 0;
const AVAILABLE: u8 = 1;
const UNAVAILABLE: u8 = 2;

/// Cached result of IOCP availability check.
static IOCP_STATUS: AtomicU8 = AtomicU8::new(NOT_CHECKED);

/// Whether FILE_SKIP_SET_EVENT_ON_HANDLE optimization is active.
static SKIP_EVENT_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Minimum file size to use IOCP instead of standard buffered I/O.
///
/// Files smaller than this are read/written synchronously since the IOCP
/// setup overhead exceeds the async benefit for tiny files.
pub const IOCP_MIN_FILE_SIZE: u64 = 64 * 1024;

/// Lower bound for the auto-sized concurrent-ops depth.
///
/// Even on a single-CPU host we keep at least 8 overlapped `WriteFile`
/// operations in flight so the IOCP drain loop is never starved by a
/// pathological 1-deep submission window. Matches `IoUringConfig::default`
/// behaviour where the SQ depth never falls below the default ring entries.
pub const MIN_CONCURRENT_OPS: u32 = 8;

/// Upper bound for the auto-sized concurrent-ops depth.
///
/// 64 matches the io_uring CQE batch sizing in
/// `crates/fast_io/src/iocp/disk_batch.rs::COMPLETION_DRAIN_BATCH` and the
/// initial drain size in `crates/fast_io/src/iocp/pump.rs::DEFAULT_BATCH_SIZE`.
/// Keeping the submission window aligned with the drain batch lets a single
/// `GetQueuedCompletionStatusEx` reap an entire in-flight cohort.
pub const MAX_CONCURRENT_OPS: u32 = 64;

/// Default I/O buffer size for IOCP operations.
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// Auto-sizes the concurrent-ops depth from `cpus`.
///
/// Returns `(cpus * 4).clamp(MIN_CONCURRENT_OPS, MAX_CONCURRENT_OPS)`.
/// Mirrors the io_uring SQ-entry derivation: 4 in-flight ops per logical
/// core keeps every CPU fed without overwhelming the completion drain.
#[must_use]
pub const fn concurrent_ops_for_cpus(cpus: u32) -> u32 {
    let raw = cpus.saturating_mul(4);
    if raw < MIN_CONCURRENT_OPS {
        MIN_CONCURRENT_OPS
    } else if raw > MAX_CONCURRENT_OPS {
        MAX_CONCURRENT_OPS
    } else {
        raw
    }
}

/// Auto-sized default concurrent I/O operations per completion port.
///
/// Derives the value from `std::thread::available_parallelism()` so the
/// in-flight depth scales with the host's logical CPU count. Falls back to
/// `MIN_CONCURRENT_OPS` when parallelism detection fails.
#[must_use]
pub fn default_concurrent_ops() -> u32 {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpus_u32 = u32::try_from(cpus).unwrap_or(u32::MAX);
    concurrent_ops_for_cpus(cpus_u32)
}

/// Configuration for IOCP instances.
#[derive(Debug, Clone)]
pub struct IocpConfig {
    /// Number of concurrent I/O operations to submit.
    pub concurrent_ops: u32,
    /// Size of each I/O buffer.
    pub buffer_size: usize,
    /// Whether to use unbuffered I/O (`FILE_FLAG_NO_BUFFERING`).
    ///
    /// Unbuffered I/O bypasses the OS file cache, which is beneficial for
    /// large sequential transfers that would otherwise pollute the cache.
    /// Requires sector-aligned buffers and offsets.
    pub unbuffered: bool,
    /// Whether to use write-through (`FILE_FLAG_WRITE_THROUGH`).
    ///
    /// Write-through ensures data is flushed to disk on each write,
    /// providing durability at the cost of throughput.
    pub write_through: bool,
}

impl Default for IocpConfig {
    fn default() -> Self {
        Self {
            concurrent_ops: default_concurrent_ops(),
            buffer_size: DEFAULT_BUFFER_SIZE,
            unbuffered: false,
            write_through: false,
        }
    }
}

impl IocpConfig {
    /// Creates a config optimized for large file transfers.
    ///
    /// The concurrent-ops depth uses the CPU-derived default so wide hosts
    /// can keep more overlapped writes in flight, matching the io_uring
    /// large-file preset's larger SQ depth.
    #[must_use]
    pub fn for_large_files() -> Self {
        Self {
            concurrent_ops: default_concurrent_ops(),
            buffer_size: 256 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }

    /// Creates a config optimized for many small files.
    ///
    /// The concurrent-ops depth uses the CPU-derived default; small-file
    /// pipelines are typically syscall-bound so feeding every CPU pays off
    /// more than a shallow fixed window.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            concurrent_ops: default_concurrent_ops(),
            buffer_size: 16 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }
}

/// Check whether IOCP is available on this system.
///
/// On Windows Vista+, IOCP is always available. This function probes by
/// creating a minimal completion port and caches the result process-wide.
#[must_use]
pub fn is_iocp_available() -> bool {
    match IOCP_STATUS.load(Ordering::Relaxed) {
        AVAILABLE => true,
        UNAVAILABLE => false,
        _ => probe_iocp(),
    }
}

/// Returns whether `FILE_SKIP_SET_EVENT_ON_HANDLE` is available.
///
/// This optimization reduces per-completion overhead by not signaling the
/// file handle's event object on I/O completion (since we use the
/// completion port instead).
#[must_use]
pub fn skip_event_optimization_available() -> bool {
    // Force the probe to run so SKIP_EVENT_AVAILABLE reflects detected support.
    let _ = is_iocp_available();
    SKIP_EVENT_AVAILABLE.load(Ordering::Relaxed)
}

/// Returns a human-readable string describing IOCP support.
#[must_use]
pub fn iocp_availability_reason() -> String {
    if is_iocp_available() {
        let skip_event = if skip_event_optimization_available() {
            ", FILE_SKIP_SET_EVENT_ON_HANDLE active"
        } else {
            ""
        };
        format!("IOCP available (Windows){skip_event}")
    } else {
        "IOCP unavailable: CreateIoCompletionPort failed".to_string()
    }
}

/// Probes IOCP availability by creating a test completion port.
fn probe_iocp() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::IO::CreateIoCompletionPort;

    // SAFETY: CreateIoCompletionPort with INVALID_HANDLE_VALUE creates a new
    // completion port without associating any file handle. This is the
    // documented way to create a standalone completion port.
    #[allow(unsafe_code)]
    let handle =
        unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };

    if handle.is_null() {
        IOCP_STATUS.store(UNAVAILABLE, Ordering::Relaxed);
        logging::debug_log!(
            Iocp,
            1,
            "IOCP unavailable: CreateIoCompletionPort failed - using standard I/O"
        );
        return false;
    }

    #[allow(unsafe_code)]
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(handle);
    }

    // FILE_SKIP_SET_EVENT_ON_HANDLE is unconditionally safe under our model
    // because we always use completion ports rather than waiting on the
    // file handle's event. Available on Windows Vista+.
    SKIP_EVENT_AVAILABLE.store(true, Ordering::Relaxed);

    IOCP_STATUS.store(AVAILABLE, Ordering::Relaxed);
    logging::debug_log!(Iocp, 1, "IOCP available (Windows): dispatching IOCP writer");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_values() {
        let config = IocpConfig::default();
        assert!(
            (MIN_CONCURRENT_OPS..=MAX_CONCURRENT_OPS).contains(&config.concurrent_ops),
            "default concurrent_ops {} must sit inside [{}, {}]",
            config.concurrent_ops,
            MIN_CONCURRENT_OPS,
            MAX_CONCURRENT_OPS,
        );
        assert_eq!(config.concurrent_ops, default_concurrent_ops());
        assert_eq!(config.buffer_size, DEFAULT_BUFFER_SIZE);
        assert!(!config.unbuffered);
        assert!(!config.write_through);
    }

    #[test]
    fn config_large_files_preset() {
        let config = IocpConfig::for_large_files();
        assert_eq!(config.concurrent_ops, default_concurrent_ops());
        assert_eq!(config.buffer_size, 256 * 1024);
    }

    #[test]
    fn config_small_files_preset() {
        let config = IocpConfig::for_small_files();
        assert_eq!(config.concurrent_ops, default_concurrent_ops());
        assert_eq!(config.buffer_size, 16 * 1024);
    }

    #[test]
    fn concurrent_ops_eight_cpu_host_yields_thirty_two() {
        // 8 logical CPUs * 4 in-flight ops per core = 32, inside the clamp.
        assert_eq!(concurrent_ops_for_cpus(8), 32);
    }

    #[test]
    fn concurrent_ops_one_cpu_host_clamps_to_minimum() {
        // 1 CPU * 4 = 4, below MIN_CONCURRENT_OPS (8) so the floor wins.
        assert_eq!(concurrent_ops_for_cpus(1), MIN_CONCURRENT_OPS);
        assert_eq!(concurrent_ops_for_cpus(2), MIN_CONCURRENT_OPS);
    }

    #[test]
    fn concurrent_ops_wide_host_clamps_to_maximum() {
        // 16 CPUs * 4 = 64, exactly the ceiling; 32 CPUs * 4 = 128, clamped.
        assert_eq!(concurrent_ops_for_cpus(16), MAX_CONCURRENT_OPS);
        assert_eq!(concurrent_ops_for_cpus(32), MAX_CONCURRENT_OPS);
        assert_eq!(concurrent_ops_for_cpus(256), MAX_CONCURRENT_OPS);
    }

    #[test]
    fn concurrent_ops_saturates_on_overflow() {
        // u32::MAX * 4 saturates; the clamp still pins to MAX_CONCURRENT_OPS.
        assert_eq!(concurrent_ops_for_cpus(u32::MAX), MAX_CONCURRENT_OPS);
    }

    #[test]
    fn default_concurrent_ops_matches_detected_cpus() {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let cpus_u32 = u32::try_from(cpus).unwrap_or(u32::MAX);
        assert_eq!(default_concurrent_ops(), concurrent_ops_for_cpus(cpus_u32));
    }

    #[test]
    fn iocp_available_on_windows() {
        // IOCP is part of the kernel on Windows Vista+, so the probe must
        // succeed on every supported Windows host.
        assert!(is_iocp_available());
    }

    #[test]
    fn iocp_availability_cached() {
        let first = is_iocp_available();
        let second = is_iocp_available();
        assert_eq!(first, second);
    }

    #[test]
    fn availability_reason_is_well_formed() {
        let reason = iocp_availability_reason();
        assert!(!reason.is_empty());
        assert!(reason.contains("IOCP"));
    }
}
