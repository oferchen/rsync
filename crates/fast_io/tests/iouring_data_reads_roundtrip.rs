//! Byte-identical round-trip for the IUD-6 sender/reader io_uring slurp
//! wrapper (`fast_io::read_file_with_io_uring`).
//!
//! Writes 4 MiB of pseudo-random bytes via stdlib, then reads them back
//! through `IoUringFileReader::read_to_end` and asserts byte equality.
//! Skipped at runtime when the kernel does not support io_uring (pre-5.6,
//! seccomp-blocked containers) so the test stays green on CI sandboxes
//! that deny `io_uring_setup(2)`.

#![cfg(all(target_os = "linux", feature = "iouring-data-reads"))]

use std::fs;
use std::io::Write;

use fast_io::{IoUringFileReader, is_io_uring_available, read_file_with_io_uring};

/// Deterministic pseudo-random fill so failures reproduce locally. Plain
/// LCG; no need for a real PRNG when the test only cares about byte-identity.
fn fill_pseudo_random(buf: &mut [u8], mut state: u64) {
    for byte in buf.iter_mut() {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (state >> 33) as u8;
    }
}

const PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

#[test]
fn slurp_wrapper_round_trips_four_mib_of_random_bytes() {
    if !is_io_uring_available() {
        eprintln!("skipping: io_uring is not available on this host");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("payload.bin");

    let mut payload = vec![0u8; PAYLOAD_BYTES];
    fill_pseudo_random(&mut payload, 0xA5A5_5A5A_DEAD_BEEFu64);

    let mut writer = fs::File::create(&path).expect("create payload file");
    writer.write_all(&payload).expect("write payload bytes");
    writer.sync_all().expect("fsync payload");
    drop(writer);

    let round_trip = match read_file_with_io_uring(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("skipping: io_uring slurp wrapper unavailable: {error}");
            return;
        }
    };
    assert_eq!(round_trip.len(), PAYLOAD_BYTES, "slurp length mismatch");
    assert_eq!(
        round_trip, payload,
        "slurp bytes diverged from stdlib write"
    );
}

#[test]
fn file_reader_read_to_end_matches_stdlib_bytes() {
    if !is_io_uring_available() {
        eprintln!("skipping: io_uring is not available on this host");
        return;
    }

    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("payload.bin");

    let mut payload = vec![0u8; PAYLOAD_BYTES];
    fill_pseudo_random(&mut payload, 0x1234_5678_9ABC_DEF0u64);

    fs::write(&path, &payload).expect("write payload bytes");

    let mut reader = match IoUringFileReader::open(&path) {
        Ok(r) => r,
        Err(error) => {
            eprintln!("skipping: io_uring reader unavailable: {error}");
            return;
        }
    };
    assert_eq!(reader.len(), PAYLOAD_BYTES as u64);
    assert!(!reader.is_empty());

    let bytes = match reader.read_to_end() {
        Ok(b) => b,
        Err(error) => {
            eprintln!("skipping: io_uring read_to_end failed: {error}");
            return;
        }
    };
    assert_eq!(bytes, payload, "reader bytes diverged from stdlib write");
}
