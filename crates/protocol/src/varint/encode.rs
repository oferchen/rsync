use std::io::{self, Write};

/// Encodes an i32 into rsync's variable-length format.
///
/// The encoding uses a first-byte indicator scheme where high bits signal
/// how many extra bytes follow:
///
/// | First byte pattern | Extra bytes | Total | Range |
/// |-------------------|-------------|-------|-------|
/// | `0xxx_xxxx` | 0 | 1 | 0..127 |
/// | `10xx_xxxx` | 1 | 2 | 0..16383 |
/// | `110x_xxxx` | 2 | 3 | 0..2097151 |
/// | `1110_xxxx` | 3 | 4 | 0..268435455 |
/// | `1111_0xxx` | 4 | 5 | any i32 |
///
/// Returns (byte_count, bytes_array) where bytes_array[0..byte_count] is the encoded data.
#[inline]
pub(super) fn encode_bytes(value: i32) -> (usize, [u8; 5]) {
    let mut bytes = [0u8; 5];
    bytes[1..5].copy_from_slice(&value.to_le_bytes());

    let mut count = 4usize;
    while count > 1 && bytes[count] == 0 {
        count -= 1;
    }

    let shift = 7 - ((count - 1) as u32);
    let bit = 1u8 << shift;
    let current = bytes[count];

    if current >= bit {
        count += 1;
        bytes[0] = !(bit - 1);
    } else if count > 1 {
        let double_bit = bit << 1;
        let mask = !(double_bit - 1);
        bytes[0] = current | mask;
    } else {
        bytes[0] = bytes[1];
    }

    (count, bytes)
}

/// Encodes `value` using rsync's variable-length integer format and writes it to
/// `writer`.
///
/// The implementation mirrors `write_varint()` from upstream `io.c`, including
/// the exact branching and bit layout used to determine how many bytes are
/// emitted for a particular integer. Only the I/O abstraction differs: this
/// variant accepts any [`Write`] implementation.
///
/// # Errors
///
/// Propagates any error returned by `writer` while writing the encoded bytes.
#[inline]
pub fn write_varint<W: Write + ?Sized>(writer: &mut W, value: i32) -> io::Result<()> {
    let (len, bytes) = encode_bytes(value);
    writer.write_all(&bytes[..len])
}

/// Writes a variable-length 64-bit integer using rsync's varlong format.
///
/// This mirrors upstream's `write_varlong(int f, int64 x, uchar min_bytes)` from io.c.
/// The encoding packs the value into the minimum number of bytes, with a leading
/// byte that indicates how many bytes follow.
///
/// # Arguments
///
/// * `writer` - Destination for the encoded bytes
/// * `value` - The 64-bit value to encode
/// * `min_bytes` - Minimum number of bytes to use (typically 3 or 4 for file sizes/times)
#[inline]
pub fn write_varlong<W: Write + ?Sized>(
    writer: &mut W,
    value: i64,
    min_bytes: u8,
) -> io::Result<()> {
    let bytes = value.to_le_bytes();

    let mut cnt = 8;
    while cnt > min_bytes as usize && bytes[cnt - 1] == 0 {
        cnt -= 1;
    }

    // Wrapping arithmetic avoids overflow when cnt > 7
    let bit = 1u8 << ((7 + min_bytes as usize).wrapping_sub(cnt));
    let leading = if bytes[cnt - 1] >= bit {
        cnt += 1;
        !(bit - 1)
    } else if cnt > min_bytes as usize {
        bytes[cnt - 1] | !(bit * 2 - 1)
    } else {
        bytes[cnt - 1]
    };

    writer.write_all(&[leading])?;
    writer.write_all(&bytes[..cnt - 1])
}

/// Writes a 64-bit integer using rsync's legacy longint format (protocol < 30).
///
/// This mirrors upstream's `write_longint(int f, int64 x)` from io.c.
/// The encoding:
/// - For values 0 <= x <= 0x7FFFFFFF: writes 4 bytes (little-endian i32)
/// - For larger values: writes 0xFFFFFFFF (4 bytes) followed by the full 8 bytes
pub fn write_longint<W: Write + ?Sized>(writer: &mut W, value: i64) -> io::Result<()> {
    if (0..=0x7FFF_FFFF).contains(&value) {
        writer.write_all(&(value as i32).to_le_bytes())
    } else {
        writer.write_all(&0xFFFF_FFFFu32.to_le_bytes())?;
        writer.write_all(&value.to_le_bytes())
    }
}

/// Writes a variable-length integer using protocol 30+ varlong encoding.
///
/// This mirrors upstream's `write_varlong30(int f, int64 x, uchar min_bytes)` inline function.
/// For protocol < 30, callers should use [`write_longint`] instead.
pub fn write_varlong30<W: Write + ?Sized>(
    writer: &mut W,
    value: i64,
    min_bytes: u8,
) -> io::Result<()> {
    write_varlong(writer, value, min_bytes)
}

/// Writes a 32-bit integer using rsync's fixed 4-byte little-endian format.
///
/// This mirrors upstream's `write_int()` from io.c. Used for protocol versions < 30.
#[inline]
pub fn write_int<W: Write + ?Sized>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Writes a 32-bit integer using the protocol 30+ varint30 encoding.
///
/// This mirrors upstream's `write_varint30()` inline function from io.h:
/// - Protocol < 30: uses fixed 4-byte little-endian format
/// - Protocol >= 30: uses variable-length encoding
pub fn write_varint30_int<W: Write + ?Sized>(
    writer: &mut W,
    value: i32,
    protocol_version: u8,
) -> io::Result<()> {
    if protocol_version < 30 {
        write_int(writer, value)
    } else {
        write_varint(writer, value)
    }
}

/// Encodes `value` into `out` using rsync's variable-length integer format.
///
/// The helper mirrors [`write_varint`] but appends the encoded bytes to a
/// caller-provided [`Vec`], making it convenient for fixtures and golden tests
/// that need the serialized representation without going through a [`Write`]
/// adapter.
#[inline]
pub fn encode_varint_to_vec(value: i32, out: &mut Vec<u8>) {
    let (len, bytes) = encode_bytes(value);
    out.extend_from_slice(&bytes[..len]);
}
