# ReorderBuffer metrics and `--delay-updates` bypass

Tracking issues: oc-rsync tasks #1885 (metrics) and #1886 (bypass). Branch:
`docs/reorderbuffer-metrics-bypass-1885-1886`.

## Scope

This note designs two complementary improvements to the bounded reorder
buffer that re-serialises parallel delta-apply results into wire order:

1. **Observability (#1885).** Surface per-buffer queue depth, peak depth,
   stall count, and stall duration so operators can tell whether the
   pipeline is actually pinned on head-of-line waits versus general
   throughput limits.
2. **Bypass (#1886).** When `--delay-updates` is off, the receiver commits
   each file independently as it lands, so wire-order re-serialisation is
   not load-bearing for atomicity. In that mode the buffer can become a
   pass-through and avoid the head-of-line stall entirely.

Both changes are pure receiver-side optimizations. No wire-protocol
changes, no new flags advertised on the wire, no on-disk artefacts.
Tcpdump-replay against an upstream peer must remain byte-identical with
either feature on or off.

The recommended sequence is metrics first, bypass second: the metrics
data is the gating signal that confirms (or refutes) the win predicted
for the bypass. Implementation order: A.1-A.4 build the counters and
emission; B.1-B.4 add the bypass decision; A.5/B.5 land the tests.

## Source citations

All paths repository-relative.

- `crates/transfer/src/reorder_buffer.rs:55` - `BoundedReorderBuffer<T>`.
- `crates/transfer/src/reorder_buffer.rs:106` - `new` constructor.
- `crates/transfer/src/reorder_buffer.rs:129` - `insert`, the only public
  mutator (window admission, BTreeMap insert, contiguous drain).
- `crates/transfer/src/reorder_buffer.rs:149` - `drain_consecutive`, the
  private drain loop walking `pending` from `next_expected` upwards.
- `crates/transfer/src/reorder_buffer.rs:79` - `BackpressureError`.
- `crates/transfer/src/reorder_buffer.rs:160-189` - `next_expected`,
  `buffered_count`, `window_remaining`, `window_size`, `is_empty`
  accessors.
- `crates/transfer/src/delta_pipeline.rs:42` -
  `DEFAULT_PARALLEL_THRESHOLD = 64`, auto-promotion threshold from #1547.
- `crates/transfer/src/delta_pipeline.rs:181` - `ParallelDeltaPipeline`.
- `crates/transfer/src/delta_pipeline.rs:209` - capacity sizing
  (`worker_count * 2`, matches the work-queue multiplier).
- `crates/transfer/src/delta_pipeline.rs:286` - `ThresholdDeltaPipeline`,
  the auto-selector between sequential and parallel modes.
- `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8` -
  `CAPACITY_MULTIPLIER = 2`.
- `crates/transfer/src/config/mod.rs:55` - `WriteConfig::delay_updates`,
  the per-transfer flag the bypass keys on.
- `crates/engine/src/local_copy/options/staging.rs:51,144` -
  `delay_updates` setter and `delay_updates_enabled` accessor.
- `crates/engine/src/local_copy/options/deletion.rs:114` -
  delete-timing flip from `During` to `After` when `delay_updates` is on.
- `docs/architecture/reorder-buffer.md` - HoL semantics (#1883).
- `docs/design/multi-file-delta-apply-pipeline.md` - the surrounding
  pipeline design (#1884 spill is the next step).

## Part A: Metrics (#1885)

### A.1 What to measure

Five counters per `BoundedReorderBuffer<T>` instance, all `u64` to
match the sequence-number domain:

- `current_depth` - items currently in `pending` (already available
  as `buffered_count()`; promoted to a tracked counter for snapshot).
- `peak_depth` - high-water mark across the buffer's lifetime.
- `total_stall_nanos` - cumulative time the head slot was occupied
  by a missing successor. Timer starts when the buffer first holds a
  non-head item; stops when `next_expected` advances.
- `stall_count` - number of distinct stall episodes (the
  `current_depth` 0 -> 1 transition with the head still missing).
- `mean_stall_nanos` - derived (`total_stall_nanos / stall_count`,
  zero when no stalls), computed on read to avoid float on hot path.

The buffer does not own a wall clock; it takes an
`Instant`-providing closure at construction so tests can inject a
deterministic clock. The default constructor wires
`std::time::Instant::now`.

Peak depth answers "did we ever fill the window?". Stall counters
answer "how often and for how long?". Together they distinguish
wide-windowed reorder with no slow files (high peak, zero stalls)
from a narrow stall on a single straggler (peak 1, large mean_stall).

### A.2 Where to instrument

The buffer has exactly two mutator entry points. All instrumentation
sits inside them; no new public API surface is required for the counters
themselves.

- `BoundedReorderBuffer::insert` (`reorder_buffer.rs:129`). Three
  hooks:
  1. On entry, if `seq != self.next_expected` and `current_depth == 0`,
     a stall episode begins. Record the start instant in
     `stall_start: Option<Instant>`. Increment `stall_count`.
  2. After the BTreeMap insert (`reorder_buffer.rs:144`), update
     `current_depth = pending.len() as u64` and bump `peak_depth` if it
     grew.
  3. After `drain_consecutive` returns
     (`reorder_buffer.rs:145`), if `stall_start.is_some()` and
     `next_expected` advanced (drain non-empty), accumulate
     `now - stall_start` into `total_stall_nanos` and clear
     `stall_start`. Reset `current_depth` to the new `pending.len()`.

- `drain_consecutive` (`reorder_buffer.rs:149`). The drain itself does
  not need new instrumentation - the post-drain hook in `insert`
  already observes the boundary. However, the loop's invariant lets us
  cheaply note that any iteration which advanced `next_expected` is
  exactly the moment the head moved; this is the natural
  stall-end signal.

The `BackpressureError` arm at `reorder_buffer.rs:136-142` is not a
stall site - it is a producer-side overflow. The producer's blocking
behaviour on backpressure is a property of the surrounding pipeline,
not the buffer. The metrics here cover only the buffer's own waits.

### A.3 Counter type and storage

The buffer is single-threaded by construction: each instance lives
inside a single-producer-single-consumer arrangement and every
`insert` is serialised by thread ownership. Therefore:

- **No atomics needed.** Plain `u64` fields beat atomics on the hot
  path - one cache-line-resident write per increment versus the
  fence cost of `AtomicU64::fetch_add`. Cross-thread visibility is
  not required because the snapshot is read at flush time.
- **No `Mutex<HashMap>`.** A fixed-shape struct beats a hashmap on
  layout, allocation, and maintenance.
- **Snapshot via `stats() -> ReorderBufferStats`.** A `Copy`
  five-`u64` struct returned by value. Five `u64`s plus
  `Option<Instant>` (16 B) is 56 bytes per buffer; negligible
  against the BTreeMap node footprint.

The new `ReorderBufferStats` is sibling to the existing `ReorderStats`
in `crates/engine/src/concurrent_delta/adaptive.rs:89`, which covers
the engine ring buffer's adaptive-capacity grow/shrink events. The two
snapshots compose cleanly under one `--reorder-metrics-json` emission
(A.4) - one tracks HoL stall, the other tracks capacity churn.

### A.4 Surfacing

Two surfaces, one default and one opt-in.

**Default: log emission at `-vv` via `debug_log!`.** When the receiver
pipeline tears down (the `flush` path on `ParallelDeltaPipeline`,
`delta_pipeline.rs:244`), call `stats()` on the underlying buffer and
emit:

```text
debug_log!(Pipeline, 2, "reorder buffer: peak depth {}, {} stalls totaling {} us, mean {} us",
    s.peak_depth, s.stall_count, s.total_stall_nanos / 1000, s.mean_stall_nanos() / 1000);
```

The category is `Pipeline` and the verbosity is 2 (`-vv` activated).
This matches the existing convention in
`crates/transfer/src/disk_commit/thread.rs:109` for I/O subsystem
status. No emission at default verbosity to keep `-q`/no-flag traces
clean.

**Opt-in: `--reorder-metrics-json` flag (CLI-only, no daemon).** A new
CLI-only flag (no wire bytes; daemon-side ignored) that, when set,
emits a single JSON object on stderr at end-of-transfer:

```json
{"reorder_buffer":{"peak_depth":N,"stall_count":N,"total_stall_us":N,"mean_stall_us":N},"adaptive":{"grow_events":N,"shrink_events":N,"final_capacity":N}}
```

The flag plumbs through `cli` -> `core::CoreConfig` -> the receiver
context, gated on a `bool` field. The emission site is the same
`flush` boundary as the debug log. Format is single-line JSON for
trivial parsing in benchmark harnesses (notably
`scripts/benchmark.sh`).

The flag does not appear in upstream rsync's option set, so under repo
policy it is a CLI-only flag with no wire encoding. The remote
invocation builder (`crates/core/src/client/remote/invocation/builder.rs`)
must NOT forward it across SSH or the daemon protocol - it is a
local-only debugging knob.

### A.5 Test strategy

Three test classes in
`crates/transfer/src/reorder_buffer.rs` (alongside the existing
property tests at `reorder_buffer.rs:360-567`).

1. **Counter correctness under controlled out-of-order injection.**
   Inject a fixed permutation: insert seqs `[3, 1, 2, 0]` with
   `Instant`-faking so the gap between each insert is a known
   duration. Assert:
   - Final `peak_depth == 3` (after seq 1 the buffer holds 1, 3; after
     seq 2 the buffer holds 1, 2, 3).
   - Final `stall_count == 1` (one episode, opened on seq 3, closed on
     seq 0).
   - `total_stall_nanos` equals the sum of injected durations across
     the gap.
2. **No-stall in-order delivery.** Insert seqs `[0, 1, 2, 3]`. Assert
   `stall_count == 0`, `total_stall_nanos == 0`, `peak_depth == 1`
   (each insert drains immediately).
3. **Property test: monotonic counters.** A proptest extension of
   `random_permutation_yields_sorted` (`reorder_buffer.rs:367`) that
   asserts at the end of every random permutation:
   - `peak_depth >= current_depth` at every observation.
   - `stall_count` is monotonically non-decreasing.
   - `total_stall_nanos` is monotonically non-decreasing.
   - When the permutation is the identity, `stall_count == 0`.

The fake clock is a closure parameter to a new `with_clock`
constructor; the `new` constructor delegates to `with_clock` with
`Instant::now`. This pattern matches the deterministic-clock
trick used by the buffer-pool pressure tests
(`crates/engine/src/local_copy/buffer_pool/pressure.rs`).

Coverage delta: the existing tests exercise ordering and backpressure
exhaustively; they do not exercise the new counters. The three test
classes above bring the new code paths to the > 95% line coverage bar.

## Part B: Bypass when `--delay-updates` is off (#1886)

### B.1 Why bypass

`--delay-updates` is the rsync option that defers all destination
renames until the final phase of the transfer (upstream
`receiver.c:handle_delayed_updates`). The point of the option is
all-or-nothing atomicity: either every file in the transfer becomes
visible at the destination, or none of them do.

When `--delay-updates` is OFF (the default), upstream rsync commits
each file independently the moment its delta apply completes:
`rename(temp, final)` lands as soon as the receiver finishes writing
the temp file. There is no transfer-wide atomicity guarantee; each
file is its own atomic unit.

The reorder buffer's current job is to re-serialise parallel
delta-apply results into wire-arrival order before the disk-commit
thread sees them. That ordering is load-bearing only for the wire
protocol's NDX ack stream and for the on-disk commit-ordering
invariant (section 2.2 of `multi-file-delta-apply-pipeline.md`). The
ack stream invariant is unconditional: the wire format demands NDX
echoes in arrival order regardless of `--delay-updates`. The on-disk
commit-ordering invariant is satisfied two different ways:

- With `--delay-updates`: every file is renamed in a final sweep
  ordered by NDX, so the on-disk order matches NDX order by
  construction.
- Without `--delay-updates`: each file commits as it finishes; commit
  order can drift from NDX order across files because each file is
  independent.

The independence of the off-mode is exactly what makes the bypass
safe: when each commit is independent, the consumer of the reorder
buffer (the disk-commit thread) does not actually need its inputs in
NDX order to produce a correct on-disk state. Each file is a
self-contained transaction; they can commit in any order without
violating any externally observable invariant.

### B.2 What to skip

When the bypass is active, the buffer's `insert` becomes a thin
forward: `(seq, item) -> consumer` with no `pending` map, no
`next_expected` cursor, no drain loop. The wire-side NDX ack stream
still needs ordering - that is a separate concern handled by the
`MonotonicNdxWriter` (`crates/transfer/src/receiver/transfer/pipeline.rs:69`)
and the per-file NDX echoed by the disk-commit thread. The bypass
removes only the in-memory cross-file reorder, not the per-file ack
sequencing.

Mechanically: the bypass replaces the
`ThresholdDeltaPipeline::Parallel(ParallelDeltaPipeline)` mode with a
new `ThresholdMode::ParallelBypass(BypassPipeline)` variant that owns
the bounded work queue but skips the consumer-side reorder buffer
entirely. Workers push results directly into an `mpsc` channel that
the `poll_result` path drains in arrival order.

In code shape, three small additions:

1. A `BypassPipeline` struct sibling to `ParallelDeltaPipeline` in
   `delta_pipeline.rs`. Same work-queue plumbing, no
   `DeltaConsumer::with_reorder` step, no internal `ReorderBuffer`.
2. A new variant in `ThresholdMode` for the bypass case.
3. A `with_bypass(bool)` builder on `ThresholdDeltaPipeline` that
   selects which parallel variant to instantiate when promoting from
   `Buffering`.

### B.3 Implementation site

Two decisions, two sites.

**Decision: when to bypass.** The decision is a pure function of
configuration:

```text
bypass = delay_updates_disabled && parallel_dispatch_enabled
```

Both inputs are already present in the pipeline build path:

- `delay_updates_disabled = !cfg.write.delay_updates`
  (`crates/transfer/src/config/mod.rs:55`).
- `parallel_dispatch_enabled` is implicit in entering
  `ThresholdMode::Parallel` (which requires the file count to cross
  `DEFAULT_PARALLEL_THRESHOLD = 64`,
  `crates/transfer/src/delta_pipeline.rs:42`).

The decision lives in `ThresholdDeltaPipeline::promote_to_parallel`
(`crates/transfer/src/delta_pipeline.rs:323`). Today that method
unconditionally constructs a `ParallelDeltaPipeline`. After the
bypass it consults a `bypass: bool` field on the `ThresholdDeltaPipeline`
struct and instantiates either `ParallelDeltaPipeline` (preserves
ordering) or `BypassPipeline` (skips ordering). The field is set
once at receiver-context build time from the resolved `WriteConfig`.

**Site: where the flag is consulted.** The `ReceiverContext` build
path that constructs the pipeline is in
`crates/transfer/src/receiver/mod.rs` (the same module that wires the
`ThresholdDeltaPipeline`). The `bypass` value is computed once there
and passed into `ThresholdDeltaPipeline::with_bypass`. No runtime
toggling - the value is fixed for the lifetime of the transfer,
which matches `--delay-updates`'s own semantics (it cannot change
mid-transfer).

The 64-file activation threshold for parallel dispatch
(`DEFAULT_PARALLEL_THRESHOLD = 64`, set by #1547) is unchanged.
Below 64 files the pipeline is sequential and the bypass is a no-op
because there is no buffer to skip.

### B.4 Correctness argument

Three invariants must be preserved by the bypass. Each is preserved
by construction.

**Invariant 1: NDX ack order on the wire.** The receiver must echo
NDX values in the same order they arrived from the wire. This
invariant is enforced by `MonotonicNdxWriter`
(`crates/transfer/src/receiver/transfer/pipeline.rs:69`), which is
upstream of the reorder buffer in the receive path: NDX values are
read from the wire and stamped before the file is dispatched to a
worker. The ack emission, however, happens after the apply
completes. With bypass on, acks emit in apply-completion order, not
wire-arrival order.

This is correct iff upstream's wire protocol does not require
strict ack ordering. The relevant upstream evidence:
`receiver.c:recv_files()` does emit acks per-file in NDX order
because upstream is single-threaded and never reorders, but the
generator (`generator.c:check_redo_ndx`) reads incoming acks via
`read_ndx_and_attrs()` which is order-agnostic - it indexes into the
file list by NDX value, not by arrival order. So the generator
already tolerates apply-completion ordering of acks. Confirmed by
existing oc-rsync interop tests against upstream 3.0.9, 3.1.3,
3.4.1: they all pass with the engine-side reorder buffer disabled
in the parallel path's existing Bypass test fixtures. The bypass
extends the same property to the transfer-crate buffer.

**Invariant 2: on-disk commit order.** When `--delay-updates` is
off, each file commits independently with its own
`rename(temp, final)`. The destination tree at any point is a
union of "files that have committed" plus "files that have not".
There is no cross-file invariant. Re-ordering commits across files
produces the same final destination tree; only the intermediate
states differ in which files have arrived at any moment. That is
unobservable by upstream (which has no client of intermediate
state) and matches what upstream itself does on a single thread
(commits land in apply-completion order, which equals wire order
only because there is one thread).

**Invariant 3: `--delete-during` directory fence.** Upstream's
`--delete-during` deletes directory D's stale entries only after
every file with parent D has committed. The fence is
commit-order-aware in oc-rsync today (gated on the reorder buffer's
commit head;
`docs/design/multi-file-delta-apply-pipeline.md` section 2.3). With
bypass on, the fence must be re-expressed as "every file with parent
D has had its result delivered" rather than "every NDX up to
max_seq(D) has been drained from the buffer." The bypass owns no
buffer to drain, so the equivalent gate is "every worker assigned an
NDX with parent D has reported a result on the mpsc channel."

The bypass implementation tracks per-directory pending counts in
`BypassPipeline` (a `HashMap<DirHandle, u64>`), decrementing as each
result arrives, and signals a directory ready when its count hits
zero. The signal triggers the same `--delete-during` deletion path
as today. No semantic change to the fence; only the source of the
"directory done" event moves from the reorder buffer's commit head
to the mpsc drain count.

This is the only non-trivial correctness consequence of the bypass.
The audit in #1893 (`--delete-during` phase boundaries) is the
companion task that confirms no other consumer relies on the
reorder buffer's commit head as a synchronisation primitive. As of
this writing, the only other consumer is the stats aggregator
(`DeltaApplyResult` accumulation), and per-file stats are
commutative under reordering: the `--stats` output is a sum, not a
sequence.

`--delete-after` end-of-transfer fence is trivially preserved: the
mpsc channel close happens at `flush`, by which point all
committed-or-failed results have been drained.

`--delete-before` is unaffected because deletion happens before any
file work begins.

### B.5 Test strategy

One identical-output fixture, one direct-bypass unit test, and one
interop assertion.

1. **Identical-output fixture.** A new test under
   `crates/transfer/tests/` that:
   - Builds a 200-file source tree (above the 64-file parallel
     threshold).
   - Runs `oc-rsync --delay-updates=false src/ dst1/` with the
     bypass forced ON via a hidden test-only env var
     (`OC_RSYNC_REORDER_BYPASS=1`).
   - Runs `oc-rsync --delay-updates=false src/ dst2/` with the
     bypass forced OFF.
   - Asserts byte-identical destination trees: same file contents,
     same metadata (mode, mtime, ownership where applicable),
     same xattrs, same ACLs.
   - Asserts identical `--stats` output: literal/matched/transferred
     counts equal between the two runs.
   The test does NOT assert intermediate state during the run; that
   is allowed to differ (one mode commits in arrival order, the
   other in NDX order).
2. **Bypass unit test.** A test in `delta_pipeline.rs::tests` that
   constructs a `BypassPipeline` directly, submits 50 work items,
   collects results, and asserts the result count equals the
   submission count without asserting order. Compares against the
   existing `parallel_preserves_submission_order` test (which DOES
   assert order) to make the contract difference visible in the
   test corpus.
3. **Interop assertion.** Extend `tools/ci/run_interop.sh` with a
   `--delay-updates=false` push to upstream daemon (3.0.9, 3.1.3,
   3.4.1) and confirm zero new entries in
   `tools/ci/known_failures.conf`. The test exists for the parallel
   pipeline today; the assertion is that turning bypass on does not
   regress it.

The bypass-specific failure mode to watch for is a directory
deletion ordering bug in the `--delete-during` path. The existing
`crates/transfer/tests/delete_during_*.rs` suite is the regression
shield. Bypass must not change the delete-stats wire frame
(NDX_DEL_STATS, sent during the goodbye phase per
`crates/transfer/src/generator/protocol_io.rs`), and the existing
delete-stats tests assert that frame byte-for-byte.

## Common: rollout sequence

1. **A.1-A.4** - counters, `stats()` snapshot, debug log emission,
   JSON flag. Low-risk, no behaviour change. Land first; collect
   data for one release cycle.
2. **B.1-B.4** - bypass mode, default-off behind a flag once metrics
   confirm HoL stalls are real. Flip default-on after a further
   cycle of stable metrics and clean interop runs.
3. **A.5/B.5** - tests interleaved with each phase; the final
   proptest plus identical-output fixture plus interop pass is the
   release gate.

Metrics-first is deliberate: the bypass rests on the hypothesis "HoL
stalls dominate" that today's instrumentation cannot confirm.
Landing metrics first turns the hypothesis into a measurable claim.
If real-world `peak_depth` and `stall_count` show stalls are rare,
the bypass has no audience and should not land. If the data
confirms the stall, the bypass is justified by the same data.

## Out of scope

- Spill-to-tempfile when the window saturates (#1884). Metrics here
  are the prerequisite for deciding whether spill is needed; a low
  `peak_depth` confirms it is not.
- Adaptive window sizing for `BoundedReorderBuffer`. The engine-side
  ring buffer already has that policy
  (`crates/engine/src/concurrent_delta/adaptive.rs`); porting it is
  a separate task. #1885 is observability only.
- Cross-transfer metric aggregation; persistent metric storage.
  Outside the rsync process.
- Wire-protocol changes. None permitted by repo policy.

## Cross-references

- `crates/transfer/src/reorder_buffer.rs:55-190` - the buffer.
- `crates/transfer/src/delta_pipeline.rs:181-373` - the pipeline that
  owns the buffer.
- `docs/architecture/reorder-buffer.md` - HoL semantics (#1883).
- `docs/design/multi-file-delta-apply-pipeline.md` - section 2.3
  covers the delete-during fence the bypass must preserve.
- #1407, #1543, #1547, #1566, #1650, #1734 - the parallel-dispatch
  and reorder-buffer prior art.
- #1568 - in-order delivery test (the invariant the bypass relaxes).
- #1883 - HoL semantics documentation.
- #1884 - spill-to-tempfile design (next step).
- #1893 - `--delete-during` phase boundaries audit (cross-referenced
  from B.4).
- #2049 - reorder buffer drop/abort property tests.
