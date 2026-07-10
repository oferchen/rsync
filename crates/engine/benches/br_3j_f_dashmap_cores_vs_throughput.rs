//! Criterion bench: BR-3j.f post-DashMap re-bench of the BR-3i.f
//! cores-vs-throughput sweep for [`ParallelDeltaApplier`].
//!
//! # Why this exists
//!
//! BR-3j.c/d/e (PRs #4634/#4635/#4636) replaced the applier's outer
//! `Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>` slot map with a
//! [`DashMap`]-backed shard layout. The original BR-3i.f harness in
//! `parallel_verify_chunk.rs` was authored against the pre-DashMap shape
//! and produced the baseline cores-vs-throughput curve the receiver's
//! parallel-vs-sequential gate (#4666) was sized against. BR-3j.f re-runs
//! the same workload through the new applier so the curve can be
//! captured after the outer-lock removal.
//!
//! This bench is a deliberate sibling of `parallel_verify_chunk.rs`
//! rather than an in-place edit: keeping the BR-3i.f harness untouched
//! means the pre-DashMap criterion baseline saved under
//! `target/criterion/parallel_verify_chunk/` stays comparable to the
//! BR-3j.f run saved under `target/criterion/br_3j_f_dashmap_cores_vs_throughput/`.
//! Criterion's compare-to-baseline workflow then yields a direct
//! before/after diff per (workload, strategy, worker_count) cell with no
//! manual bookkeeping.
//!
//! # Sweep shape
//!
//! Identical to BR-3i.f so the cells line up cell-for-cell:
//!
//! 1. Worker count - `{1, 2, 4, 8, 16, available_parallelism()}` deduplicated.
//! 2. Checksum strategy - `{MD4, MD5, XXH3}`.
//! 3. Workload shape - large_chunks_few_files vs small_chunks_many_files.
//!
//! # Outer-map probe
//!
//! BR-3j.f also adds a dedicated `register_finish_churn` group that
//! exercises the path the DashMap migration most directly affects: many
//! short-lived files being registered, immediately drained, and finished
//! across a large rayon pool. This is the path the pre-DashMap
//! single-mutex map serialised end-to-end; the bench surfaces whether the
//! shard layout actually lets N workers register/finish in parallel or
//! whether some other lock (e.g. the per-file slot mutex) is now the
//! gate. Numbers from this group complement - they do not replace - the
//! main cores-vs-throughput sweep.
//!
//! # Concurrent dispatch probe
//!
//! The `concurrent_dispatch` group drives `apply_one_chunk` from N rayon
//! workers in parallel, each targeting a distinct file. Every call
//! independently acquires and releases a DashMap shard guard, so the
//! bench amplifies the number of shard operations per iteration relative
//! to `apply_batch_parallel`. Linear throughput scaling as workers grow
//! confirms the shard layout is not the bottleneck; sub-linear scaling
//! points at per-file mutex or hash-distribution contention.
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
//! - PR #4634 - BR-3j.c DashMap migration under audit by this re-bench.
//! - PR #4653 - BR-3i.f baseline harness in `parallel_verify_chunk.rs`.
//! - `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` -
//!   DashMap-vs-sharded selection audit; cells in this bench validate
//!   the audit's "shard guard is short, never iterated in hot path"
//!   assumptions on real workloads.
//! - `docs/design/br-3j-f-dashmap-rebench-2026-05-21.md` - methodology,
//!   number-capture procedure, and the deferred-numbers status.
//! - `crates/engine/src/concurrent_delta/parallel_apply.rs` - the
//!   implementation under measurement.
//!
//! Run: `cargo bench -p engine --bench br_3j_f_dashmap_cores_vs_throughput`

#![deny(unsafe_code)]

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
/// payload so identical runs reproduce byte-for-byte. Distinct from the
/// BR-3i.f sibling's root so post-hoc analyses cannot accidentally
/// alias the two corpora when both criterion baselines are loaded.
const SEED_ROOT: u64 = 0xB33D_BEEF_5EE0_C0DE;

/// Workload A: large chunks, few files. 4 files x 256 chunks x 1 MiB.
/// Matches BR-3i.f exactly so the re-bench delta is a like-for-like.
const WORKLOAD_A_FILES: usize = 4;
const WORKLOAD_A_CHUNKS_PER_FILE: usize = 256;
const WORKLOAD_A_CHUNK_SIZE: usize = 1024 * 1024;

/// Workload B: small chunks, many files. 256 files x 64 chunks x 16 KiB.
/// Matches BR-3i.f exactly so the re-bench delta is a like-for-like.
const WORKLOAD_B_FILES: usize = 256;
const WORKLOAD_B_CHUNKS_PER_FILE: usize = 64;
const WORKLOAD_B_CHUNK_SIZE: usize = 16 * 1024;

/// Register/finish churn workload C: many files, one chunk each.
///
/// Selected to maximise the share of wall time spent inside
/// `register_file` + `finish_file` so the DashMap shard layout's
/// concurrency is the dominant signal. 4 KiB per chunk keeps the
/// per-chunk write under a single page so the inner per-file mutex
/// window is minimal.
const WORKLOAD_C_FILES: usize = 4096;
const WORKLOAD_C_CHUNKS_PER_FILE: usize = 1;
const WORKLOAD_C_CHUNK_SIZE: usize = 4 * 1024;

/// In-memory sink that discards bytes after acknowledging the write.
///
/// The bench measures verify+dispatch+per-file mutex cost. The applier
/// already tracks `bytes_written` through its own counter, so the sink
/// does not need its own bookkeeping; keeping `write` trivial avoids
/// contaminating the cores-vs-throughput curve with allocator or
/// shared-state pressure.
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
/// `workload_tag` is folded into the per-chunk seed so workloads A, B,
/// and C do not share payloads.
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
/// 2, 4, 8, 16, and the machine's reported parallelism so the curve
/// covers the full range from single-threaded through high core counts.
/// 16 is included unconditionally so the DashMap shard scaling is
/// visible even when the host has fewer physical cores - the rayon pool
/// will over-subscribe, which is the interesting contention regime for
/// the outer map.
fn worker_counts() -> Vec<usize> {
    let host = available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut counts = vec![1usize, 2, 4, 8, 16, host];
    counts.sort_unstable();
    counts.dedup();
    counts
}

/// Strategies covered by the sweep. Mirrors BR-3i.b's supported set so
/// this bench will fail to compile if a future change drops one of them.
fn strategies() -> Vec<(&'static str, ChecksumAlgorithmKind)> {
    vec![
        ("md4", ChecksumAlgorithmKind::Md4),
        ("md5", ChecksumAlgorithmKind::Md5),
        ("xxh3", ChecksumAlgorithmKind::Xxh3),
    ]
}

/// Runs one apply pass through the supplied applier, returning the total
/// bytes routed to sinks. The applier is rebuilt per iteration so each
/// sample starts from a clean DashMap shape - critical for BR-3j.f
/// because the bench's claim of interest is exactly how the DashMap
/// behaves when populated under contention from zero.
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
    let mut group = c.benchmark_group(format!(
        "br_3j_f_dashmap_cores_vs_throughput/{}",
        workload.label
    ));
    group.throughput(Throughput::Bytes(workload.total_bytes));
    // Each iteration walks the full chunk list (potentially 1 GiB for
    // workload A). Keep the sample budget modest so the matrix finishes
    // in a reasonable wall-clock window; offline number-capture runs
    // override with criterion's CLI flags when tighter confidence
    // intervals are needed (see the BR-3j.f design doc).
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
                .thread_name(|i| format!("dashmap-rebench-{i}"))
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

/// Drives the register/finish churn sweep for workload C. The bench
/// measures the wall time of a full register-all + apply-all + finish-all
/// cycle on the DashMap-backed applier; under the pre-DashMap shape this
/// path would serialise every register and finish behind one outer
/// mutex, so the BR-3j.f numbers should show worker scaling that the
/// baseline would not have produced.
///
/// File count is the throughput unit so the criterion report yields
/// files/sec directly; bytes are negligible in this workload.
fn bench_register_finish_churn(c: &mut Criterion, workload: Workload) {
    let mut group = c.benchmark_group(format!(
        "br_3j_f_dashmap_cores_vs_throughput/register_finish_churn/{}",
        workload.label
    ));
    group.throughput(Throughput::Elements(workload.file_count as u64));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));

    // Only MD5 here - this workload is dominated by map ops, not
    // checksum cost, and sweeping three strategies would triple the
    // matrix without changing the signal.
    let strategy: Arc<dyn ChecksumStrategy> = Arc::from(ChecksumStrategySelector::for_algorithm(
        ChecksumAlgorithmKind::Md5,
        0,
    ));
    let prepared_chunks = with_expected_digests(&workload.chunks, strategy.as_ref());
    let prepared_workload = Workload {
        label: workload.label,
        file_count: workload.file_count,
        chunks: prepared_chunks,
        total_bytes: workload.total_bytes,
    };

    for &workers in &worker_counts() {
        let pool = ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|i| format!("dashmap-churn-{i}"))
            .build()
            .expect("rayon pool");

        let id = BenchmarkId::new(format!("md5/threads={workers}"), workload.label);
        group.bench_with_input(id, &workers, |b, _| {
            b.iter(|| {
                let strategy_clone = Arc::clone(&strategy);
                let applier = ParallelDeltaApplier::with_strategy(workers, strategy_clone);
                let files = pool.install(|| run_apply(&prepared_workload, &applier));
                black_box(files);
            });
        });
    }

    group.finish();
}

/// Concurrent dispatch contention benchmark.
///
/// Exercises the DashMap lookup hot path by dispatching single chunks to
/// distinct files from N rayon workers simultaneously. Under the
/// pre-DashMap `Mutex<HashMap>` layout every dispatch serialised behind
/// one lock regardless of file identity; the DashMap layout partitions
/// the keyspace across shards so lookups to distinct files run in
/// parallel. This bench makes the difference visible by measuring
/// throughput as worker count grows: linear scaling indicates the shard
/// layout is not the bottleneck; sub-linear scaling means either the
/// per-file mutex or the shard hash is introducing contention.
///
/// Uses `apply_one_chunk` (the per-chunk entry point) rather than
/// `apply_batch_parallel` so each dispatch independently acquires and
/// releases a DashMap shard guard, amplifying the number of shard
/// operations per iteration.
fn bench_concurrent_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("br_3j_f_dashmap_cores_vs_throughput/concurrent_dispatch");

    // 512 files, each receiving 8 chunks of 4 KiB. Total 16 MiB.
    // File count chosen to exceed DashMap's default shard count (num_cpus * 4)
    // so every shard serves multiple files.
    const FILES: u32 = 512;
    const CHUNKS_PER_FILE: u64 = 8;
    const CHUNK_SIZE: usize = 4 * 1024;
    let total_bytes = FILES as u64 * CHUNKS_PER_FILE * CHUNK_SIZE as u64;

    group.throughput(Throughput::Bytes(total_bytes));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));

    // Pre-build chunks outside the timed region. Chunks are grouped by
    // file so rayon's work-stealing spreads them across files naturally.
    let mut chunks: Vec<DeltaChunk> = Vec::with_capacity(FILES as usize * CHUNKS_PER_FILE as usize);
    for file_idx in 0..FILES {
        for seq in 0..CHUNKS_PER_FILE {
            let seed = SEED_ROOT
                ^ 0xD15D_A7C4_u64.wrapping_mul(file_idx as u64)
                ^ seq.wrapping_mul(0x94D0_49BB_1331_11EB);
            let mut rng = SmallRng::seed_from_u64(seed);
            let mut payload = vec![0u8; CHUNK_SIZE];
            rng.fill_bytes(&mut payload);
            chunks.push(DeltaChunk::literal(FileNdx::new(file_idx), seq, payload));
        }
    }

    for &workers in &worker_counts() {
        let pool = ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|i| format!("dispatch-contention-{i}"))
            .build()
            .expect("rayon pool");

        let id = BenchmarkId::new(
            format!("per_chunk_dispatch/threads={workers}"),
            "512_files_8_chunks",
        );
        group.bench_with_input(id, &workers, |b, _| {
            b.iter(|| {
                let applier = ParallelDeltaApplier::new(workers);
                for i in 0..FILES {
                    applier
                        .register_file(FileNdx::new(i), Box::new(CountingSink))
                        .expect("register");
                }
                // Dispatch each chunk through apply_one_chunk so every
                // call independently acquires/releases a DashMap shard.
                pool.install(|| {
                    use rayon::prelude::*;
                    chunks.par_iter().for_each(|chunk| {
                        applier
                            .apply_one_chunk(chunk.clone())
                            .expect("apply_one_chunk");
                    });
                });
                let mut total = 0u64;
                for i in 0..FILES {
                    total += applier
                        .bytes_written(FileNdx::new(i))
                        .expect("bytes_written");
                    let _ = applier.finish_file(FileNdx::new(i)).expect("finish");
                }
                black_box(total);
            });
        });
    }

    group.finish();
}

fn bench_br_3j_f(c: &mut Criterion) {
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
    let workload_c = build_workload(
        "register_finish_churn",
        0xC,
        WORKLOAD_C_FILES,
        WORKLOAD_C_CHUNKS_PER_FILE,
        WORKLOAD_C_CHUNK_SIZE,
    );
    bench_workload_sweep(c, workload_a);
    bench_workload_sweep(c, workload_b);
    bench_register_finish_churn(c, workload_c);
    bench_concurrent_dispatch(c);
}

criterion_group!(benches, bench_br_3j_f);
criterion_main!(benches);
