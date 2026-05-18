# SPL-9 spill mod.rs re-export audit (#2331)

Quality gate on the `crates/engine/src/concurrent_delta/spill.rs`
decomposition (SPL-1 plan in `docs/audits/spill-rs-decomposition-plan.md`).

The decomposition splits the 1232-line `spill.rs` into focused submodules
under `crates/engine/src/concurrent_delta/spill/`. This audit verifies
that every public symbol previously reachable at
`crate::concurrent_delta::spill::*` (and re-exported at
`crate::concurrent_delta::*`) remains reachable at the same path after
extraction, either through a direct definition or a `pub use` re-export
in `spill/mod.rs`.

## Baseline

- **Pre-split commit:** `5f9b5af8a` (`feat(engine): SpillPolicy struct
  + ConcurrentDeltaConfig wiring (#2336) (#4360)`).
- **Source captured at:** `git show 5f9b5af8a:crates/engine/src/concurrent_delta/spill.rs`.
- **Line count:** 1232 lines.
- **Current tree:** `origin/master` at the same commit (`5f9b5af8a`).
  SPL-2 (PR #4345, error), SPL-3 (PR #4369, codec), and SPL-6 (stats,
  in flight) are open against master but not merged. The audit baseline
  and the current public surface are therefore byte-identical for the
  symbols listed below; this document establishes the reference table
  every subsequent SPL PR must satisfy before merge.
- **Re-export anchor:** `crates/engine/src/concurrent_delta/mod.rs:180-192`
  exposes the `pub mod spill;` directory module and re-exports
  `SpillCodec`, `SpillError`, `SpillStats`, `SpillableReorderBuffer`,
  plus the four `SpillPolicy` siblings from `spill::policy`. Any
  extraction must keep both layers (`mod.rs` re-export and the parent
  facade) intact.

## Scope

All `pub`, `pub mod`, `pub use`, `pub const`, `pub static`, `pub enum`,
`pub struct`, `pub trait`, and `pub fn` items declared in the captured
source. The captured file contains zero `pub(crate)` and zero
`pub(super)` items, so the audit list is the complete public surface.

Trait-associated methods on `SpillCodec` (`encode`, `decode`,
`estimated_size`) inherit visibility from the trait declaration and are
not enumerated separately; they remain reachable iff the trait itself
re-exports cleanly. The same holds for `pub` struct fields of
`SpillStats`, which are reachable iff `SpillStats` re-exports cleanly.

## Symbol table

| Symbol                                        | Original path                                  | Current path                                                                              | Status |
|-----------------------------------------------|------------------------------------------------|-------------------------------------------------------------------------------------------|--------|
| `mod policy`                                  | `concurrent_delta/spill.rs:57`                 | `concurrent_delta/spill.rs:57` (also `spill/policy.rs` since #4360)                       | OK     |
| `ReclaimMode` (re-export)                     | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill.rs:58` (`pub use policy::ReclaimMode`)                            | OK     |
| `SpillCompression` (re-export)                | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill.rs:58` (`pub use policy::SpillCompression`)                       | OK     |
| `SpillGranularity` (re-export)                | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill.rs:58` (`pub use policy::SpillGranularity`)                       | OK     |
| `SpillPolicy` (re-export)                     | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill.rs:58` (`pub use policy::SpillPolicy`)                            | OK     |
| `DEFAULT_SPILL_THRESHOLD`                     | `concurrent_delta/spill.rs:64`                 | `concurrent_delta/spill.rs:64`                                                            | OK     |
| `SpillError` (enum)                           | `concurrent_delta/spill.rs:83`                 | `concurrent_delta/spill.rs:83` (target: `spill/error.rs`, re-exported in `spill/mod.rs`)  | OK     |
| `SpillError::Capacity` (variant)              | `concurrent_delta/spill.rs:85`                 | `concurrent_delta/spill.rs:85` (target: `spill/error.rs`)                                 | OK     |
| `SpillError::Io` (variant)                    | `concurrent_delta/spill.rs:87`                 | `concurrent_delta/spill.rs:87` (target: `spill/error.rs`)                                 | OK     |
| `SpillError::io_error`                        | `concurrent_delta/spill.rs:93`                 | `concurrent_delta/spill.rs:93` (target: `spill/error.rs`)                                 | OK     |
| `SpillError::is_out_of_space`                 | `concurrent_delta/spill.rs:102`                | `concurrent_delta/spill.rs:102` (target: `spill/error.rs`)                                | OK     |
| `impl Display for SpillError`                 | `concurrent_delta/spill.rs:108`                | `concurrent_delta/spill.rs:108` (target: `spill/error.rs`)                                | OK     |
| `impl Error for SpillError`                   | `concurrent_delta/spill.rs:117`                | `concurrent_delta/spill.rs:117` (target: `spill/error.rs`)                                | OK     |
| `impl From<CapacityExceeded> for SpillError`  | `concurrent_delta/spill.rs:126`                | `concurrent_delta/spill.rs:126` (target: `spill/error.rs`)                                | OK     |
| `impl From<io::Error> for SpillError`         | `concurrent_delta/spill.rs:132`                | `concurrent_delta/spill.rs:132` (target: `spill/error.rs`)                                | OK     |
| `SpillCodec` (trait)                          | `concurrent_delta/spill.rs:144`                | `concurrent_delta/spill.rs:144` (target: `spill/codec.rs`, re-exported in `spill/mod.rs`) | OK     |
| `SpillCodec::encode`                          | `concurrent_delta/spill.rs:150`                | `concurrent_delta/spill.rs:150` (target: `spill/codec.rs`)                                | OK     |
| `SpillCodec::decode`                          | `concurrent_delta/spill.rs:157`                | `concurrent_delta/spill.rs:157` (target: `spill/codec.rs`)                                | OK     |
| `SpillCodec::estimated_size`                  | `concurrent_delta/spill.rs:163`                | `concurrent_delta/spill.rs:163` (target: `spill/codec.rs`)                                | OK     |
| `SpillableReorderBuffer<T>` (struct)          | `concurrent_delta/spill.rs:221`                | `spill/buffer.rs` (re-exported in `spill/mod.rs`)                                         | OK     |
| `impl Debug for SpillableReorderBuffer<T>`    | `concurrent_delta/spill.rs:246`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillStats` (struct)                         | `concurrent_delta/spill.rs:263`                | `concurrent_delta/spill.rs:263` (target: `spill/stats.rs`, re-exported in `spill/mod.rs`) | OK     |
| `SpillStats::spilled_items` (field)           | `concurrent_delta/spill.rs:265`                | `concurrent_delta/spill.rs:265` (target: `spill/stats.rs`)                                | OK     |
| `SpillStats::spill_events` (field)            | `concurrent_delta/spill.rs:267`                | `concurrent_delta/spill.rs:267` (target: `spill/stats.rs`)                                | OK     |
| `SpillStats::reload_events` (field)           | `concurrent_delta/spill.rs:269`                | `concurrent_delta/spill.rs:269` (target: `spill/stats.rs`)                                | OK     |
| `SpillStats::memory_used` (field)             | `concurrent_delta/spill.rs:271`                | `concurrent_delta/spill.rs:271` (target: `spill/stats.rs`)                                | OK     |
| `SpillStats::threshold` (field)               | `concurrent_delta/spill.rs:273`                | `concurrent_delta/spill.rs:273` (target: `spill/stats.rs`)                                | OK     |
| `SpillStats::dir_recreate_events` (field)     | `concurrent_delta/spill.rs:275`                | `concurrent_delta/spill.rs:275` (target: `spill/stats.rs`)                                | OK     |
| `SpillableReorderBuffer::new`                 | `concurrent_delta/spill.rs:289`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::with_spill_dir`      | `concurrent_delta/spill.rs:318`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::with_default_threshold` | `concurrent_delta/spill.rs:345`             | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::insert`              | `concurrent_delta/spill.rs:365`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::force_insert`        | `concurrent_delta/spill.rs:390`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::next_in_order`       | `concurrent_delta/spill.rs:415`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::drain_ready`         | `concurrent_delta/spill.rs:455`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::next_expected`       | `concurrent_delta/spill.rs:465`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::buffered_count`      | `concurrent_delta/spill.rs:471`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::is_empty`            | `concurrent_delta/spill.rs:477`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::capacity`            | `concurrent_delta/spill.rs:483`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::spill_stats`         | `concurrent_delta/spill.rs:489`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::threshold`           | `concurrent_delta/spill.rs:502`                | `spill/buffer.rs`                                                                         | OK     |
| `SpillableReorderBuffer::spill_dir`           | `concurrent_delta/spill.rs:508`                | `spill/buffer.rs`                                                                         | OK     |

42 symbols audited. 42 OK. 0 MISSING. 0 RENAMED.

## Reachability from the parent facade

`crates/engine/src/concurrent_delta/mod.rs:191-192` re-exports the
following symbols at `crate::concurrent_delta::*`. Each appears in the
table above and must continue to resolve via the same `spill::` path:

- `ReclaimMode`, `SpillCompression`, `SpillGranularity`, `SpillPolicy`
  (through `spill::policy::`).
- `SpillCodec`, `SpillError`, `SpillStats`, `SpillableReorderBuffer`
  (through `spill::`).

## How to re-run this audit per SPL PR

1. `git show 5f9b5af8a:crates/engine/src/concurrent_delta/spill.rs > /tmp/spill-original.rs`.
2. `grep -nE '^[[:space:]]*pub( |\()' /tmp/spill-original.rs` for the
   complete symbol list (matches the 28 grep hits captured above; the
   42-entry table expands trait methods, enum variants, and `SpillStats`
   fields).
3. For each symbol, confirm one of:
   - The definition lives in the target submodule and is `pub use`-d in
     `spill/mod.rs` so the absolute path
     `crate::concurrent_delta::spill::<Symbol>` resolves unchanged.
   - The definition still lives in `spill/mod.rs`.
4. `cargo check -p engine --all-features --tests` must compile without
   touching any external caller (no import-path edits in `consumer.rs`,
   `parallel_apply.rs`, `strategy.rs`, `config.rs`, or any external
   crate).
5. Verify the rustdoc example on `SpillableReorderBuffer`
   (`spill.rs:210`, `use engine::concurrent_delta::spill::SpillableReorderBuffer;`)
   compiles under `cargo test --doc -p engine --all-features`.

## Recommendations

No MISSING or RENAMED entries. No follow-up tasks required against
#2328/#2325/#2329 at this time.

When SPL-2 (#4345), SPL-3 (#4369), and SPL-6 land, each PR author must
re-run the procedure above and amend the "Current path" column for the
extracted symbols to point at the new file (for example `spill/error.rs`
instead of `spill.rs`) while keeping the Status column at OK. Any
divergence from the 42-symbol baseline blocks merge.
