# RSS-8.a: arena-handle type definition for FileEntry paths

> **Status: prototype / not landed.** This `PathHandle` type was never
> added to the tree. The production `FileEntry` still uses `PathBuf` +
> `Arc<Path>`; no `PathHandle`/`PathArena` type exists. The only landed
> dedup is the `Arc<Path>` dirname interner. The real arena/flat backing
> store is designed in `docs/design/flat-flist-representation.md` and
> built from scratch by RSS-A.5.a-f (gated on RSS-2 profiling). See
> `docs/audit/arena-prototype-landing-gap.md`.

Task: RSS-8.a. Branch: `docs/rss-8a-arena-handle-type`.
Prerequisites: RSS-5 (pool-allocator FileEntry shape), RSS-7 (arena
path prototype). Downstream: RSS-8.b (migration implementation),
RSS-9 (consumer migration), RSS-10 (benchmark validation).

## Summary

This document defines `PathHandle` - the type that replaces
`Arc<Path>` in `FileEntry::dirname` and `PathBuf` in
`FileEntry::name`. A `PathHandle` is a 4-byte opaque token (`Spur`
from the `lasso` crate) that indexes into a per-flist string interner.
It is `Copy + Send + Sync + 'static`, costs zero atomic operations on
clone, and reduces per-entry inline path storage from 40 bytes to 8
bytes.

## Type definition

```rust
use lasso::Spur;

/// Opaque handle referencing a path string stored in the flist arena.
///
/// A `PathHandle` is meaningless without its owning `PathArena`.
/// Resolving the handle to `&str` or `&Path` requires passing the
/// arena as context. The handle is a `NonZeroU32` internally, so
/// `Option<PathHandle>` is also 4 bytes (niche-optimized).
///
/// # Size invariant
///
/// `size_of::<PathHandle>() == 4` on all platforms.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct PathHandle(Spur);
```

### Size target

| Field | Before (bytes) | After (bytes) |
|-------|----------------|---------------|
| `name: PathBuf` | 24 (Unix) / 32 (Windows) | 4 (`PathHandle`) |
| `dirname: Arc<Path>` | 16 | 4 (`PathHandle`) |
| **Total path fields** | **40 / 48** | **8** |

The handle pair occupies 8 bytes total - a 32-byte reduction on Unix
and 40-byte reduction on Windows compared to the current layout. This
brings `FileEntry` from 88 bytes (Unix) / 104 bytes (Windows) down to
the 64-byte target established in RSS-5.

### Why not a pointer or index pair?

Alternatives considered and rejected:

- **Raw pointer (`*const u8` + length)**: 16 bytes, requires unsafe
  for dereference, lifetime not statically checked.
- **Offset + length (`u32, u32`)**: 8 bytes per field (16 total for
  two paths), no deduplication, requires unsafe arena indexing.
- **Generation counter + index**: 8 bytes minimum, adds runtime
  validation overhead on every resolve. Unnecessary when the borrow
  checker already prevents use-after-free via the accessor signature.
- **`Spur` (selected)**: 4 bytes, deduplicates identical strings
  (critical for dirname sharing), resolves to `&str` via safe API,
  `Copy + Send + Sync`, niche-optimized for `Option`.

## Arena backing: `lasso` interner

### Choice rationale

The `lasso` crate (`Rodeo` / `RodeoReader`) was selected in RSS-4
over `bumpalo` and `typed-arena` for three reasons:

1. **Native deduplication.** `Rodeo::get_or_intern()` returns the same
   `Spur` for identical strings. Dirname sharing (the primary
   `Arc<Path>` use case) becomes free with no side table. Basenames
   that repeat across directories (`Cargo.toml`, `mod.rs`,
   `__init__.py`) also deduplicate, providing an additional 5-13 MiB
   heap reduction at 1M files on typical workloads.

2. **Smallest inline footprint.** Two `Spur` handles (4 bytes each)
   replace 40 bytes of inline path storage. `bumpalo`/`typed-arena`
   require fat `&str` references (16 bytes each) or unsafe offset
   arithmetic.

3. **Build-then-freeze lifecycle.** `Rodeo` (mutable, `!Sync`) for
   the single-threaded decode/walk phase; `RodeoReader` (immutable,
   `Sync`) for the parallel consumer phase. This maps directly to the
   existing `PathInterner` lifecycle.

### Arena type: `PathArena`

```rust
use lasso::{Rodeo, RodeoReader, Spur};
use std::path::Path;

/// Owns all interned path strings for a single file list (or segment).
///
/// Build phase: backed by `Rodeo` (single-threaded insert).
/// Consume phase: backed by `RodeoReader` (lock-free parallel reads).
pub struct PathArena {
    state: PathArenaState,
}

enum PathArenaState {
    /// Mutable insert during decode or filesystem walk.
    Building(Rodeo),
    /// Immutable read during sort, filter, transfer, and emit.
    Frozen(RodeoReader),
}
```

This replaces the current `PathInterner` at
`crates/protocol/src/flist/intern.rs`.

## Lifetime model

### Structural co-ownership

`PathHandle` has no lifetime parameter. The safety invariant is
**structural**: a `PathHandle` is only meaningful within the context of
its owning `PathArena`, and the accessor API enforces this at compile
time:

```rust
impl FileEntry {
    pub fn name<'a>(&self, arena: &'a PathArena) -> &'a str { ... }
    pub fn dirname<'a>(&self, arena: &'a PathArena) -> &'a Path { ... }
}
```

The borrow checker guarantees:
- The arena cannot be dropped while a returned `&str` / `&Path` is
  live.
- No `unsafe` is needed for resolve.

### Ownership hierarchy

```
FileList
├── arena: PathArena       (owns all string bytes)
└── entries: Vec<FileEntry> (owns PathHandle tokens)
```

`FileList::drop()` drops `entries` first (trivial - no heap for path
fields), then `arena` (frees all interned strings in one pass). This
mirrors upstream's `pool_destroy()` in `lib/pool_alloc.c`.

### No `'static` refcount

The handle is `'static` in the type system (`Spur: 'static`), but it
is **not** self-validating. Resolving a `Spur` from the wrong arena
returns garbage or panics (out-of-range). This is analogous to using a
`HashMap` key on the wrong map - a logic bug, not a memory safety
issue. Cross-arena contamination is prevented by the API design: every
`FileList` owns exactly one `PathArena`, and accessors take `&self` on
the same `FileList`.

## API surface

### Resolution methods on `PathArena`

```rust
impl PathArena {
    /// Resolves a handle to a string slice.
    ///
    /// # Panics
    /// Panics if `handle` was not interned in this arena.
    pub fn resolve(&self, handle: PathHandle) -> &str { ... }

    /// Resolves a handle to a `Path` reference.
    pub fn resolve_path(&self, handle: PathHandle) -> &Path {
        Path::new(self.resolve(handle))
    }

    /// Attempts resolution without panicking.
    pub fn try_resolve(&self, handle: PathHandle) -> Option<&str> { ... }
}
```

### Insertion methods (build phase only)

```rust
impl PathArena {
    /// Interns a string, returning its handle. Deduplicates.
    ///
    /// # Panics
    /// Panics if the arena is frozen.
    pub fn intern(&mut self, s: &str) -> PathHandle { ... }

    /// Interns a byte slice as a path (for wire-format decoding).
    /// On Unix, bytes are stored verbatim. On Windows, lossy UTF-8
    /// conversion is applied.
    pub fn intern_bytes(&mut self, bytes: &[u8]) -> PathHandle { ... }

    /// Transitions from mutable build phase to immutable consume phase.
    /// After this call, `intern()` will panic but `resolve()` becomes
    /// lock-free and `Sync`.
    pub fn freeze(&mut self) { ... }
}
```

### FileEntry accessors (changed signatures)

```rust
impl FileEntry {
    /// Returns the relative path name as a string.
    pub fn name<'a>(&self, arena: &'a PathArena) -> &'a str;

    /// Returns the relative path as a `Path` reference.
    pub fn path<'a>(&self, arena: &'a PathArena) -> &'a Path;

    /// Returns the interned parent directory path.
    pub fn dirname<'a>(&self, arena: &'a PathArena) -> &'a Path;

    /// Returns wire-format bytes for sorting and encoding.
    pub fn name_bytes<'a>(&self, arena: &'a PathArena) -> Cow<'a, [u8]>;
}
```

### Trait implementations on `PathHandle`

| Trait | Implementation | Notes |
|-------|---------------|-------|
| `Copy` | Derived (4-byte value) | Zero-cost clone |
| `Clone` | Derived | Delegates to `Copy` |
| `PartialEq` / `Eq` | Derived (`u32` equality) | Same-arena: string equality. Cross-arena: undefined |
| `PartialOrd` / `Ord` | **Not meaningful** for lexicographic path order | Insertion-order comparison only; sort uses `arena.resolve()` |
| `Hash` | Derived (`u32` hash) | Suitable for `HashMap<PathHandle, _>` within same arena |
| `Debug` | Shows `PathHandle(N)` | Does not resolve to string (no arena in scope) |
| `Send` | Auto (inner `NonZeroU32`) | Safe to send across threads |
| `Sync` | Auto (inner `NonZeroU32`) | Safe to share across threads |

**Note on Ord:** `PathHandle` derives `Ord` from `Spur`'s insertion
order, which is NOT lexicographic. File list sorting must resolve
handles to `&str` via the arena before comparison. The existing
`name_cmp` module already resolves paths before comparing; no semantic
change.

## Thread safety

### Build phase

`PathArena` in `Building` state wraps `Rodeo`, which is `!Sync`.
Build is single-threaded by construction:
- Receiver: sequential wire decode in `FileListReader`.
- Sender: sequential filesystem walk in the walker thread.

No `ThreadedRodeo` is needed. If a future parallel walker is added,
`ThreadedRodeo` is API-compatible and freezes to the same
`RodeoReader`.

### Consume phase

`PathArena` in `Frozen` state wraps `RodeoReader`, which is
`Send + Sync`. Lock-free reads use a flat indexed table (no hash
lookup on resolve - `RodeoReader` stores strings in a `Vec` indexed by
`Spur`). This enables:

- `rayon::par_iter` over `&[FileEntry]` with `&PathArena` shared.
- Parallel stat workers (existing `PARALLEL_STAT_THRESHOLD = 64`).
- Parallel sort comparisons via `name_cmp`.
- Parallel filter evaluation.
- Parallel delta generation scheduling.

`PathHandle` itself is `Copy + Send + Sync + 'static` - it can be
freely moved, shared, and stored in any thread-safe container.

## Drop semantics

### Arena bulk-free

Dropping a `PathHandle` does nothing. Handles are plain `u32` values
with no destructor. Memory reclamation happens exclusively when the
`PathArena` is dropped:

- `drop(Rodeo)` / `drop(RodeoReader)` frees all interned string
  chunks in O(chunks) - typically 1-4 large allocations for a
  million-entry file list.

This matches upstream rsync's `pool_destroy()` pattern: individual
`file_struct` entries do not free their `basename`/`dirname` pointers;
the pool is destroyed as a unit when the flist is discarded.

### Consequence: no per-entry reclamation

Once a string is interned, it remains allocated until the arena is
dropped. This is acceptable because:

1. File list entries are never individually removed during a transfer
   (they may be marked as excluded, but remain in the list).
2. INC_RECURSE segments have per-segment arenas; when a segment is
   consumed, its arena is dropped and all segment paths are freed.
3. The deduplication benefit (shared dirnames, repeated basenames)
   means the arena holds far fewer unique strings than entries.

### FileEntry::drop becomes trivial

With `PathHandle: Copy`, `FileEntry::drop` reduces to:
- Drop `Option<Box<FileEntryExtras>>` (only allocated for symlinks,
  devices, hardlinks, ACLs, xattrs).
- No `PathBuf::drop` (was: free heap string + vec metadata).
- No `Arc::drop` (was: atomic decrement + conditional free).

For typical file lists (95%+ regular files), `FileEntry::drop` is a
no-op. This eliminates the per-entry destructor cost that dominated
teardown time at scale.

## INC_RECURSE interaction

Each INC_RECURSE segment owns an independent `PathArena`:

```
Session
├── Segment 0: PathArena + Vec<FileEntry>  (dropped after consume)
├── Segment 1: PathArena + Vec<FileEntry>  (dropped after consume)
└── Segment N: PathArena + Vec<FileEntry>  (active)
```

Handles from segment 0 are invalid in segment 1's arena. This is safe
because segments are separate sort/compare/transfer domains - no
cross-segment handle comparison is needed. The per-segment lifetime
bounds memory growth, matching upstream's per-flist pool model.

## Migration path: callers requiring `Arc<Path>` semantics

### Callers that need change

Sites that currently hold `Arc<Path>` beyond the `FileEntry` lifetime
(i.e., store a cloned `Arc<Path>` independently):

| Caller | Current pattern | Migration |
|--------|----------------|-----------|
| `progress.rs:179` | `destination_root: Arc<Path>` | Unrelated to flist dirname; no change needed |
| `summary/event.rs:77` | `destination_root: Arc<Path>` | Unrelated to flist dirname; no change needed |
| Tests (`Arc::ptr_eq` assertions) | Verify sharing | Replace with `PathHandle` equality (`==`) |

No caller outside the `protocol` crate stores `FileEntry::dirname()`
as an `Arc<Path>` for use after the file list is dropped. The
`Arc<Path>` was used for two purposes:
1. **Sharing within the file list** - replaced by handle equality.
2. **Cheap clone during iteration** - replaced by `Copy` on handle.

### Callers that work unchanged

- `name_cmp.rs`: Already resolves `dirname()` to bytes for comparison.
  Migration: resolve via `arena` instead of deref.
- `sort.rs`: Comparator takes `&[FileEntry]` and calls `name_cmp`.
  Migration: thread `&PathArena` into comparator closure.
- `write/mod.rs`: Encodes path bytes onto wire. Migration: resolve
  handle to bytes before emit.
- `incremental/mod.rs`: `prepend_dir` re-interns the joined path.

### Pattern: accessor-with-arena-param

Every site that today calls `entry.name()` or `entry.dirname()`
receives the arena as a parameter. The file list itself provides a
convenience accessor:

```rust
impl FileList {
    pub fn entry_name(&self, idx: usize) -> &str {
        self.entries[idx].name(&self.arena)
    }
    pub fn entry_dirname(&self, idx: usize) -> &Path {
        self.entries[idx].dirname(&self.arena)
    }
}
```

For iteration:

```rust
let arena = file_list.arena();
for entry in file_list.entries() {
    let name = entry.name(arena);
    let dir = entry.dirname(arena);
    // ...
}
```

## Performance model

### Per-entry savings at 1M files

| Component | Before (bytes) | After (bytes) | Saving |
|-----------|---------------|---------------|--------|
| Inline `name` field | 24 | 4 | 20 B |
| Inline `dirname` field | 16 | 4 | 12 B |
| Heap: `PathBuf` backing (`name`) | ~32 (allocator class for ~20 B basename) | 0 (arena-owned) | ~32 B |
| Heap: `Arc<Path>` inner (amortized) | ~0.1 B (shared across ~100 entries/dir) | 0 | ~0.1 B |
| Atomic RC traffic per clone | 2 atomic ops | 0 | - |
| **Total per-entry saving** | | | **~64 B inline+heap** |

### RSS projection at 1M files

| Metric | Before | After | Reduction |
|--------|--------|-------|-----------|
| `Vec<FileEntry>` inline | 88 MB | 64 MB | 24 MB (-27%) |
| `PathBuf` heap (1M names) | ~32 MB | 0 | 32 MB (-100%) |
| Arena overhead | 0 | ~22 MB (unique strings + table) | +22 MB |
| `Arc<Path>` heap (shared dirs) | ~0.5 MB | 0 | 0.5 MB (-100%) |
| **Net RSS** | **~120 MB** | **~86 MB** | **~34 MB (-28%)** |

The arena stores each unique string once. For 1M files with ~10K
unique directories and ~200K unique basenames (typical monorepo), the
arena holds ~220K strings averaging 20 bytes = ~4.4 MB string data +
~17.6 MB hash table overhead (load factor ~0.7, 8-byte entries) = ~22
MB total.

### Compared to upstream rsync

Upstream at 1M files: `file_struct` is 24 bytes inline + pool-
allocated `basename`/`dirname` strings in contiguous chunks. Estimated
RSS: ~60-70 MB. After RSS-8.a, oc-rsync's ~86 MB is within 1.2-1.4x
of upstream - down from the current 3-11x gap.

### Resolve cost

`RodeoReader::resolve()` is a single indexed array access: `O(1)` with
no hash lookup. The `Spur` value is used directly as an index into a
`Vec<&str>`. This adds ~1 ns per resolve (L1 cache hit) versus the
current 0 ns (direct pointer deref on `Arc<Path>`). At 1M entries
with ~5 resolves per entry during transfer, total added cost is ~5 ms
- negligible compared to the 34 MB RSS reduction.

## Backward compatibility

### Public API changes required

All changes are in the `protocol` crate's `flist` module. The
`FileEntry` type is `pub` but its fields are `pub(super)`. The
accessor methods are the public API:

| Method | Current signature | New signature | Breaking? |
|--------|------------------|---------------|-----------|
| `name()` | `&self -> &str` | `&self, &PathArena -> &str` | Yes |
| `path()` | `&self -> &PathBuf` | `&self, &PathArena -> &Path` | Yes |
| `dirname()` | `&self -> &Arc<Path>` | `&self, &PathArena -> &Path` | Yes |
| `name_bytes()` | `&self -> Cow<[u8]>` | `&self, &PathArena -> Cow<[u8]>` | Yes |
| `set_dirname()` | `&mut self, Arc<Path>` | `&mut self, &mut PathArena, &Path` | Yes |
| `prepend_dir()` | `&mut self, &Path` | `&mut self, &mut PathArena, &Path` | Yes |
| `strip_leading_slashes()` | `&mut self` | `&mut self, &mut PathArena` | Yes |
| `new_file()` et al. | `PathBuf, ...` | `&mut PathArena, &str, ...` | Yes |
| `from_raw_bytes()` | `Vec<u8>, ...` | `&mut PathArena, &[u8], ...` | Yes |

### Mitigation: single-crate scope

All breaking changes are within the `protocol` crate's public API.
Callers are exclusively internal crates (`core`, `engine`,
`transfer`). No external consumers exist. The migration is a
workspace-internal refactor with no semver implications.

### Migration sequencing (from RSS-5)

1. **RSS-7** (done): Prototype `bumpalo`-backed `ArenaFileEntry` with
   feature gate. Validates the accessor-with-arena pattern.
2. **RSS-8.a** (this document): Define `PathHandle` type and
   `PathArena` API.
3. **RSS-8.b**: Implement `PathArena` wrapping `lasso::Rodeo` /
   `RodeoReader`. Replace `PathInterner` at `intern.rs`.
4. **RSS-8.c**: Replace `name: PathBuf` with `name: PathHandle`.
   Migrate constructors, `from_raw_bytes`, `strip_leading_slashes`.
   Size: 88 -> 72 B (Unix).
5. **RSS-8.d**: Replace `dirname: Arc<Path>` with
   `dirname: PathHandle`. Migrate `set_dirname`, `prepend_dir`,
   `extract_dirname`. Size: 72 -> 64 B.
6. **RSS-9**: Migrate all consumers (`name_cmp`, `sort`, `write`,
   `incremental`, engine, core, transfer) to pass `&PathArena`.
7. **RSS-10**: Benchmark validation. Tighten size assertion to 64 B.

Each step keeps golden wire-format tests green and shrinks
`size_of::<FileEntry>()` monotonically.

## Windows path encoding

`lasso`'s `Rodeo` interns `&str` (UTF-8). On Unix, rsync paths are
arbitrary bytes. On Windows, `PathBuf` uses WTF-8 internally.

Strategy: **byte-key arena shim.** The `PathArena` stores raw bytes
(`&[u8]`) and exposes both `resolve() -> &str` (with UTF-8 validation
deferred to display) and `resolve_bytes() -> &[u8]` (for wire
encoding). Implementation:

- On Unix: `intern_bytes()` stores bytes directly; `resolve()` returns
  `std::str::from_utf8_unchecked` (paths from wire are byte-level
  faithful, same as today's `OsStr::from_bytes`).
- On Windows: `intern_bytes()` applies lossy UTF-8 conversion (same as
  today's `from_raw_bytes` constructor); `resolve()` returns valid
  UTF-8.

If `lasso` 0.7 does not support `&[u8]` keys natively, the fallback
is a `bumpalo::Bump` + `HashMap<&[u8], PathHandle>` shim with
identical semantics (~10 lines). RSS-8.b will determine which path is
needed.

## Open questions for RSS-8.b

1. **lasso 0.7 byte-key support.** Verify whether `Rodeo` can intern
   `&[u8]` via the `Key + Interner` trait generics, or whether the
   shim is needed.
2. **`RodeoReader` index table memory.** Measure actual overhead of
   the reader's `Vec<&str>` at 200K unique strings to validate the
   22 MB estimate.
3. **`ThreadedRodeo` future-proofing.** Confirm that `PathArena` can
   swap its `Building` variant from `Rodeo` to `ThreadedRodeo` without
   changing `PathHandle` or `freeze()` semantics.
4. **Sort comparator ergonomics.** Determine whether to thread
   `&PathArena` as a closure capture in `sort_unstable_by` or wrap
   entries in a `SortKey` newtype that carries the arena reference.

## Cross-references

- `crates/protocol/src/flist/entry/core.rs:32-72` - current struct.
- `crates/protocol/src/flist/entry/accessors.rs:11-481` - current
  accessors.
- `crates/protocol/src/flist/entry/constructors.rs:18-174` - current
  constructors.
- `crates/protocol/src/flist/intern.rs:42-114` - current
  `PathInterner`.
- `crates/protocol/src/flist/entry/tests.rs:293-311` - size assertion.
- `docs/design/rss-4-arena-allocator-eval.md` - crate selection.
- `docs/design/rss-5-fileentry-pool-shape.md` - target layout and
  lifecycle.
- `target/interop/upstream-src/rsync-3.4.1/flist.c` - upstream pool
  allocator and `f_name()` path reconstruction.
- `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c` -
  upstream bulk-free pattern.
