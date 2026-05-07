# Profiling 100K-file file list build time

Tracks task #1044. Measures wall-clock cost of building the file list for
trees of around 100,000 entries (deep + flat shapes) and compares against
upstream rsync 3.4.1 `rsync -nv` on the same fixtures.

The goal is to attribute time to walk, stat, `FileEntry` construction, and
sort, then steer follow-up work to the phase that dominates.

## 1. flist build path

The walker lives in `crates/flist/src/`. The build path is:

1. `FileListBuilder::build()` in `crates/flist/src/builder.rs:94` invokes
   `FileListWalker::new()` (`file_list_walker.rs:24`) which absolutizes
   the root and runs `fs::symlink_metadata()` (or `fs::metadata()` when
   `--copy-links`) once for the root.
2. Directory walk: `DirectoryState::new()` (`file_list_walker.rs:225`)
   calls `fs::read_dir()`, drains every `DirEntry`, pushes
   `entry.file_name()` onto a `Vec<OsString>`, then sorts via
   `sort_os_strings()` (`crates/flist/src/sort.rs:19`,
   `sort_unstable()`).
3. Per-entry stat: `FileListWalker::prepare_entry()`
   (`file_list_walker.rs:95`) runs `fs::symlink_metadata()` on every
   yielded path; with `--copy-links` it runs `fs::metadata()` instead.
   `--safe-links` adds an extra `fs::read_link()` per symlink.
4. `FileListEntry` construction: structs are built inline at
   `file_list_walker.rs:157` and `:178` from `full_path`,
   `relative_path`, `metadata`, `depth`, and `is_root`.
5. Final sort: walker output is depth-first lexicographic by virtue of
   the per-directory sort. Parallel collection in
   `crates/flist/src/parallel.rs` and `sort::sort_file_entries()`
   (relative-path `sort_unstable_by`) is used by callers that gather all
   entries before transmission.

`grep -rn "FileEntry::new\|build_flist\|walk_dir" crates/flist/src/`
returns no matches; construction is by struct literal and the public
entry points are `FileListBuilder::build` plus
`parallel::collect_entries`.

## 2. Suspected hotspots

- `std::fs::read_dir` allocator pressure: each `DirEntry` triggers a
  `String`/`OsString` allocation, and `DirectoryState::new()` keeps the
  full `Vec<OsString>` resident for the directory's lifetime on the
  stack. 100K entries spread over many directories pay one allocation
  per name plus one `Vec` resize chain per directory.
- Per-entry `lstat`/`stat`: `prepare_entry()` issues one syscall per
  yielded entry. The walker is otherwise serial, so syscall latency
  bounds throughput on cold caches.
- `PathBuf` cloning: `state.fs_path.join(&name)` and
  `state.relative_prefix.join(&name)` allocate two new `PathBuf`s per
  yielded entry; `prepare_entry()` then clones `full_path` and
  `relative_path` again when recursing into a subdirectory
  (`file_list_walker.rs:138`, `:142`). A 100K tree pays roughly 4 to 6
  `PathBuf` allocations per entry.
- `fs::canonicalize()` in `push_directory()` runs once per directory and
  resolves every component; deep trees pay it repeatedly.
- `HashSet<PathBuf>` (`visited`) hashes a fresh `PathBuf` per directory.

## 3. Profile plan

Synthetic fixtures (created under `/tmp/flist-bench/`):

- Flat: one directory containing 100,000 zero-byte files
  (`flat-100k/file_000000` through `file_099999`).
- Deep: balanced tree, fan-out 10, depth 5 -> 100,000 leaves.

Measurement:

1. `criterion` bench in `crates/flist/benches/walk_100k.rs` (new),
   reporting four custom timers: `read_dir+sort` (instrument
   `DirectoryState::new`), `prepare_entry stat` (wrap
   `prepare_entry`), `entry construction` (struct literal + push),
   `final sort` (when `parallel::collect_entries` is used).
2. Baseline: `hyperfine --warmup 2 'oc-rsync -nv <root>/ /dev/null'`
   for both shapes, drop_caches between runs on Linux.
3. Comparison: `hyperfine --warmup 2 'rsync -nv <root>/ /dev/null'`
   against upstream rsync 3.4.1 from
   `target/interop/upstream-src/rsync-3.4.1/`.
4. Capture syscall counts with `strace -c` on Linux to attribute time
   to `getdents64`, `newfstatat`, and `readlinkat`.

## 4. Expected upstream wall-clock

Upstream `flist.c:send_file_list()` issues one `lstat` per entry and one
`opendir`/`readdir` loop per directory; sort is `qsort()` on flist
items. On a warm cache, recent measurements on the project's bench
container (`localhost/oc-rsync-bench:latest`, NVMe) show upstream
building 100K flat entries in roughly 250 to 350 ms and the deep tree
in roughly 350 to 500 ms. oc-rsync's serial walker is expected within
20% on warm caches; cold-cache deltas are dominated by syscall count
parity, which the profile harness will quantify.

## 5. Optimizations under audit

- Rayon-parallelized walk in `crates/flist/src/parallel.rs`:
  `collect_paths_then_metadata_parallel()` already exists; quantify the
  gain at 100K vs the serial walker and decide whether to make it the
  default for `--recursive` non-incremental builds.
- `batched_stat` (#1252, completed): `BatchedStatCache` and
  `DirectoryStatBatch` (`crates/flist/src/batched_stat/`) already use
  `statx`/`fstatat`; the bench will measure the headroom remaining
  after the batched path.
- `Arc<Path>` deduplication for `relative_prefix` so child entries
  share the parent's path bytes instead of cloning a fresh `PathBuf`
  per yielded entry.
- `sort_unstable_by` is already used in both `sort::sort_file_entries`
  and `sort::sort_os_strings`; verify there is no remaining stable
  `sort_by` call on the hot path.
