//! Byte-counting writer wrapper for transfer statistics.
//!
//! Mirrors upstream rsync's `stats.total_written` tracking in `io.c:859`.

use std::io::{self, IoSlice, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A writer wrapper that counts the total bytes written.
///
/// Used to track bytes sent during transfers for statistics.
/// Mirrors upstream rsync's `stats.total_written` tracking in `io.c:859`.
pub struct CountingWriter<W> {
    inner: W,
    bytes_written: u64,
    /// Optional shared handle mirroring the running total.
    ///
    /// Set via [`CountingWriter::new_shared`] when the count must be sampled
    /// from a different scope than the one owning the writer - e.g. the raw
    /// transport counter that feeds `stats.total_written` (io.c:859) but is read
    /// at the generator's `handle_stats` point (main.c:979-980), after the writer
    /// has been moved into the protocol stack. The handle stays valid after the
    /// writer is moved or dropped, exactly like the read-side `CountingReader`.
    shared: Option<Arc<AtomicU64>>,
}

impl<W> CountingWriter<W> {
    /// Creates a new counting writer wrapping the given writer.
    pub const fn new(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
            shared: None,
        }
    }

    /// Creates a counting writer that also publishes its running total through a
    /// shared [`Arc<AtomicU64>`] obtainable via [`CountingWriter::counter`].
    ///
    /// Mirrors the read-side `CountingReader`: the shared handle lets a caller
    /// sample the raw wire byte total after the writer has been moved into the
    /// protocol stack, matching upstream's raw descriptor counter
    /// `stats.total_written` (io.c:859).
    pub fn new_shared(inner: W) -> Self {
        Self {
            inner,
            bytes_written: 0,
            shared: Some(Arc::new(AtomicU64::new(0))),
        }
    }

    /// Returns a shared handle to the running byte total.
    ///
    /// Only advances for a writer built with [`CountingWriter::new_shared`];
    /// for a plain [`CountingWriter::new`] writer the returned counter stays 0.
    pub fn counter(&self) -> Arc<AtomicU64> {
        self.shared
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicU64::new(0)))
    }

    /// Returns the total number of bytes written through this wrapper.
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Records `n` freshly written bytes in the local and (if present) shared totals.
    fn record(&mut self, n: usize) {
        self.bytes_written = self.bytes_written.saturating_add(n as u64);
        if let Some(shared) = &self.shared {
            shared.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    /// Returns a mutable reference to the inner writer.
    ///
    /// Used by [`MsgInfoSender`](super::MsgInfoSender) to delegate protocol
    /// messages without going through the byte-counting `Write` path.
    pub(super) fn inner_ref_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Returns a shared reference to the inner writer.
    ///
    /// Used by [`MsgInfoSender`](super::MsgInfoSender) to query multiplex state
    /// without going through the byte-counting `Write` path.
    pub(super) fn inner_ref(&self) -> &W {
        &self.inner
    }

    /// Consumes the wrapper, returning the inner writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.record(n);
        Ok(n)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let n = self.inner.write_vectored(bufs)?;
        self.record(n);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::CountingWriter;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    /// The shared handle mirrors the running total and survives the writer.
    ///
    /// WHY: the generator samples raw wire bytes at its `handle_stats` point
    /// (main.c:979-980) after the writer has been moved into the protocol stack,
    /// so the count must be reachable through the shared handle, matching the
    /// read-side `CountingReader` (io.c:820/859).
    #[test]
    fn shared_counter_tracks_raw_bytes_and_outlives_writer() {
        let mut writer = CountingWriter::new_shared(Vec::new());
        let counter = writer.counter();

        writer.write_all(b"hello").unwrap();
        writer.write_all(b" world").unwrap();
        assert_eq!(writer.bytes_written(), 11);
        assert_eq!(counter.load(Ordering::Relaxed), 11);

        // Handle stays valid after the writer is dropped.
        drop(writer);
        assert_eq!(counter.load(Ordering::Relaxed), 11);
    }

    /// A plain `new` writer keeps its local count but publishes nothing shared.
    ///
    /// WHY: the receiver's existing local `CountingWriter::new` usage must be
    /// unaffected - only the transport wrapper opts into the shared handle.
    #[test]
    fn plain_counter_is_local_only() {
        let mut writer = CountingWriter::new(Vec::new());
        let counter = writer.counter();
        writer.write_all(b"abcd").unwrap();
        assert_eq!(writer.bytes_written(), 4);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
