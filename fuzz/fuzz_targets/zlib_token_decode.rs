#![no_main]

//! Fuzz target for the per-token streaming raw-deflate decoder.
//!
//! Complements `fuzz/fuzz_targets/decompressor_zlib.rs`: that target drives
//! the one-shot `decompress_to_vec` helper, while this target exercises the
//! `CountingZlibDecoder` streaming path under adversarial read sizes and
//! malformed frame boundaries (truncated DEFLATED_DATA blocks, half-written
//! sync markers, arbitrary flag-byte combinations).
//!
//! Invariants enforced per input:
//!
//! 1. The decoder may return any `io::Result` value but MUST NOT panic
//!    across COPY/INSERT/MAP_END token combinations expressed in the
//!    fuzzed byte stream.
//! 2. The bytes-read counter never wraps. `bytes_read` is `u64` and the
//!    decoder uses `saturating_add`, so a malformed stream that exhausts
//!    the underlying reader must not produce a counter value greater than
//!    the actual output written into the caller-supplied buffer.
//! 3. Output never exceeds the 100x expansion ceiling enforced by the
//!    sibling `decompressor_zlib` target. Mirrors the same zip-bomb cap.
//!
//! No new public APIs are introduced by this target - it only consumes
//! existing entry points (`CountingZlibDecoder::new`, `Read::read`,
//! `Read::read_to_end`, `bytes_read`).
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run zlib_token_decode
//! ```

use std::io::Read;

use libfuzzer_sys::fuzz_target;

use compress::zlib::CountingZlibDecoder;

/// Hard cap on the expansion ratio. See `decompressor_zlib` for rationale.
const MAX_RATIO: u64 = 100;

/// Absolute cap on bytes pulled out of the streaming decoder so the fuzzer
/// cannot stall when an input expands without bound before the ratio check
/// fires.
const READ_BUDGET: u64 = 8 * 1024 * 1024;

/// Minimum number of bytes to allocate as a streaming read chunk. Drives
/// the streaming decoder through a per-call read loop so partial frames
/// surface as `Err` rather than as a panic from the underlying inflate
/// engine.
const CHUNK_SIZE: usize = 4096;

fuzz_target!(|data: &[u8]| {
    // Path A: streaming reads through fixed-size chunks.
    //
    // The streaming decoder must accept arbitrary `read()` granularity.
    // Mirrors the receiver pipeline that pulls per-token literals out of
    // a wire stream where DEFLATED_DATA frames can split mid-block.
    let mut decoder = CountingZlibDecoder::new(data);
    let mut chunk = [0u8; CHUNK_SIZE];
    let mut total_written: u64 = 0;
    let mut last_counter: u64 = decoder.bytes_read();
    loop {
        let cap = std::cmp::min(
            READ_BUDGET.saturating_sub(total_written),
            chunk.len() as u64,
        );
        if cap == 0 {
            break;
        }
        match decoder.read(&mut chunk[..cap as usize]) {
            Ok(0) => break,
            Ok(n) => {
                total_written = total_written.saturating_add(n as u64);
                let counter = decoder.bytes_read();
                // Counter must move forward with each successful read.
                assert!(
                    counter >= last_counter,
                    "bytes_read regressed: {counter} < {last_counter}",
                );
                assert!(
                    counter <= total_written,
                    "bytes_read {counter} exceeds bytes written {total_written}",
                );
                last_counter = counter;
            }
            Err(_) => break,
        }
    }
    assert_expansion_bounded(data.len(), total_written as usize);

    // Path B: one-shot read_to_end through the streaming decoder, capped
    // by `take(READ_BUDGET)`. Exercises the inflate engine's
    // chunked-state path under maximum-size buffer growth pressure.
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
});

/// Enforce the 100x expansion ceiling. Zero-length input is allowed to
/// produce up to a single byte of output before the ratio check kicks in,
/// matching the sibling `decompressor_zlib` target.
fn assert_expansion_bounded(input_len: usize, output_len: usize) {
    let allowed = (input_len as u64)
        .saturating_mul(MAX_RATIO)
        .saturating_add(1);
    assert!(
        (output_len as u64) <= allowed,
        "deflate expansion exceeded {MAX_RATIO}x ceiling: input={input_len} output={output_len}",
    );
}
