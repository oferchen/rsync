# FFL-7: `flat-flist` cfg-site inventory

Date: 2026-06-19
Scope: every `#[cfg(feature = "flat-flist")]`, `#[cfg(all(test, feature = "flat-flist"))]`, `#[cfg(feature = "flat-flist-rayon")]`, and `#![cfg(feature = "flat-flist")]` site in the workspace.
Feeds into: FFL-FLIP.6 dual-path removal (tasks FFL-8 through FFL-15).

## 1. Summary

The `flat-flist` Cargo feature was flipped default-on in FFL-FLIP.4 (`crates/protocol/Cargo.toml:19` now reads `default = ["zlib-ng", "flat-flist"]`). FFL-7 inventories every conditional compilation site so subsequent removal PRs can be scoped to single concerns.

| Metric | Count |
|---|---|
| Total `cfg(feature = "flat-flist")` attribute sites | 38 |
| Total `cfg(all(test, feature = "flat-flist"))` sites | 1 |
| Total `cfg(feature = "flat-flist-rayon")` attribute sites | 3 |
| Total `#![cfg(feature = "flat-flist")]` inner attribute sites (whole-file gates) | 1 |
| Total `cfg(not(feature = "flat-flist"))` sites | **0** |
| Crates touched | 4 (`protocol`, `transfer`, `engine`, `filters`) |
| Cargo.toml feature declarations | 4 (`protocol`, `transfer`, `engine`, `filters`) |

There is **no `cfg(not(feature = "flat-flist"))` branch anywhere** in the codebase. Every site is purely additive: the legacy `Vec<FileEntry>` path stays compiled unconditionally and the `flat-flist` cfg only gates *additional* arena-backed code. This means FFL-FLIP.6 removal is a straight delete of cfg attributes plus their guarded items - there is no `#[cfg(not(...))]` branch to also collapse.

The `flat-flist-rayon` feature is a separate, opt-in parallel builder. It is **not** flipped default-on and is out of scope for FFL-7..15. The 3 sites are listed here for completeness only.

## 2. Cargo.toml declarations

| File | Line | Declaration |
|---|---|---|
| `crates/protocol/Cargo.toml` | 19 | `default = ["zlib-ng", "flat-flist"]` (flipped on by FFL-FLIP.4) |
| `crates/protocol/Cargo.toml` | 38 | `flat-flist = []` |
| `crates/protocol/Cargo.toml` | 41 | `flat-flist-rayon = ["flat-flist", "dep:rayon"]` (out of scope) |
| `crates/protocol/Cargo.toml` | 97 | `required-features = ["flat-flist"]` (bench target) |
| `crates/filters/Cargo.toml` | 25 | `flat-flist = ["dep:protocol", "protocol/flat-flist"]` |
| `crates/transfer/Cargo.toml` | 75 | `flat-flist = ["protocol/flat-flist"]` |
| `crates/engine/Cargo.toml` | 108 | `flat-flist = ["protocol/flat-flist"]` |

## 3. `crates/protocol` (29 sites)

The protocol crate owns the arena types (`FlatFileList`, `FileEntryHeader`, `PathArena`, `ExtrasArena`) and the `DualFileList` wrapper that fans pushes into both stores.

### 3.1 `crates/protocol/src/flist/mod.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 38 | active | `mod flat` declaration | Gates the entire `flat` submodule tree (header, intern, extras, flist, parallel_builder, tests). |
| 60 | active | `pub use flat::{...}` re-export | Re-exports 13 flat types: `EXTRA_*`, `ExtrasArena`, `ExtrasError`, `ExtrasRef`, `FileEntryHeader`, `FlatExtras`, `FlatFileEntry`, `FlatFileList`, `PRESENT_*`, `PathArena`, `PathHandle`, `Segment`. |
| 68 | active | `pub use flat::{...}` re-export (rayon) | Re-exports `ParallelFlatFileListBuilder`, `extend_from`. Gated on `flat-flist-rayon`, NOT in scope for FFL-7..15. |
| 87 | active | `pub use sort::{...}` re-export | Re-exports `compare_entries_generic`, `sort_entries_generic`. |

### 3.2 `crates/protocol/src/flist/dual.rs`

`DualFileList` is the wrapper that fans every `push` into both the legacy `Vec<FileEntry>` and the arena-backed `FlatFileList`. After FFL-FLIP.6 it should become unconditionally dual-backed; many of these cfg attributes can be removed by simply deleting the attribute and keeping the inner code.

| Line | Branch | Kind | Description |
|---|---|---|---|
| 21 | active | `use` import | Pulls in `FileEntryHeader`, `FlatExtras`, `FlatFileList`, and `PRESENT_*` constants from `super::flat`. |
| 39 | active | struct field | `flat: FlatFileList` field on `DualFileList`. |
| 57 | active | struct literal field init in `new()` | Initializes `flat: FlatFileList::new()`. |
| 67 | active | struct literal field init in `with_capacity()` | Initializes `flat: FlatFileList::with_capacity(cap)`. |
| 78 | active | block inside `push()` | Converts the `FileEntry` to a flat header + extras and appends via `flat.push_with_extras`. |
| 130 | active | block inside `clear()` | Replaces `self.flat` with a fresh `FlatFileList::new()` instead of relying on `Vec::clear` semantics. |
| 146 | active | inherent method `flat(&self)` | Read accessor returning `&FlatFileList`. |
| 156 | active | inherent method `extras(&self)` | Read accessor returning `&ExtrasArena`. |
| 299 | active | free fn `file_entry_to_flat()` | Converts a `&FileEntry` into a `(FileEntryHeader, FlatExtras)` pair, interning paths through the arena. |
| 361 | active | free fn `build_flat_extras()` | Packs optional fields (link target, rdev, hardlink idx, ACL/xattr ndx, checksum, user/group names, atime/crtime) into a `FlatExtras`. |
| 616 | active | nested `mod flat_tests` inside `#[cfg(test)]` | Unit tests asserting flat accessors observe the same data as the legacy Vec. |

### 3.3 `crates/protocol/src/flist/sort.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 28 | active | `use` import | Pulls in `FileEntryAccessor` trait used by generic comparators. |
| 58 | active | `impl SortKey` method `from_accessor()` | Generic counterpart of `SortKey::new` that accepts any `FileEntryAccessor`. |
| 472 | active | pub fn `compare_entries_generic()` | Generic counterpart of `compare_file_entries` for both `FileEntry` and `FlatFileEntry`. |
| 490 | active | pub fn `sort_entries_generic()` | Generic counterpart of `sort_file_list` for both backing types. |
| 991 | active | nested `mod generic_tests` inside `#[cfg(test)]` | Tests asserting `compare_entries_generic` and `sort_entries_generic` produce identical results to the concrete versions. |

### 3.4 `crates/protocol/src/flist/accessor.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 319 | active | `mod flat_impl` | Submodule containing the `FileEntryAccessor` impl for `FlatFileEntry`. |
| 649 | active | nested `mod flat_tests` inside `#[cfg(test)]` | Tests asserting `FlatFileEntry` as a `&dyn FileEntryAccessor` returns the same values as the equivalent `FileEntry`. |

### 3.5 `crates/protocol/src/flist/flat/mod.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 20 | active | `mod parallel_builder` declaration | Out of scope (rayon-only). |
| 37 | active | `pub use parallel_builder::{...}` re-export | Out of scope (rayon-only). |

### 3.6 `crates/protocol/tests/flat_flist_transfer_regression.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 11 | active | inner attribute `#![cfg(...)]` | Gates the **entire test file** (RSS-A.7.i full-transfer regression test). |

## 4. `crates/transfer` (10 sites)

The transfer crate (generator + receiver) holds the trait-generic versions of itemize, filter, and sender helpers.

### 4.1 `crates/transfer/src/generator/mod.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 83 | active | `pub mod entry_accessor` | Gates the generator-side `FileEntryAccessor`-generic module (RSS-A.7.e). |
| 93 | active | `pub mod sender_accessor` | Gates the sender-side trait-generic helpers (itemize, skip, display name). |

### 4.2 `crates/transfer/src/generator/sender_accessor.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 20 | active | `use` import | `use protocol::flist::FileEntryAccessor;`. |
| 23 | active | `use` import | `use super::item_flags::ItemFlags;`. |
| 25 | active | `use` import | `use super::itemize::ItemizeContext;`. |
| 41 | active | pub fn `format_iflags_generic()` | Trait-generic 11-character `YXcstpoguax` itemize string. |
| 196 | active | pub fn `format_itemize_line_generic()` | Trait-generic `"%i %n%L\n"` itemize output line. |
| 235 | active | pub fn `entry_display_name()` | Trait-generic directory-aware display name with trailing `/`. |
| 254 | active | pub fn `should_skip_entry()` | Trait-generic sender-side `!is_file()` filter. |
| 259 | active | `mod tests` (compound: `all(test, feature = "flat-flist")`) | Unit tests for the four generic helpers above. |

### 4.3 `crates/transfer/src/receiver/mod.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 34 | active | `pub mod entry_accessor` | Gates the receiver-side `FileEntryAccessor`-generic module. |

## 5. `crates/engine` (3 sites)

### 5.1 `crates/engine/src/delete/mod.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 38 | active | `pub mod entry_accessor` | Gates the delete-pipeline `FileEntryAccessor`-generic module (RSS-A.7.f). |
| 56 | active | `pub use entry_accessor::{...}` re-export | Re-exports `GenericCohortIndex`, `collect_child_dirs_generic`, `compute_extras_generic`, `compute_extras_with_cohorts_generic`, `segment_basenames_generic`. |

### 5.2 `crates/engine/src/delete/traversal.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 171 | active | `impl DirTraversalCursor` method `child_dirs_mut()` | `pub(super)` accessor exposed for the generic `observe_segment_generic` in the `entry_accessor` module. |

## 6. `crates/filters` (1 site)

### 6.1 `crates/filters/src/lib.rs`

| Line | Branch | Kind | Description |
|---|---|---|---|
| 117 | active | `mod entry_filter` | Gates the filters-side trait-generic `FileEntryAccessor` evaluation module. |

## 7. `cfg(feature = "flat-flist-rayon")` sites (out of scope)

These three sites and the `Cargo.toml:41` declaration gate the opt-in parallel builder (RSS-A.11.c). They are **not** scheduled for removal in FFL-7..15. Listed for completeness.

| File | Line | Description |
|---|---|---|
| `crates/protocol/src/flist/flat/mod.rs` | 20 | `mod parallel_builder` declaration |
| `crates/protocol/src/flist/flat/mod.rs` | 37 | `pub use parallel_builder::{ParallelFlatFileListBuilder, extend_from}` |
| `crates/protocol/src/flist/mod.rs` | 68 | `pub use flat::{ParallelFlatFileListBuilder, extend_from}` from the top-level flist module |

## 8. Removal strategy

Ordered easiest -> hardest. Each tier is a candidate PR boundary for FFL-8..15.

### Tier 1 - trivial attribute deletes (re-exports and `use` lines)

Pure cosmetic deletes: drop the `#[cfg(feature = "flat-flist")]` attribute, keep the item. No body changes, no semantics changes.

- `crates/protocol/src/flist/mod.rs:38` - `mod flat` decl
- `crates/protocol/src/flist/mod.rs:60` - `pub use flat::{...}` re-export (13 items)
- `crates/protocol/src/flist/mod.rs:87` - `pub use sort::{...}` re-export
- `crates/protocol/src/flist/dual.rs:21` - `use` import block
- `crates/protocol/src/flist/sort.rs:28` - `use super::accessor::FileEntryAccessor`
- `crates/transfer/src/generator/sender_accessor.rs:20,23,25` - three `use` lines
- `crates/transfer/src/generator/mod.rs:83,93` - two `pub mod` decls
- `crates/transfer/src/receiver/mod.rs:34` - `pub mod entry_accessor` decl
- `crates/engine/src/delete/mod.rs:38,56` - mod decl + re-export
- `crates/filters/src/lib.rs:117` - `mod entry_filter` decl

**Suggested PR scope (FFL-8):** one PR per crate, delete attributes only. Should compile cleanly with zero diff in `cargo expand` output once the feature is unconditional.

### Tier 2 - test gate deletes (whole-file and nested-mod)

Same shape as Tier 1 but inside `#[cfg(test)]` blocks or test files. Deleting these requires confirming the tests do not import items that remain feature-gated elsewhere.

- `crates/protocol/tests/flat_flist_transfer_regression.rs:11` - `#![cfg(...)]` whole-file inner attribute
- `crates/protocol/src/flist/dual.rs:616` - `mod flat_tests` inside `#[cfg(test)]`
- `crates/protocol/src/flist/sort.rs:991` - `mod generic_tests` inside `#[cfg(test)]`
- `crates/protocol/src/flist/accessor.rs:649` - `mod flat_tests` inside `#[cfg(test)]`
- `crates/transfer/src/generator/sender_accessor.rs:259` - `mod tests` with compound `all(test, feature = "flat-flist")` (collapses to `#[cfg(test)]`)

**Suggested PR scope (FFL-9):** one PR collapsing all test gates. Verify the regression test file is picked up by `cargo nextest` after the gate is dropped.

### Tier 3 - struct-field + simple-method deletes

The `DualFileList` struct field and its small accessor methods. Mechanical, but touches the public API surface (the methods become unconditionally available).

- `crates/protocol/src/flist/dual.rs:39` - `flat: FlatFileList` field on `DualFileList`
- `crates/protocol/src/flist/dual.rs:57` - `flat: FlatFileList::new()` in `new()`
- `crates/protocol/src/flist/dual.rs:67` - `flat: FlatFileList::with_capacity(cap)` in `with_capacity()`
- `crates/protocol/src/flist/dual.rs:130` - `self.flat = FlatFileList::new()` in `clear()`
- `crates/protocol/src/flist/dual.rs:146` - `pub fn flat(&self) -> &FlatFileList`
- `crates/protocol/src/flist/dual.rs:156` - `pub fn extras(&self) -> &ExtrasArena`
- `crates/engine/src/delete/traversal.rs:171` - `pub(super) fn child_dirs_mut()` accessor
- `crates/protocol/src/flist/sort.rs:58` - `SortKey::from_accessor()` method
- `crates/protocol/src/flist/accessor.rs:319` - `mod flat_impl` (the `FileEntryAccessor` impl for `FlatFileEntry`)

**Suggested PR scope (FFL-10):** delete the attributes; verify the `DualFileList` public API still satisfies all consumer call sites listed in `crates/transfer/src/generator/context.rs:45`.

### Tier 4 - generic-function deletes (trait-generic helpers)

These are unconditional public functions in the active branch; they become permanently exported. Risk: callers in other crates may rely on their absence. Audit before deleting.

- `crates/protocol/src/flist/sort.rs:472` - `pub fn compare_entries_generic()`
- `crates/protocol/src/flist/sort.rs:490` - `pub fn sort_entries_generic()`
- `crates/transfer/src/generator/sender_accessor.rs:41` - `pub fn format_iflags_generic()`
- `crates/transfer/src/generator/sender_accessor.rs:196` - `pub fn format_itemize_line_generic()`
- `crates/transfer/src/generator/sender_accessor.rs:235` - `pub fn entry_display_name()`
- `crates/transfer/src/generator/sender_accessor.rs:254` - `pub fn should_skip_entry()`

**Suggested PR scope (FFL-11):** one PR for protocol::sort generics, one for transfer::sender_accessor. Each PR confirms downstream callers compile when the helpers are unconditional.

### Tier 5 - block-inside-function deletes (`push`, conversion helpers)

These edit function bodies. The cfg-gated blocks inside `DualFileList::push` and the free functions `file_entry_to_flat` / `build_flat_extras` become unconditional code paths. Risk: this changes the runtime cost profile for builds that previously had the feature off (none in CI, but downstream consumers may have been disabling it for binary size).

- `crates/protocol/src/flist/dual.rs:78` - block inside `push()` that calls `file_entry_to_flat` and `flat.push_with_extras`
- `crates/protocol/src/flist/dual.rs:299` - free fn `file_entry_to_flat()`
- `crates/protocol/src/flist/dual.rs:361` - free fn `build_flat_extras()`

**Suggested PR scope (FFL-12):** single PR consolidating `DualFileList::push` to unconditionally fan out. Drops the cfg branches inside `push` and `clear`, makes the two private helpers unconditional, and removes the now-orphaned `cfg` import (Tier 1 item at line 21).

### Tier 6 - Cargo.toml manifest cleanups

After all attributes are gone, drop the feature declarations themselves. This is the final step and removes the feature from `--features` flags and CI scripts.

- `crates/protocol/Cargo.toml:38` - delete `flat-flist = []`
- `crates/protocol/Cargo.toml:19` - drop `"flat-flist"` from `default = [...]`
- `crates/protocol/Cargo.toml:41` - update `flat-flist-rayon` to no longer depend on the removed feature (becomes `["dep:rayon"]`); the rayon path is out of scope
- `crates/protocol/Cargo.toml:97` - remove `required-features = ["flat-flist"]` from the bench target
- `crates/filters/Cargo.toml:25` - delete `flat-flist = [...]`
- `crates/transfer/Cargo.toml:75` - delete `flat-flist = [...]`
- `crates/engine/Cargo.toml:108` - delete `flat-flist = [...]`

**Suggested PR scope (FFL-13):** manifest-only PR that drops the feature from all four `Cargo.toml` files plus any references in CI workflow files. Search globally for `--features flat-flist`, `--no-default-features ... flat-flist`, and similar before landing.

### Tier 7 - dead-doc-comment cleanup

After the feature flag is gone, the doc comments that say "behind `flat-flist`" are misleading. One-shot grep-and-clean pass.

Affected comments (already enumerated in the second `grep` pass above):

- `crates/protocol/src/flist/dual.rs:4,12,30,34,74,145,154,614`
- `crates/protocol/src/flist/flat/intern.rs:39`
- `crates/protocol/src/flist/flat/tests.rs:619,621`
- `crates/protocol/src/flist/sort.rs:57,464,989`
- `crates/transfer/src/generator/context.rs:45`
- `crates/transfer/src/generator/entry_accessor.rs:7`
- `crates/transfer/src/receiver/entry_accessor.rs:17`
- `crates/filters/src/lib.rs:116`
- `crates/filters/src/entry_filter.rs:8`
- `crates/engine/src/delete/entry_accessor.rs:21`

**Suggested PR scope (FFL-14):** doc-only PR scrubbing all "behind `flat-flist`" / "Feature-gated behind `flat-flist`" / "when the `flat-flist` feature is enabled" phrasing. Leaves design references (`docs/design/flat-flist-representation.md`) alone - those describe the on-disk and in-memory layout, not the feature flag.

### Tier 8 - reserved for `flat-flist-rayon` follow-up

Out of scope for FFL-7..15. The 3 sites and the matching Cargo declaration remain in place. A future RSS-A.11.c follow-up will decide whether to flip `flat-flist-rayon` default-on or leave it opt-in.

## 9. Notes for executors

- The `flat` submodule itself (`crates/protocol/src/flist/flat/`) is *not* cfg-gated internally - only its `mod` declaration and re-exports are. Once those gates drop, the whole submodule compiles unconditionally with no further edits.
- `DualFileList` is the only struct with a cfg-gated field. After Tier 3, its struct layout changes for builds that previously disabled the feature. This is the only observable ABI change in the removal.
- The `cfg(all(test, feature = "flat-flist"))` site at `sender_accessor.rs:259` is the **only** compound cfg expression. Everything else is a simple single-feature gate.
- No `cfg_attr` uses of `flat-flist` exist. The removal does not touch derive macros or conditional trait bounds.
