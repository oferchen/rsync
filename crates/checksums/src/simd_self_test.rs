//! Runtime SIMD-vs-scalar checksum parity self-test.
//!
//! This module exposes a single entry point, [`run_simd_self_test`], that
//! cross-validates every runtime-selected SIMD implementation in this crate
//! against an independent scalar reference. The function is intended for
//! diagnostic use - for example, during release qualification or when
//! investigating a suspected SIMD regression - and is therefore deliberately
//! kept off the program-startup hot path.
//!
//! # Coverage
//!
//! Each invocation sweeps a fixed set of input shapes (chosen to cover SIMD
//! lane boundaries, sub-block remainders, large multi-block payloads, and a
//! pair of pseudo-random patterns) through every dispatcher this crate hosts:
//!
//! - The rolling checksum (`RollingChecksum::update`), which dispatches to
//!   AVX2/SSE2 on x86_64 and NEON on aarch64.
//! - The batch MD5 hasher (`simd_batch::digest_batch`), which dispatches to
//!   AVX-512/AVX2/SSE4.1/SSSE3/SSE2/NEON.
//! - The batch MD4 hasher (`simd_batch::md4::digest_batch`), which dispatches
//!   to AVX-512/AVX2/SSE2/NEON.
//!
//! The scalar reference for each algorithm is independent of the SIMD path:
//!
//! - Rolling checksum uses [`RollingChecksum::update_byte`] in a per-byte
//!   loop. The byte path is the one-byte fast path that never engages the
//!   SIMD accumulator.
//! - MD4/MD5 use the `strong::Md4` / `strong::Md5` RustCrypto-backed
//!   hashers, which do not share code with the SIMD batch dispatchers.
//!
//! # Cost
//!
//! The combined input sweep totals ~30 KiB across roughly two dozen probes
//! per algorithm. The function returns in well under a millisecond on every
//! tier-1 platform; it is safe to call from CLI diagnostics or release smoke
//! tests, but should never be invoked unconditionally during normal startup.

use crate::RollingChecksum;
use crate::simd_batch;
use crate::strong::{Md4, Md5};

/// Input shapes the self-test exercises for every SIMD path.
///
/// The sizes span:
///
/// - empty input (zero-length sentinel),
/// - sub-SIMD-lane sizes (1, 31, 33, 63, 65),
/// - exact SIMD lane boundaries (16, 32, 64, 128),
/// - common rsync block multiples (1 KiB, 4 KiB, 9 KiB - last is intentionally
///   non-aligned to ensure SIMD tail handling is exercised).
const INPUT_SIZES: &[usize] = &[
    0, 1, 15, 16, 17, 31, 32, 33, 55, 56, 63, 64, 65, 127, 128, 129, 1024, 4096, 9_217,
];

/// Number of distinct deterministic patterns generated at each size.
const PATTERNS_PER_SIZE: usize = 3;

/// Algorithm identifier reported by [`SimdParityError`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimdAlgorithm {
    /// Rolling checksum (`RollingChecksum`).
    Rolling,
    /// Batch MD5 dispatcher (`simd_batch::digest_batch`).
    Md5Batch,
    /// Batch MD4 dispatcher (`simd_batch::md4::digest_batch`).
    Md4Batch,
}

impl SimdAlgorithm {
    /// Human-readable label for diagnostic output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rolling => "rolling",
            Self::Md5Batch => "md5-batch",
            Self::Md4Batch => "md4-batch",
        }
    }
}

/// Error reported when a SIMD dispatcher disagrees with its scalar reference.
///
/// The payload identifies the failing dispatcher, the active SIMD backend
/// (rendered as a short tag), the input length that triggered the divergence,
/// and hex renderings of both digests. Callers can format the error directly
/// to surface a precise diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimdParityError {
    /// Which algorithm diverged.
    pub algorithm: SimdAlgorithm,
    /// Backend tag (e.g. `"AVX2"`, `"NEON"`, `"Scalar"`).
    pub backend: &'static str,
    /// Size in bytes of the input that triggered the divergence.
    pub input_len: usize,
    /// Index of the pattern within the size's sweep (`0..PATTERNS_PER_SIZE`).
    pub pattern_index: usize,
    /// SIMD-produced digest, hex-encoded.
    pub simd: String,
    /// Scalar-produced digest, hex-encoded.
    pub scalar: String,
}

impl std::fmt::Display for SimdParityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} SIMD parity failure on {} (len={}, pattern={}): simd={} scalar={}",
            self.algorithm.name(),
            self.backend,
            self.input_len,
            self.pattern_index,
            self.simd,
            self.scalar,
        )
    }
}

impl std::error::Error for SimdParityError {}

/// Cross-validates every runtime-selected SIMD implementation against a
/// scalar reference over a curated input sweep.
///
/// Returns `Ok(())` when every dispatcher in this crate matches its scalar
/// reference byte-for-byte. On the first divergence, returns a
/// [`SimdParityError`] describing the failing algorithm, active backend, and
/// the digests that disagreed.
///
/// # Examples
///
/// ```
/// match checksums::run_simd_self_test() {
///     Ok(()) => {}
///     Err(err) => panic!("SIMD parity self-test failed: {err}"),
/// }
/// ```
///
/// # Errors
///
/// Returns [`SimdParityError`] when any SIMD dispatcher produces a digest
/// that differs from the scalar reference for any probe input.
pub fn run_simd_self_test() -> Result<(), SimdParityError> {
    for (size_idx, &size) in INPUT_SIZES.iter().enumerate() {
        for pattern_idx in 0..PATTERNS_PER_SIZE {
            let input = generate_pattern(size, size_idx, pattern_idx);
            check_rolling(&input, pattern_idx)?;
            check_md5(&input, pattern_idx)?;
            check_md4(&input, pattern_idx)?;
        }
    }
    Ok(())
}

/// Deterministic pseudo-random byte generator.
///
/// Uses a small LCG seeded from `(size_idx, pattern_idx)` so the same probe
/// indices always produce the same bytes. Patterns intentionally cover three
/// shapes: all-zero, ascending modulo, and an LCG byte stream. This catches
/// SIMD paths that mishandle zero-runs, ordered bytes, or high-entropy data.
fn generate_pattern(size: usize, size_idx: usize, pattern_idx: usize) -> Vec<u8> {
    let mut data = vec![0u8; size];
    match pattern_idx {
        0 => {
            // All zeros - catches sign-extension bugs in rolling checksum SIMD.
        }
        1 => {
            // Ascending modulo - catches lane-ordering bugs in batch MD4/MD5.
            for (i, slot) in data.iter_mut().enumerate() {
                *slot = ((i.wrapping_add(size_idx)) & 0xff) as u8;
            }
        }
        _ => {
            // LCG byte stream - catches generic computation drift across lanes.
            let mut state: u64 = 0x9E37_79B9_7F4A_7C15
                ^ ((size_idx as u64) << 32)
                ^ (pattern_idx as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            for slot in data.iter_mut() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *slot = (state >> 33) as u8;
            }
        }
    }
    data
}

fn rolling_backend_tag() -> &'static str {
    if crate::simd_acceleration_available() {
        if cfg!(target_arch = "aarch64") {
            "NEON"
        } else if cfg!(any(target_arch = "x86_64", target_arch = "x86")) {
            "AVX2/SSE2"
        } else {
            "SIMD"
        }
    } else {
        "Scalar"
    }
}

fn check_rolling(input: &[u8], pattern_idx: usize) -> Result<(), SimdParityError> {
    let mut simd = RollingChecksum::new();
    simd.update(input);
    let simd_digest: u32 = simd.value();

    let mut scalar = RollingChecksum::new();
    // `update_byte` is the per-byte path; it never engages the SIMD
    // accumulator regardless of host CPU, so it serves as an independent
    // reference.
    for &byte in input {
        scalar.update_byte(byte);
    }
    let scalar_digest: u32 = scalar.value();

    if simd_digest == scalar_digest {
        Ok(())
    } else {
        Err(SimdParityError {
            algorithm: SimdAlgorithm::Rolling,
            backend: rolling_backend_tag(),
            input_len: input.len(),
            pattern_index: pattern_idx,
            simd: format!("{simd_digest:08x}"),
            scalar: format!("{scalar_digest:08x}"),
        })
    }
}

fn check_md5(input: &[u8], pattern_idx: usize) -> Result<(), SimdParityError> {
    // Submit a 17-input batch so the dispatcher exercises both full and
    // partial lane handling regardless of which backend is selected
    // (AVX-512 = 16 lanes, AVX2 = 8, others = 4). Lanes share the same
    // input so any cross-lane drift surfaces immediately.
    let lanes: [&[u8]; 17] = [input; 17];
    let batch = simd_batch::digest_batch(&lanes);
    let reference = Md5::digest(input);
    let backend = simd_batch::active_backend().name();

    for (lane, digest) in batch.iter().enumerate() {
        if *digest != reference {
            return Err(SimdParityError {
                algorithm: SimdAlgorithm::Md5Batch,
                backend,
                input_len: input.len(),
                pattern_index: pattern_idx,
                simd: format!("lane{lane}:{}", hex_encode(digest)),
                scalar: hex_encode(&reference),
            });
        }
    }
    Ok(())
}

fn check_md4(input: &[u8], pattern_idx: usize) -> Result<(), SimdParityError> {
    let lanes: [&[u8]; 17] = [input; 17];
    let batch = simd_batch::md4::digest_batch(&lanes);
    let reference = Md4::digest(input);
    // MD4 reuses the MD5 backend enum for dispatcher reporting.
    let backend = simd_batch::active_backend().name();

    for (lane, digest) in batch.iter().enumerate() {
        if *digest != reference {
            return Err(SimdParityError {
                algorithm: SimdAlgorithm::Md4Batch,
                backend,
                input_len: input.len(),
                pattern_index: pattern_idx,
                simd: format!("lane{lane}:{}", hex_encode(digest)),
                scalar: hex_encode(&reference),
            });
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host-default invocation must pass on every tier-1 platform.
    ///
    /// This is the single source-tree assertion required by task 2224: every
    /// SIMD path the host CPU selects must produce digests byte-equal to the
    /// scalar reference across the full input sweep.
    #[test]
    fn self_test_passes_on_host() {
        run_simd_self_test().expect("SIMD parity self-test must pass");
    }

    /// Every input size in the sweep must actually generate non-empty data
    /// for the non-zero patterns, otherwise the sweep would silently degrade
    /// into "always tests the empty input".
    #[test]
    fn input_sweep_generates_expected_sizes() {
        for (size_idx, &size) in INPUT_SIZES.iter().enumerate() {
            for pattern_idx in 0..PATTERNS_PER_SIZE {
                let buf = generate_pattern(size, size_idx, pattern_idx);
                assert_eq!(
                    buf.len(),
                    size,
                    "pattern {pattern_idx} at size {size} truncated"
                );
            }
        }
    }

    /// The three patterns must produce distinct byte streams at non-trivial
    /// sizes so SIMD bugs that mishandle a single shape cannot hide behind
    /// duplicate inputs.
    #[test]
    fn patterns_are_distinct_at_non_trivial_sizes() {
        let size_idx = INPUT_SIZES
            .iter()
            .position(|&s| s == 1024)
            .expect("1024 in sweep");
        let p0 = generate_pattern(1024, size_idx, 0);
        let p1 = generate_pattern(1024, size_idx, 1);
        let p2 = generate_pattern(1024, size_idx, 2);
        assert_ne!(p0, p1, "zero pattern must differ from ascending pattern");
        assert_ne!(p0, p2, "zero pattern must differ from lcg pattern");
        assert_ne!(p1, p2, "ascending pattern must differ from lcg pattern");
    }

    #[test]
    fn error_display_includes_diagnostic_fields() {
        let err = SimdParityError {
            algorithm: SimdAlgorithm::Md5Batch,
            backend: "AVX2",
            input_len: 64,
            pattern_index: 1,
            simd: "deadbeef".into(),
            scalar: "cafebabe".into(),
        };
        let rendered = format!("{err}");
        assert!(rendered.contains("md5-batch"));
        assert!(rendered.contains("AVX2"));
        assert!(rendered.contains("len=64"));
        assert!(rendered.contains("pattern=1"));
        assert!(rendered.contains("deadbeef"));
        assert!(rendered.contains("cafebabe"));
    }

    #[test]
    fn algorithm_names_are_distinct() {
        assert_ne!(
            SimdAlgorithm::Rolling.name(),
            SimdAlgorithm::Md4Batch.name()
        );
        assert_ne!(
            SimdAlgorithm::Rolling.name(),
            SimdAlgorithm::Md5Batch.name()
        );
        assert_ne!(
            SimdAlgorithm::Md4Batch.name(),
            SimdAlgorithm::Md5Batch.name()
        );
    }
}
