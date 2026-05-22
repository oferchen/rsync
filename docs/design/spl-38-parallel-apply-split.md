# SPL-38.a: Split-point audit for `parallel_apply.rs`

Audit-only mapping for the planned decomposition of
`crates/engine/src/concurrent_delta/parallel_apply.rs` (1579 LoC at
commit `515d6d72d`). Subsequent tickets SPL-38.b/c/d/e carry out the
mechanical extraction; this document fixes the boundaries so each
follow-up PR is a pure code move with no behavioural change.

## 1. Scope and current shape

Source file: `crates/engine/src/concurrent_delta/parallel_apply.rs`
Total lines: 1579 (902 production + 677 tests)
Public surface (re-exported from `concurrent_delta::mod`):

- `ParallelDeltaApplier`
- `ParallelApplyError`
- `DeltaChunk`

Crate-private types (file-local today):

- `FileSlot`
- `SlotBarrier`
- `DecrementGuard`
- `SlotHandle`
- `VerifiedChunk`
- `hex_lower` (free function)

Top-level structure of the current file:

| Lines | Item |
|------:|------|
| 1-38 | Module rustdoc |
| 39-51 | Imports |
| 53-139 | `ParallelApplyError` + `From<ParallelApplyError> for io::Error` |
| 141-223 | `DeltaChunk` struct + constructors |
| 225-278 | `FileSlot` struct + impl |
| 280-356 | `SlotBarrier` struct + impl (BarrierState shape) |
| 358-371 | `DecrementGuard` struct + Drop |
| 373-409 | `SlotHandle` struct + impl |
| 411-423 | `VerifiedChunk` struct |
| 425-483 | `ParallelDeltaApplier` struct + `Debug` impl |
| 485-586 | Applier constructors + `register_file` |
| 588-676 | `apply_one_chunk` + `apply_batch_parallel` |
| 678-689 | `bytes_written` |
| 691-772 | `finish_file` (includes spin-loop workaround) |
| 774-833 | `flush_workers` + `drain_inflight` |
| 835-848 | `slot_for` |
| 850-890 | `verify_chunk` |
| 892-901 | `hex_lower` |
| 903-1579 | `#[cfg(test)] mod tests` |

## 2. Target layout

```
crates/engine/src/concurrent_delta/parallel_apply/
    mod.rs            # ParallelDeltaApplier core + register/lookup + verify
    slot_barrier.rs   # SlotBarrier + (post DG-3) BarrierState/SlotData/SlotEntry
    decrement_guard.rs# DecrementGuard + SlotHandle
    batch.rs          # apply_one_chunk + apply_batch_parallel + VerifiedChunk
    drain.rs          # finish_file + flush_workers + drain_inflight
```

The split keeps `parallel_apply` a single `mod.rs`-rooted module so the
public re-exports in `concurrent_delta/mod.rs` stay byte-identical and
no downstream import changes. `FileSlot`, `DeltaChunk`,
`ParallelApplyError`, `hex_lower`, and the module rustdoc remain at the
root because every submodule depends on them.

## 3. Planned submodules

### 3.1 `parallel_apply/slot_barrier.rs`

Owns the per-slot synchronisation primitive backing FFB-1/FFB-2.

- **Starting line in current file:** 280
- **Ending line in current file:** 356
- **LoC in current file:** 77
- **Estimated post-split LoC:** ~95 (adds `use` block, file-level
  rustdoc, optional `cfg(test)` constructor for direct tests).

**Public items exported to siblings (`pub(super)`):**

- `struct SlotBarrier`
- `impl SlotBarrier`
  - `fn new(slot: FileSlot) -> Self`
  - `fn lock_slot(&self, ndx: FileNdx, kind: &'static str) -> io::Result<MutexGuard<'_, FileSlot>>`
  - `fn increment_inflight(&self)`
  - `fn decrement_inflight(&self)`
  - `fn wait_until_idle(&self, ndx: FileNdx, kind: &'static str) -> io::Result<()>`

**Private items kept inside the submodule:**

- Inner field set (`slot: Mutex<FileSlot>`, `inflight: Mutex<usize>`,
  `notify: Condvar`).
- All `expect("inflight mutex poisoned ...")` messages stay local; they
  are not part of the API surface.

**Cross-references that need rewiring:**

| Caller | Action |
|--------|--------|
| `mod.rs::ParallelDeltaApplier::files` field type | Use `super::slot_barrier::SlotBarrier` (or `BarrierState` post DG-3). |
| `mod.rs::register_file` (line 573) | Import `slot_barrier::SlotBarrier`. |
| `decrement_guard.rs::DecrementGuard::drop` | Call `self.barrier.decrement_inflight()` via the re-exported trait surface. |
| `decrement_guard.rs::SlotHandle::new` | Calls `barrier.increment_inflight()`. |
| `drain.rs::flush_workers` | Calls `barrier.wait_until_idle(...)`. |
| `drain.rs::finish_file` (post DG-3 path) | Reads `barrier.slot` via `Arc::try_unwrap` and `into_inner`. Today this happens against the `SlotBarrier` directly; coordinate with DG-2.b (see s.5). |
| `tests::flush_workers_survives_spurious_wakeup` (line 1300) | Touches `barrier.notify` directly; expose `#[cfg(test)] pub(super) fn notify_all(&self)` instead of leaking the bare `Condvar` field. |

**Why this seam is clean:** `SlotBarrier` has zero external callers and
its `Mutex`/`Condvar` fields are private today. Lifting it to its own
file is a verbatim move plus a `pub(super)` visibility audit. The only
test that pokes at the internals (`notify`) needs a tiny accessor.

### 3.2 `parallel_apply/decrement_guard.rs`

Owns the RAII pair (`DecrementGuard`, `SlotHandle`) that keeps the
in-flight counter exception-safe.

- **Starting line in current file:** 358
- **Ending line in current file:** 409
- **LoC in current file:** 52
- **Estimated post-split LoC:** ~75 (file rustdoc + imports + the
  existing inline rustdoc).

**Public items exported to siblings (`pub(super)`):**

- `struct SlotHandle`
- `impl SlotHandle`
  - `fn new(barrier: Arc<SlotBarrier>) -> Self`
  - `fn lock_slot(&self, ndx: FileNdx, kind: &'static str) -> io::Result<MutexGuard<'_, FileSlot>>`

**Private items kept inside the submodule:**

- `struct DecrementGuard` (today only constructed inside
  `SlotHandle::new` and only consumed by `SlotHandle`'s field drop;
  there is no external constructor).
- `impl Drop for DecrementGuard`.

**Cross-references that need rewiring:**

| Caller | Action |
|--------|--------|
| `mod.rs::slot_for` (line 835-848) | Returns `SlotHandle`; import from `decrement_guard`. |
| `batch.rs::apply_one_chunk` (line 625) | Holds the `SlotHandle` between `slot_for` and `lock_slot`. |
| `batch.rs::apply_batch_parallel` (line 671) | Same pattern, one handle per verified chunk. |
| `mod.rs::bytes_written` (line 686) | Calls `handle.lock_slot(...)`. |
| `drain.rs` | Does not touch handles directly; the barrier wait is
  the synchronisation point. |
| `tests::flush_workers_blocks_until_worker_drops_arc` (1167-1206) | Uses `slot_for(...)` to obtain a `SlotHandle`; that path is unchanged. |
| `tests::drain_inflight_drains_all_files` (1208-1256) | Same. |

**Why this seam is clean:** `DecrementGuard` and `SlotHandle` are
file-private today and the only callers are `slot_for` (constructor)
and the workers (drop on scope exit). The pair has a single
dependency (`SlotBarrier`) and exposes only `new` + `lock_slot`.

**DG-3 coordination caveat:** see s.5. Under the DG-2.a Option B
restructure, `DecrementGuard` keeps its `Arc<BarrierState>` (renamed
from `SlotBarrier`) but the parent `SlotHandle` gains a second
`Arc<SlotData>` field whose declaration order matters for drop
sequencing. This file remains the natural home for both types after
DG-3; the only API impact is the `SlotHandle::new` signature, which
becomes `fn new(entry: SlotEntry) -> Self`.

### 3.3 `parallel_apply/batch.rs`

Owns the chunk-level dispatch surface: the public per-chunk and batch
entry points plus the CPU-bound verify result holder.

- **Starting line in current file:** 588 (start of `apply_one_chunk`)
- **Ending line in current file:** 676 (end of `apply_batch_parallel`)
- **Plus:** `VerifiedChunk` at 411-423 (~13 LoC) moves here too because
  it is the return type of the verify step and is only consumed by these
  two methods.
- **Plus:** `verify_chunk` at 850-889 (~40 LoC) and `hex_lower` at
  892-901 (~10 LoC) move with it - `verify_chunk` is the per-chunk CPU
  step both entry points invoke, and `hex_lower` is its only consumer.
- **LoC in current file:** 89 + 13 + 40 + 10 = ~152
- **Estimated post-split LoC:** ~190 (adds file rustdoc + imports +
  module-level test for `hex_lower` boundary cases if missing).

**Public items exported to siblings (`pub(super)`):**

- `impl ParallelDeltaApplier` block carrying:
  - `pub fn apply_one_chunk(&self, chunk: DeltaChunk) -> io::Result<()>`
  - `pub fn apply_batch_parallel(&self, chunks: Vec<DeltaChunk>) -> io::Result<()>`

(Both methods are already `pub` on the public type; they stay `pub`
through a second `impl ParallelDeltaApplier` block in `batch.rs`.)

**Private items kept inside the submodule:**

- `struct VerifiedChunk`
- `fn verify_chunk(strategy: &dyn ChecksumStrategy, chunk: DeltaChunk) -> Result<VerifiedChunk, ParallelApplyError>` - `pub(super)` only if `bench` modules need it; otherwise file-local.
- `fn hex_lower(bytes: &[u8]) -> String` - file-local.

**Cross-references that need rewiring:**

| Caller | Action |
|--------|--------|
| `mod.rs::slot_for` | Already `pub(super)`; called from `batch.rs` via `self.slot_for(ndx)`. |
| `mod.rs::strategy` field | Accessed as `self.strategy` from the `impl` block in `batch.rs`; field stays `pub(super)`. |
| `slot_barrier.rs` | No direct dependency. |
| `decrement_guard.rs::SlotHandle` | Used as the return of `slot_for`. |
| `drain.rs` | No direct dependency. |
| `tests::*` | All tests stay in `mod.rs`'s `tests` module; they call only the public `apply_one_chunk` / `apply_batch_parallel` surface, so the move is invisible to them. |

**Why this seam is clean:** the two entry points share `slot_for`,
`VerifiedChunk`, and `verify_chunk`, and nothing else in the file
consumes those three helpers. Bundling them keeps the
verify-then-ingest flow in one place and removes the temptation to
add a third entry point in another file.

### 3.4 `parallel_apply/drain.rs`

Owns the lifecycle barrier surface: per-file flush, applier-wide
drain, and the file finaliser that bakes the barrier in.

- **Starting line in current file:** 691 (start of `finish_file`)
- **Ending line in current file:** 833 (end of `drain_inflight`)
- **LoC in current file:** 143
- **Estimated post-split LoC:** ~170 (file rustdoc + imports + the
  spin-loop comment block trims to a one-line cross-ref to DG-2.a once
  DG-3 lands).

**Public items exported to siblings (`pub(super)`):**

- `impl ParallelDeltaApplier` block carrying:
  - `pub fn finish_file(&self, ndx: impl Into<FileNdx>) -> io::Result<Box<dyn Write + Send>>`
  - `pub fn flush_workers(&self, ndx: impl Into<FileNdx>) -> io::Result<()>`
  - `pub fn drain_inflight(&self) -> io::Result<()>`

(All three already `pub` on the public type; they stay `pub` through a
second `impl ParallelDeltaApplier` block in `drain.rs`.)

**Private items kept inside the submodule:**

- None today. Post DG-3, the bounded spin loop (lines 729-748)
  disappears entirely; the file shrinks by ~25 LoC. No new private
  helpers are needed.

**Cross-references that need rewiring:**

| Caller | Action |
|--------|--------|
| `mod.rs::files` field | Accessed as `self.files.get(...)` / `self.files.remove(...)`; field stays `pub(super)`. |
| `slot_barrier.rs::SlotBarrier::wait_until_idle` | Called from `flush_workers`. |
| `slot_barrier.rs::SlotBarrier::slot` | Read by `finish_file` via `Arc::try_unwrap` + `into_inner`. **Post DG-3**, the field moves under `SlotData`; coordinate with DG-2.b s.8 step 8. |
| `batch.rs` | No direct dependency. |
| `decrement_guard.rs` | No direct dependency. |
| `tests::flush_workers_returns_immediately_when_no_inflight`, `flush_workers_returns_ok_for_unknown_ndx`, `flush_workers_blocks_until_worker_drops_arc`, `drain_inflight_drains_all_files`, `finish_file_calls_flush_workers_internally`, `finish_file_with_pending_chunks_errors`, `cursor_writer_round_trip`, `flush_workers_survives_spurious_wakeup` | All exercise the public surface; no edits needed beyond the existing test module continuing to live in `mod.rs`. |

**Why this seam is clean:** the three drain entry points share no
helpers with the batch surface. They depend only on
`SlotBarrier::wait_until_idle`, the `DashMap` lookup, and (in
`finish_file`) the typed `ParallelApplyError` variants. The spin loop
inside `finish_file` is the one quirk; see s.5.

### 3.5 `parallel_apply/mod.rs` (core + register/insert/lookup)

What remains at the module root after the four extractions.

- **Item set:**
  - Module rustdoc (1-37, ~37 lines).
  - Imports (38-51).
  - `ParallelApplyError` + `From<ParallelApplyError> for io::Error` (53-139, ~87 lines).
  - `DeltaChunk` + constructors (141-223, ~83 lines).
  - `FileSlot` + impl (225-278, ~54 lines).
  - `ParallelDeltaApplier` struct + `Debug` impl (425-483, ~59 lines).
  - Core impl: `DEFAULT_PER_FILE_REORDER_CAPACITY`, `new`,
    `with_strategy`, `strategy`, `with_per_file_reorder_capacity`,
    `concurrency`, `register_file`, `bytes_written`, `slot_for`
    (485-586 + 678-689 + 835-848, ~115 lines).
  - `#[cfg(test)] mod tests` (903-1579, ~677 lines).

- **LoC in current file kept:** ~1112 production-stripped figure adds
  up as: 37 + 14 + 87 + 83 + 54 + 59 + 115 = 449 production +
  677 tests = ~1126 with whitespace + new `mod` declarations.
- **Estimated post-split LoC:** ~1140 (adds five `mod` declarations,
  two `use` statements pulling siblings back in for the inner test
  helpers, but loses the items moved out).

Note that the tests stay in `mod.rs` for now. Splitting them out is a
follow-up (SPL-38.f if scoped) that is independent of the production
moves and would dwarf the production diff if bundled here.

**Public items kept at the root:**

- `pub struct ParallelDeltaApplier`
- `pub enum ParallelApplyError`
- `pub struct DeltaChunk` (+ `literal`, `matched`,
  `with_expected_strong`)
- `pub fn ParallelDeltaApplier::new`
- `pub fn ParallelDeltaApplier::with_strategy`
- `pub fn ParallelDeltaApplier::strategy`
- `pub fn ParallelDeltaApplier::with_per_file_reorder_capacity`
- `pub fn ParallelDeltaApplier::concurrency`
- `pub fn ParallelDeltaApplier::register_file`
- `pub fn ParallelDeltaApplier::bytes_written`
- `impl From<ParallelApplyError> for io::Error`
- `impl std::fmt::Debug for ParallelDeltaApplier`

**Private items kept at the root:**

- `struct FileSlot` + `impl FileSlot` (`new`, `ingest`, `write_chunk`,
  `bytes_written`, `drained`).
- `fn slot_for(&self, ndx: FileNdx) -> io::Result<SlotHandle>` - kept
  here because every submodule that uses it (`batch.rs`, `drain.rs`
  via `finish_file`'s implicit invariant) goes through the DashMap
  field which also stays at the root. Marked `pub(super)` so siblings
  can call it.

**Cross-references that need rewiring (root side):**

| Reference | Action |
|-----------|--------|
| `use super::reorder::ReorderBuffer;` | Stays at root (`FileSlot` uses it). |
| `use super::types::FileNdx;` | Stays at root; siblings re-import from `super::super::types`. |
| `pub use parallel_apply::{...}` in `concurrent_delta/mod.rs` | Unchanged - the public names still live at `concurrent_delta::parallel_apply::*`. |

## 4. Estimated post-split LoC summary

| File | Estimated LoC |
|------|--------------:|
| `parallel_apply/mod.rs` (incl. tests) | ~1140 |
| `parallel_apply/slot_barrier.rs` | ~95 |
| `parallel_apply/decrement_guard.rs` | ~75 |
| `parallel_apply/batch.rs` | ~190 |
| `parallel_apply/drain.rs` | ~170 |
| **Total** | **~1670** |

The total grows by ~90 LoC (5.7%) over the current 1579 to cover
per-file rustdoc, imports, and the `mod` declaration overhead. The
production-only split (excluding the 677-line test module that stays
co-located with `mod.rs`) goes from a single 902-line file to a
distribution of 463 / 95 / 75 / 190 / 170 - none over 500 LoC.

If SPL-38.f later moves the tests out into a sibling
`parallel_apply/tests.rs` (or split per topic), `mod.rs` shrinks to
~450 LoC.

## 5. Coordination with DG-2.b (DG-3 restructure)

DG-2.a Option B (spec'd in
`docs/design/dg-2a-option-b-spec.md`) renames `SlotBarrier` to
`BarrierState`, introduces a sibling `SlotData` for the
`Mutex<FileSlot>` payload, and packages both into a `SlotEntry` value
stored in the DashMap. `DecrementGuard` keeps its `Arc<BarrierState>`
field; the DG-3 race fix lives entirely in field ordering and
`Arc::try_unwrap` against `SlotData` instead of `SlotBarrier`.

The SPL-38 boundaries below were chosen so DG-3 can land **before or
after** SPL-38.b/c/d/e without invalidating the split. The two changes
commute because:

- `SlotBarrier` (today) and `BarrierState` (post DG-3) have the same
  external surface from siblings' point of view:
  `lock_slot`/`increment_inflight`/`decrement_inflight`/`wait_until_idle`
  on `BarrierState`, plus a `lock_slot` adapter on `SlotData`. Both
  live in `slot_barrier.rs` after the split. The DG-3 patch is a
  single-file edit inside `slot_barrier.rs` plus the unwrap-site fix in
  `drain.rs`.
- `DecrementGuard` stays a one-field struct holding `Arc<BarrierState>`
  (today: `Arc<SlotBarrier>`). The Drop impl is unchanged.
  `decrement_guard.rs` does not need to be touched by DG-3.
- `SlotHandle` gains a new `data: Arc<SlotData>` field and its
  constructor signature changes from `fn new(barrier: Arc<SlotBarrier>)`
  to `fn new(entry: SlotEntry) -> Self`. Both signatures fit inside
  `decrement_guard.rs` and the call sites (`slot_for` at the root,
  `apply_one_chunk` in `batch.rs`, `apply_batch_parallel` in
  `batch.rs`) flip together in DG-3 without needing extra files.
- `finish_file` in `drain.rs` carries the workaround DG-1 s.5
  documents. DG-3 deletes lines 720-748 (the bounded spin loop) and
  replaces the `Arc::try_unwrap(slot_arc)` against `SlotBarrier` with
  the same call against `Arc<SlotData>`. The file boundary holds; the
  diff is contained.
- The DashMap field at the root flips from
  `DashMap<FileNdx, Arc<SlotBarrier>>` to
  `DashMap<FileNdx, SlotEntry>`. Either way the field lives in
  `mod.rs`. Siblings reach it via `pub(super)` access.

**Flagged seam (not invalidated, but worth sequencing):**

The one place where DG-3 and SPL-38 visibly interact is
`finish_file`'s spin loop in `drain.rs`. If SPL-38.e (drain
extraction) lands first, the spin loop moves verbatim into the new
file; DG-3 then deletes ~25 lines from `drain.rs` rather than from
`parallel_apply.rs`. If DG-3 lands first, SPL-38.e ships a smaller
`drain.rs` (~145 LoC instead of ~170). Either order works. The
recommended sequence is **DG-3 first**: it removes the spin-loop
workaround and one test scenario (`finish_file_calls_flush_workers_internally`,
1259-1297, which targets the race the spin loop hides), shrinking the
SPL-38.e diff and removing a comment block whose ownership would
otherwise straddle the file split.

**No invalidation:** no SPL-38 seam below cuts a structure that DG-3
adds or removes. The `SlotBarrier` -> `BarrierState`/`SlotData`/
`SlotEntry` migration is internal to `slot_barrier.rs` and the
DashMap field type at the root; neither crosses a file boundary
introduced by SPL-38.

## 6. Execution order for SPL-38.b/c/d/e

Recommended order so each PR compiles in isolation:

1. **SPL-38.b** - extract `slot_barrier.rs`. Foundational; everything
   depends on it.
2. **SPL-38.c** - extract `decrement_guard.rs`. Depends only on
   `slot_barrier.rs`.
3. **SPL-38.d** - extract `batch.rs`. Depends on `slot_for` (root) and
   `decrement_guard.rs`.
4. **SPL-38.e** - extract `drain.rs`. Depends on
   `SlotBarrier::wait_until_idle` and the root DashMap. The most
   self-contained of the four.

Steps 3 and 4 can run in either order or in parallel; they do not
share helpers. Each PR moves a single file and the matching `mod`
declaration; tests stay in `mod.rs` and exercise only the public
surface, so no test edits are required for any of the four moves.

## 7. Items deliberately kept at the root

- **`FileSlot`** - the `Mutex<FileSlot>` payload sits inside
  `SlotBarrier`, but the `FileSlot` *type* is constructed by
  `register_file` and consumed by `SlotBarrier::new`. Moving it into
  `slot_barrier.rs` would couple the slot's internal layout to the
  barrier; keeping it at the root lets `register_file` stay there
  unchanged and lets a future split (e.g. SPL-39 if scoped) extract
  the slot+reorder ingest path on its own terms.
- **`DeltaChunk`** - public type, returned by callers; it has no
  natural home in any submodule.
- **`ParallelApplyError`** - referenced by every submodule; lives at
  the root so siblings import it via `super::ParallelApplyError`.
- **`slot_for`** - the DashMap lookup is the bridge between the root
  field and the submodules. Keeping it at the root preserves the
  shard-discipline invariant documented at line 836 (drop the guard
  before any work longer than an `Arc::clone`).

## 8. Out of scope for SPL-38

- Moving the 677-line test module out of `mod.rs`. Deferred to
  SPL-38.f if scoped.
- Renaming any type. DG-3 owns the `SlotBarrier` -> `BarrierState`
  rename; SPL-38 only moves the existing types into files.
- Changing visibility from `pub` to `pub(super)` on items that are
  already private to the crate. The four submodules use `pub(super)`
  exclusively; nothing new becomes `pub`.
- Touching `FileSlot::ingest`, `write_chunk`, or the `ReorderBuffer`
  integration. The slot internals stay as-is.
