#![no_main]

//! Fuzz target for varint parsing functions.
//!
//! Tests the variable-length integer decoding that rsync uses for
//! protocol efficiency. These functions must handle arbitrary byte
//! sequences without panicking or causing undefined behavior.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Test read_varint
    let mut cursor = Cursor::new(data);
    let _ = protocol::read_varint(&mut cursor);

    // Test read_varlong with various min_bytes values
    for min_bytes in [0u8, 1, 2, 3, 4, 5, 6, 7, 8] {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varlong(&mut cursor, min_bytes);
    }

    // Test read_varlong30 with various min_bytes values
    for min_bytes in [0u8, 1, 2, 3, 4] {
        let mut cursor = Cursor::new(data);
        let _ = protocol::read_varlong30(&mut cursor, min_bytes);
    }

    // Test decode_varint (slice-based)
    let _ = protocol::decode_varint(data);
});
