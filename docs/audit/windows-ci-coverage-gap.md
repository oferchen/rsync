# Windows CI Test Coverage Gap Audit (WSD-1)

Audit of Windows CI test coverage compared to Linux CI coverage.
Identifies crates, features, and test modules that lack Windows exercise.

## 1. CI Job Comparison

### Linux CI Coverage

| Job | Scope | Required |
|-----|-------|----------|
| `nextest (stable/beta/nightly)` | `--workspace --all-features` | stable = required |
| `Linux musl (stable/beta/nightly)` | `--workspace` (no io_uring/iocp) | stable = required |
| `interop` | Full upstream interop (daemon, SSH, xattr, ACL, hardlinks) | yes |
| `coverage` | `--workspace --all-features` (llvm-cov) | no (informational) |
| Feature flags (linux-only) | io_uring, copy_file_range, landlock, openssl, zlib-ng, zlib-rs, flat-flist, parallel, compression, incremental-flist, no-default-features, default-features | no |
| SSH integration tests | Loopback SSH with `--run-ignored` | no (continue-on-error) |

### Windows CI Coverage

| Job | Scope | Required |
|-----|-------|----------|
| `Windows (stable/beta/nightly)` | `-p core -p engine -p cli --all-features` | stable = required |
| `Windows IOCP` | `-p fast_io --no-default-features --features iocp` + `-p transfer --all-features` | yes |
| `Windows ACL/xattr` | `-p metadata --features acl,xattr` | yes |
| `Windows GNU cross-check` | `cargo check --workspace` (build only, no tests) | yes |
| `interop (Windows, best-effort)` | Smoke subset via MSYS2 rsync | no (continue-on-error) |
| `benchmark (Windows)` | Throughput only, no correctness | no (continue-on-error) |
| Feature flags (cross-OS) | async, tracing, serde, concurrent-sessions, daemon-tls | no |
| `DG-3 stress` | engine only (parallel applier stress) | no (continue-on-error) |

## 2. Per-Crate Coverage Comparison

The Linux CI runs the full workspace (all 25 crates). Windows CI only runs
a subset. Below is the coverage matrix:

| Crate | Total Tests | Unix-Gated Tests | Tested on Windows CI | Windows Gap |
|-------|-------------|------------------|---------------------|-------------|
| protocol | 5,828 | 31 | no (cross-OS feature rows only) | **critical** |
| engine | 4,707 | 458 | yes (windows-test) | partial - 458 tests skipped |
| cli | 3,316 | 72 | yes (windows-test) | partial - 72 tests skipped |
| core | 2,470 | 22 | yes (windows-test) | partial - 22 tests skipped |
| transfer | 1,836 | 97 | IOCP job only (--all-features) | partial |
| daemon | 1,480 | 90 | no | **critical** |
| checksums | 1,421 | 0 | no | **high** |
| bandwidth | 1,300 | 0 | no | **high** |
| filters | 1,284 | 0 | no | **high** |
| fast_io | 1,080 | 10 | IOCP job (--no-default-features --features iocp) | partial |
| rsync_io | 1,066 | 51 | no | **high** |
| compress | 965 | 0 | no | **high** |
| metadata | 783 | 92 | yes (windows-acl-xattr) | partial - 92 tests skipped |
| flist | 568 | 42 | no | **medium** |
| matching | 355 | 0 | no | **medium** |
| logging | 321 | 0 | no | **low** |
| signature | 237 | 0 | no | **low** |
| branding | 232 | 0 | no | **low** |
| batch | 200 | 0 | no | **medium** |
| logging-sink | 136 | 0 | no | **low** |
| platform | 58 | 18 | no | **medium** |
| apple-fs | 37 | 2 | no (macOS-only) | n/a |
| embedding | 36 | 0 | no | **low** |
| windows-gnu-eh | 1 | 0 | no (GNU ABI only) | n/a |

**Summary**: Of 25 crates, Windows CI tests only **5 crates** directly
(core, engine, cli, fast_io, metadata). Two additional crates (transfer,
protocol/flist/daemon) get partial exercise through cross-OS feature flag
rows. The remaining **~15 crates** have zero test execution on Windows.

## 3. Test Modules Gated Behind `#[cfg(unix)]`

985 individual `#[test]` functions are preceded by `#[cfg(unix)]` and never
run on Windows. Distribution by crate:

| Crate | Unix-Only Tests | Category |
|-------|-----------------|----------|
| engine | 458 | Delete ops, hard links, permissions, local-copy, spill |
| transfer | 97 | Symlink preservation, sandbox attacks, dir_sandbox |
| metadata | 92 | UID/GID integration, ownership, ACL handling |
| daemon | 90 | Auth, chroot, filter merge, itemize, pre-xfer-exec |
| cli | 72 | Frontend output identity, symlink tests |
| rsync_io | 51 | SSH stderr, config lookup |
| flist | 42 | Batched stat, symlinks, special chars, file walker |
| protocol | 31 | Wire mode, file entry tests |
| core | 22 | INC_RECURSE stress, error recovery, SSH transfer |
| platform | 18 | Name resolution, privilege |
| fast_io | 10 | Splice integration, io_uring stub modules |
| apple-fs | 2 | macOS-only (expected) |

### Entire Test Files Gated `#![cfg(unix)]`

These files are completely excluded from Windows compilation:

- `crates/engine/tests/spill_env_e2e.rs`
- `crates/engine/tests/delete_determinism_property.rs`
- `crates/engine/tests/delete_poison_recovery.rs`
- `crates/transfer/tests/dir_sandbox_carrier.rs`
- `crates/transfer/tests/fstatat_swap_resistance.rs`
- `crates/transfer/tests/sec_1_m_symlink_swap_attack.rs`
- `crates/transfer/tests/sec_1_n_legitimate_symlinks_interop.rs`
- `crates/transfer/tests/symlink_preservation.rs`
- `crates/transfer/tests/unlinkat_swap_resistance.rs`
- `crates/transfer/tests/delete_sandbox_swap.rs`
- `crates/flist/tests/symlink_handling.rs`
- `crates/flist/tests/special_characters.rs`
- `crates/core/tests/sigint_temp_cleanup.rs`
- `crates/core/tests/ssh_transfer.rs`
- `crates/metadata/tests/uidgid_integration.rs`
- `crates/rsync_io/tests/ssh_stderr_default_path.rs`

## 4. Features Not Tested on Windows

### Linux-Only Features (by design)

| Feature | Reason |
|---------|--------|
| io_uring | Linux 5.6+ kernel API |
| copy_file_range | Linux syscall (though fallback exists) |
| landlock | Linux LSM |
| splice/vmsplice | Linux zero-copy pipe API |
| sendfile | Linux/macOS (not Windows) |
| openssl / openssl-vendored | Linux-only CI row (could be cross-platform) |
| zlib-ng / zlib-rs | Linux-only CI row (could be cross-platform) |

### Features That Could Run on Windows But Do Not

| Feature | Crates Affected | Risk |
|---------|-----------------|------|
| `--workspace` (full suite) | All 25 crates | High - protocol/filters/checksums etc. never tested |
| compression (zstd, lz4) | compress, protocol, engine, transfer | Medium - pure algorithmic code |
| parallel | checksums, flist | Medium - threading model differences |
| incremental-flist | transfer | Medium |
| flat-flist | protocol, transfer, filters | Low |
| no-default-features | workspace | Medium - compile-only on GNU cross-check |
| default-features | workspace | Medium |
| daemon mode | daemon | High - explicitly refuses `--daemon` on Windows |

### Interop Gaps

The Windows interop job is best-effort (non-required) and intentionally
skips:

- Symlinks and hardlinks
- oc-rsync daemon scenarios (daemon refuses `--daemon` on Windows)
- SSH loopback (no sshd on windows-latest)
- xattr / ACL direction parity (exercised separately in windows-acl-xattr)
- `--list-only` format parity

## 5. Quantified Gap

| Metric | Linux | Windows | Gap |
|--------|-------|---------|-----|
| Crates with test execution | 25 | 5-7 | 18-20 crates untested |
| Total test functions (approx) | 29,900 | ~12,300 (core+engine+cli+transfer+fast_io+metadata) | ~17,600 tests never run |
| Unix-gated tests (never on Windows) | 985 | 0 | 985 tests unreachable |
| Linux-gated tests (never on Windows) | 33 | 0 | 33 tests unreachable |
| Required interop scenarios | Full suite | 0 (best-effort only) | All interop non-required |
| Feature flag rows on Windows | 5 (cross-OS) | 13 (linux-only) | 8 feature combos untested |
| Coverage measurement | Yes (llvm-cov) | No | No coverage tracking |

## 6. Prioritized Recommendations

### P0 - Critical (blocks correctness confidence)

1. **Add `protocol` crate to Windows test matrix.**
   The `protocol` crate has 5,828 tests and handles wire format encoding,
   which must work identically on Windows. Only 31 tests are unix-gated.
   Add `-p protocol` to the `windows-test` job.

2. **Add `filters` crate to Windows test matrix.**
   The `filters` crate has 1,284 tests with zero unix gates. Path
   filtering logic is critical for correct Windows operation. All tests
   should compile and pass on Windows.

3. **Add `checksums` crate to Windows test matrix.**
   The `checksums` crate has 1,421 tests with zero unix gates. SIMD
   fast-paths (SSE2/AVX2) exist on x86_64 Windows and need validation.

### P1 - High (significant blind spots)

4. **Add `daemon` crate to Windows test matrix.**
   1,480 tests (90 unix-gated). While daemon mode refuses to start on
   Windows, the crate contains protocol negotiation, authentication, and
   configuration parsing logic used by the client side.

5. **Add `rsync_io` crate to Windows test matrix.**
   1,066 tests (51 unix-gated). Contains SSH transport logic, which is
   relevant on Windows (PuTTY/OpenSSH for Windows).

6. **Add `compress` crate to Windows test matrix.**
   965 tests with zero unix gates. Compression codecs (zlib, zstd, lz4)
   are platform-agnostic and should pass identically on Windows.

7. **Add `bandwidth` crate to Windows test matrix.**
   1,300 tests with zero unix gates. Rate limiting is pure algorithmic
   code.

### P2 - Medium (defense in depth)

8. **Add `flist`, `matching`, `batch`, `platform` to Windows CI.**
   These crates have moderate test counts and are relevant to Windows
   operation.

9. **Promote Windows interop to a required check.**
   Currently best-effort. Once baseline parity is green, make it a merge
   gate.

10. **Add `compression` feature flag to cross-OS matrix.**
    zstd/lz4 are platform-agnostic; the linux-only row is an artifact
    of the initial CI setup.

11. **Add Windows coverage measurement.**
    No llvm-cov run exists for Windows. Even a non-blocking report would
    surface gaps in platform-specific code paths.

### P3 - Low (nice to have)

12. **Add `logging`, `signature`, `branding`, `logging-sink`, `embedding`
    to Windows CI.**
    These are platform-agnostic utility crates with no unix gates.

13. **Consider `--workspace` on Windows.**
    The most direct fix is switching from `-p core -p engine -p cli` to
    `--workspace` in the Windows test job. This was intentionally scoped
    to reduce CI minutes, but the coverage gap is now quantified and
    significant. A phased approach (add P0 crates first, then expand)
    reduces risk.

14. **Audit the 985 unix-gated tests for Windows equivalents.**
    Many test symlinks, permissions, or UID/GID - concepts that have
    Windows analogs (junctions, DACL, SID). Some could be adapted to
    run on Windows with platform-specific setup.

## 7. CI Runtime Budget Consideration

The current Windows test job takes ~45 minutes with 3 crates. Adding
`--workspace` would likely push this to 60-90 minutes due to compilation
of all crates. Two viable strategies:

1. **Split into two Windows jobs**: one for platform-specific crates
   (core, engine, cli, metadata, fast_io) and one for platform-agnostic
   crates (protocol, filters, checksums, compress, bandwidth, etc.).

2. **Gradual expansion**: add P0 crates to the existing job first, measure
   the runtime delta, then decide whether to split.

The cross-OS feature flag matrix (`_test-features.yml`) already covers 5
feature combinations on all three OS. This is the right pattern to extend.
