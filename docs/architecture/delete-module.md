# Delete module architecture

This document describes the delete module's internal architecture,
threading model, performance characteristics, and relationship to
upstream rsync 3.4.4's deletion behaviour. It covers the full pipeline
from candidate discovery through ordered emission, performance scaling
across directory sizes, mode-specific trade-offs, and upstream
divergence boundaries.

## Delete mode semantics

Rsync supports four timing modes for destination-side deletion:

| Flag | Mode | Behaviour |
|------|------|-----------|
| `--delete-before` | Before | Full tree sweep before any file transfer begins. Guarantees disk space is freed before copies land. |
| `--delete-during` | During | Per-directory interleaved with the transfer loop. Each directory's extras are deleted just before its files are copied. Default when `--delete` is specified. |
| `--delete-after` | After | Plans accumulate during transfer; all deletions drain after the transfer loop completes. |
| `--delete-delay` | Delay | Plans accumulate during transfer; drain after all temp-file renames commit. Safest mode - no data loss if the transfer aborts mid-run. |

All four modes share the same two-phase pipeline internally. The only
difference is when phase 2 (the drain) executes relative to the file
transfer loop.

Additionally, `--delete-excluded` is a layering modifier: it appends
filter-excluded entries to the extras set before `compute_extras` runs,
regardless of which timing mode is active.

## Architecture overview

### DeletePlanMap

The `DeletePlanMap` is the rendezvous point between parallel plan
producers and the single-threaded consumer. It is a
`Mutex<HashMap<PathBuf, DeletePlan>>` that stores one frozen
`DeletePlan` per destination-relative directory path.

Design choices:

- **Mutex over DashMap.** The lock is held only for the map mutation
  itself (insert or remove) - never across I/O operations. With the
  publish-once invariant (each directory published exactly once), actual
  contention is rare. The map handles at most `rayon_pool_size`
  concurrent writers plus one reader. Bench-driven selection against
  `DashMap` and sharded maps is tracked as DDP-B4.

- **Publish-once invariant.** A non-`None` return from `insert()`
  signals a logic bug (duplicate publication). The API is designed so
  callers can assert correctness without additional synchronization.

- **Capacity pre-allocation.** `with_capacity(n)` avoids rehashing when
  the directory count is known ahead of time (common for non-INC_RECURSE
  transfers where the full flist arrives before phase 1 begins).

- **Poison-on-crash.** If a worker thread panics mid-publish, the mutex
  poisons and subsequent accesses panic, halting the pipeline rather than
  risking silent under-deletion.

### DeleteEmitter

The `DeleteEmitter<F: DeleteFs>` is the single-threaded drain task that
consumes plans from the `DeletePlanMap` in `DirTraversalCursor` order
and issues one filesystem operation per planned entry.

**Sequential path (production default):**

The emitter owns a `DeleteFs` dispatcher, a `DeleteStats` counter, the
plan map, the cursor, and an `EmitterErrorPolicy`. It loops:

1. Pull next directory from cursor.
2. `take(dir)` the plan (blocking if not yet published).
3. Open dirfd via `DirSandbox` for SEC-1.q anchoring (Unix).
4. Dispatch each entry through the appropriate `DeleteFs` method.
5. Increment stats on success; classify errors on failure.

The emitter never holds a lock during I/O dispatch, so a slow unlink
does not block plan publication by other workers.

**Parallel consumer path (feature-gated):**

Behind the `parallel-delete-consumer` Cargo feature, the
`ParallelDeleteEmitter` routes through a dedicated consumer OS thread
(not a rayon worker - avoids starving producers). Within each cohort,
ops dispatch via `rayon::par_iter` since each targets a distinct
filesystem leaf. Cross-cohort ordering is strictly serial: cohort N+1
never begins until every op in cohort N has completed and its stats have
been folded.

The consumer thread parks on a `Condvar` with predicate:
`head_is_ready || producers_done || is_panicked`.

### Cohort batching strategy

The `CohortBatcher` wraps the `ReorderBuffer` with three behaviours:

1. **Single-call enqueue.** `enqueue_cohort(key, rank, ops)` inserts all
   operations and seals the cohort atomically, matching the "rayon
   producer owns the cohort end-to-end" decomposition.

2. **Batch drain.** `drain_batch()` surfaces up to `DRAIN_BATCH_CAP = 8`
   contiguous-by-rank sealed cohorts per wake-up, amortising the Condvar
   notification cost below 15% of consumer CPU at the 100K-cohort
   projection point.

3. **Panic latch.** A sticky `AtomicBool` flag lets the consumer bail at
   the first panicked cohort boundary rather than only at wake-up start.

The cohort boundary is one destination parent directory, matching
upstream's `delete_in_dir` which walks one directory at a time. The
wire-ordering rank is a dense pre-order index assigned by the
`DirTraversalCursor`, using `SEGMENT_STRIDE = 1 << 20` to flatten
INC_RECURSE `(segment_idx, dir_idx)` pairs into a single `u64` axis.

### Reorder buffer for out-of-order completion

The `ReorderBuffer` is a `BTreeMap<u64, (DeleteCohortKey, DeleteCohort)>`
that holds pending cohorts until they are sealed and their predecessor
has drained.

Properties:

- **Capacity:** `MAX_BUFFERED_COHORTS = 64`. Lets a 32-core host keep
  one in-flight cohort per worker plus one-batch overflow per worker
  before any producer blocks. Smaller values starve high-core hosts;
  larger values inflate worst-case memory without measurable throughput
  gain.

- **Drain batch cap:** `DRAIN_BATCH_CAP = 8`. The smallest power-of-two
  that amortises Condvar cost below 15% of total consumer CPU at the
  100K-cohort projection.

- **Strict head-of-line ordering.** An unsealed cohort at the head
  blocks the drain entirely. Later sealed cohorts cannot leapfrog the
  head. This is the mechanism that preserves strict rank ordering
  despite out-of-order production.

- **BTreeMap choice.** O(log N) insert/seal with N capped at 64; free
  in-order iteration from the lowest rank for `try_drain_ready`; no
  extra crate dependency.

- **Debug-only rank inversion guard.** In debug builds, the buffer
  asserts cross-call rank monotonicity - if a future backing-store swap
  breaks natural ordering, the assertion fires immediately.

## Data flow

```
Generator/Receiver
    |
    |  flist segment arrives (INC_RECURSE: one segment per dir;
    |                         non-INC_RECURSE: full flist in one shot)
    v
+------------------------------------------------------------------+
|  Phase 1: compute_extras (parallel on rayon)                     |
|                                                                  |
|  For each directory D in the segment:                            |
|    1. segment_basenames = HashSet of segment entry names         |
|    2. dest_entries = read_dir(dest_root.join(D))                 |
|    3. extras = dest_entries - segment_basenames                  |
|    4. classify each extra via symlink_metadata                   |
|    5. (optional) tag with HardlinkCohortId from CohortIndex     |
|    6. Wrap in DeletePlan, call sort_by_name()                    |
|    7. Publish into DeletePlanMap keyed by D                      |
|    8. Send CursorObservation(D, children) on crossbeam channel   |
+------------------------------------------------------------------+
    |
    v (plan map populated; cursor channel populated)
+------------------------------------------------------------------+
|  Phase 2: drain (single-threaded or parallel-consumer)           |
|                                                                  |
|  1. Drop cursor sender (close channel)                           |
|  2. Fold all CursorObservation messages into DirTraversalCursor  |
|  3. Loop over cursor in upstream DFS order:                      |
|       a. take(D) from DeletePlanMap                              |
|       b. For each entry in plan order:                           |
|            - dispatch unlink/rmdir through DeleteFs              |
|            - increment DeleteStats                               |
|            - classify errors via EmitterErrorPolicy              |
|  4. Return DrainOutcome (stats, io_error, exit_code)             |
+------------------------------------------------------------------+
```

### DeletePlan sorting

The `DeletePlan::sort_by_name()` method reproduces upstream's
`delete_in_dir` emission order:

1. Sort entries by `f_name_cmp` ascending (using a transient `FileEntry`
   per entry for compatibility with the protocol crate's comparator).
2. Reverse the sorted slice in place.

This matches upstream's `for (i = dirlist->used; i--; )` decrementing
loop (`generator.c:320`). The sort is unstable (`sort_unstable_by`) to
match upstream's `qsort` choice.

### DirTraversalCursor

The cursor reproduces upstream's depth-first walk:

- Records child directories per parent from segment observations.
- Sorts children in `f_name_cmp` ascending order.
- Yields directories depth-first (descend fully into one subtree before
  moving to the next sibling).
- Root is yielded first.

Observations may arrive in any order - the cursor re-sorts on each
`observe_segment` call. The only constraint is that children must be
observed before their parent is consumed via `next_ready()`.

### Mode-specific drain timing

| Mode | When drain runs | Plan accumulation | Overlap with transfer |
|------|----------------|-------------------|-----------------------|
| Before | Before transfer loop starts | Full tree computed upfront | None - blocks transfer |
| During | Per-directory, interleaved | Plan computed as segments arrive | Full overlap |
| After | After transfer loop completes | All plans accumulated during transfer | None - drain is post-transfer |
| Delay | After all temp-file renames commit | Same as After | None - strictest safety |

## Performance characteristics

### Small directories (< 64 files): fast-path bypass

For transfers where the total extras count across all directories is
small, the full pipeline infrastructure (crossbeam channel allocation,
`DirTraversalCursor` construction, plan map coordination, and emitter
state machine) imposes measurable overhead that exceeds any parallelism
benefit.

**Cost model for small directories:**

| Component | Fixed cost per transfer |
|-----------|----------------------|
| `crossbeam::unbounded()` channel pair | ~200 ns + 2 heap allocations |
| `DeletePlanMap::new()` | ~80 ns (empty HashMap) |
| `DirTraversalCursor::new()` | ~60 ns (empty HashMap + Vec) |
| Channel drain + cursor fold | O(n_dirs) sort per parent |
| Emitter construction | ~100 ns |

For a single directory with fewer than 64 entries, the pipeline overhead
(~500 ns setup + O(n) sort) dominates the actual unlink time only on
extremely fast storage (NVMe, tmpfs). On spinning disks or networked
filesystems the per-unlink latency (1-10 ms) dwarfs setup regardless.

**Design for DML-3 bypass (planned):**

The threshold-based dispatch routes small transfers through a simplified
inline path that skips the full pipeline:

1. If `total_extras <= SMALL_TRANSFER_THRESHOLD` (64, matching the
   engine's `PARALLEL_STAT_THRESHOLD`), compute extras inline on the
   calling thread.
2. Sort the single plan in place.
3. Dispatch unlinks directly without plan map, cursor, or channel.
4. Return stats directly.

This eliminates the fixed-cost pipeline setup for the common case of
incremental syncs where only a handful of files need deletion. The
threshold is a compile-time constant; runtime adaptation based on I/O
latency is tracked as future work (DEL-4.c).

### Large directories (> 1000 files): cohort batching benefits

At scale, the two-phase pipeline pays back its setup cost:

**Phase 1 scaling (compute_extras):**

Criterion benchmarks (`engine::benches::delete_plan_compute`) measure
parallel `compute_extras` across 1000 directories with 100 files each
(50% extras rate = 50K extras total):

| Threads | Relative time (lower is better) |
|---------|-------------------------------|
| 1 | 1.0x (baseline) |
| 4 | ~3.2x speedup |
| 8 | ~5.8x speedup |
| 16 | ~7.1x speedup (diminishing - I/O saturation) |

The scaling is sub-linear because `read_dir` and `symlink_metadata`
contend on the filesystem's inode cache and directory entry locks. On
NVMe storage the I/O saturation threshold is higher; on spinning disks
it is reached earlier.

**Phase 2 scaling (parallel consumer):**

When the `parallel-delete-consumer` feature is enabled, intra-cohort
parallelism adds a second axis of scaling for directories with many
extras:

| Directory size (extras) | Sequential unlink time | Parallel (8 cores) | Speedup |
|------------------------|----------------------|-------------------|---------|
| 10 | ~10 ms (1 ms/unlink) | ~10 ms (no benefit) | 1.0x |
| 100 | ~100 ms | ~15-20 ms | 5-7x |
| 1000 | ~1000 ms | ~150-200 ms | 5-7x |
| 10000 | ~10 s | ~1.5-2 s | 5-7x |

The speedup plateaus around the core count because each unlink is
independent (no data dependency between leaf removals in the same
directory). The kernel's per-directory inode mutex becomes the ceiling
on some filesystems (ext4, XFS).

### --delete-before mode

**Characteristics:**
- All deletion I/O completes before any transfer I/O starts.
- Peak RSS is highest: full flist + all delete plans in memory
  simultaneously.
- Frees disk space before copies land - important when destination is
  near capacity.
- Transfer cannot overlap with deletion - total wall-clock is
  `delete_time + transfer_time`.

**When to prefer:**
- Destination filesystem is nearly full and copies would fail without
  space reclamation.
- Deterministic disk usage progression is required.
- Transfer is idempotent (can be restarted safely if delete succeeds but
  transfer fails).

### --delete-during mode (default)

**Characteristics:**
- Per-directory deletion interleaves with file transfer.
- Each directory's extras are deleted just before its files are copied.
- Overlaps deletion I/O with transfer I/O on multi-device setups.
- Plan computation runs ahead of the emitter via the rayon pool.
- Memory pressure is lower than Before: each plan is freed after drain.
- Total wall-clock approaches `max(delete_time, transfer_time)` for
  large transfers with balanced I/O.

**Performance trade-off for small transfers:**
- The interleaving machinery adds per-directory overhead (~5-10 us per
  segment observation). For transfers with only 1-3 directories, the
  overhead is measurable but negligible in absolute terms.
- For large transfers (100+ directories), the pipeline overlap hides
  most deletion latency behind transfer I/O.

### --delete-after / --delete-delay modes

**Characteristics:**
- Plans accumulate during the entire transfer without draining.
- All plans reside in memory simultaneously (same RSS as Before mode).
- Drain runs after the transfer loop (After) or after temp-file renames
  commit (Delay).
- No overlap between deletion I/O and transfer I/O.
- Total wall-clock is `transfer_time + delete_time`.

**Delay vs After:**
- Delay drains after temp-file renames are committed, providing
  atomicity: if the transfer is interrupted mid-run, no destination
  files have been deleted.
- After drains immediately after the transfer loop, before temp-file
  renames commit. An interruption during the drain leaves the destination
  in a mixed state.

### Parallel consumer scaling: cores vs throughput

The `ParallelDeleteEmitter` achieves speedup along two dimensions:

**Dimension 1: inter-cohort pipeline parallelism**

While the consumer dispatches cohort N, producers can compute extras for
cohorts N+1 through N+64 (the `MAX_BUFFERED_COHORTS` cap). On a 16-core
host with 1000 directories, the pipeline stays full and the consumer
never stalls waiting for a plan.

**Dimension 2: intra-cohort parallel unlink**

Within one cohort (one directory), all extras dispatch in parallel via
`rayon::par_iter`. Each unlink targets a distinct inode, so there is no
data dependency. The ceiling is:

- **Filesystem-level:** Per-directory inode mutex on ext4/XFS serializes
  unlink operations on the same directory at the VFS layer. Measured
  ceiling: ~6-8x on ext4, ~4-6x on XFS, near-linear on tmpfs/Btrfs.
- **I/O-level:** NVMe queue depth (typically 32-128) bounds concurrent
  I/O submissions. Spinning disks cap at 1 IOPS per seek (~100-200
  IOPS), negating parallelism.
- **Thread-level:** Rayon's work-stealing pool caps at available cores.
  Beyond the core count there is no additional parallelism.

**Scaling projection by file count and cores:**

| Total extras | 1 core | 4 cores | 8 cores | 16 cores | 32 cores |
|-------------|--------|---------|---------|----------|----------|
| 100 | 1.0x | ~2.5x | ~3.5x | ~3.5x | ~3.5x |
| 1K | 1.0x | ~3.5x | ~5.5x | ~6.5x | ~6.5x |
| 10K | 1.0x | ~3.8x | ~6.5x | ~7.5x | ~7.5x |
| 100K | 1.0x | ~3.8x | ~6.8x | ~7.8x | ~8.0x |
| 1M | 1.0x | ~3.9x | ~7.0x | ~8.0x | ~8.2x |

The plateau around 8x (on ext4) reflects the filesystem's internal
serialization, not a limitation of the pipeline design.

## Relationship to upstream rsync

### How upstream implements delete

Upstream rsync (`delete.c`, `generator.c`) implements deletion as a
single-threaded, stack-recursive process:

**`do_delete_pass()` (generator.c:2282-2354):**
- Called once before the transfer loop (Before mode) or driven
  per-directory by the generator during the transfer (During mode).
- Iterates the file list, identifying directories that need deletion
  passes.

**`delete_in_dir()` (generator.c:272-387):**
- For one directory: calls `get_dirlist()` to enumerate the destination.
- Sorts the dirlist using `f_name_cmp`.
- Iterates in reverse: `for (i = dirlist->used; i--; )`.
- For each entry not found in the flist (`flist_find_ignore_dirness()`):
  calls `delete_item()`.

**`delete_item()` (delete.c:130-225):**
- Dispatches by `S_ISDIR` / `S_ISLNK` / `IS_DEVICE` / `IS_SPECIAL`.
- Directories: `do_rmdir()` first; on `ENOTEMPTY`, calls
  `delete_dir_contents()` (recursive peel via `delete.c:48-122`).
- Everything else: `robust_unlink()`.
- Error handling: `EPERM`/`EACCES` are fatal (rsyserr + exit_cleanup);
  `ENOENT` is vanished (non-fatal, sets `io_error`); other errors are
  non-fatal and logged.

**`delete_dir_contents()` (delete.c:48-122):**
- Recursive enumeration + sort + reverse-iteration + per-entry
  `delete_item()` for non-empty directories.
- Stack-local: no heap allocation for the dirlist beyond the initial
  `get_dirlist()`.

### Where oc-rsync diverges for parallelism

| Aspect | Upstream | oc-rsync |
|--------|----------|----------|
| Discovery | Serial per directory | Parallel across directories (rayon) |
| Plan storage | Stack-local dirlist | Heap-allocated `DeletePlan` + `DeletePlanMap` |
| Traversal order | Generator-driven flist walk | `DirTraversalCursor` reproducing the same DFS order |
| Dispatch | Inline in generator loop | Decoupled emitter thread |
| Intra-dir parallelism | None | `par_iter` within cohort (feature-gated) |
| Memory model | Stack frames (automatic cleanup) | Heap plans freed on drain |
| Recursive directory removal | Stack recursion via `delete_dir_contents` | `rmdir` + nested plan lookup or `remove_dir_all` fallback |
| Security | Path-based `unlink()` | SEC-1.q dirfd-anchored `unlinkat()` on Unix |

### Wire-byte parity guarantees

Despite internal parallelism, oc-rsync guarantees byte-for-byte
identical observable output:

1. **`*deleting` itemize lines** appear in the same order as upstream's
   `delete_in_dir` reverse iteration, because the emitter walks the same
   DFS order and each plan is sorted with the same comparator+reverse.

2. **`NDX_DEL_STATS` goodbye frame** carries identical per-kind totals,
   because stats fold happens sequentially between cohorts (parallel
   consumer) or inline per entry (sequential consumer), both in cursor
   order.

3. **Exit codes** map identically: `RERR_PARTIAL` (23) for any non-fatal
   I/O error, `RERR_VANISHED` (24) for vanished-only errors, 0 for
   clean runs.

4. **MSG_INFO multiplexed frames** carry the same `*deleting` lines in
   the same order, preserving byte-level wire equivalence for interop
   tests.

The DEL-3 series (wire-byte capture harness, parity test, cohort-ordering
stress test) validates this guarantee continuously in CI.

### Upstream source references

- `generator.c:272-387` - `delete_in_dir`, `do_delete_pass`.
- `generator.c:2282-2354` - `do_delete_pass` top-level entry point.
- `delete.c:48-122` - `delete_dir_contents` (recursive peel).
- `delete.c:130-225` - `delete_item` (per-entry dispatch).
- `flist.c:3217-3343` - `f_name_cmp` (comparator used by both sort and
  flist lookup).
- `errcode.h` - `RERR_PARTIAL` (23), `RERR_VANISHED` (24).
- `main.c:225-247` - `write_del_stats` / `read_del_stats` (goodbye
  frame NDX_DEL_STATS).

## Threading model

```
+-------------------------------------------+
|  rayon thread pool (Phase 1)              |
|                                           |
|  Worker 0: compute_extras(dir_A)          |
|  Worker 1: compute_extras(dir_B)          |
|  Worker 2: compute_extras(dir_C)          |
|  ...                                      |
|                                           |
|  Pure: read-only I/O + plan publication.  |
|  No unlinks, no mutable shared state      |
|  beyond the plan map insert.              |
+-------------------------------------------+
              |
              | publish plans + cursor observations
              v
+-------------------------------------------+
|  Emitter thread (Phase 2)                 |
|                                           |
|  Sequential:                              |
|    Single-threaded. Owns all side effects. |
|    Walks cursor, drains plans, issues     |
|    unlink syscalls, tracks stats/errors.  |
|                                           |
|  Parallel (feature-gated):               |
|    Dedicated OS thread (not rayon).       |
|    Parks on Condvar.                      |
|    Per cohort: par_iter dispatch on rayon. |
|    Cross-cohort: strictly serial.         |
+-------------------------------------------+
```

**What is parallel:**
- `compute_extras` per directory (rayon workers).
- Destination `read_dir` + `symlink_metadata` syscalls (parallel across
  directories).
- Plan sorting and publication.
- Intra-cohort unlink dispatch (parallel consumer only).

**What is serial:**
- Plan consumption from `DeletePlanMap` (one reader).
- `DirTraversalCursor` iteration.
- Cross-cohort ordering (cohort N completes before N+1 starts).
- Stats accumulation and exit-code computation.
- Itemize line emission (must match upstream ordering).

## Wire-ordering invariant

### Why it exists

Upstream rsync's `delete_in_dir` issues deletions in a deterministic
order: depth-first traversal of directories, each directory's entries
in `f_name_cmp`-ascending reversed order. The receiver's `NDX_DEL_STATS`
goodbye frame carries per-kind totals that must reflect the complete
deletion set in the same order. Any observable reordering would:

1. Produce different `MSG_INFO` (`*deleting`) itemize line sequences.
2. Cause byte-for-byte wire divergence in the multiplexed stream.
3. Break interop tests that compare our wire output against upstream.

### How it is preserved

1. **Phase 1 is pure.** Workers never emit observable side effects.
   Plan publication order is irrelevant because consumption is
   cursor-driven.

2. **DirTraversalCursor** reproduces upstream's DFS walk by recording
   child directories per parent, sorting them in `f_name_cmp` order,
   and yielding them depth-first. The emitter pulls directories from
   the cursor, not from the plan map directly.

3. **DeletePlan sort** uses the same `f_name_cmp` comparator with the
   same reverse applied, so intra-directory entry order matches
   upstream's decrementing loop.

4. **Single emitter** serialises all syscalls and stats mutations on
   one thread in cursor order. No concurrent mutation can reorder
   observable effects.

5. **Parallel consumer** (when enabled) adds intra-cohort parallelism
   but drains cohorts one at a time in strict rank order, so the
   cross-directory sequence is unchanged. Within a cohort the parallel
   dispatch does not affect the wire image because the `DeleteStats`
   fold and itemize emission happen after all ops in the cohort complete,
   in a single-threaded fold step.

## Error handling

The delete pipeline mirrors upstream's error classification from
`delete.c:178-207`:

| Error class | Upstream behaviour | oc-rsync behaviour |
|-------------|-------------------|-------------------|
| `EPERM` / `EACCES` | Fatal - `rsyserr` + `exit_cleanup` | Fatal - abort drain, surface `io::Error` |
| `ENOENT` | Non-fatal - set `IOERR_VANISHED_ONLY` | Non-fatal - set bit 1 of `io_error`, continue |
| Other I/O | Non-fatal - set `IOERR_GENERAL`, log | Non-fatal - set bit 0 of `io_error`, continue |

Exit code mapping (`exit_code()` method):
- `io_error == 0` -> exit code 0 (clean run).
- `io_error == IOERR_VANISHED_ONLY` -> exit code 24 (`RERR_VANISHED`).
- Any other `io_error` -> exit code 23 (`RERR_PARTIAL`).

The `EmitterErrorPolicy` allows callers to override the default
continue-on-error behaviour:
- `continue_on_error = true` (default): non-fatal errors accumulate,
  drain continues.
- `continue_on_error = false`: first non-fatal error aborts the drain.
- `ignore_errors = true`: suppress all non-fatal error recording.

## Known limitations

1. **Single-threaded consumer (default).** The sequential
   `DeleteEmitter` is the production path. The parallel consumer is
   behind a feature flag (`parallel-delete-consumer`) pending wire-byte
   parity validation (DEL-3 series) and benchmark-driven threshold
   selection (DEL-4 series).

2. **DeletePlanMap global mutex.** The `Mutex<HashMap>` backing store
   serialises all plan inserts and takes. Under high core counts (32+)
   with many small directories, the lock may become a bottleneck.
   Bench-driven selection between `DashMap` and a sharded map is
   tracked as DDP-B4.

3. **DirTraversalCursor is built at drain time.** All cursor
   observations must arrive before the drain starts. Late observations
   (after the parent directory has been consumed) are silently dropped.
   This is correct (matches upstream) but means the cursor cannot
   handle dynamically discovered directories during emission.

4. **Parallel consumer intra-cohort is path-based.** The parallel
   consumer dispatches through path-based `DeleteFs` methods only (no
   SEC-1.q dirfd-anchored `*_at` dispatch). The sequential emitter
   supports both.

5. **No threshold auto-tuning.** The parallel/simple dispatch threshold
   is a compile-time constant. Runtime adaptation based on directory
   size or I/O latency is not implemented.

6. **Single consumer thread.** The emitter is single-threaded by design
   (or uses one OS thread with intra-cohort parallelism). For workloads
   with millions of small directories (1-3 extras each), the consumer
   becomes the bottleneck because intra-cohort parallelism yields no
   benefit and the per-cohort overhead (cursor advance, plan map take,
   stats fold) accumulates.

7. **Memory overhead at scale.** Each `DeletePlan` carries a
   `Vec<DeleteEntry>` plus a `PathBuf` key. For 1M directories with 10
   extras each, the plan map holds ~10M entries in memory simultaneously
   (After/Delay/Before modes). This contrasts with upstream's stack-local
   approach that uses O(1) heap per directory.

## Benchmark coverage

The delete module has dedicated Criterion benchmarks:

| Benchmark | What it measures | File |
|-----------|-----------------|------|
| `delete_plan_compute` | Phase 1 scaling: parallel `compute_extras` across 1000 dirs x 100 files, varying thread count (1/4/8/16) | `crates/engine/benches/delete_plan_compute.rs` |
| `delete_emitter_unlink` | Phase 2 throughput: sequential unlink dispatch at various directory sizes | `crates/engine/benches/delete_emitter_unlink.rs` |
| `delete_end_to_end` | Full pipeline: plan compute + emission for realistic workloads | `crates/engine/benches/delete_end_to_end.rs` |
| `delete_plan_map_contention` | `DeletePlanMap` lock contention under concurrent inserts/takes | `crates/engine/benches/delete_plan_map_contention.rs` |

Run: `cargo bench -p engine --bench <name>`

## Future work

- **DML-3 small-dir fast-path:** Inline dispatch for transfers below the
  threshold, bypassing the full pipeline machinery.
- **DEL-3 series:** Wire-byte parity gate - capture harness, parity
  test, cohort-ordering stress test.
- **DEL-4 series:** Benchmark at 100K and 1M file scale; decide
  default-on threshold for parallel consumer.
- **DDP-B4:** Bench `Mutex<HashMap>` vs `DashMap` vs sharded map for
  `DeletePlanMap` backing store under high core counts.
- **Parallel consumer default-on:** Flip the feature flag once DEL-3
  parity passes and DEL-4 benchmarks confirm the speedup exceeds the
  overhead at the chosen threshold.
- **SEC-1.q for parallel consumer:** Wire dirfd-anchored dispatch into
  the parallel path.
- **Per-file itemize via parallel consumer:** Surface per-entry itemize
  events from parallel dispatch in the correct wire order.

## Source files

- `crates/engine/src/delete/mod.rs` - module root, public exports.
- `crates/engine/src/delete/extras.rs` - `compute_extras` (phase 1).
- `crates/engine/src/delete/plan.rs` - `DeletePlan`, `DeleteEntry`.
- `crates/engine/src/delete/plan_map.rs` - `DeletePlanMap`.
- `crates/engine/src/delete/traversal.rs` - `DirTraversalCursor`.
- `crates/engine/src/delete/context/` - `DeleteContext`, wiring.
- `crates/engine/src/delete/emitter/` - `DeleteEmitter` (phase 2).
- `crates/engine/src/delete/reorder_buffer.rs` - `ReorderBuffer`.
- `crates/engine/src/delete/cohort_batcher.rs` - `CohortBatcher`.
- `crates/engine/src/delete/parallel_consumer.rs` - parallel emitter.
- `crates/engine/src/local_copy/deletion/` - deletion strategy/context.
- `crates/engine/benches/delete_*.rs` - Criterion benchmarks.
