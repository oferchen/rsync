# CI and Interop Hardening Analysis

**Date**: 2025-11-25
**Status**: ANALYSIS COMPLETE - Ready for implementation
**Phase**: Phase 4 of rsync 3.4.1 parity

## Executive Summary

The oc-rsync project has strong test infrastructure with 2935 tests passing and comprehensive interop test scripts. However, the interop tests are **not integrated into CI**, representing a significant gap in upstream compatibility validation. This document provides a complete analysis and actionable plan to harden CI and restore interop testing.

## Current CI State

### `.github/workflows/ci.yml`

**Status**: ✅ WORKING

**Jobs**:
- **lint-and-test**: Runs on `ubuntu-latest` for pushes/PRs
  - `cargo fmt --all -- --check` ✅
  - `cargo clippy --workspace --all-targets --all-features --no-deps` ✅
  - `cargo nextest run --workspace --all-targets --all-features` ✅
  - Environment: `RUSTFLAGS="-D warnings"`
  - Uses `Swatinem/rust-cache@v2` for caching

**Test Coverage**: 2935 tests across workspace

**Strengths**:
- Comprehensive workspace test coverage
- Strict linting (warnings as errors)
- Fast with caching
- Concurrency control to avoid redundant runs

**Gaps**:
1. **No interop testing** against upstream rsync versions
2. **No protocol version testing** (28, 29, 30, 31, 32)
3. **No cross-version compatibility matrix**
4. **No SSH transport testing** (only local copies)
5. **No performance regression testing**
6. **Single platform** (ubuntu-latest only)

### `.github/workflows/build-cross.yml`

**Status**: ✅ WORKING

**Jobs**:
- **linux**: x86_64 and aarch64, `.deb`, `.rpm`, `.tar.gz`
- **macos**: x86_64 and aarch64, `.tar.gz`
- **windows**: x86_64, `.tar.gz`, `.zip`
- **release-artifacts**: Upload to GitHub Releases
- **brew-formula**: Generate Homebrew formula

**Trigger**: Manual or tag `v*.*.*-rust`

**Strengths**:
- Multi-platform release artifacts
- Automated package generation
- SBOM support (via `cargo xtask sbom`)

**Gaps**:
1. **No artifact testing** before release
2. **No smoke tests** on cross-platform binaries
3. **No integration with interop tests**

### `.config/nextest.toml`

**Status**: ✅ MINIMAL BUT CORRECT

```toml
[profile.default]
default-filter = "all()"
status-level = "fail"
final-status-level = "fail"
failure-output = "immediate-final"
```

**Strengths**:
- Runs all tests by default
- Clear failure reporting
- Works for both local and CI

**Gaps**:
1. **No CI-specific profile** with JUnit XML output
2. **No slow-test categorization**
3. **No interop-specific profile**

## Interop Testing Infrastructure

### Existing Scripts

#### 1. `scripts/rsync-interop-orchestrator.sh`
- **Purpose**: Multi-daemon orchestrator
- **Features**:
  - Starts daemons for all versions (3.0.9, 3.1.3, 3.4.1)
  - Writes per-version environment descriptors
  - Cleans up all daemons on exit
- **Status**: ✅ COMPLETE, not integrated into CI

#### 2. `scripts/rsync-interop-server.sh`
- **Purpose**: Start oc-rsync and upstream daemons
- **Features**:
  - Fetches upstream binaries (Debian/Ubuntu packages or source build)
  - Starts bidirectional daemons (oc-rsync ↔ upstream)
  - Proper `--port` handling for non-privileged ports
  - Fallback to source build if packages unavailable
- **Status**: ✅ COMPLETE, not integrated into CI

#### 3. `scripts/rsync-interop-client.sh`
- **Purpose**: Run bidirectional interop tests
- **Features**:
  - Tests upstream client → oc-rsync daemon
  - Tests oc-rsync client → upstream daemon
  - Validates file transfer success
  - Aggregates failures for CI reporting
- **Status**: ✅ COMPLETE, not integrated into CI

#### 4. `tools/ci/run_interop.sh`
- **Purpose**: Self-contained CI interop runner
- **Features**:
  - Sequential test cases (no orchestrator needed)
  - Starts/stops daemons per version
  - Tests both directions
  - Same upstream binary acquisition logic
- **Status**: ✅ COMPLETE, not integrated into CI

### Interop Test History

**Key Events**:
- **Added**: Commit `2f219cad` - "Add short-option clustering and interop CI harness"
- **Updated**: Multiple commits improving `.deb` extraction, tarball handling
- **Deleted**: Commit `f90ecd94` - Deleted `.github/workflows/interop.yml`
  - Reason: Workflow had `|| true` (always passed, dead code)
  - Was `workflow_call` only, not triggered anywhere

### `docs/INTEROP.md`

**Test Matrix**:
| Scenario | Result |
|----------|--------|
| Local copy (archive mode) | ✅ |
| Sparse file round-trip | ⚠️ layout (NOW FIXED ✅) |
| Remote copy via SSH | ✅ |
| Daemon transfer | ⚠️ auth (NOW FIXED ✅) |
| Filters | ✅ |
| Compression | ✅ |
| Metadata | ⚠️ ACL |
| Delete options | ✅ |
| File list diffing | ✅ |
| Exit code match | ✅ |

**Known Gaps (from doc)**:
- ~~Daemon auth secrets not enforced~~ ✅ FIXED (Phase 3)
- ACLs partially implemented ⚠️ ONGOING
- ~~Sparse hole layout not guaranteed~~ ✅ FIXED (Phase 2)
- Filter `--filter='merge ...'` edge cases ⚠️ ONGOING

**Status**: Document is outdated, needs refresh based on current state

## Gap Analysis

### Critical Gaps

#### 1. No CI Interop Testing ❌
**Impact**: HIGH
- Upstream compatibility regressions undetected until manual testing
- No validation against protocol versions 28-32
- No bidirectional (client/daemon) compatibility checks

**Evidence**:
- `.github/workflows/interop.yml` was deleted
- `tools/ci/run_interop.sh` exists but not called
- All interop infrastructure unused in CI

**Risk**:
- Breaking changes to protocol negotiation undetected
- Daemon compatibility issues with upstream clients
- Message format changes breaking scripts

#### 2. Single Platform Testing ❌
**Impact**: MEDIUM
- CI only runs on `ubuntu-latest` (Linux x86_64)
- Platform-specific bugs (macOS, Windows) undetected
- Architecture-specific issues (aarch64) undetected

**Evidence**:
- `ci.yml` hardcoded to `ubuntu-latest`
- No macOS or Windows test jobs

**Risk**:
- Path handling differences (Windows)
- Filesystem permission issues (macOS vs Linux)
- SIMD acceleration bugs (ARM)

#### 3. No Protocol Version Matrix ❌
**Impact**: MEDIUM
- Only protocol 32 tested implicitly
- No explicit tests for protocols 28, 29, 30, 31
- No version negotiation edge case testing

**Evidence**:
- Protocol constants defined but no CI validation
- No protocol negotiation property tests in CI

**Risk**:
- Older clients fail to connect
- Protocol fallback broken
- Version negotiation deadlocks

#### 4. No Performance Regression Detection ❌
**Impact**: LOW (but important for production)
- Performance regressions undetected
- SIMD optimizations not validated for speed
- Bandwidth limiter accuracy not measured

**Evidence**:
- No criterion benchmarks in CI
- No transfer speed baselines
- No comparison with upstream rsync

**Risk**:
- Gradual performance degradation
- Optimization regressions
- SIMD fast paths broken

#### 5. No SSH Transport Testing ❌
**Impact**: HIGH (once native SSH implemented)
- Currently blocked: SSH not natively implemented (uses fallback)
- Future risk: No SSH transport validation in CI

**Evidence**:
- `SSH_TRANSPORT_ARCHITECTURE.md` shows infrastructure exists
- No SSH-based interop tests
- Only daemon (rsync://) protocol tested

**Risk**:
- SSH transport integration bugs undetected
- Remote operand parsing failures
- Protocol-over-SSH negotiation issues

### Minor Gaps

#### 6. No JUnit XML Output ⚠️
- Test results not machine-readable for dashboards
- No test result archiving
- No failure trend analysis

#### 7. No Slow Test Isolation ⚠️
- All tests run together
- No fast/slow separation for quick feedback
- No ability to skip slow tests locally

#### 8. No Release Artifact Smoke Tests ⚠️
- Cross-platform binaries built but not tested
- No `--version` check after packaging
- No basic transfer validation before release

## CI Hardening Plan

### Phase 1: Restore Interop Testing (HIGH PRIORITY)

**Goal**: Integrate interop tests into CI to catch upstream compatibility regressions

**Tasks**:
1. **Add interop job to `ci.yml`** (1 hour)
   - New job: `interop-upstream` that depends on `lint-and-test`
   - Uses `tools/ci/run_interop.sh`
   - Runs on pushes to `master`/`main` and PRs
   - Allowed to fail initially (`continue-on-error: true`) during stabilization
   - Remove `continue-on-error` once tests are stable

2. **Update `.config/nextest.toml`** (30 minutes)
   - Add `[profile.ci]` with JUnit XML output
   - Add `[profile.interop]` for interop-specific configuration

3. **Update `INTEROP.md`** (30 minutes)
   - Mark daemon auth and sparse files as FIXED
   - Update test matrix with current results
   - Add CI integration section

**Success Criteria**:
- Interop tests run on every PR
- Tests validate against rsync 3.0.9, 3.1.3, 3.4.1
- Both directions tested (oc-rsync client/daemon ↔ upstream daemon/client)
- Test failures block merge (after stabilization)

**Estimated Effort**: 2 hours

### Phase 2: Multi-Platform Testing (MEDIUM PRIORITY)

**Goal**: Test on multiple OS and architectures to catch platform-specific bugs

**Tasks**:
1. **Add macOS test job** (1 hour)
   - New job: `test-macos` in `ci.yml`
   - Runs on `macos-latest`
   - Same test suite as Linux
   - Uses native macOS filesystem

2. **Add Windows test job** (1-2 hours)
   - New job: `test-windows` in `ci.yml`
   - Runs on `windows-latest`
   - Handle path differences (`\` vs `/`)
   - Skip Unix-specific tests (permissions, symlinks)

3. **Add architecture matrix** (1 hour)
   - Linux: x86_64 (existing) + aarch64 (via cross-compilation or QEMU)
   - macOS: x86_64 + aarch64 (native GitHub runners)

**Success Criteria**:
- Tests pass on Linux, macOS, Windows
- Tests pass on x86_64 and aarch64
- Platform-specific issues detected before merge

**Estimated Effort**: 3-4 hours

### Phase 3: Protocol Version Matrix (MEDIUM PRIORITY)

**Goal**: Explicitly test all protocol versions (28-32) for compatibility

**Tasks**:
1. **Add protocol negotiation tests** (2 hours)
   - New test module: `crates/protocol/src/tests/versions.rs`
   - Property tests for each protocol version
   - Edge cases: version mismatch, fallback, unknown versions

2. **Add interop protocol tests** (1-2 hours)
   - Extend `run_interop.sh` to force specific protocol versions
   - Test oc-rsync with `--protocol=28/29/30/31/32` flags
   - Validate upstream rsync accepts each version

3. **Document protocol matrix** (30 minutes)
   - Create `docs/PROTOCOL_MATRIX.md`
   - Show compatibility grid: oc-rsync × upstream versions × protocols

**Success Criteria**:
- All protocols 28-32 tested
- Version negotiation validated
- Fallback behavior tested
- Protocol compatibility documented

**Estimated Effort**: 3-4 hours

### Phase 4: Performance Benchmarks (LOW PRIORITY)

**Goal**: Detect performance regressions and validate optimizations

**Tasks**:
1. **Add criterion benchmarks** (3-4 hours)
   - Benchmark suite: `benches/` in relevant crates
   - Target: checksums, compression, delta generation
   - Baseline: Save results for comparison

2. **Add CI benchmark job** (1-2 hours)
   - New job: `benchmarks` in `ci.yml`
   - Runs on stable hardware (avoid variance)
   - Compares against baseline
   - Fails if >10% regression

3. **Add transfer speed tests** (2-3 hours)
   - Compare oc-rsync vs upstream rsync
   - Test scenarios: large files, many small files, sparse files
   - Measure: throughput (MB/s), CPU usage, memory

**Success Criteria**:
- Benchmarks run on every PR
- Regressions detected and reported
- Performance within 10% of upstream rsync

**Estimated Effort**: 6-9 hours

### Phase 5: Release Artifact Validation (MEDIUM PRIORITY)

**Goal**: Test cross-platform binaries before releasing

**Tasks**:
1. **Add smoke tests to `build-cross.yml`** (2 hours)
   - After building each platform binary:
     - Run `oc-rsync --version`
     - Perform basic local copy
     - Validate exit code 0

2. **Add package validation** (1-2 hours)
   - Test `.deb` installation on Ubuntu
   - Test `.rpm` installation on Fedora (via Docker)
   - Test macOS `.tar.gz` extraction and execution
   - Test Windows `.zip` extraction and execution

3. **Add xtask command** (1 hour)
   - `cargo xtask validate-artifacts`
   - Checks all artifacts in `target/dist/`
   - Runs smoke tests on each

**Success Criteria**:
- All release artifacts smoke-tested
- Package installation validated
- Failures prevent release

**Estimated Effort**: 4-5 hours

### Phase 6: SSH Transport Testing (DEFERRED)

**Goal**: Test SSH transport once native implementation complete

**Tasks**:
- Add SSH-based interop tests
- Test remote operand parsing
- Test protocol-over-SSH negotiation
- Validate `--rsync-path` handling

**Status**: BLOCKED by SSH transport implementation (see `SSH_TRANSPORT_ARCHITECTURE.md`)

**Estimated Effort**: 2-3 hours (after SSH implementation)

## Implementation Recommendations

### Immediate Actions (Do Now)

**Phase 1 should be implemented immediately** because:
1. Infrastructure already exists (scripts are ready)
2. Minimal effort (2 hours)
3. High impact (catches upstream compatibility regressions)
4. User reported `--rsync-path` issue highlights need for interop validation

**Steps**:
1. Create feature branch: `ci/restore-interop-tests`
2. Add interop job to `.github/workflows/ci.yml`
3. Update `.config/nextest.toml` with CI profile
4. Update `docs/INTEROP.md` to reflect current state
5. Test locally with `tools/ci/run_interop.sh`
6. Push and verify CI passes
7. Merge with `continue-on-error: true` initially
8. Remove `continue-on-error` after stabilization

### Short-term Actions (This Week)

**Phase 2 (Multi-platform)** should follow immediately because:
1. Platform-specific bugs are hard to debug after merge
2. macOS and Windows users need assurance
3. GitHub Actions provides free runners

**Phase 5 (Artifact validation)** should be prioritized because:
1. Release artifacts are public-facing
2. Broken packages harm project reputation
3. Low effort, high value

### Medium-term Actions (Next Month)

**Phase 3 (Protocol matrix)** should be scheduled because:
1. Protocol compatibility is critical for interop
2. Testing is lightweight (no external dependencies)
3. Provides confidence for multi-version support

### Long-term Actions (When Needed)

**Phase 4 (Performance)** can be deferred because:
1. Functional correctness is higher priority
2. Benchmarking requires careful setup
3. Performance is already good (SIMD, delta compression)

**Phase 6 (SSH transport)** is blocked by implementation work:
1. Native SSH transport not yet integrated (infrastructure exists)
2. Current fallback mechanism works
3. Should be added when SSH implementation complete

## Proposed `.github/workflows/ci.yml` Changes

```yaml
jobs:
  lint-and-test:
    # ... existing job ...

  interop-upstream:
    name: interop with upstream rsync
    runs-on: ubuntu-latest
    needs: lint-and-test
    # Remove continue-on-error once tests are stable
    continue-on-error: true

    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust (stable)
        uses: dtolnay/rust-toolchain@stable

      - name: Rust cache
        uses: Swatinem/rust-cache@v2

      - name: Install build dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            build-essential \
            pkg-config \
            zlib1g-dev \
            curl \
            autoconf \
            automake \
            libtool \
            libacl1-dev \
            libattr1-dev

      - name: Run interop tests
        run: bash tools/ci/run_interop.sh

  test-macos:
    name: test on macOS
    runs-on: macos-latest

    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust (stable)
        uses: dtolnay/rust-toolchain@stable

      - name: Rust cache
        uses: Swatinem/rust-cache@v2
        with:
          key: test-macos

      - name: Install cargo-nextest
        run: cargo install cargo-nextest --locked

      - name: Nextest
        run: cargo nextest run --workspace --all-targets --all-features

  test-windows:
    name: test on Windows
    runs-on: windows-latest

    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Rust (stable)
        uses: dtolnay/rust-toolchain@stable

      - name: Rust cache
        uses: Swatinem/rust-cache@v2
        with:
          key: test-windows

      - name: Install cargo-nextest
        run: cargo install cargo-nextest --locked

      - name: Nextest
        run: cargo nextest run --workspace --all-targets --all-features
```

## Proposed `.config/nextest.toml` Changes

```toml
[profile.default]
default-filter = "all()"
status-level = "fail"
final-status-level = "fail"
failure-output = "immediate-final"

[profile.ci]
default-filter = "all()"
status-level = "fail"
final-status-level = "fail"
failure-output = "immediate-final"
success-output = "never"
# Generate JUnit XML for CI dashboards
junit.path = "junit.xml"

[profile.ci-fast]
default-filter = "not test(slow)"
status-level = "pass"
final-status-level = "fail"
failure-output = "immediate"
```

## Metrics

### Current State
- **Total tests**: 2935
- **Test coverage**: Comprehensive (all crates)
- **CI platforms**: 1 (ubuntu-latest)
- **CI runtime**: ~3-5 minutes (with cache)
- **Interop tests**: 0 (not integrated)
- **Protocol versions tested**: 1 (protocol 32 implicit)

### Target State (After Phase 1-3)
- **Total tests**: 2935 + interop tests
- **Test coverage**: Comprehensive + upstream compatibility
- **CI platforms**: 3 (Linux, macOS, Windows)
- **CI runtime**: ~10-15 minutes (with interop + multi-platform)
- **Interop tests**: 6 (3 versions × 2 directions)
- **Protocol versions tested**: 5 (protocols 28-32 explicit)

### Target State (After Phase 1-5)
- **CI runtime**: ~15-20 minutes (with benchmarks)
- **Performance baselines**: Established
- **Release validation**: 100% (all artifacts tested)
- **Benchmark coverage**: Critical paths (checksums, compression, delta)

## Dependencies

### Required for Phase 1 (Interop)
- Upstream rsync binaries (3.0.9, 3.1.3, 3.4.1)
  - Fetched via Debian/Ubuntu packages or built from source
  - Already handled by `run_interop.sh`
- Build dependencies: `build-essential`, `autoconf`, `pkg-config`, etc.
  - Available in ubuntu-latest runner
- Network access: Download packages/tarballs
  - Available in GitHub Actions

### Required for Phase 2 (Multi-platform)
- macOS runner: `macos-latest` (provided by GitHub)
- Windows runner: `windows-latest` (provided by GitHub)
- Cross-compilation tooling (if using QEMU for aarch64)

### Required for Phase 4 (Benchmarks)
- Criterion crate (already used in workspace)
- Stable hardware (GitHub runners are consistent enough)
- Baseline storage (GitHub Actions artifacts)

## Risks and Mitigations

### Risk 1: Interop Tests Flaky
**Mitigation**:
- Start with `continue-on-error: true`
- Fix flakiness before requiring pass
- Add retry logic for network failures

### Risk 2: CI Runtime Too Long
**Mitigation**:
- Run interop only on `master` pushes, not all PRs (initially)
- Use matrix parallelization
- Cache upstream binaries
- Skip slow tests on draft PRs

### Risk 3: Multi-platform Tests Fail
**Mitigation**:
- Start with `continue-on-error: true`
- Fix platform-specific issues incrementally
- Use platform-specific test filters

### Risk 4: Upstream Binary Availability
**Mitigation**:
- `run_interop.sh` already has multi-tier fallback:
  1. Try Debian/Ubuntu packages (fast)
  2. Try source tarball from rsync.samba.org
  3. Try git clone from GitHub
- Cache built binaries in GitHub Actions cache

## Conclusion

**Current State**: CI is functional but lacks upstream compatibility validation

**Gaps**: No interop testing, single platform, no protocol matrix, no performance tracking

**Priority**: Phase 1 (Interop) is **IMMEDIATE** - 2 hours to implement, high impact

**Effort**: Phases 1-3 total ~9 hours over next week, Phases 4-5 deferred

**Recommendation**: Implement Phase 1 immediately to catch upstream compatibility regressions and validate daemon fixes from Phase 3

**Next Steps**:
1. Create `ci/restore-interop-tests` branch
2. Implement Phase 1 changes
3. Test locally with `bash tools/ci/run_interop.sh`
4. Push and verify CI passes
5. Merge and monitor for stability
6. Remove `continue-on-error: true` after 1 week
7. Proceed to Phase 2 (Multi-platform)

## References

- CI workflows: `.github/workflows/ci.yml`, `.github/workflows/build-cross.yml`
- Interop scripts: `scripts/rsync-interop-*.sh`, `tools/ci/run_interop.sh`
- Interop docs: `docs/INTEROP.md`
- Session summary: `docs/SESSION_SUMMARY_2025-11-25.md`
- SSH analysis: `docs/SSH_TRANSPORT_ARCHITECTURE.md`
- Gap tracking: `docs/gaps.md`
