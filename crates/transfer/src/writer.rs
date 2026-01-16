//! Server-side writer that can switch between plain and multiplexed modes.

use std::io::{self, Write};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use protocol::MessageCode;

use super::compressed_writer::CompressedWriter;

/// Writer that can switch from plain to multiplex mode after protocol setup.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_out()`.
/// We achieve the same by wrapping the writer and delegating based on mode.
#[allow(clippy::large_enum_variant)]
#[allow(private_interfaces)]
pub enum ServerWriter<W: Write> {
    /// Plain mode - write data directly without framing
    Plain(W),
    /// Multiplex mode - wrap data in MSG_DATA frames
    Multiplex(MultiplexWriter<W>),
    /// Compressed+Multiplex mode - compress then multiplex
    Compressed(CompressedWriter<MultiplexWriter<W>>),
    /// Temporary state during in-place transformations.
    /// Any operation on a Taken writer panics.
    #[doc(hidden)]
    Taken,
}

impl<W: Write> ServerWriter<W> {
    /// Creates a new plain-mode writer
    #[inline]
    pub const fn new_plain(writer: W) -> Self {
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
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }

    /// Activates compression on top of multiplex mode.
    ///
    /// This must be called AFTER activate_multiplex() to match upstream behavior.
    /// Upstream rsync activates compression in io.c:io_start_buffering_out()
    /// which wraps the already-multiplexed stream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The writer is not in multiplex mode (compression requires multiplex first)
    /// - Compression is already active
    /// - The compression algorithm is not supported in this build
    pub fn activate_compression(
        self,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        match self {
            Self::Multiplex(mux) => {
                let compressed = CompressedWriter::new(mux, algorithm, level)?;
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
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }

    /// Returns true if multiplex is active
    #[inline]
    #[allow(dead_code)]
    pub const fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_) | Self::Compressed(_))
    }

    /// Activates multiplex mode in place (mirrors upstream io_start_multiplex_out).
    ///
    /// This is used when the generator needs to activate multiplex AFTER sending
    /// the file list but before sending file data. Upstream rsync client sender
    /// calls io_start_multiplex_out() after send_file_list().
    ///
    /// # Panics
    ///
    /// Panics if the writer is in the Taken state (internal error).
    pub fn activate_multiplex_in_place(&mut self) -> io::Result<()> {
        // Use a take-and-replace pattern with the Taken variant as placeholder
        let old_self = std::mem::replace(self, Self::Taken);

        match old_self {
            Self::Plain(writer) => {
                *self = Self::Multiplex(MultiplexWriter::new(writer));
                Ok(())
            }
            Self::Taken => {
                panic!("ServerWriter in invalid Taken state");
            }
            other => {
                // Restore original state
                *self = other;
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "multiplex already active",
                ))
            }
        }
    }

    /// Sends a control message (non-DATA message) through the multiplexed stream.
    ///
    /// This is used for sending protocol messages like MSG_IO_TIMEOUT that need
    /// to be sent as separate message types, not wrapped in MSG_DATA frames.
    ///
    /// Control messages bypass the compression layer (if active) to match upstream
    /// rsync behavior where they go directly through the multiplex layer.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The writer is not in multiplex mode
    /// - The underlying I/O operation fails
    pub fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        match self {
            Self::Multiplex(mux) => mux.send_message(code, payload),
            Self::Compressed(compressed) => {
                // Control messages bypass compression layer - send directly to multiplex
                compressed.inner_mut().send_message(code, payload)
            }
            Self::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot send control messages in plain mode",
            )),
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }

    /// Writes raw bytes directly to the underlying stream, bypassing multiplexing.
    ///
    /// This is used for protocol exchanges like the final goodbye handshake where
    /// upstream rsync's write_ndx() writes directly without MSG_DATA framing.
    ///
    /// IMPORTANT: Flushes any buffered multiplexed data before writing raw bytes
    /// to maintain proper message ordering.
    ///
    /// # Arguments
    /// * `data` - Raw bytes to write directly to the stream
    ///
    /// # Errors
    /// Returns an error if the underlying I/O operation fails.
    pub fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        // First flush any buffered data
        self.flush()?;

        match self {
            Self::Plain(w) => {
                w.write_all(data)?;
                w.flush()
            }
            Self::Multiplex(mux) => {
                // Write directly to inner writer, bypassing multiplex framing
                mux.write_raw(data)
            }
            Self::Compressed(compressed) => {
                // Write directly to the multiplex layer's inner writer
                compressed.inner_mut().write_raw(data)
            }
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }
}

impl<W: Write> Write for ServerWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Multiplex(w) => w.write(buf),
            Self::Compressed(w) => w.write(buf),
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Multiplex(w) => w.flush(),
            Self::Compressed(w) => w.flush(),
            Self::Taken => panic!("ServerWriter in invalid Taken state"),
        }
    }
}

/// Writer that wraps data in multiplex MSG_DATA frames
///
/// Buffers writes to avoid sending tiny multiplex frames for every write call.
/// Mirrors upstream rsync's buffering behavior in io.c.
pub(super) struct MultiplexWriter<W> {
    inner: W,
    buffer: Vec<u8>,
    /// Buffer size matching upstream rsync's IO_BUFFER_SIZE (default 4096)
    buffer_size: usize,
}

impl<W: Write> MultiplexWriter<W> {
    fn new(inner: W) -> Self {
        const DEFAULT_BUFFER_SIZE: usize = 4096;
        Self {
            inner,
            buffer: Vec::with_capacity(DEFAULT_BUFFER_SIZE),
            buffer_size: DEFAULT_BUFFER_SIZE,
        }
    }

    /// Flushes the internal buffer by sending it as a MSG_DATA frame
    fn flush_buffer(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let code = MessageCode::Data;
            protocol::send_msg(&mut self.inner, code, &self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }

    /// Sends a control message with the specified message code.
    ///
    /// Unlike the Write trait which always sends MSG_DATA, this method
    /// allows sending other message types like MSG_IO_TIMEOUT.
    /// Flushes buffered data first to maintain message ordering.
    pub(super) fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        // Flush any buffered DATA first
        self.flush_buffer()?;
        // Send the control message
        protocol::send_msg(&mut self.inner, code, payload)?;
        self.inner.flush()
    }

    /// Writes raw bytes directly to the inner writer, bypassing multiplex framing.
    ///
    /// This is used for protocol exchanges like goodbye handshakes where upstream
    /// rsync writes directly without MSG_DATA wrapping.
    pub(super) fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        // Flush any buffered data first
        self.flush_buffer()?;
        // Write directly without framing
        self.inner.write_all(data)?;
        self.inner.flush()
    }
}

impl<W: Write> Write for MultiplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // If buffer would overflow, flush first
        if self.buffer.len() + buf.len() > self.buffer_size {
            self.flush_buffer()?;
        }

        // If buf is larger than buffer size, send directly
        if buf.len() > self.buffer_size {
            let code = MessageCode::Data;
            protocol::send_msg(&mut self.inner, code, buf)?;
            return Ok(buf.len());
        }

        // Buffer the data
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_writer_new_plain() {
        let buf = Vec::new();
        let writer = ServerWriter::new_plain(buf);
        assert!(matches!(writer, ServerWriter::Plain(_)));
    }

    #[test]
    fn server_writer_activate_multiplex() {
        let buf = Vec::new();
        let writer = ServerWriter::new_plain(buf);
        let result = writer.activate_multiplex();
        assert!(result.is_ok());
        let multiplexed = result.unwrap();
        assert!(matches!(multiplexed, ServerWriter::Multiplex(_)));
    }

    #[test]
    fn server_writer_activate_multiplex_twice_fails() {
        let buf = Vec::new();
        let writer = ServerWriter::new_plain(buf);
        let multiplexed = writer.activate_multiplex().unwrap();
        let result = multiplexed.activate_multiplex();
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn server_writer_is_multiplexed() {
        let buf = Vec::new();
        let plain_writer = ServerWriter::new_plain(buf);
        assert!(!plain_writer.is_multiplexed());

        let buf2 = Vec::new();
        let mux_writer = ServerWriter::new_plain(buf2).activate_multiplex().unwrap();
        assert!(mux_writer.is_multiplexed());
    }

    #[test]
    fn server_writer_activate_multiplex_in_place() {
        let buf = Vec::new();
        let mut writer = ServerWriter::new_plain(buf);
        assert!(!writer.is_multiplexed());

        let result = writer.activate_multiplex_in_place();
        assert!(result.is_ok());
        assert!(writer.is_multiplexed());
    }

    #[test]
    fn server_writer_activate_multiplex_in_place_twice_fails() {
        let buf = Vec::new();
        let mut writer = ServerWriter::new_plain(buf);
        writer.activate_multiplex_in_place().unwrap();

        let result = writer.activate_multiplex_in_place();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn server_writer_plain_write() {
        let mut buf = Vec::new();
        {
            let mut writer = ServerWriter::new_plain(&mut buf);
            writer.write_all(b"hello").unwrap();
            writer.flush().unwrap();
        }
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn server_writer_send_message_plain_mode_fails() {
        let mut buf = Vec::new();
        let mut writer = ServerWriter::new_plain(&mut buf);
        let result = writer.send_message(MessageCode::Data, b"test");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn server_writer_write_raw_plain() {
        let mut buf = Vec::new();
        {
            let mut writer = ServerWriter::new_plain(&mut buf);
            writer.write_raw(b"raw data").unwrap();
        }
        assert_eq!(buf, b"raw data");
    }

    #[test]
    fn server_writer_write_raw_multiplexed() {
        let mut buf = Vec::new();
        {
            let mut writer = ServerWriter::new_plain(&mut buf)
                .activate_multiplex()
                .unwrap();
            writer.write_raw(b"raw").unwrap();
        }
        // Raw data should be written directly, not wrapped in MSG_DATA
        assert_eq!(buf, b"raw");
    }

    #[test]
    fn multiplex_writer_empty_write() {
        let mut buf = Vec::new();
        let mut mux = MultiplexWriter::new(&mut buf);
        let n = mux.write(&[]).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn multiplex_writer_flush_empty_buffer() {
        let mut buf = Vec::new();
        {
            let mut mux = MultiplexWriter::new(&mut buf);
            mux.flush().unwrap();
        }
        // Empty flush should not write anything
        assert!(buf.is_empty());
    }

    #[test]
    fn activate_compression_on_plain_mode_fails() {
        use std::num::NonZeroU8;
        let buf = Vec::new();
        let writer = ServerWriter::new_plain(buf);
        let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
        let result = writer.activate_compression(CompressionAlgorithm::Zlib, level);
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn activate_compression_on_multiplex_succeeds() {
        use std::num::NonZeroU8;
        let buf = Vec::new();
        let writer = ServerWriter::new_plain(buf).activate_multiplex().unwrap();
        let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
        let result = writer.activate_compression(CompressionAlgorithm::Zlib, level);
        assert!(result.is_ok());
        let compressed = result.unwrap();
        assert!(compressed.is_multiplexed());
    }

    #[test]
    fn activate_compression_twice_fails() {
        use std::num::NonZeroU8;
        let buf = Vec::new();
        let level = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
        let writer = ServerWriter::new_plain(buf)
            .activate_multiplex()
            .unwrap()
            .activate_compression(CompressionAlgorithm::Zlib, level)
            .unwrap();
        let level2 = CompressionLevel::precise(NonZeroU8::new(6).unwrap());
        let result = writer.activate_compression(CompressionAlgorithm::Zlib, level2);
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("expected error"),
        }
    }
}
