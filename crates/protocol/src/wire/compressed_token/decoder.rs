//! Algorithm-agnostic decoder for compressed token wire format.
//!
//! Dispatches to the appropriate per-algorithm codec (zlib, zstd, lz4)
//! based on the negotiated compression algorithm.
//!
//! - upstream: token.c:recv_compressed_token() (algorithm dispatch)

use std::io::{self, Read};

use super::CompressedToken;
use super::zlib_codec::ZlibTokenDecoder;

/// Decoder state for receiving compressed tokens.
///
/// Wraps a per-algorithm decoder implementation. The public API is
/// algorithm-agnostic - callers interact through `recv_token` and
/// `see_token` regardless of which compression algorithm is active.
///
/// # Examples
///
/// ```no_run
/// use protocol::wire::{CompressedTokenDecoder, CompressedToken};
/// use std::io::Cursor;
///
/// let mut decoder = CompressedTokenDecoder::new();
/// # let encoded_data: Vec<u8> = vec![];
/// let mut cursor = Cursor::new(&encoded_data);
///
/// loop {
///     match decoder.recv_token(&mut cursor)? {
///         CompressedToken::Literal(data) => { /* write to output */ }
///         CompressedToken::BlockMatch(index) => { /* copy from basis */ }
///         CompressedToken::End => break,
///     }
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct CompressedTokenDecoder {
    inner: DecoderInner,
}

enum DecoderInner {
    Zlib(ZlibTokenDecoder),
}

impl Default for CompressedTokenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressedTokenDecoder {
    /// Creates a new zlib decoder.
    ///
    /// This constructor creates a zlib/zlibx decoder, matching the original
    /// rsync decompression behavior.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: DecoderInner::Zlib(ZlibTokenDecoder::new()),
        }
    }

    /// Resets the decoder for a new file.
    pub fn reset(&mut self) {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.reset(),
        }
    }

    /// Receives the next token from the stream.
    ///
    /// Reads and decodes the next token from the compressed stream.
    /// Automatically decompresses literal data and returns it in chunks.
    pub fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.recv_token(reader),
        }
    }

    /// Feeds block data into the decompressor's dictionary.
    ///
    /// Only needed for CPRES_ZLIB mode. Noop for zlibx, zstd, and lz4.
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.see_token(data),
        }
    }

    /// Configures zlibx mode for this decoder.
    ///
    /// When `true`, [`Self::see_token`] becomes a no-op.
    pub fn set_zlibx(&mut self, zlibx: bool) {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.set_zlibx(zlibx),
        }
    }

    /// Returns whether the decoder has been initialized (received first token).
    #[must_use]
    pub fn initialized(&self) -> bool {
        match &self.inner {
            DecoderInner::Zlib(dec) => dec.initialized,
        }
    }
}
