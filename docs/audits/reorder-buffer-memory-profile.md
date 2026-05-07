# ReorderBuffer Memory Profile Plan

Tracking: oc-rsync task #1564.

> Static analysis and profiling plan. No code lands in this PR.
> Cross-references task #1884 for the spill-to-tempfile follow-up.

## 1. Implementations

Two reorder buffers coexist in the workspace:

| Crate | Path | Backing store | Bound |
|-------|------|---------------|-------|
| `transfer` | `src/reorder_buffer.rs` | `BTreeMap<u64, T>` | `BoundedReorderBuffer` enforces `[next_expected, next_expected + W)`; out-of-window inserts return `BackpressureError`. Default `W = 64`. |
| `engine` | `src/concurrent_delta/reorder.rs` | `Box<[Option<T>]>` ring | `ReorderBuffer` rejects with `CapacityExceeded` once offset >= capacity; optional `AdaptiveCapacityPolicy` grows up to `max`. `force_insert` grows unconditionally. |

The engine ring is the production path consumed by `concurrent_delta::consumer`
and `delta_pipeline`. The transfer `BoundedReorderBuffer` is used by the
sliding-window classification path; both must be profiled. Critically, the
engine ring's `force_insert` and the adaptive `grow()` path are *unbounded* in
the absence of a `policy.max` cap, and `force_insert` ignores that cap entirely.
That is the path that 100K stalled successors can drive without rejection.

## 2. Worst-case memory at 100K queued tails

`DeltaResult` (`crates/engine/src/concurrent_delta/types.rs`) on 64-bit:

| Field | Bytes |
|-------|-------|
| `ndx: FileNdx` (`u32`) | 4 |
| `sequence: u64` | 8 |
| `bytes_written: u64` | 8 |
| `literal_bytes: u64` | 8 |
| `matched_bytes: u64` | 8 |
| `status: DeltaResultStatus` (enum + `String`) | 32 (discriminant + 24 B `String` header, padded) |
| Padding / alignment | ~4 |
| **Inline size** | **~72 B** |

Per `Option<DeltaResult>` slot adds 0 B (niche-optimised by `String` in the
variants? no - the success default has no `String`, so `Option` adds 1 B + 7 B
padding). Realistic per-slot footprint: **80 B**. With 100K stalled tails:

- Ring slots: `100_000 * 80 B = 8.0 MB` (engine ring path).
- BTreeMap node overhead (transfer path): ~48 B per node + key + value rounds
  to ~144 B per entry, i.e. `100_000 * 144 B = 14.4 MB`.
- `NeedsRedo`/`Failed` payloads carry heap `String`s (typically 32-128 B each).
  Worst-case redo-only flood: `100_000 * 128 B = 12.8 MB` extra heap.

Total worst-case resident set at 100K queued tails with one stalled head:
**~21-27 MB** for the ring path, **~27-33 MB** for the BTreeMap path. Below
the 100 MB threshold that would warrant spill-to-tempfile in normal operation,
but linear in queue depth and unbounded under the `force_insert`/grow paths.

## 3. Profile plan

Use `dhat-rs` (heap profiler) under a synthetic workload that reproduces the
stall-head pathology:

1. `cargo bench -p engine --bench reorder_buffer_scaling -- --profile-time 30`
   with `dhat::HeapProfiler` enabled via a `#[cfg(feature = "dhat-heap")]`
   guard added to `concurrent_delta::reorder` tests.
2. Workload: produce sequences `1..=100_000`, withhold sequence `0` until the
   end. Measure `dhat::Stats::max_blocks` and `max_bytes` at the moment the
   ring is fullest.
3. Repeat for `BoundedReorderBuffer` with `W = 100_001` to confirm parity.
4. Compare against the realistic load - sequences `0..100_000` arriving in
   pseudo-random order with window `W = 64` - which must show steady-state
   memory bounded by `W * 80 B = 5.1 KB`.
5. Capture the dhat output to `target/dhat/reorder-buffer-100k.json` and
   include it as a release asset attached to the #1564 closeout.

## 4. Bound design

Two complementary bounds:

1. **Per-window cap.** Engine ring exposes a hard ceiling identical to the
   adaptive policy's `max`, but enforced at every entry point including
   `force_insert`. `force_insert` becomes `force_insert_within_cap` and
   surfaces `CapacityExceeded` once the cap is hit. Default cap: 4096 slots
   (≈320 KB at 80 B per slot).
2. **Spill-to-tempfile.** Once the ring rejects, the consumer offloads the
   surplus to a `tempfile::tempfile()` keyed by sequence. On drain, the
   consumer reads the spill file in sequence order and feeds it through the
   normal `next_in_order` path. Cross-references the storage abstraction
   already discussed in task #1884.

Spill semantics: write `bincode`-encoded `(seq, DeltaResult)` records,
mmap-backed reads on drain, deleted automatically on `Drop` of the spill
handle. No on-disk format compatibility requirement - the file is purely
in-process scratch.

## 5. Decision criteria

Adopt the per-window cap unconditionally. Adopt spill-to-tempfile only if
profiling shows max-blocks exceeding **64 MB** at expected production scale
(100K-1M file transfers with adversarial stall patterns). Below 64 MB the
ring cap alone is sufficient and avoids the disk-I/O complexity. Decision
gate references the dhat output captured in section 3.
