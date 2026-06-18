#![no_main]

//! Differential fuzz target for the multiplex `MSG_*` frame layer.
//!
//! Unlike the existing `multiplex_frame_parse` target (which checks that
//! arbitrary bytes do not cause panics), this target verifies *structural
//! consistency* across every public entry point in the multiplex layer.
//! Divergences between owned and borrowed decoders, encode/decode
//! round-trip mismatches, and header-vs-frame inconsistencies are all
//! findings.
//!
//! # Invariants checked
//!
//! 1. `MessageHeader::decode` and `MessageHeader::from_raw` agree on
//!    every 4-byte input: both succeed with identical fields, or both
//!    reject.
//!
//! 2. For valid headers, `encode_raw` round-trips: `from_raw(h.encode_raw())
//!    == h`.
//!
//! 3. `MessageFrame::encode_into_vec` followed by `decode_from_slice`
//!    reproduces the original code and payload exactly.
//!
//! 4. `MessageFrame::encode_into_writer` followed by `recv_msg` agrees
//!    with the vec round-trip path.
//!
//! 5. `BorrowedMessageFrame::decode_from_slice` agrees with
//!    `MessageFrame::decode_from_slice` on every input: both succeed
//!    with identical code, payload, and remainder length, or both fail.
//!
//! 6. The `BorrowedMessageFrames` iterator produces the same sequence
//!    of (code, payload) pairs as repeated `decode_from_slice` calls.
//!
//! 7. `MessageFrame::header()` agrees with the header decoded from the
//!    encoded bytes.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run differential_multiplex -- -max_total_time=120
//! ```

use std::io::Cursor;

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use protocol::{
    recv_msg, BorrowedMessageFrame, BorrowedMessageFrames, MessageCode, MessageFrame,
    MessageHeader, MESSAGE_HEADER_LEN,
};

/// Structured input for differential testing of the multiplex layer.
#[derive(Arbitrary, Debug)]
struct DiffInput {
    /// Code selector mapped to a valid `MessageCode`.
    code_selector: u8,
    /// Payload bytes (fuzzer-controlled content and length).
    payload: Vec<u8>,
    /// Raw 4-byte header value for header-level differential tests.
    raw_header: u32,
    /// Concatenated wire bytes for multi-frame iterator testing.
    multi_frame_bytes: Vec<u8>,
}

/// Maps a selector byte to a `MessageCode` using the full set of codes.
fn select_code(selector: u8) -> MessageCode {
    let all = MessageCode::ALL;
    all[selector as usize % all.len()]
}

/// Cap payload to the 24-bit maximum to avoid valid-frame construction failure.
const MAX_PAYLOAD: usize = 0x00FF_FFFF;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(input) = DiffInput::arbitrary(&mut u) else {
        // Fall back to raw-byte differential checks when structured
        // generation fails.
        check_raw_bytes(data);
        return;
    };

    check_header_differential(input.raw_header);
    check_frame_roundtrip(&input);
    check_owned_vs_borrowed(&input);
    check_iterator_vs_manual(&input.multi_frame_bytes);
    check_raw_bytes(data);
});

/// Invariant 1-2: `decode` vs `from_raw` agreement and encode round-trip.
fn check_header_differential(raw: u32) {
    let bytes = raw.to_le_bytes();
    let from_bytes = MessageHeader::decode(&bytes);
    let from_raw = MessageHeader::from_raw(raw);

    match (&from_bytes, &from_raw) {
        (Ok(a), Ok(b)) => {
            assert_eq!(
                a.code(),
                b.code(),
                "decode/from_raw code divergence: raw=0x{raw:08x}"
            );
            assert_eq!(
                a.payload_len(),
                b.payload_len(),
                "decode/from_raw payload_len divergence: raw=0x{raw:08x}"
            );

            // Invariant 2: encode_raw round-trip.
            let re_encoded = a.encode_raw();
            let re_decoded =
                MessageHeader::from_raw(re_encoded).expect("encode_raw produced invalid header");
            assert_eq!(
                a.code(),
                re_decoded.code(),
                "encode_raw roundtrip code mismatch"
            );
            assert_eq!(
                a.payload_len(),
                re_decoded.payload_len(),
                "encode_raw roundtrip payload_len mismatch"
            );

            // Also check the byte-level encode path.
            let encoded_bytes = a.encode();
            let re_decoded_bytes =
                MessageHeader::decode(&encoded_bytes).expect("encode produced invalid bytes");
            assert_eq!(a.code(), re_decoded_bytes.code());
            assert_eq!(a.payload_len(), re_decoded_bytes.payload_len());
        }
        (Err(_), Err(_)) => {
            // Both reject - consistent.
        }
        (a, b) => {
            panic!("decode/from_raw disagree on raw=0x{raw:08x}: bytes={a:?} raw={b:?}");
        }
    }
}

/// Invariants 3-4: frame encode/decode round-trip via vec and writer paths.
fn check_frame_roundtrip(input: &DiffInput) {
    let code = select_code(input.code_selector);
    let payload = if input.payload.len() > MAX_PAYLOAD {
        &input.payload[..MAX_PAYLOAD]
    } else {
        &input.payload[..]
    };

    let frame = match MessageFrame::new(code, payload.to_vec()) {
        Ok(f) => f,
        Err(_) => return,
    };

    // Invariant 3: encode_into_vec + decode_from_slice round-trip.
    let mut vec_encoded = Vec::new();
    if frame.encode_into_vec(&mut vec_encoded).is_err() {
        return;
    }
    let (vec_decoded, vec_remainder) = MessageFrame::decode_from_slice(&vec_encoded)
        .expect("decode_from_slice failed on encode_into_vec output");
    assert!(
        vec_remainder.is_empty(),
        "trailing bytes after vec roundtrip"
    );
    assert_eq!(vec_decoded.code(), code, "vec roundtrip code mismatch");
    assert_eq!(
        vec_decoded.payload(),
        payload,
        "vec roundtrip payload mismatch"
    );

    // Invariant 7: frame.header() agrees with decoded header.
    let frame_header = frame.header().expect("header() failed on valid frame");
    assert_eq!(frame_header.code(), code);
    assert_eq!(frame_header.payload_len() as usize, payload.len());

    // Invariant 4: encode_into_writer + recv_msg round-trip.
    let mut writer_encoded = Vec::new();
    if frame.encode_into_writer(&mut writer_encoded).is_err() {
        return;
    }
    let mut cursor = Cursor::new(&writer_encoded);
    let writer_decoded =
        recv_msg(&mut cursor).expect("recv_msg failed on encode_into_writer output");
    assert_eq!(
        writer_decoded.code(),
        code,
        "writer roundtrip code mismatch"
    );
    assert_eq!(
        writer_decoded.payload(),
        payload,
        "writer roundtrip payload mismatch"
    );

    // Cross-path check: both encode paths produce the same bytes.
    assert_eq!(
        vec_encoded, writer_encoded,
        "encode_into_vec and encode_into_writer produced different bytes"
    );
}

/// Invariant 5: owned vs borrowed decoder agreement on arbitrary wire bytes.
fn check_owned_vs_borrowed(input: &DiffInput) {
    let code = select_code(input.code_selector);
    let payload = if input.payload.len() > MAX_PAYLOAD {
        &input.payload[..MAX_PAYLOAD]
    } else {
        &input.payload[..]
    };

    // Build a well-formed wire frame to test both decoders.
    let frame = match MessageFrame::new(code, payload.to_vec()) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut wire = Vec::new();
    if frame.encode_into_vec(&mut wire).is_err() {
        return;
    }
    // Append some trailing bytes to test remainder handling.
    wire.extend_from_slice(&input.multi_frame_bytes[..input.multi_frame_bytes.len().min(32)]);

    let owned_result = MessageFrame::decode_from_slice(&wire);
    let borrowed_result = BorrowedMessageFrame::decode_from_slice(&wire);

    match (&owned_result, &borrowed_result) {
        (Ok((owned, owned_rem)), Ok((borrowed, borrowed_rem))) => {
            assert_eq!(
                owned.code(),
                borrowed.code(),
                "owned/borrowed code divergence"
            );
            assert_eq!(
                owned.payload(),
                borrowed.payload(),
                "owned/borrowed payload divergence"
            );
            assert_eq!(
                owned_rem.len(),
                borrowed_rem.len(),
                "owned/borrowed remainder length divergence"
            );
        }
        (Err(_), Err(_)) => {}
        (a, b) => {
            panic!("owned/borrowed disagree: owned={a:?} borrowed={b:?}");
        }
    }
}

/// Invariant 6: iterator vs manual decode_from_slice produce the same sequence.
fn check_iterator_vs_manual(bytes: &[u8]) {
    // Collect frames from the iterator.
    let mut iter_frames: Vec<(MessageCode, Vec<u8>)> = Vec::new();
    let mut iter_stopped_on_error = false;
    for frame_result in BorrowedMessageFrames::new(bytes) {
        match frame_result {
            Ok(frame) => {
                iter_frames.push((frame.code(), frame.payload().to_vec()));
            }
            Err(_) => {
                iter_stopped_on_error = true;
                break;
            }
        }
    }

    // Collect frames via manual decode_from_slice loop. Mirror BorrowedMessageFrames::next:
    // attempt to decode while the remainder is non-empty so a short tail surfaces a
    // TruncatedHeader Err rather than silently exiting, matching iterator semantics.
    let mut manual_frames: Vec<(MessageCode, Vec<u8>)> = Vec::new();
    let mut remaining = bytes;
    let mut manual_stopped_on_error = false;
    while !remaining.is_empty() {
        match BorrowedMessageFrame::decode_from_slice(remaining) {
            Ok((frame, rest)) => {
                manual_frames.push((frame.code(), frame.payload().to_vec()));
                remaining = rest;
            }
            Err(_) => {
                manual_stopped_on_error = true;
                break;
            }
        }
    }

    assert_eq!(
        iter_frames.len(),
        manual_frames.len(),
        "iterator/manual frame count divergence: iter={} manual={} input_len={}",
        iter_frames.len(),
        manual_frames.len(),
        bytes.len()
    );

    for (i, (iter_f, manual_f)) in iter_frames.iter().zip(manual_frames.iter()).enumerate() {
        assert_eq!(
            iter_f.0, manual_f.0,
            "frame {i}: iterator/manual code divergence"
        );
        assert_eq!(
            iter_f.1, manual_f.1,
            "frame {i}: iterator/manual payload divergence"
        );
    }

    assert_eq!(
        iter_stopped_on_error, manual_stopped_on_error,
        "iterator/manual error-stop divergence"
    );
}

/// Raw byte differential: ensure decode/from_raw agree on arbitrary 4-byte
/// slices extracted from the fuzzer input.
fn check_raw_bytes(data: &[u8]) {
    if data.len() >= MESSAGE_HEADER_LEN {
        let mut buf = [0u8; MESSAGE_HEADER_LEN];
        buf.copy_from_slice(&data[..MESSAGE_HEADER_LEN]);
        let raw = u32::from_le_bytes(buf);
        check_header_differential(raw);
    }

    // Also feed into owned and borrowed decoders to check agreement.
    let owned = MessageFrame::decode_from_slice(data);
    let borrowed = BorrowedMessageFrame::decode_from_slice(data);
    match (&owned, &borrowed) {
        (Ok((o, o_rem)), Ok((b, b_rem))) => {
            assert_eq!(o.code(), b.code(), "raw: owned/borrowed code divergence");
            assert_eq!(
                o.payload(),
                b.payload(),
                "raw: owned/borrowed payload divergence"
            );
            assert_eq!(
                o_rem.len(),
                b_rem.len(),
                "raw: owned/borrowed remainder divergence"
            );
        }
        (Err(_), Err(_)) => {}
        _ => {
            // One succeeded and the other failed - finding.
            panic!("raw bytes: owned/borrowed disagree: owned={owned:?} borrowed={borrowed:?}");
        }
    }
}
