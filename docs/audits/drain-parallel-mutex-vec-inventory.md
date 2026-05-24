# DPC-1: `drain_parallel` `Arc<Mutex<Vec<_>>>` Inventory

Tracking ticket: DPC-1 (#2846). First step toward the per-worker channel
restructure tracked under DPC-3..7.

## Scope

Audit of every site in `crates/engine/src/concurrent_delta/` that uses a
`Mutex<Vec<_>>` (with or without an `Arc`) for cross-worker result
collection, batch staging, or per-file write buffering. The audit was
seeded from the `drain_parallel` hotspot called out in the
`project_drain_parallel_mutex_vec_contention` memory note and expanded
to cover sibling sites in `parallel_apply/` and `consumer/` per SPL-38
module layout.

The search predicate used was the regex
`Arc<Mutex<Vec|Arc::new\(Mutex|Mutex<Vec` rooted at
`crates/engine/src/concurrent_delta/`.

## Inventory

| file | line | type | purpose | hot_path (Y/N) | notes |
| ---- | ---- | ---- | ------- | -------------- | ----- |
| `crates/engine/src/concurrent_delta/work_queue/drain.rs` | 63 | `Vec<std::sync::Mutex<Vec<R>>>` | `drain_parallel` shard array: per-thread result buckets sized to `rayon::current_num_threads()`; each rayon worker pushes its mapped result into its shard before the final flatten | Y | Multi-writer. Hot path: one `lock+push` per `DeltaWork` item dispatched through the bounded work queue. Mitigation in place is shard fanout (one mutex per rayon thread + thread-id-hash fallback for non-pool threads); DPC-3..7 will replace this with per-worker SPSC channels and a single owning collector. |
| `crates/engine/src/concurrent_delta/work_queue/drain.rs` | 81 | `Mutex<Vec<R>>` (per-shard lock site) | The `shards[idx % num_shards].lock().unwrap().push(result)` call inside `rayon::scope` that closes the loop on the shard array above | Y | Same site as the row above, listed separately to flag the lock acquisition itself (the contention point). One lock + one push per `DeltaWork`. Shard-id derived from `rayon::current_thread_index()` or a hashed `ThreadId` when the worker is outside the rayon pool. |
| `crates/engine/src/concurrent_delta/work_queue/drain.rs` | 86-89 | `Vec<std::sync::Mutex<Vec<R>>>` (final flatten) | `shards.into_iter().flat_map(|s| s.into_inner().unwrap()).collect()` - consumes the shard array and flattens to a single `Vec<R>` once `rayon::scope` returns | N | Single-threaded. Runs once after all workers have joined the scope; no contention because every `Mutex` is uncontended by construction (every spawned task has retired). Pure ownership move via `into_inner`. |
| `crates/engine/src/concurrent_delta/parallel_apply/mod.rs` | 703, 707-708, 745 | `Arc<Mutex<Vec<u8>>>` | `VecSink` test fixture used by `parallel_apply/batch.rs` and `parallel_apply/mod.rs` unit tests to capture per-file destination bytes for byte-equality assertions vs the sequential reference path | N | Test-only (gated by `#[cfg(test)]` on `mod tests`). Multi-writer in test scenarios that exercise `apply_batch_parallel` across threads but irrelevant to production hot paths; included here so the audit is exhaustive and a future grep does not regress this entry into a production claim. |

## Hot-Path Classification

The only production `Mutex<Vec<_>>` cross-worker collection site reachable
from a delta-apply run is `WorkQueueReceiver::drain_parallel` in
`work_queue/drain.rs`. The sharded layout puts contention at
`O(1 / num_shards)` of the naive single-`Mutex<Vec<R>>` baseline, which
is why the memory note classifies it "acknowledged not-hot". All other
hits in the search radius are either:

- The same site's terminal flatten (line 86-89), which runs on the
  drain thread after every rayon task has retired - serial by
  construction.
- Test fixtures (`VecSink` in `parallel_apply/mod.rs`).

`drain_parallel_into` (`work_queue/drain.rs:136`) is the streaming
sibling of `drain_parallel`; it does not use `Mutex<Vec<_>>` at all and
relies entirely on a `crossbeam_channel::Sender<R>` for backpressure.
The DPC-3..7 restructure will move `drain_parallel`'s callers onto a
shape closer to `drain_parallel_into` (per-worker bounded channel,
single collector) so the shard array can retire.

## Sites Deliberately Excluded From This Inventory

The following sites matched a broader `Mutex<...>` grep but are out of
scope for DPC-1 because they do not implement cross-worker `Vec<_>`
result collection:

- `parallel_apply/slot_barrier.rs:162` - `BarrierState.inflight:
  Mutex<usize>` (in-flight counter for the per-file barrier; FFB-2).
- `parallel_apply/slot_barrier.rs:262` - `SlotData.slot:
  Mutex<FileSlot>` (per-file destination write lock; DG-3.a).
- `consumer/mod.rs:150` - `DeltaConsumer.metrics: Arc<Mutex<ReorderMetrics>>`
  (single metrics struct, not a `Vec<_>`).
- `consumer/loops.rs:27` - the corresponding `&Arc<Mutex<ReorderMetrics>>`
  borrow inside the reorder loop.

These are listed only so a future reviewer rerunning the grep does not
have to re-classify them.

## Follow-Up

The structural fix (per-worker SPSC channels + single owning collector)
is tracked under DPC-3..7. This audit is the inventory checkpoint
DPC-1 was created to deliver; no code changes are made here.
