# Flat flist backing-store representation

Task: RSS-A.4 (design). Branch: `docs/flat-flist-representation-design`.
Prerequisites: RSS-A.2 (FileEntry layout audit). Downstream: RSS-A.5..A.11
(implementation), RSS-2 (allocation profiling, blocking validation),
RSS-A.5.c (build the 4-byte interner handle this design needs).

Premise correction (RSS-A.0.c): an earlier draft of this document assumed
the RSS-8/RSS-9 arena path migration had already landed and that a 4-byte
`PathHandle` (backed by `PathArena` / `StringArena` and a `lasso`
interner) was in production. The RSS-A.0.a audit
(`docs/audit/arena-prototype-landing-gap.md`) verified against the tree
that this is false. No `PathHandle`, `PathArena`, or `StringArena` type
exists anywhere in the workspace. The production `FileEntry` still stores
`name: PathBuf` and `dirname: Arc<Path>`, and the only landed dedup is the
`Arc<Path>` dirname `PathInterner` (`crates/protocol/src/flist/intern.rs`).
The only arena code that landed was an unused `bumpalo` prototype, removed
in RSS-A.0.b. This document therefore treats the 4-byte interner handle as
something the flat-store effort must build from scratch (RSS-A.5.c), not as
a prerequisite that already exists.

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

The path allocations are only partly addressed today. The dirname
`PathInterner` (`crates/protocol/src/flist/intern.rs`) deduplicates
directory names by sharing one `Arc<Path>` per unique directory, but
`name` is still a per-entry `PathBuf` and dirname references still cost a
pointer-sized `Arc<Path>` each, not a 4-byte handle. The flat flist
design described here goes further: it stores all entries in one
contiguous buffer and all variable-length data in a shared arena,
introduces its own 4-byte interner handle (the `PathHandle` type that
RSS-A.5.c must build), and removes the per-entry `Vec` and `Box` overhead
entirely, leaving the per-entry node at a fixed 48-64 bytes with zero
per-entry heap allocations.

This is a design-only document. The projected savings below are
structural estimates derived from the RSS-A.2 audit. They MUST be
validated against allocation profiling (RSS-2) before any of the
RSS-A.5..A.11 implementation steps land. See "Validation gate".

## Problem statement

### Current representation

```
FileList
├── entries: Vec<FileEntry>          (96 B inline per entry)
│       ├── name: PathBuf                          (per-entry heap allocation)
│       ├── dirname: Arc<Path>                     (shared via PathInterner)
│       ├── size, mtime, uid, gid, mode, flags, content_dir (inline)
│       └── extras: Option<Box<FileEntryExtras>>  (224 B heap when set)
└── interner: PathInterner            (HashMap<PathBuf, Arc<Path>>, dirnames only)
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

The dirname `PathInterner` partly addresses contributor 2 by sharing one
`Arc<Path>` per directory, but it leaves each entry's `name: PathBuf`
allocation and each `Box<FileEntryExtras>` allocation in place, and it
hands back a pointer-sized `Arc<Path>` rather than a 4-byte handle. The
4-byte name/dirname interner that would fully address contributor 2 does
not exist yet; this design builds it (RSS-A.5.c). RSS-A.3 (compact Option
fields) addresses contributor 4. This design addresses contributors 1 and
3, builds the 4-byte path handle for contributor 2, and unifies the result
into one contiguous store.

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

The header references names and dirnames through its own concrete 4-byte
interner handle. That handle type does not exist today (see the premise
correction above and `docs/audit/arena-prototype-landing-gap.md`); this
design defines it and the flat-store effort must build it from scratch as
task RSS-A.5.c. The field type is written here as `PathHandle` to name the
to-be-built type, not to reuse a pre-existing one. Concretely it is a
4-byte index (`u32` newtype) into the flat store's own name/dirname
interner, with `u32::MAX` reserved as a null/empty sentinel.

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
    /// Interned name handle. `PathHandle` is a 4-byte `u32` index into
    /// the flat store's own name interner, built by RSS-A.5.c; it does
    /// not exist in the tree today.
    name: PathHandle,
    /// Interned dirname handle. Same to-be-built 4-byte `PathHandle`
    /// type as `name`, indexed into the same interner.
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

### StringArena (built by this effort, not pre-existing)

The flat store introduces a new arena; there is no `PathArena` or
`StringArena` in the tree to extend. RSS-A.5.c must build a path interner
(`PathArena`) that maps each unique name/dirname string to a 4-byte
`PathHandle`, plus the `StringArena` wrapper below. A `lasso` interner
(`Rodeo` while building, `RodeoReader` once frozen) is the recommended
backing, giving O(1) indexed resolution once frozen; the exact crate is an
implementation choice for RSS-A.5.c. Interning names and dirnames this way
gives the flat store both dirname deduplication (which the existing
`Arc<Path>` `PathInterner` already provides) and basename deduplication
(which it does not), and shrinks each reference from a pointer-sized
`Arc<Path>` to a 4-byte handle.

The second part: the arena gains a non-interned blob region for the
variable-length extras tail (symlink target, xattr/ACL/checksum bytes,
optional user/group names). Interning is the wrong tool for these -
they are mostly unique and some are binary - so they are appended to a
growable byte region and referenced by `(offset, len)`:

```rust
pub struct StringArena {
    /// Interned path strings (names, dirnames). Built by RSS-A.5.c;
    /// resolves a 4-byte PathHandle to a string in O(1) once frozen.
    paths: PathArena,
    /// Append-only byte region for non-interned variable tails
    /// (symlink targets, xattr/ACL/checksum blobs, user/group names).
    blobs: Vec<u8>,
}
```

The intended contract: `PathHandle` resolution is an indexed read, the
arena follows a build-then-freeze lifecycle (mutable interner while the
file list is being built, immutable reader once frozen), and the drop
semantics are a single bulk free (the `blobs` `Vec` and the interner
chunks free in O(chunks), mirroring upstream `pool_destroy()`).

### Offset-based indexing scheme

A header references its variable tail through two reference kinds:

- `PathHandle` for `name` and `dirname`: a 4-byte handle indexed into
  the interner that RSS-A.5.c builds. Resolution is
  `arena.paths.resolve(handle) -> &str`, an O(1) indexed read with no
  hash lookup once frozen.
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
header has no arena in scope), since the header holds only handles and
offsets and needs the arena to resolve them:

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
  `name`/`dirname` through the arena. The comparator gains an
  arena-resolve step (handle to `&str`) and indexes through `index[i]`
  into `headers`; this resolve cost is new work the flat store accepts in
  exchange for moving 4-byte indices instead of 48-64-byte headers.
- Deduplication (`flist_clean`) marks duplicate header indices in a
  side bitset rather than removing entries, then drops them from
  `index`. Headers themselves are never removed, preserving offset
  stability.
- Filtering sets an "excluded" presence bit or omits the index from a
  filtered view, again without moving headers. This matches upstream,
  where excluded entries stay in `flist` and are skipped by NDX.

## Relationship to the existing arena prototype and dirname interner

There is no shipped `PathHandle`/`PathArena` migration to build on. The
RSS-A.0.a audit (`docs/audit/arena-prototype-landing-gap.md`) verified the
actual landed state, which the flat store relates to as follows:

- The production `FileEntry` still uses `name: PathBuf` and
  `dirname: Arc<Path>` plus `extras: Option<Box<FileEntryExtras>>`. The
  flat store replaces all three: names and dirnames become 4-byte handles
  into a new interner (RSS-A.5.c), and extras become a packed arena tail.
- The only landed deduplication is the `Arc<Path>` dirname `PathInterner`
  (`crates/protocol/src/flist/intern.rs`, a `HashMap<PathBuf, Arc<Path>>`).
  It dedups dirnames but not basenames, and it returns a pointer-sized
  `Arc<Path>`, not a 4-byte handle. The flat store's interner subsumes it:
  same dirname deduplication, plus basename deduplication, plus a 4-byte
  reference. Once the flat path is live the `Arc<Path>` interner is
  redundant and can be retired.
- A `bumpalo` arena prototype (`ArenaFileEntry` / `ArenaFileEntryBuilder` /
  `FilePath` in `crates/protocol/src/flist/entry/arena.rs`) was the only
  arena code that landed. It had no production caller and was removed in
  RSS-A.0.b. It is not a foundation this design extends; the flat store is
  a fresh build.
- There is therefore no pre-existing build-then-freeze lifecycle,
  drop-as-bulk-free arena, or `&PathArena`-threaded consumer to reuse.
  RSS-A.5.c builds the interner and its lifecycle; RSS-A.6..A.9 thread
  `&FlatFileList` (which owns the arena) through the sort, filter,
  transfer, and engine consumers, replacing today's `&[FileEntry]` plus
  `&PathInterner` call sites.

The `blobs` byte region for extras tails is likewise new. The extras
compaction recommended in RSS-A.12 (replace `Option<u32>`/`Option<i64>`
extras fields with raw values plus a presence mask) is realized naturally
by the extras-tail layout above: the presence mask in the tail is exactly
the RSS-A.12 bitfield.

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
  domains - cross-segment handle comparison is never needed because each
  segment carries its own interner and NDX range - so no global index is
  required. (This mirrors today's per-segment `Vec<FileEntry>` ownership
  in `SegmentedFileList`.)
- When a segment is fully consumed, its `FlatFileList` is dropped,
  freeing its `headers` `Vec`, its `arena.blobs`, and its interner
  chunks in one pass - matching upstream's per-flist pool destroy.

Non-INC_RECURSE transfers are the degenerate single-segment case: one
`FlatFileList` for the whole list.

## Migration phasing (RSS-A.5..A.11)

This design feeds the existing RSS-A implementation tracker. The phasing
below maps the design onto those task slots; exact slot assignments are
finalized in the tracker.

1. **Build the store behind an accessor facade (RSS-A.5).** First build
   the 4-byte name/dirname interner and its `PathHandle` type (RSS-A.5.c),
   since none exists today. Then introduce `FlatFileList`,
   `FileEntryHeader`, and the `blobs` arena region. The file-list builder
   (`read`/walker path) emits headers and arena tails. Expose accessor
   methods (`name`, `dirname`, `link_target`, `checksum`, ...) so consumers
   read through one API. Keep the old `Vec<FileEntry>` path compiling in
   parallel behind a build flag.
2. **Migrate the sort consumer (RSS-A.6).** Point `sort_file_list`,
   `compare_file_entries`, `name_cmp`, and `flist_clean` at the
   `index: Vec<u32>` permutation. Extend the comparators to resolve each
   slot's handle through the arena (new work - there is no arena-threaded
   comparator to reuse). Verify byte-identical sort order against golden
   tests.
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
| Name heap (per entry) | ~46 B | 0 (arena, dedup) | removed by the new interner (RSS-A.5.c) |
| Dirname heap | ~0 (shared) | 0 | `Arc<Path>` interner today; 4-byte handle in flat store |
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
  semver implications.
- Wire format is unchanged. The flat store is purely an in-memory
  representation; encode/decode produce byte-identical wire output.
  Golden wire-format tests (`crates/protocol/tests/golden_*`) are the
  binding contract and must stay green at every step.
- The public `FileEntry` type's accessor methods change shape. Today they
  return owned/`Arc` path data directly from the struct; under the flat
  store they move onto `FlatFileList` and take a header index, resolving
  through the arena. A thin `FileEntry`-shaped view can be retained during
  migration so consumers convert incrementally.
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

## Upstream-grounded layout resolution (RSS-A.5.f)

This section resolves the open questions below against the real upstream
`struct file_struct` and `union file_extras` layout, read from the local
upstream tree (`target/interop/upstream-src/rsync-3.4.1/`). It supersedes
the earlier note that the `file_struct` quote came from the RSS-A.2 audit
because the tree was absent; the definition is now confirmed first-hand.
Every citation below is a file:line that was read directly.

### Confirmed upstream field set and sizes

The authoritative `file_struct` (upstream: `rsync.h:801-812`,
`#[derive]`-free C) is 24 bytes fixed on a 64-bit target:

```c
struct file_struct {       // upstream: rsync.h:801
    const char *dirname;   //  8 B  - rsync.h:802, shared pointer (lastdir)
    time_t modtime;        //  8 B  - rsync.h:803
    uint32 len32;          //  4 B  - rsync.h:804, low 32 bits of length
    uint16 mode;           //  2 B  - rsync.h:805, type + permissions
    uint16 flags;          //  2 B  - rsync.h:806, FLAG_* bits
    const char basename[]; //  0 B  - rsync.h:808, flexible array tail
};                         // = 24 B = FILE_STRUCT_LEN (rsync.h:827)
```

The earlier draft's quoted definition matches the real source exactly.
The `FileEntryHeader` field set is a faithful superset: it carries
`dirname`, `mtime`, `size`, `mode`, `flags`, and a name reference, and
adds inline `uid`/`gid`/`mtime_nsec`/`extras`/`present` fields that
upstream keeps as conditional 4-byte extras rather than inline. The flat
header trades upstream's per-allocation conditional extras for a few
always-present inline words plus a `present` bitfield; this is the
deliberate "wider node, simpler access" choice, kept within 48 bytes
(2x upstream's 24, not 6.7x like the legacy `FileEntry`).

### How upstream packs variable extras, and how `ExtrasRef` mirrors it

Upstream does not store optional metadata inline. Each optional field is a
4-byte `union file_extras { int32 num; uint32 unum; const char* ptr; }`
slot (upstream: `rsync.h:786-792`, `EXTRA_LEN = sizeof(union file_extras)`
at `rsync.h:831`). The slots are allocated **before** the `file_struct`
pointer in the same block and addressed by negative index:
`REQ_EXTRA(f,ndx) = (union file_extras*)(f) - (ndx)` (upstream:
`rsync.h:837`) for always-present-when-enabled fields (uid/gid/acl/xattr,
selected by the global `*_ndx` counters at `rsync.h:816-823`), and
`OPT_EXTRA(f,bump)` (upstream: `rsync.h:838`) for per-entry-optional
fields whose presence is computed from the `flags` via the `*_BUMP`
macros (`rsync.h:844-848`). The total extra count for a node is summed
incrementally in `extra_len` as each optional field is detected
(upstream: `flist.c:704` init, then `flist.c:965,972,976,980,984,1007`
for hardlink/ACL/checksum/length64/nsec/device), then the whole block is
allocated as `FILE_STRUCT_LEN + extra_len + basename_len + linkname_len`
in one `pool_alloc` (upstream: `flist.c:1018-1025` recv side,
`flist.c:1423-1433` sender side).

This is exactly the model `ExtrasRef` mirrors, with one structural
difference. Upstream selects which extras a node carries via *global*
config indices plus per-entry flag bumps, so the slot order is implicit
and shared across the whole flist. The flat store instead makes the
selection *self-describing per entry*: `ExtrasRef` points at a tail in
`arena.blobs` whose first 2 bytes are a presence mask, followed by the
present fields in canonical order. The semantic content is the same set
(symlink target, rdev, hardlink, acl/def-acl/xattr indices, checksum,
user/group names, atime/crtime/nsec) - compare the flat tail layout
("Offset-based indexing scheme" above) field-for-field against the
upstream `F_*` accessors: `F_OWNER`/`F_GROUP` (`rsync.h:875-876`),
`F_ACL`/`F_XATTR` (`rsync.h:877-878`), `F_ATIME`/`F_CRTIME`
(`rsync.h:880-881`), `F_HL_GNUM`/`F_HL_PREV` (`rsync.h:884-885`),
`F_RDEV_P` (`rsync.h:895`), `F_SUM` (`rsync.h:898`), `F_MOD_NSEC`
(`rsync.h:858`), `F_HIGH_LEN` (`rsync.h:854`). The flat store pays a
2-byte per-tail mask that upstream avoids by deriving presence from
global config; in exchange it needs no global `*_ndx` state threaded
through every accessor.

### Name and dirname interning vs upstream's sharing

Upstream stores the basename **inline** as the `file_struct` flexible
array tail (upstream: `rsync.h:808`; written by `memcpy(bp, basename,
basename_len)` at `flist.c:1027` / `flist.c:1435`), so there is no
per-entry basename allocation but also **no basename deduplication** -
two files named `README` in different directories store the string
twice. The dirname is shared by a different mechanism: a `lastdir`
one-entry cache. When consecutive entries share a directory prefix the
same heap `lastdir` pointer is reused; only when the prefix changes is a
new `lastdir` allocated (upstream: `flist.c:765-773` recv,
`flist.c:1373-1380` sender; assigned via `file->dirname = lastdir` at
`flist.c:1076` / `flist.c:1487`). The full path is reconstructed on
demand by `f_name()`, which concatenates `dirname + '/' + basename`
(upstream: `flist.c:3360-3377`). The sender additionally keeps a
`F_PATHNAME` extra pointing at the on-disk root (upstream: `rsync.h:866`,
`rsync.h:868`).

The flat store's interner is therefore strictly stronger than upstream
on basenames and equivalent on dirnames: it interns *both* name and
dirname to 4-byte `PathHandle`s, giving basename deduplication upstream
lacks (the duplicate-`README` case collapses to one arena string) while
matching upstream's dirname sharing (upstream's `lastdir` cache shares
only runs of identical consecutive dirnames; a true interner shares all
occurrences). Reconstructing the full path is the flat-store analogue of
`f_name()`: resolve both handles and join. The flat store has no need for
a separate `F_PATHNAME` extra because name resolution is already an arena
read, not a pointer into transfer-root-relative storage.

### Final byte target and per-target padding caveats

**Final target: 48 bytes on a 64-bit target**, the low end of the stated
48-64 range, with the field order in the struct above yielding no tail
padding (8+8+4+4+4+4+4+4+4+2+2 = 48). Caveats grounded in upstream:

- Upstream's node is 24 bytes fixed (`FILE_STRUCT_LEN`, `rsync.h:827`)
  plus conditional extras; the flat header's 48 bytes is 2x that, the
  cost of inlining uid/gid/nsec/size that upstream conditionalizes. This
  is acceptable per the sizing rationale above and far below the legacy
  ~96-160 bytes.
- 32-bit targets: upstream's `union file_extras` is 4 bytes there
  (`PTRS_ARE_32`, `rsync.h:767-769`) and pointers shrink, but the flat
  header's `i64 mtime` + `u64 size` stay 8 bytes each, so the header does
  **not** shrink proportionally on 32-bit; it remains 48 bytes
  (the scalar fields are pointer-width-independent). State the 48-byte
  target as a 64-bit figure and add a `const _: () = assert!(size_of ==
  48)` only under `#[cfg(target_pointer_width = "64")]`.
- Extras-tail alignment: upstream rounds every node's `extra_len` up to a
  multiple of `EXTRA_LEN` via `EXTRA_ROUNDING` (upstream: `rounding.h:1`
  defines it as 1; applied at `flist.c:1014-1015` / `flist.c:1419-1420`),
  and guarantees the int64 extras (`REQ_EXTRA64`) are allocated first so
  they are 8-byte aligned (upstream: `rsync.h:840-842`). See open
  question 4 below for how the flat `blobs` region handles this.

### Resolved open questions

1. **Inline size versus conditional size - RESOLVED: keep inline `u64`.**
   Upstream splits size into `len32` (inline, `rsync.h:804`) plus a
   conditional `F_HIGH_LEN` extra gated by `FLAG_LENGTH64`
   (`rsync.h:855`, `flist.c:1466-1470`) purely because its base node is
   24 bytes and every inline byte is precious. The flat header is already
   48 bytes with no tail padding; reclaiming 4 bytes by conditionalizing
   the high word would add an arena read on every large-file access and a
   `present` bit, for under 10% of the node. Keep `size: u64` inline. The
   `FLAG_LENGTH64` mechanism stays relevant only on the wire, which is
   unchanged. Revisit only if the RSS-A.11 profile shows the inline word
   is binding.
2. **Mode width - RESOLVED: narrow to `u16`.** Upstream stores mode as
   `uint16` (upstream: `rsync.h:805`) and assigns it directly from
   `st.st_mode` (`flist.c:1472`). The full POSIX type+permission set
   (`S_IFMT` | `ACCESSPERMS`) fits in 16 bits, and the wire encodes mode
   as a 32-bit value but upstream truncates to `uint16` on receive, so
   there are no upper mode bits to preserve. Narrow `FileEntryHeader.mode`
   from `u32` to `u16`, saving 2 bytes; reallocate them to keep the node
   at a clean 48 with the `present`/`flags` `u16` pair.
3. **Extras tail read ergonomics - RESOLVED: both, mirroring upstream.**
   Upstream itself uses both styles: per-field macro reads on hot paths
   (`F_OWNER`, `F_SUM`, etc., `rsync.h:875-899`) and full reconstruction
   only where a complete view is needed (`f_name`, `flist.c:3360`).
   Provide on-demand per-field accessors on `FlatFileList` for hot paths
   and a transient `FileEntryExtras` view constructor for migration call
   sites, exactly as the design's accessor list already sketches.
4. **Arena blob fragmentation / alignment - RESOLVED: byte-packed tail,
   unaligned reads.** Upstream keeps its extras 8-byte aligned by
   prepending them and ordering int64 slots first (`rsync.h:840-842`,
   `EXTRA_ROUNDING` at `rounding.h:1`). The flat `blobs` tail cannot cheaply
   replicate that because tails are densely appended at arbitrary offsets.
   Resolve by reading multi-byte extras fields with explicit
   `from_le_bytes` / `read_unaligned`-equivalent byte reads rather than
   reinterpreting a `*const u32`; this is endian-stable and alignment-safe
   on all targets (including strict-alignment ones), and the extras path is
   cold (only entries with symlinks/devices/checksums hit it), so the
   byte-assembly cost is negligible. Do **not** pad the `blobs` region;
   padding would reintroduce the per-entry slack the flat store exists to
   remove.
5. **`index` materialization cost - RESOLVED: single `Vec<u32>`, alias
   when unsorted.** Upstream proves the sort-the-pointers model: it keeps
   `files` in build order and a separate `sorted` permutation
   (upstream: `rsync.h:943` `struct file_struct **files, **sorted;`),
   sorting only the pointer array via `fsort`/`file_compare`
   (`flist.c:1696-1756`). Crucially, when no unsorted view is needed
   upstream aliases `sorted = files` and allocates no second array
   (upstream: `flist.c:2149-2153`, gated on `need_unsorted_flist`). The
   flat store adopts the same discipline: one `index: Vec<u32>`, and when
   build order already equals transfer order, treat `index` as the
   identity permutation (or skip materializing it) rather than allocating
   parallel sorted/filtered views. A `u32` index at 1M entries is 4 MB, an
   order of magnitude below the per-entry savings, matching upstream's
   single-pointer-array overhead.

## Cross-references

- `docs/audit/arena-prototype-landing-gap.md` - RSS-A.0.a audit
  establishing that the `PathHandle`/`PathArena` migration never landed;
  the basis for this document's premise correction (RSS-A.0.c).
- `docs/audit/file-entry-layout-audit.md` - RSS-A.2 layout audit and
  per-entry overhead numbers cited here.
- `docs/design/rss-8a-arena-handle-type.md` - earlier PathHandle /
  PathArena design proposal. Not implemented; see the audit above. The
  4-byte handle described there is the type RSS-A.5.c must still build.
- `docs/design/rss-9a-sort-consumer-pathhandle-migration.md` - earlier
  proposal for threading the arena through sort consumers. Not
  implemented; RSS-A.6 does this work from scratch.
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
  The "Upstream-grounded layout resolution (RSS-A.5.f)" section above
  confirms the `file_struct` / `file_extras` layout, extras packing, and
  sort-the-pointers model against the local upstream tree
  (`target/interop/upstream-src/rsync-3.4.1/`), with file:line citations.
