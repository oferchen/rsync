#![no_main]

//! Fuzz target for delta wire protocol parsing.
//!
//! Tests the delta token and operation decoding used during file
//! transfers. Malformed delta streams must be rejected gracefully
//! without panics or memory safety issues.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Test read_token (single token)
    let mut cursor = Cursor::new(data);
    let _ = protocol::wire::read_token(&mut cursor);

    // Test read_delta (full delta stream)
    let mut cursor = Cursor::new(data);
    let _ = protocol::wire::read_delta(&mut cursor);

    // Test read_int (used by delta operations)
    let mut cursor = Cursor::new(data);
    let _ = protocol::wire::read_int(&mut cursor);

    // Test read_signature
    let mut cursor = Cursor::new(data);
    let _ = protocol::wire::read_signature(&mut cursor);
});
