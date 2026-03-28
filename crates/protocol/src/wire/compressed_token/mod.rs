//! Compressed token wire format for file reconstruction.
//!
//! This module implements the compressed token format used by rsync when
//! compression is enabled (-z flag). It wraps literal data in DEFLATED_DATA
//! headers and encodes block match tokens efficiently.
//!
//! ## Wire Format (from upstream token.c)
//!
//! Flag bytes in compressed stream:
//! - `END_FLAG (0x00)` - end of file marker
//! - `TOKEN_LONG (0x20)` - followed by 32-bit token number
//! - `TOKENRUN_LONG (0x21)` - followed by 32-bit token + 16-bit run count
//! - `DEFLATED_DATA (0x40)` - + 6-bit high len, then low len byte
//! - `TOKEN_REL (0x80)` - + 6-bit relative token number
//! - `TOKENRUN_REL (0xC0)` - + 6-bit relative token + 16-bit run count
//!
//! ## DEFLATED_DATA Format
//!
//! ```text
//! Byte 0: 0x40 | (len >> 8)   // DEFLATED_DATA flag + upper 6 bits of length
//! Byte 1: len & 0xFF         // lower 8 bits of length
//! Bytes 2..: compressed data (raw deflate, no zlib header)
//! ```
//!
//! Maximum data count is 16383 (14 bits).
//!
//! ## Compression Model
//!
//! Upstream rsync maintains a single deflate stream per file transfer, using
//! `Z_SYNC_FLUSH` to produce incrementally decompressible output. This differs
//! from compressing each chunk independently with `Z_FINISH`.
//!
//! ## References
//!
//! - `token.c` lines 321-329: flag byte definitions
//! - `token.c:send_deflated_token()` lines 357-485
//! - `token.c:recv_deflated_token()` lines 500-630

mod decoder;
mod encoder;
#[cfg(feature = "lz4")]
mod lz4_codec;
mod zlib_codec;
#[cfg(feature = "zstd")]
mod zstd_codec;

#[cfg(test)]
mod tests;

use std::io::{self, Read, Write};

pub use self::decoder::CompressedTokenDecoder;
pub use self::encoder::CompressedTokenEncoder;

/// End of file marker.
///
/// Signals the end of a compressed token stream. No additional data follows.
pub const END_FLAG: u8 = 0x00;

/// Token encoding: absolute block index follows.
///
/// Followed by 32-bit token number (little-endian). Used when the relative
/// encoding can't represent the offset (> 63 blocks from last token).
pub const TOKEN_LONG: u8 = 0x20;

/// Token run encoding: absolute block index + run count follow.
///
/// Followed by 32-bit token number (LE) and 16-bit run count (LE).
/// Represents multiple consecutive block matches.
pub const TOKENRUN_LONG: u8 = 0x21;

/// Compressed literal data follows.
///
/// Format: `DEFLATED_DATA | (len >> 8)` where the low 6 bits contain
/// the upper 6 bits of the length. The next byte contains the low 8 bits.
/// Maximum length is 16383 (14 bits).
pub const DEFLATED_DATA: u8 = 0x40;

/// Token encoding: relative block index.
///
/// The low 6 bits contain the relative offset from the last token.
/// Used for offsets 0-63.
pub const TOKEN_REL: u8 = 0x80;

/// Token run encoding: relative block index + run count follow.
///
/// The low 6 bits contain the relative offset from the last token.
/// Followed by 16-bit run count (LE). Represents multiple consecutive
/// block matches with relative addressing.
pub const TOKENRUN_REL: u8 = 0xC0;

/// Maximum compressed data count (14 bits).
///
/// The DEFLATED_DATA header uses 6 bits from the first byte and 8 bits
/// from the second byte, allowing lengths up to 2^14 - 1 = 16383.
pub const MAX_DATA_COUNT: usize = 16383;

/// Chunk size for compression input (32 KiB).
///
/// Literal data is compressed in chunks of this size. Matches the
/// CHUNK_SIZE constant used in upstream rsync's token.c.
pub const CHUNK_SIZE: usize = 32 * 1024;

/// A token received from a compressed stream.
///
/// Represents the different types of operations that can appear in a compressed
/// delta stream. Tokens are produced by [`CompressedTokenDecoder::recv_token`].
///
/// # Examples
///
/// ```
/// use protocol::wire::CompressedToken;
///
/// // Pattern match on tokens
/// # let token = CompressedToken::Literal(vec![1, 2, 3]);
/// match token {
///     CompressedToken::Literal(data) => {
///         println!("Received {} bytes of literal data", data.len());
///     }
///     CompressedToken::BlockMatch(index) => {
///         println!("Copy block {}", index);
///     }
///     CompressedToken::End => {
///         println!("End of stream");
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressedToken {
    /// Literal data to write to output.
    ///
    /// The contained bytes should be written directly to the output file
    /// at the current position.
    Literal(Vec<u8>),

    /// Copy from block index in basis file.
    ///
    /// The receiver should copy one block's worth of data from the basis file
    /// starting at the given block index. The block size is determined by the
    /// signature header sent earlier in the protocol.
    BlockMatch(u32),

    /// End of file marker.
    ///
    /// Signals that the file transfer is complete. No more tokens will follow.
    End,
}

/// Writes a DEFLATED_DATA header.
///
/// Encodes the length into a 2-byte header: first byte is `DEFLATED_DATA | (len >> 8)`,
/// second byte is `len & 0xFF`. Maximum length is [`MAX_DATA_COUNT`] (14 bits).
///
/// Reference: upstream token.c lines 451-453.
#[inline]
fn write_deflated_data_header<W: Write>(writer: &mut W, len: usize) -> io::Result<()> {
    debug_assert!(len <= MAX_DATA_COUNT);
    let header = [DEFLATED_DATA | ((len >> 8) as u8), (len & 0xFF) as u8];
    writer.write_all(&header)
}

/// Writes compressed data as one or more DEFLATED_DATA blocks.
///
/// Splits `data` into [`MAX_DATA_COUNT`]-sized pieces, each prefixed with
/// a DEFLATED_DATA header. Upstream token.c writes output in the same
/// chunked fashion (lines 440-455).
fn write_deflated_data_pieces<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let piece_len = (data.len() - offset).min(MAX_DATA_COUNT);
        write_deflated_data_header(writer, piece_len)?;
        writer.write_all(&data[offset..offset + piece_len])?;
        offset += piece_len;
    }
    Ok(())
}

/// Reads the length from a DEFLATED_DATA header.
///
/// Decodes the 14-bit length from the DEFLATED_DATA header where the first byte
/// contains the flag and upper 6 bits, and the second byte (read from reader)
/// contains the lower 8 bits.
///
/// # Arguments
///
/// * `reader` - The input stream to read the second byte from
/// * `first_byte` - The first byte of the header (already read)
#[inline]
fn read_deflated_data_length<R: Read>(reader: &mut R, first_byte: u8) -> io::Result<usize> {
    let high = (first_byte & 0x3F) as usize;
    let mut low_buf = [0u8; 1];
    reader.read_exact(&mut low_buf)?;
    Ok((high << 8) | (low_buf[0] as usize))
}
