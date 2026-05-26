# ABW-5.b: Verify-write overlap stress test spec

Tracking: ABW-5.b
Status: Spec
Date: 2026-05-26
Depends on: ABW-5.c (safety analysis), ABW-5.a (debug assertions)

## 1. Objective

Validate under real concurrency that the three invariants identified by
ABW-5.c hold without data corruption, deadlock, or assertion failure:

1. **Verify is pure** - `verify_chunk` reads only owned `chunk.data`
   and the immutable `Arc<dyn ChecksumStrategy>`.
2. **Write is Mutex-guarded** - every write to a file's destination goes
   through `lock_slot()` on the per-file `Mutex<FileSlot>`.
3. **Reorder buffer restores sequence order** - chunks arrive at the
   writer in strict `chunk_sequence` order regardless of Mutex
   acquisition order across threads.

The stress test exercises worst-case interleaving that unit tests cannot
reach: cross-file batch boundaries, rapid batch succession, high thread
counts, adversarial chunk orderings, and mixed file sizes.

---

## 2. Test scenarios

### 2.a - Batch spanning a file boundary

A single `apply_batch_parallel` call receives chunks that belong to two
different files whose NDX values are adjacent (file N and file N+1).
This forces the serial write loop to alternate Mutex acquisitions
between two files within a single batch. The reorder buffer for each
file must independently reconstruct submission order.

**Parameters:**

- 2 files, 128 chunks each (256 total per batch).
- Chunk sizes: alternating 1 byte and 8 KiB to stress reorder buffer
  under size asymmetry.
- 50 batch iterations per run.

### 2.b - Rapid batch succession with verify/write overlap

Two threads submit batches concurrently to the same `ParallelDeltaApplier`
instance. Thread A calls `apply_batch_parallel` with chunks for files
0-9. Thread B calls `apply_batch_parallel` with subsequent chunks for
the same files 0-9. The verify step of batch B runs in parallel with the
write step of batch A - the exact overlap the ABW-5.c audit proved safe.

**Parameters:**

- 10 files, 64 chunks per file per batch.
- 2 producer threads, each submitting 100 batches.
- No artificial delay between batches - maximum temporal overlap.

### 2.c - 64 threads, 1000 files, random chunk sizes

Maximum-concurrency scenario that saturates the rayon pool and the
DashMap shard locks. Each of 64 producer threads submits batches for a
disjoint subset of files (to avoid starving the reorder buffer) plus a
shared "hot" file that all threads write to simultaneously.

**Parameters:**

- 1000 files total: 960 partitioned across 64 threads (15 files each)
  plus 40 shared "hot" files written by all threads.
- Chunk sizes: uniform random in `[1, 16384]` bytes.
- Chunks per file: uniform random in `[4, 128]`.
- Rayon pool: 64 threads (explicit `ThreadPoolBuilder`).

---

## 3. Adversarial patterns

Each scenario applies the following adversarial patterns to the chunk
submission order within each batch. The pattern is selected per-batch
using a seeded PRNG so failures are reproducible.

### 3.a - Out-of-order chunk delivery

Chunks within a batch are shuffled with a Fisher-Yates permutation
before submission. The reorder buffer must reconstruct the original
`chunk_sequence` order.

### 3.b - Maximum reorder depth

Chunks are submitted in reverse `chunk_sequence` order so the reorder
buffer reaches its capacity limit on every insert except the last. For
scenario 2.c, the per-file reorder capacity is set to the maximum chunk
count for that file to avoid buffer-full errors under this pathological
pattern.

### 3.c - Alternating tiny/huge files

Files alternate between 1-chunk (single byte) and 128-chunk (8 KiB per
chunk) sizes. The applier must handle registration, ingestion, and
`finish_file` for trivially small files interleaved with large
multi-batch files without leaking slots or corrupting the DashMap.

### 3.d - Interleaved file lifecycle

While producers are submitting chunks for files 500-999, a separate
thread calls `finish_file` on files 0-499 (which have already received
all their chunks). This exercises the `Arc::try_unwrap` path
concurrently with active batch dispatch, testing the DG-3.c invariant
that the payload Arc strong count returns to 1 after workers drain.

---

## 4. Verification method

### 4.1 SHA-256 parity oracle

Every test run computes a reference output using a sequential,
single-threaded path: chunks are sorted by `(ndx, chunk_sequence)` and
concatenated in order. The SHA-256 digest of the reference output for
each file is compared against the SHA-256 digest of the parallel
applier's output.

```
for each file:
    reference_bytes = sort chunks by chunk_sequence, concatenate data
    reference_hash  = sha256(reference_bytes)
    parallel_bytes  = read from VecSink after finish_file
    parallel_hash   = sha256(parallel_bytes)
    assert reference_hash == parallel_hash
```

Both the per-file digests and the aggregate byte count are compared.
A single mismatch fails the run immediately with the offending NDX,
expected hash, actual hash, and the PRNG seed for reproduction.

### 4.2 Byte-count invariant

After all files are finished, the sum of `bytes_written()` across all
files must equal the sum of `chunk.data.len()` across all submitted
chunks. This catches dropped or duplicated writes that might produce a
correct hash by coincidence (practically impossible for SHA-256 but
cheap to assert).

### 4.3 Debug assertion coverage

The test binary is compiled with `debug_assertions` enabled so the
ABW-5.a assertions fire:

- **Invariant 1**: `verify_chunk` strategy digest-length witness.
- **Invariant 2**: `bytes_written` monotonicity under the Mutex guard.
- **Invariant 3**: verified batch length equals submitted chunk count.

A `debug_assert` failure triggers a panic, caught by the test harness as
a test failure. No separate assertion-monitoring infrastructure is needed.

---

## 5. Duration and iteration count

### 5.1 Default (CI)

| Scenario | Iterations | Estimated wall-clock |
|----------|-----------|---------------------|
| 2.a      | 200       | ~2 s                |
| 2.b      | 50        | ~5 s                |
| 2.c      | 10        | ~15 s               |

Total: under 30 seconds on a 4-core CI runner. The test uses
`#[ignore]` and runs only under the stress-test CI job (section 6).

### 5.2 Extended (local soak)

An environment variable `ABW5B_SOAK_ITERATIONS` multiplies the default
iteration counts. Setting it to `100` gives ~50 minutes of sustained
concurrency - suitable for overnight validation before a release.

### 5.3 Timeout

Each scenario has a per-iteration wall-clock deadline:

| Scenario | Deadline per iteration |
|----------|----------------------|
| 2.a      | 5 s                  |
| 2.b      | 10 s                 |
| 2.c      | 30 s                 |

If an iteration exceeds the deadline, the test fails with a deadlock
diagnostic that dumps the rayon pool status and the per-file reorder
buffer metrics (`stall_duration`, `current_depth`, `max_depth`).

---

## 6. CI integration

### 6.1 Workflow

Add a job to the existing `ci.yml` matrix or a dedicated
`stress-test.yml` workflow:

```yaml
stress-test-abw5b:
  name: "Stress: verify-write overlap (ABW-5.b)"
  runs-on: ubuntu-latest
  timeout-minutes: 10
  continue-on-error: true        # non-blocking
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - uses: taiki-e/install-action@nextest
    - run: |
        cargo nextest run \
          --workspace --all-features \
          -E 'test(stress_abw5b)' \
          --run-ignored ignored-only \
          --test-threads 1 \
          --color never
```

### 6.2 Non-required check

The job uses `continue-on-error: true` and is not added to the branch
protection required checks. Failures trigger a GitHub annotation but do
not block merge. This prevents flaky-timeout regressions from gating
unrelated PRs while still surfacing real data-corruption failures in the
CI dashboard.

### 6.3 Test location

```
crates/engine/src/concurrent_delta/parallel_apply/stress.rs
```

Registered as a submodule of `parallel_apply/mod.rs` behind
`#[cfg(test)]`. All three scenarios live in this file, each as a
separate `#[test] #[ignore]` function prefixed `stress_abw5b_`.

---

## 7. Pass/fail criteria

A run passes if and only if all of the following hold:

| Criterion | How verified |
|-----------|-------------|
| Zero SHA-256 mismatches | Per-file digest comparison (section 4.1) |
| No `debug_assert` fires | Process exits without panic (section 4.3) |
| No deadlocks | Per-iteration wall-clock deadline (section 5.3) |
| No dropped/duplicated writes | Aggregate byte-count invariant (section 4.2) |
| No `finish_file` errors | Every file completes without `ApplierStillReferenced` or `UndrainedChunks` |
| No Mutex poisoning | No `SlotPoisoned` errors from any `lock_slot` call |

A single violation in any iteration fails the entire test.

---

## 8. Reproducibility

Every run logs its PRNG seed at the start. On failure, the seed is
included in the panic message so the exact chunk permutation can be
replayed:

```
thread 'stress_abw5b_concurrent_batches' panicked at
  'SHA-256 mismatch for ndx=7: expected=abc... actual=def...
   [seed=0x1A2B3C4D, iteration=42, scenario=2.b]'
```

The seed defaults to a hash of the current time but can be overridden
via `ABW5B_SEED=<u64>` for deterministic replay.

---

## 9. Implementation notes

### 9.1 Thread pool isolation

Scenarios 2.b and 2.c build a dedicated `rayon::ThreadPool` via
`ThreadPoolBuilder::new().num_threads(N).build()` and run all work
inside `pool.install(|| ...)`. This avoids polluting the global rayon
pool and ensures the thread count matches the scenario's design.

### 9.2 Memory budget

Scenario 2.c with 1000 files and random chunk sizes up to 16 KiB can
allocate up to ~2 GiB of chunk data. The test pre-calculates the total
byte count and skips (not fails) if it exceeds a 1 GiB memory budget,
logging the skip reason. The budget is sized for the 7 GiB RAM available
on `ubuntu-latest` runners.

### 9.3 No filesystem I/O

All scenarios use `VecSink` (in-memory `Arc<Mutex<Vec<u8>>>`) as the
destination writer. No temp files, no disk I/O, no filesystem-dependent
flakiness.

### 9.4 Checksum strategy

All scenarios use `ChecksumAlgorithmKind::Xxh3` (fastest available) so
the verify step does not dominate wall-clock time. The test attaches
correct `expected_strong` digests to every chunk so the ABW-5.a
invariant-1 assertion exercises the full comparison path.

---

## 10. Relationship to other ABW tasks

| Task    | Status   | Relationship |
|---------|----------|-------------|
| ABW-5.c | Complete | Safety proof this test validates empirically |
| ABW-5.a | Complete | Debug assertions this test exercises |
| ABW-5.b | This spec | Stress test implementation |
| ABW-2   | Deferred | Pipelined verify-write (would change concurrency model; stress test must be updated if ABW-2 lands) |
| ABW-3   | Complete | Closure analysis (no test impact) |
