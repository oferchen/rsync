# BR-4a: Workspace `cargo llvm-cov` Baseline (2026-05-20)

Read-only measurement of workspace line- and function-coverage produced by the
`Coverage` GitHub Actions workflow. Captures the beta-readiness baseline against
the project target of **>= 95% line coverage** (declared in
`.github/workflows/coverage.yml` header and step summary). The current
informational gate enforced in CI is 84% line coverage; this audit records
where each crate sits between that gate and the 95% target so that BR-4b can
attack the largest gaps first.

## Data source

- Workflow: `Coverage` (`.github/workflows/coverage.yml`).
- Run: [26166532067](https://github.com/oferchen/rsync/actions/runs/26166532067),
  push to `master`, head SHA `cd70daedc12fa51e70988a71e8b67faf0ff1e9f4`,
  conclusion `success`.
- Command: `cargo llvm-cov nextest --branch --workspace --all-features --lcov
  --output-path lcov.info --fail-under-lines 84`.
- Runner: `ubuntu-24.04`, nightly toolchain pinned to `nightly-2026-04-20`
  (required for `--branch`).
- Test run: nextest profile `default`, **27 958 tests passed**, 137 skipped,
  8 slow, in 610.4 s.
- LCOV artifact (12.6 MB) downloaded from the run and parsed locally.

## Reproducing this baseline

Re-running locally is heavy (full workspace compile + 25+ crate test suites,
several integration suites use real-time sleeps and take minutes). The
authoritative source is the workflow above. To re-run the same command locally:

```sh
cargo llvm-cov nextest --branch --workspace --all-features \
    --lcov --output-path lcov.info --fail-under-lines 84
```

For a faster JSON export without nextest (matches the prompt brief):

```sh
cargo llvm-cov --workspace --all-features --no-cfg-coverage \
    --ignore-filename-regex 'tests/|benches/|examples/|build\.rs|fuzz/' \
    --json --output-path coverage.json
```

## Workspace totals

| Metric | Covered / Total | Percent |
|--------|----------------:|--------:|
| Lines | 145 055 / 169 620 | **85.52%** |
| Functions | 18 752 / 20 957 | 89.48% |
| Files measured | 1 185 (after ignore regex) | - |

Branch coverage is enabled in the workflow (`--branch`), but the
`lcov-branch.info` artifact only contains the three top-level `src/bin/*` files
(the report step is run without `--workspace`). Branch coverage is therefore
omitted here; raise as a follow-up if needed.

Distance to project target: **9.48 percentage points**, or **16 058 additional
lines** must be covered to reach 95.00% at the current line total.

## Per-crate coverage

Sorted ascending by line %. Status `OK` if line % >= 95.00, else `WARN`.

| Crate | Lines covered / total | Line % | Func % | Status |
|-------|----------------------:|------:|------:|:------:|
| test-support | 5 / 11 | 45.45% | 100.00% | WARN |
| xtask | 5 075 / 9 403 | 53.97% | 54.40% | WARN |
| batch | 1 445 / 1 955 | 73.91% | 65.91% | WARN |
| checksums | 5 735 / 7 653 | 74.94% | 88.81% | WARN |
| transfer | 11 528 / 15 362 | 75.04% | 86.46% | WARN |
| daemon | 9 208 / 11 850 | 77.70% | 81.45% | WARN |
| fast_io | 7 667 / 9 328 | 82.19% | 80.88% | WARN |
| core | 14 300 / 16 637 | 85.95% | 89.47% | WARN |
| metadata | 3 917 / 4 555 | 85.99% | 86.32% | WARN |
| engine | 25 559 / 28 909 | 88.41% | 91.44% | WARN |
| platform | 525 / 585 | 89.74% | 91.67% | WARN |
| rsync_io | 8 333 / 9 040 | 92.18% | 94.79% | WARN |
| cli | 13 302 / 14 330 | 92.83% | 95.72% | WARN |
| flist | 1 581 / 1 661 | 95.18% | 94.25% | OK |
| protocol | 20 484 / 21 493 | 95.31% | 97.01% | OK |
| logging | 1 479 / 1 546 | 95.67% | 95.12% | OK |
| filters | 2 077 / 2 163 | 96.02% | 97.31% | OK |
| matching | 2 323 / 2 410 | 96.39% | 95.07% | OK |
| compress | 2 533 / 2 620 | 96.68% | 93.82% | OK |
| signature | 1 843 / 1 900 | 97.00% | 94.87% | OK |
| apple-fs | 402 / 414 | 97.10% | 90.14% | OK |
| embedding | 441 / 454 | 97.14% | 98.48% | OK |
| branding | 1 820 / 1 850 | 98.38% | 98.77% | OK |
| bin | 149 / 151 | 98.68% | 100.00% | OK |
| logging-sink | 1 109 / 1 121 | 98.93% | 99.49% | OK |
| bandwidth | 2 211 / 2 215 | 99.82% | 99.71% | OK |
| windows-gnu-eh | 4 / 4 | 100.00% | 100.00% | OK |

Summary: **13 of 27** workspace members are below the 95% line-coverage
target. Listed in descending order of impact (absolute uncovered lines):

| Crate | Uncovered lines |
|-------|---------------:|
| xtask | 4 328 |
| transfer | 3 834 |
| engine | 3 350 |
| daemon | 2 642 |
| core | 2 337 |
| checksums | 1 918 |
| fast_io | 1 661 |
| cli | 1 028 |
| rsync_io | 707 |
| metadata | 638 |
| batch | 510 |
| platform | 60 |
| test-support | 6 |

Five crates contribute 70% of the workspace gap: `xtask`, `transfer`,
`engine`, `daemon`, `core`.

## Lowest-coverage files (>= 25 measurable lines)

| File | Line % | Missing |
|------|------:|-------:|
| `crates/transfer/src/receiver/transfer/sync.rs` | 0.00% | 334 |
| `xtask/src/commands/preflight/validation.rs` | 0.00% | 230 |
| `crates/engine/src/local_copy/executor/special/device.rs` | 0.00% | 190 |
| `xtask/src/commands/interop/messages/mod.rs` | 0.00% | 187 |
| `xtask/src/commands/benchmark/report.rs` | 0.00% | 183 |
| `xtask/src/commands/interop/exit_codes/mod.rs` | 0.00% | 168 |
| `crates/transfer/src/transfer_ops/response.rs` | 0.00% | 163 |
| `xtask/src/commands/interop/exit_codes/runner.rs` | 0.00% | 139 |
| `crates/transfer/src/receiver/transfer/pipelined.rs` | 0.00% | 123 |
| `xtask/src/task/tasks/common.rs` | 0.00% | 101 |
| `xtask/src/commands/benchmark/mod.rs` | 0.00% | 95 |
| `xtask/src/commands/interop/exit_codes/golden.rs` | 0.00% | 89 |
| `crates/protocol/src/flist/incremental/streaming.rs` | 0.00% | 64 |
| `crates/cli/src/frontend/execution/drive/metadata/mapping.rs` | 0.00% | 61 |
| `crates/metadata/src/acl_exacl/read.rs` | 0.00% | 60 |
| `crates/fast_io/src/iocp_stub/file_writer.rs` | 0.00% | 49 |
| `xtask/src/task/tasks/docs.rs` | 0.00% | 48 |
| `crates/fast_io/src/iocp_stub/file_reader.rs` | 0.00% | 42 |
| `crates/fast_io/src/iocp_stub/socket.rs` | 0.00% | 42 |
| `crates/core/src/client/module_list/socket_options/apply.rs` | 0.00% | 41 |

## Top 10 fully-uncovered functions

LCOV `FN`/`FNDA` records, demangled via `rustfilt`. Generic monomorphisations
collapsed to base name; closures and `#[cfg(test)]` helpers filtered out.

| Function | File | Line |
|----------|------|-----:|
| `checksums::simd_batch::md5_simd::avx512::digest_x16` | `crates/checksums/src/simd_batch/md5_simd/avx512.rs` | 120 |
| `checksums::simd_batch::md5_simd::avx512::process_block_avx512` | `crates/checksums/src/simd_batch/md5_simd/avx512.rs` | 217 |
| `cli::frontend::arguments::parser::parse_spill_size` | `crates/cli/src/frontend/arguments/parser/mod.rs` | 965 |
| `cli::frontend::arguments::parser::parse_args` | `crates/cli/src/frontend/arguments/parser/mod.rs` | 34 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_charset` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 660 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_comment` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 26 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_timeout` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 309 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_log_file` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 786 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_temp_dir` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 639 |
| `<daemon::daemon::ModuleDefinitionBuilder>::set_read_only` | `crates/daemon/src/daemon/sections/module_definition/setters.rs` | 190 |

## Excluded paths and platform notes

The CI command does not pass an `--ignore-filename-regex`; the prompt's
ignore set (`tests/|benches/|examples/|build\.rs|fuzz/`) is applied
post-hoc when parsing the LCOV artifact and removes 1 file. Other gaps to
keep in mind when interpreting the per-crate %:

- **Linux-only measurement.** The CI run is `ubuntu-latest`. Windows-only
  paths (`crates/fast_io/src/iocp/`, `crates/cli/src/frontend/execution/drive/.../windows*`,
  Windows ACL/xattr code in `metadata`) are compiled but never executed and
  therefore drag the line counters down even when they have full Windows-side
  test suites. The reverse holds for `crates/fast_io/src/iocp_stub/*` and the
  io_uring stub: those Linux stubs are unused when the real `iocp`/`io_uring`
  modules compile, so they sit at 0% on this runner.
- **CPU-feature-gated SIMD.** `checksums::simd_batch::md5_simd::avx512::*`
  requires AVX-512, which the GitHub-hosted Ubuntu runner does not advertise.
  AVX-2, SSE2 and scalar fall-backs are exercised by the parity tests in
  `crates/checksums/tests/`; the AVX-512 dispatch is provably unreachable on
  the runner, so its 0% line coverage is expected, not a regression.
- **Privileged interop scaffolding.** `crates/engine/src/local_copy/executor/special/device.rs`
  needs `mknod(2)` privileges; the Linux runner has none, so the device-node
  copy path stays at 0%. Same shape for `crates/metadata/src/acl_exacl/read.rs`
  (needs a filesystem with POSIX ACLs and a non-root id mapping).
- **`xtask` is a maintenance binary.** Its commands run via `cargo xtask ...`
  in standalone CI jobs (preflight, benchmark, interop), not via
  `cargo nextest`. None of those code paths are reached from the coverage
  workflow, so the crate's 53.97% reflects "tested only through its CLI
  wrappers". A separate xtask test suite would be the cleanest fix; gating
  it behind `--all-features` is harmless either way.
- **`crates/transfer/src/receiver/transfer/sync.rs`** is the legacy
  single-file-at-a-time receiver loop kept "for compatibility and testing".
  Production paths use the pipelined receivers; the synchronous loop is
  effectively dead unless something explicitly opts in. Either revive it
  in tests or schedule removal as part of BR-4b.

## Recommendations for BR-4b

Order the remediation work by the absolute-gap column (it is the only metric
that meaningfully closes the 9.48 pp distance to 95.00%). Suggested batching:

1. **`xtask` (4 328 lines).** Decide first whether to *measure* xtask at all.
   It is a developer-facing CLI, not a shipped artifact. If we keep it in
   the coverage scope, the cheapest unit-testable surfaces are
   `commands/interop/exit_codes/`, `commands/benchmark/report.rs`,
   `commands/preflight/validation.rs` - all pure-data helpers.
2. **`transfer` (3 834 lines).** Three top files dominate: `receiver/transfer/sync.rs`
   (334 lines, deprecated), `transfer_ops/response.rs` (163 lines), and
   `receiver/transfer/pipelined.rs` (123 lines). The pipelined receiver is
   live code; missing coverage there is the highest-priority real regression.
3. **`engine` (3 350 lines).** `local_copy/executor/special/device.rs` (190
   lines, privileged) plus several large `local_copy/executor/file/*` and
   `concurrent_delta/*` files with partial coverage. Many require root or a
   second filesystem - prefer mocking those at the trait boundary rather than
   real syscalls.
4. **`daemon` (2 642 lines).** Almost the entire
   `daemon/sections/module_definition/setters.rs` builder is untested - those
   are simple setter methods. A single table-driven test would lift the crate
   noticeably.
5. **`core` (2 337 lines).** `client/module_list/socket_options/apply.rs`
   (41 lines) and the option-parsing helpers under `client/config/` are the
   cheapest wins.
6. **`checksums` (1 918 lines).** Excluding AVX-512 (legitimate platform gap),
   the remaining gap is mostly in `parallel/files.rs` and `strong/openssl_support.rs`.
   The OpenSSL path needs the `openssl` feature; coverage of that path requires
   a separate CI matrix entry.
7. **`fast_io` (1 661 lines).** Linux runner cannot exercise the IOCP path or
   the iocp_stub fallbacks. A Windows coverage job would close roughly half
   of this gap; the Linux-side gap (io_uring SQPOLL paths, statx tests) is
   smaller and tractable.
8. **`cli`, `rsync_io`, `metadata`, `batch`, `platform`, `test-support`** are
   each within 700 uncovered lines of target and can be folded into the other
   PRs without their own initiative.

After remediation, raise the workflow's `--fail-under-lines` from 84 to 87
(today's overall, rounded), then in subsequent PRs ratchet by 2 pp until
95 is reached. The ratchet keeps the gate strictly informational until BR-4b
is complete, after which it becomes a required check.

## Failed local attempts

The prompt also asked for a local `--json` run. The initial local invocation
on macOS with `--all-features` was killed after ~30 minutes inside the
`bandwidth` integration suite. The `bwlimit_*` tests use real `std::thread::sleep`
when the lib is compiled without `#[cfg(test)]` (see
`crates/bandwidth/src/limiter/sleep.rs:82-98`): the `test-support` feature
records sleeps, but the `#[cfg(not(test))]` guard inside the recording branch
still calls `thread::sleep` for the integration-test binary. Single tests
budget for 100 s of real sleep at 1 byte/s. The CI artifact above contains
the same numbers (nextest parallelises across cores, so the wall-time hit
is bounded) and is used as the authoritative source for this baseline.

No tests failed in the CI run (`27958 passed, 137 skipped`).
