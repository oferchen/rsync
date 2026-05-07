# ReorderBuffer memory profile at 100K+ parallel results

Tracking issue: oc-rsync task #1564. Branch: `docs/reorder-buffer-memory-1564`.

## Scope

Quantify the in-memory cost of buffering up to 100K (and beyond) `DeltaResult`
items in oc-rsync's two reorder-buffer implementations, identify the operating
points where peak memory becomes a concern, document the status of the
spill-to-tempfile mitigation tracked in #1884, and sketch a benchmark fixture
that measures peak resident set size (RSS) on top of the existing 10M-item
throughput bench from #3780.

This audit focuses on memory; the head-of-line (HoL) blocking semantics that
make these buffers hold so many items in the first place are documented in
`docs/architecture/reorder-buffer.md`.

Source files inspected (all paths repository-relative):

- `crates/engine/src/concurrent_delta/reorder.rs` (ring-buffer
  `ReorderBuffer<T>`, fixed-capacity `Box<[Option<T>]>` storage,
  `force_insert` deadlock break, optional adaptive policy).
- `crates/engine/src/concurrent_delta/types.rs` (`DeltaResult`,
  `DeltaResultStatus`, `FileNdx`, the field set the buffer carries).
- `crates/engine/src/concurrent_delta/consumer.rs` (`DeltaConsumer::spawn`,
  the `delta-reorder` thread that owns the buffer and clones each
  `DeltaResult` once per insert).
- `crates/transfer/src/reorder_buffer.rs` (`BoundedReorderBuffer<T>`,
  `BTreeMap`-backed sliding-window variant with `BackpressureError`).
- `crates/transfer/src/delta_pipeline.rs` (`ParallelDeltaPipeline::new`,
  reorder-capacity sizing - currently `2 * num_threads`).
- `crates/engine/benches/reorder_buffer_scaling.rs` (engine ring-buffer
  scaling bench, 10K / 100K / 1M / 10M cases).
- `crates/transfer/benches/reorder_buffer_benchmark.rs`
  (`BoundedReorderBuffer` vs collect-then-sort, 10K / 100K cases).

## TL;DR

A single `DeltaResult` occupies 64 bytes for the success-path `struct` itself
(no heap fan-out) and rises to 88 bytes plus the inline reason string when
`DeltaResultStatus::NeedsRedo` or `Failed` carries a message. A 100K-item
buffer therefore costs roughly:

- **Engine ring buffer (`Box<[Option<DeltaResult>]>`)** - 7.6 MiB at 100K
  slots in the success path, 8.4 MiB plus reason-string heap once a fraction
  of the items carry redo/failure messages. Scales linearly to the slot
  count: 76 MiB at 1M, 760 MiB at 10M.
- **Bounded sliding-window buffer (`BTreeMap<u64, DeltaResult>`)** - the same
  payload bytes plus roughly 56-64 bytes per entry of `BTreeMap` node
  overhead, landing around 12 MiB at 100K entries; 1M entries cost about
  120 MiB.

`DeltaResult` does not hold any `Arc`. The basis path lives on the producer
side `DeltaWork` and is consumed before the result enters the reorder
buffer, so per-result memory is independent of basis-path length.

The peak-memory failure mode is the `force_insert` escape hatch in the engine
ring buffer (`reorder.rs:334`). With a stalled head and `N` already-completed
successors, the ring grows from `2 * num_threads` to whatever offset the
incoming sequence demands. Worst case `N - 1` successors (one slow head,
all others completed) - at 100K files this peaks near 7-8 MiB but rises with
file-list size linearly. Spill-to-tempfile (#1884) is **not implemented**:
the audit document `docs/architecture/reorder-buffer.md` explicitly notes
"there is no spill-to-disk", and the only memory-bound knob today is the
optional `AdaptiveCapacityPolicy` that bounds growth but does not page out.

The bench fixture sketch in Section 4 extends the existing
`reorder_buffer_scaling` (10K / 100K / 1M / 10M) and `reorder_buffer_large_scale`
(1M / 10M, gated on `BENCH_REORDER_10M=1`) targets with peak-RSS measurement
points - sampled inside the benchmark via `ru_maxrss` on Unix and
`PROCESS_MEMORY_COUNTERS::PeakWorkingSetSize` on Windows - so memory growth
under stalled heads is recorded alongside throughput.

## 1. Storage layout of the two implementations

Both buffers store `DeltaResult` payloads under `u64` sequence keys and yield
items strictly in `next_expected` order. They differ in the data structure
that holds the unyielded items.

### 1.1 `engine::concurrent_delta::ReorderBuffer<T>` (ring buffer)

`crates/engine/src/concurrent_delta/reorder.rs:65` defines the production
buffer used by the parallel delta pipeline. Storage:

```rust
pub struct ReorderBuffer<T> {
    slots: Box<[Option<T>]>,
    head: usize,
    next_expected: u64,
    count: usize,
    capacity: usize,
    high_water_offset: usize,
    adaptive: Option<AdaptiveState>,
}
```

The dominant term is `slots: Box<[Option<T>]>` - one heap allocation of
`capacity * size_of::<Option<DeltaResult>>()` bytes. For
`T = DeltaResult` (Section 2) this is `capacity * 80` bytes (the
`Option<DeltaResult>` discriminant rounds up to the next 8-byte alignment of
the inner struct, which is 64 + 16 padding = 80 bytes when the variant tag
sits in a separate slot).

There is no per-entry heap node, so memory usage is exactly the slot array
plus the inline payload of any `String` reasons that escape to the heap.

### 1.2 `transfer::reorder_buffer::BoundedReorderBuffer<T>` (BTreeMap)

`crates/transfer/src/reorder_buffer.rs:57` uses a `BTreeMap<u64, T>` with an
explicit acceptance window. Storage:

```rust
pub struct BoundedReorderBuffer<T> {
    next_expected: u64,
    window_size: u64,
    pending: BTreeMap<u64, T>,
}
```

`BTreeMap` is a B-tree of key-value pairs with internal nodes carrying up to
11 entries each (Rust's `BTREE_MIN_LEN_AFTER_SPLIT` and friends - cf.
`std::collections::btree_map`). Each entry incurs:

- `(u64, DeltaResult)` payload: 8 + 64 = 72 bytes (success path) or
  8 + 88 = 96 bytes (`NeedsRedo`/`Failed` path) plus reason string heap.
- B-tree node overhead: amortized 56-64 bytes per entry across leaf pointers,
  edge slots, and length tags.

So `BoundedReorderBuffer` carries roughly 130-160 bytes per buffered item
versus the ring buffer's 80 bytes per slot. The compensating advantage is
that it only allocates for present entries, not for the entire window.

### 1.3 Capacity sizing today

`ParallelDeltaPipeline::new(worker_count)` passes
`worker_count.saturating_mul(2).max(2)` as both the work-queue capacity and
the reorder capacity (`crates/transfer/src/delta_pipeline.rs:209-212`). On a
16-thread machine the ring buffer is therefore 32 slots by default, and
peak in-flight memory is roughly 32 * 80 = 2.5 KiB - trivial compared to the
file-list itself.

`BoundedReorderBuffer::DEFAULT_WINDOW_SIZE` is 64
(`crates/transfer/src/reorder_buffer.rs:26`). Same regime.

The "100K parallel results" framing of this audit therefore describes the
worst case after `force_insert` has fired (Section 3), not the steady-state
behaviour of the default config.

## 2. Per-entry memory cost

`DeltaResult` (`crates/engine/src/concurrent_delta/types.rs:309`) has the
following fields (all paths repository-relative):

```rust
pub struct DeltaResult {
    ndx: FileNdx,                  // u32 (4 bytes)
    sequence: u64,                 // 8 bytes
    bytes_written: u64,            // 8 bytes
    literal_bytes: u64,            // 8 bytes
    matched_bytes: u64,            // 8 bytes
    status: DeltaResultStatus,     // see below
}
```

`FileNdx` is `#[repr(transparent)] pub struct FileNdx(u32);`
(`types.rs:23-25`), so it occupies exactly 4 bytes with no padding of its
own. The struct enforces 8-byte alignment because of the `u64` fields, so
the `u32` slot grows to 8 bytes via tail padding for the `ndx + sequence`
pair.

`DeltaResultStatus` (`types.rs:331`) is:

```rust
pub enum DeltaResultStatus {
    Success,
    NeedsRedo { reason: String },
    Failed   { reason: String },
}
```

`String` is `(ptr, len, cap) = 24 bytes` on 64-bit targets. The enum's
payload is at most one `String`, plus a discriminant tag rounded to 8-byte
alignment. So:

- Success path: 1-byte tag + 7 bytes padding = 8 bytes (no heap).
- `NeedsRedo`/`Failed`: 1-byte tag + 7 bytes padding + 24-byte `String` =
  32 bytes inline + the heap allocation backing the reason string (typically
  16-128 bytes).

Putting it together (success path):

| Field           | Size (bytes) |
|-----------------|--------------|
| `ndx`           | 4 (+4 pad)   |
| `sequence`      | 8            |
| `bytes_written` | 8            |
| `literal_bytes` | 8            |
| `matched_bytes` | 8            |
| `status`        | 8 (Success tag + pad) |
| `Default::default` derive | (no extra fields) |
| **Total inline** | **48**      |

The struct is `#[derive(Debug, Clone, Default)]` and the layout above is
without any padding required between fields - 5 x `u64` plus the `ndx + pad`
slot plus an 8-byte status discriminant = 48 bytes.

For `NeedsRedo`/`Failed` the inline footprint becomes 48 + 24 = 72 bytes
plus reason-string heap.

`Option<DeltaResult>` adds another 8 bytes of niche-or-tag-plus-padding, so
the slot in the ring buffer is 56 bytes for success and 80 bytes for redo/
failed. (Rust's niche optimisation does not apply here because the inner
struct has no obvious niche; the discriminant therefore consumes a full
8 bytes after alignment.) The audit picks the conservative 64- and 80-byte
upper bounds for the totals in Section 3 to keep the analysis insensitive to
compiler-rustc-version layout shifts.

There is **no `Arc<...>` in `DeltaResult`**. The producer-side
`DeltaWork::basis_path` (`Option<PathBuf>`) is consumed before the result
returns via `strategy::dispatch` in
`crates/engine/src/concurrent_delta/strategy.rs`. The reorder buffer never
holds a basis-path allocation. Per-entry cost is therefore independent of
file-tree depth or path length.

### 2.1 Clone amplification on insert

`DeltaConsumer::spawn` clones each result once during insert
(`consumer.rs:154,165`):

```rust
while reorder.insert(result.sequence(), result.clone()).is_err() { ... }
```

`DeltaResult: Clone` is derived, so the clone deep-copies the `String`
reason in the redo/failed paths. A 100K-buffer where every result carries a
24-byte reason allocates 100K x 24 bytes = 2.4 MiB of reason heap on top of
the slot array. The success path skips that work because `String::clone()`
of an empty `String` is cheap (no allocation).

This is a known clone-heavy spot - the surrounding loop also clones once
more before `force_insert` (line 165). Removing those two clones would halve
the reason-string allocator pressure but is out of scope for this audit.

## 3. Worst-case memory at 100K parallel results

The "100K parallel results in flight" scenario lands in one of three regimes,
each with different memory characteristics.

### 3.1 Steady-state (capacity = 2 * num_threads)

Default deployment on, say, 16 threads. Capacity is 32 slots, so 100K
results never coexist in the buffer. Memory is bounded at
`32 * 80 = 2560` bytes for the ring buffer or 32 * 130 ~ 4 KiB for the
`BoundedReorderBuffer` BTreeMap.

This is the common case. A 100K-file transfer streams through with at most
32 in-flight results.

### 3.2 Stalled head, force_insert escape hatch

Section 2.3 of `docs/architecture/reorder-buffer.md` documents that when the
ring fills with results all stuck behind a missing `next_expected`, the
consumer calls `force_insert` which grows the ring to fit the incoming
sequence (`reorder.rs:334-360`):

```rust
} else if sequence >= self.next_expected {
    let offset = (sequence - self.next_expected) as usize;
    let new_capacity = offset + 1;
    self.grow(new_capacity);
    ...
}
```

`grow` uses `min_capacity.max(self.capacity * 2)` for the non-adaptive path,
i.e. amortized doubling. With one stalled head and 99 999 already-completed
successors, the ring climbs to the next power-of-two >= 100 000:

| Slots (post-grow) | Slot bytes | Total ring memory |
|-------------------|------------|-------------------|
| 131 072           | 80         | 10.0 MiB          |
| 262 144           | 80         | 20.0 MiB          |
| 524 288           | 80         | 40.0 MiB          |
| 1 048 576         | 80         | 80.0 MiB          |

`resize_to` (`reorder.rs:379`) reallocates to the exact target rather than
the doubled value when called from the adaptive path, so the practical
ceiling for 100K stalled successors is 100 000 * 80 = 7.6 MiB before the
ring quiesces. The growth event itself momentarily holds two arrays
simultaneously: the old `Box<[Option<T>]>` and the new one, which adds a
short-lived spike of `~2 * capacity * 80` bytes.

For the 1M-file case (one of the bench points in
`reorder_buffer_scaling.rs:124`), the worst case is 76 MiB; for the 10M case
gated on `BENCH_REORDER_10M=1` it is 760 MiB. Both numbers are dominated by
the slot array; reason-string heap is negligible because the success path
has no allocation.

### 3.3 BoundedReorderBuffer at 100K

`BoundedReorderBuffer` does not have a `force_insert` path. When the window
fills (`pending.len() >= window_size` is implied; the actual check is
`seq >= next_expected + window_size` in `reorder_buffer.rs:135-142`), the
producer receives `BackpressureError` and must either drain or yield.

If the caller is given `window_size = 100_000` to deliberately tolerate a
100K stall, the BTreeMap would carry roughly:

- `100_000 * 72` bytes = 6.9 MiB of payload + key.
- B-tree node overhead at ~56-64 bytes per entry = 5.3-6.1 MiB.
- Total: 12-13 MiB.

That is roughly 1.6x the ring buffer at the same item count, but with the
advantage that the buffer only allocates per-present entry rather than
pre-allocating the full window.

### 3.4 Adaptive policy bounds

`AdaptiveCapacityPolicy` (`crates/engine/src/concurrent_delta/adaptive.rs`)
caps growth at `policy.max`. When attached via
`ReorderBuffer::with_adaptive_policy`, the buffer grows up to `max` slots
under sustained pressure and shrinks back to `min` once the gap closes.
`DeltaConsumer::spawn` does **not** attach an adaptive policy by default
(`consumer.rs:149`), so production deployments rely on the implicit doubling
in `grow` rather than a hard bound.

If we wanted a hard 100K cap on memory use, we could attach
`AdaptiveCapacityPolicy::new(min=32, max=100_000, grow_factor=2.0)` from
`DeltaConsumer::spawn`. The ceiling becomes 100K * 80 = 7.6 MiB and any
attempt to exceed it returns `CapacityExceeded` to the caller, which today
would route to `force_insert` and bypass the cap. Closing that loophole is a
separate change tracked in #1884 (Section 5).

### 3.5 Summary table

| Scenario | Capacity | Per-slot bytes | Per-entry overhead | Total memory |
|----------|---------:|---------------:|-------------------:|-------------:|
| Default, 16 threads, ring | 32 | 80 | - | 2.5 KiB |
| Default, 16 threads, BTreeMap | 64 | - | 130-160 | 8-10 KiB |
| Stalled head, 100K successors, ring | 100 000 | 80 | - | 7.6 MiB |
| Stalled head, 1M successors, ring | 1 048 576 | 80 | - | 80 MiB (allocator-rounded) |
| Stalled head, 10M successors, ring | 13 421 772 (next ^2 / 16M) | 80 | - | 1.0-1.3 GiB |
| Stalled head, 100K successors, BTreeMap | 100 000 | - | 130-160 | 12-13 MiB |
| Force-insert resize spike (transient) | 2 * capacity | 80 | - | doubles the steady-state line above for the duration of the copy |

The 10M figure is the single largest memory point on a typical many-small-file
push and is the reason #1884 exists: 1 GiB of in-memory buffer for an
otherwise streaming workload is not a tenable failure mode on memory-tight
hosts.

## 4. Spill-to-tempfile (#1884) status

Issue #1884 in the issue tracker is closed and was repurposed for an
unrelated release-asset fix. The original spill-to-tempfile design, however,
is captured in `docs/architecture/reorder-buffer.md` Section 5 as
"#1884 - Bounded-memory spill-to-tempfile for stalled successors":

- Proposes streaming completed successors to a per-transfer tempfile when the
  ring would otherwise grow without bound.
- Reads them back in NDX order when the head finally drains.
- Open questions: tempfile location (partial dir? `TMPDIR`?), interaction
  with `--inplace` and `--partial-dir`.

**Implementation status today:** none. There is no `spill`/`tempfile`/
`disk-backed` code path in either reorder buffer (search of
`crates/engine/src/concurrent_delta/` and `crates/transfer/src/reorder_buffer.rs`
returns no matches for those terms). The only memory-bound knob is the
`AdaptiveCapacityPolicy::max` parameter, which is opt-in and not wired into
`DeltaConsumer::spawn` by default.

The closed-issue status of #1884 means the work needs a fresh tracker entry
before any code lands. Suggested wording is in Section 5.

## 5. Bench fixture sketch (peak-RSS measurement)

The existing benches measure throughput, not memory:

- `crates/engine/benches/reorder_buffer_scaling.rs` covers the ring buffer at
  10K / 100K / 1M / 10M items (10M gated on `BENCH_REORDER_10M=1`) and
  compares against a `BTreeMap` baseline.
- `crates/transfer/benches/reorder_buffer_benchmark.rs` covers the
  `BoundedReorderBuffer` at 10K / 100K against a collect-then-sort baseline.

Neither bench records peak resident set size. The fixture below extends the
engine bench with an RSS sample per iteration using platform-specific syscalls
that do not require unsafe in the bench crate.

### 5.1 Layout

A new bench file `crates/engine/benches/reorder_buffer_memory.rs` registered
in `crates/engine/Cargo.toml` alongside the existing scaling bench. The bench
reuses the worst-case insertion pattern from
`reorder_buffer_scaling.rs::shuffled_with_local_swaps` but seeds a single
slow-head sequence to force `force_insert` growth, then samples peak RSS at
each milestone.

### 5.2 Peak-RSS sampling

On Unix, peak RSS is read from `getrusage(RUSAGE_SELF, ...)`'s `ru_maxrss`
field. The Rust ecosystem ships safe wrappers - `peak_alloc::PeakAlloc` for
allocator-level tracking, or `sysinfo::Process::memory()` for OS-level
sampling. Neither needs unsafe in the bench crate.

On Windows, `GetProcessMemoryInfo` provides `PeakWorkingSetSize` via the
`windows` crate (already an approved dependency under
`#[cfg(windows)]`-gated unsafe in `fast_io`).

The bench crate stays unsafe-free by depending on `peak_alloc` (a
small wrapper around `GlobalAlloc` that registers a process-wide peak
counter) and reads the high-water mark between iterations. Sample skeleton:

```rust
// crates/engine/benches/reorder_buffer_memory.rs

#![deny(unsafe_code)]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use engine::concurrent_delta::ReorderBuffer;
use peak_alloc::PeakAlloc;
use std::hint::black_box;

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

/// Insertion order with one slow head: sequence 0 arrives last, every other
/// sequence arrives in shuffled order. Forces force_insert growth.
fn slow_head_pattern(count: usize) -> Vec<u64> {
    let mut seq: Vec<u64> = (1..count as u64).collect();
    // Light shuffle to mimic worker completion jitter without altering peak.
    let mut state: u64 = 0xCAFE_F00D_DEAD_BEEF;
    for i in 0..seq.len() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = ((state >> 33) as usize) % seq.len();
        seq.swap(i, j);
    }
    seq.push(0); // head arrives last - worst case for ring growth.
    seq
}

fn run_with_peak(insertion_order: &[u64], capacity: usize) -> (u64, usize) {
    ALLOC.reset_peak_usage();
    let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(capacity);
    let mut sum: u64 = 0;
    for &seq in insertion_order {
        if buf.insert(seq, seq).is_err() {
            for v in buf.drain_ready() {
                sum = sum.wrapping_add(v);
            }
            buf.force_insert(seq, seq);
        }
        for v in buf.drain_ready() {
            sum = sum.wrapping_add(v);
        }
    }
    for v in buf.drain_ready() {
        sum = sum.wrapping_add(v);
    }
    (sum, ALLOC.peak_usage())
}

fn bench_memory(c: &mut Criterion, count: usize, group_name: &str) {
    let mut group = c.benchmark_group(group_name);
    if count >= 1_000_000 {
        group.sample_size(10);
    }
    let order = slow_head_pattern(count);
    let capacity = 1024;

    group.bench_with_input(
        BenchmarkId::new("slow_head_peak_rss", format!("n={count}")),
        &order,
        |b, order| {
            b.iter(|| {
                let (sum, peak) = run_with_peak(black_box(order), capacity);
                black_box((sum, peak));
            });
        },
    );

    group.finish();
}

fn bench_100k(c: &mut Criterion) {
    bench_memory(c, 100_000, "reorder_memory_100k");
}

fn bench_1m(c: &mut Criterion) {
    bench_memory(c, 1_000_000, "reorder_memory_1m");
}

fn bench_10m(c: &mut Criterion) {
    if std::env::var("BENCH_REORDER_10M").is_err() {
        return;
    }
    bench_memory(c, 10_000_000, "reorder_memory_10m");
}

criterion_group!(benches, bench_100k, bench_1m, bench_10m);
criterion_main!(benches);
```

### 5.3 Expected output

Running `cargo bench -p engine --bench reorder_buffer_memory` produces
criterion's standard timing output plus a `(sum, peak)` tuple per iteration.
The `peak` value is the allocator-level high-water mark for the iteration.
Expected ranges based on Section 3:

- 100K: ~7.6-8 MiB peak (allocator-rounded slot array + transient resize
  spike).
- 1M: ~80 MiB peak.
- 10M: ~1.0-1.3 GiB peak (gated; only run when explicitly requested).

The 10M case is the one that motivates spill-to-tempfile: 1 GiB resident
for an otherwise streaming workload is not acceptable on memory-tight
embedded receivers (4 GiB Raspberry Pi class hosts, container quota
constraints).

### 5.4 Variants

- **Scattered head**: insert in order 1..N then force-insert 0 once the ring
  has already drained partially. Tests the resize-during-active-buffer code
  path (`resize_to` linearises the ring during the resize).
- **Adaptive cap**: same workload but constructed via
  `ReorderBuffer::with_adaptive_policy(min=32, max=10_000)`. Confirms the
  adaptive policy's hard cap and measures the rejection-rate-vs-memory
  trade-off.
- **BoundedReorderBuffer parity**: mirror the above on
  `transfer::reorder_buffer::BoundedReorderBuffer` to compare BTreeMap node
  overhead at 100K against the ring buffer's slot array.

These variants would land alongside the primary bench; they share the same
sampling harness and only differ in setup.

## 6. Recommendations

1. **Re-open the spill-to-tempfile work.** #1884 is closed and the
   architecture doc points at it for the bounded-memory mitigation. A new
   tracker entry should pick up the open questions (tempfile location,
   `--inplace`/`--partial-dir` interaction) and own the implementation.
2. **Wire the adaptive policy by default.** `DeltaConsumer::spawn` could
   construct the buffer with `with_adaptive_policy` and a sensible
   `max` (for example 4 * file_count or 10_000, whichever is smaller). The
   ceiling caps memory and lets `force_insert` continue to break deadlocks
   inside the cap.
3. **Add the peak-RSS bench.** Section 5's fixture provides the data points
   needed to evaluate the spill-to-tempfile design in #1884 - without it,
   any threshold value is a guess.
4. **Audit the redo/failure clone path.** The `result.clone()` calls in
   `consumer.rs:154,165` allocate one extra `String` per insert in the redo
   path. A small refactor that consumes `result` on the success branch and
   only clones on the rare retry branch would remove that overhead.

## 7. Summary

The reorder buffers are memory-light per entry (48-72 bytes inline, no
basis-path or `Arc` carry-over) but the engine ring buffer's `force_insert`
escape hatch removes the steady-state cap when a slow head stalls the
window. At 100K successors the worst case is 7.6 MiB - manageable. At 10M
the worst case is 1+ GiB, which justifies the spill-to-tempfile work. The
sliding-window `BoundedReorderBuffer` does not have the same escape hatch
and trades growth for `BackpressureError`, but at the same item counts it
costs roughly 1.6x the ring buffer because of `BTreeMap` node overhead.

The bench fixture in Section 5 extends the existing scaling benches
(`reorder_buffer_scaling` and `reorder_buffer_large_scale`) with peak-RSS
sampling so the memory characteristics described above can be measured
directly. The data is prerequisite for the spill-to-tempfile design tracked
under the (currently closed) #1884.
