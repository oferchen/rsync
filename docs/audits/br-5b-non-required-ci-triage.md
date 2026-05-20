# BR-5b: Non-Required CI Workflow Failure Triage

Read-only triage of GitHub Actions workflows that are not part of the required
merge-gating set. Required checks remain green on `master` (`fmt + clippy`,
`nextest (stable)`, `Windows (stable)`, `macOS (stable)`, `Linux musl (stable)`,
`interop`). This document covers every other workflow with at least one
recorded run in the last 30 days, the failure pattern, the category, and a
recommended next action.

Data source: `gh run list --workflow=<id>` and `gh run view`/`gh run view
--log-failed` for the failing jobs, captured 2026-05-20.

## Summary

| Category | Count | Workflows |
|----------|-------|-----------|
| (a) Genuine regression worth fixing now | 4 | Release-cross, Known Failures Dashboard, Filter Fuzzer Overnight, Fuzz Coverage Smoke |
| (b) Known issue / documented limitation | 2 | Benchmark (post-tag), CI non-required Windows cells |
| (c) Flaky / non-deterministic | 1 | Feature flag combinations (Windows matrix rows) |
| (d) Informational-only by design | 4 | Fuzz Coverage Report, Coverage, Criterion Benchmarks, unsafe-safety-comment-audit (job within CI) |
| (e) Infra-only failure | 1 | Dependabot Updates |
| Healthy (no action) | 12 | CodeQL, pages-build-deployment, Interop Validation, Label PRs, Parallel Determinism, Security Audit, MSRV Check, Dependency Review, Upstream Release Watcher, CI skip interop, ci-skip (CI), reusable `_*.yml` workflows |

Reusable workflows (`_interop.yml`, `_interop-macos.yml`, `_interop-windows.yml`,
`_test-features.yml`, `_ci-skip-interop.yml`, `_benchmark-windows.yml`) are
listed by GitHub but never run standalone; their conclusions roll up into the
caller (CI, ci-skip, benchmark). They are not separately triageable.

## Per-Workflow Triage

### Release-cross (`release-cross.yml`)

- Workflow id: 219298606
- Latest run: 25960148806 (push, tag `v0.6.2`, 2026-05-16, failure)
- Last 3 results: failure, failure, failure (every tag push since 2026-02-22)
- Failure category: (a) genuine regression worth fixing now
- Root cause: `libc::statx` is not defined in `libc 0.2.186` for any of the
  `linux-musl-{x86_64,aarch64}-{stable,beta,nightly}` targets used by `cross`.
  All six musl matrix legs fail at the `Build static musl binary (pure Rust)`
  step with `cannot find type 'statx' in crate 'libc'` from
  `crates/fast_io/src/io_uring/statx.rs:464,468`. The Windows legs of the
  same matrix pass.
- Recommendation: open a follow-up issue. The fix is to feature-gate the
  `libc::statx` reference under `#[cfg(not(target_env = "musl"))]` or to use
  the in-crate `statx` shim already defined in
  `io_uring::statx::types::statx` (the compiler's own help text suggests
  this). Until fixed, every tag-driven release will publish without musl
  artifacts.

### Known Failures Dashboard (`known-failures.yml`)

- Workflow id: 249593328
- Latest run: 26027303576 (schedule, 2026-05-18, failure)
- Last 3 results: failure, failure, failure (all weekly Monday runs)
- Failure category: (a) genuine regression worth fixing now
- Root cause: the workflow sets `RUSTFLAGS: "-D warnings"` for the
  `Build oc-rsync` step. Master currently has `dead_code` warnings (e.g.
  `BasisReader::Slurped` in `crates/engine/src/concurrent_delta/strategy.rs`
  and `has_bandwidth_limiter` in
  `crates/engine/src/local_copy/context_impl/state.rs`). The lints fire
  outside CI because the main CI build invokes `cargo clippy` with the
  workspace's clippy config, while this workflow runs `cargo build --release`
  with the more aggressive `-D warnings` env override and no `--all-features`.
  The variants/methods are reachable only under feature gates the workflow
  does not enable, so the warning is real for this specific build.
- Recommendation: drop `RUSTFLAGS: "-D warnings"` from this workflow (it is
  a status-tracking dashboard, not a lint gate), or pass `--all-features` to
  the build so the cfg-gated items are seen as used. File a follow-up.

### Filter Fuzzer Overnight (`filter-fuzzer-overnight.yml`)

- Workflow id: 278411551
- Latest run: 26145017202 (schedule, 2026-05-20, failure)
- Last 3 results: failure, failure, failure (every nightly since workflow was
  added 2026-05-18)
- Failure category: (a) genuine regression worth fixing now (never green)
- Root cause: `cargo +nightly fuzz run` defaults to building against
  `x86_64-unknown-linux-musl` and asks rustc to link with `-Zsanitizer=address`.
  rustc rejects this with `sanitizer is incompatible with statically linked
  libc, disable it using '-C target-feature=-crt-static'` followed by
  `E0463: can't find crate for 'core'` because the musl std is not preinstalled
  on `ubuntu-latest`. Both `filter_differential` and
  `filter_rules_vs_upstream` targets fail identically.
- Recommendation: open a follow-up. Either preinstall the musl std
  (`rustup target add x86_64-unknown-linux-musl` + `apt install musl-tools`)
  or pin the fuzz target to `--target x86_64-unknown-linux-gnu` in
  `tools/ci/run_filter_fuzz.sh` and `tools/ci/run_filter_differential_fuzz.sh`.
  Until fixed there is zero overnight fuzz coverage.

### Fuzz Coverage Smoke (`fuzz-coverage.yml`)

- Workflow id: 278473603
- Latest run: 26020745534 (schedule, 2026-05-18, failure)
- Last 3 results: failure (only one run on record; weekly cron)
- Failure category: (a) genuine regression worth fixing now (never green)
- Root cause: cascading. The smoke-run step fails with
  `The corpus does not contain program-input files`, then the coverage step
  fails with `no such command: 'llvm-cov'`. The cargo-fuzz `-runs=0` flag
  asks the engine to run 0 inputs over a 60s budget; the engine then has no
  corpus to compute coverage from. The `cargo cov` invocation hits the
  unrelated `llvm-cov` missing-command error because
  `llvm-tools-preview` is installed for the nightly toolchain but
  `cargo +nightly cov` is invoked via the stable cargo proxy that does not
  see the nightly component.
- Recommendation: open a follow-up. Use `cargo +nightly cov` consistently
  with `cargo +nightly fuzz coverage`, drop the `-runs=0` flag so the engine
  generates seed inputs, or seed the corpus before running. Until fixed,
  the smoke run gives no signal.

### Benchmark (`benchmark.yml`)

- Workflow id: 226250313
- Latest run: 25960148803 (push, tag `v0.6.2`, 2026-05-16, failure)
- Last 3 results: failure (tag v0.6.2), failure (2026-05-03), failure
  (2026-05-03)
- Failure category: (b) known issue / documented limitation
  - Latest failure: `FileNotFoundError: target/interop/upstream-src/rsync-3.4.1/rsync`
    because the workflow expects an upstream binary that the cached path no
    longer ships. Per CLAUDE.md "Benchmark appends to release body" already
    notes the workflow's brittleness around release tags.
  - 2026-05-03 failures: build step `Build oc-rsync with embedded-ssh
    (release)` failed because `embedded-ssh` feature was renamed; that
    specific failure mode is no longer reproducible on master.
- Recommendation: file a follow-up to make the benchmark robust to upstream
  version pin drift (probe both `rsync-3.4.1` and `rsync-3.4.2` paths, build
  if absent). Low priority: only fires on tag pushes and the benchmark is
  informational - the release artifact uploads still succeed via separate
  workflows.

### CI (`ci.yml`) - non-required cells

- Workflow id: 201634757
- Latest run: 26157641444 (push, master, 2026-05-20, success)
- Last 5 master push results: success, cancelled, cancelled, success, failure
- Failure category: (b) known issue / documented limitation (Windows
  best-effort cells)
- The required jobs in this workflow (`fmt + clippy`, `nextest (stable)`,
  `Windows (stable)`, `macOS (stable)`, `Linux musl (stable)`, `interop`) are
  green. The aggregate workflow conclusion sometimes flips to `failure` when
  one of the *non-required* matrix rows fails:
  - `interop (Windows, best-effort) / interop with upstream rsync (Windows,
    best-effort)` - documented as best-effort by the reusable workflow's own
    comment block; tagged for promotion to required only after baseline parity
    is green.
  - `unsafe-safety-comment-audit (informational)` - has
    `continue-on-error: true`, does not contribute to the run conclusion.
- Recommendation: ignore / mark expected. The reusable
  `_interop-windows.yml` already runs under `continue-on-error: true` at its
  step level; the rolled-up workflow conclusion still surfaces the failure to
  the GitHub UI because the matrix job exit code is non-zero. If the noisy
  red runs are a sign-off blocker, the cleanest fix is to gate the workflow
  conclusion using a `needs:` aggregator with `if: always()` and ignore the
  best-effort rows there.

### Feature flag combinations (Windows matrix rows via `_test-features.yml`)

- Reusable workflow callee inside CI; no standalone runs.
- Recent observed failures on master: `concurrent-sessions (windows-latest)`,
  `tracing (windows-latest)`, `async (windows-latest)`, `serde
  (windows-latest)` in run 26144923567 (2026-05-20). The same matrix rows
  passed in the more recent run 26157641444.
- Failure category: (c) flaky / non-deterministic. The log shows a Rust
  test process panicking with a Windows backtrace
  (`std::sys::pal::windows::thread::impl$0::new::thread_start`) and
  `106/1114 tests were not run due to test failure`. This is the
  panicking-thread-on-Windows pattern flagged in CLAUDE.md
  "Cross-Platform Compilation" notes.
- Recommendation: file a follow-up to add a retry shim (e.g.
  `nextest --retries 2` on Windows) and to capture the panicking test name
  via `--message-format=json`. Until then the workflow self-heals on rerun.

### Dependabot Updates (`dependabot-updates`, system workflow)

- Workflow id: 265589481
- Last 3 results: failure, failure, failure (2026-05-11, 2026-05-18,
  2026-05-18); 17 prior successes.
- Failure category: (e) infra-only failure.
- Root cause: Dependabot's update engine reports
  `error: no such commit 4be9e76fd7c4901c61fb841f559994984270fce7` for
  `dtolnay/rust-toolchain`. The pinned commit no longer exists in the
  upstream action repository (likely force-pushed). Dependabot then fails
  to compute the latest version for the actions group update.
- Recommendation: action now (workflow-only repin), or wait until Dependabot
  reschedules. Repin every `dtolnay/rust-toolchain@4be9e76fd7c4901c61fb841f559994984270fce7`
  reference (used in `ci.yml`, `coverage.yml`, `filter-fuzzer-overnight.yml`,
  `fuzz-coverage.yml`, `fuzz-coverage-report.yml`, `known-failures.yml`,
  `interop-validation.yml`, `parallel_determinism.yml`, `release-cross.yml`,
  `msrv.yml`, `security.yml`) to a currently published commit SHA of
  `dtolnay/rust-toolchain`. This is out of scope for this audit (workflow
  change) and intentionally not opened as a fix PR alongside.

### Fuzz Coverage Report (`fuzz-coverage-report.yml`)

- Workflow id: 278453065
- Last 3 results: success, success, success.
- Failure category: (d) informational-only by design. Marked
  `continue-on-error: true` per CLAUDE.md known-good list.
- Recommendation: no action.

### Coverage (`coverage.yml`)

- Workflow id: 250331833
- Last 20 runs: 8 success, 4 cancelled (superseded by newer pushes),
  8 skipped (PRs without `coverage-required` label).
- Failure category: (d) informational-only by design. No failures; skips
  are intended via `if: contains(... 'coverage-required')`.
- Recommendation: no action.

### Criterion Benchmarks (`benchmark-release.yml`)

- Workflow id: 249586957
- Last 12 results: cancelled x9, failure x3 (most recent failure
  2026-05-03).
- Failure category: (d) informational-only by design (post-release
  perf collection). The cancellations are concurrency cancellations during
  rapid pushes.
- Most recent failure root cause: `Run criterion benchmarks` step ran 28
  benches as `ignored` (none executed); job timeout/non-zero exit from an
  empty benchmark set. No regression observed on master since 2026-05-03.
- Recommendation: ignore. Reassess if the next scheduled run also fails.

### unsafe-safety-comment-audit (job in `ci.yml`)

- Failure category: (d) informational-only by design.
  `continue-on-error: true` at the job level; the job itself reports
  remaining unsafe-block SAFETY-comment debt that is being burned down per
  `docs/audits/unsafe-safety-comment-audit.md`.
- Recommendation: no action.

### Healthy Workflows (last 30 days, no failures observed on master)

| Workflow | Id | Path | Notes |
|----------|----|----|-------|
| CodeQL | 203267279 | dynamic/github-code-scanning/codeql | All 17 runs successful (3 cancelled by concurrency). |
| pages-build-deployment | 207441846 | dynamic/pages/pages-build-deployment | All deploys successful. |
| Interop Validation | 212402121 | `.github/workflows/interop-validation.yml` | 17 success, 3 concurrency cancellations. |
| Label PRs | 236342196 | `.github/workflows/labeler.yml` | 20/20 success. |
| Parallel Determinism | 263819462 | `.github/workflows/parallel_determinism.yml` | 18/20 success, 2 cancelled. |
| Security Audit | 265582364 | `.github/workflows/security.yml` | 20/20 success. |
| MSRV Check | 265582722 | `.github/workflows/msrv.yml` | 18/20 success, 2 cancelled. |
| Dependency Review | 265582967 | `.github/workflows/dependency-review.yml` | 20/20 success. |
| Upstream Release Watcher | 277291498 | `.github/workflows/upstream-release-watch.yml` | 1 run, success. |
| ci-skip ("CI" duplicate, `ci-skip.yml`) | 236291214 | `.github/workflows/ci-skip.yml` | Path-skip variant; 18 success, 2 cancelled. |
| Reusable callees (`_interop.yml`, `_test-features.yml`, `_ci-skip-interop.yml`, `_interop-macos.yml`, `_interop-windows.yml`, `_benchmark-windows.yml`) | n/a | `.github/workflows/_*.yml` | Triggered only via `workflow_call`; results roll up into their callers. |

## Top 3 Genuine Regression Candidates

1. **Release-cross musl** - blocks every tagged release from producing musl
   binaries since 2026-02-22. Fix path is small (feature-gate the
   `libc::statx` reference; the in-crate shim is already there).
2. **Known Failures Dashboard** - weekly dashboard has not produced data
   since being added because `RUSTFLAGS=-D warnings` rejects dead-code
   warnings that the main CI doesn't see (different feature set). Either
   drop the env override or build with `--all-features`.
3. **Filter Fuzzer Overnight** + **Fuzz Coverage Smoke** - both new fuzz
   schedules have never produced a green run. The overnight differential
   fuzzer is the canonical regression catcher referenced in
   `docs/process/filter-fuzzer-24h-cumulative.md`; running zero fuzz hours
   per night silently. Common fix: pin the fuzz build to
   `--target x86_64-unknown-linux-gnu` (avoids the musl sanitizer
   incompatibility) and seed the corpus before requesting coverage.

## Open Questions

- The Dependabot pinned-commit failure for `dtolnay/rust-toolchain` will
  recur every dependabot run until the pin is refreshed across all
  workflow files. Out of scope for this audit (workflow-only change) and
  unclear whether the maintainer wants Dependabot's batched PR or a manual
  repin first.
- Several "CI" runs on master are marked `failure` while the required jobs
  are green; this confuses the at-a-glance branch health. Consider an
  aggregator job that explicitly OK's the non-required cells (or moving
  `_interop-windows.yml` and the Windows feature-flag rows out of the
  required CI workflow and into a sibling workflow whose red status is not
  visually conflated with the gating set).
