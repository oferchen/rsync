# CI workflow audit - hazards and quick wins

Scope: every file under `.github/workflows/*.yml` (22 workflows). Findings
focus on four hazard classes:

1. Deprecated or out-of-date third-party actions.
2. Missing `Swatinem/rust-cache` (or equivalent) on cargo-building jobs.
3. Long-running serial steps that could parallelise (matrix split, nextest
   partitioning, or job fan-out).
4. Missing matrix `fail-fast` settings and missing explicit job-level
   `permissions:` blocks.

This audit is informational. No workflow files are modified by this PR.

## Action version baseline (2026-05)

Pin baseline used to grade "current" vs "stale":

| Action | Latest stable | Repo pin | Status |
|---|---|---|---|
| `actions/checkout` | v6 | v6.0.2 (SHA-pinned) | current |
| `actions/upload-artifact` | v7 | v7.0.1 (SHA-pinned) | current |
| `actions/download-artifact` | v8 | v8.0.1 (SHA-pinned) | current |
| `actions/cache` | v5 | v5.0.5 (SHA-pinned) | current |
| `actions/github-script` | v9 | v9 (unpinned) | current major, pin missing |
| `actions/dependency-review-action` | v4 | v4 (unpinned) | current major, pin missing |
| `Swatinem/rust-cache` | v2 | v2 (SHA-pinned) | current |
| `dtolnay/rust-toolchain` | rolling | SHA-pinned to `stable` | current |
| `taiki-e/install-action` | v2 | v2 (SHA-pinned) | current |
| `EmbarkStudios/cargo-deny-action` | v2.0.17 | v2.0.17 (SHA-pinned) | current |
| `awalsh128/cache-apt-pkgs-action` | v1.6.0 | v1.6.0 (SHA-pinned) | current |
| `msys2/setup-msys2` | v2.27.0 | v2.27.0 (SHA-pinned) | current |
| `docker/setup-qemu-action` | v4 | v4.0.0 (SHA-pinned) | current |
| `docker/setup-buildx-action` | v4 | v4.0.0 (SHA-pinned) | current |
| `docker/login-action` | v4 | v4.1.0 (SHA-pinned) | current |
| `docker/build-push-action` | v8 | v7.1.0 (SHA-pinned) | one major behind |
| `softprops/action-gh-release` | v2 | v2 (SHA-pinned) | current |

There are no `actions/checkout@v3`, `Swatinem/rust-cache@v1`, or
`actions/upload-artifact@v3` references in any workflow.

---

## Per-workflow findings

### `ci.yml` - main CI pipeline (10 jobs)

Severity: low-medium. Largest workflow, generally well-structured.

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | `lint` runs `cargo fmt --check` and `cargo clippy` serially in one job. Clippy dominates wall time (~3-4 min); fmt is < 5 s. | low | Optional split into `fmt` + `clippy` jobs sharing the same cache key. Saves nothing if fmt always passes; saves ~3 min of cancel-time when fmt fails. Not a quick win. |
| 2 | `test` matrix runs `cargo build --workspace --all-features` then `cargo nextest run --workspace --all-features` as one serial step per toolchain. nextest builds test binaries itself; the prior `cargo build` rebuilds the workspace twice on a cold cache. | medium | Drop the standalone `cargo build` step; nextest's compile already covers it. Saves ~3-5 min cold-cache per toolchain (x3 toolchains = ~10-15 min CI minutes per push). |
| 3 | `test` is a single `cargo nextest run --workspace`. Project uses ~2k tests; could be partitioned via `--partition`. | medium | Run nextest with `--partition count:1/N` across a 2- or 4-way shard matrix. Already supported by nextest; needs matrix change in workflow only. Estimated ~30-40% wall time reduction on the stable row. |
| 4 | `windows-test` and `macos-test` each `cargo build --workspace --all-features` then `cargo nextest run -p ...` only a few crates. The build step compiles crates that the test step never touches. | medium | Replace `cargo build --workspace` with the same `-p` list used in the test step. Saves ~5-8 min cold-cache per Windows toolchain row. |
| 5 | `windows-iocp` does three sequential `cargo build` invocations and two `nextest run` invocations. Each cargo invocation pays a fresh linker pass. | low | Combine builds into a single `cargo build --tests` to share intermediate artefacts where possible. Saves ~1-2 min per run. |
| 6 | `linux-musl` rebuilds the full workspace from scratch for the test step (different feature set than the GNU `test` job, so cache hit rate is partial). | low | Already correctly isolated; flag only as a reminder that musl cache is large and should not be combined with the GNU cache. |
| 7 | All non-matrix jobs declare `permissions: contents: read` at the workflow scope. Good - already explicit. | n/a | None. |
| 8 | All matrices use `fail-fast: false`. Good. | n/a | None. |
| 9 | `lint` job blocks every other job via `needs: lint`. If lint takes 5 min, all 9 OS/feature jobs wait 5 min before they start. | medium | Consider moving the lint dependency off the long-pole jobs (`windows-test`, `macos-test`, `linux-musl`) and letting them race lint. Tradeoff: more CI minutes on broken PRs. Quantify before applying. |

### `_test-features.yml` (reusable from `ci.yml`)

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Matrix has `fail-fast: false` and explicit `permissions: contents: read`. Good. | n/a | None. |
| 2 | Each row runs `cargo build` then `cargo nextest run`. Same redundant-build pattern as the main `test` job. | medium | Drop the standalone `cargo build` step; nextest compiles test binaries. Saves ~1-2 min per matrix row across ~12 rows (24+ CI minutes saved per push). |
| 3 | The `feature-flags-cross-os` matrix runs the same 4 feature rows on 3 OSes (12 cells), each with cold-cache potential. `rust-cache` `shared-key` is per-OS but `key` differentiates by row - good. | n/a | Already optimal for current scope. |

### `_interop.yml` (reusable from `ci.yml`)

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | The job runs Linux interop, builds upstream rsync (or restores cache), runs the SSH interop sub-test block. Total wall time ~15-25 min. The SSH-loopback subtests run sequentially after `tools/ci/run_interop.sh`. | medium | Split SSH sub-tests into a second job that runs in parallel with the main interop run. Both can share the upstream-rsync cache restore. Saves ~3-5 min wall on a clean run. |
| 2 | Permissions explicit; uses cache for upstream rsync source + install. Good. | n/a | None. |

### `_interop-macos.yml` and `_interop-windows.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | macOS builds release with `--bin oc-rsync` only - already minimal. | n/a | None. |
| 2 | Windows builds release with `--bin oc-rsync` only - already minimal. | n/a | None. |
| 3 | Both correctly use cache and explicit `permissions: contents: read`. | n/a | None. |

### `_benchmark-windows.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | `continue-on-error: true` and `runs-on: windows-latest`. Already documented as a soft signal. | n/a | None. |
| 2 | Uses `Swatinem/rust-cache`, explicit permissions. Good. | n/a | None. |

### `benchmark.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Three sequential `cargo build --release` invocations (default, openssl, embedded-ssh). Each is a fresh link of the workspace. | medium-high | Combine into one matrix or run with a single multi-feature build where the feature sets allow. Saves ~5-10 min wall on a cold run (only runs on tags, so impact is bounded). |
| 2 | `actions/cache@v5` used directly for upstream-rsync source; good fallback ordering. | n/a | None. |
| 3 | Job has `timeout-minutes: 90` and runs `python3 .github/scripts/benchmark.py` serially. Already documented. | n/a | None. |
| 4 | `permissions: contents: write, pull-requests: write` at workflow scope - needed for release-notes edit. Justified. | n/a | None. |

### `benchmark-release.yml` (Criterion)

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | `cargo bench --workspace` is a single 60-90 min serial run. The 7 bench crates could fan out across a matrix of 7 jobs and complete in 15-30 min wall time. | high | Convert to a matrix with one cell per bench crate. Each cell runs `cargo bench -p <crate>`. Massive wall-time reduction (~60 min saved on tag pushes). |
| 2 | Uses `rust-cache` and explicit permissions. Good. | n/a | None. |

### `coverage.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Single 45-min job runs `cargo llvm-cov nextest --workspace --all-features`. nextest partitioning is not used. | medium | Coverage tools like `cargo-llvm-cov` support `--no-report` per partition with a final `--no-run --report` merge step. Worth investigating; saves ~30-50% wall time on a cold run. |
| 2 | All upload-artifact steps gate on `if: always() && steps.coverage.outcome != 'skipped'`. Good. | n/a | None. |
| 3 | `continue-on-error: true` on the coverage step is deliberate (threshold is informational). | n/a | None. |

### `interop-validation.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Seven near-identical jobs (`validate-exit-codes`, `validate-messages`, `validate-behavior`, `validate-batch`, `validate-filters`, `validate-compress`, `validate-inc-recurse`, plus the manual `regenerate-goldens`). Each duplicates: checkout, install Rust, rust-cache, cache APT packages, cache upstream rsync, build upstream rsync, build oc-rsync, then runs the specific xtask/script. | high | Refactor into a reusable workflow with a job matrix over the validation kind. A single shared `build-oc-rsync` job that uploads the binary as an artifact would eliminate 6x duplicate `cargo build --release` invocations. Estimated CI savings: ~6 * 5 min = 30 min wall + ~30 min CI minutes per push that touches relevant paths. |
| 2 | Each job re-runs `bash tools/ci/run_interop.sh build-only` only on cache miss - already optimal. | n/a | None. |
| 3 | `permissions: contents: read` at workflow scope. Good. | n/a | None. |
| 4 | All jobs lack `fail-fast` semantics because they are independent jobs (not a matrix). Correct. | n/a | None. |

### `filter-fuzzer-overnight.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Installs stable AND nightly Rust toolchains. The stable toolchain is only used by upstream `build-only`; nightly is the actual cargo-fuzz runner. Could possibly drop stable install (the install action handles either). | low | Verify with one run that dropping the stable install does not break `cargo +stable` callers, then remove. ~30 s saved per run. |
| 2 | `cargo +nightly install cargo-fuzz --locked` reinstalls every run. The `taiki-e/install-action` can install `cargo-fuzz` from prebuilt binary in seconds. | medium | Replace `cargo install` with `taiki-e/install-action`. Saves ~3-5 min per matrix row (x2 rows = 6-10 min). |
| 3 | Matrix `fail-fast: false` and permissions explicit. Good. | n/a | None. |

### `fuzz-coverage-report.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Same `cargo install cargo-fuzz --locked` pattern as above. Runs across 17 matrix rows. Each install eats ~3-5 min. | high | Switch to `taiki-e/install-action`. Saves an estimated 50-80 min of aggregate CI minutes per nightly run. |
| 2 | Installs both stable and nightly toolchains; only nightly is invoked. Likely safe to drop stable. | low | Same as fuzzer-overnight item 1. |
| 3 | `fail-fast: false`, explicit permissions, `continue-on-error: true`. Good. | n/a | None. |
| 4 | 17 cells differ only by `workspace`/`target`/`needs_upstream`. Cache scope is per-cell which is correct. | n/a | None. |

### `known-failures.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Permissions explicit, cache present, weekly schedule. | n/a | None. |
| 2 | Sequential build of upstream rsync (cache miss) then oc-rsync, then runs the check. Standard pattern. | n/a | None. |

### `dependency-review.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | `actions/dependency-review-action@v4` floating major tag instead of SHA. | low | Pin to SHA for supply-chain consistency with the rest of the repo. |
| 2 | Permissions explicit (`contents: read`, `pull-requests: write`). | n/a | None. |

### `labeler.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | `actions/github-script@v9` floating major tag instead of SHA pin. | low | Pin to SHA. |
| 2 | Permissions explicit (`pull-requests: write`). | n/a | None. |
| 3 | No cargo build, no Rust toolchain, runs in seconds. | n/a | None. |

### `msrv.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Single 20-min `cargo check --locked --workspace --all-features` on Rust 1.88. Cached. | n/a | None. |
| 2 | Explicit permissions, `cancel-in-progress`. Good. | n/a | None. |

### `parallel_determinism.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Uses floating tags throughout: `actions/checkout@v6.0.2` (version not SHA), `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `actions/upload-artifact@v7`. Inconsistent with every other workflow in the repo. | medium | Convert to SHA pins, matching the project's supply-chain hardening posture. Quick win. |
| 2 | No explicit `permissions` block. Implicit token has read access on PRs. | low | Add `permissions: contents: read`. Quick win. |
| 3 | Six sequential test steps each invoke `./target/release/oc-rsync` against different fixtures. Could be parallelised across two jobs (one sequential-mode, one parallel-mode) sharing a build artifact. | low-medium | Job split saves ~1-2 min wall; build artifact share saves another 2-3 min on the second job. |

### `release-cross.yml` - release pipeline

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | All matrices have `fail-fast: false`. Permissions explicit (`contents: write`, `pull-requests: write`, `packages: write`). Good. | n/a | None. |
| 2 | `linux-musl` job runs two `cross build` invocations sequentially (pure-Rust + OpenSSL-vendored). The second rebuild relinks the workspace. | medium | Investigate whether the two builds can share intermediates via a single `cargo build --features ...` invocation that emits both binaries. Likely saves ~5-10 min per row (x6 rows = 30-60 min). |
| 3 | `docker/build-push-action@v7.1.0` is one major version behind (v8 is current). | low | Bump to v8.x in a separate PR after smoke-testing the build push step. |
| 4 | `validate-version` is a fail-fast gate before any platform builds. Good. | n/a | None. |
| 5 | All `actions/*` references SHA-pinned. Good. | n/a | None. |

### `security.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Four sequential `cargo-deny check` invocations (advisories, licenses, bans, sources). Each invocation re-parses lockfile and Cargo.toml. | medium | Replace with a single `command: check all` (or just `check`) which runs all four in one process. Saves ~1-2 min per run, ~30 s per check. Quick win. |
| 2 | Permissions explicit (`contents: read`). | n/a | None. |
| 3 | All SHA pinned. Good. | n/a | None. |

### `upstream-release-watch.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Single 10-min job, no cargo build, no Rust toolchain. | n/a | None. |
| 2 | Permissions explicit (`contents: read`, `issues: write`). Good. | n/a | None. |

### `ci-skip.yml` and `_ci-skip-interop.yml`

| # | Finding | Severity | Recommended fix |
|---|---|---|---|
| 1 | Stub jobs that exist to satisfy branch protection required-check names when only docs change. No actions used beyond echo. | n/a | None. |
| 2 | No `permissions` declared. Acceptable since they perform no API calls. | low | Optional: add `permissions: {}` to drop the default token. |

---

## Quick wins

Changes that take under five minutes to apply and yield over 30 s of CI
wall time savings.

1. **`security.yml`: collapse four `cargo-deny` invocations into one**
   (`check all` or `check`). Single edit, saves ~30 s to 2 min per run.
   Runs on every PR that touches `Cargo.toml`/`Cargo.lock` and on the
   weekly schedule.

2. **`filter-fuzzer-overnight.yml` and `fuzz-coverage-report.yml`:
   replace `cargo install cargo-fuzz --locked` with
   `taiki-e/install-action@v2` (prebuilt binary).** Saves ~3-5 min per
   matrix row. `fuzz-coverage-report.yml` has 17 rows, so the aggregate
   saving is ~50-80 CI minutes per nightly run.

3. **`ci.yml` `test`, `windows-test`, `macos-test`, and
   `_test-features.yml`: drop the redundant `cargo build` step before
   `cargo nextest run`.** nextest already compiles test binaries; the
   separate build re-links the workspace with the non-test target set.
   Saves ~3-5 min cold-cache per job. Aggregate across ~10+ jobs per
   push: ~30+ CI minutes saved per push.

4. **`parallel_determinism.yml`: add `permissions: contents: read` and
   convert floating action tags to SHA pins** (one-line edits matching
   the pattern used elsewhere in the repo).

5. **`dependency-review.yml` and `labeler.yml`: pin floating major tags
   (`@v4`, `@v9`) to SHA** for supply-chain consistency with the rest
   of the workflows.

## Larger structural wins (not quick, but high payoff)

These are flagged for separate tracking:

1. **`interop-validation.yml`: refactor seven near-duplicate jobs into a
   reusable workflow with a build-once / validate-many topology.**
   Estimated savings ~30 min CI wall + ~30 min CI minutes per push.

2. **`benchmark-release.yml`: matrix the 7 bench crates across 7 jobs.**
   Cuts wall time from 60-90 min to 15-30 min on tag pushes.

3. **`ci.yml` `test`: shard nextest with `--partition count:1/N` across
   a 2- or 4-way matrix.** Cuts stable-row wall time by 30-40%.

4. **`release-cross.yml` `linux-musl`: investigate single-invocation
   pure+openssl build** to avoid the two sequential `cross build` calls.

5. **`coverage.yml`: explore `cargo llvm-cov` partition support** so
   coverage can fan out the same way nextest does.

## Severity legend

- **high**: > 10 min CI wall savings or supply-chain risk.
- **medium**: 1-10 min wall savings or correctness/consistency issue.
- **low**: < 1 min wall savings or stylistic-only.
- **n/a**: informational, no action required.
