use std::io::{self, Read};

use super::round_up_1024;

/// Upstream's raw input buffer size.
///
/// upstream: `io.c:1587` `alloc_xbuf(&iobuf.in, ROUND_UP_1024(IO_BUFFER_SIZE))`
/// with `IO_BUFFER_SIZE = 32*1024` (`rsync.h:160`). Never reallocated
/// (io.c:663 "We never resize the circular input buffer."); a request larger
/// than the buffer is a fatal protocol error, never a growth.
pub const IN_BUFFER_SIZE: usize = 32 * 1024;

/// Circular input buffer sitting *beneath* the multiplex demultiplexer.
///
/// Mirrors upstream's `iobuf.in` (`xbuf`, io.c:101-110): `pos` is the offset of
/// the next unread byte, `len` the number of readable bytes, and the region
/// wraps around a fixed `size`. The allocation happens once and never grows, so
/// steady-state memory is bounded no matter what a peer sends.
///
/// Nothing above this type knows the wire exists: the decoders read from the
/// demultiplexer, the demultiplexer reads from here, and only this type touches
/// the descriptor - which is the layering upstream enforces with
/// `assert(fd != iobuf.in_fd)` in `safe_read()` (io.c:314).
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
    /// the buffer (io.c:663).
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
    /// upstream: io.c:655-659 - an emptied buffer rewinds to offset 0 so the
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

    /// Whether the buffer has room for more input.
    ///
    /// upstream: io.c:752 - perform_io polls the input descriptor only while
    /// `iobuf.in.size - iobuf.in.len` is non-zero.
    #[must_use]
    pub fn has_room(&self) -> bool {
        self.len < self.capacity()
    }

    /// Fills the largest contiguous free span from `source`, which returns
    /// the bytes it placed there, or `None` when it has none yet.
    ///
    /// The non-blocking sibling of [`InBuf::fill`]: a full buffer, or a source
    /// with nothing ready, buffers nothing and returns `Ok(None)`.
    pub fn fill_with(
        &mut self,
        source: impl FnOnce(&mut [u8]) -> io::Result<Option<usize>>,
    ) -> io::Result<Option<usize>> {
        let size = self.capacity();
        if self.len == size {
            return Ok(None);
        }
        let mut end = self.pos + self.len;
        if end >= size {
            end -= size;
        }
        let limit = if end >= self.pos { size } else { self.pos };
        let n = source(&mut self.buf[end..limit])?;
        if let Some(n) = n {
            self.len += n;
        }
        Ok(n)
    }

    /// Reads once from `reader` into the largest contiguous free span.
    ///
    /// Returns the number of bytes buffered, `0` on EOF. The buffer is never
    /// resized to make room; when it is full this is a no-op returning `0`,
    /// which is the point at which upstream's `perform_io` would instead be
    /// draining output (io.c:746-755 only polls for read when there is free
    /// space).
    pub fn fill<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<usize> {
        Ok(self
            .fill_with(|span| reader.read(span).map(Some))?
            .unwrap_or(0))
    }
}
