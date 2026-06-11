# PIP-7 lessons learned: adversarial-ordering discipline for concurrent paths

Date: 2026-06-10
Status: post-mortem
Scope: the broader engineering discipline lesson from the PIP-7
parallel-receive-delta receiver corruption defect, generalised to all
future concurrent code paths (DASYNC, RUSSH-ASY, ASY-G, and the
DG-series races).
Tracker: PIP-LESSONS series (#4000-#4003)
References:
- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
  - the defect-level post-mortem and immediate mitigation
- `docs/design/pip-10b-adversarial-chunk-ordering-stress.md` - the
  adversarial-ordering stress test spec that codified the discipline
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` -
  the wire-up plan that re-introduces the parallel apply path

## 1. Background

The `parallel-receive-delta` feature is the receive-side fan-out for
delta application: per-file slot registration, per-chunk strong-checksum
verify on rayon workers, and serial commit to the destination writer.
PIP-4 added an interop scenario (`parallel-threshold-trip`) that exceeds
the file-count threshold and exercises the dispatch path. PIP-5
flipped the feature into the default feature set across `cli`, `core`,
`transfer`, and `engine`. The intent was that default `cargo build
--release` would exercise the parallel path on workloads above the
threshold and the interop matrix would catch regressions before
release.

## 2. Defect

PIP-7 surfaced as a deterministic content mismatch on the **first
dispatched file** of any tree with more than
`PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100` files. The
`parallel-threshold-trip` CI scenario built a source directory of 120
files holding `pt-payload-NNN\n` (~15 bytes each) and observed:

```
parallel-threshold: content mismatch for parallel_threshold/file_1.txt
```

The corruption was concentrated on `file_1.txt` (the lexically first
entry in the file list) and reproduced in both transfer directions
(`up:` upstream sender to oc-rsync receiver and `oc:` oc-rsync sender
to upstream receiver), indicating a side effect at parallel-pipeline
construction rather than a per-direction bug. Subsequent files
(`file_2.txt`, `file_3.txt`, ...) compared byte-identical.

The PIP-8 investigation identified the root cause as a
1-writer / 0-readers dead-state scaffolding: the
`enable_parallel_receive_delta()` swap installed a
`ParallelDeltaPipeline` into `ReceiverContext::delta_pipeline`, but the
production receive loop never consumed the field. The only observable
side effect of the swap was the `DeltaConsumer::spawn` call that
started a background reorder thread, and that thread raced the first
sequential write through the disk-commit SPSC channel. PIP-8 deleted
the dead scaffolding; PIP-9 re-wires the parallel apply path **with**
an observable reader.

The receiver-corruption defect itself is well documented in
`pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`. This
document covers the broader lesson: how the original tests missed it,
how the adversarial-ordering corpus finally caught it, and what
discipline to apply to every future concurrent path.

## 3. Why the original tests missed it

The test surfaces that exercised `parallel-receive-delta` before PIP-5
flipped it default-on were:

- `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` unit
  tests: `single_file_out_of_order_preserves_byte_order`,
  `batch_apply_matches_sequential_byte_for_byte`,
  `random_chunk_sizes_and_permutations_match_sequential` (proptest).
- `crates/engine/src/concurrent_delta/reorder/tests.rs` unit tests:
  in-order, out-of-order, capacity bounds, drain.
- `crates/engine/tests/pipeline_reorder_integration.rs` end-to-end
  pipeline test with 500 in-memory items.

Every one of these surfaces drove `ParallelDeltaApplier` directly,
exercised its sequencing invariant under randomly permuted chunks,
and produced byte-identical output to the sequential reference.
**None of them caught PIP-7.**

The miss is structural, not a coverage gap that another in-order or
random-permutation test would have closed. Three factors aligned:

1. **The tests exercised the applier, not the wire-up.** The defect
   was not in `ParallelDeltaApplier::apply_batch_parallel` or in the
   per-file reorder buffer. Both were correct. The defect was a side
   effect of constructing a `ParallelDeltaPipeline` that the receive
   loop never consumed - a wire-up bug that only manifested when the
   pipeline was instantiated against a real `ReceiverContext` mid-transfer.
2. **The tests skipped the threshold trip.** `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD`
   was a runtime decision in `transfer/src/receiver/mod.rs`, gated on
   file-list size. The unit tests bypassed it by constructing the
   applier directly. The CI interop scenario `parallel-threshold-trip`
   was the only surface that crossed the threshold, and PIP-4 added it
   **after** PIP-5 flipped the feature default-on.
3. **The tests ran sequential setup.** None forced the threshold to
   trip mid-transfer, mid-handshake, or against a partially-built
   `ReceiverContext`. The DeltaConsumer-vs-disk-commit race was a
   construction-time race; sequential test setup never reproduced it.

The takeaway: **structural test coverage of the dispatch primitive is
not coverage of the runtime wire-up.** Default-on promotion requires
runtime evidence at the integration boundary, not unit coverage of the
parallel apply machinery.

## 4. Discovery: the adversarial-ordering corpus that caught it

PIP-9.c added an adversarial-ordering test corpus to the per-file
reorder path and asserted a SHA-256 oracle: the parallel output bytes
must hash equal to the sequential reference for every file under every
adversarial ordering pattern. The corpus design is captured in
`docs/design/pip-10b-adversarial-chunk-ordering-stress.md`. The minimal
fixture set distilled from PIP-9.c (and from the DG-3 sibling races
documented in `project_concurrent_dispatch_test_flake` and
`project_slothandle_decrementguard_release_race`) that would have
caught PIP-7 earlier is:

1. **Threshold-trip mid-batch.** A file-list that crosses
   `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD` partway through delivery.
   The first file dispatched after the trip must SHA-256-match its
   sequential reference. PIP-7's `file_1.txt` corruption surfaced
   exactly here: the first file's COPY tokens resolved while the
   pipeline was still bringing up its background consumer thread.
2. **Reverse-completion within a file.** All chunks for `file_1`
   arrive in strictly descending `chunk_sequence` order. The reorder
   buffer fills before the first chunk can drain; any race between
   `force_insert` growth and the disk-commit writer surfaces as a
   content mismatch.
3. **First-file dispatch after pipeline construction.** Construct
   the parallel pipeline, immediately submit a single-file batch,
   and assert SHA-256 equality. This is the minimal repro for PIP-7's
   construction-time side effect, independent of file count.
4. **Slot recycle while DecrementGuard alive.** A worker holds an
   `Arc<BarrierState>` clone past `notify_all`; the registrar
   recycles the slot for a new NDX before the worker's Arc drops.
   This is the DG-3 race shape from `parallel_apply_dg3_stress.rs`,
   and it shares PIP-7's pattern: a construction-time Arc transition
   that the sequential test path never exercises.
5. **Cross-file chunk arrival.** Chunks for `file_2` arrive before
   any chunk for `file_1` in the same batch. The pipeline-level
   reorder buffer must hold `file_2`'s results while `file_1`
   blocks on its first chunk. Any inversion in the slot-map lookup
   keyed by file NDX surfaces as a cross-file content swap.

Each case carries a SHA-256 assertion on the reconstructed
destination bytes against the sequential reference. The assertion is
the load-bearing invariant: any wire-up regression that corrupts the
output - including construction-time side effects that the sequential
path never sees - flips the SHA-256 and surfaces the bug.

## 5. Recommendations

The lesson generalises beyond PIP-7. Every new concurrent code path
needs adversarial-ordering stress fuzz as a **design-time invariant**,
not a post-ship discovery. The candidates currently in flight that
must adopt this discipline before any default-on flip:

- **DASYNC** (delta-apply async dispatch series): adversarial chunk
  ordering across files, mid-transfer threshold trips,
  construction-time side-effect repro.
- **RUSSH-ASY** (russh async SSH transport): adversarial frame
  arrival ordering, connection-establishment races,
  spawn_blocking-bridge saturation patterns.
- **ASY-G** (async generator wire-up): adversarial file-list arrival
  ordering, INC_RECURSE segment boundary races, mid-segment
  threshold trips.

For each series the discipline is:

1. **Before the flip-default decision task,** ship a stress-fuzz
   harness that:
   - Generates adversarial orderings (chunk reorder, file boundary
     races, slot-barrier exhaustion, threshold-trip switching).
   - Asserts a SHA-256 oracle on the reconstructed output against
     the sequential reference.
   - Exercises the **wire-up** path (real `ReceiverContext`,
     `TransferOps`, or `Transport` instance), not only the
     dispatch primitive.
2. **Extend the WDF differential fuzzer** (#3119) with
   concurrent-ordering arms. WDF is the standard wire-level
   harness; new concurrent paths should add their adversarial
   patterns there alongside the structured input generators.
3. **Seed the corpus with the known regression shapes.** Every
   concurrent-path series carries forward the PIP-7 threshold-trip
   seed and the DG-3 slot-recycle seed as a regression floor.
4. **Bisect under the release profile.** PIP-7 reproduced under
   `cargo build --profile dist` (LTO, panic=abort, opt-level=z) but
   not under `cargo build --release`. Adversarial-ordering fuzz
   runs must reproduce under the same profile the production
   release uses.

The fix-forward path for `parallel-receive-delta` itself is PIP-9
(wire-up) followed by re-promotion to default-on once the
adversarial-ordering harness is green on both `up:` and `oc:`
directions across all forced protocol tiers (28/29/30/native).

## 6. References

- `docs/design/pip-7-parallel-receive-delta-receiver-corruption-2026-05-22.md`
  - defect-level post-mortem, investigation results, PIP-8 resolution.
- `docs/design/pip-9-parallel-receive-delta-wire-up-2026-05-22.md` -
  proper RJN-3 fan-out wire-up plan, supersedes the dead PIP-5
  scaffolding.
- `docs/design/pip-10b-adversarial-chunk-ordering-stress.md` -
  comprehensive adversarial ordering stress test design.
- `crates/engine/tests/parallel_apply_dg3_stress.rs` - DG-3 stress
  test that motivates the slot-recycle race seed.
- `crates/engine/tests/parallel_apply_concurrent.rs` - the
  `concurrent_register_and_dispatch_on_overlapping_files` test that
  exposed the DG-3 release race on Windows.
- `fuzz/fuzz_targets/parallel_receive_delta_adversarial.rs` - the
  adversarial-ordering fuzz target seeded with the PIP-7 and DG-3
  regression shapes.
