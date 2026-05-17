# ReorderBuffer spill-to-tempfile for stalled successors

Tracking issue: oc-rsync task #1884. Branch:
`docs/reorderbuffer-spill-design-1884`.

## TL;DR (recommendation)

The bounded-memory spill-to-tempfile layer requested by #1884 is
**already implemented** as `SpillableReorderBuffer<T>` in
`crates/engine/src/concurrent_delta/spill.rs:218`, including the
hardening for ENOSPC and temp-dir vanish requested by the parent task
list. The hot-zone heuristic, RAII cleanup, length-prefixed binary
codec, and directory-backed flavour are all present (`spill.rs:1-687`)
and the `SpillCodec` trait is implemented for `DeltaResult`
(`types.rs:457`). PR history confirms the original feature shipped as
PR #3982 and the hardening as PR #2247 (cross-referenced from the
prompt as #4247).

What is **not** yet done:

1. The consumer pipeline still uses the bare `ReorderBuffer`
   (`consumer.rs:174-176`) and never constructs a
   `SpillableReorderBuffer`. The spill machinery is therefore dead
   code from the receiver's perspective; the in-RAM ring is
   load-bearing for the whole transfer.
2. Neither #4195 stall metrics nor #4204 memory benches expose a
   `max_depth` reading large enough to justify pulling the spill
   layer onto the critical path. The 1M-item bench prints
   `max_depth` values that track `4 * drift` (see
   `reorderbuffer_memory.rs:127`); on the largest configured drift
   (16 384) with the heaviest `DeltaResult` payload (~200 B) that is
   ~13 MB - two orders of magnitude under the 64 MB default
   threshold at `spill.rs:61`.

The recommendation in section 9 is therefore: **do not wire the spill
layer in by default**. Instead, (a) close the dead-code gap by
opt-in opt-in CLI plumbing for adversarial transfers, (b) extend the
existing `reorderbuffer_memory` bench with a synthetic huge-drift
scenario that actually exercises the spill path, and (c) gate further
work on bench evidence that `max_depth` × per-item bytes exceeds the
threshold on realistic transfers.

## Scope

This note covers the receiver-side bounded-memory plan for the
`concurrent_delta` reorder buffer when a slow worker stalls delivery
and successor results accumulate. It is a pure receiver-side concern:
no wire-protocol changes, no on-disk artefacts visible to a peer,
tcpdump-replay against upstream rsync must remain byte-identical with
or without the spill layer engaged.

## Source citations

All paths repository-relative.

### `ReorderBuffer` (in-RAM ring)

- `crates/engine/src/concurrent_delta/reorder/mod.rs:89` -
  `ReorderBuffer<T>` struct with `slots: Box<[Option<T>]>` and a
  `capacity` field.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:150` -
  `ReorderBuffer::new(capacity)` panics if `capacity == 0` and
  pre-allocates the ring.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:297` - `insert`
  returns `Err(CapacityExceeded)` when the sequence is outside the
  current ring window unless an adaptive policy is configured.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:328` -
  `try_adaptive_preinsert_grow` extends the ring inside the
  `AdaptiveCapacityPolicy` `[min, max]` bounds before rejecting.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:502` -
  `force_insert` ignores the capacity bound and, for sequences past
  the ring, calls `grow` which doubles capacity (fixed-cap) or grows
  by one slot per call (adaptive). This is the unbounded growth path
  the spill layer is meant to displace.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:544` - `grow`
  doubles for the non-adaptive case (`min_capacity.max(self.capacity * 2)`).
- `crates/engine/src/concurrent_delta/reorder/mod.rs:555` -
  `resize_to` linearises occupied entries into a fresh `Box<[Option<T>]>`.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:582` - `take`
  removes a slot without advancing the delivery cursor; the spill
  layer uses this to extract candidates without disturbing ordering
  state.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:464` -
  `metrics()` (added by PR #4195) returns `current_depth`,
  `max_depth`, and `stall_duration`.

### Existing `SpillableReorderBuffer`

- `crates/engine/src/concurrent_delta/spill.rs:61` -
  `DEFAULT_SPILL_THRESHOLD = 64 * 1024 * 1024` (64 MB).
- `crates/engine/src/concurrent_delta/spill.rs:68` - `HOT_ZONE = 16`
  items pinned near `next_expected` to avoid thrashing.
- `crates/engine/src/concurrent_delta/spill.rs:80` - `SpillError`
  with `Capacity` and `Io` variants; `is_out_of_space` matches
  `io::ErrorKind::StorageFull`.
- `crates/engine/src/concurrent_delta/spill.rs:141` - `SpillCodec`
  trait: `encode`, `decode`, `estimated_size`.
- `crates/engine/src/concurrent_delta/spill.rs:174` -
  `SpillBackend::{Spooled, Directory}` - default backend is
  `tempfile::SpooledTempFile` (in-RAM up to 1 MB, then disk);
  directory-backed flavour uses `tempfile::tempfile_in`.
- `crates/engine/src/concurrent_delta/spill.rs:218` -
  `SpillableReorderBuffer<T: SpillCodec>` wrapper with the same
  public API surface as `ReorderBuffer`.
- `crates/engine/src/concurrent_delta/spill.rs:226` -
  `spill_index: BTreeMap<u64, u64>` maps sequence number to byte
  offset; reload is O(log S) where S is the spilled-item count.
- `crates/engine/src/concurrent_delta/spill.rs:286` - `new(capacity, threshold)`.
- `crates/engine/src/concurrent_delta/spill.rs:315` -
  `with_spill_dir` allows an explicit on-disk scratch directory.
- `crates/engine/src/concurrent_delta/spill.rs:362` - `insert` calls
  `spill_excess` whenever `memory_used > threshold`.
- `crates/engine/src/concurrent_delta/spill.rs:412` - `next_in_order`
  reloads from disk transparently when the spilled sequence equals
  `next_expected`.
- `crates/engine/src/concurrent_delta/spill.rs:515` - `spill_excess`
  walks candidates from highest sequence downward, skipping the hot
  zone, and stops as soon as `memory_used <= threshold`.
- `crates/engine/src/concurrent_delta/spill.rs:570` - `spill_item`
  encodes the payload up front so a codec failure never leaves a
  partial record; on `NotFound` for a directory-backed buffer it
  calls `recreate_spill_dir` and retries once.
- `crates/engine/src/concurrent_delta/spill.rs:656` -
  `recreate_spill_dir` drops the stale file handle, runs
  `create_dir_all`, and only succeeds when no prior items had been
  spilled (otherwise those items are unrecoverable and the error is
  surfaced).
- `crates/engine/src/concurrent_delta/spill.rs:675` - `open_backend`
  returns `SpooledTempFile` for the default flavour, which the OS
  deletes when the buffer is dropped (RAII).

### `DeltaResult` codec

- `crates/engine/src/concurrent_delta/types.rs:457` -
  `impl super::spill::SpillCodec for DeltaResult` with binary
  encoding for ndx, sequence, status, byte counters, and error
  messages.

### Consumer wiring (the dead-code gap)

- `crates/engine/src/concurrent_delta/mod.rs:177` - `pub mod spill`.
- `crates/engine/src/concurrent_delta/mod.rs:185` - `pub use`
  re-exports `SpillCodec`, `SpillError`, `SpillStats`,
  `SpillableReorderBuffer`.
- `crates/engine/src/concurrent_delta/consumer.rs:50` - the consumer
  imports the bare `ReorderBuffer`, not `SpillableReorderBuffer`.
- `crates/engine/src/concurrent_delta/consumer.rs:173-177` -
  `delta-reorder` thread constructs `ReorderBuffer::passthrough()`
  or `ReorderBuffer::new(reorder_capacity)`. Neither path admits
  the spill layer.
- `crates/engine/src/concurrent_delta/consumer.rs:193` - when the
  ring fills and `next_expected` is missing, the loop calls
  `force_insert`. This is the "consumer.rs force_insert smell"
  tracked in memory: under sustained backpressure the ring grows
  without bound rather than spilling to disk.

### Stall metrics (#4195) and memory bench (#4204)

- `crates/engine/src/concurrent_delta/reorder/mod.rs:43-52` -
  `Metrics` struct (PR #4195).
- `crates/engine/src/concurrent_delta/reorder/mod.rs:118-125` -
  `max_depth`, `stall_duration`, `stall_started_at` fields.
- `crates/engine/src/concurrent_delta/reorder/mod.rs:251` -
  `refresh_stall_state` only samples `Instant::now()` on the stall
  edges, keeping the hot path allocation-free.
- `crates/engine/benches/reorderbuffer_memory.rs:58` -
  `DRIFTS: [usize; 4] = [32, 256, 2_048, 16_384]`.
- `crates/engine/benches/reorderbuffer_memory.rs:61-64` -
  `DEFAULT_COUNTS = [100_000, 500_000]`, `HEAVY_COUNT = 1_000_000`
  gated behind `BENCH_REORDER_MEMORY_1M`.
- `crates/engine/benches/reorderbuffer_memory.rs:127` -
  `let capacity = (drift * 4).max(64);` - capacity is provisioned at
  4x the drift window so the steady-state insert stays on the
  fast path.
- `crates/engine/benches/reorderbuffer_memory.rs:90-109` -
  `run_cycle` calls `metrics()` and returns the peak `max_depth`
  alongside the elements sum.

### Cache bench (#4180) and wire-format audit (#4205)

- `crates/engine/benches/reorder_buffer_cache.rs:1-49` - cache-miss
  / LLC behaviour at 1M items, gated behind `BENCH_REORDER_CACHE=1`.
  Establishes the storage-layout baseline the spill design must not
  regress.
- `docs/audits/parallel-dispatch-wire-format.md` (PR #4205) - confirms
  parallel dispatch and the reorder buffer do not perturb the wire
  protocol; spill-to-tempfile inherits that property because all
  serialisation is local-only.

### Hardening already in tree (PR #2247, referenced as "#4247")

- `crates/engine/src/concurrent_delta/spill.rs:1039` -
  `enospc_during_spill_propagates_as_io_error`.
- `crates/engine/src/concurrent_delta/spill.rs:1077` -
  `partial_write_surfaces_as_write_zero` exercises the `WriteZero`
  contract that the standard library promises on partial writes.
- `crates/engine/src/concurrent_delta/spill.rs:1111` -
  `temp_dir_vanish_recreates_when_no_prior_spills`.
- `crates/engine/src/concurrent_delta/spill.rs:1142` -
  `temp_dir_vanish_after_prior_spills_returns_error` proves we do
  not silently swallow data when prior spills are stranded.
- `crates/engine/src/concurrent_delta/spill.rs:1183` -
  `dir_recreate_failure_surfaces_io_error`.
- `docs/design/multi-file-delta-apply-pipeline.md:82-86` -
  "spill-to-tempfile pending" risk note that this design retires.

## 1. Current memory cap

The on-the-wire reorder path has two memory budgets, neither of which
is strictly bounded today:

1. **Fixed-capacity ring.** `ReorderBuffer::new(capacity)` allocates
   exactly `capacity` `Option<T>` slots
   (`reorder/mod.rs:150-167`). Out-of-window inserts fail with
   `CapacityExceeded` at `reorder/mod.rs:308`.
2. **Adaptive ring.** `with_adaptive_policy` accepts a `[min, max]`
   range; `try_adaptive_preinsert_grow`
   (`reorder/mod.rs:328-349`) extends the ring inside `max` before
   surrendering to `CapacityExceeded`.

The consumer's behaviour on overflow today is **re-allocate, not
block**:

- `consumer.rs:182-196` drains ready items first; if no drain made
  progress (head missing) it falls through to
  `reorder.force_insert(seq, result.clone())` at line 193.
- `ReorderBuffer::force_insert` (`reorder/mod.rs:502-536`) writes
  the slot if it fits and otherwise calls `grow`. `grow`
  (`reorder/mod.rs:544-550`) doubles capacity for non-adaptive
  buffers and adds one slot for adaptive buffers, then `resize_to`
  (`reorder/mod.rs:555-571`) reallocates a fresh `Box<[Option<T>]>`
  and relocates every occupied entry.

Outcome: a single stalled successor combined with sustained
backpressure can grow the ring monotonically until the process runs
out of address space. There is no upper bound on the ring's RAM use,
and the doubling step also doubles the relocation cost. PR #4195
exposed `max_depth` so an operator can finally measure that growth,
but no policy reacts to it.

`SpillableReorderBuffer` exists to bound that growth by serialising
the highest-sequence items to a tempfile, but
**no consumer constructs it** (`mod.rs:185` exports it,
`consumer.rs:50` does not import it, `consumer.rs:173-177` builds a
bare `ReorderBuffer`). The spill layer is therefore present in the
crate and absent from the runtime path.

## 2. Spill trigger

Threshold inputs:

- `max_depth` from `Metrics::max_depth`
  (`reorder/mod.rs:50`), surfaced by PR #4195.
- Mean per-item bytes from `SpillCodec::estimated_size`
  (`spill.rs:160`) - for `DeltaResult` this is dominated by the
  status discriminant and optional error-message string
  (`types.rs:457`+).
- `reorderbuffer_memory` bench data
  (`benches/reorderbuffer_memory.rs`) for empirical depth versus
  drift.

The existing spill layer triggers on bytes alone
(`spill.rs:371`: `if self.memory_used > self.threshold`). That
behaviour is sound and matches the requested "N MB" threshold.
Default: 64 MB (`spill.rs:61`). The bench's worst-case readings sit
comfortably under that ceiling (see TL;DR), which is the empirical
basis for the recommendation in section 9 not to enable the spill on
the default path.

For an opt-in adversarial scenario the recommended threshold pair is:

- **Bytes:** keep the existing `DEFAULT_SPILL_THRESHOLD = 64 MB`.
  The 1 MB `SpooledTempFile` rollover at `spill.rs:683` means
  transient spikes below ~1 MB never touch disk anyway.
- **Items:** no separate item-count cap is needed. The ring's own
  `capacity` field bounds the offset window, and `force_insert`
  growth is the symptom the spill is replacing - once the spill is
  on the critical path, `force_insert` becomes the spill trigger of
  last resort rather than an unbounded growth signal. Adding a
  second cap is configuration surface without behavioural lift.

A future tightening, predicated on bench evidence from section 6,
could parameterise the threshold per transfer from a CLI flag
(`--reorder-spill-mb`). That belongs to a follow-up task; the design
here keeps the threshold a single compile-time constant to match the
existing `SpillableReorderBuffer::new` signature.

## 3. Spill layout

The on-disk format is already specified by `SpillCodec` and
`spill.rs:614`:

```
+--------+--------+--------+--------+----- ... -----+
|        u32 little-endian length    | payload bytes |
+--------+--------+--------+--------+----- ... -----+
```

Records are appended sequentially. The memory-resident index is
`spill_index: BTreeMap<u64, u64>` (`spill.rs:226`), mapping
sequence number to record start offset. `BTreeMap` gives O(log S)
reload lookup and ordered iteration that matches the
highest-sequence-first spill order chosen in section 4.

The codec format is **opaque to the spill layer** (`spill.rs:140`):
only `encode`/`decode` must agree. That keeps the on-disk layout
free to evolve with `DeltaResult` and any future spillable result
type without touching the spill machinery.

Backend choice:

- Default: `SpooledTempFile` at 1 MB rollover (`spill.rs:683`). The
  OS deletes the tempfile on drop, so RAII cleanup is automatic
  whether the transfer succeeds, fails, or panics.
- Optional: `tempfile_in(dir)` (`spill.rs:677`) when the operator
  needs the scratch on a dedicated volume (encrypted scratch,
  tmpfs, dedicated SSD). This flavour also benefits from
  `recreate_spill_dir` recovery (`spill.rs:656-671`).

## 4. Drain

Drain is fully handled by the existing `next_in_order`
(`spill.rs:412-438`):

1. Check the in-memory ring first.
2. If empty for `next_expected`, consult `spill_index`.
3. If a spilled record exists, `reload_item`
   (`spill.rs:628-647`) seeks to the offset, reads the length
   prefix and payload, decodes the item, removes the index entry,
   increments `reload_count`, then `force_insert`s the item back
   at offset 0 of the in-RAM ring and drives a single
   `next_in_order` to advance the cursor.

The `force_insert`-at-offset-0 step is unconditionally safe because
the ring always has at least one free slot at the head after the
previous `take` (the slot we are about to re-fill). The
`debug_assert!` on `spill.rs:433-436` guards that invariant.

Drain ordering invariant: `spill_excess` only spills sequences
**above** the hot zone (`spill.rs:524-538`). The hot zone always
includes `next_expected`, so the head slot can never be on disk
unless the spill threshold itself shrinks below one item's payload -
the existing tests pin that boundary
(`spill.rs:858-879`, `spill.rs:893-908`).

## 5. Hazards

### 5.1 Tempfile cleanup on panic or drop

The default backend (`SpooledTempFile`) drops its `File` handle on
`Drop` and the OS unlinks the underlying inode (`spill.rs:174-186`).
A panic that unwinds through the spill buffer therefore leaves no
on-disk debris. The directory-backed flavour uses
`tempfile::tempfile_in`, which creates an anonymous handle (already
unlinked before any data is written), so the same property holds.

Test coverage: `spill.rs:816-829`
(`cleanup_on_drop`) and the directory round-trip at
`spill.rs:1202-1217`.

### 5.2 ENOSPC during spill

Hardened in PR #2247. `spill_item` (`spill.rs:570-607`) encodes the
payload into a `Vec<u8>` before any disk I/O, so a codec failure
cannot leave a partial record. ENOSPC from the kernel is surfaced
as `io::ErrorKind::StorageFull` and propagates as
`SpillError::Io`, which the caller can match via
`SpillError::is_out_of_space` (`spill.rs:99`). The failing item is
re-inserted into the ring at `spill.rs:555` so the caller can
retry or shut down without losing the result.

The receiver must map `SpillError::Io` to rsync exit code 11
(`FileIo`) to mirror upstream's I/O-failure handling. That is the
contract documented at `spill.rs:75-77`.

Test coverage: `enospc_during_spill_propagates_as_io_error`
(`spill.rs:1039-1074`),
`partial_write_surfaces_as_write_zero` (`spill.rs:1077-1108`).

### 5.3 Temp directory vanish mid-transfer

Hardened in PR #2247. For the directory-backed flavour,
`recreate_spill_dir` (`spill.rs:656-671`) runs exactly once and
only when **no prior items had been spilled**; otherwise those
stranded items would be silently lost and the transfer would commit
an incomplete merge. `dir_recreate_count` (`spill.rs:240`) is
exposed via `SpillStats` for operator diagnostics.

Test coverage: `temp_dir_vanish_recreates_when_no_prior_spills`
(`spill.rs:1111-1139`),
`temp_dir_vanish_after_prior_spills_returns_error`
(`spill.rs:1142-1180`),
`dir_recreate_failure_surfaces_io_error`
(`spill.rs:1183-1199`).

### 5.4 Concurrent reordering vs spill writes

`SpillableReorderBuffer` is `!Sync` by construction: it takes
`&mut self` on every mutator (`spill.rs:362`, `:387`, `:412`).
The consumer thread is already the sole owner of the buffer
(`consumer.rs:170-216` spawns one `delta-reorder` thread per
pipeline), so no additional locking is required. The spill writes
happen on the same thread that performs reorder admission, so a
slow disk back-pressures the upstream `stream_rx` channel
(`consumer.rs:158`) naturally.

Caveat: if a future refactor moves drain onto a second thread - for
example to parallelise commit alongside reorder - the spill buffer
will need either an outer `Mutex` or a split owner/reader API. That
is out of scope for #1884 and would be a sibling task to the
shared-ring serialisation work tracked in MEMORY for io_uring.

### 5.5 Memory-accounting drift

`memory_used` is updated by `estimated_size()`, which the codec
documents as "approximate" (`spill.rs:156-161`). Tests exercise
the boundary at `spill.rs:858-879` to guarantee a single insert
past the threshold triggers a spill. The accounting tolerates
under-estimates (the worst case is one extra in-RAM item before the
next insert spills) and over-estimates (the worst case is an
unnecessary disk round-trip). Neither outcome is data-lossy.

## 6. Bench plan

Extend `crates/engine/benches/reorderbuffer_memory.rs` rather than
introduce a new bench file. The existing scaffolding already builds
deterministic drifted permutations
(`reorderbuffer_memory.rs:70-83`) and prints `max_depth` alongside
Criterion timings (`reorderbuffer_memory.rs:131-136`); reusing it
keeps the comparison fair.

Proposed additions:

1. **Spill cell.** Add a fourth bench function,
   `bench_spill_path`, that runs the same `drifted_permutation`
   inputs through `SpillableReorderBuffer::new(capacity, threshold)`
   with `threshold` set to a fraction of the projected
   `max_depth * mean_item_bytes`. Initial sweep:
   `threshold in [1 MB, 8 MB, 64 MB]` against
   `drift = 16_384` and `count = 100_000`. The 64 MB cell verifies
   the default never spills under the same load that the in-RAM
   bench already proves comfortable.
2. **Adversarial drift.** Add a single synthetic case where one
   sequence (the head) is deliberately withheld until all
   successors are inserted - the canonical pathological scenario
   that motivates the spill. This is a closed-form input
   (`Vec::from_iter((1..N).chain(std::iter::once(0)))`), not a
   permutation, and it exercises `spill_excess` deterministically.
3. **DeltaResult payload.** Replace `u64` with `DeltaResult` so the
   `SpillCodec` for the real production type is exercised end to
   end (`types.rs:457`). Item size grows from 8 B to ~96-160 B
   depending on status and error-message presence; the spill
   threshold is then crossed at realistic item counts.
4. **Spill stats reporting.** Print
   `spill_stats.spill_events`, `reload_events`,
   `dir_recreate_events`, and `memory_used` alongside the existing
   `max_depth` line so the bench output documents which cells
   actually exercised the disk path.

Gate the spill cells behind a new `BENCH_REORDER_SPILL=1` env var
(matches the pattern at `reorder_buffer_cache.rs:35`) so the
default `cargo bench -p engine --bench reorderbuffer_memory`
remains a no-op for the disk-touching cases.

Success criterion: for the `threshold = 64 MB` cell at
`count = 100_000 / drift = 16_384`,
`spill_stats.spill_events == 0`. That is the empirical proof that
the default threshold is not on the critical path for realistic
transfers and that opt-in plumbing is sufficient.

## 7. Wiring plan (deferred)

Wiring `SpillableReorderBuffer` into the consumer is a single-file
change in scope but a behavioural change in impact, so it is
**explicitly deferred** until section 6 produces bench evidence:

- `consumer.rs:50` would gain `use super::spill::SpillableReorderBuffer;`
- `consumer.rs:173-177` would branch on a `SpillPolicy` enum
  (`Off`, `On { threshold }`, `Auto { threshold, max_depth_trigger }`)
  rather than hard-coding `ReorderBuffer::new`.
- `consumer.rs:182-203` `insert` and `drain_ready` calls return
  `Result<_, SpillError>` instead of bare values; capacity errors
  remain non-fatal (the existing loop) and I/O errors propagate to
  the `result_tx` consumer via a new `DeltaResult::failed` mapping
  to exit code 11.
- A `DeltaConsumer::spawn_with_spill(rx, capacity, threshold)`
  constructor matches the existing `spawn` /
  `spawn_bypass` factory pair (`consumer.rs:128-145`).

This is recorded for completeness; it is **not** the deliverable for
#1884.

## 8. What this design replaces / supersedes

- `docs/design/multi-file-delta-apply-pipeline.md:82-86` "spill-to-
  tempfile pending" risk - retired by the existing
  `SpillableReorderBuffer` implementation; the note should be
  updated in a follow-up to point at this doc.
- The "consumer.rs force_insert smell" entry in project memory -
  the underlying behaviour is unchanged, but section 1 documents
  the exact code path and section 7 sketches the wiring that would
  retire it.

## 9. Recommendation

**Do not enable the spill layer on the default consumer path
without bench evidence from section 6.** Concretely:

1. Land the extended `reorderbuffer_memory` bench cells from
   section 6 (separate PR, prefix `bench:`).
2. Run those cells in the `rsync-profile` container with the
   adversarial drift scenario and the `DeltaResult` payload.
3. If any realistic cell triggers `spill_events > 0` at the 64 MB
   threshold, file a follow-up to wire `SpillableReorderBuffer`
   into `consumer.rs` per section 7.
4. If no realistic cell triggers a spill, expose the spill layer
   only behind an explicit `--reorder-spill-mb=<N>` CLI knob for
   the adversarial transfers (one tiny file followed by a multi-GB
   delta) that motivated #1884 in the multi-file pipeline note.

The bounded-memory plan requested by #1884 is **already in tree** at
`crates/engine/src/concurrent_delta/spill.rs:218`, the hardening
asked for is **already in tree** via PR #2247, and the metrics
needed to size the threshold are **already in tree** via PR #4195.
The remaining work is bench-evidence and a wiring decision, both of
which belong to follow-up tasks once section 6 produces data.

## Cross-references

- PR #3982 - `feat: add bounded-memory spill-to-tempfile for ReorderBuffer (#1884)`.
- PR #2247 - `fix(engine): harden reorder spill against ENOSPC and temp-dir vanish` (referenced in the prompt as #4247).
- PR #4195 - `feat(engine): expose ReorderBuffer stall and queue-depth metrics`.
- PR #4204 - `bench(engine): profile ReorderBuffer memory at 100K/500K/1M`.
- PR #4180 - `bench(engine): profile ReorderBuffer cache behavior at 1M items`.
- PR #4205 - `docs(audits): wire-format unchanged under parallel dispatch`.
- Issue #1885 - reorder-buffer metrics, the gating signal for spill threshold tuning.
- Issue #1886 - reorder-buffer bypass when `--delay-updates` is off (sibling design).
- `docs/design/reorderbuffer-metrics-and-bypass.md` - sibling
  metrics and bypass design.
- `docs/design/multi-file-delta-apply-pipeline.md` - the surrounding
  pipeline design that flagged #1884 as "pending".
