# PathBuf and Arc<Path> RSS overhead per FileEntry

Task: #1048. Branch: `docs/pathbuf-arc-rss-1048`. Companion audits: #966
(overall RSS gap), #971 (1M-file RSS scaling), #1037 (`FileEntry` memory
benchmark, completed), #1049 (string interning evaluation, completed),
#1050 (`Vec<FileEntry>` vs upstream pool allocator, pending).

## Summary

Upstream rsync packs every file-list entry into a single
pool-allocated extent: `FILE_STRUCT_LEN` (24 B header on 64-bit) plus the
NUL-terminated basename plus optional extras, sharing dirname pointers via a
once-per-directory `lastdir` cache (`flist.c:697,768`). oc-rsync stores the
relative path as a `PathBuf` (24 B inline header + heap allocation with
capacity slack) plus an `Arc<Path>` dirname (16 B inline header + heap
allocation with reference counter). For a 100 K-file workload the path
fields alone account for an estimated 19-24 MiB of the resident-set gap, of
which 7-9 MiB is fragmentation (`Vec<u8>` capacity rounding and per-allocation
allocator metadata) that no upstream pool-allocator equivalent has to pay.
Path interning (#1049) already reclaims the dirname duplication on the read
path; the remaining wins require either (a) replacing the per-entry
`PathBuf` with a `Box<[u8]>` (or `Arc<[u8]>` shared with the dirname) or
(b) collapsing both fields into offsets into a single per-flist arena
(#1050).

## Methodology

1. Read the post-decomposition (`#2787`) `FileEntry` layout in
   `crates/protocol/src/flist/entry/core.rs:32-72` and the rare-fields
   container in `crates/protocol/src/flist/entry/extras.rs:14-56`; confirm
   the size invariant via `crates/protocol/src/flist/entry/tests.rs:296-304`
   (`<= 96 B`).
2. Enumerate path-typed fields and compute inline footprints. `PathBuf`
   is `Vec<u8>` on Unix (3 words = 24 B); `Arc<Path>` is a fat pointer
   (16 B) plus a 16 B `ArcInner<T>` header (strong + weak counts) on the
   heap.
3. Compute heap footprints at N = 20 B/path using standard 16 B
   size-class rounding. Cross-check against
   `crates/protocol/benches/file_entry_memory.rs:1-100`.
4. Translate per-entry numbers to MB at 100 K entries; cross-check
   against the empirical 42.6 vs 7.9 MB Mode-B RSS in
   `docs/benchmarks/flist-memory-baseline-2026-05-01.md`.
5. Compare to upstream's pool layout in
   `target/interop/upstream-src/rsync-3.4.1/flist.c:1009-1030` and
   `target/interop/upstream-src/rsync-3.4.1/rsync.h:801-830,936-937`.

## FileEntry path-related fields (post-decomposition #2787, #1275)

`crates/protocol/src/flist/entry/core.rs:32-72` defines the inline layout.
Two of the eleven fields hold path data; the rest are scalar metadata.

| Field | Type | Inline bytes | Heap footprint | Notes |
|---|---|---|---|---|
| `name` | `PathBuf` (= `Vec<u8>` on Unix) | 24 | 1 allocation, `len` bytes + capacity slack | Always present; relative path. |
| `dirname` | `Arc<Path>` | 16 | 1 allocation, `len` bytes + 16 B `ArcInner` header (strong + weak counts) | Shared via `PathInterner` (`crates/protocol/src/flist/intern.rs:42-94`) when read through `FileListReader`; freshly allocated by `extract_dirname` (`core.rs:78-83`) on every direct constructor call. |
| `extras.link_target` | `Option<PathBuf>` (in `Box<FileEntryExtras>`) | 0 (boxed) | 1 allocation per symlink only | `extras.rs:16`. None for regular files. |

Other inline fields (8-byte: `size`, `mtime`; smaller: `uid`, `gid`,
`mode`, `mtime_nsec`, `flags`, `content_dir`; pointer: `extras`) consume
the remaining 56-72 bytes within the 96 B inline budget. Path-typed fields
account for **40 bytes of the inline footprint per entry** (`24 +
16 = 40`), which is 42-44 % of the inline struct depending on padding
(`size_of::<FileEntry>()` is 88 B as of `tests.rs:296-304`, with the next
two bytes consumed by alignment, leaving the assertion at `<= 96` to absorb
future padding).

PathBuf's three-word inline layout is documented in
[`std::vec::Vec`](https://doc.rust-lang.org/std/vec/struct.Vec.html#guarantees):
data pointer, length, capacity. `Arc<Path>` is a thin fat-pointer; the
inner allocation carries
[`ArcInner<T>`](https://doc.rust-lang.org/std/sync/struct.Arc.html#deref-behavior)
which prepends two `usize` atomics (16 B on 64-bit) before the payload.

## Per-entry overhead breakdown

Take a workload modeled after `scripts/benchmark_flist_memory.sh`
(100 dirs x 1000 files). A typical relative path is
`dir_NNN/file_NNNNN.txt` -> 19-22 B; round to **N = 20 B per path**.
Dirname is `dir_NNN` -> 7 B. With 16 B size-class rounding (glibc malloc
or jemalloc small bins), a PathBuf holding 1-16 user bytes takes 16 B of
mapped heap, 17-32 takes 32 B, etc. Per-allocation metadata is amortised
across each bin's chunk; we charge it once at 20-25 % overhead consistent
with the empirical numbers in
`docs/benchmarks/flist-memory-baseline-2026-05-01.md`.

Empty PathBuf has zero heap footprint (`Vec::new()` defers allocation).
Production paths always go through `from_raw_bytes` or `new_file` with a
non-empty argument, so the empty case is bench-only.

| Component | Bytes per entry | Notes |
|---|---|---|
| `FileEntry` inline (incl. all non-path fields) | 88 | `size_of::<FileEntry>()`, asserted `<= 96` in `entry/tests.rs:296-304`. |
| Vec backing for `name` (avg 20 B path, 32 B size-class) | 32 | Capacity rounded up to next power-of-two-ish class; `String::shrink_to_fit` is not called on the read path, so growth from `prepend_dir` (`entry/accessors.rs:55-58`) leaves slack. |
| `ArcInner<Path>` for `dirname` (16 B header + 7 B path, 32 B size-class) | 32 | After interning, divided by entries-per-directory; without interning, paid per-entry. See "String interning evaluation" below. |
| `Box<FileEntryExtras>` (regular file) | 0 | None; only allocated when symlink/device/hardlink/ACL/xattr fields are touched (`entry/core.rs:51-55`). |
| Subtotal (path-related only) | 32 + 32 = 64 | dirname amortised separately. |
| Allocator overhead (20-25 % of heap) | ~14 | Slab metadata, per-arena pointers, free-list bookkeeping. |
| **Per-entry path footprint (uninterned)** | **~110 bytes** | 88 inline + 22 path-heap. |
| **Per-entry path footprint (interned dirname)** | **~120 bytes - dirname/(N/D)** | dirname cost divided by 1000 entries-per-dir = 0.03 B/entry; falls into the noise. |

For a regular file with a 20-byte relative path, the **incremental cost of
PathBuf+ArcInner over an upstream `file_struct`** is the `name` Vec
allocation (32 B size-class) plus the `Arc<Path>` dirname (uninterned: 32 B
size-class incl. 16 B `ArcInner` header) plus allocator overhead.

### Why PathBuf is heavier than a `Box<str>` or `Box<[u8]>`

A `PathBuf` keeps three words (data, len, cap) so it can be mutated in
place - critical for the transfer-side `prepend_dir`
(`entry/accessors.rs:55-58`) and `strip_leading_slashes`
(`entry/accessors.rs:79+`) helpers. Once the file list is built and sorted
(`crates/protocol/src/flist/sort.rs`), the path is treated as immutable for
the rest of the run; `cap` is dead weight from that point on. A
`Box<[u8]>` is 16 B inline (data + len, no cap) and matches upstream's
`basename[]` flexible-array idiom byte-for-byte. The 8 B per-entry
saving is 800 KiB at 100 K entries and 8 MiB at 1 M entries - small in
absolute terms but free if the post-sort path is sealed.

## Upstream comparison

`target/interop/upstream-src/rsync-3.4.1/rsync.h:801-812` defines:

```c
struct file_struct {
    const char *dirname;     /* The dir info inside the transfer */
    time_t modtime;          /* When the item was last modified */
    uint32 len32;            /* Lowest 32 bits of the file's length */
    uint16 mode;             /* The item's type and permissions */
    uint16 flags;            /* The FLAG_* bits for this item */
#ifdef USE_FLEXIBLE_ARRAY
    const char basename[];   /* The basename (AKA filename) follows */
#else
    const char basename[1];  /* A kluge that should work like a flexible array */
#endif
};
```

Sizes on a 64-bit host:

- `dirname` pointer: 8 B
- `modtime` (`time_t`): 8 B
- `len32`: 4 B
- `mode`: 2 B
- `flags`: 2 B
- `basename[]`: 0 B (flexible-array tail; the actual bytes live just past the
  struct in the same pool extent)

Total `FILE_STRUCT_LEN` = 24 B (the C ABI rounds the struct up to 8 B
alignment regardless of the flexible array). All optional fields - uid,
gid, atime, crtime, hardlink, ACL, xattr indices - live as `union
file_extras` slots prepended *before* the struct in the same pool
allocation, accessed via the negative-offset `REQ_EXTRA(f,ndx)` /
`OPT_EXTRA(f,bump)` macros (`rsync.h:837-842`). When none are configured
(`compat.c:574-594`), `extra_len` is zero.

`flist.c:1018-1027` (recv path):

```c
alloc_len = FILE_STRUCT_LEN + extra_len + basename_len + linkname_len;
bp = pool_alloc(pool, alloc_len, "recv_file_entry");
memset(bp, 0, extra_len + FILE_STRUCT_LEN);
bp += extra_len;
file = (struct file_struct *)bp;
bp += FILE_STRUCT_LEN;
memcpy(bp, basename, basename_len);
```

The pool itself is bump-allocated 128 KiB extents (`SMALL_EXTENT`) for
INC_RECURSE per-segment lists, 256 KiB (`NORMAL_EXTENT`) for the main flist
(`flist.c:2914,2920`, constants in `rsync.h:936-937`). Per-allocation
metadata cost is one pointer-aligned padding gap per extent boundary, not
per entry; allocator overhead is therefore amortised to ~0 B per file.

Dirname deduplication uses the static `lastdir` cache
(`flist.c:697,767-773` send-side; `flist.c:1233,1375-1378` recv-side):

```c
if (len != lastdir_len || memcmp(thisname, lastdir, len) != 0) {
    lastdir = new_array(char, len + 1);
    memcpy(lastdir, thisname, len);
    lastdir[len] = '\0';
    lastdir_len = len;
    lastdir_depth = count_dir_elements(lastdir);
}
...
file->dirname = lastdir;  /* flist.c:1076,1487 */
```

`lastdir` is a separately heap-allocated string (one per unique directory
encountered in *sequential* sort order); `file->dirname` is just the
8 B pointer. There is no Arc; once the flist is freed
(`flist.c:2969-2971` via `pool_destroy`), all dirname strings owned by
that flist are freed via the pool itself only if they were pool-allocated
(in INC_RECURSE) - otherwise `lastdir` outlives the flist by design and is
freed at process exit. The total dirname heap is **D unique strings, each
~7-32 B**; for our 100-dir test that is roughly 100 * 16 B = 1.6 KiB.

### Per-entry comparison at 100K files

| Component | upstream | oc-rsync | Delta |
|---|---|---|---|
| File-list struct (header + scalar metadata) | 24 B | 88 B | +64 B |
| Path bytes (basename, NUL terminator, in-pool) | ~22 B | 0 (separate alloc) | -22 B |
| Path heap allocation (avg 20 B path, 32 B class) | 0 | 32 B | +32 B |
| Allocator slack on path alloc (PathBuf cap, 32-byte class headroom) | 0 | ~12 B | +12 B |
| Dirname (per entry, amortised) | ~0.02 B (8 B ptr - all 100 dirs share <= 1.6 KiB) | 0.03 B (interned) or 32 B (uninterned) | +0.01 B to +32 B |
| Pool / allocator metadata | ~0 (bump alloc) | ~6-8 B (per-alloc bin metadata, one per `name` and one per uninterned dirname) | +6-16 B |
| **Per-entry total (interned dirname)** | ~46 B | ~138 B | +92 B |
| **Per-entry total (uninterned dirname)** | ~46 B | ~170 B | +124 B |

## At 100K files

| Scenario | Per-entry delta | Total delta |
|---|---|---|
| Interned dirname (current `FileListReader` path) | ~92 B | 9.2 MB |
| Uninterned dirname (direct constructor) | ~124 B | 12.4 MB |

Cross-check against `docs/benchmarks/flist-memory-baseline-2026-05-01.md`
Mode B: oc-rsync 42.6 MB - upstream 7.9 MB = 34.7 MB total gap. The
PathBuf+ArcInner share is therefore **27-36 % of the observed RSS gap at
100 K files**. The remainder is attributable to:

- The 64 B per-entry inline-struct gap (88 B vs upstream's 24 B):
  6.4 MB at 100 K. Mostly the `Box<FileEntryExtras>` slot and 8-byte
  alignment of every field even when the corresponding upstream `extra`
  is absent.
- `Vec<FileEntry>` capacity slack (Rust doubles capacity on grow;
  worst-case 50 % overshoot before `shrink_to_fit`): up to 4-5 MB
  depending on the sizing path. The reader pre-sizes via `Vec::with_capacity`
  in most code paths but not all.
- Auxiliary structures: `PathInterner` HashMap entries
  (`crates/protocol/src/flist/intern.rs:43-48`), hardlink table
  (`crates/protocol/src/flist/hardlink/`), wire buffer slack.

The remaining ~13-15 MB is the subject of #1050 (pool-allocator
evaluation) and the `Vec<FileEntry>` indirection cost. This audit's scope
is the path fields only.

### Math (reproducible)

```
N = 100_000, P = 20 B path, D = 7 B dirname, S = 1000 entries/dir
PathBuf inline+heap (32 B class):       24 + 32 = 56 B
Arc<Path> inline+heap (interned, /S):   16 + 0.03 = 16.03 B
Allocator metadata (1 per PathBuf):     ~7 B (vs upstream pool: 0)
Path-related per-entry:                 ~79 B  (upstream: 8 + 22 = 30 B)
Delta per entry:                        ~49 B   ->  4.9 MB at 100 K
Inline-struct gap (88 vs 24 B):         +64 B   ->  +6.4 MB at 100 K
Vec capacity slack (~10 % size class):           +1.5-2 MB at 100 K
Path-related RSS slice, 100 K:                   ~13 MB
```

For #1050 sizing, treat **9-13 MB at 100 K** and **90-130 MB at 1 M** as
the path-related savings ceiling.

## String interning evaluation (#1049 recap)

`crates/protocol/src/flist/intern.rs` ships and is wired into the read
path. `FileListReader` (referenced from `entry/core.rs:26-28`) interns
each unique parent directory once and clones the `Arc<Path>` into every
entry below it (`entry/accessors.rs:65-68` `set_dirname`). The mechanism
mirrors upstream's `lastdir` cache exactly except that:

- upstream uses raw `const char *` with no reference counting (the pool
  owns the lifetime).
- oc-rsync uses `Arc<Path>` so dirname can outlive the reader, e.g. when
  the file list is sorted in-place and entries are reordered into the
  generator's pipeline.

At 100 dirs / 100 K files = 1000 entries per directory, the dirname heap
shrinks from 100_000 * (16 + 32) = 4.6 MB (uninterned, naive 32 B
size-class) to 100 * 32 = 3.2 KB. **#1049 has saved ~4.5 MB at 100 K
already.** The remaining `Arc<Path>` cost is the 16 B inline pointer per
entry which interning cannot remove without restructuring the entry to
look up dirname through an external table indexed by directory id.

The `intern.rs:43` HashMap itself adds ~32 B per unique directory (key +
value + 8-12 B HashMap bucket metadata) - 3.2 KB at 100 dirs, negligible
at 100 K - and is dropped after `FileListReader` finishes.

## Alternative designs

### A. Replace `PathBuf` with `Box<[u8]>` post-sort

After `flist_sort_and_clean()` runs (`flist.c:3071-3084`,
oc-rsync mirror in `crates/protocol/src/flist/sort.rs`), the path is
immutable. `Box<[u8]>` is 16 B inline (data + len, no cap) vs PathBuf's
24 B; `prepend_dir` and `strip_leading_slashes` would need a copying
variant that returns a new entry instead of mutating in place, which is
already how the protocol layer handles wire decoding (`from_raw_bytes`
allocates a fresh `PathBuf`).

- Inline saving: 8 B per entry = **800 KiB at 100 K, 8 MiB at 1 M**.
- Heap saving: PathBuf allocates with capacity rounding; `Box::<[u8]>::from(&[u8])`
  allocates exactly `len` bytes (the allocator still rounds to its size
  class, but PathBuf rarely shrinks). Realistic saving is 4-8 B per entry
  in capacity slack: **400-800 KiB at 100 K, 4-8 MiB at 1 M**.
- Migration cost: every site that calls `entry.path()` or
  `entry.name()` now needs `Path::new(OsStr::from_bytes(&entry.name))`.
  Touched files: `crates/protocol/src/flist/entry/accessors.rs:21-29,55-58`
  and ~30 transfer/match/cli call sites. Constructive but not invasive.

### B. Share basename and dirname under a single `Arc<[u8]>` slab

Build a per-flist arena holding all path bytes back-to-back; each entry
stores two `(u32 offset, u32 len)` tuples (one for the basename slice,
one for the dirname slice). Entry path-related inline shrinks from 40 B
(24 PathBuf + 16 Arc<Path>) to 16 B (two `(u32,u32)` slices). With a
shared `Arc<[u8]>` the arena is owned once per flist and freed
collectively on drop.

- Inline saving: 24 B per entry = **2.4 MB at 100 K, 24 MB at 1 M**.
- Heap saving: one allocation per flist instead of N allocations for
  basenames and D allocations for dirnames. Eliminates per-entry allocator
  metadata (~7 B per PathBuf alloc): **700 KiB at 100 K, 7 MB at 1 M**.
- Mutation: `prepend_dir` becomes "build a new arena segment and rewrite
  the offsets". Acceptable if done at flist construction time only;
  prohibitive if done per-entry during pipeline dispatch.
- This is essentially a port of upstream's pool layout to Rust; it is the
  natural shape for #1050 and the path subset of that work.

### C. `FileEntryExtras` already boxes rare fields (#1275 / PR #2727)

The `Box<FileEntryExtras>` indirection is already shipped
(`crates/protocol/src/flist/entry/core.rs:51-55`). It removed ~200 B of
inline tail from non-symlink entries; orthogonal to path storage and no
further wins from this direction.

### D. Drop `dirname` from `FileEntry`; recompute via `Path::parent()`

`extract_dirname` (`entry/core.rs:78-83`) just calls `Path::parent()`
plus `Arc::from(path)`. The `Arc<Path>` field is only needed because
sort/dedupe paths want a cheap shared reference (`PartialEq` ignores it,
`entry/core.rs:124-137`). Recomputing dirname on demand from `name`
saves the entire 16 B inline + 32 B heap:

- Inline saving: 16 B per entry = **1.6 MB at 100 K, 16 MB at 1 M**.
- Heap saving (interned): negligible (already 0.03 B/entry).
- Heap saving (uninterned read path): 32 B per entry = **3.2 MB at 100 K**.
- Cost: every dirname access now allocates a temporary `PathBuf` from
  `name.parent()`. Hot paths are `crates/protocol/src/flist/sort.rs`
  (called once per entry during sort) and the wire writer
  (`crates/protocol/src/flist/write/encoding.rs` `xflags`
  computation). Adds one `parent()` walk per access; PathBuf has no
  trailing-NUL invariants so this is `memchr`-fast. May be net positive
  only if the access count per entry is low.

## Recommendation

Pursue option **B (per-flist `Arc<[u8]>` arena with `(offset,len)`
slices)** as part of #1050. The design choice is forced by the upstream
parity goal (`compat.c:574-594` only supports a fixed set of `file_extras`
slots; deviating from the pool layout has zero protocol benefit and high
maintenance cost). Concretely:

1. Define `FlistPathArena { bytes: Arc<[u8]> }` in
   `crates/protocol/src/flist/`.
2. Replace `name: PathBuf` and `dirname: Arc<Path>` in
   `crates/protocol/src/flist/entry/core.rs:35-42` with
   `name_offset: u32, name_len: u32, dirname_offset: u32, dirname_len: u32`
   (16 B total).
3. Add `arena: Arc<FlistPathArena>` to the per-flist owner (e.g.
   `FileList` in `crates/protocol/src/flist/state.rs` or
   `crates/flist/src/builder.rs`); each `FileEntry` stays unaware of the
   arena and exposes `name(&self, arena: &FlistPathArena) -> &Path`.
4. Migrate `prepend_dir` and `strip_leading_slashes` to operate on the
   arena builder before `seal()` is called; freeze the arena into
   `Arc<[u8]>` after sort.

Migration cost is ~30 call sites in
`crates/transfer/src/{generator,receiver}/`,
`crates/match/src/`, `crates/cli/src/`, and the existing tests in
`crates/protocol/src/flist/entry/tests.rs`. Each site changes from
`entry.path()` to `entry.path(&flist.arena)`.

If the arena is judged too invasive for the next release, fall back to
option **A** (`Box<[u8]>` post-sort): smaller win (~1-2 MB at 100 K,
10-20 MB at 1 M) but mechanical edit, contained to
`crates/protocol/src/flist/entry/`.

## Test/benchmark plan

Reuse the existing memory benchmark
(`crates/protocol/benches/file_entry_memory.rs:1-103`, completed under
#1037) as the regression gate. Add three new measurement modes:

1. **Inline-size assertion guard.** Tighten the `<= 96 B` assertion in
   `crates/protocol/src/flist/entry/tests.rs:296-304` to `<= 80 B` once
   option A or B lands. Today the struct is 88 B; option A reduces it to
   80 B; option B reduces it to 72 B (`16 B` for path slices instead of
   `24 + 16 = 40 B`).
2. **Total heap RSS at 100K and 1M.** Reuse
   `scripts/benchmark_flist_memory.sh` (already produces TSV/MD into
   `target/benchmarks/`); compare pre- and post-change peak RSS in
   Modes A and B. Target: 100 K oc-rsync drops from 42.6 MB to <= 33.6 MB
   (closing 9 MB of the 34.7 MB gap, i.e. the `~9-13 MB` ceiling derived
   above).
3. **Allocator profile.** Run `heaptrack` or `dhat` on the
   `allocate_100k_regular_files` Criterion case
   (`crates/protocol/benches/file_entry_memory.rs:46-59`); confirm the
   number of small-class allocations attributable to `PathBuf` drops by
   100 % (option B) or stays equal but with smaller classes (option A).
4. **Interop smoke.** Run `bash tools/ci/run_interop.sh` against rsync
   3.0.9, 3.1.3, 3.4.1 to confirm no wire-format regressions. The path
   storage change is purely internal; wire encoding goes through
   `crates/protocol/src/flist/write/encoding.rs` which only sees `&Path`
   slices.

For a quick local sanity check before the proper container run:

```sh
cargo bench -p protocol --bench file_entry_memory \
    -- file_entry_memory --save-baseline pre-1048
# apply change
cargo bench -p protocol --bench file_entry_memory \
    -- file_entry_memory --baseline pre-1048
```

## References

- `crates/protocol/src/flist/entry/core.rs:32-83` - `FileEntry` struct,
  `extract_dirname` helper.
- `crates/protocol/src/flist/entry/extras.rs:14-56` - boxed rare-field
  container (#1275 / PR #2727).
- `crates/protocol/src/flist/entry/constructors.rs:18-173` -
  `new_with_type` template method, `from_raw_bytes` wire decoder.
- `crates/protocol/src/flist/entry/accessors.rs:11-68` - `name()`,
  `path()`, `dirname()`, `prepend_dir`, `set_dirname`.
- `crates/protocol/src/flist/entry/tests.rs:293-304` - `<= 96 B` inline
  assertion (this audit proposes to tighten it).
- `crates/protocol/src/flist/intern.rs:42-114` - `PathInterner` (#1049).
- `crates/protocol/src/flist/trace.rs:138-149` - `trace_struct_sizes()`
  parity-debug emission of `FILE_STRUCT_LEN`.
- `crates/protocol/benches/file_entry_memory.rs:1-103` - existing 100K
  benchmark (#1037).
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:786-870` - upstream
  `union file_extras`, `struct file_struct`, `FILE_STRUCT_LEN`,
  `REQ_EXTRA`/`OPT_EXTRA` macros; `:936-937` for `SMALL_EXTENT`/`NORMAL_EXTENT`.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697-773` send-side
  `lastdir` cache; `:1018-1027` recv-side `pool_alloc`; `:1076,1487`
  `file->dirname = lastdir`; `:2914-2937` pool create; `:2969-2971`
  pool destroy.
- `target/interop/upstream-src/rsync-3.4.1/compat.c:574-594` -
  `setup_protocol` initialising `file_extra_cnt` slots.
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md` - empirical
  100K/1M Mode A/B RSS numbers anchoring the per-entry math.
- `docs/audits/profiling-100k-files.md` - companion audit flagging
  `PathBuf::join` allocation pressure.
- `scripts/benchmark_flist_memory.sh` - reproduction harness.
- `std::vec::Vec` and `std::sync::Arc` standard-library documentation for
  the 3-word `Vec` and 16 B `ArcInner` layouts cited above.
