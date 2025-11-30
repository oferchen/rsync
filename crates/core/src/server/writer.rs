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
    #[allow(dead_code)]
    pub fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_))
    }
}

impl<W: Write> Write for ServerWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => {
                eprintln!("[ServerWriter::Plain] Writing {} bytes", buf.len());
                eprintln!("[ServerWriter::Plain] Bytes: {buf:02x?}");
                // Also log to file
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/rsync-debug/server-writes.log")
                {
                    use std::io::Write as _;
                    let _ = writeln!(f, "[PLAIN] {} bytes: {:02x?}", buf.len(), buf);
                }
                w.write(buf)
            }
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
pub(super) struct MultiplexWriter<W> {
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
        eprintln!(
            "[multiplex] Writing {} bytes as MSG_DATA (code {})",
            buf.len(),
            code.as_u8()
        );
        eprintln!(
            "[multiplex] Payload (first 64 bytes): {:02x?}",
            &buf[..buf.len().min(64)]
        );

        // Log to file what we're about to send (including the wire format)
        // Wire format: 4-byte header [tag, len_byte1, len_byte2, len_byte3] + payload
        let tag = code.as_u8() + 7; // MPLEX_BASE = 7
        let len_bytes = [
            (buf.len() & 0xFF) as u8,
            ((buf.len() >> 8) & 0xFF) as u8,
            ((buf.len() >> 16) & 0xFF) as u8,
        ];
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/rsync-debug/server-writes.log")
        {
            use std::io::Write as _;
            let _ = writeln!(
                f,
                "[MULTIPLEX] tag={} ({:#04x}), len={} ({:#08x})",
                tag,
                tag,
                buf.len(),
                buf.len()
            );
            let _ = writeln!(
                f,
                "[MULTIPLEX] Wire header: [{:#04x}, {:#04x}, {:#04x}, {:#04x}]",
                tag, len_bytes[0], len_bytes[1], len_bytes[2]
            );
            let _ = writeln!(f, "[MULTIPLEX] Payload: {buf:02x?}");
        }

        protocol::send_msg(&mut self.inner, code, buf)?;
        eprintln!("[multiplex] Message sent successfully");
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
