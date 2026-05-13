#![deny(unsafe_code)]
//! Token-based upstream wire format for delta streams.
//!
//! Implements the simple token format from upstream `token.c:simple_send_token()`:
//! - Literal data: `write_int(length)` (positive i32 LE) followed by raw bytes
//! - Block match: `write_int(-(token+1))` where token is the block index
//! - End marker: `write_int(0)`

use std::io::{self, Read, Write};

use super::int_encoding::{read_int, write_int};
use super::types::{CHUNK_SIZE, DeltaOp};

/// Writes literal data in upstream token format.
///
/// Large data is automatically chunked into CHUNK_SIZE (32KB) pieces.
/// Each chunk is written as `write_int(length)` followed by raw bytes.
///
/// # Wire Format
///
/// For data of length N:
/// - If N <= 32KB: `write_int(N)` + N bytes
/// - If N > 32KB: Multiple chunks of format `write_int(chunk_len)` + chunk_bytes
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
/// - block 0 -> -1
/// - block 1 -> -2
/// - block 42 -> -43
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
/// Used when there is no basis file available (e.g., when the receiver
/// doesn't have the file). The entire file is sent as literal data with no
/// block matches.
///
/// # Wire Format
///
/// - Literal data (chunked): `write_int(chunk_len)` + raw bytes (repeated as needed)
/// - End marker: `write_int(0)`
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
/// Reads a single token value and interprets it according to rsync's token
/// encoding rules. The caller is responsible for reading any associated data
/// (for literals) based on the returned value.
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
