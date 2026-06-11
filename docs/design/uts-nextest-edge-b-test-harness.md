# UTS-NEXTEST-EDGE.b: nextest harness design for upstream runtests.py edge cases

Tracks: UTS-NEXTEST-EDGE.b (design phase)
Companion: `docs/audits/uts-nextest-edge-a-runtests-inventory.md` (UTS-NEXTEST-EDGE.a)

## 1. Purpose

The companion audit (UTS-NEXTEST-EDGE.a) catalogues the 111 upstream `*_test.py` files and identifies 30 Tier-1 ports plus 53 Tier-2 ports. This document specifies the harness primitives those ports will share so that:

1. Every Tier 1 port has a single, well-defined seam to spawn a daemon, spawn a client, drive a remote-shell stub, and compare result trees.
2. The harness composes - depth-property tests in `crates/transfer/tests/operational/` and security tests in `crates/daemon/tests/operational/` use the same building blocks.
3. Per-test wall time stays under 5 s P95.
4. The harness is self-skipping on hosts that lack a prerequisite (e.g. no `setfacl`, no `mknod`, no root) instead of failing or hanging.

Design only. No implementation in this PR. The harness lands as a separate follow-up PR before any Tier 1 ports.

## 2. Crate placement convention

```
crates/test-support/src/
  lib.rs                  (existing - create_tempdir)
  daemon.rs               (new - OcRsyncDaemonHarness)
  cli.rs                  (new - OcRsyncCliRunner)
  lsh.rs                  (new - LshRunnerStub)
  dir_diff.rs             (new - DirDiff)
  wire_capture.rs         (new - WireCapture, deferred / opt-in)

crates/daemon/tests/operational/      (new)
  daemon_munge.rs
  daemon_auth.rs
  ...

crates/transfer/tests/operational/    (new)
  batch_mode.rs
  append_shortsum.rs
  ...

crates/metadata/tests/operational/    (new)
  acls_default.rs
  xattrs_depth.rs
  ...

crates/core/tests/operational/        (new)
  daemon_gzip_upload.rs
  ...
```

Rules:

- Every `operational/` directory holds a single nextest entry point per Tier 1 test, named after the upstream test minus `_test.py`.
- The harness primitives live in `test-support` so every operational test imports a stable surface.
- A new test SHOULD have at most one `#[test]` function plus parameterized variants - keep the unit of failure small.
- Operational tests are NOT `#[ignore]`d. They self-skip via `test-support::should_skip(...)` when prerequisites are missing.

## 3. `OcRsyncDaemonHarness`

### 3.1 Responsibilities

- Spawn `oc-rsync --daemon --no-detach --config <path>` on an OS-assigned loopback port.
- Wait for the daemon to accept a TCP connection before returning.
- Provide accessors for the listen port, log path, module root, and `rsync://127.0.0.1:PORT/` URL.
- Kill + reap the child process on `Drop` (RAII).
- Capture stderr to a temp file for failure diagnosis.

### 3.2 API skeleton

```rust
// crates/test-support/src/daemon.rs

use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Module configuration that drives the daemon's `rsyncd.conf`.
#[derive(Default)]
pub struct ModuleConfig {
    pub name: String,
    pub path: PathBuf,
    /// Free-form `key = value` lines.
    pub params: Vec<(String, String)>,
}

/// Global daemon parameters (lines outside any `[module]`).
#[derive(Default)]
pub struct GlobalConfig {
    pub use_chroot: bool,
    pub munge_symlinks: bool,
    pub hosts_allow: Option<String>,
    pub extra: Vec<(String, String)>,
}

/// Builder for `OcRsyncDaemonHarness`.
pub struct OcRsyncDaemonHarnessBuilder {
    binary: PathBuf,
    globals: GlobalConfig,
    modules: Vec<ModuleConfig>,
    ready_timeout: Duration,
    use_chroot_default: bool,
}

impl OcRsyncDaemonHarnessBuilder {
    pub fn new() -> Self { /* ... */ }
    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self { /* ... */ }
    pub fn module(mut self, cfg: ModuleConfig) -> Self { /* ... */ }
    pub fn global(mut self, cfg: GlobalConfig) -> Self { /* ... */ }
    pub fn ready_timeout(mut self, timeout: Duration) -> Self { /* ... */ }
    pub fn spawn(self) -> io::Result<OcRsyncDaemonHarness> { /* ... */ }
}

/// Live daemon instance. Drops cleanly on test exit.
pub struct OcRsyncDaemonHarness {
    workdir: TempDir,
    config_path: PathBuf,
    log_path: PathBuf,
    stderr_path: PathBuf,
    pid_path: PathBuf,
    modules: Vec<PathBuf>,
    port: u16,
    process: Option<Child>,
}

impl OcRsyncDaemonHarness {
    pub fn builder() -> OcRsyncDaemonHarnessBuilder { /* ... */ }
    pub fn port(&self) -> u16 { self.port }
    pub fn url(&self, module: &str) -> String {
        format!("rsync://127.0.0.1:{}/{module}", self.port)
    }
    pub fn module_path(&self, index: usize) -> &Path { &self.modules[index] }
    pub fn log_contents(&self) -> io::Result<String> { /* ... */ }
    pub fn stderr_contents(&self) -> io::Result<String> { /* ... */ }
    /// Forces an immediate kill (otherwise Drop handles it).
    pub fn shutdown(&mut self) { /* ... */ }
}

impl Drop for OcRsyncDaemonHarness {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
```

### 3.3 Port allocation strategy

Use OS-assigned ports via `TcpListener::bind("127.0.0.1:0")`. The listener is dropped before spawning the daemon; the race window (listener-drop -> daemon-bind) is bounded by the OS port-reuse policy and has been acceptable in `crates/core/tests/common/mod.rs::allocate_test_port` for months. Retry once on `EADDRINUSE` to absorb the rare collision.

### 3.4 Readiness detection

Poll `TcpStream::connect("127.0.0.1:PORT")` at 50 ms intervals up to `ready_timeout` (default 5 s, configurable). Returns immediately on the first successful connect. On timeout, panic with the daemon log contents so the failure is loud.

No `thread::sleep` for synchronization. Every wait is bounded by a polling deadline.

### 3.5 Auth + secrets

Tests that exercise `auth users` (UTS-NEXTEST-EDGE.a row 32) write their secrets file via `tempfile::NamedTempFile`, set mode `0o600` explicitly, and pass `--password-file=PATH` to the client. Provide a `secrets_file(user, password)` helper on the builder so tests don't reinvent permission-bit handling.

### 3.6 Munge symlinks

The daemon-munge test (UTS-NEXTEST-EDGE.a row 41) drives a module with `munge symlinks = yes`. The builder exposes `module(ModuleConfig { params: vec![("munge symlinks".into(), "yes".into())], ... })`. The test asserts the literal stored target string `"/rsyncd-munged/f3"` and the stripped pull. No special harness support beyond `module_path()`.

## 4. `OcRsyncCliRunner`

### 4.1 Responsibilities

- Locate the `oc-rsync` binary via `CARGO_BIN_EXE_oc-rsync` (set by cargo for `[[bin]]` consumers of the workspace) or walk up `current_exe()` to `target/{debug,release}/`.
- Accept argv, env vars, stdin, and a child-process configurator.
- Capture stdout, stderr, exit code.
- Provide convenience constructors for common cases: `local_push`, `local_pull`, `daemon_push`, `daemon_pull`, `ssh_push`, `ssh_pull`.

### 4.2 API skeleton

```rust
// crates/test-support/src/cli.rs

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Builder + executor for an oc-rsync invocation.
pub struct OcRsyncCliRunner {
    binary: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    env_clear: bool,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    cwd: Option<PathBuf>,
}

impl OcRsyncCliRunner {
    pub fn new() -> Self { /* binary resolution */ }
    pub fn binary(mut self, path: impl Into<PathBuf>) -> Self { /* ... */ }
    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self { /* ... */ }
    pub fn args<I, S>(mut self, args: I) -> Self
    where I: IntoIterator<Item = S>, S: AsRef<OsStr> { /* ... */ }
    pub fn env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Self { /* ... */ }
    pub fn env_clear(mut self) -> Self { /* ... */ }
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self { /* ... */ }
    pub fn timeout(mut self, t: Duration) -> Self { /* ... */ }
    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self { /* ... */ }

    /// Run synchronously with timeout enforcement. Panics on timeout (loud).
    pub fn run(self) -> io::Result<CliOutput> { /* ... */ }
}

pub struct CliOutput {
    pub status: Option<i32>,    // None = signal death
    pub signal: Option<i32>,    // Some when killed by signal
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration: Duration,
}

impl CliOutput {
    pub fn assert_success(&self) -> &Self { /* ... */ }
    pub fn assert_exit(&self, code: i32) -> &Self { /* ... */ }
    pub fn assert_no_signal_death(&self) -> &Self { /* ... */ }
    pub fn stderr_str(&self) -> &str { /* lossy */ }
    pub fn stdout_str(&self) -> &str { /* lossy */ }
    pub fn stderr_contains(&self, needle: &str) -> bool { /* ... */ }
}
```

### 4.3 Exit-code matching

Upstream rsync defines exit codes in `errcode.h`; oc-rsync mirrors them via `core::ExitCode`. The runner's `assert_exit` accepts a single code; tests expecting partial-transfer codes (23) explicitly allow that via `assert_exit_in(&[0, 23])`.

The `assert_no_signal_death` helper exists specifically for tests like `proxy-response-line-too-long_test.py` (UTS-NEXTEST-EDGE.a row 87) and `clean-fname-underflow_test.py` (row 23) which must reject a crafted input *without* SIGSEGV/SIGABRT.

### 4.4 Timeout enforcement

Every runner has a default 30 s wall-time timeout. On timeout, the child is killed and the test panics with stderr captured up to that point. This is the only place a test can hang; the timeout is the kill-switch.

Implement via `wait_timeout::ChildExt::wait_timeout` (already a workspace dep) or a simple poll loop on `Child::try_wait`. No additional crate.

## 5. `LshRunnerStub`

### 5.1 Responsibilities

Upstream's `support/lsh.sh` is a shell stub that pretends to be `ssh` and runs the remote command locally. It is invoked via `-e <stub>` (or `RSYNC_RSH=<stub>`) and supports only the hostnames `localhost` and `lh`. The Rust equivalent must:

- Be a deployable executable so `Command::new(stub)` works.
- Accept the upstream argv shape (`-l USER` ignored or `sudo`'d, `localhost`/`lh`, then the remote argv).
- Re-exec the remote argv in-process (or as a child) and pipe stdin/stdout through.

### 5.2 Approach: build a tiny `lsh-stub` test binary

```rust
// crates/test-support/src/bin/lsh-stub.rs
//
// Rust port of upstream support/lsh.sh. Strips `ssh`-style flags from argv,
// asserts the hostname is "localhost" or "lh", then execvp's the remaining
// argv. Pipes stdio through transparently.

fn main() -> std::io::Result<()> {
    let argv: Vec<_> = std::env::args_os().skip(1).collect();
    let mut remaining = argv.into_iter().peekable();

    let mut user: Option<String> = None;
    let mut no_cd = false;
    while let Some(arg) = remaining.peek() {
        let bytes = arg.to_string_lossy();
        if bytes.starts_with("-l") {
            // -l USER or -lUSER
            // ...
        } else if bytes == "--no-cd" {
            no_cd = true;
            remaining.next();
        } else if bytes.starts_with('-') {
            remaining.next();
        } else if bytes == "localhost" {
            remaining.next();
            break;
        } else if bytes == "lh" {
            no_cd = true;
            remaining.next();
            break;
        } else {
            eprintln!("lsh-stub: unable to connect to host {}", bytes);
            std::process::exit(1);
        }
    }

    let cmd: Vec<_> = remaining.collect();
    // exec the remote argv with stdin/stdout/stderr inherited.
    // ...
}
```

The stub is registered in `crates/test-support/Cargo.toml` as `[[bin]] name = "lsh-stub"`, so `CARGO_BIN_EXE_lsh-stub` resolves to its path inside the test. No shell-script dependency, no platform fork on Windows.

### 5.3 Why an executable not a Rust struct

Upstream `lsh.sh` is invoked by `oc-rsync` itself via `Command::new(env::var("RSYNC_RSH"))`, so it must be a path on disk. Building it as a `[[bin]]` keeps the path resolution stable across `cargo nextest run`, `cargo test`, and CI.

### 5.4 Windows handling

`lsh-stub` is `#[cfg(unix)]`-only initially. Tests that depend on it (`exclude-lsh_test`, `files-from_test`, `ssh-basic_test`) are gated `#[cfg(unix)]`. A future follow-up can add a Windows version using `CreateProcess`.

## 6. `DirDiff`

### 6.1 Responsibilities

Recursively compare two directory trees with byte-equality + mode + owner + (optional) atime/mtime/xattr/acl. Mirrors upstream's `rsync_ls_lR` + `diff -r` pair.

### 6.2 API skeleton

```rust
// crates/test-support/src/dir_diff.rs

use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct DirDiffOptions {
    pub check_mode: bool,
    pub check_mtime: bool,
    pub check_owner: bool,
    pub check_atime: bool,
    pub check_xattr: bool,
    pub check_acl: bool,
    pub follow_symlinks: bool,
    /// Compare symlink targets directly instead of dereferencing.
    pub literal_symlinks: bool,
}

impl DirDiffOptions {
    /// Equivalent of upstream `checkit()` default: listing + byte content.
    pub fn structural() -> Self { /* check_mode = true, rest false */ }
    /// Equivalent of `-a` archive mode.
    pub fn archive() -> Self { /* mode + mtime + owner + symlinks */ }
}

pub struct DirDiff;

impl DirDiff {
    /// Returns Ok(()) if `expected` and `actual` are equivalent under `opts`.
    /// Returns a diff string on mismatch.
    pub fn compare(
        expected: &Path,
        actual: &Path,
        opts: DirDiffOptions,
    ) -> Result<(), DirDiffMismatch> { /* ... */ }
}

pub struct DirDiffMismatch {
    pub differences: Vec<DirDiffEntry>,
}

pub enum DirDiffEntry {
    OnlyInExpected(PathBuf),
    OnlyInActual(PathBuf),
    ContentMismatch { path: PathBuf, expected_len: u64, actual_len: u64 },
    ModeMismatch { path: PathBuf, expected: u32, actual: u32 },
    OwnerMismatch { path: PathBuf, expected: (u32, u32), actual: (u32, u32) },
    SymlinkMismatch { path: PathBuf, expected: PathBuf, actual: PathBuf },
    XattrMismatch { path: PathBuf, key: String },
    AclMismatch { path: PathBuf },
}

impl DirDiffMismatch {
    pub fn into_panic_message(self) -> String { /* upstream-style diff output */ }
}
```

### 6.3 Implementation notes

- Use `walkdir` (workspace dep) for traversal.
- Byte equality via `memchr::memcmp` or stdlib `==` on contents under 1 MB; fall back to streaming compare above that threshold.
- Mode/owner via `std::fs::symlink_metadata` (`MetadataExt` on Unix).
- xattr/ACL only on `cfg(unix)`; gated behind `DirDiffOptions::check_xattr` so most tests don't pay the cost.
- Output formats as unified diff style so a failure prints cleanly.

## 7. `WireCapture` (deferred / opt-in)

### 7.1 Why deferred

`pcap`-based byte capture is useful for cell tests like UTS-15.d (batch-mode tcpdump) but:

- Needs root or `CAP_NET_RAW` on Linux, which CI runners may not grant.
- Adds ~10 MB of dependency surface (`pcap` crate + libpcap system dep).
- The 30 Tier-1 ports do not require wire-byte assertions to encode the regression - exit-code + dir-diff + stderr-signature is sufficient for "did this regress at PR-time".

### 7.2 API sketch (for the follow-up)

```rust
// crates/test-support/src/wire_capture.rs (FUTURE)

#[cfg(feature = "wire-capture")]
pub struct WireCapture { /* ... */ }

#[cfg(feature = "wire-capture")]
impl WireCapture {
    pub fn start_loopback(port: u16) -> io::Result<Self> { /* ... */ }
    pub fn stop_and_collect(self) -> io::Result<PcapBuffer> { /* ... */ }
}
```

Defer until a Tier 1 port surfaces a regression that *only* a wire-byte assertion can pin. Track as a follow-up sub-task under UTS-NEXTEST-EDGE.c.

### 7.3 Alternative: socketpair tap

For the in-process daemon case (no real TCP), a simpler approach is a thin "tee" wrapper around the socket fd that records all bytes to a `Vec<u8>`. That stays inside the existing process, needs no privileges, and is enough for goodbye-flush regressions. Track this under UTS-NEXTEST-EDGE.c as the recommended first-pass implementation.

## 8. Speed budget

| Operation | Target | Strategy |
|---|---|---|
| Daemon spawn + ready | <= 200 ms | poll TCP at 50 ms; debug binary cold-start ~50-150 ms |
| Tree population (depth-3, 10 files/dir) | <= 50 ms | direct `std::fs` calls; no recursion via `tar` |
| Single transfer (small tree) | <= 500 ms | local pipe, no shell |
| Single transfer (with `-zz` + ~700 KB) | <= 2 s | small payload sized to clear codec invariants |
| DirDiff (depth-3 tree) | <= 50 ms | walkdir + memcmp |
| Test wall time P95 | <= 5 s | sum of above plus margin |
| Test wall time hard cap | 30 s | `OcRsyncCliRunner::timeout` |

The hard 30 s cap exists so a regression that *would* hang (e.g. goodbye flush regression like UTS-9) fails loud within nextest's per-test wall time instead of stalling the cell.

## 9. Self-skip vs `#[ignore]`

Operational tests are NOT `#[ignore]`d. They self-skip via a helper that prints a clear reason and returns early:

```rust
// crates/test-support/src/lib.rs

#[must_use]
pub fn require_unix() -> bool {
    if cfg!(unix) { true } else {
        eprintln!("Skipping: requires Unix");
        false
    }
}

#[must_use]
pub fn require_root() -> bool {
    #[cfg(unix)]
    {
        if nix::unistd::Uid::effective().is_root() { return true; }
    }
    eprintln!("Skipping: requires root");
    false
}

#[must_use]
pub fn require_binary(name: &str) -> bool {
    if locate_workspace_binary(name).is_some() { true } else {
        eprintln!("Skipping: {name} binary not found");
        false
    }
}

#[must_use]
pub fn require_setfacl() -> bool { /* PATH check */ }

#[must_use]
pub fn require_xattr_support(path: &Path) -> bool { /* probe by setting a test xattr */ }
```

The contract: every operational test starts with one or more `require_*()` guards. If any returns false, the test prints the reason and returns successfully. nextest reports the test as passing; CI logs show the skip reason. This avoids the "did this test actually run?" ambiguity of `#[ignore]`.

Rationale: the failure mode UTS-NEXTEST-EDGE addresses is regressions reaching the upstream-testsuite cell undetected. `#[ignore]` tests only run in the interop cell, which defeats the entire purpose. Self-skip ensures the test attempts to run on every CI cell that has the prerequisites, and skips silently on cells that don't (Windows on a POSIX-ACL test, for example).

## 10. Test parameterization

Several Tier 1 tests have small decision tables (e.g. daemon-access has 3 modules x 4 access combos = 12 cells). Parameterize with `#[test_case::test_case]` (already a workspace dev-dep) rather than `#[test]` per cell:

```rust
#[test_case::test_case("read-only", true, false ; "read_only_blocks_push")]
#[test_case::test_case("write-only", false, true ; "write_only_blocks_pull")]
#[test_case::test_case("read-write", true, true ; "read_write_allows_both")]
fn module_access_modes(module_name: &str, push_ok: bool, pull_ok: bool) {
    if !test_support::require_unix() { return; }
    // ... single body that drives the harness
}
```

This keeps the test entry-points discoverable to nextest while compressing the table to a single body.

## 11. Skeleton wiring per Tier 1 test

Each Tier 1 port follows the same template:

```rust
// crates/daemon/tests/operational/daemon_munge.rs

use test_support::{
    OcRsyncDaemonHarness, OcRsyncCliRunner, DirDiff, DirDiffOptions,
    ModuleConfig, require_unix, require_binary,
};

#[test]
fn munge_symlinks_adds_and_strips_prefix_at_depth() {
    if !require_unix() { return; }
    if !require_binary("oc-rsync") { return; }

    let src = test_support::create_tempdir();
    let dst = test_support::create_tempdir();
    let pull = test_support::create_tempdir();
    populate_depth_3_tree(src.path());
    std::os::unix::fs::symlink("f3", src.path().join("d1/d2/sl")).unwrap();

    let daemon = OcRsyncDaemonHarness::builder()
        .module(ModuleConfig {
            name: "munge".into(),
            path: dst.path().to_path_buf(),
            params: vec![
                ("read only".into(), "no".into()),
                ("munge symlinks".into(), "yes".into()),
            ],
        })
        .spawn()
        .expect("spawn daemon");

    // Push: stored symlink must be /rsyncd-munged/f3.
    OcRsyncCliRunner::new()
        .arg("-al")
        .arg(format!("{}/", src.path().display()))
        .arg(format!("{}munge/", daemon.url("")))
        .run()
        .expect("push ok")
        .assert_success();

    let stored = std::fs::read_link(dst.path().join("d1/d2/sl")).unwrap();
    assert_eq!(stored.to_str(), Some("/rsyncd-munged/f3"),
        "munge-symlinks must prefix stored target");

    // Pull: stripped back to f3.
    OcRsyncCliRunner::new()
        .arg("-al")
        .arg(format!("{}munge/", daemon.url("")))
        .arg(format!("{}/", pull.path().display()))
        .run()
        .expect("pull ok")
        .assert_success();

    let pulled = std::fs::read_link(pull.path().join("d1/d2/sl")).unwrap();
    assert_eq!(pulled.to_str(), Some("f3"),
        "munge-symlinks must strip prefix on pull");
}

fn populate_depth_3_tree(root: &std::path::Path) {
    use std::fs;
    let _ = fs::create_dir_all(root.join("d1/d2"));
    let _ = fs::write(root.join("d1/d2/f3"), b"hello\n");
    let _ = fs::write(root.join("d1/d2/f3.b"), b"world\n");
}
```

The template generalises: 3-5 lines of harness setup, the body asserts the upstream-contract invariant, and `DirDiff::compare(...)` checks the result tree when the test isn't a single-bit assertion.

## 12. CI integration

- All operational tests are wired into the existing `nextest run --workspace --all-features` job. No new workflow.
- Tier 1 lands first; the harness primitives in `test-support` land in a single PR before the first Tier 1 port.
- The `target/release/oc-rsync` binary that operational tests need is the same one CI already builds for argument-parsing tests; no extra build step.
- `lsh-stub` is built automatically as a workspace `[[bin]]`.

The upstream-testsuite cell stays in place as an authoritative final check. The nextest operational suite is the *PR-time* guardrail.

## 13. Rollback criteria

Deprecate the operational suite and remove `crates/*/tests/operational/` directories if any of the following becomes true:

- More than 10% of operational tests turn out to be intermittent (flaky pass/fail on the same code state) over a 30-day window. The harness's deterministic-port + polling-readiness design should keep this at zero, but the criterion is the contract.
- The PR-time wall time on nextest grows beyond 10% of total CI wall time. Each operational test is budgeted <= 5 s P95; with 30 tests this is ~150 s, well below the cap.
- The upstream-testsuite cell catches a regression the operational suite missed *because* the operational suite was misleading reviewers into thinking a class was covered. (If the operational suite simply doesn't cover something, that's a gap to fill, not a reason to deprecate.)

Rollback steps:

1. Open one PR removing `crates/*/tests/operational/` directories.
2. Open a separate PR removing the new `test-support` modules.
3. Document the rollback rationale in a follow-up `docs/audits/uts-nextest-edge-c-rollback.md` for future reference.

## 14. Open extensions tracked separately

- **WireCapture** (section 7): track as UTS-NEXTEST-EDGE.c. Build the socketpair-tap approach first, defer pcap until a real test demands it.
- **Tier 2 ports** (53 tests): track as UTS-NEXTEST-EDGE.d.* per crate (one parent sub-task per target crate).
- **Windows lsh-stub variant** (section 5.4): track as UTS-NEXTEST-EDGE.e if any cross-platform port needs it.

## 15. Non-goals

- The operational suite is not a replacement for the interop cell that runs against upstream binaries. It pins *oc-rsync's behaviour* against the upstream contract; matching upstream wire bytes remains the interop cell's job.
- The harness does not capture or replay byte-exact wire frames. That is `WireCapture`'s eventual job.
- The harness does not provide a Python or shell entry point. Tests are Rust-only; this is intentional - the goal is integration with `cargo nextest`, not a parallel runtests.py.

## 16. Acceptance criteria for the follow-up PR landing the harness

The harness PR (separate from this design PR) is acceptance-ready when:

1. `test-support` exposes `OcRsyncDaemonHarness`, `OcRsyncCliRunner`, `LshRunnerStub` (binary), `DirDiff`, and the `require_*` self-skip helpers.
2. A single smoke operational test under `crates/daemon/tests/operational/smoke.rs` proves the harness can spawn the daemon, run a client, and DirDiff a 5-file tree, all under 2 s wall time on the standard CI runner.
3. `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings` pass.
4. The smoke test runs unconditionally on Linux, macOS, and Windows nextest cells (self-skipping on Windows for paths that need Unix).

Each Tier 1 port PR after that is acceptance-ready when:

1. The ported test matches the upstream test's invariant set (not necessarily its exact assertions - upstream's checks are sometimes shell-quoting workarounds we don't need).
2. Wall time <= 5 s P95 on CI.
3. The PR description cites the audit row (UTS-NEXTEST-EDGE.a row N) it ports.
