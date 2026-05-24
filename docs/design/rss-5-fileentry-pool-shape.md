# RSS-5: pool-allocator-backed `FileEntry` shape

Task: RSS-5. Branch: `docs/rss-5-fileentry-pool-shape`.
Prerequisites: `docs/audits/rss-3-fileentry-size-breakdown.md` (cost
analysis), `docs/design/rss-4-arena-allocator-eval.md` (recommends
`lasso` / `Rodeo` + `RodeoReader`). Downstream: RSS-6 (back-compat
assessment), RSS-7/8 (prototype + staged migration). Structural
spec; the prototype lives in RSS-7.

> **Prototype landed in RSS-7.** A feature-gated, parallel
> `ArenaFileEntry<'arena>` shape backed by `FilePath::Owned(PathBuf) |
> Arena(&'arena Path)` lives at
> `crates/protocol/src/flist/entry/arena.rs`. The `bumpalo` arena was
> selected for the prototype because the shape (`&'arena Path`) maps
> 1:1 to a bump-allocator slice; the `lasso` interner remains the
> RSS-8/RSS-9 target. Default builds are byte-identical: the
> `FilePath::Arena` variant is uninhabited
> (`std::convert::Infallible`-backed) and `bumpalo` is not pulled in.
> Migration of every consumer (sort/filter/transfer) is RSS-9; the
> RSS-10 benchmark task picks up from the prototype to measure
> per-entry allocator savings.

## Goals

1. Drop `FileEntry` inline from **88 B Unix / 104 B Windows** to
   **64 B on both** by replacing `name: PathBuf` (24 B) and
   `dirname: Arc<Path>` (16 B) with two `Spur` handles (4 B each).
2. Eliminate the per-entry `PathBuf` allocation for `name` and the
   per-unique-dir `ArcInner<Path>` allocation for `dirname`.
3. Eliminate the atomic-RC traffic on `FileEntry::clone()`.
4. Preserve every public API signature where reasonable; explicitly
   flag the ones that must change.
5. Stay wire-compatible: the interner is in-memory only.

## Non-goals

- `FileEntryExtras` stays boxed. Pool allocation here is for the
  path fields only; extras already pay at most one heap allocation
  per entry that needs them.
- `link_target` storage is deferred. Symlink workloads are rare and
  per-entry-unique; revisit only if RSS-9 measures regression.
- `Vec<FileEntry>` doubling slack is `rss-flist-vec-vs-pool.md`'s
  scope.
- No wire protocol changes
  (per `feedback_no_wire_protocol_features`).

## New struct layout

### Before (current, 88 B Unix / 104 B Windows)

```rust
pub struct FileEntry {
    pub(super) name: PathBuf,                          // 24 B (32 B Windows)
    pub(super) dirname: Arc<Path>,                     // 16 B
    pub(super) size: u64,                              // 8 B
    pub(super) mtime: i64,                             // 8 B
    pub(super) uid: Option<u32>,                       // 8 B
    pub(super) gid: Option<u32>,                       // 8 B
    pub(super) extras: Option<Box<FileEntryExtras>>,   // 8 B
    pub(super) mode: u32,                              // 4 B
    pub(super) mtime_nsec: u32,                        // 4 B
    pub(super) flags: FileFlags,                       // 3 B + 1 B pad
    pub(super) content_dir: bool,                      // 1 B + 3 B tail
}
// = 88 B Unix, 104 B Windows
```

### After (target, ~64 B Unix / ~80 B Windows)

```rust
pub struct FileEntry {
    // 8-byte aligned fields (sized to keep extras null-niche-optimised)
    pub(super) size: u64,                              // 8 B
    pub(super) mtime: i64,                             // 8 B
    pub(super) uid: Option<u32>,                       // 8 B
    pub(super) gid: Option<u32>,                       // 8 B
    pub(super) extras: Option<Box<FileEntryExtras>>,   // 8 B

    // 4-byte aligned fields
    pub(super) name: Spur,                             // 4 B (NonZeroU32 niche)
    pub(super) dirname: Spur,                          // 4 B (NonZeroU32 niche)
    pub(super) mode: u32,                              // 4 B
    pub(super) mtime_nsec: u32,                        // 4 B

    // 2-byte / 1-byte aligned
    pub(super) flags: FileFlags,                       // 3 B
    pub(super) content_dir: bool,                      // 1 B
    // tail pad to 8-B alignment: 0 B (perfectly packed)
}
// = 64 B Unix; 64 B Windows (Spur is platform-independent)
```

Field-by-field accounting:

| Field | Before B | After B | Notes |
|---|---|---|---|
| `name` | 24 (PathBuf) | 4 (Spur) | -20 B inline, -1 heap alloc/entry |
| `dirname` | 16 (Arc<Path>) | 4 (Spur) | -12 B inline, -1 atomic/clone |
| `size`/`mtime`/`uid`/`gid`/`extras` | 40 | 40 | unchanged |
| `mode`/`mtime_nsec` | 8 | 8 | unchanged |
| `flags`/`content_dir` | 4 + 4 pad | 4 | tail pad collapses |
| **Total inline** | **88** | **64** | **-24 B per entry (-27 %)** |

Windows: `Spur` is platform-independent so the `Wtf8Buf` padding cost
on `PathBuf` is erased - Windows drops from 104 B to 64 B (-38 %).
The size assertion at `tests.rs:299-311` collapses to a single
`MAX = 64` with no per-platform branch.

`Option<Spur>` is also 4 B (`Spur` is `NonZeroU32`, niche-filled).

## Where the interner lives

A new per-flist owner replaces the current `PathInterner` in
`crates/protocol/src/flist/read/mod.rs:116`:

```rust
pub struct PathInterner {
    inner: PathInternerState,
}

enum PathInternerState {
    Building(Rodeo),         // mutable insert during decode/build
    Frozen(RodeoReader),     // immutable read during transfer/consume
}
```

`Rodeo` is mutable and single-threaded - used during the entire
build phase (sender walk, receiver wire decode). `RodeoReader` is
`Sync` with lock-free reads (see Concurrency) - used by every
consumer after freeze.

The `PathInterner` is owned by whatever currently owns the
`Vec<FileEntry>`:

- `FileListReader` keeps its `dirname_interner` field; the type
  changes from today's `HashMap<PathBuf, Arc<Path>>` to the
  `Rodeo`-backed one.
- The sender's flist builder (does not intern today, see RSS-3
  \S "smoking gun") gains an owned `Rodeo`.
- The consumed `FileList` holds a `RodeoReader` alongside its
  `Vec<FileEntry>`. They form one unit of ownership.

A `FileEntry` is meaningless without its interner - a deliberate
invariant (see Failure modes).

## API shape: accessor-with-interner-param

**Chosen: `entry.name(&interner) -> &str`.** The interner is threaded
through accessor calls. Existing call sites already have a
`FileList`-shaped parent in scope, so `&interner` is a one-token add.

```rust
impl FileEntry {
    pub fn name<'a>(&self, paths: &'a PathInterner) -> &'a str {
        paths.resolve(self.name)
    }
    pub fn dirname<'a>(&self, paths: &'a PathInterner) -> &'a Path {
        Path::new(paths.resolve(self.dirname))
    }
}
```

**Rejected: `entry.view(&interner) -> FileEntryView<'_>`.** A wrapper
holding the interner ref. Cleaner at call sites but injects a lifetime
into every consumer, doubles the API surface during migration, and
re-introduces per-iteration value construction equivalent to the
Arc-clone churn we are removing.

Accessor-with-param keeps `FileEntry` POD-like (no lifetime parameter,
iteration unaffected). `Spur: Copy + 'static`; the interner's lifetime
is only on the returned `&str`/`&Path`, matching today's `Arc<Path>`
borrow semantics. A small `FileListEntryRef<'a>` convenience type can
sit on top for sites that want zero-arg accessors during iteration.

## Lifecycle

1. **Construct.** `FileListReader::new()` (or sender walker) creates
   an empty `Rodeo` at the same site as today's `PathInterner::new()`.
2. **Build.** For every entry, call `interner.get_or_intern(basename)`
   and `get_or_intern(dirname)`, store the `Spur` handles. Phase is
   single-threaded by construction.
3. **Freeze.** After sort+dedup, call `PathInterner::freeze()` to
   move `Rodeo` -> `RodeoReader`. Same handoff point as today's
   decoder-to-consumer transition.
4. **Consume.** Readers receive `&PathInterner` (or `&RodeoReader`
   directly at hot sites). Reads are lock-free.
5. **Drop.** `drop(FileList)` frees all interned strings in one pass.
   `FileEntry::drop` becomes trivial (no `PathBuf::drop`, no
   `Arc::drop`); matches upstream's `pool_destroy()` O(extents)
   teardown.

### INC_RECURSE interaction

INC_RECURSE produces per-segment flists. **Pick: per-segment
interner.** Each segment owns its `Rodeo`; handles are local to
their segment; memory is freed when the segment is consumed. Reasons:

1. Upstream's `pool_alloc()` is per-flist and destroyed with it
   (`flist.c:1018`, `pool_alloc.c`); per-segment matches that lifetime.
2. A per-session interner would defeat INC_RECURSE's memory-growth
   bound.
3. Cross-segment handle comparison is not required - segments
   already separate compare/sort domains.

`prepend_dir(parent: &Path)` (`accessors.rs:55`) becomes
`prepend_dir(&mut self, paths: &mut PathInterner, parent: &Path)`
and re-interns the joined path - one `get_or_intern` per call.

## Concurrency

**Confirmed Sync claims.** Per `lasso` 0.7 docs
(https://docs.rs/lasso/0.7/lasso/struct.RodeoReader.html):

> `RodeoReader` is a read-only view of a `Rodeo`, intended for use in
> situations where the interner does not need to be mutated. Because
> it is read-only, it can be used in multi-threaded contexts. ...
> Implements `Send + Sync`.

`Spur` is `Copy + Send + Sync + 'static`
(https://docs.rs/lasso/0.7/lasso/struct.Spur.html).

**Build-side: single-threaded.** `Rodeo` is `!Sync`; build runs on
the decoder or walker thread (matches today). RSS-5 does not require
`ThreadedRodeo` (avoids `dashmap` + `parking_lot` and write-lock
contention). A future parallel walker upgrades cleanly to
`ThreadedRodeo` - it is API-compatible and also freezes to
`RodeoReader`.

**Consume-side: full parallelism.** `&RodeoReader` flows into any
rayon `par_iter` consumer; reads are lock-free flat-table lookups.
Matches `PARALLEL_STAT_THRESHOLD = 64`.

**Drop ordering.** Structural: the interner and `Vec<FileEntry>`
co-own through `FileList`; the accessor API takes `&PathInterner`,
so stale-`Spur` use is a borrow-check error. No runtime check needed.

## Backward-compat surface

Every public API on `FileEntry` that today returns `&Path` / `&PathBuf`
/ `&str` / `&Arc<Path>`. Marked **K**eep, **C**hange-with-wrapper, or
**M**ust-change.

| API today | Returns today | Disposition | New signature |
|---|---|---|---|
| `name(&self) -> &str` | `&str` | **M** | `name(&self, paths: &PathInterner) -> &str` |
| `path(&self) -> &PathBuf` | `&PathBuf` | **M** | `path(&self, paths: &PathInterner) -> &Path` |
| `dirname(&self) -> &Arc<Path>` | `&Arc<Path>` | **M** | `dirname(&self, paths: &PathInterner) -> &Path` |
| `set_dirname(&mut self, Arc<Path>)` | () | **M** | `set_dirname(&mut self, &mut PathInterner, &Path)` |
| `name_bytes(&self) -> Cow<'_, [u8]>` | `Cow<'_, [u8]>` | **C** | adds `&PathInterner` param; same return |
| `prepend_dir(&mut self, &Path)` | () | **M** | adds `&mut PathInterner` param; re-interns |
| `strip_leading_slashes(&mut self)` | () | **M** | adds `&mut PathInterner` param; re-interns |
| `new_file`/`new_directory`/...constructors | `Self` | **M** | take `&mut PathInterner` instead of `PathBuf` |
| `from_raw_bytes(Vec<u8>, ...)` | `Self` | **M** | `from_raw_bytes(&mut PathInterner, &[u8], ...)` |
| `link_target(&self) -> Option<&PathBuf>` | `Option<&PathBuf>` | **K** | unchanged (extras stay boxed) |
| `clone(&self) -> Self` | `Self` | **C** | unchanged; now Arc-clone-free, may add `Copy` once extras land in arena |
| `size`/`mtime`/`mode`/`uid`/`gid`/... | scalar | **K** | unchanged |

`PartialEq for FileEntry` today does byte-compare on `name`. After:
same-interner `Spur` equality is `u32 ==` (correct and faster);
cross-interner equality is meaningless. Remove the auto-derive on
`FileEntry` and provide `entry.eq(other, &paths)` instead.

## Migration plan (RSS-7 / RSS-8 sequencing)

Staged so `size_of::<FileEntry>` shrinks monotonically and golden
tests stay green at every step.

**RSS-7: `PathBuf -> Spur` first.** `name: PathBuf` is the larger
field (24 B) and is RSS-3's #1 contributor. Add `Rodeo`/`RodeoReader`
to `FileListReader` and the sender builder; co-store `name: Spur`
beside `name: PathBuf` for one PR (size assertion relaxed); flip
readers to consult `Spur` first; migrate constructors to take an
interner; drop legacy `name: PathBuf`. Size: 88 -> 72 B Unix.

**RSS-8: `Arc<Path> -> Spur` second.** Replace `dirname: Arc<Path>`
with `dirname: Spur` in the same interner; migrate `set_dirname`,
`prepend_dir`, `strip_leading_slashes`; drop the `extract_dirname()
-> Arc<Path>` helper. Size: 72 -> 64 B.

This ordering banks the dominant allocation-path win before touching
the atomic-RC subsystem; if RSS-8 hits an INC_RECURSE edge, RSS-7's
gains are already in.

## Failure modes

1. **Interner dropped before `Spur` used.** Compile-time impossible:
   `name(&self, paths: &PathInterner) -> &str` borrows the interner;
   the borrow checker refuses to let it drop while the returned
   `&str` is live. Dangling-handle access is rejected at compile time.
2. **`Spur` from wrong interner.** Both interners hand out
   `NonZeroU32` values starting at 1, so a wrong-interner resolve
   returns a different string (or panics on out-of-range). Public
   boundaries use `try_resolve -> Option<&str>`; internal sites use
   `resolve` when both halves are co-owned. Cross-interner
   contamination is a bug, not a recoverable error.
3. **Empty path.** Today's `PathInterner` short-circuits `Path::new("")`
   to a cached `Arc`. The new design interns `""` explicitly; all
   root-level entries share `Spur(1)`. `extract_dirname` collapses
   to a one-line `get_or_intern("")` call.
4. **`Spur::default()`.** lasso does not impl `Default` on `Spur`.
   `FileEntry` does not derive `Default`, so no churn.

## Answers to RSS-4 open questions

1. **One rodeo or two?** **One.** A single per-flist `Rodeo` interns
   both basenames and dirnames. Two rodeos adds handle-ambiguity for
   no measurable win at realistic working-set sizes; `Spur`'s `u32`
   covers up to 2^32-1 entries. `MicroSpur` (u16) would cap dirnames
   at 65535 (uncomfortable on monorepos). Revisit only if profiling
   shows the unified hash hot.
2. **Sender vs receiver coverage.** **Both.** Sender's walker does
   not intern today (RSS-3 "smoking gun"). RSS-7 gives the walker
   its own `Rodeo`; sender and receiver never share an in-memory
   flist, so no double-interning.
3. **Symlink target storage.** **Deferred.** `extras.link_target`
   stays `Option<PathBuf>`. Symlinks are rare and per-entry-unique;
   if RSS-9 finds regression, fold into the same `Rodeo` as
   `Option<Spur>`.
4. **Wire-replay ordering.** **Per-segment interner.** Each
   INC_RECURSE segment owns an independent `Rodeo`; the previous
   segment's `RodeoReader` is dropped before the next is built. The
   wire encoder always resolves to bytes before emit; cross-segment
   handle leakage is impossible.
5. **Bench harness reuse.** Tighten `tests.rs:299-311` to `MAX = 64`
   (no per-platform branch) and `benches/file_entry_memory.rs`
   accordingly after RSS-8. Add a separate `size_of::<PathInterner>`
   bench so interner growth is tracked but not charged against the
   per-entry cap.
6. **Migration sequencing.** RSS-7 (`PathBuf -> Spur`, 88 -> 72 B)
   then RSS-8 (`Arc<Path> -> Spur`, 72 -> 64 B); each step keeps
   wire format, goldens, and interop green; each tightens the size
   assertion.
7. **`lasso` feature flags.** Default features only (`hashbrown` +
   `ahash`, already transitively present - confirm with `cargo tree`
   in RSS-7). Do **not** enable `multi-threaded` (no `ThreadedRodeo`)
   or `serde` (handles are runtime-only). Net new direct deps in
   transitive closure: zero beyond `lasso` itself.
8. **Path encoding on Windows.** **Byte-key rodeo.** lasso's
   `Interner`/`Resolver` traits are generic over the value type;
   use a `[u8]`-keyed variant to preserve the `as_encoded_bytes()`
   round-trip in `from_raw_bytes` (`constructors.rs:138-173`).
   `to_string_lossy` is rejected (drops non-UTF-8 bytes upstream
   transmits faithfully). If lasso 0.7 lacks the byte-key variant,
   fall back to a `bumpalo::Bump` + `HashMap<&[u8], Spur>` shim -
   identical semantics, ~10 LoC.

## Cross-references

- `crates/protocol/src/flist/entry/core.rs:32-83`, `accessors.rs:11-481`,
  `constructors.rs:18-173`, `extras.rs:13-56`, `tests.rs:293-311` -
  the struct, accessor, constructor, extras, and size-assertion sites.
- `crates/protocol/src/flist/intern.rs:42-114` - `PathInterner` to
  retype to `Rodeo` / `RodeoReader`.
- `crates/protocol/src/flist/read/mod.rs:116,168,656-659` - decoder
  ownership and per-entry intern call site.
- `crates/protocol/src/flist/name_cmp.rs`, `sort.rs`, `incremental/mod.rs`
  - consumers of `name()`/`dirname()` that take the new interner ref.
- `crates/protocol/benches/file_entry_memory.rs:19,37-40` - bench
  assertion to tighten to 64 B.
- `docs/audits/rss-3-fileentry-size-breakdown.md`,
  `docs/design/rss-4-arena-allocator-eval.md` - cost analysis and
  `lasso` rationale.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697,1018-1027,
  1421-1442` and `lib/pool_alloc.c:1-175` - upstream `pool_alloc`
  and `lastdir` semantics this lifecycle mirrors.
