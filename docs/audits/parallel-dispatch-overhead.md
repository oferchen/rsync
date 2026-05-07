# Parallel dispatch overhead profile (#1551)

Static analysis of the per-file overhead the receiver-side parallel delta
pipeline pays for thread-pool dispatch, channel hand-off, and reorder-buffer
ordering when the work-set scales to 100K and 1M files. No runtime numbers;
every cost claim is derived from source. The goal is to identify the
cross-over point below which parallel dispatch is a net loss and to propose
focused reductions that keep behaviour wire-compatible with upstream rsync.

Source files inspected (paths repository-relative):

- `crates/engine/src/concurrent_delta/work_queue/{bounded,drain,capacity}.rs`
  - bounded `crossbeam_channel` (SPMC, `2 * num_threads`), rayon-scope
    `drain_parallel{,_into}` with per-thread mutex-guarded shards.
- `crates/engine/src/concurrent_delta/consumer.rs` - `DeltaConsumer::spawn`
  (drain thread + reorder thread + mpsc channel).
- `crates/engine/src/concurrent_delta/reorder.rs` - ring-buffer
  `ReorderBuffer<T>`, `force_insert` growth path.
- `crates/transfer/src/reorder_buffer.rs` - `BoundedReorderBuffer<T>`
  `BTreeMap` variant (default window 64).
- `crates/transfer/src/delta_pipeline.rs` - `ParallelDeltaPipeline::new`,
  `ThresholdDeltaPipeline` with `DEFAULT_PARALLEL_THRESHOLD = 64`.
- `crates/transfer/src/receiver/transfer/pipeline.rs` -
  `run_pipeline_loop_decoupled` (integration point on the receiver).

## TL;DR

Per-file dispatch cost is dominated by three charges: one
`crossbeam_channel::bounded` send + recv pair (work queue), one
`rayon::scope` task spawn per item (no thread spawn after pool warm-up),
and one ring-buffer `insert` plus an mpsc hand-off in the reorder thread.
Each item also pays one `Mutex` lock on its assigned shard inside
`drain_parallel`, and the streaming variant `drain_parallel_into` adds one
`Sender::clone` (`Arc` increment) per spawn.

The break-even threshold today is hard-coded at
`DEFAULT_PARALLEL_THRESHOLD = 64`
(`crates/transfer/src/delta_pipeline.rs:42`); below that the
`ThresholdDeltaPipeline` keeps work in a `Vec<DeltaWork>` and never spins up
the parallel path. The five reductions in Section 4 trade implementation
complexity for fewer atomic ops, fewer allocations, and fewer mutex
acquisitions per file.

## 1. Dispatch path summary

`ParallelDeltaPipeline::new(worker_count)` constructs the entire plumbing:

```rust
let capacity = worker_count.saturating_mul(2).max(2);
let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
let consumer = DeltaConsumer::spawn(work_rx, capacity);
```

(`crates/transfer/src/delta_pipeline.rs:209-212`).

`DeltaConsumer::spawn` layers two background threads:

```text
WorkQueueReceiver
   |  drain_parallel_into(strategy::dispatch, stream_tx)
   v
rayon::scope { for work in work_rx { s.spawn(...) } }   (delta-drain)
   |  crossbeam_channel::bounded::<DeltaResult>(stream_capacity)
   v
ReorderBuffer::insert(seq, result)                       (delta-reorder)
   |  ReorderBuffer::drain_ready -> mpsc::Sender::send
   v
DeltaConsumer::try_recv (in-order)                       (receiver)
```

(`crates/engine/src/concurrent_delta/consumer.rs:128-194`,
`crates/engine/src/concurrent_delta/work_queue/drain.rs:136-155`).

The receiver hot loop calls `submit_work` per file from
`run_pipeline_loop_decoupled`
(`crates/transfer/src/receiver/transfer/pipeline.rs:38-115` plus
`receiver/transfer.rs:574,607,757,790`). Each file pays one path through
this entire pipeline; every line in the diagram is a per-file cost.

## 2. Cost components

### 2.1 Thread pool spawn (one-time, amortised)

`rayon::scope` reuses the rayon global thread pool. The pool is
initialised on first use via `OnceLock` and persists for the process
lifetime; no `std::thread::spawn` runs per item. The two
`thread::Builder::new().spawn` calls in `DeltaConsumer::spawn`
(`crates/engine/src/concurrent_delta/consumer.rs:138,146`) fire once per
`ParallelDeltaPipeline` instance, not once per file. For a 100K-file
transfer with one pipeline, that is two `pthread_create` calls amortised
across the run.

The cost line that does scale is `s.spawn(move |_| { ... })` inside
`rayon::scope` (`work_queue/drain.rs:71-83`). Each spawn allocates a small
heap closure (rayon's `HeapJob`) and pushes it onto the calling thread's
deque via an atomic CAS. Steal returns a closure pointer plus an
`Arc::clone` of the scope state. Per-spawn overhead is therefore one
allocation, one CAS push, and one CAS pop, plus the closure invocation.

### 2.2 Crossbeam channel send / recv

`bounded_with_capacity` constructs a `crossbeam_channel::bounded` ring
(`crates/engine/src/concurrent_delta/work_queue/bounded.rs:101-103`).
Per-item cost on the work queue:

- `send`: one waiter increment, one CAS to claim a slot, one payload move,
  one notify if a receiver was parked.
- `recv` (the iterator drives this in `drain_parallel_into`): one CAS to
  claim the slot, one payload move out, one notify if a sender was parked.
- Per `DeltaWork`: 32 bytes inline plus an `Option<PathBuf>` (basis path)
  whose heap allocation is moved by pointer.

The streaming variant `drain_parallel_into` clones `tx` per spawn
(`work_queue/drain.rs:144`). `crossbeam_channel::Sender::clone` is one
`Arc::clone` (one `fetch_add` of a `Relaxed` atomic). At 100K items that
is 100 000 extra `Arc` increments plus matching decrements at drop. The
non-streaming `drain_parallel` does not clone the sender per spawn but
pays one `Mutex` lock per shard insert (`work_queue/drain.rs:81`).

### 2.3 ReorderBuffer insert / drain

`ReorderBuffer<T>` is a fixed-capacity ring of
`Box<[Option<T>]>` plus head/count/capacity/high-water fields
(`crates/engine/src/concurrent_delta/reorder.rs:86-95`). `insert` is O(1)
(modulo to compute the slot, write into `Option<T>`, counter bump,
high-water update). `next_in_order` is also O(1) (`Option::take`, head
advance, decrement).

The `delta-reorder` thread inserts one `DeltaResult` per delivered worker
result and drains zero or more contiguous items per insert. Per item the
inner loop (`crates/engine/src/concurrent_delta/consumer.rs:151-176`)
charges: one `crossbeam_channel::Receiver::recv` (stream channel), one
`result.clone()` (per the loop at `consumer.rs:154`; the redo path
clones a second time at `consumer.rs:165`), one `ReorderBuffer::insert`,
and zero or more `drain_ready` iterations each yielding one `mpsc` send.

The `BoundedReorderBuffer<T>` (`crates/transfer/src/reorder_buffer.rs:55-64`)
is the alternative variant in the older `transfer` crate path: `BTreeMap<u64, T>`
with O(log n) insert / remove; for a window of 64 the constant factor is
small (3-5 cmp + tree-node load) but allocator pressure exists as one
B-tree node split per ~11 inserts.

### 2.4 mpsc hand-off

The reorder thread forwards each in-order result through `std::sync::mpsc`
(`crates/engine/src/concurrent_delta/consumer.rs:130`). `mpsc::channel` is
unbounded, so `send` is one heap allocation plus an atomic CAS push.
`recv` is one CAS pop plus a potential park/unpark. At 100K files this is
100 000 allocations on the result lifetime - one per delivered file.

### 2.5 Per-file cost summary

| Stage | Atomic ops | Allocations | Mutex locks |
|-------|-----------|-------------|-------------|
| `submit_work` -> work_tx.send | 1-2 CAS | 0 | 0 |
| `WorkQueueIter::next` -> recv | 1-2 CAS | 0 | 0 |
| `s.spawn` HeapJob | 1 CAS | 1 | 0 |
| `Sender::clone` (drain_parallel_into) | 1 CAS | 0 | 0 |
| `tx.send(result)` (stream) | 1-2 CAS | 0 | 0 |
| stream rx | 1-2 CAS | 0 | 0 |
| `result.clone()` | 0 | 1 if redo | 0 |
| `ReorderBuffer::insert` | 0 | 0 | 0 |
| `drain_ready` -> mpsc send | 1 CAS | 1 | 0 |
| consumer `try_recv` | 1 CAS | 0 | 0 |

Per-file total: ~7-10 atomic ops, 2 allocations (HeapJob + mpsc node),
and zero mutex locks in the streaming path. The non-streaming
`drain_parallel` adds one `Mutex` lock and trades the per-spawn
`Sender::clone` for one `Vec::push` under that lock.

## 3. Per-file overhead at 100K and 1M

### 3.1 Aggregate ops

| Files | Work-queue CAS | Spawns | mpsc allocs | HeapJob allocs |
|-------|---------------:|-------:|------------:|---------------:|
| 100K  | ~400 K        | 100 K  | 100 K       | 100 K          |
| 1M    | ~4 M          | 1 M    | 1 M         | 1 M            |

A modern x86_64 CAS resolves in ~10-30 cycles when uncontended; at 4 M
ops that is 10-120 M cycles, or 3-40 ms on a 3 GHz core. The dominant
aggregate cost is therefore allocations (HeapJob + mpsc), not the channel
operations. At 1 M files, 2 M allocations through the system allocator
represent 10-50 ms of allocator time on top of the CAS budget.

### 3.2 Cross-over threshold

The current cross-over is hard-coded at
`DEFAULT_PARALLEL_THRESHOLD = 64` (`delta_pipeline.rs:42`). Below that
count `ThresholdDeltaPipeline` keeps work in a `Vec<DeltaWork>` and only
promotes once the buffered count reaches the threshold
(`delta_pipeline.rs:322-331`). Mirrors `ParallelThresholds::stat = 64`
in the receiver.

From Section 2.5 the dispatch overhead is on the order of 1-3
microseconds (HeapJob alloc, mpsc alloc, ~7 CAS). If per-file work itself
is below ~10 microseconds (a typical no-op quick-check skip), the
parallel path is a net loss because HeapJob spawn alone exceeds the saved
time. Three regimes:

- **`< ~32 files`**: parallel pipeline never reaches steady state. Spawn
  overhead dominates and `ThresholdDeltaPipeline` correctly defers.
- **`32-128 files`**: marginal. The pipeline reaches steady state but
  amortised gain over a sequential path is small because workers spend
  more time waiting for queue refills than processing.
- **`> 128 files`**: parallelism wins. Workers stay saturated and the
  per-file dispatch cost is hidden under per-file work.

The current 64 threshold is on the edge of the marginal zone. A higher
threshold (128 or 256) would reduce the chance of paying dispatch cost
without recovering it on workloads with very fast per-file work
(quick-check no-ops, empty files).

### 3.3 Reorder-buffer scaling

At default capacity (`2 * worker_count`) the ring is 32 slots on a
16-thread machine. The buffer never holds 100K items in steady state; the
"100K" point is only reached when the head stalls and `force_insert`
grows the ring to fit the next out-of-order successor
(`reorder.rs:334-360`). That growth path is the memory regime documented
in `docs/audits/reorder-buffer-memory-100k.md`; this audit treats it as a
worst-case latency tail rather than a steady-state cost.

Per-file the steady-state reorder cost is constant: O(1) insert plus O(1)
drain, with `(head + offset) % capacity` as the only meaningful
arithmetic. The cost does not grow with file count.

## 4. Proposed reductions

Listed in increasing complexity. Each is wire-compatible and preserves
the strict-ordering invariant the consumer requires.

### 4.1 Thread-local accumulators in `drain_parallel`

`drain_parallel` shards results across `num_threads` mutex-guarded `Vec`s
(`work_queue/drain.rs:62-90`). Each push pays one `Mutex` lock plus a
possible `Vec::reserve` on capacity overflow. A `thread_local!`
accumulator owned by each rayon worker removes the lock entirely: each
worker keeps a `RefCell<Vec<R>>` in TLS, pushes without locking, and
`drain_parallel` flushes the TLS vectors at the end of `rayon::scope`.
TLS access is one load plus one `RefCell` borrow check, both cheaper
than `Mutex::lock`. At 100K items that saves ~100K mutex lock pairs.

### 4.2 Lock-free reorder buffer for the steady-state fast path

The `delta-reorder` thread today holds the only mutable reference to the
`ReorderBuffer`, so there is no contention on the structure itself. The
remaining cost is one cache-miss per `insert` when the slot array is
large.

A lock-free SPSC variant - a fixed-size `AtomicUsize` slot-tag array plus
a `Box<[UnsafeCell<MaybeUninit<T>>]>` payload array - would let the drain
thread write directly into the buffer, removing the intermediate
`crossbeam_channel::bounded::<DeltaResult>(stream_capacity)` stage: one
`send`, one `recv`, and one bounded-channel allocation per item gone.
The reorder thread becomes a pure poller. Trade-off: a small `unsafe`
surface in `fast_io` per the unsafe-code policy and loss of the stream
channel's natural backpressure. Bench data should confirm the cache-miss
profile before this is worth landing.

### 4.3 Pre-sized buckets in the streaming path

`drain_parallel_into` clones the `Sender<R>` per spawn
(`work_queue/drain.rs:144`). At 100K spawns that is 100K extra `Arc`
increments and 100K matching decrements at drop. Two cheaper hand-off
shapes:

1. **Per-worker batch flush.** Each rayon worker accumulates results into
   a thread-local `Vec` of fixed capacity (say 16 items) and flushes via
   a single batched send when the bucket is full or the scope is closing.
   Reduces the number of channel ops by 16x and amortises the
   `Sender::clone` cost.
2. **Per-worker dedicated channels.** Construct one
   `crossbeam_channel::bounded` per rayon worker and have the reorder
   thread drain via `select!`. Each worker writes to its own channel
   without the per-spawn clone. The reorder thread pays one extra
   `select!` arm per worker but no per-item clone.

Either option preserves wire ordering because the reorder buffer still
sequences by `DeltaResult::sequence`, assigned by the producer before
dispatch.

### 4.4 Lift the redo-path clone in `DeltaConsumer`

`consumer.rs:154-165` clones each `DeltaResult` once on every insert:

```rust
while reorder.insert(result.sequence(), result.clone()).is_err() {
    ...
    reorder.force_insert(result.sequence(), result.clone());
    break;
}
```

A small refactor consumes `result` on the success branch (the common
case) and only clones when the buffer is at capacity and a retry is
needed. This removes 100K-1M `DeltaResult::clone` calls per transfer plus
the redo-path `String` heap allocation.

### 4.5 Adaptive threshold based on observed per-file work

Today `DEFAULT_PARALLEL_THRESHOLD` is a single compile-time constant.
`ThresholdDeltaPipeline` could measure the time each buffered item takes
to dispatch in sequential mode and compare it to a fixed dispatch budget
(say 5 microseconds per item). If the average per-file work is below the
budget the buffer stays sequential indefinitely; otherwise it promotes to
parallel as today. The measurement is one `Instant::now()` pair per
buffered item plus a running mean.

## 5. Recommendations

1. **Land the thread-local accumulator change (4.1) first.** Contained
   diff, measurable win, removes a `Mutex` lock from the per-file hot
   path on `drain_parallel`. The bench point already exists in
   `crates/engine/benches/reorder_buffer_scaling.rs`.
2. **Fix the redo-path clone (4.4).** Cheap and obvious. A clean prereq
   for any further consumer-side optimisation.
3. **Defer the lock-free reorder buffer (4.2).** Without bench data
   showing the bounded stream channel is the bottleneck, the unsafe
   surface area is not justified.
4. **Prototype per-worker channels (4.3 option 2) before per-worker
   batches (4.3 option 1).** The two-channel layout is simpler and the
   batch flush adds latency that hurts the streaming-disk-commit overlap
   the consumer relies on.
5. **Treat 4.5 as research.** A workload-aware threshold may be better
   implemented as a CLI knob (`--parallel-threshold`) before being made
   adaptive.

## 6. Out of scope

- Multi-producer dispatch. The receiver wire is single-threaded by
  protocol design (`work_queue/mod.rs:11-21`).
- Spill-to-tempfile for stalled successors. Tracked separately in
  `docs/audits/reorder-buffer-memory-100k.md` and the (closed) #1884.
- Async / tokio rewrite of the receiver. Tracked under the
  `tokio-vs-asyncstd-daemon.md` audit; the synchronous rayon path
  covered here is the production path.

## 7. Summary

Per-file dispatch through the parallel delta pipeline is bounded - 7-10
atomic ops, 2 allocations, one mutex lock per shard, and one ring-buffer
insert plus an mpsc hand-off per delivered result. Aggregate cost at 100K
files lands in the low milliseconds for the channel ops and tens of
milliseconds for the allocator pressure; at 1M files those numbers grow
linearly. The cross-over threshold today is `64`, on the edge of the
marginal zone. The five reductions in Section 4 trim mutex locks (4.1),
redundant clones (4.4), and the per-spawn `Sender::clone` (4.3) without
changing wire behaviour, with the lock-free reorder variant (4.2) and
adaptive threshold (4.5) reserved as second-tier work pending bench data.
