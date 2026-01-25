#![deny(unsafe_code)]
//! Signature block wire format for delta generation.
//!
//! This module implements serialization for file signatures used in the rsync
//! delta transfer protocol. Signatures consist of rolling and strong checksums
//! for fixed-size blocks of the basis file.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

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
/// use rsync_core::protocol::wire::SignatureBlock;
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
/// - Block count (varint) - Number of blocks in the file
/// - Block length (varint) - Size of each block in bytes
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
/// use rsync_core::protocol::wire::{SignatureBlock, write_signature};
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
    write_varint(writer, block_count as i32)?;
    write_varint(writer, block_length as i32)?;
    write_varint(writer, strong_sum_length as i32)?;

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
/// ```
/// use rsync_core::protocol::wire::read_signature;
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
    let block_count = read_varint(reader)? as u32;
    let block_length = read_varint(reader)? as u32;
    let strong_sum_length = read_varint(reader)? as u8;

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
