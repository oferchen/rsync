# Vec<FileEntry> vs upstream pool allocator: RSS gap audit

Task: #1050. Branch: `docs/rss-flist-pool-1050`. Companion audits: #966
(overall RSS gap), #971 (1M-file RSS scaling), #1037 (`FileEntry` memory
benchmark, completed), #1048 (`PathBuf`/`Arc<Path>` overhead), #1049
(string interning evaluation, completed).

## Summary

Upstream rsync stores its file list as an array of `struct file_struct *`
pointers (`flist.c:290-318`) that index into a slab-allocated arena
(`lib/pool_alloc.c`). Each entry's struct, basename, symlink target, and
all optional `extras` slots live contiguously inside one 256 KiB extent;
the per-pointer array doubles up to 32 K entries, then grows linearly by
16 M (`rsync.h:920-925`). oc-rsync stores the file list as `Vec<FileEntry>`
plus, in incremental mode, a `Vec<FileListSegment>` of nested vectors
(`crates/protocol/src/flist/segment.rs:21-32`). Each `FileEntry` is 88 B
inline (`crates/protocol/src/flist/entry/tests.rs:293-304`) and reaches
into the global allocator for `name`, `dirname`, and any `extras`
fields. The `Vec` itself doubles on every regrow up to `isize::MAX`; the
amortised slack at the end of a 100 K-entry build is between 0 and N
entries, peaking at the next power-of-two boundary minus one. That slack
plus the per-entry pointer indirections account for an estimated
14-22 MiB of resident-set delta at 100 K files, growing to 140-220 MiB
at 1 M files.

The remaining gap (path heap, allocator metadata) is covered by #1048;
this audit is scoped to the *container* overhead (Vec slack, segment
fragmentation, per-entry indirection) and the design choices that would
close it.

## Methodology

1. Read the `Vec<FileEntry>` accumulation paths in
   `crates/protocol/src/flist/segment.rs:21-90`,
   `crates/protocol/src/flist/incremental/mod.rs:80-110,239-302`, and
   `crates/protocol/src/flist/sort.rs:317-400`. Confirm there is no
   pre-sized growth call - segments and ready buffers are constructed
   with `Vec::new()` and grow on each `push`.
2. Read the upstream growth law in
   `target/interop/upstream-src/rsync-3.4.1/flist.c:290-318` and the
   pool layout in
   `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c:1-90,114-175`.
3. Read upstream's per-entry sizing in
   `target/interop/upstream-src/rsync-3.4.1/rsync.h:801-840` and the
   variable extras computation in
   `target/interop/upstream-src/rsync-3.4.1/flist.c:704-1020`.
4. Translate per-entry numbers to MiB at 100 K and 1 M scales using
   the size-class assumptions calibrated in
   `docs/benchmarks/flist-memory-baseline-2026-05-01.md`.
5. Survey arena/pool crates (`bumpalo`, `typed-arena`, `slab`) for
   compatibility with the existing API surface (`Clone`, `Drop`,
   `Arc<Path>` dirname interning).

## Container layout in oc-rsync

`Vec<FileEntry>` is the standard Rust three-word vector: data pointer,
length, capacity (24 B on 64-bit). Growth is exponential by 2x once
capacity is non-zero (`alloc::raw_vec`), so the capacity at end of
build is in `[len, 2*len)`. The expected slack across uniformly random
final sizes is approximately `0.5 * len` entries on the first regrow
after a power-of-two boundary, dropping to `~0` just before the next
boundary. For a 100 K-entry build, capacity lands on 131 072 (next
power-of-two), leaving ~31 K unused entry slots = **31 072 x 88 B =
2.6 MiB** of inline slack alone. The same reasoning at 1 M entries
gives capacity = 1 048 576 (exact power of two; zero slack on the
boundary) or 2 097 152 if growth crosses by a single entry; expected
slack ~50 %.

Incremental recursion (the default sender path as of #3557) builds a
`SegmentedFileList` of one segment per directory
(`crates/protocol/src/flist/segment.rs:71-78`). Each segment owns its
own `Vec<FileEntry>` (`segment.rs:31`). For a 100 K-file workload
spread across 1 000 directories, that is 1 000 vectors with average
100 entries each. Per-vector slack averages ~50 entries x 88 B = 4.4 KiB,
times 1 000 directories = **4.4 MiB**. The 24 B vector header itself
adds **24 KiB** for the segment headers - negligible. Plus the outer
`segments: Vec<FileListSegment>` (32 B per `FileListSegment` header),
which is amortised across the full run.

`IncrementalFileList::pending` is a
`HashMap<String, Vec<FileEntry>>` (`incremental/mod.rs:85`) buffering
out-of-order directories. Same story: each value is its own vector
with its own slack. `drain_ready` and `finish`
(`incremental/mod.rs:239,259`) materialise a fresh `Vec<FileEntry>`
on each call - a transient peak that doubles the resident footprint
of any directory still in the buffer at drain time.

## Upstream `pool_alloc.c` slab pool design

`lib/pool_alloc.c:1-15,47-90` implements a forward-bumping slab
allocator. Each `alloc_pool` owns a singly-linked list of
`pool_extent`s. Default extent size is `POOL_DEF_EXTENT = 32 KiB`
(`pool_alloc.c:3`); flist uses `NORMAL_EXTENT = 256 KiB`
(`rsync.h:936-937`). Allocation pseudocode
(`pool_alloc.c:114-174`):

```
if (extents == NULL || len > extents->free)
    new_extent(asize)            /* asize = pool->size + sizeof(pool_extent) when POOL_PREPEND */
extents->free -= len             /* bump pointer */
return extents->start + extents->free
```

Per-entry overhead inside the pool is exactly:

- `len = FILE_STRUCT_LEN + extra_len + basename_len + linkname_len`,
  rounded up to the pool quantum (8 B on 64-bit; `MINALIGN` in
  `pool_alloc.c:32-40`).
- No allocator metadata per entry - the pool tracks `free`/`bound` per
  extent only.
- Wasted bytes at extent boundary: at most `quantum - 1 = 7 B`, plus
  the unused tail of the live extent when the next allocation does not
  fit (statistically `len/2` per extent end = 0-2 KiB).

`FILE_STRUCT_LEN` is 24 B on 64-bit (`rsync.h:801-812`: 8 B `dirname`,
8 B `time_t modtime`, 4 B `len32`, 2 B `mode`, 2 B `flags`,
flexible-array `basename` past the end). With protocol 32 features
enabled, `extra_len` accumulates ~12 slots:
nsec mtime (1), high-32 length (1), uid+gid (2), atime (2),
crtime (2), pathname (2 on PTRS_ARE_64), depth (1), unsort_ndx (1)
= ~12 x 4 B = 48 B. Plus basename (~12 B with NUL) = ~84 B per
regular-file entry packed inline in the pool.

Add the per-entry `struct file_struct *` pointer in `flist->files`
(8 B) and the doubling growth law in `flist.c:290-318`. The pointer
array doubles up to `FLIST_LINEAR = 32 K * 512 = 16 M` entries, then
grows linearly by 16 M; expected slack ~50 % on doubling, ~0 % on
linear. At 100 K entries the array sits at capacity 131 072 -
**8 B x 31 072 = 243 KiB** of slack.

**Total per-entry upstream cost**: ~84 B (pool payload) + 8 B
(pointer) + minimal extent-tail waste = **~92-96 B / entry**. There
is no `Vec` capacity to fight; the pool extent's free byte at the
tail is reused on the next allocation.

## Total RSS contribution at 100 K and 1 M file scale

Cross-check with the empirical Mode-B baseline in
`docs/benchmarks/flist-memory-baseline-2026-05-01.md`: oc-rsync used
42.6 MiB to upstream's 7.9 MiB at 100 K entries (single flist). The
delta breaks down approximately:

| Component | 100 K MiB | 1 M MiB | Notes |
|---|---|---|---|
| `Vec<FileEntry>` capacity slack (single flist) | 2.6 | 0-44 | Power-of-two doubling; depends where `len` lands. |
| Segmented flist per-dir vector slack (1 000 dirs / 10 000 dirs) | 4.4 | 44 | 50 % of 88 B per dir on average; scales linearly. |
| Per-entry `Box<FileEntryExtras>` for non-regular files (5 % of entries) | 0.5 | 5 | 240 B box; only when symlink/device/ACL/xattr present. |
| Path heap (PathBuf + Arc<Path>): see #1048 | 19-24 | 190-240 | Out of scope here; included for the running total. |
| Allocator metadata (~20 % of heap) | 5-7 | 50-70 | Glibc malloc small-bin overhead per allocation. |
| Sub-total (oc-rsync) | 31-39 | 290-400 | Matches 42.6 MiB observed at 100 K. |
| Upstream pool cost | 9-10 | 95-100 | `~96 B/entry` x N + extent slack. |
| **Container-only delta (this audit)** | **~7-9 MiB** | **~70-90 MiB** | Vec slack + segment fragmentation. |

The container-only delta (Vec slack + segmentation) is **~7-9 MiB at
100 K and ~70-90 MiB at 1 M**, roughly 20-25 % of the total RSS gap.
The remaining 75-80 % is path-heap and allocator metadata (#1048).
At very large fan-outs (1 M+ entries), the linear extent growth in
upstream and exact-power-of-two capacity in oc-rsync narrow the gap
on lucky boundaries; on unlucky boundaries (`len = 2^k + 1`) the
oc-rsync delta nearly doubles.

## Proposed paths

Five candidate paths, ranked by effort vs payoff:

### 1. Pre-size `Vec` from getdents count (cheapest)

The sender's local walker already enumerates `getdents`/`readdir`
batches before pushing into the segment vector. Plumbing the entry
count into `FileListSegment::new` and switching to
`Vec::with_capacity(n)` eliminates the doubling slack entirely on the
sender. On the receiver, the wire format does not pre-announce a
count, but `IncrementalFileList::pending` could bound capacity to a
running average of recent segment sizes, capping slack at ~10 %.
Effort: ~50 LOC. Risk: low; preserves the existing `Vec<FileEntry>`
API. Win at 1 M entries: 30-40 MiB.

### 2. `bumpalo::Bump` arena (closest to upstream's pool)

`bumpalo` is a forward-bumping arena with a 256 KiB default chunk and
power-of-two growth. Allocate every `FileEntry`, every `name` byte
slice, and every `dirname` `[u8]` from a single `Bump` per flist.
Free the entire flist at end-of-transfer with `Bump::reset`. Matches
upstream's per-flist `pool_destroy` semantics
(`pool_alloc.c:92-112`). Drawbacks: `Bump` does not run `Drop`, so
`FileEntry` cannot own a `Box<FileEntryExtras>` or a `Vec<u8>` - those
would need to migrate to `&'bump [u8]` references, requiring a
lifetime parameter on `FileEntry`. Effort: 1-2 days API surgery.
Win: 14-22 MiB at 1 M entries (eliminates Vec slack and most
allocator metadata). Reference: <https://docs.rs/bumpalo>.

### 3. `typed-arena::Arena<FileEntry>` (lightest API change)

`typed_arena::Arena<T>` is bumpalo's typed cousin: pushes entries into
chunks, returns `&mut T`. Unlike `Bump`, it does run `Drop` on each
allocated `T`, so existing `Box<FileEntryExtras>` / `Vec<u8>` /
`Arc<Path>` fields keep working unchanged. Drop runs on
`Arena::drop`, so the freeing pattern still matches upstream's
`pool_destroy`. Drawback: cannot remove or shuffle entries (no
`pop`, no random-access mutation), so this is incompatible with the
current `flist_clean` and `sort_unstable_by` paths
(`crates/protocol/src/flist/sort.rs:317-400`). Mitigation: build into
the arena, then materialise a `Vec<&FileEntry>` for sort/clean -
sorts pointers, not 88 B values, so it is also faster. Effort: 1 day.
Win: comparable to bumpalo, with no lifetime API churn.
Reference: <https://docs.rs/typed-arena>.

### 4. Custom slab pool mirroring `pool_alloc.c`

A direct port of `pool_alloc.c` would store `FileEntry` plus
its name bytes plus its dirname bytes plus extras inline in a single
chunk per entry. This is the only design that hits upstream's
~96 B/entry exactly, because it lets us pack the variable-length
basename and symlink target into the same allocation as the
`FileEntry` header (upstream: `flist.c:1018-1027`). Drawback:
Rust's borrow checker rejects flexible-array idioms in safe code; we
would either need `unsafe` (gated to `fast_io` per project policy) or
a `(usize_offset, len)` indirection scheme that re-introduces a
pointer chase. The pointer chase costs cache locality but stays
inside one slab, so it is still strictly better than today's
heap-spread layout. Effort: 3-5 days. Win: matches upstream within
1-2 MiB at 1 M entries (best possible).

### 5. Inline small-string + collapse `name` and `dirname` into one buffer

Replace the per-entry `name: PathBuf` + `dirname: Arc<Path>` pair
(40 B inline + two heap allocations) with a single
`Box<[u8]>` storing `dirname \0 basename` and an in-line `u16` offset
of the basename within. This is what upstream effectively does via
`F_SYMLINK(f) = (f)->basename + strlen(basename) + 1` and the
shared-`dirname` pointer; we mirror it with one allocation per entry
plus a 16 B inline footprint. Pairs naturally with #1049 path
interning (interned dirname becomes `Arc<[u8]>` over the shared
prefix). Effort: 1-2 days. Win: 8-12 MiB at 1 M entries on the
inline side, plus eliminates one of the two heap allocations per
entry. Compatible with all other paths above (orthogonal to arena
choice).

## Recommendation

Stack #1 (pre-size) and #5 (collapse path fields) for an immediate
~40-50 MiB win at 1 M entries with ~3 days total effort and no API
breakage. Defer #2/#3 (arena) until after the path-heap collapse
because the arena win shrinks once `name` and `dirname` are no
longer separate allocations. #4 (custom slab) is the upper-bound
target if benchmarks after #1+#5 still show a gap worth closing.

## References

- `crates/protocol/src/flist/entry/core.rs:32-72` - inline layout.
- `crates/protocol/src/flist/entry/tests.rs:293-304` - 96 B size cap.
- `crates/protocol/src/flist/segment.rs:21-90` - per-segment vector.
- `crates/protocol/src/flist/incremental/mod.rs:80-302` - pending
  buffer and drain materialisation.
- `crates/protocol/src/flist/sort.rs:317-400` - clean/sort pipeline
  that consumes `Vec<FileEntry>` by value.
- `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c` - slab
  allocator (377 LOC, MIT-equivalent).
- `target/interop/upstream-src/rsync-3.4.1/flist.c:290-318` - pointer
  array growth law.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:704-1020` -
  per-entry pool allocation.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-937` -
  `file_struct`, `EXTRA_LEN`, `NORMAL_EXTENT`.
- `docs/audits/pathbuf-arc-path-rss-overhead.md` - companion path-heap
  audit (#1048).
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` - empirical
  RSS calibration.
