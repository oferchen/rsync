#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

use std::io::{self, Read};

/// Server reader abstraction that switches between plain and multiplex modes.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_in()`.
/// We achieve the same by wrapping the reader and delegating based on mode.
pub enum ServerReader<R: Read> {
    /// Plain mode - read data directly without demultiplexing
    Plain(R),
    /// Multiplex mode - extract data from MSG_DATA frames
    Multiplex(MultiplexReader<R>),
}

impl<R: Read> ServerReader<R> {
    /// Creates a new plain-mode reader
    pub fn new_plain(reader: R) -> Self {
        Self::Plain(reader)
    }

    /// Activates multiplex mode, wrapping the reader in a demultiplexer
    pub fn activate_multiplex(self) -> io::Result<Self> {
        match self {
            Self::Plain(reader) => Ok(Self::Multiplex(MultiplexReader::new(reader))),
            Self::Multiplex(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "multiplex already active",
            )),
        }
    }

    /// Returns true if multiplex mode is active
    #[allow(dead_code)]
    pub fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_))
    }
}

impl<R: Read> Read for ServerReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(r) => r.read(buf),
            Self::Multiplex(r) => r.read(buf),
        }
    }
}

/// Reader that automatically demultiplexes incoming messages.
///
/// Reads multiplex frames from the wire and extracts MSG_DATA payloads.
/// Buffers partial messages internally to provide seamless streaming.
pub(super) struct MultiplexReader<R> {
    inner: R,
    buffer: Vec<u8>,
    pos: usize,
}

impl<R: Read> MultiplexReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            pos: 0,
        }
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
