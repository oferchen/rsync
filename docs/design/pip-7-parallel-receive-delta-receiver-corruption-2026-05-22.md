# PIP-7 - parallel-receive-delta receiver corruption

Date: 2026-05-22
Status: **OPEN - urgent regression discovered by PIP-4 (#4720) interop scenario.**

Tracking: follow-up to #4720 (PIP-4 - added the `parallel-threshold-trip`
interop scenario) and #4666 (PIP-5 - flipped `parallel-receive-delta`
into default features across `cli`, `core`, `transfer`, and `engine`).

The immediate-mitigation PR (this PR) reverts the PIP-5 default flip and
removes the failing interop scenario so default `cargo build --release`
no longer corrupts data; the receiver corruption itself is still open
and must be fixed before the feature can return to the default set.

## Symptom

The `parallel-threshold-trip` interop scenario fails with

```
parallel-threshold: content mismatch for parallel_threshold/file_1.txt
```

in **both** directions of the matrix:

- `up:` upstream rsync sender -> oc-rsync receiver.
- `oc:` oc-rsync sender -> upstream rsync receiver (this direction also
  shows the same first-file mismatch under the same conditions,
  indicating shared state or shared dispatch logic that the oc-side
  influences regardless of which end runs the parallel receiver).

The receiver writes wrong bytes for the **first dispatched file** in a
directory of 120 small files. Subsequent files (`file_2.txt`,
`file_3.txt`, ...) compare equal; the corruption is concentrated on
`file_1.txt`. This is a real corruption bug, not a permission, mtime,
or filesystem-attribute mismatch.

## Reproduction

Source dir with 120 small files, each holding `pt-payload-NNN\n` (about
15 bytes per file). Total bytes stay well under
`PARALLEL_RECEIVE_BYTES_THRESHOLD = 64 MiB`, so the only threshold that
trips is the file-count threshold.

```sh
mkdir -p source/parallel_threshold
for i in $(seq 1 120); do
  printf 'pt-payload-%03d\n' "$i" > "source/parallel_threshold/file_${i}.txt"
done

# Default-features build (PIP-5 flipped `parallel-receive-delta` on).
oc-rsync -av source/ dest/

# Or via daemon, mirroring the interop matrix:
oc-rsync -av source/ rsync://localhost/dest/
```

`PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD = 100` is defined in
`crates/transfer/src/receiver/mod.rs`. Any tree above 100 files trips
the parallel-receive-delta dispatch path, exposing the corruption.

The PIP-4 master CI run that first reproduced the failure on commit
`bc1476c` (the PIP-4 merge) is at
<https://github.com/oferchen/rsync/actions/runs/26279354408>.

## Suspected location

The dispatch entry points that feed the first file of a parallel batch:

- `crates/engine/src/concurrent_delta/parallel_apply.rs` -
  `ParallelDeltaApplier` (per-file slot registration, per-chunk verify
  fan-out, write commit).
- `crates/transfer/src/delta_pipeline/parallel.rs` - parallel
  delta-pipeline glue feeding the applier.

Hypotheses, ranked by plausibility given that the corruption is
deterministic on `file_1.txt` and reproduces in both directions:

1. **First-dispatch slot reuse on the receive side.** A stale slot or
   stale `BasisFile` handle from registrar bootstrap may be reused
   for the first applier dispatch, so the first file's COPY tokens
   resolve against the wrong basis offsets.
2. **Wrong slot lookup keyed by index 0 / first FileNdx.** The slot
   map (`Mutex<HashMap<FileNdx, _>>` documented in BR-3j.a-f) may
   collide on the sentinel/initial entry for the first file in the
   batch.
3. **Sender-side shared state (less likely but consistent with both
   directions failing).** A statically-initialised buffer or
   chunk-builder state on the sender side may bleed into the first
   parallel batch when the threshold trips, irrespective of which
   peer applies the delta.

## Immediate mitigation (shipped in this PR)

1. Remove `parallel-receive-delta` from the `default = [...]` feature
   list in the workspace `Cargo.toml`, `crates/transfer/Cargo.toml`,
   and `crates/engine/Cargo.toml`. The feature stays defined and is
   opt-in via `--features parallel-receive-delta`.
2. Remove the `parallel-threshold-trip` matrix entry and its setup,
   cleanup, and verify branches from `tools/ci/run_interop.sh` until
   the receiver fix lands.
3. Flag the affected claims in `README.md`, `CHANGELOG.md`, and the
   related design docs (`parallel-receive-delta-default-on.md`,
   `pip-4-closure-2026-05-21.md`, `br-6-sign-off-check-in-2026-05-21.md`)
   so they no longer read as if default builds exercise the parallel
   receive-delta path.

The `ParallelDeltaApplier`, `dispatch_receiver_strategy()`,
`enable_parallel_receive_delta()`, and the `OC_RSYNC_FORCE_PARALLEL`
debug env knob from PIP-4 are intentionally **not** removed. They
remain reachable when the feature is opted into so the PIP-7
investigation can iterate against them.

## Required investigation

1. **Capture wire bytes on the `up:` direction.** Run `tcpdump` (or
   `strace -e write` on the upstream sender) while the
   `parallel-threshold-trip` scenario runs against an oc-rsync
   receiver built with `--features parallel-receive-delta`. Compare
   the bytes delivered to the receiver against the expected
   `pt-payload-001\n` payload for `file_1.txt`. If the wire bytes are
   correct but the on-disk bytes are wrong, the bug is in the
   receive-side parallel writer (most likely). If the wire bytes are
   already wrong, the bug is in shared sender-side state that the
   threshold trip exposes.
2. **Slot-map instrumentation.** Add a debug log of `(FileNdx, slot
   identity, BasisFile range)` at every registration and lookup in
   `ParallelDeltaApplier`. Confirm whether `file_1.txt` ever sees a
   slot that was registered for a different file.
3. **Serialise the first batch.** As a diagnostic (not a fix), force
   the first parallel batch through the sequential path and confirm
   that this alone makes the scenario pass. That isolates the bug to
   the first-dispatch path versus a generic parallel-apply issue.

## Acceptance

The PIP-7 fix is complete when:

- The receiver no longer corrupts `file_1.txt` (or any other file) when
  the file-count threshold is crossed, validated by re-introducing the
  `parallel-threshold-trip` interop scenario in
  `tools/ci/run_interop.sh` and confirming green CI for both `up:` and
  `oc:` directions.
- `parallel-receive-delta` is re-added to the `default = [...]`
  feature lists in the workspace `Cargo.toml`,
  `crates/transfer/Cargo.toml`, and `crates/engine/Cargo.toml`.
- The deferral notes added in this PR to `README.md`, `CHANGELOG.md`,
  `docs/design/parallel-receive-delta-default-on.md`,
  `docs/design/pip-4-closure-2026-05-21.md`, and
  `docs/design/br-6-sign-off-check-in-2026-05-21.md` are removed (or
  superseded by a re-promotion note that cites this document).

## References

- `docs/design/parallel-receive-delta-application.md` - umbrella design.
- `docs/design/parallel-receive-delta-default-on.md` - default-on
  promotion rationale (now historical until PIP-7 lands).
- `docs/design/pip-4-closure-2026-05-21.md` - PIP-4 closure note (now
  deferred pending this fix).
- `crates/transfer/src/receiver/mod.rs` -
  `PARALLEL_RECEIVE_FILE_COUNT_THRESHOLD` and
  `PARALLEL_RECEIVE_BYTES_THRESHOLD` constants (kept in place; only
  the interop scenario that trips them is removed).
- `crates/engine/src/concurrent_delta/parallel_apply.rs` - applier
  implementation under investigation.
- `crates/transfer/src/delta_pipeline/parallel.rs` - parallel pipeline
  glue under investigation.

## Investigation results (2026-05-22)

### Container repro confirmed: NO

Reproduced inside the `rsync-profile` podman container (Debian
`rust:latest`, bind-mounted host repo) against the worktree build:

```
podman exec rsync-profile bash -c 'cd /workspace/.claude/worktrees/<wt> && \
  cargo build --release --features parallel-receive-delta'
```

Multiple repro shapes attempted, all yielded byte-identical destinations:

1. `up:` direction - upstream `rsync 3.4.x` client pushing into an
   `oc-rsync --daemon` receiver, 120 files of `pt-payload-NNN\n`, fresh
   empty destination (initial copy / no delta).
2. `up:` direction with the comprehensive fixtures from
   `tools/ci/run_interop.sh::setup_comprehensive_src` (~130 files
   including the standard `hello.txt`/`binary.dat`/`large.dat` mix)
   plus the 120-file `parallel_threshold/` tree, fresh empty dest.
3. `up:` direction with a pre-populated destination tree (delta apply
   path), backdated `mtime`s so quick-check forces a delta.
4. `oc:` direction - `oc-rsync` client pulling from an upstream
   `rsync --daemon` source, comprehensive fixtures + 120-file tree.
5. Stress loop: 10 sequential runs of the `up:` daemon scenario; all
   120 files matched on every run (`Total fails: 0 / 10`).
6. `OC_RSYNC_FORCE_PARALLEL=1` set on the receiver process (both
   daemon mode and `--rsync-path` client mode) to force the parallel
   strategy below the 100-file threshold; all 5 small-file deltas
   compared byte-identical.

`file_1.txt` was always:

```
00000000: 7074 2d70 6179 6c6f 6164 2d30 3031 0a    pt-payload-001.
md5: 0ade962f75abd69faa1c87f2359b9477 (src == dest)
size: 15 bytes (src == dest)
```

The CI run that originally surfaced the failure
(<https://github.com/oferchen/rsync/actions/runs/26279354408>) shows
the bug as deterministic across every protocol forced-tier
(28/29/30/native) and across both `up:` and `oc:` directions, so the
gap between the container and CI is environmental (kernel io_uring
support, jobserver concurrency, build profile, or libc / filesystem)
rather than a flaky timing window. The container runs `cargo build
--release`; CI runs `cargo build --profile dist` (LTO, panic=abort,
opt-level=z) - the dist profile narrows scheduling and may expose a
release-only race.

### Force-parallel small-payload repro: N/A

`OC_RSYNC_FORCE_PARALLEL=1` did not reproduce in the container even
on a 5-file delta, but the static read below shows why that result
is uninformative: the env knob swaps the `delta_pipeline` field, but
the swapped pipeline is never consumed in the production receiver
loop, so flipping it on or off has no observable effect on the
written bytes.

### Suspected location

`crates/transfer/src/receiver/mod.rs:418-422` -
`ReceiverContext::enable_parallel_receive_delta` and its single
`set_delta_pipeline(Box::new(ParallelDeltaPipeline::new(...)))` write
site. Specifically, the side effect of constructing
`ParallelDeltaPipeline::new` - which spawns a `DeltaConsumer`
background thread via `DeltaConsumer::spawn` in
`crates/engine/src/concurrent_delta/consumer/mod.rs:186` - is the
only observable production behaviour the feature gate enables. The
pipeline field itself is dead state in the receive hot path.

### Why this is the location

The design doc's two hypothesised modules
(`crates/engine/src/concurrent_delta/parallel_apply.rs` and
`crates/transfer/src/delta_pipeline/parallel.rs`) carry the
parallel-apply state machine, but a `grep` for every entry point on
both surfaces (`ParallelDeltaApplier::register_file`,
`::apply_one_chunk`, `::apply_batch_parallel`, `::finish_file`,
`ReceiverDeltaPipeline::submit_work`, `::poll_result`) inside
`crates/transfer/src/receiver/`, `crates/transfer/src/pipeline/`, and
`crates/transfer/src/transfer_ops/` returns **only test/bench
references**. The sole call site that touches the field in
production is `ReceiverContext::set_delta_pipeline` at
`crates/transfer/src/receiver/mod.rs:400`, which writes the field
without any reader.

This matches the standing note in `MEMORY.md` under
`project_parallel_interop_parity_gap.md` -
> "feature-gated scaffolding (default on), but production token_loop
> still uses sequential DeltaWork path; full upstream interop suite
> NOT validated through parallel path."

So the receiver's actual delta-apply still runs through
`SequentialDeltaPipeline` -> `apply_delta` -> the per-file
`disk_commit` thread, regardless of which pipeline object lives in
`ReceiverContext::delta_pipeline`. The CI corruption on `file_1.txt`
therefore cannot be the parallel applier writing the wrong bytes -
it must be a **side effect** of the parallel pipeline's construction
or its background `DeltaConsumer` thread interfering with the
sequential write path. Candidate side effects (ranked by
plausibility):

1. **`DeltaConsumer::spawn` resource contention.** The pipeline
   constructor spawns a `crossbeam`-backed work queue plus a
   background reorder thread (`consumer/mod.rs:186`); under the
   `dist` profile and CI's higher core count this thread may compete
   with the disk-commit thread for the rayon thread pool or for the
   first batched `write_all` against the SPSC `file_tx` channel in
   `crates/transfer/src/disk_commit/thread.rs`. First-file ordering
   in the file list (`file_1.txt` is the lexically first entry in
   `parallel_threshold/`) means the very first delta write races
   pipeline-bring-up.
2. **Buffer pool / rayon pool warm-up.** Constructing
   `ParallelDeltaPipeline::new(rayon::current_num_threads())` (line
   420) eagerly sizes a queue against the ambient rayon pool; the
   first call to `rayon::current_num_threads()` initialises rayon's
   global pool, which can recolor threads that were already mid-flight
   on the receiver setup. A first-file write that overlaps with that
   initialisation can land in a thread whose TLS buffer is still
   zero-initialised.
3. **Shared state inside `DeltaConsumer`.** The consumer constructor
   takes a reorder-buffer capacity argument; if the receiver later
   re-uses the same `ReorderBuffer` instance for the sequential
   commit path (it does not today, but cross-module sharing is easy
   to introduce by accident), the first file's delta would land in
   the parallel reorder buffer and never reach the destination
   writer.

`file_1.txt` being the lexically first child of `parallel_threshold/`
and the first file in the file list that exceeds the file-count
threshold is consistent with all three candidates: whatever the
side effect is, it fires once at pipeline construction and corrupts
the next write that arrives.

### Proposed fix sketch

- **First**, instrument or remove dead state. Add a `debug_log!`
  inside `enable_parallel_receive_delta` confirming the pipeline
  swap is observable (it currently is not - no production reader of
  the field), and either wire the parallel pipeline into the
  receiver hot loop (RJN-3 work) or delete the swap so the feature
  no longer spawns a background consumer that the receiver never
  drains. Either resolution removes the unexplained side effect that
  the CI failure depends on.
- **Add a CI-only repro guard.** Re-add `parallel-threshold-trip`
  to `tools/ci/run_interop.sh` behind an explicit env gate
  (`OC_RSYNC_PIP7_REPRO=1`) so the failure stays visible in CI runs
  that opt in without re-introducing the unconditional regression.
- **Bisect under the `dist` profile.** The container could not
  reproduce under `cargo build --release`; the next iteration must
  reproduce under `cargo build --profile dist` in the same
  rsync-profile container before attempting any code fix, otherwise
  the fix lacks an evidence loop.
- **Confirm the side effect.** Once the bisect identifies the
  responsible construction step (pipeline `new`, consumer `spawn`,
  reorder buffer alloc, or rayon `current_num_threads`), the fix is
  to drop that side effect entirely while the parallel pipeline is
  not wired to the receive loop, then re-introduce it together with
  the RJN-3 fan-out caller that actually consumes the pipeline.
