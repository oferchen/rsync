# ABW-3 - Pipelined verify/write implementation (closure, N/A pending per-file Mutex refactor)

Date: 2026-05-21
Scope: closure note for the ABW-3 implementation track
Status: N/A pending per-file Mutex refactor in `ParallelDeltaApplier`
Predecessors:
  - `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` (PR #4670, merged)
  - `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` (PR #4673, merged)
Sibling closure: `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`
  - The sibling defers ABW-2/3/4 pending BR-3j.f bench evidence; this note
    adds the structural reason ABW-3 also cannot ship as a standalone
    implementation even once that bench evidence arrives.
Tracker: ABW-3 (#2572); kept open with the
  "N/A pending per-file Mutex refactor" label

## 1. Decision

ABW-3 (the implementation step that would build pipelined verify/write
overlap on top of the ABW-2 design) is **N/A pending per-file Mutex
refactor in `ParallelDeltaApplier`**.

Even if BR-3j.f (#2508) bench evidence reopens the ABW-2 design gate,
ABW-3 stays closed until the per-file `Mutex<FileSlot>` in
`ParallelDeltaApplier` is replaced with a structure that does not
serialise writes. Building pipelining on top of a serial-write critical
section spends review and maintenance budget on a refactor whose
throughput ceiling is set by the bottleneck the pipelining cannot
touch.

ABW-4 (the companion bench) follows: it is **N/A** for the same
reason, because there is nothing measurable to bench until ABW-3 has a
real artifact to compare against.

## 2. Why ABW-3 is structurally blocked

The verify/write pipeline ABW-2 sketches overlaps verify batch `N+1`
with write batch `N`. The write side of that pipeline is
`FileSlot::ingest` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:248-258`, which
runs under the per-file `Mutex<FileSlot>` declared at
`crates/engine/src/concurrent_delta/parallel_apply.rs:227-231` and
wrapped by the `SlotBarrier` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:280-296`.

Two observations from the established record:

1. **The per-file Mutex is the dominating bottleneck.**
   `project_apply_batch_write_serial.md` records the per-file
   `Mutex<FileSlot>` as the serialisation point that gates any
   verify/write overlap: `par_iter` verifies in parallel, but the
   inner per-file mutex serialises writes. The ABW-1 audit reaches
   the same conclusion (audit table at section 2.2): in every
   workload cell except CPU-bound verify, the write wall dominates
   the pipelined total, so verify/write overlap cannot exceed the
   write side's serial ceiling.
2. **ABW-1 / ABW-2 already account for this.** The sibling closure
   doc (`docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md`,
   section 5 "Do not kill the option") explicitly calls out the
   per-file `Mutex<FileSlot>` as the bottleneck and lists
   "multi-threaded per-file writer" as one of the two future shifts
   that would change the picture. ABW-2's design defers waiting for
   that shift; ABW-3's implementation cannot ship before it.

The compound is: an ABW-3 patch that pipelines verify and write while
the per-file Mutex still serialises every write critical section adds
a bounded channel, a writer thread, error-propagation rework, a memory
ceiling argument, and a second test surface (the proptest at
`crates/engine/src/concurrent_delta/parallel_apply.rs:858+` would have
to keep both paths green) - all to ship code whose write side still
funnels through a single Mutex per file. The ABW-1 audit's "I/O-bound
write" cell (1.03x speedup) is the realistic regime here, and even
that 1.03x is the ceiling, not the expected value.

## 3. Downstream impact

- **ABW-4 (#2573) - bench.** N/A for the same reason: there is no
  pipelined implementation to bench against the serial baseline. The
  scheduler-shape effort the BR-3j.f bench (#2508) carries already
  covers the `apply_batch_parallel` shape that is in production today;
  adding an ABW-4 cell would measure code that does not exist.
- **The per-file Mutex refactor is a larger initiative, not in
  scope for this task.** It is tracked by two project memory pages:
  - `project_apply_batch_write_serial.md` - records the per-file
    Mutex as the serialisation point that bounds pipelined throughput.
  - `project_parallel_delta_apply_outer_mutex.md` - records the
    outer `HashMap` mutex on `ParallelDeltaApplier` that serialises
    per-file registration and lookup at high file counts. A refactor
    that replaces only the inner per-file Mutex without addressing
    the outer one would still leave a Mutex on the hot path; both
    layers need a coherent design.
  Both pages remain open. This closure note does not pre-commit to
  a refactor shape (lock-free `FileSlot`, sharded inner map,
  per-file work-queue, or another design); that is the job of the
  follow-on initiative.

## 4. Re-open trigger

ABW-3 re-opens when **all three** preconditions hold:

1. **The per-file `Mutex<FileSlot>` is replaced** by a structure that
   does not serialise writes across the workers that hold the file's
   `SlotHandle` (e.g. a lock-free `FileSlot`, a sharded inner map,
   or a per-file work queue with explicit serial-per-file dispatch).
   The replacement must keep the per-file byte-order invariant the
   reorder buffer enforces today
   (`crates/engine/src/concurrent_delta/parallel_apply.rs:242-258`)
   and stay green against the golden byte tests at
   `crates/protocol/tests/golden/` plus the proptest at
   `crates/engine/src/concurrent_delta/parallel_apply.rs:858+`.
2. **ABW-2 is revisited first.** The ABW-2 design doc was written
   against the current serial-write assumption. A refactored writer
   side changes the verify/write ratio table the design hangs on;
   ABW-2 must be re-derived before ABW-3 implements against it.
3. **BR-3j.f (#2508) bench evidence supports it.** The bench's
   `verify_wall / write_wall` ratio gate from the ABW-1 audit
   (section 4) still applies under the new writer shape: pipelining
   is only worth implementing if the verify and write walls are
   within 2x of each other on a production-relevant cell.

The first precondition is the structural one this closure note adds.
The other two are inherited from the sibling closure doc.

## 5. Closure shape for the tracker

- ABW-3 (#2572): stays open. Label changes from `deferred pending
  BR-3j.f bench evidence` to `N/A pending per-file Mutex refactor`.
  Linked back to this doc, the sibling ABW-2 closure, the ABW-1
  audit, and the two project memory pages above.
- ABW-4 (#2573): stays open. Same label change. Blocked-on ABW-3
  via this closure.

Project memory pages
`project_apply_batch_write_serial.md` and
`project_parallel_delta_apply_outer_mutex.md` keep their existing
entries and gain a reference to this closure doc. Neither page is
marked resolved; the per-file Mutex refactor is the open initiative
those pages track.

## 6. References

- `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` -
  the ABW-1 audit; section 2.2 quantifies the pipelining ceiling
  under the current serial-write writer, section 4 carries the
  decision gate.
- `docs/design/abw-2-pipelined-verify-write-deferred-2026-05-21.md` -
  the ABW-2 design closure; section 5 names the per-file Mutex as
  the bottleneck this closure formalises into a re-open
  precondition.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:227-231` -
  `FileSlot`, the per-file destination writer that the per-file
  Mutex guards.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:248-258` -
  `FileSlot::ingest`, the serial-write critical section pipelining
  would feed but cannot accelerate.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:280-296` -
  `SlotBarrier`, the wrapper around `Mutex<FileSlot>` plus
  in-flight counter; any replacement of the per-file Mutex has to
  preserve the barrier contract FFB-1 / FFB-2 depend on.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:858+` - the
  parallel-apply proptest; any refactor must keep both the old and
  new writer paths green here while the per-file byte-order
  invariant is in flight.
- `project_apply_batch_write_serial.md` - project memory entry for
  the per-file Mutex serialisation observation.
- `project_parallel_delta_apply_outer_mutex.md` - project memory
  entry for the outer HashMap mutex on `ParallelDeltaApplier`; the
  refactor that unblocks ABW-3 has to consider both Mutex layers.
- BR-3j.f (#2508) - re-bench task; the ratio data the ABW-2 design
  gate consumes once the writer shape changes.
