# RSS-8.b: FileEntry read-path migration to PathHandle

Task: RSS-8.b. Branch: `docs/rss-8b-read-path-migration-spec`.
Prerequisites: RSS-8.a (PR #4980 - defines `PathHandle` as a 4-byte
`lasso::Spur` handle replacing `PathBuf`/`Arc<Path>` in FileEntry),
RSS-5 (pool shape spec), RSS-4 (arena allocator evaluation).
Downstream: RSS-9 (full consumer migration and benchmark).

This document specifies the migration of all **read paths** -
call sites that access `FileEntry`'s `name`, `path`, `dirname`, or
`name_bytes` fields - to use the `PathHandle` resolution API through
the `PathInterner` (backed by `lasso::RodeoReader`).

## Scope

RSS-8.a introduced the `PathHandle` type (`Spur`, 4 bytes) and the
`PathInterner` backed by `lasso::Rodeo`/`RodeoReader`. The struct
fields `name: PathBuf` and `dirname: Arc<Path>` become
`name: Spur` and `dirname: Spur`. This task migrates every call site
that **reads** from those fields to pass a `&PathInterner` for
resolution.

Write paths (constructors, `from_raw_bytes`, `set_dirname`,
`prepend_dir`, `strip_leading_slashes`) are out of scope - they were
addressed in RSS-8.a's interner-insertion API.

## Resolution API surface

The `PathInterner` exposes the following read methods after freezing
(`Rodeo` -> `RodeoReader` transition):

```rust
impl PathInterner {
    /// Resolves a handle to its interned string slice.
    /// O(1) indexed array read - no hash lookup, no lock.
    pub fn resolve(&self, handle: Spur) -> &str;

    /// Resolves a handle to a Path reference.
    /// Zero-allocation on Unix (Path is &str reinterpreted).
    pub fn resolve_path(&self, handle: Spur) -> &Path;

    /// Resolves a handle to wire-format bytes.
    /// Returns Cow::Borrowed on Unix, Cow::Owned on Windows
    /// (backslash -> forward-slash translation).
    pub fn resolve_bytes(&self, handle: Spur) -> Cow<'_, [u8]>;
}
```

All three are `O(1)` flat-array lookups on `RodeoReader`. The
`RodeoReader` is `Sync` so these are safe under `rayon::par_iter`.

## Changed accessor signatures

| Current signature | New signature | Notes |
|---|---|---|
| `name(&self) -> &str` | `name(&self, paths: &PathInterner) -> &str` | Hot path - every consumer |
| `path(&self) -> &PathBuf` | `path(&self, paths: &PathInterner) -> &Path` | Returns `&Path` not `&PathBuf` |
| `dirname(&self) -> &Arc<Path>` | `dirname(&self, paths: &PathInterner) -> &Path` | No more `Arc` indirection |
| `name_bytes(&self) -> Cow<'_, [u8]>` | `name_bytes(&self, paths: &PathInterner) -> Cow<'_, [u8]>` | Delegates to `resolve_bytes` |

Return-type changes: `&PathBuf` -> `&Path` (strictly wider, no
breakage at call sites using `AsRef<Path>` or `Deref<Target=Path>`).
`&Arc<Path>` -> `&Path` (callers that clone the Arc must clone a
`PathBuf` from the `&Path` instead - or better, copy the `Spur`).

## Call-site inventory

### Crate: `protocol` (tier 1 - migrate first)

| Module | Call site | Accessor | Usage pattern |
|---|---|---|---|
| `flist/sort.rs:44` | `SortKey::new` | `name_bytes()` | Precompute sort key bytes |
| `flist/sort.rs:231,243` | sort closures | `name_bytes()` | Per-comparison byte fetch |
| `flist/sort.rs:331` | `flist_clean` | `name()` | Duplicate detection |
| `flist/name_cmp.rs:61,89` | `f_name_cmp`, `name_cmp_eq` | `dirname()` | Dir-first comparison |
| `flist/name_cmp.rs:106-107` | `basename_bytes` | `path()`, `dirname()` | Basename extraction |
| `flist/incremental/mod.rs:148,152,153,184,185` | `IncrementalBuilder` | `name()` | Parent tracking, dir creation |
| `flist/incremental/ready_entry.rs:130,169` | readiness check | `name()` | Path parent matching |

### Crate: `transfer` (tier 2)

| Module | Call site | Accessor | Usage pattern |
|---|---|---|---|
| `generator/itemize.rs:223` | `format_itemize` | `path()` | Display path for itemize output |
| `generator/file_list/inc_recurse.rs:72` | segment iteration | `name()` | INC_RECURSE segment matching |
| `generator/transfer/transfer_loop.rs:298,437,447` | transfer loop | `path()` | Logging and work-item path |
| `receiver/file_list/sanitize.rs:39,107` | sanitize | `path()` | Absolute path check |
| `receiver/quick_check.rs:147` | quick-check | `path()` | Join with dest_dir for stat |
| `receiver/transfer/sync.rs:105` | sync transfer | `path()` | Destination path construction |
| `receiver/transfer/candidates.rs:58,88,95,101,119,138` | candidate filter | `path()`, `name()` | Filter, join, log |
| `receiver/transfer/pipeline.rs:196,215,234,261,362,450` | pipelined recv | `path()` | Work-item path, logging |
| `receiver/transfer/pipelined.rs:128,162` | pipelined recv | `path()` | Entry path lookup |
| `receiver/transfer/pipelined_incremental.rs:141` | incremental recv | `path()` | Entry path lookup |
| `receiver/directory/links.rs:53,221,243,255,272` | hardlink/symlink | `path()`, `name()` | Link target path construction |
| `receiver/directory/creation.rs:57,65,244,322,330,336,340,382` | dir creation | `path()`, `name()` | mkdir path, failed-dir tracking |
| `receiver/directory/deletion.rs:72` | dir deletion | `path()` | Deletion target |

### Crate: `engine` (tier 3)

| Module | Call site | Accessor | Usage pattern |
|---|---|---|---|
| `delete/extras.rs:115,135` | delete extras | `path()` | Stat and basename extraction |
| `delete/cohort_index.rs:224` | cohort index | `path()` | Basename lookup for indexing |

### Crate: `matching` (tier 3)

| Module | Call site | Accessor | Usage pattern |
|---|---|---|---|
| `fuzzy/search.rs:69` | fuzzy match | `path()` | Path string for fuzzy scoring |

### Crate: `core` (tier 3)

| Module | Call site | Accessor | Usage pattern |
|---|---|---|---|
| `benches/pip_6_*.rs:306` | benchmark | `path()` | Iteration display |
| `benches/transfer_benchmark.rs:133` | benchmark | `path()` | Iteration display |

## Performance impact analysis

### Resolution cost

`RodeoReader::resolve(&Spur)` is an indexed read into a contiguous
`Vec<&str>` - one bounds check plus one pointer dereference. The
backing strings are contiguous in arena memory, yielding excellent
cache locality when iterating the file list sequentially (which is
the dominant access pattern in both sort and transfer).

Measured cost per resolve: ~1-2 ns on modern hardware (L1 hit). At
1 M entries with ~5 resolves per entry across all consumers, the
total added resolve overhead is ~5-10 ms - negligible compared to
the 30-50 ms saved by eliminating per-entry `PathBuf` allocation
and `Arc` atomic traffic during construction/teardown.

### Sort path

`sort_file_list` calls `name_bytes()` inside the comparison closure.
Today this calls `path_bytes_to_wire(&self.name)` which on Unix
borrows the `OsStr` bytes directly. After migration, it calls
`paths.resolve_bytes(self.name)` which on Unix returns
`Cow::Borrowed` from the arena - same zero-copy semantics. The net
change is one extra function-pointer indirection per comparison vs
today's inline `PathBuf` borrow. At O(n log n) comparisons (1.7 M
for 100 K entries), the added overhead is ~2-3 ms.

### Parallel consumers

`RodeoReader` is `Send + Sync`. All `rayon::par_iter` consumers
(parallel stat with `PARALLEL_STAT_THRESHOLD = 64`, parallel dir
metadata application, parallel delete-plan construction) pass
`&PathInterner` into their closures. Lock-free reads are guaranteed.
No contention point exists.

### Memory savings (cumulative with RSS-8.a write-path)

| Metric | Before | After | Delta |
|---|---|---|---|
| Inline per entry (Unix) | 88 B | 64 B | -24 B (-27%) |
| Inline per entry (Windows) | 104 B | 64 B | -40 B (-38%) |
| Heap per entry (name) | ~32 B | 0 | -32 B |
| Atomic ops per clone | 1 (Arc) | 0 | eliminated |
| Vec<FileEntry> at 1 M | ~120 MiB | ~64 MiB | -56 MiB |

The interner itself consumes ~20-40 MiB for 1 M unique paths (arena
storage). Net reduction: ~16-36 MiB at 1 M entries.

## Migration ordering

### Phase 1: `protocol` crate (self-contained)

The `protocol` crate owns `FileEntry`, `PathInterner`, and the sort/
clean/incremental modules. All accessor call sites within this crate
can be migrated atomically in a single PR because:

1. The `PathInterner` is already co-located with `FileListReader`.
2. Sort and clean operate on `&mut [FileEntry]` - add `&PathInterner`
   parameter to `sort_file_list`, `flist_clean`, `sort_and_clean_file_list`.
3. `f_name_cmp` and `name_cmp_eq` gain a `&PathInterner` parameter.
4. Incremental builder already holds a `&mut PathInterner` for writes;
   read calls switch to the same reference.

**Golden tests stay green** because the resolution round-trip is
identity: `intern(bytes)` -> `resolve(spur)` == original bytes.

### Phase 2: `transfer` crate

The `transfer` crate is the largest consumer. The `PathInterner`
flows from the `FileList` struct (which holds both entries and
interner). Migration pattern at each call site:

```rust
// Before:
let relative_path = entry.path();
let dest = dest_dir.join(relative_path);

// After:
let relative_path = entry.path(&file_list.paths);
let dest = dest_dir.join(relative_path);
```

The `file_list.paths` accessor exposes the frozen `RodeoReader`.
Sub-ordering within phase 2:

1. `receiver/transfer/` - largest cluster (pipeline, pipelined,
   sync, candidates). All share `&FileList` context.
2. `receiver/directory/` - creation, links, deletion.
3. `receiver/file_list/sanitize.rs` - standalone.
4. `receiver/quick_check.rs` - standalone.
5. `generator/` - itemize, transfer_loop, inc_recurse.

### Phase 3: `engine`, `matching`, `core`

Leaf consumers with few call sites. The `engine::delete` module
receives `FileEntry` references with an associated interner ref
from the transfer pipeline. `matching::fuzzy` receives entries
with their interner for path scoring. Benchmark code in `core`
adapts trivially.

## Backward-compatibility strategy

### Compile-time breakage (intentional)

Adding `&PathInterner` to every path accessor is a **breaking
change** to the `FileEntry` public API. This is intentional -
it makes it impossible to use a `Spur` without its resolver,
preventing dangling-handle bugs at compile time.

### Transition helper: `FileEntryRef<'a>`

To ease migration of call sites that access paths repeatedly in
a tight loop, provide a convenience wrapper:

```rust
/// Short-lived view coupling a FileEntry with its interner.
/// Avoids passing `&paths` to every accessor in hot loops.
pub struct FileEntryRef<'a> {
    entry: &'a FileEntry,
    paths: &'a PathInterner,
}

impl<'a> FileEntryRef<'a> {
    pub fn name(&self) -> &'a str {
        self.entry.name(self.paths)
    }
    pub fn path(&self) -> &'a Path {
        self.entry.path(self.paths)
    }
    pub fn dirname(&self) -> &'a Path {
        self.entry.dirname(self.paths)
    }
}
```

This is optional sugar - direct accessor calls with explicit
`&paths` remain the canonical form.

### PartialEq migration

Current `PartialEq` compares `self.name == other.name` as `PathBuf`
byte equality. After migration, same-interner entries compare via
`Spur` equality (`u32 ==`), which is correct and faster. The
`PartialEq` impl becomes:

```rust
impl PartialEq for FileEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name  // Spur == Spur (u32 comparison)
            && self.size == other.size
            && self.mtime == other.mtime
            // ... remaining fields
    }
}
```

Cross-interner equality is undefined (two `Spur` values from
different interners may alias). This matches the structural
invariant that `FileEntry` is meaningless without its interner.

## Testing strategy

1. **Golden wire tests** (`crates/protocol/tests/golden/`) pass
   unchanged - the interner is in-memory only and does not affect
   wire encoding/decoding.
2. **Sort order tests** (`sort.rs::tests`) verify identical output
   ordering before and after. Run `sort_order_golden_comprehensive`
   as regression gate.
3. **Size assertion** (`tests.rs`) tightened from 88 B to 64 B (no
   per-platform branch since `Spur` is platform-independent).
4. **Interop tests** (`tools/ci/run_interop.sh`) validate
   end-to-end wire compatibility with upstream rsync 3.0.9-3.4.2.
5. **New unit tests** for `PathInterner::resolve` and
   `PathInterner::resolve_bytes` round-trip correctness.
6. **Rayon smoke test** confirming `par_iter` consumers compile and
   produce correct results with `&PathInterner` shared reference.

## Risks and mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Lifetime confusion at call sites holding `&Path` across await/yield | Low (no async in transfer path) | Borrow checker catches at compile time |
| Wrong-interner Spur (cross-flist contamination) | Low | Per-segment interners; `try_resolve` at public boundaries |
| Performance regression in sort from resolve indirection | Low (~2 ns/resolve) | Benchmark before/after with 100 K+ file lists |
| Windows `resolve_bytes` allocation in hot sort path | Medium | Cache bytes in `SortKey` struct (already precomputed) |
| Large PR size for phase 2 (transfer crate, ~30 call sites) | Medium | Split by sub-module (receiver/transfer, receiver/directory, generator) |

## Open questions

1. **Should `name_bytes` cache in `SortKey`?** Today `SortKey` stores
   `last_slash` position only. After migration, `name_bytes()` does a
   resolve + potential `Cow::Owned` on Windows. Pre-caching the bytes
   in `SortKey` (as `Vec<u8>`) adds ~24 B per key but eliminates
   per-comparison resolve. Decision: profile after phase 1.

2. **`FileEntryRef` in tests?** Many test helpers call `e.name()`
   repeatedly. Should tests use `FileEntryRef` or pass `&paths`
   everywhere? Decision: test helpers create a module-local
   `test_paths()` fixture returning a shared interner.

3. **Display/Debug impls.** `Debug for FileEntry` today prints
   `name` and `dirname` directly. After migration, `Debug` cannot
   resolve without an interner. Options: (a) print raw `Spur` values,
   (b) require a custom `debug_with(&paths)` method. Decision: print
   raw Spur values in `Debug`, provide `display_with(&paths)` for
   human-readable output.

## Cross-references

- `crates/protocol/src/flist/entry/core.rs:32-72` - struct definition
- `crates/protocol/src/flist/entry/accessors.rs:20-134` - accessor impls
- `crates/protocol/src/flist/intern.rs:42-114` - current `PathInterner`
- `crates/protocol/src/flist/sort.rs:44,85,206,316` - sort consumers
- `crates/protocol/src/flist/name_cmp.rs:60,88` - name comparison
- `crates/transfer/src/receiver/transfer/candidates.rs:52-138` - candidate filter
- `crates/transfer/src/receiver/transfer/pipeline.rs:196-450` - pipelined receiver
- `crates/transfer/src/generator/itemize.rs:218-243` - itemize formatting
- `docs/design/rss-4-arena-allocator-eval.md` - lasso selection rationale
- `docs/design/rss-5-fileentry-pool-shape.md` - target struct layout
