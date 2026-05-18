//! IOCP configuration, availability detection, and caching (stub).
//!
//! Mirrors the public surface of [`crate::iocp::config`] so cross-platform
//! callers can name the same items regardless of which backend is compiled.
//! All availability probes report `false` and the configuration values are
//! informational only on this platform.

/// Minimum file size threshold (informational only on this platform).
pub const IOCP_MIN_FILE_SIZE: u64 = 64 * 1024;

/// Lower bound for the auto-sized concurrent-ops depth. Mirrors the Windows
/// surface so cross-platform code can reference the constant unconditionally.
pub const MIN_CONCURRENT_OPS: u32 = 8;

/// Upper bound for the auto-sized concurrent-ops depth. Mirrors the Windows
/// surface so cross-platform code can reference the constant unconditionally.
pub const MAX_CONCURRENT_OPS: u32 = 64;

/// Auto-sizes the concurrent-ops depth from `cpus`.
///
/// Returns `(cpus * 4).clamp(MIN_CONCURRENT_OPS, MAX_CONCURRENT_OPS)`,
/// matching the Windows implementation so cross-platform tests and tools
/// see identical values for a given CPU count.
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
/// On non-Windows hosts the value is informational only; the stub
/// `IocpDiskBatch` never runs. Derives from
/// `std::thread::available_parallelism()` for parity with the Windows path.
#[must_use]
pub fn default_concurrent_ops() -> u32 {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpus_u32 = u32::try_from(cpus).unwrap_or(u32::MAX);
    concurrent_ops_for_cpus(cpus_u32)
}

/// Check whether IOCP is available (always `false` on this platform).
#[must_use]
pub fn is_iocp_available() -> bool {
    false
}

/// Returns whether FILE_SKIP_SET_EVENT_ON_HANDLE is available (always `false`).
#[must_use]
pub fn skip_event_optimization_available() -> bool {
    false
}

/// Returns a human-readable string describing IOCP availability.
#[must_use]
pub fn iocp_availability_reason() -> String {
    "IOCP unavailable: platform is not Windows".to_string()
}

/// Configuration for IOCP instances (informational only on this platform).
#[derive(Debug, Clone)]
pub struct IocpConfig {
    /// Number of concurrent I/O operations.
    pub concurrent_ops: u32,
    /// Size of each I/O buffer.
    pub buffer_size: usize,
    /// Whether to use unbuffered I/O (no-op on non-Windows).
    pub unbuffered: bool,
    /// Whether to use write-through (no-op on non-Windows).
    pub write_through: bool,
}

impl Default for IocpConfig {
    fn default() -> Self {
        Self {
            concurrent_ops: default_concurrent_ops(),
            buffer_size: 64 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }
}

impl IocpConfig {
    /// Creates a config optimized for large file transfers.
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
