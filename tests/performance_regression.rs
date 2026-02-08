//! Performance regression test suite for oc-rsync critical hot paths.
//!
//! This test suite runs quick micro-benchmarks with known inputs and asserts
//! they complete within acceptable thresholds. The goal is to catch performance
//! regressions in CI without requiring full benchmark runs.
//!
//! Tests only run in release mode (with `#[cfg(not(debug_assertions))]`) to
//! avoid false positives from debug builds.
//!
//! ## Threshold Design Philosophy
//!
//! All thresholds are set to 2-3x slower than typical benchmarked values to:
//! - Avoid flaky failures due to CI variability
//! - Catch significant regressions (>2x slowdown) while ignoring noise
//! - Work reliably across different CPU architectures (x86_64, aarch64)
//!
//! ## Test Coverage
//!
//! - Rolling checksum: > 1 GiB/s (typically ~10+ GiB/s with SIMD)
//! - MD4 digest: > 400 MiB/s (typically ~800+ MiB/s with OpenSSL)
//! - XXH3 digest: > 5 GiB/s (typically ~15+ GiB/s with SIMD)
//! - File list encoding/decoding: < 50ms for 1000 entries
//! - Delta token encoding: > 1 GiB/s
//!
//! Run with: `cargo nextest run --release --test performance_regression`

#![cfg(not(debug_assertions))]

use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use checksums::RollingChecksum;
use checksums::strong::{Md4, Xxh3};
use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter};
use protocol::wire::{write_token_end, write_token_literal};

// ============================================================================
// Helper Functions
// ============================================================================

/// Generate random-ish data for benchmarking.
fn generate_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Run a benchmark function multiple times and return the median duration.
///
/// This function:
/// 1. Runs one warmup iteration
/// 2. Runs `iterations` timed iterations
/// 3. Returns the median time
fn benchmark<F>(iterations: usize, mut f: F) -> Duration
where
    F: FnMut(),
{
    // Warmup
    f();

    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        f();
        times.push(start.elapsed());
    }

    // Calculate median
    times.sort();
    times[times.len() / 2]
}

/// Assert that throughput meets or exceeds the minimum threshold.
///
/// Panics with a clear message if throughput is below threshold.
fn assert_throughput(
    name: &str,
    bytes_processed: u64,
    duration: Duration,
    min_throughput_gib_per_sec: f64,
) {
    let elapsed_secs = duration.as_secs_f64();
    let throughput_gib_per_sec =
        (bytes_processed as f64 / elapsed_secs) / (1024.0 * 1024.0 * 1024.0);

    assert!(
        throughput_gib_per_sec >= min_throughput_gib_per_sec,
        "Performance regression: {} throughput {:.2} GiB/s < minimum {:.2} GiB/s",
        name,
        throughput_gib_per_sec,
        min_throughput_gib_per_sec
    );
}

/// Assert that throughput meets or exceeds the minimum threshold in MiB/s.
fn assert_throughput_mib(
    name: &str,
    bytes_processed: u64,
    duration: Duration,
    min_throughput_mib_per_sec: f64,
) {
    let elapsed_secs = duration.as_secs_f64();
    let throughput_mib_per_sec = (bytes_processed as f64 / elapsed_secs) / (1024.0 * 1024.0);

    assert!(
        throughput_mib_per_sec >= min_throughput_mib_per_sec,
        "Performance regression: {} throughput {:.2} MiB/s < minimum {:.2} MiB/s",
        name,
        throughput_mib_per_sec,
        min_throughput_mib_per_sec
    );
}

/// Assert that an operation completes within the maximum allowed duration.
fn assert_max_duration(name: &str, duration: Duration, max_duration: Duration) {
    assert!(
        duration <= max_duration,
        "Performance regression: {} took {:.2}ms > maximum {:.2}ms",
        name,
        duration.as_secs_f64() * 1000.0,
        max_duration.as_secs_f64() * 1000.0
    );
}

// ============================================================================
// Performance Tests
// ============================================================================

#[test]
fn test_rolling_checksum_throughput() {
    // Test rolling checksum computation on 32KB blocks
    // Should achieve > 1 GiB/s (conservative threshold, typically ~10+ GiB/s)
    const BLOCK_SIZE: usize = 32 * 1024;
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_GIB_PER_SEC: f64 = 1.0;

    let data = generate_data(BLOCK_SIZE);

    let median_time = benchmark(ITERATIONS, || {
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);
        std::hint::black_box(checksum.value());
    });

    assert_throughput(
        "Rolling checksum (32KB blocks)",
        BLOCK_SIZE as u64,
        median_time,
        MIN_THROUGHPUT_GIB_PER_SEC,
    );
}

#[test]
fn test_md4_digest_throughput() {
    // Test MD4 digest computation
    // Should achieve > 400 MiB/s (conservative threshold)
    // Note: With OpenSSL feature enabled, this can be ~800+ MiB/s
    const DATA_SIZE: usize = 1024 * 1024; // 1 MiB
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_MIB_PER_SEC: f64 = 400.0;

    let data = generate_data(DATA_SIZE);

    let median_time = benchmark(ITERATIONS, || {
        std::hint::black_box(Md4::digest(&data));
    });

    assert_throughput_mib(
        "MD4 digest",
        DATA_SIZE as u64,
        median_time,
        MIN_THROUGHPUT_MIB_PER_SEC,
    );
}

#[test]
fn test_xxh3_digest_throughput() {
    // Test XXH3 digest computation
    // Should achieve > 5 GiB/s (conservative threshold, typically ~15+ GiB/s with SIMD)
    const DATA_SIZE: usize = 1024 * 1024; // 1 MiB
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_GIB_PER_SEC: f64 = 5.0;

    let data = generate_data(DATA_SIZE);
    let seed = 0u64;

    let median_time = benchmark(ITERATIONS, || {
        std::hint::black_box(Xxh3::digest(seed, &data));
    });

    assert_throughput(
        "XXH3 digest",
        DATA_SIZE as u64,
        median_time,
        MIN_THROUGHPUT_GIB_PER_SEC,
    );
}

#[test]
fn test_file_list_encoding_performance() {
    // Test file list encoding for 1000 entries
    // Should complete in < 50ms
    const NUM_ENTRIES: usize = 1000;
    const ITERATIONS: usize = 5;
    const MAX_DURATION_MS: u64 = 50;

    // Generate realistic file entries
    let entries: Vec<FileEntry> = (0..NUM_ENTRIES)
        .map(|i| {
            let depth = (i % 5) + 1;
            let path: PathBuf = (0..depth)
                .map(|d| format!("dir_{}", (i + d) % 100))
                .collect::<PathBuf>()
                .join(format!("file_{i}.txt"));

            FileEntry::new_file(path, (i * 1024) as u64, 0o644)
        })
        .collect();

    let median_time = benchmark(ITERATIONS, || {
        let mut buf = Vec::with_capacity(NUM_ENTRIES * 100);
        let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);

        for entry in &entries {
            writer.write_entry(&mut buf, entry).unwrap();
        }
        writer.write_end(&mut buf, None).unwrap();

        std::hint::black_box(&buf);
    });

    assert_max_duration(
        "File list encoding (1000 entries)",
        median_time,
        Duration::from_millis(MAX_DURATION_MS),
    );
}

#[test]
fn test_file_list_decoding_performance() {
    // Test file list decoding for 1000 entries
    // Should complete in < 50ms
    const NUM_ENTRIES: usize = 1000;
    const ITERATIONS: usize = 5;
    const MAX_DURATION_MS: u64 = 50;

    // Generate and encode file entries
    let entries: Vec<FileEntry> = (0..NUM_ENTRIES)
        .map(|i| {
            let path: PathBuf = format!("dir_{}/file_{i}.txt", i % 100).into();
            FileEntry::new_file(path, (i * 1024) as u64, 0o644)
        })
        .collect();

    let mut encoded = Vec::with_capacity(NUM_ENTRIES * 100);
    {
        let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
        for entry in &entries {
            writer.write_entry(&mut encoded, entry).unwrap();
        }
        writer.write_end(&mut encoded, None).unwrap();
    }

    let median_time = benchmark(ITERATIONS, || {
        let mut cursor = Cursor::new(&encoded);
        let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
        let mut decoded = Vec::with_capacity(NUM_ENTRIES);

        while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
            decoded.push(entry);
        }

        std::hint::black_box(decoded);
    });

    assert_max_duration(
        "File list decoding (1000 entries)",
        median_time,
        Duration::from_millis(MAX_DURATION_MS),
    );
}

#[test]
fn test_delta_token_encoding_throughput() {
    // Test delta token encoding for 1MB of literal data
    // Should achieve > 1 GiB/s
    const DATA_SIZE: usize = 1024 * 1024; // 1 MiB
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_GIB_PER_SEC: f64 = 1.0;

    let data = generate_data(DATA_SIZE);

    let median_time = benchmark(ITERATIONS, || {
        let mut buf = Vec::with_capacity(DATA_SIZE + 1024);

        // Encode as a single literal token
        write_token_literal(&mut buf, &data).unwrap();
        write_token_end(&mut buf).unwrap();

        std::hint::black_box(&buf);
    });

    assert_throughput(
        "Delta token encoding (1MB literal)",
        DATA_SIZE as u64,
        median_time,
        MIN_THROUGHPUT_GIB_PER_SEC,
    );
}

#[test]
fn test_file_list_roundtrip_performance() {
    // Test complete file list encode-decode roundtrip
    // Should complete in < 100ms for 1000 entries
    const NUM_ENTRIES: usize = 1000;
    const ITERATIONS: usize = 5;
    const MAX_DURATION_MS: u64 = 100;

    let entries: Vec<FileEntry> = (0..NUM_ENTRIES)
        .map(|i| {
            let path: PathBuf = format!("project/src/module_{}/file_{i}.rs", i / 10).into();
            FileEntry::new_file(path, (i * 512) as u64, 0o644)
        })
        .collect();

    let median_time = benchmark(ITERATIONS, || {
        // Encode
        let mut buf = Vec::with_capacity(NUM_ENTRIES * 100);
        let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
        for entry in &entries {
            writer.write_entry(&mut buf, entry).unwrap();
        }
        writer.write_end(&mut buf, None).unwrap();

        // Decode
        let mut cursor = Cursor::new(&buf);
        let mut reader = FileListReader::new(ProtocolVersion::NEWEST);
        let mut decoded = Vec::with_capacity(NUM_ENTRIES);
        while let Some(entry) = reader.read_entry(&mut cursor).unwrap() {
            decoded.push(entry);
        }

        std::hint::black_box(decoded);
    });

    assert_max_duration(
        "File list roundtrip (1000 entries)",
        median_time,
        Duration::from_millis(MAX_DURATION_MS),
    );
}

#[test]
fn test_rolling_checksum_large_block() {
    // Test rolling checksum on larger block size (128KB)
    // Should achieve > 1 GiB/s
    const BLOCK_SIZE: usize = 128 * 1024;
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_GIB_PER_SEC: f64 = 1.0;

    let data = generate_data(BLOCK_SIZE);

    let median_time = benchmark(ITERATIONS, || {
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);
        std::hint::black_box(checksum.value());
    });

    assert_throughput(
        "Rolling checksum (128KB blocks)",
        BLOCK_SIZE as u64,
        median_time,
        MIN_THROUGHPUT_GIB_PER_SEC,
    );
}

#[test]
fn test_xxh3_small_blocks() {
    // Test XXH3 on small 4KB blocks (typical for many-file transfers)
    // Should still maintain > 5 GiB/s aggregate throughput
    const BLOCK_SIZE: usize = 4 * 1024;
    const NUM_BLOCKS: usize = 256; // Total 1 MiB
    const ITERATIONS: usize = 5;
    const MIN_THROUGHPUT_GIB_PER_SEC: f64 = 5.0;

    let blocks: Vec<Vec<u8>> = (0..NUM_BLOCKS).map(|_| generate_data(BLOCK_SIZE)).collect();

    let seed = 0u64;
    let total_bytes = (BLOCK_SIZE * NUM_BLOCKS) as u64;

    let median_time = benchmark(ITERATIONS, || {
        for block in &blocks {
            std::hint::black_box(Xxh3::digest(seed, block));
        }
    });

    assert_throughput(
        "XXH3 digest (256 x 4KB blocks)",
        total_bytes,
        median_time,
        MIN_THROUGHPUT_GIB_PER_SEC,
    );
}
