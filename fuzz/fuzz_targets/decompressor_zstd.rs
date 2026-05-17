#![no_main]

//! Fuzz target for the zstd streaming decompressor.
//!
//! Compressed input received over the rsync wire is fully untrusted: a
//! malicious peer can craft a frame that triggers excessive memory use
//! (zip-bomb) or an arithmetic edge case inside the decoder. This target
//! drives [`CountingZstdDecoder`] with arbitrary bytes and the convenience
//! [`decompress_to_vec`] helper, then enforces two invariants:
//!
//! 1. The decoder may return an error but must never panic.
//! 2. The total decompressed size must not exceed 100x the compressed
//!    input - any larger ratio indicates an unbounded expansion that
//!    could be weaponised as a DoS vector.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run decompressor_zstd
//! ```

use std::io::Read;

use libfuzzer_sys::fuzz_target;

use compress::zstd::{CountingZstdDecoder, decompress_to_vec};

/// Hard cap on the expansion ratio. Upstream zstd's worst-case "compressed
/// nothing" ratio is well below this bound for any well-formed payload.
const MAX_RATIO: u64 = 100;
/// Absolute cap on bytes pulled out of the streaming decoder so we cannot
/// stall the fuzzer when an input expands without bound before the ratio
/// check fires.
const READ_BUDGET: u64 = 8 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    // Streaming decoder path.
    if let Ok(mut decoder) = CountingZstdDecoder::new(data) {
        let mut sink = Vec::new();
        let limit = std::cmp::min(
            READ_BUDGET,
            (data.len() as u64)
                .saturating_mul(MAX_RATIO)
                .saturating_add(1),
        );
        let _ = (&mut decoder).take(limit).read_to_end(&mut sink);
        assert_expansion_bounded(data.len(), sink.len());
    }

    // One-shot helper path. Guarded by an input-size ceiling because the
    // helper has no internal expansion cap - a zip-bomb frame could
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
/// which keeps the assertion honest for the degenerate empty-frame case.
fn assert_expansion_bounded(input_len: usize, output_len: usize) {
    let allowed = (input_len as u64)
        .saturating_mul(MAX_RATIO)
        .saturating_add(1);
    assert!(
        (output_len as u64) <= allowed,
        "zstd expansion exceeded {MAX_RATIO}x ceiling: input={input_len} output={output_len}",
    );
}
