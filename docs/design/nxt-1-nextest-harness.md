# NXT-1: nextest harness for upstream-compat integration tests

Tracks: NXT-1 (parent NXT, sibling tasks NXT-2..NXT-8).

Companion: `docs/design/uts-nextest-edge-b-test-harness.md` (UTS-NEXTEST-EDGE.b).
That doc designs an *internal-only* operational harness (oc-rsync drives
oc-rsync). NXT-1 is the *cross-binary* harness: every test pits oc-rsync
against an installed upstream rsync release, mirroring the high-value edge
cases currently caught only by the nightly `tools/ci/run_upstream_testsuite.sh`
workflow. The two harnesses share primitives but have different goals; this
document specifies the cross-binary primitives, the existing internal
primitives are reused as-is.

## 1. Purpose and scope

`tools/ci/run_upstream_testsuite.sh` runs upstream's `runtests.py` against
oc-rsync. It is the authoritative compatibility gate but it only runs on the
nightly UTS workflow because it requires a `./configure`-built upstream source
tree and 5-15 minute wall time. Regressions in edge cases land on master
between nightly runs.

NXT-2..NXT-8 port the highest-value edge cases into nextest so they run on
every PR. NXT-1 specifies the shared harness those ports use. Concrete
targets identified for NXT-2..NXT-8:

| Sub-task | Upstream test | Edge case |
|---|---|---|
| NXT-2 | `daemon-gzip-download_test.py` | goodbye flush does not truncate trailing frame |
| NXT-3 | `exclude-lsh_test.py` | `--filter` rules survive lsh-style remote shell |
| NXT-4 | `daemon-refuse_test.py` | option-refusal under mux tag 72 |
| NXT-5 | `chmod-symlink-race_test.py` | dirfd sandbox holds across symlink swap |
| NXT-6 | `daemon-groupmap-wild_test.py` | secluded-args path with groupmap |
| NXT-7 | `batch-mode_test.py` | daemon `--write-batch` byte parity |
| NXT-8 | `alt-dest-symlinks_test.py` | symlink mtime via `--copy-dest` SSH mode |

Out of scope (non-goals):

- Not a replacement for `tools/ci/run_upstream_testsuite.sh`. The nightly UTS
  workflow remains authoritative; this harness is the PR-time guardrail for a
  curated subset.
- Not porting all 108 upstream tests. Selection is by regression risk.
- No wire-byte differential capture in v1 (deferred per UTS-NEXTEST-EDGE.c
  socketpair-tap approach).
- No new CI workflow file. The tests are part of the existing nextest cell but
  gated by an env var (section 6).

## 2. Existing primitives that NXT-1 reuses

- `crates/test-support/` already exposes `create_tempdir()` with Windows
  retry. Extend, do not replace.
- `crates/core/tests/common/mod.rs::TestDaemon` already spawns either upstream
  or oc-rsync via the `DaemonBinary` enum (`UPSTREAM_3_0_9`, `UPSTREAM_3_1_3`,
  `UPSTREAM_3_4_1` path constants). The strategy pattern there is the right
  shape; we lift it into `test-support` rather than duplicating in every
  crate's `common/`.
- `tools/ci/run_interop.sh` already builds the upstream binaries into
  `target/interop/upstream-install/{3.0.9,3.1.3,3.4.4}/bin/rsync`. That is the
  canonical artifact path the harness consumes.

The sibling `OcRsyncDaemonHarness` design (UTS-NEXTEST-EDGE.b section 3)
covers the internal daemon. NXT-1 extends the existing strategy enum so a
single `DaemonHarness` builder selects oc-rsync **or** upstream by version.

## 3. Harness shape

New module: `crates/test-support/src/upstream_compat.rs`. Public surface:

```rust
// crates/test-support/src/upstream_compat.rs

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Upstream rsync version pinned by an NXT-* test.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UpstreamVersion {
    V3_0_9,
    V3_1_3,
    V3_4_4,
}

/// Locate the upstream rsync binary for `version`.
///
/// Resolution order:
/// 1. `OC_RSYNC_UPSTREAM_BIN_<VERSION>` env var (CI override).
/// 2. `target/interop/upstream-install/<version>/bin/rsync` relative to the
///    workspace root resolved from `CARGO_MANIFEST_DIR`.
///
/// Returns `None` if neither path resolves to an executable. Tests then
/// self-skip via `require_upstream_rsync`.
pub fn locate_upstream_rsync(version: UpstreamVersion) -> Option<PathBuf>;

/// Resolved handle to an upstream rsync binary.
pub struct UpstreamRsync {
    binary: PathBuf,
    version: UpstreamVersion,
}

impl UpstreamRsync {
    pub fn binary(&self) -> &Path { &self.binary }
    pub fn version(&self) -> UpstreamVersion { self.version }
    pub fn command(&self) -> Command { Command::new(&self.binary) }
}

/// Self-skip helper. Returns `Some(UpstreamRsync)` when the binary exists,
/// else prints a clear reason and returns `None`. Tests early-return on `None`
/// so nextest reports them as passing with the skip reason in stderr.
#[must_use]
pub fn require_upstream_rsync(version: UpstreamVersion) -> Option<UpstreamRsync>;

/// Returns `true` when the env gate (section 6) is set. Tests early-return
/// without printing a skip line when this is false, matching upstream's
/// `WHICHTESTS` env-gating convention.
#[must_use]
pub fn upstream_compat_enabled() -> bool;
```

The harness also exposes a thin `DaemonHandle` wrapper that wraps either
oc-rsync (via `OcRsyncDaemonHarness` from UTS-NEXTEST-EDGE.b) or upstream
rsync as `--daemon`, parameterised by `DaemonBinary::Upstream(version)` or
`DaemonBinary::OcRsync`. The strategy enum mirrors the one in
`crates/core/tests/common/mod.rs`; lifting it into `test-support` lets every
NXT-* crate share one definition.

## 4. Test template (one fn per scenario)

Each NXT-2..NXT-8 test follows the same skeleton:

```rust
// crates/transfer/tests/upstream_compat/daemon_gzip_goodbye.rs

use test_support::{
    create_tempdir, require_upstream_rsync, upstream_compat_enabled,
    UpstreamVersion,
};

#[test]
fn daemon_gzip_goodbye_does_not_truncate() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_4_4) else {
        return;
    };

    let src = create_tempdir();
    let dst = create_tempdir();
    populate_tree(src.path());

    // 1. oc-rsync daemon serves `src`.
    let daemon = test_support::DaemonHarness::oc_rsync()
        .module("data", src.path())
        .spawn()
        .expect("oc-rsync daemon");

    // 2. Upstream client pulls with -z. Asserts:
    //    - exit code 0
    //    - dst content byte-equal to src
    //    - daemon log shows complete final frame (no truncation)
    let out = upstream
        .command()
        .arg("-az")
        .arg(format!("rsync://127.0.0.1:{}/data/", daemon.port()))
        .arg(format!("{}/", dst.path().display()))
        .output()
        .expect("upstream client");

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    test_support::DirDiff::compare(src.path(), dst.path(), Default::default())
        .expect("trees diverged");
    assert!(daemon.log_contents().expect("log").contains("final frame ok"));
}
```

Properties of the template:

- One `#[test]` per scenario. No `#[test_case]` parameterisation across
  versions; if a scenario needs to run against multiple upstream versions,
  write one test per version with a discoverable name suffix.
- The two early-returns (env gate then binary presence) are the only conditions
  under which the test exits successfully without asserting. Self-skip is
  visible in nextest stderr; no `#[ignore]`.
- Wall-time budget per test: <= 5 s P95. Hard cap 30 s via the runner's
  internal timeout (shared with the sibling harness).

## 5. Upstream binary provenance

In order of preference:

1. **CI pre-built artifact**. `tools/ci/run_interop.sh` already builds
   `target/interop/upstream-install/<version>/bin/rsync`. When the
   `upstream-compat` CI cell runs, that workflow either reuses a cached
   artifact or runs `build_upstream.sh` first. The cell is gated on a
   successful build before running nextest.
2. **Developer-local install**. Same path - developers run
   `tools/ci/run_interop.sh` once and the binaries persist under
   `target/interop/upstream-install/`. The harness uses
   `CARGO_MANIFEST_DIR` to resolve the workspace root, so the lookup works
   from any crate.
3. **Env override**. `OC_RSYNC_UPSTREAM_BIN_3_4_4=/usr/local/bin/rsync` lets
   contributors point at a system install when the local build is missing.
   The env var name encodes the version so multi-version tests do not collide.

If none of those resolves, `require_upstream_rsync()` returns `None`. The
test prints `Skipping daemon_gzip_goodbye_does_not_truncate: upstream rsync
3.4.4 not installed` and exits successfully. nextest reports the test as
passing; the skip is loud in stderr but does not fail CI.

The harness does **not** download the upstream tarball at test time. Network
fetches inside nextest are a flake source; we delegate to the
`run_interop.sh` build step which is already battle-tested.

## 6. CI wire-up

Tests live under `crates/<consumer>/tests/upstream_compat/`. They compile
unconditionally on every nextest cell but self-skip via
`upstream_compat_enabled()`, which returns `true` only when
`OC_RSYNC_UPSTREAM_COMPAT=1` is set in the environment.

- **Standard PR nextest cell**: env var unset, tests no-op early. No upstream
  binary required, no extra wall time.
- **New `upstream-compat` cell**: a separate matrix entry in the existing
  nextest workflow that:
  1. Runs `tools/ci/run_interop.sh` (build step) to produce the upstream
     binaries.
  2. Sets `OC_RSYNC_UPSTREAM_COMPAT=1`.
  3. Runs `cargo nextest run --workspace --all-features --test-threads=1`
     filtered to `package(test_name=~upstream_compat)`.

Adding a single cell to the existing workflow file keeps the change minimal.
The cell only runs on Linux (the only platform where `run_interop.sh`
currently builds upstream); macOS and Windows nextest cells run the same tests
but every one self-skips because the binary path does not exist.

The wallclock impact on the standard PR cell is bounded by the cost of the
early-return: a single function call, well under 1 ms per test.

## 7. Cross-platform handling

- Linux: full coverage. Upstream binaries built by `run_interop.sh`.
- macOS: tests self-skip on missing binary. A follow-up may extend
  `run_interop.sh` to build upstream on macOS; until then, the
  `upstream-compat` cell only runs on Linux and the macOS PR cell self-skips.
- Windows: tests self-skip. Upstream rsync does not target Windows directly.

Every NXT-2..NXT-8 test that asserts Unix-only behaviour (`mknod`, ACLs,
`munge symlinks`) additionally guards with `if !cfg!(unix) { return; }`, even
on the upstream-compat cell.

## 8. Failure diagnosis

When a test fails, the diagnostic chain is:

1. The `assert!` macro fires with the captured stderr of the failing process
   embedded.
2. The harness propagates daemon log + temp-dir path. The temp dir is held
   open via `PRESERVE_SCRATCH=1` (env var read by `test-support`) so post-
   mortem inspection is possible.
3. A trailing `eprintln!` in each test's drop guard prints the daemon log
   path on failure so CI logs link to the artifact.

The flake-mitigation contract is the same as the sibling harness: every wait
is bounded, every process has a 30 s hard cap, ports are OS-assigned.

## 9. Non-goals (recap)

- Not a `runtests.py` replacement. Nightly UTS workflow remains.
- Not porting all 108 upstream tests. NXT-2..NXT-8 is the curated list.
- No wire-byte capture in v1. Track under UTS-NEXTEST-EDGE.c.
- No new workflow file. One new cell in the existing nextest workflow.
- No async runtime. Tests are blocking; the harness uses `std::process`
  directly.

## 10. Acceptance criteria for NXT-1

This design PR (NXT-1) is acceptance-ready when:

1. The `crates/test-support/src/upstream_compat.rs` API surface above is
   reviewed and agreed.
2. The CI wire-up plan (section 6) is reviewed by the workflow owner.
3. NXT-2..NXT-8 each cite this doc and follow the section 4 template.

The follow-up harness implementation PR (separate sub-task, not NXT-1) is
acceptance-ready when:

1. `crates/test-support/src/upstream_compat.rs` lands with the public API.
2. A single smoke test under `crates/test-support/tests/upstream_compat_smoke.rs`
   self-skips cleanly when `OC_RSYNC_UPSTREAM_COMPAT` is unset, and exercises
   `require_upstream_rsync(V3_4_4)` end-to-end when set.
3. `cargo fmt --all -- --check` and clippy (`-D warnings`) pass.

Each NXT-2..NXT-8 port PR is acceptance-ready when:

1. The ported test reproduces the upstream regression on a known-bad
   oc-rsync revision and passes on the current revision.
2. Wall time <= 5 s P95 on the `upstream-compat` cell.
3. The PR cites this design doc and the upstream `*_test.py` it replaces.

## 11. Rollback criteria

Deprecate the upstream-compat suite and remove
`crates/*/tests/upstream_compat/` directories if any of the following is
true over a 30-day window:

- More than 10% of NXT-* tests flake (pass/fail on the same code state).
- The `upstream-compat` CI cell wall time exceeds 5 minutes (1/3 of the
  current `run_upstream_testsuite.sh` budget). At that point the cost
  advantage over the nightly UTS workflow is gone.
- A nightly UTS regression slips past the curated subset and the curated
  subset's signal misled a reviewer into thinking the class was covered.

Rollback steps:

1. One PR removes the `upstream-compat` matrix cell from the nextest
   workflow.
2. A second PR removes `crates/test-support/src/upstream_compat.rs` and the
   per-crate `tests/upstream_compat/` directories.
3. A close-out audit at `docs/audits/nxt-rollback.md` documents which signals
   the curated subset failed to catch.
