//! Wire-byte regression test for `zlib_codec` at protocol < 31 (RP28.i).
//!
//! Pins the observable wire-byte divergence between protocol 30 and
//! protocol 31 when `see_token` is fed an input larger than 0xFFFF bytes,
//! ensuring the protocol-conditional `offset` advance in
//! [`super::zlib_codec::ZlibTokenEncoder::see_token`] keeps mirroring the
//! upstream `protocol_version >= 31` branch in `token.c`.
//!
//! # Upstream reference
//!
//! upstream: `token.c:send_deflated_token()` lines 463-484 in
//! `target/interop/upstream-src/rsync-3.4.1/token.c`:
//!
//! ```c
//! } else if (token != -2 && do_compression == CPRES_ZLIB) {
//!     do {
//!         int32 n1 = toklen > 0xffff ? 0xffff : toklen;
//!         toklen -= n1;
//!         tx_strm.next_in = (Bytef *)map_ptr(buf, offset, n1);
//!         tx_strm.avail_in = n1;
//!         if (protocol_version >= 31) /* Newer protocols avoid a data-duplicating bug */
//!             offset += n1;
//!         tx_strm.next_out = (Bytef *) obuf;
//!         tx_strm.avail_out = AVAIL_OUT_SIZE(CHUNK_SIZE);
//!         r = deflate(&tx_strm, Z_INSERT_ONLY);
//!         ...
//!     } while (toklen > 0);
//! }
//! ```
//!
//! At protocol < 31 the `offset` cursor is never advanced between the
//! 0xFFFF-sized chunks fed through `deflate(..., Z_INSERT_ONLY)`. The
//! same data window is re-inserted into the deflate dictionary on every
//! loop iteration, producing a different compressor state than at
//! protocol 31 or later, where the cursor walks forward. Subsequent
//! literal output produced by `send_deflated_token()` therefore differs
//! between the two protocol families even though the outer
//! DEFLATED_DATA framing and END_FLAG terminator are identical.
//!
//! # Fixture
//!
//! Two encoders are constructed with the same compression level
//! ([`CompressionLevel::Default`]) and the same fixed input:
//!
//! 1. A 0x10001-byte `see_token` payload (`0x10001 == 65537`, one byte
//!    past the 0xFFFF chunk boundary) so the inner loop iterates twice
//!    and the protocol < 31 path re-inserts the first 0xFFFF bytes.
//! 2. A short trailing literal whose compressed encoding samples the
//!    deflate dictionary state. The compressor uses Z_SYNC_FLUSH, so the
//!    output of `send_literal` is deterministic for a fixed dictionary
//!    on any given backend.
//!
//! # Backend portability
//!
//! Exact compressed bytes inside DEFLATED_DATA payload differ across the
//! `flate2` backends (default `miniz_oxide`, `zlib-ng`, and `zlib-rs`).
//! The test therefore asserts framing invariants and inter-protocol
//! divergence rather than pinning a backend-specific compressed byte
//! sequence:
//!
//! - Both protocols MUST terminate with [`END_FLAG`].
//! - Both protocols MUST contain at least one DEFLATED_DATA block for the
//!   trailing literal.
//! - The compressed payload at protocol 30 MUST differ from protocol 31
//!   for this fixture - this is the wire-byte signature of the upstream
//!   data-duplicating bug.
//!
//! If a future change accidentally drops the `protocol_version >= 31`
//! gate (e.g. by always advancing the offset, or never advancing it), the
//! third assertion fires immediately.

use protocol::wire::{CompressedTokenEncoder, DEFLATED_DATA, END_FLAG};

use compress::zlib::CompressionLevel;

/// Builds the fixture-encoded buffer for the given protocol version.
///
/// Feeds `0x10001` bytes through `see_token` (one byte past the 0xFFFF
/// chunk boundary so the upstream inner loop iterates twice) and then
/// emits a trailing literal whose first bytes deliberately probe the
/// deflate dictionary's most-recent positions.
///
/// At `protocol_version >= 31`, the dictionary tail after see_token is
/// `[.., 254, 255, 0]` (the offset cursor walks forward correctly, so
/// the second iteration feeds `see_buf[0xFFFF..0x10001]`). At
/// `protocol_version <= 30`, the dictionary tail is `[.., 254, 0, 1]`
/// because the offset cursor never advances and the second iteration
/// re-feeds `see_buf[0..2]` (the upstream data-duplicating bug fixed at
/// protocol 31, see `token.c:473`).
///
/// The literal begins with `[0xFE, 0xFF, 0x00]`, which matches the
/// `[254, 255, 0]` triple sitting at the very tail of the protocol-31
/// dictionary at a small back-reference distance. At protocol <= 30 the
/// same triple only occurs in the cyclic interior of the dictionary at
/// a much larger distance. Deflate encodes the two distances with
/// different bit lengths, so the compressed payload diverges between
/// the two protocol families even though the outer framing is identical.
fn encode_fixture(protocol_version: u32) -> Vec<u8> {
    let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, protocol_version);

    // Size = 0xFFFF + 4 so the second iteration of see_token feeds 4 bytes,
    // creating a 4-byte tail divergence between protocol families:
    //   protocol >= 31 dict tail ends with [..., 254, 255, 0, 1, 2]
    //   protocol <= 30 dict tail ends with [..., 254, 0, 1, 2, 3]
    // (deflate needs >= 3-byte matches to emit a back-reference, so a
    // 4-byte tail diff is the minimum that produces wire-visible
    // divergence via different back-reference lengths and distances.)
    let mut see_buf = vec![0u8; 0xFFFF + 4];
    for (idx, byte) in see_buf.iter_mut().enumerate() {
        *byte = (idx & 0xFF) as u8;
    }
    encoder.see_token(&see_buf).unwrap();

    // Literal begins with [0xFF, 0x00, 0x01, 0x02] - exactly the 4-byte
    // sequence sitting at the very tail of the protocol-31 dictionary
    // (distance ~5 back-reference). At protocol <= 30 the same sequence
    // is not at the tail; deflate falls back to an interior cyclic
    // occurrence at a much larger distance, encoding it with a
    // different bit pattern. The trailing ASCII keeps human-readable
    // diff context.
    let mut trailing = Vec::with_capacity(32);
    trailing.extend_from_slice(&[0xFF, 0x00, 0x01, 0x02]);
    trailing.extend_from_slice(b" rp28-i fixture literal");

    let mut output = Vec::new();
    encoder.send_literal(&mut output, &trailing).unwrap();
    encoder.finish(&mut output).unwrap();
    output
}

/// Returns `true` if `buf` contains at least one DEFLATED_DATA header byte.
///
/// A DEFLATED_DATA header has its top two bits equal to `0b01`, matching
/// the upstream wire constant defined in `token.c:329`.
fn contains_deflated_data(buf: &[u8]) -> bool {
    buf.iter().any(|&b| (b & 0xC0) == DEFLATED_DATA)
}

/// Wire-byte regression: protocol 30 and protocol 31 must produce
/// distinguishable output when `see_token` straddles the 0xFFFF chunk
/// boundary.
///
/// Both protocols share the outer DEFLATED_DATA framing and END_FLAG
/// terminator. The difference is confined to the compressed payload
/// inside DEFLATED_DATA blocks and reflects the data-duplicating bug
/// fixed at protocol 31 (upstream `token.c:473`).
#[test]
fn rp28i_zlib_codec_protocol_30_diverges_from_protocol_31() {
    let buf_30 = encode_fixture(30);
    let buf_31 = encode_fixture(31);

    assert_eq!(
        *buf_30.last().unwrap(),
        END_FLAG,
        "protocol 30 output must terminate with END_FLAG"
    );
    assert_eq!(
        *buf_31.last().unwrap(),
        END_FLAG,
        "protocol 31 output must terminate with END_FLAG"
    );

    assert!(
        contains_deflated_data(&buf_30),
        "protocol 30 output must contain at least one DEFLATED_DATA block"
    );
    assert!(
        contains_deflated_data(&buf_31),
        "protocol 31 output must contain at least one DEFLATED_DATA block"
    );

    assert_ne!(
        buf_30, buf_31,
        "protocol 30 must diverge from protocol 31 on the see_token >0xFFFF \
         path - this is the wire-byte signature of the upstream data-duplicating \
         bug fixed at protocol 31 (token.c:473)"
    );
}

/// Wire-byte regression: protocol 32 (default) shares the >= 31 branch
/// with protocol 31, so their outputs must be byte-identical for this
/// fixture.
///
/// Pinning equality across 31 and 32 catches regressions that would
/// inadvertently treat protocol 32 as if it were on the pre-31 path.
#[test]
fn rp28i_zlib_codec_protocol_31_matches_protocol_32() {
    let buf_31 = encode_fixture(31);
    let buf_32 = encode_fixture(32);

    assert_eq!(
        buf_31, buf_32,
        "protocol 31 and 32 share the `protocol_version >= 31` see_token branch \
         and must produce identical wire bytes for the same fixture"
    );
}

/// Wire-byte regression: every protocol < 31 version must produce output
/// distinct from the protocol >= 31 baseline.
///
/// Iterates the supported pre-31 protocol versions (28-30) and confirms
/// each diverges from protocol 32. Any future change that silently
/// converges pre-31 behaviour onto the modern branch flips this test.
#[test]
fn rp28i_zlib_codec_all_pre_31_protocols_diverge_from_modern() {
    let modern = encode_fixture(32);

    for protocol in 28u32..=30 {
        let buf = encode_fixture(protocol);
        assert_eq!(
            *buf.last().unwrap(),
            END_FLAG,
            "protocol {protocol} output must terminate with END_FLAG"
        );
        assert!(
            contains_deflated_data(&buf),
            "protocol {protocol} output must contain at least one DEFLATED_DATA block"
        );
        assert_ne!(
            buf, modern,
            "protocol {protocol} must diverge from protocol 32 on the see_token \
             >0xFFFF path (pre-31 data-duplicating bug, upstream token.c:473)"
        );
    }
}
