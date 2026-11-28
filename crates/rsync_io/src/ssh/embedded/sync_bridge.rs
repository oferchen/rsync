//! Sync/async bridge primitives for embedded SSH streams.
//!
//! This module provides the inverse of `crate::channel_adapter` (named without
//! an intra-doc link: that module is gated behind the off-by-default
//! `async-ssh` feature, so the link would be unresolvable in a default build
//! and `#![deny(rustdoc::broken_intra_doc_links)]` would fail `cargo doc`):
//! instead of
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
//! - `ChunkPool` and `drain_into` are shared with `super::connect`, whose
//!   `ChannelReader` / `ChannelWriter` bridge a russh channel through
//!   bounded queues for `connect_and_exec`.
//!
//! # Invariants
//!
//! - Each `read` consumes one chunk from the async stream into a caller
//!   buffer; oversized chunks are buffered internally and drained on the next
//!   read.
//! - Inbound chunks cross the queue as the `bytes::Bytes` russh delivered, so
//!   the bridge adds no allocation or copy beyond the one into the caller's
//!   buffer. Outbound chunks come from a bounded `ChunkPool` whose buffers
//!   return to the pool when russh drops them, so steady-state writes do not
//!   allocate.
//! - `write` always consumes the entire input slice via one `write_all` on
//!   the underlying async writer; partial writes are never reported to
//!   callers.
//! - A closed/EOF async stream surfaces as `Ok(0)` from `read`, matching the
//!   `std::io::Read` contract.
//! - A closed downstream receiver surfaces as
//!   `std::io::ErrorKind::BrokenPipe` from `write`.

use std::io;
use std::pin::Pin;
use std::sync::mpsc as std_mpsc;

use bytes::{Buf, Bytes};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::runtime::{Builder, Runtime};

/// Default capacity for the in-process queues used by
/// `super::connect::connect_and_exec`.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 64;

/// Largest buffer a [`ChunkPool`] keeps for reuse.
///
/// Covers a full multiplex frame (`MplexWriter` stages 32 KiB plus its 4-byte
/// header). A larger write still
/// goes out as one chunk, but its buffer is freed rather than retained, so
/// one oversized write cannot pin memory for the rest of the session.
pub(super) const MAX_RECYCLED_CHUNK: usize = 64 * 1024;

/// Bounded free list of outbound chunk buffers.
///
/// Every chunk handed to russh is a `Bytes` that owns a pooled `Vec<u8>`.
/// When russh drops the chunk (after copying it into its packet buffer), the
/// `Vec` returns here instead of being freed, so a steady stream of writes
/// reuses the same few allocations. The free list holds at most `capacity`
/// idle buffers of at most `MAX_RECYCLED_CHUNK` bytes each; a buffer that
/// would exceed either bound is freed, so the pool never grows without limit.
pub(super) struct ChunkPool {
    free: std_mpsc::Receiver<Vec<u8>>,
    home: std_mpsc::SyncSender<Vec<u8>>,
}

impl ChunkPool {
    /// Creates a pool that retains at most `capacity` idle buffers.
    #[must_use]
    pub(super) fn new(capacity: usize) -> Self {
        let (home, free) = std_mpsc::sync_channel(capacity.max(1));
        Self { free, home }
    }

    /// Takes an empty buffer from the pool, or a fresh one when none is idle.
    #[must_use]
    pub(super) fn take(&self) -> Vec<u8> {
        self.free.try_recv().unwrap_or_default()
    }

    /// Wraps `buf` as a `Bytes` chunk that returns `buf` to this pool when
    /// the last reference to the chunk is dropped.
    #[must_use]
    pub(super) fn wrap(&self, buf: Vec<u8>) -> Bytes {
        Bytes::from_owner(Recycled {
            buf,
            home: self.home.clone(),
        })
    }

    /// Copies `data` into a pooled buffer and returns it as a chunk.
    #[must_use]
    pub(super) fn chunk(&self, data: &[u8]) -> Bytes {
        if data.len() > MAX_RECYCLED_CHUNK {
            return Bytes::copy_from_slice(data);
        }
        let mut buf = self.take();
        buf.extend_from_slice(data);
        self.wrap(buf)
    }
}

/// Owner of a pooled buffer while russh holds it as a `Bytes` chunk.
struct Recycled {
    buf: Vec<u8>,
    home: std_mpsc::SyncSender<Vec<u8>>,
}

impl AsRef<[u8]> for Recycled {
    fn as_ref(&self) -> &[u8] {
        &self.buf
    }
}

impl Drop for Recycled {
    fn drop(&mut self) {
        if self.buf.capacity() <= MAX_RECYCLED_CHUNK {
            let mut buf = std::mem::take(&mut self.buf);
            buf.clear();
            // A full free list or a dropped pool frees the buffer instead.
            let _ = self.home.try_send(buf);
        }
    }
}

/// Copies as much of `pending` as fits into `buf` and advances past it.
pub(super) fn drain_into(pending: &mut Bytes, buf: &mut [u8]) -> usize {
    let n = pending.len().min(buf.len());
    buf[..n].copy_from_slice(&pending[..n]);
    pending.advance(n);
    n
}

/// Wraps an `AsyncRead + AsyncWrite` stream as a synchronous
/// `Read + Write` source by driving an internal current-thread tokio
/// runtime via `block_on` on every call.
///
/// # When to use
///
/// Prefer this adapter for one-shot situations where the bridged stream is
/// owned exclusively by the bridge (i.e., no other tokio tasks need to drive
/// it concurrently). Russh channels driven by `Channel::wait()` and
/// `Handle::data()` go through `super::connect`'s `ChannelReader` /
/// `ChannelWriter` instead.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
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

    /// A chunk's buffer returns to the pool once russh drops the chunk, so
    /// the next write reuses the same allocation instead of allocating. This
    /// is the whole point of the pool: steady-state writes do not allocate.
    #[test]
    fn pool_reuses_buffer_after_chunk_is_dropped() {
        let pool = ChunkPool::new(4);
        let chunk = pool.chunk(b"first payload");
        let first_ptr = chunk.as_ptr();
        drop(chunk);

        let reused = pool.chunk(b"second");
        assert_eq!(reused.as_ptr(), first_ptr, "dropped buffer must be reused");
        assert_eq!(
            &reused[..],
            b"second",
            "a reused buffer carries no stale bytes"
        );
    }

    /// A chunk still held by russh is never handed out again: the pool only
    /// sees a buffer after its last reference is gone, so a later write
    /// cannot overwrite bytes that have not been packetised yet.
    #[test]
    fn pool_never_reissues_a_live_chunk() {
        let pool = ChunkPool::new(4);
        let held = pool.chunk(b"in flight");
        let held_ptr = held.as_ptr();
        let other = pool.chunk(b"next write");

        assert_ne!(other.as_ptr(), held_ptr);
        assert_eq!(&held[..], b"in flight");
    }

    /// The free list keeps at most `capacity` idle buffers; extra returns are
    /// freed. A burst of writes therefore cannot grow the pool without bound.
    #[test]
    fn pool_retains_at_most_capacity_idle_buffers() {
        let pool = ChunkPool::new(2);
        let chunks: Vec<Bytes> = (0..5).map(|_| pool.chunk(&[7u8; 128])).collect();
        drop(chunks);

        let retained = (0..5).filter(|_| pool.take().capacity() > 0).count();
        assert_eq!(retained, 2, "only `capacity` buffers may stay idle");
    }

    /// Buffers above `MAX_RECYCLED_CHUNK` are freed on drop rather than
    /// retained, so one oversized write cannot pin memory for the session.
    #[test]
    fn pool_frees_oversized_buffers() {
        let pool = ChunkPool::new(4);
        let big = pool.chunk(&vec![1u8; MAX_RECYCLED_CHUNK + 1]);
        assert_eq!(big.len(), MAX_RECYCLED_CHUNK + 1);
        drop(big);

        let mut grown = pool.take();
        grown.resize(MAX_RECYCLED_CHUNK + 1, 0);
        drop(pool.wrap(grown));

        assert_eq!(
            pool.take().capacity(),
            0,
            "oversized buffers must not be pooled"
        );
    }
}
