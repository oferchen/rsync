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
#[allow(private_interfaces)]
#[allow(clippy::large_enum_variant)]
pub enum ServerReader<R: Read> {
    /// Plain mode - read data directly without demultiplexing
    Plain(R),
    /// Multiplex mode - extract data from MSG_DATA frames
    Multiplex(MultiplexReader<R>),
    /// Compressed+Multiplex mode - decompress then demultiplex
    Compressed(CompressedReader<MultiplexReader<R>>),
}

impl<R: Read> ServerReader<R> {
    /// Creates a new plain-mode reader
    #[inline]
    pub const fn new_plain(reader: R) -> Self {
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
    #[inline]
    pub const fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_) | Self::Compressed(_))
    }

    /// Attempts to borrow exactly `len` bytes from the internal frame buffer.
    ///
    /// Returns `Some(&[u8])` when the multiplexed reader's current frame has
    /// enough data, avoiding one buffer copy. Returns `None` for plain or
    /// compressed modes (where no internal frame buffer exists) or when the
    /// data spans a frame boundary.
    ///
    /// Callers should fall back to `Read::read_exact()` when this returns `None`.
    pub fn try_borrow_exact(&mut self, len: usize) -> io::Result<Option<&[u8]>> {
        match self {
            Self::Multiplex(mux) => mux.try_borrow_exact(len),
            _ => Ok(None),
        }
    }

    /// Returns and resets accumulated `MSG_IO_ERROR` flags from the sender.
    ///
    /// When the multiplexed reader encounters `MSG_IO_ERROR` messages, it
    /// accumulates the 4-byte little-endian error flags via bitwise OR.
    /// The receiver should call this periodically and forward any non-zero
    /// value to the generator via `MSG_IO_ERROR`.
    ///
    /// Returns 0 for plain-mode readers (no multiplexing, no MSG_IO_ERROR possible).
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1521-1528`: receiver accumulates `io_error |= val` and forwards
    ///   via `send_msg_int(MSG_IO_ERROR, val)` when `am_receiver`.
    pub fn take_io_error(&mut self) -> i32 {
        match self {
            Self::Multiplex(mux) => mux.take_io_error(),
            Self::Compressed(compressed) => compressed.get_mut().take_io_error(),
            Self::Plain(_) => 0,
        }
    }

    /// Returns and drains accumulated `MSG_NO_SEND` file indices from the sender.
    ///
    /// When the sender cannot open a file it was asked to transfer, it sends
    /// `MSG_NO_SEND` with the 4-byte little-endian file index (protocol >= 30).
    /// The receiver accumulates these indices during normal reads and drains
    /// them via this method.
    ///
    /// Returns an empty `Vec` for plain-mode readers.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1618-1627`: `MSG_NO_SEND` dispatches to `got_flist_entry_status(FES_NO_SEND, val)`
    ///   on the generator side, or forwards to the generator if on the receiver side.
    /// - `sender.c:367-368`: sender sends `MSG_NO_SEND` for protocol >= 30 when file open fails.
    pub fn take_no_send_indices(&mut self) -> Vec<i32> {
        match self {
            Self::Multiplex(mux) => mux.take_no_send_indices(),
            Self::Compressed(compressed) => compressed.get_mut().take_no_send_indices(),
            Self::Plain(_) => Vec::new(),
        }
    }

    /// Returns and drains accumulated `MSG_REDO` file indices from the receiver.
    ///
    /// When the receiver detects a whole-file checksum failure, it sends
    /// `MSG_REDO` with the 4-byte little-endian file index. The generator
    /// accumulates these indices during normal reads and drains them via
    /// this method.
    ///
    /// Returns an empty `Vec` for plain-mode readers.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1514-1519`: `MSG_REDO` dispatches to `got_flist_entry_status(FES_REDO, val)`
    ///   on the generator side, pushing the NDX to `redo_list`.
    /// - `receiver.c:970-974`: receiver sends `send_msg_int(MSG_REDO, ndx)` on checksum failure.
    pub fn take_redo_indices(&mut self) -> Vec<i32> {
        match self {
            Self::Multiplex(mux) => mux.take_redo_indices(),
            Self::Compressed(compressed) => compressed.get_mut().take_redo_indices(),
            Self::Plain(_) => Vec::new(),
        }
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
///
/// When `MSG_IO_ERROR` frames are received, the 4-byte little-endian payload
/// is OR'd into an internal accumulator. Callers retrieve and forward the
/// accumulated value via [`MultiplexReader::take_io_error`].
///
/// When `MSG_NO_SEND` frames are received, the 4-byte little-endian file index
/// is accumulated into an internal queue. Callers drain the queue via
/// [`MultiplexReader::take_no_send_indices`].
///
/// # Upstream Reference
///
/// - `io.c:1521-1528`: receiver reads `MSG_IO_ERROR`, OR's value into
///   `io_error`, and forwards it to the generator when `am_receiver`.
/// - `io.c:1618-1627`: `MSG_NO_SEND` received on the sender/receiver pipe;
///   if `am_generator`, calls `got_flist_entry_status(FES_NO_SEND, val)`,
///   otherwise forwards to the generator.
pub(super) struct MultiplexReader<R> {
    inner: R,
    buffer: Vec<u8>,
    pos: usize,
    /// Accumulated I/O error flags from `MSG_IO_ERROR` messages.
    ///
    /// Uses the same bitfield constants as [`super::io_error_flags`].
    /// upstream: io.c:1526 `io_error |= val;`
    io_error: i32,
    /// File indices received via `MSG_NO_SEND` from the sender.
    ///
    /// When the sender fails to open a file, it sends `MSG_NO_SEND` with the
    /// 4-byte little-endian file index. The receiver accumulates these indices
    /// so it can skip waiting for delta data for those files.
    ///
    /// upstream: io.c:1618-1627, sender.c:367-368
    no_send_indices: Vec<i32>,
    /// File indices received via `MSG_REDO` from the receiver.
    ///
    /// When the receiver detects a whole-file checksum mismatch, it sends
    /// `MSG_REDO` with the 4-byte little-endian file index. The generator
    /// accumulates these indices and re-sends the files with full checksum
    /// length (no delta basis) in a redo pass.
    ///
    /// upstream: io.c:1514-1519, receiver.c:970-974
    redo_indices: Vec<i32>,
}

/// Default buffer capacity for MultiplexReader.
///
/// 64KB matches the `MultiplexWriter` buffer size and upstream rsync's
/// `IO_BUFFER_SIZE`. When receiving from an oc-rsync sender, frames can
/// be up to 64KB — a smaller staging buffer forces extra reads per frame.
const MULTIPLEX_READER_BUFFER_CAPACITY: usize = 64 * 1024;

impl<R: Read> MultiplexReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            // Pre-allocate buffer to avoid repeated allocations during transfer
            buffer: Vec::with_capacity(MULTIPLEX_READER_BUFFER_CAPACITY),
            pos: 0,
            io_error: 0,
            no_send_indices: Vec::new(),
            redo_indices: Vec::new(),
        }
    }

    /// Returns the accumulated `MSG_IO_ERROR` flags and resets the accumulator.
    ///
    /// The receiver should call this after each read batch and forward any
    /// non-zero value to the generator via `MSG_IO_ERROR`.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1526-1528`: `io_error |= val; if (am_receiver) send_msg_int(MSG_IO_ERROR, val);`
    fn take_io_error(&mut self) -> i32 {
        std::mem::take(&mut self.io_error)
    }

    /// Returns and drains the accumulated `MSG_NO_SEND` file indices.
    ///
    /// When the sender cannot open a file, it sends `MSG_NO_SEND` with the
    /// file index. The receiver accumulates these indices so it can skip
    /// those files during transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1618-1627`: `MSG_NO_SEND` handling — generator calls
    ///   `got_flist_entry_status(FES_NO_SEND, val)`, receiver forwards to generator.
    /// - `sender.c:367-368`: sender sends `MSG_NO_SEND` when `protocol_version >= 30`
    ///   and the source file cannot be opened.
    fn take_no_send_indices(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.no_send_indices)
    }

    /// Handles a `MSG_IO_ERROR` payload by accumulating the error flags.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32`).
    /// upstream: io.c:1522-1526
    fn handle_io_error_msg(&mut self) {
        if self.buffer.len() == 4 {
            let val = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.io_error |= val;
        }
    }

    /// Returns and drains the accumulated `MSG_REDO` file indices.
    ///
    /// When the receiver detects a whole-file checksum failure, it sends
    /// `MSG_REDO` with the file index. The generator accumulates these
    /// indices so it can re-send those files with full checksum length.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1514-1519`: `MSG_REDO` received on the sender/receiver pipe;
    ///   calls `got_flist_entry_status(FES_REDO, val)` which pushes to `redo_list`.
    /// - `receiver.c:970-974`: receiver sends `MSG_REDO` when `!redoing`.
    fn take_redo_indices(&mut self) -> Vec<i32> {
        std::mem::take(&mut self.redo_indices)
    }

    /// Handles a `MSG_REDO` payload by recording the file index.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32` file index).
    /// upstream: io.c:1514-1519 — `val = raw_read_int();` reads 4-byte LE int.
    fn handle_redo_msg(&mut self) {
        if self.buffer.len() == 4 {
            let ndx = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.redo_indices.push(ndx);
        }
    }

    /// Handles a `MSG_NO_SEND` payload by recording the file index.
    ///
    /// The payload must be exactly 4 bytes (little-endian `i32` file index).
    /// upstream: io.c:1618-1627 — `val = raw_read_int();` reads 4-byte LE int.
    fn handle_no_send_msg(&mut self) {
        if self.buffer.len() == 4 {
            let ndx = i32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            self.no_send_indices.push(ndx);
        }
    }
}

impl<R: Read> MultiplexReader<R> {
    /// Attempts to borrow exactly `len` bytes from the internal frame buffer.
    ///
    /// Returns `Some(&[u8])` if the current frame buffer has at least `len` bytes
    /// available, avoiding a copy into an intermediate buffer. If the buffer is
    /// empty, reads the next MSG_DATA frame first. Returns `None` when the
    /// requested data spans frame boundaries — the caller should fall back to
    /// `Read::read_exact()` with a separate buffer.
    ///
    /// # Zero-copy optimization
    ///
    /// This eliminates one buffer copy for literal delta tokens that fit within
    /// a single MSG_DATA frame (the common case for tokens up to 32–64KB).
    fn try_borrow_exact(&mut self, len: usize) -> io::Result<Option<&[u8]>> {
        // If buffer exhausted, read next MSG_DATA frame
        if self.pos >= self.buffer.len() {
            loop {
                self.buffer.clear();
                self.pos = 0;

                let code = protocol::recv_msg_into(&mut self.inner, &mut self.buffer)?;

                match code {
                    protocol::MessageCode::Data => break,
                    protocol::MessageCode::Info
                    | protocol::MessageCode::Warning
                    | protocol::MessageCode::Log
                    | protocol::MessageCode::Client => {
                        if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                            eprint!("{msg}");
                        }
                    }
                    protocol::MessageCode::Error
                    | protocol::MessageCode::ErrorXfer
                    | protocol::MessageCode::ErrorSocket
                    | protocol::MessageCode::ErrorUtf8
                    | protocol::MessageCode::ErrorExit => {
                        if let Ok(msg) = std::str::from_utf8(&self.buffer) {
                            eprint!("{msg}");
                        }
                    }
                    protocol::MessageCode::IoError => {
                        // upstream: io.c:1521-1526
                        self.handle_io_error_msg();
                    }
                    protocol::MessageCode::NoSend => {
                        // upstream: io.c:1618-1627
                        self.handle_no_send_msg();
                    }
                    protocol::MessageCode::Redo => {
                        // upstream: io.c:1514-1519
                        self.handle_redo_msg();
                    }
                    _ => {}
                }
            }
        }

        let available = self.buffer.len() - self.pos;
        if available >= len {
            let start = self.pos;
            self.pos += len;
            Ok(Some(&self.buffer[start..start + len]))
        } else {
            // Token spans frame boundary — caller must use read_exact fallback
            Ok(None)
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
                protocol::MessageCode::IoError => {
                    // upstream: io.c:1521-1526
                    // Accumulate the I/O error flags from the sender.
                    // The receiver will forward these to the generator.
                    self.handle_io_error_msg();
                }
                protocol::MessageCode::NoSend => {
                    // upstream: io.c:1618-1627
                    // Accumulate the file index from the sender indicating
                    // it could not open the requested file.
                    self.handle_no_send_msg();
                }
                protocol::MessageCode::Redo => {
                    // upstream: io.c:1514-1519
                    // Accumulate the file index from the receiver indicating
                    // a whole-file checksum verification failure.
                    self.handle_redo_msg();
                }
                _ => {
                    // Other message types (Stats, etc.): silently skip
                    // Continue loop to read next message
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn server_reader_new_plain() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data));
        assert!(matches!(reader, ServerReader::Plain(_)));
    }

    #[test]
    fn server_reader_activate_multiplex() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data));
        let result = reader.activate_multiplex();
        assert!(result.is_ok());
        let multiplexed = result.unwrap();
        assert!(matches!(multiplexed, ServerReader::Multiplex(_)));
    }

    #[test]
    fn server_reader_activate_multiplex_twice_fails() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data));
        let multiplexed = reader.activate_multiplex().unwrap();
        let result = multiplexed.activate_multiplex();
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn server_reader_is_multiplexed_plain() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data));
        assert!(!reader.is_multiplexed());
    }

    #[test]
    fn server_reader_is_multiplexed_multiplex() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data))
            .activate_multiplex()
            .unwrap();
        assert!(reader.is_multiplexed());
    }

    #[test]
    fn server_reader_plain_read() {
        let data = vec![1, 2, 3, 4, 5];
        let mut reader = ServerReader::new_plain(Cursor::new(data));
        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(buf, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn server_reader_plain_partial_read() {
        let data = vec![1, 2, 3, 4, 5];
        let mut reader = ServerReader::new_plain(Cursor::new(data));
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, [1, 2, 3]);
    }

    #[test]
    fn server_reader_plain_empty_read() {
        let data: Vec<u8> = vec![];
        let mut reader = ServerReader::new_plain(Cursor::new(data));
        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn server_reader_activate_compression_on_plain_fails() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data));
        let result = reader.activate_compression(CompressionAlgorithm::Zlib);
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn server_reader_activate_compression_on_multiplex_succeeds() {
        let data = vec![1, 2, 3, 4, 5];
        let reader = ServerReader::new_plain(Cursor::new(data))
            .activate_multiplex()
            .unwrap();
        let result = reader.activate_compression(CompressionAlgorithm::Zlib);
        assert!(result.is_ok());
        let compressed = result.unwrap();
        assert!(compressed.is_multiplexed());
    }

    #[test]
    fn server_reader_activate_compression_twice_fails() {
        let data = vec![1, 2, 3, 4, 5];
        let compressed = ServerReader::new_plain(Cursor::new(data))
            .activate_multiplex()
            .unwrap()
            .activate_compression(CompressionAlgorithm::Zlib)
            .unwrap();
        let result = compressed.activate_compression(CompressionAlgorithm::Zlib);
        assert!(result.is_err());
        match result {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::AlreadyExists),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn multiplex_reader_new() {
        let data = vec![1, 2, 3, 4, 5];
        let mux = MultiplexReader::new(Cursor::new(data));
        assert!(mux.buffer.is_empty());
        assert_eq!(mux.pos, 0);
    }

    #[test]
    fn multiplex_reader_buffered_read() {
        // Create a MultiplexReader with pre-populated buffer (simulating internal state)
        let data = vec![];
        let mut mux = MultiplexReader::new(Cursor::new(data));

        // Manually populate the buffer as if we had read a message
        mux.buffer = vec![10, 20, 30, 40, 50];
        mux.pos = 0;

        // Read from buffer
        let mut buf = [0u8; 3];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, [10, 20, 30]);
        assert_eq!(mux.pos, 3);
    }

    #[test]
    fn multiplex_reader_buffered_read_complete() {
        let data = vec![];
        let mut mux = MultiplexReader::new(Cursor::new(data));

        // Populate buffer
        mux.buffer = vec![10, 20, 30];
        mux.pos = 0;

        // Read entire buffer
        let mut buf = [0u8; 5];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], &[10, 20, 30]);
        // Buffer should be cleared when exhausted
        assert!(mux.buffer.is_empty());
        assert_eq!(mux.pos, 0);
    }

    #[test]
    fn multiplex_reader_buffered_partial_read() {
        let data = vec![];
        let mut mux = MultiplexReader::new(Cursor::new(data));

        // Populate buffer
        mux.buffer = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        mux.pos = 2; // Start from position 2

        // Read 3 bytes
        let mut buf = [0u8; 3];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 3);
        assert_eq!(buf, [3, 4, 5]);
        assert_eq!(mux.pos, 5);
    }

    #[test]
    fn multiplex_reader_accumulates_msg_io_error() {
        // Construct a stream with MSG_IO_ERROR interleaved with MSG_DATA.
        // MSG_IO_ERROR carries a 4-byte LE i32 payload.
        // upstream: io.c:1521-1526
        let mut stream = Vec::new();

        // First: MSG_IO_ERROR with IOERR_GENERAL (1)
        let io_err_val: i32 = 1; // IOERR_GENERAL
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::IoError,
            &io_err_val.to_le_bytes(),
        )
        .unwrap();

        // Then: MSG_DATA with some file data
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"hello").unwrap();

        // Another MSG_IO_ERROR with IOERR_VANISHED (2) — should be OR'd in
        let io_err_val2: i32 = 2; // IOERR_VANISHED
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::IoError,
            &io_err_val2.to_le_bytes(),
        )
        .unwrap();

        // More MSG_DATA
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"world").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));

        // Read first data message — should skip over the MSG_IO_ERROR
        let mut buf = [0u8; 5];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");

        // After first read, IOERR_GENERAL should be accumulated
        assert_eq!(mux.io_error, 1);

        // Read second data message — skips second MSG_IO_ERROR
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");

        // Both flags should be OR'd together: 1 | 2 = 3
        assert_eq!(mux.io_error, 3);

        // take_io_error returns the accumulated value and resets
        let taken = mux.take_io_error();
        assert_eq!(taken, 3);
        assert_eq!(mux.io_error, 0);
    }

    #[test]
    fn multiplex_reader_io_error_wrong_payload_length_ignored() {
        // MSG_IO_ERROR with wrong payload length (not 4 bytes) should be ignored.
        // upstream: io.c:1522 `if (msg_bytes != 4) goto invalid_msg;`
        let mut stream = Vec::new();

        // MSG_IO_ERROR with 3 bytes (invalid — should be ignored)
        protocol::send_msg(&mut stream, protocol::MessageCode::IoError, &[1, 0, 0]).unwrap();

        // MSG_DATA follows
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 2];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf, b"ok");

        // Invalid payload length should not accumulate any error
        assert_eq!(mux.io_error, 0);
    }

    #[test]
    fn server_reader_take_io_error_plain_returns_zero() {
        let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
        assert_eq!(reader.take_io_error(), 0);
    }

    #[test]
    fn server_reader_take_io_error_multiplex_accumulates() {
        // Build a multiplex stream with MSG_IO_ERROR + MSG_DATA
        let mut stream = Vec::new();
        let io_err: i32 = 1; // IOERR_GENERAL
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::IoError,
            &io_err.to_le_bytes(),
        )
        .unwrap();
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

        let mut reader = ServerReader::new_plain(Cursor::new(stream))
            .activate_multiplex()
            .unwrap();

        // Read the data (which skips the IO_ERROR message)
        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");

        // take_io_error should return the accumulated value
        let io_error = reader.take_io_error();
        assert_eq!(io_error, 1);

        // Second call returns 0 (reset)
        assert_eq!(reader.take_io_error(), 0);
    }

    #[test]
    fn msg_io_error_round_trip_through_multiplex_layer() {
        // Verifies the full MSG_IO_ERROR round-trip:
        // 1. Sender writes MSG_IO_ERROR via multiplex writer
        // 2. Receiver reads it via multiplex reader (accumulates flags)
        // 3. Receiver forwards accumulated flags via multiplex writer
        // 4. Generator receives the forwarded MSG_IO_ERROR
        //
        // upstream: io.c:1521-1528 — receiver accumulates and forwards MSG_IO_ERROR.
        use super::super::io_error_flags;
        use protocol::{MessageCode, MplexWriter};
        use std::io::Write;

        // Step 1: Build a wire stream with two MSG_IO_ERROR messages
        // interleaved with MSG_DATA.
        //
        // Wire layout: [IO_ERROR(1)] [DATA("part1")] [IO_ERROR(2)] [DATA("part2")]
        let mut wire = Vec::new();
        {
            let mut writer = MplexWriter::new(&mut wire);

            let flags1 = io_error_flags::IOERR_GENERAL;
            writer
                .write_message(MessageCode::IoError, &flags1.to_le_bytes())
                .unwrap();
            writer.write_all(b"part1").unwrap();
            writer.flush().unwrap();

            let flags2 = io_error_flags::IOERR_VANISHED;
            writer
                .write_message(MessageCode::IoError, &flags2.to_le_bytes())
                .unwrap();
            writer.write_all(b"part2").unwrap();
            writer.flush().unwrap();
        }

        // Step 2: Receiver reads through the stream. Each read consumes
        // MSG_IO_ERROR messages encountered while searching for MSG_DATA.
        let mut reader = MultiplexReader::new(Cursor::new(wire));
        let mut buf = [0u8; 5];

        // First read: skips IO_ERROR(1), returns DATA("part1")
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"part1");

        // Drain the first error (IOERR_GENERAL=1)
        let first = reader.take_io_error();
        assert_eq!(first, io_error_flags::IOERR_GENERAL);

        // Second read: skips IO_ERROR(2), returns DATA("part2")
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"part2");

        // Drain the second error (IOERR_VANISHED=2)
        let second = reader.take_io_error();
        assert_eq!(second, io_error_flags::IOERR_VANISHED);

        // Combine both errors (as the receiver would do before forwarding)
        let combined = first | second;
        assert_eq!(
            combined,
            io_error_flags::IOERR_GENERAL | io_error_flags::IOERR_VANISHED
        );

        // Step 3: Receiver forwards the accumulated io_error to the generator
        let mut forward_wire = Vec::new();
        {
            let mut fwd_writer = MplexWriter::new(&mut forward_wire);
            fwd_writer
                .write_message(MessageCode::IoError, &combined.to_le_bytes())
                .unwrap();
        }

        // Step 4: Generator receives the forwarded MSG_IO_ERROR
        let mut fwd_cursor = Cursor::new(forward_wire);
        let frame = protocol::recv_msg(&mut fwd_cursor).unwrap();
        assert_eq!(frame.code(), MessageCode::IoError);
        assert_eq!(frame.payload().len(), 4);
        let forwarded_flags = i32::from_le_bytes(frame.payload().try_into().unwrap());
        assert_eq!(
            forwarded_flags,
            io_error_flags::IOERR_GENERAL | io_error_flags::IOERR_VANISHED
        );

        // Verify the exit code mapping
        let exit_code = io_error_flags::to_exit_code(forwarded_flags);
        assert_eq!(exit_code, 23); // RERR_PARTIAL — IOERR_GENERAL takes priority
    }

    #[test]
    fn multiplex_reader_accumulates_msg_no_send() {
        // MSG_NO_SEND carries a 4-byte LE i32 file index.
        // upstream: io.c:1618-1627, sender.c:367-368
        let mut stream = Vec::new();

        // MSG_NO_SEND with file index 42
        let ndx1: i32 = 42;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::NoSend,
            &ndx1.to_le_bytes(),
        )
        .unwrap();

        // MSG_DATA with some file data
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"hello").unwrap();

        // Another MSG_NO_SEND with file index 99
        let ndx2: i32 = 99;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::NoSend,
            &ndx2.to_le_bytes(),
        )
        .unwrap();

        // More MSG_DATA
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"world").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));

        // Read first data message — should skip over the MSG_NO_SEND
        let mut buf = [0u8; 5];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");

        // After first read, ndx 42 should be accumulated
        assert_eq!(mux.no_send_indices, vec![42]);

        // Read second data message — skips second MSG_NO_SEND
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");

        // Both indices should be accumulated
        assert_eq!(mux.no_send_indices, vec![42, 99]);

        // take_no_send_indices returns accumulated values and resets
        let taken = mux.take_no_send_indices();
        assert_eq!(taken, vec![42, 99]);
        assert!(mux.no_send_indices.is_empty());
    }

    #[test]
    fn multiplex_reader_no_send_wrong_payload_length_ignored() {
        // MSG_NO_SEND with wrong payload length (not 4 bytes) should be ignored.
        // upstream: io.c:1619 `if (msg_bytes != 4) goto invalid_msg;`
        let mut stream = Vec::new();

        // MSG_NO_SEND with 3 bytes (invalid — should be ignored)
        protocol::send_msg(&mut stream, protocol::MessageCode::NoSend, &[1, 0, 0]).unwrap();

        // MSG_DATA follows
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 2];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf, b"ok");

        // Invalid payload length should not accumulate any index
        assert!(mux.no_send_indices.is_empty());
    }

    #[test]
    fn server_reader_take_no_send_indices_plain_returns_empty() {
        let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
        assert!(reader.take_no_send_indices().is_empty());
    }

    #[test]
    fn server_reader_take_no_send_indices_multiplex_accumulates() {
        // Build a multiplex stream with MSG_NO_SEND + MSG_DATA
        let mut stream = Vec::new();
        let ndx: i32 = 7;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::NoSend,
            &ndx.to_le_bytes(),
        )
        .unwrap();
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

        let mut reader = ServerReader::new_plain(Cursor::new(stream))
            .activate_multiplex()
            .unwrap();

        // Read the data (which skips the MSG_NO_SEND message)
        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");

        // take_no_send_indices should return the accumulated index
        let indices = reader.take_no_send_indices();
        assert_eq!(indices, vec![7]);

        // Second call returns empty (reset)
        assert!(reader.take_no_send_indices().is_empty());
    }

    #[test]
    fn multiplex_reader_accumulates_msg_redo() {
        // MSG_REDO carries a 4-byte LE i32 file index.
        // upstream: io.c:1514-1519, receiver.c:970-974
        let mut stream = Vec::new();

        // MSG_REDO with file index 5
        let ndx1: i32 = 5;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::Redo,
            &ndx1.to_le_bytes(),
        )
        .unwrap();

        // MSG_DATA with some file data
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"chunk1").unwrap();

        // Another MSG_REDO with file index 17
        let ndx2: i32 = 17;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::Redo,
            &ndx2.to_le_bytes(),
        )
        .unwrap();

        // More MSG_DATA
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"chunk2").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));

        // Read first data message — should skip over the MSG_REDO
        let mut buf = [0u8; 6];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"chunk1");

        // After first read, ndx 5 should be accumulated
        assert_eq!(mux.redo_indices, vec![5]);

        // Read second data message — skips second MSG_REDO
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 6);
        assert_eq!(&buf, b"chunk2");

        // Both indices should be accumulated
        assert_eq!(mux.redo_indices, vec![5, 17]);

        // take_redo_indices returns accumulated values and resets
        let taken = mux.take_redo_indices();
        assert_eq!(taken, vec![5, 17]);
        assert!(mux.redo_indices.is_empty());
    }

    #[test]
    fn multiplex_reader_redo_wrong_payload_length_ignored() {
        // MSG_REDO with wrong payload length (not 4 bytes) should be ignored.
        // upstream: io.c:1516 reads exactly 4 bytes for val
        let mut stream = Vec::new();

        // MSG_REDO with 3 bytes (invalid — should be ignored)
        protocol::send_msg(&mut stream, protocol::MessageCode::Redo, &[1, 0, 0]).unwrap();

        // MSG_DATA follows
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"ok").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 2];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf, b"ok");

        // Invalid payload length should not accumulate any index
        assert!(mux.redo_indices.is_empty());
    }

    #[test]
    fn server_reader_take_redo_indices_plain_returns_empty() {
        let mut reader = ServerReader::new_plain(Cursor::new(vec![]));
        assert!(reader.take_redo_indices().is_empty());
    }

    #[test]
    fn server_reader_take_redo_indices_multiplex_accumulates() {
        // Build a multiplex stream with MSG_REDO + MSG_DATA
        let mut stream = Vec::new();
        let ndx: i32 = 13;
        protocol::send_msg(&mut stream, protocol::MessageCode::Redo, &ndx.to_le_bytes()).unwrap();
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"data").unwrap();

        let mut reader = ServerReader::new_plain(Cursor::new(stream))
            .activate_multiplex()
            .unwrap();

        // Read the data (which skips the MSG_REDO message)
        let mut buf = [0u8; 4];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");

        // take_redo_indices should return the accumulated index
        let indices = reader.take_redo_indices();
        assert_eq!(indices, vec![13]);

        // Second call returns empty (reset)
        assert!(reader.take_redo_indices().is_empty());
    }

    #[test]
    fn multiplex_reader_redo_and_no_send_interleaved() {
        // Verify MSG_REDO and MSG_NO_SEND accumulate independently
        let mut stream = Vec::new();

        // MSG_REDO with index 3
        let redo_ndx: i32 = 3;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::Redo,
            &redo_ndx.to_le_bytes(),
        )
        .unwrap();

        // MSG_NO_SEND with index 7
        let no_send_ndx: i32 = 7;
        protocol::send_msg(
            &mut stream,
            protocol::MessageCode::NoSend,
            &no_send_ndx.to_le_bytes(),
        )
        .unwrap();

        // MSG_DATA
        protocol::send_msg(&mut stream, protocol::MessageCode::Data, b"x").unwrap();

        let mut mux = MultiplexReader::new(Cursor::new(stream));
        let mut buf = [0u8; 1];
        let n = mux.read(&mut buf).unwrap();
        assert_eq!(n, 1);
        assert_eq!(&buf, b"x");

        // Each list accumulates independently
        assert_eq!(mux.redo_indices, vec![3]);
        assert_eq!(mux.no_send_indices, vec![7]);
    }
}
