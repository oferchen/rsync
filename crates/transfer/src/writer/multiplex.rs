//! Buffered writer that frames output in `MSG_DATA` multiplex frames.
//!
//! Mirrors upstream rsync's buffering behavior in `io.c` where a single buffer
//! accumulates data before flushing to the socket, sized like upstream's
//! `iobuf.out` (io.c:1567) and flushed by the same rule (io.c:2487-2488).
//!
//! The frame header is reserved *inside* that buffer and back-filled at flush
//! time (upstream io.c:2694-2695 and io.c:775-776), so a header and its payload
//! are adjacent bytes of one buffer before any syscall runs. Nothing can be
//! scheduled between them and no partial write can leave a header on the wire
//! without its payload queued behind it.
//!
//! The buffer itself lives in [`MuxOut`], apart from the writer that fills it,
//! so the connection's transport core can drain it too: upstream's
//! `perform_io()` writes `iobuf.out` whenever it is about to wait for input
//! (io.c:766-768), not only when the writer asks.

use std::cell::RefCell;
use std::io::{self, IoSlice, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use protocol::iobuf::{OUT_BUFFER_SIZE, OutBuf};
use protocol::{MESSAGE_HEADER_LEN, MessageCode, MessageHeader};

/// Destination selector for bytes written through a batch-recording writer.
///
/// Upstream keeps two distinct batch wirings and this enum names them:
/// `--write-batch` tees every socket write into `batch_fd`
/// (`io.c:2505-2506 write_batch_monitor_out`), while `--only-write-batch` sends the
/// token stream to `batch_fd` *instead of* the socket
/// (`sender.c:502 f_xfer = write_batch < 0 ? batch_fd : f_out`).
///
/// # Upstream Reference
///
/// - `io.c:2505-2506` - `if (f == write_batch_monitor_out) safe_write(batch_fd, ...)`
/// - `sender.c:502` - `int f_xfer = write_batch < 0 ? batch_fd : f_out;`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatchRoute {
    /// Write to the wire and copy into the batch (upstream's tee monitor).
    #[default]
    Tee,
    /// Write into the batch only, leaving the wire untouched.
    Divert,
}

/// Payload bytes one buffered run holds: upstream's `iobuf.out` less the
/// `MSG_DATA` header reserved at its front (io.c:1567, io.c:2690).
const DEFAULT_BUFFER_SIZE: usize = OUT_BUFFER_SIZE - MESSAGE_HEADER_LEN;

/// Capacity the control-frame staging buffer is allowed to retain.
///
/// A single oversized frame must not pin its staging allocation for the rest of
/// the run; upstream keeps steady-state memory bounded by never growing its
/// buffers at all (io.c:582-598 `restore_iobuf_size()`).
const SCRATCH_RETAIN: usize = DEFAULT_BUFFER_SIZE + MESSAGE_HEADER_LEN;

/// Size of the message queue. upstream: io.c:2688
/// `alloc_xbuf(&iobuf.msg, ROUND_UP_1024(IO_BUFFER_SIZE))`.
const MSG_BUFFER_SIZE: usize = 32 * 1024;

/// Room `send_msg()` reserves beyond the payload: the 4-byte header plus up to
/// 3 bytes lost when a header would straddle the circular end (io.c:1101-1114).
const MSG_FRAME_SLACK: usize = MESSAGE_HEADER_LEN + 3;

/// The buffered output of a multiplexed stream. upstream: `iobuf.out` and
/// `iobuf.msg` (io.c:101-115).
pub(crate) struct MuxOut {
    /// Circular output buffer whose first four bytes are the reserved
    /// `MSG_DATA` header. upstream: `iobuf.out` (io.c:101-115).
    buffer: OutBuf,
    /// Payload bytes buffered before a flush is forced. Matches upstream's
    /// `IO_BUFFER_SIZE`-derived output sizing pattern.
    buffer_size: usize,
    /// Framed messages waiting to be written; they leave ahead of buffered
    /// data. upstream: `iobuf.msg`, filled by `send_msg()` (io.c:1088).
    msgs: Vec<u8>,
    /// Reusable staging area for the oversized `MSG_DATA` frames that do not
    /// fit the circular buffer, and for keepalives.
    scratch: Vec<u8>,
    /// True when data has been written to the sink since its last successful
    /// `flush()`. Prevents redundant flush syscalls on transfer hot paths where
    /// `flush()` is called per-iteration but many iterations produce no output.
    dirty: bool,
    /// Instant of the last actual write to the sink, tracking upstream's
    /// `last_io_out`. A lull is measured from this point.
    last_io_out: Instant,
}

/// The output buffer shared by a multiplex writer and the transport core
/// that drains it.
pub(crate) type SharedMuxOut = Rc<RefCell<MuxOut>>;

impl MuxOut {
    fn new() -> Self {
        let mut buffer = OutBuf::new(DEFAULT_BUFFER_SIZE);
        // upstream: io.c:2690-2695 io_start_multiplex_out() reserves the first
        // MSG_DATA header before a single payload byte is buffered.
        buffer.start_multiplex();
        Self {
            buffer,
            buffer_size: DEFAULT_BUFFER_SIZE,
            msgs: Vec::new(),
            scratch: Vec::new(),
            dirty: false,
            last_io_out: Instant::now(),
        }
    }

    /// True when messages or payload bytes are buffered and not yet written.
    pub(crate) fn has_pending(&self) -> bool {
        !self.msgs.is_empty() || !self.buffer.is_empty()
    }

    /// Writes everything buffered, in perform_io()'s priority order
    /// (io.c:628-636): the rest of a `MSG_DATA` frame whose write stalled
    /// part-way, then every queued message, then the buffered data as one
    /// `MSG_DATA` frame.
    ///
    /// The data frame's header is back-filled into the four bytes reserved at
    /// the front of the buffered run (upstream io.c:775-776) and the whole
    /// frame leaves through one drain loop, so a short write can never separate
    /// the header from its payload.
    pub(crate) fn flush_buffer<W: Write + ?Sized>(&mut self, sink: &mut W) -> io::Result<()> {
        self.write_messages(sink)?;
        if !self.buffer.is_empty() {
            self.buffer.flush(sink)?;
            self.mark_written();
        }
        Ok(())
    }

    /// Writes the queued messages, after finishing a stalled data frame.
    /// upstream: perform_io() with `PIO_NEED_MSGROOM` (io.c:768-803).
    fn write_messages<W: Write + ?Sized>(&mut self, sink: &mut W) -> io::Result<()> {
        if self.buffer.is_flushing() {
            self.buffer.finish_flush(sink)?;
            self.mark_written();
        }
        if !self.msgs.is_empty() {
            sink.write_all(&self.msgs)?;
            self.msgs.clear();
            self.mark_written();
        }
        Ok(())
    }

    /// Queues one message frame without writing anything, unless the queue
    /// lacks room for it, in which case the queued messages are written first.
    /// upstream: io.c:1088-1180 `send_msg()`; the sender's
    /// `perform_io(needed, PIO_NEED_MSGROOM)` at io.c:1115-1117.
    fn queue_message<W: Write + ?Sized>(
        &mut self,
        sink: &mut W,
        code: MessageCode,
        payload: &[u8],
    ) -> io::Result<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload length overflow"))?;
        let header = MessageHeader::new(code, len)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if self.msgs.len() + payload.len() + MSG_FRAME_SLACK > MSG_BUFFER_SIZE {
            self.write_messages(sink)?;
        }
        self.msgs.extend_from_slice(&header.encode());
        self.msgs.extend_from_slice(payload);
        Ok(())
    }

    /// [`Self::flush_buffer`], then flushes `sink` itself if anything reached
    /// it since its last flush.
    pub(crate) fn flush<W: Write + ?Sized>(&mut self, sink: &mut W) -> io::Result<()> {
        self.flush_buffer(sink)?;
        if self.dirty {
            sink.flush()?;
            self.dirty = false;
            self.last_io_out = Instant::now();
        }
        Ok(())
    }

    /// Writes one frame whose header and payload are staged contiguously.
    ///
    /// upstream: `send_msg()` (io.c:1088) builds every non-`MSG_DATA` frame
    /// header-then-payload in one shot inside `iobuf.msg`; the same treatment is
    /// applied to a `MSG_DATA` payload too large for the circular buffer, which
    /// would otherwise be the one place a header and its body could be split
    /// across two `write_all` calls.
    fn write_frame<'b, W: Write + ?Sized>(
        &mut self,
        sink: &mut W,
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
        sink.write_all(&self.scratch)?;

        if self.scratch.capacity() > SCRATCH_RETAIN {
            self.scratch = Vec::new();
        }
        Ok(())
    }

    /// Buffers `buf` as `MSG_DATA` payload, writing the buffered run first
    /// when it does not fit. upstream: io.c:2487-2488 write_buf() reaches
    /// perform_io() only when the bytes do not fit, so a write that fits
    /// costs no syscall at all.
    ///
    /// A payload that cannot fit the circular buffer is staged contiguously
    /// and sent as its own `MSG_DATA` frame. upstream splits such a write into
    /// buffer-sized chunks instead (io.c:2465-2476 write_bigbuf); keeping the
    /// single frame preserves the bytes already on the wire while still
    /// guaranteeing the header and payload leave together.
    fn write_data<W: Write + ?Sized>(&mut self, sink: &mut W, buf: &[u8]) -> io::Result<()> {
        if self.buffer.data_len() + buf.len() > self.buffer_size {
            self.flush_buffer(sink)?;
        }
        if buf.len() > self.buffer_size {
            self.write_frame(sink, MessageCode::Data, buf.len(), std::iter::once(buf))?;
            self.mark_written();
            return Ok(());
        }
        self.buffer.push(buf);
        Ok(())
    }

    /// Records that bytes reached the sink, for the flush and lull logic.
    fn mark_written(&mut self) {
        self.dirty = true;
        self.last_io_out = Instant::now();
    }
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
    /// The buffered frame run, shared with the transport core that may drain
    /// it while waiting for input.
    out: SharedMuxOut,
    /// Optional recorder for batch mode - captures pre-mux data.
    /// upstream: `io.c` `write_batch_monitor_out` + `safe_write(batch_fd, buf, len)`
    pub(crate) batch_recorder: Option<Arc<Mutex<dyn Write + Send>>>,
    /// Selects whether recorded bytes are also framed onto the wire.
    /// upstream: `sender.c:502` `f_xfer = write_batch < 0 ? batch_fd : f_out`
    pub(crate) batch_route: BatchRoute,
    /// The keep-alive lull interval, `None` when `--timeout` is not set.
    ///
    /// upstream: `io.c:set_io_timeout()` sets `allowed_lull = (io_timeout + 1) / 2`
    /// (io.c:1652); a keepalive is emitted once this much time has elapsed with
    /// no output.
    allowed_lull: Option<Duration>,
    /// Payload bytes accepted through the `Write` trait and bound for the
    /// wire; bytes diverted to the batch file are not counted.
    /// upstream: io.c:2502 `total_data_written += len` in `write_buf()`.
    data_written: u64,
    /// Where this server's diagnostic lines go, and the peer protocol their
    /// frame codes are chosen for. `None` until a server sets it: a client
    /// writer frames what it is given unchanged.
    message_routing: Option<(super::MessageRoute, protocol::ProtocolVersion)>,
}

impl<W: Write> MultiplexWriter<W> {
    /// Creates a new multiplex writer whose buffer is sized like upstream's
    /// `iobuf.out` (io.c:1567).
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            out: Rc::new(RefCell::new(MuxOut::new())),
            batch_recorder: None,
            batch_route: BatchRoute::default(),
            allowed_lull: None,
            data_written: 0,
            message_routing: None,
        }
    }

    /// The buffered frame run, for a transport core to drain while it waits.
    pub(crate) fn shared_out(&self) -> SharedMuxOut {
        Rc::clone(&self.out)
    }

    /// Payload bytes written for the wire so far (upstream `total_data_written`).
    pub(crate) const fn data_written(&self) -> u64 {
        self.data_written
    }

    /// Sets where this server's diagnostic lines go and the peer protocol their
    /// frame codes are chosen for. upstream: options.c:103, compat.c:653-654.
    pub(crate) fn set_message_routing(
        &mut self,
        route: super::MessageRoute,
        protocol: protocol::ProtocolVersion,
    ) {
        self.message_routing = Some((route, protocol));
    }

    /// The routing set by [`Self::set_message_routing`], if any.
    pub(crate) const fn message_routing(
        &self,
    ) -> Option<(super::MessageRoute, protocol::ProtocolVersion)> {
        self.message_routing
    }

    /// Configures the keep-alive lull interval.
    ///
    /// upstream: `io.c:set_io_timeout()` derives `allowed_lull = (io_timeout + 1) / 2`
    /// (io.c:1652). Passing `None` (no `--timeout`) disables lull keepalives, so
    /// the default transfer path stays byte-for-byte identical.
    pub(crate) fn set_allowed_lull(&mut self, lull: Option<Duration>) {
        self.allowed_lull = lull;
        self.out.borrow_mut().last_io_out = Instant::now();
    }

    /// Returns the configured keep-alive lull interval, or `None` when
    /// `--timeout` is not set.
    ///
    /// Callers use this to derive upstream's `lull_mod = allowed_lull * 5`
    /// cadence (sender.c:341) when poking keepalives inside a long read loop.
    pub(crate) fn allowed_lull(&self) -> Option<Duration> {
        self.allowed_lull
    }

    /// Emits a lull keepalive if the configured `allowed_lull` has elapsed with
    /// no output since the last write.
    ///
    /// Returns `true` when an empty `MSG_DATA` keepalive was written.
    ///
    /// Mirrors upstream `io.c:maybe_send_keepalive()` (io.c:1639-1667): the
    /// keepalive is emitted only when a full `allowed_lull` has passed since the
    /// last output and the output buffer sits at a frame boundary. When data is
    /// still buffered, flushing it is itself output activity, so upstream flushes
    /// instead of emitting the empty frame (io.c:1660-1665).
    pub(crate) fn maybe_send_keepalive(&mut self) -> io::Result<bool> {
        let Some(lull) = self.allowed_lull else {
            return Ok(false);
        };
        self.send_keepalive_after_lull(lull)
    }

    /// Answers a peer's protocol-29 keep-alive frame (`rsync.c:389-390`).
    ///
    /// Upstream routes the reply through `maybe_send_keepalive()` even when
    /// `--timeout` is not set: `allowed_lull` starts at 0 (io.c:83) and
    /// `set_io_timeout(0)` recomputes it as `(0 + 1) / 2 == 0` (io.c:1299), so
    /// the `now - last_io_out >= allowed_lull` test (io.c:1652) passes
    /// unconditionally and the reply goes out immediately. With `--timeout`
    /// the configured half-interval gates the reply exactly as it gates a
    /// spontaneous keepalive, so this treats an unset lull as zero rather
    /// than as "disabled".
    pub(crate) fn answer_keepalive(&mut self) -> io::Result<bool> {
        let lull = self.allowed_lull.unwrap_or(Duration::ZERO);
        self.send_keepalive_after_lull(lull)
    }

    /// The shared body of [`Self::maybe_send_keepalive`] and
    /// [`Self::answer_keepalive`]: upstream `io.c:maybe_send_keepalive()`
    /// (io.c:1639-1667) with `allowed_lull` supplied by the caller.
    fn send_keepalive_after_lull(&mut self, lull: Duration) -> io::Result<bool> {
        let mut out = self.out.borrow_mut();
        if out.last_io_out.elapsed() < lull {
            return Ok(false);
        }

        // upstream: io.c:1658-1665 - pending output is written rather than
        // emitting a keepalive; the write itself is the I/O that resets the
        // lull. Queued messages go alone (PIO_NEED_MSGROOM), buffered data
        // only when no message is queued. "Empty" data is `out.len ==
        // out_empty_len`, not `len == 0`: the reserved header occupies the
        // first four bytes (io.c:2694-2695).
        if !out.msgs.is_empty() {
            out.write_messages(&mut self.inner)?;
            self.inner.flush()?;
            out.dirty = false;
            out.last_io_out = Instant::now();
            return Ok(false);
        }
        if out.has_pending() {
            out.flush_buffer(&mut self.inner)?;
            self.inner.flush()?;
            out.dirty = false;
            out.last_io_out = Instant::now();
            return Ok(false);
        }

        // upstream: io.c:1658-1659 - only at a frame boundary, emit an empty
        // MSG_DATA that the peer absorbs as a no-op keepalive.
        out.write_frame(&mut self.inner, MessageCode::Data, 0, std::iter::empty())?;
        self.inner.flush()?;
        out.dirty = false;
        out.last_io_out = Instant::now();
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
    /// - `io.c:2482-2485` - `write_buf()` with `f != iobuf.out_fd` bypasses the
    ///   multiplex buffer and writes straight to the fd (here, the batch).
    /// - `io.c:2505-2506` - the tee into `batch_fd` for `--write-batch`.
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

    /// Admits a `MSG_DATA` payload: the one gate both `Write` entry points
    /// pass before any byte is buffered or written. Returns `true` when the
    /// payload was diverted to the batch file and must not reach the wire.
    ///
    /// upstream: io.c:997 - perform_io() checks got_kill_signal after each
    /// output flush too, so a sender that never reads (a pull server streaming
    /// one large file) still stops at the next frame. Control frames
    /// (`send_message`) stay open so the exit path can still report the signal
    /// and send `MSG_ERROR_EXIT`.
    fn admit_data<'b>(
        &mut self,
        chunks: impl Iterator<Item = &'b [u8]>,
        len: usize,
    ) -> io::Result<bool> {
        crate::shared::check_shutdown()?;
        // upstream: io.c:write_buf() - tee pre-mux data to batch_fd, or divert
        // to it entirely when this stream is upstream's `f_xfer`.
        if self.record_to_batch(chunks)? {
            return Ok(true);
        }
        self.data_written += len as u64;
        Ok(false)
    }

    /// Sends a control message with the specified message code.
    ///
    /// Unlike the `Write` trait which always sends `MSG_DATA`, this method
    /// allows sending other message types like `MSG_IO_TIMEOUT`.
    ///
    /// The frame is queued, not written: upstream's `send_msg()` appends to
    /// `iobuf.msg` (io.c:1088), and the queue leaves at the next write-out
    /// ahead of any buffered data (io.c:628-636). No message code is written
    /// at once.
    pub(crate) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        self.out
            .borrow_mut()
            .queue_message(&mut self.inner, code, payload)
    }

    /// Writes raw bytes directly to the inner writer, bypassing multiplex framing.
    ///
    /// Used for protocol exchanges like goodbye handshakes where upstream rsync
    /// writes directly without `MSG_DATA` wrapping.
    pub(crate) fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        let mut out = self.out.borrow_mut();
        out.flush_buffer(&mut self.inner)?;
        self.inner.write_all(data)?;
        self.inner.flush()?;
        out.dirty = false;
        out.last_io_out = Instant::now();
        Ok(())
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.admit_data(std::iter::once(buf), buf.len())? {
            return Ok(buf.len());
        }
        self.out.borrow_mut().write_data(&mut self.inner, buf)?;
        Ok(buf.len())
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let total_len: usize = bufs.iter().map(|b| b.len()).sum();
        if total_len == 0 {
            return Ok(0);
        }
        if self.admit_data(bufs.iter().map(|b| &b[..]), total_len)? {
            return Ok(total_len);
        }

        // Each slice is its own write_buf() call upstream, so each gets the
        // fit check on its own: a length prefix that fits stays in this run
        // even when the payload behind it starts the next one.
        let mut out = self.out.borrow_mut();
        for buf in bufs.iter().filter(|b| !b.is_empty()) {
            out.write_data(&mut self.inner, buf)?;
        }
        Ok(total_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.borrow_mut().flush(&mut self.inner)
    }
}

/// A pending signal stops every `MSG_DATA` write before it reaches the wire,
/// whichever `Write` entry point carries it.
///
/// upstream: io.c:997 checks got_kill_signal on the output side of
/// perform_io(), io.c:600-605 handle_kill_signal() then exits RERR_SIGNAL.
#[cfg(test)]
mod signal_tests {
    use super::*;
    use std::io::IoSlice;

    #[test]
    fn a_pending_signal_stops_plain_and_vectored_data_writes() {
        fast_io::signal::mark_shutdown();
        let mut out = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);

        let plain = w.write(b"data").expect_err("write must stop");
        let vectored = w
            .write_vectored(&[IoSlice::new(b"da"), IoSlice::new(b"ta")])
            .expect_err("write_vectored must stop");
        // Control frames stay open for the exit path's MSG_ERROR_EXIT: the
        // frame is queued (io.c:1088 send_msg()) and the flush that sends it
        // is not gated either.
        let control = w
            .send_message(MessageCode::ErrorExit, &20i32.to_le_bytes())
            .and_then(|()| w.flush());
        fast_io::signal::reset_shutdown_for_testing();

        assert!(crate::shared::is_signal_shutdown(&plain), "{plain}");
        assert!(crate::shared::is_signal_shutdown(&vectored), "{vectored}");
        control.expect("control frames are not gated");
        drop(w);
        assert_eq!(
            &out[4..],
            &20i32.to_le_bytes(),
            "only the control frame reached the wire"
        );
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
    /// `now - last_io_out >= allowed_lull`, io.c:1652).
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
    /// is emitted, matching upstream `send_msg(MSG_DATA, "", 0, 0)` (io.c:1659).
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
    /// lull (upstream io.c:1660-1665).
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

    /// Answering a protocol-29 keep-alive frame without `--timeout` replies
    /// immediately: upstream's `allowed_lull` is 0 in that case (io.c:83,
    /// io.c:1299), so `maybe_send_keepalive()` at rsync.c:389-390 always sends.
    #[test]
    fn answer_keepalive_without_timeout_replies_immediately() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        assert!(w.answer_keepalive().unwrap());

        let frame = recv_msg(&mut Cursor::new(&out)).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert!(frame.payload().is_empty());
    }

    /// With `--timeout` configured, the reply is gated on the same lull as a
    /// spontaneous keepalive (`now - last_io_out >= allowed_lull`, io.c:1652):
    /// nothing goes out before the half-interval elapses.
    #[test]
    fn answer_keepalive_with_unelapsed_lull_stays_silent() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        w.set_allowed_lull(Some(Duration::from_secs(3600)));
        assert!(!w.answer_keepalive().unwrap());
        assert!(out.is_empty());
    }

    /// Like the spontaneous path, an answer with data still buffered flushes
    /// that data instead of emitting an empty frame (io.c:1662-1665).
    #[test]
    fn answer_keepalive_flushes_pending_data_instead_of_empty_frame() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = MultiplexWriter::new(&mut out);
        w.write_all(b"pending").unwrap();
        assert!(!w.answer_keepalive().unwrap());

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
/// back-filled inside the buffer before any syscall (io.c:775-776), a write
/// that fits does no I/O (io.c:2478-2507), and nothing can be scheduled between
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
    /// upstream: io.c:2487-2488 - `write_buf()` reaches `perform_io()` only when
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
            writer.flush().unwrap();

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

    /// WHY: upstream queues messages in `iobuf.msg`, apart from the data in
    /// `iobuf.out`, and writes the whole queue before the buffered data
    /// (io.c:628-636 rule 2). A message neither ends the data run it was sent
    /// in nor waits behind it, so 3.5.1 frames messages ahead of earlier data.
    #[test]
    fn queued_messages_leave_ahead_of_buffered_data() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer.write_all(b"data-one").unwrap();
        writer.send_message(MessageCode::Info, b"info").unwrap();
        writer.write_all(b"data-two").unwrap();
        writer.send_message(MessageCode::Error, b"err").unwrap();
        assert_eq!(
            writer.inner.writes, 0,
            "nothing is written until a write-out"
        );
        writer.flush().unwrap();

        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![
                (MessageCode::Info, b"info".to_vec()),
                (MessageCode::Error, b"err".to_vec()),
                (MessageCode::Data, b"data-onedata-two".to_vec()),
            ]
        );
    }

    /// WHY: a data frame whose write stalled part-way is finished before any
    /// queued message (io.c:628-636 rule 1), or the message would land inside
    /// the frame and the peer would read it as payload.
    #[test]
    fn a_stalled_data_frame_finishes_before_queued_messages() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(6));
        writer.write_all(b"payload").unwrap();
        writer.flush().expect_err("the flush must stall mid-frame");
        writer.send_message(MessageCode::Info, b"info").unwrap();
        writer.inner.budget = usize::MAX;
        writer.flush().unwrap();

        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![
                (MessageCode::Data, b"payload".to_vec()),
                (MessageCode::Info, b"info".to_vec()),
            ]
        );
    }

    /// WHY: a sender whose message queue lacks room writes the queued
    /// messages out and nothing else (io.c:1115-1117 `PIO_NEED_MSGROOM`); the
    /// buffered data keeps accumulating in its own run.
    #[test]
    fn a_full_message_queue_writes_only_the_messages() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer.write_all(b"data").unwrap();
        let line = vec![b'm'; 1000];
        let mut sent = 0;
        while writer.inner.writes == 0 {
            writer.send_message(MessageCode::Info, &line).unwrap();
            sent += 1;
        }
        let frames = parse_frames(&writer.inner.sink);
        assert_eq!(frames.len(), sent - 1, "every message but the newest");
        assert!(frames.iter().all(|(code, _)| *code == MessageCode::Info));
        assert_eq!(
            frames.len() * (MESSAGE_HEADER_LEN + line.len()),
            writer.inner.sink.len()
        );
        assert!(sent - 1 <= MSG_BUFFER_SIZE / (MESSAGE_HEADER_LEN + line.len()));
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
    /// steady-state memory bounded by never growing its buffers (io.c:582-598).
    #[test]
    fn oversized_staging_buffer_is_released() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        writer
            .write_all(&vec![7u8; DEFAULT_BUFFER_SIZE * 4])
            .unwrap();
        assert!(
            writer.out.borrow().scratch.capacity() <= SCRATCH_RETAIN,
            "a one-off large frame must not pin its staging allocation"
        );
    }

    /// WHY: upstream's run holds `iobuf.out`'s 65536 bytes, header included
    /// (io.c:1567), and each write_buf() call is fit-checked on its own
    /// (io.c:2487-2488). A literal's length prefix that still fits stays in
    /// the current frame while its bytes open the next one, as 3.5.1 frames
    /// a whole-file send (token.c:323-324).
    #[test]
    fn each_vectored_slice_is_fit_checked_against_the_upstream_buffer_size() {
        let mut writer = MultiplexWriter::new(StallingWriter::new(usize::MAX));
        let lead = vec![1u8; OUT_BUFFER_SIZE - MESSAGE_HEADER_LEN - 8];
        writer.write_all(&lead).unwrap();
        let prefix = 4i32.to_le_bytes();
        let body = [9u8; 8];
        let n = writer
            .write_vectored(&[IoSlice::new(&prefix), IoSlice::new(&body)])
            .unwrap();
        assert_eq!(n, 12);
        writer.flush().unwrap();

        let mut first = lead;
        first.extend_from_slice(&prefix);
        assert_eq!(
            parse_frames(&writer.inner.sink),
            vec![
                (MessageCode::Data, first),
                (MessageCode::Data, body.to_vec()),
            ]
        );
    }
}
