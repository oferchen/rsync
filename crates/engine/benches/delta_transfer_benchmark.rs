//! Criterion benchmark suite for the delta transfer path.
//!
//! Run with: `cargo bench -p engine --bench delta_transfer_benchmark`
//!
//! Benchmarks:
//! 1. Signature generation - block signatures for 1KB, 64KB, 1MB, 16MB files
//! 2. Delta computation - deltas between files at 90%, 50%, and 0% similarity
//! 3. Delta application - applying computed deltas to reconstruct files
//! 4. End-to-end delta transfer - full signature -> match -> delta -> apply pipeline

use std::io::Cursor;
use std::num::{NonZeroU8, NonZeroU32};

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use matching::{DeltaSignatureIndex, apply_delta, generate_delta};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// File sizes used across benchmark groups.
const SIZE_1KB: usize = 1_024;
const SIZE_64KB: usize = 64 * 1_024;
const SIZE_1MB: usize = 1_024 * 1_024;
const SIZE_16MB: usize = 16 * 1_024 * 1_024;

/// All parametric file sizes with human-readable labels.
const FILE_SIZES: &[(&str, usize)] = &[
    ("1KB", SIZE_1KB),
    ("64KB", SIZE_64KB),
    ("1MB", SIZE_1MB),
    ("16MB", SIZE_16MB),
];

/// Similarity levels for delta computation benchmarks.
/// The value is the percentage of bytes that are *changed* (not similar).
const SIMILARITY_LEVELS: &[(&str, u8)] = &[
    ("90pct_similar", 10), // 10% changed = 90% similar
    ("50pct_similar", 50), // 50% changed = 50% similar
    ("0pct_similar", 100), // 100% changed = 0% similar
];

/// Creates a deterministic data buffer of the given size.
fn generate_basis(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Creates a pair of (basis, modified) buffers where `change_percent` of the
/// modified bytes differ from the basis.
fn create_test_pair(size: usize, change_percent: u8) -> (Vec<u8>, Vec<u8>) {
    let basis = generate_basis(size);
    let mut modified = basis.clone();

    let change_count = (size as f64 * (change_percent as f64 / 100.0)) as usize;
    let seed = change_percent as usize * 7919;
    for i in 0..change_count {
        let pos = (seed + i * 31337) % size;
        modified[pos] = modified[pos].wrapping_add(128);
    }

    (basis, modified)
}

/// Builds a [`DeltaSignatureIndex`] from raw data using the given algorithm
/// and optional explicit block size.
fn build_index(
    data: &[u8],
    block_size: Option<u32>,
    algorithm: SignatureAlgorithm,
) -> DeltaSignatureIndex {
    let strong_sum_len = match algorithm {
        SignatureAlgorithm::Md4
        | SignatureAlgorithm::Md4Seeded { .. }
        | SignatureAlgorithm::Md5 { .. } => 16,
        SignatureAlgorithm::Xxh3 { .. } | SignatureAlgorithm::Xxh64 { .. } => 8,
        SignatureAlgorithm::Xxh3_128 { .. } | SignatureAlgorithm::Sha1 => 16,
    };
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        block_size.and_then(NonZeroU32::new),
        ProtocolVersion::NEWEST,
        NonZeroU8::new(strong_sum_len).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature = generate_file_signature(data, layout, algorithm).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, algorithm).expect("index")
}

// ---------------------------------------------------------------------------
// 1. Signature generation
// ---------------------------------------------------------------------------

/// Benchmarks block signature generation for 1KB, 64KB, 1MB, and 16MB files.
fn bench_signature_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("signature_generation");

    for &(label, size) in FILE_SIZES {
        let data = generate_basis(size);

        group.throughput(Throughput::Bytes(size as u64));

        // MD4 (legacy protocol default)
        group.bench_with_input(BenchmarkId::new("md4", label), &data, |b, data| {
            b.iter(|| {
                let params = SignatureLayoutParams::new(
                    data.len() as u64,
                    None,
                    ProtocolVersion::NEWEST,
                    NonZeroU8::new(16).unwrap(),
                );
                let layout = calculate_signature_layout(params).expect("layout");
                black_box(
                    generate_file_signature(
                        black_box(data.as_slice()),
                        layout,
                        SignatureAlgorithm::Md4,
                    )
                    .expect("signature"),
                )
            });
        });

        // XXH3 (modern fast path)
        group.bench_with_input(BenchmarkId::new("xxh3", label), &data, |b, data| {
            b.iter(|| {
                let params = SignatureLayoutParams::new(
                    data.len() as u64,
                    None,
                    ProtocolVersion::NEWEST,
                    NonZeroU8::new(8).unwrap(),
                );
                let layout = calculate_signature_layout(params).expect("layout");
                black_box(
                    generate_file_signature(
                        black_box(data.as_slice()),
                        layout,
                        SignatureAlgorithm::Xxh3 { seed: 0 },
                    )
                    .expect("signature"),
                )
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// 2. Delta computation
// ---------------------------------------------------------------------------

/// Benchmarks delta computation between files at 90%, 50%, and 0% similarity
/// across all four file sizes.
fn bench_delta_computation(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_computation");

    for &(size_label, size) in FILE_SIZES {
        for &(sim_label, change_pct) in SIMILARITY_LEVELS {
            let (basis, modified) = create_test_pair(size, change_pct);
            let index = build_index(&basis, None, SignatureAlgorithm::Md4);

            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::new(sim_label, size_label),
                &(&modified, &index),
                |b, &(modified, index)| {
                    b.iter(|| {
                        black_box(
                            generate_delta(black_box(modified.as_slice()), index).expect("delta"),
                        )
                    });
                },
            );
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Delta application
// ---------------------------------------------------------------------------

/// Benchmarks applying pre-computed deltas to reconstruct target files.
///
/// Each combination of file size and similarity level is measured. The delta
/// script is computed once during setup so only the apply step is timed.
fn bench_delta_application(c: &mut Criterion) {
    let mut group = c.benchmark_group("delta_application");

    for &(size_label, size) in FILE_SIZES {
        for &(sim_label, change_pct) in SIMILARITY_LEVELS {
            let (basis, modified) = create_test_pair(size, change_pct);
            let index = build_index(&basis, None, SignatureAlgorithm::Md4);
            let script = generate_delta(modified.as_slice(), &index).expect("delta");

            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::new(sim_label, size_label),
                &(&basis, &index, &script),
                |b, &(basis, index, script)| {
                    b.iter(|| {
                        let mut cursor = Cursor::new(black_box(basis.as_slice()));
                        let mut output = Vec::with_capacity(basis.len());
                        apply_delta(&mut cursor, &mut output, index, script).expect("apply");
                        black_box(output)
                    });
                },
            );
        }
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// 4. End-to-end delta transfer
// ---------------------------------------------------------------------------

/// Benchmarks the complete delta transfer pipeline:
/// signature generation -> index construction -> delta generation -> delta application.
///
/// This measures the full cost a receiver/sender pair would incur for a single
/// file update, exercised at every (size x similarity) combination.
fn bench_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end_delta_transfer");

    for &(size_label, size) in FILE_SIZES {
        for &(sim_label, change_pct) in SIMILARITY_LEVELS {
            let (basis, modified) = create_test_pair(size, change_pct);

            group.throughput(Throughput::Bytes(size as u64));
            group.bench_with_input(
                BenchmarkId::new(sim_label, size_label),
                &(basis, modified),
                |b, (basis, modified)| {
                    b.iter(|| {
                        // Step 1: Generate block signatures from basis file
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

                        // Step 2: Build delta signature index (hash table)
                        let index = DeltaSignatureIndex::from_signature(
                            &signature,
                            SignatureAlgorithm::Md4,
                        )
                        .expect("index");

                        // Step 3: Compute delta script against modified file
                        let script =
                            generate_delta(black_box(modified.as_slice()), &index).expect("delta");

                        // Step 4: Apply delta to reconstruct the target
                        let mut cursor = Cursor::new(basis.as_slice());
                        let mut output = Vec::with_capacity(modified.len());
                        apply_delta(&mut cursor, &mut output, &index, &script).expect("apply");

                        black_box(output)
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(std::time::Duration::from_secs(5));
    targets =
        bench_signature_generation,
        bench_delta_computation,
        bench_delta_application,
        bench_end_to_end
);

criterion_main!(benches);
