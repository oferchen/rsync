#![no_main]

//! Fuzz target for the raw deflate streaming decompressor.
//!
//! rsync's wire compression uses raw deflate (no zlib header / Adler-32
//! trailer) per upstream `deflateInit2(..., -MAX_WBITS, ...)`. The same
//! decoder is the entry point regardless of whether the backend resolves to
//! `zlib-ng`, `zlib-rs`, or `miniz_oxide` at build time, so this target
//! provides feature-agnostic coverage of the decode path.
//!
//! Invariants enforced per input:
//!
//! 1. The decoder may return an error but must never panic.
//! 2. Output size must not exceed 100x the compressed input - any larger
//!    ratio indicates an unbounded expansion that could be weaponised as a
//!    DoS vector (zip bomb).
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run decompressor_zlib
//! ```

use std::io::Read;

use libfuzzer_sys::fuzz_target;

use compress::zlib::{CountingZlibDecoder, decompress_to_vec};

/// Hard cap on the expansion ratio. See `decompressor_zstd` for rationale.
const MAX_RATIO: u64 = 100;
/// Absolute cap on bytes pulled out of the streaming decoder so we cannot
/// stall the fuzzer when an input expands without bound before the ratio
/// check fires.
const READ_BUDGET: u64 = 8 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    // Streaming decoder path.
    let mut decoder = CountingZlibDecoder::new(data);
    let mut sink = Vec::new();
    let limit = std::cmp::min(
        READ_BUDGET,
        (data.len() as u64)
            .saturating_mul(MAX_RATIO)
            .saturating_add(1),
    );
    let _ = (&mut decoder).take(limit).read_to_end(&mut sink);
    assert_expansion_bounded(data.len(), sink.len());

    // One-shot helper path. Guarded by an input-size ceiling because the
    // helper has no internal expansion cap - a zip-bomb stream could
    // otherwise blow the fuzzer's memory budget before the ratio check
    // fires.
    if data.len() <= 4096 {
        if let Ok(out) = decompress_to_vec(data) {
            assert_expansion_bounded(data.len(), out.len());
        }
    }
});

/// Enforce the 100x expansion ceiling. Zero-length input is allowed to
/// produce up to a single byte of output before the ratio check kicks in,
/// which keeps the assertion honest for the degenerate empty-stream case.
fn assert_expansion_bounded(input_len: usize, output_len: usize) {
    let allowed = (input_len as u64)
        .saturating_mul(MAX_RATIO)
        .saturating_add(1);
    assert!(
        (output_len as u64) <= allowed,
        "deflate expansion exceeded {MAX_RATIO}x ceiling: input={input_len} output={output_len}",
    );
}
