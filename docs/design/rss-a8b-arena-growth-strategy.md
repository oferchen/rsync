# RSS-A.8.b: Arena growth strategy for mid-transfer segment appends

Task: RSS-A.8.b. Branch: `docs/rss-a8b-arena-growth-strategy`.
Prerequisites: RSS-A.8.a (INC_RECURSE segment append audit),
RSS-A.5.c (PathArena), RSS-A.5.e (ExtrasArena).
Downstream: RSS-A.8.c (FlatFileList segment growth implementation).

## Summary

When INC_RECURSE is active, the sender transmits the file list as
multiple segments - one per directory - that arrive mid-transfer. Each
segment appends entries to the receiver's file list, growing the
PathArena, ExtrasArena, and header Vec. This document evaluates three
growth strategies and recommends the one that best fits the project's
constraints.

The critical invariant: **existing PathHandle and ExtrasRef values must
remain valid after growth.** Every handle issued before a segment append
must resolve to the same data after it.

## Current handle representations

### PathHandle (intern.rs)

A `u32` newtype indexing into `PathArena::spans: Vec<(u32, u32)>`.
Each span records `(byte_offset, byte_length)` into `PathArena::bytes:
Vec<u8>`. Resolution is O(1): look up the span by handle index, slice
the byte arena.

```
PathHandle(3)  -->  spans[3] = (offset=42, len=8)  -->  bytes[42..50]
```

`PathHandle::NONE` is `u32::MAX`, reserved as the empty sentinel.

Deduplication uses a `HashMap<Box<str>, PathHandle>` side table. Two
entries with the same basename share one handle - upstream cannot do
this because basenames are inline in `file_struct`.

### ExtrasRef (extras.rs)

A `u32` newtype storing a raw byte offset into `ExtrasArena::blobs:
Vec<u8>`. The record at that offset starts with a 2-byte presence mask
followed by the present fields in canonical order. Resolution is O(1):
seek to the offset, decode the mask, read fields.

```
ExtrasRef(128)  -->  blobs[128..128+N]  (self-describing length-prefixed tail)
```

`ExtrasRef::NO_EXTRAS` is `u32::MAX`, the common-case sentinel for
entries with no optional metadata.

### FileEntryHeader (header.rs)

A fixed-size (48 bytes), `Copy` struct stored in `FlatFileList::headers:
Vec<FileEntryHeader>`. Contains inline scalars (mtime, size, uid, gid,
mode, flags, present bitfield) plus three arena references: `name:
PathHandle`, `dirname: PathHandle`, `extras: ExtrasRef`.

## Handle stability under growth

Both handle types are **index-based**, not pointer-based:

- `PathHandle` is an index into the `spans` Vec. When `bytes` or `spans`
  reallocates (moves to a larger buffer), the index remains valid because
  Vec reallocation preserves element order and index semantics. The span's
  `(offset, len)` pair still points to the correct byte range within the
  (possibly relocated) `bytes` buffer.

- `ExtrasRef` is a byte offset into the `blobs` Vec. Reallocation
  preserves byte order and offset semantics. An `ExtrasRef(128)` still
  addresses byte 128 in the (possibly relocated) buffer.

- `FileEntryHeader` values in the `headers` Vec are `Copy` and
  self-contained. Reallocation moves headers but their arena references
  are value types (u32 indices/offsets), not pointers.

**Conclusion: Vec reallocation does not invalidate any existing handle.**
This was identified in RSS-A.8.a finding F4 and is the foundation for
the recommended strategy.

## Upstream rsync's approach

Upstream rsync uses `lib/pool_alloc.c` - a chunk-based pool allocator.
Each `file_list` has its own pool from which `file_struct` nodes
(variable-size because of the flexible array basename tail) are
allocated. The pool grows by allocating new chunks from `malloc`:

```c
// upstream: lib/pool_alloc.c
// pool_alloc() bumps a pointer within the current chunk.
// When the current chunk is exhausted, a new chunk is malloc'd.
// Old chunks are never freed until pool_destroy().
```

Key properties of the upstream model:

- **No reallocation.** Individual nodes are never moved. The pool grows
  by adding new chunks, and `file_struct` pointers remain stable.
- **Per-flist lifetime.** With INC_RECURSE, each sub-list gets its own
  `file_list` with its own pool. When a sub-list is fully consumed, the
  entire pool is destroyed in one call (`pool_destroy`), freeing all
  nodes at once with no per-entry destructor.
- **The `files[]` pointer array.** A separate `realloc`-grown array of
  `file_struct*` pointers provides indexed and sorted access. This array
  can move, but the nodes it points to do not.

The oc-rsync flat-flist design doc (`flat-flist-representation.md`)
proposes a per-segment `FlatFileList` model that mirrors this: each
INC_RECURSE segment owns an independent `FlatFileList` with its own
`headers`, `PathArena`, and `ExtrasArena`, droppable as a unit.

However, the actual receiver implementation (RSS-A.8.a finding F1) uses
a **single contiguous** `Vec<FileEntry>` (or `DualFileList`) with
index-based segment boundaries (`ndx_segments: Vec<(usize, i32)>`), not
per-segment containers. This document evaluates growth strategies for
that single-container model, since the implementation must work with the
code as it exists today.

## Strategy evaluation

### Strategy A: Pre-allocate generously

Reserve a large initial capacity for all three backing stores based on
an estimate of the total file count.

**Mechanism:**
- At the start of a transfer, estimate total files (from the sender's
  hint or a heuristic like 2x the initial segment count).
- Call `Vec::with_capacity(estimate)` on `headers`, `bytes`, `spans`,
  and `blobs`.
- Subsequent pushes within capacity do not reallocate.

**Handle stability:** Trivially satisfied - no reallocation occurs as
long as the estimate holds. If the estimate is exceeded, Vec grows
normally with the same index stability as Strategy B.

**Tradeoffs:**

| Dimension | Assessment |
|-----------|------------|
| Memory overhead | High. Over-estimation wastes memory. A 2x estimate for 1M files wastes 48 MB in headers alone. Under-estimation gains nothing - Vec falls back to doubling. |
| Fragmentation | Low during growth (no reallocation churn). |
| Implementation complexity | Low - one `with_capacity` call per arena. |
| Upstream parity | Poor. Upstream does not pre-allocate; it grows chunk by chunk. |
| Predictability | Poor. File counts are often unknown before traversal, especially with INC_RECURSE where segments arrive incrementally. The sender does not communicate total count ahead of the sub-lists. |

**Verdict:** Not recommended as a primary strategy. The file count is
inherently unknown with INC_RECURSE. Pre-allocation is a useful
optimization hint (e.g., sizing PathArena for expected dirname
cardinality) but cannot serve as the growth strategy itself.

### Strategy B: Grow-on-demand with Vec (index-based handles)

Use the standard `Vec` growth policy (amortized doubling) for all three
backing stores. Handles are indices/offsets that survive reallocation.

**Mechanism:**
- `PathArena::intern()` pushes to `bytes` and `spans`. Vec doubles
  capacity when full.
- `ExtrasArena::append()` pushes to `blobs`. Same Vec growth.
- `FlatFileList::push()` pushes to `headers`. Same Vec growth.
- No special action needed on segment boundaries.

**Handle stability:** Inherently satisfied by the index-based handle
design. As analyzed above, `PathHandle` (span index) and `ExtrasRef`
(byte offset) are value types that do not reference memory addresses.
Vec reallocation changes the buffer address but not the logical
index/offset semantics.

**Tradeoffs:**

| Dimension | Assessment |
|-----------|------------|
| Memory overhead | Standard Vec overhead: up to 2x capacity vs used at any point. Amortized O(1) push. Identical to the current `Vec<FileEntry>` model. |
| Fragmentation | Moderate. Each doubling allocates a new buffer and copies, leaving the old buffer for the allocator to reclaim. Standard Vec behavior - well-understood and optimized by system allocators. |
| Implementation complexity | Minimal. No new data structures. The current PathArena and ExtrasArena already use this pattern. No code changes needed for growth behavior. |
| Upstream parity | Reasonable. Upstream uses chunk-based growth (no copying), but the practical outcome is similar: O(1) amortized append, bounded overhead. The Vec model trades copy cost for simpler layout and better cache locality. |
| Predictability | High. Vec growth is deterministic and well-characterized. No heuristics or estimates needed. |

**Verdict:** Recommended. The index-based handle design was specifically
chosen to tolerate Vec reallocation. This strategy requires zero new
code for growth behavior - it is what the current implementation already
does.

### Strategy C: Chain of fixed-size arena blocks

Allocate arenas as a linked list (or Vec) of fixed-size blocks. Handles
encode `(block_index, offset_within_block)`.

**Mechanism:**
- `PathArena` would store `blocks: Vec<Vec<u8>>` instead of a single
  `bytes: Vec<u8>`. When the current block fills, allocate a new one.
- Handles become `(u16 block_idx, u16 offset)` or similar compound
  encodings.
- Resolution: `blocks[handle.block_idx][handle.offset..handle.offset + len]`.

**Handle stability:** Fully stable. New blocks do not move old blocks.
The `blocks` Vec itself may reallocate, but that moves the block
pointers, not the block contents (blocks are heap-allocated via inner
Vec). Handles reference block contents via two-level indirection.

**Tradeoffs:**

| Dimension | Assessment |
|-----------|------------|
| Memory overhead | Low per-block waste (only the last block has unused capacity). But each block is a separate allocation with its own malloc header. |
| Fragmentation | Low. Blocks are never moved or copied after allocation. No reallocation churn. |
| Implementation complexity | High. Requires redesigning PathHandle and ExtrasRef to encode block coordinates. The dedup HashMap in PathArena needs to resolve handles through the block chain. Resolution becomes two-level indirection instead of one array index. Sorting comparators become slower (two lookups per resolve). |
| Upstream parity | Good. Mirrors upstream's `pool_alloc` chunk model. |
| Predictability | High. Growth is bounded per-block. |
| Handle encoding | The 4-byte handle budget is tight. Encoding both block index and offset in 32 bits limits either the block count or the per-block capacity. For example, 12-bit block index (4096 blocks) + 20-bit offset (1 MB per block) fits u32 but constrains the design. |

**Verdict:** Not recommended. The complexity cost is high and the
benefit (avoiding Vec copy on reallocation) is marginal. The Vec copy
cost is amortized O(1) and happens infrequently. The cache-friendly
contiguous layout of a single Vec outweighs the zero-copy advantage of
chained blocks for the access patterns in this codebase (sequential
scan during sort, random access during transfer). The 4-byte handle
encoding constraint adds fragility.

## Recommendation

**Strategy B: Grow-on-demand with Vec** is the recommended approach.

### Rationale

1. **Already working.** The current PathArena and ExtrasArena
   implementations use exactly this strategy. No new growth code is
   needed. The index-based handle types (PathHandle as span index,
   ExtrasRef as byte offset) were designed for this.

2. **Handle stability is inherent.** Unlike pointer-based handles that
   break on reallocation, index-based handles are value types that
   survive any number of Vec reallocations. This is the core insight
   from RSS-A.8.a finding F4.

3. **Minimal complexity.** No new data structures, no handle encoding
   changes, no two-level indirection. The implementation stays simple
   and auditable.

4. **Performance characteristics are well-understood.** Vec's amortized
   O(1) push with geometric growth is the standard Rust growth strategy.
   The copy cost during reallocation is bounded (each element is copied
   at most O(log N) times total across all reallocations). For the
   access patterns in this codebase - sequential scan during sort,
   random access during transfer - contiguous memory is optimal.

5. **Compatible with both receiver models.** Whether the receiver uses
   a single `Vec<FileEntry>` (current implementation) or per-segment
   `FlatFileList` containers (design doc proposal), Vec growth works
   identically. The strategy does not constrain the segment ownership
   model.

### Combining with pre-allocation hints

Strategy A (pre-allocation) is complementary as an optimization:

- `PathArena::with_capacity(estimated_unique_paths)` pre-sizes the span
  table and dedup map, reducing early reallocation churn. This already
  exists.
- `FlatFileList::with_capacity(estimated_entries)` pre-sizes the header
  Vec. This already exists.
- For the byte arenas (`PathArena::bytes`, `ExtrasArena::blobs`), a
  heuristic like `estimated_entries * avg_name_len` could pre-size the
  byte buffer. This is a minor optimization for a future task.

These hints reduce the number of reallocations but are not required for
correctness. If the estimate is wrong, Vec grows normally.

## Per-segment vs single-container model

The flat-flist design doc proposes per-segment `FlatFileList` ownership:
each INC_RECURSE segment gets its own `FlatFileList` with independent
arenas, droppable as a unit. This mirrors upstream's per-flist pool
destroy.

The current receiver implementation uses a single contiguous
`Vec<FileEntry>` with index-based segment boundaries. RSS-A.8.a
confirmed this in finding F1.

The recommended growth strategy (B) works with both models:

- **Single container:** One PathArena, one ExtrasArena, one headers Vec
  grow throughout the transfer. Handles from all segments share the same
  arena and remain valid. Segment boundaries are tracked by
  `ndx_segments` indices into the single array.

- **Per-segment containers:** Each segment's FlatFileList has its own
  arenas. Handles are only valid within their segment. No cross-segment
  handle comparison is needed (RSS-A.8.a finding, flat-flist design doc
  section "INC_RECURSE incremental segment growth").

The choice between these models is a separate design decision (RSS-A.8.c
scope). The growth strategy is orthogonal: Vec growth works in both.

## Design doc vs implementation gap

The flat-flist design doc describes per-segment FlatFileList ownership
with segment-scoped arenas. The current implementation uses a single
flat Vec across all segments. This gap is documented in RSS-A.8.a
findings F1 and F5. The migration path is:

1. **Phase 1 (current):** DualFileList pushes to both legacy Vec and
   FlatFileList. Growth is Vec-based. Single container.
2. **Phase 2 (RSS-A.8.c):** Add `sort_range()`, `headers_slice()`, and
   segment-aware accessors to FlatFileList so segment operations work
   on the single container.
3. **Phase 3 (optional, RSS-A.9+):** If per-segment ownership proves
   beneficial (memory reclaim, cache pressure), refactor to per-segment
   FlatFileList containers. The growth strategy remains Vec-based within
   each segment.

## Verification plan

The following properties should be tested when RSS-A.8.c implements
segment growth:

1. **Handle stability across segment appends.** Push entries in segment
   0, record their PathHandles and ExtrasRefs, then push segment 1
   entries. Verify all segment 0 handles still resolve correctly.

2. **Cross-segment deduplication.** Intern "README" in segment 0, intern
   "README" in segment 1 (same PathArena). Verify same handle returned
   (single-container model only).

3. **Sort within segment range.** Push entries for two segments, sort
   only the second segment's range. Verify segment 0 entries are
   untouched and segment 1 is correctly sorted.

4. **Large-scale growth.** Push 100K entries across 1K segments. Verify
   no handle corruption and O(1) amortized push cost.

## References

- `crates/protocol/src/flist/flat/intern.rs` - PathArena implementation
- `crates/protocol/src/flist/flat/extras.rs` - ExtrasArena implementation
- `crates/protocol/src/flist/flat/header.rs` - PathHandle, ExtrasRef types
- `crates/protocol/src/flist/flat/flist.rs` - FlatFileList container
- `crates/protocol/src/flist/dual.rs` - DualFileList migration wrapper
- `crates/transfer/src/receiver/file_list/receive.rs` - receiver segment
  append (`receive_extra_file_lists`)
- `docs/design/flat-flist-representation.md` - flat-flist design doc
  (INC_RECURSE section)
- `docs/design/rss-a8a-inc-recurse-segment-audit.md` - prior audit
- upstream: `lib/pool_alloc.c` - chunk-based pool allocator
- upstream: `flist.c:flist_new()`, `flist.c:send_extra_file_list()` -
  per-segment file list allocation
