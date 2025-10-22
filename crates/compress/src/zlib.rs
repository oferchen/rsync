//! # Overview
//!
//! Zlib compression helpers shared across the workspace. The module currently
//! exposes a [`CountingZlibEncoder`] that mirrors upstream rsync's compression
//! pipeline by accepting incremental input while tracking the number of bytes
//! produced by the compressor. This allows higher layers to report accurate
//! compressed sizes without buffering the resulting payload in memory.
//!
//! # Examples
//!
//! Compress data incrementally and obtain the compressed length:
//!
//! ```
//! use rsync_compress::zlib::{CompressionLevel, CountingZlibEncoder};
//!
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
//! encoder.write(b"payload").unwrap();
//! let compressed_len = encoder.finish().unwrap();
//! assert!(compressed_len > 0);
//! ```
//!
//! Obtain a compressed buffer and decompress it:
//!
//! ```
//! use rsync_compress::zlib::{CompressionLevel, compress_to_vec, decompress_to_vec};
//!
//! let data = b"highly compressible payload";
//! let compressed = compress_to_vec(data, CompressionLevel::Best).unwrap();
//! let decoded = decompress_to_vec(&compressed).unwrap();
//! assert_eq!(decoded, data);
//! ```

use std::{
    fmt,
    io::{self, Write},
    num::NonZeroU8,
};

use flate2::{Compression, read::ZlibDecoder as FlateDecoder, write::ZlibEncoder as FlateEncoder};

/// Compression levels recognised by the zlib encoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionLevel {
    /// Favour speed over compression ratio.
    Fast,
    /// Use zlib's default balance between speed and ratio.
    Default,
    /// Favour the best possible compression ratio.
    Best,
    /// Use an explicit zlib compression level in the range `1..=9`.
    Precise(NonZeroU8),
}

impl CompressionLevel {
    /// Creates a [`CompressionLevel::Precise`] value from an explicit numeric level.
    ///
    /// The supplied `level` must fall within the inclusive range `1..=9`. The
    /// caller is responsible for interpreting `0` as disabled compression; this
    /// helper mirrors zlib's accepted range and returns an error when the value
    /// exceeds the supported bounds.
    pub fn from_numeric(level: u32) -> Result<Self, CompressionLevelError> {
        if (1..=9).contains(&level) {
            // SAFETY: the range check above guarantees `level` is non-zero and fits in a `u8`.
            let precise = NonZeroU8::new(level as u8).expect("validated non-zero level");
            Ok(Self::Precise(precise))
        } else {
            Err(CompressionLevelError::new(level))
        }
    }

    /// Constructs a [`CompressionLevel::Precise`] variant from the provided zlib level.
    #[must_use]
    pub const fn precise(level: NonZeroU8) -> Self {
        Self::Precise(level)
    }
}

impl From<CompressionLevel> for Compression {
    fn from(level: CompressionLevel) -> Self {
        match level {
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(value) => Compression::new(u32::from(value.get())),
        }
    }
}

/// Error returned when a requested compression level falls outside the
/// permissible zlib range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompressionLevelError {
    level: u32,
}

impl CompressionLevelError {
    /// Creates a new error capturing the unsupported compression level.
    const fn new(level: u32) -> Self {
        Self { level }
    }

    /// Returns the invalid compression level that triggered the error.
    #[must_use]
    pub const fn level(&self) -> u32 {
        self.level
    }
}

impl fmt::Display for CompressionLevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "compression level {} is outside the supported range 1-9",
            self.level
        )
    }
}

impl std::error::Error for CompressionLevelError {}

/// Streaming encoder that records the number of compressed bytes produced.
pub struct CountingZlibEncoder {
    inner: FlateEncoder<CountingWriter>,
}

impl CountingZlibEncoder {
    /// Creates a new encoder that counts the compressed output produced by zlib.
    #[must_use]
    pub fn new(level: CompressionLevel) -> Self {
        Self {
            inner: FlateEncoder::new(CountingWriter::default(), level.into()),
        }
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input)
    }

    /// Returns the number of compressed bytes produced so far without finalising the stream.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.inner.get_ref().bytes()
    }

    /// Completes the stream and returns the total number of compressed bytes generated.
    pub fn finish(self) -> io::Result<u64> {
        let writer = self.inner.finish()?;
        Ok(writer.bytes())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CountingWriter {
    bytes: u64,
}

impl CountingWriter {
    fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Compresses `input` into a new [`Vec`].
pub fn compress_to_vec(input: &[u8], level: CompressionLevel) -> io::Result<Vec<u8>> {
    let mut encoder = FlateEncoder::new(Vec::new(), level.into());
    encoder.write_all(input)?;
    encoder.finish()
}

/// Decompresses `input` into a new [`Vec`].
pub fn decompress_to_vec(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = FlateDecoder::new(input);
    let mut output = Vec::new();
    io::copy(&mut decoder, &mut output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_encoder_tracks_bytes() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        encoder.write(b"payload").expect("compress payload");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn counting_encoder_reports_incremental_bytes() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        assert_eq!(encoder.bytes_written(), 0);
        encoder.write(b"payload").expect("compress payload");
        let after_first = encoder.bytes_written();
        encoder.write(b"more payload").expect("compress payload");
        let after_second = encoder.bytes_written();
        assert!(after_second >= after_first);
        let final_len = encoder.finish().expect("finish stream");
        assert!(final_len >= after_second);
    }

    #[test]
    fn streaming_round_trip_preserves_payload() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        let input = b"The quick brown fox jumps over the lazy dog".repeat(8);
        for chunk in input.chunks(11) {
            encoder.write(chunk).expect("write chunk");
        }
        let compressed_len = encoder.finish().expect("finish stream");
        assert!(compressed_len > 0);

        let compressed = compress_to_vec(&input, CompressionLevel::Default).expect("compress");
        assert!(compressed.len() as u64 >= compressed_len);
        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn helper_functions_round_trip() {
        let payload = b"highly compressible payload";
        let compressed = compress_to_vec(payload, CompressionLevel::Best).expect("compress");
        let decoded = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn precise_level_converts_to_requested_value() {
        let level = NonZeroU8::new(7).expect("non-zero");
        let compression = Compression::from(CompressionLevel::precise(level));
        assert_eq!(compression.level(), u32::from(level.get()));
    }

    #[test]
    fn numeric_level_constructor_accepts_valid_range() {
        for level in 1..=9 {
            let precise = CompressionLevel::from_numeric(level).expect("valid level");
            let expected = NonZeroU8::new(level as u8).expect("validated");
            assert_eq!(precise, CompressionLevel::Precise(expected));
        }
    }

    #[test]
    fn numeric_level_constructor_rejects_out_of_range() {
        let err = CompressionLevel::from_numeric(10).expect_err("level above 9 rejected");
        assert_eq!(err.level(), 10);
    }
}
