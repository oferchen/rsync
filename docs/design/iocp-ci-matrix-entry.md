# IOCP CI matrix entry (#1900)

This note specifies the dedicated `windows-iocp` CI job that pins the
`iocp` feature against the `fast_io` and `transfer` crates on
`windows-latest`. It is the IOCP-side sibling of the Windows
ACL/xattr CI matrix work (#1869) and follows the same matrix-shaping
principles: pin the feature combination explicitly, run a focused test
selection, mark the new entry non-required during burn-in, and promote
after a 7-day green streak.

## 1. IOCP feature gate (#1717 done)

The `iocp` feature is the single switch that enables the I/O completion
ports backend for Windows file writes. PR #1717 landed the gate and the
follow-on PRs (#1718-#1721) wired the runtime probe, the overlapped
write path, and the disk-commit dispatch. The feature is declared in
the workspace root and fans out to two crates:

```
# workspace Cargo.toml
iocp = ["transfer/iocp", "fast_io/iocp"]
```

`iocp` is part of the workspace default feature set
(`Cargo.toml:33`). The main `windows-test` job therefore exercises it
implicitly via `--all-features`. Implicit coverage is not enough for a
beta-grade Windows path: a future change to `default-features` could
silently drop the IOCP wire-up from CI without any test going red.
A pinned feature flag in a dedicated job protects against that
regression class.

`fast_io` owns the IOCP modules (`crates/fast_io/src/iocp/`); `transfer`
is the consumer that hands off `FileMessage` items to the IOCP
disk-commit thread (`crates/transfer/src/disk_commit/process.rs`).
Both crates must be built and tested with `iocp` explicit so the FFI
surface, the overlapped completion handler, and the dispatch glue stay
in lockstep.

## 2. Proposed `.github/workflows/ci.yml` entry

A new top-level job named `windows-iocp` is added to
`.github/workflows/ci.yml`, structured to mirror `windows-test`
(`ci.yml:169-216`) so the runner image, cache key shape, and toolchain
action stay consistent. The job runs in parallel with `windows-test`,
`macos-test`, and `linux-musl`; it does not extend critical path
unless it is the slowest leg.

| Attribute | Value |
|-----------|-------|
| `runs-on` | `windows-latest` |
| `needs` | `lint` (matches `windows-test:172`) |
| `timeout-minutes` | `30` |
| Toolchain | `stable` only at first; matrix expansion deferred |
| Cache shared-key | `ci-windows-iocp-cargo-${{ env.CACHE_VERSION }}-${{ hashFiles('**/Cargo.toml') }}` |
| Cache key | `windows-iocp` (separate from `windows-test`) |

The job intentionally uses `--features iocp` rather than
`--all-features` so the failure mode is immediately readable: "the
IOCP feature broke" rather than "something in `--all-features` broke
and the bisect is across 12 features." This mirrors the chosen design
in #1869 (separate `windows-acl-xattr` job) over a combined
`windows-fast-paths` job.

## 3. Cargo build + nextest steps

Three build-and-test steps, each with an explicit feature combination
so a future change to `default-features` cannot silently drop coverage:

1. Build the workspace with IOCP explicitly enabled:

       cargo build --locked --workspace --features iocp

2. Build `fast_io` with only `iocp` enabled (no other defaults) so
   the IOCP code path is verified in isolation. This catches regressions
   where `fast_io` accidentally relies on a sibling default feature
   such as `io_uring` or `copy_file_range`:

       cargo build --locked -p fast_io --no-default-features --features iocp

3. Run nextest twice, once for the owner crate and once for the
   primary consumer:

       cargo nextest run --locked -p fast_io --no-default-features --features iocp
       cargo nextest run --locked -p transfer --all-features

   `fast_io` runs in isolation; `transfer` runs against the
   IOCP-enabled `fast_io` via its path dependency, so the consumer
   surface is exercised end-to-end. `transfer` has no own `iocp`
   feature, so `--all-features` here is the simplest way to enable
   the full transfer test surface without dropping IOCP.

## 4. Artifact upload binary for downstream tests

The job publishes a debug-built `oc-rsync.exe` so downstream interop
and parallel-determinism workflows can pull a known-IOCP-on binary
without rebuilding. The upload step mirrors `windows-test:208-216`:

```
- name: Upload IOCP-enabled build artifact
  uses: actions/upload-artifact@v7
  with:
    name: build-windows-iocp
    path: |
      target/debug/oc-rsync.exe
      target/debug/xtask.exe
    if-no-files-found: warn
    retention-days: 7
```

Downstream consumers (e.g. `interop-validation.yml`,
`parallel_determinism.yml`) reference the artifact name
`build-windows-iocp` via `actions/download-artifact` and
`needs: windows-iocp`. The 7-day retention matches the existing
`build-windows-${{ matrix.toolchain }}` artifact cadence so cross-job
storage budgets stay flat.

The artifact is debug-build, not release-build. Release artifacts come
from `release-cross.yml` and `benchmark-release.yml`; the CI job is
only for downstream test wiring inside the same PR run.

## 5. Rollout

Phase 1 (week 0): land the job marked non-required.
Phase 2 (week 0 - week 1): observe over a 7-day window; flake-driven
reds revert to non-required and restart the clock.
Phase 3 (week 1+): promote to branch-protection required checks.
Phase 4 (post #1897 / #1898 / #1929 / #1930): expand toolchain matrix
from `[stable]` to `[stable, beta, nightly]` with `continue-on-error`
on non-stable legs.

## 6. References

- IOCP feature gate: workspace `Cargo.toml:77`, default at
  `Cargo.toml:33`. Owner crate: `crates/fast_io/src/iocp/`. Consumer:
  `crates/transfer/src/disk_commit/`.
- Sibling CI matrix entry (#1869): `docs/design/windows-acl-xattr-ci-matrix.md`.
- Workflow paths: `.github/workflows/{ci,interop-validation,parallel_determinism}.yml`.
- Tasks: #1717 (IOCP gate, done), #1718-#1721 (IOCP wiring, done),
  #1868 (disk-commit IOCP, done), #1869 (Windows ACL/xattr CI),
  #1897 / #1898 / #1929 / #1930 (in-flight IOCP hardening),
  #1900 (this work).
