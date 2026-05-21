# ABW-2/3/4 - Pipelined verify/write for `apply_batch_parallel` (deferred)

Date: 2026-05-21
Scope: closure note for the ABW-2/3/4 design and implementation track
Status: deferred pending BR-3j.f (#2508) bench evidence
Predecessor: `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` (PR #4670, merged)
Tracker: ABW-2 (#2571), ABW-3 (#2572), ABW-4 (#2573); kept open with the
"deferred pending bench evidence" label

## 1. ABW-1 recap

The ABW-1 audit catalogued `ParallelDeltaApplier::apply_batch_parallel`
(`crates/engine/src/concurrent_delta/parallel_apply.rs:515-542`) as a
hard-barriered two-phase loop: parallel `par_iter().map(verify_chunk).collect()`
followed by a serial drain that holds the per-file
`Mutex<FileSlot>` while `slot.ingest`
(`crates/engine/src/concurrent_delta/parallel_apply.rs:248-258`) inserts and
writes each chunk.

The quantified pipelining ceiling (audit table at section 2.2):

| Case                                | verify_wall | write_wall | current | pipelined | Speedup |
|-------------------------------------|-------------|------------|---------|-----------|---------|
| Balanced (`C = W`, `K=8`, `N=64`)   | 8C          | 64C        | 72C     | 64C       | 1.13x   |
| CPU-bound verify (`C = 4W`, ditto)  | 32W         | 64W        | 96W     | 64W       | 1.50x   |
| I/O-bound write (`W = 4C`, ditto)   | 8C          | 256C       | 264C    | 256C      | 1.03x   |
| Single-file batch                   | -           | -          | -       | -         | ~0x     |

The audit's explicit recommendation (section 4): **skip ABW-2/3 until
BR-3i.f bench evidence shows verify cost and write cost are within 2x of each
other on the production workload.** RJN-1 (PR #4656) also confirmed
`apply_batch_parallel` has zero production callers today; only the
`parallel_receive_delta_perf` bench and the in-tree tests exercise it.

This document discharges the audit's recommendation by formally deferring
ABW-2 (design), ABW-3 (implementation), and ABW-4 (bench) until a re-bench
under BR-3j.f produces the ratio data the decision gate depends on.

## 2. Why we defer the design (not just the implementation)

A design doc that we do not intend to implement still costs reviewer time,
review-cycle context, and a doc surface we have to keep in lockstep with the
code. Three reasons to also defer the design:

1. **Peak benefit is workload-dependent.** The 1.5x case requires verify to
   dominate write by 4x. Production parallel-receive-delta workloads have
   not been characterised on the verify-vs-write axis; we do not know which
   audit cell they sit in. Designing for the 1.5x cell would over-fit a
   design point we cannot defend.
2. **Complexity-to-payoff is poor in the measured cells.** The simplest
   pipelined shape (audit section 2.1) adds a bounded
   `crossbeam_channel::bounded::<Result<VerifiedChunk,
   ParallelApplyError>>(cap)`, a writer thread, error propagation rework, a
   memory-ceiling argument, and new tests for backpressure and mid-batch
   verify failure. The audit estimates ~200-300 LoC of net new code plus a
   second test surface (sequential and pipelined paths both need to keep
   passing the proptest at
   `crates/engine/src/concurrent_delta/parallel_apply.rs:858+`). Paying that
   for a 1.13x balanced-case win is not a trade we want to lock in.
3. **Single-file workloads get nothing.** The production heuristic wired
   in PIP-3+5 (PR #4666) dispatches parallel-receive-delta only when
   `file_count > 100 || total_size > 64 MiB`, so the single-file degenerate
   case is already gated out at dispatch time. The remaining cases are
   exactly the many-file ones the audit shows still gain 0% from pipelining
   while the writer phase stays single-threaded.

The compound of these is that even an optimistic ABW-2 design lands code
that is fastest in a cell we have not proven is on the hot path, neutral or
slightly negative in the cells we know the production dispatcher actually
reaches, and unable to help the workload shape (single-file) the production
dispatcher steers away from anyway. Deferring the design preserves the
option without burning the review and maintenance budget.

## 3. What would change the call

The BR-3j.f re-bench task (#2508) extends
`crates/engine/benches/parallel_receive_delta_perf.rs` to emit per-batch
`verify_wall` and `write_wall` wall-clock measurements (the audit's `C` and
`W` aggregates) across the existing workload cells (`mixed`,
`large_files`, plus the NVMe and HDD-ish container variants).

Decision gate, lifted verbatim from the ABW-1 audit (section 4):

- Run BR-3i.f and `parallel_receive_delta_perf` on the rsync-profile
  container (NVMe + xxh3) and on the rsync-bench container (HDD-ish + MD5
  aarch64 software path).
- Compute `ratio = verify_wall / write_wall` per workload cell.
- If `0.5 <= ratio <= 2.0` on any production-relevant cell, proceed to
  ABW-2.
- If `ratio < 0.5` or `ratio > 2.0` on every cell, mark
  `project_apply_batch_write_serial.md` as "investigated; pipelining not
  justified by measurement" and close the line of work.

The gate is symmetric: BR-3j.f data can either unblock the design or close
out the project memory. Either outcome is a useful conclusion.

## 4. Closure shape for the tracker

- ABW-2 (#2571): stays open. Label `deferred pending BR-3j.f bench
  evidence`. No assignee. Linked back to this doc and the ABW-1 audit.
- ABW-3 (#2572): stays open. Same label. Blocked-on ABW-2 implicitly via
  ABW-2's gate.
- ABW-4 (#2573): stays open. Same label. The `parallel_receive_delta_perf`
  extension it depends on is partly delivered by BR-3j.f itself, so ABW-4
  becomes thin once BR-3j.f lands and ABW-2 proceeds.

Project memory page `project_apply_batch_write_serial.md` keeps the
existing "pipelined design would overlap write batch N with verify batch
N+1" entry and gains a reference to this closure doc; we do not mark the
project as "investigated; not justified" yet because BR-3j.f has not been
run.

## 5. Do not kill the option

The per-file `Mutex<FileSlot>` at
`crates/engine/src/concurrent_delta/parallel_apply.rs:248-258` is the
bottleneck, not the verify/write barrier. The audit shows that as long as
the writer side is single-threaded, pipelining's gain ceiling is the
verify-side amortisation, which is small whenever write is cheap or
verify is fully cores-bound. Two future shifts would change the picture:

1. **Multi-threaded per-file writer.** If a future applier ships
   per-file work queues with explicit serial-per-file dispatch (the
   audit's "ABW-x" hypothetical, section 2.3), the writer side
   parallelises across files and pipelining the verify/write overlap
   becomes attractive again. This requires per-file ordering proofs
   against the golden byte tests (`crates/protocol/tests/golden/`) and
   the proptest at
   `crates/engine/src/concurrent_delta/parallel_apply.rs:858+`; it is
   not a drop-in change.
2. **CPU-bound verify regime.** If a future checksum strategy
   (post-MD5, or a hardware-poor target where MD5 is the software
   fallback) pushes the verify side into the 2-4x-of-write regime, the
   "CPU-bound verify" row of the audit table becomes the production
   cell and ABW-2's 1.5x gain becomes a real win. The verify-side cost
   already varies across the algorithms enumerated at
   `crates/engine/src/concurrent_delta/parallel_apply.rs:632-652`
   (`verify_chunk` delegates to the per-batch `ChecksumStrategy`); a
   future addition that biases towards software MD5/aarch64 would
   shift the regime.

Both shifts are out-of-scope today. We list them so that future
contributors with bench data in hand know which ABW-2 design assumptions
this closure rests on and which would need re-opening.

## 6. References

- `docs/audits/abw-1-apply-batch-verify-write-overlap-2026-05-21.md` -
  the predecessor audit; section 4 carries the decision gate this doc
  discharges.
- `docs/audits/rjn-1-apply-chunk-parallel-call-sites-2026-05-21.md` -
  call-site catalogue confirming zero production callers as of master.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:515-542` -
  `apply_batch_parallel`, the function ABW-2 would refactor.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:248-258` -
  `FileSlot::ingest`, the per-file Mutex-protected drain.
- `crates/engine/src/concurrent_delta/parallel_apply.rs:632-652` -
  `verify_chunk`, the CPU-bound work the pipelining would overlap.
- `crates/engine/benches/parallel_receive_delta_perf.rs` - the bench
  harness BR-3j.f extends to produce verify/write ratio data.
- BR-3j.f (#2508) - re-bench task; gating dependency for re-opening
  ABW-2/3/4.
- PIP-3+5 (PR #4666) - the dispatch heuristic that gates single-file
  batches out of parallel-receive-delta at the receiver entry point.
- `project_apply_batch_write_serial.md` - project memory page tracking
  the per-file Mutex serialisation observation.
