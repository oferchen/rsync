//! Property-based tests for varint encoding boundaries.
//!
//! Verifies encoding minimality, determinism, and roundtrip correctness at
//! byte-size boundaries for all varint codec variants (varint i32, longint i64,
//! varlong i64). These complement the roundtrip tests in
//! `proptest_codec_roundtrips.rs` by focusing on encoding size invariants.

use proptest::prelude::*;
use protocol::{
    decode_varint, encode_varint_to_vec, read_longint, read_varint, read_varint30_int,
    read_varlong, read_varlong30, write_longint, write_varint30_int, write_varlong,
    write_varlong30,
};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Varint (i32) encoding size boundaries
// ---------------------------------------------------------------------------

/// Returns the expected encoding length for a given i32 value.
///
/// The varint format encodes non-negative values compactly:
/// - 1 byte: 0..=127
/// - 2 bytes: 128..=16383
/// - 3 bytes: 16384..=2097151
/// - 4 bytes: 2097152..=268435455
/// - 5 bytes: 268435456..=i32::MAX and all negative values
fn expected_varint_len(value: i32) -> usize {
    let unsigned = value as u32;
    if unsigned <= 0x7F {
        1
    } else if unsigned <= 0x3FFF {
        2
    } else if unsigned <= 0x1F_FFFF {
        3
    } else if unsigned <= 0x0FFF_FFFF {
        4
    } else {
        5
    }
}

proptest! {
    /// Varint encoding length matches the expected size for all i32 values.
    ///
    /// This verifies encoding minimality - the encoder never uses more bytes
    /// than strictly necessary for any given value.
    #[test]
    fn varint_encoding_length_matches_expected(value in any::<i32>()) {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let expected = expected_varint_len(value);
        prop_assert_eq!(
            encoded.len(), expected,
            "value {} (0x{:08X}): expected {} bytes, got {}",
            value, value, expected, encoded.len()
        );
    }

    /// Encoding is minimal: no shorter byte sequence decodes to the same value.
    ///
    /// We verify this indirectly by checking that the encoded length equals the
    /// theoretical minimum derived from the value's magnitude.
    #[test]
    fn varint_encoding_is_minimal(value in 0i32..=i32::MAX) {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        // For non-negative values, verify that a shorter encoding would be
        // insufficient by checking the value exceeds the max for the previous tier.
        let len = encoded.len();
        if len > 1 {
            let prev_tier_max: u32 = match len {
                2 => 0x7F,
                3 => 0x3FFF,
                4 => 0x1F_FFFF,
                5 => 0x0FFF_FFFF,
                _ => unreachable!(),
            };
            prop_assert!(
                (value as u32) > prev_tier_max,
                "value {} fits in {} bytes but encoded as {}",
                value, len - 1, len
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Varint (i32) roundtrip with u32 bit patterns
// ---------------------------------------------------------------------------

proptest! {
    /// All u32 bit patterns roundtrip through varint when reinterpreted as i32.
    ///
    /// rsync's varint codec operates on i32 but the wire format preserves the
    /// full 32-bit pattern. This test ensures no bit pattern is lost.
    #[test]
    fn varint_roundtrip_all_u32_bit_patterns(bits in any::<u32>()) {
        let value = bits as i32;
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        // Slice-based decode
        let (decoded, remainder) = decode_varint(&encoded)?;
        prop_assert_eq!(decoded, value);
        prop_assert!(remainder.is_empty());

        // Reader-based decode
        let mut cursor = Cursor::new(&encoded);
        let read_back = read_varint(&mut cursor)?;
        prop_assert_eq!(read_back, value);
    }

    /// Specific boundary values that sit at encoding tier transitions.
    #[test]
    fn varint_tier_boundary_roundtrip(
        offset in -2i32..=2i32
    ) {
        // Boundaries: 0x7F, 0x3FFF, 0x1FFFFF, 0x0FFFFFFF
        let boundaries: &[i32] = &[0x7F, 0x3FFF, 0x1F_FFFF, 0x0FFF_FFFF];
        for &boundary in boundaries {
            let value = boundary.saturating_add(offset);
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded)?;
            prop_assert_eq!(decoded, value);
        }
    }
}

// ---------------------------------------------------------------------------
// Longint (i64) property tests
// ---------------------------------------------------------------------------

proptest! {
    /// All i64 values roundtrip through longint encoding.
    ///
    /// Longint uses 4 bytes for values in 0..=0x7FFFFFFF and 12 bytes otherwise.
    #[test]
    fn longint_roundtrip_arbitrary_i64(value in any::<i64>()) {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor)?;
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// Longint encoding size is always either 4 or 12 bytes.
    #[test]
    fn longint_encoding_size_is_4_or_12(value in any::<i64>()) {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;

        let is_inline = (0..=0x7FFF_FFFF_i64).contains(&value);
        if is_inline {
            prop_assert_eq!(encoded.len(), 4, "inline value {} should be 4 bytes", value);
        } else {
            prop_assert_eq!(encoded.len(), 12, "extended value {} should be 12 bytes", value);
        }
    }

    /// Longint encoding is deterministic.
    #[test]
    fn longint_encoding_is_deterministic(value in any::<i64>()) {
        let mut enc1 = Vec::new();
        let mut enc2 = Vec::new();
        write_longint(&mut enc1, value)?;
        write_longint(&mut enc2, value)?;
        prop_assert_eq!(enc1, enc2);
    }

    /// Longint encoding is minimal - inline values never use 12 bytes.
    #[test]
    fn longint_encoding_is_minimal(value in 0i64..=0x7FFF_FFFF_i64) {
        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;
        prop_assert_eq!(
            encoded.len(), 4,
            "value {} fits in 4 bytes but got {} bytes", value, encoded.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Varlong (i64 with min_bytes) property tests
// ---------------------------------------------------------------------------

proptest! {
    /// Varlong with min_bytes=3 roundtrips file-size-range values.
    #[test]
    fn varlong_min3_roundtrip(value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64) {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 3)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 3)?;
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// Varlong with min_bytes=4 roundtrips timestamp-range values.
    #[test]
    fn varlong_min4_roundtrip(value in 0i64..=0x003F_FFFF_FFFF_FFFFi64) {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, 4)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 4)?;
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// Varlong encoding size is bounded: at most min_bytes + extra indicator bytes.
    ///
    /// The maximum encoding size is 9 bytes (1 leading byte + up to 8 data bytes).
    #[test]
    fn varlong_encoding_size_bounded(
        value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64,
        min_bytes in 3u8..=8u8
    ) {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes)?;

        // Leading byte + at most 8 data bytes
        prop_assert!(encoded.len() <= 9, "encoded {} bytes, max is 9", encoded.len());
        // Must be at least min_bytes (leading byte + min_bytes-1 data bytes)
        prop_assert!(
            encoded.len() >= min_bytes as usize,
            "encoded {} bytes, min is {}", encoded.len(), min_bytes
        );
    }

    /// Varlong encoding is deterministic for the same min_bytes.
    #[test]
    fn varlong_encoding_is_deterministic(
        value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64,
        min_bytes in 3u8..=8u8
    ) {
        let mut enc1 = Vec::new();
        let mut enc2 = Vec::new();
        write_varlong(&mut enc1, value, min_bytes)?;
        write_varlong(&mut enc2, value, min_bytes)?;
        prop_assert_eq!(enc1, enc2);
    }
}

// ---------------------------------------------------------------------------
// Explicit boundary value tests as property tests (parameterized)
// ---------------------------------------------------------------------------

proptest! {
    /// All specified boundary values roundtrip through varint.
    #[test]
    fn varint_explicit_boundaries_roundtrip(
        idx in 0usize..14
    ) {
        let boundaries: &[i32] = &[
            0, 1, 127, 128, 255, 256,
            0x7F, 0x80, 0xFF, 0x7FFF, 0xFFFF,
            u32::MAX as i32,  // -1
            i32::MIN,
            i32::MAX,
        ];
        let value = boundaries[idx];

        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        let (decoded, remainder) = decode_varint(&encoded)?;
        prop_assert_eq!(decoded, value);
        prop_assert!(remainder.is_empty());

        // Also verify via reader
        let mut cursor = Cursor::new(&encoded);
        let read_back = read_varint(&mut cursor)?;
        prop_assert_eq!(read_back, value);
    }

    /// All specified boundary values roundtrip through longint.
    #[test]
    fn longint_explicit_boundaries_roundtrip(
        idx in 0usize..14
    ) {
        let boundaries: &[i64] = &[
            0, 1, 127, 128, 255, 256,
            65535, 65536,
            u32::MAX as i64,
            i32::MIN as i64,
            i32::MAX as i64,
            i64::MAX,
            i64::MIN,
            -1,
        ];
        let value = boundaries[idx];

        let mut encoded = Vec::new();
        write_longint(&mut encoded, value)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_longint(&mut cursor)?;
        prop_assert_eq!(decoded, value);
    }
}

// ---------------------------------------------------------------------------
// Cross-codec consistency
// ---------------------------------------------------------------------------

proptest! {
    /// Values that fit in both varint (i32) and longint inline range produce
    /// compatible 4-byte LE encodings for longint.
    #[test]
    fn longint_inline_matches_le_bytes(value in 0i32..=0x7FFF_FFFF_i32) {
        let mut longint_encoded = Vec::new();
        write_longint(&mut longint_encoded, value as i64)?;

        // Inline longint should be the raw LE bytes of the i32
        let expected = value.to_le_bytes();
        prop_assert_eq!(
            longint_encoded.as_slice(), expected.as_slice(),
            "longint inline encoding should match raw LE bytes"
        );
    }

    /// Varlong with min_bytes=3 sequences roundtrip correctly.
    #[test]
    fn varlong_sequence_roundtrip(
        values in prop::collection::vec(0i64..=0x03FF_FFFF_FFFF_FFFFi64, 1..16)
    ) {
        let min_bytes = 3u8;
        let mut encoded = Vec::new();
        for &v in &values {
            write_varlong(&mut encoded, v, min_bytes)?;
        }

        let mut cursor = Cursor::new(&encoded);
        for &expected in &values {
            let decoded = read_varlong(&mut cursor, min_bytes)?;
            prop_assert_eq!(decoded, expected);
        }
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }
}

// ---------------------------------------------------------------------------
// Monotonicity: larger non-negative values never encode to fewer bytes
// ---------------------------------------------------------------------------

proptest! {
    /// For non-negative i32 values, a larger value never encodes to fewer bytes
    /// than a smaller value. This ensures the encoding preserves magnitude ordering
    /// in terms of wire size.
    #[test]
    fn varint_encoding_length_monotonic(a in 0u32..=u32::MAX, b in 0u32..=u32::MAX) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut enc_lo = Vec::new();
        let mut enc_hi = Vec::new();
        encode_varint_to_vec(lo as i32, &mut enc_lo);
        encode_varint_to_vec(hi as i32, &mut enc_hi);
        prop_assert!(
            enc_lo.len() <= enc_hi.len(),
            "value {} ({} bytes) > value {} ({} bytes)",
            lo, enc_lo.len(), hi, enc_hi.len()
        );
    }

    /// For non-negative i64 values in longint range, larger values never use
    /// fewer bytes than smaller values.
    #[test]
    fn longint_encoding_length_monotonic(a in 0i64..=i64::MAX, b in 0i64..=i64::MAX) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let mut enc_lo = Vec::new();
        let mut enc_hi = Vec::new();
        write_longint(&mut enc_lo, lo)?;
        write_longint(&mut enc_hi, hi)?;
        prop_assert!(
            enc_lo.len() <= enc_hi.len(),
            "value {} ({} bytes) > value {} ({} bytes)",
            lo, enc_lo.len(), hi, enc_hi.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Varlong30 (protocol 30+ alias) property tests
// ---------------------------------------------------------------------------

proptest! {
    /// write_varlong30/read_varlong30 roundtrips for arbitrary non-negative values.
    ///
    /// varlong30 is the protocol >= 30 entry point that delegates to varlong.
    /// This verifies the public API surface independently.
    #[test]
    fn varlong30_roundtrip(value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64, min_bytes in 3u8..=8u8) {
        let mut encoded = Vec::new();
        write_varlong30(&mut encoded, value, min_bytes)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong30(&mut cursor, min_bytes)?;
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// varlong30 produces identical bytes to varlong for all inputs.
    #[test]
    fn varlong30_matches_varlong(value in 0i64..=0x03FF_FFFF_FFFF_FFFFi64, min_bytes in 3u8..=8u8) {
        let mut enc30 = Vec::new();
        let mut enc_plain = Vec::new();
        write_varlong30(&mut enc30, value, min_bytes)?;
        write_varlong(&mut enc_plain, value, min_bytes)?;
        prop_assert_eq!(enc30, enc_plain);
    }
}

// ---------------------------------------------------------------------------
// Varint30 (protocol-versioned i32) property tests
// ---------------------------------------------------------------------------

proptest! {
    /// write_varint30_int/read_varint30_int roundtrips for protocol >= 30 (varint mode).
    #[test]
    fn varint30_int_proto30_roundtrip(value in any::<i32>()) {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, 30)?;

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varint30_int(&mut cursor, 30)?;
        prop_assert_eq!(decoded, value);
        prop_assert_eq!(cursor.position() as usize, encoded.len());
    }

    /// write_varint30_int/read_varint30_int roundtrips for protocol < 30 (fixed 4-byte mode).
    #[test]
    fn varint30_int_proto29_roundtrip(value in any::<i32>()) {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, 29)?;

        // Fixed 4-byte encoding
        prop_assert_eq!(encoded.len(), 4);

        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varint30_int(&mut cursor, 29)?;
        prop_assert_eq!(decoded, value);
    }

    /// Protocol < 30 always produces exactly 4 bytes regardless of value magnitude.
    #[test]
    fn varint30_int_proto_below_30_always_4_bytes(
        value in any::<i32>(),
        proto in 0u8..30u8
    ) {
        let mut encoded = Vec::new();
        write_varint30_int(&mut encoded, value, proto)?;
        prop_assert_eq!(encoded.len(), 4);
    }
}
