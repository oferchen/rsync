//! io_uring-specific tracing (Level 2-3) and mmap tracing.

#[cfg(feature = "tracing")]
use tracing::{debug, trace};

/// Trace an io_uring submission (Level 3).
///
/// # Arguments
///
/// * `operation` - Type of io_uring operation (read, write, fsync)
/// * `fd` - File descriptor
/// * `offset` - Offset for the operation
/// * `length` - Length of data for read/write
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_uring_submit(operation: &str, fd: i32, offset: u64, length: usize) {
    trace!(
        target: "rsync::io",
        operation = "io_uring_submit",
        io_op = operation,
        fd = fd,
        offset = offset,
        length = length,
        "[IO3] io_uring submit: {} fd={} offset={} len={}",
        operation,
        fd,
        offset,
        length
    );
}

/// Trace an io_uring submission (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_uring_submit(_operation: &str, _fd: i32, _offset: u64, _length: usize) {}

/// Trace an io_uring completion (Level 3).
///
/// # Arguments
///
/// * `result` - Result of the operation (bytes transferred or error code)
/// * `user_data` - User data associated with the completion
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_uring_complete(result: i32, user_data: u64) {
    trace!(
        target: "rsync::io",
        operation = "io_uring_complete",
        result = result,
        user_data = user_data,
        "[IO3] io_uring complete: result={} user_data=0x{:x}",
        result,
        user_data
    );
}

/// Trace an io_uring completion (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_uring_complete(_result: i32, _user_data: u64) {}

/// Trace mmap operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file being mapped
/// * `size` - Size of the mapping
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_mmap(path: &str, size: u64) {
    debug!(
        target: "rsync::io",
        operation = "mmap",
        path = path,
        size = size,
        "[IO2] mmap {} ({} bytes)",
        path,
        size
    );
}

/// Trace mmap operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_mmap(_path: &str, _size: u64) {}

/// Trace mmap unmap operation (Level 2).
///
/// # Arguments
///
/// * `path` - Path to the file being unmapped
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_munmap(path: &str) {
    debug!(
        target: "rsync::io",
        operation = "munmap",
        path = path,
        "[IO2] munmap {}",
        path
    );
}

/// Trace mmap unmap operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_munmap(_path: &str) {}

/// Trace mmap advice (Level 3).
///
/// # Arguments
///
/// * `path` - Path to the file
/// * `advice` - Type of advice (sequential, random, willneed)
/// * `offset` - Starting offset (0 for whole file)
/// * `length` - Length of region (0 for whole file)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_mmap_advise(path: &str, advice: &str, offset: usize, length: usize) {
    trace!(
        target: "rsync::io",
        operation = "madvise",
        path = path,
        advice = advice,
        offset = offset,
        length = length,
        "[IO3] madvise {} {} (offset={}, len={})",
        path,
        advice,
        offset,
        length
    );
}

/// Trace mmap advice (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_mmap_advise(_path: &str, _advice: &str, _offset: usize, _length: usize) {}
