//! Sync/async bridge primitives for embedded SSH streams.
//!
//! This module provides the inverse of [`crate::channel_adapter`]: instead of
//! wrapping a sync byte stream as `AsyncRead`/`AsyncWrite`, it wraps an
//! `AsyncRead + AsyncWrite` stream (specifically `russh`'s channel I/O) as
//! `std::io::Read`/`std::io::Write` so the existing synchronous multiplex and
//! transfer code can drive russh-backed channels without being ported to
//! async.
//!
//! # Overview
//!
//! - [`SyncAsyncBridge`] wraps any
//!   `AsyncRead + AsyncWrite + Unpin + Send + 'static` stream and exposes
//!   `std::io::Read + std::io::Write` by driving an internal current-thread
//!   tokio runtime via [`tokio::runtime::Runtime::block_on`] on every call.
//! - [`into_sync_halves`] takes ownership of a [`russh::Channel`] and returns
//!   (`SyncReader`, `SyncWriter`) halves backed by a background tokio task
//!   that pumps channel data into bounded `std::sync::mpsc` /
//!   `tokio::sync::mpsc` queues. This is the bridge variant used by
//!   `connect_and_exec`; it is re-exposed
//!   here so other call-sites can construct the same wiring around channels
//!   they own.
//!
//! # Invariants
//!
//! - Each `read` consumes one chunk from the async stream into a caller
//!   buffer; oversized chunks are buffered internally and drained on the next
//!   read.
//! - `write` always consumes the entire input slice via one `write_all` on
//!   the underlying async writer; partial writes are never reported to
//!   callers.
//! - A closed/EOF async stream surfaces as `Ok(0)` from `read`, matching the
//!   `std::io::Read` contract.
//! - A closed downstream receiver surfaces as
//!   `std::io::ErrorKind::BrokenPipe` from `write`.
//!
//! # Backpressure
//!
//! The channel-based [`into_sync_halves`] variant uses bounded
//! `mpsc::channel(64)` queues in both directions, so a stalled async reader
//! will eventually block sync writers and vice versa. This matches how the
//! existing system-SSH transport behaves under load.
//!
//! # Runtime liveness
//!
//! The channel-based variant is driven by a background tokio task spawned in
//! [`into_sync_halves`]. That task owns the inbound `data_tx` sender; the sync
//! `SyncReader` blocks on the matching receiver. Liveness therefore hinges
//! on the pump task:
//!
//! - **Task exits / panics / runtime is dropped**: the captured `data_tx` is
//!   dropped along with the task, so the blocking `recv` observes
//!   all-senders-dropped and the `SyncReader` surfaces a clean EOF
//!   (`Ok(0)`) rather than hanging. The outbound `SyncWriter` symmetrically
//!   observes a closed receiver and surfaces `io::ErrorKind::BrokenPipe`.
//! - **Task wedged but alive** (deadlocked while still holding `data_tx`):
//!   the sender is never dropped, so an unbounded blocking `recv` would hang
//!   the sync side forever. `SyncReader::read_with_timeout` bounds this
//!   residual case: a wedged runtime surfaces as an
//!   `io::ErrorKind::TimedOut` transport error instead of a silent hang.
//!   The infallible `io::Read` impl keeps the unbounded happy-path
//!   behaviour; callers that need a liveness guarantee opt into the bounded
//!   helper.

use std::io;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc as tokio_mpsc;

/// Default capacity for the in-process queues used by [`into_sync_halves`].
///
/// Matches the queue depth used by `super::connect::connect_and_exec` so
/// the two paths behave identically under load.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// Wraps an `AsyncRead + AsyncWrite` stream as a synchronous
/// `Read + Write` source by driving an internal current-thread tokio
/// runtime via `block_on` on every call.
///
/// # When to use
///
/// Prefer this adapter for one-shot situations where the bridged stream is
/// owned exclusively by the bridge (i.e., no other tokio tasks need to drive
/// it concurrently). For russh channels where data arrives via
/// `Channel::wait()` and writes go through `Handle::data()`, use
/// [`into_sync_halves`] instead - it spawns a dedicated pump task on the
/// caller-supplied runtime and is friendlier to concurrent use.
///
/// # Lifetime
///
/// The bridge owns both the stream and the runtime; dropping it shuts the
/// runtime down and closes the stream.
pub struct SyncAsyncBridge<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    runtime: Runtime,
    stream: Pin<Box<S>>,
}

impl<S> SyncAsyncBridge<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Wraps `stream` in a sync facade backed by a fresh current-thread
    /// tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] if the tokio runtime cannot be
    /// constructed (typically a resource-exhaustion failure).
    pub fn new(stream: S) -> io::Result<Self> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| io::Error::other(format!("async runtime: {e}")))?;
        Ok(Self {
            runtime,
            stream: Box::pin(stream),
        })
    }

    /// Consumes the bridge and returns the inner stream.
    ///
    /// The internal runtime is dropped before the stream is returned, so
    /// the stream must be used from another runtime after this call.
    pub fn into_inner(self) -> S {
        let Self { runtime, stream } = self;
        drop(runtime);
        *Pin::into_inner(stream)
    }
}

impl<S> io::Read for SyncAsyncBridge<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut stream = self.stream.as_mut();
        self.runtime.block_on(async move { stream.read(buf).await })
    }
}

impl<S> io::Write for SyncAsyncBridge<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut stream = self.stream.as_mut();
        self.runtime.block_on(async move {
            stream.write_all(buf).await?;
            Ok(buf.len())
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut stream = self.stream.as_mut();
        self.runtime.block_on(async move { stream.flush().await })
    }
}

/// Synchronous reader half returned by [`into_sync_halves`].
///
/// Drains byte chunks delivered by a background pump task through a bounded
/// `std::sync::mpsc::sync_channel`. Partial reads from oversized chunks are
/// buffered internally. A closed sender (channel EOF or pump exit) yields
/// `Ok(0)` to signal `std::io::Read` EOF.
pub struct SyncReader {
    rx: std_mpsc::Receiver<Vec<u8>>,
    buffered: Vec<u8>,
    offset: usize,
}

impl SyncReader {
    fn new(rx: std_mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffered: Vec::new(),
            offset: 0,
        }
    }

    /// Reads the next chunk of channel data, bounded by `budget`.
    ///
    /// This is the liveness-guarded counterpart to the infallible
    /// [`io::Read`] impl. The plain `read` blocks on `recv` until the pump
    /// task delivers data or drops its sender; that is correct when the pump
    /// task is guaranteed to make progress or exit. When the async runtime
    /// can wedge while still holding the inbound sender (a deadlocked-but-
    /// alive pump task), `recv` would block forever. This method instead
    /// waits at most `budget` and maps a wedged runtime to a clean
    /// [`io::ErrorKind::TimedOut`] transport error.
    ///
    /// # Returns
    ///
    /// - `Ok(n)` with `n` bytes written into `buf` when a chunk is available
    ///   (draining any internally buffered remainder first, exactly like
    ///   [`io::Read::read`]).
    /// - `Ok(0)` when the pump task has dropped its sender (channel EOF),
    ///   which also covers a panicked task or a dropped runtime.
    /// - `Err(io::ErrorKind::TimedOut)` when no chunk and no disconnect are
    ///   observed within `budget`, signalling a wedged runtime rather than a
    ///   silent hang.
    ///
    /// A buffered remainder from a previous oversized chunk is served without
    /// touching the channel, so `budget` only bounds the wait for fresh data.
    pub fn read_with_timeout(&mut self, buf: &mut [u8], budget: Duration) -> io::Result<usize> {
        if self.offset >= self.buffered.len() {
            match self.rx.recv_timeout(budget) {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        return Ok(0);
                    }
                    self.buffered = chunk;
                    self.offset = 0;
                }
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
                Err(std_mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("sync bridge read stalled after {budget:?}"),
                    ));
                }
            }
        }
        let available = &self.buffered[self.offset..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.offset += n;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        Ok(n)
    }
}

impl io::Read for SyncReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.offset >= self.buffered.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        return Ok(0);
                    }
                    self.buffered = chunk;
                    self.offset = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let remaining = &self.buffered[self.offset..];
        let n = remaining.len().min(buf.len());
        buf[..n].copy_from_slice(&remaining[..n]);
        self.offset += n;
        if self.offset >= self.buffered.len() {
            self.buffered.clear();
            self.offset = 0;
        }
        Ok(n)
    }
}

/// Internal write buffer capacity for `SyncWriter`.
///
/// Small writes (4-byte multiplex headers, NDX values, etc.) are coalesced
/// into this staging buffer instead of allocating a `Vec` per write and
/// sending it over the channel. The buffer is flushed when full or when
/// `flush()` is called. Sized to match upstream rsync's `IO_BUFFER_SIZE`
/// (32 KB) - large enough to batch a full multiplex frame's header + data.
const SYNC_WRITER_BUF_SIZE: usize = 32 * 1024;

/// Synchronous writer half returned by [`into_sync_halves`].
///
/// Coalesces small writes into an internal staging buffer to reduce
/// per-write heap allocations and channel sends. Data is flushed to the
/// channel when the buffer fills or when `flush()` is called explicitly.
/// Each channel message still uses `blocking_send` which honors
/// backpressure: when the channel is full the calling thread blocks until
/// the async side drains capacity. A closed receiver surfaces as
/// [`io::ErrorKind::BrokenPipe`].
pub struct SyncWriter {
    tx: Option<tokio_mpsc::Sender<Vec<u8>>>,
    /// Staging buffer for small writes. Avoids allocating a `Vec<u8>` per
    /// `write()` call and amortizes the channel-send overhead across many
    /// small writes (4-byte headers, varint NDX values, etc.).
    buf: Vec<u8>,
}

impl SyncWriter {
    fn new(tx: tokio_mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tx: Some(tx),
            buf: Vec::with_capacity(SYNC_WRITER_BUF_SIZE),
        }
    }

    /// Sends the staging buffer contents through the channel and clears it.
    fn flush_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "writer shut down"))?;
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(SYNC_WRITER_BUF_SIZE));
        tx.blocking_send(chunk)
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
        Ok(())
    }
}

impl io::Write for SyncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Large writes bypass the staging buffer to avoid an extra memcpy.
        if buf.len() >= SYNC_WRITER_BUF_SIZE {
            self.flush_buf()?;
            let tx = self
                .tx
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "writer shut down"))?;
            tx.blocking_send(buf.to_vec())
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
            return Ok(buf.len());
        }

        // Flush if appending would exceed capacity.
        if self.buf.len() + buf.len() > SYNC_WRITER_BUF_SIZE {
            self.flush_buf()?;
        }

        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buf()
    }
}

impl Drop for SyncWriter {
    fn drop(&mut self) {
        // Best-effort flush of any remaining buffered data before closing.
        let _ = self.flush_buf();
        // Dropping the sender signals EOF to the outbound pump, which then
        // closes the async write half cleanly.
        self.tx = None;
    }
}

/// Splits a [`russh::Channel`] into synchronous `Read`/`Write` halves.
///
/// Spawns a background task on the current tokio runtime to pump channel
/// data into bounded queues that the returned halves drain synchronously.
/// This is the channel-based bridge variant used by
/// `super::connect::connect_and_exec`; it is exposed here for callers who
/// already own a channel and need the same wiring (for example, after
/// opening a session channel for a custom subsystem).
///
/// # Runtime requirement
///
/// Must be called from within a tokio runtime context (the spawn relies on
/// `tokio::spawn`). The returned halves are `Send` and can be moved to a
/// blocking worker thread (`tokio::task::spawn_blocking` or `std::thread`).
///
/// # Capacity
///
/// Both directions use bounded `mpsc` queues of [`DEFAULT_CHANNEL_CAPACITY`]
/// chunks. A stalled async reader will eventually block sync writers and
/// vice versa.
///
/// # EOF semantics
///
/// - The reader yields `Ok(0)` when the channel emits
///   [`russh::ChannelMsg::Eof`] or closes.
/// - Dropping the writer signals EOF to the pump, which stops issuing
///   further `data()` calls on the channel.
#[must_use]
pub fn into_sync_halves(channel: russh::Channel<russh::client::Msg>) -> (SyncReader, SyncWriter) {
    into_sync_halves_with_capacity(channel, DEFAULT_CHANNEL_CAPACITY)
}

/// Variant of [`into_sync_halves`] with an explicit queue capacity.
///
/// Useful for tests that need to assert backpressure semantics by sizing the
/// queue down to a single message.
#[must_use]
pub fn into_sync_halves_with_capacity(
    channel: russh::Channel<russh::client::Msg>,
    capacity: usize,
) -> (SyncReader, SyncWriter) {
    let cap = capacity.max(1);
    let (data_tx, data_rx) = std_mpsc::sync_channel::<Vec<u8>>(cap);
    let (write_tx, mut write_rx) = tokio_mpsc::channel::<Vec<u8>>(cap);

    let mut writer = Box::pin(channel.make_writer());

    tokio::spawn(async move {
        let mut channel = channel;
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(russh::ChannelMsg::Data { data }) => {
                            if data_tx.send(data.to_vec()).is_err() {
                                break;
                            }
                        }
                        Some(russh::ChannelMsg::Eof) | None => break,
                        _ => continue,
                    }
                }
                outbound = write_rx.recv() => {
                    match outbound {
                        Some(chunk) => {
                            if writer.write_all(&chunk).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    (SyncReader::new(data_rx), SyncWriter::new(write_tx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    /// `SyncAsyncBridge` round-trips bytes by driving an async echo task on
    /// the opposite half of an in-memory duplex.
    #[test]
    fn sync_async_bridge_round_trip() {
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (client, mut server) = duplex(64);

        // Spawn an echo task on the multi-thread runtime so the bridge's
        // own current-thread runtime is not blocked by it.
        rt.spawn(async move {
            let mut buf = vec![0u8; 64];
            loop {
                let n = match tokio::io::AsyncReadExt::read(&mut server, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if tokio::io::AsyncWriteExt::write_all(&mut server, &buf[..n])
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Run the bridge on a separate OS thread because constructing a
        // current-thread runtime inside an existing runtime is forbidden.
        let handle = std::thread::spawn(move || {
            let mut bridge = SyncAsyncBridge::new(client).unwrap();
            bridge.write_all(b"ping").unwrap();
            bridge.flush().unwrap();
            let mut buf = [0u8; 4];
            bridge.read_exact(&mut buf).unwrap();
            buf
        });

        assert_eq!(&handle.join().unwrap(), b"ping");
    }

    /// EOF on the underlying async stream surfaces as `Ok(0)` from the
    /// sync `Read` impl.
    #[test]
    fn sync_async_bridge_eof_on_close() {
        let (client, server) = duplex(8);
        // Drop the server half so the duplex reports EOF when the bridge
        // reads from the client side.
        drop(server);

        let mut bridge = SyncAsyncBridge::new(client).unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(bridge.read(&mut buf).unwrap(), 0);
    }

    /// `SyncReader` drains oversized chunks across multiple reads.
    #[test]
    fn sync_reader_drains_chunks_across_reads() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(2);
        tx.send(b"hello world".to_vec()).unwrap();
        drop(tx);

        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"hello");

        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b" world");
    }

    /// `SyncReader` reports EOF when the upstream sender is dropped.
    #[test]
    fn sync_reader_closed_channel_is_eof() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(1);
        drop(tx);
        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 4];
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    /// A dropped pump runtime (all senders gone) surfaces as a clean EOF
    /// through the bounded helper too, not a `TimedOut` error and not a hang.
    /// This models a panicked or dropped async runtime: the captured sender
    /// dies with the task, so `recv_timeout` observes `Disconnected`.
    #[test]
    fn read_with_timeout_dropped_runtime_is_eof() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(1);
        // Simulate the pump task dropping its sender on runtime death.
        drop(tx);
        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 8];
        let start = Instant::now();
        let n = reader
            .read_with_timeout(&mut buf, Duration::from_secs(5))
            .expect("dropped sender must surface as EOF, not error");
        assert_eq!(n, 0, "dropped sender is EOF");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "EOF must return promptly, not wait out the budget",
        );
    }

    /// A wedged-but-alive runtime (sender held open, no data delivered)
    /// surfaces as a bounded `TimedOut` transport error instead of hanging
    /// the sync side forever.
    #[test]
    fn read_with_timeout_wedged_runtime_times_out() {
        // Keep `tx` alive for the whole test so the receiver never observes
        // a disconnect - this is the simulated deadlock.
        let (_tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(1);
        let mut reader = SyncReader::new(rx);
        let mut buf = [0u8; 8];

        let budget = Duration::from_millis(50);
        let start = Instant::now();
        let err = reader
            .read_with_timeout(&mut buf, budget)
            .expect_err("wedged runtime must surface a timeout error");
        let elapsed = start.elapsed();

        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed >= budget,
            "returned before budget elapsed: {elapsed:?} < {budget:?}",
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "took longer than the upper bound: {elapsed:?}",
        );
    }

    /// The bounded helper returns available data promptly and drains
    /// oversized chunks across calls, matching the infallible `read` path.
    #[test]
    fn read_with_timeout_drains_buffered_chunk() {
        let (tx, rx) = std_mpsc::sync_channel::<Vec<u8>>(2);
        tx.send(b"hello world".to_vec()).unwrap();
        drop(tx);
        let mut reader = SyncReader::new(rx);

        let mut buf = [0u8; 5];
        let n = reader
            .read_with_timeout(&mut buf, Duration::from_secs(5))
            .unwrap();
        assert_eq!(&buf[..n], b"hello");

        // Remainder is served from the internal buffer without touching the
        // channel, so a zero budget still returns it.
        let mut rest = [0u8; 6];
        let n = reader.read_with_timeout(&mut rest, Duration::ZERO).unwrap();
        assert_eq!(&rest[..n], b" world");
    }

    /// `SyncWriter` returns `BrokenPipe` once the downstream receiver is
    /// gone. Small writes are buffered, so the error surfaces on `flush()`.
    #[test]
    fn sync_writer_to_closed_channel_is_broken_pipe() {
        // Build the channel outside a runtime - blocking_send must not be
        // called from within one.
        let (tx, rx) = tokio_mpsc::channel::<Vec<u8>>(1);
        drop(rx);
        let mut writer = SyncWriter::new(tx);
        // Small write succeeds because it stays in the staging buffer.
        writer.write_all(b"x").unwrap();
        // Flush forces the channel send, which detects the closed receiver.
        let err = writer.flush().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    /// Bounded `SyncWriter` blocks when the receiver is full and unblocks
    /// once a slot is drained by an async consumer.
    #[test]
    fn sync_writer_backpressure_blocks_until_drained() {
        let rt = Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        let (tx, mut rx) = tokio_mpsc::channel::<Vec<u8>>(1);

        // Pre-fill the single slot so the next write blocks.
        rt.block_on(async {
            tx.send(b"first".to_vec()).await.unwrap();
        });

        let mut writer = SyncWriter::new(tx);
        let (signal_tx, signal_rx) = std_mpsc::channel::<()>();

        // Write data larger than the staging buffer to force a flush,
        // which will block because the channel is full.
        let large_payload = vec![0x42u8; SYNC_WRITER_BUF_SIZE + 1];

        let handle = std::thread::spawn(move || {
            // Notify that the writer is about to block.
            signal_tx.send(()).unwrap();
            writer.write_all(&large_payload).unwrap();
        });

        // Wait for the writer thread to be ready and prove the second write
        // is still blocked by draining only the first message.
        signal_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let first = rt.block_on(rx.recv()).unwrap();
        assert_eq!(first, b"first");

        // Once a slot is free the large write should complete promptly.
        let second = rt.block_on(rx.recv()).unwrap();
        assert_eq!(second.len(), SYNC_WRITER_BUF_SIZE + 1);
        handle.join().unwrap();
    }

    /// Dropping the `SyncWriter` closes the underlying sender so the async
    /// consumer observes channel EOF.
    #[test]
    fn dropping_sync_writer_closes_channel() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let (tx, mut rx) = tokio_mpsc::channel::<Vec<u8>>(4);
        let writer = SyncWriter::new(tx);
        drop(writer);
        let received = rt.block_on(rx.recv());
        assert!(received.is_none(), "sender drop should close the channel");
    }

    // Wire-byte parity: SyncAsyncBridge (async-native) vs channel-pump
    // (spawn_blocking) bridging strategies (RUSSH-12).
    //
    // Both strategies convert an async duplex stream into sync Read + Write.
    // These tests verify that the bytes observed on the sync side are
    // identical regardless of which bridge is used, for payloads ranging
    // from trivial to multi-chunk.

    /// Sends `payload` through the `SyncAsyncBridge` path and returns the
    /// bytes the sync reader observes after a round-trip through an echo
    /// server on the async side.
    ///
    /// # Bridge path: `SyncAsyncBridge` (async-native)
    ///
    /// Uses the direct `block_on`-per-call strategy. The bridge owns a
    /// current-thread runtime and drives the async I/O inline on every
    /// `read()` / `write()` call.
    fn round_trip_via_bridge(payload: &[u8]) -> Vec<u8> {
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (client, mut server) = duplex(64 * 1024);

        // Async echo server: reads everything, writes it back verbatim.
        rt.spawn(async move {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                let n = match server.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if server.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            let _ = server.shutdown().await;
        });

        let payload_owned = payload.to_vec();

        // Run on a separate OS thread because SyncAsyncBridge constructs its
        // own current-thread runtime internally, which cannot nest inside
        // an existing runtime.
        let handle = std::thread::spawn(move || {
            let mut bridge = SyncAsyncBridge::new(client).unwrap();

            if payload_owned.is_empty() {
                return Vec::new();
            }

            // Interleave writes and reads in chunks to prevent deadlock:
            // the echo server writes back inline, so both duplex buffer
            // directions fill if we do a single large write_all before
            // reading. Each chunk must be smaller than the duplex buffer
            // (64 KiB) so the echo round-trip fits without blocking.
            let chunk_size = 32 * 1024;
            let mut received = Vec::with_capacity(payload_owned.len());
            let mut offset = 0;

            while offset < payload_owned.len() {
                let end = (offset + chunk_size).min(payload_owned.len());
                bridge.write_all(&payload_owned[offset..end]).unwrap();
                bridge.flush().unwrap();

                let mut buf = vec![0u8; end - offset];
                bridge.read_exact(&mut buf).unwrap();
                received.extend_from_slice(&buf);
                offset = end;
            }

            received
        });

        handle.join().unwrap()
    }

    /// Sends `payload` through the channel-pump bridge path and returns
    /// the bytes the sync reader observes after a round-trip echo.
    ///
    /// # Bridge path: channel-pump (`SyncReader` / `SyncWriter`)
    ///
    /// Uses bounded `std::sync::mpsc` + `tokio::sync::mpsc` queues with a
    /// background pump task, matching the spawn_blocking-compatible strategy
    /// used by `connect_and_exec` and the async SSH transport.
    fn round_trip_via_channel_pump(payload: &[u8]) -> Vec<u8> {
        let rt = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let (client, mut server) = duplex(64 * 1024);

        // Async echo server on the duplex's server half.
        rt.spawn(async move {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                let n = match server.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if server.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            let _ = server.shutdown().await;
        });

        // Build the channel-pump bridge manually (same wiring as
        // into_sync_halves, but over a duplex instead of a russh Channel).
        let cap = DEFAULT_CHANNEL_CAPACITY;
        let (data_tx, data_rx) = std_mpsc::sync_channel::<Vec<u8>>(cap);
        let (write_tx, mut write_rx) = tokio_mpsc::channel::<Vec<u8>>(cap);

        let (mut async_reader, mut async_writer) = tokio::io::split(client);

        // Inbound pump: async reader -> sync reader channel.
        rt.spawn(async move {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                let n = match async_reader.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if data_tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        // Outbound pump: sync writer channel -> async writer.
        rt.spawn(async move {
            while let Some(chunk) = write_rx.recv().await {
                if async_writer.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = async_writer.shutdown().await;
        });

        let sync_reader = SyncReader::new(data_rx);
        let sync_writer = SyncWriter::new(write_tx);

        let payload_owned = payload.to_vec();
        let expected_len = payload_owned.len();

        let handle = std::thread::spawn(move || {
            let mut writer = sync_writer;
            let mut reader = sync_reader;
            writer.write_all(&payload_owned).unwrap();
            writer.flush().unwrap();
            // Drop writer to signal EOF to the outbound pump, which shuts
            // down the async write half so the echo server sees EOF.
            drop(writer);

            let mut received = Vec::with_capacity(expected_len);
            reader.read_to_end(&mut received).unwrap();
            received
        });

        handle.join().unwrap()
    }

    /// Both bridge strategies must produce identical bytes for a small
    /// single-chunk payload.
    #[test]
    fn wire_byte_parity_small_payload() {
        let payload = b"hello rsync wire parity test";
        let bridge_bytes = round_trip_via_bridge(payload);
        let pump_bytes = round_trip_via_channel_pump(payload);

        assert_eq!(
            bridge_bytes, pump_bytes,
            "bridge and channel-pump must produce identical bytes for small payload"
        );
        assert_eq!(bridge_bytes, payload.as_slice());
    }

    /// Both bridge strategies must produce identical bytes for a multi-chunk
    /// payload that exceeds the duplex buffer and exercises chunked I/O.
    #[test]
    fn wire_byte_parity_multi_chunk_payload() {
        // 256 KiB payload - exceeds the 64 KiB duplex buffer and forces
        // multiple read/write cycles through both bridge strategies.
        let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
        let bridge_bytes = round_trip_via_bridge(&payload);
        let pump_bytes = round_trip_via_channel_pump(&payload);

        assert_eq!(
            bridge_bytes.len(),
            pump_bytes.len(),
            "both paths must return the same number of bytes"
        );
        assert_eq!(
            bridge_bytes, pump_bytes,
            "bridge and channel-pump must produce identical bytes for multi-chunk payload"
        );
        assert_eq!(bridge_bytes, payload);
    }

    /// Both bridge strategies must produce identical bytes for a payload
    /// that contains the full byte range (0x00-0xFF) including null bytes,
    /// high-bit bytes, and the newline/CR sequences that naive text-mode
    /// bridging would corrupt.
    #[test]
    fn wire_byte_parity_binary_payload() {
        let mut payload = Vec::with_capacity(512);
        // Two full rounds of 0x00..0xFF to exercise any off-by-one in
        // chunk boundary handling when the same byte value straddles a
        // duplex buffer boundary.
        for _ in 0..2 {
            for b in 0..=255u8 {
                payload.push(b);
            }
        }
        let bridge_bytes = round_trip_via_bridge(&payload);
        let pump_bytes = round_trip_via_channel_pump(&payload);

        assert_eq!(
            bridge_bytes, pump_bytes,
            "bridge and channel-pump must handle all byte values identically"
        );
        assert_eq!(bridge_bytes, payload);
    }

    /// Empty payload round-trips cleanly through both paths.
    #[test]
    fn wire_byte_parity_empty_payload() {
        let payload = b"";
        let bridge_bytes = round_trip_via_bridge(payload);
        let pump_bytes = round_trip_via_channel_pump(payload);

        assert_eq!(bridge_bytes, pump_bytes);
        assert!(bridge_bytes.is_empty());
    }

    /// Single-byte payload - boundary case for the minimum non-empty
    /// transfer.
    #[test]
    fn wire_byte_parity_single_byte() {
        let payload = &[0x42u8];
        let bridge_bytes = round_trip_via_bridge(payload);
        let pump_bytes = round_trip_via_channel_pump(payload);

        assert_eq!(bridge_bytes, pump_bytes);
        assert_eq!(bridge_bytes, payload.as_slice());
    }
}
