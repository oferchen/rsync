# RSS gap: PathBuf and Arc<Path> overhead per FileEntry

Task: #1048. Branch: `docs/rss-pathbuf-overhead-1048`. Companion audits:
`docs/audits/pathbuf-arc-path-rss-overhead.md` (earlier path-field audit),
`docs/audits/profiling-100k-files.md`, and the
`docs/benchmarks/flist-memory-baseline-2026-05-01.md` baseline that
quantified the resident-set gap against upstream.

## Summary

oc-rsync stores per-entry path data as a `PathBuf` (`name`) plus an
`Arc<Path>` (`dirname`), with rare-field metadata behind
`Option<Box<FileEntryExtras>>`. Upstream rsync packs the basename, the
optional extras (atime, crtime, hardlink, checksum, length-high, mtime
nsec, etc.), and the file_struct header into a single pool-allocated
extent and shares a `dirname` C-string pointer via the
`lastdir`/`F_PATHNAME()` cache. For a 100 K-file workload, the inline +
heap footprint of the two path fields and the extras box accounts for
~19-24 MiB of the resident-set gap, of which ~7-9 MiB is fragmentation
(allocator metadata, 16 B size-class rounding, capacity slack) that the
upstream pool allocator does not pay.

The reductions worth pursuing in priority order:

1. Replace `name: PathBuf` with `name: Box<Path>` (or `Box<[u8]>`):
   eliminates the `Vec` capacity field and excess slack with no API
   churn. Saves 8-16 B per entry.
2. Move the basename out of `FileEntry` into a per-flist arena
   (`Vec<u8>` byte pool plus `(start, len)` indices), matching upstream
   `pool_alloc(file_pool, ...)`.
3. Pack the extras inline at the tail of the same arena allocation
   instead of `Box<FileEntryExtras>`, mirroring upstream's
   `extra_len + FILE_STRUCT_LEN + basename_len + linkname_len` single
   allocation.
4. Intern basenames as well as dirnames when filenames repeat across
   directories (`README.md`, `Cargo.toml`, `mod.rs`, `__init__.py`).
5. Use a bump allocator (`bumpalo` or a hand-rolled one) for the entire
   `FileList` to amortise allocator overhead and free everything in O(1)
   when the flist is dropped.

## FileEntry layout

### `crates/protocol/src/flist/entry/core.rs`

Lines 32-72 declare `FileEntry`. The path-typed fields are:

| Field | Type | Inline bytes | Heap allocation |
|---|---|---|---|
| `name` | `PathBuf` (= `Vec<u8>` on Unix) | 24 | one `Vec` buffer of `len` + capacity slack |
| `dirname` | `Arc<Path>` (fat pointer) | 16 | one `ArcInner<Path>` (16 B header + path bytes), shared via `PathInterner` |
| `extras` | `Option<Box<FileEntryExtras>>` | 8 | one `Box<FileEntryExtras>` only when symlinks/devices/hardlinks/ACLs/xattrs/atimes/crtimes/checksums are set |

Other inline fields (`size`, `mtime`, `uid`, `gid`, `mode`,
`mtime_nsec`, `flags`, `content_dir`) consume the remaining bytes. The
total inline size is asserted at `<= 96 B`
(`crates/protocol/src/flist/entry/tests.rs:296-304`).

### `crates/flist/src/entry.rs`

`FileListEntry` (lines 6-13) is the traversal-step variant emitted by
the directory walker. It owns *two* `PathBuf` instances:

```rust
pub struct FileListEntry {
    pub(crate) full_path: PathBuf,      // 24 B inline + heap
    pub(crate) relative_path: PathBuf,  // 24 B inline + heap
    pub(crate) metadata: fs::Metadata,
    pub(crate) depth: usize,
    pub(crate) is_root: bool,
}
```

The walker emits `FileListEntry` values per directory step before they
are translated into protocol `FileEntry` values, so during the build
phase both representations co-exist on the heap.

## Per-entry heap allocations

Assume an average path length of 20 bytes (the workload measured in
`docs/benchmarks/flist-memory-baseline-2026-05-01.md`).

### `name: PathBuf`

`Vec<u8>` rounds capacity to allocator size classes. For a 20-byte
path:

- 24 B inline header (`ptr`, `len`, `cap`).
- 1 heap allocation. Glibc/jemalloc round up to the next 16 B class:
  the buffer occupies 32 B.
- Plus per-allocation allocator metadata (8-16 B in glibc,
  effectively 0 in jemalloc/mimalloc bins but still chargeable to RSS
  via slab-padding rounding).

Per-entry cost: 24 B inline + 32 B heap + 8 B metadata = ~64 B.

### `dirname: Arc<Path>`

`Arc<Path>` is a fat pointer: `(ptr, len)` = 16 B inline. The heap
side is an `ArcInner<Path>` containing two atomic counts plus the
path bytes:

- 16 B inline header.
- 1 heap allocation: 16 B `ArcInner` (strong + weak `AtomicUsize`)
  plus the path bytes. For a 12-byte parent dir, the allocation is
  16 + 12 = 28 B, rounded to 32 B.

`PathInterner`
(`crates/protocol/src/flist/intern.rs:42-94`) deduplicates dirnames so
the per-entry amortised heap cost depends on `unique_dirs / entries`.
For a 100-directory tree of 10 000 files, each `Arc` has refcount 100
and the amortised heap cost per entry is `32 B / 100 = 0.32 B`. At
N = 1 directory (the worst case for interning, all files in one dir),
amortisation saves nothing.

`extract_dirname` (`core.rs:78-83`) is called by every direct
constructor and produces a *fresh* `Arc<Path>` per call. Only the
`FileListReader` path interns. CLI-driven file-list construction does
not.

### `extras: Option<Box<FileEntryExtras>>`

`Option<Box<T>>` is a single 8 B pointer (null-pointer optimisation).
When `Some`, the `Box` allocates `sizeof(FileEntryExtras)`. The
struct (`extras.rs:14-56`) is approximately 200 B (six `Option<u32>`,
two `Option<i64>`, three `i64`/`u32` raw, `Option<PathBuf>`,
`Option<String>` x2, `Option<Vec<u8>>`, `Option<XattrList>`).

For a vanilla file transfer (no `--hard-links`, `-A`, `-X`,
`--atimes`, `--crtimes`, `--checksum`), `extras` stays `None` and
costs only the 8 B inline pointer slot. This is already in lockstep
with upstream's "extras only when needed" behaviour.

## Upstream comparison

`target/interop/upstream-src/rsync-3.4.1/rsync.h:801-829`:

```c
struct file_struct {
    const char *dirname;    /* The dir info inside the transfer */
    time_t modtime;
    uint32 len32;
    uint16 mode;
    uint16 flags;
    const char basename[];  /* Flexible-array tail */
};
#define FILE_STRUCT_LEN (sizeof(struct file_struct))   /* 24 B on 64-bit */
```

`flist.c:1423-1435`:

```c
alloc_len = FILE_STRUCT_LEN + extra_len + basename_len + linkname_len;
bp = pool_alloc(pool, alloc_len, "make_file");
memset(bp, 0, extra_len + FILE_STRUCT_LEN);
bp += extra_len;
file = (struct file_struct *)bp;
bp += FILE_STRUCT_LEN;
memcpy(bp, basename, basename_len);
```

Each entry is exactly one `pool_alloc()` from a 256 KiB
`NORMAL_EXTENT` pool (`rsync.h:936`). The pool is bump-allocated
inside fixed-size extents:

- Header (`FILE_STRUCT_LEN` = 24 B) and basename are contiguous, no
  separate pointer indirection.
- Optional extras (`F_ATIME`, `F_CRTIME`, `F_OWNER`, `F_GROUP`,
  `F_HL_PREV`, `F_HIGH_LEN`, `F_MOD_NSEC`, `F_NDX`, `F_DEPTH`,
  `F_SUM`) prepend the header in the same allocation; addressed via
  negative indexing (`OPT_EXTRA(f, bump)`).
- `dirname` is a `const char *` shared via the `lastdir` cache
  (`flist.c:697,768`): when the parsed dir matches the previous
  entry's dir, the same heap pointer is reused. Otherwise a single
  `new_array(char, len + 1)` copy is made.
- Free is O(1) per pool-extent (`pool_destroy()`), not per entry.

Per-entry cost in upstream for a 20-byte basename + 12-byte dirname:

| Component | Size |
|---|---|
| `FILE_STRUCT_LEN` header | 24 B |
| Basename (NUL-terminated) | 21 B |
| `extra_len` (typical: 0) | 0 B |
| Pool overhead (amortised) | < 1 B |
| Dirname (shared via `lastdir`, amortised across run of same-dir entries) | ~0 B |

Total per entry: ~45-50 B in the worst case, ~25 B amortised when
runs of same-dir entries hit `lastdir`.

oc-rsync per entry (no extras, post-interning):

| Component | Size |
|---|---|
| `FileEntry` inline | ~88 B |
| `name` heap (PathBuf) | 32 B |
| `dirname` heap (`Arc<Path>` allocation, amortised over 100 entries/dir) | ~0.32 B |
| Allocator metadata | ~8 B |

Total: ~128 B per entry. At 100 K entries: ~12.5 MiB inline +
~3.2 MiB heap = ~15.7 MiB; upstream is ~5 MiB total. Gap aligns
with the empirical 42.6 vs 7.9 MB Mode-B RSS reading.

## Reductions

### 1. `Box<Path>` over `PathBuf` for `name`

`PathBuf` (= `Vec<u8>`) carries a 24 B header (ptr/len/cap) and the
buffer can be over-allocated. `Box<Path>` is a 16 B fat pointer with
no spare capacity, sized exactly to `len`.

- Saves 8 B inline per entry (`-800 KiB at 100 K entries`).
- Saves the slack between `len` and `cap`: at 20 B paths and 32 B
  size class, that's 12 B of slack on every entry (`-1.2 MiB at
  100 K entries`).
- API surface unaffected: existing accessors take `&Path`.
  Constructors that currently take `PathBuf` can take `impl Into<PathBuf>`
  and call `.into_boxed_path()` once.
- Implementation cost: low. Touches `core.rs` and the constructor
  module; reader/writer paths already produce `PathBuf` and need a
  single `.into_boxed_path()` shim.

### 2. Per-flist arena for basenames

Mirror upstream by allocating all basenames into a single
`Vec<u8>` per flist and storing `(start: u32, len: u32)` in the
entry instead of a `Box<Path>` or `PathBuf`.

- Eliminates per-entry path allocation entirely.
- 8 B inline per entry replaces 16-24 B inline + 32 B heap.
- Frees in O(1) when the `FileList` drops.
- Indices are 64 bits combined; the arena cap is 4 GiB which is
  more than enough for any realistic transfer.
- Implementation cost: medium. Path accessors must reconstruct
  `&Path` via `Path::new(OsStr::from_bytes(&arena[start..start+len]))`
  on Unix and via `OsStr::new(str::from_utf8(&...).unwrap())` on
  Windows. The conversion is a no-op on Unix (zero copy) and a
  fast UTF-8 validation on Windows.
- Saves ~2.4 MiB at 100 K entries (heap) plus ~0.8 MiB inline.

### 3. Pack extras into the arena tail

Extend the arena strategy: when an entry needs extras, allocate
`sizeof(FileEntryExtras)` bytes at the tail of the same arena
allocation as the basename. Replace `Option<Box<FileEntryExtras>>`
with a single `Option<NonZeroU32>` offset.

- Eliminates the `Box<FileEntryExtras>` allocation when present.
- Saves 0 B for vanilla transfers (where `extras` is already
  `None`) but reclaims ~200 B per entry plus allocator overhead
  for `--hard-links`, `-A`, `-X`, etc.
- Mirrors upstream `OPT_EXTRA(f, bump)` semantics.
- Implementation cost: high. `FileEntryExtras` accessors need to
  resolve the offset against the arena. Best done together with
  reduction (2).

### 4. Basename interning

`PathInterner` (`intern.rs:42-94`) covers dirnames. Many transfers
have repeating basenames (`README.md`, `Cargo.toml`, `mod.rs`,
`__init__.py`, `index.html`, `.gitignore`). A `BasenameInterner`
keyed on `&[u8]` and yielding `Arc<Path>` would deduplicate those.

- High variance: a tar of a deep nested project saves a lot
  (10-15% of unique paths), a random media archive saves nothing.
- Implementation cost: low (clone of `PathInterner` keyed on
  basename).
- Subsumed by reduction (2) if the arena uses content-addressable
  offsets (a `HashMap<&[u8], u32>` during build).

### 5. Bump allocator for the whole FileList

Use `bumpalo::Bump` (or hand-rolled equivalent) for the
`FileList`'s entries, basenames, dirnames, and extras. Per-entry
allocation becomes a pointer bump; deallocation is `Bump::reset()`
or drop. Upstream effectively does this with `pool_create()`.

- Removes `Box<FileEntryExtras>` overhead (no `Box`, just a bump
  ptr).
- Removes `Arc<Path>` reference counts: dirnames live as
  `&'arena Path` for the lifetime of the flist, no atomics.
- Saves the 16 B `ArcInner` header per unique dirname.
- Saves allocator metadata across thousands of small allocations.
- Implementation cost: high. `FileEntry` becomes generic over the
  arena lifetime (`FileEntry<'a>`) or stores arena-relative
  offsets. The latter aligns with reductions (2) and (3).
- Saves ~4-6 MiB at 100 K entries when combined with (2) and (3).

## Recommended sequence

1. Land reduction 1 (Box<Path>) as a small, low-risk PR. Measurable
   ~2 MiB win at 100 K entries.
2. Land reduction 2 (basename arena) plus the inline-offset variant of
   the entry struct. Touches the hot path; gated behind the existing
   `<= 96 B` size assertion.
3. Land reduction 3 (extras in arena) once 2 is stable.
4. Evaluate reduction 4 (basename interning) only if the post-arena
   numbers still show a gap; otherwise skip (data-dependent ROI).
5. Defer reduction 5 (`bumpalo`) until 2 and 3 are merged; at that
   point, the `Vec<u8>` + offsets layout already provides 90% of the
   bump-allocator's wins without an external dep.

## Cross-references

- `crates/flist/src/entry.rs`: traversal-step entry layout.
- `crates/protocol/src/flist/entry/core.rs:32-83`: `FileEntry` and
  `extract_dirname`.
- `crates/protocol/src/flist/entry/extras.rs:14-56`: `FileEntryExtras`.
- `crates/protocol/src/flist/entry/constructors.rs:18-46`: per-entry
  allocations.
- `crates/protocol/src/flist/intern.rs:42-94`: dirname interning.
- `crates/protocol/src/flist/entry/tests.rs:296-304`: 96 B size guard.
- `target/interop/upstream-src/rsync-3.4.1/rsync.h:801-829,936-937`:
  upstream `file_struct` and pool-extent constants.
- `target/interop/upstream-src/rsync-3.4.1/flist.c:697-769,1230-1512,
  2910-2925`: `lastdir`, `make_file`, pool creation.
- `docs/audits/pathbuf-arc-path-rss-overhead.md`: prior path-field
  audit (this audit extends that work to cover the extras box and
  proposes concrete reduction sequencing).
- `docs/benchmarks/flist-memory-baseline-2026-05-01.md`: empirical
  RSS baseline.
