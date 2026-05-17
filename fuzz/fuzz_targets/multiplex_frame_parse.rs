#![no_main]

//! Fuzz target for the multiplex `MSG_*` frame header parser.
//!
//! The 4-byte little-endian envelope header is the very first thing every
//! peer parses on a multiplexed stream. A panic here is a remote attack
//! surface, so we fuzz it directly via [`MessageHeader::decode`] / `from_raw`
//! and via the higher-level [`BorrowedMessageFrames`] walker that wraps the
//! same parser plus payload-length validation.
//!
//! Two input modes are alternated based on the first byte of the fuzzer
//! input so coverage-guided search explores both "raw header bytes" and
//! "structured (code, payload_len) pairs that may or may not be valid". The
//! structured path lets libFuzzer find divergences between `from_raw` and
//! `decode` that a purely byte-oriented harness would only stumble onto by
//! chance.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run multiplex_frame_parse
//! ```

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use protocol::{BorrowedMessageFrames, MESSAGE_HEADER_LEN, MessageHeader};

/// Structured input mirroring the on-wire shape: a 32-bit raw header plus
/// trailing payload bytes. The fuzzer explores oversized lengths, invalid
/// tag bytes (below `MPLEX_BASE`), and unknown message codes through the
/// derived [`Arbitrary`] implementation.
#[derive(Arbitrary, Debug)]
struct StructuredHeader {
    raw: u32,
    trailing: Vec<u8>,
}

fuzz_target!(|data: &[u8]| {
    parse_raw_bytes(data);
    parse_structured(data);
});

/// Feed the raw byte stream into both the single-shot [`MessageHeader`]
/// decoder and the frame walker. Both refuse to panic on any input.
fn parse_raw_bytes(data: &[u8]) {
    let _ = MessageHeader::decode(data);

    if data.len() >= MESSAGE_HEADER_LEN {
        let mut buf = [0u8; MESSAGE_HEADER_LEN];
        buf.copy_from_slice(&data[..MESSAGE_HEADER_LEN]);
        let raw = u32::from_le_bytes(buf);
        let _ = MessageHeader::from_raw(raw);
    }

    for frame in BorrowedMessageFrames::new(data) {
        match frame {
            Ok(frame) => {
                let _ = frame.code();
                let _ = frame.payload_len();
                let _ = frame.payload();
            }
            Err(_) => break,
        }
    }
}

/// Build a structured (raw header, trailing payload) pair via [`Arbitrary`]
/// and exercise both decode entry points so the fuzzer can target the
/// validation matrix (oversized payloads, invalid tags, unknown codes).
fn parse_structured(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(input) = StructuredHeader::arbitrary(&mut u) else {
        return;
    };

    let encoded = input.raw.to_le_bytes();
    let decoded_bytes = MessageHeader::decode(&encoded);
    let decoded_raw = MessageHeader::from_raw(input.raw);

    // Both entry points must agree byte-for-byte on success and failure.
    match (decoded_bytes, decoded_raw) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.code(), b.code());
            assert_eq!(a.payload_len(), b.payload_len());
            assert_eq!(a.encode_raw(), b.encode_raw());
        }
        (Err(_), Err(_)) => {}
        (a, b) => panic!("decode/from_raw disagree: bytes={a:?} raw={b:?}"),
    }

    // Compose a synthetic frame and run it through the streaming walker.
    let mut wire = Vec::with_capacity(MESSAGE_HEADER_LEN + input.trailing.len());
    wire.extend_from_slice(&encoded);
    wire.extend_from_slice(&input.trailing);
    for frame in BorrowedMessageFrames::new(&wire) {
        match frame {
            Ok(frame) => {
                let _ = frame.code();
                let _ = frame.payload_len();
                let _ = frame.payload();
            }
            Err(_) => break,
        }
    }
}
