//! Streaming zlib encoder with compressed byte counting.

use std::fmt;
use std::io::{self, IoSlice, Write};

use flate2::write::DeflateEncoder as FlateEncoder;

use super::CompressionLevel;
use crate::common::{CountingSink, CountingWriter};

/// Streaming encoder that records the number of compressed bytes produced.
///
/// The encoder implements [`std::io::Write`], enabling integration with APIs
/// such as [`std::io::copy`], [`write!`](std::write), and
/// [`std::io::Write::write_all`]. By default compressed bytes are discarded
/// after being counted, matching upstream rsync's bandwidth accounting path
/// where the payload itself is forwarded separately. Callers that need to
/// forward the compressed stream can construct the encoder with an explicit
/// sink via [`CountingZlibEncoder::with_sink`] so the counted bytes are written
/// into the provided writer.
pub struct CountingZlibEncoder<W = CountingSink>
where
    W: Write,
{
    inner: FlateEncoder<CountingWriter<W>>,
}

impl CountingZlibEncoder<CountingSink> {
    /// Creates a new encoder that counts the compressed output produced by zlib.
    #[must_use]
    pub fn new(level: CompressionLevel) -> Self {
        Self::with_sink(CountingSink, level)
    }

    /// Completes the stream and returns the total number of compressed bytes generated.
    pub fn finish(self) -> io::Result<u64> {
        let (_sink, bytes) = self.finish_into_inner()?;
        Ok(bytes)
    }
}

impl<W> CountingZlibEncoder<W>
where
    W: Write,
{
    /// Creates a new encoder that forwards compressed bytes into `sink`.
    #[must_use]
    pub fn with_sink(sink: W, level: CompressionLevel) -> Self {
        Self {
            inner: FlateEncoder::new(CountingWriter::new(sink), level.into()),
        }
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input)
    }

    /// Returns the number of compressed bytes produced so far without finalising the stream.
    #[inline]
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.inner.get_ref().bytes()
    }

    /// Provides immutable access to the underlying sink.
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref().inner_ref()
    }

    /// Provides mutable access to the underlying sink.
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut().inner_mut()
    }

    /// Completes the stream, returning the sink and the total number of compressed bytes produced.
    ///
    /// # Errors
    ///
    /// Propagates any I/O errors reported by the underlying writer or zlib
    /// during stream finalisation.
    pub fn finish_into_inner(self) -> io::Result<(W, u64)> {
        let writer = self.inner.finish()?;
        Ok(writer.into_parts())
    }
}

impl<W> Write for CountingZlibEncoder<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.inner.write_vectored(bufs)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.inner.write_fmt(fmt)
    }
}
