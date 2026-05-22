//! PIP-9.c regression test: parallel-threshold-trip with sha256 byte-identity assertion.
//!
//! Reintroduces the `parallel-threshold-trip` interop scenario removed by the
//! revert that mitigated the PIP-4 (#4720) receiver-corruption surfaced when
//! the source tree crossed the historical 100-file dispatch threshold.
//!
//! Background:
//! - PIP-4 added an interop scenario with 120 tiny files under
//!   `parallel_threshold/file_N.txt` that crossed the receiver's parallel
//!   dispatch threshold for the first time.
//! - The scenario surfaced receiver-side corruption: under `dist`-profile
//!   builds (LTO + panic=abort) with `parallel-receive-delta` enabled,
//!   `parallel_threshold/file_1.txt` ended up with wrong bytes on the
//!   destination side.
//! - The revert that mitigated PIP-7 (#4730 follow-up) removed the scenario
//!   from `tools/ci/run_interop.sh`. PIP-8 (#4731) then tore out the dead
//!   dispatch scaffolding. PIP-9 (design doc
//!   `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md`) is the
//!   plan for the proper wire-up; PIP-9.c is this regression test so a future
//!   wire-up cannot reopen the corruption silently.
//!
//! Today the sequential receiver path is correct, so this test passes on
//! master HEAD. Once PIP-9.b lands the parallel applier production wire-up,
//! the same test catches a regression of the original corruption.
//!
//! Test shape (mirrors the deleted scenario from commit 531e88d97 / PR #4725):
//! - 120 files named `parallel_threshold/file_N.txt`, N = 1..=120.
//! - Each file is 16 KiB of deterministic pseudo-random bytes derived from a
//!   xorshift64* PRNG seeded from a fixed constant plus the file index.
//! - Total payload is ~1.92 MiB, well above the historical 100-file
//!   dispatch threshold and well below the 64 MiB total-bytes cutoff.
//! - Transfer runs in local-mode (no daemon, no SSH) - direct local-copy
//!   stresses the same receiver per-file write path that the parallel
//!   dispatch corrupted.
//! - After transfer, every source file's SHA-256 is compared byte-for-byte
//!   against its destination counterpart. `file_1.txt` is asserted
//!   explicitly to flag the historical regression first.

mod integration;

use integration::helpers::{RsyncCommand, TestDir};

use checksums::strong::Sha256;
use std::fs;
use std::path::Path;

/// Number of files in the parallel-threshold scenario.
///
/// 120 was chosen to comfortably exceed the historical
/// `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD` of 100 (removed in PIP-8) while
/// keeping the total payload tiny enough for CI.
const FILE_COUNT: usize = 120;

/// Per-file size in bytes (16 KiB).
///
/// Large enough that the delta pipeline produces multiple real chunks per
/// file; small enough that 120 files transfer in well under 30 seconds.
const FILE_SIZE: usize = 16 * 1024;

/// Deterministic PRNG seed. Documented here so test reproductions across
/// hosts always produce the same source bytes and the same expected digests.
const PRNG_SEED: u64 = 0x0C_71_5C_C9_E4_DA_3F_2A;

/// xorshift64*: minimal seeded PRNG with no external crate dependency.
///
/// Adequate for generating deterministic test payloads; not a cryptographic
/// generator. Same construction as `tests/inc_recurse_sender_fuzz_1863.rs`.
struct Rng {
    state: u64,
}

impl Rng {
    fn from_seed(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        let len = buf.len();
        let mut i = 0;
        while i + 8 <= len {
            let v = self.next_u64().to_le_bytes();
            buf[i..i + 8].copy_from_slice(&v);
            i += 8;
        }
        if i < len {
            let v = self.next_u64().to_le_bytes();
            buf[i..].copy_from_slice(&v[..len - i]);
        }
    }
}

/// Produce the deterministic payload for file `index` (1-based).
///
/// Seed is `PRNG_SEED ^ (index as u64)` so every file has distinct content
/// but the test is fully reproducible.
fn payload_for(index: usize) -> Vec<u8> {
    let mut rng = Rng::from_seed(PRNG_SEED ^ (index as u64));
    let mut buf = vec![0u8; FILE_SIZE];
    rng.fill_bytes(&mut buf);
    buf
}

/// Compute SHA-256 of an entire file on disk.
fn sha256_file(path: &Path) -> [u8; 32] {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Sha256::digest(&bytes)
}

/// Format a SHA-256 digest as a 64-char lowercase hex string for assertion messages.
fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Return the offset of the first differing byte between two equal-length
/// slices, or `None` if they match.
fn first_byte_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}

/// PIP-9.c regression: byte-identical local-mode transfer for the
/// parallel-threshold-trip scenario.
///
/// The test must pass on master today (sequential receiver is correct). It
/// is designed to catch a regression of the PIP-4 receiver corruption once
/// PIP-9.b wires the parallel applier into the production path.
///
/// Name starts with `parallel_threshold_` so the upcoming CI cell from
/// PIP-9.d can filter via `nextest run -E 'test(parallel_threshold)'`.
#[test]
fn parallel_threshold_trip_local_mode_byte_identity() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").expect("create src dir");
    let dest_dir = test_dir.mkdir("dest").expect("create dest dir");

    let scenario_dir = src_dir.join("parallel_threshold");
    fs::create_dir_all(&scenario_dir).expect("create scenario dir");

    // Materialise 120 files of 16 KiB each, deterministically seeded.
    for i in 1..=FILE_COUNT {
        let payload = payload_for(i);
        let path = scenario_dir.join(format!("file_{i}.txt"));
        fs::write(&path, &payload).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }

    // Local-mode recursive transfer. Using -a for the standard archive
    // semantics, matching what the deleted scenario used (`-av`). Verbosity
    // omitted to keep test stdout small; assert_success surfaces stderr on
    // failure.
    let mut cmd = RsyncCommand::new();
    let src_arg = format!("{}/", src_dir.display());
    cmd.args(["-a", &src_arg, dest_dir.to_str().unwrap()]);
    cmd.assert_success();

    let dest_scenario = dest_dir.join("parallel_threshold");
    assert!(
        dest_scenario.is_dir(),
        "destination parallel_threshold/ directory missing: {}",
        dest_scenario.display()
    );

    // Assert file_1.txt first. This is the file PIP-4 surfaced corruption on
    // (the first file dispatched after the parallel threshold tripped); a
    // future regression of that bug must light up here unambiguously, not
    // get buried under an "any-file mismatched" summary.
    let src_file_1 = scenario_dir.join("file_1.txt");
    let dest_file_1 = dest_scenario.join("file_1.txt");
    assert!(
        dest_file_1.is_file(),
        "destination file_1.txt missing: {}",
        dest_file_1.display()
    );
    let src_1_digest = sha256_file(&src_file_1);
    let dest_1_digest = sha256_file(&dest_file_1);
    if src_1_digest != dest_1_digest {
        let src_bytes = fs::read(&src_file_1).expect("re-read src file_1");
        let dest_bytes = fs::read(&dest_file_1).expect("re-read dest file_1");
        let first_diff = first_byte_diff(&src_bytes, &dest_bytes);
        panic!(
            "PIP-9.c regression: file_1.txt sha256 mismatch\n  \
             src sha256:  {}\n  \
             dest sha256: {}\n  \
             src len:  {}\n  \
             dest len: {}\n  \
             first differing byte offset: {:?}",
            hex(&src_1_digest),
            hex(&dest_1_digest),
            src_bytes.len(),
            dest_bytes.len(),
            first_diff,
        );
    }

    // Sweep every remaining file. Collect all mismatches before panicking so
    // a future regression that hits many files reports the full pattern in
    // one failure rather than one file at a time.
    let mut mismatches: Vec<String> = Vec::new();
    for i in 1..=FILE_COUNT {
        let name = format!("file_{i}.txt");
        let src_path = scenario_dir.join(&name);
        let dest_path = dest_scenario.join(&name);

        if !dest_path.is_file() {
            mismatches.push(format!("{name}: missing on destination"));
            continue;
        }

        let src_digest = sha256_file(&src_path);
        let dest_digest = sha256_file(&dest_path);
        if src_digest != dest_digest {
            let src_bytes = fs::read(&src_path).expect("re-read src");
            let dest_bytes = fs::read(&dest_path).expect("re-read dest");
            let first_diff = first_byte_diff(&src_bytes, &dest_bytes);
            mismatches.push(format!(
                "{name}: src={} dest={} first_diff_offset={:?}",
                hex(&src_digest),
                hex(&dest_digest),
                first_diff,
            ));
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "PIP-9.c regression: {} of {} files mismatched after transfer:\n  {}",
            mismatches.len(),
            FILE_COUNT,
            mismatches.join("\n  "),
        );
    }
}

#[cfg(test)]
mod prng_invariants {
    use super::*;

    /// Sanity: the PRNG is deterministic across calls with the same seed.
    #[test]
    fn parallel_threshold_payload_is_deterministic() {
        let a = payload_for(1);
        let b = payload_for(1);
        assert_eq!(a, b, "payload_for(1) must be reproducible");
        assert_eq!(a.len(), FILE_SIZE);
    }

    /// Sanity: different file indices produce different payloads. Without
    /// this guard the byte-identity assertion would trivially pass even if
    /// the receiver wrote the same blob to every destination path.
    #[test]
    fn parallel_threshold_payload_varies_per_index() {
        let p1 = payload_for(1);
        let p2 = payload_for(2);
        let p120 = payload_for(FILE_COUNT);
        assert_ne!(p1, p2, "file_1 and file_2 must differ");
        assert_ne!(p1, p120, "file_1 and file_120 must differ");
        assert_ne!(p2, p120, "file_2 and file_120 must differ");
    }
}
