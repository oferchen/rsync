//! File operation tracing: open, close, create (Level 1).

#[cfg(feature = "tracing")]
use tracing::debug;

/// Trace a file open operation (Level 1).
///
/// # Arguments
///
/// * `path` - Path to the file being opened
/// * `size` - Size of the file in bytes (0 if unknown)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_open(path: &str, size: u64) {
    debug!(
        target: "rsync::io",
        operation = "open",
        path = path,
        size = size,
        "[IO1] open {} ({} bytes)",
        path,
        size
    );
}

/// Trace a file open operation (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_open(_path: &str, _size: u64) {}

/// Trace a file close operation (Level 1).
///
/// # Arguments
///
/// * `path` - Path to the file being closed
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_close(path: &str) {
    debug!(
        target: "rsync::io",
        operation = "close",
        path = path,
        "[IO1] close {}",
        path
    );
}

/// Trace a file close operation (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_close(_path: &str) {}

/// Trace a file create operation (Level 1).
///
/// # Arguments
///
/// * `path` - Path to the file being created
/// * `preallocate_size` - Pre-allocated size if any
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_create(path: &str, preallocate_size: Option<u64>) {
    if let Some(size) = preallocate_size {
        debug!(
            target: "rsync::io",
            operation = "create",
            path = path,
            preallocate = size,
            "[IO1] create {} (preallocate {} bytes)",
            path,
            size
        );
    } else {
        debug!(
            target: "rsync::io",
            operation = "create",
            path = path,
            "[IO1] create {}",
            path
        );
    }
}

/// Trace a file create operation (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_create(_path: &str, _preallocate_size: Option<u64>) {}
