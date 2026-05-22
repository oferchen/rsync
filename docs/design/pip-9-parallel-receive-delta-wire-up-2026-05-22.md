# PIP-9 - parallel-receive-delta wire-up via RJN-3 fan-out

Date: 2026-05-22
Status: **OPEN** - follow-up to PIP-7 (#4730) investigation and PIP-8
dead-scaffolding teardown. The `parallel-receive-delta` feature flag
currently compiles as a no-op; PIP-9 is the task of giving it a real
production reader for the first time.

## Background

PIP-3+5 (#4666) wired `ReceiverContext::enable_parallel_receive_delta`
to swap a `ParallelDeltaPipeline` into a `delta_pipeline` field on the
receiver context. PIP-4 (#4720) added the `parallel-threshold-trip`
interop scenario and surfaced receiver-side corruption that wrote
wrong bytes for the first dispatched file. PIP-7 (#4730) reproduced
the failure off-host and bisected the only observable production
behaviour the feature flag enabled - the
`DeltaConsumer::spawn` side effect inside `ParallelDeltaPipeline::new`
- because the swapped pipeline field had 1 writer (the
`enable_parallel_receive_delta` setter) and 0 readers (no production
code path drained `delta_pipeline`). PIP-8 tore out the dead
scaffolding, leaving the `ParallelDeltaApplier`,
`ParallelDeltaPipeline`, and `DeltaConsumer` types compiled but
unwired.

## Goal

Replace the deleted Path B dispatch glue with a real wire-up that
routes the receiver's per-file delta apply through
`ParallelDeltaApplier` via the RJN-3 fan-out caller. The wire-up
must:

1. Have an observable production reader of the pipeline - no
   side-effect-only swaps.
2. Re-introduce a dispatch heuristic (file-count and total-bytes
   thresholds) only after the apply loop actually consumes the
   pipeline output, so the heuristic is meaningful rather than
   ornamental.
3. Re-validate the `parallel-threshold-trip` interop scenario under
   `cargo build --profile dist` (the build profile under which PIP-4
   originally surfaced the corruption) before re-adding the scenario
   to `tools/ci/run_interop.sh`.
4. Re-add `parallel-receive-delta` to the default feature set on
   `cli`, `core`, `transfer`, `engine`, and the workspace binary
   only after step (3) is green for at least three consecutive CI
   runs across all required-check matrices.

## Substrate to reuse

- `crates/engine/src/concurrent_delta/parallel_apply.rs` -
  `ParallelDeltaApplier`. The per-file slot registration / per-chunk
  verify fan-out / write commit machinery already lives here and
  ships unchanged through PIP-8. Benches
  (`parallel_receive_delta_perf`,
  `br_3j_f_dashmap_cores_vs_throughput`, `parallel_verify_chunk`)
  drive it directly today.
- `crates/transfer/src/delta_pipeline/parallel.rs` -
  `ParallelDeltaPipeline`. The bounded work-queue + `DeltaConsumer`
  glue PIP-7 identified as the suspected corruption source. Kept
  compiled so PIP-9 has a starting point, but the wire-up must add a
  real reader before re-introducing the `new()` side effect into a
  production code path.
- `crates/transfer/src/delta_pipeline/chunk_builder.rs` -
  `ChunkBuilder`. Already feeds `ParallelDeltaApplier::apply_one_chunk`
  in tests; PIP-9 needs to plug it into the receiver's token loop
  rather than the test scaffolding.
- RJN-3 fan-out work - see
  `docs/design/parallel-receive-delta-application.md` (the umbrella
  design) for the apply-loop architecture and per-file ordering
  invariants the new wiring must preserve.

## Acceptance

PIP-9 is complete when:

- The receiver's token loop dispatches per-file work through
  `ParallelDeltaApplier` via the RJN-3 fan-out caller, with a
  production reader that drains pipeline output and commits bytes to
  disk in submission order.
- The `parallel-threshold-trip` interop scenario is back in
  `tools/ci/run_interop.sh` (no env gate) and passes green for both
  `up:` (upstream sender -> oc-rsync receiver) and `oc:` (oc-rsync
  sender -> upstream receiver) directions across every required-check
  matrix.
- `parallel-receive-delta` is back in the default feature set on
  `cli`, `core`, `transfer`, `engine`, and the workspace `Cargo.toml`.
- The deferral / supersession notes in
  `docs/design/parallel-receive-delta-default-on.md`,
  `docs/design/pip-4-closure-2026-05-21.md`,
  `docs/design/br-6-sign-off-check-in-2026-05-21.md`,
  `CHANGELOG.md`, and `README.md` are removed or updated to cite the
  PIP-9 re-promotion.

## References

- PR #4730 - PIP-7 investigation that proved the dispatch scaffolding
  was dead code (1 writer / 0 readers).
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
  - PIP-7 investigation results and the PIP-8 resolution note.
- `docs/design/parallel-receive-delta-application.md` - umbrella
  design covering the apply-loop architecture, per-file ordering
  invariants, and wire-format parity strategy that PIP-9 must
  preserve.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` -
  `ParallelDeltaApplier` substrate.
- `crates/transfer/src/delta_pipeline/parallel.rs` -
  `ParallelDeltaPipeline` substrate.
- `crates/engine/src/concurrent_delta/consumer/mod.rs` -
  `DeltaConsumer::spawn`; the construction step PIP-7 identified as
  the suspected corruption source. PIP-9 must add a real reader for
  the consumer's output before re-introducing the spawn into a
  production code path.
