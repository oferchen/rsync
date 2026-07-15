//! Multiplexed reader that implements `std::io::Read` with transparent demultiplexing.
//!
//! This module provides [`MplexReader`], a wrapper around any `Read` implementor that
//! automatically demultiplexes incoming rsync protocol messages. It extracts MSG_DATA
//! payloads for transparent reading and handles out-of-band messages (errors, warnings,
//! info) through a configurable handler.

use std::io::{self, Read};

use super::helpers::{map_envelope_error, read_payload_into};
use crate::envelope::{HEADER_LEN, MessageCode, MessageHeader};

/// Type alias for the message handler callback.
type MessageHandler = Box<dyn FnMut(MessageCode, &[u8]) + Send>;

/// A reader that transparently demultiplexes rsync protocol messages.
///
/// `MplexReader` wraps any `Read` implementor and automatically handles the rsync
/// multiplex protocol. When reading, it:
///
/// 1. Reads multiplex frames from the underlying stream
/// 2. Extracts `MSG_DATA` payloads for the caller
/// 3. Handles out-of-band messages (errors, warnings, info) via a message handler
/// 4. Buffers partial data to provide seamless streaming
///
/// # Protocol Details
///
/// The rsync multiplex protocol wraps all data in 4-byte headers:
/// - Byte 0: Tag (message code + MPLEX_BASE=7)
/// - Bytes 1-3: Payload length (24-bit little-endian)
///
/// # Examples
///
/// ```
/// use std::io::{self, Read, Cursor};
/// use protocol::{MplexReader, MessageCode, send_msg};
///
/// # fn example() -> io::Result<()> {
/// // Create a multiplex stream with a DATA message
/// let mut stream = Vec::new();
/// send_msg(&mut stream, MessageCode::Data, b"hello world")?;
///
/// // Wrap in MplexReader and read transparently
/// let mut reader = MplexReader::new(Cursor::new(stream));
/// let mut buf = [0u8; 5];
/// reader.read_exact(&mut buf)?;
/// assert_eq!(&buf, b"hello");
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
///
/// # Message Handling
///
/// Out-of-band messages can be handled via [`MplexReader::set_message_handler`]:
///
/// ```
/// use std::io::{self, Cursor};
/// use protocol::{MplexReader, MessageCode};
///
/// # fn example() -> io::Result<()> {
/// let reader = Cursor::new(Vec::<u8>::new());
/// let mut mplex = MplexReader::new(reader);
///
/// mplex.set_message_handler(|code, msg| {
///     eprintln!("Received {:?}: {:?}", code, msg);
/// });
/// # Ok(())
/// # }
/// # example().unwrap();
/// ```
pub struct MplexReader<R> {
    inner: R,
    /// Buffer for the current message payload
    buffer: Vec<u8>,
    /// Current read position in the buffer
    pos: usize,
    /// Handler for out-of-band messages
    message_handler: Option<MessageHandler>,
}

impl<R> MplexReader<R> {
    /// Creates a new multiplexed reader wrapping the given reader.
    ///
    /// The buffer is pre-allocated to 32KB, balancing memory usage and
    /// throughput across both local (pipe) and remote (TCP) paths.
    #[inline]
    pub fn new(inner: R) -> Self {
        Self::with_capacity(inner, 32 * 1024)
    }

    /// Creates a new multiplexed reader with a specific buffer capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::Cursor;
    /// use protocol::MplexReader;
    ///
    /// let reader = Cursor::new(Vec::<u8>::new());
    /// let mplex = MplexReader::with_capacity(reader, 32 * 1024);
    /// ```
    pub fn with_capacity(inner: R, capacity: usize) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(capacity),
            pos: 0,
            message_handler: None,
        }
    }

    /// Returns a reference to the underlying reader.
    ///
    /// Note that reading directly from the underlying reader will corrupt the
    /// multiplex stream. This method is primarily useful for inspecting reader
    /// state or calling methods that don't consume data.
    #[inline]
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the underlying reader.
    ///
    /// # Warning
    ///
    /// Reading directly from the underlying reader will corrupt the multiplex
    /// stream and break subsequent reads. Only use this when you need to call
    /// methods that don't consume data.
    #[inline]
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Consumes this reader and returns the underlying reader.
    ///
    /// Any buffered data is discarded. If you need to preserve buffered data,
    /// read it all before calling this method.
    #[inline]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Sets a handler for out-of-band messages.
    ///
    /// The handler will be called for all non-DATA messages (errors, warnings,
    /// info, etc.) received during reads. If no handler is set, these messages
    /// are silently discarded.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::io::Cursor;
    /// use protocol::{MplexReader, MessageCode};
    ///
    /// let mut reader = MplexReader::new(Cursor::new(Vec::<u8>::new()));
    ///
    /// reader.set_message_handler(|code, payload| {
    ///     if let Ok(msg) = std::str::from_utf8(payload) {
    ///         eprintln!("[{:?}] {}", code, msg);
    ///     }
    /// });
    /// ```
    pub fn set_message_handler<F>(&mut self, handler: F)
    where
        F: FnMut(MessageCode, &[u8]) + Send + 'static,
    {
        self.message_handler = Some(Box::new(handler));
    }

    /// Clears the message handler, causing out-of-band messages to be silently discarded.
    #[inline]
    pub fn clear_message_handler(&mut self) {
        self.message_handler = None;
    }

    /// Returns the number of bytes currently buffered.
    #[inline]
    pub fn buffered(&self) -> usize {
        self.buffer.len().saturating_sub(self.pos)
    }
}

impl<R: Read> MplexReader<R> {
    /// Reads the next multiplex frame header.
    fn read_header(&mut self) -> io::Result<MessageHeader> {
        let mut header_bytes = [0u8; HEADER_LEN];
        self.inner.read_exact(&mut header_bytes)?;
        MessageHeader::decode(&header_bytes).map_err(map_envelope_error)
    }

    /// Reads a complete multiplex message and handles it.
    ///
    /// Returns `true` if a DATA message was received and buffered,
    /// `false` if a non-DATA message was handled.
    fn read_message(&mut self) -> io::Result<bool> {
        let header = self.read_header()?;
        let code = header.code();
        let len = header.payload_len_usize();

        self.buffer.clear();
        self.pos = 0;
        read_payload_into(&mut self.inner, &mut self.buffer, len)?;

        match code {
            MessageCode::Data => Ok(true),
            other => {
                if let Some(ref mut handler) = self.message_handler {
                    handler(other, &self.buffer);
                }
                Ok(false)
            }
        }
    }
}

impl<R: Read> Read for MplexReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.buffer.len() {
            let available = self.buffer.len() - self.pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
            self.pos += to_copy;
            return Ok(to_copy);
        }

        // Buffer exhausted: pump out-of-band frames through the handler until
        // the next non-empty DATA frame arrives, then return its bytes to the
        // caller.
        //
        // An empty DATA frame (zero-length payload) is upstream rsync's lull
        // keepalive (io.c:maybe_send_keepalive sends `send_msg(MSG_DATA, "", 0, 0)`).
        // It carries no data, so it must be absorbed silently: returning the
        // resulting `Ok(0)` here would be read by callers as end-of-stream and
        // would truncate the transfer. Continue the loop instead so the next
        // real frame is fetched. Genuine end-of-stream still surfaces as the
        // `UnexpectedEof` error from `read_header`, never as `Ok(0)`.
        loop {
            if self.read_message()? && !self.buffer.is_empty() {
                let to_copy = self.buffer.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
                self.pos = to_copy;
                return Ok(to_copy);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageCode, send_msg};
    use std::io::Cursor;

    #[test]
    fn mplex_reader_new() {
        let data: Vec<u8> = Vec::new();
        let reader = MplexReader::new(Cursor::new(data));
        assert_eq!(reader.buffered(), 0);
    }

    #[test]
    fn mplex_reader_with_capacity() {
        let data: Vec<u8> = Vec::new();
        let reader = MplexReader::with_capacity(Cursor::new(data), 128);
        assert_eq!(reader.buffer.capacity(), 128);
    }

    #[test]
    fn mplex_reader_read_single_data_message() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"hello world").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 11];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn mplex_reader_read_partial() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"hello world").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b" worl");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 1);
        assert_eq!(&buf[..1], b"d");
    }

    #[test]
    fn mplex_reader_read_multiple_data_messages() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"first").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"second").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 10];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"first");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf[..6], b"second");
    }

    #[test]
    fn mplex_reader_skips_non_data_messages() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Info, b"info message").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"data").unwrap();
        send_msg(&mut stream, MessageCode::Warning, b"warning").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"more").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 10];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"data");

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"more");
    }

    #[test]
    fn mplex_reader_message_handler() {
        use std::sync::{Arc, Mutex};

        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Info, b"info").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"data").unwrap();
        send_msg(&mut stream, MessageCode::Warning, b"warn").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"more").unwrap();

        let messages = Arc::new(Mutex::new(Vec::new()));
        let messages_clone = messages.clone();

        let mut reader = MplexReader::new(Cursor::new(stream));
        reader.set_message_handler(move |code, payload| {
            messages_clone
                .lock()
                .unwrap()
                .push((code, payload.to_vec()));
        });

        let mut buf = [0u8; 10];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"data");

        // The Info frame ahead of the first DATA frame should have been
        // dispatched to the handler before the read returns.
        {
            let captured = messages.lock().unwrap();
            assert_eq!(captured.len(), 1);
            assert_eq!(captured[0].0, MessageCode::Info);
            assert_eq!(captured[0].1, b"info");
        }

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..4], b"more");

        let captured = messages.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].0, MessageCode::Info);
        assert_eq!(captured[0].1, b"info");
        assert_eq!(captured[1].0, MessageCode::Warning);
        assert_eq!(captured[1].1, b"warn");
    }

    #[test]
    fn mplex_reader_absorbs_empty_data_keepalive() {
        // An empty DATA frame is upstream's lull keepalive (io.c:1473
        // `send_msg(MSG_DATA, "", 0, 0)`). It must be absorbed silently: the
        // reader skips it and returns the following real frame's bytes, never a
        // premature `Ok(0)` that a caller would read as end-of-stream.
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, &[]).unwrap();
        send_msg(&mut stream, MessageCode::Data, b"next").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 10];

        let n = reader.read(&mut buf).unwrap();
        assert_eq!(
            n, 4,
            "empty keepalive frame must be absorbed, not seen as EOF"
        );
        assert_eq!(&buf[..4], b"next");
    }

    #[test]
    fn mplex_reader_read_exact() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"exactly 16 bytes").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"exactly 16 bytes");
    }

    #[test]
    fn mplex_reader_large_message() {
        let large_data = vec![0x42u8; 100_000];
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, &large_data).unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = vec![0u8; 100_000];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(buf, large_data);
    }

    #[test]
    fn mplex_reader_get_ref() {
        let data = vec![1, 2, 3];
        let reader = MplexReader::new(Cursor::new(data.clone()));
        assert_eq!(reader.get_ref().get_ref(), &data);
    }

    #[test]
    fn mplex_reader_get_mut() {
        let data = vec![1, 2, 3];
        let mut reader = MplexReader::new(Cursor::new(data));
        reader.get_mut().set_position(1);
        assert_eq!(reader.get_ref().position(), 1);
    }

    #[test]
    fn mplex_reader_into_inner() {
        let data = vec![1, 2, 3];
        let reader = MplexReader::new(Cursor::new(data.clone()));
        let inner = reader.into_inner();
        assert_eq!(inner.into_inner(), data);
    }

    #[test]
    fn mplex_reader_buffered() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"hello world").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        assert_eq!(reader.buffered(), 0);

        let mut buf = [0u8; 5];
        let _ = reader.read(&mut buf).unwrap();
        // 11-byte payload, 5 consumed -> 6 remaining (" world").
        assert_eq!(reader.buffered(), 6);

        let _ = reader.read(&mut buf).unwrap();
        // Another 5 bytes consumed (" worl") -> 1 remaining ("d").
        assert_eq!(reader.buffered(), 1);
    }

    #[test]
    fn mplex_reader_clear_message_handler() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Info, b"info").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"data").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        reader.set_message_handler(|_, _| panic!("should not be called"));
        reader.clear_message_handler();

        let mut buf = [0u8; 10];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn mplex_reader_eof() {
        let stream = Vec::new();
        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 10];
        let result = reader.read(&mut buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn mplex_reader_interleaved_messages() {
        let mut stream = Vec::new();
        send_msg(&mut stream, MessageCode::Data, b"first").unwrap();
        send_msg(&mut stream, MessageCode::Info, b"info1").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"second").unwrap();
        send_msg(&mut stream, MessageCode::Warning, b"warn1").unwrap();
        send_msg(&mut stream, MessageCode::Error, b"err1").unwrap();
        send_msg(&mut stream, MessageCode::Data, b"third").unwrap();

        let mut reader = MplexReader::new(Cursor::new(stream));
        let mut result = Vec::new();
        let mut buf = [0u8; 10];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => result.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(result, b"firstsecondthird");
    }
}
