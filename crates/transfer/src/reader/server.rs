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
    /// - `io.c:1521-1528`: receiver accumulates `io_error |= val` and forwards
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
    /// - `main.c:1635`: `if (got_xfer_error) _exit(RERR_PARTIAL);`
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
    /// - `io.c:1514-1519`: `MSG_REDO` dispatches to `got_flist_entry_status(FES_REDO, val)`
    ///   on the generator side, pushing the NDX to `redo_list`.
    /// - `receiver.c:970-974`: receiver sends `send_msg_int(MSG_REDO, ndx)` on checksum failure.
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

/// Async twin of [`ServerReader`], gated on the `tokio-transfer` feature.
///
/// This is the `.await`-driven counterpart to the blocking [`ServerReader`]
/// mode dispatch. It exists so the coupled receiver rung can assemble a genuine
/// async reader chain up to [`MultiplexReader::read_async`]:
///
/// ```text
/// AsyncTransport -> CountingReader (AsyncRead) -> tokio BufReader
///                -> AsyncServerReader -> MultiplexReader::read_async
/// ```
///
/// # Why a dedicated twin rather than generalising `ServerReader`
///
/// [`ServerReader`] is declared `ServerReader<R: Read>` and its `Compressed`
/// variant is `CompressedReader<MultiplexReader<R>>`, which requires
/// `R: Read`. An `AsyncRead` transport is not `Read`, so the same struct cannot
/// hold both. This twin therefore carries only the two async-reachable modes -
/// plain pass-through and multiplex demux - and drops no read logic: the demux
/// itself lives in the shared, reader-free [`MultiplexReader`] core, and this
/// type only selects the mode and awaits it.
///
/// The compression mode is intentionally absent: decompression runs through the
/// synchronous decoder crates (`compress::zlib::CountingZlibDecoder` and
/// friends), which have no `.await` variant, so a compressed async twin cannot
/// be built byte-identically at this rung. It is deferred to the receiver-
/// routing rung.
///
/// Additive and unwired: only the parity tests drive this type.
#[cfg(feature = "tokio-transfer")]
pub(crate) struct AsyncServerReader<R> {
    inner: AsyncServerReaderInner<R>,
}

/// Async mode dispatch for [`AsyncServerReader`].
///
/// Mirrors the plain and multiplex arms of the blocking
/// [`ServerReaderInner`]; the compressed arm is deferred (see the type docs).
#[cfg(feature = "tokio-transfer")]
enum AsyncServerReaderInner<R> {
    /// Plain mode - await the inner transport directly, no demux.
    Plain(R),
    /// Multiplex mode - await MSG_DATA frames via the shared demux core.
    ///
    /// Wrapped in a [`MuxState`] so the [`tokio::io::AsyncRead`] impl can hold an
    /// in-flight demux future across a `Poll::Pending` without losing frame
    /// bytes already pulled off the transport (see the `poll_read` docs). The
    /// inherent [`AsyncServerReader::read_async`] path, which `.await`s to
    /// completion in one call, only ever sees the [`MuxState::Idle`] reader.
    Multiplex(MuxState<R>),
}

/// Read state for the multiplex arm of [`AsyncServerReader`].
///
/// The blocking [`ServerReader`] holds a bare [`MultiplexReader`] because its
/// `Read::read` runs to completion in one call. A `poll_read`, by contrast, can
/// return [`Poll::Pending`](std::task::Poll::Pending) mid-frame, so the
/// in-flight demux future - which owns the reader plus any bytes already pulled
/// off the transport for the current frame header/payload - must persist across
/// polls. Dropping and recreating it would silently discard those consumed bytes
/// and corrupt the stream. This state enum keeps that future alive between polls
/// and hands the reader back once the frame completes.
///
/// No demux logic is duplicated: the `Reading` future simply awaits the shared
/// [`MultiplexReader::read_async`], the exact `.await` core the inherent
/// [`AsyncServerReader::read_async`] uses.
#[cfg(feature = "tokio-transfer")]
enum MuxState<R> {
    /// No read in flight - the demultiplexer is available to start one.
    Idle(MultiplexReader<R>),
    /// A read is in flight. The boxed future owns the [`MultiplexReader`] plus a
    /// staging buffer and resolves to both (so the reader returns to `Idle`)
    /// alongside the demuxed byte count. Boxing erases the future type so the
    /// state can be stored in the struct across polls.
    Reading(MuxReadFuture<R>),
    /// Transient placeholder held only while ownership moves between `Idle` and
    /// `Reading`. Never observed by a subsequent poll: every transition that
    /// takes the state restores a real variant before `poll_read` returns.
    Transitioning,
}

/// Boxed in-flight multiplex read future.
///
/// Resolves to the reclaimed [`MultiplexReader`], its staging buffer (returned
/// so the allocation can be reused), and the demuxed byte count (or error). See
/// [`MuxState::Reading`].
#[cfg(feature = "tokio-transfer")]
type MuxReadFuture<R> = std::pin::Pin<
    Box<dyn std::future::Future<Output = (MultiplexReader<R>, Vec<u8>, io::Result<usize>)> + Send>,
>;

#[cfg(feature = "tokio-transfer")]
impl<R: tokio::io::AsyncRead + Unpin> AsyncServerReader<R> {
    /// Creates a new plain-mode async reader.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new_plain(reader: R) -> Self {
        Self {
            inner: AsyncServerReaderInner::Plain(reader),
        }
    }

    /// Activates multiplex mode, wrapping the reader in the demultiplexer.
    ///
    /// Mirrors [`ServerReader::activate_multiplex`]: a plain reader becomes a
    /// [`MultiplexReader`]; calling it twice is an error.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn activate_multiplex(self) -> io::Result<Self> {
        match self.inner {
            AsyncServerReaderInner::Plain(reader) => Ok(Self {
                inner: AsyncServerReaderInner::Multiplex(MuxState::Idle(MultiplexReader::new(
                    reader,
                ))),
            }),
            AsyncServerReaderInner::Multiplex(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "multiplex already active",
            )),
        }
    }

    /// Returns true if multiplex mode is active.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_multiplexed(&self) -> bool {
        matches!(self.inner, AsyncServerReaderInner::Multiplex(_))
    }

    /// Reads demultiplexed (or plain) data into `buf`, awaiting the transport.
    ///
    /// Byte-for-byte equivalent to [`ServerReader::read`] restricted to the
    /// plain and multiplex modes: plain mode delegates to the inner async
    /// transport's `read`, multiplex mode delegates to the shared
    /// [`MultiplexReader::read_async`]. No demux logic is duplicated here.
    ///
    /// This inherent path `.await`s to completion in a single call, so it only
    /// ever observes the [`MuxState::Idle`] reader; a `Reading`/`Transitioning`
    /// state can arise only inside a [`tokio::io::AsyncRead::poll_read`] cycle,
    /// which never overlaps a `read_async` call.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn read_async(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use tokio::io::AsyncReadExt;
        match &mut self.inner {
            AsyncServerReaderInner::Plain(r) => r.read(buf).await,
            AsyncServerReaderInner::Multiplex(MuxState::Idle(r)) => r.read_async(buf).await,
            AsyncServerReaderInner::Multiplex(_) => Err(io::Error::other(
                "read_async called while a poll_read demux future was in flight",
            )),
        }
    }
}

/// Real [`tokio::io::AsyncRead`] for the async server reader, gated on the
/// `tokio-transfer` feature.
///
/// This is the poll-based counterpart to the inherent
/// [`AsyncServerReader::read_async`]: it yields the exact same demuxed byte
/// stream (plain pass-through, or MSG_DATA payloads via the shared
/// [`MultiplexReader`] demux core), so the follow-up async read leaves
/// (flist / attrs / stats / NDX) can all read from one uniform
/// `R: AsyncRead` source rather than reaching for the inherent method.
///
/// # Sharing the demux core - no duplication
///
/// The multiplex arm does not reimplement any framing, dispatch, buffering, or
/// batch-tee logic. It drives the same [`MultiplexReader::read_async`] `.await`
/// core the inherent `read_async` uses, which itself runs the shared,
/// reader-free [`dispatch_message_with`](MultiplexReader) core. MSG_* side
/// effects (stdout/stderr print + flush) therefore fire in the identical order
/// relative to delivered data as the sync [`ServerReader`] and the inherent
/// async path.
///
/// # `Poll::Pending` safety (the correctness crux)
///
/// A `poll_read` can suspend in the middle of a frame - after the demux future
/// has already consumed part of a header or payload off the transport. Those
/// bytes live inside the future's state, so the future must survive across
/// polls. Recreating it on the next poll would re-read from the middle of the
/// stream and corrupt it. The [`MuxState`] holds the in-flight future between
/// polls (owning the reader) and reclaims the reader into `Idle` only once the
/// frame completes, so a suspended read resumes exactly where it left off.
///
/// Because the future owns the reader, this impl requires `R: Send + 'static`,
/// which the intended receiver transport stack (`tokio::io::BufReader` over the
/// counting reader over the async transport) satisfies.
///
/// Additive and unwired: only the parity tests drive this impl.
#[cfg(feature = "tokio-transfer")]
impl<R> tokio::io::AsyncRead for AsyncServerReader<R>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            // Plain mode: delegate straight to the inner transport. Byte-for-byte
            // the `read_async` plain arm, just expressed in poll form.
            AsyncServerReaderInner::Plain(r) => std::pin::Pin::new(r).poll_read(cx, buf),
            AsyncServerReaderInner::Multiplex(state) => poll_read_multiplex(state, cx, buf),
        }
    }
}

/// Drives one multiplex demux step for [`AsyncServerReader::poll_read`].
///
/// Starts (or resumes) the in-flight [`MultiplexReader::read_async`] future held
/// by `state`, copying its demuxed bytes into `buf` on completion. See the
/// `AsyncRead` impl docs for the `Poll::Pending` safety argument.
#[cfg(feature = "tokio-transfer")]
fn poll_read_multiplex<R>(
    state: &mut MuxState<R>,
    cx: &mut std::task::Context<'_>,
    buf: &mut tokio::io::ReadBuf<'_>,
) -> std::task::Poll<io::Result<()>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use std::task::Poll;

    // A zero-capacity target reads nothing and consumes no wire, matching the
    // sync `Read::read` contract for an empty buffer.
    if buf.remaining() == 0 {
        return Poll::Ready(Ok(()));
    }

    // Begin a read if idle: move the reader into a future that owns it plus a
    // staging buffer sized to the caller's free space, so the demux fills at
    // most `buf.remaining()` bytes - the identical per-call chunking the sync
    // and inherent-async drivers produce for the same buffer size.
    let mut fut = match std::mem::replace(state, MuxState::Transitioning) {
        MuxState::Idle(mut reader) => {
            let cap = buf.remaining();
            Box::pin(async move {
                let mut scratch = vec![0u8; cap];
                let result = reader.read_async(&mut scratch).await;
                (reader, scratch, result)
            }) as MuxReadFuture<R>
        }
        MuxState::Reading(fut) => fut,
        MuxState::Transitioning => {
            // Only reachable if a prior poll panicked mid-transition. The
            // reader is gone, so the stream cannot continue safely.
            return Poll::Ready(Err(io::Error::other(
                "multiplex reader lost during a suspended poll_read",
            )));
        }
    };

    match fut.as_mut().poll(cx) {
        Poll::Pending => {
            *state = MuxState::Reading(fut);
            Poll::Pending
        }
        Poll::Ready((reader, scratch, result)) => {
            *state = MuxState::Idle(reader);
            match result {
                Ok(n) => {
                    buf.put_slice(&scratch[..n]);
                    Poll::Ready(Ok(()))
                }
                Err(err) => Poll::Ready(Err(err)),
            }
        }
    }
}
