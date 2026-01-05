#![no_main]

//! Fuzz target for multiplex frame parsing.
//!
//! Tests the multiplexed I/O frame decoding used for rsync's
//! bidirectional communication. This is a critical attack surface
//! as it processes untrusted network data.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Test BorrowedMessageFrame::decode_from_slice (re-exported at crate root)
    let _ = protocol::BorrowedMessageFrame::decode_from_slice(data);

    // Test MessageHeader::decode (re-exported at crate root)
    let _ = protocol::MessageHeader::decode(data);

    // Test recv_msg (reader-based)
    let mut cursor = Cursor::new(data);
    let _ = protocol::recv_msg(&mut cursor);

    // Test recv_msg_into (reader-based with buffer)
    let mut cursor = Cursor::new(data);
    let mut buffer = Vec::new();
    let _ = protocol::recv_msg_into(&mut cursor, &mut buffer);
});
