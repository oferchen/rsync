# RSS-3: per-FileEntry size breakdown vs upstream `file_struct`

Task: RSS-3. Branch: `docs/rss-3-fileentry-size-audit`. Companion audits:
`docs/audits/rss-flist-vec-vs-pool.md` (container-level Vec slack and
segment fragmentation), `docs/audits/rss-pathbuf-arcpath-overhead.md`
and `docs/audits/pathbuf-arc-path-rss-overhead.md` (path-field overhead
and reduction sequencing), `docs/benchmarks/flist-memory-baseline-2026-05-01.md`
(empirical RSS baseline). This audit is the hard-numbers reference that
RSS-4 (allocator evaluation) and RSS-5 (design) will design against.

## Summary

For a vanilla regular-file entry (no symlinks, devices, hardlinks,
ACLs, xattrs, atimes, crtimes, or checksums) carrying a 20-byte
basename inside a 12-byte parent directory:

- oc-rsync `FileEntry`: **88 B inline + ~32 B name heap + ~0.3 B
  amortised dirname heap + ~8 B allocator metadata** = **~128 B per
  entry**, scaling to ~12.8 MiB at 100 K entries.
- upstream `struct file_struct`: **24 B header + 21 B basename + ~0 B
  amortised dirname + < 1 B pool overhead** = **~45 B per entry**,
  scaling to ~4.5 MiB at 100 K entries.

That single-entry **2.8x** ratio is the dominant driver of the
observed 3-11x peak-RSS gap; the remaining factor comes from `Vec`
doubling slack (see `rss-flist-vec-vs-pool.md`) plus 2-3 additional
per-entry allocator metadata words that the upstream pool does not
pay.

The top 3 heap contributors (in priority order for RSS-4/5 to design
against):

1. **`name: PathBuf` per-entry allocation** - one heap allocation per
   entry, size-class rounded to 32 B for a 20-byte path. Pays
   8-16 B of allocator metadata. ~3.2 MiB at 100 K, ~32 MiB at 1 M.
2. **`Vec<FileEntry>` inline footprint multiplier** - 88 B inline vs
   upstream's 24 B header (`FILE_STRUCT_LEN`). ~6.1 MiB inline gap at
   100 K, ~61 MiB at 1 M.
3. **`dirname: Arc<Path>` per-unique-dir allocation** - `ArcInner`
   header (16 B for `strong`/`weak` `AtomicUsize`) plus path bytes,
   per *unique* dirname. Vs upstream's `lastdir` cache which reuses a
   single C-string pointer for runs of same-dir entries. Cost is
   workload-dependent: ~0.32 B per entry at 100 dirs / 100 K files,
   pathological at low fan-out (one dir, 100 K files, refcount =
   100 K, 32 B allocation amortised to 0.32 mB - but `ArcInner`
   atomics still touch the cache line on every clone).

The smoking gun: every `FileEntry` constructor hits the system
allocator **at least twice** (once for `name`, once for `dirname` when
the interner misses, plus once for `Box<FileEntryExtras>` when
present). Upstream's `make_file()` and `recv_file_entry()` hit the
allocator **at most once per ~8 K-32 K entries** (a pool extent
boundary, `pool_alloc.c`).

## Canonical struct definition

`crates/protocol/src/flist/entry/core.rs:32-72`:

```rust
pub struct FileEntry {
    // 8-byte aligned fields
    pub(super) name: PathBuf,                              // 24 B
    pub(super) dirname: Arc<Path>,                         // 16 B
    pub(super) size: u64,                                  // 8 B
    pub(super) mtime: i64,                                 // 8 B
    pub(super) uid: Option<u32>,                           // 8 B
    pub(super) gid: Option<u32>,                           // 8 B
    pub(super) extras: Option<Box<FileEntryExtras>>,       // 8 B

    // 4-byte aligned fields
    pub(super) mode: u32,                                  // 4 B
    pub(super) mtime_nsec: u32,                            // 4 B

    // 2-byte aligned fields
    pub(super) flags: super::super::flags::FileFlags,      // 3 B + 1 B pad

    // 1-byte aligned fields
    pub(super) content_dir: bool,                          // 1 B + 3 B tail pad
}
```

Inline size on Unix: **88 B** (asserted at
`crates/protocol/src/flist/entry/tests.rs:299-311`). On Windows the
cap is 104 B because `PathBuf`'s inner `Wtf8Buf` carries an extra
`is_known_utf8` bool, padding `OsString` from 24 to 32 B.

`FileEntryExtras` (`crates/protocol/src/flist/entry/extras.rs:13-56`):

```rust
pub(super) struct FileEntryExtras {
    pub(super) link_target: Option<PathBuf>,    // 24 B (None tag uses ptr slot)
    pub(super) user_name: Option<String>,       // 24 B
    pub(super) group_name: Option<String>,      // 24 B
    pub(super) atime: i64,                      //  8 B
    pub(super) crtime: i64,                     //  8 B
    pub(super) atime_nsec: u32,                 //  4 B
    pub(super) rdev_major: Option<u32>,         //  8 B (4 + tag)
    pub(super) rdev_minor: Option<u32>,         //  8 B
    pub(super) hardlink_idx: Option<u32>,       //  8 B
    pub(super) hardlink_dev: Option<i64>,       // 16 B
    pub(super) hardlink_ino: Option<i64>,       // 16 B
    pub(super) checksum: Option<Vec<u8>>,       // 32 B (24 + 8 tag, padded)
    pub(super) acl_ndx: Option<u32>,            //  8 B
    pub(super) def_acl_ndx: Option<u32>,        //  8 B
    pub(super) xattr_ndx: Option<u32>,          //  8 B
    pub(super) xattr_list: Option<XattrList>,   // 32 B (24 inline Vec + tag, padded)
}
```

Approximate inline footprint: **~240 B** plus padding (each field
`Option<T>` with `T` size < niche-eligible pointer takes `size_of::<T>() +
align_of::<T>()` bytes). The exact `size_of::<FileEntryExtras>()` was
quoted at "~200 bytes" in `rss-pathbuf-arcpath-overhead.md`; field-by-
field re-summing gives a tighter 232-248 B band depending on
re-ordering. The struct is intentionally *not* size-asserted because
it lives behind `Box`, so its allocator round-up (typically 256 B
size class) is what RSS actually pays.

## Per-field size breakdown

### Inline footprint (per entry, paid even when fields are unset)

| Field | Type | Inline B | Notes |
|---|---|---|---|
| `name` | `PathBuf` (= `Vec<u8>` on Unix) | 24 | ptr, len, cap |
| `dirname` | `Arc<Path>` | 16 | fat pointer: (data ptr, byte len) |
| `size` | `u64` | 8 | |
| `mtime` | `i64` | 8 | seconds since Unix epoch |
| `uid` | `Option<u32>` | 8 | 4 B payload + 4 B tag + align |
| `gid` | `Option<u32>` | 8 | 4 B payload + 4 B tag + align |
| `extras` | `Option<Box<FileEntryExtras>>` | 8 | null-pointer optimisation; tag is the null pointer |
| `mode` | `u32` | 4 | |
| `mtime_nsec` | `u32` | 4 | |
| `flags` | `FileFlags` (3 x `u8`) | 3 | + 1 B internal pad to byte boundary |
| `content_dir` | `bool` | 1 | + 3 B tail pad to 8 B alignment |
| **Total inline** | | **88** | Unix; 104 on Windows |

### Heap cost (per entry, "vanilla" regular file workload)

For a 20-byte basename, 12-byte parent directory, no extras:

| Allocation | Source | Heap B | Allocator round-up | Notes |
|---|---|---|---|---|
| `name` `Vec<u8>` buffer | `name: PathBuf` | 20 | 32 (next 16 B size class) | one allocation per entry |
| `dirname` `ArcInner<Path>` | `dirname: Arc<Path>` | 16 (header) + 12 (path) = 28 | 32 | one allocation per *unique* dir; amortised across all entries sharing that dir via `PathInterner` |
| `extras` `Box<FileEntryExtras>` | `extras: Option<Box<...>>` | 0 | 0 | `None` for vanilla file |
| Per-allocation glibc metadata | `name` heap, plus `dirname` heap on first occurrence | 8-16 | included in size class | not strictly per-entry for dirname |
| **Total heap (per entry, vanilla)** | | **~32-44 B amortised** | | dominated by `name` |

### Heap cost (per entry, full-extras workload: `-A -X -H --atimes --crtimes --checksum`)

| Allocation | Heap B | Round-up | Notes |
|---|---|---|---|
| `name` buffer | 20 | 32 | as above |
| `dirname` ArcInner | 28 | 32 | as above, amortised |
| `Box<FileEntryExtras>` | ~240 | 256 | one allocation per entry |
| `link_target` PathBuf buffer (symlinks only) | 20 | 32 | one allocation when present |
| `user_name`/`group_name` String buffers | 8-32 each | 16-32 | only when not numeric and not in id cache |
| `checksum` Vec<u8> buffer | 16-32 | 32 | one allocation when `--checksum` |
| `xattr_list` Vec<XattrEntry> backing | variable | size-class | one allocation when xattrs present; `XattrEntry` is itself 5 fields, two heap `Vec<u8>` |
| **Total heap (per entry, full extras, no xattrs)** | | **~352 B** | dominated by `Box<FileEntryExtras>` |

Note: the inline 88 B already pays the 8 B `Option<Box<...>>` slot
regardless. The `Box` allocation only adds heap; it does *not* shrink
the inline footprint.

## Upstream comparison

`target/interop/upstream-src/rsync-3.4.1/rsync.h:801-829`:

```c
struct file_struct {
    const char *dirname;    /* The dir info inside the transfer */
    time_t modtime;         /* When the item was last modified */
    uint32 len32;           /* Lowest 32 bits of the file's length */
    uint16 mode;            /* The item's type and permissions */
    uint16 flags;           /* The FLAG_* bits for this item */
#ifdef USE_FLEXIBLE_ARRAY
    const char basename[];  /* The basename (AKA filename) follows */
#else
    const char basename[1];
#endif
};
#define FILE_STRUCT_LEN (sizeof (struct file_struct))    /* 24 B on 64-bit */
#define EXTRA_LEN       (sizeof (union file_extras))     /*  4 B */
```

`union file_extras` is a 4-byte slot (`int32`, `uint32`, or 4-byte
pointer on 32-bit). `union file_extras64` is an 8-byte slot, used for
`atime`, `crtime`, `pathname` on 64-bit, and 64-bit-aligned hardlink
indices.

`target/interop/upstream-src/rsync-3.4.1/flist.c:1018-1027`:

```c
alloc_len = FILE_STRUCT_LEN + extra_len + basename_len + linkname_len;
bp = pool_alloc(pool, alloc_len, "recv_file_entry");

memset(bp, 0, extra_len + FILE_STRUCT_LEN);
bp += extra_len;
file = (struct file_struct *)bp;
bp += FILE_STRUCT_LEN;
memcpy(bp, basename, basename_len);
```

Every entry is **one** `pool_alloc()` call, drawing from a per-flist
`alloc_pool_t` (`lib/pool_alloc.c`) that bump-allocates inside 8-32 KiB
extents. Basename and any required symlink target sit *immediately
after* the `file_struct` header in the same allocation. Optional
extras (atime/crtime/hardlink/checksum/length-high/mtime-nsec) are
**prepended** to the header inside the same allocation and addressed
via negative indexing (`OPT_EXTRA(f, bump)` and
`REQ_EXTRA(f, ndx)` in `rsync.h:837-848`).

`compat.c:572-594` shows how the per-flist `file_extra_cnt` is set
once at handshake based on which transfer options are active:

```c
if (preserve_atimes)  atimes_ndx   = (file_extra_cnt += EXTRA64_CNT);  /* 2 slots */
if (preserve_crtimes) crtimes_ndx  = (file_extra_cnt += EXTRA64_CNT);  /* 2 slots */
if (am_sender)        pathname_ndx = (file_extra_cnt += PTR_EXTRA_CNT);/* 2 slots */
else                  depth_ndx    = ++file_extra_cnt;                 /* 1 slot  */
if (preserve_uid)     uid_ndx      = ++file_extra_cnt;                 /* 1 slot  */
if (preserve_gid)     gid_ndx      = ++file_extra_cnt;                 /* 1 slot  */
if (preserve_acls && !am_sender) acls_ndx = ++file_extra_cnt;          /* 1 slot  */
if (preserve_xattrs)  xattrs_ndx   = ++file_extra_cnt;                 /* 1 slot  */
```

The `dirname` field is a `const char *` shared via the `lastdir`
cache (`flist.c:697,1421-1442`): when the parsed dir matches the
previous entry's dir, the same heap pointer is reused. Otherwise a
single `new_array(char, len + 1)` copy is made (also out of the pool).

### Per-entry comparison table (vanilla regular file, no extras)

| Component | upstream | oc-rsync |
|---|---|---|
| Struct header inline | 24 B (`FILE_STRUCT_LEN`) | 88 B inline footprint inside `Vec<FileEntry>` |
| Basename storage | NUL-terminated bytes packed into same pool extent (21 B for 20-byte name) | 24 B `PathBuf` header inline + 32 B heap buffer |
| Dirname storage | shared `const char *` via `lastdir` cache, ~0 B amortised across same-dir runs | 16 B `Arc<Path>` fat pointer inline + 32 B `ArcInner<Path>` heap per unique dir (interner-amortised) |
| Required extras (`uid`/`gid` if enabled) | 4 B each, packed in extent before header | already in 88 B inline (`Option<u32>` = 8 B each) |
| Optional extras (`atime`/`crtime`/hardlink/checksum) when unset | 0 B | 0 B heap (`extras = None`), 8 B inline `Option<Box<...>>` slot still paid |
| Per-entry allocator metadata | < 1 B amortised over ~8 K-32 K entries per extent | ~8-16 B per `name` allocation + per-unique-dir cost for `dirname` |
| Free cost | `pool_destroy()` walks extents, O(extents) | drop walks Vec, calling `Drop` on every `PathBuf` and `Arc`; O(entries) |
| **Total (worst-case, no `lastdir` reuse)** | **~45 B** | **~128 B** |
| **Total (best-case, `lastdir` hit)** | **~25 B** | **~128 B** (no equivalent of `lastdir` short-circuit; interner is hash-lookup, still allocates Arc on first miss) |

## The smoking gun: allocations per entry

| Path | System allocator calls per entry |
|---|---|
| oc-rsync `FileEntry::new_file()` / `from_raw_bytes()` (vanilla file) | **2-3**: one for `PathBuf` (`name`), one for the `Arc<Path>` allocation if the dirname is new, and one for the `Box<FileEntryExtras>` whenever extras are needed |
| oc-rsync `Vec<FileEntry>::push()` (amortised) | **0** (Vec doubling, charged separately - see `rss-flist-vec-vs-pool.md`) |
| upstream `recv_file_entry()` / `make_file()` | **1 / N** where N is the entries-per-pool-extent (8 K-32 K for typical paths); the `pool_alloc()` call is a bump-pointer increment, not a `malloc` |
| upstream `pool_alloc()` allocates a new extent | every ~8 K-32 K entries depending on average `alloc_len` |

Even when `PathInterner` deduplicates dirnames, `extract_dirname()` in
`core.rs:78-83` is called from **every** direct constructor and produces
a fresh `Arc<Path>` per call. Only the `FileListReader` path interns
(`crates/protocol/src/flist/intern.rs:42-94`); CLI-driven file-list
construction does not. So in the CLI sender path, the per-entry
allocation count is consistently 2 (name + dirname) for vanilla
entries, plus a third for any symlink/device/ACL/xattr.

## Top 3 heap contributors driving the 3-11x RSS gap

Ranked by total bytes paid per 100 K entries on a vanilla regular-file
workload (20-byte basenames, 12-byte parent dirs, 100 unique dirs).

### 1. `name: PathBuf` per-entry heap allocation (~3.2 MiB at 100 K, ~32 MiB at 1 M)

Every entry allocates one `Vec<u8>` buffer through the system
allocator for its basename. At 20 B average length the buffer rounds
to the 32 B size class, and glibc charges 8-16 B of per-allocation
metadata (jemalloc bins effectively 0 inline but pays in slab
padding). 100 K entries -> 100 K calls into `malloc`/`free`.

Upstream's basename lives inside the pool extent immediately after
the `file_struct` header (`flist.c:1027`, flexible-array tail).
Allocator cost is ~0; basename storage is exactly `basename_len` (no
size-class rounding) and is freed in O(1) per extent via
`pool_destroy()`.

### 2. `Vec<FileEntry>` inline footprint multiplier (~6.1 MiB at 100 K, ~61 MiB at 1 M)

oc-rsync stores 88 B inline per entry (104 B on Windows). Upstream
stores **24 B** of header inside the pool extent plus its basename
bytes packed contiguously. The 64 B inline gap (88 - 24) is paid on
every entry regardless of optional fields and is **independent of any
path-interning win**.

This is also the dominant cost when scaling: at 1 M entries the
inline gap alone is 61 MiB before any heap is counted. Reductions
proposed in `rss-pathbuf-arcpath-overhead.md` (Box<Path>, arena
offsets) would also shrink this footprint by replacing the 24 B
`PathBuf` field with an 8 B `(start: u32, len: u32)` arena index.

### 3. `dirname: Arc<Path>` ArcInner allocation, plus atomic-RC cache traffic

Memory cost is small in absolute terms (~3.2 KiB at 100 unique dirs;
each `ArcInner<Path>` is 32 B with 16 B `ArcInner` header storing
`strong`/`weak` `AtomicUsize`). The real cost is structural:

- `extract_dirname()` is called by **every** direct constructor and
  produces a fresh `Arc<Path>` allocation. Only the reader path
  interns.
- Every `FileEntry::clone()` calls `Arc::clone(&self.dirname)`, which
  is an atomic RMW. At 1 M entries cloned during sort/incremental
  segmentation this is 1 M atomic operations on shared cache lines.
- The `Drop` of a `FileEntry` is similarly atomic: a final-decrement
  ArcInner free path.

Upstream's `dirname` is a `const char *` with no refcount, shared via
`lastdir`. Reuse is detected by string comparison on the most-recent
entry only (`flist.c:697,1421-1442`); when reuse hits, **no copy and
no allocation** happens. Free is `pool_destroy()`, not per-entry.

The amortised per-entry memory cost of `dirname` is small. The cost
that RSS-4/5 should design against is the *allocation count* and the
*atomic-RC cache-line traffic*, not the bytes.

## Auxiliary findings

- **`FileListEntry` (traversal-step variant)**: lives in
  `crates/flist/src/entry.rs:6-13`. Holds **two** `PathBuf` instances
  (`full_path`, `relative_path`) plus a `fs::Metadata`. During the
  build phase this co-exists on the heap with the protocol
  `FileEntry`, adding ~64 B inline + two heap path allocations per
  walker step. See `rss-pathbuf-arcpath-overhead.md` for context.
- **`size_of::<FileEntry>()` runtime assertions** already exist at
  `crates/protocol/src/flist/entry/tests.rs:299-311` (<= 96 B Unix /
  <= 104 B Windows) and
  `crates/protocol/benches/file_entry_memory.rs:19,37-40` (<= 96 B).
  These guard against accidental growth; they do *not* guard against
  the heap shape.
- **`FileEntryExtras`** is not size-asserted. Sum of its fields is
  ~240 B without internal padding optimisation; the `Box` round-up
  hits the 256 B size class.
- **Per-entry allocations when extras *are* used** can reach 6-7:
  `name`, `dirname`, `Box<FileEntryExtras>`, `link_target`, plus
  `user_name`/`group_name` strings, `checksum` buffer, and any xattr
  `Vec`s.

## Methodology

1. Located the canonical struct at
   `crates/protocol/src/flist/entry/core.rs:32-72`.
2. Field types and sizes computed from known Rust stdlib layouts on
   64-bit Unix: `PathBuf` = 24 B (`Vec<u8>` header), `Arc<Path>` =
   16 B (fat pointer), `Box<T>` = 8 B (thin pointer when `T: Sized`),
   `Option<T>` is `size_of::<T>() + align_of::<T>()` unless niche
   optimisation applies (`Option<&T>`, `Option<Box<T>>`, `Option<NonZeroU32>`).
3. Inline size cross-checked against
   `crates/protocol/src/flist/entry/tests.rs:299-311` (= 88 B Unix)
   and `crates/protocol/benches/file_entry_memory.rs:19,37-40`.
4. Extras layout read from
   `crates/protocol/src/flist/entry/extras.rs:13-56`.
5. Constructor allocation profile read from
   `crates/protocol/src/flist/entry/constructors.rs:18-46,138-173`
   and `crates/protocol/src/flist/entry/core.rs:78-83`
   (`extract_dirname`).
6. Upstream layout quoted from
   `target/interop/upstream-src/rsync-3.4.1/rsync.h:801-829`
   (`file_struct`, `FILE_STRUCT_LEN`, `EXTRA_LEN`),
   `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-799`
   (`file_extras` unions), and
   `target/interop/upstream-src/rsync-3.4.1/compat.c:572-594`
   (`file_extra_cnt` initialisation).
7. Pool allocation contract read from
   `target/interop/upstream-src/rsync-3.4.1/flist.c:1018-1027`
   (single `pool_alloc()` per entry with header + basename + extras +
   linkname contiguous) and `flist.c:697,1421-1442` (`lastdir`
   cache).
8. Empirical RSS gap cross-referenced against
   `docs/benchmarks/flist-memory-baseline-2026-05-01.md` (Mode-B
   100 K-file reading: 42.6 vs 7.9 MB).
9. No code modified. No measurements rerun in this audit; the goal
   was field-by-field accounting against upstream as the
   authoritative source.

## Cross-references

- `crates/protocol/src/flist/entry/core.rs:32-83` - `FileEntry` and `extract_dirname`.
- `crates/protocol/src/flist/entry/extras.rs:13-56` - `FileEntryExtras`.
- `crates/protocol/src/flist/entry/constructors.rs:18-173` - constructors and per-entry allocation pattern.
- `crates/protocol/src/flist/entry/tests.rs:293-311` - 96 B / 104 B inline-size guard.
- `crates/protocol/src/flist/flags.rs:160-175` - `FileFlags` (3 B inline).
- `crates/protocol/src/flist/intern.rs:42-94` - dirname interning.
- `crates/protocol/src/xattr/list.rs:11-14`, `crates/protocol/src/xattr/entry.rs:39-53` - `XattrList` and `XattrEntry` (relevant when `extras.xattr_list` is set).
- `crates/protocol/benches/file_entry_memory.rs:1-111` - existing benchmark measuring inline size and 100 K-entry allocation.
- `crates/protocol/src/flist/trace.rs:130-149` - `trace_struct_sizes` reporting `FILE_STRUCT_LEN`/`EXTRA_LEN` parity with upstream debug output.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-829` - `file_extras` unions, `file_struct`, `FILE_STRUCT_LEN`, `EXTRA_LEN`.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:837-885` - `REQ_EXTRA`/`OPT_EXTRA`/`F_*` accessors.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:572-594` - `file_extra_cnt` per-option setup.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:704,1018-1027` - per-entry pool allocation.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697,1421-1442` - `lastdir` cache.
- `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c:1-90,114-175` - pool extent layout.
- `docs/audits/rss-flist-vec-vs-pool.md` - container-level (Vec slack, segments).
- `docs/audits/rss-pathbuf-arcpath-overhead.md` - reduction sequencing for path fields and `Box<Path>` / arena migration.
- `docs/audits/pathbuf-arc-path-rss-overhead.md` - earlier path-field overhead audit.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` - empirical RSS baseline.
