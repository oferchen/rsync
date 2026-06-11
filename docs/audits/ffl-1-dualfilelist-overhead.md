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

## 2. Call-site inventory by crate

`DualFileList` is referenced by name in 6 files (workspace-wide). The wrapper lives in
`protocol` and is consumed only on the sender / generator side - the receiver side keeps a
plain `Vec<FileEntry>` (see `crates/transfer/src/receiver/mod.rs:251`), so the dual path
is sender-only.

| Crate / module | File | Role |
|----------------|------|------|
| `protocol` | `crates/protocol/src/flist/dual.rs` | Type + impls + tests |
| `protocol` | `crates/protocol/src/flist/mod.rs` | `pub use dual::DualFileList` re-export |
| `protocol` | `crates/protocol/src/flist/flat/mod.rs` | Module docstring cross-ref only |
| `protocol` (tests) | `crates/protocol/tests/flat_flist_transfer_regression.rs` | Parity regression suite |
| `transfer` | `crates/transfer/src/generator/context.rs` | Field declaration `GeneratorContext.file_list: DualFileList`; `new`, `clear_file_list`, `push_file_item` |
| `transfer` | `crates/transfer/src/generator/file_list/inc_recurse.rs` | INC_RECURSE reorder rebuilds the list via `DualFileList::with_capacity(total)` and re-pushes entries |

Indirect call sites that read or mutate the wrapped list through `GeneratorContext` (grep for
`self.file_list`, `push_file_item`, `clear_file_list` under `crates/transfer/src/generator/`,
excluding comments):

| File | Sites |
|------|-------|
| `crates/transfer/src/generator/file_list/mod.rs` | 15 |
| `crates/transfer/src/generator/transfer/transfer_loop.rs` | 9 |
| `crates/transfer/src/generator/protocol_io.rs` | 8 |
| `crates/transfer/src/generator/file_list/inc_recurse.rs` | 8 |
| `crates/transfer/src/generator/context.rs` | 8 |
| `crates/transfer/src/generator/file_list/hardlinks.rs` | 7 |
| `crates/transfer/src/generator/file_list/walk.rs` | 3 |
| `crates/transfer/src/generator/tests.rs` | 2 |
| `crates/transfer/src/generator/transfer/stats.rs` | 1 |
| **Total (generator side, non-test/non-comment)** | **~60** |

Reads of `DualFileList::flat()` outside `dual.rs` tests: **zero**. The
`flat-flist-transfer-regression` integration test (`crates/protocol/tests/`) reads the flat
side, but no production code path under `crates/transfer/` or `crates/cli/` does. This is
the read-side validation gap revisited in Section 8 (Recommendation): the flat store is
written-to but never queried at runtime.

## 3. Per-method indirection inventory

The dual wrapper exposes 14 methods + 4 trait impls. Hot-path status is determined by call-site
frequency in `crates/transfer/src/generator/` (see Section 2 for the per-file breakdown):

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

## 4. Memory footprint

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

## 5. Compile-time overhead

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

## 6. Measured RSS state (dual vs flat-only vs legacy-only at 1M files)

The FFL-4 decision matrix needs three paired measurements at the 1M-file scale:

| Variant | Workload | Status | Source |
|---------|----------|--------|--------|
| Legacy-only (default build, `flat-flist` off) | 1M-file push, INC_RECURSE on | **CAPTURED**: 197 MB peak RSS (ratio 25.9x vs upstream's 7.6 MB) | `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md` |
| Legacy-only, no-INC_RECURSE | 1M-file push, INC_RECURSE off | **CAPTURED**: 198 MB peak RSS (ratio 2.6x vs upstream's 76.8 MB) | `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md` |
| Legacy-only, dry-run | 1M-file dry-run | **CAPTURED**: 19.1 MB (ratio 2.4x vs upstream's 7.9 MB) | `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md` |
| Dual (`--features flat-flist`, current code) | 1M-file push, INC_RECURSE on | **NOT YET CAPTURED**. Methodology only: `docs/design/flat-flist-rss-bench-fixture.md` (RSS-A.9.a), `docs/design/flat-flist-rss-comparison.md` (RSS-A.10.a). Expected: WORSE than legacy because both stores live (FFL-4 section 5 Option A risk) | gap |
| Flat-only (hypothetical post-FFL-7..10) | 1M-file push, INC_RECURSE on | **NOT YET CAPTURED**. Design target: < 85 MB steady-state (RSS-A.9.a stretch), hard ceiling 115 MB (1.5x upstream's no-INC_RECURSE). Predicted from sizing math (`flat-flist-rss-comparison.md` row "flat steady"): ~70 bytes/entry x 1M = 70 MB plus baseline overhead | gap |

Per-entry sizing math from `flat-flist-rss-comparison.md` line 205-218:

- Upstream `pool_alloc`: ~70 bytes/file (control)
- Legacy `Vec<FileEntry>`: ~182 bytes/file (current default)
- Flat-flist target: matches upstream pool_alloc shape

The dual variant adds the flat store's per-entry cost ON TOP of the legacy ~182 bytes/file
during the build phase. At 1M files this would add roughly 70 MB of dual-store overhead,
projecting the dual-write peak to ~267 MB (197 MB legacy + ~70 MB flat-side header arena +
ExtrasArena + PathArena bytes). The flat arenas are NOT released by `reclaim_segment`
(`dual.rs:188-197`), so under INC_RECURSE the dual variant degrades worse than the legacy
path because legacy reclaims while the flat side does not.

**Read this section as: the FFL-4 Option B "hold dual-keep" verdict is defensible only
because the default build never instantiates the flat store. The `--features flat-flist`
matrix cell pays the full dual-write penalty (RSS goes UP, not down) until FFL-7..10
migrate read consumers and FFL-FLIP retires the legacy side.**

## 7. Measured throughput state (RSS-A.10.a/.b)

| Variant | Status |
|---------|--------|
| Legacy throughput baseline (RSS-A.10.a) | **METHODOLOGY ONLY**. `docs/design/flat-flist-throughput-baseline.md` defines the workloads + cells but no numbers are captured. Execution gated on RSS-A.10.b (#3232). |
| Flat-flist throughput (RSS-A.10.b) | **METHODOLOGY ONLY**. `docs/design/flat-flist-throughput-post-migration.md` defines the paired AFTER methodology but no numbers are captured. Execution gated on RSS-A.LAND.3/.4. |

Predicted regression from `flat-flist-throughput-post-migration.md` sec. 3:

| Hot path | Predicted ratio |
|----------|-----------------|
| Push initial (FlatFileList sequential build) | 0-2% slowdown vs legacy |
| Delta-sync extras decode (ExtrasArena lookup per entry) | 0-2% slowdown |
| Filter evaluation (PathArena::resolve per entry) | < 1% slowdown |
| INC_RECURSE per-segment dispatch | 1-3% slowdown at 1M files |

P0 gate (sec. 5.1): all four P0 configs (initial-100K and initial-1M, all-small,
INC_RECURSE on/off) must ratio <= 1.03 against legacy. Above 1.03 blocks the flat-flist
default-on flip per `flat-flist-throughput-post-migration.md` table 5.1.

**Was there a measurable throughput regression from DualFileList?** Direct answer: not yet
observed because RSS-A.10.a/.b have not been executed. The dual-write path has been in tree
since RSS-A.6 (`docs/design/rss-a6-dual-emit-pattern.md`) and lives behind the opt-in
`flat-flist` feature, so the default-build throughput is the legacy baseline (no
regression). Once RSS-A.10.b runs against the dual-write code, the measured ratio will
include `file_entry_to_flat` + `build_flat_extras` + arena interning per push.

## 8. Recommendation (feeds FFL-4)

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

## 9. Recommended next steps for FFL-2 / FFL-3

The decision input in Section 8 is conditional on bench numbers that must be captured next.
The two follow-on tasks should do the following, in order, in podman containers per
`feedback_use_container_for_linux_bench.md`:

**FFL-2 (#3708): production-legacy-RSS profile.** Re-capture the legacy path peak RSS at
1M files on the current master HEAD using the RSS-1.b/1.c methodology
(`docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md`) but pinned to the post-RSS-A.12
codebase to refresh the 197 MB / 198 MB / 19.1 MB numbers. Steps:

1. Build `cargo build --release` with default features (NO `--features flat-flist`).
2. In the `oc-rsync-bench:latest` podman container, run `/usr/bin/time -v` against the
 `flat-flist-1m-shared` fixture for three modes: INC_RECURSE on, INC_RECURSE off,
 dry-run.
3. Record peak RSS from "Maximum resident set size (kbytes)". 5 iterations per mode,
 report median. Store in `target/bench/rss/legacy/` and append to
 `docs/benchmarks/ffl-2-legacy-rss-2026-XX-XX.md`.
4. Compare against the RSS-1.b baseline (197 MB / 198 MB / 19.1 MB). If within 5%,
 accept as the FFL-4 control value. Otherwise investigate post-RSS-A.12 drift before
 running FFL-3.

**FFL-3 (#3709): production-flat-RSS profile.** Run the same fixture and methodology
with `--features flat-flist`, capturing the dual-write peak RSS. Steps:

1. Build `cargo build --release --features flat-flist`.
2. In the same container, run the same three modes against the same fixture.
3. Record peak RSS. Store in `target/bench/rss/flat-flist/` and append to
 `docs/benchmarks/ffl-3-flat-flist-rss-2026-XX-XX.md`.
4. Compute the dual-write penalty: `dual_peak_rss / legacy_peak_rss` for each of the
 three modes. The current dual-write design predicts this ratio is > 1.0 (worse RSS),
 matching the Section 6 prediction of ~267 MB at 1M push + INC_RECURSE.
5. Also capture per-phase timings if `bench-instrumentation` is wired, to feed
 RSS-A.10.b's pass-criteria table.

These two captures together produce the RSS-A.LAND.1 (#3627) and RSS-A.LAND.2 (#3628)
data the FFL-4 decision matrix is gated on. Without them, FFL-4 cannot move past Option B
(hold dual-keep) in either direction (flip default-on or revert).

**Followups that fall out of FFL-2/3 results:**

- **Working-set impact under INC_RECURSE.** Does `reclaim_segment`'s flat-side leak
 measurably break RSS-A.8's segment-reclamation contract on multi-segment transfers
 (e.g., 10K segments)? Worth a focused INC_RECURSE-only follow-up using a many-segment
 fixture, gated on FFL-3 results showing the dual-write RSS gap widens with segment
 count.
- **Build-time cost on `--features flat-flist`.** Does adding 21 cfg sites + a 148-line
 `flat_impl` mod measurably extend compile wall time in the `protocol` crate? Cheap to
 measure with `cargo build --timings`; useful baseline before FFL-7..11 removal claims
 a build-time win.
- **Read-side validation gap, already verified.** Production reads of
 `DualFileList::flat()` are zero outside `dual.rs` tests and the
 `flat-flist-transfer-regression` integration test (Section 2). The dual-write is pure
 write-side insurance with no active read-path comparison; FFL-4 already reflects this
 in its Option B verdict.

## 10. Cited files

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
- `crates/transfer/src/generator/file_list/walk.rs`
- `crates/transfer/src/generator/tests.rs`
- `crates/transfer/src/receiver/mod.rs`
- `crates/protocol/src/flist/flat/flist.rs`
- `crates/protocol/src/flist/flat/intern.rs`
- `crates/protocol/src/flist/flat/extras.rs`
- `crates/protocol/tests/flat_flist_transfer_regression.rs`
- `docs/benchmarks/rss-1b-1c-peak-rss-2026-05-29.md`
- `docs/design/flat-flist-rss-bench-fixture.md`
- `docs/design/flat-flist-rss-comparison.md`
- `docs/design/flat-flist-throughput-baseline.md`
- `docs/design/flat-flist-throughput-post-migration.md`
- `docs/design/ffl-4-flat-flist-flip-decision.md`
- `docs/design/rss-a6-dual-emit-pattern.md`
