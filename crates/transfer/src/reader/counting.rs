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
