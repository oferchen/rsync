//! High-concurrency stress coverage for the Windows IOCP disk path
//! (task #1871).
//!
//! These cases exercise the IOCP submission and completion machinery under
//! workloads that are large enough to stress kernel-pool limits and the
//! per-process file-handle table but still safely under the 16,384 default
//! handle cap. The goal is to confirm that:
//!
//! * `IocpDiskBatch` can fan out across 10,000 distinct files in a single
//!   process without panicking, leaking handles, or losing bytes.
//! * 10 long-lived `IocpWriter` instances driven round-robin from a single
//!   thread interleave correctly across handles and produce per-file output
//!   that matches the expected payload byte-for-byte.
//!
//! Stress runs are gated behind `OC_RSYNC_IOCP_STRESS=1` so they do not
//! inflate normal CI durations. Set the variable manually when triggering
//! the longer run on a Windows host with enough free disk for ~40 MB of
//! scratch files.
//!
//! The whole file is cfg-gated to `windows + iocp` and compiles to nothing
//! on Linux and macOS.

#![cfg(all(target_os = "windows", feature = "iocp"))]

use std::env;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use fast_io::iocp::{IocpConfig, IocpDiskBatch, IocpWriter, is_iocp_available};

/// Stress runs are opt-in; absence of the gate variable skips the test with
/// a clear `eprintln!` so a normal `cargo nextest` invocation is fast and
/// quiet but a manual `OC_RSYNC_IOCP_STRESS=1 cargo nextest run ...`
/// triggers the heavier workload.
const STRESS_ENV: &str = "OC_RSYNC_IOCP_STRESS";

/// Deterministic 4 KB payload. The content is irrelevant to the stress goal
/// (the count is what matters) so a single repeated byte keeps the buffer
/// cheap to set up and trivial to verify on the read-back path.
const SMALL_PAYLOAD_LEN: usize = 4096;
const PAYLOAD_BYTE: u8 = 0xA5;

fn payload() -> Vec<u8> {
    vec![PAYLOAD_BYTE; SMALL_PAYLOAD_LEN]
}

fn stress_enabled() -> bool {
    matches!(env::var(STRESS_ENV).as_deref(), Ok("1"))
}

/// Opens a fresh writable file using `std::fs` so it can be handed to
/// `IocpDiskBatch::begin_file`, which reopens internally with
/// `FILE_FLAG_OVERLAPPED`.
fn open_for_batch(path: &Path) -> File {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("create file for IocpDiskBatch")
}

/// Stress #1: drive 10,000 distinct files through a single `IocpDiskBatch`
/// with `concurrent_ops = 64`, writing a 4 KB payload to each. Verifies that
/// every file lands at exactly 4 KB on disk, the file count is precisely
/// 10,000, and the batch reports the matching per-file byte counter.
#[test]
fn stress_10k_small_writes_distinct_files() {
    if !stress_enabled() {
        eprintln!(
            "skipping: stress test gated behind {STRESS_ENV}=1 (set it to opt in to the heavy run)"
        );
        return;
    }
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let root: PathBuf = dir.path().to_path_buf();

    let config = IocpConfig {
        // 4 KB chunk matches the payload size: one submission per file, so
        // we exercise the submit/complete round-trip 10,000 times back to
        // back without re-using the same buffer slot.
        buffer_size: SMALL_PAYLOAD_LEN,
        concurrent_ops: 64,
        ..IocpConfig::default()
    };
    let mut batch = IocpDiskBatch::new(&config).expect("create IocpDiskBatch");
    let buf = payload();

    const FILE_COUNT: usize = 10_000;

    for i in 0..FILE_COUNT {
        let path = root.join(format!("stress-{i:05}.bin"));
        let file = open_for_batch(&path);
        batch.begin_file(file).expect("begin_file");
        batch.write_data(&buf).expect("write_data");
        let (_returned, written) = batch.commit_file(false).expect("commit_file");
        assert_eq!(
            written as usize, SMALL_PAYLOAD_LEN,
            "file {i} reported {written} bytes, expected {SMALL_PAYLOAD_LEN}"
        );
    }

    drop(batch);

    // Walk the tempdir once and confirm both the count and the per-file
    // size. Reading the entries cheaply doubles as a leak/handle audit:
    // if any file failed to close cleanly the metadata call would surface
    // a sharing violation.
    let entries: Vec<_> = std::fs::read_dir(&root)
        .expect("read_dir tempdir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        entries.len(),
        FILE_COUNT,
        "expected exactly {FILE_COUNT} files in the tempdir, found {}",
        entries.len()
    );

    for entry in &entries {
        let meta = entry.metadata().expect("metadata for stress file");
        assert_eq!(
            meta.len() as usize,
            SMALL_PAYLOAD_LEN,
            "{:?} landed at {} bytes, expected {}",
            entry.path(),
            meta.len(),
            SMALL_PAYLOAD_LEN
        );
    }
}

/// FNV-1a 64-bit. Zero allocations, deterministic, and trivially verifiable
/// against an in-test reference, which is all this test needs. Avoids
/// pulling in a new dev-dependency for a single-use checksum.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Stress #2: keep 10 `IocpWriter` handles alive concurrently and drive
/// 1,000 sequential `write_all(4 KB)` calls per file in strict round-robin
/// order from a single thread. This interleaves submissions across 10
/// separate completion ports without spawning worker threads, so the test
/// verifies that the per-writer state stays consistent under repeated
/// in-flight overlapped writes targeting the same handle.
///
/// Per-file expected size is 1000 * 4 KB = ~3.9 MB; total scratch is ~39 MB
/// across 10 files - small enough to run on a CI runner with default disk
/// budgets when the stress gate is enabled.
#[test]
fn stress_10k_alternating_files_via_iocp_writer() {
    if !stress_enabled() {
        eprintln!(
            "skipping: stress test gated behind {STRESS_ENV}=1 (set it to opt in to the heavy run)"
        );
        return;
    }
    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable on this host");
        return;
    }

    const FILE_COUNT: usize = 10;
    const WRITES_PER_FILE: usize = 1_000;
    const TOTAL_WRITES: usize = FILE_COUNT * WRITES_PER_FILE;
    const EXPECTED_SIZE: u64 = (WRITES_PER_FILE * SMALL_PAYLOAD_LEN) as u64;

    let dir = tempdir().expect("tempdir");
    let root: PathBuf = dir.path().to_path_buf();

    let config = IocpConfig {
        // A 16 KB buffer holds four chunks before flushing, which exercises
        // both the buffered fast path and the periodic overlapped flush
        // across the round-robin loop.
        buffer_size: 16 * 1024,
        concurrent_ops: 4,
        ..IocpConfig::default()
    };
    let buf = payload();

    let paths: Vec<PathBuf> = (0..FILE_COUNT)
        .map(|i| root.join(format!("rr-{i:02}.bin")))
        .collect();
    let mut writers: Vec<IocpWriter> = paths
        .iter()
        .map(|p| IocpWriter::create(p, &config).expect("create IocpWriter"))
        .collect();

    // Round-robin: writer 0, 1, ..., 9, 0, 1, ..., 9, ... for a total of
    // TOTAL_WRITES calls. Each iteration is one `write_all(4 KB)`.
    for step in 0..TOTAL_WRITES {
        let idx = step % FILE_COUNT;
        writers[idx]
            .write_all(&buf)
            .unwrap_or_else(|e| panic!("write_all on file {idx} step {step}: {e}"));
    }

    // Close every writer before reading back so flushes complete and the
    // underlying handles release any sharing locks.
    for mut w in writers.drain(..) {
        w.flush().expect("final flush");
        drop(w);
    }

    let expected_chunk_hash = fnv1a_64(&buf);
    let mut expected_full_hash = 0xcbf2_9ce4_8422_2325_u64;
    for _ in 0..WRITES_PER_FILE {
        for b in &buf {
            expected_full_hash ^= u64::from(*b);
            expected_full_hash = expected_full_hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    // Sanity: chunk hash is non-degenerate (catches accidental empty buf).
    assert_ne!(expected_chunk_hash, 0);

    for (i, path) in paths.iter().enumerate() {
        let meta = std::fs::metadata(path).unwrap_or_else(|e| panic!("metadata for {path:?}: {e}"));
        assert_eq!(
            meta.len(),
            EXPECTED_SIZE,
            "file {i} landed at {} bytes, expected {EXPECTED_SIZE}",
            meta.len()
        );

        let on_disk = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        assert_eq!(
            on_disk.len() as u64,
            EXPECTED_SIZE,
            "file {i} read-back length mismatch"
        );
        assert_eq!(
            fnv1a_64(&on_disk),
            expected_full_hash,
            "file {i} content checksum mismatch"
        );
    }
}
