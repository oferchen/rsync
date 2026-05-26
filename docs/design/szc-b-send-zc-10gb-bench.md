# SZC.b - SEND_ZC vs plain SEND 10 GiB single-file bench spec

Date: 2026-05-26
Scope: first throughput bench in the SZC series - measures sustained
send performance on a single 10 GiB file over loopback daemon transfer.
Status: design spec; implementation is a follow-up PR.
Predecessors:
- SZC.a (PR #5028): production-scale bench workload design - defines
  Scenario 1 (10 GiB single file), fixture generation, daemon config,
  metric collection, and pass/fail criteria. This spec is the
  implementation-level detail for Scenario 1.
- SZC.e (PR #5036): per-kernel correctness validation across 5.16-6.6.
  Confirms the SEND_ZC code path is functionally correct before we
  bench it.
Successors:
- SZC.c: 100K-file high-IOPS bench (SZC.a Scenario 2).
- SZC.d: 50-client concurrent daemon bench (SZC.a Scenario 3).
- SZC.f: promotion decision - consumes SZC.b/c/d numbers to revisit
  the IUS-4 keep-opt-in gate.

## 1. Motivation

SZC.a designed three production-scale bench scenarios to evaluate
whether `IORING_OP_SEND_ZC` merits promotion from opt-in to default.
SZC.e validated correctness. SZC.b implements the first and most
important scenario: a single 10 GiB file transferred over a loopback
daemon pull.

This scenario isolates the send primitive from per-file overhead. The
entire wall-clock delta between SEND_ZC and plain SEND is attributable
to the kernel's page-copy avoidance on the socket send path. A 10 GiB
transfer moves enough data through the CPU cache that the memcpy
elimination should be measurable - SZC.a predicts 5-15% throughput
improvement on kernels >= 6.1.

If SEND_ZC cannot demonstrate a win at this scale, it has no path to
default-on for bulk transfers.

## 2. Bench arms

### 2.1 Arm definitions

| Arm | Build command | Runtime flag | Send opcode |
|-----|---------------|--------------|-------------|
| `send-plain` | `cargo build --release` | (none) | `IORING_OP_SEND` |
| `send-zc` | `cargo build --release --features iouring-send-zc` | `--zero-copy` | `IORING_OP_SEND_ZC` |

Both arms use the default `io_uring` feature (enabled by default on
Linux). The only difference is whether the socket writer dispatches
through the zero-copy path in `IoUringSocketWriter::submit_send`
(at `crates/fast_io/src/io_uring/socket_writer.rs`).

### 2.2 Binary placement

The bench script builds both binaries before any measurement begins
and places them in distinct directories to avoid rebuilds between
iterations:

```
/tmp/szc-bench/bin/
  send-plain/oc-rsync     # default features
  send-zc/oc-rsync        # --features iouring-send-zc
```

Building both binaries up front eliminates cargo-lock contention and
ensures hyperfine iterations do not include compile time.

## 3. Bench methodology

### 3.1 Transfer shape

A single daemon pull of one 10 GiB file over TCP loopback:

```
rsync://localhost:<port>/bench/large.bin /tmp/szc-bench/dest/
```

Flags: `--no-compress --whole-file`

- `--no-compress` isolates the send path from codec CPU.
- `--whole-file` bypasses delta computation and basis reads, ensuring
  the wall-clock is dominated by file read + socket send.

### 3.2 Daemon lifecycle

The daemon is restarted between each hyperfine iteration to capture
cold-start overhead and prevent warm-cache artifacts from biasing
later iterations. The daemon config uses `port = 0` (OS-assigned)
to avoid port conflicts:

```ini
port = 0
log file = /tmp/szc-bench/rsyncd.log

[bench]
    path = /tmp/szc-bench/fixtures/large
    read only = true
    use chroot = false
```

The bench harness starts the daemon, reads the assigned port from
the startup log, runs the client transfer, then kills the daemon.
This start-transfer-kill cycle is wrapped in a single script that
hyperfine invokes.

### 3.3 Hyperfine invocation

```bash
hyperfine \
  --warmup 1 \
  --runs 5 \
  --export-json /tmp/szc-bench/results/scenario1.json \
  --command-name send-plain \
    '/tmp/szc-bench/bin/run-bench.sh send-plain' \
  --command-name send-zc \
    '/tmp/szc-bench/bin/run-bench.sh send-zc'
```

Both arms are interleaved within the same hyperfine session so they
experience identical runner conditions per iteration. Five runs per
arm provides enough samples to compute a reliable mean and standard
deviation on a CI runner.

The `--warmup 1` run primes page-cache state for the fixture file.
Subsequent runs measure the steady-state send throughput.

### 3.4 Iteration wrapper script

`run-bench.sh` performs the per-iteration lifecycle:

1. Start daemon with the specified binary.
2. Wait for the port log line (timeout 5 seconds).
3. Run the client transfer under `/usr/bin/time -v`.
4. Kill the daemon (SIGTERM, wait, SIGKILL fallback).
5. Clean the destination directory.

The `/usr/bin/time -v` output is saved per-iteration for CPU and RSS
extraction.

## 4. Metrics

### 4.1 Primary metrics (go/no-go)

| Metric | Source | Computation |
|--------|--------|-------------|
| Wall-clock throughput (MB/s) | `hyperfine --export-json` | `10737418240 / mean_seconds / 1048576` |
| Sys-CPU time (seconds) | `/usr/bin/time -v` `System time` | Direct parse |

Wall-clock throughput is the operator-facing signal - faster transfers
justify the feature. Sys-CPU captures the documented SEND_ZC benefit
(memcpy elimination) even when wall-clock is bounded by other factors.

### 4.2 Diagnostic metrics

| Metric | Source | Purpose |
|--------|--------|---------|
| User-CPU time (seconds) | `/usr/bin/time -v` `User time` | Detect overhead shifts between arms |
| Context switches (vol + invol) | `/usr/bin/time -v` | SEND_ZC drains 2 CQEs per submission vs 1; extra switches indicate CQ pressure |
| Peak RSS (KiB) | `/usr/bin/time -v` `Maximum resident set size` | SEND_ZC pins user pages via `get_user_pages_fast`; RSS growth indicates buffer sizing issues |
| `perf stat` counters | `perf stat -e cycles,instructions,cache-misses,context-switches` | CPU-cycle attribution; optional - only collected when `perf` is available on the runner |

### 4.3 Validation metric

The bench harness logs whether the SEND_ZC path was actually
exercised by checking the `ZeroCopySender::sends_completed` counter
(exposed via `--stats` output). If the send-zc arm shows zero
zero-copy sends, the bench is invalid - the feature gate or runtime
probe silently fell back to plain SEND.

## 5. Fixture generation

A single 10 GiB file with deterministic pseudo-random content,
generated once and cached across iterations:

```bash
FIXTURE_DIR="/tmp/szc-bench/fixtures/large"
mkdir -p "$FIXTURE_DIR"

if [ ! -f "$FIXTURE_DIR/large.bin" ] || \
   [ "$(stat -c%s "$FIXTURE_DIR/large.bin" 2>/dev/null)" != "10737418240" ]; then
  openssl enc -aes-256-ctr \
    -K "$(printf '0%.0s' {1..64})" \
    -iv "$(printf '0%.0s' {1..32})" \
    -nosalt < /dev/zero 2>/dev/null | \
    head -c 10737418240 > "$FIXTURE_DIR/large.bin"
fi
```

The AES-256-CTR stream with a fixed key/IV produces byte-identical
output across runs, ensuring cross-run comparability. The pseudo-random
content defeats filesystem compression and deduplication.

On CI, the fixture directory is an Actions cache artifact keyed on the
generation script hash. Fixture generation takes ~30 seconds on a
modern NVMe drive.

## 6. Hardware and kernel requirements

### 6.1 Storage

NVMe SSD required. The 10 GiB file read must not be the bottleneck -
a SATA drive at ~500 MB/s would cap throughput below the loopback TCP
ceiling (~6-8 GB/s), masking the SEND vs SEND_ZC delta. NVMe drives
sustain 2-3 GB/s sequential reads, which is well above the expected
loopback TCP throughput for a single connection.

GitHub-hosted runners use NVMe storage by default.

### 6.2 Network

Loopback only. The bench measures the send primitive's CPU efficiency,
not network throughput. Loopback eliminates NIC DMA latency, switch
buffering, and TCP congestion. This makes the results a conservative
lower bound - if SEND_ZC wins on loopback, it wins by more on a real
NIC where DMA scatter-gather benefits compound.

### 6.3 Kernel version

| Minimum | Recommended | Reason |
|---------|-------------|--------|
| 6.0 | 6.6 LTS | SEND_ZC opcode present at 6.0; registered-buffer support at 6.2; stability fixes through 6.5; 6.6 is the first LTS where SEND_ZC is considered production-ready |

The bench workflow probes the running kernel at startup:

```bash
KERNEL_VERSION=$(uname -r | cut -d. -f1-2)
KERNEL_MAJOR=$(echo "$KERNEL_VERSION" | cut -d. -f1)
KERNEL_MINOR=$(echo "$KERNEL_VERSION" | cut -d. -f2)

if [ "$KERNEL_MAJOR" -lt 6 ]; then
  echo "::warning::Kernel $KERNEL_VERSION < 6.0; SEND_ZC unsupported. Skipping."
  exit 0
fi
```

On kernels 6.0-6.1, the bench runs but results are annotated as
"unregistered SEND_ZC" (the registered-buffer path requires >= 6.2).
On kernels < 6.0, the workflow exits with a green status - SEND_ZC
cannot be tested.

### 6.4 Fallback behavior on older kernels

When `IORING_OP_SEND_ZC` is not available (kernel < 6.0 or probe
returns unsupported), the `iouring-send-zc` feature-gated code path
falls back to `IORING_OP_SEND` transparently. The bench detects this
via the validation metric (Section 4.3) and marks the run as invalid
rather than silently comparing plain SEND against plain SEND.

### 6.5 Disk space

The fixture requires ~11 GiB (10 GiB file + destination headroom).
GitHub-hosted runners provide ~14 GiB free on the SSD partition. The
bench script checks available space before fixture generation:

```bash
AVAIL_GB=$(df --output=avail /tmp | tail -1 | awk '{print int($1/1048576)}')
if [ "$AVAIL_GB" -lt 12 ]; then
  echo "::error::Insufficient disk space (${AVAIL_GB} GiB < 12 GiB). Skipping."
  exit 1
fi
```

### 6.6 Memory

Peak RSS for a daemon serving a single 10 GiB transfer is ~200 MiB
(buffer pool + file-list metadata + protocol buffers). The SEND_ZC arm
may add ~2 MiB for registered-buffer pinning (8 slots x 256 KiB). The
runner provides 7 GiB - no memory constraint.

## 7. Pass/fail criteria

These thresholds are derived from SZC.a Section 6.1, narrowed to
Scenario 1 scope.

### 7.1 Go/no-go gates

| Criterion | Threshold | Notes |
|-----------|-----------|-------|
| Throughput | `send-zc` wall-clock MB/s >= 1.05x `send-plain` | 5% improvement is the minimum to justify feature complexity |
| Sys-CPU reduction | `send-zc` sys-CPU <= 0.85x `send-plain` | 15% sys-CPU reduction validates the memcpy-elimination claim |
| RSS stability | `send-zc` peak RSS <= 1.10x `send-plain` | No more than 10% RSS growth from page pinning |

### 7.2 Statistical significance

A result is significant only when the mean difference between arms
exceeds 2x the pooled standard deviation. Results within the noise
floor are reported as "no significant difference" regardless of point
estimates. With 5 runs per arm, this threshold filters out CI runner
CPU variance (typically 2-10%).

### 7.3 Interpretation matrix

| Throughput | Sys-CPU | RSS | Outcome |
|------------|---------|-----|---------|
| Pass | Pass | Pass | Proceed to SZC.c (high-IOPS). Strong evidence for default-on. |
| Pass | Pass | Fail | Investigate `ZERO_COPY_SLOT_COUNT`/`ZERO_COPY_SLOT_BYTES` sizing. Retest. |
| Pass | Fail | Pass | Profile for hidden overhead. Do not proceed without explanation. |
| Fail | Pass | Pass | CPU-only benefit. Document. Proceed to SZC.c to see if the pattern holds. |
| Fail | Fail | Any | Keep opt-in. File evidence in SZC.f decision doc. |

## 8. CI integration

### 8.1 Workflow placement

Non-required, on-demand + nightly workflow at
`.github/workflows/bench-send-zc-10gb.yml`. Not triggered on pull
requests - the 10 GiB fixture and 5-run hyperfine session take
15-25 minutes and exceed the free-tier runner disk budget on some
runner images.

```yaml
name: Bench SEND_ZC 10GiB
on:
  workflow_dispatch:
  schedule:
    - cron: '17 4 * * *'  # 04:17 UTC, offset from other bench crons
```

### 8.2 Workflow structure

```yaml
jobs:
  bench-10gb:
    runs-on: ubuntu-latest
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@v4

      - name: Check kernel version
        id: kernel
        run: |
          KVER=$(uname -r | cut -d. -f1-2)
          echo "version=$KVER" >> "$GITHUB_OUTPUT"
          MAJOR=$(echo "$KVER" | cut -d. -f1)
          if [ "$MAJOR" -lt 6 ]; then
            echo "::warning::Kernel $KVER < 6.0; skipping."
            echo "skip=true" >> "$GITHUB_OUTPUT"
          fi

      - name: Check disk space
        if: steps.kernel.outputs.skip != 'true'
        run: |
          AVAIL_GB=$(df --output=avail /tmp | tail -1 | awk '{print int($1/1048576)}')
          if [ "$AVAIL_GB" -lt 12 ]; then
            echo "::error::Need 12 GiB, have ${AVAIL_GB} GiB"
            exit 1
          fi

      - name: Install toolchain
        if: steps.kernel.outputs.skip != 'true'
        uses: dtolnay/rust-toolchain@stable

      - name: Install hyperfine
        if: steps.kernel.outputs.skip != 'true'
        run: |
          wget -q https://github.com/sharkdp/hyperfine/releases/download/v1.18.0/hyperfine_1.18.0_amd64.deb
          sudo dpkg -i hyperfine_1.18.0_amd64.deb

      - name: Build both arms
        if: steps.kernel.outputs.skip != 'true'
        run: |
          mkdir -p /tmp/szc-bench/bin/send-plain /tmp/szc-bench/bin/send-zc
          cargo build --release
          cp target/release/oc-rsync /tmp/szc-bench/bin/send-plain/
          cargo build --release --features iouring-send-zc
          cp target/release/oc-rsync /tmp/szc-bench/bin/send-zc/

      - name: Generate 10GiB fixture
        if: steps.kernel.outputs.skip != 'true'
        run: scripts/szc-b-generate-fixture.sh

      - name: Run bench
        if: steps.kernel.outputs.skip != 'true'
        run: scripts/szc-b-run-bench.sh

      - name: Render summary
        if: steps.kernel.outputs.skip != 'true'
        run: scripts/szc-b-render-summary.sh >> "$GITHUB_STEP_SUMMARY"

      - name: Upload artifacts
        if: steps.kernel.outputs.skip != 'true'
        uses: actions/upload-artifact@v4
        with:
          name: szc-b-results-${{ github.run_id }}
          path: /tmp/szc-bench/results/
          retention-days: 30
```

### 8.3 Runner hardware logging

The first step logs `uname -r`, `/proc/cpuinfo` model name, `lscpu`
output, and `df` free space. Results from different runner hardware
generations can be separated during post-hoc analysis.

### 8.4 Relationship to other bench workflows

| Workflow | Scenario | Overlap with SZC.b |
|----------|----------|--------------------|
| `benchmark.yml` | Release regression | None - measures oc-rsync vs upstream rsync, not SEND_ZC arms |
| `bench-daemon-coldstart.yml` | Daemon cold-start (DIS-8) | Shares daemon lifecycle pattern but different fixture and metric set |
| `bench-daemon-concurrency.yml` | Thread ceiling | None - SZC.b is single-client |
| SZC.c (future) | 100K-file high-IOPS | Same two arms, different fixture (small files vs one large file) |
| SZC.d (future) | 50-client concurrent | Same two arms, different concurrency level |

SZC.b is independent. No shared fixtures, artifacts, or gates with
existing workflows.

## 9. Bench script design

### 9.1 `scripts/szc-b-generate-fixture.sh`

Generates the 10 GiB file at `/tmp/szc-bench/fixtures/large/large.bin`
if not already present and correctly sized. Uses the AES-256-CTR
approach from SZC.a Section 3.1.

### 9.2 `scripts/szc-b-run-bench.sh`

Orchestrates the hyperfine run:

1. Write the `oc-rsyncd.conf` to `/tmp/szc-bench/rsyncd.conf`.
2. Create `run-iteration.sh` - the per-iteration wrapper that starts
   the daemon, reads the port, runs the transfer under
   `/usr/bin/time -v`, and cleans up.
3. Invoke hyperfine with both `send-plain` and `send-zc` command
   names, interleaved.
4. Collect all `/usr/bin/time -v` outputs into
   `/tmp/szc-bench/results/`.

### 9.3 `scripts/szc-b-render-summary.sh`

Parses `hyperfine --export-json` output and `/usr/bin/time -v` files
to produce a markdown summary table:

```
## SZC.b Results: SEND_ZC vs plain SEND (10 GiB single file)

| Metric | send-plain | send-zc | Delta | Significant? |
|--------|------------|---------|-------|--------------|
| Wall-clock (s) | 4.21 +/- 0.12 | 3.89 +/- 0.09 | -7.6% | Yes |
| Throughput (MB/s) | 2430 | 2628 | +8.1% | Yes |
| Sys-CPU (s) | 1.82 | 1.41 | -22.5% | Yes |
| User-CPU (s) | 0.93 | 0.91 | -2.2% | No |
| Context switches | 4201 | 4387 | +4.4% | No |
| Peak RSS (KiB) | 201344 | 203520 | +1.1% | No |

Kernel: 6.8.0-40-generic
Runner: AMD EPYC 7763 64-Core
```

The "Significant?" column applies the 2x pooled standard deviation
threshold from Section 7.2.

## 10. Risk factors

### 10.1 Fixture disk pressure on CI runners

The 10 GiB fixture + ~10 GiB destination headroom requires ~20 GiB
during the transfer phase (source + destination). The bench cleans the
destination between iterations via `rm -rf /tmp/szc-bench/dest/*`, so
steady-state disk usage is ~11 GiB. If the runner's free space drops
below 12 GiB (due to GitHub image bloat), the bench fails at the
disk-check step with a clear error rather than mid-transfer.

### 10.2 CI runner CPU variance

GitHub-hosted runners exhibit 2-10% CPU variance across runs on
identical workloads. The 5-run sample with 2x pooled standard deviation
threshold (Section 7.2) filters most of this noise, but borderline
results (e.g., 5.1% improvement with 4% standard deviation) will be
reported as "no significant difference."

For definitive numbers, the bench should also run on dedicated hardware
(the `localhost/oc-rsync-bench:latest` container on a bare-metal host).
The CI workflow is a screening pass; final promotion evidence comes
from controlled-environment runs.

### 10.3 Page-cache warming

The first transfer reads the 10 GiB fixture from disk into page cache.
Subsequent iterations read from page cache - effectively measuring
"send from hot cache" rather than "send from cold disk." This is the
intended measurement: the bench isolates the send primitive, not the
disk read path. The `--warmup 1` run ensures the first measured
iteration also reads from page cache.

If cold-disk behavior matters for the promotion decision, a follow-up
bench should use `echo 3 > /proc/sys/vm/drop_caches` between
iterations. That is out of scope here.

### 10.4 Loopback TCP ceiling

Loopback TCP on Linux typically saturates at 6-8 GB/s for a single
connection (kernel version and socket buffer sizes affect the exact
ceiling). If both arms hit this ceiling, the wall-clock delta is zero
and only the CPU metrics differentiate the arms. This is a valid
outcome - it means SEND_ZC does not improve throughput on a
CPU-unconstrained host, but may still save CPU on a busy server. The
interpretation matrix (Section 7.3) accounts for this case.

### 10.5 `RLIMIT_MEMLOCK` on CI runners

SEND_ZC pins user pages via `get_user_pages_fast`. The pinned pages
count against `RLIMIT_MEMLOCK`. On GitHub runners this limit is
typically 64 KiB or "unlimited" depending on the systemd slice. The
bench harness sets `ulimit -l unlimited` before starting the daemon.
If the runner restricts this, the registered-buffer path silently
falls back to unregistered SEND_ZC (still zero-copy, but with
per-send page pinning overhead).

## 11. Relationship to SZC series

```
SZC.a (design)        Workload design for all three scenarios
  |
  +-- SZC.b (this)    Scenario 1: 10 GiB single-file throughput bench
  +-- SZC.c           Scenario 2: 100K-file high-IOPS bench
  +-- SZC.d           Scenario 3: 50-client concurrent daemon bench
  |
SZC.e (correctness)   Per-kernel correctness validation
  |
  +-- SZC.f           Promotion decision - consumes b/c/d numbers
```

SZC.b is the first data point. It answers: "does SEND_ZC improve
sustained single-file throughput?" SZC.c then asks: "does it regress
on high-IOPS workloads?" SZC.d asks: "do CPU savings compound across
concurrent clients?" SZC.f synthesizes all three into a promote or
keep-opt-in decision.

If SZC.b shows no throughput improvement and no CPU improvement, the
remaining scenarios are unlikely to change the outcome. SZC.f may
shortcut to "keep opt-in" without running SZC.c/d. However, a
CPU-only win on SZC.b (throughput flat, sys-CPU down) still warrants
SZC.d to see if the CPU savings compound at concurrency.

## 12. Out of scope

- **Delta transfers.** `--whole-file` isolates the send path. Delta
  bench is a follow-up if whole-file results justify it.
- **Compression.** `--no-compress` avoids conflating codec CPU with
  send CPU.
- **SSH transport.** SEND_ZC requires a socket fd; SSH uses pipe I/O.
- **Multi-kernel matrix.** SZC.b runs on whichever kernel the CI
  runner provides (expected >= 6.2). Multi-kernel comparison is
  SZC.e's scope.
- **Real-NIC bench.** Loopback only. Two-host bench requires
  dedicated hardware.
- **Registered vs unregistered SEND_ZC split.** Both are zero-copy
  at the socket layer. Split bench is a follow-up if results diverge
  by kernel version.

## 13. References

- SZC.a bench workload design: `docs/design/szc-a-send-zc-bench-workload.md`
- SZC.e kernel correctness: `docs/design/szc-e-send-zc-kernel-correctness.md`
- IUS-3 primitive bench: `docs/design/ius-3-send-zc-bench-design-2026-05-21.md`
- IUS-4 decision (keep opt-in): `docs/design/ius-4-decision-2026-05-22.md`
- SEND_ZC design: `docs/design/iouring-send-zc.md`
- SEND_ZC implementation: `crates/fast_io/src/io_uring/send_zc.rs`
- Socket writer dispatch: `crates/fast_io/src/io_uring/socket_writer.rs`
- Feature flag: `crates/fast_io/Cargo.toml` (`iouring-send-zc`)
