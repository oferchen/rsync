#![no_main]

//! Fuzz target for the post-auth NDX (file-index) codec.
//!
//! After negotiation, every file requested during the transfer phase is keyed
//! by a delta-encoded NDX value on the wire. The decoder is stateful (each
//! value mutates the rolling previous-positive / previous-negative
//! accumulators) and the modern variant accepts multi-byte extension
//! prefixes, so a coverage-guided fuzzer is well-placed to find divergence
//! and truncation bugs.
//!
//! We exercise the legacy (protocol 28-29) and modern (protocol 30+) codecs
//! against every prefix of the input until the inner reader is exhausted.
//! Upstream reference: `io.c:read_ndx()`.
//!
//! # Oracle
//!
//! Beyond panic-freedom this target enforces an encoder/decoder inverse
//! invariant: every value `v` the decoder pulls off the fuzzed wire is fed
//! back through a lock-step encoder and re-decoder pair, and the re-decoded
//! value must equal `v`. Because the NDX wire is *not* canonical (the same
//! delta has a short and a 0xFE-prefixed long form), the oracle compares
//! decoded *values*, never re-encoded bytes: `read` and `write` must be
//! mutually inverse across the exact value sequence the fuzzer produced. The
//! encoder and re-decoder each only observe that value sequence, so their
//! `prev_positive` / `prev_negative` accumulators evolve identically and a
//! faithful codec round-trips every value.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run ndx_codec
//! ```

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;

use protocol::codec::{NdxCodec, create_ndx_codec};

fuzz_target!(|data: &[u8]| {
    for version in [28u8, 29, 30, 31, 32] {
        let mut decoder = create_ndx_codec(version);
        // The encoder and verifier only ever see the value sequence the
        // decoder emits, so their rolling state stays in lock-step with each
        // other and `decode(encode(v)) == v` must hold for a correct codec.
        let mut encoder = create_ndx_codec(version);
        let mut verifier = create_ndx_codec(version);

        let mut cursor = Cursor::new(data);
        // Loop until the codec returns an error (typically truncated input)
        // so libFuzzer is rewarded for byte sequences that exercise the
        // multi-byte 0xFE / 0xFF / extended-encoding branches.
        while let Ok(value) = decoder.read_ndx(&mut cursor) {
            let mut encoded = Vec::new();
            encoder
                .write_ndx(&mut encoded, value)
                .expect("writing into a Vec never fails");

            let mut encoded_cursor = Cursor::new(encoded.as_slice());
            let round_tripped = verifier
                .read_ndx(&mut encoded_cursor)
                .expect("re-decoding self-encoded NDX bytes must succeed");

            assert_eq!(
                value, round_tripped,
                "NDX round-trip mismatch (protocol {version}): decoded {value} but \
                 encode+decode yielded {round_tripped}",
            );
        }
    }
});
