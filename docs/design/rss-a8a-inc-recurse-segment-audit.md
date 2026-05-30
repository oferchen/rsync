# RSS-A.8.a: INC_RECURSE segment append audit

Task: RSS-A.8.a. Branch: `docs/rss-a8a-inc-recurse-segment-audit`.
Prerequisites: RSS-A.5.d (FlatFileList skeleton), RSS-A.5.e (ExtrasArena).
Downstream: RSS-A.8.b (FlatFileList segment growth implementation).

## Summary

This audit documents how Vec<FileEntry> grows during INC_RECURSE
transfers and how segment boundaries are tracked, so the flat flist
migration (FlatFileList with arena backing) can replicate the behavior
without invalidating handles.

## Background: upstream segment model

In upstream rsync (`flist.c`), INC_RECURSE causes the file list to be
sent as multiple sub-lists (one per directory). Each sub-list is a
separate `struct file_list` allocated by `flist_new()` with its own
`ndx_start`:

```c
// flist.c:2923 - first flist starts at NDX 1 with INC_RECURSE
flist->ndx_start = flist->flist_num = inc_recurse ? 1 : 0;

// flist.c:2931 - subsequent flists get ndx_start = prev + used + 1
flist->ndx_start = prev->ndx_start + prev->used + 1;
```

The +1 gap between segments is a sentinel that separates NDX ranges.
Sub-lists are linked in a doubly-linked list and freed as the
generator/receiver finishes processing each one.

A separate `dir_flist` accumulates all directory entries across all
segments (`flist.c:2667-2669`). The receiver validates each sub-list's
dirname against `dir_flist` entries (`flist.c:2652-2659`).

## Current oc-rsync implementation

### Sender (GeneratorContext)

The sender stores all entries in a single `DualFileList` (wraps
`Vec<FileEntry>` plus optional `FlatFileList`). INC_RECURSE partitioning
happens in-memory after the full file list is built and sorted.

**File list building** (`generator/file_list/mod.rs`):
1. `build_file_list()` walks the filesystem, pushes entries to
   `self.file_list: DualFileList`, sorts via indirect permutation.
2. `partition_file_list_for_inc_recurse()` (in `file_list/inc_recurse.rs`)
   reorders the flat array so initial (top-level) entries come first,
   followed by sub-directory entries in depth-first order.
3. Classification produces `PendingSegment` structs referencing the
   reordered array by `(flist_start, count)` ranges.

**Segment tracking** (`generator/segments.rs`):
- `IncrementalState.ndx_segments: Vec<(usize, i32)>` maps
  `(flat_start, ndx_start)` for each segment.
- `IncrementalState.initial_segment_count: Option<usize>` limits how many
  entries `send_file_list()` emits.
- `IncrementalState.pending_segments: Vec<PendingSegment>` holds segments
  for lazy dispatch during the transfer loop.
- `SegmentScheduler` wraps pending_segments with a cursor and
  `MIN_FILECNT_LOOKAHEAD` (1000) throttling.

**Segment dispatch** (`generator/protocol_io.rs`):
- `send_file_list()` sends only the initial segment, caches the
  `FileListWriter` for compression state continuity.
- `encode_and_send_segment()` computes `seg_ndx_start` using the +1 gap
  formula, writes `NDX_FLIST_OFFSET - parent_dir_ndx`, encodes entries,
  writes end marker, and pushes `(flat_start, seg_ndx_start)` to
  `ndx_segments`.
- `send_flist_eof()` writes `NDX_FLIST_EOF` when the scheduler is
  exhausted.

**NDX conversion** (`generator/context.rs`):
- `wire_to_flat_ndx(wire_ndx)` - binary search on `ndx_segments` via
  `partition_point`, returns flat array index.
- `flat_to_wire_ndx(flat_idx)` - inverse binary search.

### Receiver (ReceiverContext)

The receiver stores all entries in `self.file_list: Vec<FileEntry>`.
Segments are appended in-place as they arrive from the wire.

**Initial file list** (`receiver/file_list/receive.rs`):
1. `receive_file_list()` reads entries via `FileListReader`,
   appends to `self.file_list` starting from `seg_start = file_list.len()`.
2. After all entries: receives UID/GID lists (non-INC_RECURSE only),
   sorts the segment slice, matches hardlinks.
3. Caches the `FileListReader` for sub-list state continuity.

**Sub-list reception** (`receive_extra_file_lists()`):
1. Reads `NDX_FLIST_OFFSET - dir_ndx` framing.
2. Records `flat_start = self.file_list.len()`.
3. Computes `seg_ndx_start = prev_ndx_start + prev_used + 1` (the +1 gap).
4. Reads entries, appends to `self.file_list`.
5. Sorts the segment slice `self.file_list[flat_start..]`.
6. Matches hardlinks within the segment.
7. Pushes `(flat_start, seg_ndx_start)` to `self.ndx_segments`.
8. Publishes segment to delete pipeline.
9. Repeats until `NDX_FLIST_EOF`.

**NDX conversion** (`receiver/mod.rs`):
- Same `partition_point`-based O(log n) lookup as the sender.
- `wire_to_flat_ndx()` additionally bounds-checks against the next
  segment's flat_start.

### DualFileList (protocol/flist/dual.rs)

Wraps `Vec<FileEntry>` (legacy) and optionally `FlatFileList + ExtrasArena`
(behind `flat-flist` feature flag). Key methods for INC_RECURSE:

- `push(entry)` - appends to both stores.
- `segment_start()` - returns `legacy.len()`, used as segment boundary.
- `as_mut_vec()` - exposes `&mut Vec<FileEntry>` for sorting and
  INC_RECURSE segment manipulation.
- Index operators delegate to legacy Vec.

### SegmentedFileList (protocol/flist/segment.rs)

A standalone segment container (`FileListSegment` with `ndx_start`,
`parent_dir_ndx`, `entries: Vec<FileEntry>`). Currently used for
structural modeling but not wired into the transfer pipeline - both
sender and receiver use a single flat Vec with index-based segment
boundaries instead.

### IncrementalFileList (protocol/flist/incremental/mod.rs)

Dependency-tracking state machine for streaming file list processing.
Tracks parent directory availability via `created_dirs: HashSet<String>`.
Entries are queued in `pending: HashMap<String, Vec<FileEntry>>` until
their parent is ready. Used by `IncrementalFileListReceiver` on the
receiver side for streaming processing of INC_RECURSE entries.

## Operations that work on segment slices

### 1. Sort (per-segment)

**Sender**: Sorts the full file list once before partitioning
(`build_file_list()` in `file_list/mod.rs`). The partition step reorders
entries so segments are contiguous, but does not re-sort within segments.

**Receiver**: Sorts each segment independently after reception:
- Initial: `sort_file_list(&mut self.file_list, ...)` on the full list.
- Sub-lists: `sort_file_list(&mut self.file_list[flat_start..], true, false)`.

Sort operates on `&mut [FileEntry]` via `compare_file_entries()` from
`protocol/flist/sort.rs`. Uses `sort_unstable_by` (no stability needed).

### 2. Hardlink matching (per-segment)

Both sender and receiver run `match_hard_links()` on segment slices.
Receiver also runs `normalize_pre30_hardlinks()` for protocol < 30.
These operate on `&mut [FileEntry]`.

### 3. NDX lookup (cross-segment)

Both sender and receiver convert between wire NDX and flat array index
using the `ndx_segments` table. This is a hot path during the transfer
loop - every file transfer request requires a conversion.

### 4. Delete pipeline publication (per-segment, receiver only)

`publish_segment_to_delete_pipeline()` passes `&self.file_list[flat_start..]`
to `DeleteContext::observe_segment_for_delete()`. Uses `wire_to_flat_ndx()`
to resolve the parent directory entry.

### 5. Entry access by flat index (cross-segment)

The transfer loop accesses entries by flat index: `self.file_list[ndx]`.
Both sender and receiver index into the combined flat array after
converting wire NDX to flat index.

### 6. Segment scheduling (sender only)

`SegmentScheduler` holds `Vec<PendingSegment>` referencing the flat
array by `(flist_start, count)` ranges. Yields segments when remaining
file count drops below `MIN_FILECNT_LOOKAHEAD` (1000).

## FlatFileList requirements for segment growth

### Append without invalidating handles

The critical requirement: appending entries to FlatFileList must not
invalidate existing `PathHandle` or `ExtrasRef` values. The current
implementation satisfies this because:

- `PathArena` uses a `Vec<u8>` byte arena plus a `Vec<Span>` handle
  table. Appending new strings may reallocate the Vec, but existing
  handles (indices into the span table) remain valid - they reference
  positions within the arena that don't move relative to each other.
- `ExtrasArena` uses a similar append-only byte buffer with offset-based
  references. Appending new extras records doesn't invalidate existing
  offsets.
- `Vec<FileEntryHeader>` reallocation preserves index-based access.

This is already the correct design for INC_RECURSE segment growth.
No pointer-based references exist that could be invalidated by
reallocation.

### Segment-slice access

FlatFileList needs to support slicing over segment ranges for:
- Sorting a segment: `flat.sort_range(flat_start..flat_end)`.
- Iterating a segment: `flat.iter_range(flat_start..flat_end)`.
- Passing segment data to delete pipeline.

Currently, FlatFileList exposes `get(index)` and `iter()` but lacks
range-based accessors. These are straightforward to add since the
underlying `Vec<FileEntryHeader>` supports standard slice operations.

### Sort within segments

FlatFileList.sort() currently sorts the entire list. For INC_RECURSE,
segment-local sorting is needed. The sort comparator resolves path
handles through the PathArena, so sorting a sub-range of headers is
safe as long as all handles reference the same arena (which they do -
there is one PathArena per FlatFileList).

Implementation: add `sort_range(range: Range<usize>)` that calls
`self.headers[range].sort_unstable_by(...)` with the same comparator.

### NDX segment table

The `ndx_segments: Vec<(usize, i32)>` translation table is independent
of the file list storage format. It maps flat indices to wire NDX values
regardless of whether the backing store is Vec<FileEntry> or
FlatFileList. No changes needed.

### DualFileList synchronization

DualFileList.push() already appends to both legacy Vec and FlatFileList.
For INC_RECURSE sub-list sorting, DualFileList needs to sort both stores
in sync. Currently, `as_mut_vec()` exposes only the legacy Vec for
sorting. Options:

1. **Sort both independently** - sort legacy Vec via `sort_file_list()`,
   sort FlatFileList via `sort_range()`. Both produce the same order
   because they use equivalent comparators.
2. **Sort via permutation** - compute the sort permutation on one store,
   apply it to both. More complex but guarantees identical ordering.
3. **Sort legacy, rebuild flat** - sort the legacy Vec, then rebuild the
   FlatFileList segment from the sorted entries. Simple but wasteful.

Option 1 is cleanest: each store sorts independently using its own
optimized comparator, and the invariant that they produce the same order
is tested.

## Segment lifecycle summary

```
  Sender                          Wire                        Receiver
  ------                          ----                        --------
  build_file_list()                                           
  sort(full list)                                             
  partition_for_inc_recurse()                                 
    -> initial entries at [0..N)                               
    -> pending segments at [N..)                               
                                                              
  send_file_list()  ---------->  entries[0..N) + end  ------> receive_file_list()
    ndx_start = 1                                               sort(0..N)
                                                                match_hardlinks(0..N)
                                                              
  transfer loop:                                              
    scheduler.next_if_needed()                                
    encode_and_send_segment() ->  NDX_FLIST_OFFSET-dir_ndx    receive_extra_file_lists()
                                  entries + end  ------------>   append to file_list
                                                                 sort(flat_start..)
                                                                 match_hardlinks(flat_start..)
                                                                 push ndx_segment
                                                                 publish to delete pipeline
    ...repeat per segment...      ...repeat...                   ...repeat...
                                                              
    send_flist_eof() ---------->  NDX_FLIST_EOF  ------------>   break loop
```

## Findings

### F1: Single flat array with index-based segmentation

Both sender and receiver use a single contiguous Vec<FileEntry> with
`ndx_segments: Vec<(usize, i32)>` for segment boundaries. This is
already compatible with FlatFileList's `Vec<FileEntryHeader>` model.
No structural changes needed for the arena migration - segment
boundaries remain index-based.

### F2: Per-segment sort uses sub-slice operations

The receiver sorts each segment independently via
`sort_file_list(&mut self.file_list[flat_start..], ...)`. FlatFileList
needs a `sort_range()` method but the underlying operation is
straightforward.

### F3: +1 NDX gap between segments is computed, not stored

The gap between segments (`ndx_start = prev_ndx_start + prev_used + 1`)
is computed at segment creation time and stored in `ndx_segments`. It
does not affect the flat array layout - entries are contiguous in the
Vec regardless of the NDX gap.

### F4: PathArena/ExtrasArena handles survive reallocation

Arena handles are index-based (PathHandle is an index into a span
table, ExtrasRef is a byte offset). Vec reallocation during append
preserves these indices. INC_RECURSE segment growth is safe.

### F5: DualFileList.as_mut_vec() bypasses flat store

The `as_mut_vec()` escape hatch exposes the legacy Vec for direct
manipulation (sorting, INC_RECURSE segment reordering). The flat
store is not updated when the legacy Vec is mutated directly. This
must be addressed before the flat store can become authoritative:
either sort both stores independently (recommended) or sort via
shared permutation.

### F6: SegmentedFileList is unused in the pipeline

`protocol/flist/segment.rs` defines `SegmentedFileList` with per-segment
`Vec<FileEntry>` storage, but it is not used by the transfer pipeline.
Both sender and receiver use the flat-array-plus-segment-table approach
instead. This type can serve as documentation but should not be confused
with the actual implementation.

### F7: FileListWriter/Reader cache across segments

Both sender and receiver cache the flist writer/reader between segments
to preserve compression state (`prev_name`, `prev_mode`, `prev_uid`,
`prev_gid`). This is orthogonal to the backing store format - it
concerns wire encoding, not storage.

### F8: Sender partitions in-place, receiver appends incrementally

The sender builds the full list, sorts it, then partitions into segments
by reordering entries. The receiver appends entries as they arrive and
sorts each segment independently. FlatFileList must support both
patterns:
- Sender: bulk push, then reorder/partition (needs mutable header access
  or permutation-based reorder).
- Receiver: incremental push with per-segment sort.

### F9: Delete pipeline observes segment slices

The delete pipeline receives `&[FileEntry]` slices for each segment.
When FlatFileList becomes authoritative, the delete pipeline will need
either:
- A conversion from FlatFileList range to `Vec<FileEntry>` (temporary).
- Direct access to FlatFileList segment ranges (preferred long-term).

## Action items for RSS-A.8.b

1. Add `sort_range(range: Range<usize>)` to FlatFileList.
2. Add `headers_slice(range: Range<usize>) -> &[FileEntryHeader]` for
   segment access.
3. Update DualFileList to sort both stores when a segment is sorted
   (option 1: independent sort with parity test).
4. Add `reorder_by_permutation(perm: &[usize])` to FlatFileList for
   the sender's partition step.
5. Verify PathHandle/ExtrasRef stability across segment-growth pushes
   with a dedicated test.
