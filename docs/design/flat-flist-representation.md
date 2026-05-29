# Flat flist backing-store representation

Task: RSS-A.4 (design). Branch: `docs/flat-flist-representation-design`.
Prerequisites: RSS-A.2 (FileEntry layout audit), RSS-8.a (PathHandle
type), RSS-8/9 (arena path migration). Downstream: RSS-A.5..A.11
(implementation), RSS-2 (allocation profiling, blocking validation).

## Summary

This document proposes a flat, contiguous backing store for the rsync
file list that matches upstream rsync's memory model: a fixed-size
header array plus variable-length tails packed in an arena, with a
separate sort index. The goal is to close the remaining resident-memory
(RSS) gap against upstream and reach the project target of under 10%
overhead at scale.

The current representation is a `Vec<FileEntry>` where `FileEntry` is
~96 bytes inline plus one to two heap allocations per entry for the
path, and an `Option<Box<FileEntryExtras>>` (224 bytes when populated)
for symlink targets, device numbers, hardlink data, ACL/xattr indices,
checksums, and user/group names. The RSS-A.2 audit measured a 25.9x RSS
gap at 1 million files (197 MB versus 7.6 MB for upstream) and a 2.7x
structural overhead in the common case (~160 bytes per entry versus
~60 bytes upstream).

The PathHandle/PathArena work (RSS-8/9) already attacks the path
allocations by replacing `name: PathBuf` and `dirname: Arc<Path>` with
4-byte interned handles. The flat flist design described here is the
next, more fundamental step: it removes the per-entry `Vec` and `Box`
overhead entirely by storing all entries in one contiguous buffer and
all variable-length data in a shared arena, leaving the per-entry node
at a fixed 48-64 bytes with zero per-entry heap allocations.

This is a design-only document. The projected savings below are
structural estimates derived from the RSS-A.2 audit. They MUST be
validated against allocation profiling (RSS-2) before any of the
RSS-A.5..A.11 implementation steps land. See "Validation gate".

## Problem statement

### Current representation

```
FileList
├── entries: Vec<FileEntry>          (96 B inline per entry)
│       ├── name: PathBuf / PathHandle
│       ├── dirname: Arc<Path> / PathHandle
│       ├── size, mtime, uid, gid, mode, flags, content_dir (inline)
│       └── extras: Option<Box<FileEntryExtras>>  (224 B heap when set)
└── interner: PathInterner            (shared dirname/name strings)
```

Cost contributors identified by RSS-A.2, ranked:

1. `Vec<FileEntry>` capacity slack. `Vec` doubles capacity on growth,
   so a 1M-entry list can reserve space for up to 2M entries, wasting
   up to 50% of the inline array.
2. Per-allocation malloc metadata (~16 bytes per allocation on
   glibc/jemalloc) and small-allocation fragmentation. Each `PathBuf`
   name and each `Box<FileEntryExtras>` carries this tax.
3. The `Option<Box<FileEntryExtras>>` shape: 224 bytes plus a malloc
   header whenever any rarely-used field is set, even if only one of
   the sixteen fields is populated. The boxed block itself contains
   five `Option<{PathBuf,String,Vec<u8>,XattrList}>` fields, each of
   which triggers a further heap allocation when populated.
4. Eight `Option<u32>`/`Option<i64>` fields in the extras block that
   waste a full word on the discriminant rather than using a presence
   bitfield.

PathHandle (RSS-8) addresses contributor 2 for paths. RSS-A.3 (compact
Option fields) addresses contributor 4. This design addresses
contributors 1 and 3, and unifies the result into one contiguous store.

### Upstream representation

Upstream rsync stores the file list as a pool-allocated, contiguous
array of fixed-size `file_struct` nodes:

```c
struct file_struct {              // upstream: rsync.h, struct file_struct
    const char *dirname;          //  8 B - shared pointer into a string pool
    time_t modtime;               //  8 B - mtime
    uint32 len32;                 //  4 B - low 32 bits of size
    uint16 mode;                  //  2 B - type + permissions
    uint16 flags;                 //  2 B - FLAG_* bits
    const char basename[];        //  0 B - flexible array member, inline tail
};                                // = 24 B fixed (FILE_STRUCT_LEN)
```

Two upstream techniques are central:

- Variable tail packing. The `basename` flexible array member stores
  the file name inline immediately after the struct - no separate
  allocation. (upstream: `flist.c:make_file()`, `flist.c:f_name()`.)
- Conditional `union file_extras`. Optional metadata (uid, gid, device
  numbers, ACL/xattr indices) is stored as 4-byte `union file_extras`
  slots prepended before the `file_struct` pointer in the same
  allocation, gated on global config flags (`uid_ndx`, `gid_ndx`,
  `acls_ndx`, `xattrs_ndx`). Absent fields cost zero bytes.
  (upstream: `rsync.h` `union file_extras`, `flist.c:make_file()`.)

The nodes are allocated from a pool (upstream: `lib/pool_alloc.c`) and
freed as a unit (`pool_destroy()`), so there is no per-entry destructor
cost and no per-entry malloc header.

For a typical transfer with uid + gid preservation, upstream spends
~52 bytes per allocation plus an 8-byte `files[]` pointer, for ~60
bytes per entry total. oc-rsync's common-case ~160 bytes is 2.7x that,
and the measured 25.9x RSS figure layers `Vec` slack, malloc metadata,
fragmentation, and ancillary structures on top.

## Proposed design

### Three components

```
FlatFileList
├── headers: Vec<FileEntryHeader>   (fixed 48-64 B, contiguous, never reordered)
├── arena: StringArena              (all variable-length bytes: names, dirnames,
│                                     symlink targets, xattr/ACL/checksum blobs)
└── index: Vec<u32>                 (sorted permutation of header indices)
```

Key principle: **sort the index, not the entries.** Headers are
allocated in build order and never move. Ordering, deduplication, and
filtering operate on the `index: Vec<u32>` permutation. This keeps
header offsets stable so that any side table keyed by header index
(hardlink map, NDX mapping, delta scheduling) stays valid across sorts,
and it makes a sort move 4 bytes per entry instead of 48-64.

### FileEntryHeader

A fixed-size, `Copy`, `repr(C)`-considered struct holding only inline
scalars and arena references. No `PathBuf`, no `Box`, no `Option<T>`
with discriminant words. Target 48-64 bytes.

```rust
/// Fixed-size header for one file-list entry in the flat backing store.
///
/// Holds inline scalar metadata plus arena references to variable-length
/// tails (name, dirname, and optional extras blob). Never moved after
/// insertion; sort order is expressed through `FlatFileList::index`.
#[derive(Clone, Copy)]
pub struct FileEntryHeader {
    /// Modification time, seconds since Unix epoch.
    mtime: i64,
    /// File size in bytes (0 for directories and special files).
    size: u64,
    /// User ID; meaningful only when the `UID` presence bit is set.
    uid: u32,
    /// Group ID; meaningful only when the `GID` presence bit is set.
    gid: u32,
    /// Interned name handle (4-byte PathHandle / Spur).
    name: PathHandle,
    /// Interned dirname handle (4-byte PathHandle / Spur).
    dirname: PathHandle,
    /// Arena offset of the packed extras tail, or `NO_EXTRAS` sentinel
    /// when the entry has no rarely-used fields.
    extras: ExtrasRef,
    /// Modification time nanoseconds (protocol 31+).
    mtime_nsec: u32,
    /// Unix mode bits (type + permissions).
    mode: u32,
    /// Wire flags (FileFlags packed into u16).
    flags: u16,
    /// Presence bitfield: which optional inline fields are set
    /// (uid, gid, mtime_nsec, content_dir, length64, ...).
    present: u16,
}
```

Sizing rationale (64-bit target): 8 (mtime) + 8 (size) + 4 (uid) +
4 (gid) + 4 (name) + 4 (dirname) + 4 (extras) + 4 (mtime_nsec) +
4 (mode) + 2 (flags) + 2 (present) = 48 bytes, no tail padding. This
hits the low end of the 48-64 byte target and is within ~2x of
upstream's 24-byte `file_struct`, with the remainder explained by
oc-rsync carrying full 64-bit size and i64 mtime inline rather than
upstream's 32-bit `len32` plus conditional extra. Promoting size to a
conditional arena field (upstream's `FLAG_LENGTH64` approach) is an
optional follow-up tracked as an open question; it would trade an
inline word for a presence bit.

The `present` bitfield replaces every `Option<u32>` discriminant word.
A field such as `uid` is read as `Some(uid)` only when
`present & UID_BIT != 0`, eliminating the 4-byte-per-Option waste the
RSS-A.2 audit flagged (contributor 4) without an enum discriminant.

### StringArena (extends existing arena work)

The flat store does not introduce a new arena. It extends the
`PathArena` / `StringArena` already specified in RSS-8.a, which today
backs `PathHandle`-based `name` and `dirname` storage via a `lasso`
interner (`Rodeo` while building, `RodeoReader` once frozen). Names and
dirnames continue to be interned exactly as RSS-8 defines, so the flat
store inherits dirname deduplication and basename deduplication for
free.

The extension: the arena gains a non-interned blob region for the
variable-length extras tail (symlink target, xattr/ACL/checksum bytes,
optional user/group names). Interning is the wrong tool for these -
they are mostly unique and some are binary - so they are appended to a
growable byte region and referenced by `(offset, len)`:

```rust
pub struct StringArena {
    /// Interned path strings (names, dirnames) - RSS-8 lasso interner.
    paths: PathArena,
    /// Append-only byte region for non-interned variable tails
    /// (symlink targets, xattr/ACL/checksum blobs, user/group names).
    blobs: Vec<u8>,
}
```

This keeps the RSS-8 contract intact: `PathHandle` resolution is
unchanged, the build-then-freeze lifecycle is unchanged, and the drop
semantics remain a single bulk free (the `blobs` `Vec` and the interner
chunks free in O(chunks), mirroring upstream `pool_destroy()`).

### Offset-based indexing scheme

A header references its variable tail through two reference kinds:

- `PathHandle` for `name` and `dirname`: a 4-byte `Spur` indexed into
  the interner. Resolution is `arena.paths.resolve(handle) -> &str`
  (RSS-8.a API), an O(1) indexed read with no hash lookup once frozen.
- `ExtrasRef` for the optional extras tail: a 4-byte arena offset (or
  the `NO_EXTRAS` sentinel) pointing into `arena.blobs`. The extras
  tail is a length-prefixed, self-describing record: a 2-byte presence
  mask of which extras fields are present, followed by the present
  fields in a fixed canonical order, each length-prefixed where
  variable.

```
arena.blobs (extras tail at offset E):
  [E+0]  u16 extras_present   (LINK_TARGET | RDEV | HARDLINK | ACL | XATTR | CKSUM | ...)
  [E+2]  fields in canonical order, present ones only:
         link_target:  u16 len, then len bytes
         rdev:         u32 major, u32 minor
         hardlink:     u32 idx  (or u64 dev + u64 ino for proto < 30)
         acl_ndx, def_acl_ndx, xattr_ndx: u32 each
         checksum:     u8 len, then len bytes (<= 32)
         user_name, group_name: u16 len, then len bytes
         atime/crtime/atime_nsec: i64 / i64 / u32
```

A reader reconstructs a transient `FileEntryExtras` view (or, more
efficiently, reads individual fields on demand) by seeking to
`ExtrasRef`, reading the presence mask, and walking the present fields.
Because the tail is written once at build time and never mutated,
offsets are stable for the life of the arena.

Resolution helpers live on the flat store, not on the header (the
header has no arena in scope), matching the RSS-8.a accessor-with-arena
pattern:

```rust
impl FlatFileList {
    pub fn name(&self, ndx: u32) -> &str;            // resolves PathHandle
    pub fn dirname(&self, ndx: u32) -> &Path;        // resolves PathHandle
    pub fn link_target(&self, ndx: u32) -> Option<&Path>;  // walks extras tail
    pub fn checksum(&self, ndx: u32) -> Option<&[u8]>;
    // ... one accessor per extras field, all reading from the arena
}
```

### Sort-the-index discipline

`index: Vec<u32>` is the only structure reordered by sort, dedup, and
filter. Implications:

- Sorting calls the existing comparator (`compare_file_entries`,
  `name_cmp`) but operates on `index` slots, resolving each slot's
  `name`/`dirname` through the arena. This is the same resolve cost
  RSS-9.a already accepts for the PathHandle migration; the only change
  is that the comparator indexes through `index[i]` into `headers`.
- Deduplication (`flist_clean`) marks duplicate header indices in a
  side bitset rather than removing entries, then drops them from
  `index`. Headers themselves are never removed, preserving offset
  stability.
- Filtering sets an "excluded" presence bit or omits the index from a
  filtered view, again without moving headers. This matches upstream,
  where excluded entries stay in `flist` and are skipped by NDX.

## Integration with existing PathHandle/StringArena work

The flat flist is a strict superset of the RSS-8/9 arena migration, not
a parallel design:

- RSS-8 already replaced `name`/`dirname` with `PathHandle`. The flat
  header stores those same handles. No second name-storage scheme.
- RSS-9 already threads `&PathArena` (the interner) through every sort,
  filter, and transfer consumer. The flat store reuses those call
  sites verbatim; consumers receive `&FlatFileList` (which owns the
  arena) instead of `(&[FileEntry], &PathArena)`, a mechanical change.
- The build-then-freeze lifecycle, drop-as-bulk-free semantics, and
  per-segment arena ownership from RSS-8.a carry over unchanged.

The only new arena surface is the `blobs` byte region for extras tails,
which did not exist in RSS-8 because RSS-8 scoped itself to paths. The
extras compaction recommended in RSS-A.12 (replace `Option<u32>`/
`Option<i64>` extras fields with raw values plus a presence mask) is
realized naturally by the extras-tail layout above: the presence mask
in the tail is exactly the RSS-A.12 bitfield.

## INC_RECURSE incremental segment growth

Upstream's incremental recursion sends the file list as multiple
segments (sub-lists), one per directory, each with its own NDX range
(upstream: `flist.c:flist_new()`, `flist.c:send_extra_file_list()`).
oc-rsync mirrors this with `FileListSegment` / `SegmentedFileList`
(`crates/protocol/src/flist/segment.rs`), each segment holding its own
`Vec<FileEntry>` and `ndx_start`.

In the flat model each segment owns an independent `FlatFileList`:

```
SegmentedFlatFileList
├── Segment 0: FlatFileList { headers, arena, index }  (ndx_start = 1)
├── Segment 1: FlatFileList { headers, arena, index }  (ndx_start = N0 + 1)
└── Segment K: FlatFileList { headers, arena, index }  (active)
```

This preserves the existing per-segment lifetime and bounds memory:

- Growth within a segment is amortized `Vec::push` onto `headers` and
  `extend` onto `arena.blobs` - no reallocation of prior entries' tails
  because tails are append-only and referenced by stable offset.
- A new segment appends a fresh `FlatFileList`; the global NDX continues
  from the prior segment's `ndx_end()`, exactly as today.
- `index` is per-segment. Segments are independent sort/compare/transfer
  domains (RSS-8.a establishes that cross-segment handle comparison is
  never needed), so no global index is required.
- When a segment is fully consumed, its `FlatFileList` is dropped,
  freeing its `headers` `Vec`, its `arena.blobs`, and its interner
  chunks in one pass - matching upstream's per-flist pool destroy.

Non-INC_RECURSE transfers are the degenerate single-segment case: one
`FlatFileList` for the whole list.

## Migration phasing (RSS-A.5..A.11)

This design feeds the existing RSS-A implementation tracker. The phasing
below maps the design onto those task slots; exact slot assignments are
finalized in the tracker.

1. **Build the store behind an accessor facade (RSS-A.5).** Introduce
   `FlatFileList`, `FileEntryHeader`, and the `blobs` arena extension.
   The file-list builder (`read`/walker path) emits headers and arena
   tails. Expose accessor methods (`name`, `dirname`, `link_target`,
   `checksum`, ...) so consumers read through one API. Keep the old
   `Vec<FileEntry>` path compiling in parallel behind a build flag.
2. **Migrate the sort consumer (RSS-A.6).** Point `sort_file_list`,
   `compare_file_entries`, `name_cmp`, and `flist_clean` at the
   `index: Vec<u32>` permutation. Reuse the RSS-9.a arena-threaded
   comparators. Verify byte-identical sort order against golden tests.
3. **Migrate the filter consumer (RSS-A.7).** Filter evaluation reads
   `name`/`dirname` via the flat accessors and marks exclusions in the
   presence/index structures rather than mutating a `Vec`.
4. **Migrate the transfer/wire consumer (RSS-A.8).** The wire encoder
   reads name bytes and extras from the arena; the receiver decodes
   directly into headers + arena tails (no intermediate `FileEntry`).
5. **Migrate engine consumers (RSS-A.9).** Delete pipeline, hardlink
   table, and NDX mapping switch to header-index keys (stable because
   headers never move).
6. **Remove the legacy `Vec<FileEntry>` path (RSS-A.10).** Delete the
   old representation and its build flag once all consumers are flat.
   Tighten the `size_of::<FileEntryHeader>()` assertion to the chosen
   target (48-64 B).
7. **Benchmark and validate (RSS-A.11).** Re-run the 1M-file RSS
   benchmark (`crates/protocol/benches/flist_rss_fixture.rs`,
   `docs/design/flist-memory-benchmark-plan.md`) and the allocation
   profile (RSS-2). Confirm the projected reduction and the under-10%
   target, or revise.

Each step keeps golden wire-format tests green and shrinks measured RSS
monotonically, matching the RSS-8 sequencing discipline.

## Projected savings (UNVALIDATED - see validation gate)

Structural estimate at 1M files, common case (regular files, uid+gid,
no extras), building on the RSS-A.2 audit numbers:

| Component | Current | Flat store | Note |
|---|---:|---:|---|
| Per-entry inline node | 96 B | 48 B | header replaces FileEntry |
| `Vec` capacity slack | up to 50% | up to 50% of 48 B | smaller base shrinks slack |
| Name heap (per entry) | ~46 B | 0 (arena, dedup) | already removed by RSS-8 |
| Dirname heap | ~0 (shared) | 0 | unchanged |
| Extras malloc header | 16 B when set | 0 | tail packed in arena |
| Per-entry malloc metadata | ~16-32 B | 0 | no per-entry alloc |

These figures are derived from struct sizes, not measured. The 25.9x
gap in RSS-A.2 exceeded the calculated 2.7x structural overhead because
of `Vec` slack, malloc metadata, fragmentation, and ancillary
structures (sort index, filter chain, hardlink map, cached
`fs::Metadata`). The flat store removes the per-entry malloc metadata
and fragmentation and shrinks the inline node, but the realized
reduction depends on those ancillary sources, which only profiling can
quantify.

## Backward compatibility and public-API impact

- Scope is the `protocol` crate's `flist` module plus its direct
  consumers in `transfer`, `engine`, and `core`. There are no external
  crate consumers, so this is a workspace-internal refactor with no
  semver implications, consistent with the RSS-8 assessment.
- Wire format is unchanged. The flat store is purely an in-memory
  representation; encode/decode produce byte-identical wire output.
  Golden wire-format tests (`crates/protocol/tests/golden_*`) are the
  binding contract and must stay green at every step.
- The public `FileEntry` type's accessor methods change shape (they
  already changed in RSS-8 to take `&PathArena`). Under the flat store
  they move onto `FlatFileList` and take a header index. A thin
  `FileEntry`-shaped view can be retained during migration so consumers
  convert incrementally.
- `FileListSegment::entries: Vec<FileEntry>` becomes a per-segment
  `FlatFileList`. `SegmentedFileList` accessors (global NDX lookup)
  keep their signatures.

## Rollback plan

- Phases 1-5 keep the legacy `Vec<FileEntry>` path compiling behind a
  build flag. If profiling (RSS-A.11 / RSS-2) shows the flat store does
  not deliver the projected reduction, or surfaces a correctness or
  performance regression, revert to the legacy path by flipping the
  flag; no wire or on-disk format changed, so rollback is local.
- The legacy path is only deleted in RSS-A.10, after the flat store has
  passed golden tests, interop, and the RSS benchmark. RSS-A.10 is the
  point of no easy return and must be gated on the validation in
  RSS-A.11 completing first.
- Per-segment arena ownership means a partial rollout (e.g. flat store
  for the receiver, legacy for the sender) is possible if one consumer
  proves problematic, since segments do not share representation across
  the wire.

## Validation gate

The projected RSS savings in this document are structural estimates and
MUST be validated against allocation profiling before RSS-A.5
implementation begins in earnest, and again before RSS-A.10 deletes the
legacy path. Required evidence (RSS-2, still pending, runs in the Linux
benchmark container):

- heaptrack or massif peak-RSS and allocation-count comparison, flat
  store versus legacy, at 100K and 1M files.
- dhat allocation profile confirming zero per-entry heap allocations in
  the common case and bounded arena growth.
- The 1M-file RSS benchmark (`flist_rss_fixture.rs`) showing the gap
  against upstream closing toward the under-10% target.

If profiling does not corroborate the estimates, the design is revised
or the flat store is not adopted. No implementation step past RSS-A.5
should proceed on the strength of the estimates alone.

## Open questions

1. **Inline size versus conditional size.** Should `size` stay an
   inline `u64` (48-byte header) or move to a conditional arena field
   following upstream's `len32` + `FLAG_LENGTH64` split? The latter
   saves 4 bytes inline for the > 99% of files under 4 GB but adds an
   arena read for large files. Decide with the RSS-A.11 profile.
2. **Mode width.** Upstream uses `uint16` for mode. oc-rsync stores
   `u32`. Narrowing to `u16` saves 2 bytes inline but must be verified
   against upstream parity for any upper mode bits oc-rsync relies on.
3. **Extras tail read ergonomics.** Whether consumers read individual
   extras fields on demand (cheapest) or reconstruct a transient
   `FileEntryExtras` view (most compatible with current call sites).
   Likely both: a view for migration, on-demand reads for hot paths.
4. **Arena blob fragmentation.** Whether the append-only `blobs` region
   needs alignment padding for the `u32`/`i64` extras fields, and
   whether unaligned reads are acceptable on all target architectures.
5. **`index` materialization cost.** Whether to keep a single
   `Vec<u32>` index or multiple views (sorted, filtered, transfer
   order); profiling should confirm the index does not reintroduce
   meaningful overhead at 1M entries.

## Cross-references

- `docs/audit/file-entry-layout-audit.md` - RSS-A.2 layout audit and
  per-entry overhead numbers cited here.
- `docs/design/rss-8a-arena-handle-type.md` - PathHandle / PathArena
  definition and build-then-freeze lifecycle this design extends.
- `docs/design/rss-9a-sort-consumer-pathhandle-migration.md` - arena
  threading through sort consumers, reused by RSS-A.6.
- `docs/design/flist-memory-benchmark-plan.md` - 1M-file RSS benchmark
  used for RSS-A.11 validation.
- `crates/protocol/src/flist/entry/core.rs` - current `FileEntry`.
- `crates/protocol/src/flist/entry/extras.rs` - current
  `FileEntryExtras`.
- `crates/protocol/src/flist/segment.rs` - `FileListSegment` /
  `SegmentedFileList` for INC_RECURSE.
- `crates/protocol/src/flist/intern.rs` - current `PathInterner`.
- `crates/protocol/benches/flist_rss_fixture.rs` - RSS benchmark
  fixture.
- upstream: `flist.c` (`make_file`, `f_name`, `flist_new`,
  `send_extra_file_list`), `rsync.h` (`struct file_struct`,
  `union file_extras`), `lib/pool_alloc.c` (pool allocate / destroy).
  Consult these for exact contiguous-layout and bulk-free semantics;
  the upstream tree was not present locally at authoring time, so the
  `file_struct` definition above is quoted from the RSS-A.2 audit.
