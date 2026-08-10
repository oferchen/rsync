# --delete-before latency bench design (DML-1)

Status: Design (task DML-1, #3306)
Audience: engine, CI, and performance maintainers
Scope: a benchmark measuring wall-clock latency from --delete-before start
to first file transfer at file scales 10 through 100K, comparing oc-rsync
against upstream rsync 3.4.1.

Depends on: DML-6 architecture doc (merged, PR #5243).
Feeds into: DML-3 (fast-path design, complete), DML-4 (fast-path
implementation, design merged PR #5249).

Out of scope: transfer throughput, --delete-during timing, parallel-delete
pipeline throughput at scale (covered by DEL-4.a/b/c benches).

## 1. Motivation

The oc-rsync delete module uses a cohort/reorder buffer pipeline that adds
constant-factor overhead vs upstream rsync's inline `delete_in_dir` loop.
For --delete-before, the entire delete phase completes before any file
transfer begins - making its latency directly visible to users as
time-to-first-byte.

No existing benchmark isolates delete-phase latency from transfer
throughput. The criterion benches (`delete_end_to_end.rs`,
`delete_emitter_unlink.rs`) measure unlink throughput in isolation, not
the full delete-before lifecycle including plan computation, cohort
batching, reorder drain, and the handoff to the transfer phase.

DML-1 provides the quantitative baseline needed to:

1. Validate DML-4's fast-path threshold (64 extras) by showing where the
   cohort pipeline overhead crosses the syscall-dominated regime.
2. Detect regressions in delete-phase startup latency.
3. Compare oc-rsync's latency profile against upstream at each scale to
   confirm parity (or quantify the gap).

## 2. Bench fixture setup

### 2.1 Scenario

The bench uses the strongest possible delete stimulus: an empty source
directory forces --delete-before to remove every file at the destination.
This isolates the delete phase from file-list comparison logic (no
matching, no partial deletes) and maximizes the ratio of delete time to
total wall-clock time.

### 2.2 Fixture layout

For each scale N in {10, 100, 1000, 10000, 100000}:

```
$FIXTURE_ROOT/
  src/              # empty directory (triggers full delete)
  dst/              # pre-populated with N regular files
    dir_0000/
      f_000000.dat  # 0 bytes (unlink cost is independent of size)
      f_000001.dat
      ...
    dir_0001/
      ...
```

Directory fan-out: `ceil(sqrt(N))` directories, each containing
`ceil(N / ceil(sqrt(N)))` files. This avoids ext4/XFS single-directory
scaling artifacts while keeping the topology simple and reproducible.

| Scale | Directories | Files/dir | Total files |
|-------|-------------|-----------|-------------|
| 10    | 4           | 3         | 10          |
| 100   | 10          | 10        | 100         |
| 1K    | 32          | 32        | 1,024       |
| 10K   | 100         | 100       | 10,000      |
| 100K  | 317         | 316       | ~100,000    |

File size is zero bytes. The benchmark measures `unlink(2)` dispatch
latency, not data reclamation. Zero-byte files also ensure the fixture
creation itself is fast and does not pollute page cache.

### 2.3 Fixture creation

A setup script (`tools/bench/delete_before_latency_fixture.sh`) creates
the fixture tree. Re-creation happens before every measurement run to
guarantee identical starting state. The script accepts `--scale N` and
`--root DIR` arguments.

```sh
#!/usr/bin/env bash
set -euo pipefail

SCALE=${1:-1000}
ROOT=${2:-/tmp/dml1-fixture}
DIRS=$(python3 -c "import math; print(math.ceil(math.sqrt($SCALE)))")
FILES_PER_DIR=$(python3 -c "import math; d=math.ceil(math.sqrt($SCALE)); print(math.ceil($SCALE/d))")

rm -rf "$ROOT"
mkdir -p "$ROOT/src"  # empty source
for d in $(seq -w 0 $((DIRS - 1))); do
    dir="$ROOT/dst/dir_${d}"
    mkdir -p "$dir"
    for f in $(seq -w 0 $((FILES_PER_DIR - 1))); do
        : > "$dir/f_${f}.dat"
    done
done
```

### 2.4 Filesystem requirements

- tmpfs or ext4/XFS with noatime. Avoid btrfs (CoW overhead skews
  unlink latency).
- The bench script validates the filesystem type and warns if running on
  an unsupported FS.
- Drop caches between runs: `sync && echo 3 > /proc/sys/vm/drop_caches`
  (requires root or CAP_SYS_ADMIN in containers).

## 3. Measurement methodology

### 3.1 What to measure

**Primary metric:** wall-clock time from the start of the delete phase to
the moment the first file transfer begins (or transfer phase reports
"nothing to transfer" for an empty source).

For upstream rsync, this is the time from `--delete-before` log output
("deleting...") to the first file-list send or "sent N bytes" summary.
For oc-rsync, instrumentation is more precise (see 3.2).

**Secondary metric:** total wall-clock time for the full transfer
(delete + file-list + transfer). This confirms the delete phase dominates
at each scale.

### 3.2 Instrumentation points

#### oc-rsync (internal timers)

Two instrumentation approaches, used in parallel:

1. **Environment-gated timestamps.** When `OC_RSYNC_BENCH_DELETE=1` is
   set, the delete context logs epoch-nanosecond timestamps at:
   - `T0`: entry to `DeleteContext::drain_before_transfer()`
   - `T1`: exit from `drain_before_transfer()` (all unlinks complete,
     stats frames emitted, consumer has drained)

   Output to stderr: `DML1_DELETE_NS=<T1 - T0>`.

2. **External wall-clock wrapper.** For comparison against upstream where
   internal instrumentation is unavailable, the bench script also records
   wall-clock of the full binary invocation. Since the fixture is
   src=empty and dst=populated, the transfer phase does zero work, so
   wall-clock approximately equals delete-phase time plus process
   startup overhead.

#### Upstream rsync

Upstream rsync does not expose sub-phase timers. Measurement uses the
external wall-clock approach:

```sh
/usr/bin/time -f '%e' rsync -a --delete-before src/ dst/ 2>&1
```

Since the source is empty, rsync's output time is dominated by the
delete phase plus startup. Process startup overhead (~5ms) is subtracted
using a no-op baseline measurement (`rsync --version`).

### 3.3 Warm-up and iteration count

| Scale | Warm-up runs | Measured runs | Reporting |
|-------|--------------|---------------|-----------|
| 10    | 3            | 21            | Median, P5, P95 |
| 100   | 3            | 21            | Median, P5, P95 |
| 1K    | 3            | 15            | Median, P5, P95 |
| 10K   | 2            | 11            | Median, P5, P95 |
| 100K  | 1            | 7             | Median, P5, P95 |

Warm-up runs are discarded. They prime the dentry cache and inode slab
so measured runs reflect steady-state `unlink(2)` performance.

Higher scales use fewer iterations because fixture re-creation time
dominates at 100K (creating 100K files takes ~2s on ext4). Total bench
time must stay under 10 minutes for CI feasibility.

### 3.4 Statistical reporting

Each scale produces:

- Median latency (ms) for oc-rsync and upstream
- P5 and P95 bounds (jitter envelope)
- Ratio: `oc_rsync_median / upstream_median`
- Verdict: PASS if ratio <= 1.20 at scales >= 1K, or <= 2.00 at
  scales < 1K (small-scale overhead is expected pre-DML-4)

Output format: CSV for machine consumption, ASCII table for human review.

```
scale,oc_rsync_median_ms,upstream_median_ms,ratio,p5_oc,p95_oc,p5_up,p95_up,verdict
10,2.3,1.1,2.09,1.8,3.1,0.9,1.4,PASS
100,4.1,2.8,1.46,3.5,5.2,2.3,3.4,PASS
1000,18.2,15.4,1.18,16.1,21.3,13.8,17.2,PASS
10000,142.0,128.5,1.10,135.2,155.1,121.3,138.7,PASS
100000,1380.0,1290.0,1.07,1320.0,1450.0,1240.0,1360.0,PASS
```

## 4. Scales and expected outcomes

### 4.1 Scale: 10 files

- Delete phase time: < 5ms for both implementations.
- Expected ratio: 1.5-2.5x (oc-rsync slower). The cohort pipeline setup
  cost (~50us for Condvar + slot + cursor) is comparable to unlink cost
  (~30us for 10 files). Process startup noise may dominate.
- Post-DML-4: fast path bypasses pipeline entirely, ratio drops to ~1.0.
- Significance: validates the fast-path threshold is needed.

### 4.2 Scale: 100 files

- Delete phase time: 5-15ms.
- Expected ratio: 1.2-1.8x. Pipeline overhead is visible but shrinking
  relative to unlink work.
- Post-DML-4: fast path active (100 > 64 threshold likely not triggered,
  but depends on per-directory counts). Ratio ~1.1-1.3.

### 4.3 Scale: 1K files

- Delete phase time: 15-40ms.
- Expected ratio: 1.05-1.20x. Unlink syscalls dominate. The pipeline
  overhead is amortized across enough cohorts that per-directory
  bookkeeping is negligible.
- This is the crossover point where the parallel pipeline's benefits
  begin to offset its costs.

### 4.4 Scale: 10K files

- Delete phase time: 100-200ms.
- Expected ratio: 0.95-1.10x. At this scale the parallel plan-compute
  phase can overlap with emitter drain, potentially making oc-rsync
  faster than upstream's serial loop.
- Rayon work-stealing across 100 directories provides measurable speedup
  on multi-core CI runners.

### 4.5 Scale: 100K files

- Delete phase time: 1.0-2.0s.
- Expected ratio: 0.80-1.05x. Parallel delete throughput dominates.
  oc-rsync's rayon-parallel `compute_extras` and parallel unlink (when
  `parallel-delete-consumer` is enabled) should yield clear wins.
- This scale is the primary justification for the pipeline's existence.

### 4.6 Threshold validation table

| Scale | Expected pre-DML-4 ratio | Expected post-DML-4 ratio | Fast-path active? |
|-------|--------------------------|---------------------------|-------------------|
| 10    | 1.5-2.5                  | ~1.0                      | Yes               |
| 100   | 1.2-1.8                  | 1.0-1.3                   | Depends on layout |
| 1K    | 1.05-1.20                | 1.05-1.20                 | No                |
| 10K   | 0.95-1.10                | 0.95-1.10                 | No                |
| 100K  | 0.80-1.05                | 0.80-1.05                 | No                |

## 5. Comparison methodology

### 5.1 Same fixture, same measurement

Both oc-rsync and upstream rsync run against the identical fixture tree.
The fixture is re-created between each individual measurement run to
restore the destination to its populated state.

### 5.2 Binary versions

- **Upstream rsync:** 3.4.1 (the protocol version target). Built from
  source at `target/interop/upstream-src/rsync-3.4.1/` or installed via
  the interop harness (`tools/ci/run_interop.sh`).
- **oc-rsync:** release build (`cargo build --release`) from the branch
  under test. Both with and without `--features parallel-delete-consumer`
  to show the sequential-only vs full-pipeline paths.

### 5.3 Transfer mode

Local transfer (`rsync -a --delete-before src/ dst/`). No daemon, no
SSH. This eliminates network/protocol overhead and isolates the delete
phase syscall cost.

### 5.4 Controlling for process startup

Process startup overhead is measured separately and subtracted:

```sh
# Measure startup overhead (no transfer, just arg parse + exit)
for i in $(seq 1 21); do
    /usr/bin/time -f '%e' rsync --version >/dev/null 2>>startup.log
done
startup_median=$(sort -n startup.log | awk 'NR==11')
```

The same technique applies to oc-rsync. Startup overhead is typically
3-8ms for upstream rsync, 1-3ms for oc-rsync (no libc startup).

### 5.5 Environment controls

- `RAYON_NUM_THREADS=4` (matches CI runner core count)
- `OC_RSYNC_BENCH_DELETE=1` (enables internal timestamps)
- `LANG=C` (avoids locale-related overhead)
- Taskset to a fixed CPU set where possible (`taskset -c 0-3`)
- No concurrent I/O workloads during measurement

## 6. Bench script

### 6.1 Location

`tools/bench/delete_before_latency.sh` - self-contained script that:

1. Validates prerequisites (upstream rsync binary, oc-rsync release
   binary, tmpfs or ext4 root)
2. Iterates through scales [10, 100, 1000, 10000, 100000]
3. At each scale: creates fixture, runs warm-up, runs measured iterations
   for both binaries, records times
4. Outputs CSV and ASCII summary table
5. Exits with code 1 if any scale exceeds its pass threshold

### 6.2 Usage

```sh
# Inside rsync-profile container or CI runner:
bash tools/bench/delete_before_latency.sh \
    --oc-rsync ./target/release/oc-rsync \
    --upstream /usr/bin/rsync \
    --root /tmp/dml1 \
    --output /tmp/dml1-results.csv
```

### 6.3 Output example

```
=== DML-1: --delete-before latency benchmark ===

Scale    oc-rsync (ms)   upstream (ms)   ratio   verdict
-----    -------------   -------------   -----   -------
10       2.3 [1.8-3.1]  1.1 [0.9-1.4]   2.09    PASS
100      4.1 [3.5-5.2]  2.8 [2.3-3.4]   1.46    PASS
1000     18.2 [16-21]   15.4 [14-17]    1.18    PASS
10000    142 [135-155]   128 [121-139]   1.10    PASS
100000   1380 [1320-1450] 1290 [1240-1360] 1.07  PASS

Results written to /tmp/dml1-results.csv
```

## 7. CI bench cell integration

### 7.1 Workflow

A new workflow `.github/workflows/bench-delete-latency.yml`:

```yaml
name: Delete Latency Bench (DML-1)
on:
  push:
    branches: [master]
    paths:
      - 'crates/engine/src/delete/**'
      - 'crates/engine/src/local_copy/deletion/**'
      - 'tools/bench/delete_before_latency.sh'
  pull_request:
    paths:
      - 'crates/engine/src/delete/**'
      - 'crates/engine/src/local_copy/deletion/**'
  workflow_dispatch: {}

concurrency:
  group: bench-delete-latency-${{ github.ref }}
  cancel-in-progress: true

jobs:
  delete-latency:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release
      - run: |
          # Build upstream rsync 3.4.1
          bash tools/ci/build_upstream_rsync.sh 3.4.1
      - run: |
          bash tools/bench/delete_before_latency.sh \
              --oc-rsync ./target/release/oc-rsync \
              --upstream ./target/interop/upstream-bin/rsync-3.4.1 \
              --root /tmp/dml1 \
              --output /tmp/dml1-results.csv
      - uses: actions/upload-artifact@v4
        with:
          name: dml1-results
          path: /tmp/dml1-results.csv
```

### 7.2 Trigger policy

- **On push to master:** only when delete-module source paths change.
  Keeps CI cost low (the 100K scale alone takes ~60s per binary).
- **On PR:** same path filter. Allows regression detection before merge.
- **Manual dispatch:** for ad-hoc profiling and pre/post DML-4
  comparison.

### 7.3 Failure policy

The bench cell is advisory (non-required). A ratio exceeding the
threshold at any scale posts a PR comment via `actions/github-script`
but does not block merge. Rationale: filesystem performance varies across
CI runner generations and co-tenant load; advisory comments surface
regressions without false-positive merge blocks.

### 7.4 Historical tracking

Results CSV is uploaded as an artifact. A companion step appends the
current run's median ratios to a persistent JSON file in
`gh-pages` (if the benchmark workflow is on master push), enabling
trend-over-time visualization in the repository's GitHub Pages site.

## 8. Acceptance criteria

DML-1 is complete when:

1. `tools/bench/delete_before_latency.sh` runs successfully on a Linux
   host with upstream rsync 3.4.1 available
2. Results at 1K+ scales show oc-rsync within 20% of upstream (ratio
   <= 1.20)
3. Results at 10/100 scales document the expected small-scale overhead
   that DML-4's fast path is designed to eliminate
4. The CI workflow triggers on delete-module path changes and uploads
   results as an artifact
5. CSV output is machine-parseable for downstream trend analysis

## 9. Limitations

- **Process startup noise.** At 10-file scale, startup overhead (~5ms)
  may exceed delete-phase time (~2ms). The startup-subtraction technique
  reduces but cannot eliminate this noise. Internal timestamp
  instrumentation (OC_RSYNC_BENCH_DELETE) provides ground truth for
  oc-rsync but not for upstream.

- **CI runner variability.** GitHub-hosted runners share physical hosts.
  Unlink latency varies 2-3x depending on co-tenant load and whether the
  runner's /tmp is backed by SSD or HDD. The median-of-N approach and
  advisory-only policy mitigate this.

- **No macOS/Windows.** The bench targets Linux ext4/tmpfs. macOS APFS
  has different unlink characteristics (metadata journaling overhead).
  Windows NTFS delete is substantially slower. Cross-platform benches
  are out of scope for DML-1.

- **Sequential emitter only.** The bench measures the production
  sequential emitter path (default build). The parallel-delete-consumer
  feature flag path is a separate measurement axis - it can be added as
  a third column in a follow-up iteration but is not in initial scope.

## 10. Relationship to other DML tasks

| Task   | Status    | Relationship to DML-1 |
|--------|-----------|----------------------|
| DML-1  | This doc  | Bench design and implementation |
| DML-2  | Open      | Profiling hotspots (uses DML-1 data as input) |
| DML-3  | Complete  | Fast-path design (justified by DML-1's small-scale data) |
| DML-4  | Design    | Fast-path implementation (validates against DML-1 baseline) |
| DML-5  | Open      | Regression gate (consumes DML-1's CI cell) |
| DML-6  | Complete  | Architecture doc (provides context for DML-1) |
