//! Multiplex stream wrappers for daemon mode.
//!
//! After protocol setup, the client expects all I/O to go through the multiplex
//! layer. These wrappers automatically frame writes with MSG_DATA tags and
//! extract payloads from incoming multiplexed messages.

use std::io::{self, Read, Write};

/// A Read wrapper that automatically demultiplexes incoming messages.
pub struct MultiplexReader<R> {
    inner: R,
    buffer: Vec<u8>,
    pos: usize,
}

impl<R: Read> MultiplexReader<R> {
    #[allow(dead_code)]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            pos: 0,
        }
    }

    #[allow(dead_code)]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for MultiplexReader<R> {
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

        // Read next multiplexed message
        self.buffer.clear();
        self.pos = 0;

        let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

        // For now, only handle MSG_DATA (7). Other messages should be handled by higher layers.
        // If it's not MSG_DATA, we'll just return the payload anyway for compatibility.
        let _ = code; // Ignore message type for now

        // Copy from buffer to output
        let to_copy = self.buffer.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
        self.pos = to_copy;

        Ok(to_copy)
    }
}

/// A Write wrapper that automatically multiplexes outgoing data.
pub struct MultiplexWriter<W> {
    inner: W,
}

impl<W: Write> MultiplexWriter<W> {
    #[allow(dead_code)]
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    #[allow(dead_code)]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
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

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
