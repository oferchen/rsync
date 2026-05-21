# RJN-1 - `apply_chunk_parallel` call sites and per-chunk dispatch benefit

Date: 2026-05-21
Scope: read-only research for RJN-2
Tracked under: #2557

## Goal

`ParallelDeltaApplier::apply_chunk_parallel`
(`crates/engine/src/concurrent_delta/parallel_apply.rs:468`) dispatches its
single chunk through:

```rust
let (verified, _) = rayon::join(|| Self::verify_chunk(strategy.as_ref(), chunk), || ());
//                                                                              ^^^^^
//                                                                  second closure: no-op
```

(line `crates/engine/src/concurrent_delta/parallel_apply.rs:477`)

The second closure is `|| ()`. `rayon::join` schedules its two closures so the
verify runs on a rayon worker, but the caller still blocks until that single
worker returns. Per-chunk dispatch therefore yields **no cross-chunk
parallelism** - only `apply_batch_parallel`
(`crates/engine/src/concurrent_delta/parallel_apply.rs:499`) actually fans
verifies across the rayon pool, by collecting `chunks.into_par_iter().map(...)`
into a single `Vec`.

This audit catalogues every call site of both functions in the workspace,
estimates whether each site is hot enough for the per-chunk dispatch shape to
matter, and feeds the call-shape recommendation into RJN-2.

## 1. Call sites - exhaustive grep

Source: `grep -rn "apply_chunk_parallel\|apply_batch_parallel" --include='*.rs' .`
filtered to non-`//` lines.

### 1.1 `apply_chunk_parallel`

| # | File:line | Kind | Surrounding gate |
|---|-----------|------|------------------|
| 1 | `crates/engine/src/concurrent_delta/parallel_apply.rs:468` | definition (`pub fn`) | `#[cfg(feature = "parallel-receive-delta")]` implicit via `pub mod parallel_apply` gate (see 1.3 below) |
| 2 | `crates/engine/src/concurrent_delta/parallel_apply.rs:720` | unit test (`single_file_in_order_matches_sequential`) | `#[cfg(test)] mod tests` |
| 3 | `crates/engine/src/concurrent_delta/parallel_apply.rs:741` | unit test (`single_file_out_of_order_preserves_byte_order`) | `#[cfg(test)] mod tests` |
| 4 | `crates/engine/src/concurrent_delta/parallel_apply.rs:784` | unit test (`missing_file_registration_errors`) | `#[cfg(test)] mod tests` |
| 5 | `crates/engine/src/concurrent_delta/parallel_apply.rs:807` | unit test (`finish_file_with_pending_chunks_errors`) | `#[cfg(test)] mod tests` |
| 6 | `crates/engine/src/concurrent_delta/parallel_apply.rs:821` | unit test (`bytes_written_tracks_in_order_writes`) | `#[cfg(test)] mod tests` |
| 7 | `crates/engine/src/concurrent_delta/parallel_apply.rs:825` | unit test (`bytes_written_tracks_in_order_writes`) | `#[cfg(test)] mod tests` |
| 8 | `crates/engine/src/concurrent_delta/parallel_apply.rs:867` | proptest (`random_chunk_sizes_and_permutations_match_sequential`) | `#[cfg(test)] mod tests` |
| 9 | `crates/engine/src/concurrent_delta/parallel_apply.rs:881` | unit test (`cursor_writer_round_trip`) | `#[cfg(test)] mod tests` |
| 10 | `crates/engine/src/concurrent_delta/parallel_apply.rs:985` | unit test (`unverified_chunk_preserves_writer_byte_stream`) | `#[cfg(test)] mod tests` |
| 11 | `crates/engine/src/concurrent_delta/parallel_apply.rs:1015` | unit test (`verify_chunk_accepts_matching_digest_md5`) | `#[cfg(test)] mod tests` |
| 12 | `crates/engine/src/concurrent_delta/parallel_apply.rs:1033` | unit test (`verify_chunk_accepts_matching_digest_xxh3`) | `#[cfg(test)] mod tests` |
| 13 | `crates/engine/src/concurrent_delta/parallel_apply.rs:1050` | unit test (`verify_chunk_rejects_mismatched_digest_and_does_not_write`) | `#[cfg(test)] mod tests` |
| 14 | `crates/engine/tests/parallel_apply_concurrent.rs:125` | integration test (`concurrent_files_under_dashmap_shards_match_expected_bytes`) | `#![cfg(feature = "parallel-receive-delta")]` |
| 15 | `crates/engine/tests/parallel_apply_concurrent.rs:222` | integration test (`concurrent_register_and_dispatch_on_overlapping_files`) | `#![cfg(feature = "parallel-receive-delta")]` |
| 16 | `crates/engine/tests/arc_drain_panic_recovery.rs:184` | integration test (`parallel_applier_finish_file_surfaces_typed_applier_still_referenced`) | `#[cfg(feature = "parallel-receive-delta")] mod parallel_apply_drain` |
| 17 | `crates/transfer/src/delta_pipeline/chunk_builder.rs:411` | unit test (`end_to_end_corrupted_basis_fails_verify`) | `#[cfg(test)] mod tests` inside `#[cfg(feature = "parallel-receive-delta")] pub mod chunk_builder` |

**Production call sites: 0.** Every non-definition call sits inside a `#[cfg(test)]`
boundary or an integration test gated on `parallel-receive-delta`.

### 1.2 `apply_batch_parallel`

| # | File:line | Kind | Surrounding gate |
|---|-----------|------|------------------|
| 1 | `crates/engine/src/concurrent_delta/parallel_apply.rs:499` | definition (`pub fn`) | gated module (1.3) |
| 2 | `crates/engine/src/concurrent_delta/parallel_apply.rs:775` | unit test (`batch_apply_matches_sequential_byte_for_byte`) | `#[cfg(test)] mod tests` |
| 3 | `crates/engine/src/concurrent_delta/parallel_apply.rs:1077` | unit test (`verify_batch_rejects_mismatched_digest`) | `#[cfg(test)] mod tests` |
| 4 | `crates/engine/src/concurrent_delta/parallel_apply.rs:1109` | unit test (`parallel_apply_with_real_digests_matches_sequential_byte_for_byte`) | `#[cfg(test)] mod tests` |
| 5 | `crates/engine/benches/parallel_receive_delta_perf.rs:263` | criterion bench (`parallel_apply` fn used by `bench_workload`) | `#![cfg(feature = "parallel-receive-delta")]` |
| 6 | `crates/engine/benches/parallel_verify_chunk.rs:206` (BR-3i.f branch only - PR not yet merged at the time of this audit; see commit `9a4b97f53`) | criterion bench (`run_apply`) | `#![cfg(feature = "parallel-receive-delta")]` |

**Production call sites: 0.** Both benches that exercise the batch path use it
exclusively, but neither is a production code path - they live under
`crates/engine/benches/`.

### 1.3 Module-level feature gating

`ParallelDeltaApplier`, `apply_chunk_parallel`, and `apply_batch_parallel` are
re-exported at `crates/engine/src/concurrent_delta/mod.rs:189`:

```text
pub use parallel_apply::{DeltaChunk, ParallelApplyError, ParallelDeltaApplier};
```

The whole `parallel-receive-delta` feature stays opt-in for production binaries
even though `crates/engine/Cargo.toml:58` lists it in the default feature set
of the engine crate itself - the receiver and CLI never invoke
`enable_parallel_receive_delta` outside tests, and
`crates/transfer/src/receiver/mod.rs:295` initialises the production receiver
with `SequentialDeltaPipeline::new()` regardless. The threshold-aware
`ThresholdDeltaPipeline` and `ParallelDeltaPipeline` types in
`crates/transfer/src/delta_pipeline/` route `DeltaWork` items through
`engine::concurrent_delta::consumer::DeltaConsumer` and
`engine::concurrent_delta::strategy::dispatch`, not through
`ParallelDeltaApplier`. Greps confirm `ParallelDeltaApplier` is not referenced
from `consumer.rs`, `strategy.rs`, or any production module under
`crates/transfer/src/` outside the `chunk_builder.rs` test mod.

## 2. Per-site shape and parallelism benefit

Estimates assume the BR-3i.c verifier (`strategy.compute(&chunk.data)`) is the
dominant per-chunk CPU cost. MD5 throughput on a modern Arm core (`aarch64`
NEON path) is approximately 600 MiB/s/core; XXH3 is approximately 10 GiB/s/core.
For a 64 KiB chunk the verify takes roughly 100 us (MD5) or 6 us (XXH3) - well
above rayon's microsecond-scale fork/join cost. "Chunks/sec" below is the
order-of-magnitude rate the call site would deliver in steady state if the
per-chunk path were the only verifier; "parallelism benefit" reports the
realised speed-up from rayon at that shape.

### 2.1 `apply_chunk_parallel` sites

| # | Caller | Chunks/sec (order) | Parallelism benefit | Easy to batch? | Notes |
|---|--------|--------------------|---------------------|----------------|-------|
| 2-13, 17 | per-test single calls (`parallel_apply.rs`, `chunk_builder.rs`) | <= 32/iteration | Zero (single chunk per call; second join arm is `|| ()`) | N/A | Each test deliberately exercises the per-chunk shape. Renaming the function would not change the test value. |
| 14 | `parallel_apply_concurrent.rs:125` (`concurrent_files_under_dashmap_shards_match_expected_bytes`) | TOTAL_OPS=10_000 across WORKERS=8 -> approximately 1250 chunks/worker/sec when each chunk is 16 bytes; the verify is a no-op on the per-chunk path because the second closure is `|| ()` and the chunk has no `expected_strong` | Zero per call. Across the test the rayon `par_iter` over `0..WORKERS` is the parallelism source, not `apply_chunk_parallel` itself. | Hard - the test deliberately drives concurrent register/dispatch via `par_iter` to exercise DashMap shards, not the verify path. | The stress test is a contention probe, not a throughput probe. |
| 15 | `parallel_apply_concurrent.rs:222` (`concurrent_register_and_dispatch_on_overlapping_files`) | SMALL_OPS=4_000 / WORKERS=8 | Same as above. | Hard - same shape as #14. | Tests the unknown-NDX race. |
| 16 | `arc_drain_panic_recovery.rs:184` (`parallel_applier_finish_file_surfaces_typed_applier_still_referenced`) | 1/test | Zero (single call). | N/A - test is asserting drop ordering, not throughput. | Deliberately invokes the per-chunk path to keep the writer-side `Arc` live while `finish_file` races. |
| 17 | `chunk_builder.rs:411` (`end_to_end_corrupted_basis_fails_verify`) | 1/test | Zero. | N/A. | Single-shot mismatch assertion. |

**No production call site exists today.** Every site is either a test exercising
the typed-error path or a contention probe that depends on the per-chunk shape
specifically.

### 2.2 `apply_batch_parallel` sites

| # | Caller | Chunks/sec (order) | Parallelism benefit | Easy to batch? | Notes |
|---|--------|--------------------|---------------------|----------------|-------|
| 2-4 | unit tests (`parallel_apply.rs:775`, `:1077`, `:1109`) | <= 48 chunks/iteration | Bounded by `into_par_iter` over the batch; speed-up is real but the batch is tiny so the practical win is masked by `rayon::scope` overhead. | Already batched. | Coverage tests. |
| 5 | `parallel_receive_delta_perf.rs:263` (`parallel_apply` in `bench_workload`) | Workloads small_files (~10_240 chunks), mixed (~5_000 chunks at 64 KiB), large_files (~16_384 chunks at 64 KiB). | Real - workload spans the rayon pool; the bench's whole purpose is to measure this. | Already batched (the bench feeds a single `Vec<DeltaChunk>` to `apply_batch_parallel`). | The bench reports parallel-vs-sequential and is the gate for promotion (`docs/design/parallel-receive-delta-default-on.md`). |
| 6 | `parallel_verify_chunk.rs:206` (BR-3i.f, branch `perf/engine-parallel-verify-chunk-bench-br3if`, commit `9a4b97f53`) | Workload A 1024 chunks x 1 MiB; Workload B 16384 chunks x 16 KiB | Real - sweep across `{1, 2, 4, available_parallelism, 8}` workers and `{MD4, MD5, XXH3}` strategies. | Already batched. | Targets cores-vs-throughput curve for the verify step. |

## 3. Where the per-chunk path is exercised, and why it does not matter

1. **Receiver production loop**: the transfer crate's production receiver routes
   delta work through `SequentialDeltaPipeline`
   (`crates/transfer/src/receiver/mod.rs:295`) or, when
   `enable_parallel_receive_delta` is invoked, through `ParallelDeltaPipeline`
   (`crates/transfer/src/delta_pipeline/parallel.rs:46`). Both pipelines speak
   `DeltaWork` -> `DeltaResult` via `DeltaConsumer`. **Neither pipeline touches
   `ParallelDeltaApplier`.** As of today the applier is unused by the production
   binary.
2. **`ChunkBuilder` adapter**: BR-3i.d (PR #4646) added
   `crates/transfer/src/delta_pipeline/chunk_builder.rs` to convert wire tokens
   into `DeltaChunk` values one-at-a-time. The builder has **no non-test
   callers** in the workspace (grep for `ChunkBuilder::new`, `.matched_chunk`,
   `.literal_chunk`, `.next_chunk` returns only `chunk_builder.rs` itself). When
   it is eventually wired into the receiver pipeline, today's natural plumbing
   would call `apply_chunk_parallel` per token (mirroring the test at
   `chunk_builder.rs:411`), which would hit the misleading no-op `rayon::join`
   path.
3. **Stress tests** (`parallel_apply_concurrent.rs`) probe DashMap shard
   contention; they require the per-chunk entry point precisely because they
   need to drive concurrent `register_file` + dispatch races. Renaming the
   function does not affect their value.
4. **Bench harnesses** (`parallel_receive_delta_perf.rs`,
   `parallel_verify_chunk.rs` from BR-3i.f) exercise **only**
   `apply_batch_parallel`. The cores-vs-throughput evidence the project relies
   on is therefore measuring the batch path, not the per-chunk path the
   eventual receiver wiring would call.

### Conclusion

The per-chunk `apply_chunk_parallel` path:

- Is used today only by tests that either (a) sanity-check the typed-error
  surface, (b) probe DashMap shard contention, or (c) target a single-chunk
  shape on purpose for drop-ordering assertions.
- Will be exercised by the future receiver pipeline if BR-3i.d's `ChunkBuilder`
  is wired in without further changes - the test at `chunk_builder.rs:411` is
  the canonical example of that shape.
- Has **zero cross-chunk parallelism**, because the second `rayon::join` arm is
  `|| ()`. Even when called inside an ambient rayon scope, only one verify per
  call runs on a worker before the caller blocks.

Real-traffic call sites are therefore split into two bins:

- **Test/stress sites** (1.1 #2-17 above) where parallelism is irrelevant by
  construction. The name `apply_chunk_parallel` is misleading here, but the
  semantics are correct: schedule one verify onto rayon, wait for it.
- **No production hot path exists today.** Every batchable producer (the bench
  harnesses) already calls `apply_batch_parallel`. The one producer that
  currently emits chunks one-at-a-time (the BR-3i.d `ChunkBuilder`) is not yet
  wired into the receiver pipeline, so it has not started exercising the
  per-chunk path in production traffic.

## 4. RJN-2 recommendation: rename for clarity, defer fan-out refactor

Two candidate fixes were on the table:

- **Rename** `apply_chunk_parallel` to something that does not promise
  cross-chunk parallelism (e.g. `apply_chunk_serial`, `apply_chunk_one`,
  `apply_chunk_offthread`).
- **Refactor** to a batched fan-out (drop the `pub fn apply_chunk_parallel`
  entry point entirely and force callers through `apply_batch_parallel`, or
  introduce an internal accumulator that batches per-NDX before dispatching).

### Recommended: rename only.

Justifications:

1. **No production hot path is on the per-chunk method today.** A fan-out
   refactor cannot deliver a throughput win against zero production callers.
2. **Tests deliberately use the single-chunk shape.** The DashMap stress tests
   (`parallel_apply_concurrent.rs`) and the drop-ordering test
   (`arc_drain_panic_recovery.rs`) need a per-chunk entry point that returns
   when the chunk has been ingested. Removing the entry point would force the
   tests to allocate a one-element `Vec` per call, adding noise without adding
   coverage.
3. **The naming is the bug.** The project memory note
   `project_rayon_join_per_chunk_noop.md` flags this precisely: the issue is
   that the name implies cross-chunk parallelism. A rename plus a one-paragraph
   rustdoc edit ("schedules a single verify onto a rayon worker and waits;
   exposes no cross-chunk fan-out - use [`apply_batch_parallel`] for that")
   cures the misleading-name problem at zero risk to existing tests.
4. **The fan-out refactor belongs with the receiver-wiring work.** When BR-3i.d
   plumbs `ChunkBuilder` into the receiver pipeline, the right answer is for
   the receiver to accumulate chunks into a per-file batch before dispatching
   (the natural shape because `chunk_sequence` is per-file). That accumulation
   belongs on the receiver side, not behind a renamed applier method, so the
   accumulator can also drive the existing `DeltaConsumer` reorder buffer
   without re-allocating intermediate `Vec`s. RJN-2 should land the rename;
   RJN-3 (receiver-side batching) is where the throughput work belongs.

Suggested rename targets, ordered by clarity:

1. `apply_chunk_offthread(chunk)` - explicit about "verify runs off the calling
   thread; no cross-chunk parallelism".
2. `apply_one_chunk(chunk)` - mirrors `apply_batch` naming convention.
3. `submit_chunk(chunk)` - if the eventual receiver flow looks asynchronous.

Recommended target: `apply_one_chunk` for parallelism with `apply_batch` and
because the method does NOT submit-and-return (it blocks for the verify).

## 5. Cross-reference: BR-3i.f bench harness

The BR-3i.f bench (`crates/engine/benches/parallel_verify_chunk.rs`) on branch
`perf/engine-parallel-verify-chunk-bench-br3if` (commit `9a4b97f53`) calls
`applier.apply_batch_parallel(workload.chunks.clone())` at line 206 inside
`run_apply`. It does NOT call `apply_chunk_parallel` anywhere.

Same shape for the existing in-tree bench
`crates/engine/benches/parallel_receive_delta_perf.rs` at line 263.

### Implication

The cores-vs-throughput curves the project will collect from BR-3i.f measure
the **batch** path's scaling, not the per-chunk path that any naive
`ChunkBuilder` wiring would hit. If the receiver pipeline ships against
`apply_chunk_parallel` (as the BR-3i.d test demonstrates), the production
shape will differ measurably from the bench shape - the bench result will
over-state the achievable scaling.

### Bench-harness extension question (defer to RJN-4)

A parallel cell in the BR-3i.f harness that drives the same workload through
`apply_chunk_parallel` (one chunk per call, no internal batching) would let
RJN-4 quantify the per-chunk-vs-batch gap concretely. The harness already
generates per-chunk `expected_strong` digests up front, so adding a
`run_apply_per_chunk` variant is purely additive. Concrete bench design,
sample budget, and acceptance thresholds for that extension are out of scope
here; RJN-4 will own the harness change and the measurements.

## Provenance

- `grep -rn "apply_chunk_parallel\|apply_batch_parallel" --include='*.rs'` over
  the worktree at branch `docs/audits/rjn-1-apply-chunk-parallel-sites`.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` at HEAD of branch.
- `crates/transfer/src/delta_pipeline/chunk_builder.rs` at HEAD of branch
  (BR-3i.d landed in PR #4646, commit `6907fc916`).
- `crates/engine/benches/parallel_verify_chunk.rs` at commit `9a4b97f53`
  on branch `perf/engine-parallel-verify-chunk-bench-br3if` (BR-3i.f).
- Project memory: `project_rayon_join_per_chunk_noop.md` (2026-05-21 entry).

No code was modified by this audit.
