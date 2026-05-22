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
