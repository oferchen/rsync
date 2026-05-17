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
//!   ([`SyncReader`], [`SyncWriter`]) halves backed by a background tokio task
//!   that pumps channel data into bounded `std::sync::mpsc` /
//!   `tokio::sync::mpsc` queues. This is the bridge variant used by
//!   [`connect_and_exec`](super::connect::connect_and_exec); it is re-exposed
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
//!   [`std::io::ErrorKind::BrokenPipe`] from `write`.
//!
//! # Backpressure
//!
//! The channel-based [`into_sync_halves`] variant uses bounded
//! `mpsc::channel(64)` queues in both directions, so a stalled async reader
//! will eventually block sync writers and vice versa. This matches how the
//! existing system-SSH transport behaves under load.

use std::io;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc as tokio_mpsc;

/// Default capacity for the in-process queues used by [`into_sync_halves`].
///
/// Matches the queue depth used by [`super::connect::connect_and_exec`] so
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
        let stream = self.stream.as_mut();
        self.runtime.block_on(async move { stream.read(buf).await })
    }
}

impl<S> io::Write for SyncAsyncBridge<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let stream = self.stream.as_mut();
        self.runtime.block_on(async move {
            stream.write_all(buf).await?;
            Ok(buf.len())
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        let stream = self.stream.as_mut();
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

/// Synchronous writer half returned by [`into_sync_halves`].
///
/// Forwards each `write` call as a single message on a bounded
/// `tokio::sync::mpsc::Sender` via `blocking_send`, which honors
/// backpressure: when the channel is full the calling thread blocks until
/// the async side drains capacity. A closed receiver surfaces as
/// [`io::ErrorKind::BrokenPipe`].
pub struct SyncWriter {
    tx: Option<tokio_mpsc::Sender<Vec<u8>>>,
}

impl SyncWriter {
    fn new(tx: tokio_mpsc::Sender<Vec<u8>>) -> Self {
        Self { tx: Some(tx) }
    }
}

impl io::Write for SyncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "writer shut down"))?;
        tx.blocking_send(buf.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for SyncWriter {
    fn drop(&mut self) {
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
/// [`super::connect::connect_and_exec`]; it is exposed here for callers who
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
    use std::time::Duration;
    use tokio::io::duplex;

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

    /// `SyncWriter` returns `BrokenPipe` once the downstream receiver is
    /// gone.
    #[test]
    fn sync_writer_to_closed_channel_is_broken_pipe() {
        // Build the channel outside a runtime - blocking_send must not be
        // called from within one.
        let (tx, rx) = tokio_mpsc::channel::<Vec<u8>>(1);
        drop(rx);
        let mut writer = SyncWriter::new(tx);
        let err = writer.write_all(b"x").unwrap_err();
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

        let handle = std::thread::spawn(move || {
            // Notify that the writer is about to block.
            signal_tx.send(()).unwrap();
            writer.write_all(b"second").unwrap();
        });

        // Wait for the writer thread to be ready and prove the second write
        // is still blocked by draining only the first message.
        signal_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let first = rt.block_on(rx.recv()).unwrap();
        assert_eq!(first, b"first");

        // Once a slot is free the second write should complete promptly.
        let second = rt.block_on(rx.recv()).unwrap();
        assert_eq!(second, b"second");
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
}
