# Cross-platform CI coverage gaps

Tracking issue: cross-platform CI inventory across `.github/workflows/`. No
code changes.

## 1. Scope and methodology

This audit catalogs which crates, Cargo features, and subsystems are
exercised on which platforms by the workflow files in
`.github/workflows/`. It complements the existing
`docs/audits/cross-platform-parity-matrix.md` (code-side parity) and
`docs/audits/windows-acl-xattr-ci-matrix.md` (Windows ACL/xattr scope) by
focusing strictly on CI-runner coverage versus exposed Cargo surface.

Inputs:

- `.github/workflows/ci.yml` (required matrix), `_test-features.yml`,
  `_interop.yml`, plus the supporting `parallel_determinism.yml`,
  `interop-validation.yml`, `coverage.yml`, `msrv.yml`, `security.yml`,
  `known-failures.yml`.
- Workspace root and each crate `Cargo.toml` `[features]` table.
- `cfg(target_os = ...)`, `cfg(unix)`, `cfg(windows)`, and
  `cfg(feature = ...)` gates across `crates/`.
- Recent GitHub Actions history (`gh run list --workflow=ci.yml` over the
  last 14 days, including failure-only filter).

Per `CLAUDE.md` the gating required checks are: `fmt+clippy`,
`nextest (stable)`, `Windows (stable)`, `macOS (stable)`, and
`Linux musl (stable)`. Everything else is informational.

## 2. CI matrix as actually executed

### 2.1 Per-platform job inventory

| Job (`ci.yml`)                  | Runner          | Toolchains                | Crates tested                                             | Features                                              | Gating |
|---------------------------------|-----------------|---------------------------|-----------------------------------------------------------|-------------------------------------------------------|--------|
| `fmt + clippy`                  | ubuntu-latest   | stable                    | workspace (clippy only)                                   | `--all-features`                                      | yes    |
| `nextest (stable)`              | ubuntu-latest   | stable                    | full `--workspace`                                        | `--all-features`                                      | yes    |
| `nextest (beta)`                | ubuntu-latest   | beta                      | full `--workspace`                                        | `--all-features`                                      | no     |
| `nextest (nightly)`             | ubuntu-latest   | nightly                   | full `--workspace`                                        | `--all-features`                                      | no     |
| `Feature flag combinations`     | ubuntu-latest   | stable                    | per-row scoped (see 2.2)                                  | per-row                                               | no     |
| `Windows (stable)`              | windows-latest  | stable                    | `-p core -p engine -p cli`                                | `--all-features` (build full workspace)               | yes    |
| `Windows (beta)`                | windows-latest  | beta                      | `-p core -p engine -p cli`                                | `--all-features`                                      | no     |
| `Windows (nightly)`             | windows-latest  | nightly                   | `-p core -p engine -p cli`                                | `--all-features`                                      | no     |
| `Windows IOCP`                  | windows-latest  | stable                    | `-p fast_io` (no-default + `iocp`), `-p transfer` (all)   | `--no-default-features --features iocp` / all          | no     |
| `Windows ACL/xattr`             | windows-latest  | stable                    | `-p metadata`, plus filter pass over workspace            | `--features acl,xattr`                                | no     |
| `Windows GNU cross-check`       | ubuntu-latest   | stable                    | `cargo check` only, `x86_64-pc-windows-gnu`               | default                                               | no     |
| `macOS (stable)`                | macos-latest    | stable                    | `-p core -p engine -p cli`                                | `--all-features` (build full workspace)               | yes    |
| `macOS (beta)`                  | macos-latest    | beta                      | `-p core -p engine -p cli`                                | `--all-features`                                      | no     |
| `macOS (nightly)`               | macos-latest    | nightly                   | `-p core -p engine -p cli`                                | `--all-features`                                      | no     |
| `Linux musl (stable)`           | ubuntu-latest   | stable, musl target       | full `--workspace`                                        | `--no-default-features --features "zstd,lz4,xattr,iconv,parallel,copy_file_range"` | yes |
| `Linux musl (beta)`             | ubuntu-latest   | beta, musl target         | full `--workspace`                                        | same                                                  | no     |
| `Linux musl (nightly)`          | ubuntu-latest   | nightly, musl target      | full `--workspace`                                        | same                                                  | no     |
| `interop / upstream rsync`      | ubuntu-latest   | stable                    | binary harness, daemon push/pull, SSH push/pull           | release defaults                                      | no     |

### 2.2 Feature-flag matrix (`_test-features.yml`)

Runs only on `ubuntu-latest`. The matrix entries are:

| Row                  | Crates                                                | Features                                          |
|----------------------|-------------------------------------------------------|---------------------------------------------------|
| `no-default-features`| `--workspace`                                         | `--no-default-features`                           |
| `parallel`           | `checksums`, `flist`                                  | `parallel`                                        |
| `async`              | `daemon`, `core`, `protocol`, `engine`                | `async`                                           |
| `concurrent-sessions`| `daemon`                                              | `concurrent-sessions`                             |
| `tracing`            | `daemon`, `core`, `engine`                            | `tracing`                                         |
| `serde`              | `logging`, `protocol`, `flist`                        | `serde`                                           |
| `incremental-flist`  | `transfer`                                            | `incremental-flist`                               |
| `compression`        | `compress`, `protocol`, `engine`, `transfer`          | `zstd,lz4`                                        |
| `io_uring`           | `transfer`, `fast_io`                                 | `io_uring` (Linux only)                           |
| `copy_file_range`    | `fast_io`                                             | `copy_file_range` (Linux only)                    |

The macOS/Windows runners ignore this workflow entirely.

### 2.3 Other workflows

| Workflow                       | Runner          | Trigger                            | Purpose                              |
|--------------------------------|-----------------|------------------------------------|--------------------------------------|
| `parallel_determinism.yml`     | ubuntu-latest   | push/PR on `crates/`               | sequential vs parallel output diff   |
| `interop-validation.yml`       | ubuntu-latest (x8 jobs) | push/PR/schedule           | exit codes, batch, daemon scenarios  |
| `coverage.yml`                 | ubuntu-latest   | push/PR/schedule (nightly)         | `cargo llvm-cov`, informational      |
| `msrv.yml`                     | ubuntu-latest   | push/PR                            | `cargo check` workspace on 1.88      |
| `security.yml`                 | ubuntu-latest   | push/PR/schedule                   | `cargo audit`/dep-review             |
| `known-failures.yml`           | ubuntu-latest   | weekly                             | upstream-rsync known-failure list    |
| `benchmark.yml`/`benchmark-release.yml` | ubuntu-latest | tag/dispatch                | hyperfine and microbench charts      |
| `release-cross.yml`            | linux/macos/win | tag/dispatch                       | release artifact builds (no tests)   |
| `dependency-review.yml`        | ubuntu-latest   | PR                                 | dependency policy check              |

## 3. Workspace feature inventory

Aggregated from each crate's `Cargo.toml` `[features]` table.

| Feature              | Defined in (crates)                                                  | Type                | Default in workspace? |
|----------------------|----------------------------------------------------------------------|---------------------|------------------------|
| `zstd`               | root, `core`, `cli`, `engine`, `transfer`, `protocol`, `compress`, `batch` | compression         | yes                    |
| `lz4`                | root, `core`, `cli`, `engine`, `transfer`, `protocol`, `compress`     | compression         | yes                    |
| `zlib-ng`            | root, `core`, `engine`, `transfer`, `protocol`, `compress`            | compression         | no                     |
| `zlib-rs`            | `protocol`, `compress`                                                | compression         | no                     |
| `zstdmt`             | `compress`                                                            | compression         | no                     |
| `acl`                | root, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata`       | metadata            | yes                    |
| `xattr`              | root, `cli`, `core`, `engine`, `transfer`, `daemon`, `metadata`       | metadata            | yes                    |
| `iconv`              | root, `cli`, `core`, `transfer`, `daemon`, `protocol`                 | i18n                | yes                    |
| `parallel`           | root, `cli`, `checksums`, `flist`, `engine`, `signature`              | perf                | yes                    |
| `io_uring`           | root, `transfer`, `fast_io`                                           | I/O (Linux)         | yes                    |
| `iocp`               | root, `transfer`, `fast_io`                                           | I/O (Windows)       | yes                    |
| `copy_file_range`    | root, `fast_io`                                                       | I/O (Linux)         | yes                    |
| `syscall_batch`      | `fast_io` (alias)                                                     | I/O                 | implicit               |
| `openssl`            | root, `checksums`                                                     | crypto              | no                     |
| `openssl-vendored`   | root, `checksums`                                                     | crypto              | no                     |
| `embedded-ssh`       | root, `core`, `rsync_io`                                              | transport           | no                     |
| `async-ssh`          | `rsync_io`                                                            | transport           | no                     |
| `async`              | root, `core`, `daemon`, `engine`, `transfer`, `bandwidth`, `protocol` | runtime             | yes                    |
| `async-daemon`       | `daemon`                                                              | runtime (skeleton)  | no                     |
| `concurrent-sessions`| `daemon`                                                              | daemon              | no                     |
| `sd-notify`          | root, `daemon`                                                        | systemd             | no                     |
| `tracing`            | `daemon`, `core`, `engine`, `transfer`, `matching`, `signature`, `protocol`, `filters`, `logging` | observability | no |
| `serde`              | `protocol`, `flist`, `logging`                                        | serialization       | no                     |
| `incremental-flist`  | `transfer`                                                            | protocol            | yes                    |
| `multi-producer`     | `engine`                                                              | engine plumbing     | no                     |
| `lazy-metadata`      | `engine`                                                              | engine perf         | yes                    |
| `bench-internal`     | `matching`                                                            | bench scaffold      | no                     |
| `test-support`       | `bandwidth`                                                           | test scaffold       | no (dev-only)          |

## 4. Coverage cross-product (feature x platform x runner)

Cells: `R` = real test execution; `B` = build only (no nextest); `-` = not
built; `inactive` = compiled out by platform `cfg`.

| Feature             | Linux gnu (stable) | Linux musl (stable) | Linux beta/nightly | macOS (stable) | macOS beta/nightly | Windows MSVC (stable) | Windows MSVC beta/nightly | Windows GNU |
|---------------------|--------------------|---------------------|--------------------|----------------|---------------------|------------------------|---------------------------|-------------|
| `zstd`              | R                  | R                   | R                  | R (core/engine/cli) | R                | R (core/engine/cli)    | R                         | B           |
| `lz4`               | R                  | R                   | R                  | R              | R                   | R                      | R                         | B           |
| `zlib-ng`           | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `zlib-rs`           | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `zstdmt`            | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `acl`               | R                  | R                   | R                  | R (no metadata) | R                  | R (windows-acl-xattr)  | -                         | -           |
| `xattr`             | R                  | R                   | R                  | R (no metadata) | R                  | R (windows-acl-xattr)  | -                         | -           |
| `iconv`             | R                  | R                   | R                  | R (core/engine/cli) | R                | R (core/engine/cli)    | R                         | B           |
| `parallel`          | R (workspace+feature row) | R           | R                  | R (workspace via all-features) | R   | R (core/engine/cli)    | R                         | B           |
| `io_uring`          | R (feature row)    | R                   | R                  | inactive       | inactive            | inactive               | inactive                  | inactive    |
| `iocp`              | inactive           | inactive            | inactive           | inactive       | inactive            | R (windows-iocp)       | R (via all-features)      | B (default) |
| `copy_file_range`   | R (feature row)    | R                   | R                  | inactive       | inactive            | inactive               | inactive                  | inactive    |
| `openssl`           | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `openssl-vendored`  | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `embedded-ssh`      | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `async-ssh`         | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `async`             | R (workspace+row)  | -                   | R                  | R              | R                   | R                      | R                         | B           |
| `async-daemon`      | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `concurrent-sessions`| R (row)           | -                   | R                  | -              | -                   | -                      | -                         | -           |
| `sd-notify`         | -                  | -                   | -                  | inactive       | inactive            | inactive               | inactive                  | inactive    |
| `tracing`           | R (workspace+row)  | -                   | R                  | R              | R                   | R                      | R                         | B           |
| `serde`             | R (workspace+row)  | -                   | R                  | R              | R                   | R                      | R                         | B           |
| `incremental-flist` | R (workspace+row)  | R                   | R                  | R              | R                   | R                      | R                         | B           |
| `multi-producer`    | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |
| `lazy-metadata`     | R (default)        | R                   | R                  | R              | R                   | R                      | R                         | B           |
| `bench-internal`    | -                  | -                   | -                  | -              | -                   | -                      | -                         | -           |

Notes:

- "core/engine/cli" cells mean that platform's nextest step is scoped to
  `-p core -p engine -p cli` only; the rest of the workspace is built
  with `--all-features` but never run as tests.
- musl drops `acl`, `io_uring`, and `iocp` from `--features`; `acl`
  reverts to `acl_noop` on musl through the same cfg path as Windows.
- `iocp` only links on Windows; the `windows-iocp` job is the only place
  it is built with `--no-default-features --features iocp`.
- `io_uring` and `copy_file_range` only link on `target_os = "linux"`;
  the `_test-features.yml` rows are explicitly `linux_only: true`.

## 5. Identified gaps

Severity scale:

- **High** - subsystem ships with no test execution on any platform that
  matters operationally, or a feature has zero coverage anywhere.
- **Medium** - a feature is exercised on Linux but not on the platform
  whose code path it primarily targets, or a platform skips a crate that
  contains platform-specific code.
- **Low** - documentation/observability features with low blast radius.

### G1 - High - `openssl` and `openssl-vendored` never tested

- Defined in `crates/checksums/Cargo.toml`, surfaced at workspace root.
  No CI job builds `--features openssl` or `--features openssl-vendored`.
- `crates/checksums/src/strong/md4.rs`, `md5.rs`, `mod.rs`, and
  `comprehensive_tests.rs` all carry `cfg(feature = "openssl")` branches
  that compile only when explicitly requested.
- Risk: a `digest` or `openssl` crate bump silently breaks the
  OpenSSL-accelerated path; nothing fails until a downstream packager
  enables it (Debian/RHEL builds frequently link system OpenSSL).

### G2 - High - `zlib-ng` and `zlib-rs` backends never tested

- Defined in `crates/compress/Cargo.toml` and `crates/protocol/Cargo.toml`,
  surfaced at workspace root. `crates/compress/tests/zlib_ng_backend.rs`
  is the only integration test, gated behind `cfg(feature = "zlib-ng")`,
  but no CI matrix entry enables that feature.
- The release-time recommendation in the workspace `Cargo.toml`
  ("matches upstream rsync performance") promotes a backend nobody runs
  through CI; a `flate2` upgrade can silently change wire-level deflate
  output and pass.

### G3 - High - `embedded-ssh` and `async-ssh` transports never tested

- `crates/rsync_io/src/ssh/embedded/` and `crates/rsync_io/src/ssh/async_transport.rs`
  are gated on `cfg(feature = "embedded-ssh")` / `cfg(feature = "async-ssh")`,
  with substantial code (`config.rs`, `auth.rs`, `ssh_config.rs`,
  `mod.rs`). `crates/core/src/client/run/mod.rs` and
  `crates/core/src/client/remote/mod.rs` also branch on `embedded-ssh`.
- Neither feature appears in any workflow. `russh` is a security-sensitive
  dependency (Marvin Attack note in workspace `Cargo.toml`); compile
  failures and behavioural drift go undetected until users opt in.

### G4 - High - `metadata` crate skipped on macOS

- `windows-acl-xattr` job covers `-p metadata` on Windows, but the
  `macos-test` job runs only `-p core -p engine -p cli`. macOS uses
  `acl_exacl` (Darwin ACL API) and a different timestamp path
  (`crates/metadata/src/apply/timestamps.rs` has `cfg(target_os = "macos")`
  branches) that no test ever touches in CI.
- macOS also exercises `apple-fs` (resource forks, `_AppleDouble`)
  through `engine`, but `crates/apple-fs/tests/apple_double_round_trip.rs`
  runs only when the `apple-fs` crate is selected, which the
  macOS job's `-p core -p engine -p cli` does not include.

### G5 - High - macOS and Windows never run interop tests

- `interop-upstream`, `interop-validation`, and the
  `interop / SSH push/pull` steps all hard-code `runs-on: ubuntu-latest`.
  Wire-level divergence specific to macOS BSD coreutils (e.g. `rsync`
  versions linked against different `iconv`/`zlib` builds) or Windows
  path normalisation regressions cannot be caught.
- **Status: partially closed.** `.github/workflows/_interop-macos.yml`
  (required) runs the portable harness in
  `tools/ci/run_interop_smoke.sh` against `brew install rsync` on
  `macos-latest`. `.github/workflows/_interop-windows.yml` (best-effort,
  `continue-on-error: true`) runs the same harness against MSYS2's
  upstream rsync on `windows-latest`. Both are wired in `ci.yml` as
  `interop-upstream-macos` and `interop-upstream-windows`. Skipped per
  OS: xattr/ACL on both, daemon mode on both, SSH loopback on both,
  `--list-only` parity on Windows (Cygwin path-style differences).
  `interop-validation` still hard-codes `ubuntu-latest`; a follow-up
  will extend that workflow when the smoke surface stabilises on
  macOS/Windows.

### G6 - Medium - `concurrent-sessions`, `sd-notify`, `tracing`, `serde`, `async`, `async-daemon` only built on Linux

- The `_test-features.yml` matrix runs every row on `ubuntu-latest`. The
  Windows and macOS runners skip these features entirely (they build
  `--all-features`, which includes `async` and `tracing` indirectly, but
  not the standalone `--features tracing` / `--features serde`
  combinations that exercise the cfg branches).
- `async-daemon` (`crates/daemon` skeleton tracked by issue #1935) and
  `sd-notify` (daemon-only Linux) have zero feature-flag coverage on any
  platform.

### G7 - Medium - musl drops `acl` from feature list

- `Linux musl (stable)` builds with
  `--no-default-features --features "zstd,lz4,xattr,iconv,parallel,copy_file_range"`.
  `acl` is intentionally omitted (the `exacl` crate links against
  `libacl` which is not in the musl static toolchain), so `metadata`
  routes through `acl_noop`. That stub is then the only thing the musl
  job exercises, and the gnu Linux job runs `acl_exacl` from
  `--all-features`.
- The gap is acceptable for runtime parity, but no job verifies the
  `acl` feature alone on musl. The `windows-acl-xattr` job is the
  closest analogue (since both fall through to `acl_noop` on the
  `linux,musl` target only when the feature is off). Drift between
  `acl_noop` and `acl_exacl` is invisible until a user reports it.

### G8 - Medium - `Windows GNU cross-check` is compile-only

- `windows-gnu-cross-check` runs `cargo check --target x86_64-pc-windows-gnu`
  but never builds (`cargo build`) and never runs `nextest`. The
  `crates/windows-gnu-eh` shim exists exclusively for this target. A
  link or runtime regression in that shim slips past CI silently.

### G9 - Medium - `Windows IOCP` runs `transfer` with `--all-features`, not isolated `iocp`

- The job builds `fast_io` with `--no-default-features --features iocp`
  (correct) but then tests `transfer` with `--all-features`, which on
  Windows pulls in `iocp` through default features. There is no
  "transfer with only `iocp`" test that would surface a missed
  forwarding link from `transfer` to `fast_io`.

### G10 - Medium - `multi-producer` and `bench-internal` features exist but are unreachable from CI

- `crates/engine/Cargo.toml` defines `multi-producer` (Clone for
  `WorkQueueSender`); no workflow row enables it. `crates/matching/Cargo.toml`
  defines `bench-internal` and is explicitly described as never enabled
  by any release path, but it backs benchmarks that the workflow does
  occasionally invoke (e.g. `bench(matching): compact-keys cache
  behavior`). The benchmark workflow runs `cargo bench` without
  `--features bench-internal`, so the bench harness exercises the
  default surface only.

### G11 - Low - `tracing` feature build does not assert any tracing output

- `_test-features.yml` row `tracing` is build/test only; the test set
  for the listed crates does not include any structured-log assertion.
  Coverage is opportunistic (whatever existing tests happen to assert
  via `tracing::Subscriber` set up by `test-support`).

### G12 - Low - benchmark and known-failure workflows count toward "intermittent" noise

- The recent `gh run list --status=failure` output is dominated by
  `bench(*)` and `known-failures.yml` runs; none of these are required
  checks, but they do consume runner minutes and clutter the dashboard.
  A May 16 burst (run IDs 25958255709 - 25960144038) shows the entire
  required matrix failing in lockstep, including `Windows ACL/xattr`
  and the full musl trio. Root cause was a workspace-wide regression
  fixed forward; no platform-specific intermittency is currently
  visible.

## 6. Recommendations

For each gap, pick one of: extend the matrix, mark acceptable, or drop
the feature.

| Gap | Recommendation                                                                                   | Action |
|-----|--------------------------------------------------------------------------------------------------|--------|
| G1  | Extend matrix: add `openssl` and `openssl-vendored` rows to `_test-features.yml` (Linux only).   | extend |
| G2  | Extend matrix: add `zlib-ng` and `zlib-rs` rows to `_test-features.yml`; install `libz-ng-dev`. | extend |
| G3  | Extend matrix: add `embedded-ssh` row (Linux) using `russh` against the existing SSH loopback in `_interop.yml`; add `async-ssh` row scoped to `-p rsync_io`. | extend |
| G4  | Extend matrix: append `-p metadata -p apple-fs` to the macOS nextest step (mirrors `windows-acl-xattr`). | extend |
| G5  | Extend matrix: spin a macOS-host interop run using upstream rsync from Homebrew; add a Windows interop smoke test that only validates the daemon's `--list-only` path (full SSH interop on Windows is out of scope). | extend |
| G6  | Extend matrix: make `_test-features.yml` a cross-OS strategy (matrix.os = ubuntu-latest, macos-latest, windows-latest) for the OS-agnostic rows (`tracing`, `serde`, `async`, `concurrent-sessions`); keep `io_uring`/`copy_file_range` Linux-only. | extend |
| G7  | Mark acceptable. Document the musl/`acl_noop` fall-through in `docs/platform-support.md`; add a single `Linux musl + acl (gnu sidecar)` informational job only if a regression surfaces. | document |
| G8  | Extend matrix: upgrade `windows-gnu-cross-check` to `cargo build --target x86_64-pc-windows-gnu` and (under `wine` or via `cross`) run the `windows-gnu-eh` test subset. | extend |
| G9  | Extend matrix: add a `transfer with --no-default-features --features iocp` build step inside `windows-iocp`. | extend |
| G10 | Drop or document: `multi-producer` is unused outside engine internals; either wire a feature row that runs `-p engine --features multi-producer` or remove the feature flag. `bench-internal` should be acknowledged as "harness-only, never gates CI". | drop/document |
| G11 | Mark acceptable. The feature is observability; the design audit in `docs/audits/tracing-instrumentation.md` already covers behavioural assertions. | document |
| G12 | Mark acceptable. Bench runs are inherently noisy. Move benchmark failure notifications off the master CI dashboard via the existing `continue-on-error` pattern; no required check is affected. | document |

## 7. Suggested follow-up tasks

Each task is sized to roughly one PR. None require wire-protocol
changes.

1. **Feature flag rows for crypto and compression backends.** Add four
   matrix entries (`openssl`, `openssl-vendored`, `zlib-ng`, `zlib-rs`)
   to `.github/workflows/_test-features.yml`. APT install steps:
   `libssl-dev` for `openssl`, none for `openssl-vendored`,
   `zlib1g-dev`/`libz-ng-dev` for the deflate backends.
2. **Embedded-SSH coverage row.** Add `embedded-ssh` and `async-ssh`
   rows scoped to `-p rsync_io -p core --features embedded-ssh` plus a
   smoke test that reuses the SSH loopback fixture in `_interop.yml`.
3. **macOS metadata coverage.** Extend `macos-test` to include
   `-p metadata -p apple-fs`. Verify `acl_exacl` round-trip and Apple
   Double round-trip tests are picked up. Pair with `feature_matrix.md`
   update.
4. **Cross-OS feature matrix.** Promote `_test-features.yml` to a
   strategy matrix over `[ubuntu-latest, macos-latest, windows-latest]`
   for the OS-agnostic rows. Skip Linux-only rows with `if:` guards.
5. **Windows GNU exec coverage.** Switch the GNU cross-check to
   `cargo build` plus a minimal `wine`-driven smoke run, or migrate to
   `cross` for an actual test step. Document the choice in
   `docs/platform-support.md`.
6. **macOS interop job.** Mirror `_interop.yml` on `macos-latest` with
   Homebrew-installed `rsync@3.4.1`. Mark `continue-on-error: true`
   until baseline parity is established, then promote to required.
7. **Windows daemon smoke.** Add a `windows-latest` job that runs
   `oc-rsync` in daemon mode against a localhost client to validate
   `daemon::name_converter` and `crates/platform/src/windows_service.rs`.
8. **Cleanup `multi-producer` and `bench-internal`.** Either wire them
   into the feature matrix or remove the feature gate. Update
   `docs/audits/cross-platform-parity-matrix.md` (section 4) and
   workspace `Cargo.toml` comments accordingly.
9. **Drop `nextest (beta/nightly)` from `--all-features` workspace
   build duplication.** They already mark `continue-on-error: true`;
   if budget pressure rises, scope them to lint+build only (no
   nextest) to keep nightly compiler signal without the test cost.

## 8. References

- Workflows: `.github/workflows/ci.yml`,
  `.github/workflows/_test-features.yml`,
  `.github/workflows/_interop.yml`,
  `.github/workflows/coverage.yml`,
  `.github/workflows/msrv.yml`,
  `.github/workflows/parallel_determinism.yml`,
  `.github/workflows/known-failures.yml`.
- Workspace features: `Cargo.toml`, `crates/*/Cargo.toml`.
- Related audits: `docs/audits/cross-platform-parity-matrix.md`,
  `docs/audits/windows-acl-xattr-ci-matrix.md`,
  `docs/audits/cross-platform-fast-io-gap-bench.md`,
  `docs/platform-support.md`, `docs/platform-io-fast-paths.md`,
  `docs/windows_platform_parity.md`.
