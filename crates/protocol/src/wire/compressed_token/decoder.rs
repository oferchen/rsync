//! Algorithm-agnostic decoder for compressed token wire format.
//!
//! Dispatches to the appropriate per-algorithm codec (zlib, zstd, lz4)
//! based on the negotiated compression algorithm.
//!
//! - upstream: token.c:recv_compressed_token() (algorithm dispatch)

use std::io::{self, Read};

use super::CompressedToken;
#[cfg(feature = "lz4")]
use super::lz4_codec::Lz4TokenDecoder;
use super::zlib_codec::ZlibTokenDecoder;
#[cfg(feature = "zstd")]
use super::zstd_codec::ZstdTokenDecoder;

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
    #[cfg(feature = "zstd")]
    Zstd(ZstdTokenDecoder),
    #[cfg(feature = "lz4")]
    Lz4(Lz4TokenDecoder),
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

    /// Creates a new zstd decoder.
    ///
    /// upstream: token.c:recv_zstd_token()
    #[cfg(feature = "zstd")]
    pub fn new_zstd() -> io::Result<Self> {
        Ok(Self {
            inner: DecoderInner::Zstd(ZstdTokenDecoder::new()?),
        })
    }

    /// Creates a new LZ4 decoder.
    ///
    /// upstream: token.c:recv_compressed_token() (SUPPORT_LZ4)
    #[cfg(feature = "lz4")]
    #[must_use]
    pub fn new_lz4() -> Self {
        Self {
            inner: DecoderInner::Lz4(Lz4TokenDecoder::new()),
        }
    }

    /// Resets the decoder for a new file.
    ///
    /// For zlib/lz4, reinitializes the decompression context (per-file streams).
    /// For zstd, only resets token index and buffer state - the decompression
    /// context is preserved across files (one continuous stream).
    ///
    /// upstream: token.c:807-810 (zstd r_init), token.c:496 (zlib inflateReset)
    pub fn reset(&mut self) {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.reset(),
            #[cfg(feature = "zstd")]
            DecoderInner::Zstd(dec) => dec.reset(),
            #[cfg(feature = "lz4")]
            DecoderInner::Lz4(dec) => dec.reset(),
        }
    }

    /// Receives the next token from the stream.
    ///
    /// Reads and decodes the next token from the compressed stream.
    /// Automatically decompresses literal data and returns it in chunks.
    pub fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.recv_token(reader),
            #[cfg(feature = "zstd")]
            DecoderInner::Zstd(dec) => dec.recv_token(reader),
            #[cfg(feature = "lz4")]
            DecoderInner::Lz4(dec) => dec.recv_token(reader),
        }
    }

    /// Feeds block data into the decompressor's dictionary.
    ///
    /// Only needed for CPRES_ZLIB mode. Noop for zlibx, zstd, and lz4.
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.see_token(data),
            #[cfg(feature = "zstd")]
            DecoderInner::Zstd(dec) => dec.see_token(data),
            #[cfg(feature = "lz4")]
            DecoderInner::Lz4(dec) => dec.see_token(data),
        }
    }

    /// Configures zlibx mode for this decoder.
    ///
    /// When `true`, [`Self::see_token`] becomes a no-op. No-op for non-zlib algorithms.
    pub fn set_zlibx(&mut self, zlibx: bool) {
        match &mut self.inner {
            DecoderInner::Zlib(dec) => dec.set_zlibx(zlibx),
            #[cfg(feature = "zstd")]
            DecoderInner::Zstd(_) => {}
            #[cfg(feature = "lz4")]
            DecoderInner::Lz4(_) => {}
        }
    }

    /// Returns whether the decoder has been initialized (received first token).
    #[must_use]
    pub fn initialized(&self) -> bool {
        match &self.inner {
            DecoderInner::Zlib(dec) => dec.initialized,
            #[cfg(feature = "zstd")]
            DecoderInner::Zstd(dec) => dec.initialized,
            #[cfg(feature = "lz4")]
            DecoderInner::Lz4(dec) => dec.initialized,
        }
    }
}
