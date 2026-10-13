//! Zstd per-token codec for compressed token wire format.
//!
//! Implements the zstd-specific encoder and decoder used by CPRES_ZSTD mode.
//! Unlike zlib, zstd does not use sync marker stripping/restoration, and
//! `see_token` is always a noop (no dictionary synchronization needed).
//!
//! ## Flush boundary alignment
//!
//! Upstream rsync uses `ZSTD_e_flush` at each token boundary (block match or
//! end-of-file). Literal data fed between token boundaries is compressed with
//! `ZSTD_e_continue` (no flush point). When a token arrives, the encoder
//! flushes, producing a decompressible boundary that the receiver can
//! decompress before processing the token.
//!
//! Compressed output is accumulated in a `MAX_DATA_COUNT`-sized buffer and
//! only written as a DEFLATED_DATA block when the buffer is full or a flush
//! completes. Deviation from upstream: upstream writes a block after every
//! `ZSTD_compressStream2()` call that produced output (token.c:815-821), so
//! block boundaries can differ; the decompressed bytes are identical.
//!
//! - upstream: token.c:send_zstd_token() lines 735-835
//! - upstream: token.c:recv_zstd_token() lines 839-924

mod decoder;
mod encoder;

#[cfg(test)]
mod tests;

pub(in crate::wire::compressed_token) use self::decoder::ZstdTokenDecoder;
pub(in crate::wire::compressed_token) use self::encoder::ZstdTokenEncoder;
