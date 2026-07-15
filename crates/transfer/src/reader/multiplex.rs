use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use crate::handshake::IoTimeoutReapply;

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

/// Client-side rendering state for received `MSG_DELETED` frames.
///
/// A server generator sends only the raw deleted name (a directory carries a
/// trailing NUL); the client formats and gates the line itself, exactly as
/// upstream `log_delete()` does on the non-`am_server` side. Present only on the
/// client-sender reader (a push, where the remote receiver performs `--delete`);
/// every other reader leaves it `None` so the `MSG_DELETED` arm is inert.
///
/// # Upstream Reference
///
/// - `log.c:870-874` - the client renders `stdout_format` (itemize) when set,
///   else `"deleting %n"`, and skips output when neither `INFO_GTE(DEL, 1)` nor
///   `stdout_format` is active.
/// - `io.c:1616-1621` - a trailing NUL in the payload marks a directory.
#[derive(Clone, Copy)]
pub(crate) struct DeletedRender {
    /// `--itemize-changes` is active: render the `*deleting` row form.
    pub(crate) itemize: bool,
    /// `INFO_GTE(DEL, 1)` (e.g. `-v` or `--info=del`): render the plain
    /// `deleting <path>` line when itemize is off.
    pub(crate) show_plain: bool,
}

impl DeletedRender {
    /// Formats a received `MSG_DELETED` payload into the client-visible line, or
    /// `None` when the current verbosity suppresses it.
    ///
    /// upstream: log.c:870-874 - `stdout_format` (itemize) wins over
    /// `"deleting %n"`; both append a trailing slash for a directory (log.c:%n).
    fn format(&self, payload: &[u8]) -> Option<String> {
        if !self.itemize && !self.show_plain {
            return None;
        }
        // upstream: io.c:1616 - a trailing NUL byte marks a deleted directory.
        let (is_dir, name_bytes) = match payload.split_last() {
            Some((0, rest)) => (true, rest),
            _ => (false, payload),
        };
        let name = String::from_utf8_lossy(name_bytes);
        let display = if is_dir {
            format!("{name}/")
        } else {
            name.into_owned()
        };
        Some(if self.itemize {
            format!("*deleting   {display}\n")
        } else {
            format!("deleting {display}\n")
        })
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
    /// Client's current effective I/O timeout in seconds, or `None`/`0` for
    /// infinite. Set only on the client-receiver path; drives the upstream
    /// adoption test. upstream: io.c:1556 `!io_timeout || io_timeout > val`.
    io_timeout: Option<u32>,
    /// Live-socket re-apply hook for an adopted daemon `MSG_IO_TIMEOUT`. `Some`
    /// only when this reader is the client receiver of a daemon transfer; its
    /// absence also marks the `am_server || am_generator` role for which a
    /// received `MSG_IO_TIMEOUT` is an invalid message. upstream: io.c:1551-1561.
    io_timeout_reapply: Option<IoTimeoutReapply>,
    /// Set once a received `MSG_IO_TIMEOUT` violated the upstream `msg_bytes == 4`
    /// / role invariant (upstream `goto invalid_msg`, fatal `RERR_STREAMIO`).
    io_timeout_invalid: bool,
    /// Client-sender rendering state for `MSG_DELETED` frames. `Some` only on
    /// the client side of a push, where the remote receiver runs `--delete`;
    /// every other reader leaves it `None` so the frame is dropped like upstream
    /// drops it on `am_server`. upstream: log.c:870-874.
    deleted_render: Option<DeletedRender>,
}

/// Exit code for partial transfer due to error.
///
/// upstream: errcode.h `RERR_PARTIAL = 23`. Produced only when
/// `got_xfer_error` is set (cleanup.c:217-218, main.c:1608-1609).
pub(super) const RERR_PARTIAL: i32 = 23;

/// Typed error carrying the exit code a remote peer requested via
/// `MSG_ERROR_EXIT`.
///
/// [`MultiplexReader::check_error_exit`] wraps this in an [`io::Error`] as its
/// inner error so the code survives `?` propagation up the read stack. Callers
/// that terminate a transfer can `downcast_ref` the inner error to recover the
/// peer's exact exit code (e.g. `RERR_SYNTAX = 1` for a read-only module
/// rejection) instead of collapsing every failure to a generic partial-transfer
/// (23) code.
///
/// The [`Display`](std::fmt::Display) output is byte-identical to the previous
/// inline string form (`"remote error exit (code N)"`), so any diagnostic that
/// rendered the wrapping `io::Error` verbatim is unchanged.
///
/// upstream: io.c:1663-1701 - on `MSG_ERROR_EXIT` the client calls the NORETURN
/// `_exit_cleanup(val)`, exiting with the peer-supplied code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteExitError {
    /// Exit code the remote peer requested via `MSG_ERROR_EXIT`.
    pub code: i32,
}

impl std::fmt::Display for RemoteExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remote error exit (code {})", self.code)
    }
}

impl std::error::Error for RemoteExitError {}

/// Default buffer capacity for `MultiplexReader`.
///
/// 64KB matches the `MultiplexWriter` buffer size and upstream rsync's
/// `IO_BUFFER_SIZE`. When receiving from an oc-rsync sender, frames can
/// be up to 64KB - a smaller staging buffer forces extra reads per frame.
const MULTIPLEX_READER_BUFFER_CAPACITY: usize = 64 * 1024;

/// Returns the timeout the client should adopt from a daemon-advertised
/// `MSG_IO_TIMEOUT` value `val`, or `None` to keep the current setting.
///
/// Mirrors upstream `io.c:1556` `if (!io_timeout || io_timeout > val)`: adopt
/// the stricter (smaller, non-zero) timeout. A current value of `None` or `0`
/// means the client set no timeout (infinite), so any daemon value is adopted;
/// otherwise the daemon value is adopted only when it is smaller than the
/// client's own. This is the single reconciliation point for adoption.
///
/// upstream: io.c:1551-1561 `read_a_msg()` case `MSG_IO_TIMEOUT`.
fn reconcile_io_timeout(current: Option<u32>, val: u32) -> Option<u32> {
    match current {
        None | Some(0) => Some(val),
        Some(c) if c > val => Some(val),
        Some(_) => None,
    }
}

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
            io_timeout: None,
            io_timeout_reapply: None,
            io_timeout_invalid: false,
            deleted_render: None,
        }
    }

    /// Enables client-side rendering of received `MSG_DELETED` frames.
    ///
    /// Called only on the client-sender reader (a push, where the remote
    /// receiver performs `--delete`), so a server or client-receiver reader
    /// leaves the state unset and silently drops any `MSG_DELETED` frame,
    /// mirroring upstream's `am_server`/`am_generator` non-rendering path.
    ///
    /// upstream: log.c:870-874 `log_delete()` renders on the non-server side.
    pub(super) fn set_deleted_render(&mut self, render: DeletedRender) {
        self.deleted_render = Some(render);
    }

    /// Installs client-receiver I/O-timeout adoption state.
    ///
    /// `current` is the client's own `--timeout` in seconds (`None`/`0` =
    /// infinite); `reapply` re-applies an adopted daemon timeout to the live
    /// socket. Only the client receiver of a daemon transfer calls this; every
    /// other reader leaves it unset and treats a received `MSG_IO_TIMEOUT` as an
    /// invalid message, mirroring upstream's `am_server || am_generator` guard.
    ///
    /// upstream: io.c:1551-1561 `read_a_msg()` case `MSG_IO_TIMEOUT`.
    pub(super) fn set_io_timeout_adoption(
        &mut self,
        current: Option<u32>,
        reapply: IoTimeoutReapply,
    ) {
        self.io_timeout = current;
        self.io_timeout_reapply = Some(reapply);
    }

    /// Returns the accumulated `MSG_IO_ERROR` flags and resets the accumulator.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1526-1528`: `io_error |= val; if (am_receiver) send_msg_int(MSG_IO_ERROR, val);`
    pub(super) fn take_io_error(&mut self) -> i32 {
        std::mem::take(&mut self.io_error)
    }

    /// Returns the count of `MSG_ERROR_XFER` frames received so far.
    ///
    /// A non-zero count is upstream's `got_xfer_error`: the peer reported a
    /// per-file transfer error (e.g. a failed output `mkstemp()`), so the run
    /// must terminate with `RERR_PARTIAL` (exit 23) rather than success.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:311`: receipt of `FERROR_XFER` sets `got_xfer_error = 1`
    /// - `main.c:1635`: `if (got_xfer_error) _exit(RERR_PARTIAL);`
    pub(super) fn xfer_error_count(&self) -> u32 {
        self.xfer_error_count
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
                RemoteExitError { code },
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

    /// Handles a received `MSG_IO_TIMEOUT` (a daemon's advertised `--timeout`).
    ///
    /// Mirrors upstream `io.c:1551-1561` `read_a_msg()`:
    ///
    /// ```text
    /// case MSG_IO_TIMEOUT:
    ///     if (msg_bytes != 4 || am_server || am_generator) goto invalid_msg;
    ///     val = raw_read_int();
    ///     if (!io_timeout || io_timeout > val) {
    ///         if (INFO_GTE(MISC, 2)) rprintf(FINFO, "Setting --timeout=%d to match server\n", val);
    ///         set_io_timeout(val);
    ///     }
    /// ```
    ///
    /// Only the client receiver installs a re-apply hook (see
    /// [`MultiplexReader::set_io_timeout_adoption`]); its absence marks the
    /// `am_server || am_generator` role for which the frame is invalid, as is a
    /// payload that is not exactly 4 bytes. Adoption re-applies the value to the
    /// live socket read/write timeouts, the oc analogue of upstream's
    /// `set_io_timeout()` updating `select_timeout`/`allowed_lull`.
    fn handle_io_timeout_msg<S: MuxSink>(&mut self, sink: &mut S) {
        let Some(reapply) = self.io_timeout_reapply.clone() else {
            // Only the client receiver adopts (the hook is installed there).
            // Upstream aborts with `invalid_msg` for `am_server || am_generator`,
            // but that is a defensive check that never fires with a well-behaved
            // daemon: a daemon echoes MSG_IO_TIMEOUT to whichever client it
            // serves, including a client sender that oc runs locally as the
            // generator. Aborting there would break an otherwise valid transfer
            // (the client's own --timeout already covers the socket), so a reader
            // without the adoption hook silently ignores the frame - matching the
            // pre-adoption behaviour. upstream: io.c:1551-1561.
            return;
        };
        if self.buffer.len() != 4 {
            // upstream: msg_bytes != 4 -> goto invalid_msg. Reachable only for
            // the client receiver we adopt for; a real daemon always sends a
            // 4-byte int, so a bad length is a genuine protocol violation.
            self.io_timeout_invalid = true;
            return;
        }
        let val = u32::from_le_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]);
        if let Some(adopted) = reconcile_io_timeout(self.io_timeout, val) {
            // upstream: INFO_GTE(MISC, 2) gate on the "Setting --timeout" notice.
            if logging::info_gte(logging::InfoFlag::Misc, 2) {
                sink.info(&format!("Setting --timeout={val} to match server\n"));
            }
            self.io_timeout = Some(adopted);
            // upstream: set_io_timeout(val). oc re-applies to the live socket
            // read/write timeouts instead of a global select_timeout/allowed_lull.
            let _ = reapply.apply(adopted);
        }
    }

    /// Returns an error if a received `MSG_IO_TIMEOUT` was invalid.
    ///
    /// upstream: io.c `invalid_msg` label - a bad `msg_bytes`/role for
    /// `MSG_IO_TIMEOUT` is fatal (`exit_cleanup(RERR_STREAMIO)`); we surface it
    /// as an `InvalidData` error so the read loop aborts the transfer.
    fn check_io_timeout(&self) -> io::Result<()> {
        if self.io_timeout_invalid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid MSG_IO_TIMEOUT message from peer",
            ));
        }
        Ok(())
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
            protocol::MessageCode::IoTimeout => {
                // upstream: io.c:1551-1561
                self.handle_io_timeout_msg(sink);
            }
            protocol::MessageCode::Deleted => {
                // upstream: io.c:1614-1621 + log.c:870-874 - the client formats
                // the raw deleted name (a trailing NUL marks a directory) and
                // gates on its own info=del / itemize verbosity. A server or
                // client-receiver reader has no render state and drops the frame,
                // matching upstream's non-rendering `am_server` path.
                if let Some(render) = self.deleted_render {
                    if let Some(line) = render.format(&self.buffer) {
                        sink.info(&line);
                    }
                }
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
                self.check_io_timeout()?;
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
            self.check_io_timeout()?;
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
            self.check_io_timeout()?;
        }
    }
}

#[cfg(test)]
mod remote_exit_error_tests {
    use super::*;

    /// The typed error must render byte-identically to the previous inline
    /// string form so any diagnostic that displayed the wrapping `io::Error`
    /// verbatim is unchanged.
    #[test]
    fn display_matches_legacy_string_form() {
        for code in [0, 1, 23, 255, -1] {
            let err = RemoteExitError { code };
            assert_eq!(err.to_string(), format!("remote error exit (code {code})"));
        }
    }

    /// `check_error_exit` must wrap the code as the inner error of a
    /// `ConnectionAborted` `io::Error` so callers can `downcast_ref` it and
    /// honour the peer's exit code instead of forcing 23.
    #[test]
    fn check_error_exit_wraps_typed_inner_error() {
        let mut reader = MultiplexReader::new(io::empty());
        reader.error_exit_code = Some(1);
        let err = reader.check_error_exit().expect_err("code 1 must abort");
        assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted);
        let inner = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<RemoteExitError>())
            .expect("inner RemoteExitError must survive");
        assert_eq!(inner.code, 1);
    }

    /// RERR_PARTIAL (23) suppression must remain: the msg2sndr race means a
    /// bare exit-23 is non-fatal and must not abort the read loop.
    #[test]
    fn check_error_exit_suppresses_rerr_partial() {
        let mut reader = MultiplexReader::new(io::empty());
        reader.error_exit_code = Some(RERR_PARTIAL);
        assert!(reader.check_error_exit().is_ok());
    }
}

/// Tests for the client-side `MSG_DELETED` render, encoding the upstream
/// `log.c:870-874` + `io.c:1614-1621` contract: the client formats the raw
/// deleted name (a trailing NUL marks a directory), itemize (`stdout_format`)
/// wins over `"deleting %n"`, and a reader without render state drops the frame
/// exactly as upstream drops it on `am_server`.
#[cfg(test)]
mod deleted_render_tests {
    use super::*;

    /// Minimal capturing sink so the render assertion is byte-exact.
    struct CapturingSink(Vec<String>);

    impl MuxSink for CapturingSink {
        fn info(&mut self, msg: &str) {
            self.0.push(msg.to_owned());
        }
        fn error(&mut self, _msg: &str) {
            panic!("MSG_DELETED must render via info(), never error()");
        }
    }

    fn render(payload: &[u8], render: DeletedRender) -> Vec<String> {
        let mut reader = MultiplexReader::new(io::empty());
        reader.set_deleted_render(render);
        reader.buffer = payload.to_vec();
        let mut sink = CapturingSink(Vec::new());
        // A MSG_DELETED frame never breaks the read loop.
        assert!(!reader.dispatch_message_with(protocol::MessageCode::Deleted, &mut sink));
        sink.0
    }

    #[test]
    fn plain_render_file_and_dir() {
        // upstream: log.c:874 "deleting %n"; %n appends a trailing slash for a
        // directory (the trailing-NUL payload marks it).
        let cfg = DeletedRender {
            itemize: false,
            show_plain: true,
        };
        assert_eq!(render(b"foo", cfg), vec!["deleting foo\n".to_owned()]);
        assert_eq!(render(b"bar\0", cfg), vec!["deleting bar/\n".to_owned()]);
    }

    #[test]
    fn itemize_render_wins_over_plain() {
        // upstream: log.c:873 - stdout_format (itemize) is used instead of the
        // plain form when active, even at -v.
        let cfg = DeletedRender {
            itemize: true,
            show_plain: true,
        };
        assert_eq!(render(b"foo", cfg), vec!["*deleting   foo\n".to_owned()]);
        assert_eq!(render(b"bar\0", cfg), vec!["*deleting   bar/\n".to_owned()]);
    }

    #[test]
    fn suppressed_when_neither_gate_active() {
        // upstream: log.c:870 - `!INFO_GTE(DEL,1) && !stdout_format` prints
        // nothing even though the frame arrived.
        let cfg = DeletedRender {
            itemize: false,
            show_plain: false,
        };
        assert!(render(b"foo", cfg).is_empty());
    }

    #[test]
    fn dropped_without_render_state() {
        // A server or client-receiver reader never enables rendering, so a
        // MSG_DELETED frame is silently dropped (upstream `am_server` path).
        let mut reader = MultiplexReader::new(io::empty());
        reader.buffer = b"foo".to_vec();
        let mut sink = CapturingSink(Vec::new());
        assert!(!reader.dispatch_message_with(protocol::MessageCode::Deleted, &mut sink));
        assert!(sink.0.is_empty());
    }
}

/// Tests for the client-receiver adoption of a daemon-advertised
/// `MSG_IO_TIMEOUT`. Encodes the upstream `io.c:1551-1561` contract: adopt the
/// stricter timeout, ignore a non-stricter one, treat a bad length or the
/// wrong role as an invalid (fatal) message, and re-apply the adopted value to
/// the live socket.
#[cfg(test)]
mod io_timeout_adoption_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Builds a `MultiplexReader` with adoption enabled, plus an observer that
    /// records the seconds passed to the live-socket re-apply hook (`u32::MAX`
    /// sentinel = never invoked).
    fn reader_with_adoption(current: Option<u32>) -> (MultiplexReader<io::Empty>, Arc<AtomicU32>) {
        let observed = Arc::new(AtomicU32::new(u32::MAX));
        let sink = observed.clone();
        let reapply = IoTimeoutReapply(Arc::new(move |secs: u32| {
            sink.store(secs, Ordering::SeqCst);
            Ok(())
        }));
        let mut reader = MultiplexReader::new(io::empty());
        reader.set_io_timeout_adoption(current, reapply);
        (reader, observed)
    }

    /// The reconciliation helper is the single source of the upstream test
    /// `!io_timeout || io_timeout > val`.
    #[test]
    fn reconcile_matches_upstream_condition() {
        // No client timeout (infinite) adopts any daemon value.
        assert_eq!(reconcile_io_timeout(None, 30), Some(30));
        // A zero client timeout is also infinite and adopts.
        assert_eq!(reconcile_io_timeout(Some(0), 30), Some(30));
        // A larger client timeout adopts the stricter daemon value.
        assert_eq!(reconcile_io_timeout(Some(60), 30), Some(30));
        // An equal or smaller client timeout is already at least as strict.
        assert_eq!(reconcile_io_timeout(Some(30), 30), None);
        assert_eq!(reconcile_io_timeout(Some(10), 30), None);
    }

    /// A client with no timeout adopts the daemon value and re-applies it to
    /// the live socket - this is the whole point of the message: a client that
    /// set no `--timeout` still detects a stalled daemon.
    #[test]
    fn adopts_when_client_has_no_timeout() {
        let (mut reader, observed) = reader_with_adoption(None);
        reader.buffer = 45u32.to_le_bytes().to_vec();
        reader.handle_io_timeout_msg(&mut RealSink);
        assert_eq!(reader.io_timeout, Some(45), "effective timeout adopted");
        assert_eq!(observed.load(Ordering::SeqCst), 45, "re-applied to socket");
        assert!(reader.check_io_timeout().is_ok());
    }

    /// A client whose timeout is larger adopts the daemon's stricter value.
    #[test]
    fn adopts_stricter_daemon_timeout() {
        let (mut reader, observed) = reader_with_adoption(Some(120));
        reader.buffer = 30u32.to_le_bytes().to_vec();
        reader.handle_io_timeout_msg(&mut RealSink);
        assert_eq!(reader.io_timeout, Some(30));
        assert_eq!(observed.load(Ordering::SeqCst), 30);
    }

    /// A client whose timeout is already at least as strict keeps its own value
    /// and never touches the socket - adopting a larger daemon timeout would
    /// weaken the client's stall detection.
    #[test]
    fn keeps_client_timeout_when_not_stricter() {
        let (mut reader, observed) = reader_with_adoption(Some(10));
        reader.buffer = 30u32.to_le_bytes().to_vec();
        reader.handle_io_timeout_msg(&mut RealSink);
        assert_eq!(reader.io_timeout, Some(10), "unchanged");
        assert_eq!(
            observed.load(Ordering::SeqCst),
            u32::MAX,
            "re-apply hook must not fire"
        );
        assert!(reader.check_io_timeout().is_ok());
    }

    /// A payload that is not exactly 4 bytes is an invalid message
    /// (upstream `msg_bytes != 4 -> goto invalid_msg`, fatal).
    #[test]
    fn wrong_length_is_invalid_message() {
        let (mut reader, observed) = reader_with_adoption(None);
        reader.buffer = vec![1, 2, 3]; // 3 bytes
        reader.handle_io_timeout_msg(&mut RealSink);
        assert!(reader.io_timeout.is_none(), "no adoption on invalid frame");
        assert_eq!(observed.load(Ordering::SeqCst), u32::MAX);
        let err = reader.check_io_timeout().expect_err("invalid frame aborts");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// A reader with no adoption installed (a non-client-receiver: a client
    /// sender that oc runs as the generator, an SSH client, or the server side)
    /// silently ignores `MSG_IO_TIMEOUT` and never aborts. A real daemon echoes
    /// the frame to whichever client it serves, so aborting here would break an
    /// otherwise valid transfer; leniency matches the pre-adoption behaviour.
    #[test]
    fn non_adopting_reader_ignores_message() {
        let mut reader = MultiplexReader::new(io::empty());
        reader.buffer = 30u32.to_le_bytes().to_vec();
        reader.handle_io_timeout_msg(&mut RealSink);
        assert!(reader.io_timeout.is_none(), "no adoption without a hook");
        assert!(
            reader.check_io_timeout().is_ok(),
            "a non-adopting reader must not abort on MSG_IO_TIMEOUT"
        );
    }
}
