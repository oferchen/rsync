//! Low-level wire encoding helpers for batch file serialization.
//!
//! These functions handle reading and writing primitive types in the
//! little-endian and varint formats used by the batch file format.
//! Varint encoding delegates to the `protocol` crate to match upstream
//! rsync's `io.c` format exactly.

use std::io::{self, Read, Write};

/// Write a 32-bit integer in little-endian byte order.
pub(crate) fn write_i32<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 32-bit integer in little-endian byte order.
pub(crate) fn read_i32<R: Read>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Write a variable-length integer using upstream rsync's varint format.
///
/// Delegates to [`protocol::write_varint`] which mirrors `write_varint()` from
/// upstream `io.c`. The encoding uses high bits of the first byte as a length
/// tag, not LEB128 continuation bits.
pub(crate) fn write_varint<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    protocol::write_varint(writer, value)
}

/// Read a variable-length integer using upstream rsync's varint format.
///
/// Delegates to [`protocol::read_varint`] which mirrors `read_varint()` from
/// upstream `io.c`.
pub(crate) fn read_varint<R: Read>(reader: &mut R) -> io::Result<i32> {
    protocol::read_varint(reader)
}

/// Write a variable-length string (length prefix + bytes).
pub(crate) fn write_string<W: Write>(writer: &mut W, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    write_varint(writer, bytes.len() as i32)?;
    writer.write_all(bytes)
}

/// Read a variable-length string (length prefix + bytes).
pub(crate) fn read_string<R: Read>(reader: &mut R) -> io::Result<String> {
    let len = read_varint(reader)? as usize;
    if len > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "string too long",
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write a 64-bit unsigned integer in little-endian byte order.
pub(crate) fn write_u64<W: Write>(writer: &mut W, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 64-bit unsigned integer in little-endian byte order.
pub(crate) fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Write a 32-bit unsigned integer in little-endian byte order.
pub(crate) fn write_u32<W: Write>(writer: &mut W, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Read a 32-bit unsigned integer in little-endian byte order.
pub(crate) fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}
