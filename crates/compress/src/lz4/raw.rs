//! Raw LZ4 block compression for rsync wire protocol compatibility.
//!
//! This module implements LZ4 compression using raw blocks with rsync-specific
//! framing, matching upstream rsync 3.4.1's `token.c` implementation.
//!
//! # Wire Format
//!
//! Upstream rsync uses a 2-byte header followed by raw LZ4 compressed data:
//!
//! ```text
//! [DEFLATED_DATA + (size >> 8)] [size & 0xFF] [compressed data...]
//! ```
//!
//! Where:
//! - `DEFLATED_DATA = 0x40` indicates compressed data follows
//! - The size field is 14 bits (max 16383 bytes)
//! - The compressed data is raw LZ4 block output (no frame header/footer)
//!
//! # Differences from Frame Format
//!
//! Unlike the [`super::frame`] module which uses LZ4 frame format with magic
//! bytes, block checksums, and content checksums, this module produces raw
//! compressed blocks suitable for the rsync wire protocol.

use std::io::{self, Read, Write};

use lz4_flex::block::{compress_into, decompress_into, get_maximum_output_size};

/// Flag byte indicating compressed data follows (upstream token.c DEFLATED_DATA).
pub const DEFLATED_DATA: u8 = 0x40;

/// Maximum compressed block size in bytes (14-bit field, upstream MAX_DATA_COUNT).
pub const MAX_BLOCK_SIZE: usize = 16383;

/// Minimum header size for a compressed block.
pub const HEADER_SIZE: usize = 2;

/// Error returned when compression or decompression fails.
#[derive(Debug, thiserror::Error)]
pub enum RawLz4Error {
    /// Input data exceeds maximum block size.
    #[error("input size {0} exceeds maximum block size {MAX_BLOCK_SIZE}")]
    InputTooLarge(usize),

    /// Compressed output exceeds maximum block size.
    #[error("compressed size {0} exceeds maximum block size {MAX_BLOCK_SIZE}")]
    CompressedTooLarge(usize),

    /// Output buffer is too small.
    #[error("output buffer too small: need {needed}, have {available}")]
    BufferTooSmall {
        /// Number of bytes needed.
        needed: usize,
        /// Number of bytes available.
        available: usize,
    },

    /// Invalid header format.
    #[error("invalid header: expected DEFLATED_DATA flag 0x40, got 0x{0:02x}")]
    InvalidHeader(u8),

    /// Decompression failed.
    #[error("decompression failed: {0}")]
    DecompressFailed(#[from] lz4_flex::block::DecompressError),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Encodes the 2-byte rsync header for a compressed block.
///
/// The header format is: `[DEFLATED_DATA + (size >> 8)] [size & 0xFF]`
#[inline]
pub const fn encode_header(compressed_size: usize) -> [u8; 2] {
    [
        DEFLATED_DATA | ((compressed_size >> 8) as u8 & 0x3F),
        (compressed_size & 0xFF) as u8,
    ]
}

/// Decodes the compressed size from a 2-byte rsync header.
///
/// Returns `None` if the header doesn't have the DEFLATED_DATA flag set.
#[inline]
pub const fn decode_header(header: [u8; 2]) -> Option<usize> {
    if (header[0] & 0xC0) != DEFLATED_DATA {
        return None;
    }
    let size = ((header[0] as usize & 0x3F) << 8) | (header[1] as usize);
    Some(size)
}

/// Checks if a flag byte indicates compressed data.
#[inline]
pub const fn is_deflated_data(flag: u8) -> bool {
    (flag & 0xC0) == DEFLATED_DATA
}

/// Compresses a block and writes it with rsync wire protocol framing.
///
/// Writes the 2-byte header followed by raw LZ4 compressed data to `output`.
/// Returns the total number of bytes written (header + compressed data).
///
/// # Errors
///
/// Returns an error if:
/// - The input exceeds `MAX_BLOCK_SIZE`
/// - The compressed output exceeds `MAX_BLOCK_SIZE`
/// - The output buffer is too small
pub fn compress_block(input: &[u8], output: &mut [u8]) -> Result<usize, RawLz4Error> {
    if input.len() > MAX_BLOCK_SIZE {
        return Err(RawLz4Error::InputTooLarge(input.len()));
    }

    let max_compressed = get_maximum_output_size(input.len());
    let needed = HEADER_SIZE + max_compressed;

    if output.len() < needed {
        return Err(RawLz4Error::BufferTooSmall {
            needed,
            available: output.len(),
        });
    }

    // Compress into buffer after header space
    let compressed_size = compress_into(input, &mut output[HEADER_SIZE..])?;

    if compressed_size > MAX_BLOCK_SIZE {
        return Err(RawLz4Error::CompressedTooLarge(compressed_size));
    }

    // Write header
    let header = encode_header(compressed_size);
    output[0] = header[0];
    output[1] = header[1];

    Ok(HEADER_SIZE + compressed_size)
}

/// Compresses a block to a new Vec with rsync wire protocol framing.
///
/// Returns a Vec containing the 2-byte header followed by raw LZ4 compressed data.
///
/// # Errors
///
/// Returns an error if:
/// - The input exceeds `MAX_BLOCK_SIZE`
/// - The compressed output exceeds `MAX_BLOCK_SIZE`
pub fn compress_block_to_vec(input: &[u8]) -> Result<Vec<u8>, RawLz4Error> {
    if input.len() > MAX_BLOCK_SIZE {
        return Err(RawLz4Error::InputTooLarge(input.len()));
    }

    let max_compressed = get_maximum_output_size(input.len());
    let mut output = vec![0u8; HEADER_SIZE + max_compressed];

    let total = compress_block(input, &mut output)?;
    output.truncate(total);
    Ok(output)
}

/// Decompresses a block from rsync wire protocol framing.
///
/// Reads the 2-byte header to determine compressed size, then decompresses
/// the raw LZ4 data into `output`.
///
/// Returns the number of decompressed bytes written.
///
/// # Arguments
///
/// * `input` - Buffer starting with the 2-byte header
/// * `output` - Buffer to write decompressed data (must be large enough)
///
/// # Errors
///
/// Returns an error if:
/// - The input is too short for the header
/// - The header doesn't have the DEFLATED_DATA flag
/// - The output buffer is too small
/// - Decompression fails
pub fn decompress_block(input: &[u8], output: &mut [u8]) -> Result<usize, RawLz4Error> {
    if input.len() < HEADER_SIZE {
        return Err(RawLz4Error::BufferTooSmall {
            needed: HEADER_SIZE,
            available: input.len(),
        });
    }

    let header = [input[0], input[1]];
    let compressed_size = decode_header(header).ok_or(RawLz4Error::InvalidHeader(header[0]))?;

    let total_input = HEADER_SIZE + compressed_size;
    if input.len() < total_input {
        return Err(RawLz4Error::BufferTooSmall {
            needed: total_input,
            available: input.len(),
        });
    }

    let compressed = &input[HEADER_SIZE..total_input];
    let decompressed_size = decompress_into(compressed, output)?;

    Ok(decompressed_size)
}

/// Decompresses a block from rsync wire protocol framing to a new Vec.
///
/// # Arguments
///
/// * `input` - Buffer starting with the 2-byte header
/// * `max_decompressed_size` - Maximum expected decompressed size
///
/// # Errors
///
/// Returns an error if decompression fails or the header is invalid.
pub fn decompress_block_to_vec(
    input: &[u8],
    max_decompressed_size: usize,
) -> Result<Vec<u8>, RawLz4Error> {
    let mut output = vec![0u8; max_decompressed_size];
    let size = decompress_block(input, &mut output)?;
    output.truncate(size);
    Ok(output)
}

/// Writes a compressed block to a writer with rsync wire protocol framing.
///
/// # Errors
///
/// Returns an error if compression fails or writing fails.
pub fn write_compressed_block<W: Write>(
    input: &[u8],
    writer: &mut W,
) -> Result<usize, RawLz4Error> {
    let compressed = compress_block_to_vec(input)?;
    writer.write_all(&compressed)?;
    Ok(compressed.len())
}

/// Reads and decompresses a block from a reader with rsync wire protocol framing.
///
/// # Arguments
///
/// * `reader` - Source to read compressed data from
/// * `max_decompressed_size` - Maximum expected decompressed size
///
/// # Errors
///
/// Returns an error if reading fails, the header is invalid, or decompression fails.
pub fn read_compressed_block<R: Read>(
    reader: &mut R,
    max_decompressed_size: usize,
) -> Result<Vec<u8>, RawLz4Error> {
    // Read header
    let mut header = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header)?;

    let compressed_size = decode_header(header).ok_or(RawLz4Error::InvalidHeader(header[0]))?;

    // Read compressed data
    let mut compressed = vec![0u8; compressed_size];
    reader.read_exact(&mut compressed)?;

    // Decompress
    let mut output = vec![0u8; max_decompressed_size];
    let size = decompress_into(&compressed, &mut output)?;
    output.truncate(size);

    Ok(output)
}

/// Returns the size of compressed data from a header, if valid.
///
/// This is useful for reading just the header to determine how many more
/// bytes to read for the compressed payload.
#[inline]
pub const fn compressed_size_from_header(header: [u8; 2]) -> Option<usize> {
    decode_header(header)
}

// Implement From for io::Error conversion
impl From<lz4_flex::block::CompressError> for RawLz4Error {
    fn from(e: lz4_flex::block::CompressError) -> Self {
        RawLz4Error::Io(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }
}

impl From<RawLz4Error> for io::Error {
    fn from(e: RawLz4Error) -> Self {
        match e {
            RawLz4Error::Io(io_err) => io_err,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_encode_decode_roundtrip() {
        for size in [0, 1, 100, 1000, 16383] {
            let header = encode_header(size);
            let decoded = decode_header(header).expect("valid header");
            assert_eq!(decoded, size, "size {size} roundtrip failed");
        }
    }

    #[test]
    fn header_has_correct_flag() {
        let header = encode_header(100);
        assert!(
            is_deflated_data(header[0]),
            "header should have DEFLATED_DATA flag"
        );
    }

    #[test]
    fn invalid_header_rejected() {
        // TOKEN_REL flag (0x80) should not be decoded as compressed data
        let header = [0x80, 0x00];
        assert!(decode_header(header).is_none());

        // END_FLAG (0x00) should not be decoded as compressed data
        let header = [0x00, 0x00];
        assert!(decode_header(header).is_none());
    }

    #[test]
    fn compress_decompress_roundtrip() {
        let input = b"Hello, rsync wire protocol!";
        let compressed = compress_block_to_vec(input).expect("compress");

        // Verify header
        assert!(is_deflated_data(compressed[0]));

        // Decompress
        let decompressed = decompress_block_to_vec(&compressed, input.len()).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn compress_decompress_large_block() {
        // Test near max block size
        let input = vec![b'x'; 16000];
        let compressed = compress_block_to_vec(&input).expect("compress");
        let decompressed = decompress_block_to_vec(&compressed, input.len()).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn compress_decompress_compressible_data() {
        // Highly compressible data
        let input = vec![0u8; 10000];
        let compressed = compress_block_to_vec(&input).expect("compress");

        // Should compress well
        assert!(
            compressed.len() < input.len() / 2,
            "zeros should compress significantly"
        );

        let decompressed = decompress_block_to_vec(&compressed, input.len()).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn input_too_large_rejected() {
        let input = vec![0u8; MAX_BLOCK_SIZE + 1];
        let result = compress_block_to_vec(&input);
        assert!(matches!(result, Err(RawLz4Error::InputTooLarge(_))));
    }

    #[test]
    fn read_write_roundtrip() {
        let input = b"streaming roundtrip test";
        let mut buffer = Vec::new();

        write_compressed_block(input, &mut buffer).expect("write");

        let mut cursor = std::io::Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, input.len()).expect("read");

        assert_eq!(decompressed, input);
    }

    #[test]
    fn empty_input() {
        let input = b"";
        let compressed = compress_block_to_vec(input).expect("compress");
        let decompressed = decompress_block_to_vec(&compressed, 0).expect("decompress");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn buffer_compress_decompress() {
        let input = b"buffer-based compression test";
        let mut compressed = vec![0u8; HEADER_SIZE + get_maximum_output_size(input.len())];

        let compressed_len = compress_block(input, &mut compressed).expect("compress");
        compressed.truncate(compressed_len);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len =
            decompress_block(&compressed, &mut decompressed).expect("decompress");

        assert_eq!(&decompressed[..decompressed_len], input.as_slice());
    }
}
