//! Multiplex stream wrappers for daemon mode.
//!
//! After protocol setup, the client expects all I/O to go through the multiplex
//! layer. These wrappers automatically frame writes with MSG_DATA tags and
//! extract payloads from incoming multiplexed messages.

use std::io::{self, Read, Write};

/// A Read wrapper that automatically demultiplexes incoming messages.
///
/// After daemon protocol setup, all I/O must go through the multiplex layer to handle
/// rsync's message framing protocol. This wrapper transparently extracts MSG_DATA payloads
/// from incoming multiplexed messages, filtering out informational and error messages by
/// printing them to stderr.
///
/// The reader maintains an internal buffer to handle partial reads when the caller's
/// buffer is smaller than the received message payload.
///
/// # Protocol Behavior
///
/// - **MSG_DATA**: Payload is returned to the caller for protocol processing
/// - **MSG_INFO/MSG_WARNING/MSG_LOG/MSG_CLIENT**: Printed to stderr and skipped
/// - **MSG_ERROR variants**: Printed to stderr and skipped
/// - **Other messages**: Skipped (handled at higher protocol levels)
pub struct MultiplexReader<R> {
    /// The underlying reader from which multiplexed messages are received
    inner: R,
    /// Buffer for holding message payloads across multiple read() calls
    buffer: Vec<u8>,
    /// Current position in the buffer for partial reads
    pos: usize,
}

impl<R: Read> MultiplexReader<R> {
    /// Creates a new multiplexing reader wrapping the given input stream.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            pos: 0,
        }
    }

    /// Consumes this wrapper and returns the underlying reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for MultiplexReader<R> {
    /// Reads demultiplexed data from the underlying stream.
    ///
    /// Loops reading multiplexed messages until a MSG_DATA frame arrives.
    /// Non-data messages (INFO, WARNING, ERROR) are printed to stdout/stderr
    /// and discarded so the caller only sees protocol data.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.buffer.len() {
            let available = self.buffer.len() - self.pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
            self.pos += to_copy;

            if self.pos >= self.buffer.len() {
                self.buffer.clear();
                self.pos = 0;
            }

            return Ok(to_copy);
        }

        loop {
            self.buffer.clear();
            self.pos = 0;

            let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

            match code {
                protocol::MessageCode::Data => {
                    let to_copy = self.buffer.len().min(buf.len());
                    buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
                    self.pos = to_copy;
                    return Ok(to_copy);
                }
                protocol::MessageCode::Info | protocol::MessageCode::Client => {
                    // upstream: log.c:rwrite() — FINFO and FCLIENT go to stdout
                    if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                        print!("{}", msg);
                        let _ = io::stdout().flush();
                    }
                }
                protocol::MessageCode::Warning | protocol::MessageCode::Log => {
                    // upstream: log.c:rwrite() — FWARNING to stderr, FLOG to daemon log
                    if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                        eprint!("{}", msg);
                    }
                }
                protocol::MessageCode::Error
                | protocol::MessageCode::ErrorXfer
                | protocol::MessageCode::ErrorSocket
                | protocol::MessageCode::ErrorUtf8 => {
                    // upstream: log.c:rwrite() — FERROR* to stderr
                    if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                        eprint!("{}", msg);
                    }
                }
                protocol::MessageCode::ErrorExit => {
                    // upstream: io.c:1663-1701 — MSG_ERROR_EXIT carries a
                    // 4-byte exit code and triggers _exit_cleanup(val).
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
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        format!("remote error exit (code {exit_code})"),
                    ));
                }
                _ => {
                    // Other message types (Redo, Stats, etc.): continue reading
                    // These are handled at higher protocol levels
                }
            }
        }
    }
}

/// A Write wrapper that automatically multiplexes outgoing data.
///
/// After daemon protocol setup, all outgoing data must be wrapped in MSG_DATA frames
/// to comply with rsync's multiplex protocol. This wrapper automatically frames all
/// writes with the appropriate MSG_DATA header.
///
/// # Protocol Behavior
///
/// All data written through this wrapper is sent as MSG_DATA (code 0) messages. The
/// underlying `protocol::send_msg` function handles the 4-byte header construction:
/// - 3 bytes for payload length (enforcing the 16MB limit)
/// - 1 byte for message code (always 0 for MSG_DATA)
pub struct MultiplexWriter<W> {
    /// The underlying writer to which multiplexed messages are sent
    inner: W,
}

impl<W: Write> MultiplexWriter<W> {
    /// Creates a new multiplexing writer wrapping the given output stream.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Consumes this wrapper and returns the underlying writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    /// Writes data by wrapping it in a single MSG_DATA multiplex frame.
    ///
    /// Payload length is validated by `protocol::send_msg` against the 24-bit limit.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let code = protocol::MessageCode::try_from(0u8).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid message code: {e}"),
            )
        })?;

        protocol::send_msg(&mut self.inner, code, buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
