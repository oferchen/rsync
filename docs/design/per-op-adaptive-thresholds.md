# Per-Operation Adaptive Parallelism Thresholds

Tracking issue: #1554. Cross-references: #1083 (parallel-stat profile), #1084 (NFS/FUSE
evaluation), #1085 (signature scaling).

## 1. Current State

A single constant `PARALLEL_STAT_THRESHOLD = 64` (defined in the receiver module) gates
every rayon dual-path decision in the codebase: stat batches, signature scheduling, and
metadata application all consult the same fixed value. Below the threshold the code runs
sequentially; above it, work is dispatched through `par_iter()`. The constant is hard-coded
and unaware of the cost profile of the operation it gates, the underlying filesystem, or
prior measurements from the same run.

## 2. Operation Cost Profiles

The three call sites have markedly different per-item costs:

- **Stat / quick-check** is cheap (one `lstat` per entry). Rayon overhead dominates for
  small batches; the crossover sits around 64 entries on local ext4 and far higher on
  NFS/FUSE where each syscall round-trips.
- **Signature generation** is CPU-heavy (rolling + strong checksum over every block of a
  basis file). The crossover collapses to a handful of files because each unit of work is
  already milliseconds long.
- **Metadata apply** is syscall-heavy but cheap per call (`chmod`, `chown`, xattrs, ACLs).
  It behaves like stat on local filesystems but degrades sharply on networked mounts.
- **Delete** is intermediate: `unlinkat` plus parent-directory bookkeeping, lighter than a
  full metadata apply because no permission/ownership round-trips are issued.

A single threshold cannot serve all four well.

## 3. Per-Operation Threshold Table

Initial defaults, chosen from the profiling data referenced in #1083 / #1085:

| Operation     | Default | Rationale                                    |
|---------------|---------|----------------------------------------------|
| `STAT`        | 64      | Matches today's behaviour; cheap syscalls.   |
| `SIGNATURE`   | 4       | CPU-bound; parallelise aggressively.         |
| `METADATA`    | 64      | Syscall-heavy but fast on local FS.          |
| `DELETE`      | 128     | Lighter than metadata; avoid scheduler churn.|

Thresholds live in a `ParallelThresholds` struct in `core::parallel`, with one field per
operation and a `for_op(ParallelOp)` accessor used by every call site.

## 4. Adaptive Layer

A lightweight EMA-based feedback loop refines each threshold during a run:

- After each batch, record `(items, wall_time, mode)` where `mode` is sequential or
  parallel.
- Maintain an exponential moving average of per-item cost for both modes (`alpha = 0.2`).
- Recompute the crossover where `parallel_cost * items + scheduler_overhead` equals
  `sequential_cost * items`, clamp into `[min, max]` per-op bounds, and update the live
  threshold for subsequent batches in the same run.
- Updates are atomic (`AtomicUsize`) so the feedback loop stays lock-free on the hot path.

This keeps fast filesystems near their static optimum while letting NFS/FUSE drift the
stat and metadata thresholds upward without code changes (#1084).

## 5. Storage and Configuration

- **Defaults** ship in the binary.
- **Static overrides** load from `oc-rsync.toml` under a `[parallel.thresholds]` table:
  `stat = 64`, `signature = 4`, `metadata = 64`, `delete = 128`.
- **Environment overrides** accept a comma-separated form:
  `OC_RSYNC_PARALLEL_THRESHOLDS=stat=128,signature=2,metadata=64,delete=128`.
- **Learned values** persist between runs in `${XDG_STATE_HOME:-~/.local/state}/oc-rsync/
  parallel-thresholds.json`, keyed by filesystem type plus mount point so NFS, local ext4,
  and FUSE each retain their own EMAs. Persistence is best-effort; a missing or corrupt
  file falls back to defaults without erroring.

Precedence: env var > config file > learned file > built-in default. The adaptive layer
writes back only to the learned file, never to the user-authored config.
