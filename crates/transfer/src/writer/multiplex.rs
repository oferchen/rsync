//! Buffered writer that frames output in `MSG_DATA` multiplex frames.
//!
//! Mirrors upstream rsync's buffering behavior in `io.c` where a single buffer
//! accumulates data before flushing to the socket. Uses 64KB buffer size to
//! compensate for frame headers and batch approximately 2 wire chunks per flush.
//!
//! The frame header is reserved *inside* that buffer and back-filled at flush
//! time (upstream io.c:2461-2462 and io.c:687-688), so a header and its payload
//! are adjacent bytes of one buffer before any syscall runs. Nothing can be
//! scheduled between them and no partial write can leave a header on the wire
//! without its payload queued behind it.

use std::io::{self, IoSlice, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use protocol::iobuf::OutBuf;
use protocol::{MESSAGE_HEADER_LEN, MessageCode, MessageHeader};

/// Destination selector for bytes written through a batch-recording writer.
///
/// Upstream keeps two distinct batch wirings and this enum names them:
/// `--write-batch` tees every socket write into `batch_fd`
/// (`io.c:2282 write_batch_monitor_out`), while `--only-write-batch` sends the
/// token stream to `batch_fd` *instead of* the socket
/// (`sender.c:217 f_xfer = write_batch < 0 ? batch_fd : f_out`).
///
/// # Upstream Reference
///
/// - `io.c:2281-2283` - `if (f == write_batch_monitor_out) safe_write(batch_fd, ...)`
/// - `sender.c:217` - `int f_xfer = write_batch < 0 ? batch_fd : f_out;`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchRoute {
    /// Write to the wire and copy into the batch (upstream's tee monitor).
    #[default]
    Tee,
    /// Write into the batch only, leaving the wire untouched.
    Divert,
}

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
    /// Circular output buffer whose first four bytes are the reserved
    /// `MSG_DATA` header. upstream: `iobuf.out` (io.c:91-100).
    buffer: OutBuf,
    /// Payload bytes buffered before a flush is forced. Matches upstream's
    /// `IO_BUFFER_SIZE`-derived output sizing pattern.
    buffer_size: usize,
    /// Reusable staging area for control frames, which upstream builds
    /// header-and-payload in one shot (io.c:965-1058 `send_msg`), and for the
    /// oversized `MSG_DATA` frames that do not fit the circular buffer.
    scratch: Vec<u8>,
    /// True when data has been written to `inner` since the last successful
    /// `inner.flush()`. Prevents redundant flush syscalls on transfer hot
    /// paths where `flush()` is called per-iteration but many iterations
    /// produce no output (control NDX handling, non-transfer items).
    dirty: bool,
    /// Optional recorder for batch mode - captures pre-mux data.
    /// upstream: `io.c` `write_batch_monitor_out` + `safe_write(batch_fd, buf, len)`
    pub(crate) batch_recorder: Option<Arc<Mutex<dyn Write + Send>>>,
    /// Selects whether recorded bytes are also framed onto the wire.
    /// upstream: `sender.c:501` `f_xfer = write_batch < 0 ? batch_fd : f_out`
    pub(crate) batch_route: BatchRoute,
    /// Instant of the last actual write to `inner`, tracking upstream's
    /// `last_io_out`. A lull is measured from this point.
    last_io_out: Instant,
    /// The keep-alive lull interval, `None` when `--timeout` is not set.
    ///
    /// upstream: `io.c:set_io_timeout()` sets `allowed_lull = (io_timeout + 1) / 2`
    /// (io.c:1151); a keepalive is emitted once this much time has elapsed with
    /// no output.
    allowed_lull: Option<Duration>,
}

/// Default buffer size - 64KB to batch ~2 wire chunks per flush.
const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// Capacity the control-frame staging buffer is allowed to retain.
///
/// A single oversized frame must not pin its staging allocation for the rest of
/// the run; upstream keeps steady-state memory bounded by never growing its
/// buffers at all (io.c:579, io.c:594).
const SCRATCH_RETAIN: usize = DEFAULT_BUFFER_SIZE + MESSAGE_HEADER_LEN;

impl<W: Write> MultiplexWriter<W> {
    /// Creates a new multiplex writer with 64KB buffering.
    ///
    /// The 64KB buffer matches upstream rsync's `iobuf_out` pattern where a single
    /// buffer accumulates data before flushing to the socket. Upstream uses
    /// `IO_BUFFER_SIZE` (32KB) in `rsync.h`, but we use 64KB to compensate for
    /// `MSG_DATA` frame headers (4 bytes per frame) and to batch ~2 wire chunks
    /// per flush for better syscall efficiency.
    pub(crate) fn new(inner: W) -> Self {
        let mut buffer = OutBuf::new(DEFAULT_BUFFER_SIZE);
        // upstream: io.c:2455-2462 io_start_multiplex_out() reserves the first
        // MSG_DATA header before a single payload byte is buffered.
        buffer.start_multiplex();
        Self {
            inner,
            buffer,
            buffer_size: DEFAULT_BUFFER_SIZE,
            scratch: Vec::new(),
            dirty: false,
            batch_recorder: None,
            batch_route: BatchRoute::default(),
            last_io_out: Instant::now(),
            allowed_lull: None,
        }
    }

    /// Configures the keep-alive lull interval.
    ///
    /// upstream: `io.c:set_io_timeout()` derives `allowed_lull = (io_timeout + 1) / 2`
    /// (io.c:1151). Passing `None` (no `--timeout`) disables lull keepalives, so
    /// the default transfer path stays byte-for-byte identical.
    pub(crate) fn set_allowed_lull(&mut self, lull: Option<Duration>) {
        self.allowed_lull = lull;
        self.last_io_out = Instant::now();
    }

    /// Returns the configured keep-alive lull interval, or `None` when
    /// `--timeout` is not set.
    ///
    /// Callers use this to derive upstream's `lull_mod = allowed_lull * 5`
    /// cadence (sender.c:76) when poking keepalives inside a long read loop.
    pub(crate) fn allowed_lull(&self) -> Option<Duration> {
        self.allowed_lull
    }

    /// Emits a lull keepalive if the configured `allowed_lull` has elapsed with
    /// no output since the last write.
    ///
    /// Returns `true` when an empty `MSG_DATA` keepalive was written.
    ///
    /// Mirrors upstream `io.c:maybe_send_keepalive()` (io.c:1466-1479): the
    /// keepalive is emitted only when a full `allowed_lull` has passed since the
    /// last output and the output buffer sits at a frame boundary. When data is
    /// still buffered, flushing it is itself output activity, so upstream flushes
    /// instead of emitting the empty frame (io.c:1476-1479).
    pub(crate) fn maybe_send_keepalive(&mut self) -> io::Result<bool> {
        let Some(lull) = self.allowed_lull else {
            return Ok(false);
        };
        if self.last_io_out.elapsed() < lull {
            return Ok(false);
        }

        // upstream: io.c:1476-1479 - pending output is flushed rather than
        // emitting a keepalive; the flush itself is the I/O that resets the lull.
        // "Empty" here is `out.len == out_empty_len`, not `len == 0`: the
        // reserved header occupies the first four bytes (io.c:1472).
        if !self.buffer.is_empty() {
            self.flush_buffer()?;
            self.inner.flush()?;
            self.dirty = false;
            self.last_io_out = Instant::now();
            return Ok(false);
        }

        // upstream: io.c:1472-1473 - only at a frame boundary, emit an empty
        // MSG_DATA that the peer absorbs as a no-op keepalive.
        self.write_frame(MessageCode::Data, 0, std::iter::empty())?;
        self.inner.flush()?;
        self.dirty = false;
        self.last_io_out = Instant::now();
        Ok(true)
    }

    /// Copies `chunks` into the attached batch recorder, reporting whether the
    /// bytes have been consumed by the batch alone.
    ///
    /// A `true` return means the caller must skip the wire write entirely: the
    /// batch file *is* the destination for this stream. Without a recorder
    /// attached the route is irrelevant and nothing is ever dropped.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:2255-2258` - `write_buf()` with `f != iobuf.out_fd` bypasses the
    ///   multiplex buffer and writes straight to the fd (here, the batch).
    /// - `io.c:2281-2283` - the tee into `batch_fd` for `--write-batch`.
    fn record_to_batch<'b>(&self, chunks: impl Iterator<Item = &'b [u8]>) -> io::Result<bool> {
        let Some(recorder) = self.batch_recorder.as_ref() else {
            return Ok(false);
        };
        let mut rec = recorder
            .lock()
            .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
        for chunk in chunks {
            rec.write_all(chunk)?;
        }
        Ok(self.batch_route == BatchRoute::Divert)
    }

    /// Flushes the internal buffer by sending it as a `MSG_DATA` frame.
    ///
    /// The header is back-filled into the four bytes reserved at the front of
    /// the buffered run (upstream io.c:687-688) and the whole frame leaves
    /// through one drain loop, so a short write can never separate the header
    /// from its payload.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            self.buffer.flush(&mut self.inner)?;
            self.dirty = true;
            self.last_io_out = Instant::now();
        }
        Ok(())
    }

    /// Writes one frame whose header and payload are staged contiguously.
    ///
    /// upstream: `send_msg()` (io.c:965-1058) builds every non-`MSG_DATA` frame
    /// header-then-payload in one shot inside `iobuf.msg`; the same treatment is
    /// applied to a `MSG_DATA` payload too large for the circular buffer, which
    /// would otherwise be the one place a header and its body could be split
    /// across two `write_all` calls.
    fn write_frame<'b>(
        &mut self,
        code: MessageCode,
        payload_len: usize,
        chunks: impl Iterator<Item = &'b [u8]>,
    ) -> io::Result<()> {
        let len = u32::try_from(payload_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload length overflow"))?;
        let header = MessageHeader::new(code, len)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        self.scratch.clear();
        self.scratch.reserve(MESSAGE_HEADER_LEN + payload_len);
        self.scratch.extend_from_slice(&header.encode());
        for chunk in chunks {
            self.scratch.extend_from_slice(chunk);
        }
        self.inner.write_all(&self.scratch)?;

        if self.scratch.capacity() > SCRATCH_RETAIN {
            self.scratch = Vec::new();
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
        self.write_frame(code, payload.len(), std::iter::once(payload))?;
        self.dirty = true;
        self.last_io_out = Instant::now();
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
        self.last_io_out = Instant::now();
        Ok(())
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // upstream: io.c:write_buf() - tee pre-mux data to batch_fd, or divert
        // to it entirely when this stream is upstream's `f_xfer`.
        if self.record_to_batch(std::iter::once(buf))? {
            return Ok(buf.len());
        }

        if self.buffer.data_len() + buf.len() > self.buffer_size {
            self.flush_buffer()?;
        }

        // A payload that cannot fit the circular buffer is staged contiguously
        // and sent as its own MSG_DATA frame. upstream splits such a write into
        // buffer-sized chunks instead (io.c:2242-2253 write_bigbuf); keeping the
        // single frame preserves the bytes already on the wire while still
        // guaranteeing the header and payload leave together.
        if buf.len() >= self.buffer_size {
            self.write_frame(MessageCode::Data, buf.len(), std::iter::once(buf))?;
            self.dirty = true;
            self.last_io_out = Instant::now();
            return Ok(buf.len());
        }

        // upstream: io.c:2263-2264 write_buf() reaches perform_io() only when
        // the bytes do not fit, so a write that fits costs no syscall at all.
        self.buffer.push(buf);
        Ok(buf.len())
    }

    /// Writes multiple buffers, batching them into the internal buffer.
    ///
    /// Small writes are copied into the circular buffer and cost no syscall.
    /// When the total exceeds the buffer size the slices are staged contiguously
    /// behind their `MSG_DATA` header and written once, so the frame can never
    /// be split across two `write_all` calls.
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let total_len: usize = bufs.iter().map(|b| b.len()).sum();

        if total_len == 0 {
            return Ok(0);
        }

        // upstream: io.c:write_buf() - tee pre-mux data to batch_fd, or divert
        // to it entirely when this stream is upstream's `f_xfer`.
        if self.record_to_batch(bufs.iter().map(|b| &b[..]))? {
            return Ok(total_len);
        }

        // Fast path: if everything fits in remaining buffer space, copy all at once
        if self.buffer.data_len() + total_len <= self.buffer_size {
            for buf in bufs {
                self.buffer.push(buf);
            }
            return Ok(total_len);
        }

        self.flush_buffer()?;

        if total_len <= self.buffer_size {
            for buf in bufs {
                self.buffer.push(buf);
            }
        } else {
            self.write_frame(MessageCode::Data, total_len, bufs.iter().map(|b| &b[..]))?;
            self.dirty = true;
            self.last_io_out = Instant::now();
        }

        Ok(total_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        if self.dirty {
            self.inner.flush()?;
            self.dirty = false;
            self.last_io_out = Instant::now();
        }
        Ok(())
    }
}

#[cfg(test)]
mod keepalive_tests {
    use super::*;
    use protocol::recv_msg;
    use std::io::Cursor;

    /// Without `--timeout` there is no lull tracking: `maybe_send_keepalive` is a
    /// no-op and emits nothing, keeping the default transfer path wire-identical.
    #[test]
    fn no_lull_configured_emits_nothing() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        assert!(!w.maybe_send_keepalive().unwrap());
        assert!(
            out.is_empty(),
            "no keepalive must be written without a lull"
        );
    }

    /// A lull that has not yet elapsed produces no keepalive (upstream gates on
    /// `now - last_io_out >= allowed_lull`, io.c:1466).
    #[test]
    fn lull_not_elapsed_emits_nothing() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        w.set_allowed_lull(Some(Duration::from_secs(3600)));
        assert!(!w.maybe_send_keepalive().unwrap());
        assert!(
            out.is_empty(),
            "keepalive must not fire before the lull elapses"
        );
    }

    /// Once the lull has elapsed at a frame boundary, an empty MSG_DATA keepalive
    /// is emitted, matching upstream `send_msg(MSG_DATA, "", 0, 0)` (io.c:1633).
    #[test]
    fn lull_elapsed_emits_empty_msg_data() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        // A zero lull is always "elapsed", giving a deterministic (non-flaky)
        // trigger without sleeping.
        w.set_allowed_lull(Some(Duration::ZERO));

        assert!(w.maybe_send_keepalive().unwrap());

        let frame = recv_msg(&mut Cursor::new(&out)).unwrap();
        assert_eq!(
            frame.code(),
            MessageCode::Data,
            "keepalive must be MSG_DATA, not MSG_NOOP"
        );
        assert!(
            frame.payload().is_empty(),
            "keepalive payload must be empty"
        );
    }

    /// When output is still buffered, the lull flushes the pending data instead
    /// of emitting an empty frame; the flush is itself the I/O that resets the
    /// lull (upstream io.c:1476-1479).
    #[test]
    fn lull_with_pending_data_flushes_instead_of_keepalive() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        w.set_allowed_lull(Some(Duration::ZERO));

        w.write_all(b"pending").unwrap();
        assert!(!w.maybe_send_keepalive().unwrap());

        // The single frame on the wire carries the real data, not an empty frame.
        let frame = recv_msg(&mut Cursor::new(&out)).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), b"pending");
    }

    /// Emitting a keepalive resets the lull timer, so an immediate follow-up call
    /// does not emit a second keepalive.
    #[test]
    fn keepalive_resets_lull_timer() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        // Non-zero lull so the reset is observable.
        w.set_allowed_lull(Some(Duration::from_millis(50)));
        std::thread::sleep(Duration::from_millis(60));
        assert!(w.maybe_send_keepalive().unwrap(), "first call fires");
        assert!(
            !w.maybe_send_keepalive().unwrap(),
            "second call must not fire until the lull elapses again"
        );
    }
}

/// Tests for the framing guarantees the reserved-header buffer provides.
///
/// The contract under test is upstream's, from io.c: the `MSG_DATA` header is
/// back-filled inside the buffer before any syscall (io.c:687-688), a write
/// that fits does no I/O (io.c:2255-2284), and nothing can be scheduled between
/// a header and the end of its payload.
#[cfg(test)]
mod framing_tests {
    use super::*;
    use protocol::{MESSAGE_HEADER_LEN, MessageHeader};

    /// Writer that records every accepted span and refuses everything past a
    /// byte budget, modelling a socket buffer filling at an arbitrary offset.
    struct StallingWriter {
        sink: Vec<u8>,
        budget: usize,
        writes: usize,
    }

    impl StallingWriter {
        fn new(budget: usize) -> Self {
            Self {
                sink: Vec::new(),
                budget,
                writes: 0,
            }
        }
    }

    impl Write for StallingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.budget == 0 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "socket full"));
            }
            let n = buf.len().min(self.budget);
            self.sink.extend_from_slice(&buf[..n]);
            self.budget -= n;
            self.writes += 1;
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Decodes a complete multiplex stream into `(code, payload)` pairs,
    /// failing if any frame is torn.
    fn parse_frames(mut wire: &[u8]) -> Vec<(MessageCode, Vec<u8>)> {
        let mut frames = Vec::new();
        while !wire.is_empty() {
            assert!(
                wire.len() >= MESSAGE_HEADER_LEN,
                "trailing bytes are not a whole header: {} left",
                wire.len()
            );
            let header = MessageHeader::decode(&wire[..MESSAGE_HEADER_LEN]).expect("valid header");
            let len = header.payload_len_usize();
            let end = MESSAGE_HEADER_LEN + len;
            assert!(
                wire.len() >= end,
                "frame claims {len} payload bytes but only {} follow the header",
                wire.len() - MESSAGE_HEADER_LEN
            );
            frames.push((header.code(), wire[MESSAGE_HEADER_LEN..end].to_vec()));
            wire = &wire[end..];
        }
        frames
    }

    /// Requirement 13: a write whose bytes fit performs no I/O at all, so N
    /// small writes cost exactly one underlying write at flush, not N.
    /// upstream: io.c:2263-2264 - `write_buf()` reaches `perform_io()` only when
    /// `out.len + len > out.size`.
    #[test]
    fn small_writes_cost_one_underlying_write_at_flush() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        for i in 0..64u8 {
            writer.write_all(&[i; 8]).unwrap();
        }
        assert_eq!(writer.inner.writes, 0, "buffered writes issue no I/O");

        writer.flush().unwrap();
        assert_eq!(writer.inner.writes, 1, "one write for sixty-four appends");

        let expected: Vec<u8> = (0..64u8).flat_map(|i| [i; 8]).collect();
        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![(MessageCode::Data, expected)]
        );
    }

    /// A stall at any byte offset of a frame must leave the wire holding an
    /// in-order prefix and nothing else; resuming completes the same frame with
    /// no byte re-sent or lost, and a control frame queued afterwards still
    /// lands strictly after the payload it followed.
    #[test]
    fn stall_at_any_offset_never_tears_a_frame() {
        let payload: Vec<u8> = (0..96u16).map(|i| (i * 5) as u8).collect();
        let frame_len = MESSAGE_HEADER_LEN + payload.len();

        for stall_at in 0..frame_len {
            let mut writer = MultiplexWriter::new(StallingWriter::new(stall_at));
            writer.write_all(&payload).unwrap();

            let err = writer.flush().expect_err("the flush must stall");
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(
                writer.inner.sink.len(),
                stall_at,
                "only the accepted prefix reached the wire"
            );

            writer.inner.budget = usize::MAX;
            writer.flush().unwrap();
            writer.send_message(MessageCode::Info, b"after").unwrap();

            assert_eq!(
                parse_frames(&writer.inner.sink),
                vec![
                    (MessageCode::Data, payload.clone()),
                    (MessageCode::Info, b"after".to_vec()),
                ],
                "stalled at offset {stall_at}"
            );
        }
    }

    /// A control frame is never scheduled between a `MSG_DATA` header and the
    /// end of its payload: buffered data is flushed as a complete frame first.
    /// upstream keeps `MSG_*` in a separate buffer for exactly this reason
    /// (io.c:680-681 puts the in-progress raw run first).
    #[test]
    fn control_frame_never_splits_a_data_payload() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer.write_all(b"data-one").unwrap();
        writer.send_message(MessageCode::Info, b"info").unwrap();
        writer.write_all(b"data-two").unwrap();
        writer.send_message(MessageCode::Error, b"err").unwrap();
        writer.flush().unwrap();

        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![
                (MessageCode::Data, b"data-one".to_vec()),
                (MessageCode::Info, b"info".to_vec()),
                (MessageCode::Data, b"data-two".to_vec()),
                (MessageCode::Error, b"err".to_vec()),
            ]
        );
    }

    /// A payload too large for the circular buffer is staged contiguously and
    /// still leaves as one intact frame, header included.
    #[test]
    fn oversized_payload_stays_one_intact_frame() {
        let payload = vec![0xA5u8; DEFAULT_BUFFER_SIZE + 1024];
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer.write_all(b"lead").unwrap();
        writer.write_all(&payload).unwrap();
        writer.flush().unwrap();

        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![
                (MessageCode::Data, b"lead".to_vec()),
                (MessageCode::Data, payload),
            ]
        );
    }

    /// The oversized-frame staging buffer must not stay resident: upstream keeps
    /// steady-state memory bounded by never growing its buffers (io.c:579, 594).
    #[test]
    fn oversized_staging_buffer_is_released() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer
            .write_all(&vec![7u8; DEFAULT_BUFFER_SIZE * 4])
            .unwrap();
        assert!(
            writer.scratch.capacity() <= SCRATCH_RETAIN,
            "a one-off large frame must not pin its staging allocation"
        );
    }
}
