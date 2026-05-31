# io_uring Default-On Threshold Decision (IUB-12)

Tracking: IUB-12. Predecessor: IUB-10 (payoff matrix synthesis from
IUB-6..9 bench results). Related: IUB-11 (PR #5282, user-facing
documentation format).

## 1. Problem Statement

The current `--io-uring=auto` default enables io_uring unconditionally
when the kernel supports it (Linux 5.6+). The IUB-6..9 bench results
and the IUB-10 payoff matrix show that io_uring provides no measurable
benefit - and occasionally adds overhead - below certain workload
thresholds. The headline release bench at 148 MB / 10K files shows
~1.00x vs standard I/O.

This document decides what `Auto` should mean: "always on if supported"
or "on only when the workload exceeds a size/count threshold where
io_uring demonstrably helps."

## 2. Decision Inputs: IUB-10 Payoff Matrix

The IUB-10 payoff matrix synthesizes IUB-6..9 bench data across two
axes: total transfer size and file count. The relevant break-even
observations:

| Regime | File count | Total size | io_uring speedup | Verdict |
|--------|-----------|------------|------------------|---------|
| Small workload | < 1 000 | < 100 MB | 0.97x - 1.00x | No benefit; ring setup is overhead |
| Release bench | 10 000 | 148 MB | ~1.00x | Neutral; crossover zone |
| IOPS-bound (IUB-3) | 100 000 | 400 MB | 1.08x - 1.15x | Pays off via statx batching |
| IOPS-bound (IUB-3) | 1 000 000 | 1 GiB | 1.12x - 1.22x | Clear win at high file count |
| Throughput-bound (IUB-2) | 1 | 2 GiB | 1.06x - 1.10x | Modest win from queue depth |
| Throughput-bound (IUB-2) | 1 | 10 GiB | 1.10x - 1.18x | Sustained write batching win |
| Throughput-bound (IUB-2) | 1 | 50 GiB | 1.15x - 1.25x | Strong win on NVMe |

The 1.05x threshold (the minimum speedup to justify enabling) is
crossed when either:

- File count exceeds ~50 000, OR
- Total transfer size exceeds ~1 GiB

Below both of these, io_uring adds ring-setup cost (one `io_uring_setup`
syscall at ~15-25 us, ring memory allocation, per-SQE encoding overhead)
without measurable payoff.

## 3. Threshold Options

### Option A: Always-on (status quo)

Keep `Auto` = "enable if kernel supports." Accept the neutral/slightly
negative outcome on small workloads.

**Pros:**
- Simplest implementation - no threshold logic.
- No risk of leaving performance on the table for edge workloads.
- Users with large workloads get io_uring without configuration.

**Cons:**
- ~15-25 us ring setup cost paid on every transfer, even trivial ones.
- Completion-reaping and per-SQE encoding overhead on small batches
  can produce net-negative results (0.97x observed at < 1K files).
- User cannot distinguish "io_uring helping" from "io_uring costing"
  without benchmarking their specific workload.

### Option B: File-count threshold only

Enable io_uring only when file count > N.

**Pros:**
- Captures the IOPS-bound regime where batched statx wins.
- Simple single-variable decision.

**Cons:**
- Misses throughput-bound regime: a single 10 GiB file has count = 1
  but benefits from io_uring write batching.
- File count is only known after flist exchange completes, requiring
  late construction of the ring (acceptable but adds complexity).

### Option C: Total-size threshold only

Enable io_uring only when total transfer bytes > M.

**Pros:**
- Captures the throughput-bound regime.
- Total size can be estimated early from flist sum.

**Cons:**
- Misses the IOPS-bound regime: 100K x 4 KiB = 400 MB total, which
  is below a typical multi-GiB byte threshold, but the file count
  drives the win.

### Option D: Combined heuristic (proposed)

Enable io_uring when `file_count > N OR total_bytes > M`.

**Pros:**
- Covers both regimes in a single check.
- Conservative: only disables io_uring for workloads proven not to
  benefit (small count AND small size).
- Either condition alone is sufficient to trigger enablement.

**Cons:**
- Two tuning parameters instead of one.
- Slight conceptual complexity for users who want to understand why
  io_uring was or was not used.

## 4. Proposed Heuristic

**Selected: Option D - combined heuristic.**

```
enable_io_uring = (file_count > 10_000) OR (total_bytes > 512 MiB)
```

Rationale for threshold values:

- **`file_count > 10_000`**: The IUB-10 matrix shows the crossover into
  io_uring benefit (> 1.05x) begins around 50K files, but the overhead
  at 10K is already near-zero (1.00x). Setting the threshold at 10K
  ensures io_uring is active well before the benefit kicks in, avoiding
  the scenario where a 30K-file workload misses out on a 1.03x
  improvement. The ring-setup cost is negligible at this scale relative
  to overall transfer time.

- **`total_bytes > 512 MiB`**: The 2 GiB cell shows 1.06x-1.10x on
  NVMe. Setting the threshold at 512 MiB captures workloads approaching
  the benefit zone while excluding the sub-150 MB regime where io_uring
  is demonstrably neutral. This matches the `IoUringDiskBatch`
  amortization break-even: at 512 MiB with a 128 KiB block size,
  roughly 4096 write SQEs are submitted - enough to keep the ring
  usefully occupied.

Both thresholds are constants in `fast_io::policy` and overridable via
the existing `--io-uring=enabled` (force) and `--io-uring=disabled`
(force off) flags. The threshold check only applies when the policy is
`Auto`.

## 5. Tradeoffs

### Ring creation startup cost

A single `io_uring_setup(2)` call costs 15-25 us on modern kernels. For
a transfer of 100 files (each ~10 KiB), total transfer time is ~5-50 ms
depending on I/O subsystem. The ring setup is 0.03-0.5% of wall time -
negligible in absolute terms but yields zero benefit. The threshold
avoids this overhead for quick rsync invocations (e.g., single-file
copies, syncing a few changed config files).

### False-positive enables

The threshold is intentionally set below the clear-benefit zone. A
transfer of 15K files totaling 200 MB (above file-count threshold but
below the proven 50K sweet spot) will enable io_uring and get ~1.00x
performance - no harm, no help. This is acceptable: the goal is to
avoid measurable regressions (the 0.97x observed below 1K files), not
to guarantee speedup at every scale.

### False-negative disables

A transfer of 5K files totaling 300 MB (below both thresholds) will use
standard I/O. If that workload would have benefited from io_uring
(unlikely per IUB-10, but possible on specific hardware), the user can
force `--io-uring=enabled`.

### User confusion

The threshold introduces a case where the same binary behaves
differently on the same kernel depending on workload size. This is
mitigated by:

- `-vv` output logs whether io_uring was enabled and why/why not.
- `--io-uring=enabled` / `--io-uring=disabled` bypass the heuristic
  entirely.
- Documentation (per IUB-11 format) explains the heuristic.

## 6. Fallback Behavior

When `Auto` mode decides not to enable io_uring (workload below both
thresholds), the transfer proceeds exactly as if `--io-uring=disabled`
were passed:

- File reads use standard buffered I/O (`std::fs::File` + `Read`).
- File writes use standard buffered I/O (temp-file + `Write` + rename).
- Metadata ops use direct syscalls (`statx(2)`, `renameat2(2)`,
  `linkat(2)`, `unlinkat(2)`).
- No `io_uring_setup(2)` syscall is issued.
- No ring memory is allocated.
- No performance degradation relative to a build without io_uring
  compiled in.

The fallback is the same code path used on non-Linux platforms and on
kernels below 5.6. It is the most exercised path in the test suite.

## 7. User Override

The threshold check applies only to `BackendPolicy::Auto`. The two
explicit modes bypass it entirely:

| CLI flag | Effect |
|----------|--------|
| `--io-uring=auto` (default) | Apply threshold heuristic; enable if workload exceeds thresholds |
| `--io-uring=enabled` | Force io_uring on; error if kernel does not support it |
| `--io-uring=disabled` | Force io_uring off; always use standard I/O |
| `--no-io-uring` | Alias for `--io-uring=disabled` |

No change to the CLI surface. The threshold is purely an internal
refinement of `Auto` semantics.

## 8. Implementation

### 8.1 Threshold constants

Location: `crates/fast_io/src/policy.rs`

```rust
/// Minimum file count for `Auto` mode to enable io_uring.
/// Below this threshold, ring-setup cost is not amortized.
pub const IOURING_AUTO_FILE_COUNT_THRESHOLD: u64 = 10_000;

/// Minimum total transfer bytes for `Auto` mode to enable io_uring.
/// Below this threshold, write-batching benefit is not realized.
pub const IOURING_AUTO_TOTAL_BYTES_THRESHOLD: u64 = 512 * 1024 * 1024;
```

### 8.2 Decision point

The threshold check runs after flist exchange completes and before the
transfer pipeline starts. At that point, both file count and total
transfer size are known from the received file list.

Location: `crates/core/src/session.rs` (or the transfer-config builder
that constructs `IoUringPolicy` for the pipeline).

```rust
fn resolve_io_uring_policy(
    configured: IoUringPolicy,
    file_count: u64,
    total_bytes: u64,
) -> IoUringPolicy {
    match configured {
        IoUringPolicy::Enabled | IoUringPolicy::Disabled => configured,
        IoUringPolicy::Auto => {
            if file_count > IOURING_AUTO_FILE_COUNT_THRESHOLD
                || total_bytes > IOURING_AUTO_TOTAL_BYTES_THRESHOLD
            {
                IoUringPolicy::Auto // proceed with runtime detection
            } else {
                IoUringPolicy::Disabled // below threshold, skip ring
            }
        }
    }
}
```

The function returns `Auto` (not `Enabled`) when thresholds are
exceeded, so downstream code still performs runtime kernel detection
and fails gracefully on unsupported hosts. `Disabled` when below
threshold is a firm "do not construct a ring."

### 8.3 Logging

At `-vv` verbosity, log the decision:

```
io_uring: auto-disabled (file_count=847, total_bytes=12583724,
  thresholds: files>10000 OR bytes>536870912)
```

or:

```
io_uring: auto-enabled (file_count=150000, threshold exceeded: files>10000)
```

### 8.4 Ring construction deferral

Currently, `IoUringDiskBatch::try_new` is called at receiver startup,
before file count is known. The threshold check must run after flist
exchange. Two implementation strategies:

**Strategy A (lazy ring):** Pass `IoUringPolicy` resolved after flist
into the disk-commit thread. If `Disabled`, `IoUringDiskBatch::try_new`
short-circuits (already does today). No change to the disk-batch code
itself.

**Strategy B (deferred policy):** Construct the disk-commit thread with
an initially-unresolved policy that flips from `Auto` to resolved after
flist. More complex, no clear benefit.

**Selected: Strategy A.** The receiver already knows the file list before
it starts the disk-commit pipeline. The resolved policy is threaded
through `TransferConfig` into the disk-batch constructor. Zero change
to the disk-batch or ring code.

### 8.5 Incremental recursion consideration

With `--inc-recurse`, the full file list is not known upfront - file
entries arrive in segments. Two choices:

1. Use the first segment's count and size as a proxy. Risk: first
   segment is small (one directory), threshold not met, io_uring
   disabled for a transfer that will eventually grow large.

2. Always enable io_uring under `--inc-recurse`. The user opted into
   incremental mode, implying a large enough tree that io_uring is
   likely beneficial.

**Selected: option 2.** Incremental recursion is only negotiated for
workloads with recursive directory traversal - inherently multi-file
transfers. The false-negative risk of disabling io_uring mid-stream
outweighs the minor ring-setup cost for small `--inc-recurse`
invocations.

## 9. Future Refinements

- **Adaptive threshold (per-op EMA).** The per-operation adaptive
  threshold infrastructure (per-op-adaptive-thresholds.md, #1554) could
  learn the io_uring break-even for a given host and filesystem,
  persisting the learned threshold between runs. This is a natural
  extension but not required for the initial implementation.

- **Per-operation thresholds.** The current design uses a single
  transfer-level check. A future refinement could apply separate
  thresholds to statx-batching (file-count driven) and data-write
  batching (byte-count driven), enabling io_uring only for the
  operations that benefit. This would require splitting
  `IoUringPolicy` into per-subsystem policies at the transfer level.

- **Kernel-version-aware thresholds.** Newer kernels (6.0+) have lower
  ring-setup overhead and more efficient completion handling. The
  threshold could be relaxed on 6.0+ and tightened on 5.6-5.15 where
  per-SQE overhead is higher.

## 10. Validation Plan

1. Unit test: `resolve_io_uring_policy` returns `Disabled` below both
   thresholds, `Auto` above either threshold, passes through `Enabled`
   and `Disabled` unchanged.

2. Integration test: run a 500-file / 5 MB transfer with
   `RUST_LOG=debug` and verify the "auto-disabled" log line appears.

3. Integration test: run a 50K-file transfer and verify the
   "auto-enabled" log line appears and transfer completes successfully.

4. Bench regression: re-run the 148 MB / 10K release bench with the
   new threshold. Confirm no regression (should show ~1.00x since the
   threshold enables io_uring at exactly 10K files).

5. Bench benefit: run the IUB-3 100K-file cell with and without the
   threshold. Confirm the threshold decision correctly enables io_uring
   and preserves the observed 1.08x-1.15x speedup.
