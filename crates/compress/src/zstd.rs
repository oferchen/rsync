#![allow(clippy::module_name_repetitions)]

//! Streaming Zstandard helpers shared across the workspace.
//!
//! The interface mirrors the zlib helpers so higher layers can swap algorithms
//! without reworking their plumbing. Encoders implement [`std::io::Write`] and
//! keep track of the number of compressed bytes produced, allowing bandwidth
//! accounting to reuse the same code paths as zlib compression.

use std::io::{self, BufReader, IoSliceMut, Read, Write};

use crate::algorithm::CompressionAlgorithm;
use crate::common::{CountingSink, CountingWriter};
use crate::zlib::CompressionLevel;
use zstd::stream::{read::Decoder as ZstdDecoder, write::Encoder as ZstdEncoder};

/// Streaming encoder that records the number of compressed bytes produced.
pub struct CountingZstdEncoder<W = CountingSink>
where
    W: Write,
{
    inner: ZstdEncoder<'static, CountingWriter<W>>,
}

impl CountingZstdEncoder<CountingSink> {
    /// Creates a new encoder that discards the compressed output while tracking its length.
    pub fn new(level: CompressionLevel) -> io::Result<Self> {
        Self::with_sink(CountingSink, level)
    }

    /// Completes the stream and returns the total number of compressed bytes generated.
    pub fn finish(self) -> io::Result<u64> {
        let (_sink, bytes) = self.finish_into_inner()?;
        Ok(bytes)
    }
}

impl<W> CountingZstdEncoder<W>
where
    W: Write,
{
    /// Creates a new encoder that writes compressed bytes into `sink`.
    pub fn with_sink(sink: W, level: CompressionLevel) -> io::Result<Self> {
        let writer = CountingWriter::new(sink);
        let encoder = ZstdEncoder::new(writer, zstd_level(level)).map_err(io::Error::other)?;
        Ok(Self { inner: encoder })
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input)
    }

    /// Returns the number of compressed bytes produced so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.inner.get_ref().bytes()
    }

    /// Provides immutable access to the underlying sink.
    #[must_use]
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref().inner_ref()
    }

    /// Provides mutable access to the underlying sink.
    #[must_use]
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut().inner_mut()
    }

    /// Completes the stream and returns the sink together with the number of compressed bytes.
    pub fn finish_into_inner(self) -> io::Result<(W, u64)> {
        let writer = self.inner.finish().map_err(io::Error::other)?;
        Ok(writer.into_parts())
    }
}

/// Streaming decoder that records the number of decompressed bytes produced.
pub struct CountingZstdDecoder<R> {
    inner: ZstdDecoder<'static, BufReader<R>>,
    bytes: u64,
}

impl<R> CountingZstdDecoder<R>
where
    R: Read,
{
    /// Creates a new decoder that wraps the provided reader.
    pub fn new(reader: R) -> io::Result<Self> {
        let decoder = ZstdDecoder::new(reader).map_err(io::Error::other)?;
        Ok(Self {
            inner: decoder,
            bytes: 0,
        })
    }

    /// Returns the number of decompressed bytes read so far.
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Returns a mutable reference to the underlying reader.
    #[must_use]
    pub fn get_mut(&mut self) -> &mut R {
        self.inner.get_mut().get_mut()
    }

    /// Returns an immutable reference to the wrapped reader.
    #[must_use]
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref().get_ref()
    }

    /// Consumes the decoder and returns the wrapped reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner.finish().into_inner()
    }
}

impl<R> Read for CountingZstdDecoder<R>
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

/// Compresses `input` into a new [`Vec`].
pub fn compress_to_vec(input: &[u8], level: CompressionLevel) -> io::Result<Vec<u8>> {
    let mut encoder = ZstdEncoder::new(Vec::new(), zstd_level(level)).map_err(io::Error::other)?;
    encoder.write_all(input)?;
    encoder.finish().map_err(io::Error::other)
}

/// Decompresses `input` into a new [`Vec`].
pub fn decompress_to_vec(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = ZstdDecoder::new(input).map_err(io::Error::other)?;
    let mut output = Vec::new();
    io::copy(&mut decoder, &mut output)?;
    Ok(output)
}

/// Returns the preferred compression algorithm when callers do not provide one explicitly.
#[must_use]
pub const fn default_algorithm() -> CompressionAlgorithm {
    CompressionAlgorithm::Zstd
}

fn zstd_level(level: CompressionLevel) -> i32 {
    match level {
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 3,
        CompressionLevel::Best => 19,
        CompressionLevel::Precise(value) => i32::from(value.get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn counting_encoder_tracks_bytes() {
        let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).expect("encoder");
        encoder.write(b"payload").expect("compress payload");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn encoder_with_sink_forwards_bytes() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).expect("encoder");
        encoder.write(b"payload").expect("compress payload");
        let (compressed, bytes) = encoder.finish_into_inner().expect("finish stream");
        assert_eq!(bytes as usize, compressed.len());
    }

    #[test]
    fn decoder_tracks_bytes() {
        let compressed = compress_to_vec(b"payload", CompressionLevel::Default).expect("compress");
        let mut decoder = CountingZstdDecoder::new(&compressed[..]).expect("decoder");
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).expect("decompress");
        assert_eq!(output, b"payload");
        assert_eq!(decoder.bytes_read(), output.len() as u64);
    }
}
