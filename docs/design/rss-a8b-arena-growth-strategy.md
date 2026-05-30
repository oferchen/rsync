# RSS-A.8.b: arena growth strategy for mid-transfer segment appends

Task: RSS-A.8.b. Branch: `docs/rss-a8b-arena-growth-strategy`.
Prerequisites: RSS-A.8.a (INC_RECURSE segment append audit),
RSS-A.5.d (FlatFileList skeleton), RSS-A.5.e (ExtrasArena).
Downstream: RSS-A.8.c (FlatFileList segment growth implementation).

## Summary

This document specifies the growth strategy for the three arena-backed
stores (`Vec<FileEntryHeader>`, `PathArena`, `ExtrasArena`) when
INC_RECURSE segment appends arrive mid-transfer. The goal is to support
incremental segment growth without invalidating existing handles,
matching the single-contiguous-array-plus-segment-table model that both
sender and receiver already use (RSS-A.8.a findings F1, F4).

## Design decision: unified store with index-based segments

RSS-A.8.a audited the actual sender and receiver implementations and
found that both use a single contiguous `Vec<FileEntry>` with
`ndx_segments: Vec<(usize, i32)>` for segment boundaries (finding F1).
The `SegmentedFileList` type (per-segment `Vec<FileEntry>` storage) is
defined but unused in the transfer pipeline (finding F6).

The `flat-flist-representation.md` design proposed per-segment
`FlatFileList` instances for INC_RECURSE, motivated by upstream's
per-flist `pool_alloc` lifetime. However, the audit shows the actual
oc-rsync pipeline uses a unified flat array, not per-segment containers.
Migrating to per-segment `FlatFileList` instances would require
restructuring the entire sender and receiver pipeline - changing NDX
conversion, delete pipeline publication, entry access, and sort
operations.

This design follows the actual implementation: **one `FlatFileList` per
role (sender or receiver), with segment boundaries tracked by the
existing `ndx_segments: Vec<(usize, i32)>` table.** New segments append
entries to the tail of the same `headers` Vec, `PathArena`, and
`ExtrasArena`. This matches the current `Vec<FileEntry>` behavior
exactly and requires no pipeline restructuring.

The per-segment `FlatFileList` approach from the parent design remains
available as a future optimization for memory reclamation (dropping
consumed segments), but it is not required for correctness and is
deferred.

## Handle stability under growth

RSS-A.8.a finding F4 established that arena handles survive Vec
reallocation because they are index-based, not pointer-based:

- **`PathHandle`**: a `u32` index into `PathArena.spans`, which is a
  `Vec<(u32, u32)>`. Appending new strings may reallocate the underlying
  `bytes: Vec<u8>` and `spans: Vec<(u32, u32)>`, but existing handles
  remain valid - they index into the span table, and spans reference
  byte offsets that do not change when the arena grows (append-only).
- **`ExtrasRef`**: a `u32` byte offset into `ExtrasArena.blobs`. New
  tails are appended; existing offsets remain valid because the arena
  never mutates written bytes.
- **`FileEntryHeader` indices**: flat indices into `headers: Vec<FileEntryHeader>`.
  Vec reallocation preserves index-based access.

No handle invalidation occurs during segment growth. The invariant is
structural: all three stores are append-only, and all references are
index/offset-based. This is tested by the handle-stability test
specified in the verification section below.

## Growth policies

### 1. `Vec<FileEntryHeader>` (header array)

**Policy: Rust `Vec` default doubling with optional pre-reservation.**

The header array uses standard `Vec::push` semantics. `Vec` doubles its
capacity when it runs out of space, giving amortized O(1) append. This
matches the current `Vec<FileEntry>` behavior.

Pre-reservation is available when the segment's entry count is known
ahead of time. The receiver learns the count from the wire (the segment
end marker), so it cannot pre-reserve. The sender knows the count from
the partition step and can call `reserve(count)` before pushing. In
practice, the doubling strategy is sufficient for both cases and the
capacity slack is bounded (at most 50% waste on the header array, which
at 48 bytes per entry is 24 MB for 1M entries - acceptable).

Growth bound: at most `2 * N * 48` bytes for N entries, where the
factor of 2 accounts for Vec capacity slack.

### 2. `PathArena` (interned name/dirname strings)

**Policy: standard Vec doubling on both `bytes` and `spans`, with
deduplication absorbing growth.**

`PathArena` holds three allocations:
- `bytes: Vec<u8>` - the string arena, grows by `extend_from_slice`.
- `spans: Vec<(u32, u32)>` - per-handle span table, grows by `push`.
- `dedup: HashMap<Box<str>, PathHandle>` - dedup map, grows by `insert`.

New segments add entries whose names and dirnames are interned through
the same `PathArena`. Because the interner deduplicates, identical
strings across segments share one arena copy and one handle. In
INC_RECURSE transfers, the dirname sharing is particularly effective:
each sub-list corresponds to one directory, and all entries in that
sub-list share the same dirname handle.

The `bytes` arena grows only for genuinely new strings. The `spans`
table grows by one entry per new unique string. The `dedup` HashMap
follows its standard rehash-at-load-factor growth. None of these
invalidate existing handles.

Growth bound: `bytes` is bounded by the total unique string bytes across
all segments. For a transfer with D unique directories of average name
length L_d, and F unique basenames of average length L_b, the arena
holds `D * L_d + F * L_b` bytes. At 1M files with 10K directories,
average dirname 30 bytes, average basename 15 bytes:
`10K * 30 + 1M * 15 = 15.3 MB`. This is much smaller than the
per-entry `PathBuf` allocations it replaces (~46 MB at 1M entries per
the RSS-A.2 audit).

### 3. `ExtrasArena` (packed extras tails)

**Policy: standard Vec doubling on `blobs: Vec<u8>`.**

Each entry with non-empty extras (symlink targets, device numbers,
checksums, ACL/xattr indices, user/group names, atime/crtime) appends a
self-describing tail to the `blobs` arena. Entries without extras
(the common case for regular files) get `ExtrasRef::NO_EXTRAS` and
consume zero arena bytes.

The arena is append-only and references are byte offsets. Growth does
not invalidate existing offsets. There is no deduplication (extras tails
are per-entry-unique), so the arena grows linearly with the number of
entries that have extras.

Growth bound: for a transfer of N entries where fraction P have extras
with average tail size T bytes, the arena holds `N * P * T` bytes. In
the common case (regular files, uid+gid preserved but stored inline,
no symlinks/devices/ACLs), P is near zero and the arena stays empty.
Worst case (every entry has a symlink target or checksum): T is roughly
25 bytes (2-byte mask + 2-byte len + ~20-byte path), giving `N * 25`
bytes.

## Segment boundary tracking

The `ndx_segments: Vec<(usize, i32)>` table tracks segment boundaries
in the same way for both `Vec<FileEntry>` and `FlatFileList`. Each entry
is `(flat_start, ndx_start)`:

- `flat_start` is the index in the header array where the segment
  begins.
- `ndx_start` is the wire NDX value for the first entry in the segment.

For the initial segment, `flat_start = 0` and `ndx_start = 1` (with
INC_RECURSE). For subsequent segments:

```
seg_ndx_start = prev_ndx_start + prev_used + 1   // +1 gap sentinel
flat_start = headers.len()                        // append position
```

This formula matches upstream `flist.c:2931` and the current oc-rsync
implementation (RSS-A.8.a audit, line 94). The NDX segment table is
independent of the backing store format - it works identically whether
the store is `Vec<FileEntry>` or `Vec<FileEntryHeader>`.

## Segment lifecycle

### Receiver (append-and-sort)

1. Record `flat_start = flist.len()`.
2. Compute `seg_ndx_start` from `ndx_segments.last()`.
3. For each entry on the wire:
   a. Intern name/dirname into `PathArena` via `flist.paths_mut().intern()`.
   b. Encode extras into `ExtrasArena` via `extras_arena.append()`.
   c. Push the `FileEntryHeader` to `flist` via `flist.push()`.
4. Sort the segment range `headers[flat_start..]` via `sort_range()`.
5. Match hardlinks within the segment range.
6. Push `(flat_start, seg_ndx_start)` to `ndx_segments`.
7. Publish segment to the delete pipeline.

Step 4 requires a `sort_range(range)` method on `FlatFileList` (action
item 1 from RSS-A.8.a). Sorting reorders headers within the range but
does not move any headers outside it, so handles from prior segments
remain valid. The sort resolves path handles through the shared
`PathArena`, which is safe because all handles reference the same arena.

### Sender (partition-then-dispatch)

1. Build the full file list into `FlatFileList` (all entries).
2. Sort the full list.
3. Partition into segments by reordering the header array in-place:
   initial entries at `[0..N)`, sub-directory entries at `[N..)`.
4. Record segment boundaries via `PendingSegment` structs referencing
   `(flist_start, count)` ranges.
5. Dispatch segments lazily during the transfer loop.

Step 3 requires a `reorder_by_permutation(perm: &[usize])` or
equivalent mutation on the header array (action item 4 from RSS-A.8.a).
This reorders headers but does not change path or extras handles - the
handles are embedded in the headers and move with them.

## DualFileList synchronization

During the migration, `DualFileList` maintains both `Vec<FileEntry>` and
`FlatFileList`. For INC_RECURSE segment growth:

- **Push**: `DualFileList::push(entry)` already pushes to both stores.
  Segment growth uses the same `push` path. No change needed.
- **Sort**: The receiver sorts each segment via
  `sort_file_list(&mut self.file_list[flat_start..], ...)` on the legacy
  Vec. The flat store must be sorted in sync. **Strategy: sort both
  stores independently** (RSS-A.8.a finding F5, option 1). The legacy
  Vec sorts via `compare_file_entries()`, the flat store sorts via
  `sort_range()` with its own comparator that resolves path handles.
  Both comparators produce the same order (unsigned byte comparison on
  dirname then name). A parity test verifies identical ordering.
- **Segment boundaries**: `DualFileList::segment_start()` already
  returns `legacy.len()`, which equals `flat.len()` because push
  maintains both in lockstep.

## Required FlatFileList additions

These are the API additions identified in RSS-A.8.a's action items,
needed for segment growth support:

### 1. `sort_range(range: Range<usize>)`

Sorts a sub-range of the header array by dirname-then-name, resolving
path handles through the shared `PathArena`. Used by the receiver to
sort each INC_RECURSE segment independently.

```rust
impl FlatFileList {
    pub fn sort_range(&mut self, range: std::ops::Range<usize>) {
        let paths = &self.paths;
        self.headers[range].sort_unstable_by(|a, b| {
            let a_dir = paths.resolve(a.dirname).as_bytes();
            let b_dir = paths.resolve(b.dirname).as_bytes();
            let a_name = paths.resolve(a.name).as_bytes();
            let b_name = paths.resolve(b.name).as_bytes();
            a_dir.cmp(b_dir).then_with(|| a_name.cmp(b_name))
        });
    }
}
```

### 2. `headers_slice(range: Range<usize>) -> &[FileEntryHeader]`

Returns a slice of headers for segment-scoped operations (hardlink
matching, delete pipeline publication, iteration).

```rust
impl FlatFileList {
    pub fn headers_slice(&self, range: std::ops::Range<usize>) -> &[FileEntryHeader] {
        &self.headers[range]
    }
}
```

### 3. `reorder_by_permutation(perm: &[usize])`

Reorders the header array according to a permutation vector. Used by
the sender's INC_RECURSE partition step. The permutation is computed
from the legacy Vec's partition and applied to the flat store.

```rust
impl FlatFileList {
    pub fn reorder_by_permutation(&mut self, perm: &[usize]) {
        assert_eq!(perm.len(), self.headers.len());
        let mut reordered = Vec::with_capacity(perm.len());
        for &idx in perm {
            reordered.push(self.headers[idx]);
        }
        self.headers = reordered;
    }
}
```

Path handles and extras refs are embedded in the headers and move with
them. The `PathArena` and `ExtrasArena` are unaffected.

## Memory reclamation (future optimization)

The unified-store model does not reclaim memory as segments are consumed.
In the legacy `Vec<FileEntry>` model, consumed entries stay in the Vec
and are simply skipped by the transfer loop. The flat store follows the
same pattern: consumed headers, interned strings, and extras tails stay
allocated until the entire file list is dropped.

For long INC_RECURSE transfers with many small segments, this means the
store grows monotonically. This is acceptable because:

1. It matches the current behavior (no memory reclamation today).
2. The flat store's per-entry footprint is 2-3x smaller than the legacy
   store (48 bytes vs 96-160 bytes per entry), so the peak RSS is
   smaller even without reclamation.
3. Upstream rsync uses per-flist `pool_destroy()` to reclaim memory, but
   oc-rsync's pipeline is designed around a single flat array. Changing
   to per-segment ownership would require restructuring NDX conversion,
   delete pipeline, and entry access.

A future optimization can introduce per-segment ownership by:
- Splitting the store into per-segment `FlatFileList` instances.
- Changing NDX lookup to search across segment containers.
- Dropping consumed segments to reclaim their headers, paths, and extras.

This is tracked separately and is not required for the initial flat
store migration.

## Verification plan

### T1: handle stability across segment growth

Push entries for segment 0, record their `PathHandle`s and
`ExtrasRef`s. Then push entries for segment 1 (causing potential Vec
reallocation). Verify that resolving the segment-0 handles still
returns the correct values. Cover all three stores: headers (by index),
`PathArena` (by `PathHandle`), `ExtrasArena` (by `ExtrasRef`).

### T2: sort_range does not disturb other segments

Push entries for two segments. Sort segment 1 via `sort_range()`. Verify
that segment 0 entries remain in their original order and their handles
still resolve correctly.

### T3: DualFileList sort parity

Push entries via `DualFileList`, sort a segment range on the legacy Vec
and independently on the flat store. Verify that both stores produce the
same entry order (compare by name/dirname).

### T4: ndx_segments consistency

Simulate multi-segment append (3+ segments) with the +1 gap formula.
Verify that `wire_to_flat_ndx` and `flat_to_wire_ndx` produce correct
conversions for entries in each segment.

### T5: reorder_by_permutation preserves handles

Apply a permutation to the header array. Verify that each header's
`PathHandle` and `ExtrasRef` still resolve to the correct name,
dirname, and extras after reordering.

## Cross-references

- `docs/design/flat-flist-representation.md` - parent design for the
  flat backing store (RSS-A.4).
- `docs/design/rss-a8a-inc-recurse-segment-audit.md` - prerequisite
  audit documenting how `Vec<FileEntry>` handles INC_RECURSE segments
  (findings F1-F9 and action items referenced throughout).
- `docs/design/rss-5-fileentry-pool-shape.md` - earlier pool shape
  design (prototype, not landed).
- `crates/protocol/src/flist/flat/flist.rs` - `FlatFileList`
  implementation (RSS-A.5.d skeleton).
- `crates/protocol/src/flist/flat/intern.rs` - `PathArena`
  implementation (RSS-A.5.c).
- `crates/protocol/src/flist/flat/extras.rs` - `ExtrasArena`
  implementation (RSS-A.5.e).
- `crates/protocol/src/flist/dual.rs` - `DualFileList` migration
  wrapper.
- `crates/transfer/src/receiver/mod.rs` - receiver segment tracking
  (`ndx_segments`).
- `crates/transfer/src/generator/segments.rs` - sender segment
  scheduling (`IncrementalState`, `SegmentScheduler`).
- upstream: `flist.c:2923-2931` (ndx_start computation),
  `flist.c:flist_new()` (per-flist allocation),
  `lib/pool_alloc.c` (pool allocate/destroy).
