# Per-Thread Ring Bench Decision Framework (IUR-5.c)

Tracking task: **IUR-5.c**. Predecessors:

- IUR-5.a - bench shared_ring vs per-thread-rings on Linux hardware (pending)
- IUR-5.b - profile lock contention under parallel workload (pending)
- `docs/design/iur-2-per-thread-rings.md` - per-thread ring design
- `docs/design/iur-3f-shared-rings-decision.md` - probes/disk-commit stay shared
- `docs/design/shared-ring-removal-plan.md` - IUR-6.b removal plan
- `docs/design/shared-ring-removal-guard.md` - IUR-6.c CI lint guard

## 1. Purpose

IUR-5.a/b will produce empirical numbers comparing the shared_ring
topology (single `Arc<Mutex<IoUring>>` contended by N workers) against
the per-thread-ring topology (`per_thread_ring::with_ring()`, one ring
per rayon worker). This document pre-defines:

1. The workload and metrics to capture.
2. The decision criteria for proceeding to IUR-6 (remove shared_ring).
3. The hold/block criteria for keeping the shared_ring.
4. A results template for recording IUR-5.a/b numbers.

## 2. Bench Design

### 2.1 Workload

A file-I/O-heavy transfer workload representative of the hot path where
ring topology matters:

- **Corpus:** 10,000 files, 64 KB each (~625 MB total). Large enough to
  sustain contention, small enough to fit in RAM and avoid disk noise.
- **Operation:** receiver-side write path - the stage where file_writer
  rings submit SQEs for buffered writes, fsyncs, and renames.
- **Mode:** parallel rayon workers, each pulling files from a work queue
  and writing through the io_uring path.
- **Configurations:**
  - `shared_ring`: single `Arc<Mutex<IoUring>>` shared across all workers
    (pre-IUR-3 topology).
  - `per_thread_ring`: `per_thread_ring::with_ring()` - one ring per
    rayon worker thread (post-IUR-3 topology).

### 2.2 Worker Counts

Each configuration runs at these thread counts: **1, 2, 4, 8, 16**.

The 1-worker case establishes single-threaded baseline (no contention).
4-8 workers represent typical production concurrency. 16 workers stress
the lock under high-contention scenarios.

### 2.3 Linux Environment

- Kernel >= 6.1 (DEFER_TASKRUN, mature CQ batching).
- CPU pinned via `taskset` to avoid migration noise.
- Turbo boost disabled for stable p99.
- ext4 on dedicated block device or loopback, mounted `noatime`.
- Container: `localhost/oc-rsync-bench:latest` for consistent seccomp.
- 5 runs per cell, report geometric mean.

## 3. Metrics

| Metric | Tool | Unit |
|--------|------|------|
| Throughput | `hyperfine` wall-clock, bytes/sec | MB/s |
| Submission latency p50 | `hdrhistogram` on submit-to-CQE | us |
| Submission latency p99 | `hdrhistogram` on submit-to-CQE | us |
| CPU utilization | `/proc/stat` delta over run | % |
| Lock contention events | `perf lock contention` or mutex timing | count/s |
| io_uring_enter syscalls | `perf stat -e syscalls:sys_enter_io_uring_enter` | count |

## 4. Decision Criteria

### 4.1 IUR-6 Trigger (proceed with shared_ring removal)

All of the following must hold:

1. **Throughput:** per-thread-ring wins by >= 5% at >= 4 workers.
2. **No single-worker regression:** per-thread-ring throughput at 1 and
   2 workers is within 3% of shared_ring (no worse than -3%).
3. **Latency improvement:** p99 submission latency improves (decreases)
   at >= 4 workers with per-thread-ring.
4. **Contention elimination:** lock contention events drop to near-zero
   with per-thread-ring at all worker counts.

If all four hold, IUR-6 proceeds: delete the dormant `SharedRing`
abstraction, `SessionRingPool`, and associated stubs per the removal
plan in `shared-ring-removal-plan.md`.

### 4.2 Hold Criteria (keep shared_ring, revisit later)

If any of the following is observed:

- Per-thread-ring throughput gain is < 5% at 4+ workers (tie or marginal
  win) - the complexity of removal is not justified by the evidence.
- Per-thread-ring loses to shared_ring at any worker count - unexpected
  and requires investigation before any removal.
- Latency p99 regresses with per-thread-ring at any worker count.

Decision: keep `SharedRing` code in tree. Document the numbers. Revisit
when workload profile changes (e.g., larger file counts, different I/O
patterns) or kernel improvements shift the balance.

### 4.3 Regression Block (hard block on IUR-6)

If per-thread-ring regresses single-worker throughput by > 3% compared
to shared_ring:

- **Block IUR-6 entirely** until the regression is root-caused and fixed.
- Rationale: single-worker is the serial baseline; any regression there
  indicates a fundamental overhead in the per-thread-ring primitive
  (e.g., TLS lookup cost, ring construction on every task) that must be
  resolved before the shared path is removed.

## 5. Decision Outputs

The bench produces exactly one of three outcomes:

| Outcome | Meaning | Next Step |
|---------|---------|-----------|
| **PROCEED** | All trigger criteria met | Execute IUR-6.a/b removal plan |
| **HOLD** | Marginal or no improvement | Keep shared_ring; file revisit trigger in this doc |
| **INVESTIGATE** | Unexpected regression or anomaly | Root-cause before any topology change |

The outcome must be documented in this file (section 7) with the raw
bench numbers, kernel version, hardware spec, and date.

## 6. IUR-6 Scope Reminder

When PROCEED is the decision, IUR-6 comprises:

- **IUR-6.a:** Inventory of remaining shared_ring callers (done,
  `shared-ring-removal-plan.md` section 1).
- **IUR-6.b:** Delete `SharedRing`, `SharedRingConfig`,
  `SharedCompletion`, `SessionRingPool`, `RingLease`, and their stub
  mirrors. Gut integration tests and update benchmarks.
- **IUR-6.c:** CI lint guard already in place (PR #5251). Extend to
  reject `mod shared_ring` declarations post-removal.

The items in IUR-3.f section 1.2 (probes, disk-commit, ZeroCopySender)
are explicitly out of scope for IUR-6 - they are ratified as
intentionally shared.

## 7. Results (IUR-5.a/b)

*To be filled after bench execution on Linux hardware.*

### 7.1 Hardware & Environment

| Field | Value |
|-------|-------|
| CPU | |
| Cores / Threads | |
| Kernel | |
| Filesystem | |
| Container | |
| Date | |

### 7.2 Throughput (MB/s)

| Workers | shared_ring | per_thread_ring | Delta (%) |
|---------|-------------|-----------------|-----------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

### 7.3 Submission Latency p50 (us)

| Workers | shared_ring | per_thread_ring | Delta (%) |
|---------|-------------|-----------------|-----------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

### 7.4 Submission Latency p99 (us)

| Workers | shared_ring | per_thread_ring | Delta (%) |
|---------|-------------|-----------------|-----------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

### 7.5 CPU Utilization (%)

| Workers | shared_ring | per_thread_ring | Delta |
|---------|-------------|-----------------|-------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

### 7.6 Lock Contention Events (count/s)

| Workers | shared_ring | per_thread_ring | Delta (%) |
|---------|-------------|-----------------|-----------|
| 1 | | | |
| 2 | | | |
| 4 | | | |
| 8 | | | |
| 16 | | | |

### 7.7 Decision

| Field | Value |
|-------|-------|
| Outcome | |
| Rationale | |
| Next step | |
