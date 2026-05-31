# FlatFileList rayon compatibility audit

Task: RSS-A.11.a (#3233). Branch: `docs/rss-a11a-rayon-compat-audit`.
Prerequisites: RSS-A.5 (FlatFileList implementation), RSS-A.8 (INC_RECURSE segments).
Downstream: RSS-A.11.b (implement per-thread-then-merge builder pattern).

## Summary

This document audits the `FlatFileList` arena-backed file list
representation for compatibility with rayon parallel iteration and
building. The project uses rayon extensively for parallel stat
operations, signature computation, metadata application, and deletion
scanning. When the flat backing store replaces the legacy
`Vec<FileEntry>`, these parallel paths must continue to work correctly.

**Finding**: `FlatFileList` and its arenas are `Send + Sync` by
auto-derivation, so read-only parallel iteration works out of the box.
However, the builder API (`push`, `push_with_extras`, `paths_mut`,
`extras_mut`) requires `&mut self`, making concurrent pushes from
multiple rayon workers impossible without a structural change. A
per-thread-then-merge pattern is the recommended solution.

## Scope

1. All rayon `par_iter()` usage sites that touch file list entries.
2. `Send + Sync` status of `FlatFileList` and its sub-types.
3. Thread safety of `PathArena` and `ExtrasArena` under shared access.
4. Builder pattern concurrency constraints.
5. API gaps blocking parallel flist building with `FlatFileList`.

---

## 1. Rayon usage sites touching file list entries

### 1.1 Parallel stat / metadata fetching (read file list, write metadata)

These sites iterate over file list entries in parallel to perform I/O
operations. They read entry fields but do not mutate the file list.

| Location | Operation | Entry access |
|----------|-----------|-------------|
| `crates/flist/src/parallel.rs:83` | `process_entries_parallel` | `par_iter().map(f).collect()` over `&[FileListEntry]` |
| `crates/flist/src/parallel.rs:105` | `filter_entries_indices` | `par_iter().enumerate().filter_map()` over `&[FileListEntry]` |
| `crates/flist/src/parallel.rs:132` | `collect_paths_then_metadata_parallel` | `into_par_iter().map()` over path tuples, builds `FileListEntry` per worker |
| `crates/flist/src/parallel.rs:319` | `collect_paths_chunked_parallel` | `par_iter().map()` over path chunks, builds `FileListEntry` per worker |
| `crates/flist/src/parallel.rs:388` | `resolve_metadata_parallel` | `into_par_iter().map()` over `LazyFileListEntry` |
| `crates/flist/src/batched_stat/cache.rs:131` | `BatchedStatCache::stat_batch` | `par_iter().map()` over `&[&Path]` with sharded cache |
| `crates/transfer/src/parallel_io.rs:186` | `map_blocking` | Generic `into_par_iter().map(f).collect()` for stat/chmod/chown |
| `crates/transfer/src/generator/file_list/batch_stat.rs:43` | `batch_stat_dir_entries` | Uses `map_blocking` over `Vec<PathBuf>` |

### 1.2 Parallel signature computation (read file list + basis files)

| Location | Operation | Entry access |
|----------|-----------|-------------|
| `crates/transfer/src/receiver/transfer/pipeline.rs:191` | Generator-side basis lookup | `par_iter().map()` over `(ndx, &FileEntry, &Path)` tuples |
| `crates/engine/src/local_copy/executor/directory/parallel_checksum.rs:95` | Directory checksum | `par_iter()` over directory entries |

### 1.3 Parallel deletion scanning

| Location | Operation | Entry access |
|----------|-----------|-------------|
| `crates/engine/src/delete/parallel_consumer.rs:347` | Delete cohort dispatch | `par_iter()` over deletion plans |

### 1.4 Parallel directory metadata application

| Location | Operation | Entry access |
|----------|-----------|-------------|
| `crates/engine/src/local_copy/executor/directory/support.rs:106` | Metadata apply | `into_par_iter()` over entries |
| `crates/engine/src/local_copy/executor/directory/parallel_planner.rs:100` | Entry metadata prefetch | `par_iter().enumerate().map()` over `DirectoryEntry` |

### 1.5 Parallel delta apply

| Location | Operation | Entry access |
|----------|-----------|-------------|
| `crates/engine/src/concurrent_delta/parallel_apply/batch.rs:73` | Chunk verification | `into_par_iter()` over delta chunks |

### Classification summary

- **Read-only parallel access to file list entries**: sites 1.1, 1.2,
  1.3. These iterate `&[FileEntry]` or `&[FileListEntry]` in parallel.
  For `FlatFileList`, the equivalent is iterating `&[FileEntryHeader]`
  and resolving path handles through `&PathArena`. Requires shared
  (`&`) access only.

- **Per-worker entry construction**: `collect_paths_then_metadata_parallel`,
  `collect_paths_chunked_parallel`, `resolve_metadata_parallel`. Each
  rayon worker constructs a `FileListEntry` independently, then all
  results are collected into a single `Vec`. For `FlatFileList`, this
  pattern needs adaptation because the path interner requires `&mut`
  access.

- **No sites mutate the file list in parallel**: all rayon sites either
  read an existing file list or produce independent results that are
  collected sequentially.

---

## 2. Send + Sync analysis

### 2.1 FileEntryHeader

```
#[derive(Clone, Copy)]
pub struct FileEntryHeader {
    mtime: i64, size: u64, uid: u32, gid: u32,
    name: PathHandle(u32), dirname: PathHandle(u32),
    extras: ExtrasRef(u32), mtime_nsec: u32,
    mode: u32, flags: u16, present: u16,
}
```

All fields are `Copy` primitives. **Auto-derives `Send + Sync`.**

### 2.2 PathHandle and ExtrasRef

Both are `#[derive(Clone, Copy)] struct Foo(pub u32)`. Newtype wrappers
over `u32`. **Auto-derive `Send + Sync`.**

### 2.3 PathArena

```
pub struct PathArena {
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
    dedup: HashMap<Box<str>, PathHandle>,
}
```

All fields are `Send + Sync` (`Vec<u8>`, `Vec<(u32,u32)>`,
`HashMap<Box<str>, PathHandle>`). **Auto-derives `Send + Sync`.**

### 2.4 ExtrasArena

```
pub struct ExtrasArena {
    blobs: Vec<u8>,
}
```

Single `Vec<u8>` field. **Auto-derives `Send + Sync`.**

### 2.5 FlatFileList

```
pub struct FlatFileList {
    headers: Vec<FileEntryHeader>,
    paths: PathArena,
    extras: ExtrasArena,
    segments: Vec<Segment>,
}
```

All fields auto-derive `Send + Sync`. **`FlatFileList` auto-derives
`Send + Sync`.**

### 2.6 FlatFileEntry

```
pub struct FlatFileEntry<'a> {
    pub header: &'a FileEntryHeader,
    pub name: &'a [u8],
    pub dirname: &'a [u8],
    pub extras_arena: Option<&'a ExtrasArena>,
}
```

All fields are shared references to `Sync` types. **Auto-derives
`Send + Sync` (for `'a: Send`).**

### 2.7 Verdict

All flat flist types auto-derive `Send + Sync` through their constituent
types. No manual `unsafe impl Send` or `unsafe impl Sync` is needed.
Rayon's `par_iter()` over `&[FileEntryHeader]` and shared `&PathArena`
/ `&ExtrasArena` resolution will compile and run correctly.

---

## 3. PathArena and ExtrasArena thread-safety under shared access

### 3.1 Read path (shared `&self`)

`PathArena::resolve(&self, handle)` and `PathArena::get(&self, handle)`
perform:
1. Bounds-checked index into `self.spans: Vec<(u32, u32)>`.
2. Slice of `self.bytes: Vec<u8>` using the span's offset and length.
3. `std::str::from_utf8()` validation.

All operations are read-only on contiguous memory. Multiple rayon workers
can call `resolve()` concurrently on the same `&PathArena` with no data
races. The `dedup: HashMap` is not accessed on the read path.

`ExtrasArena::decode(&self, reference)` reads from `self.blobs: Vec<u8>`
through a forward-only `Cursor`. Read-only, no mutation. Safe for
concurrent access.

**Verdict**: shared read access is safe for both arenas.

### 3.2 Write path (exclusive `&mut self`)

`PathArena::intern(&mut self, s)` mutates all three fields: appends to
`bytes`, pushes to `spans`, and inserts into `dedup`. These operations
are not atomic and cannot be safely called from multiple threads.

`ExtrasArena::append(&mut self, extras)` extends `self.blobs` with
length-prefixed encoded data and returns an offset. Not thread-safe for
concurrent appends because `Vec::extend_from_slice` may reallocate and
a concurrent reader would see a dangling or partially-written buffer.

**Verdict**: write access requires exclusive `&mut` ownership, enforced
by Rust's borrow checker. No concurrent writes are possible through safe
Rust.

### 3.3 Build-then-freeze lifecycle

Both arenas follow a build-then-freeze pattern: append during file list
construction, then treat as immutable for the rest of the transfer. This
naturally aligns with rayon usage patterns where the file list is built
sequentially (from wire or filesystem traversal) and then iterated in
parallel for stat/signature/delete operations.

---

## 4. Builder pattern concurrency analysis

### 4.1 Current builder API

`FlatFileList` exposes these mutating methods:

- `push(&mut self, header)` - appends a header.
- `push_with_extras(&mut self, header, extras)` - encodes extras, sets
  the `ExtrasRef`, appends the header.
- `paths_mut(&mut self) -> &mut PathArena` - for interning name/dirname.
- `extras_mut(&mut self) -> &mut ExtrasArena` - for manual extras encode.
- `extend_segment(&mut self, headers)` - batch append + segment tracking.
- `sort(&mut self)` / `sort_segment(&mut self, index)` - in-place sort.

All require `&mut self`. Rust's ownership rules prevent concurrent
access from multiple rayon workers.

### 4.2 DualFileList::push

`DualFileList::push(&mut self, entry)` calls `file_entry_to_flat()` which
interns paths via `flist.paths_mut()` and then calls
`flist.push_with_extras()`. This is an inherently sequential operation
because:

1. `PathArena::intern()` checks the dedup `HashMap` and conditionally
   appends to the byte arena - a read-modify-write cycle.
2. `ExtrasArena::append()` writes length-prefixed data and returns a
   positional offset that must match the header's `ExtrasRef`.
3. `Vec::push` on the header array may trigger reallocation.

### 4.3 Can the builder support concurrent pushes?

**No**, not with the current design. The three stores (`headers`,
`paths`, `extras`) are tightly coupled:

- A header's `PathHandle` values must reference the same `PathArena`
  that the `FlatFileList` owns.
- A header's `ExtrasRef` must reference the same `ExtrasArena`.
- The dedup `HashMap` in `PathArena` is a mutable shared structure that
  cannot be safely accessed from multiple threads without locking.

Wrapping the arenas in `Mutex` or `RwLock` would serialize the critical
path and negate rayon's benefit. The interning operation (hash lookup +
conditional append) is too fine-grained for coarse locking to help.

### 4.4 Recommended pattern: per-thread-then-merge

The natural rayon-compatible pattern for building a `FlatFileList` in
parallel is:

1. **Parallel phase**: each rayon worker builds a thread-local
   `FlatFileList` (or a simpler `Vec<(FileEntryHeader, FlatExtras,
   String, String)>` buffer) from its slice of filesystem entries.
   Each worker has its own `PathArena` and `ExtrasArena` - no sharing,
   no contention.

2. **Sequential merge phase**: a single thread iterates the per-worker
   results and pushes them into the final `FlatFileList`, re-interning
   path strings through the shared `PathArena` (which deduplicates
   across workers) and re-encoding extras into the shared `ExtrasArena`.

This matches the existing codebase pattern in
`collect_paths_then_metadata_parallel()` (line 131 of
`crates/flist/src/parallel.rs`): rayon workers produce independent
`FileListEntry` values, then the results are collected into a single
`Vec` and sorted.

**Cost of the merge phase**: re-interning path strings is O(n) with
O(1) per-string hash lookups. For a 1M-entry file list with ~50K
unique directory names and ~800K unique basenames, the merge phase
performs ~850K hash lookups plus ~850K * avg_name_len bytes of arena
appends. This is dominated by the parallel stat I/O and negligible in
comparison.

### 4.5 Alternative: concurrent arena with atomic offsets

A lock-free append-only arena (bump allocator with `AtomicUsize` offset)
could support concurrent `ExtrasArena::append()`. Combined with a
`DashMap` for `PathArena`'s dedup table, this would enable fully
concurrent building. However:

- Adds `DashMap` dependency and atomic overhead to a hot path.
- Complicates the dedup logic (CAS loops for handle assignment).
- The current file list is built from wire data (sequential I/O) or
  filesystem traversal (sequential `readdir` + parallel stat). The
  sequential wire read is the bottleneck, not the arena append.
- Over-engineering for the actual workload profile.

**Not recommended** unless profiling shows the merge phase is a
bottleneck at scale (>1M entries).

---

## 5. API gaps blocking parallel flist building

### Gap 1: No per-thread builder type

There is no lightweight "partial builder" that can accumulate entries
without owning a full `PathArena` with dedup. A per-thread builder would
ideally skip dedup (each thread's entries are likely unique) and emit
raw `(name_bytes, dirname_bytes, header, extras)` tuples for the merge
phase to re-intern.

**Severity**: medium. Workaround: each thread creates a full
`FlatFileList` and the merge phase iterates it.

### Gap 2: No merge/extend API

`FlatFileList` has no `extend_from(&mut self, other: &FlatFileList)`
method that re-interns paths from another list's arena into `self`'s
arena. The caller must manually iterate, resolve, re-intern, and push.

**Severity**: medium. This is the most useful API addition for the
per-thread-then-merge pattern.

### Gap 3: FlatFileList::iter() does not return a rayon-compatible type

`FlatFileList::iter()` returns `impl Iterator<Item = FlatFileEntry<'_>>`,
which is a standard iterator, not a `ParallelIterator`. To use rayon's
`par_iter()`, callers must use the `headers` slice (via
`headers_slice()`) and resolve paths manually:

```rust
flist.headers_slice(0..flist.len())
    .par_iter()
    .map(|h| {
        let name = flist.paths().resolve(h.name);
        let dirname = flist.paths().resolve(h.dirname);
        // ...
    })
    .collect()
```

This works but is verbose. An `impl IntoParallelRefIterator` for
`FlatFileList` (yielding `FlatFileEntry`) would be ergonomic.

**Severity**: low. The verbose pattern works and is used elsewhere in
the codebase for `&[FileEntry]` parallel iteration.

### Gap 4: No `par_sort` / `par_sort_unstable_by`

`FlatFileList::sort()` and `sort_segment()` use
`sort_unstable_by()` on the header slice. For large file lists (>100K
entries), rayon's `par_sort_unstable_by()` would be faster. The sort
closure captures `&self.paths` which is `Sync`, so it is compatible
with rayon's parallel sort.

**Severity**: low. Easy to add; `[FileEntryHeader]` already supports
rayon's `ParallelSliceMut::par_sort_unstable_by()` since
`FileEntryHeader: Send`.

### Gap 5: DualFileList does not propagate flat-path sorting

`DualFileList::as_mut_vec()` exposes the legacy `Vec<FileEntry>` for
sorting, but when `flat-flist` is enabled, sorting the legacy Vec does
not sort the flat store. The flat and legacy stores get out of sync
after any sort operation. This is not a rayon-specific gap but affects
any code path that sorts the file list and then iterates the flat store.

**Severity**: high for migration correctness. Currently safe because
all read accessors delegate to the legacy Vec, but will block the
eventual cutover from legacy to flat as the primary representation.

---

## 6. Recommendations

### Immediate (RSS-A.11.b)

1. Add `FlatFileList::extend_from(&mut self, other: &FlatFileList)` that
   re-interns paths and re-encodes extras from another list. This is the
   key API for per-thread-then-merge parallel building.

2. Add a compile-time assertion that `FlatFileList: Send + Sync` to
   guard against future field additions that break the auto-derivation.

### Short-term (RSS-A.11.c)

3. Implement `IntoParallelRefIterator for &FlatFileList` yielding
   `FlatFileEntry<'_>`, enabling `flist.par_iter()` instead of the
   verbose headers-slice pattern.

4. Add `FlatFileList::par_sort()` and `par_sort_segment()` using
   rayon's `par_sort_unstable_by()` for large file lists.

### Medium-term (RSS-A.12)

5. Wire `DualFileList` sorting to keep flat and legacy stores in sync,
   or implement a sort-index permutation (`Vec<u32>`) that avoids
   moving headers entirely (as described in the flat-flist design doc).

6. Provide a `FlatFileListBuilder` type that accumulates entries without
   dedup, suitable for per-thread construction, and a `merge_into()`
   method that consumes the builder and re-interns into a target
   `FlatFileList`.

---

## 7. Conclusion

The `FlatFileList` representation is structurally compatible with rayon
parallel iteration. All types auto-derive `Send + Sync`, and the
build-then-freeze lifecycle aligns with the codebase's existing pattern
of sequential construction followed by parallel read access.

The primary gap is concurrent building: the builder API requires
exclusive `&mut self` access, which prevents multiple rayon workers from
pushing entries simultaneously. The recommended solution is a
per-thread-then-merge pattern with a new `extend_from()` API, matching
the existing `collect().sort()` pattern used for `Vec<FileListEntry>` in
the `flist` crate's parallel collection functions.

No blocking issues exist for the current `flat-flist` feature flag
usage, where `DualFileList::push()` builds both representations
sequentially from wire data. The gaps identified here become relevant
only when `FlatFileList` replaces `Vec<FileEntry>` as the primary
representation and the parallel flist builder is wired in.
