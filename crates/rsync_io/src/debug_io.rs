//! DEBUG_IO tracing support for rsync I/O operations.
//!
//! This module provides debug tracing at four levels, mirroring upstream rsync's DEBUG_IO:
//!
//! - **Level 1**: Basic I/O operations (stream open/close, connection establish)
//! - **Level 2**: Read/write operations with sizes
//! - **Level 3**: Buffer management (negotiation buffers, replay state)
//! - **Level 4**: Detailed byte-level tracing (protocol bytes, hex dumps)
//!
//! # Usage
//!
//! Enable tracing by setting the `tracing` feature and using the appropriate
//! debug level with rsync's `--debug=io` flag (e.g., `--debug=io4` for level 4).
//!
//! ```rust,ignore
//! use rsync_io::debug_io;
//!
//! // Level 1: Connection/stream operations
//! debug_io::trace_stream_open("tcp://host:873");
//! debug_io::trace_stream_close("tcp://host:873");
//!
//! // Level 2: Read/write with sizes
//! debug_io::trace_stream_read(512);
//! debug_io::trace_stream_write(256);
//!
//! // Level 3: Buffer management
//! debug_io::trace_negotiation_buffer_state(pos, len, total);
//!
//! // Level 4: Byte-level details
//! debug_io::trace_protocol_bytes("banner", &data);
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

// ─────────────────────────────────────────────────────────────────────────────
// Level 1: Basic I/O operations
// ─────────────────────────────────────────────────────────────────────────────

/// Trace opening a stream/connection (Level 1).
///
/// # Arguments
///
/// * `description` - Description of the stream (e.g., "tcp://host:873", "ssh://user@host")
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_stream_open(description: &str) {
    debug!(
        target: "rsync::io",
        operation = "stream_open",
        description = description,
        "[IO1] open stream: {}",
        description
    );
}

/// Trace opening a stream/connection (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_stream_open(_description: &str) {}

/// Trace closing a stream/connection (Level 1).
///
/// # Arguments
///
/// * `description` - Description of the stream being closed
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_stream_close(description: &str) {
    debug!(
        target: "rsync::io",
        operation = "stream_close",
        description = description,
        "[IO1] close stream: {}",
        description
    );
}

/// Trace closing a stream/connection (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_stream_close(_description: &str) {}

/// Trace SSH process spawn (Level 1).
///
/// # Arguments
///
/// * `command` - SSH command being executed
/// * `host` - Target host
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_ssh_spawn(command: &str, host: &str) {
    debug!(
        target: "rsync::io",
        operation = "ssh_spawn",
        command = command,
        host = host,
        "[IO1] spawn SSH: {} -> {}",
        command,
        host
    );
}

/// Trace SSH process spawn (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_ssh_spawn(_command: &str, _host: &str) {}

/// Trace negotiation style determination (Level 1).
///
/// # Arguments
///
/// * `style` - Negotiation style (binary/legacy)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_negotiation_style(style: &str) {
    debug!(
        target: "rsync::io",
        operation = "negotiation_style",
        style = style,
        "[IO1] negotiation style: {}",
        style
    );
}

/// Trace negotiation style determination (Level 1) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_negotiation_style(_style: &str) {}

// ─────────────────────────────────────────────────────────────────────────────
// Level 2: Read/write operations with sizes
// ─────────────────────────────────────────────────────────────────────────────

/// Trace a stream read operation (Level 2).
///
/// # Arguments
///
/// * `bytes_read` - Number of bytes actually read
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_stream_read(bytes_read: usize) {
    debug!(
        target: "rsync::io",
        operation = "stream_read",
        bytes = bytes_read,
        "[IO2] read {} bytes from stream",
        bytes_read
    );
}

/// Trace a stream read operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_stream_read(_bytes_read: usize) {}

/// Trace a stream write operation (Level 2).
///
/// # Arguments
///
/// * `bytes_written` - Number of bytes actually written
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_stream_write(bytes_written: usize) {
    debug!(
        target: "rsync::io",
        operation = "stream_write",
        bytes = bytes_written,
        "[IO2] write {} bytes to stream",
        bytes_written
    );
}

/// Trace a stream write operation (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_stream_write(_bytes_written: usize) {}

/// Trace reading from buffered replay (Level 2).
///
/// # Arguments
///
/// * `bytes_read` - Number of bytes read from buffer
/// * `from_inner` - Whether reading from inner stream (true) or replay buffer (false)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffered_read(bytes_read: usize, from_inner: bool) {
    let source = if from_inner { "inner stream" } else { "replay buffer" };
    debug!(
        target: "rsync::io",
        operation = "buffered_read",
        bytes = bytes_read,
        from_inner = from_inner,
        "[IO2] read {} bytes from {}",
        bytes_read,
        source
    );
}

/// Trace reading from buffered replay (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffered_read(_bytes_read: usize, _from_inner: bool) {}

/// Trace banner exchange (Level 2).
///
/// # Arguments
///
/// * `direction` - "send" or "recv"
/// * `banner` - Banner content (trimmed)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_banner(direction: &str, banner: &str) {
    debug!(
        target: "rsync::io",
        operation = "banner",
        direction = direction,
        banner = banner,
        "[IO2] {} banner: {}",
        direction,
        banner.trim()
    );
}

/// Trace banner exchange (Level 2) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_banner(_direction: &str, _banner: &str) {}

// ─────────────────────────────────────────────────────────────────────────────
// Level 3: Buffer management
// ─────────────────────────────────────────────────────────────────────────────

/// Trace negotiation buffer state (Level 3).
///
/// # Arguments
///
/// * `description` - Description of the state change
/// * `replay_pos` - Current replay position
/// * `buffered_len` - Total bytes buffered
/// * `sniffed_len` - Length of sniffed prefix
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_negotiation_buffer_state(
    description: &str,
    replay_pos: usize,
    buffered_len: usize,
    sniffed_len: usize,
) {
    trace!(
        target: "rsync::io",
        operation = "negotiation_buffer",
        description = description,
        replay_pos = replay_pos,
        buffered_len = buffered_len,
        sniffed_len = sniffed_len,
        "[IO3] {} (replay_pos={}, buffered={}, sniffed={})",
        description,
        replay_pos,
        buffered_len,
        sniffed_len
    );
}

/// Trace negotiation buffer state (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_negotiation_buffer_state(
    _description: &str,
    _replay_pos: usize,
    _buffered_len: usize,
    _sniffed_len: usize,
) {
}

/// Trace buffer consumption (Level 3).
///
/// # Arguments
///
/// * `consumed` - Number of bytes consumed
/// * `remaining` - Number of bytes remaining in buffer
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_consume(consumed: usize, remaining: usize) {
    trace!(
        target: "rsync::io",
        operation = "buffer_consume",
        consumed = consumed,
        remaining = remaining,
        "[IO3] consume {} bytes, {} remaining",
        consumed,
        remaining
    );
}

/// Trace buffer consumption (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_consume(_consumed: usize, _remaining: usize) {}

/// Trace buffer extension/growth (Level 3).
///
/// # Arguments
///
/// * `added` - Number of bytes added
/// * `new_total` - New total buffer size
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_buffer_extend(added: usize, new_total: usize) {
    trace!(
        target: "rsync::io",
        operation = "buffer_extend",
        added = added,
        new_total = new_total,
        "[IO3] extend buffer by {} bytes, new total={}",
        added,
        new_total
    );
}

/// Trace buffer extension/growth (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_buffer_extend(_added: usize, _new_total: usize) {}

/// Trace stream mapping/transformation (Level 3).
///
/// # Arguments
///
/// * `operation` - Type of mapping (map_inner, try_map_inner, clone)
/// * `success` - Whether the operation succeeded
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_stream_map(operation: &str, success: bool) {
    trace!(
        target: "rsync::io",
        operation = "stream_map",
        map_type = operation,
        success = success,
        "[IO3] stream {} {}",
        operation,
        if success { "succeeded" } else { "failed" }
    );
}

/// Trace stream mapping/transformation (Level 3) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_stream_map(_operation: &str, _success: bool) {}

// ─────────────────────────────────────────────────────────────────────────────
// Level 4: Detailed byte-level tracing
// ─────────────────────────────────────────────────────────────────────────────

/// Trace protocol bytes with hex dump (Level 4).
///
/// # Arguments
///
/// * `description` - Description of the data (e.g., "handshake", "banner", "checksum")
/// * `data` - The protocol bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_protocol_bytes(description: &str, data: &[u8]) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);
    let truncated = if data.len() > MAX_HEX_DUMP_BYTES {
        format!(" ({} more bytes)", data.len() - MAX_HEX_DUMP_BYTES)
    } else {
        String::new()
    };

    trace!(
        target: "rsync::io",
        operation = "protocol_bytes",
        description = description,
        length = data.len(),
        hex = %hex,
        "[IO4] {}: {}{}",
        description,
        hex,
        truncated
    );
}

/// Trace protocol bytes with hex dump (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_protocol_bytes(_description: &str, _data: &[u8]) {}

/// Trace sniffed prefix bytes (Level 4).
///
/// # Arguments
///
/// * `prefix` - The sniffed bytes
/// * `decision` - Current decision state
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_sniff_bytes(prefix: &[u8], decision: &str) {
    let hex = format_hex_dump(prefix);

    trace!(
        target: "rsync::io",
        operation = "sniff_bytes",
        decision = decision,
        length = prefix.len(),
        hex = %hex,
        "[IO4] sniff: {} -> {}",
        hex,
        decision
    );
}

/// Trace sniffed prefix bytes (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_sniff_bytes(_prefix: &[u8], _decision: &str) {}

/// Trace raw bytes being read from stream (Level 4).
///
/// # Arguments
///
/// * `data` - The raw bytes
/// * `source` - Description of the source
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_raw_read(data: &[u8], source: &str) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);
    let truncated = if data.len() > MAX_HEX_DUMP_BYTES {
        format!(" ({} more bytes)", data.len() - MAX_HEX_DUMP_BYTES)
    } else {
        String::new()
    };

    trace!(
        target: "rsync::io",
        operation = "raw_read",
        source = source,
        length = data.len(),
        hex = %hex,
        "[IO4] read from {}: {}{}",
        source,
        hex,
        truncated
    );
}

/// Trace raw bytes being read from stream (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_raw_read(_data: &[u8], _source: &str) {}

/// Trace raw bytes being written to stream (Level 4).
///
/// # Arguments
///
/// * `data` - The raw bytes
/// * `destination` - Description of the destination
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_raw_write(data: &[u8], destination: &str) {
    let display_len = data.len().min(MAX_HEX_DUMP_BYTES);
    let hex = format_hex_dump(&data[..display_len]);
    let truncated = if data.len() > MAX_HEX_DUMP_BYTES {
        format!(" ({} more bytes)", data.len() - MAX_HEX_DUMP_BYTES)
    } else {
        String::new()
    };

    trace!(
        target: "rsync::io",
        operation = "raw_write",
        destination = destination,
        length = data.len(),
        hex = %hex,
        "[IO4] write to {}: {}{}",
        destination,
        hex,
        truncated
    );
}

/// Trace raw bytes being written to stream (Level 4) - no-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_raw_write(_data: &[u8], _destination: &str) {}

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
    fn test_format_hex_dump_rsyncd_banner() {
        // Test with typical rsync banner prefix
        let data = b"@RSYNCD: 31.0\n";
        let result = format_hex_dump(data);
        assert!(result.contains("40 52 53 59")); // @RSY in hex
        assert!(result.contains("|@RSYNCD: 31.0.|")); // newline becomes dot
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
        trace_stream_open("tcp://localhost:873");
        trace_stream_close("tcp://localhost:873");
        trace_ssh_spawn("ssh", "localhost");
        trace_negotiation_style("binary");

        // Level 2
        trace_stream_read(512);
        trace_stream_write(256);
        trace_buffered_read(100, true);
        trace_buffered_read(100, false);
        trace_banner("recv", "@RSYNCD: 31.0");

        // Level 3
        trace_negotiation_buffer_state("initial", 0, 100, 14);
        trace_buffer_consume(50, 50);
        trace_buffer_extend(100, 200);
        trace_stream_map("map_inner", true);

        // Level 4
        trace_protocol_bytes("banner", b"@RSYNCD: 31.0\n");
        trace_sniff_bytes(b"@RSY", "NeedMoreData");
        trace_raw_read(&[1, 2, 3], "socket");
        trace_raw_write(&[1, 2, 3], "socket");
    }
}
