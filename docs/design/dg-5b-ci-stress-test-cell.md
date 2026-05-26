# DG-5.b: CI Cell for Concurrent `finish_file` Stress Test

**Status:** Design
**Depends on:** DG-5.a (concurrent `finish_file` stress test specification)
**Parent series:** DG (DecrementGuard/SlotBarrier release race)
**Workflow file:** `.github/workflows/ci.yml` (new `dg5a-stress` job)

## 1. Problem Statement

DG-5.a specifies a 1000-thread, 10K-iterations-per-thread stress test that
validates the DG-3 structural fix (`SlotData`/`BarrierState` split) under
extreme concurrency. The test spawns 1000 OS threads and runs 10M total cycles,
making it too expensive for standard PR test runs. It must run regularly to
catch regressions - particularly on Windows where the original race symptom
surfaced - without bloating every contributor's CI feedback loop.

This document specifies how DG-5.a integrates into CI.

## 2. Decision: Extend `ci.yml` (Not a Dedicated Workflow)

### Option A: New dedicated workflow file (rejected)

A standalone `stress-dg5a.yml` would follow the pattern used by
`bench-daemon-coldstart.yml` and `bench-daemon-concurrency.yml`. These
dedicated workflows make sense for bench cells that have unique dependencies
(hyperfine, upstream rsync builds, daemon setup/teardown). The DG-5.a stress
test has none of these - it is a pure Rust test that requires only `nextest`
and the `dg-stress` feature flag.

### Option B: New matrix entry in `ci.yml` `dg3-stress` job (selected)

The existing `dg3-stress` job in `ci.yml` already provides the exact
infrastructure DG-5.a needs:

- Three-platform matrix (`ubuntu-latest`, `windows-latest`, `macos-latest`)
- `dg-stress` feature flag compilation
- `nextest` installation (with Windows direct-download workaround)
- `continue-on-error: true` non-required posture
- 30-minute timeout
- Shared Rust cache keyed to the DG stress prefix

Rather than duplicating this infrastructure in a new workflow, DG-5.a adds a
second `nextest run` step to the existing `dg3-stress` job. The build step
already compiles with `--features dg-stress`, so both DG-3.e and DG-5.a tests
are compiled in a single `cargo build` invocation. The marginal cost of the
second test step is runtime only - no additional compilation.

**Rationale:**

1. **Shared compilation.** Both DG-3.e and DG-5.a gate on the same feature
   (`dg-stress`). Building once and running two test filters is cheaper than
   two separate jobs each building from scratch, even with cargo caching.
2. **Consistent platform coverage.** DG-5.a inherits the three-platform matrix
   without duplication. Changes to the matrix (e.g., adding `ubuntu-24.04` or
   pinning a Windows runner version) propagate automatically.
3. **No schedule trigger needed.** The `ci.yml` workflow already triggers on
   every push to `master`, every PR touching `crates/**`, and on
   `workflow_dispatch`. DG stress tests run on every code-touching PR - more
   coverage than a nightly schedule alone.
4. **Precedent.** The `dg3-stress` job was designed to be the home for DG
   stress tests. Its name, cache keys, and comments anticipate additional
   tests in the series.

### Naming

Rename the job from `dg3-stress` to `dg-stress` to reflect that it now hosts
both DG-3.e and DG-5.a tests. Update the display name to
`DG stress (${{ matrix.os }}, non-required)`.

## 3. Trigger Strategy

DG-5.a inherits `ci.yml`'s existing triggers:

| Trigger | Condition |
|---------|-----------|
| Push to `master` | Paths: `crates/**`, `src/**`, `tests/**`, `Cargo.toml`, `Cargo.lock`, etc. |
| Pull request | Same path filter |
| `workflow_dispatch` | Manual, no path filter |

No nightly schedule is added. The PR and master-push triggers provide
sufficient coverage for regression detection. A nightly schedule would add CI
cost without catching regressions any sooner than the next PR merge.

### Path-Scoped Trigger (Future Consideration)

If CI cost becomes a concern, the `dg-stress` job could be conditioned on a
path filter via a `changes` output from `dorny/paths-filter` or GitHub's
native path filtering. Relevant paths:

```
crates/engine/src/concurrent_delta/parallel_apply/**
crates/engine/tests/parallel_apply_dg*
```

This is deferred because `ci.yml` currently runs all jobs on every
code-touching PR, and adding per-job path filters would be a broader
architectural change to the CI pipeline.

## 4. Platform Matrix

```yaml
strategy:
  fail-fast: false
  matrix:
    os: [ubuntu-latest, windows-latest, macos-latest]
```

| Platform | Priority | Justification |
|----------|----------|---------------|
| `ubuntu-latest` (x86_64) | Required | Primary development and deployment target. Cheapest CI minutes. |
| `windows-latest` (x86_64) | Required | Historical symptom surface for the DG-1 race. Windows' coarser thread quantum (15.6ms) widens the drop-body execution window. Must confirm the structural fix eliminates the symptom. |
| `macos-latest` (aarch64) | Required | Different scheduler semantics (Mach threads, GCD). Per-process thread limit (~2048) is the tightest constraint on the 1000-thread test. Validates the 512KB stack size mitigation. |

All three platforms are required for the DG series because the original race
was platform-dependent. Dropping any platform would leave a known regression
surface unmonitored.

## 5. Resource Requirements

### 5.1 Thread Count

1000 OS threads per test invocation. Each thread uses a 512KB stack
(explicitly set via `std::thread::Builder::stack_size`), consuming ~500MB of
virtual address space. On GitHub-hosted runners (7GB RAM for Linux/Windows,
14GB for macOS), this is well within limits since the stacks are lazily
committed.

### 5.2 Memory

Peak RSS is dominated by the 1000 thread stacks plus the `ParallelDeltaApplier`
DashMap. Expected peak: ~200-400MB on Linux (512KB stacks are virtual; actual
resident pages are much smaller due to the lightweight per-iteration workload).

No memory limit assertion is added. The test's success criteria are
correctness-focused (zero panics, zero `ApplierStillReferenced` errors). Memory
regression detection is out of scope for DG-5.b.

### 5.3 Timeout

The job-level timeout remains 30 minutes (inherited from the existing
`dg3-stress` job). The DG-5.a test itself should complete in under 60 seconds
on a 4-core runner. The 30-minute job timeout covers:

- Rust compilation (~5-10 minutes on cache miss)
- DG-3.e stress test (~30-60 seconds)
- DG-5.a stress test (~30-60 seconds)
- Overhead (checkout, cache restore, nextest install)

If the DG-5.a test exceeds 120 seconds, it indicates a potential livelock
introduced by DG-4.a's spin-yield removal. The test's internal harness should
enforce a 120-second timeout at the Rust level (via a watchdog thread or
`std::time::Instant` check) and surface this as a test failure rather than
relying on the 30-minute job timeout.

### 5.4 Estimated CI Minutes per Run

| Step | Linux | Windows | macOS |
|------|-------|---------|-------|
| Checkout + cache | 0.5 min | 0.5 min | 0.5 min |
| Rust toolchain | 0.5 min | 0.5 min | 0.5 min |
| Build (cache hit) | 1-2 min | 2-3 min | 1-2 min |
| Build (cache miss) | 5-8 min | 8-12 min | 5-8 min |
| DG-3.e test | 0.5-1 min | 0.5-1 min | 0.5-1 min |
| DG-5.a test | 0.5-1 min | 0.5-1 min | 0.5-1 min |
| **Total (cache hit)** | **3-5 min** | **4-6 min** | **3-5 min** |
| **Total (cache miss)** | **7-11 min** | **10-15 min** | **7-11 min** |

Cross-platform total per PR (cache hit): ~10-16 minutes of CI time across all
three runners. This is comparable to the existing `dg3-stress` job cost since
the compilation step is shared and the incremental runtime of DG-5.a is under
one minute.

GitHub Actions billing:
- Linux: 1x multiplier
- Windows: 2x multiplier
- macOS: 10x multiplier

Effective billed minutes per PR (cache hit): ~3 + ~8 + ~30 = ~41 billed
minutes. The macOS leg dominates. If cost becomes a concern, macOS can be
dropped first (it is the least likely to surface a DG race due to its
scheduler characteristics, despite being the most expensive).

## 6. Feature Flag

The test is gated behind two Cargo features:

```rust
#![cfg(all(feature = "dg-stress", feature = "parallel-receive-delta"))]
```

The `dg-stress` feature in `crates/engine/Cargo.toml` implies
`parallel-receive-delta`:

```toml
dg-stress = ["parallel-receive-delta"]
```

Standard `cargo nextest run --all-features` does NOT enable `dg-stress`
because it is not listed in the workspace `default` or `all-features` lists.
The feature exists solely to gate expensive stress tests that should only run
in dedicated CI cells.

The CI step passes `--features dg-stress` explicitly:

```yaml
- name: Build engine (--features dg-stress)
  run: cargo build --locked -p engine --tests --features dg-stress
```

## 7. Test Execution Command

```yaml
- name: Run DG-5.a stress (--features dg-stress)
  run: >-
    cargo nextest run --locked -p engine --features dg-stress
    --no-tests=pass
    -E 'test(concurrent_finish_file)'
```

Key flags:

- `--locked` - Ensures Cargo.lock is authoritative; fails fast on dependency
  drift.
- `-p engine` - Scopes to the engine crate only.
- `--features dg-stress` - Enables the feature gate.
- `--no-tests=pass` - If the test filter matches zero tests (e.g., test was
  renamed), the step passes rather than failing. This keeps the cell green
  during refactors while the build step still gates compilation regressions.
- `-E 'test(concurrent_finish_file)'` - Nextest filter expression matching
  the DG-5.a test function name prefix.

Note: `--test-threads 1` is NOT passed. The DG-5.a test is a single test
function that internally spawns 1000 threads. Nextest's parallelism controls
how many test *functions* run concurrently, not threads within a test. Since
the filter matches a single test, nextest's default parallelism is fine.
However, if DG-5.a grows additional test variants (e.g.,
`concurrent_finish_file_with_multi_chunk_overlap`), and these are run in the
same step, nextest should be constrained to `--test-threads 1` to avoid
spawning 2000+ OS threads simultaneously.

## 8. Success and Failure Criteria

### 8.1 Success

The CI step succeeds (exit code 0) when:

1. `cargo nextest run` exits 0 - all matched tests passed.
2. The test function itself asserts (inside the Rust harness):
   - Zero panics across all 1000 worker threads.
   - Zero `ApplierStillReferenced` errors from `finish_file`.
   - All finisher threads complete all 10K iterations.
   - Byte integrity across all sink counters.
   - `drain_inflight()` succeeds after all threads join.
3. Runtime is under 120 seconds (enforced by the test's internal watchdog).

### 8.2 Failure

The CI step fails (nonzero exit code) when any of the above assertions fail.
Failure modes and their diagnostics are documented in DG-5.a Section 9.

### 8.3 Non-Required Status

The `dg-stress` job uses `continue-on-error: true`. A failure surfaces as an
orange check in the PR status area but does not block merging. This is the
standard posture for stress and bench cells in this repository
(`dg3-stress`, `bench-daemon-coldstart`, `bench-daemon-concurrency`,
`bench-drain-throughput`, `ssh-smoke-bench`).

Promotion to a required check is tracked separately. The promotion criteria
are:

1. Ten consecutive green runs on `master` with no false-positive flakes.
2. No flakes attributable to CI runner thread-limit pressure (particularly on
   macOS with its ~2048 thread ceiling).
3. Runtime consistently under 120 seconds across all three platforms.

## 9. Notification and Failure Surfacing

### 9.1 GitHub Check Status

The primary notification mechanism. The `DG stress` check appears in the PR's
status checks list. Orange (non-required failure) is visible to reviewers and
the PR author. Green confirms the stress test passed.

### 9.2 GitHub Step Summary

The test's stdout/stderr is captured by nextest and visible in the GitHub
Actions log. No additional step summary is added because the test output is
self-explanatory (nextest's pass/fail report with panic messages on failure).

If future iterations add performance metrics (e.g., wall-clock time per
platform), a `$GITHUB_STEP_SUMMARY` step can be added following the pattern
in `bench-daemon-coldstart.yml`.

### 9.3 Issue Creation (Not Implemented)

Automatic issue creation on failure (e.g., via `JasonEtco/create-an-issue`) is
not implemented. The non-required posture means failures are advisory. Once the
job is promoted to required, automatic issue creation for `master`-branch
failures should be considered.

### 9.4 Slack/Email (Not Implemented)

No Slack or email notification is configured. GitHub's built-in notification
system (watch settings, CODEOWNERS mentions) is sufficient for the current
team size.

## 10. Relationship to Existing Stress and Bench Cells

### 10.1 DG-3.e Stress (`concurrent_register_and_dispatch_stress`)

Same job, different test filter. DG-3.e validates register/dispatch
correctness under fan-out. DG-5.a validates `finish_file` under tight
timing overlap between `DecrementGuard::drop` and `Arc::try_unwrap`. Both
share the `dg-stress` feature gate and run sequentially in the same job.

### 10.2 Daemon Concurrency Bench (`bench-daemon-concurrency.yml`)

Superficially similar (1000 concurrent connections) but tests a completely
different layer. The daemon bench exercises the TCP accept path, module
selection, and file transfer. DG-5.a exercises the in-process concurrent
delta applier. No shared infrastructure or mutual interference.

### 10.3 Parallel Determinism (`parallel_determinism.yml`)

Tests sequential vs parallel output determinism for local copies. No
overlap with DG-5.a which tests internal applier concurrency, not
end-to-end transfer correctness.

### 10.4 Filter Fuzzer (`filter-fuzzer-overnight.yml`)

Nightly schedule, 1-hour soak per target. No overlap with DG-5.a. The
fuzzer uses cargo-fuzz (libFuzzer), not nextest.

## 11. Workflow Diff

The following changes are applied to `.github/workflows/ci.yml`:

### 11.1 Job Rename

```yaml
# Before:
dg3-stress:
  name: DG-3 stress (${{ matrix.os }}, non-required)

# After:
dg-stress:
  name: DG stress (${{ matrix.os }}, non-required)
```

### 11.2 Additional Test Step

After the existing DG-3 stress step, add:

```yaml
# Run the DG-5.a finish_file stress test. Same feature gate, same
# compiled binary. 1000 threads x 10K iterations validates the DG-3
# structural split (SlotData/BarrierState) holds under concurrent
# finish_file pressure.
- name: Run DG-5.a stress (--features dg-stress)
  run: >-
    cargo nextest run --locked -p engine --features dg-stress
    --no-tests=pass
    -E 'test(concurrent_finish_file)'
```

### 11.3 Cache Key Update

```yaml
# Before:
shared-key: ci-dg3-stress-${{ runner.os }}-...

# After:
shared-key: ci-dg-stress-${{ runner.os }}-...
```

The old cache key is abandoned (it will expire naturally). The new key
reflects the broader scope of the job.

## 12. Cost Management

### 12.1 Current Cost

The `dg3-stress` job already runs on all three platforms for every
code-touching PR. Adding DG-5.a adds ~1 minute of runtime per platform
(the test itself, no additional compilation). The marginal cost is
approximately 3 billed minutes per PR (1 Linux + 2 Windows + 10 macOS
at GitHub's billing multipliers, but the DG-5.a step is under 1 minute
on each).

### 12.2 Cost Reduction Levers

If CI cost becomes a concern, in priority order:

1. **Drop macOS.** Saves ~10 billed minutes per PR. macOS is the least
   likely to surface a DG race (its scheduler is more cooperative than
   Windows') and the most expensive. The DG-5.a spec marks macOS as
   "optional" for exactly this reason.

2. **Path-scope the job.** Only run when files under
   `crates/engine/src/concurrent_delta/` or
   `crates/engine/tests/parallel_apply_dg*` change. Saves the full job
   cost on PRs that don't touch the applier.

3. **Move to nightly schedule.** Run on `master` nightly instead of per-PR.
   Catches regressions within 24 hours instead of at PR time. Saves
   all per-PR cost. Use a cron slot that avoids contention with existing
   nightly workflows (e.g., `07 6 * * *` - all existing slots are before
   06:00 UTC).

4. **Reduce thread count.** Drop from 1000 to 200 threads. Weakens the
   stress level but still exercises concurrent `finish_file`. The DG-5.a
   spec's constants are compile-time, so this is a code change, not a CI
   change.

## 13. Implementation Checklist

- [ ] Rename `dg3-stress` job to `dg-stress` in `.github/workflows/ci.yml`
- [ ] Update job display name to `DG stress (${{ matrix.os }}, non-required)`
- [ ] Update cache `shared-key` from `ci-dg3-stress-*` to `ci-dg-stress-*`
- [ ] Add DG-5.a nextest run step after the existing DG-3 stress step
- [ ] Verify the workflow YAML passes `actionlint`
- [ ] Verify the job runs green on all three platforms (after DG-5.a test is implemented)
- [ ] Monitor first 10 runs on `master` for flakes before considering promotion
