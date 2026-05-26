# 100K-file delete throughput benchmark harness (DEL-4.a)

Status: Design (task DEL-4.a; depends on DEL-2.c `ParallelDeleteEmitter` and
the sequential `DeleteEmitter` already shipping in production)
Audience: engine and CI maintainers evaluating the parallel-delete pipeline's
throughput ceiling.
Scope: a criterion bench harness measuring sequential vs parallel delete
throughput at 100,000 file scale across varied directory topologies, with CI
integration for regression detection.

Out of scope: the wire-byte parity gate (DEL-3 owns that), the cohort-batching
tuning decisions (DEL-1.c), and filesystem-level tuning (e.g., `tmpfs` vs
`ext4` effects on `unlink` latency).

## 1. Motivation

The `DeleteEmitter` is the hottest syscall path during `--delete-during` and
`--delete-before` transfers over large destinations. DEL-2.c introduced
`ParallelDeleteEmitter` (behind `--features parallel-delete-consumer`) which
dispatches per-cohort ops via rayon. Without a controlled benchmark at
realistic scale, there is no quantitative evidence of:

- Parallel speedup factor vs the sequential emitter.
- Throughput scaling behaviour across directory topologies (flat, deep, mixed).
- Regression risk from lock contention in the `SharedBatcher` Condvar path.
- Per-op dispatch overhead at 100K+ entry counts.

The existing `delete_end_to_end.rs` bench exercises the pipeline at 100K files
(100 dirs x 1000 files) but only measures the sequential `emit_all` path
against a legacy baseline. DEL-4.a introduces a head-to-head comparison of
both emitter implementations under varied topologies.

## 2. Fixture generation

All benchmarks operate on ephemeral `TempDir` fixtures created per iteration
via `criterion::iter_batched`. No persistent state between iterations; each
sample starts from identical filesystem conditions.

### 2.1 File tree builder

A shared `FixtureBuilder` generates 100,000 regular files distributed across
one of three topology profiles. Each file is a zero-byte regular file created
via `File::create` - the benchmark measures `unlink(2)` throughput, not data
removal, so file size is irrelevant to the measured path.

```rust
struct FixtureBuilder {
    root: PathBuf,
    topology: Topology,
    total_files: usize,
}

enum Topology {
    /// 100 directories, 1000 files each. Matches the existing
    /// `delete_end_to_end` shape for direct comparison.
    Flat,
    /// 10 top-level dirs, each with 10 subdirs, each with 10 sub-subdirs,
    /// each containing 100 files. Depth = 3, fan-out = 10.
    /// Tests the traversal cursor's depth-first ordering overhead.
    Nested,
    /// Mixed types: 60% regular files, 20% symlinks, 10% empty dirs,
    /// 10% FIFOs (unix only). Distributed across 200 directories (500
    /// entries each). Tests per-kind dispatch branching.
    MixedTypes,
}
```

### 2.2 File naming

Filenames use zero-padded numeric format (`f{n:06}.dat`) so lexicographic
order matches creation order, which matches the segment order the
`compute_extras` phase sees. This removes sort jitter from the measured path.

### 2.3 Keep-set fraction

Each topology designates 10% of entries as "extras" (present on disk but absent
from the segment / keep set). The benchmark deletes only the extras, mirroring
a realistic `--delete-during` scenario where most destination files survive.
This yields ~10,000 delete ops per iteration across 100K total entries.

For the scaling study (section 4.3) the extras fraction varies: 10%, 50%, 100%.
The 100% case (all files are extras) isolates pure delete throughput without
`compute_extras` noise.

## 3. Bench scenarios

Each scenario is a criterion benchmark group with both emitter variants as
sub-benchmarks.

### 3.1 `flat_100k` - 100 dirs x 1000 files

Topology: `Flat`. Extras: 10% (10K deletes).
Purpose: direct comparison with `delete_end_to_end.rs` for baseline continuity.
The flat topology maximizes intra-cohort parallelism (each cohort has 100
extras) and minimizes traversal-cursor overhead (100 directories, single
depth level).

### 3.2 `nested_100k` - depth-3 tree, fan-out 10

Topology: `Nested`. Extras: 10% (10K deletes).
Purpose: stress the `DirTraversalCursor`'s depth-first enumeration at scale.
1000 leaf directories each contribute 10 extras. Small cohorts reduce
per-cohort rayon dispatch efficiency, exposing task-scheduling overhead.

### 3.3 `mixed_types_100k` - symlinks, FIFOs, dirs

Topology: `MixedTypes`. Extras: 10% (10K deletes).
Purpose: exercise the `DeleteEntryKind` dispatch branches. Symlinks use
`unlink`, FIFOs use `unlink`, empty dirs use `rmdir`. The per-kind branching
in `DeleteEmitter` and `ParallelDeleteEmitter` must not introduce measurable
overhead vs the uniform-file case.

### 3.4 `flat_100k_full_delete` - 100% extras

Topology: `Flat`. Extras: 100% (100K deletes).
Purpose: peak throughput measurement. Every file on disk is an extra. This
isolates the delete path from the `compute_extras` set-difference overhead
and gives the maximum possible parallelism window.

### 3.5 `scaling_threads` - thread count sweep

Topology: `Flat`. Extras: 100% (100K deletes).
Thread counts: 1, 2, 4, 8, 16.
Purpose: characterize scaling behaviour. The sequential emitter uses a single
thread regardless. The parallel emitter uses a custom `rayon::ThreadPool` per
parameter point. Reports ops/sec per thread-count to identify the concurrency
sweet spot and diminishing-returns inflection on typical CI runners (2-4 cores).

## 4. Measurement methodology

### 4.1 Primary metrics

| Metric | Source | Reported as |
|--------|--------|-------------|
| Wall-clock time | criterion `measurement_time` | ns/iter, with confidence interval |
| Throughput | `Throughput::Elements(extras_count)` | ops/sec (deletions per second) |
| Speedup factor | parallel time / sequential time | ratio (computed in summary) |

### 4.2 CPU utilization (advisory)

CPU utilization is not directly measurable from criterion. Instead, the bench
records `rayon::current_num_threads()` at entry and the summary script
(section 6.2) derives utilization from:

```
utilization = (sequential_time / parallel_time) / num_threads
```

A utilization below 0.5 at `num_threads >= 4` signals lock contention in
the `SharedBatcher` or insufficient cohort-level parallelism.

### 4.3 Criterion configuration

```rust
fn config() -> Criterion {
    Criterion::default()
        .sample_size(20)           // 100K unlinks per sample is expensive
        .measurement_time(Duration::from_secs(30))
        .warm_up_time(Duration::from_secs(5))
        .noise_threshold(0.05)     // 5% noise floor for tmpfs variance
        .significance_level(0.01)  // 1% significance for regression detection
}
```

`sample_size = 20` balances statistical power against the fixture rebuild cost
(creating 100K files per sample is itself ~2s on ext4). The warm-up phase
primes the page cache and dentry cache so measured iterations see steady-state
kernel performance.

### 4.4 Iteration structure

Each iteration uses `iter_batched` with `BatchSize::PerIteration`:

1. **Setup** (untimed): create `TempDir`, run `FixtureBuilder`, construct
   `DeletePlanMap` + `DirTraversalCursor` from the fixture.
2. **Measured**: invoke `DeleteEmitter::emit_all` or
   `ParallelDeleteEmitter::run`.
3. **Teardown** (untimed): `TempDir` drops, cleaning residual files.

The setup phase is outside the timed region so fixture creation does not
pollute the measurement. `PerIteration` batching ensures no cross-iteration
filesystem state leakage.

## 5. Implementation structure

### 5.1 File location

`crates/engine/benches/delete_throughput_100k.rs`

### 5.2 Cargo.toml addition

```toml
[[bench]]
name = "delete_throughput_100k"
harness = false
required-features = ["parallel-delete-consumer"]
```

The `required-features` gate ensures the bench only compiles when the parallel
consumer feature is active, matching the pattern used by
`parallel_receive_delta_perf` and `parallel_verify_chunk`.

### 5.3 Module layout

```rust
// crates/engine/benches/delete_throughput_100k.rs

mod fixture;   // FixtureBuilder, Topology, file creation
mod harness;   // criterion group functions, emitter invocation wrappers

criterion_group!(
    name = delete_throughput;
    config = config();
    targets =
        flat_100k,
        nested_100k,
        mixed_types_100k,
        flat_100k_full_delete,
        scaling_threads,
);
criterion_main!(delete_throughput);
```

Inline submodules keep the bench self-contained without polluting `src/`.

### 5.4 Sequential vs parallel invocation

The sequential path calls:

```rust
let emitter = DeleteEmitter::new(RealDeleteFs::default());
let outcome = emitter.emit_all(&plan_map, &mut cursor);
```

The parallel path calls:

```rust
let emitter = ParallelDeleteEmitter::new(RealDeleteFs::default());
// ... enqueue all cohorts from pre-built plan ...
emitter.mark_producers_done();
let outcome = emitter.run();
```

Both paths receive identical `DeletePlanMap` and `DirTraversalCursor` state
so the comparison is apples-to-apples.

## 6. CI integration

### 6.1 Workflow file

`.github/workflows/bench-delete-throughput.yml`

Follows the pattern established by `bench-drain-throughput.yml` (DPC-8) and
`bench-daemon-coldstart.yml` (DIS-8.a):

- **Triggers**: `workflow_dispatch`, nightly cron (`17 6 * * *` - offset from
  existing bench cells), and `pull_request` on paths:
  - `crates/engine/src/delete/**`
  - `crates/engine/benches/delete_throughput_100k.rs`
  - `.github/workflows/bench-delete-throughput.yml`
- **Runner**: `ubuntu-latest` (2-core, ext4 `/tmp`).
- **Timeout**: 30 minutes job-level, 600s step-level for the criterion run.
- **Status**: non-required (advisory). Promotion to required gated on 4-week
  bake-in with stable baselines.
- **Concurrency**: `bench-delete-throughput-${{ github.ref }}`,
  `cancel-in-progress: true`.

### 6.2 Artifact and summary

The workflow uploads the criterion HTML report as a build artifact
(`criterion-delete-throughput-100k/`) and emits a step summary table:

```markdown
| Scenario | Sequential (ops/s) | Parallel (ops/s) | Speedup |
|----------|-------------------|------------------|---------|
| flat_100k | ... | ... | ...x |
| nested_100k | ... | ... | ...x |
| ...
```

The summary is extracted from criterion's JSON output via a post-bench shell
script (`tools/ci/extract_delete_bench_summary.sh`) that parses
`target/criterion/*/new/estimates.json`.

### 6.3 Regression detection

Criterion's built-in regression detection (`noise_threshold` + `significance_level`)
flags performance changes between runs. The CI step fails (soft, via
`continue-on-error: true`) when criterion reports a statistically significant
regression exceeding 10%. The step summary highlights regressions in bold.

No baseline persistence across CI runs (criterion baselines are ephemeral per
job). Cross-run trending relies on the nightly schedule building a history of
artifact reports reviewable in the Actions tab.

## 7. Success criteria

| Metric | Target |
|--------|--------|
| Parallel speedup (flat, 100% extras, 4 threads) | >= 2.0x |
| Parallel speedup (nested, 10% extras, 4 threads) | >= 1.5x |
| Sequential regression vs `delete_end_to_end` baseline | < 5% |
| CI wall-clock (full bench run) | < 15 minutes |
| Mixed-types overhead vs flat | < 10% |

These targets derive from the theoretical model: `unlink(2)` on tmpfs/ext4
is ~2-5 us per call, dominated by dentry cache lookups and inode deallocation.
Parallelism helps when the kernel's per-directory lock (`i_mutex`) is not the
bottleneck - which holds for the flat topology (many directories) but
may not for the nested topology (few entries per leaf dir, high cursor
overhead).

## 8. Open questions (deferred to implementation)

1. **tmpfs vs ext4**: CI runners use ext4 for `/tmp`. Should the bench force
   `tmpfs` via a pre-step `mount -t tmpfs`? tmpfs removes journal overhead
   and isolates pure VFS cost, but ext4 matches production deployments.
   Decision: start with ext4 (no special setup), add tmpfs variant later if
   variance is too high.

2. **macOS support**: `MixedTypes` uses FIFOs (`mkfifo`), which exist on macOS
   but not Windows. The bench file should carry `#![cfg(unix)]` to match the
   existing `delete_end_to_end` pattern. No Windows bench for delete throughput.

3. **Warm filesystem caches**: The first sample after fixture creation hits
   cold dentry caches. criterion's warm-up phase mitigates this, but if
   variance remains high, consider a dummy `stat` pass over all fixture files
   before measurement begins.

4. **Interaction with `io_uring`**: The delete pipeline does not currently use
   io_uring for `unlink` (see `project_io_uring_scope_metadata_only.md`).
   If `IORING_OP_UNLINKAT` support is added later, DEL-4.a gains a third
   axis (sequential / parallel-rayon / io_uring-batched). That extension is
   out of scope for the initial harness.

## 9. Relationship to other tasks

| Task | Relationship |
|------|-------------|
| DEL-1.a | Ordering audit - informs why cohort-sequential drain is mandatory |
| DEL-1.b | Reorder buffer design - the `SharedBatcher` whose contention we measure |
| DEL-1.c | Cohort batching - producer granularity affects parallel speedup |
| DEL-2.c | `ParallelDeleteEmitter` implementation - the subject under test |
| DEL-3 | Wire-byte parity gate - correctness prerequisite; DEL-4.a is perf only |
| DDP-I3 | `delete_end_to_end` bench - the existing baseline DEL-4.a extends |

## 10. Upstream reference

- `target/interop/upstream-src/rsync-3.4.1/delete.c:130-225`
  (`delete_item`): the per-entry syscall path both emitters replicate.
- `target/interop/upstream-src/rsync-3.4.1/generator.c:351-387`
  (`do_delete_pass`): the traversal order the cursor preserves.
