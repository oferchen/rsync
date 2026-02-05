#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! Variable-length integers appear repeatedly in the rsync protocol, most
//! notably when exchanging compatibility flags once both peers have agreed on a
//! protocol version. The routines in this module mirror upstream `io.c`
//! implementations (`read_varint()`/`write_varint()`) so higher layers can
//! serialise and parse these values without depending on the original C code.
//!
//! # Design
//!
//! The codec exposes a streaming API via [`read_varint`] and [`write_varint`],
//! plus helpers for working with in-memory buffers. The lookup table that maps
//! tag prefixes to the number of continuation bytes is copied directly from
//! upstream, ensuring byte-for-byte equivalence with rsync 3.4.1.
//!
//! # Examples
//!
//! Encode a set of compatibility flags into a `Vec<u8>` and decode the result
//! without touching an I/O object:
//!
//! ```
//! use protocol::{decode_varint, encode_varint_to_vec};
//!
//! let mut encoded = Vec::new();
//! encode_varint_to_vec(255, &mut encoded);
//! let (value, remainder) = decode_varint(&encoded).expect("varint decoding succeeds");
//! assert_eq!(value, 255);
//! assert!(remainder.is_empty());
//! ```
//!
//! # See also
//!
//! - [`crate::compatibility::CompatibilityFlags`] for the compatibility flag
//!   bitfield that relies on this codec.

use std::io::{self, Read, Write};

/// Additional byte count lookup used by rsync's variable-length integer codec.
///
/// The table mirrors `int_byte_extra` from upstream `io.c`. Each entry
/// specifies how many extra bytes follow the leading tag for a particular
/// high-bit pattern.
const INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x00-0x3F) / 4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (0x40-0x7F) / 4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (0x80-0xBF) / 4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (0xC0-0xFF) / 4
];

/// Maximum number of additional bytes read after the leading tag.
const MAX_EXTRA_BYTES: usize = 4;

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

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
fn encode_bytes(value: i32) -> (usize, [u8; 5]) {
    let mut bytes = [0u8; 5];
    // Store value in little-endian at bytes[1..5], leaving bytes[0] for the header
    bytes[1..5].copy_from_slice(&value.to_le_bytes());

    // Find the minimum number of bytes needed (trim trailing zeros)
    let mut count = 4usize;
    while count > 1 && bytes[count] == 0 {
        count -= 1;
    }

    // Calculate the bit position that indicates this byte count
    // shift=7 for 1 byte, shift=6 for 2 bytes, etc.
    let shift = 7 - ((count - 1) as u32);
    let bit = 1u8 << shift;
    let current = bytes[count];

    // Determine the first byte based on whether the value fits in the available bits
    if current >= bit {
        // Value too large - need one more byte, set all indicator bits
        count += 1;
        bytes[0] = !(bit - 1);
    } else if count > 1 {
        // Multi-byte: merge indicator bits with the highest byte
        let double_bit = bit << 1;
        let mask = !(double_bit - 1);
        bytes[0] = current | mask;
    } else {
        // Single byte: value fits directly
        bytes[0] = bytes[1];
    }

    (count, bytes)
}

/// Decodes an i32 from rsync's variable-length format.
///
/// Uses the `INT_BYTE_EXTRA` lookup table to determine how many extra bytes
/// follow the first byte. The table is indexed by `first_byte / 4` (6 bits),
/// producing values 0-6 indicating extra bytes needed.
///
/// Returns (decoded_value, bytes_consumed) on success.
fn decode_bytes(bytes: &[u8]) -> io::Result<(i32, usize)> {
    if bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated variable-length integer",
        ));
    }

    let first = bytes[0];
    // Lookup extra bytes needed using the high 6 bits of the first byte
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

    // Reconstruct the value from the encoded bytes
    let mut buf = [0u8; 5];
    if extra > 0 {
        // Copy extra bytes and mask out the indicator bits from first byte
        buf[..extra].copy_from_slice(&bytes[1..1 + extra]);
        let bit = 1u8 << (8 - extra as u32);
        buf[extra] = first & (bit - 1);
    } else {
        // Single byte encoding - value is the first byte directly
        buf[0] = first;
    }

    let value = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok((value, 1 + extra))
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
pub fn write_varlong<W: Write + ?Sized>(
    writer: &mut W,
    value: i64,
    min_bytes: u8,
) -> io::Result<()> {
    // Convert to little-endian bytes
    let bytes = value.to_le_bytes();

    // Find the minimum number of significant bytes needed
    let mut cnt = 8;
    while cnt > min_bytes as usize && bytes[cnt - 1] == 0 {
        cnt -= 1;
    }

    // Calculate the leading byte
    // Use wrapping arithmetic to avoid overflow when cnt > 7
    let bit = 1u8 << ((7 + min_bytes as usize).wrapping_sub(cnt));
    let leading = if bytes[cnt - 1] >= bit {
        cnt += 1;
        !(bit - 1)
    } else if cnt > min_bytes as usize {
        bytes[cnt - 1] | !(bit * 2 - 1)
    } else {
        bytes[cnt - 1]
    };

    // Write leading byte followed by the lower bytes
    writer.write_all(&[leading])?;
    writer.write_all(&bytes[..cnt - 1])
}

/// Reads a variable-length 64-bit integer using rsync's varlong format.
///
/// This is the inverse of `write_varlong`, mirroring upstream's `read_varlong(int f, uchar min_bytes)` from io.c.
/// The function reads a leading byte that encodes the total byte count, then reads the remaining bytes.
///
/// The encoding pattern:
/// - When cnt == min_bytes: leading byte < (1 << 7), all 8 bits are data
/// - When cnt == min_bytes+1: leading byte has bit 7 set, bits 0-5 are data
/// - When cnt == min_bytes+2: leading byte has bits 7-6 set, bits 0-4 are data
/// - And so on...
///
/// # Arguments
///
/// * `reader` - Source of the encoded bytes
/// * `min_bytes` - Minimum number of bytes used in encoding (must match the write call)
pub fn read_varlong<R: Read + ?Sized>(reader: &mut R, min_bytes: u8) -> io::Result<i64> {
    // Read leading byte
    let mut leading_buf = [0u8; 1];
    reader.read_exact(&mut leading_buf)?;
    let leading = leading_buf[0];

    // Determine cnt by counting consecutive high bits set in the leading byte
    // Start with bit 7 and work down
    let mut cnt = min_bytes as usize;
    let mut bit = 1u8 << 7;

    // Each consecutive high bit set indicates one more byte beyond min_bytes
    while cnt < 8 && (leading & bit) != 0 {
        cnt += 1;
        bit >>= 1;
    }

    // Determine mask for extracting data bits from leading byte
    // When cnt == min_bytes, no flag bits were set, so all 8 bits are data.
    // Otherwise, 'bit' points to either:
    //   - The first zero bit we encountered (loop exited on bit check), or
    //   - The bit we were about to check when cnt reached 8 (loop exited on cnt check)
    // In both cases, (bit - 1) gives us the mask for the data bits.
    let mask = if cnt == min_bytes as usize {
        // No flag bits set - all 8 bits of leading byte are data
        0xFF
    } else {
        // Extract data bits below the current bit position
        bit - 1
    };

    // Read the lower bytes (bytes 0..cnt-1)
    let mut bytes = [0u8; 8];
    if cnt > 1 {
        reader.read_exact(&mut bytes[..cnt - 1])?;
    }

    // Set the highest byte from the leading byte (applying mask to extract data bits)
    bytes[cnt - 1] = leading & mask;

    // Convert from little-endian
    Ok(i64::from_le_bytes(bytes))
}

/// Writes a 64-bit integer using rsync's legacy longint format (protocol < 30).
///
/// This mirrors upstream's `write_longint(int f, int64 x)` from io.c.
/// The encoding:
/// - For values 0 <= x <= 0x7FFFFFFF: writes 4 bytes (little-endian i32)
/// - For larger values: writes 0xFFFFFFFF (4 bytes) followed by the full 8 bytes
pub fn write_longint<W: Write + ?Sized>(writer: &mut W, value: i64) -> io::Result<()> {
    if (0..=0x7FFF_FFFF).contains(&value) {
        // Fits in positive signed 32-bit
        writer.write_all(&(value as i32).to_le_bytes())
    } else {
        // Write 0xFFFFFFFF marker followed by full 64-bit value
        writer.write_all(&0xFFFF_FFFFu32.to_le_bytes())?;
        writer.write_all(&value.to_le_bytes())
    }
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
        // Marker indicating full 64-bit value follows
        let mut buf64 = [0u8; 8];
        reader.read_exact(&mut buf64)?;
        Ok(i64::from_le_bytes(buf64))
    } else {
        // Value fits in 32 bits (sign-extend to 64)
        Ok(first as i64)
    }
}

/// Writes a variable-length integer using protocol 30+ varlong encoding.
///
/// This mirrors upstream's `write_varlong30(int f, int64 x, uchar min_bytes)` inline function.
/// For protocol < 30, callers should use `write_longint` instead.
pub fn write_varlong30<W: Write + ?Sized>(
    writer: &mut W,
    value: i64,
    min_bytes: u8,
) -> io::Result<()> {
    write_varlong(writer, value, min_bytes)
}

/// Reads a variable-length integer using protocol 30+ varlong encoding.
///
/// This mirrors upstream's `read_varlong30(int f, uchar min_bytes)` inline function.
/// For protocol < 30, callers should use `read_longint` instead.
pub fn read_varlong30<R: Read + ?Sized>(reader: &mut R, min_bytes: u8) -> io::Result<i64> {
    read_varlong(reader, min_bytes)
}

/// Writes a 32-bit integer using rsync's fixed 4-byte little-endian format.
///
/// This mirrors upstream's `write_int()` from io.c. Used for protocol versions < 30.
pub fn write_int<W: Write + ?Sized>(writer: &mut W, value: i32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

/// Reads a 32-bit integer using rsync's fixed 4-byte little-endian format.
///
/// This mirrors upstream's `read_int()` from io.c. Used for protocol versions < 30.
pub fn read_int<R: Read + ?Sized>(reader: &mut R) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

/// Writes a 32-bit integer using the protocol 30+ varint30 encoding.
///
/// This mirrors upstream's `write_varint30()` inline function from io.h:
/// - Protocol < 30: uses fixed 4-byte little-endian format
/// - Protocol >= 30: uses variable-length encoding
///
/// # Arguments
///
/// * `writer` - Destination for the encoded bytes
/// * `value` - The 32-bit value to encode
/// * `protocol_version` - The negotiated protocol version
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

/// Reads a 32-bit integer using the protocol 30+ varint30 encoding.
///
/// This mirrors upstream's `read_varint30()` inline function from io.h:
/// - Protocol < 30: reads fixed 4-byte little-endian format
/// - Protocol >= 30: reads variable-length encoding
///
/// # Arguments
///
/// * `reader` - Source of the encoded bytes
/// * `protocol_version` - The negotiated protocol version
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

/// Encodes `value` into `out` using rsync's variable-length integer format.
///
/// The helper mirrors [`write_varint`] but appends the encoded bytes to a
/// caller-provided [`Vec`], making it convenient for fixtures and golden tests
/// that need the serialized representation without going through a [`Write`]
/// adapter.
pub fn encode_varint_to_vec(value: i32, out: &mut Vec<u8>) {
    let (len, bytes) = encode_bytes(value);
    out.extend_from_slice(&bytes[..len]);
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

/// Decodes a variable-length integer from the beginning of `bytes` and returns
/// the parsed value together with the remaining slice.
///
/// This is the slice-based equivalent of [`read_varint`], useful when the caller
/// already captured the serialized data in memory.
pub fn decode_varint(bytes: &[u8]) -> io::Result<(i32, &[u8])> {
    let (value, consumed) = decode_bytes(bytes)?;
    Ok((value, &bytes[consumed..]))
}

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    #[test]
    fn encode_matches_known_examples() {
        let cases = [
            (0, "00"),
            (1, "01"),
            (127, "7f"),
            (128, "8080"),
            (255, "80ff"),
            (256, "8100"),
            (16_384, "c00040"),
            (1_073_741_824, "f000000040"),
            (-1, "f0ffffffff"),
            (-128, "f080ffffff"),
            (-129, "f07fffffff"),
            (-32_768, "f00080ffff"),
        ];

        for (value, expected_hex) in cases {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let actual: String = encoded.iter().map(|byte| format!("{byte:02x}")).collect();
            assert_eq!(actual, expected_hex);
        }
    }

    #[test]
    fn read_round_trips_encoded_values() {
        let values = [
            0,
            1,
            127,
            128,
            255,
            256,
            16_384,
            1_073_741_824,
            -1,
            -128,
            -129,
            -32_768,
        ];

        for value in values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let mut cursor = Cursor::new(encoded.clone());
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(decoded, value);
            assert_eq!(cursor.position() as usize, encoded.len());
        }
    }

    #[test]
    fn decode_from_slice_advances_consumed_bytes() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(255, &mut encoded);
        encode_varint_to_vec(1, &mut encoded);

        let (first, remainder) = decode_varint(&encoded).expect("first decode succeeds");
        assert_eq!(first, 255);

        let (second, remainder) = decode_varint(remainder).expect("second decode succeeds");
        assert_eq!(second, 1);
        assert!(remainder.is_empty());
    }

    #[test]
    fn read_varint_errors_on_truncated_input() {
        let data = [0x80u8];
        let mut cursor = Cursor::new(&data[..]);
        let err = read_varint(&mut cursor).expect_err("truncated input must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    proptest! {
        #[test]
        fn encode_decode_round_trip_for_random_values(value in any::<i32>()) {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, remainder) = decode_varint(&encoded).expect("decoding succeeds");
            prop_assert_eq!(decoded, value);
            prop_assert!(remainder.is_empty());

            let mut cursor = Cursor::new(&encoded);
            let read_back = read_varint(&mut cursor).expect("reading succeeds");
            prop_assert_eq!(read_back, value);
            prop_assert_eq!(cursor.position() as usize, encoded.len());
        }

        #[test]
        fn decode_sequences_round_trip(values in prop::collection::vec(any::<i32>(), 1..=32)) {
            let mut encoded = Vec::new();
            for value in &values {
                encode_varint_to_vec(*value, &mut encoded);
            }

            let mut cursor = Cursor::new(&encoded);
            for expected in &values {
                let decoded = read_varint(&mut cursor).expect("reading succeeds");
                prop_assert_eq!(decoded, *expected);
            }

            prop_assert_eq!(cursor.position() as usize, encoded.len());

            let mut remaining = encoded.as_slice();
            for expected in &values {
                let (decoded, tail) = decode_varint(remaining).expect("decoding succeeds");
                prop_assert_eq!(decoded, *expected);
                remaining = tail;
            }
            prop_assert!(remaining.is_empty());
        }
    }

    #[test]
    fn varlong_round_trip_basic_values() {
        // Test positive values only - varlong is used for file sizes and timestamps
        let test_cases = [
            (0i64, 3u8),
            (1i64, 3u8),
            (255i64, 3u8),
            (65536i64, 3u8),
            (16777215i64, 3u8),   // Max value that fits in 3 bytes
            (16777216i64, 3u8),   // Requires 4 bytes
            (1700000000i64, 4u8), // Typical Unix timestamp
            (i64::MAX, 8u8),      // Maximum positive value
        ];

        for (value, min_bytes) in test_cases {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, value, min_bytes).expect("encoding succeeds");

            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("decoding succeeds");

            assert_eq!(
                decoded, value,
                "Round-trip failed for value={value} min_bytes={min_bytes}: encoded={encoded:02x?}"
            );
            assert_eq!(
                cursor.position() as usize,
                encoded.len(),
                "Cursor position mismatch for value={value} min_bytes={min_bytes}"
            );
        }
    }

    #[test]
    fn varlong_large_values_with_min_bytes_3() {
        // Test large values with min_bytes=3 (used for stats)
        // Note: With min_bytes=3, the maximum encodable value that round-trips
        // correctly is ~2.88e17 (288 PB). This matches upstream rsync's limitation.
        // Values larger than this would require 9 bytes but the decoder only
        // handles 8 bytes total (matching upstream io.c:read_varlong).
        let max_safe_for_min3: i64 = 0x03FF_FFFF_FFFF_FFFF; // ~288 PB
        let test_cases = [
            (max_safe_for_min3, 3u8), // Maximum safe value for min_bytes=3
            (max_safe_for_min3 / 2, 3u8),
            (1_000_000_000_000_000i64, 3u8), // 1 PB - realistic large transfer
            (100_000_000_000_000i64, 3u8),   // 100 TB
            (1_000_000_000_000i64, 3u8),     // 1 TB
            (1_000_000_000i64, 3u8),         // 1 GB
            (500_000_000i64, 3u8),
        ];

        for (value, min_bytes) in test_cases {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, value, min_bytes).expect("encoding succeeds");

            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("decoding succeeds");

            assert_eq!(
                decoded, value,
                "Round-trip failed for value={value} min_bytes={min_bytes}: encoded={encoded:02x?}"
            );
            assert_eq!(
                cursor.position() as usize,
                encoded.len(),
                "Cursor didn't consume all bytes for value={value}"
            );
        }
    }

    // ==== Additional varint tests ====

    #[test]
    fn write_varint_to_writer() {
        let mut output = Vec::new();
        write_varint(&mut output, 42).expect("write succeeds");
        assert_eq!(output, vec![42]);
    }

    #[test]
    fn write_varint_multiple_values() {
        let mut output = Vec::new();
        write_varint(&mut output, 0).expect("write 0");
        write_varint(&mut output, 127).expect("write 127");
        write_varint(&mut output, 128).expect("write 128");
        assert!(!output.is_empty());

        // Verify we can read them back
        let mut cursor = Cursor::new(&output);
        assert_eq!(read_varint(&mut cursor).unwrap(), 0);
        assert_eq!(read_varint(&mut cursor).unwrap(), 127);
        assert_eq!(read_varint(&mut cursor).unwrap(), 128);
    }

    #[test]
    fn read_varint_empty_input() {
        let data: [u8; 0] = [];
        let mut cursor = Cursor::new(&data[..]);
        let err = read_varint(&mut cursor).expect_err("empty input must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_varint_empty_input() {
        let data: [u8; 0] = [];
        let err = decode_varint(&data).expect_err("empty input must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_varint_single_byte() {
        let data = [42u8];
        let (value, remainder) = decode_varint(&data).expect("decode succeeds");
        assert_eq!(value, 42);
        assert!(remainder.is_empty());
    }

    #[test]
    fn decode_varint_boundary_127() {
        // 127 is the max single-byte value
        let data = [127u8];
        let (value, remainder) = decode_varint(&data).expect("decode succeeds");
        assert_eq!(value, 127);
        assert!(remainder.is_empty());
    }

    #[test]
    fn decode_varint_boundary_128() {
        // 128 requires two bytes
        let mut data = Vec::new();
        encode_varint_to_vec(128, &mut data);
        assert_eq!(data.len(), 2);
        let (value, remainder) = decode_varint(&data).expect("decode succeeds");
        assert_eq!(value, 128);
        assert!(remainder.is_empty());
    }

    #[test]
    fn varint_negative_values() {
        let negatives = [-1, -127, -128, -255, -256, -32768, -65536, i32::MIN];
        for value in negatives {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "failed for {value}");
        }
    }

    #[test]
    fn varint_max_values() {
        let extremes = [i32::MAX, i32::MIN, 0];
        for value in extremes {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value);
        }
    }

    #[test]
    fn encode_varint_length_varies_with_value() {
        // Small values should have shorter encodings
        let mut small = Vec::new();
        encode_varint_to_vec(1, &mut small);

        let mut large = Vec::new();
        encode_varint_to_vec(1_000_000_000, &mut large);

        assert!(small.len() < large.len());
    }

    // ==== Longint tests ====

    #[test]
    fn write_longint_small_positive() {
        let mut output = Vec::new();
        write_longint(&mut output, 42).expect("write succeeds");
        assert_eq!(output.len(), 4);
        // Read back as i32 LE
        let value = i32::from_le_bytes(output.try_into().unwrap());
        assert_eq!(value, 42);
    }

    #[test]
    fn write_longint_max_inline() {
        // 0x7FFFFFFF is the max value that fits inline (4 bytes)
        let max_inline = 0x7FFF_FFFF_i64;
        let mut output = Vec::new();
        write_longint(&mut output, max_inline).expect("write succeeds");
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn write_longint_above_max_inline() {
        // 0x80000000 requires the full 12-byte encoding
        let above_inline = 0x8000_0000_i64;
        let mut output = Vec::new();
        write_longint(&mut output, above_inline).expect("write succeeds");
        assert_eq!(output.len(), 12); // 4 (marker) + 8 (full value)

        // First 4 bytes should be 0xFFFFFFFF marker
        let marker = u32::from_le_bytes(output[0..4].try_into().unwrap());
        assert_eq!(marker, 0xFFFF_FFFF);

        // Last 8 bytes should be the value
        let value = i64::from_le_bytes(output[4..12].try_into().unwrap());
        assert_eq!(value, above_inline);
    }

    #[test]
    fn write_longint_zero() {
        let mut output = Vec::new();
        write_longint(&mut output, 0).expect("write succeeds");
        assert_eq!(output.len(), 4);
        let value = i32::from_le_bytes(output.try_into().unwrap());
        assert_eq!(value, 0);
    }

    #[test]
    fn write_longint_large_values() {
        let large_values = [
            i64::MAX,
            0x8000_0000_i64,
            0xFFFF_FFFF_i64,
            0x1_0000_0000_i64,
            1_000_000_000_000_i64,
        ];

        for value in large_values {
            let mut output = Vec::new();
            write_longint(&mut output, value).expect("write succeeds");
            assert_eq!(output.len(), 12, "large value {value} should use 12 bytes");
        }
    }

    // ==== Varlong30 wrapper tests ====

    #[test]
    fn varlong30_is_alias_for_varlong() {
        let value = 1234567i64;
        let min_bytes = 3u8;

        let mut encoded_30 = Vec::new();
        write_varlong30(&mut encoded_30, value, min_bytes).expect("write succeeds");

        let mut encoded_varlong = Vec::new();
        write_varlong(&mut encoded_varlong, value, min_bytes).expect("write succeeds");

        assert_eq!(encoded_30, encoded_varlong);

        let mut cursor = Cursor::new(&encoded_30);
        let decoded = read_varlong30(&mut cursor, min_bytes).expect("read succeeds");
        assert_eq!(decoded, value);
    }

    // ==== Varlong with different min_bytes ====

    #[test]
    fn varlong_min_bytes_1() {
        let value = 42i64;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 1).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 1).expect("read succeeds");
        assert_eq!(decoded, value);
    }

    #[test]
    fn varlong_min_bytes_4() {
        let value = 1_000_000i64;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 4).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 4).expect("read succeeds");
        assert_eq!(decoded, value);
    }

    #[test]
    fn varlong_zero_value() {
        for min_bytes in 1u8..=8 {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, 0, min_bytes).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
            assert_eq!(decoded, 0, "zero failed for min_bytes={min_bytes}");
        }
    }

    #[test]
    fn varlong_typical_timestamps() {
        // Typical Unix timestamps (seconds since 1970)
        let timestamps = [
            0i64,
            1_000_000_000i64,      // Sep 2001
            1_700_000_000i64,      // Nov 2023
            2_000_000_000i64,      // May 2033
            i32::MAX as i64,       // Jan 2038
            (i32::MAX as i64) + 1, // After Y2038
        ];

        for ts in timestamps {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, ts, 4).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, 4).expect("read succeeds");
            assert_eq!(decoded, ts, "timestamp {ts} failed");
        }
    }

    #[test]
    fn varlong_typical_file_sizes() {
        // Typical file sizes
        let sizes = [
            0i64,
            1024i64,                    // 1 KB
            1_048_576i64,               // 1 MB
            1_073_741_824i64,           // 1 GB
            1_099_511_627_776i64,       // 1 TB
            1_125_899_906_842_624i64,   // 1 PB
            100_000_000_000_000_000i64, // 100 PB
        ];

        for size in sizes {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, size, 3).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, 3).expect("read succeeds");
            assert_eq!(decoded, size, "file size {size} failed");
        }
    }

    // ==== Error handling tests ====

    #[test]
    fn read_varlong_truncated_input() {
        // A leading byte that indicates more bytes follow, but truncated
        let data = [0x80u8]; // Indicates at least 1 more byte
        let mut cursor = Cursor::new(&data[..]);
        let err = read_varlong(&mut cursor, 1).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_varlong_empty_input() {
        let data: [u8; 0] = [];
        let mut cursor = Cursor::new(&data[..]);
        let err = read_varlong(&mut cursor, 3).expect_err("empty must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    // ==== INT_BYTE_EXTRA table tests ====

    #[test]
    fn int_byte_extra_table_structure() {
        // Verify the structure of INT_BYTE_EXTRA table
        // 0x00-0x7F / 4 (indices 0-31) should be 0 extra bytes
        for (i, &val) in INT_BYTE_EXTRA[..32].iter().enumerate() {
            assert_eq!(val, 0, "index {i} should be 0");
        }
        // 0x80-0xBF / 4 (indices 32-47) should be 1 extra byte
        for (i, &val) in INT_BYTE_EXTRA[32..48].iter().enumerate() {
            assert_eq!(val, 1, "index {} should be 1", i + 32);
        }
        // 0xC0-0xDF / 4 (indices 48-55) should be 2 extra bytes
        for (i, &val) in INT_BYTE_EXTRA[48..56].iter().enumerate() {
            assert_eq!(val, 2, "index {} should be 2", i + 48);
        }
    }

    #[test]
    fn decode_bytes_validates_int_byte_extra() {
        // Test that various leading bytes produce correct extra byte counts
        // Leading byte 0x00-0x7F: 0 extra bytes (single byte encoding)
        let (value, consumed) = decode_bytes(&[0x42]).expect("decode succeeds");
        assert_eq!(value, 0x42);
        assert_eq!(consumed, 1);

        // Leading byte 0x80: 1 extra byte
        let (value, consumed) = decode_bytes(&[0x80, 0x01]).expect("decode succeeds");
        assert_eq!(consumed, 2);
        assert_eq!(value & 0xFFFF, 1); // Low byte is 0x01
    }

    #[test]
    fn invalid_data_error_message() {
        let err = invalid_data("test error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("test error"));
    }

    // ==== Encoding length tests ====

    #[test]
    fn encode_bytes_length_for_boundary_values() {
        // 0-127: 1 byte
        let (len, _) = encode_bytes(0);
        assert_eq!(len, 1);
        let (len, _) = encode_bytes(127);
        assert_eq!(len, 1);

        // 128-255: 2 bytes
        let (len, _) = encode_bytes(128);
        assert_eq!(len, 2);
        let (len, _) = encode_bytes(255);
        assert_eq!(len, 2);

        // Larger values need more bytes
        let (len, _) = encode_bytes(65536);
        assert!(len >= 3);
    }

    // ==== Fixed int (write_int/read_int) tests ====

    #[test]
    fn write_int_produces_4_bytes() {
        let mut output = Vec::new();
        write_int(&mut output, 42).expect("write succeeds");
        assert_eq!(output.len(), 4);
        assert_eq!(output, vec![42, 0, 0, 0]);
    }

    #[test]
    fn read_int_parses_4_bytes() {
        let data = [42u8, 0, 0, 0];
        let mut cursor = Cursor::new(&data[..]);
        let value = read_int(&mut cursor).expect("read succeeds");
        assert_eq!(value, 42);
    }

    #[test]
    fn write_read_int_roundtrip() {
        let test_values = [0, 1, 127, 128, 255, 256, 65536, i32::MAX, i32::MIN, -1];
        for value in test_values {
            let mut buf = Vec::new();
            write_int(&mut buf, value).expect("write succeeds");
            assert_eq!(buf.len(), 4);
            let mut cursor = Cursor::new(&buf[..]);
            let read_back = read_int(&mut cursor).expect("read succeeds");
            assert_eq!(read_back, value, "roundtrip failed for {value}");
        }
    }

    #[test]
    fn read_int_insufficient_data() {
        let data = [42u8, 0, 0]; // Only 3 bytes
        let mut cursor = Cursor::new(&data[..]);
        let err = read_int(&mut cursor).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    // ==== varint30_int tests ====

    #[test]
    fn write_varint30_int_proto29_uses_fixed_int() {
        let mut output = Vec::new();
        write_varint30_int(&mut output, 42, 29).expect("write succeeds");
        assert_eq!(output.len(), 4); // Fixed 4-byte int
        assert_eq!(output, vec![42, 0, 0, 0]);
    }

    #[test]
    fn write_varint30_int_proto30_uses_varint() {
        let mut output = Vec::new();
        write_varint30_int(&mut output, 42, 30).expect("write succeeds");
        assert_eq!(output.len(), 1); // Single-byte varint for small values
        assert_eq!(output, vec![42]);
    }

    #[test]
    fn read_varint30_int_proto29_reads_fixed_int() {
        let data = [42u8, 0, 0, 0];
        let mut cursor = Cursor::new(&data[..]);
        let value = read_varint30_int(&mut cursor, 29).expect("read succeeds");
        assert_eq!(value, 42);
    }

    #[test]
    fn read_varint30_int_proto30_reads_varint() {
        let data = [42u8];
        let mut cursor = Cursor::new(&data[..]);
        let value = read_varint30_int(&mut cursor, 30).expect("read succeeds");
        assert_eq!(value, 42);
    }

    #[test]
    fn varint30_int_roundtrip_proto29() {
        let test_values = [0, 1, 127, 128, 1000, i32::MAX, -1];
        for value in test_values {
            let mut buf = Vec::new();
            write_varint30_int(&mut buf, value, 29).expect("write succeeds");
            let mut cursor = Cursor::new(&buf[..]);
            let read_back = read_varint30_int(&mut cursor, 29).expect("read succeeds");
            assert_eq!(read_back, value, "proto29 roundtrip failed for {value}");
        }
    }

    #[test]
    fn varint30_int_roundtrip_proto30() {
        let test_values = [0, 1, 127, 128, 1000, i32::MAX, -1];
        for value in test_values {
            let mut buf = Vec::new();
            write_varint30_int(&mut buf, value, 30).expect("write succeeds");
            let mut cursor = Cursor::new(&buf[..]);
            let read_back = read_varint30_int(&mut cursor, 30).expect("read succeeds");
            assert_eq!(read_back, value, "proto30 roundtrip failed for {value}");
        }
    }

    #[test]
    fn varint30_int_proto_boundary_at_30() {
        // Protocol 29 and below should use fixed int
        for proto in [28u8, 29] {
            let mut buf = Vec::new();
            write_varint30_int(&mut buf, 1000, proto).expect("write succeeds");
            assert_eq!(buf.len(), 4, "proto {proto} should use 4-byte int");
        }

        // Protocol 30 and above should use varint
        for proto in [30u8, 31, 32] {
            let mut buf = Vec::new();
            write_varint30_int(&mut buf, 1000, proto).expect("write succeeds");
            assert!(buf.len() < 4, "proto {proto} should use varint (< 4 bytes)");
        }
    }

    // ===========================================================================
    // BOUNDARY CONDITION TESTS
    // ===========================================================================

    // ---- i32 boundary tests ----

    #[test]
    fn varint_i32_max_roundtrip() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(i32::MAX, &mut encoded);
        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, i32::MAX);
        assert!(remainder.is_empty());
    }

    #[test]
    fn varint_i32_min_roundtrip() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(i32::MIN, &mut encoded);
        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, i32::MIN);
        assert!(remainder.is_empty());
    }

    #[test]
    fn varint_i32_max_minus_one() {
        let value = i32::MAX - 1;
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, value);
    }

    #[test]
    fn varint_i32_min_plus_one() {
        let value = i32::MIN + 1;
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, value);
    }

    // ---- i64 boundary tests ----

    #[test]
    fn varlong_i64_max_roundtrip() {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, i64::MAX, 8).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 8).expect("read succeeds");
        assert_eq!(decoded, i64::MAX);
    }

    #[test]
    fn varlong_i64_zero_roundtrip() {
        // Test zero with various min_bytes
        for min_bytes in 1u8..=8 {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, 0i64, min_bytes).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
            assert_eq!(decoded, 0i64, "zero failed for min_bytes={min_bytes}");
        }
    }

    #[test]
    fn longint_i64_max_roundtrip() {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, i64::MAX).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        assert_eq!(decoded, i64::MAX);
    }

    #[test]
    fn longint_i64_min_roundtrip() {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, i64::MIN).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        assert_eq!(decoded, i64::MIN);
    }

    // ---- u32/u64 equivalent boundary tests (as signed) ----

    #[test]
    fn varint_u32_max_as_i32_roundtrip() {
        // u32::MAX interpreted as i32 is -1
        let value = u32::MAX as i32;
        assert_eq!(value, -1);
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, value);
    }

    #[test]
    fn varlong_large_positive_value_roundtrip() {
        // Varlong is designed for positive values (file sizes, timestamps).
        // Test with a large positive value that fits in i64::MAX.
        let value = i64::MAX / 2; // Large positive value
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 8).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 8).expect("read succeeds");
        assert_eq!(decoded, value);
    }

    // ---- Overflow edge cases ----

    #[test]
    fn decode_varint_overflow_tag_byte() {
        // Leading bytes 0xFC-0xFF (indices 63) in INT_BYTE_EXTRA table
        // indicate 5 or 6 extra bytes, which should trigger overflow error
        // 0xFC (252) / 4 = 63, which maps to 5 extra bytes
        // 0xFD (253) / 4 = 63, which maps to 5 extra bytes
        // 0xFE (254) / 4 = 63, which maps to 5 extra bytes
        // 0xFF (255) / 4 = 63, which maps to 6 extra bytes
        let data = [0xFCu8, 0, 0, 0, 0, 0]; // 5 extra bytes
        let err = decode_varint(&data).expect_err("overflow tag should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("overflow"));

        let data = [0xFFu8, 0, 0, 0, 0, 0, 0]; // 6 extra bytes
        let err = decode_varint(&data).expect_err("overflow tag should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn read_varint_overflow_tag_byte() {
        let data = [0xFCu8, 0, 0, 0, 0, 0];
        let mut cursor = Cursor::new(&data[..]);
        let err = read_varint(&mut cursor).expect_err("overflow tag should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ---- Encoding/decoding round-trip for boundary values ----

    #[test]
    fn varint_encoding_length_boundaries() {
        // Test values at encoding length boundaries
        // 1-byte: 0-127 (0x00-0x7F)
        // 2-byte: 128-16383 (0x80-0x3FFF)
        // 3-byte: 16384-2097151 (0x4000-0x1FFFFF)
        // etc.
        let boundary_values = [
            (0, 1),          // min 1-byte
            (127, 1),        // max 1-byte
            (128, 2),        // min 2-byte
            (16383, 2),      // near max 2-byte
            (16384, 3),      // min 3-byte
            (2097151, 3),    // near max 3-byte
            (0x200000, 4),   // 4-byte territory
            (0x10000000, 5), // 5-byte territory
        ];

        for (value, expected_min_len) in boundary_values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert!(
                encoded.len() >= expected_min_len,
                "value {value} expected at least {expected_min_len} bytes, got {}",
                encoded.len()
            );
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "roundtrip failed for {value}");
        }
    }

    // ---- Zero and negative number handling ----

    #[test]
    fn varint_zero_encoding() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(0, &mut encoded);
        assert_eq!(encoded, vec![0x00]);
        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 0);
    }

    #[test]
    fn varint_negative_one_encoding() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(-1, &mut encoded);
        // -1 as i32 is 0xFFFFFFFF, which requires 5 bytes in varint format
        assert_eq!(encoded.len(), 5);
        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, -1);
    }

    #[test]
    fn varint_all_powers_of_two_positive() {
        for shift in 0..31 {
            let value: i32 = 1 << shift;
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "failed for 2^{shift}");
        }
    }

    #[test]
    fn varint_all_powers_of_two_negative() {
        for shift in 0..31 {
            let value: i32 = -(1 << shift);
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "failed for -(2^{shift})");
        }
    }

    // ---- INT_BYTE_EXTRA lookup table edge cases ----

    #[test]
    fn int_byte_extra_first_index() {
        // Index 0: byte values 0-3
        assert_eq!(INT_BYTE_EXTRA[0], 0);
    }

    #[test]
    fn int_byte_extra_last_index() {
        // Index 63: byte values 252-255
        assert_eq!(INT_BYTE_EXTRA[63], 6);
    }

    #[test]
    fn int_byte_extra_transition_points() {
        // Verify transition points in the table
        // 0x00-0x7F (0-127): 0 extra bytes -> indices 0-31
        assert_eq!(INT_BYTE_EXTRA[31], 0); // byte 0x7C-0x7F
        // 0x80-0xBF (128-191): 1 extra byte -> indices 32-47
        assert_eq!(INT_BYTE_EXTRA[32], 1); // byte 0x80-0x83
        assert_eq!(INT_BYTE_EXTRA[47], 1); // byte 0xBC-0xBF
        // 0xC0-0xDF (192-223): 2 extra bytes -> indices 48-55
        assert_eq!(INT_BYTE_EXTRA[48], 2); // byte 0xC0-0xC3
        assert_eq!(INT_BYTE_EXTRA[55], 2); // byte 0xDC-0xDF
        // 0xE0-0xEF (224-239): 3 extra bytes -> indices 56-59
        assert_eq!(INT_BYTE_EXTRA[56], 3); // byte 0xE0-0xE3
        assert_eq!(INT_BYTE_EXTRA[59], 3); // byte 0xEC-0xEF
        // 0xF0-0xF7 (240-247): 4 extra bytes -> indices 60-61
        assert_eq!(INT_BYTE_EXTRA[60], 4); // byte 0xF0-0xF3
        assert_eq!(INT_BYTE_EXTRA[61], 4); // byte 0xF4-0xF7
        // 0xF8-0xFB (248-251): 5 extra bytes -> index 62
        assert_eq!(INT_BYTE_EXTRA[62], 5); // byte 0xF8-0xFB
        // 0xFC-0xFF (252-255): 6 extra bytes -> index 63
        assert_eq!(INT_BYTE_EXTRA[63], 6); // byte 0xFC-0xFF
    }

    #[test]
    fn int_byte_extra_decode_with_each_extra_count() {
        // Test decoding with each valid extra byte count (0, 1, 2, 3, 4)
        // 0 extra: leading byte 0x00-0x7F
        let (val, consumed) = decode_bytes(&[0x42]).unwrap();
        assert_eq!(val, 0x42);
        assert_eq!(consumed, 1);

        // 1 extra: leading byte 0x80-0xBF
        let (val, consumed) = decode_bytes(&[0x80, 0x42]).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(val, 0x42);

        // 2 extra: leading byte 0xC0-0xDF
        let (val, consumed) = decode_bytes(&[0xC0, 0x42, 0x00]).unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(val, 0x42);

        // 3 extra: leading byte 0xE0-0xEF
        let (val, consumed) = decode_bytes(&[0xE0, 0x42, 0x00, 0x00]).unwrap();
        assert_eq!(consumed, 4);
        assert_eq!(val, 0x42);

        // 4 extra: leading byte 0xF0-0xF7
        let (val, consumed) = decode_bytes(&[0xF0, 0x42, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(val, 0x42);
    }

    // ---- Longint boundary tests ----

    #[test]
    fn longint_boundary_at_0x7fffffff() {
        // 0x7FFFFFFF is the maximum value that fits in 4 bytes
        let max_inline = 0x7FFF_FFFF_i64;
        let mut encoded = Vec::new();
        write_longint(&mut encoded, max_inline).expect("write succeeds");
        assert_eq!(encoded.len(), 4);
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        assert_eq!(decoded, max_inline);
    }

    #[test]
    fn longint_boundary_at_0x80000000() {
        // 0x80000000 requires 12 bytes (marker + 8-byte value)
        let min_extended = 0x8000_0000_i64;
        let mut encoded = Vec::new();
        write_longint(&mut encoded, min_extended).expect("write succeeds");
        assert_eq!(encoded.len(), 12);
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        assert_eq!(decoded, min_extended);
    }

    #[test]
    fn longint_negative_uses_extended_format() {
        // Negative values always use extended 12-byte format
        let negative = -1i64;
        let mut encoded = Vec::new();
        write_longint(&mut encoded, negative).expect("write succeeds");
        assert_eq!(encoded.len(), 12);
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        assert_eq!(decoded, negative);
    }

    // ---- Varlong boundary tests ----

    #[test]
    fn varlong_min_bytes_boundary_values() {
        // Test with min_bytes at each extreme
        for min_bytes in [1u8, 2, 3, 4, 5, 6, 7, 8] {
            let value = 0xFFi64; // 255
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, value, min_bytes).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
            assert_eq!(decoded, value, "failed for min_bytes={min_bytes}");
        }
    }

    #[test]
    fn varlong_encodes_minimum_bytes() {
        // A value that fits in 3 bytes with min_bytes=3 should use 3 bytes
        let value = 0x1234i64;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 3).expect("write succeeds");
        // Leading byte + (min_bytes - 1) = 3 bytes minimum
        assert!(encoded.len() >= 3, "expected at least 3 bytes");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 3).expect("read succeeds");
        assert_eq!(decoded, value);
    }

    // ---- Additional truncation tests ----

    #[test]
    fn decode_varint_truncated_2_byte() {
        // 0x80 indicates 1 extra byte is needed
        let data = [0x80u8];
        let err = decode_varint(&data).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_varint_truncated_3_byte() {
        // 0xC0 indicates 2 extra bytes are needed
        let data = [0xC0u8, 0x00];
        let err = decode_varint(&data).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_varint_truncated_4_byte() {
        // 0xE0 indicates 3 extra bytes are needed
        let data = [0xE0u8, 0x00, 0x00];
        let err = decode_varint(&data).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn decode_varint_truncated_5_byte() {
        // 0xF0 indicates 4 extra bytes are needed
        let data = [0xF0u8, 0x00, 0x00, 0x00];
        let err = decode_varint(&data).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_longint_truncated_marker() {
        // Only 2 bytes when 4 are needed
        let data = [0xFFu8, 0xFF];
        let mut cursor = Cursor::new(&data[..]);
        let err = read_longint(&mut cursor).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_longint_truncated_extended() {
        // Marker present but extended value truncated
        let data = [0xFFu8, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00]; // Only 7 bytes after marker
        let mut cursor = Cursor::new(&data[..]);
        let err = read_longint(&mut cursor).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}

// ===========================================================================
// PHASE 2.7: VARINT 1-BYTE ENCODING TESTS (0-127)
// ===========================================================================

/// Tests for varint 1-byte encoding range (0-127).
///
/// The rsync varint encoding uses a single byte for values 0-127, where the
/// high bit is clear (`0xxx_xxxx`). These tests verify:
/// - All values in range [0, 127] encode to exactly 1 byte
/// - The encoded byte equals the input value
/// - Round-trip encoding/decoding preserves the value
/// - Boundary values (0, 127) are handled correctly
#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod phase2_7_varint_1byte_encoding {
    use super::*;
    use std::io::Cursor;

    /// Verifies that values 0-127 encode to exactly one byte.
    #[test]
    fn all_1byte_values_encode_to_single_byte() {
        for value in 0..=127_i32 {
            let (len, _bytes) = encode_bytes(value);
            assert_eq!(
                len, 1,
                "value {} should encode to 1 byte, got {}",
                value, len
            );
        }
    }

    /// Verifies that for 1-byte encoding, the encoded byte equals the value.
    #[test]
    fn encoded_byte_equals_value_for_1byte_range() {
        for value in 0..=127_i32 {
            let (len, bytes) = encode_bytes(value);
            assert_eq!(len, 1);
            assert_eq!(
                bytes[0], value as u8,
                "encoded byte for {} should be {}, got {}",
                value, value, bytes[0]
            );
        }
    }

    /// Verifies round-trip for all 1-byte values via encode_varint_to_vec/decode_varint.
    #[test]
    fn roundtrip_all_1byte_values_via_vec() {
        for value in 0..=127_i32 {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert_eq!(encoded.len(), 1, "value {} should encode to 1 byte", value);

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for value {}", value);
            assert!(remainder.is_empty(), "no bytes should remain");
        }
    }

    /// Verifies round-trip for all 1-byte values via write_varint/read_varint.
    #[test]
    fn roundtrip_all_1byte_values_via_stream() {
        for value in 0..=127_i32 {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");
            assert_eq!(buf.len(), 1, "value {} should write 1 byte", value);

            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(
                decoded, value,
                "stream round-trip failed for value {}",
                value
            );
            assert_eq!(cursor.position(), 1, "should have read exactly 1 byte");
        }
    }

    /// Tests boundary value 0 (minimum 1-byte value).
    #[test]
    fn boundary_zero() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(0, &mut encoded);
        assert_eq!(encoded, vec![0x00]);

        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 0);
        assert!(remainder.is_empty());
    }

    /// Tests boundary value 127 (maximum 1-byte value).
    #[test]
    fn boundary_127() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(127, &mut encoded);
        assert_eq!(encoded, vec![0x7F]);

        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 127);
        assert!(remainder.is_empty());
    }

    /// Tests that 128 requires more than 1 byte (boundary transition).
    #[test]
    fn boundary_128_is_not_1byte() {
        let (len, _) = encode_bytes(128);
        assert!(len > 1, "value 128 should NOT encode to 1 byte");
    }

    /// Tests decoding raw 1-byte sequences directly.
    #[test]
    fn decode_raw_1byte_sequences() {
        // Test decoding specific raw bytes
        for byte in 0u8..=127 {
            let data = [byte];
            let (value, consumed) = decode_bytes(&data).expect("decode succeeds");
            assert_eq!(
                value, byte as i32,
                "raw byte 0x{:02X} should decode to {}",
                byte, byte
            );
            assert_eq!(consumed, 1);
        }
    }

    /// Tests that high-bit-clear bytes are always 1-byte encodings.
    #[test]
    fn high_bit_clear_indicates_1byte() {
        // For bytes 0x00-0x7F, INT_BYTE_EXTRA should be 0
        for byte in 0u8..=127 {
            let extra = INT_BYTE_EXTRA[(byte / 4) as usize];
            assert_eq!(
                extra, 0,
                "byte 0x{:02X} should have 0 extra bytes, got {}",
                byte, extra
            );
        }
    }

    /// Tests multiple consecutive 1-byte values in a stream.
    #[test]
    fn multiple_1byte_values_in_stream() {
        let values = [0, 1, 42, 63, 64, 100, 126, 127];
        let mut encoded = Vec::new();
        for &v in &values {
            encode_varint_to_vec(v, &mut encoded);
        }
        assert_eq!(
            encoded.len(),
            values.len(),
            "all values should be 1 byte each"
        );

        let mut cursor = Cursor::new(&encoded);
        for &expected in &values {
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(decoded, expected);
        }
    }
}

// ===========================================================================
// PHASE 2.8: VARINT 2-BYTE ENCODING TESTS (128-16383)
// ===========================================================================

/// Tests for varint 2-byte encoding range (128-16383).
///
/// The rsync varint encoding uses two bytes for values that don't fit in
/// a single byte. The format is `10xx_xxxx` for the first byte, followed
/// by one additional byte. These tests verify:
/// - Values in range [128, 16383] encode to exactly 2 bytes
/// - The encoding format matches upstream rsync
/// - Round-trip encoding/decoding preserves values
/// - Boundary values (128, 16383) are handled correctly
#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod phase2_8_varint_2byte_encoding {
    use super::*;
    use std::io::Cursor;

    /// Verifies that value 128 (minimum 2-byte value) encodes correctly.
    #[test]
    fn boundary_128_encodes_to_2_bytes() {
        let (len, _) = encode_bytes(128);
        assert_eq!(len, 2, "value 128 should encode to 2 bytes");

        let mut encoded = Vec::new();
        encode_varint_to_vec(128, &mut encoded);
        assert_eq!(encoded.len(), 2);

        // Verify round-trip
        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 128);
        assert!(remainder.is_empty());
    }

    /// Verifies that value 255 encodes to 2 bytes.
    #[test]
    fn value_255_encodes_to_2_bytes() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(255, &mut encoded);
        assert_eq!(encoded.len(), 2);
        // Verify against known encoding from upstream
        assert_eq!(encoded, vec![0x80, 0xFF]);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 255);
    }

    /// Verifies that value 256 encodes to 2 bytes.
    #[test]
    fn value_256_encodes_to_2_bytes() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(256, &mut encoded);
        assert_eq!(encoded.len(), 2);
        // Verify against known encoding from upstream
        assert_eq!(encoded, vec![0x81, 0x00]);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 256);
    }

    /// Verifies round-trip for sample 2-byte values.
    #[test]
    fn roundtrip_sample_2byte_values() {
        let values = [
            128, 129, 200, 255, 256, 300, 500, 1000, 2000, 4000, 8000, 10000, 16000, 16383,
        ];
        for value in values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert!(
                encoded.len() <= 2,
                "value {} should encode to at most 2 bytes, got {}",
                value,
                encoded.len()
            );

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for value {}", value);
            assert!(remainder.is_empty());
        }
    }

    /// Verifies round-trip via write_varint/read_varint for 2-byte values.
    #[test]
    fn roundtrip_2byte_values_via_stream() {
        let values = [128, 255, 256, 1000, 8000, 16383];
        for value in values {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");

            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(
                decoded, value,
                "stream round-trip failed for value {}",
                value
            );
            assert_eq!(cursor.position() as usize, buf.len());
        }
    }

    /// Tests the boundary between 2-byte and 3-byte encoding.
    #[test]
    fn boundary_between_2byte_and_3byte() {
        // 16383 should be 2 bytes (max for 2-byte encoding with 14 bits)
        let (len_16383, _) = encode_bytes(16383);

        // 16384 should require 3 bytes
        let (len_16384, _) = encode_bytes(16384);

        assert!(
            len_16384 > len_16383,
            "16384 ({} bytes) should require more bytes than 16383 ({} bytes)",
            len_16384,
            len_16383
        );
    }

    /// Tests decoding 2-byte sequences with first byte having bit 7 set.
    #[test]
    fn decode_2byte_sequences() {
        // Test that bytes 0x80-0xBF indicate 1 extra byte
        for first_byte in 0x80u8..=0xBF {
            let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
            assert_eq!(
                extra, 1,
                "byte 0x{:02X} should have 1 extra byte, got {}",
                first_byte, extra
            );
        }
    }

    /// Tests truncated 2-byte input handling.
    #[test]
    fn truncated_2byte_input_fails() {
        // 0x80 indicates 1 extra byte is needed
        let data = [0x80u8];
        let err = decode_varint(&data).expect_err("truncated input must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// Tests multiple consecutive 2-byte values.
    #[test]
    fn multiple_2byte_values_in_stream() {
        let values = [128, 255, 256, 1000, 8000, 16000];
        let mut encoded = Vec::new();
        for &v in &values {
            encode_varint_to_vec(v, &mut encoded);
        }

        let mut cursor = Cursor::new(&encoded);
        for &expected in &values {
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(decoded, expected);
        }
        assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// Tests that the encoded format matches known upstream values.
    #[test]
    fn encoding_matches_upstream_format() {
        // Known encodings from upstream rsync io.c
        let cases = [
            (128, vec![0x80, 0x80]),
            (255, vec![0x80, 0xFF]),
            (256, vec![0x81, 0x00]),
        ];
        for (value, expected) in cases {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert_eq!(
                encoded, expected,
                "value {} encoded as {:02X?}, expected {:02X?}",
                value, encoded, expected
            );
        }
    }
}

// ===========================================================================
// PHASE 2.9: VARINT EXTENDED ENCODING TESTS (>16383)
// ===========================================================================

/// Tests for varint extended encoding (values > 16383).
///
/// Values larger than 16383 require 3, 4, or 5 bytes depending on magnitude.
/// The encoding uses progressively more indicator bits in the first byte:
/// - `110x_xxxx` + 2 bytes for 3-byte encoding
/// - `1110_xxxx` + 3 bytes for 4-byte encoding
/// - `1111_0xxx` + 4 bytes for 5-byte encoding (full i32 range)
///
/// These tests verify extended encoding for large positive and negative values.
#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod phase2_9_varint_extended_encoding {
    use super::*;
    use std::io::Cursor;

    /// Tests 3-byte encoding boundary (16384).
    #[test]
    fn boundary_16384_is_3_bytes() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(16384, &mut encoded);
        assert_eq!(encoded.len(), 3);
        // Verify known encoding
        assert_eq!(encoded, vec![0xC0, 0x00, 0x40]);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 16384);
    }

    /// Tests values requiring 3 bytes.
    #[test]
    fn roundtrip_3byte_values() {
        let values = [
            16384, // Minimum 3-byte
            20000, 50000, 100000, 500000, 1000000,
            2097151, // Maximum that might fit in 3 bytes
        ];
        for value in values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for value {}", value);
            assert!(remainder.is_empty());
        }
    }

    /// Tests values requiring 4 bytes.
    #[test]
    fn roundtrip_4byte_values() {
        let values = [
            0x20_0000,   // 2097152
            0x100_0000,  // 16777216
            0x1000_0000, // 268435456
        ];
        for value in values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for value {}", value);
            assert!(remainder.is_empty());
        }
    }

    /// Tests values requiring 5 bytes (maximum encoding).
    #[test]
    fn roundtrip_5byte_values() {
        let values = [
            0x1000_0000_i32, // 268435456
            0x4000_0000_i32, // 1073741824
            i32::MAX,        // 2147483647
        ];
        for value in values {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert!(
                encoded.len() <= 5,
                "i32 values should encode to at most 5 bytes"
            );

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for value {}", value);
            assert!(remainder.is_empty());
        }
    }

    /// Tests known 5-byte encoding from upstream.
    #[test]
    fn known_5byte_encoding() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(1_073_741_824, &mut encoded);
        // Verify against known upstream encoding
        assert_eq!(encoded, vec![0xF0, 0x00, 0x00, 0x00, 0x40]);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, 1_073_741_824);
    }

    /// Tests negative values (require 5 bytes due to sign extension).
    #[test]
    fn negative_values_require_5_bytes() {
        let negatives = [-1, -128, -129, -32768, i32::MIN];
        for value in negatives {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            // Negative values always need 5 bytes due to sign bits
            assert_eq!(
                encoded.len(),
                5,
                "negative value {} should encode to 5 bytes, got {}",
                value,
                encoded.len()
            );

            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value);
        }
    }

    /// Tests known negative value encodings from upstream.
    #[test]
    fn known_negative_encodings() {
        let cases = [
            (-1, vec![0xF0, 0xFF, 0xFF, 0xFF, 0xFF]),
            (-128, vec![0xF0, 0x80, 0xFF, 0xFF, 0xFF]),
            (-129, vec![0xF0, 0x7F, 0xFF, 0xFF, 0xFF]),
            (-32768, vec![0xF0, 0x00, 0x80, 0xFF, 0xFF]),
        ];
        for (value, expected) in cases {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert_eq!(
                encoded, expected,
                "value {} encoded as {:02X?}, expected {:02X?}",
                value, encoded, expected
            );
        }
    }

    /// Tests i32::MAX boundary.
    #[test]
    fn boundary_i32_max() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(i32::MAX, &mut encoded);
        assert_eq!(encoded.len(), 5);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, i32::MAX);
    }

    /// Tests i32::MIN boundary.
    #[test]
    fn boundary_i32_min() {
        let mut encoded = Vec::new();
        encode_varint_to_vec(i32::MIN, &mut encoded);
        assert_eq!(encoded.len(), 5);

        let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, i32::MIN);
    }

    /// Tests INT_BYTE_EXTRA table for extended encodings.
    #[test]
    fn int_byte_extra_for_extended_encodings() {
        // 0xC0-0xDF (192-223) / 4 -> indices 48-55: 2 extra bytes
        for first_byte in 0xC0u8..=0xDF {
            let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
            assert_eq!(
                extra, 2,
                "byte 0x{:02X} should have 2 extra bytes",
                first_byte
            );
        }

        // 0xE0-0xEF (224-239) / 4 -> indices 56-59: 3 extra bytes
        for first_byte in 0xE0u8..=0xEF {
            let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
            assert_eq!(
                extra, 3,
                "byte 0x{:02X} should have 3 extra bytes",
                first_byte
            );
        }

        // 0xF0-0xF7 (240-247) / 4 -> indices 60-61: 4 extra bytes
        for first_byte in 0xF0u8..=0xF7 {
            let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
            assert_eq!(
                extra, 4,
                "byte 0x{:02X} should have 4 extra bytes",
                first_byte
            );
        }
    }

    /// Tests truncated extended encoding input.
    #[test]
    fn truncated_extended_encodings_fail() {
        // 3-byte truncated (need 2 extra, only have 1)
        let err = decode_varint(&[0xC0, 0x00]).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        // 4-byte truncated (need 3 extra, only have 2)
        let err = decode_varint(&[0xE0, 0x00, 0x00]).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        // 5-byte truncated (need 4 extra, only have 3)
        let err = decode_varint(&[0xF0, 0x00, 0x00, 0x00]).expect_err("truncated must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// Tests overflow tag bytes (0xF8-0xFF indicate >4 extra bytes).
    #[test]
    fn overflow_tag_bytes_are_rejected() {
        // 0xF8-0xFB indicate 5 extra bytes (overflow for i32)
        let err = decode_varint(&[0xF8, 0, 0, 0, 0, 0]).expect_err("overflow must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("overflow"));

        // 0xFC-0xFF indicate 6 extra bytes (overflow for i32)
        let err = decode_varint(&[0xFC, 0, 0, 0, 0, 0, 0]).expect_err("overflow must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// Tests stream round-trip for extended values.
    #[test]
    fn stream_roundtrip_extended_values() {
        let values = [16384, 100000, 1000000, i32::MAX, -1, -1000, i32::MIN];
        for value in values {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");

            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(decoded, value, "stream round-trip failed for {}", value);
            assert_eq!(cursor.position() as usize, buf.len());
        }
    }

    /// Tests multiple extended values in sequence.
    #[test]
    fn multiple_extended_values_in_sequence() {
        let values = [16384, -1, 1000000, i32::MAX, i32::MIN, 100000];
        let mut encoded = Vec::new();
        for &v in &values {
            encode_varint_to_vec(v, &mut encoded);
        }

        let mut remaining = encoded.as_slice();
        for &expected in &values {
            let (decoded, rest) = decode_varint(remaining).expect("decode succeeds");
            assert_eq!(decoded, expected);
            remaining = rest;
        }
        assert!(remaining.is_empty());
    }

    /// Tests powers of two in extended range.
    #[test]
    fn powers_of_two_extended() {
        for shift in 14..31 {
            let value: i32 = 1 << shift;
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for 2^{}", shift);
        }
    }
}

// ===========================================================================
// PROPERTY-BASED TESTS
// ===========================================================================

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    proptest! {
        /// Property: varint encode/decode roundtrip preserves all i32 values.
        #[test]
        fn varint_roundtrip_all_i32(value: i32) {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            prop_assert_eq!(decoded, value);
            prop_assert!(remainder.is_empty());
        }

        /// Property: varint stream encode/decode roundtrip via Read/Write traits.
        #[test]
        fn varint_stream_roundtrip(value: i32) {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");
            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            prop_assert_eq!(decoded, value);
        }

        /// Property: fixed int (4-byte) roundtrip preserves all i32 values.
        #[test]
        fn fixed_int_roundtrip(value: i32) {
            let mut buf = Vec::new();
            write_int(&mut buf, value).expect("write succeeds");
            prop_assert_eq!(buf.len(), 4);
            let mut cursor = Cursor::new(&buf);
            let decoded = read_int(&mut cursor).expect("read succeeds");
            prop_assert_eq!(decoded, value);
        }

        /// Property: varlong roundtrip preserves non-negative i64 values with min_bytes=8.
        /// Note: varlong is designed for unsigned values (file sizes, timestamps).
        #[test]
        fn varlong_roundtrip_full_range(value in 0i64..=i64::MAX) {
            let mut buf = Vec::new();
            write_varlong(&mut buf, value, 8).expect("write succeeds");
            let mut cursor = Cursor::new(&buf);
            let decoded = read_varlong(&mut cursor, 8).expect("read succeeds");
            prop_assert_eq!(decoded, value);
        }

        /// Property: varlong roundtrip preserves small values for various min_bytes.
        #[test]
        fn varlong_roundtrip_small_values(value in 0i64..=0xFF_FFFF, min_bytes in 1u8..=8) {
            let mut buf = Vec::new();
            write_varlong(&mut buf, value, min_bytes).expect("write succeeds");
            let mut cursor = Cursor::new(&buf);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
            prop_assert_eq!(decoded, value);
        }

        /// Property: varint30_int roundtrip works for protocol versions.
        #[test]
        fn varint30_int_roundtrip_proptest(value: i32, proto in 28u8..=32) {
            let mut buf = Vec::new();
            write_varint30_int(&mut buf, value, proto).expect("write succeeds");
            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint30_int(&mut cursor, proto).expect("read succeeds");
            prop_assert_eq!(decoded, value);
        }

        /// Property: longint roundtrip preserves i64 values in 32-bit range.
        #[test]
        fn longint_roundtrip_32bit(value: i32) {
            let value64 = i64::from(value);
            let mut buf = Vec::new();
            write_longint(&mut buf, value64).expect("write succeeds");
            let mut cursor = Cursor::new(&buf);
            let decoded = read_longint(&mut cursor).expect("read succeeds");
            prop_assert_eq!(decoded, value64);
        }

        /// Property: encoding length increases monotonically with value magnitude.
        #[test]
        fn varint_encoding_length_monotonic(a: u16, b: u16) {
            let smaller = i32::from(a.min(b));
            let larger = i32::from(a.max(b));
            let (len_small, _) = encode_bytes(smaller);
            let (len_large, _) = encode_bytes(larger);
            prop_assert!(len_small <= len_large,
                "smaller value {} (len {}) should not encode longer than {} (len {})",
                smaller, len_small, larger, len_large);
        }

        /// Property: varint encoding uses at most 5 bytes for any i32.
        #[test]
        fn varint_max_encoding_length(value: i32) {
            let (len, _) = encode_bytes(value);
            prop_assert!(len <= 5, "varint encoding should use at most 5 bytes, got {}", len);
        }
    }
}

// ===========================================================================
// BYTE BOUNDARY TESTS (Task #362)
// ===========================================================================

/// Tests for varint encoding at exact byte boundaries.
///
/// The rsync varint encoding uses a variable number of bytes based on value magnitude:
/// - 1 byte: 0-127 (7 bits of data)
/// - 2 bytes: 128-16383 (14 bits of data)
/// - 3 bytes: 16384-2097151 (21 bits of data)
/// - 4 bytes: 2097152-268435455 (28 bits of data)
/// - 5 bytes: larger values and negatives
///
/// These tests verify correct behavior at exact byte boundaries:
/// - Maximum value for each byte count (127, 16383, 2097151, 268435455)
/// - Minimum value requiring next byte count (128, 16384, 2097152, 268435456)
/// - Encoding produces minimum bytes needed
/// - Round-trip consistency for all boundary values
#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod byte_boundary_tests {
    use super::*;
    use std::io::Cursor;

    // =========================================================================
    // EXACT BOUNDARY VALUES
    // =========================================================================

    /// The exact boundary values where encoding size changes.
    /// Format: (value, expected_byte_count, description)
    const BYTE_BOUNDARIES: [(i32, usize, &str); 8] = [
        (127, 1, "max 1-byte (7-bit boundary)"),
        (128, 2, "min 2-byte (just above 7-bit)"),
        (16383, 2, "max 2-byte (14-bit boundary)"),
        (16384, 3, "min 3-byte (just above 14-bit)"),
        (2097151, 3, "max 3-byte (21-bit boundary)"),
        (2097152, 4, "min 4-byte (just above 21-bit)"),
        (268435455, 4, "max 4-byte (28-bit boundary)"),
        (268435456, 5, "min 5-byte (just above 28-bit)"),
    ];

    // =========================================================================
    // TEST: Encoding produces minimum bytes needed
    // =========================================================================

    /// Verifies that each boundary value encodes to exactly the expected number of bytes.
    #[test]
    fn encoding_produces_minimum_bytes_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let (actual_len, _bytes) = encode_bytes(value);
            assert_eq!(
                actual_len, expected_len,
                "Boundary '{}': value {} should encode to {} bytes, got {}",
                desc, value, expected_len, actual_len
            );
        }
    }

    /// Verifies encode_varint_to_vec produces minimum bytes at boundaries.
    #[test]
    fn encode_varint_to_vec_minimum_bytes_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            assert_eq!(
                encoded.len(),
                expected_len,
                "Boundary '{}': value {} should encode to {} bytes, got {}",
                desc,
                value,
                expected_len,
                encoded.len()
            );
        }
    }

    /// Verifies write_varint produces minimum bytes at boundaries.
    #[test]
    fn write_varint_minimum_bytes_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");
            assert_eq!(
                buf.len(),
                expected_len,
                "Boundary '{}': value {} should write {} bytes, got {}",
                desc,
                value,
                expected_len,
                buf.len()
            );
        }
    }

    // =========================================================================
    // TEST: Round-trip consistency for boundary values
    // =========================================================================

    /// Verifies decode_varint round-trips correctly for all boundary values.
    #[test]
    fn roundtrip_decode_varint_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(
                decoded, value,
                "Boundary '{}': round-trip failed, expected {}, got {}",
                desc, value, decoded
            );
            assert!(
                remainder.is_empty(),
                "Boundary '{}': {} bytes should remain, found {}",
                desc,
                0,
                remainder.len()
            );
            assert_eq!(
                encoded.len(),
                expected_len,
                "Boundary '{}': encoding length mismatch",
                desc
            );
        }
    }

    /// Verifies read_varint round-trips correctly for all boundary values.
    #[test]
    fn roundtrip_read_varint_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let mut buf = Vec::new();
            write_varint(&mut buf, value).expect("write succeeds");

            let mut cursor = Cursor::new(&buf);
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(
                decoded, value,
                "Boundary '{}': stream round-trip failed, expected {}, got {}",
                desc, value, decoded
            );
            assert_eq!(
                cursor.position() as usize,
                expected_len,
                "Boundary '{}': cursor should be at position {}, found {}",
                desc,
                expected_len,
                cursor.position()
            );
        }
    }

    // =========================================================================
    // TEST: 7-bit boundary (1-byte to 2-byte transition)
    // =========================================================================

    /// Tests the exact 7-bit boundary: 127 is max 1-byte, 128 is min 2-byte.
    #[test]
    fn boundary_7bit_127_to_128() {
        // 127 = 0x7F = 0111_1111 - fits in 7 bits, should be 1 byte
        let (len_127, bytes_127) = encode_bytes(127);
        assert_eq!(len_127, 1, "127 should be 1 byte");
        assert_eq!(bytes_127[0], 0x7F, "127 should encode as 0x7F");

        // 128 = 0x80 - does NOT fit in 7 bits, needs 2 bytes
        let (len_128, _) = encode_bytes(128);
        assert_eq!(len_128, 2, "128 should be 2 bytes");

        // Verify the transition is exact
        assert_eq!(
            len_128,
            len_127 + 1,
            "128 should need exactly one more byte than 127"
        );
    }

    /// Tests values immediately adjacent to the 7-bit boundary.
    #[test]
    fn boundary_7bit_adjacent_values() {
        // Values 126, 127, 128, 129
        let (len_126, _) = encode_bytes(126);
        let (len_127, _) = encode_bytes(127);
        let (len_128, _) = encode_bytes(128);
        let (len_129, _) = encode_bytes(129);

        assert_eq!(len_126, 1, "126 should be 1 byte");
        assert_eq!(len_127, 1, "127 should be 1 byte");
        assert_eq!(len_128, 2, "128 should be 2 bytes");
        assert_eq!(len_129, 2, "129 should be 2 bytes");
    }

    // =========================================================================
    // TEST: 14-bit boundary (2-byte to 3-byte transition)
    // =========================================================================

    /// Tests the exact 14-bit boundary: 16383 is max 2-byte, 16384 is min 3-byte.
    #[test]
    fn boundary_14bit_16383_to_16384() {
        // 16383 = 0x3FFF = 0011_1111_1111_1111 - fits in 14 bits, should be 2 bytes
        let (len_16383, _) = encode_bytes(16383);
        assert_eq!(len_16383, 2, "16383 should be 2 bytes");

        // 16384 = 0x4000 - does NOT fit in 14 bits, needs 3 bytes
        let (len_16384, _) = encode_bytes(16384);
        assert_eq!(len_16384, 3, "16384 should be 3 bytes");

        // Verify the transition is exact
        assert_eq!(
            len_16384,
            len_16383 + 1,
            "16384 should need exactly one more byte than 16383"
        );
    }

    /// Tests values immediately adjacent to the 14-bit boundary.
    #[test]
    fn boundary_14bit_adjacent_values() {
        let (len_16382, _) = encode_bytes(16382);
        let (len_16383, _) = encode_bytes(16383);
        let (len_16384, _) = encode_bytes(16384);
        let (len_16385, _) = encode_bytes(16385);

        assert_eq!(len_16382, 2, "16382 should be 2 bytes");
        assert_eq!(len_16383, 2, "16383 should be 2 bytes");
        assert_eq!(len_16384, 3, "16384 should be 3 bytes");
        assert_eq!(len_16385, 3, "16385 should be 3 bytes");
    }

    /// Tests round-trip for 14-bit boundary values.
    #[test]
    fn boundary_14bit_roundtrip() {
        for value in [16382, 16383, 16384, 16385] {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for {}", value);
            assert!(remainder.is_empty());
        }
    }

    // =========================================================================
    // TEST: 21-bit boundary (3-byte to 4-byte transition)
    // =========================================================================

    /// Tests the exact 21-bit boundary: 2097151 is max 3-byte, 2097152 is min 4-byte.
    #[test]
    fn boundary_21bit_2097151_to_2097152() {
        // 2097151 = 0x1FFFFF = 0001_1111_1111_1111_1111_1111 - fits in 21 bits
        let (len_2097151, _) = encode_bytes(2097151);
        assert_eq!(len_2097151, 3, "2097151 should be 3 bytes");

        // 2097152 = 0x200000 - does NOT fit in 21 bits, needs 4 bytes
        let (len_2097152, _) = encode_bytes(2097152);
        assert_eq!(len_2097152, 4, "2097152 should be 4 bytes");

        // Verify the transition is exact
        assert_eq!(
            len_2097152,
            len_2097151 + 1,
            "2097152 should need exactly one more byte than 2097151"
        );
    }

    /// Tests values immediately adjacent to the 21-bit boundary.
    #[test]
    fn boundary_21bit_adjacent_values() {
        let (len_2097150, _) = encode_bytes(2097150);
        let (len_2097151, _) = encode_bytes(2097151);
        let (len_2097152, _) = encode_bytes(2097152);
        let (len_2097153, _) = encode_bytes(2097153);

        assert_eq!(len_2097150, 3, "2097150 should be 3 bytes");
        assert_eq!(len_2097151, 3, "2097151 should be 3 bytes");
        assert_eq!(len_2097152, 4, "2097152 should be 4 bytes");
        assert_eq!(len_2097153, 4, "2097153 should be 4 bytes");
    }

    /// Tests round-trip for 21-bit boundary values.
    #[test]
    fn boundary_21bit_roundtrip() {
        for value in [2097150, 2097151, 2097152, 2097153] {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for {}", value);
            assert!(remainder.is_empty());
        }
    }

    // =========================================================================
    // TEST: 28-bit boundary (4-byte to 5-byte transition)
    // =========================================================================

    /// Tests the exact 28-bit boundary: 268435455 is max 4-byte, 268435456 is min 5-byte.
    #[test]
    fn boundary_28bit_268435455_to_268435456() {
        // 268435455 = 0x0FFFFFFF - fits in 28 bits
        let (len_max4, _) = encode_bytes(268435455);
        assert_eq!(len_max4, 4, "268435455 should be 4 bytes");

        // 268435456 = 0x10000000 - does NOT fit in 28 bits, needs 5 bytes
        let (len_min5, _) = encode_bytes(268435456);
        assert_eq!(len_min5, 5, "268435456 should be 5 bytes");

        // Verify the transition is exact
        assert_eq!(
            len_min5,
            len_max4 + 1,
            "268435456 should need exactly one more byte than 268435455"
        );
    }

    /// Tests values immediately adjacent to the 28-bit boundary.
    #[test]
    fn boundary_28bit_adjacent_values() {
        let (len_268435454, _) = encode_bytes(268435454);
        let (len_268435455, _) = encode_bytes(268435455);
        let (len_268435456, _) = encode_bytes(268435456);
        let (len_268435457, _) = encode_bytes(268435457);

        assert_eq!(len_268435454, 4, "268435454 should be 4 bytes");
        assert_eq!(len_268435455, 4, "268435455 should be 4 bytes");
        assert_eq!(len_268435456, 5, "268435456 should be 5 bytes");
        assert_eq!(len_268435457, 5, "268435457 should be 5 bytes");
    }

    /// Tests round-trip for 28-bit boundary values.
    #[test]
    fn boundary_28bit_roundtrip() {
        for value in [268435454, 268435455, 268435456, 268435457] {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for {}", value);
            assert!(remainder.is_empty());
        }
    }

    // =========================================================================
    // TEST: All boundary transitions in one test
    // =========================================================================

    /// Comprehensive test: verifies all byte boundary transitions occur at exact values.
    #[test]
    fn all_byte_boundary_transitions() {
        // (max value for N bytes, min value for N+1 bytes)
        let transitions = [
            (127_i32, 128_i32, 1, 2),     // 7-bit boundary
            (16383, 16384, 2, 3),         // 14-bit boundary
            (2097151, 2097152, 3, 4),     // 21-bit boundary
            (268435455, 268435456, 4, 5), // 28-bit boundary
        ];

        for (max_n, min_n_plus_1, n_bytes, n_plus_1_bytes) in transitions {
            let (len_max, _) = encode_bytes(max_n);
            let (len_min, _) = encode_bytes(min_n_plus_1);

            assert_eq!(
                len_max, n_bytes,
                "Value {} should be {} bytes",
                max_n, n_bytes
            );
            assert_eq!(
                len_min, n_plus_1_bytes,
                "Value {} should be {} bytes",
                min_n_plus_1, n_plus_1_bytes
            );

            // Round-trip both values
            for value in [max_n, min_n_plus_1] {
                let mut encoded = Vec::new();
                encode_varint_to_vec(value, &mut encoded);
                let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
                assert_eq!(decoded, value, "round-trip failed for {}", value);
            }
        }
    }

    // =========================================================================
    // TEST: Verify exact wire format at boundaries
    // =========================================================================

    /// Tests the exact wire format for 7-bit boundary values.
    #[test]
    fn wire_format_7bit_boundary() {
        // 127 = 0x7F should encode as single byte 0x7F
        let mut enc_127 = Vec::new();
        encode_varint_to_vec(127, &mut enc_127);
        assert_eq!(enc_127, vec![0x7F], "127 should encode as [0x7F]");

        // 128 = 0x80 should encode as 2 bytes with high bit set
        let mut enc_128 = Vec::new();
        encode_varint_to_vec(128, &mut enc_128);
        assert_eq!(enc_128.len(), 2, "128 should encode as 2 bytes");
        // First byte should have high bit set (10xx_xxxx pattern)
        assert!(
            enc_128[0] & 0x80 != 0,
            "128's first byte should have high bit set"
        );
    }

    /// Tests the exact wire format for 14-bit boundary values.
    #[test]
    fn wire_format_14bit_boundary() {
        // 16383 = 0x3FFF should encode as 2 bytes
        let mut enc_16383 = Vec::new();
        encode_varint_to_vec(16383, &mut enc_16383);
        assert_eq!(enc_16383.len(), 2, "16383 should encode as 2 bytes");

        // 16384 = 0x4000 should encode as 3 bytes
        let mut enc_16384 = Vec::new();
        encode_varint_to_vec(16384, &mut enc_16384);
        assert_eq!(enc_16384.len(), 3, "16384 should encode as 3 bytes");
        // First byte should have 110x_xxxx pattern
        assert!(
            enc_16384[0] & 0xC0 == 0xC0,
            "16384's first byte should have 110x_xxxx pattern"
        );
    }

    // =========================================================================
    // TEST: Stream operations at boundaries
    // =========================================================================

    /// Tests streaming multiple boundary values and verifying cursor positions.
    #[test]
    fn stream_multiple_boundary_values() {
        let values: Vec<i32> = BYTE_BOUNDARIES.iter().map(|(v, _, _)| *v).collect();
        let expected_lengths: Vec<usize> = BYTE_BOUNDARIES.iter().map(|(_, l, _)| *l).collect();

        // Write all values
        let mut buf = Vec::new();
        for &value in &values {
            write_varint(&mut buf, value).expect("write succeeds");
        }

        // Verify total length
        let expected_total: usize = expected_lengths.iter().sum();
        assert_eq!(
            buf.len(),
            expected_total,
            "total encoded length should be {}",
            expected_total
        );

        // Read all values back and verify cursor positions
        let mut cursor = Cursor::new(&buf);
        let mut cumulative_pos = 0usize;
        for (i, (&expected_value, &expected_len)) in
            values.iter().zip(expected_lengths.iter()).enumerate()
        {
            let decoded = read_varint(&mut cursor).expect("read succeeds");
            assert_eq!(
                decoded, expected_value,
                "value {} mismatch at index {}",
                expected_value, i
            );
            cumulative_pos += expected_len;
            assert_eq!(
                cursor.position() as usize,
                cumulative_pos,
                "cursor position mismatch after reading value {} at index {}",
                expected_value,
                i
            );
        }
    }

    // =========================================================================
    // TEST: decode_bytes at boundaries
    // =========================================================================

    /// Tests decode_bytes returns correct consumed byte count at boundaries.
    #[test]
    fn decode_bytes_consumed_at_boundaries() {
        for (value, expected_len, desc) in BYTE_BOUNDARIES {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);

            let (decoded, consumed) = decode_bytes(&encoded).expect("decode succeeds");
            assert_eq!(
                decoded, value,
                "Boundary '{}': decoded value mismatch",
                desc
            );
            assert_eq!(
                consumed, expected_len,
                "Boundary '{}': consumed byte count should be {}",
                desc, expected_len
            );
        }
    }
}
