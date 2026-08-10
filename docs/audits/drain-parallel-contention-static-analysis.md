# `drain_parallel` Mutex contention: static analysis

Tracking issue: oc-rsync task #1679 (profile contention). Related
work: #1358 (par_bridge -> crossbeam work-stealing deque, done), #1617
and #1680 (per-thread Vec result accumulation, done), #1620 (DashMap
evaluation, deferred), #1681 (lock-free MPSC alternative, pending),
#1682 (benchmark Mutex vs per-thread Vec vs MPSC, pending), #1192
(profile `Arc<Mutex<Vec>>` under 100K+ files, pending), #1856
(work_queue.rs decomposition, done in PR #3420).

Last verified: 2026-05-05 against
`crates/engine/src/concurrent_delta/work_queue/{drain,bounded,capacity,iter,mod}.rs`,
`crates/engine/src/concurrent_delta/{consumer,reorder,mod}.rs`,
`crates/transfer/src/delta_pipeline.rs`,
`crates/engine/benches/drain_parallel_benchmark.rs`,
`crates/engine/benches/reorder_buffer_scaling.rs`, and
`crates/engine/benches/buffer_pool_contention.rs`.

## Summary

`WorkQueueReceiver::drain_parallel` collects rayon worker results into
`N` per-thread `Mutex<Vec<R>>` shards rather than a single
`Mutex<Vec<R>>`
(`crates/engine/src/concurrent_delta/work_queue/drain.rs:62-89`). Each
shard is keyed by `rayon::current_thread_index()`; threads outside the
rayon pool fall back to a `DefaultHasher` over
`std::thread::ThreadId`. Under steady state every rayon worker hits a
distinct shard, so the lock is effectively uncontended on the per-item
hot path. The remaining static-analysis-visible costs are (a) the first
acquire-release of each worker thread (cache-line ownership transfer on
the `Mutex` word, not lock waiting) and (b) the `flat_map` merge on the
dispatcher after `rayon::scope` returns. Neither scales with `K` (items
per worker); both scale with `W` (worker count). The
`drain_parallel_into` variant routes results through a cloned
`crossbeam_channel::Sender<R>` instead of mutexes
(`drain.rs:136-155`); its contention model is set by
`crossbeam_channel`'s segmented atomic queue, not by user-space mutexes.

This audit is read-only. No source files are modified. It does not
substitute for the runtime profiling tracked in #1679; it extracts the
load-bearing facts runtime profiling will then quantify.

## Methodology

1. Locate every `Mutex::lock()` site reachable from the result path of
   `drain_parallel` and `drain_parallel_into` via workspace ripgrep
   for `Mutex<Vec`, `Mutex::new`, and `.lock()` restricted to
   `crates/engine/src/concurrent_delta/`.
2. Classify each acquisition by frequency: `H` (hot path, per result,
   inside the worker closure), `B` (batched, per N results, merge or
   buffered flush), `S` (shutdown, per worker, once per drain).
3. Anchor on the existing bench grid in
   `drain_parallel_benchmark.rs:20-23` (`COUNTS = [10_000, 100_000]`,
   `THREAD_COUNTS = [1, 4, 8, 16]`); extend the projection to
   `W=32 / 64`. The harness already pins custom pools at `bench:48-51`.
4. Read `delta_pipeline.rs` and `consumer.rs` to identify which
   variant is exercised in production and how the downstream
   `ReorderBuffer` interacts with the bounded streaming channel.
5. No `cargo` invocations (project rule "never run cargo locally").
   `crates/engine/Cargo.toml:141` is treated as a structural reference
   only.

## Architecture

```text
Receiver / generator (single producer)
   |  WorkQueueSender::send(DeltaWork)
   v
crossbeam_channel::bounded (capacity 2 * rayon threads)
   |
   v  WorkQueueReceiver::into_iter()  (next() == rx.recv().ok())
dispatcher thread inside rayon::scope
   |  for work in iter:
   |    s.spawn(move |_| { result = f(work); shard_push(result) })
   v
N rayon worker tasks (work-steal scheduling)
   |
   v  shard_idx = rayon::current_thread_index() % num_shards
shards: Vec<Mutex<Vec<R>>> (length == rayon::current_num_threads())
   |  rayon::scope joins all spawned tasks here
   v
shards.into_iter().flat_map(into_inner).collect::<Vec<R>>()
   |  single-threaded merge on the dispatcher
   v
caller (e.g., DeltaConsumer's drain thread, tests)
```

`drain_parallel` owns the receiver
(`crates/engine/src/concurrent_delta/work_queue/drain.rs:57`). The
bounded queue (default `2 * rayon::current_num_threads()` at
`work_queue/bounded.rs:90` and `work_queue/capacity.rs:8`) supplies
backpressure to the producer; workers see the queue through
`WorkQueueIter::next` which calls `rx.recv()`
(`work_queue/iter.rs:33`).

Sequence ordering is not preserved by the queue or the shards. Each
worker output carries the sequence number stamped before dispatch
(`crates/transfer/src/delta_pipeline.rs:223-234`), and a downstream
`ReorderBuffer` re-imposes monotonic order
(`crates/engine/src/concurrent_delta/reorder.rs:30-83`).

The streaming variant `drain_parallel_into` (`drain.rs:136-155`) does
not use shards. Each spawned worker clones the
`crossbeam_channel::Sender<R>` and calls `tx.send(result)`. This is
the variant used by `DeltaConsumer::spawn` in production
(`crates/engine/src/concurrent_delta/consumer.rs:138-143`); the
shard-based `drain_parallel` is exercised by tests and the bench
harness.

## Lock acquisition sites on the result path

| Site | File:line | Class | Frequency |
|------|-----------|-------|-----------|
| Per-item shard push inside the worker closure | `work_queue/drain.rs:81` | H | one acquire-release per `DeltaWork` item per worker |
| Final shard merge after `rayon::scope` returns | `work_queue/drain.rs:86-89` (`Mutex::into_inner`) | S | one per shard, once per drain |

There are no batch-class (`B`) acquisitions on this path. The
implementation does not buffer results locally before flushing; each
result push acquires its shard mutex directly. The "batch flush"
pattern sometimes evoked in #1617 / #1680 discussions is implemented
here as "per-rayon-thread shard plus single shutdown-time merge".

`drain_parallel_into` has zero `Mutex::lock()` sites
(`drain.rs:136-155`); synchronisation is internal to
`crossbeam_channel`. Its hot path is `tx.send(result)` per item per
worker (`drain.rs:149`); the channel is dropped at scope exit
(`drain.rs:153-154`).

The receiver iterator path has one further synchronisation point that
is *not* a worker-side lock:

| Site | File:line | Class | Frequency |
|------|-----------|-------|-----------|
| `crossbeam_channel::Receiver::recv()` in `WorkQueueIter::next` | `work_queue/iter.rs:33` | H, single-threaded | one per item, dispatcher only |

The dispatcher thread is the same thread that runs `rayon::scope`. It
serialises against the producer's `tx.send`, but only one producer
exists by construction (`WorkQueueSender` is `Send` but not `Clone`,
`work_queue/bounded.rs:48-50`).

## Per-thread Vec mitigation (#1617, #1680)

```rust
// crates/engine/src/concurrent_delta/work_queue/drain.rs:62-89
let num_shards = rayon::current_num_threads();
let shards: Vec<std::sync::Mutex<Vec<R>>> = (0..num_shards)
    .map(|_| std::sync::Mutex::new(Vec::new()))
    .collect();

rayon::scope(|s| {
    for work in self.into_iter() {
        let f = &f;
        let shards = &shards;
        s.spawn(move |_| {
            let result = f(work);
            let idx = rayon::current_thread_index().unwrap_or_else(|| {
                let id = std::thread::current().id();
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&id, &mut hasher);
                std::hash::Hasher::finish(&hasher) as usize
            });
            shards[idx % num_shards].lock().unwrap().push(result);
        });
    }
});

shards.into_iter()
    .flat_map(|shard| shard.into_inner().unwrap())
    .collect()
```

What this gives, statically:

- One `Mutex<Vec<R>>` per rayon thread, sized at scope entry by
  `rayon::current_num_threads()` (`drain.rs:62`). With the rayon
  default pool, `num_shards == W`, so every worker can target a
  distinct shard.
- The worker indexes its shard via `rayon::current_thread_index()`
  (`drain.rs:73`). The rayon API guarantees this returns
  `Some(usize)` for a rayon-pool worker, with values in
  `0..num_shards` for the active pool. Within a single scope the index
  map is bijective; each shard is touched by at most one rayon-pool
  thread.
- Threads outside the rayon pool hash their `ThreadId` to distribute
  across shards (`drain.rs:74-80`), avoiding the degenerate
  `unwrap_or(0)` case that would funnel all foreign threads into shard
  0.
- The merge (`drain.rs:86-89`) calls `into_inner` on each shard. This
  is `&mut self`; it cannot contend because `rayon::scope` has already
  joined every spawned task before control returns. The merge runs on
  the dispatcher, single-threaded.

Residual locking, statically visible:

1. Cache-line ownership transfer on the very first lock the worker
   takes after work-stealing. If thread T1 stole work originally
   destined for T2's chain, the `Mutex` word for `shard[idx_T1]` is
   cold in T1's L1 cache. Steady state amortises this; static analysis
   cannot quantify it. Runtime cachegrind / `perf c2c` can.
2. The `Vec<R>` inside each shard grows by `Vec::push`. With `K` items
   per worker and doubling growth, each worker triggers `O(log2 K)`
   reallocs. The realloc is on the worker's own shard, so it does not
   contend; it does thrash the allocator. A `Vec::with_capacity` hint
   would eliminate this; that hint is not currently passed at
   `drain.rs:64`.

## Contention model

Define:

- `W` = rayon worker thread count.
- `K` = items processed per worker (`total_items / W`).
- `T_lock` = uncontended Mutex acquire-release cost (one CAS plus
  fence on success).
- `p_steal` = probability a task migrates between rayon threads;
  bounded by `W` and queue emit rate, `O(1)` in `K`.

Sharded design (current):

```
C_lock_sharded(W, K) = W * K * T_lock           (uncontended)
                     + p_steal * W * K * T_migrate   (cache-line transfer on stolen tasks only)
                     + W * T_lock_shutdown      (final into_inner per shard)
```

Single-Mutex baseline (pre-#1617 / #1680):

```
C_lock_single(W, K) = W * K * T_lock_contended
```

Where `T_lock_contended = T_lock + queueing_delay(W)`. Under typical
Linux futex plus adaptive spin, `queueing_delay(W)` grows roughly
linearly in `W` once `W` exceeds the SMT-pair count, and faster than
linearly past the physical core count due to NUMA crossings.

Predicted thresholds at the bench grid plus the W=32 / 64 extension
#1679 and #1681 will validate:

| W | sharded | single Mutex |
|---|---------|--------------|
| 1 | `K * T_lock`, no steals | identical baseline |
| 4 | `4 * K * T_lock` plus low `p_steal` | `4 * K * T_lock_contended`; queueing fits in SMT pair |
| 8 | `8 * K * T_lock` plus moderate `p_steal` | `8 * K * T_lock_contended`; queueing crosses physical cores |
| 16 | `16 * K * T_lock` plus higher `p_steal` | NUMA-visible queueing on dual-socket |
| 32 | sharded curve still flat; shard count tracks pool size | Mutex becomes the bottleneck on most consumer hardware |
| 64 | `T_steal` dominates secondary cost | single-Mutex queueing delay dominates total time |

The static prediction: sharded design is `O(W * K)` with a small
per-acquisition constant and no super-linear contention term;
single-Mutex design is `O(W * K)` with a per-acquisition constant
that grows with `W`. Both are linear in `K` at fixed `W`. None of
this substitutes for runtime measurement; it sets the *shape* runtime
data will fit. The `drain_parallel_benchmark` harness covers
`{1, 4, 8, 16} x {10_000, 100_000}` already
(`benches/drain_parallel_benchmark.rs:20-23`). Extending to W=32 / 64
inside #1679 needs either a host with `>=32` physical threads or
manual `rayon::ThreadPoolBuilder::num_threads` above hardware
concurrency (the harness already pins counts at `bench:48-51`).

## Three alternative designs

### A. Single `Mutex<Vec<R>>` (baseline, before #1617 / #1680)

```rust
let collected = std::sync::Mutex::new(Vec::new());
rayon::scope(|s| {
    for work in self.into_iter() {
        let collected = &collected;
        s.spawn(move |_| {
            let result = f(work);
            collected.lock().unwrap().push(result);
        });
    }
});
collected.into_inner().unwrap()
```

- Lock class: H, all workers contend on the same word.
- Ordering: none; consumer reorders via `ReorderBuffer`.
- Behaviour: super-linear past `W = physical cores`, per
  `C_lock_single` above.
- Static evidence against: cache-line ping-pong on the `Mutex` word
  and the `Vec`'s `(ptr, len, cap)` triple, all on the same line
  observed by every worker on every push.

### B. Per-thread Vec plus final merge (#1680, current)

See the code block under "Per-thread Vec mitigation" above; the
implementation is at `drain.rs:62-89`.

- Lock class: H plus S, no contention in steady state.
- Ordering: none; same `ReorderBuffer` downstream. Each shard's
  internal order is dispatch order from the thread that owned it; the
  cross-shard interleave is arbitrary.
- Behaviour: flat in `W` once `num_shards == W`.
- Static evidence for: each rayon thread has affinity for a single
  shard within a scope, so the cache line carrying the `Mutex` word
  and the `Vec` triple is owned exclusively by that thread for the
  scope's duration. Cross-thread coherence traffic on the result path
  is zero in steady state.
- Static evidence against: still pays one `Mutex::lock()` per item, so
  `W * K * T_lock` is paid even though no waiting occurs. On a
  highly-tuned hot path this is visible as `lock cmpxchg` cost in
  perf reports. An `UnsafeCell<Vec<R>>` per shard would eliminate it,
  but `crates/engine/src/lib.rs` carries `#![deny(unsafe_code)]`. A
  `RefCell<Vec<R>>` cannot work because rayon may reschedule a worker
  thread mid-scope.

### C. Lock-free MPSC channel (#1681, pending)

```rust
let (tx, rx) = crossbeam_channel::unbounded::<R>();
rayon::scope(|s| {
    for work in self.into_iter() {
        let tx = tx.clone();
        s.spawn(move |_| {
            let result = f(work);
            let _ = tx.send(result);
        });
    }
});
drop(tx);
rx.into_iter().collect()
```

This is the pattern already in production for `drain_parallel_into`
(`drain.rs:136-155`), restructured to terminate with a `Vec<R>` so it
can serve as a drop-in replacement for the shard-based variant.

- Lock class: H, but inside `crossbeam_channel`, not
  `std::sync::Mutex`. Bounded form uses a fixed-size array with atomic
  indices; unbounded uses segments with atomic indices. Neither uses
  a generic Mutex on the send / recv fast path.
- Ordering: none; `ReorderBuffer` continues to handle this.
- Behaviour: bounded by the channel implementation. `unbounded` sends
  never block; `bounded` senders block when full, transferring
  contention from the result path back to the dispatcher. With a
  properly sized bounded channel, contention is comparable to design
  B.
- Static evidence for: matches the design already used by
  `DeltaConsumer` (`consumer.rs:138-143`); unifying the two
  `drain_parallel` variants behind one channel-based primitive
  collapses two implementations to one.
- Static evidence against: a bounded MPSC interacts with downstream
  backpressure. The production pipeline is already
  `drain_parallel_into -> bounded crossbeam stream -> reorder thread
  -> mpsc to caller -> caller's reorder buffer`
  (`consumer.rs:128-194`). The reorder buffer
  (`reorder.rs:73-83`) provides backpressure via `CapacityExceeded`;
  the streaming channel between drain and reorder threads is sized at
  `reorder_capacity.max(rayon::current_num_threads() * 2)`
  (`consumer.rs:134`). Adding a third bounded channel inside
  `drain_parallel` would create a three-stage backpressure cascade;
  the existing two-stage cascade has been tuned for
  `BoundedReorderBuffer` (#1566). This interaction is what runtime
  profiling tracked by #1679 must characterise before #1681 commits to
  the unification. The merge step at `drain.rs:86-89` is also a
  single-threaded flatten; an MPSC drain pays one atomic fetch_add
  per item to dequeue, whereas shard-drain pays zero atomic
  operations during `into_inner`. The static cost difference favours
  design B for the batch variant even though design C is preferable
  for the streaming variant.

## Recommendation for #1681

Defer #1681 (lock-free MPSC alternative for `drain_parallel`) until
#1679 has measured the sharded design at `W=32` and `W=64` and
confirmed either:

- `T_lock` per acquire is materially visible in the perf top-10 at
  these worker counts (static analysis says it should not be, but
  cannot rule it out for novel hardware), or
- the sharded `Vec::push` realloc pattern shows up in cachegrind /
  heaptrack output, in which case the right fix is `Vec::with_capacity`
  at `drain.rs:64`, not a switch to MPSC.

Reasoning. The streaming variant `drain_parallel_into` already uses an
MPSC channel and is the variant exercised by the production pipeline
(`crates/transfer/src/delta_pipeline.rs:209-219` ->
`DeltaConsumer::spawn` -> `drain_parallel_into`). Adding a second
MPSC-based path for the batch variant would unify the two surface APIs
but does not, by construction, reduce contention below what design B
already achieves. Static analysis predicts design B is contention-free
in steady state; if runtime data confirms this, #1681 reduces to a
stylistic unification, lower priority than #1192 (profile under 100K+
files) and the open follow-ups in #1679. If runtime data instead shows
residual contention, the right next step is to investigate whether the
shard mapping is degenerating (e.g., `current_thread_index` colliding
under a custom rayon pool) before swapping the primitive.

Concrete decision criteria for #1681 to proceed:

1. Wall-clock throughput at `W=32` worse than 0.85x of `W=16`
   throughput (a clear scaling cliff), and
2. perf top-10 shows `Mutex::lock` or `parking_lot_core::futex_wait`
   above the rolling-hash worker function `simulate_work` at
   `crates/engine/benches/drain_parallel_benchmark.rs:31`.

If both hold, #1681 unification is justified. If only (1) holds, look
upstream of `drain_parallel` (e.g., `crossbeam_channel::recv` in the
dispatcher) before changing the result path.

## Open follow-ups requiring runtime measurement

Static analysis cannot answer:

- **Cache-line residency.** Whether each shard's cache line stays
  resident in the owning worker's L1 across the full scope, or whether
  rayon work-stealing rotates workers often enough to thrash. Tool:
  `perf c2c record / report` on the bench at W=16 / 32, or cachegrind.
- **Allocator pressure from `Vec::push` reallocs.** Tool: `dhat` on a
  100K-item drain. Mitigation if confirmed:
  `Vec::with_capacity(item_count / num_shards)` at `drain.rs:64`,
  which requires the dispatcher to know `item_count` (today the
  bounded queue gives a capacity, not a length; a hint parameter could
  be added).
- **Foreign-thread `ThreadId` hash cost** (`drain.rs:74-80`). Tool:
  `perf annotate` on the worker closure called from outside the rayon
  pool. Likely irrelevant in production (the production caller is the
  rayon pool via `DeltaConsumer`), but worth confirming.
- **`crossbeam_channel` internal segment churn for
  `drain_parallel_into`.** Tool: count crossbeam allocator events via
  dhat under a 100K / 1M item drain at W=16; compare against the shard
  variant to validate the design-C "no extra cost" claim.
- **NUMA behaviour past one socket.** Tool: bind the bench via
  `numactl --cpunodebind=0`, compare against unbound. Single socket
  isolates contention; cross-socket blends it with NUMA traffic.

Once the data lands, this audit can be replaced with a quantitative
followup or, if design B's predicted shape holds, closed as confirmed.

## References

- `crates/engine/src/concurrent_delta/work_queue/drain.rs:14-89,136-155`
  - shard and streaming `drain_parallel` implementations.
- `crates/engine/src/concurrent_delta/work_queue/{bounded.rs:48-104,
  capacity.rs:8-76, iter.rs:12-35, mod.rs:1-110}` - SPMC types,
  capacity policy, iterator, module ordering contract.
- `crates/engine/src/concurrent_delta/consumer.rs:97-194` -
  `DeltaConsumer::spawn`, production caller of `drain_parallel_into`.
- `crates/engine/src/concurrent_delta/reorder.rs:30-83` -
  `ReorderBuffer` ring-buffer design and capacity bound.
- `crates/transfer/src/delta_pipeline.rs:146-258` -
  `ParallelDeltaPipeline` integration site.
- `crates/engine/benches/{drain_parallel_benchmark.rs:1-89,
  reorder_buffer_scaling.rs:1-40, buffer_pool_contention.rs:1-30}` -
  bench harness, scaling reference, format reference for #1679.
- Format reference: `docs/audits/profiling-100k-files.md`,
  `docs/audits/mutex-implementation-policy.md`.
