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
pub const IOCP_MIN_FILE_SIZE: u64 = 64 * 1024; // 64 KB

/// Default number of concurrent I/O operations per completion port.
pub const DEFAULT_CONCURRENT_OPS: u32 = 4;

/// Default I/O buffer size for IOCP operations.
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024; // 64 KB

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
            concurrent_ops: DEFAULT_CONCURRENT_OPS,
            buffer_size: DEFAULT_BUFFER_SIZE,
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
            concurrent_ops: 8,
            buffer_size: 256 * 1024,
            unbuffered: false,
            write_through: false,
        }
    }

    /// Creates a config optimized for many small files.
    #[must_use]
    pub fn for_small_files() -> Self {
        Self {
            concurrent_ops: 4,
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
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default_values() {
        let config = IocpConfig::default();
        assert_eq!(config.concurrent_ops, DEFAULT_CONCURRENT_OPS);
        assert_eq!(config.buffer_size, DEFAULT_BUFFER_SIZE);
        assert!(!config.unbuffered);
        assert!(!config.write_through);
    }

    #[test]
    fn config_large_files_preset() {
        let config = IocpConfig::for_large_files();
        assert_eq!(config.concurrent_ops, 8);
        assert_eq!(config.buffer_size, 256 * 1024);
    }

    #[test]
    fn config_small_files_preset() {
        let config = IocpConfig::for_small_files();
        assert_eq!(config.concurrent_ops, 4);
        assert_eq!(config.buffer_size, 16 * 1024);
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
