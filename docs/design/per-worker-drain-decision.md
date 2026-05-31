# DPC-7: Per-Worker Drain Default-On Decision Framework

Status: decision framework. Consumes DPC-6 bench results to determine
whether `per-worker-drain-channels` becomes the default drain path.

## 1. Context

DPC-5 (PR #2850) implemented per-worker `SegQueue` lanes behind the
`per-worker-drain-channels` Cargo feature flag. The old path uses
`Arc<Mutex<Vec<R>>>` sharded collectors in `drain_parallel`
(`crates/engine/src/concurrent_delta/work_queue/drain.rs:62-90`).

DPC-6 (PR #5277) defined the A/B bench protocol comparing the two
implementations across 5 worker counts (T = 1, 4, 16, 64, 256) and
3 workload profiles (small/large/mixed). This document defines the
decision process once those results are available.

## 2. Decision Inputs

The following DPC-6 bench numbers are required before a decision:

| Cell | Metric needed |
|---|---|
| T=1, all profiles | Throughput (items/sec), A vs B |
| T=4, all profiles | Throughput (items/sec), A vs B |
| T=16, all profiles | Throughput (items/sec), A vs B |
| T=64, small profile | Throughput (items/sec), A vs B |
| T=64, mixed profile | Throughput (items/sec), A vs B |
| T=256, small profile | Throughput (items/sec), A vs B |
| T=256, all profiles | Merge phase fraction (B-path only) |
| All 15 B-cells | Coefficient of variation across 3 runs |

Additionally, the scaling efficiency at T=64 for both paths:

```
scaling_efficiency(T) = throughput(T) / (T * throughput(1))
```

This confirms whether the mutex path's contention cliff is real and
quantifies how close to linear the per-worker path scales.

## 3. Decision Criteria

Sourced from DPC-6 section 9. All conditions must hold simultaneously
for a flip:

### 3.1 Improvement Thresholds

| Condition | Requirement |
|---|---|
| T=64, Small profile | B >= 1.5x A throughput |
| T=64, Mixed profile | B >= 1.3x A throughput |
| T=256, Small profile | B >= 2.0x A throughput |

### 3.2 No-Regression Guard

| Condition | Requirement |
|---|---|
| T=1, all profiles | B >= 0.95x A (no more than 5% regression) |
| T=4, all profiles | B >= 0.95x A (no more than 5% regression) |
| T=16, all profiles | B >= 0.97x A (no more than 3% regression) |

### 3.3 Stability Requirement

All 15 per-worker cells must achieve < 5% coefficient of variation
across 3 consecutive runs. Unstable cells cannot contribute to the
flip decision.

### 3.4 Merge Phase Bound

Merge phase fraction at T=256 must remain below 0.2 for all profiles.
If the sequential lane-drain becomes the new bottleneck, the flip is
blocked until parallel merge is investigated.

## 4. Decision Path A: Flip to Default ON

If all criteria in section 3 are met, the per-worker drain becomes
the production path.

### 4.1 Implementation Changes

1. **Feature flag default**: Add `per-worker-drain-channels` to the
   `default` feature list in `crates/engine/Cargo.toml`:

   ```toml
   [features]
   default = ["per-worker-drain-channels"]
   per-worker-drain-channels = []
   ```

2. **Old path retention**: Keep the mutex path behind
   `#[cfg(not(feature = "per-worker-drain-channels"))]` for one
   release cycle. This enables quick rollback via
   `default-features = false` without a code revert.

3. **Documentation update**: Update `docs/design/per-worker-drain-channels.md`
   status from "design" to "shipped (default)" with a reference to
   the DPC-7 decision and bench numbers.

4. **Rollback runbook update**: Amend `docs/operations/drain-restructure-rollback.md`
   (DPC-4) to reflect that rollback now means disabling a default
   feature rather than reverting a merge.

5. **Bench regression gate**: Add the DPC-6 per-worker numbers as the
   new standing regression threshold. Any future drain change must
   not regress by more than 5% at any worker count.

### 4.2 PR Sequence

| PR | Content |
|---|---|
| DPC-7.a | Flip default, update Cargo.toml |
| DPC-7.b | Update design docs, rollback runbook |
| DPC-7.c (release+1) | Remove old mutex path entirely |

## 5. Decision Path B: Keep Opt-In

If improvement thresholds are not met (T=64 delta below 1.5x for
small or below 1.3x for mixed), the per-worker path remains opt-in.

### 5.1 Documentation Actions

1. Record DPC-6 bench numbers in a results section appended to
   `docs/design/drain-throughput-rebench.md`.
2. Document the gap between observed and required improvement.
3. Add a "When to re-evaluate" section specifying triggers:
   - Rayon pool default exceeds 16 threads (currently capped at
     physical core count; when 32+ core hosts become the common
     deployment target).
   - A new workload profile emerges where drain is > 30% of
     wall-clock time (currently estimated at 8-12%).
   - The merge phase is optimized (parallel merge, batched lane
     drain) reducing per-worker fixed cost.

### 5.2 Feature Flag Retention

The feature flag remains in `crates/engine/Cargo.toml` indefinitely.
Users on high-core-count hosts can opt in via:

```toml
[dependencies]
engine = { path = "../engine", features = ["per-worker-drain-channels"] }
```

Or via CLI:

```bash
cargo build --features per-worker-drain-channels
```

### 5.3 DPC Series Closure

If the improvement is definitively below threshold (< 1.2x at T=64),
close the DPC series. Document the conclusion that mutex sharding is
sufficient for current workloads and the per-worker path is an
optional acceleration for niche high-core deployments.

## 6. Decision Path C: Mixed Results

If some criteria pass but others fail - for example, strong
improvement at T=64 but regression at T=4, or improvement in small
profile but not mixed - a partial flip strategy applies.

### 6.1 Conditional Activation

Enable per-worker drain only in contexts where high concurrency is
expected:

| Context | Activation | Rationale |
|---|---|---|
| Daemon mode (multi-client) | Default ON | Daemon serves multiple simultaneous transfers; rayon pool is sized to hardware cores (typically 16-64). |
| CLI single-transfer | Default OFF | Single transfer on a workstation rarely exceeds T=4 effective parallelism in the drain path. |
| `--parallel-workers >= 16` | Default ON | User explicitly requested high parallelism; per-worker path is beneficial. |

### 6.2 Runtime Dispatch

Replace the compile-time `cfg` gate with a runtime threshold:

```rust
pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>
where
    F: Fn(DeltaWork) -> R + Send + Sync,
    R: Send,
{
    let num_workers = rayon::current_num_threads();
    if num_workers >= PER_WORKER_DRAIN_THRESHOLD {
        self.drain_parallel_per_worker(f)
    } else {
        self.drain_parallel_mutex(f)
    }
}
```

Where `PER_WORKER_DRAIN_THRESHOLD` is set from the bench results
(likely 8 or 16, depending on where the crossover point is).

### 6.3 Trade-offs

- **Pro**: Best-of-both without regression risk at low worker counts.
- **Con**: Two code paths to maintain, harder to reason about
  performance characteristics, branch prediction overhead (negligible
  but present).
- **Constraint**: Runtime dispatch requires both implementations to
  remain compiled, increasing binary size slightly.

## 7. Migration Plan

### 7.1 Phase 1: Bake Period (1 release cycle)

After flipping to default ON:

- Old path remains behind `cfg(not(...))`.
- Interop tests and CI run with default features (per-worker active).
- Monitor for correctness regressions (wire-ordering violations,
  `ReorderBuffer` panics, duplicate/dropped results).
- Track RSS overhead: per-worker lanes allocate `T` SegQueue
  instances upfront. At T=64 with 32-entry segments, baseline
  allocation is 64 * 32 * size_of::<DrainEntry<R>>()`.

### 7.2 Phase 2: Old Path Deprecation (release+1)

- Add `#[deprecated]` attribute to the mutex drain helper (if exposed
  as a named internal function).
- Log a compile-time warning if someone builds with
  `default-features = false` that excludes `per-worker-drain-channels`:

  ```rust
  #[cfg(not(feature = "per-worker-drain-channels"))]
  compile_error!(
      "The mutex-sharded drain path is deprecated. \
       Use default features or explicitly enable `per-worker-drain-channels`."
  );
  ```

  Or softer: a `#[deprecated]` on a helper that prints at build time.

### 7.3 Phase 3: Removal (release+2)

- Delete the `#[cfg(not(feature = "per-worker-drain-channels"))]`
  code path entirely.
- Remove the feature flag from `Cargo.toml` (the per-worker path
  becomes the only path).
- Simplify `drain.rs` to a single unconditional implementation.
- Update DPC-4 rollback runbook to state that rollback now requires
  a git revert of the removal PR.

### 7.4 Timeline

| Milestone | Target |
|---|---|
| DPC-6 bench results available | current sprint |
| DPC-7 decision recorded | same sprint |
| DPC-7.a flip PR (if criteria met) | next sprint |
| Bake period | 1 release cycle (~2 weeks) |
| Old path removal | release after bake |

## 8. Risk Assessment

### 8.1 Correctness Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Wire-ordering violation from lane merge | Low | High (silent data corruption) | `ReorderBuffer` sorts by `ndx`; order of drain output is irrelevant. Existing golden byte tests validate end-to-end wire output. |
| Dropped results (item pushed to lane but not drained) | Very low | High | `SegQueue` is a linked list of segments; `pop` returns `None` only when empty. Merge loop drains until `None`. No window for loss. |
| Duplicate results (item processed twice) | Very low | High | Each `DeltaWork` is moved into exactly one `rayon::scope` task. Ownership transfer prevents aliasing. |
| Non-rayon thread fallback lane collision | Low | Medium (contention, not correctness) | Hashed `ThreadId` distributes across lanes identically to the mutex path's shard selection. Worst case: two non-rayon threads share a lane, serializing their pushes via CAS retry. |

### 8.2 Performance Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Merge phase becomes bottleneck at very high T | Medium at T>=256 | Medium (throughput ceiling) | DPC-6 measures merge fraction. If > 0.2, investigate parallel merge before flipping. |
| Per-worker lane allocation overhead at T=1 | Low | Low (< 5% regression) | Bench validates no regression > 5% at T=1. Single SegQueue allocation is ~64 bytes. |
| Cache pressure from T separate SegQueue heads | Low-Medium at T>=64 | Low-Medium | SegQueue segments are 32 entries; heads are cold between pushes. No worse than T separate `Mutex<Vec>` headers. |
| Memory overhead from pre-allocated lanes | Low | Low | Lanes are empty `SegQueue` instances (one pointer). Segments allocate on first push. Total overhead: T * 8 bytes until first item arrives. |

### 8.3 Operational Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Rollback needed mid-release | Low | Low | Feature flag retained for 1 release. `default-features = false` disables per-worker path immediately. |
| Downstream crates depend on `default-features = false` | Very low | Low | `engine` is an internal crate. No external consumers. |
| Bench results are non-reproducible on different hardware | Medium | Low (decision delayed) | Stability requirement (< 5% CV) catches noisy cells. Re-bench on CI hardware if local results are inconclusive. |

### 8.4 Worst Case: Default-ON at T=1-4

Even if the per-worker path is marginally slower at low worker counts
(within the 5% guard), the impact on real workloads is negligible
because:

1. At T=1-4, drain overhead is < 5% of total transfer wall-clock.
2. A 5% regression on 5% of wall-clock is a 0.25% end-to-end impact.
3. The per-worker path's code is simpler (no mutex, no shard
   indexing) which aids maintainability even at a micro-cost.

The no-regression guard exists to prevent pathological cases (e.g.,
per-worker setup costing 20% at T=1), not to optimize for a regime
where the drain is not the bottleneck.

## 9. Decision Record Template

Once DPC-6 results are available, fill in:

```
## Decision Record

Date: YYYY-MM-DD
Bench host: [hardware description]
Rust toolchain: [version]

### Results Summary

| T | Profile | A (items/sec) | B (items/sec) | Ratio (B/A) | CV |
|---|---|---|---|---|---|
| 1 | small | | | | |
| 1 | large | | | | |
| 1 | mixed | | | | |
| 4 | small | | | | |
| ... | ... | | | | |

### Criteria Evaluation

- [ ] T=64 small >= 1.5x: [actual]x
- [ ] T=64 mixed >= 1.3x: [actual]x
- [ ] T=256 small >= 2.0x: [actual]x
- [ ] T=1 no regression > 5%: [worst profile]
- [ ] T=4 no regression > 5%: [worst profile]
- [ ] T=16 no regression > 3%: [worst profile]
- [ ] All cells CV < 5%: [count stable / 15]
- [ ] Merge fraction < 0.2 at T=256: [actual]

### Decision

[ ] Path A: Flip to default ON
[ ] Path B: Keep opt-in
[ ] Path C: Mixed results - conditional activation at T >= [threshold]

### Rationale

[1-3 sentences explaining the decision based on data above]
```

## 10. Cross-References

- DPC-1 (#2846) - Contention audit.
- DPC-2 (#2847) - Baseline bench harness.
- DPC-3 (#2848) - Per-worker drain design (`docs/design/per-worker-drain-channels.md`).
- DPC-4 (#2849) - Rollback runbook (`docs/operations/drain-restructure-rollback.md`).
- DPC-5 (#2850) - Implementation spec (`docs/design/dpc-5-per-worker-drain-impl.md`).
- DPC-6 (#5277) - Rebench protocol (`docs/design/drain-throughput-rebench.md`).
- `crates/engine/src/concurrent_delta/work_queue/drain.rs` - Production drain.
- `crates/engine/benches/drain_parallel_throughput.rs` - Bench binary.
