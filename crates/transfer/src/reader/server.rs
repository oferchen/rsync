use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use compress::algorithm::CompressionAlgorithm;

use super::MultiplexReader;
use crate::compressed_reader::CompressedReader;

/// Server reader abstraction that switches between plain and multiplex modes.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_in()`.
/// We achieve the same by wrapping the reader and delegating based on mode.
#[allow(private_interfaces)]
#[allow(clippy::large_enum_variant)]
pub struct ServerReader<R: Read> {
    inner: ServerReaderInner<R>,
    /// Batch recorder held until multiplex activation, then applied to the
    /// `MultiplexReader`. Stored here because the reader starts in Plain mode
    /// and transitions to Multiplex later.
    pending_batch_recorder: Option<Arc<Mutex<dyn Write + Send>>>,
    /// Client-receiver I/O-timeout adoption state, applied to the
    /// `MultiplexReader` on multiplex activation. Held here because the reader
    /// starts in Plain mode. `(current --timeout secs, live-socket re-apply)`.
    /// upstream: io.c:1551-1561 read_a_msg() case MSG_IO_TIMEOUT.
    pending_io_timeout_adoption: Option<(Option<u32>, crate::handshake::IoTimeoutReapply)>,
    /// Client-sender `MSG_DELETED` render state, applied to the
    /// `MultiplexReader` on multiplex activation. `Some` only on a push where
    /// the remote receiver performs `--delete`. upstream: log.c:870-874.
    pending_deleted_render: Option<super::DeletedRender>,
}

#[allow(private_interfaces)]
#[allow(clippy::large_enum_variant)]
enum ServerReaderInner<R: Read> {
    /// Plain mode - read data directly without demultiplexing.
    Plain(R),
    /// Multiplex mode - extract data from MSG_DATA frames.
    Multiplex(MultiplexReader<R>),
    /// Compressed+Multiplex mode - decompress then demultiplex.
    Compressed(CompressedReader<MultiplexReader<R>>),
}

impl<R: Read> ServerReader<R> {
    /// Creates a new plain-mode reader.
    #[inline]
    pub fn new_plain(reader: R) -> Self {
        Self {
            inner: ServerReaderInner::Plain(reader),
            pending_batch_recorder: None,
            pending_io_timeout_adoption: None,
            pending_deleted_render: None,
        }
    }

    /// Enables client-side rendering of received `MSG_DELETED` frames.
    ///
    /// Applied to the `MultiplexReader` on `activate_multiplex`. Called only on
    /// the client-sender path of a push, where the remote receiver performs
    /// `--delete`; every other reader leaves it unset and drops the frames.
    ///
    /// upstream: log.c:870-874 `log_delete()` renders on the non-server side.
    pub(crate) fn enable_deleted_render(&mut self, render: super::DeletedRender) {
        self.pending_deleted_render = Some(render);
    }

    /// Registers client-receiver I/O-timeout adoption state.
    ///
    /// `current` is the client's own `--timeout` in seconds (`None`/`0` =
    /// infinite); `reapply` re-applies an adopted daemon timeout to the live
    /// socket. Applied to the `MultiplexReader` on `activate_multiplex`. Only
    /// the client receiver of a daemon transfer calls this.
    ///
    /// upstream: io.c:1551-1561 read_a_msg() case MSG_IO_TIMEOUT.
    pub fn enable_io_timeout_adoption(
        &mut self,
        current: Option<u32>,
        reapply: crate::handshake::IoTimeoutReapply,
    ) {
        self.pending_io_timeout_adoption = Some((current, reapply));
    }

    /// Registers a batch recorder for capturing compressed protocol data.
    ///
    /// The recorder is always attached to the `MultiplexReader` so it captures
    /// data at the same level as upstream rsync's `read_buf()` tee to `batch_fd`.
    /// When compression is active, this means capturing **pre-decompression**
    /// (compressed) wire bytes. The batch header records `do_compression: true`
    /// so replay knows to decompress the tokens.
    ///
    /// If neither multiplex nor compression is active yet, the recorder is
    /// stored until `activate_multiplex`.
    ///
    /// upstream: io.c:read_buf() tees data to batch_fd before decompression.
    pub fn set_batch_recorder(&mut self, recorder: Arc<Mutex<dyn Write + Send>>) {
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => {
                mux.batch_recorder = Some(recorder);
            }
            ServerReaderInner::Compressed(compressed) => {
                // upstream: io.c:read_buf() tees at the read_buf level, which
                // is the MultiplexReader's output (compressed data). Attach to
                // the inner MultiplexReader to capture compressed wire bytes.
                compressed.get_mut().batch_recorder = Some(recorder);
            }
            ServerReaderInner::Plain(_) => {
                self.pending_batch_recorder = Some(recorder);
            }
        }
    }

    /// Activates multiplex mode, wrapping the reader in a demultiplexer.
    ///
    /// If a batch recorder was previously registered via `set_batch_recorder`,
    /// it is attached to the new `MultiplexReader` automatically.
    pub fn activate_multiplex(self) -> io::Result<Self> {
        match self.inner {
            ServerReaderInner::Plain(reader) => {
                let mut mux = MultiplexReader::new(reader);
                if let Some(recorder) = self.pending_batch_recorder {
                    mux.batch_recorder = Some(recorder);
                }
                if let Some((current, reapply)) = self.pending_io_timeout_adoption {
                    mux.set_io_timeout_adoption(current, reapply);
                }
                if let Some(render) = self.pending_deleted_render {
                    mux.set_deleted_render(render);
                }
                Ok(Self {
                    inner: ServerReaderInner::Multiplex(mux),
                    pending_batch_recorder: None,
                    pending_io_timeout_adoption: None,
                    pending_deleted_render: None,
                })
            }
            ServerReaderInner::Multiplex(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "multiplex already active",
            )),
            ServerReaderInner::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
        }
    }

    /// Activates decompression on top of multiplex mode.
    ///
    /// This must be called AFTER `activate_multiplex()` to match upstream behavior.
    /// upstream: io.c:io_start_buffering_in() wraps the already-multiplexed stream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The reader is not in multiplex mode (decompression requires multiplex first)
    /// - Compression is already active
    /// - The compression algorithm is not supported in this build
    pub fn activate_compression(self, algorithm: CompressionAlgorithm) -> io::Result<Self> {
        match self.inner {
            ServerReaderInner::Multiplex(mux) => {
                // upstream: io.c:read_buf() tees data to batch_fd at the
                // read_buf level, which is BEFORE decompression. Keep the
                // batch recorder on MultiplexReader so it captures the
                // compressed wire bytes, matching upstream behavior. The
                // batch header records do_compression=true so replay knows
                // to decompress.
                let compressed = CompressedReader::new(mux, algorithm)?;
                Ok(Self {
                    inner: ServerReaderInner::Compressed(compressed),
                    pending_batch_recorder: None,
                    pending_io_timeout_adoption: None,
                    pending_deleted_render: None,
                })
            }
            ServerReaderInner::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "compression requires multiplex mode first",
            )),
            ServerReaderInner::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
        }
    }

    /// Returns true if multiplex mode is active.
    #[inline]
    pub const fn is_multiplexed(&self) -> bool {
        matches!(
            self.inner,
            ServerReaderInner::Multiplex(_) | ServerReaderInner::Compressed(_)
        )
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
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.try_borrow_exact(len),
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
    /// - `io.c:1542-1549`: receiver accumulates `io_error |= val` and forwards
    ///   via `send_msg_int(MSG_IO_ERROR, val)` when `am_receiver`.
    pub fn take_io_error(&mut self) -> i32 {
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.take_io_error(),
            ServerReaderInner::Compressed(compressed) => compressed.get_mut().take_io_error(),
            ServerReaderInner::Plain(_) => 0,
        }
    }

    /// Returns the count of `MSG_ERROR_XFER` frames received from the peer.
    ///
    /// A non-zero count is upstream's `got_xfer_error`: the peer reported a
    /// per-file transfer error (e.g. a failed output `mkstemp()` on the
    /// receiver), so the run must exit with `RERR_PARTIAL` (23). Plain-mode
    /// readers never carry multiplexed frames and return 0.
    ///
    /// # Upstream Reference
    ///
    /// - `log.c:311`: receipt of `FERROR_XFER` sets `got_xfer_error = 1`
    /// - `main.c:1630-1631`: `if (got_xfer_error) _exit(RERR_PARTIAL);`
    pub fn xfer_error_count(&mut self) -> u32 {
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.xfer_error_count(),
            ServerReaderInner::Compressed(compressed) => compressed.get_mut().xfer_error_count(),
            ServerReaderInner::Plain(_) => 0,
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
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.take_no_send_indices(),
            ServerReaderInner::Compressed(compressed) => {
                compressed.get_mut().take_no_send_indices()
            }
            ServerReaderInner::Plain(_) => Vec::new(),
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
    /// - `io.c:1535-1540`: `MSG_REDO` dispatches to `got_flist_entry_status(FES_REDO, val)`
    ///   on the generator side, pushing the NDX to `redo_list`.
    /// - `receiver.c:1093-1097`: receiver sends `send_msg_int(MSG_REDO, ndx)` on checksum failure.
    pub fn take_redo_indices(&mut self) -> Vec<i32> {
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.take_redo_indices(),
            ServerReaderInner::Compressed(compressed) => compressed.get_mut().take_redo_indices(),
            ServerReaderInner::Plain(_) => Vec::new(),
        }
    }

    /// Returns and drains accumulated `MSG_SUCCESS` file indices from the peer.
    ///
    /// The peer's generator/receiver sends `MSG_SUCCESS` with a 4-byte
    /// little-endian file index once a file has been fully received and
    /// committed. The sender drains these to run the deferred
    /// `--remove-source-files` unlink for confirmed files only.
    ///
    /// Returns an empty `Vec` for plain-mode readers.
    ///
    /// # Upstream Reference
    ///
    /// - `io.c:1623-1637`: `MSG_SUCCESS` received; `!am_generator` calls
    ///   `successful_send(val)`.
    /// - `sender.c:131-182`: `successful_send()` performs the deferred unlink.
    pub fn take_success_indices(&mut self) -> Vec<i32> {
        match &mut self.inner {
            ServerReaderInner::Multiplex(mux) => mux.take_success_indices(),
            ServerReaderInner::Compressed(compressed) => {
                compressed.get_mut().take_success_indices()
            }
            ServerReaderInner::Plain(_) => Vec::new(),
        }
    }
}

impl<R: Read> Read for ServerReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            ServerReaderInner::Plain(r) => r.read(buf),
            ServerReaderInner::Multiplex(r) => r.read(buf),
            ServerReaderInner::Compressed(r) => r.read(buf),
        }
    }
}
