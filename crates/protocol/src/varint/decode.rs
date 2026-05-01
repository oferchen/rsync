use std::io::{self, Read};

use super::table::{INT_BYTE_EXTRA, MAX_EXTRA_BYTES, invalid_data};

/// Decodes an i32 from rsync's variable-length format.
///
/// Uses the `INT_BYTE_EXTRA` lookup table to determine how many extra bytes
/// follow the first byte. The table is indexed by `first_byte / 4` (6 bits),
/// producing values 0-6 indicating extra bytes needed.
///
/// Returns (decoded_value, bytes_consumed) on success.
#[inline]
pub(super) fn decode_bytes(bytes: &[u8]) -> io::Result<(i32, usize)> {
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated variable-length integer",
        ));
    }

    let first = bytes[0];
    let extra = INT_BYTE_EXTRA[(first / 4) as usize] as usize;
    if extra > MAX_EXTRA_BYTES {
        return Err(invalid_data("overflow in read_varint"));
    }

    if bytes.len() < 1 + extra {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated variable-length integer",
        ));
    }

    let mut buf = [0u8; 5];
    if extra > 0 {
        buf[..extra].copy_from_slice(&bytes[1..1 + extra]);
        let bit = 1u8 << (8 - extra as u32);
        buf[extra] = first & (bit - 1);
    } else {
        buf[0] = first;
    }

    let value = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok((value, 1 + extra))
}

/// Decodes a variable-length integer from `reader` using rsync's wire format.
///
/// The implementation mirrors `read_varint()` from upstream `io.c`. The leading
/// tag byte determines how many additional bytes follow, all of which are read
/// from `reader` before the value is reconstructed in little-endian order.
///
/// # Errors
///
/// Returns [`io::ErrorKind::UnexpectedEof`] when the reader does not provide the
/// required bytes and [`io::ErrorKind::InvalidData`] if the encoded value would
/// overflow the 32-bit range supported by upstream rsync.
#[inline]
pub fn read_varint<R: Read + ?Sized>(reader: &mut R) -> io::Result<i32> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;

    let extra = INT_BYTE_EXTRA[(first[0] / 4) as usize] as usize;
    if extra > MAX_EXTRA_BYTES {
        return Err(invalid_data("overflow in read_varint"));
    }

    let mut buf = [0u8; 5];
    if extra > 0 {
        reader.read_exact(&mut buf[..extra])?;
        let bit = 1u8 << (8 - extra as u32);
        buf[extra] = first[0] & (bit - 1);
    } else {
        buf[0] = first[0];
    }

    let value = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok(value)
}

/// Reads a variable-length 64-bit integer using rsync's varlong format.
///
/// This is the inverse of [`super::write_varlong`], mirroring upstream's
/// `read_varlong(int f, uchar min_bytes)` from `io.c`. The function first reads
/// `min_bytes` bytes (a leading tag byte plus `min_bytes - 1` initial data bytes),
/// then uses `INT_BYTE_EXTRA` to determine how many additional bytes follow.
///
/// # Arguments
///
/// * `reader` - Source of the encoded bytes
/// * `min_bytes` - Minimum number of bytes used in encoding (must match the write call)
#[inline]
pub fn read_varlong<R: Read + ?Sized>(reader: &mut R, min_bytes: u8) -> io::Result<i64> {
    // upstream: io.c:read_varlong() - read min_bytes first, then extra.
    let min = min_bytes as usize;

    let mut initial = [0u8; 8];
    reader.read_exact(&mut initial[..min])?;

    let leading = initial[0];

    // upstream: io.c:read_varlong() `memcpy(u.b, b2+1, min_bytes-1)` -
    // initial data bytes (after the leading tag) land at result[0..min-1].
    let mut result = [0u8; 9];
    result[..min - 1].copy_from_slice(&initial[1..min]);

    let extra = INT_BYTE_EXTRA[(leading / 4) as usize] as usize;

    if extra > 0 {
        let bit = 1u8 << (8 - extra as u32);
        // upstream: `if (min_bytes + extra > (int)sizeof u.b)` where sizeof u.b = 9
        if min + extra > 9 {
            return Err(invalid_data("overflow in read_varlong"));
        }
        reader.read_exact(&mut result[min - 1..min - 1 + extra])?;
        result[min + extra - 1] = leading & (bit - 1);
    } else {
        // No extra bytes: all 8 bits of the leading byte are data, no masking.
        result[min - 1] = leading;
    }

    Ok(i64::from_le_bytes([
        result[0], result[1], result[2], result[3], result[4], result[5], result[6], result[7],
    ]))
}

/// Reads a 64-bit integer using rsync's legacy longint format (protocol < 30).
///
/// This mirrors upstream's `read_longint(int f)` from io.c.
/// The encoding:
/// - If first 4 bytes == 0xFFFFFFFF: next 8 bytes are the full i64 value
/// - Otherwise: the 4 bytes are the value (sign-extended to i64)
pub fn read_longint<R: Read + ?Sized>(reader: &mut R) -> io::Result<i64> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    let first = i32::from_le_bytes(buf);

    if first == -1 {
        let mut buf64 = [0u8; 8];
        reader.read_exact(&mut buf64)?;
        Ok(i64::from_le_bytes(buf64))
    } else {
        Ok(first as i64)
    }
}

/// Reads a variable-length integer using protocol 30+ varlong encoding.
///
/// This mirrors upstream's `read_varlong30(int f, uchar min_bytes)` inline function.
/// For protocol < 30, callers should use [`read_longint`] instead.
pub fn read_varlong30<R: Read + ?Sized>(reader: &mut R, min_bytes: u8) -> io::Result<i64> {
    read_varlong(reader, min_bytes)
}

/// Reads a 32-bit integer using rsync's fixed 4-byte little-endian format.
///
/// This mirrors upstream's `read_int()` from io.c. Used for protocol versions < 30.
#[inline]
pub fn read_int<R: Read + ?Sized>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Reads a 32-bit integer using the protocol 30+ varint30 encoding.
///
/// This mirrors upstream's `read_varint30()` inline function from io.h:
/// - Protocol < 30: reads fixed 4-byte little-endian format
/// - Protocol >= 30: reads variable-length encoding
pub fn read_varint30_int<R: Read + ?Sized>(
    reader: &mut R,
    protocol_version: u8,
) -> io::Result<i32> {
    if protocol_version < 30 {
        read_int(reader)
    } else {
        read_varint(reader)
    }
}

/// Decodes a variable-length integer from the beginning of `bytes` and returns
/// the parsed value together with the remaining slice.
///
/// This is the slice-based equivalent of [`read_varint`], useful when the caller
/// already captured the serialized data in memory.
#[inline]
pub fn decode_varint(bytes: &[u8]) -> io::Result<(i32, &[u8])> {
    let (value, consumed) = decode_bytes(bytes)?;
    Ok((value, &bytes[consumed..]))
}
