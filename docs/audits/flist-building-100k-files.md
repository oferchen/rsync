# File list building cost at 100K entries (#1044)

Static profile of the sender-side file list build path for trees on
the order of 100,000 entries. Scope is the code that runs between
"open the source tree" and "the last entry has been handed to
`FileListWriter::write_entry`". Scope explicitly excludes signature
generation, delta dispatch, and receiver-side decode.

This document anchors to the source as it stands on
`docs/flist-building-100k-1044`. No runtime numbers appear here -
section 5 specifies how to produce them. Section 6 lists 4 throughput
candidates ranked by expected payoff.

## 1. Build path map

Three crates participate. Data flow is one-way: the engine walker
yields `WalkEntry`, the flist crate enriches it with metadata into
`FileListEntry`, and the protocol crate's `FileListWriter` turns each
entry into wire bytes.

| Stage | Crate / file | Role |
|---|---|---|
| Configuration | `crates/flist/src/builder.rs` | Builder, sets follow / copy / safe-link flags |
| Walker construction | `crates/flist/src/file_list_walker.rs:24-73` | Roots traversal, `lstat` vs `stat` choice, canonical-path loop guard |
| Per-directory read | `crates/flist/src/file_list_walker.rs:225-250` (`DirectoryState::new`) | Single `read_dir`, eager name collection, `sort_os_strings` |
| Per-entry `stat` | `crates/flist/src/file_list_walker.rs:95-164` (`prepare_entry`) | One `lstat` (or `stat` if `--copy-links`), symlink-safety re-`read_link` |
| Parallel paths | `crates/flist/src/parallel.rs:122-354` | `collect_paths_then_metadata_parallel`, `collect_with_batched_stats`, `collect_paths_chunked_parallel` |
| Stat cache | `crates/flist/src/batched_stat/cache.rs:18-178` | 16-shard `Mutex<HashMap>`, FNV-1a path hash, rayon `par_iter` over shard fan-out |
| Lower-level walker | `crates/engine/src/walk/walkdir_impl.rs:84-141` | `jwalk` parallel directory reader, sorted output, `one_file_system` filter |
| Path interning | `crates/protocol/src/flist/intern.rs:42-114` | `HashMap<PathBuf, Arc<Path>>` deduplicating directory names |
| Hardlink detection | `crates/protocol/src/flist/hardlink/table.rs:37-108` | `FxHashMap<DevIno, HardlinkEntry>` (rustc-hash) |
| Per-entry encode | `crates/protocol/src/flist/write/mod.rs:375-461` (`write_entry`) | Drives the 13-step wire format below |
| xflags compression | `crates/protocol/src/flist/write/xflags.rs:71-84` | Six sub-passes producing the 24-bit xflags word |
| Name compression | `crates/protocol/src/flist/state.rs:171-178` | Iterator-based common-prefix scan capped at 255 |
| Varint write | `crates/protocol/src/varint/encode.rs:18-99` | One to five bytes per integer field |
| Wire-path normalisation | `crates/protocol/src/flist/wire_path.rs:33-60` | Zero-copy on Unix, allocate on Windows iff `\` present |

The walker layer (`file_list_walker.rs`) and the parallel layer
(`parallel.rs`) are alternatives - the flist crate exposes both. Local
transfers and the receiver side reach the walker through
`collect_entries`, while the sender's preferred path on large trees is
`collect_with_batched_stats`. The lower-level `engine::walk` is used
by callers that want a streaming `Iterator<Item = WalkEntry>` rather
than `Vec<FileListEntry>`.

## 2. Per-entry cost components

For a tree of N entries with D directories, B average byte length per
basename, and L distinct long names (suffix > 255 bytes), the per-build
cost decomposes as follows.

### 2.1 Filesystem syscalls

- **`opendir` + `readdir` loop:** D `opendir` calls and N `readdir`
  iterations. `DirectoryState::new`
  (`file_list_walker.rs:225-250`) materialises every name into a
  `Vec<OsString>` before `sort_os_strings` runs, so peak per-directory
  memory is `Σ name_len` for the largest directory. Allocation cost
  is one `OsString` per entry (heap), one Vec growth per power-of-two
  threshold.
- **Per-entry `stat` / `lstat`:** N `lstat` calls in the sequential
  walker (`prepare_entry` line 106-110). With `--copy-links`, this
  switches to `stat`. On Linux this is `newfstatat(AT_FDCWD, path,
  AT_SYMLINK_NOFOLLOW)` - the path is resolved from the root dir
  every call, so each component traverses dcache for the full depth.
- **Canonicalize loop guard:** N\_dir `realpath` calls
  (`file_list_walker.rs:81`) on the way *into* every directory, plus
  one extra per `--copy-links` symlink (line 142). On Linux,
  `canonicalize` resolves every component via `readlink`-on-prefix.
  Cost grows linearly with directory depth. For a flat 100K tree
  this is negligible (D = 1) but for a deep mirror (D ≈ N) this
  doubles the syscall count.
- **`--safe-links` re-read:** when active, every symlink incurs an
  extra `readlink` (`file_list_walker.rs:117`). Two syscalls per
  symlink on this code path versus one for a regular file.

### 2.2 Name handling

- **`OsString` per name:** `DirectoryState` stores names as
  `OsString`, a heap allocation per entry. `next_name()` uses
  `mem::take` to avoid a clone, but the original allocation persists
  until the directory is drained.
- **`PathBuf::join` per entry:** `prepare_entry` builds
  `state.fs_path.join(&name)` and `state.relative_prefix.join(&name)`
  - two `PathBuf` allocations per entry plus the `OsString` copy
  embedded in each.
- **Wire encoding:** `path_bytes_to_wire` is zero-copy on Unix
  (line 33-39) and allocation-free on Windows when no backslash is
  present. Cost is one `Cow<[u8]>` per entry, dominated by the
  borrow path.
- **Common-prefix compression:** `calculate_name_prefix_len`
  (`state.rs:171-178`) walks the previous and current basenames byte
  by byte until divergence, capped at 255. Cost is `O(B)` per entry,
  pure CPU, no allocation. The 1024-byte fixed `prev_name` buffer in
  `FileListCompressionState` keeps this branch-free.

### 2.3 Varint and field encoding

- **Per-entry write count:** `write_entry` (`write/mod.rs:375-461`)
  emits between 4 and 12 varints depending on negotiated preserve
  flags. Minimum: flags, suffix length, size, mtime. Add one each for
  crtime, atime, atime nsec, uid, gid, hardlink index, rdev major,
  rdev minor.
- **Per-varint cost:** `encode_bytes` (`varint/encode.rs:18-43`)
  copies the value's little-endian bytes, scans backwards for trailing
  zeros, picks a leading-byte pattern, and writes 1-5 bytes. Fully
  inlined, no allocation, ~10 cycles for the common 1-2 byte case.
- **`write_all` per field:** every sub-write is a separate
  `Writer::write_all` call. With an unbuffered writer this is one
  `write(2)` syscall per field. Callers should be wrapping the wire
  in a buffered writer for the syscall amortisation to pay off.

### 2.4 Hardlink lookup

- **Per-entry probe:** when `--hard-links` is active, the generator
  inserts a `(dev, ino)` pair into `HardlinkTable::find_or_insert`
  (`hardlink/table.rs:72-83`). The map uses `FxHashMap` (rustc-hash),
  one of the fastest hashers for small fixed-size keys. Cost is one
  hash + one bucket walk per entry.
- **Memory shape:** `with_capacity(N)` is honoured, so for a 100K
  tree this is a single 1-2 MB allocation up front. No resize-amortised
  rehash if the caller sizes correctly.
- **Skipped path:** when `--hard-links` is off this whole subsystem
  is skipped at the call site (`encoding.rs:201`).

### 2.5 Parallel-stat threshold

The threshold lives in `crates/transfer/src/parallel_io.rs:16`:
`DEFAULT_STAT_THRESHOLD: usize = 64`. Below 64 entries the transfer
crate's `map_blocking` falls back to sequential iteration to avoid
rayon dispatch overhead. For 100K-entry builds this threshold is
crossed by three orders of magnitude, so the sequential fallback is
not relevant - the relevant question is whether the build path
actually reaches the parallel codepath. `collect_entries` in
`flist/src/parallel.rs:41-47` does *not*; it is sequential despite
the module name. The parallel paths (`collect_with_batched_stats`,
`collect_paths_chunked_parallel`) require explicit opt-in by the
caller. As of this audit, the default `core::session()` flow uses
the sequential walker through `FileListBuilder::build`.

## 3. Comparison to upstream `flist.c:send_file_list`

Upstream entry points are in `target/interop/upstream-src/rsync-3.4.1/flist.c`:

- `send_file_list` line 2192 - top-level driver. Allocates two
  `flist` arrays (`flist_new` line 2230, 2233), pre-sized to
  `FLIST_START_LARGE`, then drives the argv loop and dispatches each
  argument through `send_file_name` or `send_directory`.
- `send_directory` line 1820 - per-directory `opendir` + `readdir`
  loop. Note the loop is single-threaded, with a tail recursion via
  `send_if_directory` (line 1885) that drives sub-directory descent
  after the parent's entries have been emitted.
- `send_file_name` line 1534 - per-entry. Calls `make_file` for the
  `lstat`, then `send_file_entry`.
- `send_file_entry` line 380 - the wire encoder. Static locals
  (`modtime`, `mode`, `uid`, `gid`, `lastname`) hold the previous
  entry's values for compression - the same pattern oc-rsync's
  `FileListCompressionState` mirrors.

Differences with operational consequences for 100K trees:

| Concern | Upstream | oc-rsync |
|---|---|---|
| Directory walking | Single thread, one `opendir`+`readdir` per dir | Single thread by default (`FileListWalker`); `jwalk` parallel reader available via `engine::walk::WalkdirWalker` but not the default flist path |
| Per-entry `stat` | Caller-supplied `STRUCT_STAT *stp` reused when available; otherwise one `lstat` per entry | One `lstat` per entry, no reuse from `readdir` `d_type` |
| Loop-detection | Visited-set via inode-pair check inside `send_if_directory` only when `keep_dirlinks` | `HashSet<PathBuf>` of canonicalised paths, populated on every directory entry |
| Buffer management | One global flist array grown by `flist_expand` | `Vec<FileListEntry>` per call, one allocation per growth event |
| Path representation | `char *dirname` + `char *basename` (interned via `dirname_cache`) | `PathBuf` per entry; `PathInterner` exists but is used by the *receiver* decode path, not by the sender's `FileListWriter` |
| Output buffering | `io_start_buffering_out(f)` line 2240 wraps the wire in a 32 KiB buffer | Caller supplies the writer; `core::session()` does install a `BufWriter`, but the writer crate is unaware of this and emits one `write_all` per field |

The headline gap is the absence of a sender-side `dirname_cache`
analogue. Upstream's `lastdir` / `lastdir_len` static (line 2193)
combined with the `dirname_cache` interner reuses one allocation per
unique directory across the whole transfer. oc-rsync's
`PathInterner` is structurally equivalent but lives in
`protocol::flist` and is invoked by the receiver, not by
`FileListWriter` on the sender side. For a 100K tree with 1K
distinct directories this is 99K redundant `PathBuf` allocations on
the sender.

## 4. Throughput improvement candidates

Ranked by expected throughput payoff at 100K entries on a Linux
host. None require a wire-format change.

### 4.1 Default to the batched-stat path

`collect_entries` is sequential despite the misleading name (line 41
of `parallel.rs`). The session driver reaches it via
`FileListBuilder::build` -> `FileListWalker` -> `walker.collect()`
(line 42), which does one `lstat` per entry on a single thread.
Switching the default driver to `collect_with_batched_stats` (line
236) gives 16-shard rayon parallelism on the stat phase for free,
with the existing test coverage in `parallel.rs:574-636`. Expected
payoff on a 100K tree with cold dcache is a 4-8x reduction in the
stat-bound segment of the build, dominated by syscall latency
amortisation across cores. Risk is low - the post-sort
(`sort_file_entries`) preserves the wire-deterministic order. The
work is ~20 lines: replace the `collect_entries` call inside
`core::session()` with a feature-gated call to
`collect_with_batched_stats`, propagating the `Vec<(PathBuf,
io::Error)>` error variant through `FileListError`.

### 4.2 Reuse `dirent.d_type` to skip per-entry `stat`

`prepare_entry` calls `fs::symlink_metadata` for every entry
unconditionally. On Linux and BSD, `getdents64` already returns
`d_type` for most filesystems (ext4, xfs, btrfs, tmpfs). When
`d_type == DT_REG | DT_DIR` and the caller does not need
`uid/gid/mtime/mode` immediately, the per-entry `stat` can be
deferred or skipped entirely. Concretely: change
`DirectoryState::new` to capture `dirent.d_type` alongside the name,
plumb it into `FileListEntry`, and only call `lstat` lazily when
`FileListWriter::write_entry` actually reads the field. For non-
checksum, non-times transfers this could elide the entire per-entry
stat. Expected payoff: 30-50% wall-clock reduction on cold-cache
100K builds. Risk: filesystems that return `DT_UNKNOWN` (NFSv3,
some FUSE) need a fallback - `fs::symlink_metadata` becomes the slow
path, not the default. Implementation cost: ~150 lines, mostly in
`file_list_walker.rs` and `lazy_metadata.rs`.

### 4.3 Apply `PathInterner` to the sender side

`PathInterner` (`crates/protocol/src/flist/intern.rs:42-114`) is
already production code on the receiver. Wiring it into
`FileListWriter` requires capturing the parent dirname for each
entry once via `Path::parent`, calling `interner.intern(parent)`,
and storing the resulting `Arc<Path>` in `FileEntry` instead of
embedding a fresh `PathBuf` in every entry's `relative_path`. For a
100K tree with 1K distinct directories this drops dirname
allocations from 100K `PathBuf` to 1K `Arc<Path>`, plus one
`HashMap` probe per entry. Expected payoff: ~5-10% memory reduction,
~3-5% CPU reduction from cache-locality on `same_len` prefix
compression (the previous-name buffer benefits when consecutive
entries share their dirname). Risk: low - the interner has tests at
`intern.rs:117-205` and `Arc<Path>` is `Send + Sync`.

### 4.4 Batched `BufWriter` flush boundaries

`write_entry` calls `Writer::write_all` between 4 and 12 times per
entry. With a `BufWriter` upstream this is fine, without one this
is one `write(2)` per field. The writer module currently has no
opinion on buffering - the contract is "give me any `Write`". Two
options without an API change: (a) document that callers MUST wrap
in a buffered writer with at least 32 KiB, or (b) collect each
entry's bytes into a small `Vec<u8>` (typed-arena allocator
preferred) and flush once at the end of `write_entry`. Option (b)
matches upstream's behaviour at `flist.c:2240` (`io_start_buffering_out`)
and protects callers who forget the wrapper. Expected payoff: 2-3x
on the encode phase when the writer is unbuffered. Risk: the
typed-arena needs a reset between entries, but the byte budget is
bounded (one varint chain plus the name suffix, max ~300 bytes).
Implementation cost: ~80 lines.

## 5. Reproducing the numbers

A criterion bench landing at `crates/flist/benches/build_100k.rs`
must produce three series before any of the candidates in section 4
land: sequential walker (`collect_entries`), batched stat
(`collect_with_batched_stats`), and chunked parallel
(`collect_paths_chunked_parallel`). Each series sweeps tree shapes:
flat 100K, balanced 100x100x10, deep 10x10x10x10x10. Numbers must
include cold-dcache (post `echo 3 > drop_caches` on Linux) and warm
runs. Output goes to `target/criterion/flist_build_100k/` and is
folded into the next perf report.

## 6. References

- Task #1044.
- Upstream send path: `flist.c:380-750` (entry encode), `flist.c:1820-1890`
  (directory drive), `flist.c:2192-2532` (top-level).
- oc-rsync send path: `crates/flist/src/file_list_walker.rs`,
  `crates/flist/src/parallel.rs`, `crates/protocol/src/flist/write/mod.rs`,
  `crates/protocol/src/flist/intern.rs`, `crates/protocol/src/flist/hardlink/table.rs`.
- Threshold constants: `crates/transfer/src/parallel_io.rs:13-33`.
