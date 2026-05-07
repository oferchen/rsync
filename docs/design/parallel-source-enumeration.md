# Parallel Source Enumeration via Multi-Producer WorkQueue

Issue: #1573

This note evaluates whether the generator's source enumeration could exploit
the multi-producer `WorkQueue` infrastructure (gated behind the
`multi-producer` cargo feature) to fan out filesystem walking across
disjoint roots, and recommends a prototype path.

## 1. Current single-producer enumeration

The generator builds the file list serially in
`crates/transfer/src/generator/file_list/mod.rs`:

- `GeneratorContext::build_file_list(&[PathBuf])` (line 52) iterates the
  caller-supplied `base_paths` and calls `walk_path(base, base.clone())`
  once per root from a single thread (line 64-66).
- `walk_path` lives in `crates/transfer/src/generator/file_list/walk.rs:32`
  and pushes entries into `self.file_list` and `self.full_paths` in arrival
  order. NDX values are implicit Vec indices; index allocation is monotonic
  by virtue of being single-threaded.
- After the walk completes, `build_file_list` runs an indirect-permutation
  sort (`compare_file_entries`, `sort_by` / `sort_unstable_by` on
  `--qsort`) and only then assigns hardlink indices and collects id
  mappings.

`build_file_list_with_base` (line 124) follows the same pattern for
`--files-from`. Both paths mirror upstream `flist.c:2192 send_file_list()`,
which is also single-threaded.

## 2. Multi-root scenarios where parallel enum could help

Parallel enumeration only makes sense when the generator owns multiple
disjoint roots whose walk costs dominate the build phase:

- `oc-rsync src1/ src2/ src3/ dest/` - three independent CLI args, each
  passed in `base_paths`.
- `--files-from=list` whose entries straddle separate mount points or
  large subtrees.
- Daemon `path = ...` modules backed by union mounts where each top-level
  child is on a different physical device (per-device parallelism beats
  per-tree parallelism).

The win scales with `min(num_roots, available_io_parallelism)`. Single-root
recursion does not benefit unless we also parallelise interior `readdir`
fans, which is out of scope here.

## 3. Multi-producer WorkQueue contract

`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs` already
provides a `Clone` impl on `WorkQueueSender`, gated by the
`multi-producer` feature. Each clone shares the same bounded
`crossbeam_channel`, so:

- N producers can `send` concurrently; the channel applies backpressure
  uniformly when the queue is full.
- The channel is FIFO **per producer**, not across producers. Cross-thread
  ordering is determined by send timing, so wire order is **not**
  preserved automatically.
- Sequence numbers must be assigned externally; `WorkQueue` does not mint
  them. The `multi_producer_audit.rs` notes (Opportunity 1, lines 38-51)
  flag this explicitly: "Even with parallel generators, they must merge
  into wire order before transmission."

For source enumeration this is acceptable because `build_file_list`
already runs a global sort (`compare_file_entries`) before the file list
is transmitted. The walk phase therefore does not need to preserve
arrival order; it only needs every entry to land in `self.file_list`
exactly once.

## 4. Risks

- **NDX monotonicity.** Today NDX equals the push position in
  `self.file_list`. Concurrent producers must not race on this Vec.
  Two viable mitigations:
  1. *Producer-id offsets.* Pre-partition the NDX space into per-root
     ranges (`root_i` owns `[i * STRIDE, (i + 1) * STRIDE)`) and reject
     overflows. Simple but fragile when one root vastly outsizes others.
  2. *Sort-merge.* Each producer pushes into a thread-local `Vec`; the
     main thread concatenates and runs the existing
     `compare_file_entries` sort. NDX is then assigned post-sort. This
     matches upstream `flist.c:f_name_cmp()` semantics exactly and is the
     preferred path.
- **Hardlink dev/inode tables.** `assign_hardlink_indices` (called after
  the sort) assumes a single shared map. Per-producer maps must be
  merged before this step; collisions across roots are legal and must
  resolve to the lowest NDX, matching upstream `hlink.c:match_hard_links()`.
- **`io_error` accumulation.** Each producer's walk errors must funnel
  into the generator's shared `io_error` flag without dropping any.
- **INC_RECURSE segments.** With `--inc-recurse` the sender emits
  per-directory segments incrementally; parallel enumeration must
  serialise segment boundaries to keep dirstack ordering compatible with
  upstream `flist.c:send_dir_list()`.
- **Filter chain reentrancy.** `FilterChain` is `Sync` for read-only
  matching, but `.rsync-filter` merge files mutate per-directory state.
  Parallel walkers need cloned filter cursors per root.

## 5. Recommendation

Prototype the feature behind a `--parallel-enumerate` CLI flag (off by
default, wired through `CoreConfig`) and the existing `multi-producer`
cargo feature so production builds remain SPMC until the prototype is
validated:

1. Add `--parallel-enumerate` to `cli` with a hidden flag attribute and
   thread it into `GeneratorContext::config`.
2. When enabled and `base_paths.len() > 1`, fan out per-root `walk_path`
   onto a `rayon::scope`, each pushing into a thread-local
   `(Vec<FileEntry>, Vec<PathBuf>)`.
3. Concatenate results, run the existing indirect-permutation sort
   (Section 3 sort-merge path), then assign hardlink indices unchanged.
4. Add interop tests against upstream rsync 3.4.1 covering multi-root
   `oc-rsync src1/ src2/ src3/ dest/` and `--files-from` with
   cross-mount entries; verify byte-identical wire output to the
   single-producer path.
5. Benchmark on the `rsync-profile` container with cold caches against
   3+ roots totaling ~1 M entries; promote out of `--parallel-enumerate`
   only if the wall-clock win exceeds 15% without regressing single-root
   workloads.
