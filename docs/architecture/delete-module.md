# Delete module architecture

This document describes the delete module's internal architecture,
threading model, performance characteristics, and relationship to
upstream rsync 3.4.1's deletion behaviour. It covers the full pipeline
from candidate discovery through ordered emission.

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

```
 Receiver (flist segments arrive)
         |
         |  for each segment
         v
+------------------------------+       +-------------------------------+
| Phase 1: compute_extras      |       | CursorObservation channel     |
| (rayon workers, parallel)    |------>| (unbounded crossbeam)         |
|                              |       +-------------------------------+
| - read_dir(dest_dir)         |                    |
| - subtract segment basenames |                    | (folded at drain
| - classify survivors by kind |                    |  time into cursor)
| - sort by f_name_cmp, reverse|                    v
| - publish DeletePlan         |       +-------------------------------+
+------------------------------+       | DirTraversalCursor            |
         |                             | (depth-first, f_name_cmp asc) |
         v                             +-------------------------------+
+------------------------------+                    |
| DeletePlanMap                 |                    |
| (Mutex<HashMap<PathBuf,      |                    |
|  DeletePlan>>)               |                    |
+------------------------------+                    |
         |                                          |
         +--------------------+---------------------+
                              |
                              v
              +-------------------------------+
              | Phase 2: DeleteEmitter        |
              | (single-threaded drain)       |
              |                               |
              | - walk cursor in order        |
              | - take(dir) from plan map     |
              | - for each entry: dispatch    |
              |   unlink/rmdir via DeleteFs   |
              | - increment DeleteStats       |
              | - record io_error bitmask     |
              +-------------------------------+
                              |
                              v
              +-------------------------------+
              | DrainOutcome                  |
              | - DeleteStats (per-kind)      |
              | - io_error bitmask            |
              | - exit code (0/23/24)         |
              +-------------------------------+
```

## Data flow

### 1. Discovery (compute_extras)

When a file-list segment arrives at the receiver, the context invokes
`compute_extras(dest_dir, segment_entries)`:

1. Build a `HashSet<OsString>` of basenames from the segment.
2. Call `read_dir(dest_dir)` to enumerate the destination.
3. For each dest entry not in the segment set, classify it via
   `symlink_metadata` into File/Dir/Symlink/Device/Special.
4. Optionally tag entries with a `HardlinkCohortId` from the
   `CohortIndex` snapshot (for itemize formatting - does not affect
   unlink decisions).
5. Return the unsorted `Vec<DeleteEntry>`.

### 2. Planning (DeletePlan)

The caller wraps extras in a `DeletePlan` and calls `sort_by_name()`:

- Sorts entries using upstream's `f_name_cmp` comparator.
- Reverses the result to match upstream's decrementing
  `for (i = dirlist->used; i--; )` loop in `delete_in_dir`.
- Marks the plan as sorted (publish-once invariant).

### 3. Publication (DeletePlanMap)

The sorted plan is inserted into `DeletePlanMap` keyed by the
destination-relative directory path. The map uses `Mutex<HashMap>` -
simple, correct, and sufficient because:

- Inserts are O(1) amortized; the lock is held only for the map
  mutation itself.
- Each directory is published exactly once (publish-once invariant).
- At most `rayon_pool_size` concurrent writers plus one reader.

### 4. Cursor observations

Concurrently with plan publication, worker threads send
`CursorObservation` messages (directory path + child entries) through an
unbounded crossbeam channel. The drain folds these into a
`DirTraversalCursor` at startup, which then yields directories in
upstream's depth-first, `f_name_cmp`-ascending order.

### 5. Emission (DeleteEmitter)

The single-threaded emitter loops:

1. Pull the next directory from `DirTraversalCursor::next_ready()`.
2. `take(dir)` the plan from `DeletePlanMap`.
3. For each entry in plan order, dispatch through the `DeleteFs` trait:
   - Files/symlinks/devices/specials: `unlink` (or `unlinkat` with
     SEC-1.q sandbox anchoring on Unix).
   - Directories: `rmdir` first; on `ENOTEMPTY`, drain the nested
     plan or fall back to `remove_dir_all`.
4. On success: increment the matching `DeleteStats` counter.
5. On failure: classify as fatal (PermissionDenied - abort) or
   non-fatal (set `io_error` bitmask, continue). Matches upstream's
   `delete.c:178-207` behaviour.

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
|  Single-threaded. Owns all side effects.  |
|  Walks cursor, drains plans, issues       |
|  unlink syscalls, tracks stats/errors.    |
+-------------------------------------------+
```

**What is parallel:**
- `compute_extras` per directory (rayon workers).
- Destination `read_dir` + `symlink_metadata` syscalls.
- Plan sorting and publication.

**What is serial:**
- Plan consumption from `DeletePlanMap` (one reader).
- `DirTraversalCursor` iteration.
- All filesystem mutation (unlink/rmdir).
- Stats accumulation and exit-code computation.
- Itemize line emission.

## Parallel consumer (feature-gated)

Behind the `parallel-delete-consumer` Cargo feature, a second
architecture is available that parallelises dispatch within each
directory (cohort) while preserving strict cross-directory ordering:

```
+-------------------------------------------+
|  ParallelDeleteEmitter                    |
|                                           |
|  Producer: enqueue sealed cohorts via     |
|  Mutex<CohortBatcher> + Condvar           |
|                                           |
|  Consumer: dedicated OS thread            |
|    for each cohort in rank order:         |
|      ops.par_iter() -> dispatch           |
|      (parallel within one directory)      |
|      fold results into stats              |
|    wait for next sealed head              |
+-------------------------------------------+
```

Key properties:
- Cross-cohort ordering is strict: cohort N+1 never begins until every
  op in cohort N has completed.
- Intra-cohort parallelism: ops within one directory dispatch on rayon
  since each targets a distinct filesystem leaf.
- Wire-ordering invariant preserved: `NDX_DEL_STATS` goodbye frame
  sees correct totals because stats fold happens sequentially between
  cohorts.

## Reorder buffer

The `ReorderBuffer` (BTreeMap keyed by rank) holds pending cohorts:

- Capacity: `MAX_BUFFERED_COHORTS = 64` (lets a 32-core box keep one
  in-flight cohort per worker plus overflow before backpressure).
- Drain batch cap: `DRAIN_BATCH_CAP = 8` (amortises Condvar cost
  below 15% of consumer CPU).
- An unsealed cohort at the head blocks the drain entirely - this is
  the mechanism that preserves strict rank ordering despite
  out-of-order production.

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
   cross-directory sequence is unchanged.

## Performance characteristics

### Where the cohort/reorder approach wins

| Scenario | Advantage |
|----------|-----------|
| Large directories (1000+ extras per dir) | Parallel `compute_extras` saturates I/O bandwidth on multi-core |
| Deep trees with many directories | Plan computation overlaps with emission of earlier directories |
| Mixed workloads (transfer + delete) | During mode overlaps deletion I/O with file transfer I/O |
| 100K+ file deletes (parallel consumer) | Intra-cohort parallelism amortises per-file unlink latency on high-IOPS storage |

### Where it loses vs upstream's simpler linear approach

| Scenario | Cost |
|----------|------|
| Small transfers (< 100 files) | Pipeline setup overhead (channel allocation, cursor construction, plan map) adds latency that upstream's inline loop avoids |
| Single-directory transfers | No parallelism benefit; the single plan computes and drains sequentially anyway |
| Reorder buffer backpressure | If producers outrun the consumer (unlikely in practice due to I/O dominance), the 64-slot cap forces producer stalls |
| Memory overhead | Per-directory `DeletePlan` + `DeletePlanMap` + cursor state vs upstream's stack-local enumeration |

### Threshold-based dispatch

The engine uses a threshold to decide between the parallel pipeline and
a simpler inline path for small transfers. Below the threshold, the
overhead of channel allocation, cursor construction, and plan map
coordination exceeds the parallelism benefit. The threshold tuning is
tracked by DEL-4.c (benchmark-driven, scale-dependent).

## Known limitations

1. **Single-threaded consumer (default).** The sequential
   `DeleteEmitter` is the production path. The parallel consumer is
   behind a feature flag (`parallel-delete-consumer`) pending wire-byte
   parity validation (DEL-3 series) and benchmark-driven threshold
   selection (DEL-4 series).

2. **DeletePlanMap global mutex.** The `Mutex<HashMap>` backing store
   serialises all plan inserts and takes. Bench-driven selection
   between `DashMap` and a sharded map is tracked as DDP-B4.

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

## Future work

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

## Upstream references

- `generator.c:272-387` - `delete_in_dir`, `do_delete_pass`.
- `delete.c:48-122` - `delete_dir_contents` (recursive peel).
- `delete.c:130-225` - `delete_item` (per-entry dispatch).
- `flist.c:3217-3343` - `f_name_cmp` (comparator).
- `errcode.h` - `RERR_PARTIAL` (23), `RERR_VANISHED` (24).

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
