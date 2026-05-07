# Parallel dispatch overhead profile plan (#1551)

Runtime profiling plan for the receiver-side parallel delta pipeline at 100K
files. Complements the static-analysis audit
`docs/audits/parallel-dispatch-overhead.md` by specifying the workload,
counters, and decision rules used to confirm or refute its predictions.

## 1. Current dispatch flow

`ReceiverDeltaPipeline` is the trait the receiver drives per file
(`crates/transfer/src/delta_pipeline.rs:56-119`). The parallel
implementation `ParallelDeltaPipeline::new(worker_count)` builds a bounded
work queue and a `DeltaConsumer` background consumer
(`crates/transfer/src/delta_pipeline.rs:209-219`):

1. `submit_work` stamps a sequence number and pushes via `WorkQueueSender`
   onto a `crossbeam_channel::bounded` ring sized at
   `worker_count * CAPACITY_MULTIPLIER`
   (`crates/engine/src/concurrent_delta/work_queue/bounded.rs:78-103`,
   `crates/engine/src/concurrent_delta/work_queue/capacity.rs:8`).
2. `DeltaConsumer::spawn` runs two threads
   (`crates/engine/src/concurrent_delta/consumer.rs:128-189`):
   `delta-drain` calls `WorkQueueReceiver::drain_parallel_into`, which
   spawns one `rayon::scope` task per item
   (`crates/engine/src/concurrent_delta/work_queue/drain.rs:136-155`);
   `delta-reorder` consumes the bounded stream channel, inserts results
   into `ReorderBuffer`, and forwards in-order runs over `mpsc`.
3. The `BoundedReorderBuffer<T>` variant in
   `crates/transfer/src/reorder_buffer.rs:100-156` uses
   `BTreeMap<u64, T>` insert/remove for window enforcement.
4. `ThresholdDeltaPipeline` keeps work sequential below
   `DEFAULT_PARALLEL_THRESHOLD = 64`
   (`crates/transfer/src/delta_pipeline.rs:42`).

## 2. Suspected costs

Three regions to profile, in order of suspicion:

- **Rayon thread spawn.** Pool init is `OnceLock`-amortised, but each
  `s.spawn` allocates a `HeapJob` and pushes onto the worker deque. At
  100K files that is 100K heap allocations and 100K CAS pushes. One-time
  cost of `rayon::ThreadPoolBuilder` is paid once per process and should
  not appear in steady-state samples.
- **Crossbeam channel send/recv.** Per file: one `send` + one `recv` on
  the work queue, plus one `Sender::clone` (`Arc::fetch_add`) inside
  `drain_parallel_into` and one `send`/`recv` on the bounded stream
  channel (`work_queue/drain.rs:144`, `consumer.rs:135`). Six CAS
  operations per item, plus park/unpark when the queue is full or empty.
- **`BoundedReorderBuffer` BTreeMap.** Default window 64. At 100K inserts
  the B-tree splits roughly 9000 nodes and rebalances on every drain.
  `BTreeMap::remove(&self.next_expected)` in `drain_consecutive`
  (`reorder_buffer.rs:149-156`) is O(log n) per drained item - small for
  n <= 64 but allocator-heavy.

## 3. Profile plan

Run inside the `rsync-profile` podman container (Debian, `perf` available):

```sh
podman exec -it rsync-profile bash -c '
  perf stat -e cycles,instructions,context-switches,cache-misses,branch-misses \
    -- oc-rsync-dev -a /workspace/bench/src/ /workspace/bench/dst/
  perf record -F 999 -g \
    -- oc-rsync-dev -a /workspace/bench/src/ /workspace/bench/dst/
  perf script | stackcollapse-perf.pl | flamegraph.pl > flame.svg
'
```

Capture three traces: `RAYON_NUM_THREADS=1` (sequential baseline),
`RAYON_NUM_THREADS=8` (parallel default), and
`RAYON_NUM_THREADS=8 OC_RSYNC_PARALLEL_THRESHOLD=0` to force the parallel
path on every file. Diff `perf stat` cycles and context-switches between
sequential and parallel; the delta is the dispatch tax.

Inspect the flame graph for these stack frames as a fraction of total
samples: `crossbeam_channel::flavors::array::Channel::send`,
`rayon_core::scope::ScopeBase::complete`,
`<alloc::collections::btree::map::BTreeMap as ...>::insert`,
`HeapJob::execute`, and `mpsc::Sender::send`. Anything above 5 percent
self-time on the dispatch path is a target for reduction.

## 4. Workload

100K x 1 KiB regular files in a flat directory tree, fresh destination
(no skip path), checksum disabled. Files contain unique content so the
delta pipeline runs `whole_file` dispatch per file. Generate via
`tools/bench/gen_100k.sh` (parallel `dd` from `/dev/urandom`). Repeat
each run three times, drop the first to warm the page cache, report
median wall-clock and `perf stat` counters.

Comparison runs:

- **sequential**: `RAYON_NUM_THREADS=1`, `OC_RSYNC_PARALLEL_THRESHOLD=0`.
- **parallel-default**: `RAYON_NUM_THREADS=8`, threshold 64 (the shipped
  default).
- **parallel-forced**: `RAYON_NUM_THREADS=8`,
  `OC_RSYNC_PARALLEL_THRESHOLD=0` (every file dispatched).

Hardware: rsync-profile container on the developer host. Pin to a single
NUMA node with `taskset -c 0-7` to remove cross-socket noise.

## 5. Decision

| Observation | Action |
|-------------|--------|
| parallel-default beats sequential by < 5% wall-clock | Raise threshold to 256, keep `CAPACITY_MULTIPLIER = 2`. |
| parallel-forced regresses sequential at 100K | Keep threshold at 64; flag dispatch overhead as the dominant cost. |
| `crossbeam` send/recv > 15% self-time in flame graph | Tune `CAPACITY_MULTIPLIER` to 4 or adopt per-worker channels (audit 4.3). |
| `BTreeMap::insert/remove` > 5% self-time | Replace `BoundedReorderBuffer<T>` with the slot-array `ReorderBuffer<T>` from `engine::concurrent_delta::reorder` (already O(1)). |
| HeapJob alloc > 10% self-time | Land thread-local accumulators (audit 4.1). |
| All four below threshold | Keep current implementation; close #1551 with a profile artifact. |

Profile artifacts (flame SVGs, `perf stat` text) attach to the issue;
this audit defines the methodology, not the numbers themselves.
