# Flist Segmentation: Flat Sender RSS at Scale

Status: proposal (design only, no code changes).

Related work: allocator page-retention (jemalloc static decay), buffer-pool
churn, and the existing INC_RECURSE per-entry heap reclaim. This document
addresses the remaining structural leak: the never-shrinking file-list slot
array on the sender.

## 1. Problem

At scale the sender's resident set grows linearly with the file count instead
of staying flat like upstream rsync. Measured on a 1M-file local transfer:

| Peer | Sender peak RSS | Per-file overhead |
|------|-----------------|-------------------|
| oc-rsync (local) | ~174 MB | ~158 B/file |
| oc-rsync (SSH)   | (higher) | ~444 B/file |
| upstream rsync   | ~7.4 MB | bounded, near-constant |

The dominant structural cause is that the entire file list lives in one
monolithic, never-shrunk backing array:

- `DualFileList { legacy: Vec<FileEntry> }` -
  `crates/protocol/src/flist/dual.rs:17-19`.
- The generator also holds a parallel `full_paths: Vec<PathBuf>` indexed in
  lockstep - `crates/transfer/src/generator/context.rs:63`.

Each `FileEntry` occupies a fixed ~80-byte slot
(`crates/protocol/src/flist/entry/core.rs:78-127`: `name: PathBuf` 24 B,
`dirname: Arc<Path>` 16 B, `size` 8 B, `mtime` 8 B,
`extras: Option<Box<..>>` 8 B, `uid`/`gid`/`mtime_nsec` 12 B, `mode` 2 B,
`present` 1 B, padded to ~80 B). At 1M files the slot array alone is ~80 MB
that is never returned to the allocator for the life of the transfer.

INC_RECURSE segment reclaim *is* wired and called, but it only frees the
*heap owned by* each entry, never the slot itself:

- `crates/transfer/src/generator/transfer/transfer_loop.rs:177-184` -
  on the receiver's `NDX_DONE` echo, gated on
  `inc_recurse && flist_done_remaining > 0`, it calls
  `reclaim_oldest_segment()`.
- `crates/transfer/src/generator/context.rs:607-632` -
  `reclaim_oldest_segment()` computes the flat range `[start..end)` from the
  segment table and calls `file_list.reclaim_segment(start, end)` plus clears
  the parallel `full_paths[start..end]`.
- `crates/protocol/src/flist/dual.rs:161-170` - `reclaim_segment` iterates the
  slice and calls `FileEntry::reclaim_heap_data`.
- `crates/protocol/src/flist/entry/accessors.rs:660-676` -
  `reclaim_heap_data` sets `name = PathBuf::new()`,
  `dirname = Arc::from("")`, `extras = None`, and zeroes the scalars. The
  ~80-byte slot stays in place.

The reason the slot is retained is documented at
`crates/transfer/src/generator/context.rs:593-597`: entries are kept "in
place so NDX-based indexing remains valid", because NDX is currently a flat
offset into the single `Vec`.

There is a second, compounding cause specific to oc-rsync (see Section 8):
the generator builds the *entire* flist eagerly before the transfer loop
starts (`crates/transfer/src/generator/file_list/inc_recurse.rs:143-228`),
so even perfect tail reclaim leaves the *peak* at full-flist size. Store
segmentation is necessary but not sufficient for a flat peak; lazy segment
generation is the required complement and is scoped as follow-on work here.

## 2. Current-State Map (file:line)

### 2.1 Storage

- `crates/protocol/src/flist/dual.rs:17-19` - `DualFileList` is a newtype over
  `Vec<FileEntry>`. `push` (`:45`) is a plain `Vec::push`; `Index<usize>`
  (`:179-185`) and `Index<RangeFrom>` (`:187-193`) return by flat offset.
- `crates/transfer/src/generator/context.rs:63` - generator `full_paths`
  parallel array; invariant "`file_list[i]` corresponds to `full_paths[i]`"
  (`:56-63`).
- Receiver keeps its own growing flist:
  `crates/transfer/src/receiver/file_list/receive.rs:43-52` pushes received
  entries; slices such as `self.file_list[flat_start..]` (`:49,212,221,237`)
  drive per-segment sort, hardlink matching, and pruning.

### 2.2 NDX resolution (both sides)

Wire NDX values carry a +1 gap between INC_RECURSE segments
(upstream `flist.c` `ndx_start = prev->ndx_start + prev->used + 1`). Both
sides translate wire NDX to a flat array index through a segment table:

- Table: `ndx_segments: Vec<(flat_start, ndx_start)>`
  - generator: `crates/transfer/src/generator/segments.rs:156-164`, initial
    `vec![(0, initial_ndx_start)]` (`:187`).
  - receiver: `crates/transfer/src/receiver/context.rs:66`, initial
    `vec![(0, initial_ndx_start)]` (`:249`).
- Generator lookup: `wire_to_flat_ndx`
  (`crates/transfer/src/generator/context.rs:178-187`) -
  `partition_point` on `ndx_start`, then `flat_start + (wire_ndx - ndx_start)`.
  Inverse `flat_to_wire_ndx` (`:202-209`, test-only).
- Receiver lookup: `wire_to_flat_ndx`
  (`crates/transfer/src/receiver/context.rs:332-355`), bounded by the next
  segment's `flat_start`, returns `Option<usize>`.
- Hot access after resolution indexes the flat Vec directly:
  `crates/transfer/src/generator/transfer/transfer_loop.rs:284`
  (`self.wire_to_flat_ndx(wire_ndx)`), `:316-318`, `:376`
  (`&self.file_list[ndx]`), `:382` (`&self.full_paths[ndx]`).

### 2.3 Segment build (INC_RECURSE)

- `crates/transfer/src/generator/file_list/inc_recurse.rs:32-53` -
  `partition_file_list_for_inc_recurse` classifies then reorders.
- `:143-228` - `reorder_and_build_segments` moves every entry into one fresh
  `DualFileList` (initial entries first, then depth-first sub-segments),
  recording each sub-segment's `flist_start`/`count` in a `PendingSegment`
  (`:206-225`). The full flist is resident before any bytes are sent.
- `crates/transfer/src/generator/protocol_io.rs:594-614` - when a sub-list is
  dispatched, `ndx_segments.push((flist_start, seg_ndx_start))` with
  `seg_ndx_start = prev_ndx_start + prev_used + 1` (mirrors upstream +1 gap).

### 2.4 Reclaim + gate

- `crates/transfer/src/generator/segments.rs:165-176` -
  `first_segment_idx` (oldest unreclaimed segment) "mirrors upstream's
  `first_flist` pointer".
- `crates/transfer/src/generator/transfer/transfer_loop.rs:107-112` -
  `flist_done_remaining` counts pending flist-free echoes; incremented per
  segment sent (`:141`, `:594`), decremented on the `NDX_DONE` echo (`:178`).
- `reclaim_oldest_segment` keeps the current segment: guard
  `first + 1 >= segments.len()` (`context.rs:611-614`).

### 2.5 NDX consumers that constrain freeing

- Phase-2 redo uses full 16-byte checksums and re-requests by NDX:
  `crates/transfer/src/receiver/mod.rs:80` (`REDO_CHECKSUM_LENGTH =
  MAX_SUM_LENGTH`); redo indices collected in
  `crates/transfer/src/reader/multiplex.rs:88-90,247-257` and consumed against
  `self.file_list.get(idx)` in
  `crates/transfer/src/receiver/transfer/pipelined.rs:158-176`.
- Hardlink groups reference NDX leaders:
  `crates/transfer/src/generator/file_list/hardlinks.rs:34-67`
  (`wire_ndx = ndx_start + i`), receiver side
  `crates/transfer/src/receiver/file_list/hardlinks.rs:41-76`.
- Delete-stats / goodbye: `NDX_DEL_STATS = -3`
  (`crates/protocol/src/codec/ndx/constants.rs:21`), handled at
  `transfer_loop.rs:244-261`.
- Control NDX constants: `NDX_DONE = -1`, `NDX_FLIST_EOF = -2`,
  `NDX_FLIST_OFFSET = -101` (`constants.rs:9-26`).

## 3. Upstream Reference Model

Upstream stores the flist as a **circular doubly-linked list of separately
allocated `file_list` objects**, each owning its own `file_struct` array.

- `rsync.h:964-975` - `struct file_list { next, prev; files, sorted;
  file_pool; used, malloced; low, high; ndx_start; flist_num; parent_ndx;
  in_progress, to_redo; }`.
- Globals `flist.c:101-103` - `cur_flist, first_flist, dir_flist`,
  `flist_cnt`.
- `flist.c:2960-2977` - `flist_new` appends to the list and assigns
  `flist->ndx_start = prev->ndx_start + prev->used + 1` (the +1 gap oc
  reproduces in `ndx_segments`).
- `rsync.c:787-821` - `flist_for_ndx(ndx, ...)` walks from `cur_flist`
  backward/forward until `flist->ndx_start <= ndx < flist->ndx_start +
  flist->used`; then the caller resolves `file =
  flist->files[ndx - flist->ndx_start]` (e.g. `sender.c:266-269`). Out of
  range is a fatal "File-list index N not in first-last" protocol error.
- `flist.c:2980-3012` - `flist_free(flist)` unlinks the object, decrements
  `file_total`/`flist_cnt`, `pool_free_old` on the segment's pool boundary,
  and `free()`s `sorted`, `files`, and the object. The completed segment's
  entry array is **fully deallocated**.
- `sender.c:240-258` - on `NDX_DONE` in inc_recurse mode the sender does
  `file_old_total -= first_flist->used; flist_free(first_flist);` and, if
  another flist remains, echoes `NDX_DONE` and continues without advancing
  phase. This is exactly the gate oc reproduces with `flist_done_remaining`
  and `reclaim_oldest_segment`, except upstream frees the whole segment.

Only the window between `first_flist` and `cur_flist` is ever live, so peak
memory is bounded by the lookahead window (`MIN_FILECNT_LOOKAHEAD`), not by
the total file count. Generation is lazy: `send_extra_file_list` produces the
next sub-list on demand (`sender.c:230-232`).

## 4. Target Design

Replace the single flat backing array with a **list of owned segments**, and
change NDX resolution from a flat offset into one `Vec` to a
`(segment, offset)` pair - mirroring upstream `flist_for_ndx`. A completed
sender segment's entry array is then dropped in full, slots included.

### 4.1 Data structure

```
struct Segment {
    ndx_start: i32,          // wire ndx of entries[0] (upstream ndx_start)
    entries: Option<Box<[FileEntry]>>,  // None once fully freed
    // parallel full_paths carried per-segment while that array still exists
    full_paths: Option<Box<[PathBuf]>>, // generator-only; see Section 9
}

struct SegmentedFileList {
    segments: Vec<Segment>,  // append-only; ordered by ndx_start
    first_live: usize,       // index of oldest non-freed segment (== first_flist)
    total_used: usize,       // sum of live segment lengths (diagnostics)
}
```

Notes:

- `entries` is `Box<[FileEntry]>` (not `Vec`) once a segment is sealed: the
  length is fixed after build, so a boxed slice drops the growth slack a
  `Vec` would keep.
- `Option` is the tombstone: freeing sets `entries = None` (and
  `full_paths = None`) while retaining `ndx_start`/length metadata so NDX
  arithmetic and diagnostics for *later* segments remain correct. The `Vec`
  of tombstones is O(number of segments), not O(files) - a segment is a whole
  directory sub-list, so this is bounded by tree structure, typically
  thousands, and each tombstone is a few words.
- Optionally, freed tombstones below `first_live` can be coalesced by storing
  `used` alongside `ndx_start`; the metadata cost is negligible so tombstone
  retention is preferred for the simplest correctness argument.

### 4.2 NDX resolution API

```
fn segment_of(&self, wire_ndx: i32) -> Option<usize>   // partition_point on ndx_start
fn ndx_to_entry(&self, wire_ndx: i32) -> Option<&FileEntry>
fn ndx_to_entry_mut(&mut self, wire_ndx: i32) -> Option<&mut FileEntry>
```

`ndx_to_entry` finds the segment via `partition_point(|s| s.ndx_start <=
wire_ndx) - 1`, computes `offset = wire_ndx - seg.ndx_start`, bounds-checks
against the segment length, and indexes `seg.entries[offset]`. This is the
exact shape of upstream `flist_for_ndx` + `files[ndx - ndx_start]`, and it
subsumes today's `ndx_segments` table (the `(flat_start, ndx_start)` pairs
become the per-segment `ndx_start`; `flat_start` disappears once callers stop
using flat offsets).

Accessing a freed segment (`entries == None`) returns `None`, which callers
map to the same fatal "invalid file index" protocol error upstream raises -
this must never happen if the invariants in Section 5 hold, so it is a
defensive assertion, not a control path.

### 4.3 Segment append (INC_RECURSE)

`reorder_and_build_segments`
(`crates/transfer/src/generator/file_list/inc_recurse.rs:143-228`) changes
from pushing into one `DualFileList` to sealing one `Segment` per sub-list:

- initial entries become `segments[0]` with `ndx_start = initial_ndx_start`;
- each `PendingSegment` becomes a `Segment` with
  `ndx_start = prev.ndx_start + prev.used + 1` (the value already computed at
  `protocol_io.rs:602`), and its `entries`/`full_paths` boxed slices.

The wire encoding is unchanged: the +1 gap and dispatch order are preserved,
so the receiver sees byte-identical output.

### 4.4 Segment free

`reclaim_oldest_segment` becomes a true free:

```
fn reclaim_oldest_segment(&mut self) {
    if self.first_live + 1 >= self.segments.len() { return; } // keep current
    let s = &mut self.segments[self.first_live];
    s.entries = None;       // Box<[FileEntry]> dropped in full
    s.full_paths = None;    // parallel array dropped in full
    self.first_live += 1;   // advances like upstream first_flist
}
```

The gate is unchanged (`flist_done_remaining`, keep-current). The only
behavioural change is that the freed segment's slots are returned to the
allocator instead of being zeroed in place.

### 4.5 Receiver consistency

The wire protocol does not change, so the receiver is unaffected by the
sender-side rewrite. The receiver already resolves NDX through its own
`ndx_segments` table (`receiver/context.rs:332-355`) and grows its own
`file_list`. The same `SegmentedFileList` type can later back the receiver
(the receiver frees on `NDX_DONE` too, upstream `receiver.c:573`), but that is
a separate, independently testable migration. This design keeps the receiver
on its current flat store; correctness only requires that the sender emit the
identical wire stream, which it does.

## 5. Wire-Compat Invariants and Gates

A segment must not be freed while any of its NDX values may still be
referenced. Premature free produces upstream's fatal "invalid file index" or a
goodbye/phase desync ("connection unexpectedly closed"). Each invariant and
its gate:

| # | Invariant | Why | Gate |
|---|-----------|-----|------|
| I1 | Never free the current (newest) segment | in-flight requests target it | existing keep-current guard `first_live + 1 >= len` |
| I2 | Free only after the receiver acks the segment done | pending file requests for that segment | existing `flist_done_remaining > 0` + `NDX_DONE` echo (`transfer_loop.rs:177-184`), one-to-one with upstream `flist_free(first_flist)` |
| I3 | Phase-2 redo must not target a freed segment | redo re-requests by NDX with full checksums | **potential new gate**: redo requests arrive in phase > 0; verify no `NDX_DONE`-driven free crosses a segment that still has outstanding redo entries. Upstream avoids this because redo happens before the segment's `NDX_DONE`; oc must preserve the same ordering (see Section 7 risk R2) |
| I4 | Hardlink group leaders stay resolvable | later members reference leader NDX | leaders live in the same segment as members in inc_recurse (per-dir sub-list); freeing the segment frees the whole group atomically. Cross-segment hardlink groups are not produced by upstream's per-dir grouping - assert this holds |
| I5 | Goodbye / `NDX_DONE` handshake stays lock-step | phase counter desync closes the connection | unchanged: the free path already echoes `NDX_DONE` exactly as upstream; segment free must not alter the echo count |
| I6 | `NDX_DEL_STATS` and delete accounting unaffected | delete stats are read as a control NDX, not a flist entry | no flist lookup; independent of segmentation |
| I7 | Diagnostics / NDX arithmetic for later segments | freeing an early segment must not shift later `ndx_start` | tombstone retains `ndx_start`; `partition_point` over all segments (live or freed) stays monotonic |

The central invariant is **I2**: the existing `flist_done_remaining` gate is
already the upstream `flist_free(first_flist)` trigger. The rewrite reuses it
verbatim; it only changes *what* the trigger does (drop the array vs. zero it).

## 6. Touch Points

Introduce the new type behind the existing API first, then migrate callers.

- `crates/protocol/src/flist/dual.rs` - add `SegmentedFileList` (new module,
  e.g. `flist/segmented.rs`) exposing the current `DualFileList` surface
  (`push`, `len`, `get`, `iter`, `Index`, `reclaim_segment`,
  `sort_with_parallel`) so callers compile unchanged, plus the new
  `ndx_to_entry`/segment API.
- `crates/transfer/src/generator/context.rs` - `file_list` field type;
  `wire_to_flat_ndx`/`reclaim_oldest_segment`; `full_paths` per-segment
  ownership (composes with the parallel `full_paths` removal, Section 9).
- `crates/transfer/src/generator/segments.rs` - `ndx_segments` /
  `first_segment_idx` fold into the segment list; `PendingSegment` build.
- `crates/transfer/src/generator/file_list/inc_recurse.rs` -
  `reorder_and_build_segments` seals `Segment`s instead of one `Vec`.
- `crates/transfer/src/generator/file_list/iconv.rs`,
  `.../file_list/mod.rs` - flist construction / `sort_with_parallel` sites.
- `crates/transfer/src/generator/protocol_io.rs` - `ndx_segments.push` at
  sub-list dispatch (`:594-614`).
- `crates/transfer/src/generator/transfer/transfer_loop.rs` - NDX resolution
  at `:284,316-318,376,382`.
- `crates/transfer/src/generator/file_list/hardlinks.rs` - leader NDX
  assignment against per-segment `ndx_start`.
- Receiver (`receiver/context.rs`, `receiver/file_list/*`) - **no change** in
  the initial rewrite; optional later migration.

## 7. Phasing

Each phase is independently mergeable and wire-safe.

**Phase 0 - Introduce `SegmentedFileList` behind the flat API (no behaviour
change).** Add the type with an internal single-segment representation that
reproduces the current flat semantics exactly; keep `DualFileList` as a
type alias or thin wrapper. Ship it unused or wired only where trivially
equivalent. Test: unit tests for `ndx_to_entry` parity with flat indexing;
existing suite green.

**Phase 1 - Migrate NDX resolution to `(segment, offset)`.** Replace flat
`file_list[ndx]` and `wire_to_flat_ndx` call sites with `ndx_to_entry`, while
the store still holds all segments live (no freeing yet). Segment table
becomes the segment list. Test: interop parity (daemon + SSH push/pull, all
upstream versions), golden NDX round-trip, INC_RECURSE multi-segment
sub-directory pulls; RSS unchanged (still linear) - this phase is behaviour-
preserving.

**Phase 2 - Build sealed segments in `reorder_and_build_segments`.** Store
each sub-list as its own `Box<[FileEntry]>`; `first_live` tracks the oldest.
Still no freeing. Test: byte-for-byte wire equivalence (golden), hardlink and
sort-per-segment correctness.

**Phase 3 - Enable sender-segment free.** `reclaim_oldest_segment` drops the
oldest segment's arrays. Test: RSS-at-scale benchmark (1M files, expect flat/
declining sender RSS); adversarial tests for I1-I5 (phase-2 redo across a
freed boundary, hardlink group at a segment edge, forced `NDX_DONE` timing).

**Phase 4 (follow-on, separate design) - Lazy segment generation.** Build
sub-lists on demand (upstream `send_extra_file_list` + lookahead) instead of
eagerly in `reorder_and_build_segments`, so the *peak* (not just the tail)
is bounded. Required to reach upstream's ~7.4 MB; Phase 3 alone bounds the
declining tail but leaves the t=0 peak at full-flist size (Section 8).

Test strategy per phase, in short: unit parity (Phase 0-1), golden wire bytes
(Phase 2), interop matrix + RSS bench + adversarial NDX/redo/hardlink
(Phase 3), RSS-peak bench (Phase 4).

## 8. Expected Outcome and Interaction with Eager Build

- **Phase 3 (store segmentation)** makes the sender's RSS *decline* as
  segments complete: each freed sub-list returns its ~80 B/entry slots plus
  paths to the allocator. Sender tail RSS at end of transfer approaches the
  live window instead of the full flist.
- **Peak** RSS after Phase 3 is still ~full-flist because
  `reorder_and_build_segments` materializes every entry before the transfer
  loop starts. To match upstream's flat ~7.4 MB, **Phase 4 lazy generation**
  is required so only the lookahead window is ever resident. This is an
  honest limitation of scoping the rewrite to the store: segmentation is the
  enabling structural change (it makes full free possible), and lazy
  generation is what turns "declining" into "flat peak".
- **CPU tradeoff.** NDX resolution moves from an O(1) flat index to a
  `partition_point` over the segment list (O(log S), S = segment count),
  identical in cost to today's `wire_to_flat_ndx` which already does this -
  so no net regression; the flat `file_list[ndx]` fast paths are the only
  ones that gain the log factor, and S is bounded by directory count, not
  file count. Allocator traffic rises modestly (one alloc/free per segment
  instead of one big block), well within upstream's per-segment pool model.

## 9. Interaction with `full_paths` Removal

A separate effort removes the generator's parallel
`full_paths: Vec<PathBuf>` (`context.rs:63`), reconstructing the absolute
path on demand from `dirname`/`name` + the transfer root instead of storing
it. The two efforts compose cleanly and are mutually reinforcing:

- If `full_paths` is removed first, `Segment` carries only `entries`; there is
  no parallel array to box or free, simplifying Sections 4.1 and 4.4.
- If segmentation lands first, `Segment.full_paths` is the per-segment owner
  and is dropped in full on free (already better than today's in-place
  clear at `context.rs:627-630`); the later removal simply deletes that field.
- Either order is safe: neither changes the wire, and both reduce the same
  ~per-file overhead (`full_paths` removal removes the `PathBuf` slot;
  segmentation removes the `FileEntry` slot). Landing both yields the full
  reduction toward upstream parity.

Recommended order: land `full_paths` removal first (smaller, orthogonal),
then this segmentation on the simplified single-array `Segment`.

## 10. Risks

- **R1 - NDX resolution regression.** A mis-mapped `(segment, offset)` sends
  the wrong file or a fatal index error. Mitigation: Phase 1 migrates
  resolution with the store still fully live (no free), so any mapping bug
  surfaces under the full interop matrix before freeing is enabled.
- **R2 - Phase-2 redo across a freed boundary (I3).** If a redo re-requests an
  NDX in a segment already freed by an earlier `NDX_DONE`, the sender faults.
  Upstream orders redo before the segment's `NDX_DONE`; oc must preserve that
  ordering. Mitigation: adversarial test forcing a phase-1 checksum failure
  in an early segment, then verifying the segment is not freed until its redo
  is drained; if ordering cannot be guaranteed, add an explicit
  "outstanding-redo" refcount per segment gating the free.
- **R3 - Cross-segment hardlink group (I4).** Assumed absent under upstream's
  per-directory grouping; if a group ever spans segments, freeing one half
  breaks the other. Mitigation: assert single-segment groups in Phase 2;
  property test over hardlink fixtures.
- **R4 - Goodbye desync (I5).** The `NDX_DONE` echo count is load-bearing
  (a prior off-by-one caused "connection unexpectedly closed"). Mitigation:
  Phase 3 changes only the free body, not the echo logic; interop regression
  on multi-flist subdirectory pulls.
- **R5 - Peak unchanged without Phase 4.** Stakeholders expecting flat *peak*
  from Phase 3 alone will be disappointed. Mitigation: this document scopes
  the deliverable explicitly (Section 8) and lists Phase 4 as the required
  complement.

## 11. Summary

Segment the sender's flist store into a list of owned `Box<[FileEntry]>`
arrays and resolve NDX as `(segment, offset)` via `partition_point` on
per-segment `ndx_start` - directly mirroring upstream `flist_for_ndx` and
`file_list` linked list. Reuse the existing `flist_done_remaining` /
`NDX_DONE`-echo gate (already the upstream `flist_free(first_flist)` trigger)
so a completed segment's slots are dropped in full rather than zeroed in
place. Land it in four wire-safe phases (introduce type, migrate resolution,
build sealed segments, enable free), with a fifth follow-on (lazy generation)
required to bound the *peak*. Compose with the parallel `full_paths` removal
for the full per-file overhead reduction toward upstream's flat RSS.
