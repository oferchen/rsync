# Thread-safe FlatFileList builder for rayon parallel stat

Task: RSS-A.11.b. Prerequisites: RSS-A.11.a (rayon compatibility audit).
Downstream: RSS-A.11.c (parallel iterator, parallel sort).

## Problem

`FlatFileList::push()` takes `&mut self`, incompatible with rayon
`par_iter()`. The parallel flist builder uses rayon workers to call
`stat()` on directory entries, then must push `FileEntryHeader` values
into a shared list. All three mutating operations (`PathArena::intern`,
`ExtrasArena::append`, `Vec::push`) require exclusive `&mut` access.

The existing `Vec<FileListEntry>` parallel builders sidestep this
because `FileListEntry` is self-contained (owns its `PathBuf`).
`FileEntryHeader` carries `PathHandle` and `ExtrasRef` values meaningful
only relative to its owning `FlatFileList`'s arenas.

## Approaches evaluated

### (a) Per-thread local FlatFileLists + merge

Each rayon worker builds a thread-local `FlatFileList` with its own
arenas. After the parallel phase, a single thread merges all per-worker
lists into one final `FlatFileList`, re-interning paths (which
deduplicates dirnames across workers) and re-encoding extras.

**Pros**: zero contention during parallel phase. Simple - follows the
existing `par_iter().map().collect()` pattern. No new dependencies.

**Cons**: temporary memory from per-worker arenas (freed after merge).

### (b) Mutex\<FlatFileList\>

Wrap the shared `FlatFileList` in a `Mutex`, lock on every push.

**Pros**: trivial to implement, no merge step.

**Cons**: serializes arena mutations behind one lock. The
`parallel_stat_collector_contention` benchmark shows single-mutex
collectors collapsing past 4-8 workers with >20% throughput regression.
Negates the parallelism that stat benefits from.

### (c) Lock-free concurrent arena with atomic index

Replace `PathArena` internals with `AtomicUsize` bump pointer and
`DashMap` for dedup. Use atomics for the header array length.

**Pros**: highest theoretical throughput.

**Cons**: adds `DashMap` dependency. CAS retry loops for handle
assignment are complex. Two workers may race to intern the same
dirname, requiring atomic fences for visibility. Not justified - the
parallel phase is I/O-bound on stat, not CPU-bound on arena appends.

## Recommendation

**Approach (a): per-thread local FlatFileLists + merge.** Best balance
of simplicity, correctness, and performance. Zero contention during the
I/O-bound stat phase. O(n) merge cost dominated by the parallel I/O.
No new dependencies, no unsafe code.

## Merge algorithm

### Per-worker build phase (parallel)

Each rayon worker receives a slice of paths, builds a local
`FlatFileList`: intern basename/dirname into local `PathArena`, encode
extras into local `ExtrasArena`, push `FileEntryHeader`. Workers do not
sort - sorting happens once on the merged list.

### Merge phase (sequential)

Given `k` per-worker lists:

1. Create the final `FlatFileList` with `with_capacity(sum of lengths)`.
2. For each worker list, iterate headers:
   a. Resolve `name`/`dirname` handles through the worker's `PathArena`.
   b. Re-intern both strings into the final `PathArena` (dedup HashMap
      ensures dirname sharing - same dir from 3 workers yields one copy).
   c. If extras present, decode from worker's `ExtrasArena`, re-encode
      into final `ExtrasArena`.
   d. Update header handles, push into final list.
   e. Drop the worker list immediately (incremental merge).
3. Sort via `FlatFileList::sort()` (dirname-then-name, upstream
   `f_name_cmp()`).

No k-way merge sort needed - per-worker lists are unsorted (rayon's
work-stealing gives arbitrary slices). A single `sort_unstable_by` on
the merged array is simpler and has better cache locality.

## PathArena thread-safety

Each worker gets its own `PathArena` - no sharing, no synchronization.
The merge thread is the sole writer to the final `PathArena`. After
merge, per-worker arenas are dropped. The final `PathArena` is immutable
for the rest of the transfer (build-then-freeze lifecycle per
RSS-A.11.a, section 3.3). Later parallel phases access it through shared
`&PathArena` references - safe because `PathArena: Sync`.

## Public API sketch

```rust
/// Builder for constructing a FlatFileList from parallel rayon workers.
pub struct ParallelFlatFileListBuilder {
    workers: Vec<FlatFileList>,
}

impl ParallelFlatFileListBuilder {
    /// Creates a builder for `num_workers` threads, each pre-allocated
    /// for `entries_per_worker` entries.
    pub fn with_capacity(num_workers: usize, entries_per_worker: usize) -> Self;

    /// Returns the worker-local list at `index` (use
    /// `rayon::current_thread_index()`).
    pub fn worker_list_mut(&mut self, index: usize) -> &mut FlatFileList;

    /// Merges all per-worker lists into one sorted FlatFileList.
    pub fn merge(self) -> FlatFileList;
}
```

Usage with rayon:

```rust
let paths: Vec<(PathBuf, Metadata)> = enumerate_dir(root);
let n = rayon::current_num_threads();
let per = paths.len().div_ceil(n);
let mut builder = ParallelFlatFileListBuilder::with_capacity(n, per);

paths.par_chunks(per).enumerate().for_each(|(i, chunk)| {
    let list = builder.worker_list_mut(i);
    for (path, md) in chunk {
        let dirname = list.paths_mut().intern(dirname_of(path));
        let name = list.paths_mut().intern(basename_of(path));
        let header = FileEntryHeader { name, dirname, .. };
        list.push_with_extras(header, &extras);
    }
});
let flist = builder.merge();
```

The implementation may use `rayon::scope` with thread-local storage or
`UnsafeCell` behind rayon's fork-join guarantees for borrow-checker
ergonomics. The API surface remains the same.

## RSS overhead of the merge step

Estimated at 1M entries, 8 workers (125K entries each):

| Component | Per-worker | All 8 workers |
|-----------|-----------|---------------|
| Headers (48 B each) | 6.0 MB | 48 MB |
| PathArena bytes (~30 B avg) | 3.75 MB | 30 MB |
| PathArena spans (8 B each) | 1.0 MB | 8 MB |
| PathArena dedup HashMap | 8.0 MB | 64 MB |
| **Subtotal** | **~19 MB** | **~150 MB** |

Final merged list: ~97 MB (48 headers + 30 path bytes + 8 spans + 11
dedup). Legacy `Vec<FileEntry>` at 1M: ~197 MB (RSS-A.2). Steady-state
is a ~50% reduction.

With incremental merge (drop each worker before processing the next),
the high-water mark is `(1 worker ~19 MB) + (final ~97 MB) = ~116 MB`
instead of `150 + 97 = 247 MB`:

```rust
fn merge(self) -> FlatFileList {
    let total = self.workers.iter().map(|w| w.len()).sum();
    let mut merged = FlatFileList::with_capacity(total);
    for worker in self.workers {
        merged.extend_from(&worker);
        drop(worker);
    }
    merged.sort();
    merged
}
```

The dedup HashMap can be dropped post-build via `PathArena::freeze()`,
recovering ~68 MB and bringing steady-state to ~29 MB (85% reduction
versus legacy).

## Risks and mitigations

**Worker count mismatch**: if the thread pool is reconfigured between
builder creation and execution, the worker index may exceed the vector
length. Mitigation: `worker_list_mut()` grows the vector on demand.

**Extras re-encoding cost**: entries with large extras tails (symlink
targets, checksums) pay a memcpy during merge. Mitigation: <5% of
entries carry extras in typical workloads (RSS-A.2 audit). The
re-encode is negligible versus stat I/O.
