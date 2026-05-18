//! IUD-5 round-trip integration test for `write_file_with_io_uring`.
//!
//! Writes a 4 MiB pseudo-random payload through the opt-in io_uring
//! registered-buffer wrapper, then re-reads the destination with
//! `std::fs::read` and asserts byte-identical output. The test runs only on
//! Linux with both the `io_uring` and `iouring-data-writes` features compiled
//! in, and skips gracefully when the kernel rejects ring construction (older
//! than 5.6, seccomp-restricted container, etc.).

#![cfg(all(target_os = "linux", feature = "iouring-data-writes"))]

use std::fs;

use fast_io::{is_io_uring_available, write_file_with_io_uring};
use tempfile::tempdir;

/// Builds a deterministic pseudo-random buffer using a 64-bit LCG so the
/// payload survives test-to-test reproducibility without pulling in `rand`.
fn pseudo_random_payload(len: usize, seed: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len);
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    while buf.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        buf.extend_from_slice(&state.to_le_bytes());
    }
    buf.truncate(len);
    buf
}

#[test]
fn write_file_with_io_uring_round_trip_4mib() {
    if !is_io_uring_available() {
        eprintln!("io_uring unavailable on this host; skipping IUD-5 dispatch test");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let dst = dir.path().join("iud5_payload.bin");

    let payload = pseudo_random_payload(4 * 1024 * 1024, 0xC0FF_EE00_BEEF_F00D);
    write_file_with_io_uring(&dst, &payload).expect("io_uring write must succeed");

    let read_back = fs::read(&dst).expect("read destination");
    assert_eq!(
        read_back.len(),
        payload.len(),
        "destination size must match payload"
    );
    assert_eq!(read_back, payload, "round-trip must be byte-identical");
}

#[test]
fn write_file_with_io_uring_creates_empty_file() {
    if !is_io_uring_available() {
        return;
    }
    let dir = tempdir().expect("tempdir");
    let dst = dir.path().join("iud5_empty.bin");
    write_file_with_io_uring(&dst, &[]).expect("zero-length write must succeed");
    let meta = fs::metadata(&dst).expect("stat destination");
    assert_eq!(meta.len(), 0, "destination must be zero-length");
}
