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
        let mut codec = create_ndx_codec(version);
        let mut cursor = Cursor::new(data);
        // Loop until the codec returns an error (typically truncated input)
        // so libFuzzer is rewarded for byte sequences that exercise the
        // multi-byte 0xFE / 0xFF / extended-encoding branches.
        while codec.read_ndx(&mut cursor).is_ok() {}
    }
});
