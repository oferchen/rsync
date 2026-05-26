# SZC.a - IORING_OP_SEND_ZC production-scale bench workload spec

Date: 2026-05-26
Scope: bench design for production-scale SEND_ZC evidence to revisit the
IUS-4 keep-opt-in decision.
Status: design spec; implementation is a follow-up PR.
Predecessors:
- IUS-3 (PR #4680): loopback-isolated send primitive bench - four
  workload shapes (16 KiB, 256 KiB, 1 MiB, mixed), criterion harness
  at `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`.
- IUS-4 (PR merged): keep-opt-in decision under data-missing branch.
  No multi-kernel hardware numbers were captured.
Successors:
- SZC.b: implementation of the bench harness + fixture generator.
- SZC.c: numbers capture on multi-kernel hardware.
- IUS-4 reopen: consumes SZC.c numbers to revisit the default-on gate.

## 1. Motivation

IUS-3 isolated the `IORING_OP_SEND_ZC` vs `IORING_OP_SEND` primitive
on a TCP loopback pair with no filesystem I/O, no compression, and no
protocol framing. That isolation was deliberate - it attributed the
delta to the send primitive only - but it left three production-shaped
questions unanswered:

1. **Sustained large-file throughput.** IUS-3's largest workload is
   100 x 1 MiB chunks (100 MiB total). Production daemon pulls
   routinely transfer multi-GB files where the kernel's page-copy
   avoidance accumulates over minutes, not milliseconds.

2. **High-IOPS small-file storms.** IUS-3 covers 10 000 x 16 KiB
   chunks but not the full daemon transfer shape: file-list exchange,
   `MSG_DATA` framing, per-file stat, per-file checksum. The extra
   notification CQE per SEND_ZC doubles the CQ drain cost; at high
   per-file rates this may dominate the zero-copy savings.

3. **Concurrent daemon clients.** IUS-3 runs single-sender. The
   production daemon serves tens of concurrent pull clients, each on
   its own thread and its own per-thread io_uring ring. CPU savings
   from SEND_ZC compound across clients but so does the risk of
   notification-storm contention on the CQ.

This spec designs three bench scenarios that cover these gaps and
defines the pass/fail criteria that would justify reopening IUS-4.

## 2. Bench scenarios

All scenarios run oc-rsync in daemon mode (`oc-rsync --daemon`) on
localhost, transferring from a pre-generated fixture directory to
`/dev/null` (or a tmpfs mount for scenarios requiring receiver-side
state). The daemon is restarted between each hyperfine iteration to
capture cold-start + transfer end-to-end.

### 2.1 Scenario 1: single 10 GiB file (sustained throughput)

**Goal.** Measure sustained socket-send throughput where kernel copy
avoidance matters most. A single large file eliminates per-file
overhead; the entire wall-clock delta is attributable to the send
primitive and checksum computation.

| Parameter | Value |
|-----------|-------|
| Fixture | 1 file, 10 GiB, seeded pseudo-random content |
| Transfer mode | `rsync://localhost:<port>/bench/large.bin .` (daemon pull) |
| Flags | `--no-compress --whole-file` (isolate send from delta/compress) |
| Iterations | `hyperfine --warmup 1 --runs 5` |
| Measurement window | Full transfer wall-clock |

**Why this shape.** SEND_ZC's documented benefit is eliminating the
`memcpy` from user pages to kernel socket buffers. On a 10 GiB
transfer that copy moves ~10 GiB of data through the CPU cache; the
SEND_ZC path avoids this entirely by pinning user pages for DMA. If
SEND_ZC cannot demonstrate a measurable throughput or CPU improvement
on a sustained 10 GiB transfer, the feature has no path to default-on
for the bulk-transfer use case.

### 2.2 Scenario 2: 100K x 10 KiB files (high-IOPS)

**Goal.** Measure per-file overhead where the extra SEND_ZC
notification CQE may dominate. Each file requires file-list metadata,
a `MSG_DATA` frame, and checksum overhead on top of the payload send.
The SEND_ZC path drains two CQEs per submission vs one for plain SEND;
at 100K files the aggregate CQE-drain cost is 100K extra kernel-user
transitions.

| Parameter | Value |
|-----------|-------|
| Fixture | 100 000 files, each 10 KiB, in a flat directory |
| Transfer mode | `rsync://localhost:<port>/bench/ .` (daemon pull) |
| Flags | `--no-compress --whole-file` |
| Iterations | `hyperfine --warmup 1 --runs 5` |
| Measurement window | Full transfer wall-clock |

**Why this shape.** If SEND_ZC regresses on the high-IOPS workload by
more than 5%, the `SEND_ZC_MIN_BYTES` threshold at
`crates/fast_io/src/io_uring/socket_writer.rs` is too low and should
be raised, or the feature should stay opt-in for operators who know
their workload is bulk-dominated.

### 2.3 Scenario 3: 50 concurrent daemon clients x 100 MiB each

**Goal.** Measure aggregate CPU savings when SEND_ZC's per-send copy
avoidance compounds across concurrent clients. Each client pulls
100 MiB (10 files x 10 MiB) from the same daemon instance. The daemon
spawns one thread per client, each with its own per-thread io_uring
ring. 50 clients x 100 MiB = 5 GiB total transfer.

| Parameter | Value |
|-----------|-------|
| Fixture | 10 files x 10 MiB each, seeded pseudo-random content |
| Transfer mode | 50 parallel `rsync://` pulls launched via `xargs -P 50` |
| Flags | `--no-compress --whole-file` per client |
| Iterations | `hyperfine --warmup 1 --runs 3` |
| Measurement window | Wall-clock from first client start to last client finish |

**Why this shape.** The daemon's thread-per-connection model means
SEND_ZC's CPU savings multiply across clients. On a CPU-bound host
serving 50 concurrent pulls, a 25% sys-CPU reduction per client
translates to a material increase in achievable concurrency ceiling
before the host saturates. Conversely, if the extra notification CQEs
cause contention (lock pressure on the per-thread ring, or CQ overflow
from notification storms), this scenario surfaces the regression.

## 3. Fixture generation

Fixtures are generated once per bench host and cached. The bench
harness checks for the fixture directory and skips generation if the
files match the expected count and sizes.

### 3.1 Large file (Scenario 1)

```bash
#!/bin/bash
# Generate a 10 GiB file with deterministic pseudo-random content.
# fallocate pre-allocates on ext4/xfs without writing zeros;
# dd with a seeded openssl enc fills the content deterministically.
FIXTURE_DIR="${SZC_FIXTURE_DIR:-/tmp/szc-bench/large}"
mkdir -p "$FIXTURE_DIR"

if [ ! -f "$FIXTURE_DIR/large.bin" ] || \
   [ "$(stat -c%s "$FIXTURE_DIR/large.bin" 2>/dev/null)" != "10737418240" ]; then
  # Deterministic content via openssl enc with a fixed key+IV so
  # reruns produce byte-identical fixtures. AES-256-CTR is fast and
  # the output is pseudo-random with uniform byte distribution.
  openssl enc -aes-256-ctr \
    -K "$(printf '0%.0s' {1..64})" \
    -iv "$(printf '0%.0s' {1..32})" \
    -nosalt < /dev/zero 2>/dev/null | \
    head -c 10737418240 > "$FIXTURE_DIR/large.bin"
fi
```

### 3.2 Small files (Scenario 2)

```bash
#!/bin/bash
# Generate 100K x 10 KiB files with deterministic content.
FIXTURE_DIR="${SZC_FIXTURE_DIR:-/tmp/szc-bench/small}"
mkdir -p "$FIXTURE_DIR"

existing=$(find "$FIXTURE_DIR" -maxdepth 1 -type f | wc -l)
if [ "$existing" -ne 100000 ]; then
  rm -rf "$FIXTURE_DIR"
  mkdir -p "$FIXTURE_DIR"
  # Use a seeded PRNG for deterministic content. Each file is 10 KiB.
  # seq + printf is faster than 100K openssl invocations.
  for i in $(seq 0 99999); do
    dd if=/dev/urandom bs=10240 count=1 2>/dev/null \
      > "$FIXTURE_DIR/$(printf 'f%06d.dat' "$i")"
  done
fi
```

For CI, the small-file fixture generation can be parallelized with
`xargs -P $(nproc)` or replaced with a Rust helper binary that
writes all 100K files from a single seeded ChaCha20 stream (no
`rand` dependency - use the same LCG approach as the IUS-3 bench
at `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`).

### 3.3 Medium files (Scenario 3)

```bash
#!/bin/bash
# Generate 10 x 10 MiB files with deterministic content.
FIXTURE_DIR="${SZC_FIXTURE_DIR:-/tmp/szc-bench/medium}"
mkdir -p "$FIXTURE_DIR"

existing=$(find "$FIXTURE_DIR" -maxdepth 1 -type f | wc -l)
if [ "$existing" -ne 10 ]; then
  rm -rf "$FIXTURE_DIR"
  mkdir -p "$FIXTURE_DIR"
  for i in $(seq 0 9); do
    openssl enc -aes-256-ctr \
      -K "$(printf '%02x' "$i")$(printf '0%.0s' {1..62})" \
      -iv "$(printf '0%.0s' {1..32})" \
      -nosalt < /dev/zero 2>/dev/null | \
      head -c 10485760 > "$FIXTURE_DIR/$(printf 'f%02d.dat' "$i")"
  done
fi
```

### 3.4 Fixture location

All fixtures live under `$SZC_FIXTURE_DIR` (default `/tmp/szc-bench/`).
The bench script creates the directory tree:

```
/tmp/szc-bench/
  large/large.bin       # 10 GiB
  small/f000000.dat ... # 100K x 10 KiB
  medium/f00.dat ...    # 10 x 10 MiB
```

On CI, the fixture directory is an Actions cache artifact keyed on the
generation script hash so fixtures are regenerated only when the
script changes.

## 4. Measurement methodology

Each scenario captures the following metrics. The first two are
primary (go/no-go); the remaining three are diagnostic.

### 4.1 Wall-clock throughput (MB/s)

Source: `hyperfine --export-json` output. Computed as
`total_bytes_transferred / mean_wall_seconds`.

This is the first-order signal. If SEND_ZC does not improve
wall-clock throughput, operators have no reason to opt in regardless
of CPU savings - they care about how long the sync takes.

### 4.2 CPU time (user + sys)

Source: `/usr/bin/time -v` wrapper around each hyperfine iteration,
parsed from the `User time (seconds)` and `System time (seconds)`
lines. Alternatively, `/proc/<pid>/stat` fields 14 (`utime`) and 15
(`stime`) sampled at transfer start and end.

SEND_ZC's primary documented benefit is sys-CPU reduction. On a
loopback transfer the wall-clock delta may be noise while sys-CPU
drops materially. The CPU metric catches "SEND_ZC saves CPU but
wall time is flat" - still a win on CPU-bound hosts serving many
concurrent clients.

### 4.3 Context switches (voluntary + involuntary)

Source: `/usr/bin/time -v` fields `Voluntary context switches` and
`Involuntary context switches`, or `/proc/<pid>/status` fields
`voluntary_ctxt_switches` and `nonvoluntary_ctxt_switches`.

SEND_ZC drains two CQEs per submission vs one for plain SEND. If the
extra CQE drain causes measurably more context switches, the dispatch
threshold (`SEND_ZC_MIN_BYTES`) may need adjustment. Context-switch
inflation without a throughput gain is a regression signal.

### 4.4 io_uring CQE notification count

Source: instrumentation counter in the bench harness. The harness
wraps `try_send_zc` and counts the number of notification CQEs
(`IORING_CQE_F_NOTIF` set) observed per transfer. For the SEND_ZC
path this should equal the number of submissions; for the plain SEND
path it should be zero.

This metric verifies the bench is exercising the zero-copy path
(not silently falling back to plain SEND) and quantifies the extra
CQ pressure per scenario.

### 4.5 Peak RSS (KiB)

Source: `/usr/bin/time -v` field `Maximum resident set size (kbytes)`
or `/proc/<pid>/status` field `VmHWM`.

SEND_ZC pins user pages via `get_user_pages_fast`; the pinned pages
count against `RLIMIT_MEMLOCK` and inflate VmHWM. If peak RSS grows
disproportionately under SEND_ZC, the registered-buffer pool sizing
at `crates/fast_io/src/io_uring/send_zc.rs` (`ZERO_COPY_SLOT_BYTES`,
`ZERO_COPY_SLOT_COUNT`) may need adjustment.

### 4.6 Metric collection harness

```bash
#!/bin/bash
# Wrapper that collects all five metrics per iteration.
# Called by hyperfine --prepare and --command.
METRICS_DIR="${SZC_METRICS_DIR:-/tmp/szc-bench/metrics}"
mkdir -p "$METRICS_DIR"
RUN_ID="$1"  # e.g. "scenario1-sendzc-run3"

/usr/bin/time -v -o "$METRICS_DIR/${RUN_ID}.time" \
  "$@" 2>"$METRICS_DIR/${RUN_ID}.stderr"
```

The JSON output from hyperfine and the `/usr/bin/time -v` output are
both uploaded as CI artifacts for post-hoc analysis.

## 5. Comparison axes

### 5.1 Feature toggle

Each scenario runs twice:

| Label | Build | Runtime |
|-------|-------|---------|
| `send-plain` | `cargo build --release` (default features) | `IORING_OP_SEND` path |
| `send-zc` | `cargo build --release --features iouring-send-zc` | `IORING_OP_SEND_ZC` path via `--zero-copy` CLI flag |

The `send-plain` build uses the default `io_uring` feature (default on
for Linux) which routes socket sends through `IORING_OP_SEND`. The
`send-zc` build adds `iouring-send-zc` and passes `--zero-copy` to
the daemon client so `ZeroCopyPolicy::Enabled` activates the SEND_ZC
dispatch in `IoUringSocketWriter::submit_send`.

### 5.2 Kernel matrix

The bench targets four kernel versions that span the SEND_ZC
lifecycle. Each cell runs all three scenarios.

| Kernel | Distro | SEND_ZC | Why |
|--------|--------|---------|-----|
| 5.19 | (reference only) | Partial | Probe should reject; validates fallback. Not a gating row. |
| 6.0 | Debian 12 | Yes (first stable) | Registered-buffer pool unavailable (lands 6.2). Tests the unregistered SEND_ZC path. |
| 6.1 | Amazon Linux 2023 | Yes | First mainstream LTS with SEND_ZC. Registered buffers available. |
| 6.6 LTS | Ubuntu 24.04 | Yes (stable) | The realistic 2-3 year deployment target. Full registered-buffer + SEND_ZC maturity. |

The primary gating rows are 6.1 and 6.6 LTS. Kernel 6.0 is a
secondary row that confirms the unregistered path is not a regression
vs plain SEND.

### 5.3 Cross-matrix summary

The full matrix is:

```
3 scenarios x 2 feature toggles x 4 kernels = 24 cells
```

Each cell produces 5 metrics. The total data surface is 120 data
points per full run.

## 6. Pass/fail criteria

### 6.1 Promotion gate (all must pass for IUS-4 reopen)

| Criterion | Threshold | Scope |
|-----------|-----------|-------|
| Throughput improvement | send-zc wall-clock MB/s >= 1.05x send-plain | Scenario 1 (10 GiB) on kernels >= 6.1 |
| No high-IOPS regression | send-zc wall-clock <= 1.03x send-plain | Scenario 2 (100K files) on kernels >= 6.1 |
| CPU reduction (sustained) | send-zc sys-CPU <= 0.85x send-plain | Scenario 1 on kernels >= 6.1 |
| CPU reduction (concurrent) | send-zc total-CPU <= 0.90x send-plain | Scenario 3 (50 clients) on kernels >= 6.1 |
| RSS stability | send-zc peak RSS <= 1.10x send-plain | All scenarios on kernels >= 6.1 |

### 6.2 Interpretation matrix

| Throughput | CPU | RSS | Decision |
|------------|-----|-----|----------|
| Pass | Pass | Pass | **Promote to default-on.** Reopen IUS-4 with numbers. |
| Pass | Pass | Fail | Investigate registered-buffer pool sizing; reduce `ZERO_COPY_SLOT_COUNT`. Retest. |
| Pass | Fail | Pass | Anomalous. Profile for hidden overhead. Do not promote without explanation. |
| Fail (Scenario 1) | Pass | Pass | SEND_ZC is CPU-only benefit. Keep opt-in with documentation noting CPU savings. |
| Any | Any regression on Scenario 2 > 5% | Any | Raise `SEND_ZC_MIN_BYTES` threshold. Retest. |
| Fail | Fail | Any | **Keep opt-in.** File evidence in IUS-4 reopen doc. |

### 6.3 Statistical significance

Each hyperfine run produces a mean and standard deviation. A result
is considered significant only when the difference between send-zc
and send-plain exceeds 2x the pooled standard deviation of the two
measurements. Results within the noise floor are reported as "no
significant difference" regardless of the point estimate.

## 7. CI integration

### 7.1 Workflow placement

The bench runs as a **non-required, nightly-only** GitHub Actions
workflow at `.github/workflows/bench-send-zc-production.yml`. It does
not trigger on pull requests because the full matrix takes 30-60
minutes and requires fixture generation that exceeds the free-tier
runner disk budget on the 100K-file scenario.

```yaml
name: Bench SEND_ZC production-scale
on:
  workflow_dispatch:
  schedule:
    - cron: '47 5 * * *'  # 05:47 UTC, offset from other bench crons
```

### 7.2 Runner requirements

- **ubuntu-latest** (x86_64) with kernel >= 6.1. The `uname -r`
  output is logged at step start; the workflow skips with a green
  status if the kernel is below 6.0 (SEND_ZC unsupported).
- **Disk**: Scenario 1 needs ~11 GiB for the fixture + transfer
  destination. The github-hosted runner provides 14 GiB free on the
  SSD partition. Scenario 2 needs ~1 GiB. Scenario 3 needs ~100 MiB.
  The workflow runs scenarios sequentially and cleans up between them.
- **Memory**: Scenario 3 with 50 concurrent daemon threads needs
  ~2 GiB RSS. The runner provides 7 GiB.

### 7.3 Artifact upload

Each run uploads:

- `hyperfine-results.json` per scenario per feature toggle.
- `/usr/bin/time -v` output per iteration.
- A summary markdown table rendered to `$GITHUB_STEP_SUMMARY`.

Artifacts are retained for 30 days (Actions default). The numbers are
not committed to the repository; they live as CI artifacts until a
human reviews them for the IUS-4 reopen.

### 7.4 Relationship to existing bench infrastructure

| Workflow | Scope | Trigger | Required |
|----------|-------|---------|----------|
| `benchmark.yml` | Release perf regression | Tag push | No |
| `bench-daemon-coldstart.yml` | Daemon cold-start | Nightly + PR paths | No (DIS-8.b tracks promotion) |
| `bench-daemon-concurrency.yml` | Daemon thread ceiling | Nightly + PR paths | No |
| `bench-send-zc-production.yml` (this) | SEND_ZC production-scale | Nightly + manual | No |

This workflow is independent of the existing bench infrastructure.
It does not share fixtures, artifacts, or pass/fail gates with any
other workflow. If the IUS-4 reopen decision promotes SEND_ZC to
default-on, the `benchmark.yml` release workflow should add a
SEND_ZC cell as a regression guard; that change is out of scope for
this spec.

## 8. Risk factors

### 8.1 Buffer pinning pressure

`IORING_OP_SEND_ZC` pins user pages via `get_user_pages_fast`. The
pinned pages count against `RLIMIT_MEMLOCK` (default 64 KiB on most
distros, raised to 8 MiB by systemd for user sessions). On the
registered-buffer path, the `ZeroCopySender` at
`crates/fast_io/src/io_uring/send_zc.rs` pre-registers
`ZERO_COPY_SLOT_COUNT` (8) x `ZERO_COPY_SLOT_BYTES` (256 KiB) = 2 MiB
of pinned memory per sender. With 50 concurrent clients (Scenario 3)
that is 100 MiB of pinned memory - well within the 8 MiB systemd
default if the daemon runs as a systemd service with `LimitMEMLOCK=`.
Operators running the daemon outside systemd may need to raise
`ulimit -l`. The bench harness sets `ulimit -l unlimited` before
starting the daemon to avoid `ENOMEM` from `io_uring_register`.

### 8.2 Notification storms at high send rate

Each SEND_ZC submission generates an extra notification CQE. On
Scenario 2 (100K files), if each file payload is sent as a single
SEND_ZC submission, the CQ sees 200K CQEs (100K transfer + 100K
notification) vs 100K for plain SEND. The CQ ring default size is
128 entries (2x the 64-entry SQ). If submissions outpace CQ drains,
the ring overflows and the kernel drops CQEs.

Mitigations already in code:

- `try_send_zc` at `crates/fast_io/src/io_uring/send_zc.rs:130`
  blocks on both CQEs before returning, so at most one SEND_ZC
  submission is in-flight per writer at any time. CQ overflow cannot
  occur in this synchronous model.
- The per-thread ring topology (IUR-3.a) means concurrent clients
  do not share a CQ ring; each thread's ring handles only its own
  submissions.

Residual risk: if a future optimization batches multiple SEND_ZC
submissions before draining, the CQ must be sized to 2x the batch
depth. The bench should verify CQ overflow does not occur by checking
`io_uring_cq_overflow` in `/proc/<pid>/fdinfo/<ring_fd>` after each
iteration.

### 8.3 Kernel version floor sensitivity

SEND_ZC behavior differs across kernel versions:

- **6.0**: SEND_ZC opcode present; registered-buffer SEND_ZC is not.
  The unregistered path works but does not benefit from pinned-page
  registration; every send does a fresh `get_user_pages_fast`. CPU
  overhead may be higher than on 6.2+.
- **6.2**: `IORING_RECVSEND_FIXED_BUF` flag added. The registered-
  buffer fast path in `ZeroCopySender::send_zc` is fully usable.
- **6.5-6.6**: Various SEND_ZC bugfixes (completion-ordering race in
  6.3, page-pin refcount fix in 6.5). The 6.6 LTS kernel is the first
  version where SEND_ZC is considered stable for production.

The bench must record the exact kernel version (`uname -r`) and
annotate results with whether the registered-buffer path was active
(`ZeroCopySender::registered_buffers_active()`). Results from 6.0
where registration silently fails should be reported separately from
6.6+ results where registration succeeds.

### 8.4 Loopback vs real NIC

All scenarios run on loopback. Loopback eliminates NIC DMA latency,
switch buffering, and TCP congestion - all factors that amplify
SEND_ZC's benefit on real networks. The loopback results are a
conservative lower bound: if SEND_ZC wins on loopback, it will win
by more on a real NIC. Conversely, if it loses on loopback, the
real-NIC case is uncertain and would require a separate bench.

A real-NIC bench between two hosts is out of scope for this spec.
If the loopback results are inconclusive (within noise), a follow-up
spec should design a two-host bench with `iperf3` calibration.

### 8.5 Shared-runner CPU variance

GitHub-hosted runners have documented CPU variability (2-10% on
identical workloads across runs). The bench mitigates this by:

- Running each scenario with `hyperfine --warmup 1 --runs N` where
  N >= 5 for Scenarios 1-2 and N >= 3 for Scenario 3.
- Requiring that the delta exceeds 2x the pooled standard deviation
  (Section 6.3).
- Logging `/proc/cpuinfo` model name and `lscpu` output so results
  from different runner generations can be separated.

### 8.6 Fixture disk space

Scenario 1's 10 GiB fixture requires ~11 GiB of free disk (fixture +
headroom). The github-hosted runner's SSD partition provides ~14 GiB
free. If the runner image grows and squeezes free space below 11 GiB,
the fixture generation will fail. The bench script checks available
disk with `df` before generating and skips Scenario 1 with a warning
if space is insufficient.

## 9. Daemon configuration

The bench harness starts `oc-rsync --daemon` with a minimal
`oc-rsyncd.conf`:

```ini
port = 0
# Port 0 lets the OS assign a free port; the harness reads it from
# the daemon's startup log line.
log file = /tmp/szc-bench/rsyncd.log

[bench]
    path = $SZC_FIXTURE_DIR
    read only = true
    use chroot = false
```

For the `send-zc` variant, the daemon is started with:

```bash
oc-rsync --daemon --zero-copy --config=/tmp/szc-bench/rsyncd.conf
```

For the `send-plain` variant, the `--zero-copy` flag is omitted.

## 10. Execution order

The bench script runs scenarios sequentially to avoid disk and CPU
contention:

1. Generate all fixtures (once, cached).
2. Scenario 1 (`send-plain` then `send-zc`).
3. Clean Scenario 1 destination.
4. Scenario 2 (`send-plain` then `send-zc`).
5. Clean Scenario 2 destination.
6. Scenario 3 (`send-plain` then `send-zc`).
7. Aggregate results into summary table.
8. Upload artifacts.

Each scenario pair (send-plain + send-zc) is interleaved by iteration
within hyperfine using `--command-name` labels, not run as two
separate hyperfine invocations. This ensures the two variants
experience identical runner conditions per iteration.

## 11. Out of scope (deliberate)

- **Delta transfers.** Scenarios use `--whole-file` to isolate the
  send path from the delta engine. A delta-transfer bench would
  conflate SEND_ZC savings with basis-read and rolling-checksum
  costs. Delta-transfer SEND_ZC evaluation is a follow-up if the
  whole-file results justify it.

- **Compression.** Scenarios use `--no-compress`. Compression moves
  the bottleneck from socket send to CPU codec; SEND_ZC savings would
  be buried in compression noise.

- **SSH transport.** SEND_ZC is socket-only (`IORING_OP_SEND_ZC`
  requires a socket fd). The SSH transport uses pipe I/O via
  `ChildStdin` and is out of scope.

- **Registered-buffer vs unregistered SEND_ZC split.** Both paths
  are zero-copy at the socket layer. The difference is per-page
  pinning cost. A split bench is a follow-up if the aggregate
  results show the registered path winning by a significant margin
  on 6.6+ vs losing on 6.0.

- **`tc qdisc` bandwidth shaping.** Loopback-only in this spec. A
  shaped-loopback follow-up (`netem rate 1gbit`, `netem rate 10gbit`)
  would surface the "slow NIC" regime where SEND_ZC saves CPU
  without improving wall time.

- **Two-host real-NIC bench.** Requires dedicated hardware. Follow-up
  spec if loopback results are inconclusive.

## 12. Relationship to IUS-3

IUS-3 (primitive-isolated bench) and SZC.a (production-scale bench)
are complementary, not competing:

| Dimension | IUS-3 | SZC.a |
|-----------|-------|-------|
| Isolation level | Raw `try_send_zc` vs `opcode::Send` | Full daemon transfer end-to-end |
| Filesystem I/O | None | Real file reads (fixture on disk) |
| Protocol framing | None | Full `MSG_DATA` multiplex framing |
| Concurrency | Single sender | Up to 50 concurrent clients |
| Total data | 100 MiB max | 10 GiB max |
| Decision scope | Primitive viability | Production promotion |

IUS-3 answers "is the primitive faster?" SZC.a answers "does the
primitive's speed advantage survive the production transfer pipeline?"
Both are needed to justify the IUS-4 reopen.

## 13. References

- IUS-3 bench design: `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`
- IUS-3 bench harness: `crates/fast_io/benches/ius_3_send_zc_vs_send.rs`
- IUS-4 decision (keep opt-in): `docs/design/ius-4-decision-2026-05-22.md`
- IUS-4 framing: `docs/design/ius-4-decision-framing-2026-05-21.md`
- SEND_ZC design: `docs/design/iouring-send-zc.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Socket writer dispatch: `crates/fast_io/src/io_uring/socket_writer.rs`
- Feature flag: `crates/fast_io/Cargo.toml` (`iouring-send-zc`)
- Kernel compat audit: `docs/audits/ius-2-send-zc-kernel-compat-matrix.md`
- DIS-8.a bench template: `.github/workflows/bench-daemon-coldstart.yml`
