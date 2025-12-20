#![deny(unsafe_code)]
//! Delta token wire format for file reconstruction.
//!
//! This module implements serialization for delta operations used to reconstruct
//! files from a basis file. Delta streams consist of literal data writes and
//! copy operations that reference blocks in the basis file.
//!
//! ## Wire Format (Upstream rsync compatibility)
//!
//! Upstream rsync uses a simple token format in `token.c:simple_send_token()`:
//!
//! - **Literal data**: `write_int(length)` (positive i32 LE) followed by raw bytes
//!   - Large literals are chunked into CHUNK_SIZE (32KB) pieces
//! - **Block match**: `write_int(-(token+1))` where token is the block index
//!   - Example: block 0 = -1, block 1 = -2, etc.
//! - **End marker**: `write_int(-1)` when sum_count=0 (whole-file transfer)
//!
//! References:
//! - `token.c:simple_send_token()` line ~305
//! - `io.c:write_int()` line ~2082

use std::io::{self, Read, Write};

use crate::varint::{read_varint, write_varint};

/// Maximum chunk size for literal data (matches upstream CHUNK_SIZE).
pub const CHUNK_SIZE: usize = 32 * 1024;

// ============================================================================
// Upstream rsync wire format functions
// ============================================================================

/// Writes a 4-byte signed little-endian integer (upstream `write_int()`).
///
/// Reference: `io.c:write_int()` line ~2082
#[inline]
pub fn write_int<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Reads a 4-byte signed little-endian integer (upstream `read_int()`).
#[inline]
pub fn read_int<R: Read>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Writes literal data in upstream token format.
///
/// Large data is chunked into CHUNK_SIZE (32KB) pieces.
/// Each chunk is: `write_int(length)` followed by raw bytes.
///
/// Reference: `token.c:simple_send_token()` lines 307-314
pub fn write_token_literal<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let chunk_len = remaining.min(CHUNK_SIZE);
        write_int(writer, chunk_len as i32)?;
        writer.write_all(&data[offset..offset + chunk_len])?;
        offset += chunk_len;
    }
    Ok(())
}

/// Writes a block match token in upstream format.
///
/// Format: `write_int(-(block_index + 1))`
/// Example: block 0 = -1, block 1 = -2, etc.
///
/// Reference: `token.c:simple_send_token()` line 316
#[inline]
pub fn write_token_block_match<W: Write>(writer: &mut W, block_index: u32) -> io::Result<()> {
    let token = -((block_index as i32) + 1);
    write_int(writer, token)
}

/// Writes the end-of-file marker (token value 0).
///
/// This corresponds to calling send_token with token=-1, which writes
/// `-((-1)+1) = 0` to signal end of delta stream.
///
/// Reference: `match.c:matched()` line 408, `token.c:simple_send_token()` line 316
#[inline]
pub fn write_token_end<W: Write>(writer: &mut W) -> io::Result<()> {
    write_int(writer, 0)
}

/// Writes a complete delta stream in upstream wire format.
///
/// This is for whole-file transfers where we just send all data as literals.
/// Format:
/// - For each chunk of data: `write_int(chunk_len)` + raw bytes
/// - End marker: `write_int(0)`
///
/// Reference: `match.c:match_sums()` lines 404-408 (whole file case)
pub fn write_whole_file_delta<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    write_token_literal(writer, data)?;
    write_token_end(writer)
}

/// Writes a delta stream from DeltaOp slice in upstream wire format.
///
/// Format for each operation:
/// - Literal: `write_int(chunk_len)` + raw bytes (chunked to 32KB)
/// - Copy (block match): `write_int(-(block_index + 1))`
///
/// Ends with `write_int(0)` as end marker.
///
/// Note: Copy operations include block_index but length is determined
/// by the block size from the checksum header, not sent in the token.
pub fn write_token_stream<W: Write>(writer: &mut W, ops: &[DeltaOp]) -> io::Result<()> {
    for op in ops {
        match op {
            DeltaOp::Literal(data) => {
                write_token_literal(writer, data)?;
            }
            DeltaOp::Copy { block_index, .. } => {
                write_token_block_match(writer, *block_index)?;
            }
        }
    }
    write_token_end(writer)
}

/// Reads a token from upstream wire format.
///
/// Returns:
/// - `Ok(Some(n))` where n > 0: literal data of n bytes follows
/// - `Ok(Some(n))` where n < 0: block match at index `-(n+1)`
/// - `Ok(None)`: end of stream (token value 0)
pub fn read_token<R: Read>(reader: &mut R) -> io::Result<Option<i32>> {
    let token = read_int(reader)?;
    if token == 0 {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

// ============================================================================
// Internal wire format (opcode-based, for backward compatibility)
// ============================================================================

/// Delta operation for file reconstruction.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DeltaOp {
    /// Write literal bytes to output.
    Literal(Vec<u8>),
    /// Copy bytes from basis file at given block index.
    Copy {
        /// Block index in basis file.
        block_index: u32,
        /// Number of bytes to copy.
        length: u32,
    },
}

/// Writes a delta operation to the wire format.
///
/// Format:
/// - Op code (1 byte):
///   - 0x00 = Literal
///   - 0x01 = Copy
/// - For Literal:
///   - Length (varint)
///   - Data bytes
/// - For Copy:
///   - Block index (varint)
///   - Length (varint)
pub fn write_delta_op<W: Write>(writer: &mut W, op: &DeltaOp) -> io::Result<()> {
    match op {
        DeltaOp::Literal(data) => {
            writer.write_all(&[0x00])?;
            write_varint(writer, data.len() as i32)?;
            writer.write_all(data)?;
        }
        DeltaOp::Copy {
            block_index,
            length,
        } => {
            writer.write_all(&[0x01])?;
            write_varint(writer, *block_index as i32)?;
            write_varint(writer, *length as i32)?;
        }
    }
    Ok(())
}

/// Reads a delta operation from the wire format.
pub fn read_delta_op<R: Read>(reader: &mut R) -> io::Result<DeltaOp> {
    let mut opcode = [0u8; 1];
    reader.read_exact(&mut opcode)?;

    match opcode[0] {
        0x00 => {
            let length = read_varint(reader)? as usize;
            let mut data = vec![0u8; length];
            reader.read_exact(&mut data)?;
            Ok(DeltaOp::Literal(data))
        }
        0x01 => {
            let block_index = read_varint(reader)? as u32;
            let length = read_varint(reader)? as u32;
            Ok(DeltaOp::Copy {
                block_index,
                length,
            })
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid delta opcode: 0x{other:02X}"),
        )),
    }
}

/// Writes a complete delta stream to the wire format.
///
/// Format:
/// - Operation count (varint)
/// - For each operation:
///   - Delta operation (see `write_delta_op`)
pub fn write_delta<W: Write>(writer: &mut W, ops: &[DeltaOp]) -> io::Result<()> {
    write_varint(writer, ops.len() as i32)?;
    for op in ops {
        write_delta_op(writer, op)?;
    }
    Ok(())
}

/// Reads a complete delta stream from the wire format.
pub fn read_delta<R: Read>(reader: &mut R) -> io::Result<Vec<DeltaOp>> {
    let count = read_varint(reader)? as usize;
    let mut ops = Vec::with_capacity(count);

    for _ in 0..count {
        ops.push(read_delta_op(reader)?);
    }

    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_op_roundtrip_literal() {
        let op = DeltaOp::Literal(vec![0x01, 0x02, 0x03, 0x04, 0x05]);

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op).unwrap();

        let decoded = read_delta_op(&mut &buf[..]).unwrap();

        assert_eq!(decoded, op);
    }

    #[test]
    fn delta_op_roundtrip_copy() {
        let op = DeltaOp::Copy {
            block_index: 42,
            length: 4096,
        };

        let mut buf = Vec::new();
        write_delta_op(&mut buf, &op).unwrap();

        let decoded = read_delta_op(&mut &buf[..]).unwrap();

        assert_eq!(decoded, op);
    }

    #[test]
    fn delta_stream_roundtrip_mixed_ops() {
        let ops = vec![
            DeltaOp::Literal(vec![0x01, 0x02, 0x03]),
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(vec![0x04, 0x05]),
            DeltaOp::Copy {
                block_index: 5,
                length: 2048,
            },
            DeltaOp::Literal(vec![0x06]),
        ];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), ops.len());
        for (i, (decoded_op, expected_op)) in decoded.iter().zip(ops.iter()).enumerate() {
            assert_eq!(decoded_op, expected_op, "mismatch at op {i}");
        }
    }

    #[test]
    fn delta_stream_empty() {
        let ops: Vec<DeltaOp> = vec![];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), 0);
    }

    #[test]
    fn delta_op_rejects_invalid_opcode() {
        let buf = [0xFF, 0x00, 0x00, 0x00];
        let result = read_delta_op(&mut &buf[..]);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid delta opcode")
        );
    }

    #[test]
    fn delta_stream_single_large_literal() {
        let data = vec![0x42; 65536];
        let ops = vec![DeltaOp::Literal(data.clone())];

        let mut buf = Vec::new();
        write_delta(&mut buf, &ops).unwrap();

        let decoded = read_delta(&mut &buf[..]).unwrap();

        assert_eq!(decoded.len(), 1);
        if let DeltaOp::Literal(decoded_data) = &decoded[0] {
            assert_eq!(decoded_data.len(), 65536);
            assert_eq!(decoded_data, &data);
        } else {
            panic!("expected Literal operation");
        }
    }

    // ========================================================================
    // Upstream wire format tests
    // ========================================================================

    #[test]
    fn write_int_roundtrip() {
        let values = [0i32, 1, -1, 127, -128, 1000, -1000, i32::MAX, i32::MIN];
        for &value in &values {
            let mut buf = Vec::new();
            write_int(&mut buf, value).unwrap();
            assert_eq!(buf.len(), 4);
            let decoded = read_int(&mut &buf[..]).unwrap();
            assert_eq!(decoded, value, "roundtrip failed for {value}");
        }
    }

    #[test]
    fn write_int_little_endian() {
        let mut buf = Vec::new();
        write_int(&mut buf, 0x12345678).unwrap();
        assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn write_token_literal_small() {
        let data = b"hello";
        let mut buf = Vec::new();
        write_token_literal(&mut buf, data).unwrap();

        // Should be: write_int(5) + "hello"
        assert_eq!(buf.len(), 4 + 5);
        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 5);
        assert_eq!(&buf[4..], b"hello");
    }

    #[test]
    fn write_token_literal_chunked() {
        // Data larger than CHUNK_SIZE should be split
        let data = vec![0x42u8; CHUNK_SIZE + 100];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // First chunk: write_int(CHUNK_SIZE) + CHUNK_SIZE bytes
        // Second chunk: write_int(100) + 100 bytes
        assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + 100);

        let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len1, CHUNK_SIZE as i32);

        let second_header_start = 4 + CHUNK_SIZE;
        let len2 = i32::from_le_bytes([
            buf[second_header_start],
            buf[second_header_start + 1],
            buf[second_header_start + 2],
            buf[second_header_start + 3],
        ]);
        assert_eq!(len2, 100);
    }

    #[test]
    fn write_token_block_match_encoding() {
        // Block 0 should be encoded as -1
        let mut buf = Vec::new();
        write_token_block_match(&mut buf, 0).unwrap();
        assert_eq!(buf, (-1i32).to_le_bytes());

        // Block 1 should be encoded as -2
        buf.clear();
        write_token_block_match(&mut buf, 1).unwrap();
        assert_eq!(buf, (-2i32).to_le_bytes());

        // Block 42 should be encoded as -43
        buf.clear();
        write_token_block_match(&mut buf, 42).unwrap();
        assert_eq!(buf, (-43i32).to_le_bytes());
    }

    #[test]
    fn write_token_end_is_zero() {
        let mut buf = Vec::new();
        write_token_end(&mut buf).unwrap();
        assert_eq!(buf, [0, 0, 0, 0]);
    }

    #[test]
    fn write_whole_file_delta_format() {
        let data = b"test data";
        let mut buf = Vec::new();
        write_whole_file_delta(&mut buf, data).unwrap();

        // Should be: write_int(9) + "test data" + write_int(0)
        assert_eq!(buf.len(), 4 + 9 + 4);

        // Check length header
        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 9);

        // Check data
        assert_eq!(&buf[4..13], b"test data");

        // Check end marker
        let end = i32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]);
        assert_eq!(end, 0);
    }

    #[test]
    fn read_token_parses_literals_and_blocks() {
        // Literal: positive value
        let mut buf = 17i32.to_le_bytes().to_vec();
        let token = read_token(&mut &buf[..]).unwrap();
        assert_eq!(token, Some(17));

        // Block match: negative value (block 0 = -1)
        buf = (-1i32).to_le_bytes().to_vec();
        let token = read_token(&mut &buf[..]).unwrap();
        assert_eq!(token, Some(-1));

        // End marker: zero
        buf = 0i32.to_le_bytes().to_vec();
        let token = read_token(&mut &buf[..]).unwrap();
        assert_eq!(token, None);
    }

    #[test]
    fn write_token_stream_mixed_ops() {
        let ops = vec![
            DeltaOp::Literal(b"hello".to_vec()),
            DeltaOp::Copy {
                block_index: 0,
                length: 1024,
            },
            DeltaOp::Literal(b"world".to_vec()),
        ];

        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops).unwrap();

        // Parse and verify structure
        let mut cursor = &buf[..];

        // First literal: write_int(5) + "hello"
        let len1 = read_int(&mut cursor).unwrap();
        assert_eq!(len1, 5);
        let mut data1 = [0u8; 5];
        cursor.read_exact(&mut data1).unwrap();
        assert_eq!(&data1, b"hello");

        // Block match for block 0: write_int(-1)
        let block = read_int(&mut cursor).unwrap();
        assert_eq!(block, -1);

        // Second literal: write_int(5) + "world"
        let len2 = read_int(&mut cursor).unwrap();
        assert_eq!(len2, 5);
        let mut data2 = [0u8; 5];
        cursor.read_exact(&mut data2).unwrap();
        assert_eq!(&data2, b"world");

        // End marker: write_int(0)
        let end = read_int(&mut cursor).unwrap();
        assert_eq!(end, 0);

        // Should be at end
        assert!(cursor.is_empty());
    }
}
