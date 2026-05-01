# Profiling audit: 100k-small-file workload

Last verified: 2026-05-01 against `crates/transfer/src/receiver/transfer.rs`,
`crates/transfer/src/receiver/transfer/{candidates,pipeline}.rs`,
`crates/transfer/src/receiver/directory/creation.rs`,
`crates/transfer/src/generator/{transfer.rs,delta.rs,file_list/{walk,batch_stat}.rs}`,
`crates/transfer/src/disk_commit/{process.rs,thread.rs}`,
`crates/transfer/src/parallel_io.rs`, `crates/protocol/src/flist/sort.rs`,
`crates/metadata/src/apply/mod.rs`, and `scripts/benchmark_100k.sh`.

No matching open tracking issue was found via `gh issue list` for "100k
profiling" or "many small files"; this audit is filed standalone.

## Scope

Predict where wall-clock time is spent when oc-rsync transfers a workload
shaped like `scripts/benchmark_100k.sh` (100,000 files, 1-4 KiB each, across
1,000 directories), and recommend a profiling methodology runnable inside the
`rsync-profile` podman container. The goal is to point future perf work at the
right code rather than re-derive hot paths from scratch each cycle.

## Workload characterization

A 100k 1-4 KiB workload is dominated by per-file fixed costs, not bytes/s:

- Sender file-list build: `readdir` per directory, `lstat` per entry,
  `make_file()` per entry, then sort + wire encode.
- Receiver candidate selection: one `stat()` per existing destination file.
- Per-file protocol round-trip: NDX + iflags + sum-head + (often empty)
  signature + delta tokens + file checksum.
- Per-file commit: `open(O_TMPFILE)` or named temp, small write, optional
  `fsync`, `linkat`/`rename`, then `chmod`/`chown`/`utimensat` (and optional
  `setxattr` for ACL/xattr).
- Allocator/path pressure: each entry produces at least one `PathBuf`
  (`dest_dir.join(entry.path())`), often duplicated across phases.

Phase 2 (no-change) should short-circuit at quick-check and never touch the
disk-commit thread; phase 3 (10 % modified) still pays the full file-list
build but only transfers ~10k files.

## Predicted hot paths

Cited functions are the most likely consumers of CPU/wall time. Citations are
file:line as of the commit listed at the top.

1. **Sender file-list construction.**
   `crates/transfer/src/generator/file_list/walk.rs:209`
   (`scan_directory_batched`) feeds children to `batch_stat_dir_entries`
   (`crates/transfer/src/generator/file_list/batch_stat.rs:38`). With 100
   files per directory the per-dir batch crosses
   `ParallelThresholds::stat = 64` (`crates/transfer/src/parallel_io.rs:16`),
   so stat parallelism is engaged - but `read_dir`, `make_file` allocations,
   and per-entry filter evaluation
   (`crates/transfer/src/generator/file_list/walk.rs:106`) all run on one
   thread. Sort is not the dominant cost but is non-trivial; precomputed at
   `crates/protocol/src/flist/sort.rs:222`.

2. **Receiver candidate selection.**
   `crates/transfer/src/receiver/transfer/candidates.rs:34`
   (`build_files_to_transfer`) does a parallel `fs::metadata()` over every
   regular-file entry, then sequentially calls `quick_check_matches`
   (`crates/transfer/src/receiver/quick_check.rs`) and, for matches,
   `apply_metadata_with_cached_stat` plus optional ACL/xattr application. On
   a no-change run this is essentially the entire wall-clock time: 100 K stat
   calls plus 100 K `utimensat`/`chmod` calls.

3. **Pipelined per-file dispatch (receiver).**
   `crates/transfer/src/receiver/transfer/pipeline.rs:140`
   (`run_pipeline_loop_decoupled`) fills a sliding window, parallel-computes
   basis signatures above `ParallelThresholds::signature = 32`
   (`pipeline.rs:182`), then streams responses to the disk thread. Each
   iteration pays a `PathBuf` clone into `pending_files_info` and a
   `find_basis_file_with_config` call that, on a greenfield destination,
   reduces to one `fs::metadata`.

4. **Per-file commit syscall chain (disk thread).**
   `crates/transfer/src/disk_commit/process.rs:31` (`process_file`) runs
   `open_output_file` -> chunked `write` -> optional `fsync` -> `linkat`
   (`O_TMPFILE`) or `rename` -> `apply_metadata_from_file_entry`
   (`crates/metadata/src/apply/mod.rs:218`). For 1-4 KiB files this is 3-5
   syscalls per file *before* metadata. The recently merged `IoUringDiskBatch`
   wiring (PR #3452, `disk_commit/thread.rs`) batches writes only, not the
   rename/fsync/metadata tail (see
   `docs/audits/disk-commit-iouring-batching.md`).

5. **Directory metadata application.**
   `crates/transfer/src/receiver/directory/creation.rs:35`
   (`create_directories`) creates ~1 K directories sequentially then applies
   metadata via `map_blocking` (`ParallelThresholds::metadata = 64`). One-shot
   cost; worth confirming but unlikely to dominate.

## Existing optimizations already in place

- Parallel stat for dir walks: `generator/file_list/batch_stat.rs:38` (threshold 64).
- Parallel metadata for created dirs: `receiver/directory/creation.rs:122` (64).
- Parallel signature computation: `receiver/transfer/pipeline.rs:182` (threshold 32).
- Sender `read_exact` with 256 KiB reusable buffer: `generator/delta.rs:221-262`.
- Sender flushes only before blocking on next NDX: `generator/transfer.rs:135`.
- Decoupled disk-commit thread + lock-free SPSC ring: `pipeline/spsc.rs`.
- Lock-free buffer pool acquire/release (PR #3389).
- `O_TMPFILE` preferred on Linux (PR #3209).
- Per-file checksum verification moved to disk thread:
  `disk_commit/process.rs:55-58`.
- File-list sort key precomputation: `protocol/flist/sort.rs:222`.

## Recommended profiling methodology

Run inside the `rsync-profile` podman container (`podman exec -it rsync-profile
bash`). The workspace is bind-mounted at `/workspace`; binaries live at
`target/release/oc-rsync` and `/usr/local/bin/rsync` (3.4.1).

```sh
# Generate the workload once; reuse for all profiling runs.
SRC=$(mktemp -d) && DST=$(mktemp -d)
bash /workspace/scripts/benchmark_100k.sh   # populates a workload internally
# Or, for a stable corpus, replicate its loop manually into $SRC.

# 1. Syscall histogram (cheap; identifies which syscall class dominates).
strace -c -f -o /tmp/oc-strace.txt /workspace/target/release/oc-rsync -a $SRC/ $DST/
strace -c -f -o /tmp/up-strace.txt rsync -a $SRC/ $DST/
diff /tmp/up-strace.txt /tmp/oc-strace.txt

# 2. Linux perf flame graph for CPU time.
perf record -F 997 -g --call-graph dwarf -o /tmp/oc.data \
  /workspace/target/release/oc-rsync -a $SRC/ $DST/
perf script -i /tmp/oc.data | stackcollapse-perf.pl | flamegraph.pl > /tmp/oc.svg

# 3. Phase-2 (no-change) profile - isolates quick_check + metadata apply.
/workspace/target/release/oc-rsync -a $SRC/ $DST/   # warm up
perf stat -e task-clock,cycles,instructions,cache-misses,page-faults \
  /workspace/target/release/oc-rsync -a $SRC/ $DST/

# 4. Phase-3 (10 % modified): re-run benchmark_100k.sh which already covers it.
```

For wall-clock comparison against upstream the existing harness is
`scripts/benchmark_100k.sh` (3 runs, median). It already captures peak RSS via
`/usr/bin/time -v` on Linux.

## Suspected bottlenecks worth verifying

- **`fs::metadata` x100 K in `build_files_to_transfer`.** Hypothesis: stat
  syscall latency dominates phase 2. Test: count `newfstatat` in `strace -c`;
  expect ~1 per file. Linux 5.6+ could batch via `IORING_OP_STATX`.
- **`PathBuf::join` allocation pressure.** Each file passes through at least
  three `dest_dir.join(entry.path())` sites (candidates, dir creation,
  pipeline). With 100 K files that is 300 K small allocations. Test: dhat or
  heaptrack on a phase-2 run; look for `PathBuf::push` in top-N.
- **Per-file metadata chain on the disk thread.** Even with batched writes,
  `apply_metadata_from_file_entry` issues 3-4 syscalls per file
  (`utimensat`, `fchmodat`, `fchownat`, optional `setxattr`) sequentially.
  Test: `strace -c -e trace=%file` filtered to the disk thread PID.
- **Filter chain evaluation on the sender.** Even with no rules,
  `FilterChain::allows` is called per entry and copies the relative path.
  Test: `perf annotate` `walk_path_with_metadata`.
- **Buffer return latency under SPSC.** With 1-4 KiB chunks most data lands
  below the buffer pool's reuse threshold. Test: counters in `pipeline/spsc.rs`
  or `dhat` on `Vec<u8>` allocations.

## Out of scope / future work

- Any change that requires a wire-protocol extension (per project memory:
  no protocol additions for niche perf wins). For example, a "many-tiny-files
  bundle frame" would be wire-incompatible and is explicitly ruled out.
- Cross-platform parity for io_uring statx batching: `fast_io` is the only
  permitted home for that unsafe code, and macOS / Windows have no equivalent.
- Re-architecting the disk-commit thread as a thread pool: rsync's wire
  ordering constrains commit order anyway, and upstream commits sequentially.
