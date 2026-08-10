# ReorderBuffer cache-behavior static audit

Task: #1854. Branch: `docs/reorderbuffer-cache-behavior-1854`.

## Summary

This audit predicts the cache-line residency, branch-prediction, and
working-set behavior of the receiver-side `BoundedReorderBuffer<T>` at
file-list scales of 100K, 1M, and 10M entries. It is a static read of
the data structure shape and access pattern only - no `perf`, `cachegrind`,
or microbenchmark was executed. Every prediction is paired with the perf
counter that must confirm or refute it during the runtime half of #1854.

The headline finding: at the default window size of 64 the buffer's
working set is dominated by `T` payloads, not by `BTreeMap` node spine.
The hot loop's cache footprint is bounded by `64 * sizeof(T)` plus a
single `BTreeMap` interior node, regardless of file-list length, because
the window slides forward and the map empties between contiguous drains.
The `O(log n)` cost of `BTreeMap::insert` is therefore `O(log W)` with
`W = window_size`, not `O(log N)` with `N = file_count`. A `VecDeque`
or pre-allocated ring (the design used by `engine::concurrent_delta::ReorderBuffer`,
shipped under #1734) is asymptotically faster but the constant-factor
gap at `W = 64` is small and likely below the noise floor of file-level
delta apply work.

## Methodology

### What this document infers from code

- Data-structure shape from the `BTreeMap<u64, T>` field declaration at
  `crates/transfer/src/reorder_buffer.rs:63` and the `pending` access
  pattern in `insert` (line 144) and `drain_consecutive` (line 149).
- Window invariant from the type-level comment at
  `crates/transfer/src/reorder_buffer.rs:34-37` and the runtime check at
  `crates/transfer/src/reorder_buffer.rs:131-142`.
- Default window size from
  `crates/transfer/src/reorder_buffer.rs:26` (`DEFAULT_WINDOW_SIZE = 64`).
- Capacity sizing in the parallel pipeline from
  `crates/transfer/src/delta_pipeline.rs:209-219`
  (`worker_count.saturating_mul(2).max(2)`).
- Promotion threshold from
  `crates/transfer/src/delta_pipeline.rs:42`
  (`DEFAULT_PARALLEL_THRESHOLD = 64`).
- The ring-buffer alternative from
  `crates/engine/src/concurrent_delta/reorder.rs:65-83` (slot array,
  head index, count, capacity, high-water mark, optional adaptive policy).
- The metrics-counter design from PR #3703 (`docs/design/reorderbuffer-metrics-and-bypass.md`),
  cited verbatim where its instrumentation hooks land in the same lines
  this audit discusses.

### What this document does NOT infer

- Absolute cycle counts. The audit gives qualitative predictions (L1
  hit, likely L2 miss, likely L3 miss) and the ratios between them; the
  runtime side of #1854 must capture the absolute numbers.
- Concrete `T` sizes for arbitrary callers. The receiver currently
  parametrises the buffer with `DeltaResult` from
  `crates/engine/src/concurrent_delta/types.rs:310-327` (six `u64`-class
  fields plus a `DeltaResultStatus` enum carrying an optional `String`),
  but third-party callers can plug in any `T`. The cache analysis below
  is parameterised on `sizeof(T)`.
- Branch-mispredict rates from compiler output. Predicted mispredict
  hot spots are derived from the source-level control flow alone; the
  Rust standard library `BTreeMap` codegen on a given target may be
  better or worse than the rough model used here.

### Open question label convention

When a prediction cannot be reduced to a code-level fact, it is tagged
`OPEN` and folded into the open-question list at the end. The runtime
side of #1854 closes each `OPEN` with a measured number.

## The hot loop

The receiver's parallel mode dispatches each file's delta work through
`ParallelDeltaPipeline::submit_work`
(`crates/transfer/src/delta_pipeline.rs:223-234`). Workers complete in
arbitrary order. The `engine::concurrent_delta::DeltaConsumer` (cited in
the design doc at `crates/transfer/src/delta_pipeline.rs:152-153`) feeds
results into its internal reorder buffer and serves them in submission
order through an `mpsc` channel.

The `BoundedReorderBuffer<T>` covered by this audit is the receiver-side
companion at `crates/transfer/src/reorder_buffer.rs:55-64`. Each
sequence number maps to exactly one file. The hot loop is therefore:

1. `insert(seq, item)` (`reorder_buffer.rs:129-146`):
   - Stale-sequence early-out at line 131 (`seq < next_expected`).
   - Window-end overflow check at lines 135-142.
   - `BTreeMap::insert` at line 144 - the only allocator-touching call
     under steady-state pressure.
   - `drain_consecutive` invocation at line 145 - amortised O(k) where
     k is the length of the contiguous run unblocked by this insert.

2. `drain_consecutive` (`reorder_buffer.rs:149-156`):
   - `pending.remove(&self.next_expected)` repeated until the head slot
     is empty.
   - Each successful `remove` does a `BTreeMap` lookup (O(log W)),
     deletes the node, and possibly rebalances.

These two functions are the entire hot path. Everything else is
constant-time accessor logic
(`reorder_buffer.rs:160-189`: `next_expected`, `buffered_count`,
`window_remaining`, `window_size`, `is_empty`).

## Data-structure shape

### `BTreeMap<u64, T>` internals

The Rust standard library's `BTreeMap` is a B-tree with branch factor
B = 6 (each interior node holds up to 11 keys and 12 child pointers,
each leaf holds up to 11 keys). At `pending.len() = n` the tree has
roughly `log_B(n)` levels.

For the window sizes this audit considers:

| `pending.len()` | Tree shape | Pointer-chase depth |
|-----------------|-----------|---------------------|
| 0..=11          | single leaf | 0 |
| 12..=143        | root + leaves | 1 |
| 144..=1727      | three levels | 2 |

At `DEFAULT_WINDOW_SIZE = 64` (`reorder_buffer.rs:26`) the buffer can
never reach a depth of 2: even saturated it occupies one root plus at
most six leaves. In the steady state where the receiver consumes the
contiguous drain on every insert, `pending.len()` oscillates between
1 and a small handful, leaving the structure as a single leaf node.

### Node layout

A B-tree leaf node in `std::collections::BTreeMap` is heap-allocated.
The exact layout is implementation-defined and not part of the public
API; reading `liballoc/collections/btree/node.rs` suggests roughly:

- A length tag (1-2 bytes plus alignment slack).
- 11 `u64` keys, contiguous (88 bytes).
- 11 `T` values, contiguous.
- A parent pointer plus parent-edge index.

For `T = DeltaResult` (currently around 56-72 bytes depending on
`DeltaResultStatus` discriminant and string slack -
`crates/engine/src/concurrent_delta/types.rs:310-348`), one full leaf
occupies roughly `88 + 11 * 64 + 16 = ~808` bytes, spilling into 13
cache lines on a 64-byte L1.

For an `Arc<...>` payload (the design doc at PR #3703 recommends this
pattern when the buffered item carries large state), one full leaf
occupies `88 + 11 * 8 + 16 = ~192` bytes - three cache lines.

### Expected residency at `W = 64`

With a single half-full leaf and one root, the buffer's footprint in
the steady state is:

- 1 root node (interior, smaller than a leaf because it carries only
  child pointers between siblings) - 1 to 2 cache lines.
- 1 to 2 leaf nodes - 3 to 26 cache lines depending on `T`.

That fits inside L1 (typically 32-64 KiB) on every modern CPU even at
`T = DeltaResult`, and inside L2 on every shipping x86_64 and aarch64
microarchitecture this project targets.

### Expected residency scales with `W`, not `N`

The crucial property: `N` (total file count) does not appear anywhere
in the cache analysis. The acceptance window invariant at
`reorder_buffer.rs:34-37` guarantees `pending.len() <= window_size`,
and the `BackpressureError` arm at lines 135-142 enforces it. As the
receiver advances, old entries leave the map and new ones arrive,
recycling the same handful of leaf nodes.

This is the dominant static finding of this audit: working set is
bounded by `W * sizeof(T) + O(log W) * sizeof(node)`, regardless of
file-list length.

## Working-set size estimates at 100K, 1M, 10M items

The window size determines the working set; the file count does not.
The table below assumes:

- `T = DeltaResult` with `sizeof(T) ~= 64` bytes (rounded to a cache
  line for the upper estimate).
- Steady-state `pending.len()` oscillates between 1 and `W`. For a
  worst-case prediction the table uses `pending.len() = W`.
- `BTreeMap` overhead estimated at `1.5 * W * 8` bytes for the
  per-node tags and tree-shape pointers - this is conservative; the
  actual overhead is closer to `W * 8` once leaves are nearly full.

| Files (`N`) | `W` (default 64) | Saturated working set | Cache tier |
|-------------|------------------|-----------------------|------------|
| 100K | 64 | 64 * 64 + ~768 = ~4.8 KiB | L1 |
| 1M | 64 | same: ~4.8 KiB | L1 |
| 10M | 64 | same: ~4.8 KiB | L1 |

The constant `~4.8 KiB` across all three scales is the audit's central
prediction. Increasing `N` increases throughput but does not push the
buffer out of L1.

If the operator raises `W` (no public knob today; see the open questions),
the table changes:

| `W` | Working set (`T = DeltaResult`) | Cache tier |
|-----|----------------------------------|------------|
| 64 | ~4.8 KiB | L1 |
| 256 | ~19 KiB | L1 on most x86_64, edge of L1 on aarch64 |
| 1024 | ~76 KiB | L2 |
| 4096 | ~300 KiB | L2 / L3 boundary |
| 16384 | ~1.2 MiB | L3 |

The runtime half of #1854 must confirm the L1-residency claim at
`W = 64` with `cachegrind --D1=32768,8,64` or `perf stat -e
L1-dcache-load-misses,LLC-loads,LLC-load-misses` during a 1M-file
run. See section "What perf should capture" for the exact invocation.

## Comparison with VecDeque + index alternative (#1853)

The investigation tracked under #1853 considered replacing the
`BTreeMap` with a `VecDeque<Option<T>>` indexed by
`(seq - next_expected)`. The trade-offs:

| Property | `BTreeMap` (current) | `VecDeque + index` |
|----------|----------------------|--------------------|
| `insert` complexity | O(log W) | O(1) |
| `drain` per-item complexity | O(log W) | O(1) |
| Memory per slot | 8 (key) + sizeof(T) + node tag | 1 (Option tag) + sizeof(T) |
| Slot pre-allocation | No - grows on demand | Yes - `W` slots from start |
| Cache locality | scattered across 1-2 leaves | contiguous backing array |
| Allocator pressure | one allocation per insert, one free per drain | zero after construction |
| Resilience to large `W` | leaf depth grows logarithmically | array stays flat |
| Empty-slot probing on drain | direct `remove(&head)` lookup | direct array indexing |
| Branch-prediction profile | `BTreeMap::insert` interior switch on node fullness | single `is_some()` predicate, predictable |

`VecDeque<Option<T>>` is asymptotically and constant-factor better on
every hot-path metric. The cost is `W * (1 + sizeof(T))` bytes
allocated up front instead of allocated lazily, plus the
`Option<T>` tag overhead. At `W = 64` and `T = DeltaResult` the
up-front cost is `~4.5 KiB`, identical to the saturated cost of the
`BTreeMap` form.

The `engine::concurrent_delta::ReorderBuffer` shipped under #1734
already chose this design - see the next section.

## The ring-buffer migration option (#1734 already shipped)

`crates/engine/src/concurrent_delta/reorder.rs` contains a
`ReorderBuffer<T>` that is the design endpoint of the migration #1734
proposed. Citing the live source:

- `reorder.rs:65-83` declares the struct: `slots: Box<[Option<T>]>`,
  `head: usize`, `next_expected: u64`, `count`, `capacity`,
  `high_water_offset`, optional `adaptive: Option<AdaptiveState>`.
- `reorder.rs:107-119` pre-allocates `capacity` slots in `new`.
- `reorder.rs:142-151` resolves `(sequence - next_expected)` to a slot
  index, returning `None` for stale or out-of-window sequences.
- `reorder.rs:167-185` performs the `O(1)` insert with adaptive grow.
- `reorder.rs:262-274` performs the `O(1)` drain via
  `slots[head].take()` followed by head increment modulo capacity.
- `reorder.rs:281-283` exposes a `drain_ready` iterator that wraps
  repeated `next_in_order` calls.

This is a pre-allocated ring buffer with adaptive capacity grow /
shrink (`reorder.rs:215-255`). It is the asymptotic winner over the
`BTreeMap` form on every metric this audit considers.

### Why two reorder buffers exist today

The two implementations cover different layers of the receiver pipeline:

- `engine::concurrent_delta::ReorderBuffer` lives inside
  `DeltaConsumer`, between the worker pool and the consumer thread, and
  serves the consumer thread's internal reordering. Reference at
  `crates/transfer/src/delta_pipeline.rs:152-153`.
- `transfer::reorder_buffer::BoundedReorderBuffer` is the receiver-loop
  level abstraction surfaced for any caller that needs sequence-based
  reordering with explicit backpressure. The design doc at PR #3703
  pins its instrumentation surface (Part A of that doc, lines 63-end of
  the metrics section).

The two are not merged because they have different ergonomic surfaces.
`ReorderBuffer` returns `()` on success and offers a `drain_ready`
iterator; `BoundedReorderBuffer::insert` returns the freshly-drained
contiguous run as a `Vec<T>` and surfaces backpressure as a typed
`BackpressureError`. PR #3703 is treating the two as separate concerns
and instruments only the `BoundedReorderBuffer`.

A consolidation that exposes the ring-based `ReorderBuffer` through a
`BoundedReorderBuffer`-shaped facade would unify the two without a
breaking change. That is the migration #1854 should evaluate, but it is
gated on the metrics from PR #3703 confirming that the BTreeMap form is
the bottleneck. See "Decision criteria" below.

## Predicted dominant cost

Under the L1-resident steady state at `W = 64`, this audit predicts the
dominant cost is split between two sites, and the runtime half must
confirm which dominates.

### Insert path (predicted: 60-70% of buffer-attributable cycles)

`BTreeMap::insert` walks the tree from root to leaf, doing one cache
line load per level. With `pending.len() <= 64` the tree has at most
two levels, so the load chain is short. The likely costs:

- One L1 load for the root node (predictable - same node every call).
- One L1 load for the target leaf when the buffer is warm.
- Allocator metadata access on first inserts after a drain that emptied
  a leaf - this is the only path that should miss into L2.

If `T` is large (`sizeof(T) > 32`) the value-copy on insert is the
single biggest contributor and is mostly L1 store traffic, not load.

`OPEN-1`: cycle attribution between tree walk vs payload move.

### Drain path (predicted: 25-30% of buffer-attributable cycles)

`drain_consecutive` calls `BTreeMap::remove(&self.next_expected)` in a
tight loop (`reorder_buffer.rs:151-154`). Each `remove` is structurally
similar to an `insert`: a tree walk to the slot, a value move out, a
possible rebalance.

The branch-prediction picture: the `while let Some(item) = ...` at
line 151 is a do-until-empty loop. The branch predictor should learn
"taken on every iteration except the last" within a handful of drain
calls. Mispredict rate per drain is bounded by 1 mispredict per call,
or `~1/k` mispredicts per item on a contiguous run of length `k`. For
the parallel pipeline with worker_count = 8 (typical default), runs
average 1-2 items, so the mispredict-per-item rate is roughly
50-100%. This is the audit's predicted "branch mispredict on drain"
finding.

`OPEN-2`: measured branch-mispredict rate on `drain_consecutive` loop
exit on a real 1M-file run.

### L2/L3 misses on insert

Predicted to be rare at `W = 64`. The active leaf and root are L1
resident. The only path that should miss is the first insert after a
prolonged stall (the buffer's stall-count counter from PR #3703 is the
gating signal): the leaf may have been evicted by interleaved hot data
from other receiver work.

`OPEN-3`: measured `L1-dcache-load-misses` per insert when
`peak_depth > 32` vs `peak_depth < 4`.

### Allocator traffic

`BTreeMap::insert` allocates only on node split. With `W = 64` and
amortised drain, splits occur at most every 11 inserts (one per leaf
fill). `BTreeMap::remove` deallocates only when a leaf empties.
Predicted: 1 alloc and 1 dealloc per ~6 file completions in the steady
state. This is significant if the global allocator is slow (jemalloc
or system glibc malloc both handle 800-byte allocations from a tcache
fast path, so the cost is bounded).

`OPEN-4`: `perf record -e syscalls:sys_enter_brk` to confirm no
brk / mmap traffic during steady state.

## What perf / cachegrind / `perf c2c` should capture

The runtime half of #1854 must run all of the following on a
controlled 1M-file transfer with `W = 64` (default) and at least one
larger window for comparison.

### Cachegrind for cache-line residency

```
valgrind --tool=cachegrind \
         --D1=32768,8,64 \
         --LL=8388608,16,64 \
         --cachegrind-out-file=cg.out \
         oc-rsync ...
```

Then `cg_annotate cg.out crates/transfer/src/reorder_buffer.rs` to get
per-line D1mr and DLmr counts. Predicted output: D1mr concentrated on
`pending.insert` (line 144) and `pending.remove` (line 151), with
DLmr essentially zero except during the first hundred files (cold).

### `perf stat` for top-line ratios

```
perf stat -e cycles,instructions,\
              L1-dcache-loads,L1-dcache-load-misses,\
              LLC-loads,LLC-load-misses,\
              branches,branch-misses \
          oc-rsync ...
```

Compare two runs: BTreeMap form (today) and a feature-flagged
ring-buffer port. Predicted: the ring-buffer form lands at <=50% of
`L1-dcache-load-misses` per file and roughly equal `branch-misses`,
since the drain loop's predictability is similar in both forms.

### `perf c2c` for cache-line contention

The receiver thread owns the `BoundedReorderBuffer` exclusively, so
`perf c2c` should show zero HITM events on the buffer's address range.
If non-zero, that means a debug log read or metrics snapshot is racing
the hot path - flag and fix.

```
perf c2c record -F 99 -- oc-rsync ...
perf c2c report --call-graph none
```

### `perf record` plus FlameGraph for hot-line attribution

```
perf record -F 999 --call-graph=fp -g -- oc-rsync ...
perf script | stackcollapse-perf.pl | flamegraph.pl > rb.svg
```

Predicted attribution: roughly 0.5-2% of receiver-side cycles spent
inside `BoundedReorderBuffer` at `W = 64` with `worker_count = 8`. If
substantially higher, the bypass option from PR #3703 Part B becomes
the lead optimisation.

`OPEN-5`: actual receiver-cycle share spent inside
`BoundedReorderBuffer` at 1M files.

## Decision criteria for ring-buffer migration

The migration to the `engine::concurrent_delta::ReorderBuffer`-shaped
ring is justified if and only if the runtime half of #1854 shows one
or more of:

- `L1-dcache-load-misses` attributable to `pending.insert` or
  `pending.remove` exceed `2.5 * insert_count + 2.5 * drain_count` -
  i.e., more than two full cache lines of miss traffic per buffer
  operation. The ring form's contiguous backing array reduces this by
  the leaf-spread factor (predicted: leaf depth 1 collapses to depth 0).
- Aggregate receiver-cycle share spent inside `BoundedReorderBuffer`
  exceeds 5% on a 1M-file run with default `W`. Below that, the
  migration is not justified by perf alone and should land only if PR
  #3703 Part B (bypass) is also adopted, since both require the same
  test surface refresh.
- Branch-mispredict rate on `drain_consecutive` exceeds 30% per
  iteration. The ring form's `slots[self.head].take()?` is a single
  predictable branch; the BTreeMap's `BTreeMap::remove` walks a
  branchier tree.
- A stall episode (per the metrics in PR #3703) shows
  `peak_depth > 256`. Above that, the BTreeMap depth grows beyond a
  single leaf and the L1-residency claim of this audit weakens.

If none of those four triggers fires, the BTreeMap form is good
enough and the migration should be deferred. The ring form's value is
its constant-factor floor; if the BTreeMap is already on that floor,
swapping yields nothing.

## Open questions for the runtime side

- `OPEN-1`. Cycle attribution between tree walk and payload move on
  `BTreeMap::insert`. Resolves with `perf record -e cycles` plus
  source-line annotation on `reorder_buffer.rs:144`.

- `OPEN-2`. Branch-mispredict rate on the `drain_consecutive` loop
  exit (`reorder_buffer.rs:151-154`) over a 1M-file run. Resolves
  with `perf stat -e branch-misses` filtered to the function's
  address range.

- `OPEN-3`. `L1-dcache-load-misses` per insert as a function of
  `peak_depth`. Resolves with cachegrind run repeated at
  `peak_depth` 1, 8, 32, 64, 256, 1024.

- `OPEN-4`. Allocator traffic frequency under steady state. Resolves
  with `perf record -e syscalls:sys_enter_brk -e
  syscalls:sys_enter_mmap` plus `bpftrace` on `malloc` / `free`.

- `OPEN-5`. Aggregate receiver-cycle share spent inside
  `BoundedReorderBuffer`. Resolves with FlameGraph plus quantitative
  flame area measurement.

- `OPEN-6`. Effective `W` distribution in real workloads. Today the
  default is hard-wired to 64 at `reorder_buffer.rs:26`. The runtime
  side should record observed `peak_depth` from the PR #3703 metrics
  on a representative mix (small-files, large-files, slow-disk, fast-
  disk) and confirm that 64 is the correct default. If `peak_depth`
  routinely saturates at 64, the window is too small and the parallel
  pipeline is throughput-limited, not cache-limited.

- `OPEN-7`. Whether to extend the ring-based
  `engine::concurrent_delta::ReorderBuffer` to expose a
  `BoundedReorderBuffer`-shaped public facade so consolidating the
  two implementations costs zero source diff at the call site. The
  answer depends on whether PR #3703 Part B (bypass) lands first; if
  it does, the bypass path eliminates most of the call sites that
  would benefit from the consolidation.

- `OPEN-8`. Whether the parallel pipeline's promotion threshold of 64
  (`crates/transfer/src/delta_pipeline.rs:42`) and the bounded reorder
  window of 64 (`reorder_buffer.rs:26`) should track each other or
  diverge. They are independent constants today but conceptually
  related: the threshold is the file count above which parallel mode
  starts; the window is the in-flight count above which backpressure
  fires. Resolves with a sweep at the runtime side.

## Cross-references

- `crates/transfer/src/reorder_buffer.rs` - subject of the audit.
- `crates/transfer/src/delta_pipeline.rs` - consumer side.
- `crates/transfer/src/config/mod.rs` - server config; no direct
  reorder-window field today, the default lives in
  `reorder_buffer.rs:26`.
- `crates/engine/src/concurrent_delta/reorder.rs` - the ring-based
  alternative shipped under #1734.
- `crates/engine/src/concurrent_delta/types.rs:310-348` - `DeltaResult`
  payload type, governs `sizeof(T)`.
- `docs/design/reorderbuffer-metrics-and-bypass.md` (PR #3703) -
  metrics counter design and bypass proposal; this audit's runtime-side
  measurements should reuse the metric definitions from Part A of
  that doc.
- `docs/architecture/reorder-buffer.md` - prior head-of-line semantics
  note (#1883), referenced by PR #3703 source citations.
