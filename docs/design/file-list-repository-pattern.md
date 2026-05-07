# File list as a queryable repository

Task: #2135. Branch: `docs/flist-repository-pattern-2135`. Companion
context: #1037 (`FileEntry` 100K bench, completed), #1864 (full vs
incremental flist memory benchmark plan), #1048 (PathBuf/Arc<Path>
RSS overhead, completed), #1049 (string interning, completed), #1050
(`Vec<FileEntry>` vs upstream pool allocator, pending), #966 (RSS gap
context).

## 1. Goal

Replace the de facto `Vec<FileEntry>` flist representation that is
shared across `crates/flist`, `crates/transfer`, and `crates/engine`
with a single trait-based abstraction. Today the file list is exposed
as one concrete type (`Vec<FileEntry>` plus a thin `FileList` wrapper
in `crates/transfer/src/pipeline/job.rs:38-82`), and every caller pokes
at it directly: index access, full-list iteration, contiguous-range
iteration for INC_RECURSE segments, and an O(N) scan to find hardlink
siblings. The lack of an abstraction layer prevents three independent
storage strategies (vec-backed, arena-backed, mmap-backed) from coexisting,
forces every memory experiment under #1050 to fork the data structure,
and makes the receiver-side `Vec<FileEntry>` field
(`crates/transfer/src/receiver/mod.rs:106`) a hard dependency for any
caller that wants to query "give me the leader of this hardlink follower"
without scanning.

The deliverable is a `trait FlistRepository` in `crates/protocol/src/flist`
(re-exported through the `flist` crate) with four canonical access
methods, three concrete backings (`VecBacked`, `ArenaBacked`,
`MmapBacked`), and a migration ladder that replaces direct
`Vec<FileEntry>` use site-by-site without flag-day coordination
between transfer, generator, receiver, engine, and pipeline crates.

## 2. Existing access patterns

`grep -rn 'Vec<FileEntry>\|FileList' crates/flist/src/ crates/transfer/src/ crates/engine/src/`
returns 305 hits across 22 files. The hits cluster into four
categorical access shapes; everything else is constructor or test
plumbing. Numbers below are line references at HEAD.

### 2.1 By-NDX lookup (random access, hot path)

The protocol delta-encodes NDX values; every wire frame causes a single
`file_list[ndx]` read. Sites:

- `crates/transfer/src/generator/protocol_io.rs:178,182,238,308` -
  itemize callback, `send_file_list` initial segment loop,
  `encode_and_send_segment` per-entry write.
- `crates/transfer/src/generator/transfer.rs:250,272` - per-NDX entry
  resolution while waiting on receiver acks.
- `crates/transfer/src/generator/mod.rs:747-753` - bounds check on the
  generator's main dispatch path.
- `crates/transfer/src/receiver/transfer.rs:594,777` - receiver's
  `Vec::get(idx)` for ack lookups (the only `get` rather than index;
  uses `Option`).
- `crates/transfer/src/receiver/transfer/candidates.rs:137` - per-stat
  result post-processing.
- `crates/transfer/src/receiver/directory/creation.rs:105` -
  per-directory metadata application.
- `crates/transfer/src/pipeline/job.rs:54-57` - `FileList::get(ndx)`,
  the only NDX accessor wrapped in `Option`-returning safety today.

Frequency: O(N) where N is total flist size; this is the hottest read
path on the wire. NDX values arrive out of order on protocol >= 30
because the receiver acks files in stat order, not list order.

### 2.2 By-directory iteration (contiguous range, INC_RECURSE)

Once entries are sorted by `(dirname, basename)`
(`crates/protocol/src/flist/sort.rs`), one directory's worth of
entries is a contiguous slice. The INC_RECURSE pipeline reads them
that way:

- `crates/transfer/src/generator/protocol_io.rs:307-311`
  `encode_and_send_segment` iterates `segment.flist_start..end`.
- `crates/transfer/src/receiver/file_list.rs:53,185,194,204-209` -
  `read_entry_with_flist(reader, &self.file_list[seg_start..])`,
  `sort_file_list(&mut self.file_list[flat_start..], ...)`,
  `match_hard_links(&mut self.file_list[flat_start..], ...)`,
  `normalize_pre30_hardlinks(&mut self.file_list[flat_start..])`.
  All four take a `&mut [FileEntry]` slice keyed by segment start.
- `crates/protocol/src/flist/incremental/mod.rs:80-94,147-201,239-262`
  - `IncrementalFileList` stages entries in
  `pending: HashMap<String, Vec<FileEntry>>` keyed by parent
  directory, draining via `release_pending_children`.

Frequency: O(D) ranges where D is the number of directories. The
receiver's `read_entry_with_flist` slice access dominates the
INC_RECURSE memory shape - it needs the previous segment's tail for
incremental name encoding (`flist.c:577-590` uses `lastname`,
`lastdir`, `lastdir_len` static state).

### 2.3 Hardlink-sibling search (O(N) by (dev, ino), then O(1) by NDX)

`crates/transfer/src/generator/file_list/hardlinks.rs:46-70` walks
the entire file list to assign hardlink indices. The walk is O(N) but
the per-entry work is O(1) thanks to a separate
`HardlinkTable` keyed by `DevIno`
(`crates/protocol/src/flist/hardlink/table.rs`). The flist itself is
queried only by index inside the loop (`self.file_list[i]`,
`self.file_list[i].set_hardlink_idx(...)`); the (dev, ino) lookup is
indirected through the side table.

Receiver-side reverse: `crates/transfer/src/receiver/quick_check.rs:30-31`
checks `is_hardlink_follower(entry)` per entry, then resolves the
leader via `entry.hardlink_idx()` which is a wire NDX, dereferenced
back through the flist with another by-NDX lookup. So hardlink
sibling search is *not* a flist query in steady state; it is a
pre-computation against a separate map (`HardlinkTable`,
`crates/protocol/src/flist/hardlink/table.rs:1-180`) that runs once
during `assign_hardlink_indices` and afterwards every lookup is by
NDX.

Implication for the repository trait: hardlink resolution does *not*
need to be a first-class repository method. The repository only owes
"give me entry by NDX" plus "let me iterate to find candidates" -
the (dev, ino) → NDX index is a side structure that lives next to
the repository, not inside it. Section 4 keeps this orthogonal.

### 2.4 Predicate filter (full scan)

Rare but unavoidable. Sites:

- `crates/transfer/src/generator/file_list/hardlinks.rs:93-115`
  `collect_id_mappings` - iterate all entries, collect uid/gid set.
- `crates/transfer/src/generator/transfer.rs:671` -
  `let total_size: u64 = self.file_list.iter().map(|e| e.size()).sum();`
- `crates/transfer/src/receiver/transfer.rs:127,473,643,815` -
  iterating to build per-file work units, summing sizes for stats,
  and `for (file_idx, file_entry) in self.file_list.iter().enumerate()`
  in the main receive loop.
- `crates/transfer/src/generator/file_list/mod.rs:101-105,217-221` -
  collecting names for tracing.
- `crates/transfer/src/generator/file_list/inc_recurse.rs:71-124`
  - `classify_file_list_entries` walks the list once to bucket
  entries into top-level vs nested-directory segments.

Frequency: at most twice per transfer (once during build, once during
finalization). Acceptable as a `&dyn Iterator<Item = &FileEntry>`
return on the trait. No need for indexed predicate evaluation.

### 2.5 Mutation surface

For honesty: the repository is not pure read-side. Three mutating
sites exist and have to be modelled by the trait:

- `crates/transfer/src/generator/file_list/hardlinks.rs:57-68` -
  `set_hardlink_idx`, `set_hardlink_dev(0)`, `set_hardlink_ino(0)`
  per-entry mutation post-sort.
- `crates/transfer/src/receiver/file_list.rs:87,194` -
  `for (i, entry) in self.file_list.iter_mut().enumerate()` to
  apply ID mapping translations and post-sort hardlink normalization.
- `crates/transfer/src/generator/file_list/inc_recurse.rs:152-170` -
  full reorder via `std::mem::take` + push, replacing the entire
  underlying `Vec` in place.

The reorder is a one-shot bulk mutation; the per-entry mutations are
field-only. Section 4's trait separates these: bulk
construction/reorder via builder (consumes/moves the backing), in
flight per-entry mutation via a `get_mut(ndx)` accessor.

## 3. Repository trait

```rust
/// Read-side abstraction over a sorted, frozen file list.
///
/// All implementations guarantee:
/// - NDX values are stable indices into the backing storage
/// - Entries are sorted by (dirname, basename) per upstream
///   sort_file_list (`crates/protocol/src/flist/sort.rs`)
/// - `len()` is O(1)
/// - `get()` is O(1)
pub trait FlistRepository {
    /// Returns the entry at NDX, or `None` if out of range.
    /// Replaces `self.file_list[ndx]` and `Vec::get(ndx)` sites.
    fn get(&self, ndx: u32) -> Option<&FileEntry>;

    /// Returns a contiguous slice for [start, end). Used for INC_RECURSE
    /// segment iteration where lastname/lastdir state must traverse the
    /// segment in declared order. Slice cost is O(1).
    /// Replaces `&self.file_list[seg_start..seg_end]` sites.
    fn range(&self, start: u32, end: u32) -> &[FileEntry];

    /// Returns an iterator over entries in `parent_dir`, in the
    /// repository's canonical (dirname, basename) order. The default
    /// impl uses `range` over the parent's [start, end] from a
    /// directory index built at construction time.
    /// Replaces ad-hoc parent-keyed walks; mirrors upstream's
    /// `flist.c:add_dirs_to_tree` traversal shape.
    fn iter_dir<'a>(
        &'a self,
        parent: &str,
    ) -> Box<dyn Iterator<Item = (u32, &'a FileEntry)> + 'a>;

    /// Returns the wire NDX values of entries in the same hardlink
    /// group as `ndx`. The first element is the group leader (or
    /// `ndx` itself if no other follower exists). Backed by a
    /// `(dev, ino) -> Vec<u32>` side index built during construction.
    /// Replaces the `HardlinkTable::find_or_insert` round-trip plus
    /// per-entry NDX dereference.
    fn find_hardlinks_for(&self, ndx: u32) -> &[u32];

    /// Returns total entry count.
    fn len(&self) -> u32;

    /// Returns true if empty.
    fn is_empty(&self) -> bool { self.len() == 0 }

    /// Returns an iterator over all entries with their NDX, for
    /// the full-scan predicate-filter path (uid/gid collection,
    /// total size sums, name traces). The iterator is non-mutating;
    /// mutation goes through `entries_mut`.
    fn iter<'a>(
        &'a self,
    ) -> Box<dyn Iterator<Item = (u32, &'a FileEntry)> + 'a>;
}

/// Mutable construction-time view. Held only by the builder and
/// dropped before the repository is shared via `Arc`.
pub trait FlistRepositoryMut: FlistRepository {
    /// Per-entry field mutation (hardlink_idx, dev/ino clear,
    /// id translation). Cost is O(1) and must not invalidate the
    /// directory or hardlink side indices unless the mutation
    /// changes them; implementations panic if it does.
    fn get_mut(&mut self, ndx: u32) -> Option<&mut FileEntry>;

    /// Bulk reorder, consumes the repository and returns a fresh
    /// one with rebuilt side indices. Used exclusively by
    /// `partition_file_list_for_inc_recurse`
    /// (`crates/transfer/src/generator/file_list/inc_recurse.rs:152-170`).
    fn reorder(self, perm: &[u32]) -> Self where Self: Sized;
}
```

The split between `FlistRepository` and `FlistRepositoryMut` mirrors
the `FileList::new(Vec<FileEntry>)` -> `Arc<Vec<FileEntry>>` freeze
pattern already in
`crates/transfer/src/pipeline/job.rs:38-82`. After `freeze()`, only
`FlistRepository` is exposed through `Arc<dyn FlistRepository>`; the
mut handle stays in the construction site (generator file-list build
phase or receiver `read_entry_with_flist` loop).

`iter_dir` and `find_hardlinks_for` need side indices that the trait
constructs once during `freeze()`:

- `dir_index: BTreeMap<&str, (u32, u32)>` mapping parent → range.
  Built during sort because sort already produces dirname-grouped
  output.
- `hardlink_groups: HashMap<DevIno, Vec<u32>>` built during
  `assign_hardlink_indices`. Today this lives in
  `crates/protocol/src/flist/hardlink/table.rs`; under the trait it
  moves to a co-located `FlistIndexes` struct that the repository
  owns.

## 4. Backing implementations

Three implementations, each with a distinct memory and locality
profile. The benchmark anchor for "good" is upstream's 7.4-7.9 MB
peak RSS at 100 K (`docs/benchmarks/flist-memory-baseline-2026-05-01.md:50-58`).

### 4.1 VecBacked (current, default)

```rust
pub struct VecBacked {
    entries: Arc<Vec<FileEntry>>,
    dir_index: Arc<BTreeMap<Box<str>, (u32, u32)>>,
    hardlink_groups: Arc<HashMap<DevIno, Vec<u32>>>,
}
```

Identical to today's `crates/transfer/src/pipeline/job.rs:38-42`
plus the two side indices. Path strings and `Box<FileEntryExtras>`
are owned per-entry as today
(`crates/protocol/src/flist/entry/core.rs:32-83`,
`crates/protocol/src/flist/entry/extras.rs`).

`get` is `entries.get(ndx)`. `range` is `&entries[start..end]`.
`iter_dir` looks up `dir_index[parent]` then calls `range`.
`find_hardlinks_for` resolves the entry's `(dev, ino)` (or
`hardlink_idx` if the (dev, ino) was already cleared per protocol
>= 30) through the precomputed map.

Per-entry footprint: 110-138 B (audit table at
`docs/audits/pathbuf-arc-path-rss-overhead.md:198-209`). At 100 K
this lands at 42.6 MB peak in the 2026-05-01 baseline (Mode B,
sender-side). At 1 M, 218.5 MB.

This is the migration target for v0; it is the existing storage with
a trait wrapper, no change to allocation behaviour.

### 4.2 ArenaBacked (single-allocation path strings)

```rust
pub struct ArenaBacked {
    entries: Arc<Vec<FileEntryRef>>, // 32 B fixed-size POD
    paths: Arc<Bytes>,               // contiguous bump arena
    extras_pool: Arc<Vec<FileEntryExtras>>,
    dir_index: Arc<BTreeMap<u32, (u32, u32)>>, // keyed by path offset
    hardlink_groups: Arc<HashMap<DevIno, Vec<u32>>>,
}

#[repr(C)]
struct FileEntryRef {
    path_offset: u32,   // byte offset into paths arena
    path_len: u32,
    extras_idx: u32,    // u32::MAX if absent
    mode: u32,
    size: u64,
    mtime: i64,
    flags: u8,
    // total: 32 B (vs current 96 B inline)
}
```

Path strings are concatenated into a single `bytes::Bytes` arena
during construction; the per-entry `PathBuf` and `Arc<str>` headers
disappear. The audit's per-entry math forecasts a savings of
27-36 % of the 100 K RSS gap, i.e. 9-13 MB at 100 K and 90-130 MB
at 1 M (`docs/audits/pathbuf-arc-path-rss-overhead.md:212-238`).

Construction: the receiver's
`crates/transfer/src/receiver/file_list.rs:53-89` wire reader writes
each decoded path directly into the bump arena instead of allocating
a `String`. The arena resizes once per allocator class
(`Vec::reserve(N)` upfront via the wire byte-count hint that upstream
already sends). Sort happens over `FileEntryRef` plus a borrowed
`&str` view; the comparator never touches the arena, only the
fixed-size POD.

`extras_pool` collapses `Box<FileEntryExtras>` into a single Vec; the
pool is grown lazily, and entries without extras get `u32::MAX`
matching the existing `Option<Box<FileEntryExtras>>` pattern at
`crates/protocol/src/flist/entry/core.rs:51-55`.

`get` returns a `FileEntry` view assembled from `FileEntryRef` plus
the arena slice and extras lookup. `range` returns
`&[FileEntryRef]` and the public API unifies via `FileEntryView<'a>`
(see Section 6 on API drift).

Constraint: this backing requires the wire reader to feed the arena,
so the sender-side `crates/transfer/src/generator/file_list/walk.rs`
construction path must also write into an arena. Both directions go
through `FlistRepositoryBuilder::push(name, mode, size, ...)` which
internalises arena writes; callers never see the arena directly.

### 4.3 MmapBacked (huge workloads, read-only post-construction)

```rust
pub struct MmapBacked {
    file: Arc<memmap2::Mmap>,
    header: FlistHeader, // 32 B fixed prefix
    entries: &'static [FileEntryRef], // SAFETY: borrowed from mmap
    paths: &'static [u8],
    dir_index: &'static [(u32, u32, u32)], // (path_offset, start, end)
    hardlink_groups: HashMap<DevIno, Vec<u32>>, // built on load
}
```

For workloads where the flist exceeds 1 M entries
(`docs/audits/pathbuf-arc-path-rss-overhead.md:212-238` forecast at
1 M is 90-130 MB savings at the upper bound, and 1 M flists are real
when migrating photo libraries or kernel forks), spill the entries
and path arena to a file-backed mmap and let the kernel page in only
the working set.

Storage layout, packed little-endian:

```
+--------+------------------------------+
| header | 32 B fixed (magic, lengths)  |
+--------+------------------------------+
| entries[0..N]   N * 32 B FileEntryRef |
+---------------------------------------+
| paths arena     variable length       |
+---------------------------------------+
| dir_index       D * 12 B              |
+---------------------------------------+
| hardlink_index  H * 12 B (dev,ino,ndx)|
+---------------------------------------+
```

The repository path is `target/.flist-cache/<hash>.flist` keyed by
the source-tree hash so repeated runs against the same tree skip
construction. Mmap-backed repository state is read-only; mutations
require copy-on-write into a `VecBacked` or `ArenaBacked` instance
via `into_mut() -> Box<dyn FlistRepositoryMut>` which copies once.

`MmapBacked` is opt-in via `--flist-cache` CLI flag. It is *not* the
default because:

1. It is a wire-incompatible storage detail and any mismatch in the
   on-disk format between oc-rsync versions silently corrupts the
   cache; explicit opt-in lets us add a header version field and a
   `--clear-flist-cache` escape hatch.
2. The mmap working set under random NDX access can thrash if the
   page cache is small relative to the entry area (32 MB at 1 M
   entries is not free on a small-RAM box).
3. Construction requires a sender-side build phase that writes the
   file before any wire frame; this delays the wire start-of-list by
   the file-write time, undesirable for interactive transfers.

The win is at 10 M entries and beyond, the regime where #971's RSS
scaling breaks down. For the 100 K-1 M range that #966 targets,
`ArenaBacked` is the right answer.

## 5. Migration cost

The blast radius is wide. `Vec<FileEntry>` is referenced 305 times
across the three crates. Most sites are mechanical (`file_list[i]` ->
`flist.get(i as u32).unwrap()` or
`flist.range(start as u32, end as u32)`), but the type juggle through
`Arc<Vec<FileEntry>>`, `&[FileEntry]`, and `Vec<FileEntry>` at API
boundaries means any flag-day rewrite has to coordinate across:

| Site count | Subsystem | Migration shape |
|-----------:|-----------|-----------------|
| 96 | `crates/transfer/src` | Most invasive. Both `GeneratorContext::file_list` (`generator/mod.rs:371`) and `ReceiverContext::file_list` (`receiver/mod.rs:106`) are owned `Vec<FileEntry>` fields touched by 30+ methods each. |
| 47 | `crates/protocol/src/flist` | `IncrementalFileList::pending`, `FileListSegment::entries`, `flist_clean` signature change; sort and hardlink tests use `Vec` constructors. |
| 22 | `crates/transfer/src/pipeline` | `FileList::new(Vec<FileEntry>)`, `shared() -> Arc<Vec<FileEntry>>`, `entries() -> &[FileEntry]` all change shape. |
| 13 | `crates/engine/src` | Only `local_copy` summary counters and `debug_flist` tracing; no direct flist data structures. Lowest risk. |
| ~127 | `crates/transfer/src/{generator,receiver}/tests.rs` | Test fixtures construct `Vec<FileEntry>` directly. Mechanical translation but a lot of files. |

Hot-path concerns:

- `FileList::get` at `crates/transfer/src/pipeline/job.rs:54-57` is
  called once per wire frame. Trait dispatch through
  `Arc<dyn FlistRepository>` adds a vtable indirection per call. At
  100 K files this is ~100 K virtual calls per transfer, ~3 ns each
  on aarch64, totalling 300 us - negligible.
- `range` and `iter_dir` return iterators that are used in tight
  loops over directory contents. `Box<dyn Iterator>` allocates per
  call; the `range` slice variant avoids that and is the path the
  generator's `encode_and_send_segment` must use.
- `find_hardlinks_for` is called once per follower in the sender's
  `assign_hardlink_indices`, which is already O(N) so the dynamic
  dispatch does not move the asymptote.

Recommended ladder, four PRs gated by interop (existing
`tools/ci/run_interop.sh`) and the memory baseline in
`docs/benchmarks/flist-memory-baseline-2026-05-01.md`:

1. **PR A: trait + VecBacked, no callers migrated.** Land the trait
   in `crates/protocol/src/flist/repository.rs`, the `VecBacked`
   impl, and the side-index construction in `freeze()`. No callers
   change. CI green = trait surface compiles and the new code is
   covered.
2. **PR B: pipeline + receiver migration.** Migrate
   `crates/transfer/src/pipeline/job.rs:38-82` `FileList` to
   `Arc<dyn FlistRepository>`. Migrate `ReceiverContext::file_list`
   (`crates/transfer/src/receiver/mod.rs:106`) and the receiver's
   `file_list.rs:42-220` ingest path to construct via
   `FlistRepositoryBuilder`. The 47 hits in
   `crates/transfer/src/receiver/` get rewritten. Interop CI must
   pass at all protocols (29, 30, 31, 32) before merge.
3. **PR C: generator migration.** Migrate
   `GeneratorContext::file_list` (`crates/transfer/src/generator/mod.rs:371`)
   and the 96 hits in `crates/transfer/src/generator/`. Includes the
   `partition_file_list_for_inc_recurse` reorder
   (`crates/transfer/src/generator/file_list/inc_recurse.rs:152-170`)
   which becomes a `reorder` call on the mut handle. Memory baseline
   gate must show no regression vs PR B.
4. **PR D: ArenaBacked opt-in.** Add the arena impl behind a
   `--flist-arena` feature gate; default stays `VecBacked`. Only the
   builder path changes; callers see the same trait. Run the #1864
   memory benchmark on both backings; if `ArenaBacked` clears the
   1.25x upstream target at 100 K and 1 M, swap the default in a
   subsequent PR.
5. **PR E (deferred): MmapBacked.** Behind `--flist-cache` CLI flag.
   Independent of the default; can land months later.

Each PR is reversible at the trait boundary: failing benchmarks revert
the migration of one subsystem without touching the others.

Why gradual rather than flag-day:

- The 305-hit blast radius means a single PR is unreviewable. The
  test surface alone (`crates/transfer/src/{generator,receiver}/tests.rs`,
  ~1.5 KLoC of fixtures) demands incremental conversion.
- The interop matrix (`tools/ci/run_interop.sh` against rsync 3.0.9,
  3.1.3, 3.4.1) takes ~25 min per run. A staged migration lets each
  PR get its own dedicated interop pass, isolating regressions to a
  single subsystem.
- The receiver (PR B) and generator (PR C) have asymmetric error
  handling (`crates/transfer/src/receiver/file_list.rs:346-409`
  truncates on error, generator does not). Converting them in a
  single PR mixes failure modes and obscures whether a regression is
  receiver-side or generator-side.
- `ArenaBacked` (PR D) requires the wire reader to feed the arena;
  this is a behaviour change in
  `crates/transfer/src/receiver/file_list.rs` that benefits from
  landing on top of the trait rather than alongside it.
- The generator's `mem::take(&mut self.file_list).into_iter()`
  reorder
  (`crates/transfer/src/generator/file_list/inc_recurse.rs:152-159`)
  consumes the entire backing storage; getting the trait API right
  for this single mutating operation is best done after PR B
  exercises the immutable path end-to-end.

## 6. API drift and the FileEntryView question

`ArenaBacked` and `MmapBacked` cannot return `&FileEntry` directly
because their on-disk layout differs from `FileEntry`. Two options:

A. **Heterogeneous return.** Repository returns `Cow<'_, FileEntry>`
   from `get` and `range` returns
   `Box<dyn Iterator<Item = Cow<'_, FileEntry>>>`. Arena/mmap impls
   construct a `FileEntry` on read; vec-backed returns
   `Cow::Borrowed`. Cost: per-call allocation in the arena/mmap
   path. Benefit: zero API churn at callers.

B. **Trait-object entry view.** Define
   `trait FileEntryView { fn name(&self) -> &str; fn mode(&self) -> u32; ... }`
   and have repository methods return `&dyn FileEntryView`.
   `FileEntry` impls the trait; `FileEntryRef` pairs with the arena
   to impl it. Cost: every call site that does
   `entry.name()` keeps working, but pattern matches on `FileEntry`
   internals (e.g. `entry.is_dir()` style accessors) need the trait
   surface to expose every accessor in
   `crates/protocol/src/flist/entry/accessors.rs`. Benefit: zero
   per-call allocation.

Recommendation: option B. The accessor surface is already small
(~25 methods) and additive; defining `FileEntryView` once and impl-ing
it on both `FileEntry` and `FileEntryRef` is mechanical. Callers
that take `&FileEntry` change to `&dyn FileEntryView` or stay on
`FileEntry` for the vec-backed case (which is the only impl that
returns it). Option A's per-call allocation is unacceptable in the
NDX-lookup hot path (Section 2.1).

This is the load-bearing API decision; PR A must commit to option B
before PR B starts so the hot-path callers do not get rewritten
twice.

## 7. Open questions

1. **Arena path-string lifetime.** `bytes::Bytes` is the natural
   home for the arena, but `FileEntry::name()` returns `&str` today.
   `ArenaBacked` either returns `Cow<'a, str>` (allocation per call)
   or borrows from `Bytes` via `&'a str` derived from
   `Bytes::as_ref()` (safe but ties the entry view lifetime to the
   repository). Option B's `FileEntryView` returning `&str` requires
   the latter; verify with a 100 K bench that the borrow checker
   cooperates with `Arc<dyn FlistRepository + 'static>` callers.
2. **Side index construction cost.** `dir_index` and
   `hardlink_groups` add per-build cost. The dir_index walk is O(N)
   over already-sorted entries (linear scan with parent change
   detection); the hardlink groups walk is O(N) into a HashMap. Both
   are paid during construction, not transfer; estimate `<= 3 %`
   wall regression at 100 K but verify against the
   `crates/protocol/benches/file_entry_memory.rs` bench before
   committing PR A.
3. **`pending` HashMap in `IncrementalFileList`.** The hashmap at
   `crates/protocol/src/flist/incremental/mod.rs:85` shadows the
   repository's `iter_dir` access pattern. It is the receiver-side
   construction-time staging area; PR B must decide whether the
   incremental list builds directly into the repository (saving
   allocator round-trips) or stages in the existing hashmap and
   bulk-flushes. Recommendation: stage in the hashmap, bulk-flush at
   `release_pending_children`; the hashmap is short-lived (one
   directory's worth of entries) and the staging is independent of
   the long-lived repository.
4. **Mmap concurrency.** `MmapBacked` is `Send + Sync` because the
   mmap is read-only post-construction. The `Arc<Mmap>` plus
   `&'static` slice trick requires unsafe, which under the project's
   policy (CLAUDE.md) must live in `fast_io` or another permitted
   crate. PR E will need to either move the unsafe to `fast_io` or
   defer mmap support entirely.
5. **Wire reader flow control.** `ArenaBacked` construction during
   wire ingest is allocator-friendly only if the receiver can size
   the arena up-front. Upstream rsync sends an io_error end marker
   but not a flist size hint
   (`target/interop/upstream-src/rsync-3.4.1/flist.c:2518`), so the
   first allocation is speculative. Benchmark whether
   `bytes::BytesMut::with_capacity(1 << 20)` plus `extend_from_slice`
   reallocs are cheaper than `Vec<String>` per-entry today; the
   audit's #1049 work suggests yes but the experiment has not been
   run end-to-end.

## References

- `crates/transfer/src/pipeline/job.rs:38-82` - existing `FileList`
  wrapper, the natural seat for `Arc<dyn FlistRepository>`.
- `crates/transfer/src/receiver/mod.rs:106` - receiver-owned
  `Vec<FileEntry>` field, PR B target.
- `crates/transfer/src/receiver/file_list.rs:42-220,346-409,485-665`
  - wire ingest, INC_RECURSE staging, error truncation; this is the
  receiver-side migration surface.
- `crates/transfer/src/generator/mod.rs:371` - generator-owned
  `Vec<FileEntry>` field, PR C target.
- `crates/transfer/src/generator/file_list/inc_recurse.rs:38-228` -
  reorder/partition path that becomes `FlistRepositoryMut::reorder`.
- `crates/transfer/src/generator/file_list/hardlinks.rs:34-71` -
  `assign_hardlink_indices`; becomes the `hardlink_groups` builder
  invocation in `freeze`.
- `crates/transfer/src/generator/protocol_io.rs:178-326` - by-NDX
  and by-range read sites for sender wire encoding.
- `crates/transfer/src/receiver/transfer/candidates.rs:46-200` -
  receiver candidate selection; mostly by-NDX with one `iter`.
- `crates/protocol/src/flist/entry/core.rs:32-83` - `FileEntry`
  layout (post-decomposition, `<= 96 B` inline).
- `crates/protocol/src/flist/entry/extras.rs` -
  `Box<FileEntryExtras>` rare-field container; `ArenaBacked`
  collapses these into `Vec<FileEntryExtras>`.
- `crates/protocol/src/flist/incremental/mod.rs:80-94,239-262` -
  `IncrementalFileList::pending`, `drain_ready`, `finish`; PR B
  decides whether the trait absorbs this staging.
- `crates/protocol/src/flist/segment.rs:21-160` - `FileListSegment`
  per-directory grouping; `range` accommodates this directly.
- `crates/protocol/src/flist/sort.rs:317-400` - `flist_clean`
  signature today returns `Vec<FileEntry>`; trait-aware variant
  returns `Box<dyn FlistRepositoryMut>`.
- `crates/protocol/src/flist/hardlink/table.rs` - existing
  `HardlinkTable`; folds into the repository's `hardlink_groups`
  side index.
- `docs/audits/pathbuf-arc-path-rss-overhead.md:74-254` - per-entry
  cost model; anchors the `ArenaBacked` 9-13 MB / 90-130 MB savings
  forecast.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md:50-68` -
  empirical Mode A/B baseline; `ArenaBacked` must clear the 1.25x
  upstream gate before becoming default.
- `docs/design/flist-memory-benchmark-plan.md` - companion plan
  (#1864) supplying the regression gate this design is measured
  against.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697-773,2192-2937`
  - upstream `add_dirs_to_tree`, `send_file_list`, ndx_start
  arithmetic; the trait's `iter_dir` and `range` mirror this layout.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-870,936-937`
  - `struct file_struct`, `union file_extras`, pool extent constants
  that `MmapBacked` mimics.
