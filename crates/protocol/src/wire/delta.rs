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
/// use protocol::wire::write_int;
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
/// use protocol::wire::read_int;
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
/// use protocol::wire::write_token_literal;
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
/// use protocol::wire::{DeltaOp, write_token_stream};
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
/// use protocol::wire::read_token;
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
/// use protocol::wire::DeltaOp;
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
            let raw_length = read_varint(reader)?;
            if raw_length < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "negative literal length in delta op",
                ));
            }
            let length = raw_length as usize;
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
    let raw_count = read_varint(reader)?;
    if raw_count < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "negative delta operation count",
        ));
    }
    let count = raw_count as usize;
    // Cap pre-allocation to avoid OOM on malformed input; the loop still
    // iterates `count` times but will hit EOF naturally if the data is short.
    let mut ops = Vec::with_capacity(count.min(1024));

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

    // ========================================================================
    // Oversized literal block tests (Task #79)
    // ========================================================================

    /// Helper function to decode a token stream and reconstruct literal data.
    ///
    /// Returns (literals, block_indices) where literals is concatenated literal data
    /// and block_indices contains the block references encountered.
    fn decode_token_stream(data: &[u8]) -> io::Result<(Vec<u8>, Vec<u32>)> {
        let mut cursor = data;
        let mut literals = Vec::new();
        let mut block_indices = Vec::new();

        loop {
            match read_token(&mut cursor)? {
                None => break, // End marker
                Some(token) if token > 0 => {
                    // Literal data
                    let len = token as usize;
                    let mut chunk = vec![0u8; len];
                    cursor.read_exact(&mut chunk)?;
                    literals.extend_from_slice(&chunk);
                }
                Some(token) => {
                    // Block match: token is -(block_index + 1)
                    let block_index = (-(token + 1)) as u32;
                    block_indices.push(block_index);
                }
            }
        }

        Ok((literals, block_indices))
    }

    #[test]
    fn delta_oversized_literal_exactly_chunk_size() {
        // Test literal exactly at CHUNK_SIZE boundary (should be single chunk)
        let data = vec![0xABu8; CHUNK_SIZE];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be exactly one chunk: write_int(CHUNK_SIZE) + CHUNK_SIZE bytes
        assert_eq!(buf.len(), 4 + CHUNK_SIZE);

        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, CHUNK_SIZE as i32);

        // Verify all data bytes are correct
        assert!(buf[4..].iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn delta_oversized_literal_one_byte_over_chunk_size() {
        // Test literal at CHUNK_SIZE + 1 (boundary condition - should split into 2 chunks)
        let data = vec![0xCDu8; CHUNK_SIZE + 1];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be two chunks:
        // First: write_int(CHUNK_SIZE) + CHUNK_SIZE bytes
        // Second: write_int(1) + 1 byte
        assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + 1);

        let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len1, CHUNK_SIZE as i32);

        let second_header_start = 4 + CHUNK_SIZE;
        let len2 = i32::from_le_bytes([
            buf[second_header_start],
            buf[second_header_start + 1],
            buf[second_header_start + 2],
            buf[second_header_start + 3],
        ]);
        assert_eq!(len2, 1);

        // Verify the single byte in second chunk
        assert_eq!(buf[second_header_start + 4], 0xCD);
    }

    #[test]
    fn delta_oversized_literal_multiple_chunks() {
        // Test literal spanning exactly 3 full chunks
        let data = vec![0xEFu8; CHUNK_SIZE * 3];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be 3 chunks, each: write_int(CHUNK_SIZE) + CHUNK_SIZE bytes
        assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 3);

        // Verify each chunk header
        for i in 0..3 {
            let offset = i * (4 + CHUNK_SIZE);
            let len = i32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            assert_eq!(len, CHUNK_SIZE as i32, "chunk {i} header mismatch");
        }
    }

    #[test]
    fn delta_oversized_literal_multiple_chunks_with_remainder() {
        // Test 2.5 chunks worth of data
        let remainder = CHUNK_SIZE / 2;
        let data = vec![0x12u8; CHUNK_SIZE * 2 + remainder];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be 3 chunks:
        // Two full: write_int(CHUNK_SIZE) + CHUNK_SIZE bytes each
        // One partial: write_int(remainder) + remainder bytes
        assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2 + 4 + remainder);

        // Verify first two chunk headers
        for i in 0..2 {
            let offset = i * (4 + CHUNK_SIZE);
            let len = i32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            assert_eq!(len, CHUNK_SIZE as i32);
        }

        // Verify third (partial) chunk header
        let third_offset = 2 * (4 + CHUNK_SIZE);
        let len3 = i32::from_le_bytes([
            buf[third_offset],
            buf[third_offset + 1],
            buf[third_offset + 2],
            buf[third_offset + 3],
        ]);
        assert_eq!(len3, remainder as i32);
    }

    #[test]
    fn delta_oversized_literal_reconstruction() {
        // Test that oversized literal data can be correctly reconstructed
        // by reading the chunked token stream

        // Create distinctive pattern that helps verify correct reconstruction
        let size = CHUNK_SIZE * 2 + 1234;
        let mut data = Vec::with_capacity(size);
        for i in 0..size {
            data.push((i % 256) as u8);
        }

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        // Decode and reconstruct
        let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

        assert!(block_indices.is_empty(), "should have no block references");
        assert_eq!(reconstructed.len(), data.len());
        assert_eq!(
            reconstructed, data,
            "reconstructed data should match original"
        );
    }

    #[test]
    fn delta_oversized_literal_reconstruction_exact_multiple() {
        // Test reconstruction when data is exact multiple of CHUNK_SIZE
        let size = CHUNK_SIZE * 4;
        let data: Vec<u8> = (0..size).map(|i| (i as u8).wrapping_mul(7)).collect();

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();

        assert_eq!(reconstructed.len(), data.len());
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn delta_oversized_literal_mixed_with_blocks() {
        // Test oversized literals mixed with block references
        let large_literal = vec![0xAAu8; CHUNK_SIZE + 500];
        let small_literal = b"small".to_vec();

        let ops = vec![
            DeltaOp::Literal(large_literal.clone()),
            DeltaOp::Copy {
                block_index: 0,
                length: 4096,
            },
            DeltaOp::Literal(small_literal.clone()),
            DeltaOp::Copy {
                block_index: 5,
                length: 4096,
            },
            DeltaOp::Literal(vec![0xBBu8; CHUNK_SIZE * 2]),
        ];

        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops).unwrap();

        let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

        // Verify block references
        assert_eq!(block_indices, vec![0, 5]);

        // Verify total literal size
        let expected_literal_size = large_literal.len() + small_literal.len() + CHUNK_SIZE * 2;
        assert_eq!(reconstructed_literals.len(), expected_literal_size);

        // Verify first large literal
        assert_eq!(
            &reconstructed_literals[..large_literal.len()],
            &large_literal[..]
        );

        // Verify small literal after first large
        let small_start = large_literal.len();
        assert_eq!(
            &reconstructed_literals[small_start..small_start + small_literal.len()],
            &small_literal[..]
        );

        // Verify last large literal
        let last_start = small_start + small_literal.len();
        assert!(
            reconstructed_literals[last_start..]
                .iter()
                .all(|&b| b == 0xBB)
        );
    }

    #[test]
    fn delta_oversized_literal_via_whole_file() {
        // Test whole file transfer with oversized data
        let size = CHUNK_SIZE * 3 + 789;
        let data: Vec<u8> = (0..size).map(|i| ((i * 13) % 256) as u8).collect();

        let mut buf = Vec::new();
        write_whole_file_delta(&mut buf, &data).unwrap();

        let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

        assert!(block_indices.is_empty());
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn delta_oversized_literal_empty() {
        // Edge case: empty literal should not produce any chunks
        let data: Vec<u8> = vec![];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Empty literal produces no output (no chunks written)
        assert!(buf.is_empty());
    }

    #[test]
    fn delta_oversized_literal_single_byte() {
        // Edge case: single byte literal
        let data = vec![0x42u8];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        assert_eq!(buf.len(), 4 + 1);
        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, 1);
        assert_eq!(buf[4], 0x42);
    }

    #[test]
    fn delta_oversized_literal_chunk_boundary_minus_one() {
        // Test literal at CHUNK_SIZE - 1 (just under boundary)
        let data = vec![0x99u8; CHUNK_SIZE - 1];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be single chunk
        assert_eq!(buf.len(), 4 + CHUNK_SIZE - 1);

        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len, (CHUNK_SIZE - 1) as i32);
    }

    #[test]
    fn delta_oversized_literal_very_large() {
        // Test very large literal (1MB) to ensure no overflow issues
        let size = 1024 * 1024; // 1MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        // Verify we can reconstruct it
        let (reconstructed, _) = decode_token_stream(&buf).unwrap();
        assert_eq!(reconstructed.len(), size);
        assert_eq!(reconstructed, data);

        // Verify number of chunks (1MB / 32KB = 32 chunks)
        let expected_chunks = size.div_ceil(CHUNK_SIZE);
        assert_eq!(expected_chunks, 32);
    }

    #[test]
    fn delta_oversized_literal_data_integrity() {
        // Test that chunking preserves data integrity with varied content
        // Use a pattern that would expose any off-by-one errors

        let size = CHUNK_SIZE * 2 + CHUNK_SIZE / 2;
        let mut data = Vec::with_capacity(size);

        // First chunk: ascending bytes
        for i in 0..CHUNK_SIZE {
            data.push((i % 256) as u8);
        }
        // Second chunk: descending bytes
        for i in 0..CHUNK_SIZE {
            data.push((255 - (i % 256)) as u8);
        }
        // Third partial chunk: alternating pattern
        for i in 0..(CHUNK_SIZE / 2) {
            data.push(if i % 2 == 0 { 0xAA } else { 0x55 });
        }

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();

        // Verify chunk boundaries maintained data correctly
        for (i, &byte) in reconstructed.iter().enumerate().take(CHUNK_SIZE) {
            assert_eq!(byte, (i % 256) as u8, "first chunk byte {i} mismatch");
        }
        for (i, &byte) in reconstructed
            .iter()
            .skip(CHUNK_SIZE)
            .enumerate()
            .take(CHUNK_SIZE)
        {
            assert_eq!(
                byte,
                (255 - (i % 256)) as u8,
                "second chunk byte {i} mismatch"
            );
        }
        for i in 0..(CHUNK_SIZE / 2) {
            let expected = if i % 2 == 0 { 0xAA } else { 0x55 };
            assert_eq!(
                reconstructed[CHUNK_SIZE * 2 + i],
                expected,
                "third chunk byte {i} mismatch"
            );
        }
    }

    #[test]
    fn delta_stream_with_consecutive_oversized_literals() {
        // Test multiple consecutive oversized literals in a stream
        let literal1 = vec![0x11u8; CHUNK_SIZE + 100];
        let literal2 = vec![0x22u8; CHUNK_SIZE * 2 + 200];
        let literal3 = vec![0x33u8; CHUNK_SIZE + 50];

        let ops = vec![
            DeltaOp::Literal(literal1.clone()),
            DeltaOp::Literal(literal2.clone()),
            DeltaOp::Literal(literal3.clone()),
        ];

        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops).unwrap();

        let (reconstructed, block_indices) = decode_token_stream(&buf).unwrap();

        assert!(block_indices.is_empty());

        let total_size = literal1.len() + literal2.len() + literal3.len();
        assert_eq!(reconstructed.len(), total_size);

        // Verify each literal's content
        let mut offset = 0;
        assert!(
            reconstructed[offset..offset + literal1.len()]
                .iter()
                .all(|&b| b == 0x11)
        );
        offset += literal1.len();

        assert!(
            reconstructed[offset..offset + literal2.len()]
                .iter()
                .all(|&b| b == 0x22)
        );
        offset += literal2.len();

        assert!(
            reconstructed[offset..offset + literal3.len()]
                .iter()
                .all(|&b| b == 0x33)
        );
    }

    // ========================================================================
    // Additional CHUNK_SIZE boundary splitting tests
    // ========================================================================

    #[test]
    fn chunk_boundary_exact_double_chunk_size() {
        // Test literal exactly at 2 * CHUNK_SIZE boundary
        let data = vec![0xDDu8; CHUNK_SIZE * 2];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be exactly 2 chunks
        assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2);

        // Verify first chunk
        let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len1, CHUNK_SIZE as i32);

        // Verify second chunk
        let offset2 = 4 + CHUNK_SIZE;
        let len2 = i32::from_le_bytes([
            buf[offset2],
            buf[offset2 + 1],
            buf[offset2 + 2],
            buf[offset2 + 3],
        ]);
        assert_eq!(len2, CHUNK_SIZE as i32);

        // Verify all data
        assert!(buf[4..4 + CHUNK_SIZE].iter().all(|&b| b == 0xDD));
        assert!(buf[offset2 + 4..].iter().all(|&b| b == 0xDD));
    }

    #[test]
    fn chunk_boundary_two_chunks_plus_one_byte() {
        // Test literal at 2 * CHUNK_SIZE + 1 (should split into 3 chunks)
        let data = vec![0xEEu8; CHUNK_SIZE * 2 + 1];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        // Should be 3 chunks: full, full, 1 byte
        assert_eq!(buf.len(), (4 + CHUNK_SIZE) * 2 + 4 + 1);

        // Verify chunk sizes
        let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len1, CHUNK_SIZE as i32);

        let offset2 = 4 + CHUNK_SIZE;
        let len2 = i32::from_le_bytes([
            buf[offset2],
            buf[offset2 + 1],
            buf[offset2 + 2],
            buf[offset2 + 3],
        ]);
        assert_eq!(len2, CHUNK_SIZE as i32);

        let offset3 = (4 + CHUNK_SIZE) * 2;
        let len3 = i32::from_le_bytes([
            buf[offset3],
            buf[offset3 + 1],
            buf[offset3 + 2],
            buf[offset3 + 3],
        ]);
        assert_eq!(len3, 1);

        // Verify the single byte in third chunk
        assert_eq!(buf[offset3 + 4], 0xEE);
    }

    #[test]
    fn chunk_boundary_split_verification() {
        // Verify that chunks are split exactly at boundaries with distinctive content
        let mut data = Vec::new();

        // First chunk: pattern A
        for i in 0..CHUNK_SIZE {
            data.push((i & 0xFF) as u8);
        }

        // Second chunk: pattern B
        for i in 0..CHUNK_SIZE {
            data.push(((i + 128) & 0xFF) as u8);
        }

        // Third chunk (partial): pattern C
        data.extend(std::iter::repeat_n(0xCC, 1000));

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();
        assert_eq!(reconstructed, data);

        // Verify each chunk maintained its pattern
        for (i, &byte) in reconstructed.iter().enumerate().take(CHUNK_SIZE) {
            assert_eq!(byte, (i & 0xFF) as u8, "first chunk mismatch at {i}");
        }
        for (i, &byte) in reconstructed
            .iter()
            .skip(CHUNK_SIZE)
            .enumerate()
            .take(CHUNK_SIZE)
        {
            assert_eq!(
                byte,
                ((i + 128) & 0xFF) as u8,
                "second chunk mismatch at {i}"
            );
        }
        for (i, &byte) in reconstructed
            .iter()
            .skip(CHUNK_SIZE * 2)
            .enumerate()
            .take(1000)
        {
            assert_eq!(byte, 0xCC, "third chunk mismatch at {i}");
        }
    }

    #[test]
    fn chunk_boundary_reassembly_interleaved_with_blocks() {
        // Test that chunked literals are correctly reassembled when interleaved with blocks
        let literal1 = vec![0xF1u8; CHUNK_SIZE + 10];
        let literal2 = vec![0xF2u8; CHUNK_SIZE * 3 + 20];

        let ops = vec![
            DeltaOp::Literal(vec![0xAAu8; 100]), // Small literal
            DeltaOp::Copy {
                block_index: 0,
                length: 4096,
            },
            DeltaOp::Literal(literal1.clone()), // Chunked literal
            DeltaOp::Copy {
                block_index: 1,
                length: 4096,
            },
            DeltaOp::Copy {
                block_index: 2,
                length: 4096,
            },
            DeltaOp::Literal(literal2.clone()), // Larger chunked literal
            DeltaOp::Copy {
                block_index: 3,
                length: 4096,
            },
        ];

        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops).unwrap();

        let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

        // Verify blocks are in correct order
        assert_eq!(block_indices, vec![0, 1, 2, 3]);

        // Verify total literal size
        let expected_size = 100 + literal1.len() + literal2.len();
        assert_eq!(reconstructed_literals.len(), expected_size);

        // Verify content of each literal section
        assert!(reconstructed_literals[..100].iter().all(|&b| b == 0xAA));
        assert!(
            reconstructed_literals[100..100 + literal1.len()]
                .iter()
                .all(|&b| b == 0xF1)
        );
        assert!(
            reconstructed_literals[100 + literal1.len()..]
                .iter()
                .all(|&b| b == 0xF2)
        );
    }

    #[test]
    fn chunk_boundary_max_i32_size_handling() {
        // Test that chunk sizes fit in i32 (CHUNK_SIZE should be well under i32::MAX)
        assert!(CHUNK_SIZE < i32::MAX as usize);

        let data = vec![0x77u8; CHUNK_SIZE];
        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();

        let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert!(len > 0, "chunk size should be positive");
        assert_eq!(len, CHUNK_SIZE as i32);
    }

    #[test]
    fn chunk_boundary_many_small_chunks_edge() {
        // Test 10 chunks worth of data (320KB) to ensure no accumulation issues
        let size = CHUNK_SIZE * 10;
        let mut data = Vec::with_capacity(size);
        for i in 0..size {
            data.push((i / CHUNK_SIZE) as u8); // Each chunk has different byte value
        }

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();
        assert_eq!(reconstructed.len(), size);
        assert_eq!(reconstructed, data);

        // Verify each chunk has the correct pattern
        for chunk_idx in 0..10 {
            let start = chunk_idx * CHUNK_SIZE;
            let end = start + CHUNK_SIZE;
            let expected_value = chunk_idx as u8;
            assert!(
                reconstructed[start..end]
                    .iter()
                    .all(|&b| b == expected_value),
                "chunk {chunk_idx} has wrong value"
            );
        }
    }

    #[test]
    fn chunk_boundary_off_by_one_before_boundary() {
        // Test sizes around CHUNK_SIZE boundary to catch off-by-one errors
        for offset in [2, 1, 0] {
            let size = CHUNK_SIZE - offset;
            let data = vec![0x88u8; size];
            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();

            // All should be single chunk
            assert_eq!(buf.len(), 4 + size, "size {size} failed");

            let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            assert_eq!(len, size as i32);
        }
    }

    #[test]
    fn chunk_boundary_off_by_one_after_boundary() {
        // Test sizes just after CHUNK_SIZE boundary
        for offset in [0, 1, 2] {
            let size = CHUNK_SIZE + offset;
            let data = vec![0x99u8; size];
            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();

            if offset == 0 {
                // Exactly at boundary: single chunk
                assert_eq!(buf.len(), 4 + CHUNK_SIZE);
                let len = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                assert_eq!(len, CHUNK_SIZE as i32);
            } else {
                // Past boundary: two chunks
                assert_eq!(buf.len(), 4 + CHUNK_SIZE + 4 + offset);

                let len1 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                assert_eq!(len1, CHUNK_SIZE as i32);

                let offset2 = 4 + CHUNK_SIZE;
                let len2 = i32::from_le_bytes([
                    buf[offset2],
                    buf[offset2 + 1],
                    buf[offset2 + 2],
                    buf[offset2 + 3],
                ]);
                assert_eq!(len2, offset as i32);
            }
        }
    }

    #[test]
    fn chunk_boundary_streaming_reconstruction() {
        // Test that chunks can be read as they arrive (streaming scenario)
        let data: Vec<u8> = (0..CHUNK_SIZE * 2 + 500)
            .map(|i| (i % 251) as u8) // Use prime to avoid patterns
            .collect();

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        // Simulate streaming: reconstruct one token at a time
        let mut cursor = &buf[..];
        let mut reconstructed = Vec::new();

        loop {
            match read_token(&mut cursor).unwrap() {
                None => break,
                Some(token) if token > 0 => {
                    let len = token as usize;
                    let mut chunk = vec![0u8; len];
                    cursor.read_exact(&mut chunk).unwrap();
                    reconstructed.extend_from_slice(&chunk);
                }
                Some(_) => panic!("unexpected block token"),
            }
        }

        assert_eq!(reconstructed, data);
    }

    #[test]
    fn chunk_boundary_alternating_pattern_integrity() {
        // Ensure chunk boundaries don't corrupt alternating bit patterns
        let size = CHUNK_SIZE * 2 + 100;
        let data: Vec<u8> = (0..size)
            .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
            .collect();

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();

        assert_eq!(reconstructed.len(), data.len());
        for (i, (&reconstructed_byte, &original_byte)) in
            reconstructed.iter().zip(data.iter()).enumerate()
        {
            assert_eq!(reconstructed_byte, original_byte, "mismatch at byte {i}");
        }
    }

    #[test]
    fn chunk_boundary_zero_filled_chunks() {
        // Test that zero-filled data is correctly chunked (may compress differently)
        let data = vec![0x00u8; CHUNK_SIZE * 3];

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();
        assert_eq!(reconstructed.len(), data.len());
        assert!(reconstructed.iter().all(|&b| b == 0x00));
    }

    #[test]
    fn chunk_boundary_all_different_bytes() {
        // Test with maximum entropy data (all 256 byte values repeated)
        let mut data = Vec::new();
        let repetitions = (CHUNK_SIZE * 2) / 256 + 1;
        for _ in 0..repetitions {
            for b in 0..=255u8 {
                data.push(b);
            }
        }
        data.truncate(CHUNK_SIZE * 2 + 50);

        let mut buf = Vec::new();
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        let (reconstructed, _) = decode_token_stream(&buf).unwrap();
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn chunk_boundary_stress_test_many_operations() {
        // Stress test with many operations mixing small and large literals
        let ops = vec![
            DeltaOp::Literal(vec![0x01u8; CHUNK_SIZE + 1]),
            DeltaOp::Copy {
                block_index: 0,
                length: 4096,
            },
            DeltaOp::Literal(vec![0x02u8; CHUNK_SIZE * 2]),
            DeltaOp::Copy {
                block_index: 1,
                length: 4096,
            },
            DeltaOp::Literal(vec![0x03u8; CHUNK_SIZE - 1]),
            DeltaOp::Copy {
                block_index: 2,
                length: 4096,
            },
            DeltaOp::Literal(vec![0x04u8; CHUNK_SIZE]),
            DeltaOp::Copy {
                block_index: 3,
                length: 4096,
            },
            DeltaOp::Literal(vec![0x05u8; CHUNK_SIZE + 100]),
        ];

        let mut buf = Vec::new();
        write_token_stream(&mut buf, &ops).unwrap();

        let (reconstructed_literals, block_indices) = decode_token_stream(&buf).unwrap();

        // Verify blocks
        assert_eq!(block_indices, vec![0, 1, 2, 3]);

        // Verify literals were reconstructed correctly
        let expected_size = (CHUNK_SIZE + 1)
            + (CHUNK_SIZE * 2)
            + (CHUNK_SIZE - 1)
            + CHUNK_SIZE
            + (CHUNK_SIZE + 100);
        assert_eq!(reconstructed_literals.len(), expected_size);

        // Verify content integrity
        let mut offset = 0;
        let sizes = [
            (CHUNK_SIZE + 1, 0x01u8),
            (CHUNK_SIZE * 2, 0x02u8),
            (CHUNK_SIZE - 1, 0x03u8),
            (CHUNK_SIZE, 0x04u8),
            (CHUNK_SIZE + 100, 0x05u8),
        ];

        for (size, expected_byte) in sizes.iter() {
            assert!(
                reconstructed_literals[offset..offset + size]
                    .iter()
                    .all(|&b| b == *expected_byte),
                "literal at offset {offset} with size {size} has wrong value"
            );
            offset += size;
        }
    }

    #[test]
    fn chunk_boundary_write_read_symmetry() {
        // Verify that write_token_literal and read_token are perfectly symmetric
        let test_sizes = vec![
            0,
            1,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            CHUNK_SIZE * 2,
            CHUNK_SIZE * 2 + 1,
            CHUNK_SIZE * 5 + 12345,
        ];

        for size in test_sizes {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();

            let mut buf = Vec::new();
            write_token_literal(&mut buf, &data).unwrap();
            write_token_end(&mut buf).unwrap();

            let (reconstructed, _) = decode_token_stream(&buf).unwrap();

            assert_eq!(
                reconstructed.len(),
                data.len(),
                "size mismatch for input size {size}"
            );
            assert_eq!(reconstructed, data, "data mismatch for input size {size}");
        }
    }
}
