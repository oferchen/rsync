//! Property-based parity tests for the rolling checksum.
//!
//! These tests lock down the invariant that the public [`RollingChecksum`]
//! API - which dispatches to AVX2/SSE2/NEON on supported hosts and otherwise
//! falls back to a 4-byte unrolled scalar loop - produces byte-for-byte
//! identical output to a self-contained scalar reference implementation
//! derived directly from upstream rsync's `get_checksum1()`.
//!
//! By executing the same property suite on every CI matrix entry (Linux x86_64
//! with AVX2/SSE2, macOS aarch64 with NEON, Windows x86_64), regressions in any
//! one SIMD path - or in the scalar fallback - surface as a parity failure on
//! the relevant runner.
//!
//! # Upstream Reference
//!
//! - `checksum.c:get_checksum1()` - rolling checksum core (CHAR_OFFSET = 0)
//! - `match.c:hash_search()` - sliding-window consumer
//!
//! The rsync formula over a block of length `n` is:
//!
//! ```text
//! s1 = sum of bytes
//! s2 = sum of (n - i) * bytes[i]   = sum of prefix sums
//! value = (s2 << 16) | s1          (both terms masked to 16 bits)
//! ```
//!
//! Rolling update (remove `out`, add `in`, window length stays `n`):
//!
//! ```text
//! s1 = (s1 - out + in) & 0xFFFF
//! s2 = (s2 - n * out + s1) & 0xFFFF
//! ```

use checksums::{RollingChecksum, RollingDigest, simd_acceleration_available};
use proptest::prelude::*;

/// Upstream-faithful scalar reference. Computes `s1`, `s2` exactly the way
/// `checksum.c:get_checksum1()` does for a fresh window: byte-by-byte
/// accumulation with 16-bit truncation only at the end.
fn reference_digest(data: &[u8]) -> RollingDigest {
    let mut s1: u64 = 0;
    let mut s2: u64 = 0;
    for &byte in data {
        s1 = s1.wrapping_add(u64::from(byte));
        s2 = s2.wrapping_add(s1);
    }
    RollingDigest::new((s1 & 0xffff) as u16, (s2 & 0xffff) as u16, data.len())
}

/// Upstream-faithful scalar rolling update over a sliding window of length
/// `window`. Returns the digest after every step so tests can assert parity at
/// every offset, not just at the end.
///
/// Mirrors the `s1 = s1 - out + in; s2 = s2 - n*out + s1` recurrence from
/// `match.c:hash_search()`.
fn reference_rolling_digests(data: &[u8], window: usize) -> Vec<RollingDigest> {
    assert!(window > 0);
    assert!(window <= data.len());

    let mut s1: u32 = 0;
    let mut s2: u32 = 0;
    for &byte in &data[..window] {
        s1 = s1.wrapping_add(u32::from(byte));
        s2 = s2.wrapping_add(s1);
    }
    s1 &= 0xffff;
    s2 &= 0xffff;

    let mut out = Vec::with_capacity(data.len() - window + 1);
    out.push(RollingDigest::new(s1 as u16, s2 as u16, window));

    let n = window as u32;
    for start in 1..=data.len() - window {
        let outgoing = u32::from(data[start - 1]);
        let incoming = u32::from(data[start + window - 1]);

        s1 = s1.wrapping_sub(outgoing).wrapping_add(incoming) & 0xffff;
        s2 = s2.wrapping_sub(n.wrapping_mul(outgoing)).wrapping_add(s1) & 0xffff;

        out.push(RollingDigest::new(s1 as u16, s2 as u16, window));
    }
    out
}

/// Bytes strategy. Length 0..=8192 covers: empty input, single byte,
/// sub-AVX2-block (<32), exact AVX2 block (32), exact SSE2/NEON block (16),
/// AVX2-block + tail (33..63), multi-block bulk (64, 128, 4096), and the
/// max range stipulated by the task (8192). Random bytes maximise coverage
/// of the prefix-sum weights inside each SIMD lane.
fn bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=8192)
}

/// Sliding-window strategy. Picks a non-empty data slice (so the window is
/// well-defined) and a window length in `1..=data.len()`. Window sizes are
/// clamped to <= 1024 to keep the per-position step count bounded under
/// `proptest`'s default 256-case budget.
fn data_and_window_strategy() -> impl Strategy<Value = (Vec<u8>, usize)> {
    prop::collection::vec(any::<u8>(), 1..=2048).prop_flat_map(|data| {
        let max_window = data.len().min(1024);
        let len = data.len();
        (Just(data), 1usize..=max_window).prop_map(move |(data, window)| {
            // Window cannot exceed data length even after the strategy maps.
            assert!(window <= len);
            (data, window)
        })
    })
}

proptest! {
    /// Bulk update on the public API must match the upstream-faithful scalar
    /// reference for any input length in [0, 8192]. On AVX2/SSE2/NEON hosts
    /// this exercises the SIMD dispatch ladder; on hosts without SIMD it
    /// guards the scalar fallback.
    #[test]
    fn rolling_update_matches_reference(data in bytes_strategy()) {
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        let expected = reference_digest(&data);
        prop_assert_eq!(checksum.digest(), expected);
        prop_assert_eq!(
            checksum.value(),
            (u32::from(expected.sum2()) << 16) | u32::from(expected.sum1()),
        );
        prop_assert_eq!(checksum.len(), data.len());
    }

    /// Splitting the same input into multiple `update()` calls must produce
    /// the same digest as a single bulk call. This catches SIMD paths that
    /// silently drop or duplicate state at the boundary between calls
    /// (e.g. a partial 32-byte AVX2 block at the end of one call and the
    /// start of the next).
    #[test]
    fn split_update_matches_single_pass(
        data in bytes_strategy(),
        split in 0usize..=8192,
    ) {
        let split = split.min(data.len());

        let mut single = RollingChecksum::new();
        single.update(&data);

        let mut chunked = RollingChecksum::new();
        chunked.update(&data[..split]);
        chunked.update(&data[split..]);

        prop_assert_eq!(chunked.digest(), single.digest());
        prop_assert_eq!(chunked.value(), single.value());
    }

    /// Sliding window: roll across the input one byte at a time and verify
    /// the digest agrees with both (a) the upstream scalar recurrence and
    /// (b) a fresh recomputation over the same window. Catches drift in
    /// `roll()` independently from drift in `update()`.
    #[test]
    fn roll_one_byte_at_a_time_matches_reference(
        (data, window) in data_and_window_strategy(),
    ) {
        let expected = reference_rolling_digests(&data, window);

        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);
        prop_assert_eq!(rolling.digest(), expected[0]);

        for (idx, expected_digest) in expected.iter().enumerate().skip(1) {
            let outgoing = data[idx - 1];
            let incoming = data[idx + window - 1];
            rolling
                .roll(outgoing, incoming)
                .expect("roll on non-empty window must succeed");

            prop_assert_eq!(rolling.digest(), *expected_digest);

            let mut fresh = RollingChecksum::new();
            fresh.update(&data[idx..idx + window]);
            prop_assert_eq!(rolling.digest(), fresh.digest());
            prop_assert_eq!(rolling.value(), fresh.value());
        }
    }

    /// Round-trip: a digest captured after an arbitrary update must restore
    /// to a checksum that produces the same value. Locks down the
    /// `from_digest` / `digest()` pair so SIMD state changes never leak
    /// information that the digest alone cannot represent.
    #[test]
    fn digest_round_trip(data in bytes_strategy()) {
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        let digest = checksum.digest();
        let restored = RollingChecksum::from_digest(digest);

        prop_assert_eq!(restored.digest(), digest);
        prop_assert_eq!(restored.value(), checksum.value());
        prop_assert_eq!(restored.len(), checksum.len());
    }

    /// Single-byte path must agree with the slice path. Exercises the
    /// `update_byte` fast path used by the generator when building the
    /// initial sliding window.
    #[test]
    fn update_byte_matches_slice_update(data in bytes_strategy()) {
        let mut by_byte = RollingChecksum::new();
        for &byte in &data {
            by_byte.update_byte(byte);
        }

        let mut by_slice = RollingChecksum::new();
        by_slice.update(&data);

        prop_assert_eq!(by_byte.digest(), by_slice.digest());
        prop_assert_eq!(by_byte.value(), by_slice.value());
    }
}

/// Static smoke test: tickle the runtime feature detector at least once so
/// the parity property tests above run under whichever SIMD path the host
/// advertises. The boolean is informational - it is a hard error neither
/// way - but exercising the detector ensures it is initialised before the
/// proptest cases (which spawn many threads under shrinking) hit it.
#[test]
fn simd_detector_is_invocable() {
    let _ = simd_acceleration_available();
}

/// Boundary sizes that are notorious for SIMD off-by-one errors. Even though
/// the proptest above samples them probabilistically, asserting them as a
/// fixed regression set guarantees coverage on every CI run regardless of
/// proptest seed selection.
#[test]
fn boundary_sizes_match_reference() {
    // 0,1: empty + tiny. 15,16,17: SSE2/NEON block boundary +/- 1.
    // 31,32,33: AVX2 block boundary +/- 1. 63,64,65: two-AVX2-block boundary.
    // 4096: large bulk. 8192: max strategy size.
    let sizes = [
        0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 4096, 8192,
    ];

    for &size in &sizes {
        // Deterministic non-trivial pattern: every byte differs and the
        // pattern hits every byte value within the first 256 positions, so
        // the prefix-sum weights inside SIMD lanes get exercised.
        let data: Vec<u8> = (0..size)
            .map(|i| ((i as u32).wrapping_mul(2654435761) >> 24) as u8)
            .collect();

        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        assert_eq!(
            checksum.digest(),
            reference_digest(&data),
            "parity failure at size {size}",
        );
    }
}
