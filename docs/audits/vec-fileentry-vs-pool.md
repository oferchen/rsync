# `Vec<FileEntry>` vs upstream pool allocator (#1050)

Static audit of the file-list allocation strategy. Compares upstream
rsync's slab pool (`lib/pool_alloc.c`) against oc-rsync's
`Vec<FileEntry>` plus per-entry heap allocations, and lists the
single-slab arena candidates worth piloting. Ships no code change.

## 1. Upstream: `pool_alloc` slabs for `struct file_struct`

Upstream allocates every file-list entry from a per-flist slab pool:

- `lib/pool_alloc.c:48` `pool_create(SMALL_EXTENT|NORMAL_EXTENT, ...)`,
  `:115` `pool_alloc()`, `:259` `pool_free_old()`, `:310` `pool_boundary()`.
- `flist.c:2914` and `:2920` create the pool inside `flist_new()`; sibling
  flists reuse `first_flist->file_pool` (`flist.c:2929`).
- `flist.c:1018` packs each entry as a single contiguous record:
  `alloc_len = FILE_STRUCT_LEN + extra_len + basename_len + dirname_len + linkname_len`,
  filled by one `pool_alloc(pool, alloc_len, "recv_file_entry")` at `:1020`
  (sender mirror at `flist.c:1239`+).
- `util2.c` provides only the OOM helper; the slab logic lives in
  `lib/pool_alloc.c`. `flist.c:325` calls `pool_boundary(...,8*1024)` so an
  flist can be released as one batch via `pool_free_old`.

Net effect: one allocation per entry, freed in O(1) per flist, no
per-field `malloc` for path or extras.

## 2. oc-rsync: `Vec<FileEntry>` + boxed extras

`crates/protocol/src/flist/entry/core.rs:32` defines `FileEntry` with
`name: PathBuf`, `dirname: Arc<Path>`, and
`extras: Option<Box<FileEntryExtras>>` (`:55`). Every populated entry
therefore costs at least:

| Allocation | Source | Per-entry bytes (glibc, 64-bit) |
|------------|--------|---------------------------------|
| `Vec<FileEntry>` slot | inline `FileEntry` (~200 B; see `entry/tests.rs:294`) | 200 |
| `PathBuf` heap (name) | `name: PathBuf` at `core.rs:35` | 32-48 + path len |
| `Arc<Path>` heap | shared via `PathInterner` (`core.rs:42`) | amortized 24 / unique dir |
| `Box<FileEntryExtras>` | `entry/accessors.rs:14` lazy box | 0 or 64-128 |
| glibc `malloc` chunk overhead | 16 B header per chunk | 32-48 |

Three to four discrete `malloc`s per non-trivial entry (vec slot, path
buffer, extras box, optional symlink target inside extras) versus one
`pool_alloc` upstream. At 1 M entries that is ~200 MB of fragmented
heap chunks plus glibc bookkeeping, against a handful of slab extents
upstream.

## 3. Arena candidates

- `bumpalo` 3.x: `Bump` arena with `bumpalo::collections::Vec` /
  `boxed::Box`. Drop-free, per-flist reset matches `pool_free_old`. No
  `Drop` recursion - `FileEntry` must be POD-ish or wrapped in
  `ManuallyDrop`.
- `typed-arena`: simpler `Arena<FileEntry>` with stable references; no
  reset, must drop the arena to reclaim. Acceptable for incremental
  flists if each segment owns its arena.
- `slab` crate: keyed slab; useful only if we keep handles instead of
  pointers. Heavier than `bumpalo` for our access pattern.

Suggested pilot: arena per `FileList` segment, store `&'arena str`
basenames carved from the same arena, keep the existing `Arc<Path>`
dirname interner outside the arena to preserve cross-segment sharing.

## 4. Risks

- **Lifetimes.** `FileEntry` currently owns its strings; an arena
  flips ownership to a borrow tied to the flist. Every consumer
  (`engine`, `transfer`, `cli`) must accept a generic
  `FileEntry<'a>` or the arena must be `'static` (which defeats reuse).
- **Drop semantics.** `FileEntryExtras` carries `Vec<u8>` symlink
  targets and checksum buffers. Bumpalo skips `Drop`, so these would
  leak unless we keep `Box`/`Vec` outside the arena or switch to arena
  slices.
- **Signal cleanup.** Signal handlers and `RawSyncReceiver` cancel
  paths (`crates/transfer/src/receiver.rs`) currently rely on `Drop` to
  release transient flist memory. An arena-only flist must be wired
  into the cancellation tree explicitly to avoid leaks on SIGINT/abort.
- **INC_RECURSE.** Sibling segments share `first_flist->file_pool`
  upstream (`flist.c:2929`). The Rust port must mirror that sharing or
  pay re-allocation each segment.

## 5. Cross-references

- #1048 - PathBuf overhead per entry; arena work would subsume the
  basename allocation savings tracked there.
- `docs/audits/100k-stat-syscall-overhead.md` - related RSS curve at
  100 K+ entries.
- Upstream: `target/interop/upstream-src/rsync-3.4.1/lib/pool_alloc.c`,
  `flist.c:1018-1025`, `flist.c:2907-2935`.
