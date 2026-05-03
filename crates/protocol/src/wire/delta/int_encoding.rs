#![deny(unsafe_code)]

use std::io::{self, Read, Write};

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
/// assert_eq!(buf, [0x78, 0x56, 0x34, 0x12]);
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
