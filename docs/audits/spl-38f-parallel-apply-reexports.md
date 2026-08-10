# SPL-38.f parallel_apply mod.rs re-export audit

Quality gate on the
`crates/engine/src/concurrent_delta/parallel_apply.rs` decomposition
(SPL-38.a plan; SPL-38.b..e mechanical extractions).

The decomposition splits the original single-file
`parallel_apply.rs` into a directory module
`crates/engine/src/concurrent_delta/parallel_apply/` with the
following layout:

| File                  | Source of the contents                                                       |
|-----------------------|------------------------------------------------------------------------------|
| `mod.rs`              | `ParallelDeltaApplier` struct, `ParallelApplyError`, `DeltaChunk`, `FileSlot`, `SlotHandle`, glue + `#[cfg(test)] mod tests` |
| `slot_barrier.rs`     | `SlotBarrier` + `BarrierState` (SPL-38.b)                                    |
| `decrement_guard.rs`  | `DecrementGuard` RAII helper (SPL-38.c)                                      |
| `batch.rs`            | `ParallelDeltaApplier::apply_batch_parallel` + its rayon-based dispatch (SPL-38.d) |
| `drain.rs`            | `ParallelDeltaApplier::finish_file` and `flush_workers` (SPL-38.e)           |

This audit verifies that **every public symbol previously reachable
through `engine::concurrent_delta::parallel_apply::*`** (and the
re-exports at `engine::concurrent_delta::*`) remains reachable at the
same path after extraction, either through a direct definition in
`parallel_apply/mod.rs` or through an inherent `impl ParallelDeltaApplier`
block in a sibling submodule whose methods are still resolved at the
applier struct's existing path.

## Baseline

- **Pre-split commit:** `16d59215a` (parent of `4825da4c8`, the
  SPL-38.b "extract SlotBarrier into submodule" commit).
- **Pre-split source captured at:**
  `git show 16d59215a:crates/engine/src/concurrent_delta/parallel_apply.rs`
  (1,579 lines, single file).
- **Current tree:** `origin/master` with SPL-38.b (#4791, SlotBarrier),
  SPL-38.c (#4795, DecrementGuard), SPL-38.d (#4811, apply_batch_parallel)
  and SPL-38.e (#4815, finish_file + flush_workers) merged.
- **Re-export anchor:**
  `crates/engine/src/concurrent_delta/mod.rs:179-193` declares
  `pub mod parallel_apply;` and re-exports
  `DeltaChunk`, `ParallelApplyError`, `ParallelDeltaApplier` to the
  parent facade. Both lines are gated by
  `#[cfg(feature = "parallel-receive-delta")]`. Any extraction must
  keep both layers intact (`parallel_apply/mod.rs` direct/re-exported
  symbol + parent `pub use`).

## Scope

All `pub`, `pub(crate)`, `pub fn`, `pub const`, `pub enum`,
`pub struct`, and `pub` struct fields declared in the captured
pre-split source. Sibling submodules
(`slot_barrier`, `decrement_guard`, `batch`, `drain`) are declared as
private `mod` in `mod.rs:52-55`, so nothing inside them leaks unless
re-exported - that fact is itself part of the audit (no submodule
contents should escape).

`#[cfg(test)] mod tests` items and intra-submodule `pub(super)` items
(e.g. `SlotBarrier::lock_slot`, `DecrementGuard::Drop`) are explicitly
out of scope: they are private to the `parallel_apply` directory and
cannot be reached by external consumers.

## Public-API surface after the split

Captured from
`crates/engine/src/concurrent_delta/parallel_apply/mod.rs` plus the two
inherent-impl-bearing siblings (`batch.rs`, `drain.rs`):

### Types and free items in `parallel_apply/mod.rs`

| Kind     | Symbol                         | Defined at                                            | Notes                                                                 |
|----------|--------------------------------|-------------------------------------------------------|-----------------------------------------------------------------------|
| `pub enum`   | `ParallelApplyError`        | `mod.rs:73`                                           | Variants unchanged.                                                   |
| `pub struct` | `DeltaChunk`                | `mod.rs:159`                                          | Fields unchanged: `pub ndx`, `pub chunk_sequence`, `pub data`, `pub is_literal`, `pub expected_strong`. |
| `pub struct` | `ParallelDeltaApplier`      | `mod.rs:350`                                          | Field layout unchanged; opaque to consumers (no `pub` fields).        |
| `impl From<ParallelApplyError> for io::Error` | (trait impl)        | `mod.rs:138`                                          | Required by callers that bubble errors through `io::Error`.           |

### `DeltaChunk` inherent methods (`impl DeltaChunk` at `mod.rs:192`)

| Visibility | Method                        | Pre-split location (pre_split:line) | Post-split location (`parallel_apply/`) |
|------------|-------------------------------|-------------------------------------|-----------------------------------------|
| `pub fn`   | `literal`                     | `pre_split:188`                     | `mod.rs:195`                            |
| `pub fn`   | `matched`                     | `pre_split:200`                     | `mod.rs:207`                            |
| `pub fn`   | `with_expected_strong`        | `pre_split:219`                     | `mod.rs:226`                            |

### `ParallelDeltaApplier` inherent surface

Split across three `impl ParallelDeltaApplier` blocks: the main one in
`mod.rs:399`, plus `batch.rs:32` (single-method) and `drain.rs:30`
(two-method). All three resolve to the same nominal type, so the
public path `ParallelDeltaApplier::<method>` is unaffected.

| Visibility | Item                                       | Pre-split location | Post-split location                              |
|------------|--------------------------------------------|--------------------|--------------------------------------------------|
| `pub const`| `DEFAULT_PER_FILE_REORDER_CAPACITY: usize` | `pre_split:489`    | `mod.rs:403`                                     |
| `pub fn`   | `new`                                      | `pre_split:501`    | `mod.rs:415`                                     |
| `pub fn`   | `with_strategy`                            | `pre_split:516`    | `mod.rs:430`                                     |
| `pub fn`   | `strategy`                                 | `pre_split:530`    | `mod.rs:444`                                     |
| `pub fn`   | `with_per_file_reorder_capacity`           | `pre_split:540`    | `mod.rs:454`                                     |
| `pub fn`   | `concurrency`                              | `pre_split:548`    | `mod.rs:462`                                     |
| `pub fn`   | `register_file`                            | `pre_split:562`    | `mod.rs:476`                                     |
| `pub fn`   | `apply_one_chunk`                          | `pre_split:618`    | `mod.rs:532`                                     |
| `pub fn`   | `apply_batch_parallel`                     | `pre_split:650`    | `batch.rs:45`                                    |
| `pub fn`   | `bytes_written`                            | `pre_split:684`    | `mod.rs:558`                                     |
| `pub fn`   | `finish_file`                              | `pre_split:703`    | `drain.rs:43`                                    |
| `pub fn`   | `flush_workers`                            | `pre_split:794`    | `drain.rs:137`                                   |
| `pub fn`   | `drain_inflight`                           | `pre_split:825`    | `mod.rs:583`                                     |

### Submodules (private; no leak surface)

| Decl in `mod.rs`            | Items inside                        | Visibility of those items | Externally reachable? |
|-----------------------------|-------------------------------------|----------------------------|-----------------------|
| `mod batch;` (line 52)      | `apply_batch_parallel` (inherent on `ParallelDeltaApplier`) | `pub` method on existing public type | Yes, through the existing applier path |
| `mod decrement_guard;` (53) | `pub(super) struct DecrementGuard`, `impl Drop` | `pub(super)` | No |
| `mod drain;` (54)           | `finish_file`, `flush_workers` (inherent on `ParallelDeltaApplier`) | `pub` methods on existing public type | Yes, through the existing applier path |
| `mod slot_barrier;` (55)    | `pub(super) struct SlotBarrier`, `pub(super) fn` methods | `pub(super)` | No |

`use decrement_guard::DecrementGuard;` (line 57) and
`use slot_barrier::SlotBarrier;` (line 58) bring the helper types
back into `mod.rs` scope without re-exporting them. They remain
crate-private.

## Side-by-side: pre-split vs post-split public surface

| Public item                                                              | Pre-split path                                                              | Post-split path                                                                                          | Status |
|--------------------------------------------------------------------------|-----------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------|--------|
| `enum ParallelApplyError`                                                | `concurrent_delta::parallel_apply::ParallelApplyError`                      | `concurrent_delta::parallel_apply::ParallelApplyError` (defined in `parallel_apply/mod.rs:73`)           | OK     |
| `From<ParallelApplyError> for io::Error`                                 | `concurrent_delta::parallel_apply` (impl)                                   | `concurrent_delta::parallel_apply` (impl at `parallel_apply/mod.rs:138`)                                 | OK     |
| `struct DeltaChunk` + `pub` fields                                       | `concurrent_delta::parallel_apply::DeltaChunk`                              | `concurrent_delta::parallel_apply::DeltaChunk` (defined in `parallel_apply/mod.rs:159`, fields unchanged) | OK     |
| `DeltaChunk::literal`                                                    | inherent                                                                    | inherent (`parallel_apply/mod.rs:195`)                                                                    | OK     |
| `DeltaChunk::matched`                                                    | inherent                                                                    | inherent (`parallel_apply/mod.rs:207`)                                                                    | OK     |
| `DeltaChunk::with_expected_strong`                                       | inherent                                                                    | inherent (`parallel_apply/mod.rs:226`)                                                                    | OK     |
| `struct ParallelDeltaApplier`                                            | `concurrent_delta::parallel_apply::ParallelDeltaApplier`                    | `concurrent_delta::parallel_apply::ParallelDeltaApplier` (defined in `parallel_apply/mod.rs:350`)        | OK     |
| `ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY`                | inherent const                                                              | inherent const (`parallel_apply/mod.rs:403`)                                                              | OK     |
| `ParallelDeltaApplier::new`                                              | inherent                                                                    | inherent (`parallel_apply/mod.rs:415`)                                                                    | OK     |
| `ParallelDeltaApplier::with_strategy`                                    | inherent                                                                    | inherent (`parallel_apply/mod.rs:430`)                                                                    | OK     |
| `ParallelDeltaApplier::strategy`                                         | inherent                                                                    | inherent (`parallel_apply/mod.rs:444`)                                                                    | OK     |
| `ParallelDeltaApplier::with_per_file_reorder_capacity`                   | inherent                                                                    | inherent (`parallel_apply/mod.rs:454`)                                                                    | OK     |
| `ParallelDeltaApplier::concurrency`                                      | inherent                                                                    | inherent (`parallel_apply/mod.rs:462`)                                                                    | OK     |
| `ParallelDeltaApplier::register_file`                                    | inherent                                                                    | inherent (`parallel_apply/mod.rs:476`)                                                                    | OK     |
| `ParallelDeltaApplier::apply_one_chunk`                                  | inherent                                                                    | inherent (`parallel_apply/mod.rs:532`)                                                                    | OK     |
| `ParallelDeltaApplier::apply_batch_parallel`                             | inherent                                                                    | inherent (`parallel_apply/batch.rs:45`)                                                                   | OK     |
| `ParallelDeltaApplier::bytes_written`                                    | inherent                                                                    | inherent (`parallel_apply/mod.rs:558`)                                                                    | OK     |
| `ParallelDeltaApplier::finish_file`                                      | inherent                                                                    | inherent (`parallel_apply/drain.rs:43`)                                                                   | OK     |
| `ParallelDeltaApplier::flush_workers`                                    | inherent                                                                    | inherent (`parallel_apply/drain.rs:137`)                                                                  | OK     |
| `ParallelDeltaApplier::drain_inflight`                                   | inherent                                                                    | inherent (`parallel_apply/mod.rs:583`)                                                                    | OK     |
| Parent-facade re-export `pub use parallel_apply::{DeltaChunk, ParallelApplyError, ParallelDeltaApplier};` | `concurrent_delta::mod.rs:193` (pre-split) | `concurrent_delta::mod.rs:193` (post-split, identical line)                                              | OK     |

No public symbol is renamed, removed, repathed, narrowed in
visibility, or changed in signature. Submodule contents that did not
previously exist (`SlotBarrier`, `DecrementGuard`) remain `pub(super)`
and therefore add zero new public surface area.

## External consumer call sites

Located via `grep -rn` across `crates/` for the symbol names listed
above plus the import prefix `parallel_apply::`. Excludes references
inside `concurrent_delta/parallel_apply/` itself.

| Call site (`file:line`)                                                                                  | Symbol used                                | Now resolves through                                                          |
|----------------------------------------------------------------------------------------------------------|--------------------------------------------|-------------------------------------------------------------------------------|
| `crates/engine/src/concurrent_delta/mod.rs:193`                                                          | `parallel_apply::{DeltaChunk, ParallelApplyError, ParallelDeltaApplier}` (`pub use`) | `parallel_apply/mod.rs` (all three defined there)                             |
| `crates/engine/src/concurrent_delta/chunk_adapter.rs:70`                                                 | `super::parallel_apply::DeltaChunk` (`use`) | `parallel_apply/mod.rs:159`                                                   |
| `crates/transfer/src/delta_pipeline/chunk_builder.rs:50`                                                 | `engine::concurrent_delta::{DeltaChunk, FileNdx}` (`use`) | parent-facade `pub use` -> `parallel_apply/mod.rs:159`                        |
| `crates/transfer/src/delta_pipeline/chunk_builder.rs:147-149`                                            | `DeltaChunk::literal`                      | `parallel_apply/mod.rs:195`                                                   |
| `crates/transfer/src/delta_pipeline/chunk_builder.rs:179-198`                                            | `DeltaChunk::matched`, `with_expected_strong` | `parallel_apply/mod.rs:207`, `:226`                                           |
| `crates/transfer/src/delta_pipeline/chunk_builder.rs:288, 399`                                           | `ParallelDeltaApplier`, `with_strategy`    | `parallel_apply/mod.rs:350`, `:430`                                           |
| `crates/engine/tests/parallel_apply_concurrent.rs:30`                                                    | `engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier}` (`use`) | parent-facade `pub use`                                                       |
| `crates/engine/tests/parallel_apply_concurrent.rs:90, 188`                                               | `ParallelDeltaApplier::new`                | `parallel_apply/mod.rs:415`                                                   |
| `crates/engine/tests/parallel_apply_concurrent.rs:123, 241`                                              | `DeltaChunk::literal`                      | `parallel_apply/mod.rs:195`                                                   |
| `crates/engine/tests/parallel_apply_concurrent.rs:169, 272`                                              | `ParallelDeltaApplier::finish_file`        | `parallel_apply/drain.rs:43` (inherent on existing type)                      |
| `crates/engine/benches/parallel_verify_chunk.rs:79`                                                      | `engine::concurrent_delta::{DeltaChunk, FileNdx, ParallelDeltaApplier}` (`use`) | parent-facade `pub use`                                                       |
| `crates/engine/benches/parallel_verify_chunk.rs:146, 169`                                                | `DeltaChunk::literal`                      | `parallel_apply/mod.rs:195`                                                   |
| `crates/engine/benches/parallel_verify_chunk.rs:206`                                                     | `ParallelDeltaApplier::apply_batch_parallel` | `parallel_apply/batch.rs:45` (inherent on existing type)                      |
| `crates/engine/benches/parallel_verify_chunk.rs:213, 264`                                                | `ParallelDeltaApplier::finish_file`, `with_strategy` | `parallel_apply/drain.rs:43`, `parallel_apply/mod.rs:430`                     |
| `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:87`                                        | `engine::concurrent_delta::{DeltaChunk, FileNdx, ParallelDeltaApplier}` (`use`) | parent-facade `pub use`                                                       |
| `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:168, 191`                                  | `DeltaChunk::literal`                      | `parallel_apply/mod.rs:195`                                                   |
| `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:230`                                       | `ParallelDeltaApplier::apply_batch_parallel` | `parallel_apply/batch.rs:45`                                                  |
| `crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:237, 292, 352`                             | `ParallelDeltaApplier::finish_file`, `with_strategy` | `parallel_apply/drain.rs:43`, `parallel_apply/mod.rs:430`                     |
| `crates/engine/benches/parallel_receive_delta_perf.rs:72`                                                | `engine::concurrent_delta::{DeltaChunk, FileNdx, ParallelDeltaApplier}` (`use`) | parent-facade `pub use`                                                       |
| `crates/engine/benches/parallel_receive_delta_perf.rs:142-144`                                           | `DeltaChunk::matched`, `DeltaChunk::literal` | `parallel_apply/mod.rs:207`, `:195`                                           |
| `crates/engine/benches/parallel_receive_delta_perf.rs:256`                                               | `ParallelDeltaApplier::new`                | `parallel_apply/mod.rs:415`                                                   |
| `crates/engine/benches/parallel_receive_delta_perf.rs:263`                                               | `ParallelDeltaApplier::apply_batch_parallel` | `parallel_apply/batch.rs:45`                                                  |
| `crates/engine/benches/parallel_receive_delta_perf.rs:274`                                               | `ParallelDeltaApplier::finish_file`        | `parallel_apply/drain.rs:43`                                                  |

Every consumer continues to import the same symbol from the same path
it did before SPL-38.b. The submodule a method now physically lives
in (`batch.rs` for `apply_batch_parallel`, `drain.rs` for
`finish_file` / `flush_workers`) is invisible to callers because each
sibling adds methods to the same nominal `impl ParallelDeltaApplier`
block.

No consumer references `SlotBarrier`, `DecrementGuard`,
`BarrierState`, or anything else introduced by the split as a named
import. The only mention of `SlotBarrier` outside `parallel_apply/`
is in a `//!`-doc inside
`crates/engine/benches/br_3j_f_dashmap_cores_vs_throughput.rs:40`, and
in `parallel_apply/decrement_guard.rs` itself (intra-tree). Both are
prose-only and do not require the symbol to be in scope.

## Verdict

**Zero public-API drift.** The SPL-38.b..e mechanical extractions
preserve every public type, every inherent method, every `pub` field,
the inherent `pub const`, and the parent-facade `pub use` line
byte-for-byte at the same external path. All 19 external consumer
sites continue to resolve through unchanged import statements.

The two helper types introduced by the split (`SlotBarrier`,
`DecrementGuard`) and the new submodules (`batch`, `decrement_guard`,
`drain`, `slot_barrier`) are intentionally scoped to `pub(super)` /
private `mod`, so they add no public surface area and cannot be
imported by external crates - confirmed by `grep -rn` returning zero
named imports of either symbol outside `parallel_apply/`.

## Follow-ups

None. The split is a no-op for consumers and no drift was found, so
nothing needs back-fill. Future submodule extractions under
`parallel_apply/` should follow the same convention:

1. Move helper types as `pub(super)` only.
2. Add public methods via additional `impl ParallelDeltaApplier`
   blocks in the new submodule, so the existing
   `concurrent_delta::parallel_apply::ParallelDeltaApplier::<method>`
   path keeps resolving.
3. Re-run this audit's `grep -rn` checks before merging.
