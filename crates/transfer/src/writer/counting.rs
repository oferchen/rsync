//! Byte-counting writer wrapper for transfer statistics.
//!
//! Mirrors upstream rsync's `stats.total_written` tracking in `io.c:859`.

use std::io::{self, IoSlice, Write};

/// A writer wrapper that counts the total bytes written.
///
/// Used to track bytes sent during transfers for statistics.
/// Mirrors upstream rsync's `stats.total_written` tracking in `io.c:859`.
pub struct CountingWriter<W> {
    inner: W,
    bytes_written: u64,
}

impl<W> CountingWriter<W> {
    /// Creates a new counting writer wrapping the given writer.
    pub const fn new(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
        }
    }

    /// Returns the total number of bytes written through this wrapper.
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns a mutable reference to the inner writer.
    ///
    /// Used by [`MsgInfoSender`](super::MsgInfoSender) to delegate protocol
    /// messages without going through the byte-counting `Write` path.
    pub(super) fn inner_ref_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Consumes the wrapper, returning the inner writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes_written = self.bytes_written.saturating_add(n as u64);
        Ok(n)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let n = self.inner.write_vectored(bufs)?;
        self.bytes_written = self.bytes_written.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
