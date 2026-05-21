//! Criterion bench: cores-vs-throughput sweep for
//! [`ParallelDeltaApplier`]'s rayon-fanned `verify_chunk` step (BR-3i.f).
//!
//! # Why this exists
//!
//! BR-3i.c (PR #4640) replaced the length-only verify stub with a real
//! `strategy.compute(&chunk.data)` comparison against the producer's
//! `expected_strong` digest. That means the per-chunk verify path now has
//! real CPU cost - and the rayon fan-out across workers is what amortises
//! it. The promotion question for BR-3i is whether the parallel verify
//! step scales linearly with worker count, sub-linearly, or saturates
//! early; this bench produces the curve so the answer is data-driven.
//!
//! It complements `parallel_receive_delta_perf` (which compares
//! parallel-vs-sequential apply at the full applier shape) by sweeping a
//! single applier shape across three axes:
//!
//! 1. Worker count - `{1, 2, 4, max(available_parallelism, 8)}`.
//! 2. Checksum strategy - `{MD4, MD5, XXH3}` (the algorithms BR-3i.b
//!    plumbs through the selector).
//! 3. Workload shape - large chunks / few files vs small chunks / many
//!    files (see below).
//!
//! # Workloads
//!
//! - **Workload A** - large chunks: 4 files x 256 chunks x 1 MiB.
//!   Total: 1024 chunks, 1 GiB. Models VM images and container layers
//!   where each chunk dominates worker time and dispatch overhead is
//!   negligible.
//! - **Workload B** - small chunks: 256 files x 64 chunks x 16 KiB.
//!   Total: 16384 chunks, 256 MiB. Models source trees and build
//!   artefact directories where dispatch overhead and per-chunk fixed
//!   cost both bite.
//!
//! # Sink writer
//!
//! Each registered file gets an in-memory `Vec<u8>` sink so the bench
//! isolates the verify+reorder+write path from disk I/O. Real disk costs
//! are covered by the `delta_transfer_benchmark` and `local_copy_bench`
//! harnesses.
//!
//! # Reproducibility
//!
//! All chunk payloads come from a seeded [`SmallRng`]; the seed varies
//! by `(workload, file, chunk)` so the data is incompressible-looking
//! but byte-identical across runs and machines. Per-chunk
//! `expected_strong` digests are computed from each strategy ahead of
//! time so the timed loop never recomputes them.
//!
//! # Cross-references
//!
//! - PR #4640 - BR-3i.c real `verify_chunk` (prerequisite shipped).
//! - PR #4616 - BR-3i.b strategy plumbing.
//! - PR #4634 - BR-3j.c DashMap migration (this harness re-uses the
//!   same shape for BR-3j.f follow-up measurements).
//! - `crates/engine/src/concurrent_delta/parallel_apply.rs` - implementation
//!   under measurement.
//!
//! Run: `cargo bench -p engine --features parallel-receive-delta \
//!     --bench parallel_verify_chunk`

#![deny(unsafe_code)]
#![cfg(feature = "parallel-receive-delta")]

use std::hint::black_box;
use std::io::{self, Write};
use std::sync::Arc;
use std::thread::available_parallelism;
use std::time::Duration;

use checksums::strong::strategy::{
    ChecksumAlgorithmKind, ChecksumStrategy, ChecksumStrategySelector,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rayon::ThreadPoolBuilder;

use engine::concurrent_delta::{DeltaChunk, FileNdx, ParallelDeltaApplier};

/// Fixed RNG seed root. Combined with `(workload, file, chunk)` per
/// payload so identical runs reproduce byte-for-byte.
const SEED_ROOT: u64 = 0xB31F_BEDC_5EE0_5EED;

/// Workload A: large chunks, few files. 4 files x 256 chunks x 1 MiB.
const WORKLOAD_A_FILES: usize = 4;
const WORKLOAD_A_CHUNKS_PER_FILE: usize = 256;
const WORKLOAD_A_CHUNK_SIZE: usize = 1024 * 1024;

/// Workload B: small chunks, many files. 256 files x 64 chunks x 16 KiB.
const WORKLOAD_B_FILES: usize = 256;
const WORKLOAD_B_CHUNKS_PER_FILE: usize = 64;
const WORKLOAD_B_CHUNK_SIZE: usize = 16 * 1024;

/// In-memory sink that discards bytes after acknowledging the write.
///
/// The bench only measures verify+dispatch+per-file mutex cost. The
/// applier already tracks `bytes_written` through its own counter, so
/// the sink does not need its own bookkeeping; keeping `write` trivial
/// (no allocation, no atomic op) avoids contaminating the
/// cores-vs-throughput curve with allocator or shared-state pressure.
struct CountingSink;

impl Write for CountingSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One named workload: a label plus the materialised chunk list and the
/// total byte count the bench needs to report throughput.
struct Workload {
    label: &'static str,
    file_count: u32,
    chunks: Vec<DeltaChunk>,
    total_bytes: u64,
}

/// Builds a workload's chunk list with deterministic, RNG-filled payloads.
///
/// `workload_tag` is folded into the per-chunk seed so workload A and B
/// do not share payloads (and so a future workload C cannot accidentally
/// collide).
fn build_workload(
    label: &'static str,
    workload_tag: u64,
    files: usize,
    chunks_per_file: usize,
    chunk_size: usize,
) -> Workload {
    let mut chunks = Vec::with_capacity(files * chunks_per_file);
    for file_index in 0..files {
        for chunk_index in 0..chunks_per_file {
            let seed = SEED_ROOT
                ^ workload_tag.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (file_index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
                ^ (chunk_index as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut payload = vec![0u8; chunk_size];
            rng.fill_bytes(&mut payload);
            let ndx = FileNdx::new(file_index as u32);
            chunks.push(DeltaChunk::literal(ndx, chunk_index as u64, payload));
        }
    }
    let total_bytes = (files as u64) * (chunks_per_file as u64) * (chunk_size as u64);
    Workload {
        label,
        file_count: files as u32,
        chunks,
        total_bytes,
    }
}

/// Attaches the strategy-specific `expected_strong` digest to every chunk
/// so the timed loop exercises the real `compute + compare` path. The
/// digest computation runs outside the criterion sample window.
fn with_expected_digests(
    chunks: &[DeltaChunk],
    strategy: &dyn ChecksumStrategy,
) -> Vec<DeltaChunk> {
    chunks
        .iter()
        .map(|c| {
            let digest = strategy.compute(&c.data);
            DeltaChunk::literal(c.ndx, c.chunk_sequence, c.data.clone())
                .with_expected_strong(digest)
        })
        .collect()
}

/// Worker counts swept per (workload, strategy) cell. Always includes 1,
/// 2, 4, 8 and the machine's reported parallelism (deduplicated and
/// sorted) so 4- and 16-core hosts both produce a meaningful curve.
fn worker_counts() -> Vec<usize> {
    let host = available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut counts = vec![1usize, 2, 4, 8, host];
    counts.sort_unstable();
    counts.dedup();
    counts
}

/// Strategies covered by the sweep. Mirrors BR-3i.b's supported set so
/// the bench will fail to compile if a future change drops one of them.
fn strategies() -> Vec<(&'static str, ChecksumAlgorithmKind)> {
    vec![
        ("md4", ChecksumAlgorithmKind::Md4),
        ("md5", ChecksumAlgorithmKind::Md5),
        ("xxh3", ChecksumAlgorithmKind::Xxh3),
    ]
}

/// Runs one apply pass through the supplied applier, returning the total
/// bytes routed to sinks. The applier is rebuilt per iteration so each
/// sample starts from a clean DashMap shape.
fn run_apply(workload: &Workload, applier: &ParallelDeltaApplier) -> u64 {
    for i in 0..workload.file_count {
        applier
            .register_file(FileNdx::new(i), Box::new(CountingSink))
            .expect("register sink");
    }
    applier
        .apply_batch_parallel(workload.chunks.clone())
        .expect("batch apply");
    let mut total = 0u64;
    for i in 0..workload.file_count {
        total += applier
            .bytes_written(FileNdx::new(i))
            .expect("bytes written");
        let _ = applier.finish_file(FileNdx::new(i)).expect("finish file");
    }
    total
}

/// Drives the cores-vs-throughput sweep for a single workload. Each cell
/// pins the applier's concurrency limit AND the ambient rayon pool to the
/// target worker count so the dispatch surface is genuinely thread-bound,
/// not just nominally limited.
fn bench_workload_sweep(c: &mut Criterion, workload: Workload) {
    let mut group = c.benchmark_group(format!("parallel_verify_chunk/{}", workload.label));
    group.throughput(Throughput::Bytes(workload.total_bytes));
    // Each iteration walks the full chunk list (potentially 1 GiB for
    // workload A). Keep the sample budget modest so the matrix finishes
    // in a reasonable wall-clock window; users who want tighter
    // confidence intervals can override with criterion's CLI flags.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(
        if workload.label == "large_chunks_few_files" {
            12
        } else {
            6
        },
    ));

    for (strategy_label, kind) in strategies() {
        let strategy: Arc<dyn ChecksumStrategy> =
            Arc::from(ChecksumStrategySelector::for_algorithm(kind, 0));
        let prepared_chunks = with_expected_digests(&workload.chunks, strategy.as_ref());
        let prepared_workload = Workload {
            label: workload.label,
            file_count: workload.file_count,
            chunks: prepared_chunks,
            total_bytes: workload.total_bytes,
        };
        let chunks_per_iter = prepared_workload.chunks.len() as u64;

        for &workers in &worker_counts() {
            let pool = ThreadPoolBuilder::new()
                .num_threads(workers)
                .thread_name(|i| format!("verify-chunk-bench-{i}"))
                .build()
                .expect("rayon pool");

            let id = BenchmarkId::new(
                format!("{strategy_label}/threads={workers}"),
                workload.label,
            );
            group.bench_with_input(id, &workers, |b, _| {
                b.iter(|| {
                    let strategy_clone = Arc::clone(&strategy);
                    let applier = ParallelDeltaApplier::with_strategy(workers, strategy_clone);
                    let bytes = pool.install(|| run_apply(&prepared_workload, &applier));
                    // Group throughput reports bytes/sec. Chunks/sec is
                    // derivable post-hoc from `chunks_per_iter` and the
                    // reported per-iter wall time; the `black_box` here
                    // pins the chunk count into the timed region so the
                    // optimiser cannot fold it away.
                    black_box((bytes, chunks_per_iter));
                });
            });
        }
    }

    group.finish();
}

fn bench_parallel_verify_chunk(c: &mut Criterion) {
    let workload_a = build_workload(
        "large_chunks_few_files",
        0xA,
        WORKLOAD_A_FILES,
        WORKLOAD_A_CHUNKS_PER_FILE,
        WORKLOAD_A_CHUNK_SIZE,
    );
    let workload_b = build_workload(
        "small_chunks_many_files",
        0xB,
        WORKLOAD_B_FILES,
        WORKLOAD_B_CHUNKS_PER_FILE,
        WORKLOAD_B_CHUNK_SIZE,
    );
    bench_workload_sweep(c, workload_a);
    bench_workload_sweep(c, workload_b);
}

criterion_group!(benches, bench_parallel_verify_chunk);
criterion_main!(benches);
