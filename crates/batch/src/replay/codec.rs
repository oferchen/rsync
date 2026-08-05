//! Zlib decoder construction for the compressed batch token stream.
//!
//! upstream: compat.c:417-418 getenv_nstr() - when `write_batch` is set the
//! negotiated compression list is forced to "zlib", so a batch file's token
//! stream is ALWAYS zlib-compressed (never zstd or lz4). Batch replay therefore
//! pins the codec to zlib; it never sniffs the compressed payload to pick a
//! codec.

use protocol::wire::CompressedTokenDecoder;

/// Builds the zlib [`CompressedTokenDecoder`] used for compressed batch replay.
///
/// The codec is fixed at construction - upstream pins batch compression to
/// zlib, so replay never inspects the payload to choose an algorithm.
///
/// upstream: compat.c:417-418 getenv_nstr() forces `write_batch` transfers to
/// negotiate "zlib"; compat.c:195 - read-batch defaults `do_compression` to
/// CPRES_ZLIB.
///
/// Sets `zlibx=false` so `see_token()` feeds matched block data into the
/// inflate dictionary. Without this, inflate fails with "invalid distance too
/// far back". `protocol_version` gates that dictionary advance to match the
/// sender that produced the batch.
///
/// upstream: token.c:710 see_deflate_token() `if (protocol_version >= 31) buf += blklen;`
pub(super) fn create_compressed_decoder(protocol_version: u32) -> CompressedTokenDecoder {
    let mut decoder = CompressedTokenDecoder::new();
    // Batch was recorded at this protocol version; gate see_token's dictionary
    // advance to match the sender that produced the batch.
    decoder.set_protocol_version(protocol_version);
    decoder.set_zlibx(false);
    decoder
}
