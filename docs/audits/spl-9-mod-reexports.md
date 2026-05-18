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
- **Current tree:** `origin/master` with SPL-2 (#4345, error), SPL-6
  (#4386, stats), SPL-3-tempfile (#4434, tempfile), and SPL-13 (#4462,
  buffer) merged. The 42-symbol public surface captured below remains
  unchanged; this document is the reference table every subsequent SPL
  PR must satisfy before merge.
- **Re-export anchor:** `crates/engine/src/concurrent_delta/mod.rs:185-201`
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

| Symbol                                        | Original path                                  | Current path                                                                                  | Status |
|-----------------------------------------------|------------------------------------------------|-----------------------------------------------------------------------------------------------|--------|
| `mod policy`                                  | `concurrent_delta/spill.rs:57`                 | `concurrent_delta/spill/policy.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `ReclaimMode` (re-export)                     | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill/mod.rs` (`pub use policy::ReclaimMode`)                               | OK     |
| `SpillCompression` (re-export)                | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill/mod.rs` (`pub use policy::SpillCompression`)                          | OK     |
| `SpillGranularity` (re-export)                | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill/mod.rs` (`pub use policy::SpillGranularity`)                          | OK     |
| `SpillPolicy` (re-export)                     | `concurrent_delta/spill.rs:58`                 | `concurrent_delta/spill/mod.rs` (`pub use policy::SpillPolicy`)                               | OK     |
| `DEFAULT_SPILL_THRESHOLD`                     | `concurrent_delta/spill.rs:64`                 | `concurrent_delta/spill/mod.rs:93`                                                            | OK     |
| `SpillError` (enum)                           | `concurrent_delta/spill.rs:83`                 | `concurrent_delta/spill/error.rs:30` (re-exported from `spill/mod.rs`)                        | OK     |
| `SpillError::Capacity` (variant)              | `concurrent_delta/spill.rs:85`                 | `concurrent_delta/spill/error.rs:32`                                                          | OK     |
| `SpillError::Io` (variant)                    | `concurrent_delta/spill.rs:87`                 | `concurrent_delta/spill/error.rs:34`                                                          | OK     |
| `SpillError::io_error`                        | `concurrent_delta/spill.rs:93`                 | `concurrent_delta/spill/error.rs:43`                                                          | OK     |
| `SpillError::is_out_of_space`                 | `concurrent_delta/spill.rs:102`                | `concurrent_delta/spill/error.rs:52`                                                          | OK     |
| `impl Display for SpillError`                 | `concurrent_delta/spill.rs:108`                | `concurrent_delta/spill/error.rs:58`                                                          | OK     |
| `impl Error for SpillError`                   | `concurrent_delta/spill.rs:117`                | `concurrent_delta/spill/error.rs:71`                                                          | OK     |
| `impl From<CapacityExceeded> for SpillError`  | `concurrent_delta/spill.rs:126`                | `concurrent_delta/spill/error.rs:80`                                                          | OK     |
| `impl From<io::Error> for SpillError`         | `concurrent_delta/spill.rs:132`                | `concurrent_delta/spill/error.rs:86`                                                          | OK     |
| `SpillCodec` (trait)                          | `concurrent_delta/spill.rs:144`                | `concurrent_delta/spill/mod.rs:101`                                                           | OK     |
| `SpillCodec::encode`                          | `concurrent_delta/spill.rs:150`                | `concurrent_delta/spill/mod.rs:107`                                                           | OK     |
| `SpillCodec::decode`                          | `concurrent_delta/spill.rs:157`                | `concurrent_delta/spill/mod.rs:114`                                                           | OK     |
| `SpillCodec::estimated_size`                  | `concurrent_delta/spill.rs:163`                | `concurrent_delta/spill/mod.rs:120`                                                           | OK     |
| `SpillableReorderBuffer<T>` (struct)          | `concurrent_delta/spill.rs:221`                | `concurrent_delta/spill/buffer.rs:75` (re-exported from `spill/mod.rs`)                       | OK     |
| `impl Debug for SpillableReorderBuffer<T>`    | `concurrent_delta/spill.rs:246`                | `concurrent_delta/spill/buffer.rs:133` (re-exported from `spill/mod.rs`)                      | OK     |
| `SpillStats` (struct)                         | `concurrent_delta/spill.rs:263`                | `concurrent_delta/spill/stats.rs:11` (re-exported from `spill/mod.rs`)                        | OK     |
| `SpillStats::spilled_items` (field)           | `concurrent_delta/spill.rs:265`                | `concurrent_delta/spill/stats.rs:13`                                                          | OK     |
| `SpillStats::spill_events` (field)            | `concurrent_delta/spill.rs:267`                | `concurrent_delta/spill/stats.rs:15`                                                          | OK     |
| `SpillStats::reload_events` (field)           | `concurrent_delta/spill.rs:269`                | `concurrent_delta/spill/stats.rs:17`                                                          | OK     |
| `SpillStats::memory_used` (field)             | `concurrent_delta/spill.rs:271`                | `concurrent_delta/spill/stats.rs:19`                                                          | OK     |
| `SpillStats::threshold` (field)               | `concurrent_delta/spill.rs:273`                | `concurrent_delta/spill/stats.rs:21`                                                          | OK     |
| `SpillStats::dir_recreate_events` (field)     | `concurrent_delta/spill.rs:275`                | `concurrent_delta/spill/stats.rs:23`                                                          | OK     |
| `SpillableReorderBuffer::new`                 | `concurrent_delta/spill.rs:289`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::with_spill_dir`      | `concurrent_delta/spill.rs:318`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::with_default_threshold` | `concurrent_delta/spill.rs:345`             | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::insert`              | `concurrent_delta/spill.rs:365`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::force_insert`        | `concurrent_delta/spill.rs:390`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::next_in_order`       | `concurrent_delta/spill.rs:415`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::drain_ready`         | `concurrent_delta/spill.rs:455`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::next_expected`       | `concurrent_delta/spill.rs:465`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::buffered_count`      | `concurrent_delta/spill.rs:471`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::is_empty`            | `concurrent_delta/spill.rs:477`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::capacity`            | `concurrent_delta/spill.rs:483`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::spill_stats`         | `concurrent_delta/spill.rs:489`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::threshold`           | `concurrent_delta/spill.rs:502`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |
| `SpillableReorderBuffer::spill_dir`           | `concurrent_delta/spill.rs:508`                | `concurrent_delta/spill/buffer.rs` (re-exported from `spill/mod.rs`)                          | OK     |

42 symbols audited. 42 OK. 0 MISSING. 0 RENAMED.

## Reachability from the parent facade

`crates/engine/src/concurrent_delta/mod.rs:198-201` re-exports the
following symbols at `crate::concurrent_delta::*`. Each appears in the
table above and must continue to resolve via the same `spill::` path:

- `ReclaimMode`, `SpillCompression`, `SpillGranularity`, `SpillPolicy`
  (through `spill::policy::`).
- `SpillCodec`, `SpillError`, `SpillStats`, `SpillableReorderBuffer`
  (through `spill::`).

## History

The original 1232-line `spill.rs` has been progressively decomposed into
focused submodules under `crates/engine/src/concurrent_delta/spill/`.
Each extraction PR re-ran the procedure below and kept the 42-symbol
table at OK:

- **SPL-2 error** ([#4345](https://github.com/oferchen/rsync/pull/4345)) -
  extracted `SpillError`, its `Display`/`Error`/`From` impls, and the
  helper methods (`io_error`, `is_out_of_space`) into `spill/error.rs`.
- **SPL-6 stats** ([#4386](https://github.com/oferchen/rsync/pull/4386)) -
  extracted `SpillStats` (plus its six public fields) into
  `spill/stats.rs`.
- **SPL-3-tempfile** ([#4434](https://github.com/oferchen/rsync/pull/4434)) -
  extracted the internal `SpillBackend` enum and `open_backend` helper
  into `spill/tempfile.rs`. Internal-only (`pub(super)`), so the public
  surface is unchanged.
- **SPL-13 buffer** ([#4462](https://github.com/oferchen/rsync/pull/4462)) -
  extracted `SpillableReorderBuffer<T>`, its `Debug` impl, and all 13
  inherent methods into `spill/buffer.rs`.

The codec extraction (SPL-3 codec, PR #4369) did not ship; `SpillCodec`
remains defined directly in `spill/mod.rs` and the trait, its three
methods, and `DEFAULT_SPILL_THRESHOLD` continue to live there.

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
   (`spill/buffer.rs`, `use engine::concurrent_delta::spill::SpillableReorderBuffer;`)
   compiles under `cargo test --doc -p engine --all-features`.

## Recommendations

No MISSING or RENAMED entries. The 42-symbol public API has survived
four extractions (SPL-2, SPL-6, SPL-3-tempfile, SPL-13) unchanged.
Future SPL PRs must amend the "Current path" column for any newly
extracted symbols while keeping the Status column at OK. Any divergence
from the 42-symbol baseline blocks merge.
