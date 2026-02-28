//! Read/write/seek/sync tracing (Level 2).

#[cfg(feature = "tracing")]
use tracing::debug;

/// Trace a read operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file being read (or description like "fd:5")
/// * `bytes_read` - Number of bytes actually read
/// * `position` - Current position in the file after the read
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_read(path: &str, bytes_read: usize, position: u64) {
    debug!(
        target: "rsync::io",
        operation = "read",
        path = path,
        bytes = bytes_read,
        position = position,
        "[IO2] read {} bytes from {} (pos={})",
        bytes_read,
        path,
        position
    );
}

/// Trace a read operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_read(_path: &str, _bytes_read: usize, _position: u64) {}

/// Trace a write operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file being written (or description like "fd:5")
/// * `bytes_written` - Number of bytes actually written
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_write(path: &str, bytes_written: usize) {
    debug!(
        target: "rsync::io",
        operation = "write",
        path = path,
        bytes = bytes_written,
        "[IO2] write {} bytes to {}",
        bytes_written,
        path
    );
}

/// Trace a write operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_write(_path: &str, _bytes_written: usize) {}

/// Trace a seek operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `from_pos` - Previous position
/// * `to_pos` - New position
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_seek(path: &str, from_pos: u64, to_pos: u64) {
    debug!(
        target: "rsync::io",
        operation = "seek",
        path = path,
        from = from_pos,
        to = to_pos,
        "[IO2] seek {} from {} to {}",
        path,
        from_pos,
        to_pos
    );
}

/// Trace a seek operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_seek(_path: &str, _from_pos: u64, _to_pos: u64) {}

/// Trace a sync/flush operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file being synced
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_sync(path: &str) {
    debug!(
        target: "rsync::io",
        operation = "sync",
        path = path,
        "[IO2] sync {}",
        path
    );
}

/// Trace a sync/flush operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_sync(_path: &str) {}
