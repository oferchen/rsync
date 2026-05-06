# Windows ACL/xattr CI matrix entries (#1869)

This note specifies the new CI job that exercises the Windows ACL and
xattr code paths on `windows-msvc` runners and the cross-check that
keeps the wire-level outputs identical across Linux, macOS, and
Windows. It is the metadata-side sibling of the IOCP CI matrix work
(#1900) and uses the same matrix-shaping principles: pin the feature
combination explicitly, run a focused test selection, mark the new
entry non-required during burn-in, and promote after a 7-day green
streak.

The doc is informational. It does not modify `.github/workflows/ci.yml`
in this commit; the wiring lands in a follow-up that adds the job under
#1869. All file:LINE citations are against the master branch state.

## 1. Current Windows CI matrix

The CI workflow defines five Windows-relevant entries. None of them
exercise the metadata crate's ACL or ADS code paths.

- `windows-test` (`.github/workflows/ci.yml:165-211`) - runs on
  `windows-latest` (the GitHub-hosted `windows-msvc` MSVC image)
  across `[stable, beta, nightly]`. Build is workspace-wide with
  `--all-features` (`ci.yml:194-195`). Tests are scoped to three
  crates: `cargo nextest run --locked -p core -p engine -p cli
  --all-features` (`ci.yml:197-201`). The metadata crate is not in
  the test selection.
- `windows-gnu-cross-check` (`ci.yml:216-240`) - cross-`check` on
  `ubuntu-latest` against `x86_64-pc-windows-gnu`. Compile-only,
  no test execution.
- `lint` (`ci.yml:51-86`) - runs `cargo clippy --workspace
  --all-targets --all-features` on Linux only. Catches Windows
  attribute errors (cfg gates) but never executes a test on Windows.
- `feature-flags` (`ci.yml:155-160`) - delegates to
  `_test-features.yml`, which runs on `ubuntu-latest` only
  (`.github/workflows/_test-features.yml:1-47`). The feature-flag
  matrix has no Windows leg.
- `interop-upstream` (`ci.yml:355-362`) - delegates to
  `_interop.yml`, which exercises upstream rsync 3.0.9, 3.1.3, and
  3.4.1 on Linux only.

The default Cargo feature set in the workspace root does not include
`acl` or `xattr` for the metadata crate
(`crates/metadata/Cargo.toml:47-50`):

```
[features]
default = []
xattr = ["dep:xattr"]
acl = ["dep:exacl"]
```

`--all-features` does enable both. So the build leg of `windows-test`
compiles `acl_windows.rs` and `xattr_windows.rs`, but the test leg
only runs `core`, `engine`, `cli`.

## 2. Gap

No CI job runs the `metadata` crate test suite on `windows-msvc`. The
following code is built but never executed by any Windows CI leg
today:

- `crates/metadata/src/acl_windows.rs` (814 lines), including the
  unit-test module `mod tests` (`acl_windows.rs:674-...`) with 13
  `#[test]` cases covering `perms_round_trip_through_access_mask`,
  `reconstruct_acl_*`, `sync_acls_*`, and `apply_acls_from_cache_*`.
  The module is gated `#[cfg(all(feature = "acl", windows))]`
  (`acl_windows.rs:1`) and exposes `sync_acls`
  (`acl_windows.rs:415`) and `apply_acls_from_cache`
  (`acl_windows.rs:458`) via the public re-exports at
  `crates/metadata/src/lib.rs:171-172`.
- `crates/metadata/src/xattr_windows.rs` (560 lines), including
  `mod tests` (`xattr_windows.rs:362-...`) with 17 `#[test]` cases
  covering `parse_stream_name_*`, `stream_path_wide_*`,
  `write_then_read_roundtrip`, `list_returns_written_streams`, and
  unicode/binary path edge cases. The module supplies
  `list_attributes` (`xattr_windows.rs:192`), `read_attribute`
  (`xattr_windows.rs:258`), `write_attribute`
  (`xattr_windows.rs:301`), and `remove_attribute`
  (`xattr_windows.rs:345`), wired into the cross-platform `xattr`
  layer at `crates/metadata/src/xattr.rs:75-107`.
- `crates/metadata/tests/acl_handling.rs` integration suite
  (1184 lines). Most modules are POSIX-only via cfg gates
  (`acl_handling.rs:535-538`, `739-741`, `771-773`, `804-807`,
  `856-858`); none target Windows.

The metadata crate has no `windows`-gated integration test file in
`crates/metadata/tests/`. The only Windows ACL coverage today is the
inline `#[cfg(test)] mod tests` inside `acl_windows.rs` and
`xattr_windows.rs`, neither of which is run by any CI job.

A regression in any of the following code paths would land on master
without CI noticing:

- Win32 SID-to-rsync uid/gid mapping (`acl_windows.rs:190-289`).
- `LookupAccountSidW` / `LookupAccountNameW` round-trips
  (`acl_windows.rs:55-69`).
- DACL serialisation against the wire-format `RsyncAcl`
  (`acl_windows.rs:291-365`, paired with
  `crates/protocol/src/acl/wire/types.rs:3-20`).
- NTFS Alternate Data Stream enumeration via `FindFirstStreamW`
  / `FindNextStreamW` (`xattr_windows.rs:192-256`).
- Stream-path UTF-16 encoding and `:$DATA` suffix handling
  (`xattr_windows.rs:62-110`).
- `apply_xattrs_from_list` / `read_xattrs_for_wire` on Windows
  (`xattr.rs:121-220`), including the
  `FAKE_SUPER_XATTR` fallback (`crates/metadata/src/fake_super.rs:31`).

## 3. Required additions

A new top-level job named `windows-acl-xattr` is added to
`.github/workflows/ci.yml`, structured to mirror `windows-test`
(`ci.yml:165-211`) so the runner image, cache key shape, and
toolchain action stay consistent.

| Attribute | Value |
|-----------|-------|
| `runs-on` | `windows-latest` |
| `needs` | `lint` (matches `windows-test:168`) |
| `timeout-minutes` | `30` |
| Toolchain | `stable` only at first; matrix expansion deferred |
| `RUSTFLAGS` | inherits `-D warnings` from workflow `env` (`ci.yml:43`) |
| Cache shared-key | `ci-windows-acl-xattr-cargo-${{ env.CACHE_VERSION }}-${{ hashFiles('**/Cargo.toml') }}` |

Three build-and-test steps are required, each with an explicit feature
combination so a future change to `default-features` cannot silently
drop coverage:

1. Build the metadata crate with both features pinned:

       cargo build --locked -p metadata --features acl,xattr

   This compiles `acl_windows.rs` and `xattr_windows.rs` against the
   `windows = "0.62"` FFI surface declared at
   `crates/metadata/Cargo.toml:38-45`.

2. Run the metadata test suite under the same flags:

       cargo nextest run --locked -p metadata --features acl,xattr

   This executes the inline `#[cfg(test)] mod tests` modules in
   `acl_windows.rs` (13 tests) and `xattr_windows.rs` (17 tests), plus
   the integration tests in `crates/metadata/tests/` that run on
   Windows (none today; the file is the landing place for #1869's
   follow-on tests).

3. Run a workspace-scoped filter to catch ACL/xattr/ADS tests that
   land outside the metadata crate (e.g. transfer-level integration
   tests added by #1518-#1521 or future #1866 work):

       cargo nextest run --locked --workspace --features acl,xattr \
         -E 'test(acl) | test(xattr) | test(ads) | test(stream)'

   The nextest expression syntax is documented in `.config/nextest.toml`.
   `shell: bash` is required on the GitHub Windows runner so the
   single-quoted `-E` expression survives without PowerShell escape
   munging; this matches the existing pattern used for SSH integration
   tests at `ci.yml:127-140` (which uses `bash` heredoc for
   `ssh-keygen`).

The job intentionally uses `--features acl,xattr` rather than
`--all-features` so that the failure mode is immediately readable:
"the ACL or xattr feature broke" rather than "something in
`--all-features` broke and the bisect is across 12 features."

## 4. Test selection

Tests that must execute on `windows-msvc` after #1869 lands but do not
run on any Windows CI job today:

| Source | Tests | Coverage gap |
|--------|------:|--------------|
| `crates/metadata/src/acl_windows.rs:674-...` | 13 | `RsyncAcl` <-> DACL round-trip, `LookupAccountSidW` mapping, `sync_acls` follow-symlink semantics, `apply_acls_from_cache` cache hits/misses |
| `crates/metadata/src/xattr_windows.rs:362-...` | 17 | NTFS ADS list/read/write/delete, `:$DATA` suffix parsing, UTF-16 path encoding, unicode stream names, FAT32 graceful skip |
| `crates/metadata/src/xattr.rs:75-107` cross-platform layer | inline | Windows path through the shared dispatch |
| `crates/metadata/src/fake_super.rs:31-...` | inline | `FAKE_SUPER_XATTR` storage on NTFS ADS rather than POSIX user xattrs |

Tests that already run on Linux/macOS and produce output that must
match on Windows are listed in the cross-check section below.

The `acl_handling.rs` integration suite already gates its modules by
target_os: `posix_acl_tests` (`acl_handling.rs:535-538`),
`default_acl_tests` (`acl_handling.rs:739-741`), `macos_acl_tests`
(`acl_handling.rs:771-773`), `linux_acl_tests` (`acl_handling.rs:804`).
A new `windows_acl_tests` module is the natural landing place for
#1866 follow-on coverage and will be picked up automatically by step
3 of section 3 (the workspace-scoped `-E test(acl) ...` filter).

## 5. Cross-check job: parity across Linux, macOS, Windows

The wire-format ACL and xattr representations are platform-independent
by design (`crates/protocol/src/acl/wire/types.rs:3-20`,
`crates/protocol/src/acl/wire/send.rs:13-154`). A cross-platform
parity test serialises the same logical ACL/xattr through each
platform's backend and asserts byte-for-byte agreement with a checked-in
golden.

Test layout (lands in #1869's follow-up):

- `crates/metadata/tests/wire_parity.rs` - new file, gated
  `#[cfg(any(unix, windows))]`. Builds an `RsyncAcl` value, encodes
  via the wire functions in `crates/protocol/src/acl/wire/send.rs`,
  hashes the output with SHA-256, and asserts against a constant
  hex digest. The same hex digest is checked on every platform; a
  divergence fails the test.
- `crates/metadata/tests/wire_parity_xattr.rs` - new file, mirrors
  the ACL parity test for `XattrList` round-trips through
  `read_xattrs_for_wire` (`xattr.rs:121-157`) and
  `apply_xattrs_from_list` (`xattr.rs:222-...`).

The cross-check is not a separate CI job. It runs as part of the
existing `nextest (stable)` (Linux), `macOS (stable)`, and
`Windows ACL/xattr` legs because all three select `-p metadata` (or
the workspace) and pick up `tests/wire_parity*.rs`. Required-checks
configuration is updated to require all three jobs to be green; if a
backend produces a different byte sequence, all three fail and the PR
is blocked.

This pattern matches the protocol golden-byte tests in
`crates/protocol/tests/golden/`: a single source of truth, no
platform-specific golden files, divergences fail loudly.

## 6. Reuse with #1900 (IOCP)

The IOCP CI work (#1900) adds `windows-iocp` as a separate job that
pins `--features iocp` against the `fast_io` and `transfer` crates.
Both jobs share the same runner image, the same toolchain action,
and similar cache structure. Two designs were considered:

### 6.1 Combined `windows-fast-paths` job (rejected)

A single job named `windows-fast-paths` that builds with
`--features iocp,acl,xattr` and runs nextest across all three crates
in one cache-warmed pass.

Pros: one CI job pays one cold-cache build cost; secondary-test runs
that depend on shared deps (`tempfile`, `windows-rs`) compile once.

Cons:

- Failure attribution: a red `windows-fast-paths` could be
  IOCP-only, ACL-only, xattr-only, or any combination. PR authors
  would have to read the Step name in the workflow log instead of
  the Job name, which is not surfaced in the GitHub PR-checks UI.
- Coverage promotion: IOCP and ACL/xattr are at different beta-
  readiness levels. IOCP has #1717-#1721 merged and #1897 / #1898 /
  #1929 / #1930 in flight; Windows ACL is #1866 (in progress).
  Promoting one to required while keeping the other non-required
  is impossible if they share a job name.
- Crate ownership: `fast_io` is in the unsafe-allowlist, `metadata`
  is too, but the failure surface for a wrap-around bug is very
  different. Conflating them obscures the diagnostic.

### 6.2 Separate `windows-acl-xattr` and `windows-iocp` jobs (chosen)

Two sibling jobs, each pinned to its feature axis. Independent
toolchain matrices, independent caches, independent required-checks
state, independent rollout (#1900 promotes when its 7-day green
streak completes; #1869 does the same on its own clock).

This mirrors the existing pattern in `_test-features.yml` where each
feature combination is a matrix entry, not a `--all-features` lump.

The chosen design adds two job entries to the workflow rather than
one. Total CI time is slightly higher (two cold caches instead of one),
but failure attribution and rollout independence outweigh that cost
(see section 7).

## 7. Cost: minutes added to PR CI

Baseline measurements pulled from recent CI runs of `windows-test`
(stable leg, master branch):

| Stage | Time |
|-------|------|
| Checkout | 12 s |
| Toolchain install | 35 s |
| Rust cache restore | 60-120 s (cold: 0; warm-hit: 90 s avg) |
| `cargo build --workspace --all-features` | 6-8 min cold, 60-90 s warm |
| `cargo nextest run -p core -p engine -p cli` | 4-6 min |
| Artifact upload | 5 s |

The new `windows-acl-xattr` job is much narrower:

| Stage | Estimate |
|-------|----------|
| Checkout | 12 s |
| Toolchain install | 35 s |
| Rust cache restore (sibling cache key) | 60-120 s |
| `cargo build -p metadata --features acl,xattr` | 90 s warm, 4 min cold |
| `cargo nextest run -p metadata --features acl,xattr` | 30-60 s (30 unit tests + future integration) |
| Workspace `-E` filter | 60-90 s (compiled deps already warm) |

Total added wall-clock per PR (warm cache): roughly 3-4 minutes.
Total added runner-minutes per PR: roughly 5-6 minutes. The job runs
in parallel with `windows-test`, `macos-test`, and `linux-musl`, so
it does not extend critical path unless it is the slowest leg
(which it is not; `windows-test --all-features` dominates).

The trade is minutes of CI time against catching:

- Win32 ACL FFI regressions before they ship to master.
- ADS encoding bugs on unicode stream names.
- Cross-platform wire parity drift between platforms.
- `windows-rs` 0.62 -> future-version migrations.

The IOCP CI design (#1900) priced in 4-5 added minutes for the same
class of failure-mode coverage. The combined PR cost is under 10
minutes added per PR after both jobs are warm-cached, which is well
within the budget for a tier-1 platform.

## 8. Rollout

Phase 1 (week 0): land the job marked non-required.

- The `windows-acl-xattr` job is added to `ci.yml` but not added
  to the GitHub branch protection required-checks list. PR authors
  see it in the checks UI and can use it for diagnostics, but a
  red `windows-acl-xattr` does not block merge.
- This mirrors the rollout shape of historic Windows additions
  (e.g. when `windows-gnu-cross-check` was first added at
  `ci.yml:216`).

Phase 2 (week 0 - week 1): observe.

- Monitor the job over a 7-day window. The bar is "no flake-driven
  red runs". A genuine regression caught is a success, not a flake.
- If the job goes red on a PR, the author is expected to investigate
  but is not blocked.
- Concurrent #1866 work (Windows ACL in progress) lands behind this
  job. The job is the regression net for that work.

Phase 3 (week 1+): promote to required.

- After 7 consecutive days of green or genuine-regression-only
  reds, the job is added to branch-protection required checks.
- Promotion is paired with a doc note in the same PR that flips the
  required-status bit; no code change needed.
- Any new flake reverts the job to non-required and the 7-day
  clock restarts.

Phase 4 (post-#1866): expand the toolchain matrix.

- After Windows ACL implementation completes (#1866) and the job
  has been required and stable for 14 days, expand the toolchain
  matrix from `[stable]` to `[stable, beta, nightly]` to mirror
  `windows-test:170-173`. The beta and nightly legs use
  `continue-on-error: true` so a churning nightly compiler does not
  block PR throughput, matching the existing pattern at
  `windows-test`.

The same sequence applies to #1900 (IOCP) on its own timeline. The
two jobs are decoupled, so phase progression is independent.

## 9. Open questions

1. **windows-server-2022 vs windows-latest.** GitHub-hosted
   `windows-latest` currently aliases `windows-2022`. NTFS ADS and
   ACL semantics are stable across Server 2019 / 2022 / 2025, so the
   alias is fine for now. If GitHub flips the alias to
   `windows-2025`, the cache key (`ci-windows-acl-xattr-cargo-...`)
   stays valid because Cargo.toml hashing dominates. Open question:
   pin the runner image explicitly to insulate from the alias flip,
   or accept the small image-version drift in exchange for not
   maintaining a runner-version map. Recommendation: accept the
   drift; the metadata crate has no runner-version-specific code
   path.
2. **Privilege requirements for full DACL writes.** Writing the
   system ACL (SACL) requires `SE_SECURITY_NAME`, which the GitHub
   runner does not grant by default. The current implementation
   restricts itself to DACLs (`acl_windows.rs:14-20`); SACL
   preservation is deliberately left as follow-on work. Open
   question: do we add a smoke test that asserts the SACL path
   gracefully falls back when the privilege is absent? The
   skip-on-unsupported pattern at `xattr_windows.rs:368-378`
   (FAT32 ADS check) is the model.
3. **Filesystem capability detection.** The runner image's `C:\`
   drive is NTFS, so ADS works. Temp dirs created by `tempfile`
   inherit NTFS. The
   `fn ads_supported(file: &Path) -> bool` probe at
   `xattr_windows.rs:368-378` already handles non-NTFS gracefully,
   so the test infrastructure is robust. Open question: do we
   force-create a FAT32-backed VHD as part of the job to exercise
   the unsupported path? Recommendation: no; the unit-level
   `ads_supported` probe is sufficient. Filesystem-injected tests
   are a #1871-style stress concern, not a parity concern.
4. **Symlink handling under non-elevated runners.** Windows
   symlinks require either developer-mode or `SeCreateSymbolicLink`.
   GitHub-hosted runners enable developer-mode by default, but
   self-hosted runners may not. Open question: gate the symlink-
   following ACL tests on a runtime check rather than an
   unconditional `[cfg(windows)]`. Recommendation: gate the
   subset of ACL tests that exercise symlinks (`sync_acls(...,
   follow_symlinks=true)`) on a runtime probe identical in shape
   to `ads_supported`. The probe lives next to the affected tests
   and skips with an `eprintln!` rather than a hard failure.
5. **Interaction with `_test-features.yml`.** The feature-flag
   matrix runs only on Linux. Open question: do we add a
   `windows-features` matrix that tests the same combinations on
   `windows-latest`? The cost is roughly 7 jobs * 5 minutes each
   = 35 added minutes per PR. Recommendation: deferred to a
   separate task. The `windows-acl-xattr` job covers the two
   feature axes that touch Windows-only FFI; the rest of the
   feature matrix is Linux-tested and cross-compiled via
   `windows-gnu-cross-check`.
6. **Coverage reporting.** `cargo llvm-cov` runs on Linux only
   today (workflow `coverage.yml`). The 95%-line-coverage target
   in the project's quality bar is measured on Linux. Open
   question: do we extend coverage measurement to Windows? The
   tooling is supported on `windows-msvc` but the artifact
   merge across platforms is non-trivial. Recommendation: defer.
   Track Windows-only coverage manually for now via the unit test
   counts (13 ACL tests + 17 xattr tests = 30 baseline units, all
   running after #1869 lands).
7. **Interop coverage for #1518-#1521 follow-ups.** The Linux
   interop tests for ACL/xattr 3.0.x compatibility (#1391-#1394,
   merged) and the broader interop suite (#1518-#1521, merged) all
   run against upstream rsync via `_interop.yml` on Linux only. Open
   question: is there a Windows-side interop story? Upstream rsync
   has no native Windows port, so the answer for now is "no";
   wire-parity through the cross-check job (section 5) is the
   substitute. If a Cygwin or MSYS2 rsync path is later considered
   for daemon-side interop, the job structure here is the template.

## 10. References

- Upstream rsync ACL semantics: `acls.c:472-1000` in
  `target/interop/upstream-src/rsync-3.4.1/`. Cited inline at
  `crates/metadata/src/acl_windows.rs:39-43` and
  `acl_windows.rs:364`.
- Upstream rsync xattr semantics: `xattrs.c:rsync_xal_get` and
  `set_xattr`, cited at
  `crates/metadata/src/xattr.rs:117-120, 132-136, 147-153`.
- IOCP CI matrix (sibling work, #1900): the chosen separate-job
  pattern matches the IOCP rollout. See the IOCP transfer-pipeline
  wiring design for the analogous structure (the same
  `windows-latest` runner, the same cache shape, the same 7-day
  rollout cadence).
- Tasks: #1391-#1394 (ACL/xattr 3.0.x compat, completed),
  #1518-#1521 (interop tests on Linux, completed),
  #1866 (Windows ACL implementation, in progress),
  #1867 (Windows xattrs via NTFS ADS, completed),
  #1869 (this work),
  #1900 (IOCP CI matrix entry, pending).
- Workflow paths cited: `.github/workflows/{ci,_test-features,_interop}.yml`.
- Crate paths cited: `crates/metadata/{Cargo.toml,src/acl_windows.rs,src/xattr_windows.rs,src/xattr.rs,src/lib.rs,src/fake_super.rs,tests/acl_handling.rs}` and `crates/protocol/src/acl/wire/{send,types}.rs`.
