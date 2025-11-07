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
//! use oc_rsync_protocol::{decode_varint, encode_varint_to_vec};
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
pub fn write_varint<W: Write>(writer: &mut W, value: i32) -> io::Result<()> {
    let (len, bytes) = encode_bytes(value);
    writer.write_all(&bytes[..len])
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
pub fn read_varint<R: Read>(reader: &mut R) -> io::Result<i32> {
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

    Ok(i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
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
}
