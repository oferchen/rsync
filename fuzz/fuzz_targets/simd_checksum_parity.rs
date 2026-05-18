#![no_main]

//! SIMD vs scalar parity fuzz target for rolling and strong checksums.
//!
//! Feeds arbitrary bytes from libFuzzer into every runtime-selected SIMD
//! checksum dispatcher and compares the result against an independent scalar
//! reference. A panic - the signal libFuzzer treats as a finding - is raised
//! whenever the two disagree by a single byte.
//!
//! The on-tree self-test (`checksums::run_simd_self_test`) sweeps a fixed set
//! of input shapes. This target replaces that fixed sweep with coverage-guided
//! arbitrary inputs so libFuzzer can hunt for shapes the sweep does not cover
//! (odd remainders past SIMD lane boundaries, repeated byte patterns,
//! adversarial transitions across MD4/MD5 block boundaries, etc.).
//!
//! Dispatchers exercised:
//!
//! - [`checksums::RollingChecksum`] - AVX2/SSE2 on x86_64, NEON on aarch64,
//!   scalar fallback elsewhere. Reference is the per-byte `update_byte` path,
//!   which never engages the SIMD accumulator.
//! - [`checksums::strong::md5_digest_batch`] - AVX-512/AVX2/SSE4.1/SSSE3/SSE2
//!   on x86_64, NEON on aarch64, scalar fallback elsewhere. Reference is the
//!   RustCrypto-backed [`checksums::strong::Md5`].
//! - [`checksums::strong::md4_digest_batch`] - same backend matrix as MD5.
//!   Reference is the RustCrypto-backed [`checksums::strong::Md4`].
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run simd_checksum_parity
//! cargo +nightly fuzz run simd_checksum_parity -- -max_total_time=60
//! ```

use libfuzzer_sys::fuzz_target;

use checksums::RollingChecksum;
use checksums::strong::{Md4, Md5, md4_digest_batch, md5_digest_batch};

fuzz_target!(|data: &[u8]| {
    check_rolling(data);
    check_strong_batch(data);
});

/// Drive the SIMD bulk-update path and the per-byte scalar path with the same
/// bytes and assert they agree.
///
/// `update_byte` is the one-byte fast path that never engages the SIMD
/// accumulator regardless of host CPU, so it serves as an independent
/// reference for the bulk `update` dispatcher.
fn check_rolling(data: &[u8]) {
    let mut simd = RollingChecksum::new();
    simd.update(data);
    let simd_value = simd.value();

    let mut scalar = RollingChecksum::new();
    for &byte in data {
        scalar.update_byte(byte);
    }
    let scalar_value = scalar.value();

    assert_eq!(
        simd_value,
        scalar_value,
        "rolling SIMD/scalar parity violation at len={}: simd={:08x} scalar={:08x}",
        data.len(),
        simd_value,
        scalar_value,
    );
}

/// Submit a multi-lane batch to the SIMD MD4/MD5 dispatchers and assert every
/// lane matches the RustCrypto reference.
///
/// The batch contains 17 copies of the same input so the dispatcher exercises
/// both full and partial lane handling regardless of the active backend
/// (AVX-512 = 16 lanes, AVX2 = 8 lanes, SSE2/NEON = 4 lanes). Lanes share the
/// input so any cross-lane drift surfaces as a mismatch on at least one lane.
fn check_strong_batch(data: &[u8]) {
    let lanes: [&[u8]; 17] = [data; 17];

    let md5_batch = md5_digest_batch(&lanes);
    let md5_reference = Md5::digest(data);
    for (lane, digest) in md5_batch.iter().enumerate() {
        assert_eq!(
            *digest,
            md5_reference,
            "MD5 SIMD/scalar parity violation on lane {lane} (len={})",
            data.len(),
        );
    }

    let md4_batch = md4_digest_batch(&lanes);
    let md4_reference = Md4::digest(data);
    for (lane, digest) in md4_batch.iter().enumerate() {
        assert_eq!(
            *digest,
            md4_reference,
            "MD4 SIMD/scalar parity violation on lane {lane} (len={})",
            data.len(),
        );
    }
}
