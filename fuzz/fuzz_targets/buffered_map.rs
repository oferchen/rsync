#![no_main]

//! Fuzz target for `transfer::map_file::BufferedMap`.
//!
//! Locks in the regression coverage for the UTS-18.f / UTS-18.g panic class
//! addressed by PR #5566 (fail-loud `copy_within` + slice guards) and
//! PR #5569 (`window_len` clamp to remaining file bytes in `load_window`).
//!
//! The two panic sites both lived inside `BufferedMap`:
//!
//! 1. `load_window` -> `self.buffer.copy_within(src_offset..src_offset + reuse_len, 0)`
//!    aborted the process with
//!    `range end index 262144 out of range for slice of length 48128`
//!    when a malformed delta drove the overlap-reuse branch against a
//!    resized buffer that could not hold the prior window's claimed extent.
//! 2. `map_ptr` -> `&self.buffer[start..end]` carried the same panic surface
//!    whenever the cached `window_len` lied about buffer extent.
//!
//! # Invariants enforced per input
//!
//! For every (file_size, window_size, request_start, request_len) tuple
//! synthesised from the fuzzer input, both `load_window` (driven through
//! `map_ptr`) and direct `map_ptr` calls MUST return `Ok` or `Err` - never
//! panic. A panic here is an automatic finding.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run buffered_map
//! ```

use std::io::Write;

use libfuzzer_sys::fuzz_target;
use tempfile::NamedTempFile;

use transfer::map_file::{BufferedMap, MapStrategy};

/// Cap synthesised file sizes so the fuzzer does not exhaust tempfs while
/// still spanning the production crash ratio (file <= MAX_MAP_SIZE).
const MAX_FUZZ_FILE_SIZE: u64 = 512 * 1024;
/// Cap the buffered-window allocation so a single fuzz input cannot stall
/// the corpus by demanding hundreds of MiB.
const MAX_FUZZ_WINDOW: usize = 512 * 1024;
/// Cap the requested slice length so a single `map_ptr` call cannot dominate
/// the fuzzer budget.
const MAX_FUZZ_REQUEST: u64 = 512 * 1024;

fuzz_target!(|data: &[u8]| {
    // The header packs the BufferedMap state we want to exercise:
    //   bytes  0..8   : file_size      (u64 LE, capped)
    //   bytes  8..16  : window_size    (u64 LE, capped + non-zero)
    //   bytes 16..24  : request_start  (u64 LE, capped)
    //   bytes 24..32  : request_len    (u64 LE, capped)
    //   bytes 32..40  : second_start   (u64 LE, capped) - drives the
    //                   overlap-shrink branch of `load_window`
    //   bytes 40..48  : second_len     (u64 LE, capped)
    //   bytes 48..    : file payload (truncated/padded to file_size)
    if data.len() < 48 {
        return;
    }

    let file_size = u64::from_le_bytes(data[0..8].try_into().unwrap()) % (MAX_FUZZ_FILE_SIZE + 1);
    let window_size_raw =
        u64::from_le_bytes(data[8..16].try_into().unwrap()) % (MAX_FUZZ_WINDOW as u64 + 1);
    // BufferedMap rejects a zero window via downstream divisions; keep the
    // window at least 1 byte to stay on the legitimate-input path.
    let window_size = (window_size_raw as usize).max(1);
    let request_start =
        u64::from_le_bytes(data[16..24].try_into().unwrap()) % (MAX_FUZZ_FILE_SIZE + 1);
    let request_len = u64::from_le_bytes(data[24..32].try_into().unwrap()) % (MAX_FUZZ_REQUEST + 1);
    let second_start =
        u64::from_le_bytes(data[32..40].try_into().unwrap()) % (MAX_FUZZ_FILE_SIZE + 1);
    let second_len = u64::from_le_bytes(data[40..48].try_into().unwrap()) % (MAX_FUZZ_REQUEST + 1);

    let payload_seed = &data[48..];

    // Build a backing file of exactly `file_size` bytes by tiling the
    // payload bytes (or zero-filling when the seed is empty).
    let payload = build_payload(payload_seed, file_size as usize);

    let mut tmp = match NamedTempFile::new() {
        Ok(t) => t,
        Err(_) => return,
    };
    if tmp.write_all(&payload).is_err() {
        return;
    }
    if tmp.flush().is_err() {
        return;
    }

    let mut map = match BufferedMap::open_with_window(tmp.path(), window_size) {
        Ok(m) => m,
        Err(_) => return,
    };

    // First request - drives `load_window` from the empty-cache state. The
    // production crash math (file=48128, MAX_MAP_SIZE=262144) is hit
    // whenever `request_start + request_len` reaches the cached-window
    // edge near EOF.
    let _ = MapStrategy::map_ptr(&mut map, request_start, request_len as usize);

    // Second request - drives the forward-overlap branch of `load_window`
    // when `second_start` falls inside the prior window, which is the path
    // that historically panicked via `copy_within`.
    let _ = MapStrategy::map_ptr(&mut map, second_start, second_len as usize);
});

/// Tile `seed` (or zero-fill on empty seed) to exactly `len` bytes. Keeping
/// payloads deterministic from the fuzz input gives libFuzzer a stable
/// edge-cov signal across mutations.
fn build_payload(seed: &[u8], len: usize) -> Vec<u8> {
    if len == 0 {
        return Vec::new();
    }
    if seed.is_empty() {
        return vec![0u8; len];
    }
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let remaining = len - out.len();
        let take = remaining.min(seed.len());
        out.extend_from_slice(&seed[..take]);
    }
    out
}
