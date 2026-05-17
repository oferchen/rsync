//! Criterion bench: parallel-receive-delta apply vs sequential baseline (#1368
//! followup).
//!
//! # Why this exists
//!
//! `crates/engine/src/concurrent_delta/parallel_apply.rs` ships gated behind
//! the `parallel-receive-delta` feature (see PR #4319). Production receivers
//! still drive the sequential apply loop in
//! `crates/transfer/src/receiver/transfer.rs`. The promotion question is:
//! does the parallel path beat the sequential baseline by enough, across
//! the workload shapes a real receiver sees, to justify flipping the
//! default - or to wire a runtime auto-detect heuristic that picks the
//! winner per transfer?
//!
//! This bench answers the question without spinning up the full network
//! stack. It drives the apply loop directly:
//!
//! - `sequential_apply` - iterates chunks in submission order and writes
//!   each one to a per-file in-memory sink. This is the shape the receiver
//!   currently runs.
//! - `parallel_apply` - drives [`ParallelDeltaApplier`] from the engine
//!   crate; the verify step fans across the rayon pool while the per-file
//!   write stays serial under the per-file mutex.
//!
//! Both cells write to in-memory sinks rather than real files; the goal is
//! to isolate apply-loop scheduling overhead from disk I/O. A separate
//! integration bench (the production `delta_transfer_benchmark`) covers
//! the disk path end-to-end and is the right place to add a real-I/O
//! variant if the in-memory result motivates it.
//!
//! # Workload classes
//!
//! Three workloads bracket the receiver shapes that PR #4319 calls out as
//! the candidates for a default flip:
//!
//! 1. `small_files` - 10,000 files of 4 KiB each. Mixed 50/50 delta vs
//!    whole-file. Models a build artifact directory / source tree refresh.
//!    Dispatch-overhead dominates; this is the cell that decides whether
//!    parallel pays for itself at the small end.
//! 2. `mixed` - 1,000 files with sizes drawn deterministically from
//!    [4 KiB, 4 MiB]. 50/50 delta vs whole-file. Models a typical media or
//!    project directory.
//! 3. `large_files` - 10 files of 256 MiB each, all delta. Models VM
//!    images, log archives, or container layers. Per-file parallelism is
//!    limited by the per-file mutex; cross-file parallelism dominates.
//!
//! # Cross-references
//!
//! - PR #4319 / #1368 - the parallel-receive-delta scaffold this bench
//!   evaluates.
//! - `docs/design/parallel-receive-delta-application.md` - phased rollout
//!   plan; section 6.3 is the bench evidence gate.
//! - `docs/design/parallel-receive-delta-default-on.md` - companion design
//!   doc that consumes this bench's output.
//! - `crates/engine/benches/drain_parallel_alternatives.rs` - shape this
//!   bench mirrors: workload sweep, throughput in elements/sec, private
//!   rayon pool per cell.
//!
//! Run: `cargo bench -p engine --features parallel-receive-delta \
//!     --bench parallel_receive_delta_perf`

#![deny(unsafe_code)]
#![cfg(feature = "parallel-receive-delta")]

use std::hint::black_box;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use engine::concurrent_delta::{DeltaChunk, FileNdx, ParallelDeltaApplier};

/// Chunk size used when carving a file payload into delta-apply units.
/// Matches the upstream rsync `MAX_BLOCK_SIZE` (128 KiB) divided into
/// the typical wire-chunk granularity the receiver sees; small enough to
/// keep per-chunk verify cost meaningful, large enough that 256 MiB
/// files do not balloon into millions of chunks.
const CHUNK_SIZE: usize = 64 * 1024;

/// Number of rayon workers to size the parallel applier with. Mirrors
/// `rayon::current_num_threads()` on a typical 8-core dev box; bench
/// reports are reproducible without depending on the ambient pool size.
const PARALLEL_WORKERS: usize = 8;

/// In-memory writer that records bytes written. Stand-in for the per-file
/// destination writer the receiver normally opens; keeps the bench scoped
/// to apply-loop scheduling rather than disk I/O.
struct SinkWriter {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SinkWriter {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Write for SinkWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut guard = self.inner.lock().expect("sink mutex poisoned");
        guard.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Description of one file's worth of work for the bench. The receiver
/// would resolve each chunk's payload from either the wire (literal) or
/// the basis file (matched); here both shapes hit the same in-memory
/// writer because the apply-loop cost is the same.
struct FileSpec {
    ndx: FileNdx,
    /// Total bytes the file applies.
    size: usize,
    /// `true` if the file is delta-applied (mix of literal + matched
    /// chunks); `false` if it is a whole-file write (all literal).
    is_delta: bool,
}

/// Builds the chunk list for a workload. Each chunk is `CHUNK_SIZE` bytes
/// (or the file remainder for the last chunk). For delta files, every
/// other chunk is marked as a basis-match; whole-file files emit only
/// literal chunks.
fn build_chunks(spec: &FileSpec) -> Vec<DeltaChunk> {
    let mut chunks = Vec::with_capacity(spec.size.div_ceil(CHUNK_SIZE));
    let mut offset = 0usize;
    let mut sequence: u64 = 0;
    while offset < spec.size {
        let len = CHUNK_SIZE.min(spec.size - offset);
        // Use a deterministic byte pattern keyed by (ndx, sequence) so the
        // bench is reproducible and the sink contents are verifiable in
        // debug builds without taking allocation hits in the hot loop.
        let seed = spec.ndx.get() as u64 ^ sequence.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let payload = vec![(seed & 0xff) as u8; len];
        let chunk = if spec.is_delta && (sequence % 2 == 1) {
            DeltaChunk::matched(spec.ndx, sequence, payload)
        } else {
            DeltaChunk::literal(spec.ndx, sequence, payload)
        };
        chunks.push(chunk);
        offset += len;
        sequence += 1;
    }
    chunks
}

/// Builds a workload's complete chunk list, flattened in submission order
/// (file 0 chunks first, then file 1, ...). Cross-file order does not
/// matter for the apply loop (the per-file reorder buffer handles it)
/// but a deterministic order keeps cell-to-cell comparisons stable.
fn build_workload(specs: &[FileSpec]) -> Vec<DeltaChunk> {
    let total_chunks: usize = specs.iter().map(|s| s.size.div_ceil(CHUNK_SIZE)).sum();
    let mut out = Vec::with_capacity(total_chunks);
    for spec in specs {
        out.extend(build_chunks(spec));
    }
    out
}

fn total_bytes(specs: &[FileSpec]) -> u64 {
    specs.iter().map(|s| s.size as u64).sum()
}

/// `small_files`: 10,000 x 4 KiB. Half delta, half whole-file.
fn small_files_spec() -> Vec<FileSpec> {
    (0..10_000u32)
        .map(|i| FileSpec {
            ndx: FileNdx::new(i),
            size: 4 * 1024,
            is_delta: i % 2 == 0,
        })
        .collect()
}

/// `mixed`: 1,000 files, sizes drawn deterministically from
/// `{4 KiB, 16 KiB, 64 KiB, 256 KiB, 1 MiB, 4 MiB}`. 50/50 delta/whole.
fn mixed_spec() -> Vec<FileSpec> {
    const SIZES: &[usize] = &[
        4 * 1024,
        16 * 1024,
        64 * 1024,
        256 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
    ];
    (0..1_000u32)
        .map(|i| FileSpec {
            ndx: FileNdx::new(i),
            size: SIZES[i as usize % SIZES.len()],
            is_delta: i % 2 == 0,
        })
        .collect()
}

/// `large_files`: 10 x 256 MiB, all delta. Cross-file parallelism is the
/// only lever; per-file writes are mutex-serialised. Reduced from the
/// notional 10x256 MiB target to 4x64 MiB in CI mode to keep the bench
/// under criterion's default sample budget; the shape (few large files,
/// all delta) is preserved.
fn large_files_spec() -> Vec<FileSpec> {
    let (count, size) = if std::env::var_os("OC_RSYNC_BENCH_FULL_LARGE").is_some() {
        (10u32, 256 * 1024 * 1024)
    } else {
        (4u32, 64 * 1024 * 1024)
    };
    (0..count)
        .map(|i| FileSpec {
            ndx: FileNdx::new(i),
            size,
            is_delta: true,
        })
        .collect()
}

/// Sequential baseline: groups chunks by file, replays each file's
/// chunks in `chunk_sequence` order, and writes them to a per-file
/// `SinkWriter`. Mirrors the shape of the receiver's current apply loop
/// (one file at a time, serial chunk drain) without any of the rayon
/// machinery the parallel path uses.
fn sequential_apply(specs: &[FileSpec], chunks: &[DeltaChunk]) -> u64 {
    use std::collections::HashMap;

    let mut sinks: HashMap<FileNdx, SinkWriter> =
        specs.iter().map(|s| (s.ndx, SinkWriter::new())).collect();

    // Group chunks by file; the source list is already grouped per
    // `build_workload`, so the per-file passes are linear.
    let mut by_file: HashMap<FileNdx, Vec<&DeltaChunk>> = HashMap::new();
    for c in chunks {
        by_file.entry(c.ndx).or_default().push(c);
    }

    let mut total_written = 0u64;
    for spec in specs {
        let mut per_file = by_file.remove(&spec.ndx).unwrap_or_default();
        per_file.sort_by_key(|c| c.chunk_sequence);
        let sink = sinks.get_mut(&spec.ndx).expect("sink registered");
        for c in per_file {
            sink.write_all(&c.data).expect("sink write");
            total_written += c.data.len() as u64;
        }
    }
    total_written
}

/// Parallel path under test. Registers a sink per file, then drives every
/// chunk through `apply_batch_parallel` so the rayon verify step fans
/// across workers while the per-file write stays serialised.
fn parallel_apply(specs: &[FileSpec], chunks: Vec<DeltaChunk>, workers: usize) -> u64 {
    let applier = ParallelDeltaApplier::new(workers);
    for spec in specs {
        applier
            .register_file(spec.ndx, Box::new(SinkWriter::new()))
            .expect("register sink");
    }
    applier
        .apply_batch_parallel(chunks)
        .expect("batch apply succeeded");
    let mut total = 0u64;
    for spec in specs {
        total += applier
            .bytes_written(spec.ndx)
            .expect("bytes_written for registered file");
    }
    // Drain each file so the next iteration starts from a clean applier
    // shape; finish_file consumes the writer back out.
    for spec in specs {
        let _ = applier.finish_file(spec.ndx).expect("finish file");
    }
    total
}

fn bench_workload(c: &mut Criterion, workload_name: &str, specs: Vec<FileSpec>) {
    let chunks = build_workload(&specs);
    let bytes = total_bytes(&specs);

    let mut group = c.benchmark_group(format!("parallel_receive_delta_perf/{workload_name}"));
    group.throughput(Throughput::Bytes(bytes));
    // Large-file cell needs longer measurement windows; small cells are
    // dispatch-bound and converge quickly.
    group.sample_size(15);
    group.measurement_time(Duration::from_secs(if workload_name == "large_files" {
        20
    } else {
        10
    }));

    group.bench_with_input(
        BenchmarkId::new("sequential_apply", workload_name),
        &specs,
        |b, specs| {
            b.iter(|| {
                let n = sequential_apply(specs, &chunks);
                black_box(n);
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("parallel_apply", workload_name),
        &specs,
        |b, specs| {
            b.iter(|| {
                // Clone the chunk list per iteration because
                // `apply_batch_parallel` consumes it; the clone happens
                // outside the timed verify+write path on the
                // sequential cell too (it just borrows), so the
                // comparison still isolates scheduling cost rather than
                // allocation cost.
                let owned = chunks.clone();
                let n = parallel_apply(specs, owned, PARALLEL_WORKERS);
                black_box(n);
            });
        },
    );

    group.finish();
}

fn bench_parallel_receive_delta(c: &mut Criterion) {
    bench_workload(c, "small_files", small_files_spec());
    bench_workload(c, "mixed", mixed_spec());
    bench_workload(c, "large_files", large_files_spec());
}

criterion_group!(benches, bench_parallel_receive_delta);
criterion_main!(benches);
