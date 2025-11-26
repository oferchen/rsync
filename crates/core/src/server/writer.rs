//! Server-side writer that can switch between plain and multiplexed modes.

use std::io::{self, Write};

use protocol::MessageCode;

/// Writer that can switch from plain to multiplex mode after protocol setup.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_out()`.
/// We achieve the same by wrapping the writer and delegating based on mode.
pub enum ServerWriter<W: Write> {
    /// Plain mode - write data directly without framing
    Plain(W),
    /// Multiplex mode - wrap data in MSG_DATA frames
    Multiplex(MultiplexWriter<W>),
}

impl<W: Write> ServerWriter<W> {
    /// Creates a new plain-mode writer
    pub fn new_plain(writer: W) -> Self {
        Self::Plain(writer)
    }

    /// Activates multiplex mode (mirrors upstream io_start_multiplex_out)
    pub fn activate_multiplex(self) -> io::Result<Self> {
        match self {
            Self::Plain(writer) => Ok(Self::Multiplex(MultiplexWriter::new(writer))),
            Self::Multiplex(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "multiplex already active",
            )),
        }
    }

    /// Returns true if multiplex is active
    pub fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_))
    }
}

impl<W: Write> Write for ServerWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Multiplex(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Multiplex(w) => w.flush(),
        }
    }
}

/// Writer that wraps data in multiplex MSG_DATA frames
struct MultiplexWriter<W> {
    inner: W,
}

impl<W: Write> MultiplexWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Send as MSG_DATA (code 0)
        let code = MessageCode::Data;
        eprintln!("[multiplex] Writing {} bytes as MSG_DATA (code {})", buf.len(), code.as_u8());
        eprintln!("[multiplex] First 16 bytes: {:02x?}", &buf[..buf.len().min(16)]);
        protocol::send_msg(&mut self.inner, code, buf)?;
        eprintln!("[multiplex] Message sent successfully");
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
