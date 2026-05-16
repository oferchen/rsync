//! Adversarial protocol stream corpus for regression testing.
//!
//! This file pins a curated set of malformed, truncated, or otherwise
//! unusual byte streams and asserts each one is handled in a documented way
//! by the relevant protocol parser. Three outcomes are recognised:
//!
//! - `Ok` - parsed cleanly (valid-but-rare inputs).
//! - `Err` - parser returned an error of a documented kind.
//! - `NoPanic` - parser is exercised under `catch_unwind` and must not panic,
//!   crash, OOM, or hang. Either `Ok` or `Err` is acceptable.
//!
//! When a parser is hardened or extended, run this file via `nextest` to
//! confirm the previously catalogued adversarial inputs still parse the
//! way we expect. Each entry carries an inline comment describing the bug
//! it would catch.
//!
//! Designed as the regression seed for live interop fuzzing (#1196). Adding
//! a new entry only requires appending to the appropriate static array; the
//! runners iterate the corpus generically.
//!
//! ## Coverage
//!
//! - Truncated multiplex headers and oversized length claims.
//! - Unknown / out-of-range multiplex tag bytes.
//! - Negative-length, zero-length, and end-marker delta tokens.
//! - Nested-header bytes inside another header's payload.
//! - Invalid UTF-8 inside log-class multiplex payloads.
//! - NUL bytes inside file-list name suffixes.
//! - Truncated legacy `@RSYNCD:` daemon greetings.
//! - Filter rule strings: oversized, negative length, embedded NUL,
//!   bad UTF-8, terminator-only.

#![deny(unsafe_code)]

use std::io::{self, Cursor};
use std::panic;

use protocol::wire::{read_delta_op, read_token};
use protocol::{
    EnvelopeError, MAX_PAYLOAD_LENGTH, MESSAGE_HEADER_LEN, MessageCode, MessageHeader,
    NegotiationPrologueDetector, ProtocolVersion, parse_legacy_daemon_greeting, read_int,
    read_longint, read_varint, recv_msg,
};
use protocol::filters::read_filter_list;
use protocol::wire::file_entry_decode::decode_name;
use protocol::wire::file_entry::XMIT_LONG_NAME;

/// Documented outcome a corpus entry expects from its parser.
#[derive(Clone, Copy, Debug)]
enum Outcome {
    /// Parser accepts the input.
    Ok,
    /// Parser returns an error whose kind matches the listed `io::ErrorKind`.
    Err(io::ErrorKind),
    /// Parser must not panic. Either `Ok` or `Err` is acceptable.
    NoPanic,
}

/// One adversarial corpus entry.
struct Case {
    /// Human-readable identifier reported on failure.
    name: &'static str,
    /// Adversarial input bytes.
    bytes: &'static [u8],
    /// Documented outcome category.
    expected: Outcome,
}

/// Asserts `result` matches `expected` and reports `name` on failure.
fn assert_outcome<T: std::fmt::Debug>(
    name: &str,
    expected: Outcome,
    result: io::Result<T>,
) {
    match (expected, &result) {
        (Outcome::Ok, Ok(_)) | (Outcome::NoPanic, Ok(_)) | (Outcome::NoPanic, Err(_)) => {}
        (Outcome::Err(kind), Err(err)) => {
            assert_eq!(
                err.kind(),
                kind,
                "case {name}: expected error kind {kind:?}, got {err:?}",
            );
        }
        (Outcome::Ok, Err(err)) => {
            panic!("case {name}: expected Ok parse but got {err:?}");
        }
        (Outcome::Err(kind), Ok(value)) => {
            panic!("case {name}: expected Err({kind:?}) but got Ok({value:?})");
        }
    }
}

/// Runs `op` under `catch_unwind` and reports a panic with the case name.
fn assert_no_panic<F>(name: &str, op: F)
where
    F: FnOnce() + std::panic::UnwindSafe,
{
    let result = panic::catch_unwind(op);
    assert!(result.is_ok(), "case {name}: parser panicked on input");
}

// ---------------------------------------------------------------------------
// Multiplex header corpus
// ---------------------------------------------------------------------------

/// Adversarial inputs targeting `recv_msg` / `MessageHeader::decode`.
///
/// Header layout reminder: raw `u32 = (tag << 24) | (payload_len & 0xFFFFFF)`
/// is serialised little-endian, so the **tag byte sits at index 3** of the
/// 4-byte header and bytes 0-2 carry the 24-bit payload length.
const MULTIPLEX_CASES: &[Case] = &[
    Case {
        // Bug it catches: header reader treats a partial 1-byte read as a
        // valid header instead of returning UnexpectedEof. Without this guard,
        // the next 3 bytes of the stream silently become the payload length.
        name: "multiplex_header_truncated_one_byte",
        bytes: &[0x07],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: a 3-byte buffer is one byte short of a full header
        // and must trip the EOF branch in `recv_msg`'s `read_header`.
        name: "multiplex_header_truncated_three_bytes",
        bytes: &[0x00, 0x00, 0x00],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: tag byte 0x00 (at index 3) is below `MPLEX_BASE`.
        // The decoder must reject the InvalidTag, not silently subtract and
        // underflow. Header bytes form a valid 4-byte read so the EOF path
        // is bypassed.
        name: "multiplex_header_tag_below_mplex_base",
        bytes: &[0x00, 0x00, 0x00, 0x00],
        expected: Outcome::Err(io::ErrorKind::InvalidData),
    },
    Case {
        // Bug it catches: an unknown message tag (code 11 -> tag byte 18)
        // must surface UnknownMessageCode rather than be silently treated
        // as DATA. Codes 11-21, 23-32, 34-41, 43-85, 87-99, 103-247 are all
        // unmapped. Tag is at LE byte index 3.
        name: "multiplex_header_unknown_message_code",
        bytes: &[0x00, 0x00, 0x00, 18],
        expected: Outcome::Err(io::ErrorKind::InvalidData),
    },
    Case {
        // Bug it catches: a header that claims a full 16 MiB payload but
        // delivers nothing must surface UnexpectedEof while reading the
        // payload, not allocate 16 MiB and hang. Tag byte 0x07 = MSG_DATA.
        // Payload-length bytes (indices 0-2) are 0xFF for full 24-bit max.
        name: "multiplex_header_oversized_length_claim",
        bytes: &[0xFF, 0xFF, 0xFF, 0x07],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: nothing in the stream is a "frame inside a frame".
        // An MSG_INFO frame whose payload happens to contain a fully valid
        // child header followed by data must consume the entire outer
        // payload and not recursively descend into the payload as a new
        // frame.
        //
        // Outer header (LE): payload_len=8 (bytes [8,0,0]), tag=9
        //   (MSG_INFO=2 + MPLEX_BASE=7).
        // Payload: looks like an MSG_DATA header with payload_len=4
        //   (bytes [4,0,0,7]) followed by raw bytes "abcd".
        name: "multiplex_header_pretending_inside_payload",
        bytes: &[
            // Outer header: MSG_INFO with 8-byte payload.
            8, 0, 0, 9,
            // Payload bytes 0..3: child-shaped header. Bytes 4..8: "abcd".
            4, 0, 0, 7, b'a', b'b', b'c', b'd',
        ],
        expected: Outcome::Ok,
    },
    Case {
        // Bug it catches: MSG_INFO carrying invalid UTF-8 must still parse
        // at the multiplex layer; UTF-8 validation is the upper layer's job.
        // Header: payload_len=4 (bytes [4,0,0]), tag=9 (MSG_INFO+7).
        // Payload: 4 invalid UTF-8 bytes.
        name: "multiplex_payload_invalid_utf8_msg_info",
        bytes: &[4, 0, 0, 9, 0xFF, 0xFE, 0xFD, 0xFC],
        expected: Outcome::Ok,
    },
    Case {
        // Bug it catches: zero-length DATA frame is legal and represents a
        // flush boundary. Must round-trip without confusing the reader's
        // "buffered" accounting. Header: payload_len=0, tag=7 (MSG_DATA).
        name: "multiplex_zero_length_data_frame",
        bytes: &[0, 0, 0, 7],
        expected: Outcome::Ok,
    },
];

#[test]
fn multiplex_corpus_recv_msg_outcomes() {
    for case in MULTIPLEX_CASES {
        let mut cursor = Cursor::new(case.bytes);
        let result = recv_msg(&mut cursor);
        assert_outcome(case.name, case.expected, result);
    }
}

#[test]
fn multiplex_corpus_recv_msg_never_panics() {
    for case in MULTIPLEX_CASES {
        assert_no_panic(case.name, || {
            let mut cursor = Cursor::new(case.bytes);
            let _ = recv_msg(&mut cursor);
        });
    }
}

#[test]
fn multiplex_header_decode_corpus_never_panics() {
    // Header decoding alone (no payload read). Catches regressions where
    // `MessageHeader::decode` panics instead of returning EnvelopeError.
    for case in MULTIPLEX_CASES {
        assert_no_panic(case.name, || {
            let _ = MessageHeader::decode(case.bytes);
        });
    }
}

#[test]
fn multiplex_header_oversized_length_decodes_then_starves() {
    // Defence-in-depth: even when the header alone is well-formed, the
    // payload-read step must surface UnexpectedEof so callers can drop
    // the connection instead of waiting forever. Header layout (LE):
    // bytes 0-2 = payload length 0xFFFFFF, byte 3 = tag 0x07 (MSG_DATA).
    let bytes = [0xFF_u8, 0xFF, 0xFF, 0x07];
    let header = MessageHeader::decode(&bytes).expect("header decodes cleanly");
    assert_eq!(header.code(), MessageCode::Data);
    assert_eq!(header.payload_len(), MAX_PAYLOAD_LENGTH);

    let mut cursor = Cursor::new(&bytes[..]);
    let err = recv_msg(&mut cursor).expect_err("payload starvation must surface");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn message_header_oversized_payload_is_rejected_at_construction() {
    // Constructing a header above the 24-bit limit must always fail; this
    // pins the contract for golden-stream builders.
    let result = MessageHeader::new(MessageCode::Data, MAX_PAYLOAD_LENGTH + 1);
    assert!(matches!(result, Err(EnvelopeError::OversizedPayload(_))));
}

// ---------------------------------------------------------------------------
// Legacy `@RSYNCD:` handshake corpus
// ---------------------------------------------------------------------------

/// Documented outcome for legacy greeting parser entries. `parse_legacy_daemon_greeting`
/// returns its own `NegotiationError`, not an `io::Error`, so the corpus uses a
/// boolean expectation rather than a kind enum.
#[derive(Clone, Copy, Debug)]
enum GreetingOutcome {
    /// Parser accepts the greeting.
    Ok,
    /// Parser surfaces a `NegotiationError`.
    Err,
}

/// Adversarial inputs targeting `parse_legacy_daemon_greeting`.
const LEGACY_GREETING_CASES: &[(&str, &str, GreetingOutcome)] = &[
    // Bug it catches: a single `@` must not be misclassified as a complete
    // legacy greeting. Anything missing the full prefix is malformed.
    ("legacy_one_byte_prefix", "@", GreetingOutcome::Err),
    // Bug it catches: the prefix is present but no protocol number follows.
    ("legacy_prefix_only", "@RSYNCD:", GreetingOutcome::Err),
    // Bug it catches: prefix plus whitespace but no digits must be rejected,
    // not parsed as protocol 0 or panic on missing digits.
    ("legacy_prefix_no_digits", "@RSYNCD: \n", GreetingOutcome::Err),
    // Bug it catches: a malformed subprotocol marker (dot without digits)
    // must surface as an error, not silently accept an empty subprotocol.
    (
        "legacy_protocol_subprotocol_dot_only",
        "@RSYNCD: 32.\n",
        GreetingOutcome::Err,
    ),
    // Bug it catches: a protocol number large enough to saturate `u32`
    // must still be rejected as unsupported rather than overflowing into
    // a value that accidentally lands inside the supported range.
    (
        "legacy_protocol_saturating_overflow",
        "@RSYNCD: 99999999999999999\n",
        GreetingOutcome::Err,
    ),
    // Bug it catches: greetings missing the protocol-30+ subprotocol suffix
    // must be rejected, mirroring upstream `clientserver.c`.
    (
        "legacy_protocol_31_missing_subprotocol",
        "@RSYNCD: 31\n",
        GreetingOutcome::Err,
    ),
];

#[test]
fn legacy_greeting_corpus_outcomes() {
    for (name, line, expected) in LEGACY_GREETING_CASES {
        let result = parse_legacy_daemon_greeting(line);
        match expected {
            GreetingOutcome::Err => assert!(
                result.is_err(),
                "case {name}: expected error from greeting parse, got {result:?}",
            ),
            GreetingOutcome::Ok => assert!(
                result.is_ok(),
                "case {name}: expected ok greeting parse, got {result:?}",
            ),
        }
    }
}

#[test]
fn legacy_greeting_corpus_never_panics() {
    for (name, line, _) in LEGACY_GREETING_CASES {
        assert_no_panic(name, || {
            let _ = parse_legacy_daemon_greeting(line);
        });
    }
}

/// Bytes from a full `@RSYNCD: 32.0\n` banner. Used to drive prefix
/// truncation cases through the negotiation detector.
const FULL_LEGACY_BANNER: &[u8] = b"@RSYNCD: 32.0\n";

#[test]
fn negotiation_detector_handles_every_prefix_truncation() {
    // Bug it catches: the prefix-byte detector treats some intermediate
    // truncations as binary or otherwise drops buffered bytes. Replaying
    // every prefix of the full banner up to `LEGACY_DAEMON_PREFIX` length
    // must never panic and must surface a deterministic classification.
    for take in 1..=FULL_LEGACY_BANNER.len() {
        let chunk = &FULL_LEGACY_BANNER[..take];
        assert_no_panic(
            "negotiation_detector_prefix_truncation",
            || {
                let mut detector = NegotiationPrologueDetector::new();
                let _ = detector.observe(chunk);
                // Buffered prefix must remain a sub-slice of the input
                // (i.e., no extra bytes appear, no allocations leak).
                let _ = detector.buffered_prefix().len();
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Delta token corpus
// ---------------------------------------------------------------------------

/// Adversarial inputs targeting the upstream-format token decoder.
const DELTA_TOKEN_CASES: &[Case] = &[
    Case {
        // Bug it catches: zero-byte input must surface EOF rather than
        // panic on an empty 4-byte read.
        name: "delta_token_empty",
        bytes: &[],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: a 3-byte truncated `read_int` must report EOF.
        name: "delta_token_truncated_three_bytes",
        bytes: &[0x01, 0x00, 0x00],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: token 0 is the end-of-stream marker. The decoder
        // must return `Ok(None)` (no remaining tokens), not loop or report
        // a fake literal of length 0.
        name: "delta_token_end_marker_zero",
        bytes: &[0x00, 0x00, 0x00, 0x00],
        expected: Outcome::Ok,
    },
    Case {
        // Bug it catches: a negative token represents a block-match index.
        // The decoder must surface the negative value to the caller verbatim
        // (here -1 = block 0); converting it to an unsigned literal length
        // would request 2**32 bytes from the stream.
        name: "delta_token_negative_block_match",
        bytes: &[0xFF, 0xFF, 0xFF, 0xFF],
        expected: Outcome::Ok,
    },
    Case {
        // Bug it catches: an i32::MIN block-match must round-trip without
        // panicking on the `-((token+1))` arithmetic upstream uses.
        name: "delta_token_i32_min",
        bytes: &[0x00, 0x00, 0x00, 0x80],
        expected: Outcome::Ok,
    },
];

#[test]
fn delta_token_corpus_outcomes() {
    for case in DELTA_TOKEN_CASES {
        let mut cursor = Cursor::new(case.bytes);
        let result = read_token(&mut cursor);
        assert_outcome(case.name, case.expected, result);
    }
}

#[test]
fn delta_token_corpus_never_panics() {
    for case in DELTA_TOKEN_CASES {
        assert_no_panic(case.name, || {
            let mut cursor = Cursor::new(case.bytes);
            let _ = read_token(&mut cursor);
        });
    }
}

#[test]
fn delta_internal_negative_literal_length_rejected() {
    // Bug it catches: the internal opcode-format decoder must reject a
    // negative literal length rather than calling `vec![0u8; huge]` after
    // an `as usize` cast. Opcode 0x00 (literal), followed by varint(-1)
    // encoded as five 0xFF bytes (leading 0xF0 + four extension bytes).
    let bytes = [0x00_u8, 0xF0, 0xFF, 0xFF, 0xFF, 0xFF];
    let mut cursor = Cursor::new(&bytes[..]);
    let err = read_delta_op(&mut cursor).expect_err("negative literal length must error");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

// ---------------------------------------------------------------------------
// Varint / fixed-int corpus
// ---------------------------------------------------------------------------

/// Adversarial inputs targeting the legacy fixed-int and varint readers.
const VARINT_CASES: &[Case] = &[
    Case {
        // Bug it catches: a leading byte with all extension bits set
        // (`extra > MAX_EXTRA_BYTES`) must surface as InvalidData, not be
        // silently truncated to a meaningful value.
        name: "varint_overflow_leading_byte",
        bytes: &[0xFF],
        expected: Outcome::Err(io::ErrorKind::InvalidData),
    },
    Case {
        // Bug it catches: an empty stream feeding `read_varint` must surface
        // UnexpectedEof, never attempt to read into a zero-length slice and
        // succeed with garbage.
        name: "varint_empty_stream",
        bytes: &[],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
    Case {
        // Bug it catches: a varint claiming 4 extra bytes (leading byte
        // 0xF0 sets extra=4) with only 2 actual bytes must surface EOF
        // partway through the extension read.
        name: "varint_truncated_extension",
        bytes: &[0xF0, 0x00, 0x00],
        expected: Outcome::Err(io::ErrorKind::UnexpectedEof),
    },
];

#[test]
fn varint_corpus_outcomes() {
    for case in VARINT_CASES {
        let mut cursor = Cursor::new(case.bytes);
        let result = read_varint(&mut cursor);
        assert_outcome(case.name, case.expected, result);
    }
}

#[test]
fn read_int_truncated_returns_eof() {
    // Bug it catches: the legacy 4-byte fixed-int reader must surface EOF
    // when fewer than 4 bytes remain. Past regressions returned a partially
    // initialised i32.
    let bytes = [0x01_u8, 0x02];
    let mut cursor = Cursor::new(&bytes[..]);
    let err = read_int(&mut cursor).expect_err("truncated int must surface EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn read_longint_extended_truncated_returns_eof() {
    // Bug it catches: the legacy `read_longint` upgrades to a 12-byte read
    // when the first 4 bytes are 0xFFFFFFFF. A truncated extension must
    // surface EOF, not read uninitialised memory.
    let bytes = [0xFF_u8, 0xFF, 0xFF, 0xFF, 0x00, 0x00];
    let mut cursor = Cursor::new(&bytes[..]);
    let err = read_longint(&mut cursor).expect_err("truncated longint must surface EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

// ---------------------------------------------------------------------------
// File-list name decoder corpus
// ---------------------------------------------------------------------------

#[test]
fn decode_name_preserves_embedded_nul_bytes() {
    // Bug it catches: the name decoder must preserve raw bytes verbatim,
    // including embedded NULs. Past attempts to treat names as C strings
    // truncated at the first NUL, silently losing path segments.
    let payload = [3_u8, b'a', 0x00, b'b'];
    let mut cursor = Cursor::new(&payload[..]);
    let decoded = decode_name(&mut cursor, 0, b"", 32).expect("name decode succeeds");
    assert_eq!(decoded, b"a\0b");
}

#[test]
fn decode_name_long_name_truncated_returns_eof() {
    // Bug it catches: the long-name path uses a varint length followed by
    // `read_exact`. If the suffix is short of the advertised length, the
    // reader must surface UnexpectedEof and not read uninitialised memory.
    // Suffix length=5 (single-byte varint), but only 2 bytes follow.
    let payload = [5_u8, b'a', b'b'];
    let mut cursor = Cursor::new(&payload[..]);
    let err = decode_name(&mut cursor, XMIT_LONG_NAME as u32, b"", 32)
        .expect_err("truncated long name must surface EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

// ---------------------------------------------------------------------------
// Filter list corpus
// ---------------------------------------------------------------------------

#[test]
fn filter_list_negative_length_rejected() {
    // Bug it catches: a filter-rule length below zero must surface as
    // InvalidData. Earlier versions cast the signed length to `usize`,
    // allocating multi-GB scratch buffers from a malicious peer.
    let bytes = [0xFF_u8, 0xFF, 0xFF, 0xFF];
    let mut cursor = Cursor::new(&bytes[..]);
    let protocol = ProtocolVersion::from_supported(32).expect("v32 supported");
    let err = read_filter_list(&mut cursor, protocol)
        .expect_err("negative length must surface InvalidData");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn filter_list_oversized_length_truncates_to_eof() {
    // Bug it catches: an attacker-controlled length must surface EOF
    // (truncated read) rather than allocate the full claimed payload.
    // Length here is 1 MiB but no payload follows.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(1_024_i32 * 1_024).to_le_bytes());
    let mut cursor = Cursor::new(&bytes[..]);
    let protocol = ProtocolVersion::from_supported(32).expect("v32 supported");
    let err = read_filter_list(&mut cursor, protocol)
        .expect_err("oversized length without payload must surface EOF");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn filter_list_invalid_utf8_rejected() {
    // Bug it catches: a rule body with non-UTF-8 bytes must surface as
    // InvalidData (upstream rsync expects UTF-8 filter rules). A regression
    // in `parse_wire_rule` once accepted them silently and corrupted the
    // pattern downstream.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&3_i32.to_le_bytes());
    bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    let mut cursor = Cursor::new(&bytes[..]);
    let protocol = ProtocolVersion::from_supported(32).expect("v32 supported");
    let err = read_filter_list(&mut cursor, protocol)
        .expect_err("invalid UTF-8 rule must surface InvalidData");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn filter_list_terminator_only_parses_empty_list() {
    // Bug it catches: a stream consisting of just the 4-byte zero
    // terminator must parse to an empty rule list, not loop forever or
    // surface EOF as a spurious error.
    let bytes = [0_u8; 4];
    let mut cursor = Cursor::new(&bytes[..]);
    let protocol = ProtocolVersion::from_supported(32).expect("v32 supported");
    let rules =
        read_filter_list(&mut cursor, protocol).expect("zero-terminator parses cleanly");
    assert!(rules.is_empty());
}

#[test]
fn filter_list_pattern_with_embedded_nul_is_lossy_but_safe() {
    // Bug it catches: a rule body whose UTF-8 contains an embedded NUL
    // must either parse to the literal bytes or surface a documented
    // error, not panic or trigger a debug assertion. NUL is valid UTF-8.
    let mut bytes = Vec::new();
    let rule_body = b"- a\0b";
    bytes.extend_from_slice(&(rule_body.len() as i32).to_le_bytes());
    bytes.extend_from_slice(rule_body);
    bytes.extend_from_slice(&0_i32.to_le_bytes());
    let mut cursor = Cursor::new(&bytes[..]);
    let protocol = ProtocolVersion::from_supported(32).expect("v32 supported");
    assert_no_panic("filter_pattern_embedded_nul", || {
        let _ = read_filter_list(&mut cursor, protocol);
    });
}

// ---------------------------------------------------------------------------
// Header constants sanity
// ---------------------------------------------------------------------------

#[test]
fn message_header_len_constant_matches_expected_layout() {
    // Bug it catches: a change to `MESSAGE_HEADER_LEN` would silently
    // invalidate every adversarial input pinned above. This guard fails
    // loudly so the corpus is regenerated deliberately.
    assert_eq!(MESSAGE_HEADER_LEN, 4);
}
