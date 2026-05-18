#![no_main]

//! Fuzz target for the rsync varint / varlong codec.
//!
//! Variable-length integers appear repeatedly in the rsync wire protocol
//! (compatibility flags, file-list indices, varlong-encoded sizes and
//! timestamps). The reader runs on untrusted bytes received from the peer,
//! so any panic discovered here is a remote attack surface.
//!
//! This target combines two complementary strategies:
//!
//! * **Decoder-only fuzzing.** Raw fuzzer bytes are fed into [`read_varint`],
//!   [`decode_varint`], [`read_varlong`], [`read_longint`], [`read_int`], and
//!   [`read_varint30_int`]. Any panic on arbitrary input is a finding.
//! * **Encoder/decoder round-trips.** Values drawn from the fuzz input are
//!   written with the matching `write_*` helper, then read back and asserted
//!   equal. This surfaces encoder/decoder asymmetries that pure decode fuzzing
//!   would miss.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run varint_decode
//! ```

use std::io::Cursor;

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use protocol::{
    decode_varint, encode_varint_to_vec, read_int, read_longint, read_varint, read_varint30_int,
    read_varlong, write_int, write_longint, write_varint, write_varint30_int, write_varlong,
};

/// Structured round-trip inputs derived from the fuzz buffer. Each field
/// targets a separate codec entry point so libFuzzer can exercise the matrix
/// of encodings (varint, varlong with arbitrary `min_bytes`, fixed-width int,
/// legacy longint, and the protocol-version-gated `varint30`).
#[derive(Arbitrary, Debug)]
struct RoundTrip {
    varint_value: i32,
    varlong_value: i64,
    varlong_min_bytes: u8,
    int_value: i32,
    longint_value: i64,
    varint30_value: i32,
    varint30_protocol: u8,
}

fuzz_target!(|data: &[u8]| {
    decode_only(data);
    round_trip(data);
});

/// Feed the raw byte stream into every decode entry point. The decoders must
/// reject malformed input via [`io::Result`] rather than by panicking.
fn decode_only(data: &[u8]) {
    let _ = decode_varint(data);

    let mut cursor = Cursor::new(data);
    let _ = read_varint(&mut cursor);

    let mut cursor = Cursor::new(data);
    let _ = read_int(&mut cursor);

    let mut cursor = Cursor::new(data);
    let _ = read_longint(&mut cursor);

    // `read_varlong` requires `min_bytes` in `1..=8`; mirror upstream's range
    // by exercising all valid values.
    for min_bytes in 1u8..=8 {
        let mut cursor = Cursor::new(data);
        let _ = read_varlong(&mut cursor, min_bytes);
    }

    // Protocol-version-gated path: < 30 routes through fixed `read_int`,
    // >= 30 routes through `read_varint`. Cover both branches.
    for protocol_version in [28u8, 30, 31, 32] {
        let mut cursor = Cursor::new(data);
        let _ = read_varint30_int(&mut cursor, protocol_version);
    }
}

/// Draw structured values from the fuzz input, encode them, and assert the
/// decoder reproduces the original. Encoder/decoder disagreement is a bug,
/// regardless of which side is at fault.
fn round_trip(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let Ok(input) = RoundTrip::arbitrary(&mut u) else {
        return;
    };

    // varint i32 round-trip via the streaming API.
    {
        let mut buf = Vec::new();
        write_varint(&mut buf, input.varint_value).expect("write_varint to Vec cannot fail");
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = read_varint(&mut cursor).expect("varint round-trip must succeed");
        assert_eq!(
            decoded, input.varint_value,
            "varint round-trip diverged for {}",
            input.varint_value
        );
        assert_eq!(
            cursor.position() as usize,
            buf.len(),
            "varint decoder left trailing bytes"
        );
    }

    // varint i32 round-trip via the in-memory helpers - keeps the slice-based
    // decode path in lockstep with the streaming one.
    {
        let mut buf = Vec::new();
        encode_varint_to_vec(input.varint_value, &mut buf);
        let (decoded, rest) = decode_varint(&buf).expect("encode_varint_to_vec output must decode");
        assert_eq!(decoded, input.varint_value);
        assert!(rest.is_empty(), "decode_varint left trailing bytes");
    }

    // varlong i64 round-trip. Upstream clamps `min_bytes` into `1..=8`, so we
    // do the same to keep the fuzzer focused on valid inputs.
    {
        let min_bytes = (input.varlong_min_bytes % 8) + 1;
        let mut buf = Vec::new();
        write_varlong(&mut buf, input.varlong_value, min_bytes)
            .expect("write_varlong to Vec cannot fail");
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded =
            read_varlong(&mut cursor, min_bytes).expect("varlong round-trip must succeed");
        assert_eq!(
            decoded, input.varlong_value,
            "varlong round-trip diverged for value={} min_bytes={}",
            input.varlong_value, min_bytes,
        );
    }

    // Fixed 4-byte int round-trip (protocol < 30 path).
    {
        let mut buf = Vec::new();
        write_int(&mut buf, input.int_value).expect("write_int to Vec cannot fail");
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = read_int(&mut cursor).expect("int round-trip must succeed");
        assert_eq!(decoded, input.int_value);
        assert_eq!(buf.len(), 4, "write_int must emit exactly 4 bytes");
    }

    // Legacy longint round-trip (4-byte fast path or 12-byte escape).
    {
        let mut buf = Vec::new();
        write_longint(&mut buf, input.longint_value).expect("write_longint to Vec cannot fail");
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = read_longint(&mut cursor).expect("longint round-trip must succeed");
        assert_eq!(decoded, input.longint_value);
    }

    // varint30 round-trip across both protocol branches.
    {
        let mut buf = Vec::new();
        write_varint30_int(&mut buf, input.varint30_value, input.varint30_protocol)
            .expect("write_varint30_int to Vec cannot fail");
        let mut cursor = Cursor::new(buf.as_slice());
        let decoded = read_varint30_int(&mut cursor, input.varint30_protocol)
            .expect("varint30 round-trip must succeed");
        assert_eq!(decoded, input.varint30_value);
    }
}
