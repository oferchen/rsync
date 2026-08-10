# ReorderBuffer cache-behavior profiling plan (1M files)

Tracking task: oc-rsync #1854. Branch: `docs/reorderbuffer-cache-1854`.

This is a benchmark plan. It does not run benches and does not change source.
It scopes a cachegrind/perf profile at 1M files for the `BoundedReorderBuffer`
in `crates/transfer/src/reorder_buffer.rs`, sets a hypothesis about the
B-tree's memory-access shape, names the alternative ring layout from the
prior #1853 investigation, and pins the metrics that decide migration.

Prior memory-cost work: `docs/audits/reorder-buffer-memory-100k.md` (#1564).
Prior layout sketch and ring-buffer evaluation: oc-rsync #1853.

## 1. Current implementation: BTreeMap-backed sliding window

`BoundedReorderBuffer<T>` is the in-process reorder stage that restores
sequential ordering after parallel delta dispatch. Its storage is a single
`BTreeMap<u64, T>` plus three scalar fields, sized at construction by a
window value (`DEFAULT_WINDOW_SIZE = 64`).

Source: `crates/transfer/src/reorder_buffer.rs`.

Struct definition and field set, lines 57-64:

```rust
pub struct BoundedReorderBuffer<T> {
    next_expected: u64,
    window_size: u64,
    pending: BTreeMap<u64, T>,
}
```

Hot paths exercised every insert:

- `insert` (lines 129-146) does one window-bounds check, one
  `BTreeMap::insert` (line 144), then calls `drain_consecutive`.
- `drain_consecutive` (lines 149-156) does a `BTreeMap::remove` per
  yielded item, advancing `next_expected` until the lookup misses.
- `buffered_count` and `window_remaining` (lines 166-177) read
  `BTreeMap::len` on every dispatch decision.

Head-of-line blocking is intrinsic to the sliding-window contract: when the
item at `next_expected` is missing, every later item piles up in `pending`
and the producer blocks on `BackpressureError` (lines 79-86, 136-142) until
the gap fills. Out-of-order items occupy `pending` for as long as the gap
persists, then drain in a tight `while let` loop (lines 151-154) once the
head fills.

In the parallel delta pipeline, the window is sized to `2 * num_threads` at
construction. At 1M files the buffer's sequence range spans `0..1_000_000`
but `pending.len()` is bounded by the window. The interesting question is
not capacity, it is the per-insert cache cost when `pending.len()` sits at
the window watermark for the entire run.

## 2. BTreeMap cache behavior

`std::collections::BTreeMap` in current stable Rust is a B-tree with
internal nodes that hold up to 11 key/value pairs (`B = 6`, so the per-node
fanout is up to 12 children for internal nodes and up to 11 KV slots for
leaves). Each node is a heap allocation; child pointers are heap pointers.

Implications at the steady-state window size used in the pipeline (window
`W = 2 * num_threads`, typically 16-128):

- For `W <= 11` the entire map is one leaf node. `insert` and `remove`
  amount to a binary search inside one cache line set plus one branch
  predictor hit on the leaf bound.
- For `W` in the range 12-132 the map has a root plus a small number of
  leaves; every `insert`/`remove` touches the root node, then chases one
  pointer to a leaf. Two cache lines warm, one pointer dereference.
- The keys are dense `u64` sequence numbers near `next_expected`. The
  binary search inside a node is short and predictable, but the leaf node
  containing the head key changes as the window slides, so the working
  set drifts through the heap rather than staying pinned.
- Allocation churn: `BTreeMap` does not reuse freed nodes between
  unrelated insert/remove cycles in any guaranteed way; freed leaves go
  back to the global allocator. At 1M inserts the allocator sees roughly
  `1M / 11 ~= 91K` leaf alloc/free pairs even when steady-state size is
  small, plus interior splits and merges.

The hypothesis driving this plan: `BTreeMap` is correct and fast enough at
window 64, but pointer-chasing per insert + per remove + allocator round
trips show up as L1d misses and ~~50 cycles of memory latency on a workload
where the algorithmic work is roughly 8 cycles. At 1M files the constant
factor is what we are measuring.

## 3. Proposed ring buffer alternative and prior #1853 work

The prior #1853 investigation evaluated a ring-buffer layout already in use
by the engine's parallel delta consumer
(`crates/engine/src/concurrent_delta/reorder.rs::ReorderBuffer<T>`). That
type stores `slots: Box<[Option<T>]>` indexed by
`(seq - next_expected) % capacity`, with `head`, `next_expected`, and
`count` scalars. `force_insert` is the deadlock-break escape hatch when a
late head would otherwise stall the entire pipeline.

The `transfer` crate's `BoundedReorderBuffer` could adopt the same layout:

```rust
struct RingReorderBuffer<T> {
    slots: Box<[Option<T>]>,    // length = window_size
    head: usize,                // ring index of next_expected
    next_expected: u64,
    count: usize,
}
```

Cache properties relative to `BTreeMap`:

- One contiguous heap allocation, sized once. Inserts and removes touch
  one slot. No interior pointer chasing.
- Key-to-slot mapping is `((seq - next_expected) as usize) % capacity`, a
  single subtraction and either a mask (when capacity is a power of two)
  or a divide. No comparisons, no branches on the path.
- Drain is a `while slots[head].is_some()` loop with stride-1 access. The
  prefetcher handles it; the same cache line typically holds 4 `Option<T>`
  for `T = 64-byte struct` configurations.
- Memory cost is fixed: `window_size * size_of::<Option<T>>()` bytes
  regardless of fill level. At window 64 with `T` 16-32 bytes this is
  trivial; the trade is paying for empty slots in exchange for layout
  density.

#1853 left two open questions that this profile addresses:

- Does the `BTreeMap` constant factor matter at the window sizes the
  pipeline actually uses, or only at pathological window sizes?
- Does the ring layout's `force_insert` requirement (no equivalent in the
  `BTreeMap` variant, which uses backpressure exclusively) introduce any
  regression in the steady state?

## 4. Bench plan

Goal: measure cache-miss rate and IPC for `BoundedReorderBuffer` insert +
drain at 1M total items, under three delivery patterns, on Linux with
cachegrind and perf. Build the existing `reorder_buffer_large_scale` and
`reorder_buffer_benchmark` targets in release with debug info, then drive
them under each tool.

### 4.1 Workloads

All three drive 1M sequence numbers `0..1_000_000` with `T = u64` so the
bench measures buffer overhead, not payload churn. Window sizes 16, 64, 256
to bracket the production setting.

- **In-order baseline.** Insert 0, 1, 2, ... in sequence. Each insert
  drains immediately. This is the cheapest path and provides the floor
  for IPC and miss rate.
- **Mostly-in-order with 5% gap.** Permute the sequence so 95% of items
  arrive in order and 5% arrive late by a uniform random offset in
  `1..window_size`. This matches the steady-state behavior observed on
  parallel delta dispatch where most threads finish near monotonically
  but a small tail straggles.
- **Pathological out-of-order.** Reverse order within each window-sized
  segment (insert `W-1, W-2, ..., 0, 2W-1, 2W-2, ..., W, ...`). Forces
  the buffer to hold `W-1` items before each drain.

### 4.2 Tools and commands

Run on a Linux host (the `rsync-profile` podman container has the toolchain;
cachegrind is in `valgrind`, perf is in `linux-perf`).

Cachegrind, simulated cache, deterministic but slow:

```sh
cargo build --release -p transfer --bench reorder_buffer_large_scale
valgrind --tool=cachegrind \
  --cache-sim=yes --branch-sim=yes \
  --cachegrind-out-file=cg.btreemap.<workload>.<window>.out \
  ./target/release/deps/reorder_buffer_large_scale-* \
  --bench --profile-time 30
cg_annotate cg.btreemap.<workload>.<window>.out > report.txt
```

Perf, real hardware counters, fast:

```sh
cargo build --release -p transfer --bench reorder_buffer_large_scale
perf stat -e \
  cycles,instructions,\
L1-dcache-loads,L1-dcache-load-misses,\
LLC-loads,LLC-load-misses,\
branch-instructions,branch-misses \
  ./target/release/deps/reorder_buffer_large_scale-* \
  --bench --profile-time 30
```

Run each (tool x workload x window) cell three times; report median.

### 4.3 Metrics

Primary:

- L1d miss rate (`L1-dcache-load-misses / L1-dcache-loads`).
- LLC miss rate (`LLC-load-misses / LLC-loads`).
- IPC (`instructions / cycles`).
- Cycles per insert (`cycles / 1_000_000`).

Secondary, for diagnosing where the cycles go:

- Branch misprediction rate.
- Cachegrind D1mr and DLmr per `BTreeMap::insert`/`remove` callsite.
- Allocator activity proxy: `perf stat -e syscalls:sys_enter_brk,page-faults`.

Record peak RSS via `/usr/bin/time -v` for sanity (the memory profile in
`reorder-buffer-memory-100k.md` already covers absolute footprint).

### 4.4 Comparison

Repeat the entire matrix against a prototype `RingReorderBuffer<T>` placed
behind a feature flag in `crates/transfer/src/reorder_buffer.rs`. Both
types must implement the same insert/drain contract so the bench harness
swaps them by type parameter, not by editing call sites.

Report the per-cell deltas as a table: workload, window, BTreeMap L1d miss
rate, ring L1d miss rate, BTreeMap IPC, ring IPC, ring/BTreeMap cycle ratio.

## 5. Decision criteria for migration

Migrate `BoundedReorderBuffer` to the ring layout if and only if:

- The mostly-in-order 5% gap workload at window 64 shows the ring at
  >= 1.5x IPC or >= 2x lower L1d miss rate, and end-to-end cycle ratio
  <= 0.7. This is the production workload; it must win.
- The pathological out-of-order workload at window 64 does not regress by
  more than 10% on either IPC or cycle count. Worst case must not get
  worse.
- No correctness regression in `crates/transfer/src/reorder_buffer.rs`
  unit tests, the `pipeline_reorder_integration` integration tests, or
  the `concurrent_delta::work_queue` tests.
- Memory footprint at window 64 stays within the bound documented in
  `reorder-buffer-memory-100k.md`.

Stay on `BTreeMap` if:

- The BTreeMap variant is within 20% of ring IPC across all workloads at
  window 64. The structural simplicity (no `force_insert` escape hatch,
  no power-of-two capacity rounding, no fixed allocation) is worth more
  than a small constant factor at this scale.
- The 1M run completes inside the existing throughput budget set by
  the `reorder_buffer_large_scale` bench's regression gate.

Either way, the decision lands in this document with the measured numbers
attached, and the resulting code change ships behind a follow-up task that
references #1854 in the commit body.
