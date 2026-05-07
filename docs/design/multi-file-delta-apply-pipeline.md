# Multi-file delta-apply pipeline with preserved wire ordering

Tracking: oc-rsync task #1079. Receiver-side multi-file delta-apply
pipeline that overlaps per-file apply across in-flight files while
preserving strict wire-NDX order at acknowledgement and disk-commit
boundaries. No wire-protocol changes; tcpdump replay against an
upstream peer remains byte-identical with the feature on or off.

## 1. Current state and goal

The receiver applies deltas serially per file:
`crates/transfer/src/delta_apply/applicator.rs::apply_delta_stream`
runs `while applicator.apply_token(reader)? {}` for one file, drains
to disk, verifies, then renames. The next file's NDX response is
read only after the previous commit. Window fill, request emission,
and signature precomputation in
`crates/transfer/src/receiver/transfer/pipeline.rs` are pipelined
(#1543/#1544/#1547 done), but byte-level apply for files N and N+1
does not overlap.

Goal: parallelise apply across files while preserving wire-emit
order at the rename + NDX-ack boundary.

## 2. Constraints

- NDX index is strictly monotonic on the wire; the sender emits in
  file-list order.
- The `BoundedReorderBuffer` from #1407
  (`crates/transfer/src/reorder_buffer.rs`) drains in strict
  `next_expected` order; admission is windowed by `window_size`.
- Workers may dispatch and finish out of order, but emission to disk
  and to the wire must be in NDX order.
- No on-disk artefacts beyond today's `temp_guard` temp files; no
  new wire frames.

## 3. Design

```
Wire reader (1)        Apply pool (W workers)        Commit (1)
+-------------+        +----------------------+      +-------------+
| read NDX    |        | pop (seq, meta)      |      | drain RB in |
| stage delta | --WQ-->| open basis           |--RB->| seq order   |
| seq = NDX   | bound  | apply_delta_stream   | win  | rename(2)   |
|             |        | verify checksum      |      | emit ack    |
+-------------+        +----------------------+      +-------------+
```

Reused unchanged:

- `ReceiverDeltaPipeline` trait at
  `crates/transfer/src/delta_pipeline.rs` (#1543) - dispatch surface.
- `BoundedReorderBuffer` at
  `crates/transfer/src/reorder_buffer.rs` (#1407) - in-order delivery
  with `BackpressureError` when admission would exceed the window.
- `crates/engine/src/concurrent_delta/work_queue/bounded.rs` -
  bounded MPMC work queue with parking blocking.
- `apply_delta_stream` and `DeltaApplicator` - per-file state stays
  worker-local; no cross-file mutation.
- `disk_commit::spawn_disk_thread` - already a single-threaded sink.

New: `MultiFileApplyPool` at
`crates/transfer/src/delta_pipeline/apply_pool.rs` owns W workers,
the work queue, and the reorder buffer. Producer assigns
`seq = wire_NDX`; commit drains in `seq` order. Lifetime is one
transfer.

## 4. Threshold

Sequential below `PARALLEL_STAT_THRESHOLD = 64` files in the
file-list segment; parallel above (cross-ref #1547). Default worker
count W = 16, hard-capped at `2 * rayon::current_num_threads()` for
parity with `concurrent_delta/work_queue/capacity.rs`. Below the
threshold, `ThresholdDeltaPipeline` selects the existing
`SequentialDeltaPipeline` and the apply pool is not constructed.

## 5. Risks

- **Head-of-line stall (#1883).** Slow file N blocks commit for
  N+1..N+W. Reorder buffer fills, producer parks. Wall-clock cost is
  bounded by `W * max(per-file apply time)`, never below the
  sequential baseline. Documented under #1883.
- **Spill-to-tempfile pending (#1884).** Per-slot staging buffer
  bounded by `MAX_STAGED_DELTA_BYTES`; overflow must spill to a
  scratch temp file to bound heap on adversarial transfers (one tiny
  file followed by a multi-GB delta). Implementation pending under
  #1884.
- **Error propagation.** Per-file failures travel as
  `AppliedFileResult::Failed(seq, err)` through the reorder buffer
  and are emitted at the correct wire position; in-flight successors
  are not cancelled (matches upstream). Panics unwind via worker
  join handles; `TempGuard` cleans temp files on drop.

## Cross-references

- `crates/transfer/src/delta_pipeline.rs` (#1543), #1544, #1547,
  #1407, #1883, #1884.
- `docs/design/reorderbuffer-metrics-and-bypass.md` - sibling
  bypass design when `--delay-updates` is off.
