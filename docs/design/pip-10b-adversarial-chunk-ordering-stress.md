# PIP-10.b - Adversarial chunk ordering stress tests for parallel receive-delta

Date: 2026-05-26
Scope: stress test design for the reorder buffer and parallel dispatch
path under adversarial chunk arrival orderings
Status: design spec
Tracker: PIP-10.b (#3023)
Predecessors:
- PIP-10.a: full interop matrix for parallel receive-delta path
- PIP-9 (PR series): wire-up of parallel receive-delta into receiver
  pipeline
- BR-3i (PRs #2498-#2502): per-chunk strong-checksum verify and
  batch apply
- SPL-38 (module decomposition): split `parallel_apply/` into batch,
  drain, decrement_guard, slot_barrier submodules

Related code:
- `crates/engine/src/concurrent_delta/reorder/mod.rs` -
  `ReorderBuffer` (ring-buffer reordering)
- `crates/engine/src/concurrent_delta/parallel_apply/batch.rs` -
  `apply_batch_parallel` (rayon fan-out and serial write)
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` -
  `ParallelDeltaApplier` (per-file reorder + DashMap dispatch)
- `crates/engine/src/concurrent_delta/consumer/loops.rs` -
  `run_bare_loop` / `run_spillable_loop` (consumer drain loops)
- `crates/engine/src/concurrent_delta/spill/buffer/` -
  `SpillableReorderBuffer` (disk-backed overflow)

## 1. Motivation

The parallel receive-delta path dispatches per-file delta chunks to
rayon worker threads via `ParallelDeltaApplier`. Workers complete in
unpredictable order. Two distinct reorder buffers restore sequential
ordering:

1. **Per-file chunk reorder** (`FileSlot::reorder`) - ensures chunks
   within a single file are written in `chunk_sequence` order to the
   destination writer, regardless of which rayon worker verified them
   first.

2. **Pipeline-level result reorder** (`DeltaConsumer`'s
   `ReorderBuffer<DeltaResult>`) - ensures per-file results are
   delivered to the consumer in submission order (file-list NDX order)
   for post-processing (checksum verify, temp-file commit, metadata
   apply).

Under benign workloads the buffers see mostly in-order or
slightly-shuffled arrivals. Adversarial orderings - reverse
completion, worst-case interleaving, burst patterns, drip feeds -
stress the reorder machinery in ways that production traffic rarely
exercises. These edge cases can expose:

- **Ring buffer overflow**: capacity exhausted, `CapacityExceeded`
  error, forced growth via `force_insert` or unbounded memory
  consumption.
- **Incorrect sequencing**: chunks written out of order to the
  destination file, producing a corrupted reconstruction.
- **Deadlock in the drain path**: the consumer blocks waiting for
  `next_expected` while the ring is full and no progress is possible.
- **Memory pressure**: deep reorder queues under constrained capacity
  pin large amounts of data in flight.
- **Spill path failures**: adversarial orderings that repeatedly
  trigger spill-to-disk, spill-reload cycles, or ENOSPC conditions.

PIP-10.b defines a comprehensive stress test suite targeting these
failure modes.

## 2. Architecture under test

### 2.1 Per-file chunk reorder (hot path)

```
DeltaChunk (ndx=N, chunk_sequence=K)
  |
  v
ParallelDeltaApplier::apply_batch_parallel
  |-- rayon par_iter: verify_chunk (CPU-bound, out-of-order completion)
  |-- rayon collect barrier (all verifies complete before any write)
  v
serial write loop:
  |-- slot_for(ndx) -> SlotHandle
  |-- lock_slot(ndx) -> MutexGuard<FileSlot>
  |-- FileSlot::ingest(chunk)
  |     |-- ReorderBuffer::insert(chunk_sequence, chunk)
  |     |-- ReorderBuffer::drain_ready() -> write_chunk for each
  v
destination writer (sequential bytes per file)
```

The per-file `ReorderBuffer` has capacity
`DEFAULT_PER_FILE_REORDER_CAPACITY = 64`. The `ingest` method
returns `Err` if the insert exceeds capacity. The stress tests must
exercise the boundary where chunks arrive far ahead of the expected
sequence, filling the 64-slot ring.

### 2.2 Pipeline-level result reorder

```
DeltaResult (sequence=S)
  |
  v
crossbeam_channel -> consumer background thread
  |
  v
run_bare_loop / run_spillable_loop
  |-- ReorderBuffer::insert(sequence, result)
  |-- on CapacityExceeded: drain_ready then force_insert
  |-- drain_ready -> forward to mpsc::Sender<DeltaResult>
  v
consumer receives results in file-list order
```

The pipeline-level buffer size depends on the constructor. The
`run_bare_loop` path uses `force_insert` as a deadlock breaker when
the ring is full and `next_expected` is missing. The
`run_spillable_loop` path uses `SpillableReorderBuffer` which evicts
high-sequence items to disk instead of growing unboundedly.

### 2.3 SpillableReorderBuffer overflow

The spill path engages when estimated in-memory bytes exceed
`threshold`. Items in the "hot zone" (`HOT_ZONE = 16` items near
`next_expected`) are preserved in memory. Higher-sequence items are
serialized to a tempfile via the `SpillCodec` trait. On drain, spilled
items are reloaded transparently. Adversarial orderings that keep
`next_expected` blocked while high-sequence items accumulate trigger
the spill path repeatedly.

## 3. Adversarial ordering patterns

Each pattern is parameterized by file count (F) and chunks per file
(C). The test harness generates deterministic chunk streams under each
pattern and feeds them through the applier.

### 3.1 Reverse-order completion

All chunks for a single file arrive in strictly descending
`chunk_sequence` order: `[C-1, C-2, ..., 1, 0]`.

**What it stresses**: the reorder buffer fills completely before the
first chunk can drain. With capacity K and C > K, the buffer must
either grow (via `force_insert`) or the insert fails. For per-file
buffers with fixed capacity 64 and C=100, this forces 36 capacity
overflows.

**Variants**:
- Single file, C within capacity (C=32, capacity=64): buffer fills
  partially, drains entirely when seq 0 arrives.
- Single file, C exceeds capacity (C=128, capacity=64): buffer
  overflow on every insert past offset 63.
- Multi-file interleaved reverse: files interleave their reverse
  chunks - `[file0:C-1, file1:C-1, file0:C-2, file1:C-2, ...]`.

### 3.2 Worst-case interleaving

Chunks for F files arrive in round-robin order by file, each file's
chunks arriving in order, but the pipeline-level sequence numbers are
maximally spread. File 0 gets sequences 0, F, 2F, ...; file 1 gets
sequences 1, F+1, 2F+1, ...; etc.

**What it stresses**: the pipeline-level reorder buffer must hold up
to F entries simultaneously before any contiguous run can drain.
With F=1000 and capacity=64, the consumer's `force_insert` path
activates on every insertion past the 64th file.

### 3.3 Burst pattern

All C chunks for file N complete before any chunk for file N-1. The
pipeline-level sequence numbers are: `[(N-1)*C, (N-1)*C+1, ...,
N*C-1]` for file N, arriving before `[(N-2)*C, ..., (N-1)*C-1]`
for file N-1.

**What it stresses**: pipeline-level reorder buffer holds an entire
file's results (C items) while waiting for the previous file's first
result. Combined with constrained capacity, this triggers repeated
`force_insert` or spill cycles.

**Variants**:
- Forward burst: files complete in order but each file's chunks
  complete all-at-once (baseline, should not stress the buffer).
- Reverse burst: files complete in reverse file-list order -
  file F-1 first, file 0 last. Maximizes the pipeline-level
  reorder gap.
- Alternating burst: even-indexed files complete first, then
  odd-indexed files. Gap oscillates.

### 3.4 Drip feed

Chunks arrive one at a time with maximum delay between the expected
sequence and the arriving sequence. Pattern: deliver seq K*2 first,
then seq 1, then seq K*2+1, then seq 3, ... - alternating between
far-future and near-expected sequences.

**What it stresses**: the buffer alternates between "gap at
`next_expected`" and "immediate drain" states. Forces the stall
timer to start and stop repeatedly. Exercises the histogram recording
path for drain-batch sizes of exactly 1 (skewed toward head-of-line
pressure).

### 3.5 Sawtooth

For each window of W chunks, deliver in reverse within the window,
then advance to the next window. Pattern for W=4:
`[3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, ...]`.

**What it stresses**: the buffer fills to exactly W before draining
W items in a burst. Exercises the drain-batch histogram's mid-range
buckets. With W = capacity, this is the worst case for utilization
without overflow.

### 3.6 Head-blocked flood

Seq 0 is withheld. All other sequences `[1, 2, ..., N-1]` arrive
first. Then seq 0 arrives, triggering a cascade drain of all N items.

**What it stresses**: maximum buffered depth. For the pipeline-level
buffer with spill enabled, this forces all N-1 items through the
spill path and then reloads them in one drain cascade. For the
per-file buffer, this is a variant of 3.1 with the worst possible
single missing head.

## 4. Test matrix dimensions

### 4.1 File count sweep (pipeline-level)

| Files (F) | Purpose |
|-----------|---------|
| 1 | Single-file isolation: only per-file reorder active |
| 10 | Small batch: pipeline buffer stays within default capacity |
| 100 | Medium: typical production file count |
| 1,000 | Large: exercises DashMap shard concurrency |
| 10,000 | Stress: deep pipeline-level reorder, DashMap scaling |

### 4.2 Chunks-per-file sweep (per-file level)

| Chunks (C) | Purpose |
|-------------|---------|
| 1 | Degenerate: no per-file reorder needed |
| 10 | Small: well within capacity 64 |
| 100 | Overflow: exceeds default capacity, forces per-file reorder handling |
| 1,000 | Deep: sustained per-file reorder pressure |

### 4.3 Reorder capacity sweep

| Capacity | Purpose |
|----------|---------|
| 4 | Extreme constraint: forces overflow on nearly every pattern |
| 16 | Moderate constraint: exposes edge cases without excessive overhead |
| 64 | Production default |
| 256 | Relaxed: baseline for throughput comparison |

### 4.4 Spill configuration

| Config | Purpose |
|--------|---------|
| No spill (bare `ReorderBuffer`) | Tests `force_insert` growth path |
| Spill threshold 4 KB | Forces spill on small payloads |
| Spill threshold 64 KB | Moderate spill pressure |
| Spill with `RespillAfterRead` | Tests re-spill after reload |
| Spill with `PerItem` granularity | Per-item vs whole-batch coverage |

## 5. Test implementation plan

### 5.1 Test harness (`crates/engine/tests/adversarial_reorder.rs`)

A new integration test file containing the adversarial ordering suite.
Located in `crates/engine/tests/` (not inside `src/`) so it exercises
the public API surface without reaching into module internals.

```rust
// Pseudocode structure:
struct AdversarialHarness {
    file_count: usize,
    chunks_per_file: usize,
    chunk_size: usize,           // bytes per chunk payload
    reorder_capacity: usize,     // per-file reorder buffer
    pattern: OrderingPattern,
}

enum OrderingPattern {
    InOrder,                     // baseline
    Reverse,                     // 3.1
    WorstCaseInterleave,         // 3.2
    ReverseBurst,                // 3.3
    DripFeed,                    // 3.4
    Sawtooth { window: usize },  // 3.5
    HeadBlockedFlood,            // 3.6
}
```

### 5.2 Per-file chunk reorder tests

Target: `ParallelDeltaApplier` with `VecSink` writers.

For each `(pattern, C, capacity)` combination:

1. Register F files with `VecSink` writers.
2. Generate C chunks per file with deterministic payloads
   (`chunk_sequence` XOR file NDX as seed).
3. Permute chunks according to the ordering pattern.
4. Submit all chunks via `apply_batch_parallel` (batched) or
   `apply_one_chunk` (sequential).
5. Call `finish_file` for each file.
6. Compute SHA-256 of each file's collected bytes.
7. Compare against the SHA-256 of the sequential reference
   (chunks applied in order).

**Assertion**: SHA-256 match for every file under every pattern.

### 5.3 Pipeline-level reorder tests

Target: `DeltaConsumer` with `ReorderBuffer<DeltaResult>`.

For each `(pattern, F, C)` combination:

1. Create a `WorkQueue` and `DeltaConsumer`.
2. Submit F*C work items with sequence numbers assigned by the
   pattern.
3. Simulate rayon workers that complete in the adversarial order by
   feeding `DeltaResult` values into the consumer's stream channel.
4. Collect consumer output and verify:
   - All F*C results received.
   - Results arrive in strictly monotonic `sequence()` order.
   - No duplicate sequences.
   - No gaps in the sequence.

### 5.4 Spill-path stress tests

Target: `SpillableReorderBuffer<DeltaResult>`.

For each `(pattern, F*C, spill_config)` combination:

1. Create a `SpillableReorderBuffer` with constrained threshold.
2. Insert items in adversarial order.
3. Drain and collect all items.
4. Verify:
   - All items recovered in correct sequence order.
   - `spill_stats().spill_events > 0` for patterns that should
     trigger spill.
   - `spill_stats().reload_events > 0` for patterns that should
     trigger reload.
   - No items lost during spill-reload cycles.

### 5.5 Correctness oracle

The correctness oracle for every test is the sequential reference
path. For per-file tests:

```rust
fn sequential_reference(chunks: &[DeltaChunk]) -> HashMap<FileNdx, Vec<u8>> {
    let mut by_file: HashMap<FileNdx, Vec<&DeltaChunk>> = HashMap::new();
    for c in chunks {
        by_file.entry(c.ndx).or_default().push(c);
    }
    by_file.into_iter().map(|(ndx, mut cs)| {
        cs.sort_by_key(|c| c.chunk_sequence);
        let bytes: Vec<u8> = cs.iter().flat_map(|c| c.data.iter()).copied().collect();
        (ndx, bytes)
    }).collect()
}
```

SHA-256 of the sequential reference bytes is the expected digest. The
parallel path's output bytes must produce an identical SHA-256.

## 6. Performance bounds

Adversarial orderings are expected to be slower than in-order
delivery due to buffering overhead, spill I/O, and reduced cache
locality. The tests establish upper bounds:

| Metric | Bound | Rationale |
|--------|-------|-----------|
| Throughput (adversarial vs in-order) | >= 50% of in-order | Worst-case reorder overhead should not exceed 2x |
| Throughput (adversarial vs sequential path) | >= 50% of sequential | Parallel path under adversarial load must not regress below half of sequential |
| Peak RSS (adversarial, no spill) | <= 4x in-order RSS | `force_insert` ring growth is bounded |
| Peak RSS (adversarial, spill enabled) | <= threshold + 20% | Spill should keep RSS near the configured threshold |
| Spill latency | P99 < 10 ms per spill event | Disk I/O should not dominate under constrained thresholds |

Performance bounds are measured but not gating for correctness tests.
A separate bench (PIP-10.c, future) will track adversarial throughput
over time.

## 7. Concurrency and determinism

### 7.1 Deterministic ordering

All adversarial patterns must be deterministically reproducible from
`(F, C, pattern)` parameters. No randomness in the ordering itself -
the patterns are worst-case by construction, not probabilistic.
Property tests (`proptest`) complement the deterministic suite by
sampling random permutations and verifying the correctness invariant
holds for all of them.

### 7.2 Rayon thread count

Tests run with the ambient rayon pool. The ordering patterns
exercise the reorder buffer, not rayon scheduling. The rayon pool
only matters for `apply_batch_parallel` where the verify step fans
out. The test harness sets `rayon::ThreadPoolBuilder` to 4 threads
for reproducibility.

### 7.3 Timeout guard

Every test case has a 30-second wall-clock timeout (enforced by
nextest's per-test timeout). A deadlock in the drain path surfaces
as a test timeout rather than a hang.

## 8. Success criteria

All criteria must hold across every `(pattern, F, C, capacity,
spill_config)` combination in the test matrix:

| Criterion | Verification |
|-----------|-------------|
| Zero corruption | SHA-256 of parallel output matches sequential reference for every file |
| Zero hangs | All tests complete within 30-second timeout |
| Zero panics | No `unwrap`/`expect` failures, no `ReorderBuffer::finish` gap panic |
| Correct sequence | Pipeline-level results arrive in strictly monotonic order |
| Complete delivery | Every submitted chunk/result is delivered exactly once |
| Spill round-trip | Spilled items survive encode-decode-reload without data loss |
| Metrics consistency | `force_insert_count` matches the number of capacity-exceeded events |
| Memory bounded (spill mode) | RSS stays within threshold + 20% when spill is enabled |

## 9. Test priority and phasing

### Phase 1 - Core correctness (P0)

Minimum viable coverage. Blocks any promotion of
`parallel-receive-delta` to default-on.

| Test ID | Pattern | F | C | Capacity | Spill |
|---------|---------|---|---|----------|-------|
| P1-01 | Reverse | 1 | 64 | 64 | No |
| P1-02 | Reverse | 1 | 128 | 64 | No |
| P1-03 | HeadBlockedFlood | 1 | 100 | 64 | No |
| P1-04 | Reverse | 4 | 32 | 64 | No |
| P1-05 | Sawtooth(W=8) | 1 | 64 | 64 | No |
| P1-06 | ReverseBurst | 10 | 10 | 64 | No |
| P1-07 | WorstCaseInterleave | 10 | 10 | 64 | No |
| P1-08 | DripFeed | 1 | 32 | 64 | No |
| P1-09 | Reverse | 1 | 64 | 4 | No |
| P1-10 | HeadBlockedFlood | 1 | 32 | 4 | No |

### Phase 2 - Spill and memory pressure (P1)

Validates the disk-backed overflow path under adversarial load.

| Test ID | Pattern | F | C | Capacity | Spill |
|---------|---------|---|---|----------|-------|
| P2-01 | Reverse | 1 | 128 | 64 | 4 KB threshold |
| P2-02 | HeadBlockedFlood | 1 | 100 | 64 | 4 KB threshold |
| P2-03 | ReverseBurst | 10 | 50 | 16 | 4 KB threshold |
| P2-04 | Reverse | 1 | 64 | 16 | PerItem granularity, 4 KB |
| P2-05 | HeadBlockedFlood | 1 | 64 | 16 | RespillAfterRead, 4 KB |

### Phase 3 - Scale and performance (P2)

Large-scale sweeps and throughput measurement. Not gating for
correctness but required before performance claims.

| Test ID | Pattern | F | C | Capacity | Spill |
|---------|---------|---|---|----------|-------|
| P3-01 | Reverse | 1 | 1,000 | 64 | No |
| P3-02 | ReverseBurst | 100 | 100 | 64 | No |
| P3-03 | WorstCaseInterleave | 1,000 | 10 | 64 | No |
| P3-04 | HeadBlockedFlood | 1 | 10,000 | 256 | 64 KB threshold |
| P3-05 | Sawtooth(W=64) | 10 | 1,000 | 64 | No |
| P3-06 | DripFeed | 100 | 100 | 16 | 4 KB threshold |

### Phase 4 - Property tests (P2)

Proptest-driven random permutations verifying the correctness
invariant holds for arbitrary orderings, complementing the
deterministic adversarial patterns.

| Test ID | Scope | Parameters |
|---------|-------|-----------|
| P4-01 | Per-file reorder | F=1, C in 1..128, random permutation, capacity in {4, 16, 64} |
| P4-02 | Multi-file reorder | F in 1..32, C in 1..32, random interleaving |
| P4-03 | Pipeline-level reorder | N in 1..500, random permutation, capacity in {4, 16, 64} |

## 10. Relationship to existing tests

The codebase already contains reorder buffer and parallel apply tests:

- `crates/engine/src/concurrent_delta/reorder/tests.rs` - unit tests
  for `ReorderBuffer`: in-order, out-of-order, capacity bounds,
  drain, force_insert, passthrough, adaptive, metrics.
- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` tests -
  `single_file_out_of_order_preserves_byte_order`,
  `random_chunk_sizes_and_permutations_match_sequential` (proptest),
  `batch_apply_matches_sequential_byte_for_byte`.
- `crates/engine/tests/pipeline_reorder_integration.rs` - end-to-end
  pipeline test with 500 items.
- `crates/engine/src/concurrent_delta/spill/buffer/tests/` - spill
  buffer unit tests including memory pressure, ENOSPC degradation,
  fault injection, compression round-trip.

PIP-10.b extends this coverage along two axes:

1. **Adversarial ordering patterns**: existing tests use in-order,
   reversed, or randomly permuted chunks. PIP-10.b adds structurally
   adversarial patterns (burst, drip feed, sawtooth, head-blocked
   flood, worst-case interleave) designed to hit specific failure
   modes.

2. **Scale**: existing tests use small file/chunk counts (16-500).
   PIP-10.b sweeps up to 10,000 files and 1,000 chunks per file,
   exercising the buffer under sustained load.

The new tests do not replace existing tests. They run alongside them
in the nextest suite under the `adversarial_reorder` test binary.

## 11. CI integration

The adversarial stress tests run as part of the standard
`cargo nextest run --workspace --all-features` CI step. Tests in
Phase 3 (scale) are gated behind `#[cfg(not(debug_assertions))]` to
avoid excessive runtime in debug builds; they run only in the release
CI profile.

Phase 4 property tests use `proptest::test_runner::Config::with_cases(256)`
for CI and default to 64 cases for local runs via an environment
variable override.

No new CI workflows are needed. The existing nextest matrix (Linux,
macOS, Windows) covers all platforms.
