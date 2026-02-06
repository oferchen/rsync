//! Multiplexed writer that implements `std::io::Write` with transparent framing.
//!
//! This module provides [`MplexWriter`], a wrapper around any `Write` implementor that
//! automatically wraps data in rsync multiplex MSG_DATA frames. It buffers writes to
//! avoid sending tiny frames and provides methods for sending other message types.

use std::io::{self, Write};

use super::io::send_msg;
use crate::envelope::{MessageCode, MAX_PAYLOAD_LENGTH};

/// A writer that transparently multiplexes data into rsync protocol frames.
///
/// `MplexWriter` wraps any `Write` implementor and automatically frames all data
/// in MSG_DATA messages according to the rsync multiplex protocol. Features:
///
/// 1. **Buffering**: Buffers writes to avoid tiny frames (configurable buffer size)
/// 2. **Automatic framing**: Splits large writes into properly-sized frames
/// 3. **Control messages**: Send non-DATA messages via [`MplexWriter::write_message`]
/// 4. **Raw writes**: Bypass framing for protocol handshakes via [`MplexWriter::write_raw`]
///
/// # Protocol Details
///
/// The rsync multiplex protocol wraps all data in 4-byte headers:
/// - Byte 0: Tag (message code + MPLEX_BASE=7)
/// - Bytes 1-3: Payload length (24-bit little-endian)
///
/// Maximum frame size is 8192 bytes for data, matching upstream rsync's behavior.
///
/// # Examples
///
/// Basic usage with transparent framing:
///
/// ```
/// use std::io::Write;
/// use protocol::MplexWriter;
///
/// # fn example() -> std::io::Result<()> {
/// let mut output = Vec::new();
/// let mut writer = MplexWriter::new(&mut output);
///
/// // Writes are automatically framed as MSG_DATA
/// writer.write_all(b"hello world")?;
/// writer.flush()?;
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
///
/// Sending control messages:
///
/// ```
/// use std::io::Write;
/// use protocol::{MplexWriter, MessageCode};
///
/// # fn example() -> std::io::Result<()> {
/// let mut output = Vec::new();
/// let mut writer = MplexWriter::new(&mut output);
///
/// // Send an info message
/// writer.write_message(MessageCode::Info, b"Processing file...")?;
///
/// // Regular writes still work
/// writer.write_all(b"file data")?;
/// writer.flush()?;
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
pub struct MplexWriter<W> {
    inner: W,
    /// Buffer for accumulating writes before framing
    buffer: Vec<u8>,
    /// Maximum buffer size before flushing (default: 32KB)
    buffer_size: usize,
    /// Maximum frame size for data messages (default: 8192)
    max_frame_size: usize,
}

impl<W> MplexWriter<W> {
    /// Default buffer size matching upstream rsync's IO_BUFFER_SIZE (32KB).
    pub const DEFAULT_BUFFER_SIZE: usize = 32 * 1024;

    /// Default maximum frame size for data messages (8192 bytes).
    ///
    /// This matches upstream rsync's maximum message size for data frames.
    /// Larger writes are split into multiple frames.
    pub const DEFAULT_MAX_FRAME_SIZE: usize = 8192;

    /// Creates a new multiplexed writer with default buffer and frame sizes.
    ///
    /// - Buffer size: 32KB (matches upstream rsync)
    /// - Max frame size: 8192 bytes (matches upstream rsync)
    #[inline]
    pub fn new(inner: W) -> Self {
        Self::with_capacity(inner, Self::DEFAULT_BUFFER_SIZE)
    }

    /// Creates a new multiplexed writer with a specific buffer size.
    ///
    /// The max frame size is set to the default (8192 bytes).
    pub fn with_capacity(inner: W, buffer_size: usize) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(buffer_size),
            buffer_size,
            max_frame_size: Self::DEFAULT_MAX_FRAME_SIZE,
        }
    }

    /// Creates a new multiplexed writer with custom buffer and frame sizes.
    ///
    /// # Panics
    ///
    /// Panics if `max_frame_size` exceeds [`MAX_PAYLOAD_LENGTH`] (16,777,215 bytes).
    pub fn with_sizes(inner: W, buffer_size: usize, max_frame_size: usize) -> Self {
        assert!(
            max_frame_size <= MAX_PAYLOAD_LENGTH as usize,
            "max_frame_size ({}) exceeds MAX_PAYLOAD_LENGTH ({})",
            max_frame_size,
            MAX_PAYLOAD_LENGTH
        );

        Self {
            inner,
            buffer: Vec::with_capacity(buffer_size),
            buffer_size,
            max_frame_size,
        }
    }

    /// Returns a reference to the underlying writer.
    #[inline]
    pub const fn get_ref(&self) -> &W {
        &self.inner
    }

    /// Returns a mutable reference to the underlying writer.
    ///
    /// # Warning
    ///
    /// Writing directly to the underlying writer will corrupt the multiplex
    /// stream. Only use this when you need to call methods that don't write data.
    #[inline]
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consumes this writer and returns the underlying writer.
    ///
    /// Any buffered data is **not** flushed automatically. Call [`MplexWriter::flush`]
    /// before this method to ensure all data is written.
    #[inline]
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Returns the number of bytes currently buffered.
    #[inline]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Returns the configured buffer size.
    #[inline]
    pub const fn buffer_size(&self) -> usize {
        self.buffer_size
    }

    /// Returns the configured maximum frame size.
    #[inline]
    pub const fn max_frame_size(&self) -> usize {
        self.max_frame_size
    }
}

impl<W: Write> MplexWriter<W> {
    /// Flushes the internal buffer by sending it as MSG_DATA frame(s).
    ///
    /// If the buffer exceeds the max frame size, it's split into multiple frames.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        // Split buffer into frames if needed
        let mut pos = 0;
        while pos < self.buffer.len() {
            let remaining = self.buffer.len() - pos;
            let chunk_size = remaining.min(self.max_frame_size);
            let chunk = &self.buffer[pos..pos + chunk_size];
            send_msg(&mut self.inner, MessageCode::Data, chunk)?;
            pos += chunk_size;
        }

        self.buffer.clear();
        Ok(())
    }

    /// Writes a message with the specified message code.
    ///
    /// This is used for sending control messages (INFO, WARNING, ERROR, etc.)
    /// or other non-DATA message types. The message is sent immediately,
    /// flushing any buffered DATA first to maintain proper message ordering.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The payload exceeds [`MAX_PAYLOAD_LENGTH`]
    /// - The underlying I/O operation fails
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::Write;
    /// use protocol::{MplexWriter, MessageCode};
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let mut output = Vec::new();
    /// let mut writer = MplexWriter::new(&mut output);
    ///
    /// writer.write_message(MessageCode::Info, b"Processing started")?;
    /// writer.write_message(MessageCode::Warning, b"Slow network detected")?;
    /// writer.flush()?;
    /// # Ok(())
    /// # }
    /// # example().unwrap();
    /// ```
    pub fn write_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        // Flush buffered DATA first to maintain ordering
        self.flush_buffer()?;
        // Send the message
        send_msg(&mut self.inner, code, payload)?;
        self.inner.flush()
    }

    /// Writes a DATA message directly without buffering.
    ///
    /// Unlike [`Write::write`], this method immediately sends the data as MSG_DATA
    /// frame(s), splitting into multiple frames if the data exceeds the max frame size.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::Write;
    /// use protocol::MplexWriter;
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let mut output = Vec::new();
    /// let mut writer = MplexWriter::new(&mut output);
    ///
    /// // Send immediately without buffering
    /// writer.write_data(b"urgent data")?;
    /// # Ok(())
    /// # }
    /// # example().unwrap();
    /// ```
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        // Flush buffered data first
        self.flush_buffer()?;

        // Split into frames if needed
        let mut pos = 0;
        while pos < data.len() {
            let remaining = data.len() - pos;
            let chunk_size = remaining.min(self.max_frame_size);
            let chunk = &data[pos..pos + chunk_size];
            send_msg(&mut self.inner, MessageCode::Data, chunk)?;
            pos += chunk_size;
        }

        Ok(())
    }

    /// Writes raw bytes directly to the underlying stream, bypassing multiplexing.
    ///
    /// This is used for protocol exchanges like handshakes where upstream rsync
    /// writes directly without MSG_DATA framing. Any buffered data is flushed
    /// first to maintain proper message ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::Write;
    /// use protocol::MplexWriter;
    ///
    /// # fn example() -> std::io::Result<()> {
    /// let mut output = Vec::new();
    /// let mut writer = MplexWriter::new(&mut output);
    ///
    /// // Write some multiplexed data
    /// writer.write_all(b"data")?;
    ///
    /// // Write raw bytes for handshake
    /// writer.write_raw(b"@RSYNCD: 31.0\n")?;
    /// # Ok(())
    /// # }
    /// # example().unwrap();
    /// ```
    pub fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        // Flush buffered data first
        self.flush_buffer()?;
        // Write directly without framing
        self.inner.write_all(data)?;
        self.inner.flush()
    }

    /// Convenience method for writing an error message.
    ///
    /// Equivalent to `write_message(MessageCode::Error, msg.as_bytes())`.
    #[inline]
    pub fn write_error(&mut self, msg: &str) -> io::Result<()> {
        self.write_message(MessageCode::Error, msg.as_bytes())
    }

    /// Convenience method for writing a warning message.
    ///
    /// Equivalent to `write_message(MessageCode::Warning, msg.as_bytes())`.
    #[inline]
    pub fn write_warning(&mut self, msg: &str) -> io::Result<()> {
        self.write_message(MessageCode::Warning, msg.as_bytes())
    }

    /// Convenience method for writing an info message.
    ///
    /// Equivalent to `write_message(MessageCode::Info, msg.as_bytes())`.
    #[inline]
    pub fn write_info(&mut self, msg: &str) -> io::Result<()> {
        self.write_message(MessageCode::Info, msg.as_bytes())
    }
}

impl<W: Write> Write for MplexWriter<W> {
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
            self.write_data(buf)?;
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
    use crate::{recv_msg, MessageCode};

    #[test]
    fn mplex_writer_new() {
        let output: Vec<u8> = Vec::new();
        let writer = MplexWriter::new(output);
        assert_eq!(writer.buffered(), 0);
        assert_eq!(writer.buffer_size(), MplexWriter::<Vec<u8>>::DEFAULT_BUFFER_SIZE);
        assert_eq!(writer.max_frame_size(), MplexWriter::<Vec<u8>>::DEFAULT_MAX_FRAME_SIZE);
    }

    #[test]
    fn mplex_writer_with_capacity() {
        let output: Vec<u8> = Vec::new();
        let writer = MplexWriter::with_capacity(output, 1024);
        assert_eq!(writer.buffer_size(), 1024);
    }

    #[test]
    fn mplex_writer_with_sizes() {
        let output: Vec<u8> = Vec::new();
        let writer = MplexWriter::with_sizes(output, 2048, 512);
        assert_eq!(writer.buffer_size(), 2048);
        assert_eq!(writer.max_frame_size(), 512);
    }

    #[test]
    #[should_panic(expected = "exceeds MAX_PAYLOAD_LENGTH")]
    fn mplex_writer_with_sizes_exceeds_max() {
        let output: Vec<u8> = Vec::new();
        let _writer = MplexWriter::with_sizes(output, 1024, MAX_PAYLOAD_LENGTH as usize + 1);
    }

    #[test]
    fn mplex_writer_write_small() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_all(b"hello").unwrap();
        assert_eq!(writer.buffered(), 5);

        writer.flush().unwrap();
        assert_eq!(writer.buffered(), 0);

        // Verify the frame
        let mut cursor = std::io::Cursor::new(&output);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Data);
        assert_eq!(frame.payload(), b"hello");
    }

    #[test]
    fn mplex_writer_write_multiple_small() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_all(b"hello ").unwrap();
        writer.write_all(b"world").unwrap();
        assert_eq!(writer.buffered(), 11);

        writer.flush().unwrap();

        // Should be sent as a single frame
        let mut cursor = std::io::Cursor::new(&output);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.payload(), b"hello world");
    }

    #[test]
    fn mplex_writer_write_large() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::with_sizes(&mut output, 1024, 100);

        let data = vec![0x42u8; 250];
        writer.write_all(&data).unwrap();
        writer.flush().unwrap();

        // Should be split into 3 frames: 100 + 100 + 50
        let mut cursor = std::io::Cursor::new(&output);

        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.payload().len(), 100);

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.payload().len(), 100);

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.payload().len(), 50);
    }

    #[test]
    fn mplex_writer_write_message() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_message(MessageCode::Info, b"test message").unwrap();

        let mut cursor = std::io::Cursor::new(&output);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::Info);
        assert_eq!(frame.payload(), b"test message");
    }

    #[test]
    fn mplex_writer_write_message_flushes_buffer() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        // Buffer some data
        writer.write_all(b"buffered data").unwrap();
        assert_eq!(writer.buffered(), 13);

        // Write a control message - should flush buffer first
        writer.write_message(MessageCode::Warning, b"warning").unwrap();
        assert_eq!(writer.buffered(), 0);

        // Verify: DATA frame, then WARNING frame
        let mut cursor = std::io::Cursor::new(&output);

        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.code(), MessageCode::Data);
        assert_eq!(frame1.payload(), b"buffered data");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.code(), MessageCode::Warning);
        assert_eq!(frame2.payload(), b"warning");
    }

    #[test]
    fn mplex_writer_write_data() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_data(b"immediate").unwrap();

        let mut cursor = std::io::Cursor::new(&output);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.payload(), b"immediate");
    }

    #[test]
    fn mplex_writer_write_raw() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_raw(b"raw bytes").unwrap();

        // Raw bytes should be written directly, no multiplex frame
        assert_eq!(output, b"raw bytes");
    }

    #[test]
    fn mplex_writer_write_raw_flushes_buffer() {
        use std::io::Read;

        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_all(b"buffered").unwrap();
        assert_eq!(writer.buffered(), 8);

        writer.write_raw(b"raw").unwrap();
        assert_eq!(writer.buffered(), 0);

        // Should have a DATA frame followed by raw bytes
        let mut cursor = std::io::Cursor::new(&output[..]);
        let frame = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame.payload(), b"buffered");

        // Remaining bytes are raw
        let mut remaining = Vec::new();
        cursor.read_to_end(&mut remaining).unwrap();
        assert_eq!(remaining, b"raw");
    }

    #[test]
    fn mplex_writer_convenience_methods() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_error("error message").unwrap();
        writer.write_warning("warning message").unwrap();
        writer.write_info("info message").unwrap();

        let mut cursor = std::io::Cursor::new(&output);

        let frame1 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame1.code(), MessageCode::Error);
        assert_eq!(frame1.payload(), b"error message");

        let frame2 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame2.code(), MessageCode::Warning);
        assert_eq!(frame2.payload(), b"warning message");

        let frame3 = recv_msg(&mut cursor).unwrap();
        assert_eq!(frame3.code(), MessageCode::Info);
        assert_eq!(frame3.payload(), b"info message");
    }

    #[test]
    fn mplex_writer_get_ref() {
        let output = vec![1u8, 2, 3];
        let writer = MplexWriter::new(&output);
        assert_eq!(**writer.get_ref(), [1u8, 2, 3]);
    }

    #[test]
    fn mplex_writer_get_mut() {
        let mut output: Vec<u8> = Vec::new();
        let mut writer = MplexWriter::new(&mut output);
        writer.get_mut().push(42);
        assert_eq!(&**writer.get_ref(), &vec![42u8]);
    }

    #[test]
    fn mplex_writer_into_inner() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);
        writer.write_all(b"test").unwrap();
        writer.flush().unwrap();

        let inner = writer.into_inner();
        assert!(!inner.is_empty());
    }

    #[test]
    fn mplex_writer_auto_flush_on_overflow() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::with_capacity(&mut output, 10);

        writer.write_all(b"small").unwrap();
        assert_eq!(writer.buffered(), 5);

        // This should trigger auto-flush
        writer.write_all(b"trigger overflow").unwrap();
        // Buffer now contains "trigger overflow" (16 bytes > 10)
        // but since it's larger than buffer size, it was sent directly
        assert_eq!(writer.buffered(), 0);
    }

    #[test]
    fn mplex_writer_empty_write() {
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        let n = writer.write(&[]).unwrap();
        assert_eq!(n, 0);
        assert_eq!(writer.buffered(), 0);
    }

    #[test]
    fn mplex_writer_roundtrip_large_data() {
        use std::io::Read;

        let large_data = vec![0xAAu8; 100_000];
        let mut output = Vec::new();
        let mut writer = MplexWriter::new(&mut output);

        writer.write_all(&large_data).unwrap();
        writer.flush().unwrap();

        // Read back all frames
        let mut cursor = std::io::Cursor::new(&output);
        let mut reconstructed = Vec::new();

        loop {
            match recv_msg(&mut cursor) {
                Ok(frame) => {
                    assert_eq!(frame.code(), MessageCode::Data);
                    reconstructed.extend_from_slice(frame.payload());
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {}", e),
            }
        }

        assert_eq!(reconstructed, large_data);
    }
}
