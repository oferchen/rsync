//! Algorithm-agnostic encoder for compressed token wire format.
//!
//! Dispatches to the appropriate per-algorithm codec (zlib, zstd, lz4)
//! based on the negotiated compression algorithm. The outer framing
//! (flag bytes, DEFLATED_DATA headers, token run encoding) is shared
//! across all algorithms - only the payload compression differs.
//!
//! - upstream: token.c:send_compressed_token() (algorithm dispatch)

use std::io::{self, Write};

use compress::zlib::CompressionLevel;

use super::zlib_codec::ZlibTokenEncoder;

/// Encoder state for sending compressed tokens.
///
/// Wraps a per-algorithm encoder implementation. The public API is
/// algorithm-agnostic - callers interact through `send_literal`,
/// `send_block_match`, `finish`, and `see_token` regardless of
/// which compression algorithm is active.
///
/// # Examples
///
/// ```
/// use protocol::wire::CompressedTokenEncoder;
/// use compress::zlib::CompressionLevel;
///
/// let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
/// let mut output = Vec::new();
///
/// encoder.send_literal(&mut output, b"hello world").unwrap();
/// encoder.send_block_match(&mut output, 0).unwrap();
/// encoder.finish(&mut output).unwrap();
/// ```
pub struct CompressedTokenEncoder {
    inner: EncoderInner,
}

enum EncoderInner {
    Zlib(ZlibTokenEncoder),
}

impl CompressedTokenEncoder {
    /// Creates a new zlib encoder with the specified compression level and protocol version.
    ///
    /// This constructor creates a zlib/zlibx encoder, matching the original rsync
    /// compression behavior. Use [`for_algorithm`](Self::for_algorithm) to select
    /// a specific compression algorithm.
    #[must_use]
    pub fn new(level: CompressionLevel, protocol_version: u32) -> Self {
        Self {
            inner: EncoderInner::Zlib(ZlibTokenEncoder::new(level, protocol_version)),
        }
    }

    /// Resets the encoder for a new file.
    pub fn reset(&mut self) {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.reset(),
        }
    }

    /// Sends literal data with compression.
    ///
    /// Accumulates data in an internal buffer and compresses it when the buffer
    /// reaches CHUNK_SIZE (32 KiB).
    pub fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.send_literal(writer, data),
        }
    }

    /// Sends a block match token.
    ///
    /// Flushes any pending compressed literal data and writes a token indicating
    /// that the receiver should copy data from the specified block in the basis file.
    pub fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.send_block_match(writer, block_index),
        }
    }

    /// Signals end of file and flushes all pending data.
    pub fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.finish(writer),
        }
    }

    /// Feeds block data into the compressor's history without producing output.
    ///
    /// Only needed for CPRES_ZLIB mode. Noop for zlibx, zstd, and lz4.
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.see_token(data),
        }
    }

    /// Configures zlibx mode for this encoder.
    ///
    /// When `true`, [`Self::see_token`] becomes a no-op, matching upstream
    /// rsync's CPRES_ZLIBX behaviour.
    pub fn set_zlibx(&mut self, zlibx: bool) {
        match &mut self.inner {
            EncoderInner::Zlib(enc) => enc.set_zlibx(zlibx),
        }
    }
}

impl Default for CompressedTokenEncoder {
    fn default() -> Self {
        Self::new(CompressionLevel::Default, 31)
    }
}
