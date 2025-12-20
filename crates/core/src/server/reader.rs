#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

use std::io::{self, Read};

use compress::algorithm::CompressionAlgorithm;

use super::compressed_reader::CompressedReader;

/// Server reader abstraction that switches between plain and multiplex modes.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_in()`.
/// We achieve the same by wrapping the reader and delegating based on mode.
#[allow(dead_code)]
#[allow(private_interfaces)]
#[allow(clippy::large_enum_variant)]
pub enum ServerReader<R: Read> {
    /// Plain mode - read data directly without demultiplexing
    Plain(R),
    /// Multiplex mode - extract data from MSG_DATA frames
    Multiplex(MultiplexReader<R>),
    /// Compressed+Multiplex mode - decompress then demultiplex
    #[allow(dead_code)] // Used in production code once compression is integrated
    Compressed(CompressedReader<MultiplexReader<R>>),
}

#[allow(dead_code)]
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
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
        }
    }

    /// Activates decompression on top of multiplex mode.
    ///
    /// This must be called AFTER activate_multiplex() to match upstream behavior.
    /// Upstream rsync activates decompression in io.c:io_start_buffering_in()
    /// which wraps the already-multiplexed stream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The reader is not in multiplex mode (decompression requires multiplex first)
    /// - Compression is already active
    /// - The compression algorithm is not supported in this build
    #[allow(dead_code)] // Used in production code once compression is integrated
    pub fn activate_compression(self, algorithm: CompressionAlgorithm) -> io::Result<Self> {
        match self {
            Self::Multiplex(mux) => {
                let compressed = CompressedReader::new(mux, algorithm)?;
                Ok(Self::Compressed(compressed))
            }
            Self::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "compression requires multiplex mode first",
            )),
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
        }
    }

    /// Returns true if multiplex mode is active
    #[allow(dead_code)]
    pub fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_) | Self::Compressed(_))
    }
}

impl<R: Read> Read for ServerReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(r) => r.read(buf),
            Self::Multiplex(r) => r.read(buf),
            Self::Compressed(r) => r.read(buf),
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
    read_seq: usize, // Debug: track read sequence
    msg_seq: usize,  // Debug: track message sequence
}

#[allow(dead_code)]
impl<R: Read> MultiplexReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            pos: 0,
            read_seq: 0,
            msg_seq: 0,
        }
    }
}

impl<R: Read> Read for MultiplexReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_seq += 1;

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
            self.msg_seq += 1;
            let msg_seq = self.msg_seq;

            let code = match protocol::recv_msg_into(&mut self.inner, &mut self.buffer) {
                Ok(c) => c,
                Err(e) => {
                    let _ = std::fs::write(
                        format!("/tmp/mux_MSG_{msg_seq:04}_ERR"),
                        format!("{:?}: {}", e.kind(), e),
                    );
                    return Err(e);
                }
            };

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
                        eprint!("{msg}");
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
                        eprint!("{msg}");
                    }
                    // Continue loop to read next message
                }
                _ => {
                    // Other message types (Redo, Stats, etc.): log for debugging
                    let _ = std::fs::write(
                        format!("/tmp/mux_MSG_{msg_seq:04}_UNHANDLED"),
                        format!("code={:?} len={}", code, self.buffer.len()),
                    );
                    // Continue loop to read next message
                }
            }
        }
    }
}
