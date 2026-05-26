# SZC.d - SEND_ZC CPU overhead under concurrent daemon transfers

Date: 2026-05-26
Scope: bench design for measuring `IORING_OP_SEND_ZC` CPU overhead when
multiple daemon clients transfer simultaneously.
Status: design spec; implementation is a follow-up PR.
Predecessors:
- SZC.a (PR #5037): production-scale bench workload spec - defines
  three scenarios including the concurrent-client shape (section 2.3)
  that this spec refines and extends.
- SZC.b: sustained single-file throughput bench (10 GiB). Establishes
  the per-client SEND_ZC throughput baseline.
- SZC.c: high-IOPS bench (100K small files). Establishes the per-file
  overhead characteristics of the extra notification CQE.
- IUS-3 (PR #4680): primitive-isolated bench at
  `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`.
- IUS-4 (PR merged): keep-opt-in decision under data-missing branch.
Successors:
- SZC.f: decision on promoting SEND_ZC to default-on, consuming
  SZC.b + SZC.c + SZC.d numbers to revisit IUS-4.

## 1. Motivation

SZC.b measures SEND_ZC on a single sustained transfer - the
best-case scenario for zero-copy savings where the kernel avoids
copying 10 GiB through the CPU cache. SZC.c measures the worst-case
per-file overhead where double CQE draining may dominate. Neither
captures the scenario where SEND_ZC delivers its most compelling
production benefit: multi-tenant daemon serving.

A production daemon serves tens to hundreds of concurrent pull clients.
Each client occupies a thread with its own per-thread io_uring ring.
With plain `IORING_OP_SEND`, every send copies payload bytes from the
user-space buffer pool into kernel socket buffers - one memcpy per
send, per client, per chunk. Under N concurrent clients, the total
CPU cost of these copies scales linearly with N: N clients transferring
1 GiB each produce N GiB of memcpy traffic through the CPU cache.
SEND_ZC eliminates all N copies by pinning user pages for DMA, freeing
CPU cycles that can serve additional clients or reduce per-client
latency.

This spec designs a controlled bench that sweeps N from 1 to 16 and
measures how SEND_ZC's CPU savings scale with concurrency. The results
combine with SZC.b (single-client throughput ceiling) and SZC.c
(per-file overhead floor) to form the complete evidence set for the
SZC.f default-on decision.

### 1.1 Why SZC.d extends SZC.a section 2.3

SZC.a section 2.3 sketches a 50-client concurrent bench with 100 MiB
per client. That sketch serves the umbrella spec as one of three
scenarios. SZC.d refines it with:

- **Concurrency sweep.** N = 1, 4, 8, 16 instead of a single fixed
  N = 50. The sweep reveals the scaling curve - whether CPU savings
  are linear, sub-linear, or plateau at a specific N.
- **Larger per-client payload.** 1 GiB per client instead of 100 MiB.
  A larger payload sustains the transfer long enough that per-client
  CPU time measurement is statistically significant (minutes, not
  seconds).
- **Per-client metrics.** SZC.a tracks aggregate wall-clock only.
  SZC.d captures per-client latency, allowing variance analysis across
  clients sharing the same daemon.
- **CPU accounting detail.** Separate user-time and sys-time per client
  thread, not just aggregate process CPU. Sys-time is the primary
  signal for memcpy elimination.

## 2. Workload design

### 2.1 Fixture

A single 1 GiB file with deterministic pseudo-random content. All N
clients pull the same file. This isolates the send primitive from
per-file overhead (file-list exchange, metadata, etc.) and ensures
every client exercises an identical code path.

```bash
#!/bin/bash
# Generate a 1 GiB fixture with deterministic content.
FIXTURE_DIR="${SZC_FIXTURE_DIR:-/tmp/szc-bench/concurrent}"
mkdir -p "$FIXTURE_DIR"

if [ ! -f "$FIXTURE_DIR/payload.bin" ] || \
   [ "$(stat -c%s "$FIXTURE_DIR/payload.bin" 2>/dev/null)" != "1073741824" ]; then
  openssl enc -aes-256-ctr \
    -K "$(printf '0%.0s' {1..64})" \
    -iv "$(printf '0%.0s' {1..32})" \
    -nosalt < /dev/zero 2>/dev/null | \
    head -c 1073741824 > "$FIXTURE_DIR/payload.bin"
fi
```

### 2.2 Concurrency levels

| N | Total data | Why |
|---|-----------|-----|
| 1 | 1 GiB | Baseline: matches SZC.b single-client shape for cross-validation. |
| 4 | 4 GiB | Light concurrency. Exposes fixed-overhead costs that N=1 hides. |
| 8 | 8 GiB | Moderate concurrency. CPU cache pressure from 8 simultaneous memcpy streams should become visible. |
| 16 | 16 GiB | Heavy concurrency. Approaches the github-hosted runner's 4-core ceiling. CPU saturation under plain SEND should surface clearly. |

N = 16 is chosen as the upper bound because github-hosted runners
have 4 vCPUs. At N = 16 with plain SEND, the 16 memcpy streams
compete for 4 cores' cache bandwidth. Higher N values would primarily
measure OS scheduler overhead rather than SEND_ZC savings.

### 2.3 Transfer mode

Each client runs upstream `rsync` pulling from the oc-rsync daemon:

```bash
rsync --no-compress --whole-file \
  rsync://127.0.0.1:$PORT/bench/payload.bin \
  "$DST_DIR/client-$i/"
```

Using upstream rsync as the client isolates the measurement to the
daemon's send path. The client's receive path is identical across
both arms (send-plain vs send-zc) - it is upstream rsync in both
cases.

### 2.4 Destination

Each client writes to a separate directory under tmpfs to avoid disk
I/O variance contaminating the measurement:

```bash
DST_ROOT=$(mktemp -d -p /dev/shm szc-d-XXXXXX)
for i in $(seq 1 $N); do
  mkdir -p "$DST_ROOT/client-$i"
done
```

## 3. Two-arm comparison

### 3.1 Arm definitions

| Label | Build | Daemon invocation |
|-------|-------|-------------------|
| `send-plain` | `cargo build --release` (default features) | `oc-rsync --daemon --config=...` |
| `send-zc` | `cargo build --release --features iouring-send-zc` | `oc-rsync --daemon --zero-copy --config=...` |

Both arms use the same daemon configuration, same fixture directory,
same upstream rsync client binary, same concurrency level N. The only
difference is the send primitive used by the daemon's per-thread
io_uring ring.

### 3.2 Build isolation

The two binaries are built into separate target directories to avoid
rebuild costs between arms:

```bash
cargo build --release -p oc-rsync --target-dir target/send-plain
cargo build --release -p oc-rsync --features iouring-send-zc \
  --target-dir target/send-zc
```

### 3.3 Daemon restart between iterations

The daemon is stopped and restarted before each hyperfine iteration.
This ensures each measurement captures cold-start plus transfer,
matching the methodology in SZC.a. Buffer pools, per-thread rings,
and registered-buffer allocations are fresh per iteration.

## 4. Bench methodology

### 4.1 Client orchestration

All N clients are launched simultaneously and timed as a group.
The bench script uses a barrier pattern to synchronize client start:

```bash
#!/bin/bash
# Launch N concurrent rsync clients and wait for all to complete.
N=$1; PORT=$2; DST_ROOT=$3
PIDS=()

for i in $(seq 1 "$N"); do
  rsync --no-compress --whole-file \
    "rsync://127.0.0.1:$PORT/bench/payload.bin" \
    "$DST_ROOT/client-$i/" &
  PIDS+=($!)
done

# Wait for all clients and collect exit codes.
FAILURES=0
for pid in "${PIDS[@]}"; do
  wait "$pid" || ((FAILURES++))
done
exit $FAILURES
```

### 4.2 Hyperfine invocation

For each concurrency level N and each arm (send-plain, send-zc),
hyperfine runs the client orchestration script:

```bash
hyperfine \
  --warmup 1 \
  --runs 5 \
  --prepare "rm -rf $DST_ROOT && mkdir -p $DST_ROOT" \
  --setup "start_daemon $ARM $PORT" \
  --cleanup "stop_daemon" \
  --export-json "/tmp/szc-d-N${N}-${ARM}.json" \
  --command-name "N=${N}-${ARM}" \
  "./run_clients.sh $N $PORT $DST_ROOT"
```

Each concurrency level runs as a separate hyperfine invocation (not
interleaved with other N values) to avoid resource contention between
different concurrency levels.

### 4.3 Per-client timing

In addition to hyperfine's aggregate wall-clock, the client
orchestration script captures per-client timing:

```bash
for i in $(seq 1 "$N"); do
  /usr/bin/time -v -o "$METRICS_DIR/client-${N}-${ARM}-${i}.time" \
    rsync --no-compress --whole-file \
      "rsync://127.0.0.1:$PORT/bench/payload.bin" \
      "$DST_ROOT/client-$i/" &
  PIDS+=($!)
done
```

This produces N separate `/usr/bin/time -v` output files per
iteration, each containing user time, sys time, RSS, and context
switches for that client's rsync process.

### 4.4 Daemon-side CPU accounting

The daemon process's CPU consumption is the primary signal. The bench
harness captures it by wrapping the daemon start with:

```bash
# Record daemon PID and sample /proc/<pid>/stat at transfer end.
DAEMON_PID=$(cat "$WORK/rsyncd.pid")

# Before clients start:
STAT_BEFORE=$(cat "/proc/$DAEMON_PID/stat")

# After all clients finish:
STAT_AFTER=$(cat "/proc/$DAEMON_PID/stat")

# Extract utime (field 14) and stime (field 15) in jiffies.
UTIME_BEFORE=$(echo "$STAT_BEFORE" | awk '{print $14}')
STIME_BEFORE=$(echo "$STAT_BEFORE" | awk '{print $15}')
UTIME_AFTER=$(echo "$STAT_AFTER" | awk '{print $14}')
STIME_AFTER=$(echo "$STAT_AFTER" | awk '{print $15}')

CLK_TCK=$(getconf CLK_TCK)
USER_SECONDS=$(python3 -c "print(($UTIME_AFTER - $UTIME_BEFORE) / $CLK_TCK)")
SYS_SECONDS=$(python3 -c "print(($STIME_AFTER - $STIME_BEFORE) / $CLK_TCK)")
```

This isolates the daemon's CPU from the client processes' CPU,
giving a direct measurement of the send-path overhead.

## 5. Metrics

### 5.1 Primary metrics (go/no-go for SZC.f)

| Metric | Source | Unit | Significance |
|--------|--------|------|--------------|
| Aggregate throughput | `N * 1 GiB / wall_seconds` from hyperfine | MB/s | First-order operator signal: how fast does the daemon serve N clients? |
| Daemon sys-CPU time | `/proc/<pid>/stat` field 15 delta | seconds | Direct measure of kernel copy cost eliminated by SEND_ZC. |
| Daemon total CPU time | user + sys from `/proc/<pid>/stat` | seconds | Captures both memcpy (sys) and CQE-drain overhead (user). |

### 5.2 Secondary metrics (diagnostic)

| Metric | Source | Unit | Purpose |
|--------|--------|------|---------|
| Per-client latency | `/usr/bin/time -v` per client | seconds | Variance across clients reveals fairness under contention. |
| Per-client sys-CPU | `/usr/bin/time -v` per client | seconds | Client-side sys-CPU should be identical across arms (upstream rsync client, not oc-rsync). |
| Peak RSS (daemon) | `/proc/<pid>/status` VmHWM | KiB | SEND_ZC pins pages via `get_user_pages_fast`; RSS should not grow disproportionately. |
| Voluntary context switches (daemon) | `/proc/<pid>/status` | count | Extra CQE notifications may increase context switches. |
| Involuntary context switches (daemon) | `/proc/<pid>/status` | count | CPU saturation signal at high N. |

### 5.3 Derived metrics

| Metric | Formula | Purpose |
|--------|---------|---------|
| CPU savings ratio | `1 - (send_zc_total_cpu / send_plain_total_cpu)` | Headline number for the SZC.f decision. |
| CPU savings per client | `savings_ratio / N` | Whether savings scale linearly with N. |
| Throughput gain | `send_zc_throughput / send_plain_throughput` | Whether CPU savings translate to observable throughput improvement. |
| Per-client latency stddev | `stddev(client_latencies)` | Fairness metric: lower variance means more uniform service. |

## 6. Expected outcomes

### 6.1 CPU savings scaling hypothesis

Under plain SEND, the daemon copies N GiB of payload through the CPU
cache (one memcpy per send, per client). The total sys-CPU time should
scale roughly linearly with N:

```
sys_cpu_plain(N) ~ k * N  (where k is the per-client copy cost)
```

Under SEND_ZC, the memcpy is eliminated. The remaining sys-CPU is
ring submission overhead plus notification CQE drain:

```
sys_cpu_zc(N) ~ c * N  (where c << k is the per-client ring overhead)
```

The CPU savings ratio should therefore be:

```
savings(N) = 1 - c/k  (constant fraction, independent of N)
```

If SZC.b shows a 15% sys-CPU reduction at N=1, the concurrent bench
should show approximately the same 15% reduction at every N - but the
absolute savings in CPU-seconds grow linearly with N.

### 6.2 Throughput improvement hypothesis

At low N (1-4), the daemon is not CPU-bound on a 4-core runner. CPU
savings do not translate to throughput improvement because the
bottleneck is loopback socket buffer throughput, not CPU.

At high N (8-16), the 4 cores become saturated with memcpy work under
plain SEND. Eliminating the memcpy with SEND_ZC frees CPU cycles for
ring submission and protocol framing, improving aggregate throughput.

Expected shape:

| N | Throughput gain |
|---|----------------|
| 1 | ~1.00x (loopback-bound) |
| 4 | ~1.00-1.05x (some CPU headroom) |
| 8 | ~1.05-1.15x (CPU pressure emerging) |
| 16 | ~1.10-1.25x (CPU saturation under plain SEND) |

### 6.3 RSS impact

SEND_ZC pins user pages. With `ZERO_COPY_SLOT_COUNT = 8` and
`ZERO_COPY_SLOT_BYTES = 256 KiB`, each client thread pins 2 MiB.
At N = 16, total pinned memory is 32 MiB. This should be visible in
VmHWM but should not exceed 1.1x the plain SEND RSS since the daemon's
base RSS (buffer pool, file list, protocol state) dominates at these
transfer sizes.

### 6.4 Context switches

SEND_ZC drains two CQEs per submission vs one for plain SEND. With a
1 GiB file sent in 256 KiB chunks, each client issues ~4096 sends.
At N = 16, the total extra CQE drain is 16 * 4096 = 65536 additional
CQEs. These are drained in user space (no syscall per CQE) so context
switch impact should be minimal. If context switches increase by more
than 10%, the ring sizing or CQE drain strategy needs investigation.

## 7. Pass/fail criteria

### 7.1 Promotion gate (feeds SZC.f decision)

All criteria measured on kernels >= 6.1.

| Criterion | Threshold | Scope |
|-----------|-----------|-------|
| CPU savings exist | send-zc daemon total CPU < send-plain daemon total CPU | All N >= 4 |
| CPU savings scale | savings ratio at N=16 >= savings ratio at N=1 | Scaling does not degrade with concurrency |
| No throughput regression | send-zc aggregate throughput >= 0.97x send-plain | All N |
| Throughput gain at high N | send-zc throughput >= 1.05x send-plain | N >= 8 |
| RSS stability | send-zc daemon peak RSS <= 1.15x send-plain | All N |
| Context switch stability | send-zc voluntary csw <= 1.15x send-plain | All N |
| Per-client fairness | send-zc latency stddev <= send-plain latency stddev | N >= 4 |

### 7.2 Combined SZC.b + SZC.c + SZC.d interpretation

| SZC.b (sustained) | SZC.c (IOPS) | SZC.d (concurrent) | SZC.f decision |
|-------------------|--------------|-------------------|---------------|
| Throughput gain + CPU reduction | No regression (within 3%) | CPU scales + throughput gain at high N | **Promote to default-on.** |
| Throughput gain + CPU reduction | No regression | CPU scales but no throughput gain | **Promote to default-on** for bulk workloads; document high-concurrency as bonus. |
| Throughput gain + CPU reduction | Regression > 5% | CPU scales | Raise `SEND_ZC_MIN_BYTES` threshold. Retest SZC.c. Promote if retest passes. |
| CPU-only benefit (no throughput) | No regression | CPU scales, no throughput | **Keep opt-in.** Document CPU savings for CPU-constrained hosts. |
| Any | Any | CPU does not scale (savings plateau or regress at high N) | **Keep opt-in.** Investigate CQE contention or ring overhead. |
| Any | Regression > 5% | Regression | **Keep opt-in.** SEND_ZC unsuitable for mixed workloads. |

### 7.3 Statistical significance

Same methodology as SZC.a section 6.3: a result is considered
significant only when the difference between send-zc and send-plain
exceeds 2x the pooled standard deviation. Results within the noise
floor are reported as "no significant difference."

For the concurrency sweep, significance is evaluated independently at
each N value. A result that is significant at N=16 but not at N=1 is
interpretable (CPU savings emerge under contention) and does not
invalidate the N=1 result.

## 8. Kernel requirements

Same as SZC.b: Linux kernel 6.0+ minimum, with 6.1+ as the primary
gating kernel.

| Kernel | SEND_ZC | Registered buffers | Bench status |
|--------|---------|-------------------|-------------|
| < 6.0 | Absent | N/A | Skip with green status |
| 6.0 | Present | Absent (needs 6.2) | Secondary row: unregistered path only |
| 6.1 | Present | Present | Primary gating row |
| 6.6 LTS | Present (stable) | Present | Primary gating row |

The bench harness checks `uname -r` at startup and skips with a
diagnostic message if the kernel is below 6.0. On github-hosted
ubuntu-latest runners (currently kernel 6.8+), all rows are active.

## 9. Daemon configuration

```ini
port = 0
log file = /tmp/szc-bench/rsyncd.log
pid file = /tmp/szc-bench/rsyncd.pid
max connections = 64

[bench]
    path = $SZC_FIXTURE_DIR/concurrent
    read only = true
    use chroot = false
```

`max connections = 64` is set well above the maximum N (16) to ensure
the admission gate never rejects clients. The bench verifies all N
clients receive complete transfers (exit code 0) on every iteration.

For the `send-zc` arm, the daemon invocation adds `--zero-copy`:

```bash
target/send-zc/release/oc-rsync --daemon --zero-copy \
  --config=/tmp/szc-bench/rsyncd.conf
```

The harness sets `ulimit -l unlimited` before starting the daemon to
avoid `ENOMEM` from `io_uring_register` when pinning registered
buffers across N threads.

## 10. Execution order

The bench script runs the full concurrency sweep sequentially by N,
with both arms at each N value:

1. Generate fixture (once, cached).
2. Build both arm binaries.
3. For N in 1, 4, 8, 16:
   a. Run `send-plain` arm with hyperfine (5 runs).
   b. Run `send-zc` arm with hyperfine (5 runs).
   c. Collect daemon-side and per-client metrics.
4. Aggregate results into summary table.
5. Upload artifacts.

Arms are not interleaved within hyperfine (unlike SZC.a) because the
daemon binary differs between arms. Each arm requires a fresh daemon
start with the correct binary.

## 11. CI integration

### 11.1 Workflow placement

The bench runs as a **non-required, nightly-only** GitHub Actions
workflow at `.github/workflows/bench-send-zc-concurrent.yml`. It does
not trigger on pull requests because the full sweep (4 N values x
2 arms x 5 runs = 40 daemon starts + 40 client groups) takes 20-40
minutes depending on runner performance.

```yaml
name: Bench SEND_ZC concurrent daemon transfers
on:
  workflow_dispatch:
  schedule:
    - cron: '23 6 * * *'  # 06:23 UTC, offset from other bench crons
```

### 11.2 Runner requirements

- **ubuntu-latest** (x86_64) with kernel >= 6.1.
- **Disk**: fixture is 1 GiB; N=16 destinations on tmpfs need 16 GiB of
  `/dev/shm`. github-hosted runners provide 7 GiB RAM. With tmpfs
  backed by swap this is sufficient; alternatively, destinations can
  use the SSD partition (14 GiB free) at the cost of some disk I/O
  noise. The bench script checks `/dev/shm` free space and falls back
  to disk destinations with a logged warning.
- **CPU**: 4 vCPUs. The N=16 level intentionally over-subscribes CPU
  to surface the SEND_ZC advantage under saturation.
- **Memory**: daemon with 16 threads uses ~500 MiB RSS. 16 client
  rsync processes use ~50 MiB each (~800 MiB total). Well within 7 GiB.

### 11.3 Artifact upload

Each run uploads:

- `szc-d-N{N}-{arm}.json` - hyperfine results per (N, arm) pair.
- `daemon-{N}-{arm}.stat` - `/proc/<pid>/stat` deltas per (N, arm).
- `daemon-{N}-{arm}.status` - `/proc/<pid>/status` snapshot (VmHWM,
  context switches).
- `client-{N}-{arm}-{i}.time` - `/usr/bin/time -v` per client per
  (N, arm).
- `szc-d-summary.md` - rendered to `$GITHUB_STEP_SUMMARY`.

Artifacts are retained for 30 days.

### 11.4 Summary table format

The `$GITHUB_STEP_SUMMARY` renders a table for each N:

```markdown
### N = 8

| Metric | send-plain | send-zc | Delta |
|--------|-----------|---------|-------|
| Wall-clock (s) | 12.3 +/- 0.4 | 11.1 +/- 0.3 | -9.8% |
| Aggregate throughput (MB/s) | 665 | 737 | +10.8% |
| Daemon user CPU (s) | 5.1 | 4.9 | -3.9% |
| Daemon sys CPU (s) | 8.7 | 6.2 | -28.7% |
| Daemon total CPU (s) | 13.8 | 11.1 | -19.6% |
| Daemon peak RSS (KiB) | 245000 | 261000 | +6.5% |
| Daemon vol csw | 3200 | 3500 | +9.4% |
| Client latency stddev (s) | 0.8 | 0.6 | -25.0% |
```

### 11.5 Relationship to existing bench workflows

| Workflow | Scope | Trigger |
|----------|-------|---------|
| `bench-daemon-coldstart.yml` | Daemon cold-start | Nightly + PR paths |
| `bench-daemon-concurrency.yml` | Daemon thread ceiling | Nightly + PR paths |
| `bench-send-zc-production.yml` (SZC.a) | SEND_ZC production-scale | Nightly + manual |
| `bench-send-zc-concurrent.yml` (this) | SEND_ZC concurrent CPU overhead | Nightly + manual |

This workflow is independent of the existing bench infrastructure.
It does not share fixtures, artifacts, or pass/fail gates with other
workflows. If SZC.f promotes SEND_ZC to default-on, the regression
guard should be consolidated into `bench-send-zc-production.yml`
rather than running both workflows indefinitely.

## 12. Risk factors

### 12.1 Shared-runner CPU variance

github-hosted runners have 2-10% CPU variability across runs. At
N = 16 on 4 vCPUs, the CPU is saturated and variance decreases (the
system is pegged at 100%). At N = 1, variance is higher. The bench
mitigates this by:

- Running 5 iterations per (N, arm) pair.
- Requiring 2x pooled stddev significance (section 7.3).
- Logging `/proc/cpuinfo` model and `lscpu` output.

### 12.2 Loopback throughput ceiling

Loopback TCP throughput on the runner exceeds 10 Gbps. At N = 1
with a 1 GiB file, the transfer completes in under 2 seconds. CPU
measurement resolution from `/proc/<pid>/stat` is 10 ms (1 jiffy).
With only ~200 jiffies of total CPU at N = 1, a 15% difference is
~30 jiffies - measurable but noisy.

At N = 16 with 16 GiB total, the transfer runs long enough
(~20-30 seconds) to accumulate thousands of jiffies, making the CPU
measurement reliable.

### 12.3 Buffer pinning RLIMIT_MEMLOCK

With N = 16 threads, the daemon pins 16 * 2 MiB = 32 MiB of
registered buffers. The `ulimit -l unlimited` in the harness
prevents ENOMEM. On production hosts without unlimited memlock,
operators need `LimitMEMLOCK=` in the systemd unit. This is a
deployment consideration, not a bench risk.

### 12.4 tmpfs swap pressure

At N = 16 with 16 GiB of destinations on tmpfs, the 7 GiB RAM
runner will swap. Swap I/O adds noise to wall-clock measurements.
Mitigation: use disk destinations for N >= 8 and note the I/O
overhead in results. The primary metric (daemon CPU time) is not
affected by client-side disk I/O.

### 12.5 Client process overhead

16 concurrent upstream rsync processes consume CPU for receiving and
writing. On a 4-core runner, client CPU contends with daemon CPU.
This is realistic (production hosts serve real clients) but means
the daemon cannot achieve full loopback throughput at high N.
The bench accepts this as the production-representative operating
point.

## 13. Out of scope (deliberate)

- **N > 16.** Beyond 16 on a 4-core runner, the measurement is
  dominated by OS scheduler overhead and swap pressure, not SEND_ZC
  characteristics. Higher N values belong on dedicated hardware.

- **Mixed concurrency (different file sizes per client).** All
  clients pull the same 1 GiB file. Mixed workloads conflate per-file
  overhead with concurrency effects. SZC.c covers the per-file
  overhead axis independently.

- **Delta transfers.** Same rationale as SZC.a section 11:
  `--whole-file` isolates the send path from the delta engine.

- **Compression.** Same rationale as SZC.a: compression moves the
  bottleneck from socket send to CPU codec.

- **SSH transport.** SEND_ZC is socket-only. SSH pipe I/O is out of
  scope.

- **Real NIC.** Loopback is a conservative lower bound. Real-NIC
  results would amplify SEND_ZC's advantage but require dedicated
  two-host infrastructure.

- **Non-Linux platforms.** SEND_ZC is Linux io_uring only. macOS
  and Windows are not applicable.

## 14. References

- SZC.a bench workload: `docs/design/szc-a-send-zc-bench-workload.md`
- SZC.e kernel correctness: `docs/design/szc-e-send-zc-kernel-correctness.md`
- IUS-3 bench design: `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`
- IUS-3 bench harness: `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`
- IUS-4 decision (keep opt-in): `docs/design/ius-4-decision-2026-05-22.md`
- SEND_ZC design: `docs/design/iouring-send-zc.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Socket writer dispatch: `crates/fast_io/src/io_uring/socket_writer.rs`
- Feature flag: `crates/fast_io/Cargo.toml` (`iouring-send-zc`)
- DIS-8.a bench template: `.github/workflows/bench-daemon-coldstart.yml`
- Daemon concurrency bench: `.github/workflows/bench-daemon-concurrency.yml`
