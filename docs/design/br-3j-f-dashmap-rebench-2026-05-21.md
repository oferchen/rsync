# BR-3j.f - DashMap re-bench of the BR-3i.f cores-vs-throughput sweep (2026-05-21)

Date: 2026-05-21
Scope: criterion re-bench scaffold; production number capture deferred to
offline run on multi-core Linux hardware.
Status: **NUMBERS DEFERRED**. Harness shipped; the actual cores-vs-throughput
curve is captured offline on production-class hardware and appended to this
doc once measured.
Tracker: BR-3j.f (#2508). Predecessor harness: BR-3i.f (PR #4653) at
`crates/engine/benches/parallel_verify_chunk.rs`.

## 1. Why a re-bench

BR-3j.c (PR #4634), BR-3j.d (PR #4635), and BR-3j.e (PR #4636) replaced the
applier's outer `Mutex<HashMap<FileNdx, Arc<Mutex<FileSlot>>>>` slot map at
`crates/engine/src/concurrent_delta/parallel_apply.rs` with a `DashMap`
shard layout. The selection audit at
`docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` predicted that the
DashMap migration would lift the outer-map contention out of the
register/lookup/finish hot path and unblock the per-file rayon fanout that
the rest of the applier is built around. The original BR-3i.f sweep in
`parallel_verify_chunk.rs` produced the pre-DashMap baseline curve that
sized the receiver's parallel-vs-sequential dispatch heuristic at PIP-3+5
(PR #4666).

BR-3j.f re-runs the same workload through the post-DashMap applier so the
curve can be captured after the outer-lock removal. Two outcomes are
acceptable:

1. **Improvement.** The new curve scales further before saturating, or the
   absolute throughput at every worker count rises. This is the audit's
   directional prediction and the case the PIP-3+5 heuristic was sized
   against.
2. **Flat / regression.** The new curve does not improve. That is also a
   useful conclusion: it tells us the outer map was not the binding
   constraint on the BR-3i.f workload shape and points the next
   investigation at the per-file `Mutex<FileSlot>` writer barrier
   (project memory: `project_apply_batch_write_serial.md`,
   `project_parallel_delta_apply_outer_mutex.md`).

Either outcome closes the line of inquiry: the BR-3j.a audit's
recommendation rests on the assumption that the outer map was contending.
BR-3j.f is the test of that assumption on real workloads, not on the
microbench shape the audit cited.

## 2. Methodology

### 2.1 Harness

A new criterion bench at
`crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` registered
under the `parallel-receive-delta` feature gate. The bench is a deliberate
sibling of the BR-3i.f harness rather than an in-place edit so the
criterion baseline saved under `target/criterion/parallel_verify_chunk/`
stays comparable to the BR-3j.f run saved under
`target/criterion/br_3j_f_dashmap_cores_vs_throughput/`. Criterion's
compare-to-baseline workflow then yields a direct before/after diff per
(workload, strategy, worker_count) cell without manual bookkeeping.

### 2.2 Sweep cells

Identical to BR-3i.f so the post-DashMap numbers line up cell-for-cell
with the pre-DashMap baseline:

| Axis | Values | Notes |
|---|---|---|
| Worker count | `{1, 2, 4, 8, available_parallelism()}` deduplicated | Pins both `ParallelDeltaApplier::with_strategy` concurrency and the ambient rayon pool. |
| Checksum strategy | `{MD4, MD5, XXH3}` | Mirrors BR-3i.b's plumbed set; will fail to compile if a future change drops one. |
| Workload A | 4 files x 256 chunks x 1 MiB = 1024 chunks / 1 GiB | Large chunks, few files. Models VM images / container layers. |
| Workload B | 256 files x 64 chunks x 16 KiB = 16384 chunks / 256 MiB | Small chunks, many files. Models source trees / build artefact dirs. |
| Workload C | 4096 files x 1 chunk x 4 KiB = 4096 chunks / 16 MiB | Many files, one chunk each. Register/finish churn. New for BR-3j.f. |

Workload C is the BR-3j.f-specific addition. It maximises the share of
wall time spent inside `register_file` and `finish_file` so the DashMap
shard layout's concurrency is the dominant signal. Under the pre-DashMap
shape this path serialised every register and finish behind one outer
mutex, so the BR-3j.f numbers on C should show worker scaling that the
baseline would not have produced. If they do not, the per-file
`SlotBarrier` Mutex is the next bottleneck.

### 2.3 Iteration shape

Each criterion sample rebuilds the applier from scratch via
`ParallelDeltaApplier::with_strategy(workers, strategy)`. This is
deliberate for BR-3j.f: the bench's claim of interest is exactly how the
DashMap behaves when populated under contention from zero. Reusing an
applier across samples would warm the DashMap shards and mask the
register-side scaling difference.

Sink writers are `CountingSink` (no allocation, no atomic ops) so the
bench isolates verify + dispatch + per-file mutex + outer-map cost from
allocator pressure or shared sink state. Disk I/O is out of scope and is
covered separately by `delta_transfer_benchmark` and `local_copy_bench`.

### 2.4 Reproducibility

All chunk payloads come from a seeded `SmallRng`; the seed combines a
fixed root (`0xB33D_BEEF_5EE0_C0DE`, distinct from BR-3i.f's root so the
two corpora cannot accidentally alias) with `(workload_tag, file_index,
chunk_index)` triples. Per-chunk `expected_strong` digests are computed
once per (workload, strategy) and reused across samples so the timed
loop never recomputes them.

## 3. Number capture procedure

### 3.1 Target hardware

The bench is meant for production-class multi-core Linux hosts. A CI
runner is not adequate: GitHub-hosted runners typically expose 2-4 vCPUs
and noisy neighbours skew criterion's confidence intervals beyond the
signal this bench tries to surface.

Two known-good targets in this project:

1. **`oc-rsync-bench` container** (`localhost/oc-rsync-bench:latest`,
   Arch Linux). The benchmark image the release pipeline uses. Run on a
   bare-metal Linux host (16+ physical cores recommended) via
   `podman run --rm -it --cpus=$(nproc) localhost/oc-rsync-bench:latest`.
   The workspace bind-mount is intentionally **not** used here because
   criterion baseline data lands under `target/criterion/` and we want it
   on a dedicated benchmark volume, not the host source tree.
2. **`rsync-profile` container** (`rsync-profile`, Debian rust:latest).
   The persistent debug container. Exec into it with
   `podman exec -it rsync-profile bash`; criterion output lands under
   `/workspace/target/criterion/` (workspace bind-mount).

The container choice is up to the operator. Record which one was used in
the appended-numbers section so the cell-to-cell deltas can be replayed
later.

### 3.2 Commands

From the workspace root inside the chosen target:

```sh
# Capture the post-DashMap curve as a named baseline.
cargo bench -p engine --features parallel-receive-delta \
    --bench br_3j_f_dashmap_cores_vs_throughput -- --save-baseline br-3j-f-post

# Optional: re-run BR-3i.f against the same applier shape to refresh the
# pre-bench baseline if the pre-DashMap data is no longer in
# target/criterion/.
git checkout <pre-br-3j.c-rev>
cargo bench -p engine --features parallel-receive-delta \
    --bench parallel_verify_chunk -- --save-baseline br-3i-f-pre
git checkout -

# Diff: criterion writes both baselines under
# target/criterion/<group>/<id>/{br-3j-f-post,br-3i-f-pre}/. Use
# `cargo bench ... -- --baseline br-3i-f-pre` to produce the comparison
# report, or read the violin plots under target/criterion/report/.
```

### 3.3 Where the numbers land

The actual percentile table (median / lower / upper bound per cell) is
appended to section 6 of this doc as a markdown table. Raw criterion JSON
under `target/criterion/br_3j_f_dashmap_cores_vs_throughput/` is the source
of truth; the table here is a human-readable summary. Do not commit the
raw criterion output - it is reproducible from the harness.

## 4. Comparison baseline

The BR-3i.f baseline ships in `parallel_verify_chunk.rs` (PR #4653) and
was not measured to a quotable number set in any committed doc. The
pre-DashMap absolute throughput on a given host is therefore captured
either:

- by checking out the parent of PR #4634 (BR-3j.c) and running the
  BR-3i.f command above with `--save-baseline br-3i-f-pre`, or
- by reading any previously-saved baseline in
  `target/criterion/parallel_verify_chunk/` if one was preserved.

The BR-3j.f re-run then uses `--baseline br-3i-f-pre` to produce the diff
table. If neither pre-existing baseline is available, BR-3j.f stands on
its own as the post-DashMap reference point and the directional
improvement claim becomes "scales further at high worker count" rather
than "X% faster than baseline".

## 5. Expected directional improvement

The BR-3j.a audit's predictions, lifted from
`docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` sections 1, 2, and
3, applied to the BR-3j.f workload shape:

1. **Workload A (large chunks, few files).** The audit predicts minimal
   change. With 4 files and 256 chunks each, the outer-map hit rate is
   dominated by 4 register / 4 finish calls per iteration plus 4 unique
   `slot_for` Arc clones (after the per-file slot is warmed). The
   pre-DashMap baseline already amortised the outer-mutex cost across
   1024 chunks per iteration. **Expected: < 5% delta either way; this
   workload is the noise floor for the re-bench, not the headline.**
2. **Workload B (small chunks, many files).** Here the audit predicts the
   largest signal. With 256 files and 16384 chunks, the pre-DashMap
   shape forced every chunk's `slot_for` Arc clone through the outer
   mutex. With DashMap sharding (default `4 x num_cpus.next_power_of_two()`
   shards), 16-way concurrent `slot_for` calls hit independent shards
   most of the time. **Expected: visible scaling improvement at workers
   >= 4; the pre-DashMap curve should saturate earlier.**
3. **Workload C (register/finish churn).** Brand new for BR-3j.f. The
   pre-DashMap path serialised every register/finish behind one outer
   mutex. DashMap's `entry` API on a sharded layout lets N workers
   register against N different shards in parallel. **Expected: the
   most dramatic scaling difference of the three workloads. If C does
   not scale, the per-file `SlotBarrier::Mutex` is the next constraint
   and the BR-3j project memory page
   `project_parallel_delta_apply_outer_mutex.md` keeps the
   "outer mutex" descriptor as an artefact of pre-DashMap code; the
   real bottleneck has moved one level in.**

These predictions are deliberately weak: the audit and this doc both
recognise that the pre-DashMap baseline was already amortised by the
per-file slot mutex for any read-heavy access pattern. The BR-3j.a audit
selected DashMap on ergonomics and code-surface arguments at least as
much as on raw contention. BR-3j.f's job is to quantify, not to vindicate.

## 6. Captured numbers

**Status:** numbers deferred. Awaiting offline run on production-class
hardware per section 3.

Once captured, the operator appends a table of the form:

```
Host: <hostname>, kernel <uname -r>, <cpu model>, <core count> cores
Container: oc-rsync-bench:<digest> (or rsync-profile)
Date: <YYYY-MM-DD>
Criterion baselines: br-3i-f-pre, br-3j-f-post

| Workload | Strategy | Workers | br-3i-f-pre (MiB/s) | br-3j-f-post (MiB/s) | Delta |
|---|---|---|---|---|---|
| large_chunks_few_files | md4 | 1 | ... | ... | ... |
| large_chunks_few_files | md4 | 2 | ... | ... | ... |
| ...                    | ... | ... | ... | ... | ... |
| small_chunks_many_files | xxh3 | host | ... | ... | ... |

| Workload | Strategy | Workers | Files/sec (br-3j-f-post) | Notes |
|---|---|---|---|---|
| register_finish_churn | md5 | 1 | ... | baseline cell |
| register_finish_churn | md5 | 2 | ... | ... |
| ...                   | ... | ... | ... | ... |
```

The table is the deliverable. The criterion JSON is the source of truth.

## 7. Decision gate this re-bench feeds

The re-bench numbers gate three downstream decisions, each tracked
elsewhere:

1. **ABW-2/3/4 (apply-batch verify/write pipelining).**
   `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`
   section 3 holds the audit-derived condition `0.5 <= verify_wall /
   write_wall <= 2.0` on any production-relevant cell. BR-3j.f's MiB/s
   numbers feed that ratio when paired with the `delta_transfer_benchmark`
   wall-clock breakdown.
2. **RJN-3 / RJN-4 (fanout scheduler shape).**
   `docs/design/rjn-3-4-fanout-deferred-2026-05-21.md` defers the
   scheduler-shape design until BR-3j.f shows whether the cores-vs-
   throughput curve still has headroom at high worker count. A saturated
   curve closes RJN-3; a still-scaling curve keeps RJN-3 open as a
   targeted optimisation.
3. **Project memory `project_parallel_delta_apply_outer_mutex.md`.** If
   the post-DashMap workload-C curve still flattens at low worker count,
   the project page is amended with the new bottleneck name (per-file
   `SlotBarrier::Mutex`); if it scales, the page is closed.

## 8. References

- `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs` - the
  re-bench harness this doc describes.
- `crates/engine/benches/parallel_verify_chunk.rs` - the BR-3i.f baseline
  harness, kept untouched so the criterion baselines stay comparable.
- `crates/engine/src/concurrent_delta/parallel_apply.rs` - the
  implementation under measurement; `files: DashMap<FileNdx,
  Arc<SlotBarrier>>` field landed in BR-3j.c (PR #4634).
- `docs/audits/br-3j-a-dashmap-vs-sharded-2026-05-20.md` - selection
  audit; section 5 spells out the contention model the re-bench validates.
- `docs/audits/br-3i-a-verify-chunk-audit-2026-05-20.md` - the
  prerequisite verify-chunk plumbing audit; BR-3i.b/c shipped the real
  per-chunk verify cost the cores-vs-throughput sweep measures.
- `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`,
  `docs/design/rjn-3-4-fanout-deferred-2026-05-21.md` - downstream
  designs blocked on this re-bench's numbers.
- PR #4634 - BR-3j.c DashMap migration. Field swap.
- PR #4635 - BR-3j.d DashMap wire-up. Hot-path edits.
- PR #4636 - BR-3j.e DashMap removal of dead poisoned-error rungs.
- PR #4653 - BR-3i.f baseline harness landed.
- PR #4666 - PIP-3+5 receiver dispatch heuristic; sized against the
  pre-DashMap curve, so BR-3j.f's numbers are also the input to any
  future revision of the `file_count > 100 || total_size > 64 MiB`
  threshold.
- BR-3j.f tracker - #2508. Stays open with the
  "deferred pending offline number capture" label until section 6 above
  is populated.
