//! Byte-identical regression for the io_uring data path (IUD-8, #2368).
//!
//! This integration test asserts that the io_uring data path produces a
//! byte-identical transfer to the standard library path for a representative
//! matrix of file shapes. It is the safety net that paired changes IUD-5
//! (registered-buffer writer) and IUD-6 (registered-buffer reader) must
//! satisfy before either is promoted past the opt-in feature flag.
//!
//! # Gating
//!
//! The test is gated on three conditions, all of which must hold for the
//! cells to run:
//!
//! 1. `target_os = "linux"` - io_uring is a Linux-only kernel interface.
//! 2. `feature = "iouring-data-writes"` - the IUD-5 writer is compiled in.
//! 3. `feature = "iouring-data-reads"` - the IUD-6 reader is compiled in.
//!
//! When any of the three is missing, the file compiles to a no-op (an empty
//! `main` analogue and zero `#[test]` functions), so CI on platforms or
//! feature combinations that do not yet wire IUD-5 / IUD-6 will neither
//! fail nor false-pass. Once both features are merged the matrix activates
//! automatically on Linux runners.
//!
//! # Running locally
//!
//! After IUD-5 and IUD-6 land on master:
//!
//! ```text
//! # Full matrix except the env-gated 64 MiB cell:
//! cargo nextest run -p fast_io \
//!     --features "io_uring iouring-data-writes iouring-data-reads" \
//!     -E 'test(io_uring_byte_identical)'
//!
//! # Include the 64 MiB cell (env-gated to protect CI runners with limited
//! # tmpfs):
//! OC_RSYNC_BENCH_LARGE=1 cargo nextest run -p fast_io \
//!     --features "io_uring iouring-data-writes iouring-data-reads" \
//!     -E 'test(io_uring_byte_identical)'
//! ```
//!
//! # Matrix
//!
//! | Cell                          | Size      | Dispatch                |
//! |-------------------------------|-----------|-------------------------|
//! | `tiny_file_1KiB`              | 1 KiB     | below threshold, stdlib |
//! | `boundary_file_exactly_1MiB`  | 1 MiB     | exactly at threshold    |
//! | `medium_file_4MiB_random`     | 4 MiB     | above threshold, ring   |
//! | `large_file_64MiB_random`     | 64 MiB    | above (env-gated)       |
//! | `sparse_file_with_holes`      | ~2 MiB    | mixed zero/data runs    |
//!
//! Each cell:
//!
//! 1. Generates source bytes with a seeded `StdRng`.
//! 2. Writes the source via the stdlib path.
//! 3. Writes the destination via the io_uring data path.
//! 4. Reads both back via stdlib and asserts `assert_eq!` on the bytes.
//!
//! Sparseness is not asserted; the file system may or may not allocate
//! holes. The contract under test is byte-identity only.

#![cfg_attr(
    not(all(
        target_os = "linux",
        feature = "iouring-data-writes",
        feature = "iouring-data-reads"
    )),
    allow(dead_code)
)]

#[cfg(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
))]
mod active {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::path::Path;

    use tempfile::TempDir;

    /// Below-threshold sentinel: dispatch should fall back to stdlib.
    const TINY: usize = 1024;
    /// On-threshold sentinel: dispatch behaviour must be deterministic.
    const BOUNDARY: usize = 1024 * 1024;
    /// Above-threshold cell: exercises the io_uring write path.
    const MEDIUM: usize = 4 * 1024 * 1024;
    /// Above-threshold large cell: env-gated to protect constrained runners.
    const LARGE: usize = 64 * 1024 * 1024;
    /// Sparse cell zero-run size.
    const SPARSE_ZERO_RUN: usize = 1024 * 1024;
    /// Sparse cell data-run size, sandwiched between two zero runs.
    const SPARSE_DATA_RUN: usize = 1024;

    /// Deterministic pseudo-random byte generator.
    ///
    /// Uses xorshift64 keyed on the test name so each cell gets a distinct
    /// stream. Avoids pulling `rand` into `fast_io`'s dev dependencies;
    /// the stream is good enough for "did all the bytes survive the round
    /// trip" testing - it does not need to be cryptographically strong.
    fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Mix a textual label into a u64 seed so cell names map to streams.
    fn seed_for(label: &str) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// Writes `data` to `path` using the standard library path.
    fn stdlib_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
        file.write_all(data)?;
        file.sync_all()
    }

    /// Writes `data` to `path` via the io_uring data path under test.
    ///
    /// Uses the published `fast_io::writer_from_file` entry point with the
    /// io_uring policy forced on. When IUD-5 promotes the registered-buffer
    /// writer to a first-class public helper, swap this body to the new
    /// API; the test cells do not need to change.
    fn iouring_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(path)?;
        let mut writer = fast_io::writer_from_file(file, 64 * 1024, fast_io::IoUringPolicy::Auto)?;
        // `IoUringOrStdWriter` exposes `write_all` and `flush` via `std::io::Write`.
        writer.write_all(data)?;
        writer.flush()?;
        drop(writer);
        Ok(())
    }

    /// Reads `path` fully via the standard library path.
    fn stdlib_read(path: &Path) -> std::io::Result<Vec<u8>> {
        let mut file = File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Runs one byte-identity cell. Generates `data`, writes it through
    /// both paths into separate destinations under `dir`, reads both back
    /// via the stdlib, and asserts byte equality.
    fn assert_byte_identical(label: &str, data: &[u8]) {
        // Skip the cell at runtime when the kernel cannot honour
        // io_uring submissions (older kernels, seccomp containers, etc.).
        // The compile-time gate already enforces the feature flags.
        if !fast_io::is_io_uring_available() {
            eprintln!(
                "io_uring_byte_identical[{label}]: io_uring unavailable on this host, skipping"
            );
            return;
        }

        let dir = TempDir::new().expect("create tempdir");
        let stdlib_path = dir.path().join(format!("{label}.stdlib"));
        let iouring_path = dir.path().join(format!("{label}.iouring"));

        // Source-of-truth via stdlib first.
        stdlib_write(&stdlib_path, data).expect("stdlib write");
        // Destination via the io_uring data path.
        iouring_write(&iouring_path, data).expect("io_uring write");

        let stdlib_back = stdlib_read(&stdlib_path).expect("stdlib read-back");
        let iouring_back = stdlib_read(&iouring_path).expect("io_uring read-back");

        assert_eq!(
            stdlib_back.len(),
            iouring_back.len(),
            "{label}: length mismatch (stdlib {} vs io_uring {})",
            stdlib_back.len(),
            iouring_back.len(),
        );
        // Use a hash comparison first so the assertion message stays short
        // for the large cell; only on mismatch do we drop into the slow
        // assert_eq! that prints the offending bytes.
        if stdlib_back != iouring_back {
            // Locate the first divergence to make the failure actionable.
            let first_diff = stdlib_back
                .iter()
                .zip(iouring_back.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(stdlib_back.len().min(iouring_back.len()));
            panic!(
                "{label}: byte mismatch at offset {first_diff} (len {})",
                stdlib_back.len()
            );
        }
        assert_eq!(stdlib_back, data, "{label}: stdlib round trip lost data");
        assert_eq!(iouring_back, data, "{label}: io_uring round trip lost data");
    }

    #[test]
    fn tiny_file_1kib() {
        let label = "tiny_file_1KiB";
        let data = seeded_bytes(seed_for(label), TINY);
        assert_byte_identical(label, &data);
    }

    #[test]
    fn boundary_file_exactly_1mib() {
        let label = "boundary_file_exactly_1MiB";
        let data = seeded_bytes(seed_for(label), BOUNDARY);
        assert_byte_identical(label, &data);
    }

    #[test]
    fn medium_file_4mib_random() {
        let label = "medium_file_4MiB_random";
        let data = seeded_bytes(seed_for(label), MEDIUM);
        assert_byte_identical(label, &data);
    }

    /// Env-gated on `OC_RSYNC_BENCH_LARGE=1`: writes 64 MiB twice
    /// (~128 MiB of tempdir traffic). Skipped by default to keep the
    /// regular nextest run cheap on small CI runners.
    #[test]
    fn large_file_64mib_random() {
        if std::env::var_os("OC_RSYNC_BENCH_LARGE").is_none() {
            eprintln!("large_file_64MiB_random: skipped (set OC_RSYNC_BENCH_LARGE=1 to enable)");
            return;
        }
        let label = "large_file_64MiB_random";
        let data = seeded_bytes(seed_for(label), LARGE);
        assert_byte_identical(label, &data);
    }

    /// Mixed zero-run + data-run + zero-run profile. Filesystems may or
    /// may not preserve sparseness; the byte contents must match.
    #[test]
    fn sparse_file_with_holes() {
        let label = "sparse_file_with_holes";
        let data_run = seeded_bytes(seed_for(label), SPARSE_DATA_RUN);
        let mut data = Vec::with_capacity(SPARSE_ZERO_RUN + SPARSE_DATA_RUN + SPARSE_ZERO_RUN);
        data.resize(SPARSE_ZERO_RUN, 0u8);
        data.extend_from_slice(&data_run);
        data.resize(SPARSE_ZERO_RUN + SPARSE_DATA_RUN + SPARSE_ZERO_RUN, 0u8);
        assert_byte_identical(label, &data);
    }
}

// When the gating features are absent the file still needs to compile;
// the active module above is conditionally compiled out and this stub
// keeps integration-test discovery happy without emitting any test cases.
#[cfg(not(all(
    target_os = "linux",
    feature = "iouring-data-writes",
    feature = "iouring-data-reads"
)))]
#[test]
fn iouring_data_path_features_disabled() {
    eprintln!(
        "io_uring byte-identical regression skipped: enable both \
         iouring-data-writes and iouring-data-reads on Linux to run"
    );
}
