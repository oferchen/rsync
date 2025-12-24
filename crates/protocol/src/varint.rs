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

fn encode_bytes(value: i32) -> (usize, [u8; 5]) {
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

fn decode_bytes(bytes: &[u8]) -> io::Result<(i32, usize)> {
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
    // Debug logging removed - eprintln! crashes when stderr unavailable in daemon threads
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
    let mask = if cnt == min_bytes as usize {
        // No flag bits set - all 8 bits of leading byte are data
        0xFF
    } else if cnt == 8 {
        // All bits set - special case
        0xFF
    } else {
        // 'bit' is the first zero bit we encountered
        // All bits below it are data bits
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
}
