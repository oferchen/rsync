//! DEBUG_IO tracing support for I/O operations.
//!
//! This module provides debug tracing at four levels, mirroring upstream rsync's DEBUG_IO:
//!
//! - **Level 1**: Basic I/O operations (open, close)
//! - **Level 2**: Read/write operations with sizes
//! - **Level 3**: Buffer management (pool acquire/release, buffer state)
//! - **Level 4**: Detailed byte-level tracing (hex dumps, byte patterns)
//!
//! # Usage
//!
//! Enable tracing by setting the `tracing` feature and using the appropriate
//! debug level with rsync's `--debug=io` flag (e.g., `--debug=io4` for level 4).
//!
//! ```rust,ignore
//! use fast_io::debug_io;
//!
//! // Level 1: Basic I/O operations
//! debug_io::trace_open("/path/to/file", 1024);
//! debug_io::trace_close("/path/to/file");
//!
//! // Level 2: Read/write with sizes
//! debug_io::trace_read("/path/to/file", 512, 1024);
//! debug_io::trace_write("/path/to/file", 256);
//!
//! // Level 3: Buffer management
//! debug_io::trace_buffer_acquire(4096, 3);
//! debug_io::trace_buffer_release(4096);
//!
//! // Level 4: Byte-level details
//! debug_io::trace_bytes_read(&data[..32], 0);
//! debug_io::trace_bytes_written(&data[..32], 0);
//! ```
//!
//! # Integration with rsync logging
//!
//! When the `tracing` feature is enabled, this module emits tracing events with
//! the target `rsync::io`, which integrates with rsync's debug flag system.
//! The tracing level maps to rsync's `--debug=io` levels 1-4.

#[cfg(feature = "tracing")]
use tracing::{debug, trace};

/// Maximum bytes to include in level 4 hex dumps.
pub const MAX_HEX_DUMP_BYTES: usize = 64;

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

/// Trace buffer acquisition from pool (Level 3).
///
/// # Arguments
///
/// * `buffer_size` - Size of the acquired buffer
/// * `pool_available` - Number of buffers available in pool after acquisition
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_acquire(buffer_size: usize, pool_available: usize) {
    trace!(
        target: "rsync::io",
        operation = "buffer_acquire",
        buffer_size = buffer_size,
        pool_available = pool_available,
        "[IO3] acquire buffer ({} bytes), pool has {} available",
        buffer_size,
        pool_available
    );
}

/// Trace buffer acquisition from pool (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_acquire(_buffer_size: usize, _pool_available: usize) {}

/// Trace buffer release back to pool (Level 3).
///
/// # Arguments
///
/// * `buffer_size` - Size of the released buffer
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_release(buffer_size: usize) {
    trace!(
        target: "rsync::io",
        operation = "buffer_release",
        buffer_size = buffer_size,
        "[IO3] release buffer ({} bytes)",
        buffer_size
    );
}

/// Trace buffer release back to pool (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_release(_buffer_size: usize) {}

/// Trace buffer pool creation (Level 3).
///
/// # Arguments
///
/// * `max_buffers` - Maximum number of buffers the pool will retain
/// * `buffer_size` - Size of each buffer
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_pool_create(max_buffers: usize, buffer_size: usize) {
    trace!(
        target: "rsync::io",
        operation = "pool_create",
        max_buffers = max_buffers,
        buffer_size = buffer_size,
        "[IO3] create buffer pool (max={}, size={} bytes)",
        max_buffers,
        buffer_size
    );
}

/// Trace buffer pool creation (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_pool_create(_max_buffers: usize, _buffer_size: usize) {}

/// Trace buffer state change (Level 3).
///
/// # Arguments
///
/// * `description` - Description of the state change
/// * `buffer_pos` - Current position in buffer
/// * `buffer_len` - Total buffer length
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_state(description: &str, buffer_pos: usize, buffer_len: usize) {
    trace!(
        target: "rsync::io",
        operation = "buffer_state",
        description = description,
        position = buffer_pos,
        length = buffer_len,
        "[IO3] {} (pos={}, len={})",
        description,
        buffer_pos,
        buffer_len
    );
}

/// Trace buffer state change (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_state(_description: &str, _buffer_pos: usize, _buffer_len: usize) {}

/// Trace bytes read with hex dump (Level 4).
///
/// # Arguments
///
/// * `data` - The bytes that were read
/// * `offset` - Starting offset in the file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_bytes_read(data: &[u8], offset: u64) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);
    let truncated = if data.len() > MAX_HEX_DUMP_BYTES {
        format!(" ({} more bytes)", data.len() - MAX_HEX_DUMP_BYTES)
    } else {
        String::new()
    };

    trace!(
        target: "rsync::io",
        operation = "bytes_read",
        offset = offset,
        length = data.len(),
        hex = %hex,
        "[IO4] read at {}: {}{}",
        offset,
        hex,
        truncated
    );
}

/// Trace bytes read with hex dump (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_bytes_read(_data: &[u8], _offset: u64) {}

/// Trace bytes written with hex dump (Level 4).
///
/// # Arguments
///
/// * `data` - The bytes that were written
/// * `offset` - Starting offset in the file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_bytes_written(data: &[u8], offset: u64) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);
    let truncated = if data.len() > MAX_HEX_DUMP_BYTES {
        format!(" ({} more bytes)", data.len() - MAX_HEX_DUMP_BYTES)
    } else {
        String::new()
    };

    trace!(
        target: "rsync::io",
        operation = "bytes_written",
        offset = offset,
        length = data.len(),
        hex = %hex,
        "[IO4] write at {}: {}{}",
        offset,
        hex,
        truncated
    );
}

/// Trace bytes written with hex dump (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_bytes_written(_data: &[u8], _offset: u64) {}

/// Trace raw data pattern (Level 4).
///
/// # Arguments
///
/// * `description` - Description of what the data represents
/// * `data` - The data bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_data_pattern(description: &str, data: &[u8]) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);

    trace!(
        target: "rsync::io",
        operation = "data_pattern",
        description = description,
        length = data.len(),
        hex = %hex,
        "[IO4] {}: {} ({} bytes)",
        description,
        hex,
        data.len()
    );
}

/// Trace raw data pattern (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_data_pattern(_description: &str, _data: &[u8]) {}

/// Format bytes as a hex dump string.
///
/// Produces output like: `48 65 6c 6c 6f |Hello|`
#[cfg(feature = "tracing")]
fn format_hex_dump(data: &[u8]) -> String {
    if data.is_empty() {
        return String::from("<empty>");
    }

    let hex_part: String = data
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");

    let ascii_part: String = data
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();

    format!("{hex_part} |{ascii_part}|")
}

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

#[cfg(all(test, feature = "tracing"))]
mod tests {
    use super::*;

    #[test]
    fn test_format_hex_dump_empty() {
        assert_eq!(format_hex_dump(&[]), "<empty>");
    }

    #[test]
    fn test_format_hex_dump_ascii() {
        let data = b"Hello";
        let result = format_hex_dump(data);
        assert_eq!(result, "48 65 6c 6c 6f |Hello|");
    }

    #[test]
    fn test_format_hex_dump_binary() {
        let data = [0x00, 0x01, 0x02, 0xff];
        let result = format_hex_dump(&data);
        assert_eq!(result, "00 01 02 ff |....|");
    }

    #[test]
    fn test_format_hex_dump_mixed() {
        let data = [0x48, 0x69, 0x00, 0x21]; // "Hi" + null + "!"
        let result = format_hex_dump(&data);
        assert_eq!(result, "48 69 00 21 |Hi.!|");
    }
}

#[cfg(test)]
mod no_tracing_tests {
    use super::*;

    // These tests verify that the no-op functions compile and can be called
    // without the tracing feature enabled

    #[test]
    fn test_trace_functions_compile() {
        // Level 1
        trace_open("/test/path", 1024);
        trace_close("/test/path");
        trace_create("/test/path", Some(1024));
        trace_create("/test/path", None);

        // Level 2
        trace_read("/test/path", 512, 1024);
        trace_write("/test/path", 256);
        trace_seek("/test/path", 0, 100);
        trace_sync("/test/path");
        trace_mmap("/test/path", 4096);
        trace_munmap("/test/path");

        // Level 3
        trace_buffer_acquire(4096, 3);
        trace_buffer_release(4096);
        trace_buffer_pool_create(8, 4096);
        trace_buffer_state("test", 0, 100);
        trace_io_uring_submit("read", 5, 0, 1024);
        trace_io_uring_complete(1024, 0x42);
        trace_mmap_advise("/test/path", "sequential", 0, 0);

        // Level 4
        trace_bytes_read(&[1, 2, 3], 0);
        trace_bytes_written(&[1, 2, 3], 0);
        trace_data_pattern("test pattern", &[1, 2, 3]);
    }
}
