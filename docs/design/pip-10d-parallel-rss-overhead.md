# PIP-10.d - RSS overhead measurement: parallel vs sequential receive-delta

Date: 2026-05-26
Scope: measurement spec for peak RSS overhead of the parallel
receive-delta path relative to the sequential path
Status: design spec
Parent: PIP-10 (end-to-end parallel receive-delta validation)
Predecessors:
- PIP-9 wired the parallel-receive-delta feature gate into the
  receiver dispatch site.
- PIP-6 benchmarked wall-clock throughput; PIP-10.d measures the
  memory cost of the parallel path that PIP-6 demonstrated a
  throughput win for.
- RSS-12.a defined the CI workflow for flist RSS regression
  detection (absolute ceiling, 100K fixture, daemon pull).
Related: `project_rss_3_11x_upstream.md` (project-level RSS target
< 10% vs upstream)

## 1. Motivation

The parallel receive-delta path introduces memory-consuming
structures that the sequential path does not allocate:

1. **Rayon worker thread stacks.** Each rayon thread allocates an
   8 MiB stack by default (configurable via `RAYON_NUM_THREADS` and
   `rayon::ThreadPoolBuilder::stack_size`). On a 4-core runner this
   is 32 MiB of virtual address space, though RSS only reflects
   pages actually touched.
2. **Per-file reorder buffers.** `ParallelDeltaApplier` creates a
   `ReorderBuffer<DeltaChunk>` per registered file, pre-allocating
   `Box<[Option<DeltaChunk>]>` with capacity 64 slots
   (`DEFAULT_PER_FILE_REORDER_CAPACITY`). Each `DeltaChunk` carries
   a `Vec<u8>` data payload, so resident cost scales with
   `active_files * reorder_capacity * avg_chunk_size`.
3. **DashMap for concurrent file slot tracking.**
   `ParallelDeltaApplier::files` is a `DashMap<FileNdx, SlotEntry>`
   that shards across internal read-write locks. Per-entry overhead
   includes `Arc<SlotData>` (wrapping `Mutex<FileSlot>`),
   `Arc<BarrierState>` (inflight counter + Condvar), and the DashMap
   shard bookkeeping.
4. **Work queue channel buffer.** The bounded `crossbeam_channel`
   holding `DeltaWork` items has capacity `2 * thread_count`
   (default) to `8 * thread_count` (small-file adaptive). Each
   `DeltaWork` carries two `PathBuf` fields and a `u64` size.
5. **Spill buffers.** When the `SpillableReorderBuffer` layer is
   engaged (`SpillPolicy::threshold_bytes` is `Some`), in-memory
   items beyond the threshold are serialized to tempfiles. The
   in-memory residue below the threshold is additional to the
   sequential path's footprint.

Without measurement, the combined overhead is speculative. PIP-10.d
defines the methodology to quantify it and the CI cell to prevent
it from regressing.

## 2. Design principles

1. **Relative comparison, not absolute ceiling.** RSS-12.a uses an
   absolute baseline ceiling against a flist-build workload. PIP-10.d
   compares two builds of the same binary - one with
   `parallel-receive-delta` (default), one without - on the same
   workload. The delta isolates the parallel path's overhead.
2. **VmHWM over getrusage.** `/proc/self/status` `VmHWM` captures the
   kernel's high-water mark for resident pages, which is more
   precise than GNU time's `MaxRSS` (derived from `getrusage`
   `ru_maxrss`, which can lag behind THP promotions). On macOS,
   `getrusage` reports RSS in bytes (not KB), so the measurement is
   advisory-only there.
3. **Production binary, not microbench.** Both arms run a real
   `oc-rsync` daemon pull over `rsync://` loopback, exercising the
   full transfer pipeline: file-list build, signature exchange,
   delta generation, dispatch, apply, metadata commit.
4. **Three workload scales.** 1K, 10K, and 100K files exercise the
   overhead curve. 1K catches per-file fixed costs (DashMap, slot
   registration); 100K stresses cumulative costs (reorder buffers,
   work queue backpressure).
5. **Spill layer off.** The default `SpillPolicy` disables spilling
   (`threshold_bytes: None`). The measurement captures the
   pure-parallel overhead without the spill layer's tempfile I/O
   and re-serialization cost. A future PIP-10.e can layer on
   spill-enabled runs.

## 3. Measurement methodology

### 3.1 Platform

**Linux (`ubuntu-latest`) only for the gated check.** VmHWM is
read from `/proc/self/status` and is deterministic across runs for
allocation-dominated workloads. macOS results are collected as
informational (advisory) but not gated, because Darwin's page-cache
attribution inflates RSS non-deterministically.

### 3.2 RSS capture

A wrapper script reads `/proc/$PID/status` at process exit and
extracts the `VmHWM` field (kilobytes). This is more reliable than
`/usr/bin/time -v` for two reasons:

- GNU time's `MaxRSS` on some glibc versions rounds to page
  boundaries differently.
- VmHWM is updated by the kernel on every page-in, not polled.

Fallback: if `/proc/$PID/status` is unavailable (container
restriction), use `/usr/bin/time -v` and extract
`Maximum resident set size (kbytes)`.

```bash
measure_vmhwm() {
    local pid=$1
    wait "$pid"
    local exit_code=$?
    local vmhwm_kb
    vmhwm_kb=$(grep VmHWM /proc/$pid/status 2>/dev/null \
                | awk '{print $2}')
    echo "${vmhwm_kb:-0}"
    return $exit_code
}
```

For macOS (advisory), use:

```bash
/usr/bin/time -l oc-rsync ... 2>&1 | grep 'maximum resident set size'
```

Note: macOS reports bytes, not kilobytes.

### 3.3 Binary builds

Two release builds per CI run:

| Arm | Feature flags | Binary name |
|-----|---------------|-------------|
| Sequential | `--no-default-features` (strips `parallel-receive-delta`) | `oc-rsync-seq` |
| Parallel | default features (includes `parallel-receive-delta`) | `oc-rsync-par` |

Both builds share the same Rust toolchain (1.88.0), release profile,
and cargo cache. The only difference is the feature gate.

### 3.4 Transfer mode

Daemon pull over loopback (`rsync://127.0.0.1:$PORT/bench/`).
`--no-inc-recursive` forces the full file list into memory before
transfer begins, matching the RSS-12.a methodology and maximizing
the receiver-side footprint where the parallel path's structures
live.

### 3.5 Sampling

For each workload and each arm:

1. **1 warm-up run** (discarded) to prime filesystem caches and
   the rayon thread pool.
2. **5 measured runs** capturing VmHWM per run.
3. **Median** of the 5 runs is the reported value.

Five runs (vs RSS-12.a's three) because the parallel path has more
variance from rayon scheduling jitter and DashMap shard contention.

### 3.6 Thread count control

Pin `RAYON_NUM_THREADS=4` for both arms so the measurement is
reproducible across GHA runner generations (which may have 2 or 4
cores). The sequential arm ignores this variable but setting it
ensures the rayon global pool (initialized lazily) has the same
footprint in both arms if any code path touches it.

## 4. Workload matrix

### 4.1 Fixture structure

```
bench-rss-par/src/
  dir-NNNN/
    file-NNNN.dat
```

Three scale tiers:

| Tier   | Dirs | Files/dir | Total files | File size | Total data |
|--------|------|-----------|-------------|-----------|------------|
| Small  | 10   | 100       | 1,000       | 1 KiB     | ~1 MiB     |
| Medium | 100  | 100       | 10,000      | 1 KiB     | ~10 MiB    |
| Stress | 100  | 1,000     | 100,000     | 1 KiB     | ~100 MiB   |

Files are 1 KiB of deterministic content (byte = `(dir_idx ^ file_idx) & 0xFF`).
Small files maximize the per-file overhead ratio (DashMap entry,
reorder buffer allocation, slot registration) relative to data
payload. This is the adversarial shape for the parallel path's
memory cost.

### 4.2 Initial sync (cold destination)

The first sync transfers every file as whole-file (no basis).
This exercises the `WholeFileStrategy` arm of the concurrent delta
pipeline and the DashMap's registration path for every file in the
list. Cold start is the worst case for peak RSS because all file
slots are live simultaneously under `--no-inc-recursive`.

### 4.3 Delta sync (warm destination)

A second sync against the same (unmodified) destination exercises
the quick-check short-circuit path. This run should show near-zero
delta overhead for both arms. Including it guards against
regressions where the parallel path allocates structures even when
no transfer occurs.

## 5. Per-component RSS attribution

Beyond the aggregate VmHWM comparison, the spec defines a breakdown
methodology to attribute overhead to specific structures. This is
informational (not gated) and runs in a separate job or manually.

### 5.1 Rayon worker stacks

Compare VmHWM of a minimal binary that initializes the rayon pool
(4 threads, 8 MiB stack each) vs one that does not. Expected
overhead: 0-32 MiB virtual, near-zero RSS unless workers are active.

### 5.2 Per-file reorder buffers

`ReorderBuffer::new(64)` pre-allocates `Box<[Option<DeltaChunk>]>`
with 64 `None` slots. `size_of::<Option<DeltaChunk>>()` on 64-bit
is approximately 56 bytes (DeltaChunk has `Vec<u8>` + metadata;
`None` variant is pointer-sized with niche optimization or full
enum size). At 100K files:

- Reorder slot memory: `100,000 * 64 * 56 = ~341 MiB` (upper bound
  if all files are registered simultaneously).

In practice, the pipeline limits concurrent registrations to
`work_queue_capacity` (8-16 files). The reorder buffer is allocated
at `register_file` time and freed at `finish_file` time, so
steady-state live buffers equal the work queue depth, not the total
file count.

Expected overhead: `work_queue_depth * 64 * 56 = 16 * 64 * 56
= ~57 KiB` (negligible).

### 5.3 DashMap overhead

`DashMap<FileNdx, SlotEntry>` carries per-shard `RwLock` overhead
plus per-entry bookkeeping. DashMap defaults to
`num_cpus * 4` shards. Each entry stores:

- Key: `FileNdx` (4 bytes)
- Value: `SlotEntry` = `Arc<SlotData>` (8 bytes pointer) +
  `Arc<BarrierState>` (8 bytes pointer)
- Hash + bucket overhead: ~64 bytes per entry (hashbrown internals)

At 100K files, all entries live simultaneously:
`100,000 * ~80 = ~8 MiB`.

Under `--no-inc-recursive` every file is registered before transfer
starts, so the DashMap grows to full size. With INC_RECURSE the
map stays bounded by the sliding window.

### 5.4 Work queue channel

`crossbeam_channel::bounded(capacity)` with capacity 8-16 items.
Each `DeltaWork` is ~200 bytes (two `PathBuf` + scalars). Channel
overhead: `16 * 200 = ~3.2 KiB` (negligible).

### 5.5 Spill buffers

Disabled by default. When enabled, the `SpillableReorderBuffer`
adds a `BTreeMap<u64, u64>` spill index (~48 bytes per spilled
item) plus the tempfile handle. Not measured in this spec; deferred
to PIP-10.e.

### 5.6 Attribution method

Instrument a debug build with `jemalloc` profiling
(`_RJEM_MALLOC_CONF=prof:true,prof_final:true`) and post-process
with `jeprof` to produce a flame graph of heap allocations. Map
the top frames to the component categories above. This step is
manual-only; CI measures aggregate VmHWM, not per-component
breakdown.

## 6. Acceptable overhead

### 6.1 Primary gate

```
parallel_vmhwm_mb <= sequential_vmhwm_mb * 1.30
```

The parallel path may use up to 30% more peak RSS than the
sequential path on the same workload. This bound applies to each
workload tier independently.

**Why 1.3x:**

- The per-file structures (reorder buffers, DashMap entries,
  slot barriers) are lightweight individually but scale with
  concurrent file count. At 100K files the DashMap alone adds
  ~8 MiB; on a 40-50 MiB sequential baseline that is ~16-20%.
- Rayon stack pages are demand-paged; actual RSS contribution
  depends on worker utilization. Under sustained load (100K
  small files) expect 2-4 MiB of touched stack pages.
- 30% headroom accommodates allocator fragmentation differences
  between the two feature configurations. The parallel path's
  mixed allocation pattern (many small DashMap entries + few
  large reorder buffers) fragments differently than the
  sequential path's uniform allocation pattern.
- Tighter than 1.3x risks false positives from GHA runner memory
  noise (+/- 2-3% between runs). Looser than 1.5x would mask
  regressions like accidentally holding all reorder buffers
  simultaneously instead of lazily.

### 6.2 Informational metrics

Collected but not gated:

- **Delta sync overhead**: parallel vs sequential on warm destination.
  Expected: < 5% difference (neither path allocates transfer
  structures when quick-check skips all files).
- **Per-tier scaling**: ratio at 1K vs 10K vs 100K. Linear scaling
  suggests per-file overhead dominates; sub-linear suggests
  fixed costs (rayon pool, DashMap shards) dominate.
- **macOS VmHWM**: advisory comparison. Darwin page-cache behavior
  may inflate both arms, but the relative delta should be
  comparable to Linux.

## 7. CI integration

### 7.1 Workflow file

`.github/workflows/bench-rss-parallel.yml`

### 7.2 Triggers

```yaml
on:
  workflow_dispatch:
  schedule:
    - cron: '23 3 * * *'   # Nightly at 03:23 UTC
  pull_request:
    paths:
      - 'crates/engine/src/concurrent_delta/**'
      - 'crates/engine/src/delta/**'
      - 'crates/transfer/src/receiver/**'
      - 'crates/core/src/session/**'
      - '.github/workflows/bench-rss-parallel.yml'
```

**Path filtering rationale:**
- `concurrent_delta/` - all parallel path structures.
- `delta/` - block matching that feeds the parallel apply.
- `receiver/` - dispatch site that selects parallel vs sequential.
- `session/` - orchestration that wires config into the pipeline.

### 7.3 Concurrency

```yaml
concurrency:
  group: bench-rss-parallel-${{ github.ref }}
  cancel-in-progress: true
```

### 7.4 Job structure

Single job: `bench-rss-parallel` on `ubuntu-latest`, timeout
25 minutes.

Steps:

1. **Checkout.**
2. **Install Rust toolchain** (1.88.0).
3. **Cache cargo builds** (`Swatinem/rust-cache`, key
   `bench-rss-parallel`).
4. **Build sequential binary** -
   `cargo build --release -p oc-rsync --no-default-features`
   (strips `parallel-receive-delta`). Copy to `oc-rsync-seq`.
5. **Build parallel binary** -
   `cargo build --release -p oc-rsync` (default features). Copy
   to `oc-rsync-par`.
6. **Generate fixtures** - shell loops for 1K, 10K, 100K tiers.
7. **Write daemon config** - rsyncd.conf with `[bench-1k]`,
   `[bench-10k]`, `[bench-100k]` modules.
8. **Allocate dynamic port** - Python `socket.bind(0)` trick.
9. **For each tier (1k, 10k, 100k):**
   a. Start daemon (sequential binary).
   b. Warm-up run (discard).
   c. 5 measured runs, capture VmHWM.
   d. Stop daemon.
   e. Start daemon (parallel binary).
   f. Warm-up run (discard).
   g. 5 measured runs, capture VmHWM.
   h. Stop daemon.
   i. Compute medians, compute ratio.
   j. Assert `ratio <= 1.30`.
10. **Generate step summary** - markdown table.
11. **Upload artifact** - JSON with per-tier raw measurements.

### 7.5 Step summary format

```markdown
### RSS parallel vs sequential (PIP-10.d)

| Tier | Files | Sequential (MB) | Parallel (MB) | Ratio | Gate |
|------|-------|-----------------|---------------|-------|------|
| 1K   | 1,000 | 18              | 19            | 1.06x | PASS |
| 10K  | 10,000 | 24             | 28            | 1.17x | PASS |
| 100K | 100,000 | 42            | 52            | 1.24x | PASS |

Threshold: 1.30x
Result: **PASS**
```

### 7.6 Artifact JSON

```json
{
  "tiers": [
    {
      "name": "1k",
      "file_count": 1000,
      "sequential_runs_mb": [18, 18, 18, 19, 18],
      "parallel_runs_mb": [19, 19, 20, 19, 19],
      "sequential_median_mb": 18,
      "parallel_median_mb": 19,
      "ratio": 1.06
    }
  ],
  "threshold": 1.30,
  "all_pass": true,
  "rayon_threads": 4,
  "commit": "abc1234",
  "timestamp": "2026-05-26T03:23:00Z"
}
```

### 7.7 Status: non-required

Ships with `continue-on-error: true`. Promotion to a required check
is tracked as PIP-10.e, gated on:
- 2 weeks of stable nightly runs without false positives.
- Baseline parallel/sequential ratios established from real
  measurements.

## 8. Platform coverage

| Platform | Role | Rationale |
|----------|------|-----------|
| Linux (`ubuntu-latest`) | Gate (required after bake-in) | `/proc/self/status` VmHWM, deterministic RSS |
| macOS | Advisory (separate job, `continue-on-error: true`) | `getrusage` RSS in bytes; page-cache inflation; high CI cost |
| Windows | Not included | No `/proc`, no reliable VmHWM equivalent in standard tooling |

## 9. Relationship to existing harnesses

| Harness | What it measures | Automation |
|---------|-----------------|-----------|
| RSS-12.a | Flist RSS ceiling (absolute, 100K) | CI gate (post bake-in) |
| PIP-6 | Wall-clock throughput, parallel vs sequential | CI bench |
| BR-3i.f | Apply-loop scheduling cost (in-memory) | `cargo bench` |
| **PIP-10.d** | Peak RSS overhead, parallel vs sequential | CI gate (post bake-in) |

PIP-10.d complements RSS-12.a by measuring the parallel path's
incremental memory cost rather than the absolute flist footprint.
Together they ensure: (a) the flist migration keeps RSS bounded
(RSS-12.a), and (b) the parallel receive-delta path does not blow
the budget (PIP-10.d).

## 10. Risks and mitigations

| Risk | Mitigation |
|------|-----------|
| GHA runner core count varies (2 vs 4) | Pin `RAYON_NUM_THREADS=4`; worker stacks are demand-paged so extra threads cost near-zero RSS if unused |
| Allocator fragmentation differs between feature configurations | 30% threshold absorbs typical fragmentation variance; both arms use the same system allocator (no jemalloc) |
| VmHWM includes shared library pages mapped by both arms equally | Shared pages cancel in the ratio; only heap delta matters |
| `--no-inc-recursive` forces worst-case flist RSS, inflating both arms | Intentional: worst case is the right thing to gate. INC_RECURSE comparison is a future extension |
| Fixture generation time for 100K files | `seq \| xargs -P$(nproc) dd ...` parallelizes creation; ~5 s on GHA |
| Rayon pool warm-up inflates first measured run | Warm-up run is discarded; 5 measured runs with median smooth jitter |

## 11. Future extensions

1. **PIP-10.e: spill-enabled overhead.** Same workload matrix with
   `SpillPolicy::threshold_bytes` set to 16 MiB. Measures RSS under
   spill pressure (items evicted to tempfile, reloaded on delivery).
2. **INC_RECURSE mode.** Once sender-side INC_RECURSE is stable,
   repeat with default flags (no `--no-inc-recursive`). The sliding
   flist window bounds both the flist and the DashMap; RSS delta
   should shrink.
3. **Large-file workload.** Replace 1 KiB files with 1 MiB files
   (1K total, ~1 GiB). Exercises the delta apply path with real
   block matching; DeltaChunk payloads carry actual block data.
4. **Per-component CI attribution.** Wire `jemalloc` profiling into
   a nightly-only job that produces a heap flame graph artifact,
   tagged by component (reorder, DashMap, work queue, rayon).
5. **Baseline auto-ratchet.** When the parallel/sequential ratio
   drops (optimization lands), a bot opens a PR to tighten the
   1.30x threshold toward the new measured ratio + 10% headroom.
