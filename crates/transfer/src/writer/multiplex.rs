//! Buffered writer that frames output in `MSG_DATA` multiplex frames.
//!
//! Mirrors upstream rsync's buffering behavior in `io.c` where a single buffer
//! accumulates data before flushing to the socket. Uses 64KB buffer size to
//! compensate for frame headers and batch approximately 2 wire chunks per flush.

use std::io::{self, IoSlice, Write};
use std::sync::{Arc, Mutex};

use protocol::{MessageCode, MessageHeader};

/// Writer that wraps data in multiplex `MSG_DATA` frames.
///
/// Buffers writes to avoid sending tiny multiplex frames for every write call.
/// Mirrors upstream rsync's `iobuf_out` buffering pattern in `io.c`.
///
/// Tracks a `dirty` flag to avoid redundant `inner.flush()` syscalls when
/// no data has been written since the last successful flush. This eliminates
/// the per-file flush overhead that caused BPR regressions (BPR-1/2/3/6/9)
/// where oc-rsync issued 1 syscall per file vs upstream's ~10-files-per-write
/// batching pattern. Phase boundaries and control messages still flush
/// immediately when data is pending.
///
/// When a `batch_recorder` is attached, all data written through the `Write`
/// trait (pre-multiplex framing) is copied to the recorder. This mirrors
/// upstream rsync's `write_batch_monitor_out` in `io.c:write_buf()` which
/// tees data before multiplex framing is applied.
pub(crate) struct MultiplexWriter<W> {
    inner: W,
    buffer: Vec<u8>,
    /// Buffer size matching upstream rsync's IO_BUFFER_SIZE pattern.
    buffer_size: usize,
    /// True when data has been written to `inner` since the last successful
    /// `inner.flush()`. Prevents redundant flush syscalls on transfer hot
    /// paths where `flush()` is called per-iteration but many iterations
    /// produce no output (control NDX handling, non-transfer items).
    dirty: bool,
    /// Optional recorder for batch mode - captures pre-mux data.
    /// upstream: `io.c` `write_batch_monitor_out` + `safe_write(batch_fd, buf, len)`
    pub(crate) batch_recorder: Option<Arc<Mutex<dyn Write + Send>>>,
}

/// Default buffer size - 64KB to batch ~2 wire chunks per flush.
const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

impl<W: Write> MultiplexWriter<W> {
    /// Creates a new multiplex writer with 64KB buffering.
    ///
    /// The 64KB buffer matches upstream rsync's `iobuf_out` pattern where a single
    /// buffer accumulates data before flushing to the socket. Upstream uses
    /// `IO_BUFFER_SIZE` (32KB) in `rsync.h`, but we use 64KB to compensate for
    /// `MSG_DATA` frame headers (4 bytes per frame) and to batch ~2 wire chunks
    /// per flush for better syscall efficiency.
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(DEFAULT_BUFFER_SIZE),
            buffer_size: DEFAULT_BUFFER_SIZE,
            dirty: false,
            batch_recorder: None,
        }
    }

    /// Flushes the internal buffer by sending it as a `MSG_DATA` frame.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let code = MessageCode::Data;
            protocol::send_msg(&mut self.inner, code, &self.buffer)?;
            self.buffer.clear();
            self.dirty = true;
        }
        Ok(())
    }

    /// Sends a control message with the specified message code.
    ///
    /// Unlike the `Write` trait which always sends `MSG_DATA`, this method
    /// allows sending other message types like `MSG_IO_TIMEOUT`.
    /// Flushes buffered data first to maintain message ordering.
    ///
    /// Batchable message codes (`MSG_INFO`, `MSG_WARNING`) skip the
    /// immediate flush, letting the write buffer coalesce multiple
    /// control frames into fewer TCP segments. This matches upstream
    /// rsync's `send_msg()` in `io.c` which appends to `iobuf.msg`
    /// without flushing. Latency-sensitive codes (ERROR, REDO, etc.)
    /// still flush immediately.
    pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        self.flush_buffer()?;
        protocol::send_msg(&mut self.inner, code, payload)?;
        self.dirty = true;
        if code.requires_immediate_flush() {
            self.inner.flush()?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Writes raw bytes directly to the inner writer, bypassing multiplex framing.
    ///
    /// Used for protocol exchanges like goodbye handshakes where upstream rsync
    /// writes directly without `MSG_DATA` wrapping.
    pub(crate) fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        self.flush_buffer()?;
        self.inner.write_all(data)?;
        self.inner.flush()?;
        self.dirty = false;
        Ok(())
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // upstream: io.c:write_buf() - tee pre-mux data to batch_fd
        if let Some(ref recorder) = self.batch_recorder {
            let mut rec = recorder
                .lock()
                .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
            rec.write_all(buf)?;
        }

        if self.buffer.len() + buf.len() > self.buffer_size {
            self.flush_buffer()?;
        }

        // If buf fills or exceeds the buffer, send directly as a MSG_DATA frame.
        // This bypasses one copy (into the internal buffer) for bulk data,
        // matching upstream rsync's behavior of flushing iobuf_out when full.
        if buf.len() >= self.buffer_size {
            let code = MessageCode::Data;
            protocol::send_msg(&mut self.inner, code, buf)?;
            self.dirty = true;
            return Ok(buf.len());
        }

        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// Writes multiple buffers using vectored I/O to reduce syscall overhead.
    ///
    /// Small writes are batched into the internal buffer. When the total data
    /// exceeds the buffer size, a `MSG_DATA` frame is written directly to the
    /// inner writer without an intermediate allocation - the header is written
    /// first, then each slice sequentially. This mirrors upstream rsync's
    /// `writefd_unbuffered()` pattern in `io.c`.
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let total_len: usize = bufs.iter().map(|b| b.len()).sum();

        if total_len == 0 {
            return Ok(0);
        }

        // upstream: io.c:write_buf() - tee pre-mux data to batch_fd
        if let Some(ref recorder) = self.batch_recorder {
            let mut rec = recorder
                .lock()
                .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
            for buf in bufs {
                rec.write_all(buf)?;
            }
        }

        // Fast path: if everything fits in remaining buffer space, copy all at once
        if self.buffer.len() + total_len <= self.buffer_size {
            for buf in bufs {
                self.buffer.extend_from_slice(buf);
            }
            return Ok(total_len);
        }

        self.flush_buffer()?;

        if total_len <= self.buffer_size {
            for buf in bufs {
                self.buffer.extend_from_slice(buf);
            }
        } else {
            // Large vectored write: send MSG_DATA frame directly to inner writer.
            // Writes header + each slice sequentially, avoiding an intermediate
            // Vec allocation.
            let header = MessageHeader::new(MessageCode::Data, total_len as u32)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let header_bytes = header.encode();
            self.inner.write_all(&header_bytes)?;
            for buf in bufs {
                self.inner.write_all(buf)?;
            }
            self.dirty = true;
        }

        Ok(total_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        if self.dirty {
            self.inner.flush()?;
            self.dirty = false;
        }
        Ok(())
    }
}
