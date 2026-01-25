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
/// This is the fundamental integer encoding used throughout the rsync protocol
/// for token values, block indices, and lengths.
///
/// # Wire Format
///
/// Writes exactly 4 bytes in little-endian byte order:
/// ```text
/// [byte0, byte1, byte2, byte3] where value = byte0 + (byte1 << 8) + (byte2 << 16) + (byte3 << 24)
/// ```
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::write_int;
///
/// let mut buf = Vec::new();
/// write_int(&mut buf, 0x12345678).unwrap();
/// assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]); // Little-endian
/// ```
///
/// Reference: `io.c:write_int()` line ~2082
#[inline]
pub fn write_int<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Reads a 4-byte signed little-endian integer (upstream `read_int()`).
///
/// This is the counterpart to [`write_int`], reading back values written
/// by rsync's `write_int()` function.
///
/// # Wire Format
///
/// Reads exactly 4 bytes in little-endian byte order.
///
/// # Errors
///
/// Returns an error if fewer than 4 bytes are available in the reader.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::read_int;
///
/// let data = [0x78, 0x56, 0x34, 0x12];
/// let value = read_int(&mut &data[..]).unwrap();
/// assert_eq!(value, 0x12345678);
/// ```
///
/// Reference: `io.c:read_int()` line ~2091
#[inline]
pub fn read_int<R: Read>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Writes literal data in upstream token format.
///
/// Large data is automatically chunked into CHUNK_SIZE (32KB) pieces.
/// Each chunk is written as `write_int(length)` followed by raw bytes.
///
/// # Wire Format
///
/// For data of length N:
/// - If N ≤ 32KB: `write_int(N)` + N bytes
/// - If N > 32KB: Multiple chunks of format `write_int(chunk_len)` + chunk_bytes
///
/// # Arguments
///
/// * `writer` - The output stream to write to
/// * `data` - The literal data to send (will be chunked automatically)
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::write_token_literal;
///
/// let mut buf = Vec::new();
/// write_token_literal(&mut buf, b"hello").unwrap();
///
/// // Produces: write_int(5) + b"hello"
/// assert_eq!(buf.len(), 4 + 5); // 4 bytes for length + 5 bytes of data
/// ```
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
/// Encodes a reference to a block in the basis file. The receiver should copy
/// one block's worth of data from the basis file at the given block index.
///
/// # Wire Format
///
/// Block matches are encoded as negative integers: `write_int(-(block_index + 1))`
///
/// Examples:
/// - block 0 → -1
/// - block 1 → -2
/// - block 42 → -43
///
/// # Arguments
///
/// * `writer` - The output stream to write to
/// * `block_index` - The 0-based index of the block to copy
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// Reference: `token.c:simple_send_token()` line 316
#[inline]
pub fn write_token_block_match<W: Write>(writer: &mut W, block_index: u32) -> io::Result<()> {
    let token = -((block_index as i32) + 1);
    write_int(writer, token)
}

/// Writes the end-of-file marker (token value 0).
///
/// Signals the end of a delta stream. The receiver should stop reading
/// tokens after receiving this marker.
///
/// # Wire Format
///
/// Writes `write_int(0)`. This corresponds to calling send_token with token=-1,
/// which encodes as `-((-1)+1) = 0`.
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// Reference: `match.c:matched()` line 408, `token.c:simple_send_token()` line 316
#[inline]
pub fn write_token_end<W: Write>(writer: &mut W) -> io::Result<()> {
    write_int(writer, 0)
}

/// Writes a complete delta stream for a whole-file transfer.
///
/// This is used when there is no basis file available (e.g., when the receiver
/// doesn't have the file). The entire file is sent as literal data with no
/// block matches.
///
/// # Wire Format
///
/// - Literal data (chunked): `write_int(chunk_len)` + raw bytes (repeated as needed)
/// - End marker: `write_int(0)`
///
/// # Arguments
///
/// * `writer` - The output stream to write to
/// * `data` - The complete file data to send
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// Reference: `match.c:match_sums()` lines 404-408 (whole file case)
pub fn write_whole_file_delta<W: Write>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    write_token_literal(writer, data)?;
    write_token_end(writer)
}

/// Writes a delta stream from DeltaOp slice in upstream wire format.
///
/// Converts a sequence of delta operations into the token-based wire format
/// used by rsync. This is the primary function for sending delta data to
/// an rsync receiver.
///
/// # Wire Format
///
/// For each operation:
/// - **Literal**: `write_int(chunk_len)` + raw bytes (auto-chunked to 32KB)
/// - **Copy** (block match): `write_int(-(block_index + 1))`
///
/// Ends with `write_int(0)` as end marker.
///
/// # Note
///
/// Copy operations only send the block_index. The number of bytes to copy
/// is determined by the block size from the checksum header that was sent
/// earlier in the protocol exchange.
///
/// # Arguments
///
/// * `writer` - The output stream to write to
/// * `ops` - The delta operations to encode
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::{DeltaOp, write_token_stream};
///
/// let ops = vec![
///     DeltaOp::Literal(b"hello".to_vec()),
///     DeltaOp::Copy { block_index: 0, length: 1024 },
///     DeltaOp::Literal(b"world".to_vec()),
/// ];
///
/// let mut buf = Vec::new();
/// write_token_stream(&mut buf, &ops).unwrap();
/// ```
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
/// This function reads a single token value and interprets it according to
/// rsync's token encoding rules. The caller is responsible for reading any
/// associated data (for literals) based on the returned value.
///
/// # Returns
///
/// - `Ok(Some(n))` where n > 0: Literal data of n bytes follows
/// - `Ok(Some(n))` where n < 0: Block match at index `-(n+1)`
/// - `Ok(None)`: End of stream (token value 0)
///
/// # Errors
///
/// Returns an error if reading from the underlying stream fails.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::read_token;
///
/// // Read a literal token
/// let data = 17i32.to_le_bytes();
/// let token = read_token(&mut &data[..]).unwrap();
/// assert_eq!(token, Some(17)); // 17 bytes of literal data follow
///
/// // Read a block match token
/// let data = (-1i32).to_le_bytes();
/// let token = read_token(&mut &data[..]).unwrap();
/// assert_eq!(token, Some(-1)); // Block 0: -((-1) + 1) = 0
///
/// // Read end marker
/// let data = 0i32.to_le_bytes();
/// let token = read_token(&mut &data[..]).unwrap();
/// assert_eq!(token, None); // End of stream
/// ```
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
///
/// Represents the internal format for delta operations (not the upstream wire format).
/// This opcode-based format is used for backward compatibility with earlier versions
/// of this implementation.
///
/// For upstream rsync compatibility, use the token-based functions like
/// [`write_token_stream`] and [`read_token`] instead.
///
/// # Examples
///
/// ```
/// use rsync_core::protocol::wire::DeltaOp;
///
/// // Create a literal operation
/// let lit = DeltaOp::Literal(vec![1, 2, 3, 4, 5]);
///
/// // Create a copy operation
/// let copy = DeltaOp::Copy {
///     block_index: 0,
///     length: 4096,
/// };
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DeltaOp {
    /// Write literal bytes to output.
    ///
    /// The contained data should be written directly to the output stream
    /// at the current position.
    Literal(Vec<u8>),

    /// Copy bytes from basis file at given block index.
    ///
    /// The receiver should copy `length` bytes from the basis file starting
    /// at the position indicated by `block_index * block_size`, where
    /// `block_size` comes from the signature header.
    Copy {
        /// Block index in basis file (0-based).
        block_index: u32,
        /// Number of bytes to copy.
        length: u32,
    },
}

/// Writes a delta operation to the internal wire format.
///
/// This is the opcode-based format used internally for backward compatibility.
/// For upstream rsync compatibility, use [`write_token_stream`] instead.
///
/// # Wire Format
///
/// **Opcode** (1 byte):
/// - `0x00` = Literal
/// - `0x01` = Copy
///
/// **For Literal** (`0x00`):
/// - Length (varint)
/// - Data bytes
///
/// **For Copy** (`0x01`):
/// - Block index (varint)
/// - Length (varint)
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
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

/// Reads a delta operation from the internal wire format.
///
/// This is the counterpart to [`write_delta_op`], decoding the opcode-based
/// format. For upstream rsync compatibility, use [`read_token`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - Reading from the underlying stream fails
/// - An invalid opcode is encountered (not 0x00 or 0x01)
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

/// Writes a complete delta stream to the internal wire format.
///
/// This is the opcode-based format used internally. For upstream rsync
/// compatibility, use [`write_token_stream`] instead.
///
/// # Wire Format
///
/// - Operation count (varint)
/// - For each operation:
///   - Delta operation (see [`write_delta_op`])
///
/// # Errors
///
/// Returns an error if writing to the underlying stream fails.
pub fn write_delta<W: Write>(writer: &mut W, ops: &[DeltaOp]) -> io::Result<()> {
    write_varint(writer, ops.len() as i32)?;
    for op in ops {
        write_delta_op(writer, op)?;
    }
    Ok(())
}

/// Reads a complete delta stream from the internal wire format.
///
/// This is the counterpart to [`write_delta`], decoding the opcode-based format.
/// For upstream rsync compatibility, use [`read_token`] instead.
///
/// # Errors
///
/// Returns an error if:
/// - Reading from the underlying stream fails
/// - An invalid opcode is encountered in any delta operation
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
