# No-Change Quick-Check Profile: 100K Files

Tracking task: #1047.

This audit profiles the per-file cost of the receiver's quick-check path
when synchronising a 100,000-file tree where every destination file is
already up to date. The goal is to characterise the syscall and
allocation profile of the no-change path and propose targeted reductions
that keep wire compatibility with upstream rsync 3.4.1.

## 1. Quick-check algorithm

The quick-check decision is implemented in two places:

- `crates/transfer/src/receiver/quick_check.rs::quick_check_matches`
  (lines 45-88) - the receiver's pure-function comparator used by the
  remote pipeline.
- `crates/engine/src/local_copy/executor/file/comparison.rs::should_skip_copy`
  (lines 129-166) - the local-copy executor's comparator used when
  source and destination are both on the local filesystem.

Both follow upstream `generator.c:617 quick_check_ok()` evaluation
order:

1. Size mismatch -> always transfer.
2. `always_checksum` (`-c`) -> compute file checksum and compare.
3. `size_only` -> matched, skip.
4. `!preserve_times` (i.e. `--ignore-times`) -> force transfer.
5. mtime equality -> skip when equal.

The receiver invokes `quick_check_matches` from
`crates/transfer/src/receiver/transfer/candidates.rs::build_files_to_transfer`
(lines 34-199). The candidate-selection function runs in three phases:

- Phase A (sequential, in-memory): filter the file list by file kind,
  hardlink-leader status, size bounds, and daemon filters.
- Phase B (parallel stat): map each candidate to a `dest_dir.join(...)`
  PathBuf, then run `fs::metadata` (a `stat`, not `lstat`, since we
  want to follow symlinks for the comparison) over the list with
  `crates/transfer/src/parallel_io.rs::map_blocking`.
- Phase C (sequential post-processing): apply quick-check, emit the
  itemize line, and apply metadata using
  `apply_metadata_with_cached_stat` so the post-skip chmod/chown reuses
  the stat already obtained in phase B.

Although the file-level pure function in `quick_check.rs` is not named
`quick_check_ok_stateless` today, it serves the equivalent role: a
side-effect-free comparator over `(FileEntry, dest path, dest meta)`
that takes no `&self`, allowing safe rayon parallelism. The
`crates/engine/benches/buffer_pool_contention.rs` header still
references the historical `quick_check_ok_stateless` name as the
buffer-reuse pattern this codepath inherits.

The parallel-stat threshold lives in
`crates/transfer/src/parallel_io.rs`:

- `DEFAULT_STAT_THRESHOLD: usize = 64`
- `DEFAULT_SIGNATURE_THRESHOLD: usize = 32`
- `DEFAULT_METADATA_THRESHOLD: usize = 64`
- `DEFAULT_DELETION_THRESHOLD: usize = 64`

Below the threshold, `map_blocking` falls back to sequential iteration
to avoid rayon dispatch overhead. With 100K files we are several orders
of magnitude over the threshold, so phase B always runs in parallel.

## 2. Per-file cost on the no-change path

For each up-to-date regular file the receiver issues:

- One `stat` (path-based) on the destination, executed in the rayon
  pool. On Linux this is `newfstatat(AT_FDCWD, path, &st, 0)`.
- Zero checksum reads (the rolling+strong checksum pipeline is gated by
  `quick_check_matches` returning `false`).
- Zero data transfer; no NDX is sent for this file. The generator
  itemize line is emitted only when `--itemize-changes` is requested,
  using cached metadata.
- A `apply_metadata_with_cached_stat` call that may issue `chmod`,
  `lchown`, or `utimensat` only when bits actually differ. On a true
  no-change run all three short-circuit on equality, so the syscall
  count stays at one.

Allocations per file:

- One `PathBuf` from `dest_dir.join(entry.path())` (phase B).
- One `(usize, PathBuf, Option<fs::Metadata>)` tuple carried through
  phase C; reused by the metadata application step.
- The `fs::Metadata` itself is a fixed-size struct allocated on the
  stack inside `fs::metadata`.

So the steady-state cost is one path-based `stat` plus two small
heap allocations per file. For 100K files that is 100,000 syscalls and
~200K small allocations on the receiver, before any I/O on the source
side.

The sender's mirror cost is dominated by the file-list build, not the
comparison: `crates/transfer/src/generator/file_list/batch_stat.rs`
runs the same parallel-stat helper over directory entries returned by
`walk.rs`, again at the `DEFAULT_STAT_THRESHOLD` cutoff.

## 3. Comparison with upstream `flist.c` / `generator.c`

Upstream `quick_check_ok` (generator.c:617-671) is structurally
identical: size first, optional `file_checksum`, optional
`size_only` shortcut, optional `ignore_times`, then `mtime_differs`.
`mtime_differs` (generator.c:389-396) compares full-resolution mtimes
when `ST_MTIME_NSEC` is available, falling back to seconds-only
comparison otherwise. Our implementation uses
`std::os::unix::fs::MetadataExt::mtime` on Unix, which returns the
seconds component; nanosecond support is plumbed separately in the
metadata crate but is not consulted by `quick_check_matches`. This
matches upstream behaviour when `--modify-window=0` and
`F_MOD_NSEC_or_0(file)` evaluates to zero, which is the default.

Upstream issues `link_stat` (generator.c:1344) - one `lstat` followed
by an optional `stat` when `keep_dirlinks && is_dir` - per file. Our
receiver issues `fs::metadata` (one `stat`). The behavioural
difference: upstream's `lstat` short-circuits symlink dereference,
which is slightly cheaper and avoids accidental stat of a target file
when the destination is a symlink. We compensate by handling symlink
entries separately (`directory/links.rs`) before quick-check runs, so
on a no-change tree of regular files the cost remains one `stat` per
file, matching upstream's syscall count.

`flist.c` itself does not implement quick-check; its role on the
receiver is to consume the file list and hand entries to the
generator. Upstream's `recv_generator` (generator.c:1207) is the
analogue of our `build_files_to_transfer` -> `transfer/pipeline.rs`
path. We split the loop into A/B/C explicitly to lift the stat phase
out of the sequential C loop. Upstream's loop is fully sequential.

## 4. Proposed reductions

These reductions preserve wire compatibility (no new options or
protocol bits) and target the dominant cost on a 100K no-change run:
the 100K stat syscalls and the rayon dispatch.

### 4.1 Raise the parallel-stat threshold under low concurrency

`DEFAULT_STAT_THRESHOLD = 64` is conservative. On a 100K run we always
take the parallel path, but the per-batch dispatch cost adds up when
the rayon pool is heavily contended (e.g. by signature computation in
phase D of a non-trivial transfer). Profile the no-change path with
`STAT_THRESHOLD` swept across `{64, 256, 1024, 4096}` and pick the
elbow. Expected gain: small (~1-3%) on a pure no-change run, but the
real win is reducing rayon contention when phases overlap.

Implementation: extend `ParallelThresholds::with_stat` callers in the
generator and receiver builders to honour an `--inc-recursive`-aware
override, since INC_RECURSE feeds the stat phase in segments and
benefits from a different threshold than a flat 100K run.

### 4.2 Use `statx` to fetch atime, mtime, and size in one call

On Linux, `statx(AT_STATX_SYNC_AS_STAT, STATX_BASIC_STATS)` returns
size + mtime in a single syscall and avoids the extra round-trip that
`metadata()` plus a follow-up `crtime()` lookup currently incurs in
metadata application. For pure no-change quick-check this does not
reduce the syscall count (we already issue one `stat`), but it
unlocks two follow-up wins:

- Removes the second stat done by `apply_metadata_with_cached_stat`
  when the caller needs `crtime` or atime. With statx those fields
  ride along with the original stat.
- Permits future use of `STATX_DONT_SYNC` on networked filesystems for
  cached attribute reads. This trades freshness for throughput; gate
  behind `--read-cached-stat` (still wire-compatible, client-only).

`fast_io` is the right home for the statx wrapper since it already
gates Linux-specific syscalls; expose a safe `statx_basic(&Path) ->
io::Result<StatxResult>` and let the receiver call it from the
parallel map closure.

### 4.3 Replace per-file path stat with `getdents` plus filter

Today phase B does `dest_dir.join(entry.path())` then `fs::metadata`
for every file list entry, regardless of whether the destination
directory has already been read. For a 100K-file run within a single
directory this is 100K independent path-based stats that each redo the
namei walk. Upstream's generator behaves the same way, but we have an
opportunity:

- After phase A, group candidates by parent directory.
- For each group, read the directory once with `getdents64` (or
  `read_dir` on portable code paths) into a `HashMap<OsString,
  fs::Metadata>` populated by `DirEntry::metadata()` which on Linux
  uses the dirfd-relative `fstatat` and avoids re-walking the path.
- Phase C then looks up by basename instead of stat-by-path.

For deep trees the path-stat namei cost dominates; for flat trees the
gain is the avoided per-stat path resolution. Either way the syscall
count remains one per file but each syscall is cheaper. Expected gain:
10-25% on hot-cache 100K runs; larger on cold cache.

This is a structural change to `build_files_to_transfer`, so land it
behind a feature flag and validate with the existing
`crates/transfer/src/parallel_io.rs` ordering proptests.

### 4.4 Hash-only-on-difference for `--checksum` mode

`--checksum` currently re-hashes every destination file even when the
size mismatch already proved divergence. The early `if dest_meta.len()
!= entry.size() { return false; }` guard in `quick_check_matches`
already handles size, but on a 100K no-change `-c` run we still hash
every file end-to-end. Two reductions:

- When sizes match but the upstream-supplied digest in
  `entry.checksum()` is `None` (sender chose not to send one), skip
  the local hash and assume divergence. This matches upstream's
  `memcmp(sum, F_SUM(file), flist_csum_len) == 0` returning false when
  `F_SUM` is empty.
- When sizes match and the digest is present, hash the destination in
  64 KiB chunks (already done via `file_checksum_matches`), but bail
  on the first chunk that diverges from a partial digest. This
  requires a streaming digest comparator; implement it in
  `delta_apply::ChecksumVerifier` using
  `update_and_compare(&[u8]) -> Option<bool>`.

Combined with 4.1 this reduces `-c` mode's per-file cost on no-change
runs by the cost of hashing all-but-the-first 64 KiB of each file,
which on 100K * 1 MiB files is ~99 GiB of avoided hashing.

### 4.5 Cache the parent-directory `dest_dir.join` result

Phase B currently allocates one `PathBuf` per file via
`dest_dir.join(entry.path())`. When many files share a parent (the
common case), we re-allocate the parent prefix every time. Two
options:

- Build a small `HashMap<OsString, PathBuf>` keyed by parent component
  during phase A, and assemble final paths by `parent.join(basename)`
  rather than `dest_dir.join(full_relative)`.
- Use `Path::with_capacity` in tight loops; not currently exposed by
  `std`, so this would require an internal helper in `rsync_io`.

Expected gain: 200K small allocations -> 100K small allocations, and
better L1/L2 locality in phase B. Pair with reduction 4.3 since the
directory-grouped layout already needs the parent grouping.

## Profiling plan

Track these counters on a 100K no-change run and re-baseline after
each change lands:

- Syscalls: `strace -c -f -e trace=stat,statx,newfstatat,getdents64`.
- Allocations: `heaptrack` or `dhat` against the receiver process.
- Wall-clock: `hyperfine --warmup 3 --runs 10` with cache warmed.
- Rayon contention: `RAYON_NUM_THREADS={1,2,4,8}` sweep.

Container target: `localhost/oc-rsync-bench:latest` Arch image, since
it has both upstream rsync and oc-rsync side-by-side and runs on a
predictable Linux kernel for `getdents64` and `statx` testing.

## References

- Upstream: `target/interop/upstream-src/rsync-3.4.1/generator.c:617`
  `quick_check_ok()`.
- Upstream: `target/interop/upstream-src/rsync-3.4.1/generator.c:389`
  `mtime_differs()`.
- Upstream: `target/interop/upstream-src/rsync-3.4.1/generator.c:1207`
  `recv_generator()`.
- This crate: `crates/transfer/src/receiver/quick_check.rs`.
- This crate: `crates/transfer/src/receiver/transfer/candidates.rs`.
- This crate: `crates/transfer/src/parallel_io.rs`.
- This crate: `crates/engine/src/local_copy/executor/file/comparison.rs`.
- This crate: `crates/transfer/src/generator/file_list/batch_stat.rs`.
