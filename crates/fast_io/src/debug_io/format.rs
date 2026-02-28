//! Buffer tracing (Level 3) and hex dump formatting (Level 4).

#[cfg(feature = "tracing")]
use tracing::trace;

use super::MAX_HEX_DUMP_BYTES;

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
pub(super) fn format_hex_dump(data: &[u8]) -> String {
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
