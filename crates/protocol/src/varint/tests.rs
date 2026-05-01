#![allow(clippy::uninlined_format_args)]

use super::decode::{decode_bytes, decode_varint, read_varint};
use super::encode::{encode_bytes, encode_varint_to_vec, write_varint};
use super::table::{INT_BYTE_EXTRA, invalid_data};
use super::*;
use proptest::prelude::*;
use std::io;
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
    let test_cases = [
        (0i64, 3u8),
        (1i64, 3u8),
        (255i64, 3u8),
        (65536i64, 3u8),
        (16777215i64, 3u8),
        (16777216i64, 3u8),
        (1700000000i64, 4u8),
        (i64::MAX, 3u8),
        (i64::MAX, 8u8),
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
    let test_cases = [
        (i64::MAX, 3u8),
        (i64::MAX / 2, 3u8),
        (0x03FF_FFFF_FFFF_FFFFi64, 3u8),
        (1_000_000_000_000_000i64, 3u8),
        (100_000_000_000_000i64, 3u8),
        (1_000_000_000_000i64, 3u8),
        (1_000_000_000i64, 3u8),
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
    let data = [127u8];
    let (value, remainder) = decode_varint(&data).expect("decode succeeds");
    assert_eq!(value, 127);
    assert!(remainder.is_empty());
}

#[test]
fn decode_varint_boundary_128() {
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
    let mut small = Vec::new();
    encode_varint_to_vec(1, &mut small);

    let mut large = Vec::new();
    encode_varint_to_vec(1_000_000_000, &mut large);

    assert!(small.len() < large.len());
}

#[test]
fn write_longint_small_positive() {
    let mut output = Vec::new();
    write_longint(&mut output, 42).expect("write succeeds");
    assert_eq!(output.len(), 4);
    let value = i32::from_le_bytes(output.try_into().unwrap());
    assert_eq!(value, 42);
}

#[test]
fn write_longint_max_inline() {
    let max_inline = 0x7FFF_FFFF_i64;
    let mut output = Vec::new();
    write_longint(&mut output, max_inline).expect("write succeeds");
    assert_eq!(output.len(), 4);
}

#[test]
fn write_longint_above_max_inline() {
    let above_inline = 0x8000_0000_i64;
    let mut output = Vec::new();
    write_longint(&mut output, above_inline).expect("write succeeds");
    assert_eq!(output.len(), 12);

    let marker = u32::from_le_bytes(output[0..4].try_into().unwrap());
    assert_eq!(marker, 0xFFFF_FFFF);

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
    let timestamps = [
        0i64,
        1_000_000_000i64,
        1_700_000_000i64,
        2_000_000_000i64,
        i32::MAX as i64,
        (i32::MAX as i64) + 1,
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
    let sizes = [
        0i64,
        1024i64,
        1_048_576i64,
        1_073_741_824i64,
        1_099_511_627_776i64,
        1_125_899_906_842_624i64,
        100_000_000_000_000_000i64,
    ];

    for size in sizes {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, size, 3).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, 3).expect("read succeeds");
        assert_eq!(decoded, size, "file size {size} failed");
    }
}

#[test]
fn read_varlong_truncated_input() {
    let data = [0x80u8];
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

#[test]
fn int_byte_extra_table_structure() {
    for (i, &val) in INT_BYTE_EXTRA[..32].iter().enumerate() {
        assert_eq!(val, 0, "index {i} should be 0");
    }
    for (i, &val) in INT_BYTE_EXTRA[32..48].iter().enumerate() {
        assert_eq!(val, 1, "index {} should be 1", i + 32);
    }
    for (i, &val) in INT_BYTE_EXTRA[48..56].iter().enumerate() {
        assert_eq!(val, 2, "index {} should be 2", i + 48);
    }
}

#[test]
fn decode_bytes_validates_int_byte_extra() {
    let (value, consumed) = decode_bytes(&[0x42]).expect("decode succeeds");
    assert_eq!(value, 0x42);
    assert_eq!(consumed, 1);

    let (value, consumed) = decode_bytes(&[0x80, 0x01]).expect("decode succeeds");
    assert_eq!(consumed, 2);
    assert_eq!(value & 0xFFFF, 1);
}

#[test]
fn invalid_data_error_message() {
    let err = invalid_data("test error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("test error"));
}

#[test]
fn encode_bytes_length_for_boundary_values() {
    let (len, _) = encode_bytes(0);
    assert_eq!(len, 1);
    let (len, _) = encode_bytes(127);
    assert_eq!(len, 1);

    let (len, _) = encode_bytes(128);
    assert_eq!(len, 2);
    let (len, _) = encode_bytes(255);
    assert_eq!(len, 2);

    let (len, _) = encode_bytes(65536);
    assert!(len >= 3);
}

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
    let data = [42u8, 0, 0];
    let mut cursor = Cursor::new(&data[..]);
    let err = read_int(&mut cursor).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn write_varint30_int_proto29_uses_fixed_int() {
    let mut output = Vec::new();
    write_varint30_int(&mut output, 42, 29).expect("write succeeds");
    assert_eq!(output.len(), 4);
    assert_eq!(output, vec![42, 0, 0, 0]);
}

#[test]
fn write_varint30_int_proto30_uses_varint() {
    let mut output = Vec::new();
    write_varint30_int(&mut output, 42, 30).expect("write succeeds");
    assert_eq!(output.len(), 1);
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
    for proto in [28u8, 29] {
        let mut buf = Vec::new();
        write_varint30_int(&mut buf, 1000, proto).expect("write succeeds");
        assert_eq!(buf.len(), 4, "proto {proto} should use 4-byte int");
    }

    for proto in [30u8, 31, 32] {
        let mut buf = Vec::new();
        write_varint30_int(&mut buf, 1000, proto).expect("write succeeds");
        assert!(buf.len() < 4, "proto {proto} should use varint (< 4 bytes)");
    }
}

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
    for min_bytes in 1u8..=8 {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, 0i64, min_bytes).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
        assert_eq!(decoded, 0i64, "zero failed for min_bytes={min_bytes}");
    }
}

#[test]
fn varlong_u32_max_plus_one_roundtrip() {
    let value = u32::MAX as i64 + 1;
    for min_bytes in [1u8, 2, 3, 4] {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
        assert_eq!(
            decoded, value,
            "u32::MAX+1 failed for min_bytes={min_bytes}"
        );
    }
}

#[test]
fn varlong_i64_max_min_bytes_3_through_8() {
    for min_bytes in 3u8..=8 {
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, i64::MAX, min_bytes).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
        assert_eq!(
            decoded,
            i64::MAX,
            "i64::MAX failed for min_bytes={min_bytes}"
        );
        assert_eq!(
            cursor.position() as usize,
            encoded.len(),
            "cursor did not consume all bytes for min_bytes={min_bytes}"
        );
    }
}

#[test]
fn varlong_64bit_boundary_values() {
    let boundary_values: &[i64] = &[
        0,
        1,
        127,
        128,
        255,
        256,
        u16::MAX as i64,
        u32::MAX as i64,
        u32::MAX as i64 + 1,
        i64::MAX,
    ];

    for &value in boundary_values {
        for min_bytes in [3u8, 4] {
            let mut encoded = Vec::new();
            write_varlong(&mut encoded, value, min_bytes).expect("write succeeds");
            let mut cursor = Cursor::new(&encoded);
            let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
            assert_eq!(
                decoded, value,
                "value={value:#x} min_bytes={min_bytes} failed"
            );
        }
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

#[test]
fn varint_u32_max_as_i32_roundtrip() {
    let value = u32::MAX as i32;
    assert_eq!(value, -1);
    let mut encoded = Vec::new();
    encode_varint_to_vec(value, &mut encoded);
    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, value);
}

#[test]
fn varlong_large_positive_value_roundtrip() {
    let value = i64::MAX / 2;
    let mut encoded = Vec::new();
    write_varlong(&mut encoded, value, 8).expect("write succeeds");
    let mut cursor = Cursor::new(&encoded);
    let decoded = read_varlong(&mut cursor, 8).expect("read succeeds");
    assert_eq!(decoded, value);
}

#[test]
fn decode_varint_overflow_tag_byte() {
    let data = [0xFCu8, 0, 0, 0, 0, 0];
    let err = decode_varint(&data).expect_err("overflow tag should fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("overflow"));

    let data = [0xFFu8, 0, 0, 0, 0, 0, 0];
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

#[test]
fn varint_encoding_length_boundaries() {
    let boundary_values = [
        (0, 1),
        (127, 1),
        (128, 2),
        (16383, 2),
        (16384, 3),
        (2097151, 3),
        (0x200000, 4),
        (0x10000000, 5),
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

#[test]
fn int_byte_extra_first_index() {
    assert_eq!(INT_BYTE_EXTRA[0], 0);
}

#[test]
fn int_byte_extra_last_index() {
    assert_eq!(INT_BYTE_EXTRA[63], 6);
}

#[test]
fn int_byte_extra_transition_points() {
    assert_eq!(INT_BYTE_EXTRA[31], 0);
    assert_eq!(INT_BYTE_EXTRA[32], 1);
    assert_eq!(INT_BYTE_EXTRA[47], 1);
    assert_eq!(INT_BYTE_EXTRA[48], 2);
    assert_eq!(INT_BYTE_EXTRA[55], 2);
    assert_eq!(INT_BYTE_EXTRA[56], 3);
    assert_eq!(INT_BYTE_EXTRA[59], 3);
    assert_eq!(INT_BYTE_EXTRA[60], 4);
    assert_eq!(INT_BYTE_EXTRA[61], 4);
    assert_eq!(INT_BYTE_EXTRA[62], 5);
    assert_eq!(INT_BYTE_EXTRA[63], 6);
}

#[test]
fn int_byte_extra_decode_with_each_extra_count() {
    let (val, consumed) = decode_bytes(&[0x42]).unwrap();
    assert_eq!(val, 0x42);
    assert_eq!(consumed, 1);

    let (val, consumed) = decode_bytes(&[0x80, 0x42]).unwrap();
    assert_eq!(consumed, 2);
    assert_eq!(val, 0x42);

    let (val, consumed) = decode_bytes(&[0xC0, 0x42, 0x00]).unwrap();
    assert_eq!(consumed, 3);
    assert_eq!(val, 0x42);

    let (val, consumed) = decode_bytes(&[0xE0, 0x42, 0x00, 0x00]).unwrap();
    assert_eq!(consumed, 4);
    assert_eq!(val, 0x42);

    let (val, consumed) = decode_bytes(&[0xF0, 0x42, 0x00, 0x00, 0x00]).unwrap();
    assert_eq!(consumed, 5);
    assert_eq!(val, 0x42);
}

#[test]
fn longint_boundary_at_0x7fffffff() {
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
    let negative = -1i64;
    let mut encoded = Vec::new();
    write_longint(&mut encoded, negative).expect("write succeeds");
    assert_eq!(encoded.len(), 12);
    let mut cursor = Cursor::new(&encoded);
    let decoded = read_longint(&mut cursor).expect("read succeeds");
    assert_eq!(decoded, negative);
}

#[test]
fn varlong_min_bytes_boundary_values() {
    for min_bytes in [1u8, 2, 3, 4, 5, 6, 7, 8] {
        let value = 0xFFi64;
        let mut encoded = Vec::new();
        write_varlong(&mut encoded, value, min_bytes).expect("write succeeds");
        let mut cursor = Cursor::new(&encoded);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
        assert_eq!(decoded, value, "failed for min_bytes={min_bytes}");
    }
}

#[test]
fn varlong_encodes_minimum_bytes() {
    let value = 0x1234i64;
    let mut encoded = Vec::new();
    write_varlong(&mut encoded, value, 3).expect("write succeeds");
    assert!(encoded.len() >= 3, "expected at least 3 bytes");
    let mut cursor = Cursor::new(&encoded);
    let decoded = read_varlong(&mut cursor, 3).expect("read succeeds");
    assert_eq!(decoded, value);
}

#[test]
fn decode_varint_truncated_2_byte() {
    let data = [0x80u8];
    let err = decode_varint(&data).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn decode_varint_truncated_3_byte() {
    let data = [0xC0u8, 0x00];
    let err = decode_varint(&data).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn decode_varint_truncated_4_byte() {
    let data = [0xE0u8, 0x00, 0x00];
    let err = decode_varint(&data).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn decode_varint_truncated_5_byte() {
    let data = [0xF0u8, 0x00, 0x00, 0x00];
    let err = decode_varint(&data).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_longint_truncated_marker() {
    let data = [0xFFu8, 0xFF];
    let mut cursor = Cursor::new(&data[..]);
    let err = read_longint(&mut cursor).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_longint_truncated_extended() {
    let data = [0xFFu8, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00];
    let mut cursor = Cursor::new(&data[..]);
    let err = read_longint(&mut cursor).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

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

#[test]
fn boundary_zero() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(0, &mut encoded);
    assert_eq!(encoded, vec![0x00]);

    let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 0);
    assert!(remainder.is_empty());
}

#[test]
fn boundary_127() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(127, &mut encoded);
    assert_eq!(encoded, vec![0x7F]);

    let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 127);
    assert!(remainder.is_empty());
}

#[test]
fn boundary_128_is_not_1byte() {
    let (len, _) = encode_bytes(128);
    assert!(len > 1, "value 128 should NOT encode to 1 byte");
}

#[test]
fn decode_raw_1byte_sequences() {
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

#[test]
fn high_bit_clear_indicates_1byte() {
    for byte in 0u8..=127 {
        let extra = INT_BYTE_EXTRA[(byte / 4) as usize];
        assert_eq!(
            extra, 0,
            "byte 0x{:02X} should have 0 extra bytes, got {}",
            byte, extra
        );
    }
}

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

#[test]
fn boundary_128_encodes_to_2_bytes() {
    let (len, _) = encode_bytes(128);
    assert_eq!(len, 2, "value 128 should encode to 2 bytes");

    let mut encoded = Vec::new();
    encode_varint_to_vec(128, &mut encoded);
    assert_eq!(encoded.len(), 2);

    let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 128);
    assert!(remainder.is_empty());
}

#[test]
fn value_255_encodes_to_2_bytes() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(255, &mut encoded);
    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded, vec![0x80, 0xFF]);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 255);
}

#[test]
fn value_256_encodes_to_2_bytes() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(256, &mut encoded);
    assert_eq!(encoded.len(), 2);
    assert_eq!(encoded, vec![0x81, 0x00]);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 256);
}

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

#[test]
fn boundary_between_2byte_and_3byte() {
    let (len_16383, _) = encode_bytes(16383);
    let (len_16384, _) = encode_bytes(16384);

    assert!(
        len_16384 > len_16383,
        "16384 ({} bytes) should require more bytes than 16383 ({} bytes)",
        len_16384,
        len_16383
    );
}

#[test]
fn decode_2byte_sequences() {
    for first_byte in 0x80u8..=0xBF {
        let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
        assert_eq!(
            extra, 1,
            "byte 0x{:02X} should have 1 extra byte, got {}",
            first_byte, extra
        );
    }
}

#[test]
fn truncated_2byte_input_fails() {
    let data = [0x80u8];
    let err = decode_varint(&data).expect_err("truncated input must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

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

#[test]
fn encoding_matches_upstream_format() {
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

#[test]
fn boundary_16384_is_3_bytes() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(16384, &mut encoded);
    assert_eq!(encoded.len(), 3);
    assert_eq!(encoded, vec![0xC0, 0x00, 0x40]);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 16384);
}

#[test]
fn roundtrip_3byte_values() {
    let values = [16384, 20000, 50000, 100000, 500000, 1000000, 2097151];
    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
        assert!(remainder.is_empty());
    }
}

#[test]
fn roundtrip_4byte_values() {
    let values = [0x20_0000, 0x100_0000, 0x1000_0000];
    for value in values {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);

        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        assert_eq!(decoded, value, "round-trip failed for value {}", value);
        assert!(remainder.is_empty());
    }
}

#[test]
fn roundtrip_5byte_values() {
    let values = [0x1000_0000_i32, 0x4000_0000_i32, i32::MAX];
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

#[test]
fn known_5byte_encoding() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(1_073_741_824, &mut encoded);
    assert_eq!(encoded, vec![0xF0, 0x00, 0x00, 0x00, 0x40]);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, 1_073_741_824);
}

#[test]
fn negative_values_require_5_bytes() {
    let negatives = [-1, -128, -129, -32768, i32::MIN];
    for value in negatives {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
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

#[test]
fn boundary_i32_max() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(i32::MAX, &mut encoded);
    assert_eq!(encoded.len(), 5);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, i32::MAX);
}

#[test]
fn boundary_i32_min() {
    let mut encoded = Vec::new();
    encode_varint_to_vec(i32::MIN, &mut encoded);
    assert_eq!(encoded.len(), 5);

    let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
    assert_eq!(decoded, i32::MIN);
}

#[test]
fn int_byte_extra_for_extended_encodings() {
    for first_byte in 0xC0u8..=0xDF {
        let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
        assert_eq!(
            extra, 2,
            "byte 0x{:02X} should have 2 extra bytes",
            first_byte
        );
    }

    for first_byte in 0xE0u8..=0xEF {
        let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
        assert_eq!(
            extra, 3,
            "byte 0x{:02X} should have 3 extra bytes",
            first_byte
        );
    }

    for first_byte in 0xF0u8..=0xF7 {
        let extra = INT_BYTE_EXTRA[(first_byte / 4) as usize];
        assert_eq!(
            extra, 4,
            "byte 0x{:02X} should have 4 extra bytes",
            first_byte
        );
    }
}

#[test]
fn truncated_extended_encodings_fail() {
    let err = decode_varint(&[0xC0, 0x00]).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    let err = decode_varint(&[0xE0, 0x00, 0x00]).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

    let err = decode_varint(&[0xF0, 0x00, 0x00, 0x00]).expect_err("truncated must fail");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn overflow_tag_bytes_are_rejected() {
    let err = decode_varint(&[0xF8, 0, 0, 0, 0, 0]).expect_err("overflow must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("overflow"));

    let err = decode_varint(&[0xFC, 0, 0, 0, 0, 0, 0]).expect_err("overflow must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

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

proptest! {
    #[test]
    fn varint_roundtrip_all_i32(value: i32) {
        let mut encoded = Vec::new();
        encode_varint_to_vec(value, &mut encoded);
        let (decoded, remainder) = decode_varint(&encoded).expect("decode succeeds");
        prop_assert_eq!(decoded, value);
        prop_assert!(remainder.is_empty());
    }

    #[test]
    fn varint_stream_roundtrip(value: i32) {
        let mut buf = Vec::new();
        write_varint(&mut buf, value).expect("write succeeds");
        let mut cursor = Cursor::new(&buf);
        let decoded = read_varint(&mut cursor).expect("read succeeds");
        prop_assert_eq!(decoded, value);
    }

    #[test]
    fn fixed_int_roundtrip(value: i32) {
        let mut buf = Vec::new();
        write_int(&mut buf, value).expect("write succeeds");
        prop_assert_eq!(buf.len(), 4);
        let mut cursor = Cursor::new(&buf);
        let decoded = read_int(&mut cursor).expect("read succeeds");
        prop_assert_eq!(decoded, value);
    }

    #[test]
    fn varlong_roundtrip_full_range(value in 0i64..=i64::MAX) {
        let mut buf = Vec::new();
        write_varlong(&mut buf, value, 8).expect("write succeeds");
        let mut cursor = Cursor::new(&buf);
        let decoded = read_varlong(&mut cursor, 8).expect("read succeeds");
        prop_assert_eq!(decoded, value);
    }

    #[test]
    fn varlong_roundtrip_small_values(value in 0i64..=0xFF_FFFF, min_bytes in 1u8..=8) {
        let mut buf = Vec::new();
        write_varlong(&mut buf, value, min_bytes).expect("write succeeds");
        let mut cursor = Cursor::new(&buf);
        let decoded = read_varlong(&mut cursor, min_bytes).expect("read succeeds");
        prop_assert_eq!(decoded, value);
    }

    #[test]
    fn varint30_int_roundtrip_proptest(value: i32, proto in 28u8..=32) {
        let mut buf = Vec::new();
        write_varint30_int(&mut buf, value, proto).expect("write succeeds");
        let mut cursor = Cursor::new(&buf);
        let decoded = read_varint30_int(&mut cursor, proto).expect("read succeeds");
        prop_assert_eq!(decoded, value);
    }

    #[test]
    fn longint_roundtrip_32bit(value: i32) {
        let value64 = i64::from(value);
        let mut buf = Vec::new();
        write_longint(&mut buf, value64).expect("write succeeds");
        let mut cursor = Cursor::new(&buf);
        let decoded = read_longint(&mut cursor).expect("read succeeds");
        prop_assert_eq!(decoded, value64);
    }

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

    #[test]
    fn varint_max_encoding_length(value: i32) {
        let (len, _) = encode_bytes(value);
        prop_assert!(len <= 5, "varint encoding should use at most 5 bytes, got {}", len);
    }
}

/// The exact boundary values where encoding size changes.
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

#[test]
fn boundary_7bit_127_to_128() {
    let (len_127, bytes_127) = encode_bytes(127);
    assert_eq!(len_127, 1, "127 should be 1 byte");
    assert_eq!(bytes_127[0], 0x7F, "127 should encode as 0x7F");

    let (len_128, _) = encode_bytes(128);
    assert_eq!(len_128, 2, "128 should be 2 bytes");

    assert_eq!(
        len_128,
        len_127 + 1,
        "128 should need exactly one more byte than 127"
    );
}

#[test]
fn boundary_7bit_adjacent_values() {
    let (len_126, _) = encode_bytes(126);
    let (len_127, _) = encode_bytes(127);
    let (len_128, _) = encode_bytes(128);
    let (len_129, _) = encode_bytes(129);

    assert_eq!(len_126, 1, "126 should be 1 byte");
    assert_eq!(len_127, 1, "127 should be 1 byte");
    assert_eq!(len_128, 2, "128 should be 2 bytes");
    assert_eq!(len_129, 2, "129 should be 2 bytes");
}

#[test]
fn boundary_14bit_16383_to_16384() {
    let (len_16383, _) = encode_bytes(16383);
    assert_eq!(len_16383, 2, "16383 should be 2 bytes");

    let (len_16384, _) = encode_bytes(16384);
    assert_eq!(len_16384, 3, "16384 should be 3 bytes");

    assert_eq!(
        len_16384,
        len_16383 + 1,
        "16384 should need exactly one more byte than 16383"
    );
}

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

#[test]
fn boundary_21bit_2097151_to_2097152() {
    let (len_2097151, _) = encode_bytes(2097151);
    assert_eq!(len_2097151, 3, "2097151 should be 3 bytes");

    let (len_2097152, _) = encode_bytes(2097152);
    assert_eq!(len_2097152, 4, "2097152 should be 4 bytes");

    assert_eq!(
        len_2097152,
        len_2097151 + 1,
        "2097152 should need exactly one more byte than 2097151"
    );
}

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

#[test]
fn boundary_28bit_268435455_to_268435456() {
    let (len_max4, _) = encode_bytes(268435455);
    assert_eq!(len_max4, 4, "268435455 should be 4 bytes");

    let (len_min5, _) = encode_bytes(268435456);
    assert_eq!(len_min5, 5, "268435456 should be 5 bytes");

    assert_eq!(
        len_min5,
        len_max4 + 1,
        "268435456 should need exactly one more byte than 268435455"
    );
}

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

#[test]
fn all_byte_boundary_transitions() {
    let transitions = [
        (127_i32, 128_i32, 1, 2),
        (16383, 16384, 2, 3),
        (2097151, 2097152, 3, 4),
        (268435455, 268435456, 4, 5),
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

        for value in [max_n, min_n_plus_1] {
            let mut encoded = Vec::new();
            encode_varint_to_vec(value, &mut encoded);
            let (decoded, _) = decode_varint(&encoded).expect("decode succeeds");
            assert_eq!(decoded, value, "round-trip failed for {}", value);
        }
    }
}

#[test]
fn wire_format_7bit_boundary() {
    let mut enc_127 = Vec::new();
    encode_varint_to_vec(127, &mut enc_127);
    assert_eq!(enc_127, vec![0x7F], "127 should encode as [0x7F]");

    let mut enc_128 = Vec::new();
    encode_varint_to_vec(128, &mut enc_128);
    assert_eq!(enc_128.len(), 2, "128 should encode as 2 bytes");
    assert!(
        enc_128[0] & 0x80 != 0,
        "128's first byte should have high bit set"
    );
}

#[test]
fn wire_format_14bit_boundary() {
    let mut enc_16383 = Vec::new();
    encode_varint_to_vec(16383, &mut enc_16383);
    assert_eq!(enc_16383.len(), 2, "16383 should encode as 2 bytes");

    let mut enc_16384 = Vec::new();
    encode_varint_to_vec(16384, &mut enc_16384);
    assert_eq!(enc_16384.len(), 3, "16384 should encode as 3 bytes");
    assert!(
        enc_16384[0] & 0xC0 == 0xC0,
        "16384's first byte should have 110x_xxxx pattern"
    );
}

#[test]
fn stream_multiple_boundary_values() {
    let values: Vec<i32> = BYTE_BOUNDARIES.iter().map(|(v, _, _)| *v).collect();
    let expected_lengths: Vec<usize> = BYTE_BOUNDARIES.iter().map(|(_, l, _)| *l).collect();

    let mut buf = Vec::new();
    for &value in &values {
        write_varint(&mut buf, value).expect("write succeeds");
    }

    let expected_total: usize = expected_lengths.iter().sum();
    assert_eq!(
        buf.len(),
        expected_total,
        "total encoded length should be {}",
        expected_total
    );

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
