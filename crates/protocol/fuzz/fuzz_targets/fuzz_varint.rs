#![no_main]

//! Fuzz target for varint parsing functions.
//!
//! Tests the variable-length integer decoding that rsync uses for
//! protocol efficiency. These functions must handle arbitrary byte
//! sequences without panicking or causing undefined behavior.
//!
//! In addition to the unstructured byte-stream coverage, the target
//! injects structured edge cases on every iteration:
//!   - boundary values (i32::MIN/MAX, i64::MIN/MAX, 0, -1) encoded
//!     with `encode_varint_to_vec` / `write_varlong`,
//!   - truncated buffers (every prefix of a freshly encoded value),
//!   - both legacy protocol 31 (4-byte LE via `read_int`) and modern
//!     protocol 32 (varint via `read_varint`) decode paths through
//!     `read_varint30_int`.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// Boundary values exercised on every fuzz iteration to guarantee the
/// decoders survive extreme inputs even when the corpus has not yet
/// reached them.
const I32_EDGES: [i32; 8] = [
    0,
    -1,
    1,
    i32::MIN,
    i32::MAX,
    i32::MIN + 1,
    i32::MAX - 1,
    -0x4000_0000,
];

/// Boundary i64 values for the varlong path.
const I64_EDGES: [i64; 8] = [
    0,
    -1,
    1,
    i64::MIN,
    i64::MAX,
    i32::MAX as i64 + 1,
    -(i32::MIN as i64) - 1,
    0x0000_FFFF_FFFF_FFFF,
];

fuzz_target!(|data: &[u8]| {
    // Unstructured: arbitrary bytes through every decoder.
    {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varint(&mut cursor);
    }
    for min_bytes in [0u8, 1, 2, 3, 4, 5, 6, 7, 8] {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varlong(&mut cursor, min_bytes);
    }
    for min_bytes in [0u8, 1, 2, 3, 4] {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varlong30(&mut cursor, min_bytes);
    }
    let _ = protocol::decode_varint(data);

    // Legacy 4-byte LE (protocol < 30) and modern varint (protocol >= 30)
    // dispatch through the same surface; cover both branches every run.
    for proto in [28u8, 29, 30, 31, 32] {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varint30_int(&mut cursor, proto);
    }

    // Legacy longint path (protocol < 30).
    {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_longint(&mut cursor);
    }

    // Structured edges: encode boundary values, then decode the full
    // buffer and every truncated prefix to drive mid-byte EOF paths.
    for value in I32_EDGES {
        let mut buf = Vec::new();
        protocol::encode_varint_to_vec(value, &mut buf);

        // Full buffer must roundtrip when the encoder succeeded.
        if let Ok((decoded, rest)) = protocol::decode_varint(&buf) {
            assert_eq!(decoded, value, "boundary varint decode mismatch");
            assert!(rest.is_empty(), "boundary varint left trailing bytes");
        }

        // Every strict prefix must error cleanly (never panic).
        for cut in 0..buf.len() {
            let truncated = &buf[..cut];
            let _ = protocol::decode_varint(truncated);
            let mut cursor = Cursor::new(truncated);
            let _ = protocol::read_varint(&mut cursor);
        }

        // Protocol 31 (legacy 4-byte LE) and 32 (varint) dispatch.
        let mut legacy = Vec::new();
        if protocol::write_int(&mut legacy, value).is_ok() {
            let mut cursor = Cursor::new(&legacy);
            if let Ok(decoded) = protocol::read_varint30_int(&mut cursor, 28) {
                assert_eq!(decoded, value, "proto<30 read_int roundtrip mismatch");
            }
            // Truncated 4-byte LE buffer must not panic.
            for cut in 0..legacy.len() {
                let mut cursor = Cursor::new(&legacy[..cut]);
                let _ = protocol::read_varint30_int(&mut cursor, 28);
            }
        }

        let mut modern = Vec::new();
        if protocol::write_varint(&mut modern, value).is_ok() {
            let mut cursor = Cursor::new(&modern);
            if let Ok(decoded) = protocol::read_varint30_int(&mut cursor, 32) {
                assert_eq!(decoded, value, "proto>=30 read_varint roundtrip mismatch");
            }
            for cut in 0..modern.len() {
                let mut cursor = Cursor::new(&modern[..cut]);
                let _ = protocol::read_varint30_int(&mut cursor, 32);
            }
        }
    }

    // Structured edges for the 64-bit varlong path with every valid
    // min_bytes width. Truncated prefixes exercise mid-byte EOF in the
    // initial read as well as the extra-bytes read.
    for value in I64_EDGES {
        for min_bytes in 1u8..=8 {
            let mut buf = Vec::new();
            if protocol::write_varlong(&mut buf, value, min_bytes).is_ok() {
                let mut cursor = Cursor::new(&buf);
                if let Ok(decoded) = protocol::read_varlong(&mut cursor, min_bytes) {
                    assert_eq!(decoded, value, "varlong boundary roundtrip mismatch");
                }
                for cut in 0..buf.len() {
                    let mut cursor = Cursor::new(&buf[..cut]);
                    let _ = protocol::read_varlong(&mut cursor, min_bytes);
                }
            }
        }

        // Legacy longint (protocol < 30) encodes >32-bit values with a
        // 0xFFFFFFFF prefix; exercise both the short and long forms.
        let mut buf = Vec::new();
        if protocol::write_longint(&mut buf, value).is_ok() {
            let mut cursor = Cursor::new(&buf);
            if let Ok(decoded) = protocol::read_longint(&mut cursor) {
                assert_eq!(decoded, value, "longint boundary roundtrip mismatch");
            }
            for cut in 0..buf.len() {
                let mut cursor = Cursor::new(&buf[..cut]);
                let _ = protocol::read_longint(&mut cursor);
            }
        }
    }
});
