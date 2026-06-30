//! Scaling benchmark for the parallel sender-scan delta generator.
//!
//! Compares the sequential [`DeltaGenerator::generate`] against the parallel
//! [`DeltaGenerator::generate_chunked`] at increasing range counts, on a
//! large delta workload (mostly-copy, the common rsync case). The parallel
//! path's wall-clock speedup saturates at the rayon pool size; pin it with
//! `RAYON_NUM_THREADS=N` across runs to trace the per-core scaling curve:
//!
//! ```sh
//! for n in 1 2 4 8; do
//!   RAYON_NUM_THREADS=$n cargo bench -p matching --bench parallel_delta_scan
//! done
//! ```

use std::hint::black_box;
use std::io::Cursor;
use std::num::NonZeroU8;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use matching::{DeltaGenerator, DeltaSignatureIndex};
use protocol::ProtocolVersion;
use signature::{
    SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout, generate_file_signature,
};

/// Deterministic pseudo-random fill (xorshift64) so basis blocks are distinct,
/// exercising the real hash-probe + MD4-verify path rather than a degenerate
/// all-duplicate index.
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn build_index(data: &[u8]) -> DeltaSignatureIndex {
    let params = SignatureLayoutParams::new(
        data.len() as u64,
        None,
        ProtocolVersion::NEWEST,
        NonZeroU8::new(16).unwrap(),
    );
    let layout = calculate_signature_layout(params).expect("layout");
    let signature =
        generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
    DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
}

/// Basis with `change_count` scattered single-byte edits - a mostly-copy delta.
fn scattered_edits(size: usize, seed: u64, change_count: usize) -> Vec<u8> {
    let mut data = pseudo_random(size, seed);
    if change_count > 0 {
        let spacing = size / change_count;
        for i in 0..change_count {
            let pos = (i * spacing).min(size - 1);
            data[pos] ^= 0xff;
        }
    }
    data
}

fn bench_parallel_delta_scan(c: &mut Criterion) {
    const SIZE: usize = 256 * 1024 * 1024; // 256 MiB

    let basis = pseudo_random(SIZE, 0x1234_5678);
    let index = build_index(&basis);

    // 64 scattered single-byte edits across 256 MiB: ~99.6% of basis blocks
    // still match, so the scan is dominated by per-block rolling+MD4 work (the
    // real sender cost) rather than a pathological all-literal byte walk. The
    // edits must be far sparser than the block length, or every block is
    // dirtied and nothing matches.
    let source = scattered_edits(SIZE, 0x1234_5678, 64);
    let generator = DeltaGenerator::new();

    let mut group = c.benchmark_group("parallel_delta_scan_256MiB_mostly_copy");
    group.throughput(Throughput::Bytes(SIZE as u64));
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(10));

    group.bench_function("sequential", |b| {
        b.iter(|| {
            let script = generator.generate(Cursor::new(black_box(source.as_slice())), &index);
            black_box(script)
        });
    });

    for max_chunks in [2usize, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("chunked", max_chunks),
            &max_chunks,
            |b, &max_chunks| {
                b.iter(|| {
                    let script = generator.generate_chunked(
                        black_box(source.as_slice()),
                        &index,
                        max_chunks,
                    );
                    black_box(script)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default();
    targets = bench_parallel_delta_scan
);

criterion_main!(benches);
