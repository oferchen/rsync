use std::io::{self, Read};

use super::round_up_1024;

/// Upstream's raw input buffer size.
///
/// upstream: `io.c:1561` `alloc_xbuf(&iobuf.in, ROUND_UP_1024(IO_BUFFER_SIZE))`
/// with `IO_BUFFER_SIZE = 32*1024` (`rsync.h:160`). Never reallocated
/// (io.c:579 "We never resize the circular input buffer."); a request larger
/// than the buffer is a fatal protocol error, never a growth.
pub const IN_BUFFER_SIZE: usize = 32 * 1024;

/// Circular input buffer sitting *beneath* the multiplex demultiplexer.
///
/// Mirrors upstream's `iobuf.in` (`xbuf`, io.c:91-100): `pos` is the offset of
/// the next unread byte, `len` the number of readable bytes, and the region
/// wraps around a fixed `size`. The allocation happens once and never grows, so
/// steady-state memory is bounded no matter what a peer sends.
///
/// Nothing above this type knows the wire exists: the decoders read from the
/// demultiplexer, the demultiplexer reads from here, and only this type touches
/// the descriptor - which is the layering upstream enforces with
/// `assert(fd != iobuf.in_fd)` in `safe_read()` (io.c:243).
pub struct InBuf {
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

impl InBuf {
    /// Allocates a fixed-size circular input buffer.
    ///
    /// `capacity` is rounded up to a whole number of 1024-byte units, matching
    /// upstream's `ROUND_UP_1024` allocation discipline.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0u8; round_up_1024(capacity)],
            pos: 0,
            len: 0,
        }
    }

    /// Returns the fixed circular size. This never changes for the lifetime of
    /// the buffer (io.c:579).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Returns the number of bytes currently readable.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when no bytes are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the longest contiguous readable run starting at `pos`.
    ///
    /// A wrapped buffer serves its head first; the caller drains it, and the
    /// tail becomes contiguous on the next call.
    #[must_use]
    pub fn readable(&self) -> &[u8] {
        let end = (self.pos + self.len).min(self.capacity());
        &self.buf[self.pos..end]
    }

    /// Marks `n` readable bytes as consumed, wrapping `pos`.
    ///
    /// upstream: io.c:565-573 - an emptied buffer rewinds to offset 0 so the
    /// next fill gets the whole span contiguously.
    pub fn consume(&mut self, n: usize) {
        debug_assert!(n <= self.len, "consuming more than is buffered");
        self.pos += n;
        self.len -= n;
        if self.pos >= self.capacity() {
            self.pos -= self.capacity();
        }
        if self.len == 0 {
            self.pos = 0;
        }
    }

    /// Reads once from `reader` into the largest contiguous free span.
    ///
    /// Returns the number of bytes buffered, `0` on EOF. The buffer is never
    /// resized to make room; when it is full this is a no-op returning `0`,
    /// which is the point at which upstream's `perform_io` would instead be
    /// draining output (io.c:664-672 only selects for read when there is free
    /// space).
    pub fn fill<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<usize> {
        let size = self.capacity();
        if self.len == size {
            return Ok(0);
        }
        let mut end = self.pos + self.len;
        if end >= size {
            end -= size;
        }
        let limit = if end >= self.pos { size } else { self.pos };
        let n = reader.read(&mut self.buf[end..limit])?;
        self.len += n;
        Ok(n)
    }
}

/// [`Read`] adapter that funnels a transfer descriptor through an [`InBuf`].
///
/// This is the seam the multiplex demultiplexer sits on: it replaces a
/// `std::io::BufReader` whose capacity was free to differ from upstream's with
/// a buffer of exactly `IO_BUFFER_SIZE` that can never grow.
pub struct IoBufReader<R> {
    inner: R,
    buf: InBuf,
}

impl<R: Read> IoBufReader<R> {
    /// Wraps `inner` in a fixed 32 KiB input buffer (upstream io.c:1401).
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self::with_capacity(IN_BUFFER_SIZE, inner)
    }

    /// Wraps `inner` in a fixed input buffer of `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize, inner: R) -> Self {
        Self {
            inner,
            buf: InBuf::new(capacity),
        }
    }

    /// Returns a mutable reference to the wrapped reader.
    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Consumes the adapter, returning the wrapped reader.
    ///
    /// Any bytes still buffered are discarded, so this is only valid once the
    /// buffer is drained.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for IoBufReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buf.is_empty() {
            // A request at least as large as the buffer gains nothing from
            // staging: hand the caller's slice straight to the descriptor, the
            // same short-circuit `std::io::BufReader` makes.
            if out.len() >= self.buf.capacity() {
                return self.inner.read(out);
            }
            if self.buf.fill(&mut self.inner)? == 0 {
                return Ok(0);
            }
        }

        let readable = self.buf.readable();
        let n = readable.len().min(out.len());
        out[..n].copy_from_slice(&readable[..n]);
        self.buf.consume(n);
        Ok(n)
    }
}
