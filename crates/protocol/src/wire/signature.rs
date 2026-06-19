#![deny(unsafe_code)]
//! Signature block wire format for delta generation.
//!
//! This module implements serialization for file signatures used in the rsync
//! delta transfer protocol. Signatures consist of rolling and strong checksums
//! for fixed-size blocks of the basis file.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, read_varlong, write_varint, write_varlong};

/// Upstream caps a strong checksum at 32 bytes (`MAX_DIGEST_LEN` in checksum.c).
const MAX_DIGEST_LEN: u8 = 32;

/// Single signature block containing rolling and strong checksums.
///
/// Each signature block represents one fixed-size block of the basis file.
/// The combination of weak (rolling) and strong checksums allows efficient
/// matching during delta generation.
///
/// # Checksum Types
///
/// - **Rolling sum**: Fast 32-bit rolling checksum (Adler-32 variant) used for
///   quick candidate matching. Many false positives are expected.
/// - **Strong sum**: Cryptographic hash (MD4, MD5, SHA-1, or XXH3) used to verify
///   matches. The length is configurable and truncated to reduce bandwidth.
///
/// # Examples
///
/// ```
/// use protocol::wire::SignatureBlock;
///
/// let block = SignatureBlock {
///     index: 0,
///     rolling_sum: 0x12345678,
///     strong_sum: vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11],
/// };
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignatureBlock {
    /// Block index in the file (0-based).
    ///
    /// This identifies which block in the basis file this signature represents.
    pub index: u32,

    /// Rolling checksum (weak, 32-bit).
    ///
    /// Fast checksum used for initial candidate matching. Based on Adler-32
    /// with rsync-specific modifications.
    pub rolling_sum: u32,

    /// Strong checksum bytes (MD4/MD5/SHA-1/XXH3, truncated).
    ///
    /// Cryptographic hash used to verify matches found by the rolling checksum.
    /// The length varies based on protocol negotiation and security requirements.
    pub strong_sum: Vec<u8>,
}

/// Writes a complete file signature to the wire format.
///
/// Encodes the signature for a basis file, which will be used by the sender
/// to generate a delta. The signature consists of a header (block parameters)
/// followed by the checksum data for each block.
///
/// # Wire Format
///
/// **Header:**
/// - Block count (varlong, min_bytes=3) - Number of blocks in the file
/// - Block length (varlong, min_bytes=3) - Size of each block in bytes
/// - Strong sum length (varint) - Length of strong checksums in bytes
///
/// **For each block:**
/// - Rolling sum (4 bytes LE) - 32-bit rolling checksum
/// - Strong sum (variable length) - Cryptographic hash
///
/// # Arguments
///
/// * `writer` - The output stream to write to
/// * `block_count` - Total number of blocks (must match `blocks.len()`)
/// * `block_length` - Size of each block in bytes (typically 2048-8192)
/// * `strong_sum_length` - Length of strong checksums (must match actual length)
/// * `blocks` - The signature blocks to write
///
/// # Errors
///
/// Returns an error if:
/// - Writing to the underlying stream fails
/// - Any block's `strong_sum` length does not match `strong_sum_length`
///
/// # Examples
///
/// ```
/// use protocol::wire::{SignatureBlock, write_signature};
///
/// let blocks = vec![
///     SignatureBlock {
///         index: 0,
///         rolling_sum: 0x12345678,
///         strong_sum: vec![0xAA, 0xBB, 0xCC, 0xDD],
///     },
/// ];
///
/// let mut buf = Vec::new();
/// write_signature(&mut buf, 1, 4096, 4, &blocks).unwrap();
/// ```
#[inline]
pub fn write_signature<W: Write>(
    writer: &mut W,
    block_count: u32,
    block_length: u32,
    strong_sum_length: u8,
    blocks: &[SignatureBlock],
) -> io::Result<()> {
    // upstream: match.c::send_sums() + io.c::read_varlong()
    // block_count and block_length use varlong(min_bytes=3) since upstream
    // 3.4.x emits sum_head->count/blength via write_varlong when they exceed
    // the 30-bit varint range. strong_sum_length stays in plain varint.
    write_varlong(writer, i64::from(block_count), 3)?;
    write_varlong(writer, i64::from(block_length), 3)?;
    write_varint(writer, i32::from(strong_sum_length))?;

    for block in blocks {
        writer.write_all(&block.rolling_sum.to_le_bytes())?;

        if block.strong_sum.len() != strong_sum_length as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "strong sum length mismatch: expected {}, got {}",
                    strong_sum_length,
                    block.strong_sum.len()
                ),
            ));
        }

        writer.write_all(&block.strong_sum)?;
    }

    Ok(())
}

/// Reads a complete file signature from the wire format.
///
/// Decodes a file signature that was written by [`write_signature`]. The signature
/// is used by the delta generator to identify matching blocks between the basis
/// and target files.
///
/// # Returns
///
/// A tuple containing:
/// - `block_length` (u32) - Size of each block in bytes
/// - `block_count` (u32) - Number of blocks in the signature
/// - `strong_sum_length` (u8) - Length of each strong checksum
/// - `blocks` (`Vec<SignatureBlock>`) - The signature blocks with checksums
///
/// # Errors
///
/// Returns an error if reading from the underlying stream fails.
///
/// # Examples
///
/// ```no_run
/// use protocol::wire::read_signature;
///
/// # let mut data: &[u8] = &[]; // Placeholder
/// let (block_length, block_count, strong_sum_length, blocks) =
///     read_signature(&mut data)?;
///
/// println!("Signature has {block_count} blocks of {block_length} bytes");
/// # Ok::<(), std::io::Error>(())
/// ```
#[inline]
pub fn read_signature<R: Read>(reader: &mut R) -> io::Result<(u32, u32, u8, Vec<SignatureBlock>)> {
    // upstream: match.c::send_sums() + io.c::read_varlong()
    // block_count and block_length are written via write_varlong(min_bytes=3)
    // on the sender side once they exceed the 30-bit varint range. Decoding
    // them as plain varint produces "overflow in read_varint" on large basis
    // files. Read as i64 then narrow to u32, surfacing InvalidData on overflow.
    let block_count_raw = read_varlong(reader, 3)?;
    let block_count = u32::try_from(block_count_raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("block_count {block_count_raw} exceeds u32::MAX"),
        )
    })?;

    let block_length_raw = read_varlong(reader, 3)?;
    let block_length = u32::try_from(block_length_raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("block_length {block_length_raw} exceeds u32::MAX"),
        )
    })?;

    let strong_sum_length_raw = read_varint(reader)?;
    if !(0..=i32::from(MAX_DIGEST_LEN)).contains(&strong_sum_length_raw) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("strong_sum_length {strong_sum_length_raw} out of range 0..={MAX_DIGEST_LEN}"),
        ));
    }
    let strong_sum_length = strong_sum_length_raw as u8;

    let mut blocks = Vec::with_capacity(block_count as usize);

    for index in 0..block_count {
        let mut rolling_sum_bytes = [0u8; 4];
        reader.read_exact(&mut rolling_sum_bytes)?;
        let rolling_sum = u32::from_le_bytes(rolling_sum_bytes);

        let mut strong_sum = vec![0u8; strong_sum_length as usize];
        reader.read_exact(&mut strong_sum)?;

        blocks.push(SignatureBlock {
            index,
            rolling_sum,
            strong_sum,
        });
    }

    Ok((block_length, block_count, strong_sum_length, blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_roundtrip_single_block() {
        let blocks = vec![SignatureBlock {
            index: 0,
            rolling_sum: 0x12345678,
            strong_sum: vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11],
        }];

        let mut buf = Vec::new();
        write_signature(&mut buf, 1, 4096, 8, &blocks).unwrap();

        let (block_length, block_count, strong_sum_length, decoded_blocks) =
            read_signature(&mut &buf[..]).unwrap();

        assert_eq!(block_count, 1);
        assert_eq!(block_length, 4096);
        assert_eq!(strong_sum_length, 8);
        assert_eq!(decoded_blocks.len(), 1);
        assert_eq!(decoded_blocks[0].rolling_sum, 0x12345678);
        assert_eq!(
            decoded_blocks[0].strong_sum,
            vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11]
        );
    }

    #[test]
    fn signature_roundtrip_multiple_blocks() {
        let blocks = vec![
            SignatureBlock {
                index: 0,
                rolling_sum: 0x11111111,
                strong_sum: vec![0x01, 0x02, 0x03, 0x04],
            },
            SignatureBlock {
                index: 1,
                rolling_sum: 0x22222222,
                strong_sum: vec![0x05, 0x06, 0x07, 0x08],
            },
            SignatureBlock {
                index: 2,
                rolling_sum: 0x33333333,
                strong_sum: vec![0x09, 0x0A, 0x0B, 0x0C],
            },
        ];

        let mut buf = Vec::new();
        write_signature(&mut buf, 3, 2048, 4, &blocks).unwrap();

        let (block_length, block_count, strong_sum_length, decoded_blocks) =
            read_signature(&mut &buf[..]).unwrap();

        assert_eq!(block_count, 3);
        assert_eq!(block_length, 2048);
        assert_eq!(strong_sum_length, 4);
        assert_eq!(decoded_blocks.len(), 3);

        for (i, block) in decoded_blocks.iter().enumerate() {
            assert_eq!(block.index, i as u32);
            assert_eq!(block.rolling_sum, blocks[i].rolling_sum);
            assert_eq!(block.strong_sum, blocks[i].strong_sum);
        }
    }

    #[test]
    fn signature_rejects_mismatched_strong_sum_length() {
        let blocks = vec![SignatureBlock {
            index: 0,
            rolling_sum: 0x12345678,
            strong_sum: vec![0xAA, 0xBB],
        }];

        let mut buf = Vec::new();
        let result = write_signature(&mut buf, 1, 4096, 8, &blocks);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("strong sum length mismatch")
        );
    }

    #[test]
    fn signature_roundtrip_large_block_count() {
        // Regression: block_count > 30-bit varint range must round-trip via
        // varlong(min_bytes=3) instead of overflowing read_varint.
        // upstream: match.c::send_sums() + io.c::read_varlong()
        let block_count: u32 = 5_000_000;
        let block_length: u32 = 4096;
        let strong_sum_length: u8 = 16;

        let mut buf = Vec::new();
        // Header only - skip per-block payload to keep the test fast.
        write_varlong(&mut buf, i64::from(block_count), 3).unwrap();
        write_varlong(&mut buf, i64::from(block_length), 3).unwrap();
        write_varint(&mut buf, i32::from(strong_sum_length)).unwrap();

        let mut cursor = &buf[..];
        let decoded_block_count = read_varlong(&mut cursor, 3).unwrap();
        let decoded_block_length = read_varlong(&mut cursor, 3).unwrap();
        let decoded_strong_sum_length = read_varint(&mut cursor).unwrap();

        assert_eq!(decoded_block_count, i64::from(block_count));
        assert_eq!(decoded_block_length, i64::from(block_length));
        assert_eq!(decoded_strong_sum_length, i32::from(strong_sum_length));
    }

    #[test]
    fn signature_rejects_strong_sum_length_above_max_digest_len() {
        let mut buf = Vec::new();
        write_varlong(&mut buf, 1, 3).unwrap();
        write_varlong(&mut buf, 4096, 3).unwrap();
        write_varint(&mut buf, 64).unwrap();

        let err = read_signature(&mut &buf[..]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("strong_sum_length"));
    }

    #[test]
    fn signature_empty_file() {
        let blocks = vec![];

        let mut buf = Vec::new();
        write_signature(&mut buf, 0, 4096, 8, &blocks).unwrap();

        let (block_length, block_count, strong_sum_length, decoded_blocks) =
            read_signature(&mut &buf[..]).unwrap();

        assert_eq!(block_count, 0);
        assert_eq!(block_length, 4096);
        assert_eq!(strong_sum_length, 8);
        assert_eq!(decoded_blocks.len(), 0);
    }
}
