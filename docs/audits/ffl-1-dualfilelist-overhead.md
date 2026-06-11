# FFL-1: DualFileList wrapper runtime + compile-time overhead audit

Date: 2026-06-11
Scope: `crates/protocol/src/flist/dual.rs` and consumers in `crates/transfer/src/generator/`.
Feeds into: FFL-4 decision matrix (flip-default / dual-keep / revert).

## 1. Current state

The wrapper actually exists as `DualFileList` (no rename ever happened) and lives at
`crates/protocol/src/flist/dual.rs:37-198`. The relevant fragments:

```rust
// crates/protocol/src/flist/dual.rs:37-41
pub struct DualFileList {
    legacy: Vec<FileEntry>,
    #[cfg(feature = "flat-flist")]
    flat: FlatFileList,
}
```

```rust
// crates/protocol/src/flist/dual.rs:77-84
pub fn push(&mut self, entry: FileEntry) {
    #[cfg(feature = "flat-flist")]
    {
        let (header, extras) = file_entry_to_flat(&entry, &mut self.flat);
        self.flat.push_with_extras(header, &extras);
    }
    self.legacy.push(entry);
}
```

The read-side trait is `FileEntryAccessor` (`crates/protocol/src/flist/accessor.rs:33-183`). It is
implemented for both `FileEntry` (`accessor.rs:191-313`) and, behind the `flat-flist` feature, for
`FlatFileEntry<'a>` (`accessor.rs:319-467`). DualFileList itself does NOT implement
`FileEntryAccessor`; every read accessor on the wrapper delegates statically to the legacy
`Vec<FileEntry>`. Trait dispatch only enters the picture in `sort_entries_generic` /
`compare_entries_generic` (`crates/protocol/src/flist/sort.rs:429-516`), which today are
flat-flist-only and not on the production hot path.

The flat backing store side-emits via `file_entry_to_flat` (`dual.rs:259-318`) and
`build_flat_extras` (`dual.rs:321-388`). Both run on every `push` when `flat-flist` is enabled.

## 2. Per-method indirection inventory

The dual wrapper exposes 14 methods + 4 trait impls. Hot-path status is determined by call-site
frequency in `crates/transfer/src/generator/` (43 call sites total, grep summary):

| Method | Hot path? | Indirection | Estimated overhead |
|--------|-----------|-------------|--------------------|
| `push(&mut self, entry)` | Yes (per-entry) | Static cfg branch; with `flat-flist` does FileEntry -> FileEntryHeader + FlatExtras conversion + path interning + `Vec::push` on a second store | Substantial when `flat-flist` is on (one `file_entry_to_flat` call + arena interning + a second growable-vec push per file). Inlines to a single `Vec::push` when off. |
| `len(&self)` (13 call sites) | Yes | Direct `Vec::len` | Zero (inlinable). |
| `is_empty(&self)` | Yes | Direct `Vec::is_empty` | Zero. |
| `as_slice(&self)` (2 call sites) | Yes (sort, INC_RECURSE classification, stats) | Direct `&[FileEntry]` borrow | Zero. |
| `as_mut_vec(&mut self)` (2 call sites) | Yes (in-place sort permutation) | Returns `&mut Vec<FileEntry>` directly | Zero. Sort never touches the flat side. |
| `iter(&self)`, `iter_mut(&mut self)` | Yes (stats/debug) | Direct `slice::Iter` | Zero. |
| `get(&self, ndx)` | Yes (protocol_io ndx lookups) | Direct `Vec::get` | Zero. |
| `Index<usize>`, `Index<RangeFrom>`, `IndexMut<usize>` | Yes (NDX-based access) | Direct `Vec` indexing | Zero. |
| `IntoIterator` for `&Self` / `&mut Self` | Cold | Direct `slice::Iter` | Zero. |
| `segment_start(&self)` | Yes (INC_RECURSE sub-list building) | `Vec::len` alias | Zero. |
| `clear(&mut self)` | Cold (transfer reset) | Drops legacy Vec contents; reassigns `flat = FlatFileList::new()` when feature on | One arena drop + reallocation per clear if feature on; trivial otherwise. |
| `reserve(&mut self, n)` | Cold (initial pre-allocate) | Direct `Vec::reserve`; flat arenas grow dynamically | Zero. Flat arenas are NOT pre-reserved (potential growth-hop cost noted below). |
| `flat(&self)`, `extras(&self)` | flat-flist arm only | Direct field returns | Zero, but only compiled when feature on. |
| `into_vec(self)` | Cold (drain) | Discards flat side, returns legacy `Vec` | Zero. |
| `reclaim_segment(start, end)` | Per-segment in transfer loop | Walks `legacy[start..end]` calling `reclaim_heap_data`; flat side is NOT reclaimed | Zero indirection. Note: the flat store keeps memory alive even after a segment is reclaimed, eroding the RSS win the wrapper is supposed to demonstrate. |

Read accessors are entirely statically dispatched to the legacy `Vec<FileEntry>` - there is NO
`dyn FileEntryAccessor` indirection on hot paths. The trait exists only as a migration aid for the
flat-only future, not as a runtime cost today.

## 3. Memory footprint

The wrapper unconditionally stores BOTH representations simultaneously when `flat-flist` is on
(`dual.rs:37-41` shows both fields, not an enum). Every `push` writes the same entry into both
stores (`dual.rs:77-84`). The cumulative cost per entry when the feature is enabled, on top of
legacy `Vec<FileEntry>`:

- `FileEntryHeader` (Copy struct, fixed size; see `crates/protocol/src/flist/flat/header.rs`).
- Owned `Vec<u8>` clones in `FlatExtras` for link target, checksum, user/group names, etc.
 (`dual.rs:325-371`). Each clone is copied from the legacy entry rather than aliased.
- `PathArena` interner growth (the dirname-sharing path-dedup win - but tested in
 `dual.rs:840-909` to dedupe within the second store, not across the two stores).
- `ExtrasArena` length-prefixed tail blob for each entry with extras.

Initialization paths confirm this duplication:
- `DualFileList::new()` (`dual.rs:53-60`) constructs both stores empty.
- `DualFileList::with_capacity(cap)` (`dual.rs:63-70`) pre-reserves only the legacy `Vec`. The
 flat-side `FlatFileList::with_capacity(cap)` reserves header slots but the arenas grow
 ad-hoc on push (`reserve` rustdoc at `dual.rs:138-141` is explicit about this).
- `clear()` (`dual.rs:128-134`) drops and replaces the flat side without releasing the legacy
 backing capacity.
- `reclaim_segment(start, end)` (`dual.rs:188-197`) frees PathBuf + extras Box on the legacy
 side only - the flat side retains every byte. This breaks the RSS-A.8 INC_RECURSE reclamation
 invariant for the flat path.

Without `flat-flist` the struct is a transparent newtype over `Vec<FileEntry>` (no flat field, no
arena imports). Module docstring at `dual.rs:1-14` documents this zero-cost-off-feature posture.

## 4. Compile-time overhead

`#[cfg(feature = "flat-flist")]` sites the wrapper introduces or enables in the flist module
itself (grep across `crates/protocol/src/flist/`):

| File | cfg(flat-flist) lines |
|------|-----------------------|
| `dual.rs` | 11 (struct field, `push` arm, `with_capacity`, `clear`, accessors, `file_entry_to_flat`, `build_flat_extras`, test mod, plus imports) |
| `flist/mod.rs` | 4 (sub-module decl, `flat::*` re-exports including rayon, `sort` generic re-export) |
| `flist/sort.rs` | 4 (generic compare/sort entry points + tests) |
| `flist/accessor.rs` | 2 (`flat_impl` mod + tests sub-mod) |

Total: 21 cfg sites inside the flist module that exist solely because the wrapper allows the
two paths to coexist. None of these are large blocks; the largest is `flat_impl` at
`accessor.rs:319-467` (148 lines, all behind the feature).

Cargo feature surface (`crates/protocol/Cargo.toml:38-41,97`):

```toml
flat-flist = []
flat-flist-rayon = ["flat-flist", "dep:rayon"]
# bench cell:
required-features = ["flat-flist"]
```

The feature is opt-in (not in default). CI runs `--features flat-flist` matrix as well as the
default build.

## 5. Recommendation (feeds FFL-4)

Runtime overhead in the default build (feature off) is zero: `DualFileList` collapses to a
newtype around `Vec<FileEntry>`. There is no trait-object dispatch, no enum-match branch, and no
hidden per-method allocation. Every hot-path method (`push`, `len`, `as_slice`, `as_mut_vec`,
`iter`, indexing, `segment_start`) lowers to the same instruction sequence as direct `Vec`
access.

Runtime overhead with `--features flat-flist` is meaningful but bounded and confined to the
write side:
- Every `push` runs `file_entry_to_flat` (path split + 2 PathArena interns + presence-bit
 packing) and `build_flat_extras` (up to 8 owned `Vec<u8>` clones for the optional fields).
- A second growable structure (`FlatFileList`) grows alongside the legacy `Vec<FileEntry>`,
 doubling peak RSS during the flist-build phase rather than reducing it.
- `reclaim_segment` does NOT reclaim from the flat side; INC_RECURSE memory savings degrade.

Read overhead with `--features flat-flist` on is still zero on the production hot path -
because consumers (sort, NDX lookup, stats, INC_RECURSE classification) read through
`legacy`, not through the flat store. The flat store is built but never consulted at runtime.

Implication for FFL-4: The wrapper has near-zero cost when the feature is off, so option
"dual-keep + gradual FFL-7..15 removal at leisure" is viable from a default-build perspective.
However, the wrapper's stated purpose (validate flat-flist against legacy in production) is NOT
exercised at runtime - read consumers still go to the legacy Vec, so building the flat side is
write-only insurance. The cost-benefit on a `--features flat-flist` build is dominated by the
write-side amplification (double allocation + extras Vec clones + arena growth) with no
matching read-side benefit. This strengthens the case for accelerating the FFL-FLIP series:
either commit (flip default-on, migrate reads to FlatFileList, retire legacy) or revert (delete
DualFileList wrapper and re-keep `Vec<FileEntry>` directly).

Recommendation: ACCELERATE FFL-FLIP. The current dual-write pattern adds RSS to the
`--features flat-flist` build without giving the flat store any read traffic to validate
against. Holding the dual path makes sense only as a short-lived migration step; if the
RSS-A.LAND bench gate (RSS-A.LAND.2 + RSS-A.LAND.4) hasn't run, run it next and use the
outcome to drive FFL-4. Do NOT keep dual-write indefinitely - the wrapper's benefit only
materializes once reads switch over.

## 6. Open questions for FFL-2/FFL-3

The production deploys these later tasks should profile:

1. **Million-file RSS, legacy path only (FFL-2):** What is the absolute peak RSS during flist
 build of a 1M-file corpus on `cargo build` defaults (no `flat-flist`)? Needed as the
 control number. RSS-1.b numbers exist but are stale relative to the post-RSS-A.12 layout.
2. **Million-file RSS, flat-flist path (FFL-3):** Same workload with `--features flat-flist`.
 Two sub-cases:
 - Pre-FFL-7 (current dual-write): expect WORSE RSS than legacy because both stores live.
 - Post-FFL-7..10 (flat-flist alone, dual removed): expect target ~25% of legacy per
 RSS-A.5.a sizing math.
3. **Working-set impact under INC_RECURSE:** With dual-write, does `reclaim_segment`'s
 flat-side leak measurably break RSS-A.8's segment-reclamation contract on multi-segment
 transfers (e.g., 10K segments)?
4. **Build-time cost on `--features flat-flist`:** Does adding 21 cfg sites + a 148-line
 `flat_impl` mod measurably extend compile wall time in the protocol crate? Cheap to
 measure; useful baseline before FFL-7..11 removal claims a build-time win.
5. **Read-side validation gap:** Is there any code path in the workspace that reads from
 `DualFileList::flat()` other than tests? If not, dual-write is pure insurance with no
 active comparison; FFL-4 should reflect that.

## 7. Cited files

- `crates/protocol/src/flist/dual.rs`
- `crates/protocol/src/flist/accessor.rs`
- `crates/protocol/src/flist/sort.rs`
- `crates/protocol/src/flist/mod.rs`
- `crates/protocol/src/flist/flat/mod.rs`
- `crates/protocol/Cargo.toml`
- `crates/transfer/src/generator/context.rs`
- `crates/transfer/src/generator/file_list/mod.rs`
- `crates/transfer/src/generator/file_list/inc_recurse.rs`
- `crates/transfer/src/generator/file_list/hardlinks.rs`
- `crates/transfer/src/generator/transfer/transfer_loop.rs`
- `crates/transfer/src/generator/protocol_io.rs`
- `crates/transfer/src/generator/transfer/stats.rs`
