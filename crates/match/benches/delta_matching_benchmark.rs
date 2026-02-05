//! Benchmarks for delta block matching performance.
//!
//! Run with: `cargo bench -p matching --features parallel`
//!
//! This benchmark suite measures:
//! - Signature generation time
//! - Block matching time (hash table lookup + strong checksum verification)
//! - Memory usage during matching
//!
//! Test scenarios:
//! 1. 1MB file with 1% changes (best case for delta)
//! 2. 1MB file with 50% changes (moderate delta)
//! 3. 100MB file with scattered changes (large file performance)
//! 4. File with many small blocks (hash table stress test)

use std::io::Cursor;
use std::num::{NonZeroU32, NonZeroU8};

use criterion::{
    BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};

use checksums::RollingChecksum;
use matching::{DeltaGenerator, DeltaSignatureIndex, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

// ============================================================================
// Test Data Generation
// ============================================================================

/// Creates test data with specified change percentage.
///
/// The basis file is generated with a deterministic pattern.
/// The modified file has `change_percent` of bytes changed at random positions.
fn create_test_data(size: usize, change_percent: u8) -> (Vec<u8>, Vec<u8>) {
    // Generate basis with deterministic pattern
    let basis: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    // Create modified version
    let mut modified = basis.clone();
    let change_count = (size as f64 * (change_percent as f64 / 100.0)) as usize;

    // Use deterministic "random" positions based on change_percent
    let seed = change_percent as usize * 7919;
    for i in 0..change_count {
        let pos = (seed + i * 31337) % size;
        modified[pos] = modified[pos].wrapping_add(128);
    }

    (basis, modified)
}

/// Creates test data with scattered small changes throughout.
fn create_scattered_changes(size: usize, num_changes: usize, change_size: usize) -> (Vec<u8>, Vec<u8>) {
    let basis: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let mut modified = basis.clone();

    let spacing = size / (num_changes + 1);
    for i in 0..num_changes {
        let start = (i + 1) * spacing;
        let end = (start + change_size).min(size);
        for pos in start..end {
            modified[pos] = modified[pos].wrapping_add(1);
        }
    }

    (basis, modified)
}

/// Builds a signature index from basis data.
fn build_signature_index(data: &[u8], block_size: Option<u32>) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        block_size.map(NonZeroU32::new).flatten(),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

// ============================================================================
// Signature Generation Benchmarks
// ============================================================================

fn bench_signature_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("signature_generation");

    // Different file sizes
    for size in [1_024 * 1_024, 10 * 1_024 * 1_024, 100 * 1_024 * 1_024] {
        let size_name = match size {
            1_048_576 => "1MB",
            10_485_760 => "10MB",
            104_857_600 => "100MB",
            _ => "unknown",
        };

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("md4", size_name),
            &size,
            |b, &size| {
                let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
                b.iter(|| {
                    let params = SignatureLayoutParams::new(
                        data.len() as u64,
                        None,
                        ProtocolVersion::NEWEST,
                        NonZeroU8::new(16).unwrap(),
                    );
                    let layout = calculate_signature_layout(params).expect("layout");
                    let sig = generate_file_signature(
                        black_box(data.as_slice()),
                        layout,
                        SignatureAlgorithm::Md4,
                    );
                    black_box(sig)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("xxh3", size_name),
            &size,
            |b, &size| {
                let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
                b.iter(|| {
                    let params = SignatureLayoutParams::new(
                        data.len() as u64,
                        None,
                        ProtocolVersion::NEWEST,
                        NonZeroU8::new(8).unwrap(),
                    );
                    let layout = calculate_signature_layout(params).expect("layout");
                    let sig = generate_file_signature(
                        black_box(data.as_slice()),
                        layout,
                        SignatureAlgorithm::Xxh3 { seed: 0 },
                    );
                    black_box(sig)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Block Matching Benchmarks
// ============================================================================

fn bench_block_matching(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_matching");

    // 1MB file with 1% changes (best case - most blocks match)
    {
        let (basis, modified) = create_test_data(1_024 * 1_024, 1);
        let index = build_signature_index(&basis, None);

        group.throughput(Throughput::Bytes(modified.len() as u64));
        group.bench_function("1MB_1pct_changes", |b| {
            b.iter(|| {
                let script = generate_delta(black_box(modified.as_slice()), &index);
                black_box(script)
            });
        });
    }

    // 1MB file with 50% changes (moderate - mixed copy/literal)
    {
        let (basis, modified) = create_test_data(1_024 * 1_024, 50);
        let index = build_signature_index(&basis, None);

        group.throughput(Throughput::Bytes(modified.len() as u64));
        group.bench_function("1MB_50pct_changes", |b| {
            b.iter(|| {
                let script = generate_delta(black_box(modified.as_slice()), &index);
                black_box(script)
            });
        });
    }

    // 100MB file with scattered changes (large file performance)
    {
        let (basis, modified) = create_scattered_changes(100 * 1_024 * 1_024, 100, 1024);
        let index = build_signature_index(&basis, None);

        group.throughput(Throughput::Bytes(modified.len() as u64));
        group.bench_function("100MB_scattered", |b| {
            b.iter(|| {
                let script = generate_delta(black_box(modified.as_slice()), &index);
                black_box(script)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Hash Table Lookup Benchmarks
// ============================================================================

fn bench_hash_table_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash_table_lookup");

    // Many small blocks stress test hash table
    for block_size in [512, 1024, 2048, 4096, 8192] {
        let size = 10 * 1024 * 1024; // 10MB
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let index = build_signature_index(&data, Some(block_size));
        let block_count = size / block_size as usize;

        group.throughput(Throughput::Elements(block_count as u64));
        group.bench_with_input(
            BenchmarkId::new("lookup", format!("{}B_blocks", block_size)),
            &(data, index),
            |b, (data, index)| {
                b.iter(|| {
                    // Simulate matching every block
                    let block_len = index.block_length();
                    let mut matches = 0usize;
                    for chunk_start in (0..data.len()).step_by(block_len) {
                        let end = (chunk_start + block_len).min(data.len());
                        if end - chunk_start != block_len {
                            break;
                        }
                        let chunk = &data[chunk_start..end];
                        let mut rolling = RollingChecksum::new();
                        rolling.update(chunk);
                        let digest = rolling.digest();
                        if index.find_match_bytes(digest, chunk).is_some() {
                            matches += 1;
                        }
                    }
                    black_box(matches)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Rolling Checksum Performance
// ============================================================================

fn bench_rolling_checksum(c: &mut Criterion) {
    let mut group = c.benchmark_group("rolling_checksum");

    // Initial update (compute from scratch)
    for block_size in [512, 1024, 2048, 4096, 8192] {
        let data: Vec<u8> = (0..block_size).map(|i| (i % 251) as u8).collect();

        group.throughput(Throughput::Bytes(block_size as u64));
        group.bench_with_input(
            BenchmarkId::new("update", format!("{}B", block_size)),
            &data,
            |b, data| {
                b.iter(|| {
                    let mut rolling = RollingChecksum::new();
                    rolling.update(black_box(data));
                    black_box(rolling.value())
                });
            },
        );
    }

    // Roll operation (sliding window)
    {
        let window_size = 1024;
        let data: Vec<u8> = (0..window_size + 10000).map(|i| (i % 251) as u8).collect();

        group.throughput(Throughput::Elements(10000));
        group.bench_function("roll_10000_bytes", |b| {
            b.iter(|| {
                let mut rolling = RollingChecksum::new();
                rolling.update(&data[..window_size]);
                for i in 0..10000 {
                    rolling
                        .roll(data[i], data[i + window_size])
                        .expect("roll");
                }
                black_box(rolling.value())
            });
        });
    }

    group.finish();
}

// ============================================================================
// Strong Checksum Performance
// ============================================================================

fn bench_strong_checksum(c: &mut Criterion) {
    use checksums::strong::{Md4, Xxh3, StrongDigest};

    let mut group = c.benchmark_group("strong_checksum");

    for block_size in [512, 1024, 2048, 4096, 8192] {
        let data: Vec<u8> = (0..block_size).map(|i| (i % 251) as u8).collect();

        group.throughput(Throughput::Bytes(block_size as u64));

        // MD4 (legacy)
        group.bench_with_input(
            BenchmarkId::new("md4", format!("{}B", block_size)),
            &data,
            |b, data| {
                b.iter(|| {
                    let digest = Md4::digest(black_box(data));
                    black_box(digest)
                });
            },
        );

        // XXH3 (fast)
        group.bench_with_input(
            BenchmarkId::new("xxh3", format!("{}B", block_size)),
            &data,
            |b, data| {
                b.iter(|| {
                    let digest = Xxh3::digest(0, black_box(data));
                    black_box(digest)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Memory Allocation Analysis
// ============================================================================

fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    // Signature index construction memory
    for size_mb in [1, 10, 100] {
        let size = size_mb * 1024 * 1024;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

        group.bench_with_input(
            BenchmarkId::new("index_construction", format!("{}MB", size_mb)),
            &data,
            |b, data| {
                let params = SignatureLayoutParams::new(
                    data.len() as u64,
                    None,
                    ProtocolVersion::NEWEST,
                    NonZeroU8::new(16).unwrap(),
                );
                let layout = calculate_signature_layout(params).expect("layout");
                let signature = generate_file_signature(
                    data.as_slice(),
                    layout,
                    SignatureAlgorithm::Md4,
                )
                .expect("signature");

                b.iter(|| {
                    let index =
                        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4);
                    black_box(index)
                });
            },
        );
    }

    // Delta script generation memory
    {
        let (basis, modified) = create_test_data(10 * 1024 * 1024, 10);
        let index = build_signature_index(&basis, None);

        group.bench_function("delta_generation_10MB", |b| {
            b.iter(|| {
                let script = DeltaGenerator::new()
                    .with_buffer_len(256 * 1024)
                    .generate(black_box(modified.as_slice()), &index);
                black_box(script)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_signature_generation,
        bench_block_matching,
        bench_hash_table_lookup,
        bench_rolling_checksum,
        bench_strong_checksum,
        bench_memory_patterns
);

criterion_main!(benches);
