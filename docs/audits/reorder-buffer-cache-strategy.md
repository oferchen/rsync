# ReorderBuffer cache profiling strategy

Task: #1854. Branch: `docs/reorder-buffer-cache-1854`.

Companion to the static cache-behavior audit at
`docs/audits/reorderbuffer-cache-behavior-static.md` (PR for #1854,
commit `b7c714bac`). That document predicts cache residency from data
shape; this document defines the *measurement plan* the runtime side of
#1854 must execute against the 10K / 100K / 1M / 10M scaling benchmark
already shipped in `crates/engine/benches/reorder_buffer_scaling.rs`
(PR #3419 - referenced as the "10M-scale bench" tracked under #3780).

This document does **not** run `perf`, `cachegrind`, `vtune`, or any
microbenchmark. Project rule "never run cargo locally" applies. The
output is the strategy: which counters, which insertion patterns, which
buffer / payload geometry, which decision thresholds. The runtime owner
plugs the strategy into the perf harness and records the numbers.

## 1. In-scope implementations

The codebase ships two reorder buffers. Both are exercised in
production. The cache audit must cover both, because the receiver
pipeline can route through either depending on protocol path.

| Implementation | File | Backing store | Used by |
|---|---|---|---|
| `engine::concurrent_delta::ReorderBuffer<T>` | `crates/engine/src/concurrent_delta/reorder.rs:65-83` | `Box<[Option<T>]>` ring + head index | `concurrent_delta::DeltaConsumer` (`crates/engine/src/concurrent_delta/consumer.rs:146-189`) |
| `transfer::reorder_buffer::BoundedReorderBuffer<T>` | `crates/transfer/src/reorder_buffer.rs:55-64` | `BTreeMap<u64, T>` + `next_expected`, `window_size` | sliding-window transfer pipeline, doc-test at `crates/transfer/src/reorder_buffer.rs:38-54` |

The scaling bench compares both shapes:
`crates/engine/benches/reorder_buffer_scaling.rs:44-80` runs `run_ring`
against `run_btreemap` over identical insertion orders at 10K, 100K,
1M, and 10M (10M gated behind `BENCH_REORDER_10M=1` at line 132).
That bench is the input. This strategy adds the cache-counter overlay
on top of the same harness without modifying its workload.

## 2. Cache-line theoretical analysis

### 2.1 `ReorderBuffer<T>` (engine, ring-backed)

Field declaration order at
`crates/engine/src/concurrent_delta/reorder.rs:65-83`:

```text
slots:              Box<[Option<T>]>     16 B (ptr+len)
head:               usize                 8 B
next_expected:      u64                   8 B
count:              usize                 8 B
capacity:           usize                 8 B
high_water_offset:  usize                 8 B
adaptive:           Option<AdaptiveState> 8 B (None) or 8 B + heap state
                                        ----
                                         64 B header
```

The header fits in a single 64-byte cache line on x86_64, aarch64
(Apple M1/M2: 128 B), and Linux musl on x86_64. On Apple Silicon the
128 B line means a second worker dirtying any field in the same line
would still hit the same line as the consumer; that does not happen
because the buffer is owned by exactly one thread (see Section 2.3).

Hot fields per `insert` and `next_in_order` (read-modify-write,
identified at `reorder.rs:167-185` and `reorder.rs:262-274`):

- `head` - read on every insert (slot index) and write on every
  drain.
- `next_expected` - read on every insert (offset compute) and write
  on every drain.
- `count` - write on every insert with a new slot and on every drain.
- `capacity` - read on every insert (modulo and bound check); written
  only by `grow` / `resize_to`.
- `high_water_offset` - read+conditional-write on every insert,
  saturating-decrement on every drain.

All hot fields share one cache line by construction. The ring buffer
pointer (`slots[0]`) is one indirection away in a separate allocation
of `capacity * sizeof(Option<T>)` bytes. For `T = DeltaResult`
(`crates/engine/src/concurrent_delta/types.rs:309-327`,
six 8-byte fields plus `DeltaResultStatus` carrying an `Option<String>`)
`sizeof(Option<DeltaResult>)` is roughly 64-72 bytes after Rust's
niche optimization. At capacity 1024 (the bench picks this at
`reorder_buffer_scaling.rs:95`) the slot array consumes ~64 KB,
spilling out of L1 (32 KB typical) but fitting in L2 (256 KB-1 MB).

### 2.2 `BoundedReorderBuffer<T>` (transfer, BTreeMap-backed)

Field declaration order at `crates/transfer/src/reorder_buffer.rs:55-64`:

```text
next_expected:  u64           8 B
window_size:    u64           8 B
pending:        BTreeMap<u64,T>  ~32 B (root ptr + length + cached)
                              ----
                               48 B header
```

Header fits in one 64-byte line. Hot fields per `insert` (lines
129-146): `next_expected` read + occasional write, `window_size` read
only, `pending` mutated. `BTreeMap`'s root node and its B = 6 internal
nodes are heap-allocated and not laid out adjacent to the buffer
header. Each `BTreeMap::insert` chases pointers between unrelated
allocations, with cache misses scaling with the in-window resident
count - capped at `window_size = 64` (default at line 26) but variable
in the parallel pipeline where capacity is `worker_count * 2`
(`crates/transfer/src/delta_pipeline.rs:209-219`).

The static audit at
`docs/audits/reorderbuffer-cache-behavior-static.md:108-175`
already covered the BTree node arithmetic. This strategy reuses those
predictions - the new layer is the runtime confirmation.

### 2.3 False-sharing risk

Per `concurrent_delta::DeltaConsumer::start`
(`crates/engine/src/concurrent_delta/consumer.rs:138-188`):

- The `delta-reorder` thread (line 146-148) is the *only* mutator of
  the `ReorderBuffer`. It reads from a `crossbeam_channel::Receiver`
  fed by the `delta-drain` thread (line 138-143) and writes to an
  `mpsc::Sender` consumed downstream.
- Neither buffer field nor any slot is shared with another thread.
- The drain thread writes into the channel (its own segmented queue),
  not into the buffer. The receiver thread reads from the channel
  and mutates the buffer in isolation.

False-sharing risk on the buffer header itself is therefore
**structurally zero** for the engine ring buffer. The relevant
contention surface is the crossbeam channel between the two threads,
not the buffer. The runtime cache audit must still confirm the
absence of MESI ping-pong on the buffer header line because:

1. The downstream `result_tx.send` (line 158, 172, 180) crosses a
   thread boundary, and `mpsc::Sender::send` may write a buffer-line
   neighbor inside its own slab depending on stdlib internals.
2. The same crate also exposes `BoundedReorderBuffer` to callers that
   may share it across threads via `Arc<Mutex<...>>`. If any such
   call site exists outside `crates/transfer/src/reorder_buffer.rs`,
   the false-sharing surface is non-zero. Strategy: ripgrep
   `Arc<Mutex<.*ReorderBuffer` and `Arc<.*BoundedReorderBuffer` over
   the workspace as a one-shot static check before profiling.

## 3. Insertion-pattern matrix

Three canonical patterns, all already realisable via the existing
bench. The strategy is to pick one perf-counter signature per pattern
and let the runtime side fill the values.

### 3.1 In-order (`seq[i] = i`)

Implementation: replace `shuffled_with_local_swaps` in the bench with
`(0..count as u64).collect()`. Add a sibling bench function
`run_ring_in_order` that calls `buf.insert(i, i)` followed by a
drain. The buffer is empty between every pair (`count == 0` after
each drain).

Predicted counter signature:

- L1 dcache hit rate: high (the header line is hot, the slot array
  walks linearly through prefetcher-friendly stride 1).
- LLC misses per insert: near zero.
- Branch mispredict rate on `slot_index` (`reorder.rs:142-151`): low.
  The `sequence < self.next_expected` check (line 143) is statically
  false; the `offset >= self.capacity` check (line 147) is statically
  false. Both branches predict the not-taken path consistently.
- IPC: high (>=2.0 typical on modern x86_64).

What the runtime measurement confirms: linear scaling of cycles with
`count` and a flat L1-miss curve.

### 3.2 Reverse order (`seq[i] = count - 1 - i`)

Implementation: existing tests at
`crates/engine/src/concurrent_delta/reorder.rs:683-690`
(`reverse_order_insertion`) demonstrate the pattern at small scale.
Add `run_ring_reverse` to the bench using
`(0..count).rev().collect()`.

Predicted counter signature:

- L1 dcache hit rate on the slot array: lower than in-order. With
  capacity 1024 the writes hit slots `count-1, count-2, ...` which
  walk backwards through the ring; the prefetcher mostly handles
  sequential descending stride on x86_64, less reliably on aarch64.
- LLC misses per insert: still near zero at capacity 1024 because
  the slot array fits in L2.
- Branch mispredict rate: low. `sequence < next_expected` stays false
  the whole run; `offset >= capacity` is false.
- Drain phase at the end: `drain_ready` walks the ring forward and
  hits cold cache lines for the descending writes. Expect a single
  burst of L1 fills at the drain.

What the runtime measurement confirms: similar throughput to in-order
once the prefetcher catches up; a measurable L1-miss bump only at
the trailing drain.

### 3.3 Random order (existing local-swap pattern)

Implementation: already in `reorder_buffer_scaling.rs:25-40`
(`shuffled_with_local_swaps`, window 16). This is the production-like
pattern - workers complete with small local reordering.

Predicted counter signature:

- L1 dcache hit rate: dominated by the slot array. With local window
  16 and `Option<DeltaResult>` ~64 B, every insert touches the same
  L1 line as the previous one or the line one ahead. High L1 hit
  rate.
- LLC misses: near zero at capacity 1024.
- Branch mispredict rate on `slot_index`: low for the same reason as
  in-order.
- Branch mispredict rate on the `is_err()` retry path
  (`reorder_buffer_scaling.rs:49-54`): zero at capacity 1024 because
  the local window never exceeds 16.

What the runtime measurement confirms: throughput within 5-10% of
in-order, validating the shipped O(1) per-item design.

### 3.4 Pathological random (full permutation, capacity-stressing)

Implementation: add a bench function that uses the LCG shuffle
already in tests at `reorder.rs:1000-1008`
(`stress_deterministic_random_order`) but at scaling sizes 10K /
100K / 1M / 10M with capacity = 1024 (smaller than the gap). The
buffer must `force_insert` repeatedly. This is the worst case the
production path can hit if a worker stalls.

Predicted counter signature:

- L1 dcache miss rate: high. The slot array is walked in a random
  order; at 10M items even a 1024-slot ring is hit randomly.
- LLC misses: high once the slot array spills L2 (capacity 4096+).
- Branch mispredict rate on `slot_index`: high. The
  `offset >= self.capacity` branch flips frequently as `force_insert`
  triggers `grow` calls (`reorder.rs:368-374`).
- Allocator traffic: high. Every `grow` reallocates the slot array.

What the runtime measurement confirms: the failure mode the adaptive
policy
(`crates/engine/src/concurrent_delta/adaptive.rs:40-159`) exists to
defend against. Numbers establish where the adaptive `growth_factor`
should be tuned.

## 4. Counter set per pattern

| Counter | Purpose | Tool |
|---|---|---|
| `cache-misses` | Total LLC miss count | `perf stat` |
| `L1-dcache-load-misses` | L1 load miss rate | `perf stat` |
| `L1-dcache-loads` | L1 load count (denominator) | `perf stat` |
| `LLC-load-misses` | LLC load miss count | `perf stat` |
| `branch-misses` | Branch mispredict count | `perf stat` |
| `branches` | Branch count (denominator) | `perf stat` |
| `instructions` | Retired instructions | `perf stat` |
| `cycles` | Reference cycles | `perf stat` |
| Per-line miss attribution | Which `reorder.rs` line the miss landed on | `perf record -e cache-misses` + `perf annotate` |
| Cache-line residency (D1, LL) | Working-set in cache | `valgrind --tool=cachegrind --cache-sim=yes` |
| L1/L2/L3 sim hit rate | Per-line cache hit prediction | `cg_annotate` |
| False-sharing detection | HITM events on shared lines | `perf c2c record` |
| Ratio: misses-per-insert | Normalised cost | derived (`cache-misses / Throughput::Elements`) |

The runtime side runs these counters under the four patterns from
Section 3 against both 1024-slot ring and `BoundedReorderBuffer`
window-64 configurations. Twelve measurement points plus the existing
throughput numbers from `reorder_buffer_scaling.rs`.

## 5. Decision criteria (no measurement, only thresholds)

The runtime side reports decisions against these thresholds. Each
threshold is the point past which the *static* prediction would
require an architectural revision.

| Observation | Action |
|---|---|
| In-order pattern shows L1-miss / insert > 0.10 at 10M | Header layout is unexpectedly bad. Re-verify the field order in `ReorderBuffer<T>` and consider `#[repr(C)]` to lock layout. |
| In-order pattern shows branch-miss / insert > 0.01 at 10M | Compiler missed a likely-not-taken hint. Add `#[cold]` to the `CapacityExceeded` early return or `core::hint::unlikely` to the rejection branches at `reorder.rs:144, 148`. |
| Random-order misses-per-insert exceed BTreeMap baseline by < 2x | The shipped ring-buffer migration (#1734) is at risk. Re-evaluate. |
| `force_insert` rate exceeds 1% of inserts under the bench's local-swap pattern | The default capacity in production callers (`consumer.rs:149`, parameterised by `reorder_capacity`) is too small. File a follow-up to raise the default. |
| `perf c2c` reports HITM on the buffer header line | The single-owner invariant from Section 2.3 is violated. Track down the second mutator. |
| Cachegrind reports D1 miss rate > 5x between in-order and random-order | The slot-array stride is fighting the prefetcher. Consider 64-byte alignment on `slots` (already implicit via `Box<[T]>` on most allocators). |

## 6. Recommended layout-tuning hypothesis

Stated as a hypothesis to test, not a code change to land. The runtime
half of #1854 either confirms or refutes each item.

### 6.1 Hot-field reordering (low confidence, low payoff)

The five hot fields in `ReorderBuffer<T>`
(`reorder.rs:67-79`: `slots`, `head`, `next_expected`, `count`,
`capacity`, `high_water_offset`) already share one 64-byte line by
default Rust layout. Reordering inside that line cannot cross a line
boundary; the predicted gain is zero. The runtime measurement should
confirm that reordering experiments yield no statistically significant
change in cycles per insert.

If the runtime measurement contradicts this prediction, the most
likely cause is `Option<AdaptiveState>` widening the header by 8-16
bytes when `Some`, pushing one of the hot fields into a second line.
Mitigation: extract the adaptive policy out into a
`Box<AdaptiveState>` so the header stays at 56 bytes. This is a 1-line
change at `reorder.rs:82`.

### 6.2 Slot array prefetch hint (medium confidence, medium payoff)

The slot array walk in `slot_index` at `reorder.rs:150` is
`(self.head + offset) % self.capacity`. The compiler emits a `mul +
shr` for the modulo when `capacity` is non-power-of-two and a `and`
when it is. Hypothesis: rounding `capacity` up to the next power of
two lets the modulo lower to a single AND and lets the prefetcher
walk the slot array with deterministic stride. The current bench
picks 1024 (already a power of two) but production
(`reorder_capacity` plumbed via `delta_pipeline.rs`) does not enforce
this. Cost: negligible memory waste (at worst 2x). Predicted gain:
2-5% on random-order at 10M.

### 6.3 `Option<T>` niche (low confidence, payload-dependent)

`Option<DeltaResult>` is `sizeof(DeltaResult) + tag` because
`DeltaResult` has no obvious niche (`bytes_written: u64` etc.). For
payloads that *do* have a niche (`Box`, `&T`, `NonZeroU64`),
`Option<T>` collapses the tag and the slot array shrinks. Hypothesis:
boxing `DeltaResult` as `Option<Box<DeltaResult>>` halves the slot
array size at the cost of one allocation per insert. Net cost depends
on allocator throughput vs cache-miss rate; almost certainly a *loss*
for the in-order and local-swap cases, possibly a *win* for
pathological random at 10M. The runtime side should test this
explicitly only if Section 6.2 fails to recover the gap.

### 6.4 Cache-line padding for `BoundedReorderBuffer<T>` (refute)

`BoundedReorderBuffer` has no shared mutable state across threads
(Section 2.3). Adding `crossbeam-utils::CachePadded` around any field
is unnecessary. The hypothesis exists only so the runtime side can
reject it explicitly. If `perf c2c` reports HITM on the buffer header
the team should look for a second mutator, not pad the struct.

### 6.5 `BTreeMap` arena (out of scope)

Replacing `BTreeMap<u64, T>` with an indexed slot array would mirror
the engine ring buffer design. That migration is tracked separately
under #1853 and #1734 (already shipped for the engine path). Do not
re-litigate it inside the cache audit.

## 7. Output artifacts

The runtime owner produces:

- `target/perf/reorder-cache/in-order-{10k,100k,1m,10m}.{stat,c2c,cg}.txt`
- `target/perf/reorder-cache/reverse-{10k,100k,1m,10m}.{stat,c2c,cg}.txt`
- `target/perf/reorder-cache/random-{10k,100k,1m,10m}.{stat,c2c,cg}.txt`
- `target/perf/reorder-cache/pathological-{10k,100k,1m,10m}.{stat,c2c,cg}.txt`
- A summary table mapping each threshold from Section 5 to its
  measured value, plus a verdict on each hypothesis from Section 6.

The summary table is appended to the static audit at
`docs/audits/reorderbuffer-cache-behavior-static.md` Section
"Open questions for the runtime side" so the two halves of #1854
land in one location.

## 8. Cross-references

- Static cache audit: `docs/audits/reorderbuffer-cache-behavior-static.md`
- Mutex contention static audit:
  `docs/audits/drain-parallel-contention-static-analysis.md`
- 100K-files runtime profile:
  `docs/audits/profiling-100k-files.md`
- ReorderBuffer metrics design: PR #3703
  (`docs/design/reorderbuffer-metrics-and-bypass.md` if shipped)
- Scaling bench: `crates/engine/benches/reorder_buffer_scaling.rs`
- Per-buffer bench: `crates/transfer/benches/reorder_buffer_benchmark.rs`
- Engine reorder source: `crates/engine/src/concurrent_delta/reorder.rs`
- Transfer reorder source: `crates/transfer/src/reorder_buffer.rs`
- Adaptive policy: `crates/engine/src/concurrent_delta/adaptive.rs`
- Consumer integration: `crates/engine/src/concurrent_delta/consumer.rs`
