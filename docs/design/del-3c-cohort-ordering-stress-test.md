# Cohort-ordering stress test spec (DEL-3.c)

Status: Test design (task DEL-3.c; exercises the DEL-2.a
`ReorderBuffer` and DEL-2.b `CohortBatcher` under high concurrency and
adversarial input patterns; depends on DEL-3.a wire-byte parity
harness and DEL-3.b property-test suite; gated behind
`parallel-delete-consumer` feature)
Audience: engine maintainers verifying that the re-ordering buffer and
cohort batcher preserve strict wire-ordering invariants under stress.
Scope: test scenarios, adversarial input patterns, concurrency levels,
scale dimensions, determinism controls, timeout bounds, and module
placement. Design only; no source changes in this branch.

Out of scope: the wire-byte golden capture harness (DEL-3.a), the
property-test framework (DEL-3.b), the production wiring of the
parallel consumer (DEL-2.c/DEL-2.d), and the actual `DeleteFs` syscall
layer (tests use `RecordingDeleteFs` or lightweight stubs).

## 1. Goal

Prove that `ReorderBuffer` (DEL-2.a) and `CohortBatcher` (DEL-2.b)
preserve the strict wire-ordering invariant (DEL-1.a section 7) under:

1. **High concurrency** - up to 64 rayon workers racing to seal cohorts.
2. **Adversarial input orderings** - producer completion sequences
   designed to maximise head-of-line blocking, backpressure, and
   out-of-order fill.
3. **Scale** - cohort counts from 100 to 100 000.
4. **Rapid cohort transitions** - zero-op and single-op cohorts that
   stress the seal/drain hot path rather than the dispatch path.

The invariant under test: every consumer-observed drain sequence has
**monotonically increasing cohort rank**, and the per-cohort ops
appear in **producer-insertion (FIFO) order**. A violation at any
concurrency level, input pattern, or scale is a correctness failure.

## 2. Invariant definition

The concrete assertion applied to every scenario:

```text
for each pair of consecutively drained cohorts (C_i, C_{i+1}):
    assert C_i.rank < C_{i+1}.rank

for each drained cohort C:
    assert C.ops == producer_insertion_order(C)
```

This matches:
- DEL-1.b section 2.3 (strict cohort-order drain rule).
- DEL-1.c section 3.2 (contiguous-only batched drain).
- The debug-assertion in `ReorderBuffer::try_drain_ready` that panics
  on rank inversion.

For the `ParallelDeleteEmitter` (DEL-2.c) tests, the cross-cohort
invariant also requires that **cohort N's dispatch completes before
cohort N+1's dispatch begins**, verified by timeline instrumentation
as in the existing `cross_cohort_ordering_is_strict` test.

## 3. Concurrency levels

Every scenario runs at four concurrency tiers:

| Workers | Rationale |
|---------|-----------|
| 1 | Sequential baseline - proves the buffer degenerates to FIFO. |
| 4 | Typical laptop core count. Low contention, high throughput. |
| 16 | Server-class host. Enough workers to fill the 64-slot ring halfway. |
| 64 | Saturating ring - every slot can have a concurrent producer. Maximises `claim_cohort` contention and `not_full` backpressure. |

Workers are set via `RAYON_NUM_THREADS` environment variable with
`EnvGuard` isolation so concurrent test processes do not interfere.

## 4. Scale dimensions

Each scenario runs at four cohort counts:

| Cohorts | Ops per cohort | Total ops | Rationale |
|---------|----------------|-----------|-----------|
| 100 | 10 | 1 000 | Smoke test. Fast enough for debug builds. |
| 1 000 | 10 | 10 000 | Medium workload. Exercises multi-batch drain. |
| 10 000 | 5 | 50 000 | Large workload. Exercises backpressure at 64 workers. |
| 100 000 | 1 | 100 000 | Stress scale. Maximises drain-batch overhead. Single-op cohorts isolate the seal/drain path from dispatch cost. |

The 100 000-cohort tier is gated behind `#[cfg(not(debug_assertions))]`
because the BTreeMap overhead in debug builds makes it impractically
slow.

## 5. Adversarial input patterns

Each pattern is a function `(cohort_count, seed) -> Vec<(rank, ops)>`
that returns the cohort publication order the producer scope uses. The
patterns are designed to exercise specific failure modes in the
re-ordering buffer.

### 5.1 Reverse order

```text
Producer i seals cohort (N - 1 - i) before cohort i.
```

The consumer's head cohort (rank 0) is the last to seal. Every other
slot fills before the drain can begin, maximising ring occupancy. This
is the worst case for `not_full` backpressure: at 64 workers and 64
cohorts, every producer blocks on `not_full` except the one that holds
rank 0.

**Failure mode targeted:** consumer deadlocks if the drain loop
assumes the head always fills first. The re-ordering buffer must
tolerate non-head fills and wake the consumer only when head seals.

### 5.2 Alternating (even/odd interleave)

```text
Round 1: seal ranks 0, 2, 4, 6, ...
Round 2: seal ranks 1, 3, 5, 7, ...
```

The consumer can drain rank 0 immediately (even round fills it), but
rank 1 arrives only in the odd round. The drain stalls at every odd
rank, then resumes for the next even. This maximises the number of
`not_empty` Condvar wake-ups per cohort.

**Failure mode targeted:** batched drain (DEL-1.c section 3.2,
`DRAIN_BATCH_CAP = 8`) treats a partial batch as complete and emits
frames out of order. The strict contiguous-only rule must stop the
drain at the first unsealed slot.

### 5.3 Gaps (sparse rank space)

```text
Ranks are 0, 10, 20, 30, ... (stride 10).
```

The rank space is non-dense. The BTreeMap backing tolerates this by
construction, but a ring-based implementation (DEL-1.b sketch) would
waste slots. This pattern proves the buffer is correct under sparse
ranks, which matters for the INC_RECURSE segment-stride flattening
(DEL-1.c section 4, `SEGMENT_STRIDE = 1 << 20`).

**Failure mode targeted:** modulo-indexed ring collisions when two
cohorts with ranks N and N + ring_capacity map to the same slot. The
BTreeMap implementation is immune, but the test documents the
invariant for future implementations.

### 5.4 Random permutation (seeded)

```text
Ranks 0..N shuffled by a seeded PRNG.
```

The seeded shuffle covers the general case: arbitrary producer
completion order. The seed makes failures reproducible across CI runs.

**Failure mode targeted:** any ordering-dependent bug not covered by
the structured patterns above.

### 5.5 Burst-then-stall

```text
Producers 0..N/2 seal immediately. Producers N/2..N sleep for a
fixed duration (10 ms) then seal.
```

The consumer drains the first half rapidly, then stalls when the head
of the second half has not sealed yet. After the sleep, the second
half arrives in a burst. This pattern stresses the Condvar wake-up
path: the consumer must not miss the burst notification and park
forever.

**Failure mode targeted:** lost Condvar signal between the sleep
expiry and the consumer's `wait()` re-acquire. The
`Mutex<()> + Condvar` model from DEL-1.b section 3.3 prevents this by
design (the predicate is re-checked under the lock on every wake-up),
but the test proves it empirically.

### 5.6 Single slow producer (head stall)

```text
Producer 0 sleeps for 50 ms. All other producers seal immediately.
```

Every slot except rank 0 fills instantly. The ring reaches capacity,
producers block on `not_full`, and the consumer blocks on `not_empty`
because the head is unsealed. After the sleep, producer 0 seals rank
0, unblocking the entire pipeline. The consumer must drain all N
cohorts in strict rank order despite the delayed start.

**Failure mode targeted:** priority inversion where a fast producer
leapfrogs the slow head. The re-ordering buffer must never surface
rank 1 before rank 0, regardless of how long rank 0 takes.

### 5.7 Duplicate-rank attempt

```text
Two producers attempt to seal the same rank with different keys.
```

The second producer must receive a `RankConflict` error. This is not a
stress scenario per se, but it validates the buffer's invariant under
concurrent misuse. Run at 16 and 64 workers to exercise the
concurrent-insert race window.

### 5.8 Rapid empty cohorts

```text
All N cohorts have zero ops. Every producer calls
`register_empty + seal` with no insert.
```

This isolates the seal/drain hot path from any per-op overhead. At
100 000 cohorts and 64 workers the test produces ~100 000 Condvar
wake-ups (unbatched) or ~12 500 (batched at cap 8). The assertion
proves the drain order is still strictly monotonic when every cohort
is trivially empty.

**Failure mode targeted:** empty-cohort optimization that accidentally
skips the rank-ordering check. The buffer must treat empty cohorts
identically to populated ones for drain purposes.

## 6. Test scenarios

### 6.1 `ReorderBuffer` unit stress

Module: `crates/engine/src/delete/reorder_buffer.rs` (inline `#[cfg(test)]`
block or a dedicated `tests/delete_reorder_stress.rs` integration test).

Each test instantiates a `ReorderBuffer`, spawns N producers on a
rayon scope, and joins a single consumer thread that calls
`try_drain_ready` in a loop.

| Test name | Pattern | Scale | Workers | Timeout |
|-----------|---------|-------|---------|---------|
| `stress_reverse_order_w{1,4,16,64}_c{100,1k,10k,100k}` | 5.1 | all | all | 30 s |
| `stress_alternating_w{1,4,16,64}_c{100,1k,10k,100k}` | 5.2 | all | all | 30 s |
| `stress_gaps_w{1,4,16,64}_c{100,1k,10k}` | 5.3 | 100-10k | all | 30 s |
| `stress_random_w{1,4,16,64}_c{100,1k,10k,100k}` | 5.4 | all | all | 30 s |
| `stress_burst_stall_w{4,16,64}_c{100,1k}` | 5.5 | 100-1k | 4+ | 30 s |
| `stress_head_stall_w{4,16,64}_c{100,1k}` | 5.6 | 100-1k | 4+ | 30 s |
| `stress_empty_cohorts_w{1,4,16,64}_c{100,1k,10k,100k}` | 5.8 | all | all | 30 s |

Total: ~100 scenario combinations (7 patterns x 4 workers x 3-4
scales, minus inapplicable 1-worker concurrency for 5.5/5.6).

### 6.2 `CohortBatcher` integration stress

Module: `crates/engine/src/delete/cohort_batcher.rs` (inline
`#[cfg(test)]` block).

Each test instantiates a `CohortBatcher` behind an
`Arc<Mutex<CohortBatcher>>`, spawns N producers that call
`enqueue_cohort` through the mutex, and joins a consumer that calls
`drain_batch` in a loop.

The same patterns and scales as section 6.1 apply. The batcher wraps
the buffer, so the batcher tests primarily verify that the
single-call `enqueue_cohort` sealing and the `CohortBatch` grouping
do not violate the invariant the buffer provides.

Additional batcher-specific test:

| Test name | Pattern | Scale | Workers | Timeout |
|-----------|---------|-------|---------|---------|
| `stress_panic_mid_batch_w{4,16,64}_c1k` | 5.4 + panic at cohort N/2 | 1k | 4+ | 30 s |

This test injects a `record_panic` call at the midpoint and asserts
the consumer observes `is_panicked`, drains only cohorts 0..N/2 in
strict order, and exits cleanly.

### 6.3 `ParallelDeleteEmitter` end-to-end stress

Module: `crates/engine/src/delete/parallel_consumer.rs` (inline
`#[cfg(test)]` block), gated behind
`#[cfg(feature = "parallel-delete-consumer")]`.

Each test instantiates a `ParallelDeleteEmitter<RecordingDeleteFs>`,
publishes cohorts from a rayon scope via `enqueue_cohort`, signals
`mark_producers_done`, calls `run`, and asserts the
`RecordingDeleteFs` event log is in strict cohort-rank order.

| Test name | Pattern | Scale | Workers | Timeout |
|-----------|---------|-------|---------|---------|
| `e2e_random_w{1,4,16,64}_c{100,1k,10k}` | 5.4 | 100-10k | all | 60 s |
| `e2e_reverse_w{4,16,64}_c{100,1k}` | 5.1 | 100-1k | 4+ | 60 s |
| `e2e_burst_stall_w{16,64}_c1k` | 5.5 | 1k | 16, 64 | 60 s |
| `e2e_empty_cohorts_w{1,4,16,64}_c{1k,10k}` | 5.8 | 1k-10k | all | 60 s |

The e2e tests carry a higher timeout (60 s) because they spawn a
dedicated OS thread (the consumer) and exercise the Condvar wake-up
path end-to-end.

## 7. Verification assertions

Every test applies the following checks after the consumer finishes:

### 7.1 Rank monotonicity

```rust
let mut prev_rank: Option<u64> = None;
for (rank, _ops) in drained_sequence {
    if let Some(prev) = prev_rank {
        assert!(rank > prev, "rank inversion: {prev} -> {rank}");
    }
    prev_rank = Some(rank);
}
```

This is the primary correctness assertion. A failure here is a
stop-the-line bug.

### 7.2 FIFO preservation within cohort

```rust
for (rank, ops) in drained_sequence {
    let expected = producer_insertion_order.get(rank);
    assert_eq!(ops, expected, "FIFO violation in cohort rank={rank}");
}
```

Each producer builds a `Vec<DeleteOperation>` in a deterministic
order keyed by `(rank, op_index)`. The consumer's drained ops must
match that order exactly.

### 7.3 Completeness

```rust
assert_eq!(
    drained_sequence.len(),
    total_cohorts,
    "consumer lost cohorts: expected {total_cohorts}, got {}",
    drained_sequence.len(),
);
let total_ops: usize = drained_sequence.iter().map(|(_, ops)| ops.len()).sum();
assert_eq!(total_ops, expected_total_ops, "consumer lost ops");
```

Every published cohort must be drained exactly once. No cohort may be
duplicated or dropped.

### 7.4 Cross-cohort dispatch serialization (e2e only)

For `ParallelDeleteEmitter` tests, a timeline-instrumented `DeleteFs`
records `(cohort_rank, "start"/"finish")` events per dispatch. The
assertion proves every cohort N's last "finish" precedes cohort
N+1's first "start":

```rust
for window in consecutive_cohort_pairs {
    assert!(
        window.0.last_finish < window.1.first_start,
        "cohort {} started before cohort {} finished",
        window.1.rank, window.0.rank,
    );
}
```

This mirrors the existing `cross_cohort_ordering_is_strict` test
(DEL-2.c) at higher scale and concurrency.

### 7.5 No-hang timeout

Every test wraps the consumer join in a bounded-duration wait:

```rust
let outcome = rx.recv_timeout(TIMEOUT)
    .expect("consumer timed out - likely deadlocked");
```

A timeout is treated as a test failure, not a skip. The timeout values
(30 s for unit, 60 s for e2e) are conservative; under normal operation
the tests complete in under 5 s at the largest scale.

## 8. Determinism

All random-pattern tests use a **seeded PRNG** for reproducibility:

- RNG: `rand::rngs::StdRng` seeded via `rand::SeedableRng::seed_from_u64`.
- Default seed: `0xDEL3C` (a mnemonic constant).
- Seed is logged at test start so a failing CI run's log line
  suffices to reproduce locally.
- The seed is overridable via `DEL3C_SEED` environment variable for
  manual reproduction.

Worker scheduling is not deterministic (rayon's work-stealing is
nondeterministic), but the input pattern is fully deterministic for a
given seed. The test tolerates scheduling nondeterminism because the
invariant (rank monotonicity) must hold under any scheduling.

## 9. Resource bounds

### 9.1 Memory

At the largest scale (100 000 cohorts, 1 op each), peak buffer
occupancy is bounded by `MAX_BUFFERED_COHORTS = 64` slots. Each slot
holds one `DeleteOperation` (~100 bytes for a short path), so peak
buffer memory is ~6.4 KiB. The producer-side `Vec<(rank, ops)>` for
the full input is ~100 000 * 120 bytes = ~12 MiB, which is acceptable
for a stress test.

### 9.2 Threads

Rayon worker threads are bounded by `RAYON_NUM_THREADS`. The consumer
thread is one additional OS thread (spawned by
`ParallelDeleteEmitter::run`). Total: `RAYON_NUM_THREADS + 1` at
most.

### 9.3 Timeouts

| Layer | Timeout | Rationale |
|-------|---------|-----------|
| Per-test | 30 s (unit), 60 s (e2e) | Conservative bound; tests complete in < 5 s normally. |
| CI job | 300 s (from nextest profile) | The full stress suite at all tiers. |

Timeouts are enforced by `mpsc::Receiver::recv_timeout` in the test
harness, not by nextest's per-test timeout (which kills the process
and loses the failure context). The test-level timeout produces a
descriptive panic message.

### 9.4 CI gating

The 100 000-cohort tier is excluded from debug-assertion builds
(`#[cfg(not(debug_assertions))]`) because BTreeMap operations are
significantly slower with debug checks. All other tiers run in both
debug and release profiles.

## 10. Test infrastructure

### 10.1 Shared harness

A `del3c_harness` module provides:

- `AdversarialPattern` enum with variants for each pattern in
  section 5.
- `generate_input(pattern, cohort_count, ops_per_cohort, seed) ->
  Vec<(u64, Vec<DeleteOperation>)>` - builds the input sequence.
- `run_buffer_stress(pattern, cohort_count, ops_per_cohort, workers,
  seed, timeout)` - spawns producers, joins consumer, asserts
  invariants.
- `run_batcher_stress(...)` - same for `CohortBatcher`.
- `run_e2e_stress(...)` - same for `ParallelDeleteEmitter`.
- `TimelineDeleteFs` - `DeleteFs` implementation that records
  `(rank, start/finish)` events with nanosecond timestamps for
  cross-cohort serialization verification.

The harness is parameterised so individual tests are one-liners:

```rust
#[test]
fn stress_reverse_order_w16_c1k() {
    run_buffer_stress(Reverse, 1_000, 10, 16, DEFAULT_SEED, UNIT_TIMEOUT);
}
```

### 10.2 Parameterised test generation

A `macro_rules!` macro generates the combinatorial matrix:

```rust
macro_rules! stress_matrix {
    ($runner:ident, $pattern:ident, [$($workers:expr),+], [$($cohorts:expr),+]) => {
        $($(
            paste::paste! {
                #[test]
                fn [< stress_ $pattern _w $workers _c $cohorts >]() {
                    $runner($pattern, $cohorts, DEFAULT_OPS, $workers, DEFAULT_SEED, UNIT_TIMEOUT);
                }
            }
        )+)+
    };
}

stress_matrix!(run_buffer_stress, reverse, [1, 4, 16, 64], [100, 1_000, 10_000]);
```

This keeps the test source compact while generating the full
combinatorial matrix as individually named tests that nextest can
filter, retry, and report independently.

### 10.3 `EnvGuard` isolation

Each test uses `EnvGuard` to set `RAYON_NUM_THREADS` and restore the
original value on drop. Because rayon initialises its global thread
pool on first use and does not reinitialise, the stress tests must
each build a **custom `rayon::ThreadPool`** with the desired thread
count and run producers on that pool. The global pool is not touched.

```rust
let pool = rayon::ThreadPoolBuilder::new()
    .num_threads(workers)
    .build()
    .unwrap();
pool.install(|| {
    // producers run here
});
```

This avoids the OnceLock initialisation race that would make
concurrent stress tests with different worker counts interfere with
each other.

## 11. Integration with DEL-3.a and DEL-3.b

### 11.1 Relationship to DEL-3.a (wire-byte parity)

DEL-3.a ships a golden wire capture test
(`crates/engine/tests/delete_wire_parity.rs`) that captures the
sequential emitter's byte stream and asserts the parallel emitter
produces identical output. DEL-3.c does **not** duplicate that test;
it operates at the buffer/batcher layer below the wire codec. The two
suites are complementary:

- DEL-3.a proves the parallel path produces the right bytes.
- DEL-3.c proves the ordering primitive the parallel path depends on
  is correct under adversarial concurrency.

A DEL-3.c failure would eventually surface as a DEL-3.a failure too,
but DEL-3.c localises the root cause to the re-ordering buffer rather
than leaving it buried under wire codec noise.

### 11.2 Relationship to DEL-3.b (property tests)

DEL-3.b uses `proptest` to generate random cohort shapes (varying
entry counts, kinds, depth) and asserts sequential-vs-parallel
byte-stream equality. DEL-3.c differs in two ways:

- **Fixed adversarial patterns vs random generation.** DEL-3.b covers
  the shape space; DEL-3.c covers the scheduling space.
- **Concurrency as the variable, not cohort shape.** DEL-3.c holds
  cohort shape constant (uniform ops per cohort) and varies the
  producer completion order and worker count.

### 11.3 Module placement

```text
crates/engine/src/delete/
    reorder_buffer.rs         -- existing; inline unit tests extended
    cohort_batcher.rs         -- existing; inline unit tests extended
    parallel_consumer.rs      -- existing; inline e2e tests extended
    stress/
        mod.rs                -- harness (AdversarialPattern, runners)
        buffer_tests.rs       -- #[cfg(test)] ReorderBuffer stress matrix
        batcher_tests.rs      -- #[cfg(test)] CohortBatcher stress matrix
        e2e_tests.rs          -- #[cfg(test, feature = "parallel-delete-consumer")]
```

Alternatively, if the inline test blocks in `reorder_buffer.rs` and
`cohort_batcher.rs` are already large, the stress tests can live in
`crates/engine/tests/del3c_stress.rs` as an integration test. The
choice is an implementation detail; the harness API is the same either
way.

## 12. Failure analysis workflow

When a stress test fails:

1. The test logs the seed, pattern, worker count, and cohort count.
2. The test logs the rank-inversion pair (or the FIFO-violation index)
   with the full drained sequence dumped to stderr.
3. The developer reproduces locally with
   `DEL3C_SEED=<seed> cargo nextest run -p engine --all-features -E 'test(stress_<name>)'`.
4. The developer reduces the cohort count to the smallest value that
   still reproduces (binary search guided by the seed).
5. The developer enables `RUST_LOG=trace` on the buffer's internal
   tracing (added by DEL-2.a) to see per-slot state transitions.

## 13. Acceptance criteria

DEL-3.c is complete when:

1. The full stress matrix (section 6) passes on all three CI platforms
   (Linux, macOS, Windows) at the release profile.
2. No test exceeds its per-test timeout in any CI run across 5
   consecutive green runs (flake-free gate).
3. The 100 000-cohort tier at 64 workers completes in under 10 s
   wall-clock on the Linux CI runner (proves the buffer is not a
   throughput bottleneck).
4. Every adversarial pattern from section 5 is exercised at every
   applicable concurrency level.

## 14. Cross-references

- DEL-1.a upstream-ordering audit (the strictest invariant this test
  suite protects): `docs/design/del-1a-upstream-ordering-audit.md`.
- DEL-1.b re-ordering buffer spec (the data structure under test):
  `docs/design/del-1b-reordering-buffer.md`.
- DEL-1.c cohort batching strategy (the batching policy under test):
  `docs/design/del-1c-cohort-batching-strategy.md`.
- DEL-2.a buffer implementation: `crates/engine/src/delete/reorder_buffer.rs`.
- DEL-2.b batcher implementation: `crates/engine/src/delete/cohort_batcher.rs`.
- DEL-2.c parallel consumer implementation:
  `crates/engine/src/delete/parallel_consumer.rs`.
- Existing cross-cohort ordering test (DEL-2.c):
  `parallel_consumer.rs::tests::cross_cohort_ordering_is_strict`.
- Existing reorder-buffer tests (DEL-2.a):
  `reorder_buffer.rs::tests::*`.
- Existing batcher tests (DEL-2.b):
  `cohort_batcher.rs::tests::*`.
- Delta-pipeline reorder buffer (prior art for stress-test patterns):
  `crates/engine/src/concurrent_delta/` and
  `docs/design/streaming-reorder-buffer.md`.
- Memory note on reorder capacity default:
  `project_reorder_capacity_hard_default.md`.
