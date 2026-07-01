//! Byte-counting reader wrapper for transfer statistics.
//!
//! Mirrors upstream rsync's `stats.total_read` tracking in `io.c:820`, which
//! increments by the byte count of each raw socket read - below the multiplex
//! demultiplexer and below token decompression.

use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A reader wrapper that counts the total bytes pulled from the underlying
/// transport.
///
/// Used to track bytes received during transfers for statistics. Wrapping the
/// raw transport (below the multiplex demultiplexer and token decompression)
/// makes the running total reflect the compressed wire bytes, matching upstream
/// rsync's `stats.total_read` (`io.c:820`).
///
/// The reader is moved into the protocol stack ([`ServerReader`](super::ServerReader))
/// and consumed by the transfer loop, so the count is published through a shared
/// [`Arc<AtomicU64>`]. Obtain a handle with [`CountingReader::counter`] before
/// constructing the stack, then read the final total after the loop returns.
pub(crate) struct CountingReader<R> {
    inner: R,
    bytes_read: Arc<AtomicU64>,
}

impl<R> CountingReader<R> {
    /// Creates a new counting reader wrapping the given reader.
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a shared handle to the running byte total.
    ///
    /// The handle stays valid after this reader is moved and dropped; it holds
    /// the final count recorded by the last successful read.
    pub(crate) fn counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.bytes_read)
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Async counterpart to the blocking [`Read`] impl above, gated on the
/// `tokio-transfer` feature.
///
/// This is the `.await`-driven twin of the counter: it wraps the same
/// [`CountingReader`] type over an inner [`tokio::io::AsyncRead`] and updates the
/// same shared `bytes_read` [`Arc<AtomicU64>`]. There is no forked type and no
/// forked count logic - only the read primitive differs (a `poll_read` on the
/// inner async transport versus a blocking `read`).
///
/// # Byte-exact accounting (the correctness crux)
///
/// The blocking impl increments by exactly the number of bytes the underlying
/// `read` returned. The async impl increments by exactly the number of bytes the
/// inner `poll_read` added to the [`tokio::io::ReadBuf`] on each poll: the
/// difference between the filled length before and after the delegated
/// `poll_read`. A `poll_read` may fill any amount `0..=remaining`, and every
/// such byte came off the transport exactly once, so the running total counts
/// each transport byte exactly once - identical to the blocking counter.
///
/// Placing the counter directly over the raw async transport (below any
/// buffering layer, matching the sync stack's position below `BufReader`) means
/// the total tracks bytes actually pulled from the wire. When the stream is
/// drained to EOF the total equals the exact wire byte count with no buffering
/// prefetch skew, so the async counter is byte-identical to the sync counter and
/// carries none of the known feature-on prefetch quirk.
#[cfg(feature = "tokio-transfer")]
impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CountingReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        let before = buf.filled().len();
        let inner = std::pin::Pin::new(&mut self.inner);
        match inner.poll_read(cx, buf) {
            std::task::Poll::Ready(Ok(())) => {
                let added = buf.filled().len() - before;
                self.bytes_read.fetch_add(added as u64, Ordering::Relaxed);
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
