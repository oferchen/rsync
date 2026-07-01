use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

#[cfg(all(test, feature = "tokio-transfer"))]
#[path = "multiplex_parity_tests.rs"]
mod parity_tests;

/// Sink for the inline, wire-visible side effects that frame dispatch performs.
///
/// Upstream rsync's `log.c:rwrite()` writes `MSG_INFO`/`MSG_CLIENT` to stdout
/// (with a flush) and every `MSG_*ERROR*`/`MSG_WARNING` category to stderr, in
/// line with the demultiplexed data stream. The ordering of these writes
/// *relative to* delivered `MSG_DATA` payloads is observable on the wire/output,
/// so it must be identical no matter which driver (blocking or `.await`) pulls
/// the frame off the socket.
///
/// The dispatch core ([`MultiplexReader::dispatch_message_with`]) reads no wire
/// and routes every side effect through this trait, so the sync and async
/// drivers share one dispatch path and can never diverge on effect ordering.
/// Production uses [`RealSink`], which reproduces the previous inline
/// `print!`/`eprint!`/`stdout().flush()` byte-for-byte. Tests can substitute a
/// capturing sink to assert the ordered effect sequence.
pub(super) trait MuxSink {
    /// Emits an `MSG_INFO`/`MSG_CLIENT` payload to stdout and flushes.
    ///
    /// upstream: `log.c:rwrite()` - FINFO and FCLIENT go to stdout.
    fn info(&mut self, msg: &str);

    /// Emits an `MSG_WARNING`/`MSG_LOG`/`MSG_*ERROR*` payload to stderr.
    ///
    /// upstream: `log.c:rwrite()` - FWARNING/FLOG/FERROR* go to stderr.
    fn error(&mut self, msg: &str);
}

/// Production [`MuxSink`] that writes to the real stdout/stderr.
///
/// Byte-for-byte identical to the previous inline dispatch side effects:
/// `print!("{msg}")` + `io::stdout().flush()` for info, `eprint!("{msg}")` for
/// error. This is the only sink used outside tests, so the default build's
/// demux output is unchanged.
pub(super) struct RealSink;

impl MuxSink for RealSink {
    fn info(&mut self, msg: &str) {
        print!("{msg}");
        let _ = io::stdout().flush();
    }

    fn error(&mut self, msg: &str) {
        eprint!("{msg}");
    }
}

/// Reader that automatically demultiplexes incoming messages.
///
/// Reads multiplex frames from the wire and extracts MSG_DATA payloads.
/// Buffers partial messages internally to provide seamless streaming.
///
/// When `MSG_IO_ERROR` frames are received, the 4-byte little-endian payload
/// is OR'd into an internal accumulator. Callers retrieve and forward the
/// accumulated value via [`MultiplexReader::take_io_error`].
///
/// When `MSG_NO_SEND` frames are received, the 4-byte little-endian file index
/// is accumulated into an internal queue. Callers drain the queue via
/// [`MultiplexReader::take_no_send_indices`].
///
/// When a `batch_recorder` is attached, all demuxed `MSG_DATA` payloads are
/// copied to the recorder. This mirrors upstream rsync's `write_batch_monitor_in`
/// in `io.c:read_buf()` which tees data after demultiplexing.
///
/// # Upstream Reference
///
/// - `io.c:1521-1528`: receiver reads `MSG_IO_ERROR`, OR's value into
///   `io_error`, and forwards it to the generator when `am_receiver`.
/// - `io.c:1618-1627`: `MSG_NO_SEND` received on the sender/receiver pipe;
///   if `am_generator`, calls `got_flist_entry_status(FES_NO_SEND, val)`,
///   otherwise forwards to the generator.
pub(crate) struct MultiplexReader<R> {
    inner: R,
    pub(super) buffer: Vec<u8>,
    pub(super) pos: usize,
    /// Accumulated I/O error flags from `MSG_IO_ERROR` messages.
    /// upstream: io.c:1526 `io_error |= val;`
    pub(super) io_error: i32,
    /// File indices received via `MSG_NO_SEND` from the sender.
    /// upstream: io.c:1618-1627, sender.c:367-368
    pub(super) no_send_indices: Vec<i32>,
    /// File indices received via `MSG_REDO` from the receiver.
    /// upstream: io.c:1514-1519, receiver.c:970-974
    pub(super) redo_indices: Vec<i32>,
    /// Exit code from MSG_ERROR_EXIT. When set, the remote has requested
    /// immediate termination. upstream: io.c:1663-1701 calls _exit_cleanup().
    pub(super) error_exit_code: Option<i32>,
    /// Count of MSG_ERROR_XFER messages received from the remote.
    ///
    /// upstream: log.c:311 - receipt of FERROR_XFER sets `got_xfer_error = 1`.
    /// When the remote daemon's generator rejects files matching server-side
    /// module exclude rules, it emits FERROR_XFER for each rejected file and
    /// then exits with RERR_PARTIAL (23). This count is informational - the
    /// exit code 23 suppression in `check_error_exit` is unconditional.
    pub(super) xfer_error_count: u32,
    /// Optional recorder for batch mode - captures post-demux data.
    /// upstream: `io.c` `write_batch_monitor_in` + `safe_write(batch_fd, buf, len)`
    pub(crate) batch_recorder: Option<Arc<Mutex<dyn Write + Send>>>,
}

/// Exit code for partial transfer due to error.
///
/// upstream: errcode.h `RERR_PARTIAL = 23`. Produced only when
/// `got_xfer_error` is set (cleanup.c:217-218, main.c:1608-1609).
pub(super) const RERR_PARTIAL: i32 = 23;

/// Default buffer capacity for `MultiplexReader`.
///
/// 64KB matches the `MultiplexWriter` buffer size and upstream rsync's
/// `IO_BUFFER_SIZE`. When receiving from an oc-rsync sender, frames can
/// be up to 64KB - a smaller staging buffer forces extra reads per frame.
const MULTIPLEX_READER_BUFFER_CAPACITY: usize = 64 * 1024;

impl<R> MultiplexReader<R> {
    pub(super) fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(MULTIPLEX_READER_BUFFER_CAPACITY),
            pos: 0,
            batch_recorder: None,
            io_error: 0,
            no_send_indices: Vec::new(),
            redo_indices: Vec::new(),
            error_exit_code: None,
            xfer_error_count: 0,
        }
    }

    /// Returns the accumulated `MSG_IO_ERROR` flags and resets the accumulator.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1526-1528`: `io_error |= val; if (am_receiver) send_msg_int(MSG_IO_ERROR, val);`
    pub(super) fn take_io_error(&mut self) -> i32 {
        std::mem::take(&mut self.io_error)
    }

    /// Returns and drains the accumulated `MSG_NO_SEND` file indices.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1618-1627`: `MSG_NO_SEND` handling.
    /// - `sender.c:367-368`: sender sends `MSG_NO_SEND` when `protocol_version >= 30`.
    pub(super) fn take_no_send_indices(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.no_send_indices)
    }

    /// Returns and drains the accumulated `MSG_REDO` file indices.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1514-1519`: `MSG_REDO` received, calls `got_flist_entry_status(FES_REDO, val)`.
    /// - `receiver.c:970-974`: receiver sends `MSG_REDO` when `!redoing`.
    pub(super) fn take_redo_indices(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.redo_indices)
    }

    /// Returns an error if MSG_ERROR_EXIT has been received.
    ///
    /// Called after dispatching non-DATA messages to abort the read loop,
    /// matching upstream's NORETURN `_exit_cleanup(val)` behavior.
    ///
    /// When the remote daemon exits with RERR_PARTIAL (23) solely because its
    /// generator rejected files matching server-side module exclude rules
    /// (tracked by `xfer_error_count` from MSG_ERROR_XFER messages), this is
    /// non-fatal. The daemon successfully transferred all allowed files and
    /// only exited 23 because `got_xfer_error` was set. Suppressing the abort
    /// lets the transfer loop drain normally and exit 0, matching the behavior
    /// of oc-rsync's own receiver which silently skips daemon-excluded files.
    ///
    /// # Wire ordering race
    ///
    /// The upstream daemon's sender process may emit MSG_ERROR_EXIT(23) before
    /// the FERROR_XFER messages that caused it arrive on the wire. This happens
    /// because the sender reads the generator's exit code from `waitpid()`
    /// before fully draining the msg2sndr IPC pipe. When this occurs,
    /// `error_exit_code` is set but `xfer_error_count` is still zero.
    ///
    /// To handle this, exit code 23 never aborts immediately - the read loop
    /// continues draining control messages until either FERROR_XFER arrives
    /// (confirming the daemon filter scenario) or the connection closes
    /// naturally via EOF.
    ///
    /// upstream: generator.c:1269 - `rprintf(FERROR_XFER, "daemon refused...")`
    /// upstream: log.c:311 - `got_xfer_error = 1;` on FERROR_XFER receipt
    /// upstream: main.c:1608-1609 - `if (got_xfer_error) _exit(RERR_PARTIAL);`
    fn check_error_exit(&self) -> io::Result<()> {
        if let Some(code) = self.error_exit_code {
            // RERR_PARTIAL is only produced when got_xfer_error is set
            // (upstream cleanup.c:217-218, main.c:1608-1609), so receiving
            // exit code 23 guarantees that FERROR_XFER messages exist in the
            // wire even if we haven't read them yet. Suppression is always
            // safe - FERROR_XFER may still be in flight due to the msg2sndr
            // IPC pipe drain race between the generator and sender processes.
            if code == RERR_PARTIAL {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                format!("remote error exit (code {code})"),
            ))
        } else {
            Ok(())
        }
    }

    /// Handles a `MSG_IO_ERROR` payload by accumulating the error flags.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32`).
    /// upstream: io.c:1522-1526
    fn handle_io_error_msg(&mut self) {
        if self.buffer.len() == 4 {
            let val = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.io_error |= val;
        }
    }

    /// Handles a `MSG_REDO` payload by recording the file index.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32` file index).
    /// upstream: io.c:1514-1519
    fn handle_redo_msg(&mut self) {
        if self.buffer.len() == 4 {
            let ndx = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.redo_indices.push(ndx);
        }
    }

    /// Handles a `MSG_NO_SEND` payload by recording the file index.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32` file index).
    /// upstream: io.c:1618-1627
    fn handle_no_send_msg(&mut self) {
        if self.buffer.len() == 4 {
            let ndx = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.no_send_indices.push(ndx);
        }
    }

    /// Dispatches a non-data message code using the production [`RealSink`].
    ///
    /// Byte-identical to the previous inline dispatch: routes `MSG_INFO`/
    /// `MSG_CLIENT` to stdout+flush and every error/warning category to stderr.
    /// This is the entry point for the blocking `Read` driver.
    ///
    /// Returns `true` for `MSG_DATA` (caller should break the read loop),
    /// `false` for all other message types (caller should continue reading).
    fn dispatch_message(&mut self, code: protocol::MessageCode) -> bool {
        self.dispatch_message_with(code, &mut RealSink)
    }

    /// Dispatches a non-data message code to the appropriate handler, routing
    /// every wire-visible side effect through `sink`.
    ///
    /// This is the shared, reader-free dispatch core. It mutates only in-memory
    /// state (`buffer`, the `io_error`/`no_send`/`redo`/`xfer_error`/exit
    /// accumulators) and emits side effects exclusively via `sink`, in exactly
    /// the same order relative to the caller's data delivery. The blocking and
    /// `.await` drivers both call it at the identical point (immediately after a
    /// frame read), so they can never diverge on effect ordering.
    ///
    /// Returns `true` for `MSG_DATA` (caller should break the read loop),
    /// `false` for all other message types (caller should continue reading).
    fn dispatch_message_with<S: MuxSink>(
        &mut self,
        code: protocol::MessageCode,
        sink: &mut S,
    ) -> bool {
        match code {
            protocol::MessageCode::Data => return true,
            protocol::MessageCode::Info | protocol::MessageCode::Client => {
                // upstream: log.c:rwrite() - FINFO and FCLIENT go to stdout
                if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                    sink.info(msg);
                }
            }
            protocol::MessageCode::Warning | protocol::MessageCode::Log => {
                // upstream: log.c:rwrite() - FWARNING to stderr, FLOG to daemon log
                if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                    sink.error(msg);
                }
            }
            protocol::MessageCode::Error
            | protocol::MessageCode::ErrorSocket
            | protocol::MessageCode::ErrorUtf8 => {
                // upstream: log.c:rwrite() - FERROR* to stderr
                if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                    sink.error(msg);
                }
            }
            protocol::MessageCode::ErrorXfer => {
                // upstream: log.c:311 - receipt of FERROR_XFER sets
                // got_xfer_error = 1. Track the count so check_error_exit
                // can distinguish daemon filter refusals from real errors.
                self.xfer_error_count += 1;
                if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                    sink.error(msg);
                }
            }
            protocol::MessageCode::ErrorExit => {
                // upstream: io.c:1663-1701 - MSG_ERROR_EXIT carries a 4-byte
                // exit code. Upon receipt, upstream calls _exit_cleanup(val)
                // which is NORETURN. We propagate it as an io::Error so the
                // transfer loop can abort cleanly.
                let exit_code = if self.buffer.len() == 4 {
                    i32::from_le_bytes([
                        self.buffer[0],
                        self.buffer[1],
                        self.buffer[2],
                        self.buffer[3],
                    ])
                } else {
                    0
                };
                self.error_exit_code = Some(exit_code);
            }
            protocol::MessageCode::IoError => {
                // upstream: io.c:1521-1526
                self.handle_io_error_msg();
            }
            protocol::MessageCode::NoSend => {
                // upstream: io.c:1618-1627
                self.handle_no_send_msg();
            }
            protocol::MessageCode::Redo => {
                // upstream: io.c:1514-1519
                self.handle_redo_msg();
            }
            _ => {}
        }
        false
    }
}

impl<R: Read> MultiplexReader<R> {
    /// Attempts to borrow exactly `len` bytes from the internal frame buffer.
    ///
    /// Returns `Some(&[u8])` if the current frame buffer has at least `len` bytes
    /// available, avoiding a copy into an intermediate buffer. If the buffer is
    /// empty, reads the next MSG_DATA frame first. Returns `None` when the
    /// requested data spans frame boundaries - the caller should fall back to
    /// `Read::read_exact()` with a separate buffer.
    ///
    /// # Zero-copy optimization
    ///
    /// This eliminates one buffer copy for literal delta tokens that fit within
    /// a single MSG_DATA frame (the common case for tokens up to 32-64KB).
    ///
    /// # Batch recording
    ///
    /// When a batch recorder is attached, the borrowed bytes are teed to the
    /// recorder before being returned. This mirrors the recording behavior in
    /// the `Read` impl below and matches upstream rsync's `io.c:read_buf()`
    /// which tees post-demux data to `batch_fd` unconditionally. Without this
    /// tee, literal delta tokens taken via the zero-copy path would never
    /// reach the batch file, leaving `--write-batch` outputs from daemon and
    /// SSH transfers missing their delta payloads.
    pub(super) fn try_borrow_exact(&mut self, len: usize) -> io::Result<Option<&[u8]>> {
        if self.pos >= self.buffer.len() {
            loop {
                self.buffer.clear();
                self.pos = 0;

                let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

                if self.dispatch_message(code) {
                    break;
                }
                self.check_error_exit()?;
            }
        }

        let available = self.buffer.len() - self.pos;
        if available >= len {
            let start = self.pos;
            self.pos += len;
            // upstream: io.c:read_buf() - tee post-demux data to batch_fd
            if let Some(ref recorder) = self.batch_recorder {
                let mut rec = recorder
                    .lock()
                    .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
                rec.write_all(&self.buffer[start..start + len])?;
            }
            Ok(Some(&self.buffer[start..start + len]))
        } else {
            Ok(None)
        }
    }
}

impl<R> MultiplexReader<R> {
    /// Tees `bytes` to the batch recorder when one is attached.
    ///
    /// upstream: io.c:read_buf() - post-demux data is teed to batch_fd
    /// unconditionally. Shared by every read path so the blocking and `.await`
    /// drivers record identical batch output.
    fn tee_batch(&self, bytes: &[u8]) -> io::Result<()> {
        if let Some(ref recorder) = self.batch_recorder {
            let mut rec = recorder
                .lock()
                .map_err(|_| io::Error::other("batch recorder lock poisoned"))?;
            rec.write_all(bytes)?;
        }
        Ok(())
    }

    /// Drains bytes already buffered from a prior frame into `buf`.
    ///
    /// Reader-free: copies from the internal frame buffer starting at `pos`,
    /// advances `pos`, resets the buffer when drained, and tees the copied
    /// bytes to the batch recorder. Returns the number of bytes copied. Both
    /// the blocking and `.await` drivers call this first, so buffered-data
    /// delivery is byte-identical.
    fn drain_buffered(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.buffer.len() - self.pos;
        let to_copy = available.min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
        self.pos += to_copy;

        if self.pos >= self.buffer.len() {
            self.buffer.clear();
            self.pos = 0;
        }

        self.tee_batch(&buf[..to_copy])?;
        Ok(to_copy)
    }

    /// Places a freshly demuxed `MSG_DATA` frame into `buf`.
    ///
    /// Reader-free: copies the head of the frame buffer into `buf`, records the
    /// consumed length in `pos` (leaving any overflow for the next `read`), and
    /// tees the copied bytes to the batch recorder. Returns the number of bytes
    /// copied. Shared by both drivers so newly-read data delivery is identical.
    fn place_frame(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let to_copy = self.buffer.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        self.pos = to_copy;
        self.tee_batch(&buf[..to_copy])?;
        Ok(to_copy)
    }
}

impl<R: Read> Read for MultiplexReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.buffer.len() {
            return self.drain_buffered(buf);
        }

        // Loop until we get a MSG_DATA message
        loop {
            self.buffer.clear();
            self.pos = 0;

            let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

            if self.dispatch_message(code) {
                // upstream: io.c io_start_multiplex_out() sends a length-0
                // MSG_DATA frame as a multiplex activation marker. Returning
                // Ok(0) from Read::read() signals EOF in Rust, so we must
                // skip empty data frames and continue to the next message.
                if self.buffer.is_empty() {
                    continue;
                }
                return self.place_frame(buf);
            }
            self.check_error_exit()?;
        }
    }
}

/// Async `.await`-driven demux, gated on the `tokio-transfer` feature.
///
/// This is the `.await` counterpart to the blocking [`Read`] impl above. It
/// exists so a genuine receiver-side `.await` can pull multiplex frames off a
/// [`tokio::io::AsyncRead`] socket without a blocking read, per the ASY-7
/// receiver-prototype scoping (`docs/design/asy-7-receiver-tokio-prototype.md`).
///
/// The only difference from the sync driver is the frame read: it awaits
/// [`protocol::recv_msg_into_async`] instead of blocking on
/// [`protocol::recv_msg_into`]. Every non-read step - the demux loop shape, the
/// empty-frame skip, the buffer-drain/place copies, the batch tee, the
/// `check_error_exit` abort, and (crucially) the inline print/flush side effects
/// via [`MultiplexReader::dispatch_message_with`] - is the exact same shared,
/// reader-free code the sync driver runs. Because the dispatch core fires each
/// side effect at the identical point relative to data delivery, the async
/// output is byte-identical to the sync output, effect ordering included.
///
/// Additive and unwired: this driver is not connected to the receiver hot path
/// yet (that routing is the next rung). It is exercised only by the sync-vs-
/// async parity tests.
#[cfg(feature = "tokio-transfer")]
impl<R: tokio::io::AsyncRead + Unpin> MultiplexReader<R> {
    /// Reads demultiplexed data into `buf`, awaiting the underlying socket.
    ///
    /// Byte-for-byte equivalent to [`Read::read`] with the production
    /// [`RealSink`] side effects. See the impl-level docs for the parity
    /// guarantee.
    ///
    /// Unwired pending the receiver-routing rung; only tests drive it, so it is
    /// dead code in non-test builds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn read_async(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_async_with(buf, &mut RealSink).await
    }

    /// Reads demultiplexed data into `buf`, routing side effects through `sink`.
    ///
    /// Identical demux logic to [`Read::read`]; only the frame read is awaited
    /// and the side-effect sink is injectable so parity tests can capture the
    /// ordered effect sequence. Production callers use [`read_async`], which
    /// supplies [`RealSink`].
    ///
    /// [`read_async`]: MultiplexReader::read_async
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn read_async_with<S: MuxSink>(
        &mut self,
        buf: &mut [u8],
        sink: &mut S,
    ) -> io::Result<usize> {
        if self.pos < self.buffer.len() {
            return self.drain_buffered(buf);
        }

        // Loop until we get a MSG_DATA message
        loop {
            self.buffer.clear();
            self.pos = 0;

            let code = protocol::recv_msg_into_async(&mut self.inner, &mut self.buffer).await?;

            if self.dispatch_message_with(code, sink) {
                // upstream: io.c io_start_multiplex_out() sends a length-0
                // MSG_DATA frame as a multiplex activation marker. Returning
                // Ok(0) signals EOF, so we must skip empty data frames and
                // continue to the next message - matching the sync driver.
                if self.buffer.is_empty() {
                    continue;
                }
                return self.place_frame(buf);
            }
            self.check_error_exit()?;
        }
    }
}
