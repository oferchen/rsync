# SQPOLL NVMe re-bench after mmap basis dispatch fix (SQM-4.a)

Tracking task: SQM-4.a. Predecessors:

- SQM-1.a/1.b/1.c - root cause analysis, reproducer, workaround spec
  (`docs/design/sqpoll-mmap-race-symptoms.md`,
  `docs/design/sqm-1c-workaround-spec.md`).
- SQM-2.a/2.b - candidate scoring and implementation design
  (`docs/design/sqm-2a-workaround-scoring.md`,
  `docs/design/sqm-2b-implementation-design.md`).
- SQM-3 - implementation of the SQPOLL-safe mmap basis dispatch.
  When `mmap_basis_active` is true, SQPOLL is defensively disabled
  to close the page-fault race with the SQPOLL kthread.

Successor: SQM-4.b implements the chosen remediation if the measured
impact exceeds the acceptance threshold (see "Decision criteria" below).

This document does not change source. It specifies the bench plan that
quantifies the throughput cost of SQM-3's defensive disable on NVMe
hardware with large basis files.

## 1. What changed in SQM-3

The SQPOLL kthread cannot service page faults - when a basis file is
memory-mapped and the kthread dereferences an unmapped page, the
submission stalls or returns -EFAULT. SQM-3 closes this race by
disabling SQPOLL whenever the transfer plan uses an mmap'd basis:

```text
// crates/fast_io/src/io_uring/config.rs
let sqpoll_safe = sqpoll_requested && !self.mmap_basis_active;
if sqpoll_requested && !sqpoll_safe {
    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
    // build a regular ring instead of SQPOLL ring
}
```

The fallback ring uses standard `io_uring_enter(2)` submission instead
of kernel-side polling. This is correct and race-free, but the extra
syscall per submission batch introduces measurable latency on high-IOPS
NVMe devices. The expected throughput reduction is 10-15% for workloads
that would otherwise benefit from SQPOLL (large sequential transfers
with mmap'd basis files on NVMe).

## 2. Bench fixture

### File layout

Delta transfers with a high copy-token ratio stress the basis-read path
most heavily - the receiver reads large contiguous ranges from the basis
file to reconstruct the destination. The fixture must produce this
access pattern:

| Fixture | Basis size | Delta size | Copy-token ratio | Purpose |
|---------|-----------|-----------|-----------------|---------|
| `nvme_1g_high_copy` | 1 GiB | ~50 MiB (5% changed) | ~95% copy tokens | Minimum scale for SQPOLL amortisation |
| `nvme_10g_high_copy` | 10 GiB | ~500 MiB (5% changed) | ~95% copy tokens | Production-representative large-file workload |

The fixture generator mutates 5% of the basis at random 8 KiB-aligned
offsets, producing a delta stream dominated by `COPY` tokens that
reference contiguous basis ranges. This maximises basis-read I/O and
isolates the SQPOLL vs regular-ring difference from network or
checksum overhead.

### Fixture generation

Pre-generate fixtures on the bench host to avoid measuring file
creation:

```bash
# 1 GiB basis
dd if=/dev/urandom of=/bench/basis_1g bs=1M count=1024
cp /bench/basis_1g /bench/dest_1g
# Mutate 5% (random 8K blocks)
python3 -c "
import os, random
f = open('/bench/dest_1g', 'r+b')
size = f.seek(0, 2)
blocks = size // 8192
for _ in range(blocks // 20):
    off = random.randint(0, blocks-1) * 8192
    f.seek(off)
    f.write(os.urandom(8192))
f.close()
"

# 10 GiB basis (same pattern, gated behind OC_RSYNC_BENCH_LARGE=1)
dd if=/dev/urandom of=/bench/basis_10g bs=1M count=10240
cp /bench/basis_10g /bench/dest_10g
# Mutate 5%
python3 -c "
import os, random
f = open('/bench/dest_10g', 'r+b')
size = f.seek(0, 2)
blocks = size // 8192
for _ in range(blocks // 20):
    off = random.randint(0, blocks-1) * 8192
    f.seek(off)
    f.write(os.urandom(8192))
f.close()
"
```

### Environment gates

| Gate | Meaning |
|------|---------|
| `OC_RSYNC_BENCH_IOURING_RING=1` | Enable io_uring bench cells |
| `OC_RSYNC_BENCH_LARGE=1` | Enable 10 GiB cell |
| `SQM4_FORCE_SQPOLL=1` | Override the defensive disable (unsafe, measurement only) |

## 3. A/B comparison

Two arms, same workload, same hardware, same kernel:

| Arm | Ring mode | How configured | Safety |
|-----|-----------|---------------|--------|
| A: Current default | Regular ring (SQPOLL disabled due to mmap basis) | Default behaviour after SQM-3 | Safe - no page-fault race |
| B: Forced SQPOLL | SQPOLL ring with mmap basis active | `SQM4_FORCE_SQPOLL=1` overrides the `mmap_basis_active` guard | Unsafe for measurement only - may trigger EFAULT under memory pressure |

Arm B exists solely to measure the throughput delta. It must never be
exposed as a user-facing option. The bench harness gates it behind both
`SQM4_FORCE_SQPOLL=1` and `UNSAFE_ACKNOWLEDGED=1` environment
variables to prevent accidental use.

### Iteration count

Each arm runs 100 iterations with 5 warm-up iterations discarded.
Between iterations, drop page cache (`echo 3 > /proc/sys/vm/drop_caches`)
to ensure cold-start parity and prevent the page cache from masking the
SQPOLL advantage.

## 4. Metrics

### Primary

| Metric | Unit | Collection method |
|--------|------|-------------------|
| Throughput | MB/s | `bytes_transferred / wall_time` per iteration |
| Submission latency | us (p50, p95, p99) | `io_uring_enter` return timestamp delta (Arm A) vs SQPOLL polling interval (Arm B) |

### Secondary

| Metric | Unit | Collection method |
|--------|------|-------------------|
| IOPS | ops/s | SQE completions / wall_time |
| CPU utilisation | % | `/proc/self/stat` utime+stime delta over bench window |
| Syscall count | count | `strace -c` on a single representative iteration |
| Context switches | count | `/proc/self/status` voluntary_ctxt_switches delta |

### Derived

| Metric | Formula | Interpretation |
|--------|---------|---------------|
| Throughput delta | `(B_throughput - A_throughput) / B_throughput * 100` | Percentage cost of the defensive disable |
| CPU efficiency | `throughput / cpu_utilisation` | Higher = better utilisation of CPU budget |
| Latency tax | `A_p95_latency - B_p95_latency` | Per-submission overhead of userspace entry |

## 5. Expected results

Based on prior SMR-1 bench data and the architectural analysis in
`project_sqpoll_disabled_with_mmap.md`:

| Fixture | Expected throughput delta | Rationale |
|---------|--------------------------|-----------|
| 1 GiB | 8-12% | SQPOLL amortisation kicks in but queue depth is moderate |
| 10 GiB | 12-15% | Sustained sequential I/O maximises SQPOLL's polling advantage |

The delta is bounded below by the `io_uring_enter(2)` syscall cost
(~1-2 us on modern kernels) multiplied by the submission rate. On NVMe
with 4K random IOPS > 500K, the syscall overhead at queue depth 64 is:

```
submissions/s = IOPS / QD = 500000 / 64 = 7812
syscall_overhead = 7812 * 1.5us = 11.7ms/s = 1.17%
```

For sequential workloads the submission rate is lower (fewer, larger
I/Os), so the 10-15% measured delta includes not just the syscall but
also the lost benefit of polling-mode completion harvesting (no
interrupt coalescing latency, no context switch to reap completions).

## 6. Decision criteria

| Measured throughput delta | Action |
|---------------------------|--------|
| < 5% | Accept SQM-3 as-is. The defensive disable is nearly free on this hardware class. Close SQM-4 with no follow-up. |
| 5-15% | Accept SQM-3 as the safe default. Document the cost. Open SQM-4.b to evaluate `MADV_WILLNEED` prefetch as a complementary optimisation that could recover partial throughput without re-enabling SQPOLL. |
| > 15% | Investigate `MADV_WILLNEED` prefetch alternative (Candidate 1 from SQM-1.c). If prefetch can bring the delta below 5% without reintroducing the race, implement as SQM-4.c. If not, evaluate per-slide `mlock`/`munlock` (Candidate 2) cost on this hardware. |
| Arm B produces EFAULT / hangs | Confirms the race is real on this hardware. Accept SQM-3 unconditionally regardless of throughput delta. |

### Statistical significance

Throughput delta claims require Welch's t-test at alpha = 0.05 between
100-sample Arm A and 100-sample Arm B distributions. A delta below the
test's minimum detectable effect (estimated at ~2% given NVMe variance)
is reported as "no significant difference" regardless of point estimate.

## 7. Hardware requirements

| Component | Requirement | Rationale |
|-----------|------------|-----------|
| Storage | NVMe SSD, PCIe Gen3x4 or better | SQPOLL benefit only manifests when device latency is low enough that syscall overhead is proportionally significant |
| IOPS baseline | >= 200K random 4K read IOPS (fio verified) | Below this the device is the bottleneck, not the submission path |
| Sequential BW | >= 2 GB/s sequential read | Ensures the fixture is I/O-bound, not CPU-bound |
| RAM | >= 32 GiB | Prevents page cache eviction from interfering with the 10 GiB cell |
| Kernel | Linux 5.6+ (SQPOLL support) | Required for io_uring SQPOLL |
| CPU | >= 4 cores | Isolate bench threads from SQPOLL kthread; pin via `taskset` |
| Filesystem | ext4 or XFS on the NVMe device | No network or FUSE filesystems |

### Pre-bench validation

Before collecting data, run `fio` to establish the device baseline:

```bash
fio --name=baseline --filename=/bench/fio_test \
    --rw=randread --bs=4k --ioengine=io_uring \
    --iodepth=64 --numjobs=1 --size=1G \
    --time_based --runtime=30 --group_reporting
```

Record the IOPS and latency percentiles. If the device does not meet
the minimum 200K random read IOPS, the bench results are not
representative of the target workload class and should not be used for
the decision criteria above.

## 8. Execution plan

1. Provision bench host meeting hardware requirements.
2. Generate fixtures (section 2).
3. Run `fio` baseline (section 7).
4. Run Arm A (100 iterations, default config).
5. Run Arm B (100 iterations, `SQM4_FORCE_SQPOLL=1 UNSAFE_ACKNOWLEDGED=1`).
6. Collect metrics (section 4).
7. Compute derived metrics and statistical tests (section 4/6).
8. Apply decision criteria (section 6).
9. File follow-up task (SQM-4.b or closure) based on result.

## 9. Relationship to other bench efforts

- **SMR-1 bench harness** (`crates/fast_io/benches/mmap_vs_read_fixed_basis.rs`):
  measures mmap vs `READ_FIXED` for basis reads. SQM-4.a measures the
  cost of *disabling SQPOLL* when mmap is chosen - a distinct axis.
- **IUB-2 multi-GB bench** (`docs/design/iouring-multi-gb-bench-design.md`):
  measures io_uring vs stdlib at scale. SQM-4.a holds io_uring constant
  and varies only the ring submission mode (SQPOLL vs regular).
- **IUS-3 SEND_ZC bench** (`docs/design/ius-3-send-zc-bench-design-2026-05-21.md`):
  network zero-copy; orthogonal to the receive-side basis-read path
  measured here.
