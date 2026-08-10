//! Streaming zlib decoder with decompressed byte counting.

use std::io::{self, IoSliceMut, Read};

use flate2::read::DeflateDecoder;

/// Streaming decoder that records the number of decompressed bytes produced.
pub struct CountingZlibDecoder<R> {
    inner: DeflateDecoder<R>,
    bytes: u64,
}

impl<R> CountingZlibDecoder<R>
where
    R: Read,
{
    /// Creates a new decoder that wraps the provided reader.
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self {
            inner: DeflateDecoder::new(reader),
            bytes: 0,
        }
    }

    /// Returns the number of decompressed bytes read so far.
    #[inline]
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Returns a mutable reference to the underlying reader.
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self) -> &mut R {
        self.inner.get_mut()
    }

    /// Returns an immutable reference to the wrapped reader.
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref()
    }

    /// Consumes the decoder and returns the wrapped reader.
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner.into_inner()
    }
}

impl<R> Read for CountingZlibDecoder<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes = self.bytes.saturating_add(read as u64);
        Ok(read)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        let read = self.inner.read_vectored(bufs)?;
        self.bytes = self.bytes.saturating_add(read as u64);
        Ok(read)
    }
}
