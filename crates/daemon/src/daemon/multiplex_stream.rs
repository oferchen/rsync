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
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying reader to wrap with multiplex demultiplexing
    ///
    /// # Returns
    ///
    /// A new `MultiplexReader` with an empty buffer, ready to receive multiplexed messages.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            pos: 0,
        }
    }

    /// Consumes this wrapper and returns the underlying reader.
    ///
    /// # Returns
    ///
    /// The wrapped reader, allowing access to the raw stream after multiplex operations
    /// are complete.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for MultiplexReader<R> {
    /// Reads demultiplexed data from the underlying stream.
    ///
    /// This implementation automatically handles the rsync multiplex protocol by:
    /// 1. Serving any buffered data from previous messages first
    /// 2. Reading new multiplexed messages from the underlying stream
    /// 3. Filtering out non-data messages (logging them to stderr)
    /// 4. Returning only MSG_DATA payloads to the caller
    ///
    /// # Arguments
    ///
    /// * `buf` - The buffer to fill with demultiplexed data
    ///
    /// # Returns
    ///
    /// The number of bytes read into `buf`, or an I/O error if the underlying read fails.
    ///
    /// # Protocol Details
    ///
    /// The function loops reading messages until it receives a MSG_DATA frame. Messages
    /// with codes INFO, WARNING, ERROR, etc. are printed to stderr and discarded. This
    /// ensures the caller only sees actual protocol data while still surfacing diagnostic
    /// messages from the remote daemon.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If we have buffered data, copy it out first
        if self.pos < self.buffer.len() {
            let available = self.buffer.len() - self.pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
            self.pos += to_copy;

            // If buffer is exhausted, reset for next message
            if self.pos >= self.buffer.len() {
                self.buffer.clear();
                self.pos = 0;
            }

            return Ok(to_copy);
        }

        // Loop until we get a MSG_DATA message
        // Other message types (INFO, ERROR, etc.) are logged and we continue reading
        loop {
            self.buffer.clear();
            self.pos = 0;

            let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

            // Dispatch based on message type
            match code {
                protocol::MessageCode::Data => {
                    // MSG_DATA: return payload for protocol processing
                    let to_copy = self.buffer.len().min(buf.len());
                    buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
                    self.pos = to_copy;
                    return Ok(to_copy);
                }
                protocol::MessageCode::Info
                | protocol::MessageCode::Warning
                | protocol::MessageCode::Log
                | protocol::MessageCode::Client => {
                    // Info/warning messages: print to stderr and continue
                    if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                        eprint!("{}", msg);
                    }
                    // Continue loop to read next message
                }
                protocol::MessageCode::Error
                | protocol::MessageCode::ErrorXfer
                | protocol::MessageCode::ErrorSocket
                | protocol::MessageCode::ErrorUtf8
                | protocol::MessageCode::ErrorExit => {
                    // Error messages: print to stderr and continue
                    if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                        eprint!("{}", msg);
                    }
                    // Continue loop to read next message
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
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying writer to wrap with multiplex framing
    ///
    /// # Returns
    ///
    /// A new `MultiplexWriter` ready to send MSG_DATA frames.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Consumes this wrapper and returns the underlying writer.
    ///
    /// # Returns
    ///
    /// The wrapped writer, allowing access to the raw stream after multiplex operations
    /// are complete.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    /// Writes data by wrapping it in a MSG_DATA multiplex frame.
    ///
    /// The entire buffer is sent as a single MSG_DATA message. The protocol layer
    /// handles validation of the payload length against the 24-bit limit (16MB max).
    ///
    /// # Arguments
    ///
    /// * `buf` - The data to write as a MSG_DATA payload
    ///
    /// # Returns
    ///
    /// The number of bytes written (always `buf.len()` on success), or an I/O error
    /// if the underlying write fails or the payload exceeds the maximum size.
    ///
    /// # Protocol Details
    ///
    /// Each write() call produces exactly one MSG_DATA frame with a 4-byte header
    /// followed by the payload. The message code is always 0 (MSG_DATA). Payload
    /// lengths are validated by `protocol::send_msg` to ensure compliance with the
    /// upstream rsync implementation's 24-bit length field.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Send as MSG_DATA (code 0)
        let code = protocol::MessageCode::try_from(0u8).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid message code: {e}"),
            )
        })?;

        protocol::send_msg(&mut self.inner, code, buf)?;
        Ok(buf.len())
    }

    /// Flushes the underlying writer.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the flush succeeds, or an I/O error from the underlying stream.
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
