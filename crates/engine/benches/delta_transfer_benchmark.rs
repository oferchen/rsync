//! Benchmarks for delta transfer efficiency.
//!
//! Run with: `cargo bench -p engine --bench delta_transfer_benchmark`
//!
//! This benchmark suite measures end-to-end delta transfer performance:
//! 1. Full delta pipeline (signature → index → delta → apply)
//! 2. Transfer efficiency metrics (delta size vs original size)
//! 3. Block size impact on compression ratio

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use matching::{DeltaSignatureIndex, apply_delta, generate_delta};
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
fn create_scattered_changes(
    size: usize,
    num_changes: usize,
    change_size: usize,
) -> (Vec<u8>, Vec<u8>) {
    let basis: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let mut modified = basis.clone();

    let spacing = size / (num_changes + 1);
    for i in 0..num_changes {
        let start = (i + 1) * spacing;
        let end = (start + change_size).min(size);
        for byte in modified.iter_mut().skip(start).take(end - start) {
            *byte = byte.wrapping_add(1);
        }
    }

    (basis, modified)
}

/// Builds a signature index from basis data with optional block size.
fn build_signature_index(
    data: &[u8],
    block_size: Option<u32>,
    algorithm: SignatureAlgorithm,
) -> DeltaSignatureIndex {
    // Determine the appropriate strong checksum length based on algorithm
    let strong_sum_length = match algorithm {
        SignatureAlgorithm::Md4 | SignatureAlgorithm::Md5 { .. } => 16,
        SignatureAlgorithm::Xxh3 { .. } | SignatureAlgorithm::Xxh64 { .. } => 8,
        SignatureAlgorithm::Xxh3_128 { .. } | SignatureAlgorithm::Sha1 => 16,
    };

    let params = SignatureLayoutParams::new(
        data.len() as u64,
        block_size.and_then(NonZeroU32::new),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(strong_sum_length).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data, layout, algorithm).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, algorithm).expect("index")
}

// ============================================================================
// Full Delta Pipeline Benchmarks
// ============================================================================

/// Benchmarks the complete delta transfer pipeline:
/// signature generation → index construction → delta generation → delta application
fn bench_full_delta_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_delta_pipeline");

    // Test scenarios with different file sizes and change percentages
    let scenarios = [
        ("1MB_1pct", 1_024 * 1_024, 1),
        ("1MB_10pct", 1_024 * 1_024, 10),
        ("1MB_50pct", 1_024 * 1_024, 50),
        ("10MB_1pct", 10 * 1_024 * 1_024, 1),
    ];

    for (name, size, change_percent) in scenarios {
        let (basis, modified) = create_test_data(size, change_percent);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("md4", name),
            &(basis, modified),
            |b, (basis, modified)| {
                b.iter(|| {
                    // Step 1: Generate signature from basis
                    let params = SignatureLayoutParams::new(
                        basis.len() as u64,
                        None,
                        ProtocolVersion::NEWEST,
                        NonZeroU8::new(16).unwrap(),
                    );
                    let layout = calculate_signature_layout(params).expect("layout");
                    let signature = generate_file_signature(
                        black_box(basis.as_slice()),
                        layout,
                        SignatureAlgorithm::Md4,
                    )
                    .expect("signature");

                    // Step 2: Build delta index
                    let index =
                        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
                            .expect("index");

                    // Step 3: Generate delta
                    let script =
                        generate_delta(black_box(modified.as_slice()), &index).expect("script");

                    // Step 4: Apply delta to reconstruct
                    let mut basis_cursor = Cursor::new(basis.as_slice());
                    let mut output = Vec::new();
                    apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");

                    black_box(output)
                });
            },
        );
    }

    // 10MB file with scattered changes
    {
        let (basis, modified) = create_scattered_changes(10 * 1_024 * 1_024, 100, 1024);
        group.throughput(Throughput::Bytes(basis.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("md4", "10MB_scattered"),
            &(basis, modified),
            |b, (basis, modified)| {
                b.iter(|| {
                    let params = SignatureLayoutParams::new(
                        basis.len() as u64,
                        None,
                        ProtocolVersion::NEWEST,
                        NonZeroU8::new(16).unwrap(),
                    );
                    let layout = calculate_signature_layout(params).expect("layout");
                    let signature = generate_file_signature(
                        black_box(basis.as_slice()),
                        layout,
                        SignatureAlgorithm::Md4,
                    )
                    .expect("signature");
                    let index =
                        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
                            .expect("index");
                    let script =
                        generate_delta(black_box(modified.as_slice()), &index).expect("script");
                    let mut basis_cursor = Cursor::new(basis.as_slice());
                    let mut output = Vec::new();
                    apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
                    black_box(output)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Transfer Efficiency Metrics
// ============================================================================

/// Measures the efficiency of delta encoding by calculating compression ratios.
///
/// This benchmark generates deltas and compares:
/// - Delta size (literal bytes) vs original file size
/// - Ratio of matched blocks vs total blocks
fn bench_transfer_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("transfer_efficiency");

    let scenarios = [
        ("1MB_1pct", 1_024 * 1_024, 1),
        ("1MB_10pct", 1_024 * 1_024, 10),
        ("1MB_50pct", 1_024 * 1_024, 50),
        ("10MB_1pct", 10 * 1_024 * 1_024, 1),
    ];

    for (name, size, change_percent) in scenarios {
        let (basis, modified) = create_test_data(size, change_percent);
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Md4);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("delta_generation", name),
            &(modified, index),
            |b, (modified, index)| {
                b.iter(|| {
                    let script =
                        generate_delta(black_box(modified.as_slice()), index).expect("script");

                    // Calculate compression metrics
                    let delta_size = script.literal_bytes();
                    let original_size = modified.len() as u64;
                    let compression_ratio = if original_size > 0 {
                        (delta_size as f64 / original_size as f64) * 100.0
                    } else {
                        0.0
                    };

                    black_box((script, delta_size, compression_ratio))
                });
            },
        );
    }

    // Compare whole-file transfer vs delta transfer
    {
        let (basis, modified) = create_test_data(1_024 * 1_024, 10);
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Md4);

        group.bench_function("whole_file_1MB", |b| {
            b.iter(|| {
                // Simulate whole file transfer (just copy the bytes)
                let transferred = black_box(modified.clone());
                black_box(transferred.len())
            });
        });

        group.bench_function("delta_transfer_1MB_10pct", |b| {
            b.iter(|| {
                // Generate delta
                let script =
                    generate_delta(black_box(modified.as_slice()), &index).expect("script");
                // Simulate transferring only literal bytes
                let transferred_bytes = script.literal_bytes();
                black_box(transferred_bytes)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Block Size Impact
// ============================================================================

/// Benchmarks how different block sizes affect delta generation performance
/// and compression ratio.
fn bench_block_size_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_size_impact");

    let size = 1_024 * 1_024; // 1MB
    let change_percent = 10;
    let (basis, modified) = create_test_data(size, change_percent);

    let block_sizes = [512, 1024, 2048, 4096];

    for block_size in block_sizes {
        let index = build_signature_index(&basis, Some(block_size), SignatureAlgorithm::Md4);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("delta_gen", format!("{block_size}B")),
            &(modified.as_slice(), &index),
            |b, (modified, index)| {
                b.iter(|| {
                    let script = generate_delta(black_box(*modified), index).expect("script");
                    black_box(script)
                });
            },
        );
    }

    // Measure compression ratio for different block sizes
    group.bench_function("compression_ratio_comparison", |b| {
        b.iter(|| {
            let mut results = Vec::new();

            for block_size in block_sizes {
                let index =
                    build_signature_index(&basis, Some(block_size), SignatureAlgorithm::Md4);
                let script = generate_delta(modified.as_slice(), &index).expect("script");

                let literal_bytes = script.literal_bytes();
                let total_bytes = script.total_bytes();
                let compression_ratio = if total_bytes > 0 {
                    (literal_bytes as f64 / total_bytes as f64) * 100.0
                } else {
                    0.0
                };

                results.push((block_size, literal_bytes, compression_ratio));
            }

            black_box(results)
        });
    });

    group.finish();
}

// ============================================================================
// Algorithm Comparison
// ============================================================================

/// Compares MD4 vs XXH3 signature algorithms for delta generation.
fn bench_algorithm_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("algorithm_comparison");

    let size = 10 * 1_024 * 1_024; // 10MB
    let change_percent = 1;
    let (basis, modified) = create_test_data(size, change_percent);

    // MD4 algorithm
    {
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Md4);

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function("md4_10MB_1pct", |b| {
            b.iter(|| {
                let script =
                    generate_delta(black_box(modified.as_slice()), &index).expect("script");
                black_box(script)
            });
        });
    }

    // XXH3 algorithm
    {
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Xxh3 { seed: 0 });

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function("xxh3_10MB_1pct", |b| {
            b.iter(|| {
                let script =
                    generate_delta(black_box(modified.as_slice()), &index).expect("script");
                black_box(script)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Delta Application Performance
// ============================================================================

/// Benchmarks the performance of applying deltas to reconstruct files.
fn bench_delta_application(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_application");

    let scenarios = [
        ("1MB_1pct", 1_024 * 1_024, 1),
        ("1MB_50pct", 1_024 * 1_024, 50),
        ("10MB_1pct", 10 * 1_024 * 1_024, 1),
    ];

    for (name, size, change_percent) in scenarios {
        let (basis, modified) = create_test_data(size, change_percent);
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Md4);
        let script = generate_delta(modified.as_slice(), &index).expect("script");

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("apply_delta", name),
            &(basis, script, index),
            |b, (basis, script, index)| {
                b.iter(|| {
                    let mut basis_cursor = Cursor::new(black_box(basis.as_slice()));
                    let mut output = Vec::new();
                    apply_delta(&mut basis_cursor, &mut output, index, script).expect("apply");
                    black_box(output)
                });
            },
        );
    }

    group.finish();
}

// ============================================================================
// Memory Usage Benchmarks
// ============================================================================

/// Benchmarks memory allocation patterns during delta operations.
fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    let size = 10 * 1_024 * 1_024; // 10MB
    let change_percent = 10;
    let (basis, modified) = create_test_data(size, change_percent);

    // Signature generation memory
    group.bench_function("signature_generation_10MB", |b| {
        b.iter(|| {
            let params = SignatureLayoutParams::new(
                basis.len() as u64,
                None,
                ProtocolVersion::NEWEST,
                NonZeroU8::new(16).unwrap(),
            );
            let layout = calculate_signature_layout(params).expect("layout");
            let signature = generate_file_signature(
                black_box(basis.as_slice()),
                layout,
                SignatureAlgorithm::Md4,
            )
            .expect("signature");
            black_box(signature)
        });
    });

    // Index construction memory
    {
        let params = SignatureLayoutParams::new(
            basis.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature = generate_file_signature(basis.as_slice(), layout, SignatureAlgorithm::Md4)
            .expect("signature");

        group.bench_function("index_construction_10MB", |b| {
            b.iter(|| {
                let index = DeltaSignatureIndex::from_signature(
                    black_box(&signature),
                    SignatureAlgorithm::Md4,
                );
                black_box(index)
            });
        });
    }

    // Delta script generation memory
    {
        let index = build_signature_index(&basis, None, SignatureAlgorithm::Md4);

        group.bench_function("delta_script_generation_10MB", |b| {
            b.iter(|| {
                let script =
                    generate_delta(black_box(modified.as_slice()), &index).expect("script");
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
        bench_full_delta_pipeline,
        bench_transfer_efficiency,
        bench_block_size_impact,
        bench_algorithm_comparison,
        bench_delta_application,
        bench_memory_patterns
);

criterion_main!(benches);
