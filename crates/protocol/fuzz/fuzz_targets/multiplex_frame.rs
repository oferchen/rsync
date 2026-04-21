#![no_main]

//! Fuzz target for multiplex frame encode/decode roundtrip.
//!
//! Tests `MessageFrame` and `BorrowedMessageFrame` encode/decode roundtrip
//! with structured input. Also exercises `MessageHeader` parsing and
//! `recv_msg`/`send_msg` with arbitrary byte streams to catch panics
//! and logic errors in the multiplexing layer.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// Structured input for multiplex frame roundtrip testing.
#[derive(Arbitrary, Debug)]
struct MplexInput {
    /// Message code selector (mapped to valid codes).
    code_selector: u8,
    /// Payload bytes (capped at 64 KB for practical fuzzing).
    payload: Vec<u8>,
    /// Raw bytes for unstructured parsing tests.
    raw_bytes: Vec<u8>,
}

impl MplexInput {
    /// Maps code_selector to a valid `MessageCode`.
    fn message_code(&self) -> protocol::MessageCode {
        // Map to the most common message codes to get good coverage
        match self.code_selector % 10 {
            0 => protocol::MessageCode::Data,
            1 => protocol::MessageCode::ErrorXfer,
            2 => protocol::MessageCode::Info,
            3 => protocol::MessageCode::Error,
            4 => protocol::MessageCode::Warning,
            5 => protocol::MessageCode::ErrorSocket,
            6 => protocol::MessageCode::Log,
            7 => protocol::MessageCode::Redo,
            8 => protocol::MessageCode::Success,
            _ => protocol::MessageCode::Deleted,
        }
    }
}

fuzz_target!(|input: MplexInput| {
    let code = input.message_code();

    // Cap payload to the 24-bit limit (16 MB) for valid frames,
    // but also test oversized payloads for error handling
    let payload = if input.payload.len() > 0xFF_FFFF {
        &input.payload[..0xFF_FFFF]
    } else {
        &input.payload
    };

    // Roundtrip: MessageFrame encode/decode via Vec
    if let Ok(frame) = protocol::MessageFrame::new(code, payload.to_vec()) {
        let mut encoded = Vec::new();
        if frame.encode_into_vec(&mut encoded).is_ok() {
            if let Ok((decoded, remainder)) = protocol::MessageFrame::decode_from_slice(&encoded) {
                assert!(
                    remainder.is_empty(),
                    "unexpected trailing bytes after roundtrip"
                );
                assert_eq!(decoded.code(), code, "message code mismatch");
                assert_eq!(decoded.payload(), payload, "payload mismatch");
            }
        }
    }

    // Roundtrip: MessageFrame encode/decode via writer
    if let Ok(frame) = protocol::MessageFrame::new(code, payload.to_vec()) {
        let mut encoded = Vec::new();
        if frame.encode_into_writer(&mut encoded).is_ok() {
            let mut cursor = Cursor::new(&encoded);
            if let Ok(decoded) = protocol::recv_msg(&mut cursor) {
                assert_eq!(decoded.code(), code, "writer roundtrip code mismatch");
                assert_eq!(
                    decoded.payload(),
                    payload,
                    "writer roundtrip payload mismatch"
                );
            }
        }
    }

    // Roundtrip: BorrowedMessageFrame decode from encoded frame
    if let Ok(frame) = protocol::MessageFrame::new(code, payload.to_vec()) {
        let mut encoded = Vec::new();
        if frame.encode_into_vec(&mut encoded).is_ok() {
            if let Ok((borrowed, remainder)) =
                protocol::BorrowedMessageFrame::decode_from_slice(&encoded)
            {
                assert!(remainder.is_empty(), "borrowed: trailing bytes");
                assert_eq!(borrowed.code(), code, "borrowed: code mismatch");
                assert_eq!(borrowed.payload(), payload, "borrowed: payload mismatch");
            }
        }
    }

    // Test oversized payload rejection
    if input.payload.len() > 0xFF_FFFF {
        let result = protocol::MessageFrame::new(code, input.payload.clone());
        assert!(result.is_err(), "oversized payload should be rejected");
    }

    // Unstructured: parse arbitrary bytes as MessageHeader
    let _ = protocol::MessageHeader::decode(&input.raw_bytes);

    // Unstructured: parse arbitrary bytes as BorrowedMessageFrame
    let _ = protocol::BorrowedMessageFrame::decode_from_slice(&input.raw_bytes);

    // Unstructured: parse arbitrary bytes via recv_msg
    let mut cursor = Cursor::new(&input.raw_bytes);
    let _ = protocol::recv_msg(&mut cursor);

    // Unstructured: parse arbitrary bytes via recv_msg_into
    let mut cursor = Cursor::new(&input.raw_bytes);
    let mut buffer = Vec::new();
    let _ = protocol::recv_msg_into(&mut cursor, &mut buffer);

    // Unstructured: TryFrom<&[u8]> for MessageFrame
    let _ = <protocol::MessageFrame as std::convert::TryFrom<&[u8]>>::try_from(&input.raw_bytes);
});
