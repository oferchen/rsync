#![deny(unsafe_code)]
//! Signature block wire format for delta generation.
//!
//! This module implements serialization for file signatures used in the rsync
//! delta transfer protocol. Signatures consist of rolling and strong checksums
//! for fixed-size blocks of the basis file.

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

/// Single signature block containing rolling and strong checksums.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SignatureBlock {
    /// Block index in the file (0-based).
    pub index: u32,
    /// Rolling checksum (weak, 32-bit).
    pub rolling_sum: u32,
    /// Strong checksum bytes (MD4/MD5/SHA1/XXH, truncated).
    pub strong_sum: Vec<u8>,
}

/// Writes a complete file signature to the wire format.
///
/// Format:
/// - Block count (varint)
/// - Block length (varint)
/// - Strong sum length (varint)
/// - For each block:
///   - Rolling sum (4 bytes LE)
///   - Strong sum (variable length)
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
        assert_eq!(decoded_blocks[0].strong_sum, vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11]);
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
        assert!(result.unwrap_err().to_string().contains("strong sum length mismatch"));
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
