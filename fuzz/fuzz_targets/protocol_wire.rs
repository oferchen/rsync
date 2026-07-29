#![no_main]

//! Top-level fuzz target for the rsync wire protocol parser.
//!
//! Feeds arbitrary bytes from libFuzzer into the highest-level multiplex
//! frame decoder. The decoder accepts untrusted bytes from network peers,
//! so any panic discovered here represents a potential remote attack
//! surface. Coverage-guided exploration takes care of fanning the input
//! out across the underlying header, payload-length, and message-code
//! validation paths.
//!
//! # Oracle
//!
//! Beyond panic-freedom this target enforces a byte-exact round-trip on
//! every decoded frame. The multiplex header is a canonical 4-byte
//! little-endian encoding of `(code, payload_len)` with no reserved or
//! ignored bits, so re-encoding a decoded frame MUST reproduce the exact
//! bytes it was decoded from. Walking the buffer frame by frame lets the
//! oracle compare the re-encoded header + payload against the precise slice
//! the decoder consumed, catching any header field the parser silently
//! drops, mis-widths, or mis-orders.
//!
//! Additional targets (varint, file list, delta, filter rules, ...)
//! belong in sibling files under `fuzz/fuzz_targets/`.

use libfuzzer_sys::fuzz_target;

use protocol::{BorrowedMessageFrame, MessageHeader};

fuzz_target!(|data: &[u8]| {
    // Walk every frame in the buffer so libFuzzer explores both the
    // single-frame and multi-frame parser states. Errors are expected on
    // malformed input - only panics and round-trip mismatches are findings.
    let mut rest = data;
    while !rest.is_empty() {
        let before = rest;
        let (frame, remainder) = match BorrowedMessageFrame::decode_from_slice(before) {
            Ok(parsed) => parsed,
            Err(_) => break,
        };

        // The exact bytes the decoder consumed for this frame.
        let consumed_len = before.len() - remainder.len();
        let consumed = &before[..consumed_len];

        // Re-encode the decoded frame from its observable parts. The header
        // length is bounded by the just-parsed payload, so `MessageHeader`
        // construction cannot fail here.
        let payload_len = u32::try_from(frame.payload_len())
            .expect("payload length came from a parsed frame and fits in u32");
        let header = MessageHeader::new(frame.code(), payload_len)
            .expect("re-encoding a decoded header must succeed");
        let mut reencoded = Vec::with_capacity(consumed_len);
        reencoded.extend_from_slice(&header.encode());
        reencoded.extend_from_slice(frame.payload());

        assert_eq!(
            consumed,
            reencoded,
            "multiplex frame did not round-trip: code={:?} payload_len={}",
            frame.code(),
            frame.payload_len(),
        );

        rest = remainder;
    }
});
