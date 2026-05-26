# SEC-MK.e - Regression test: mknod device file through sandboxed path

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessors**:
  - SEC-MK.a - mknod/mkfifo code-path inventory (`docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`)
  - SEC-MK.b - mknodat sandbox implementation spec (`docs/design/sec-mk-b-mknodat-sandbox-impl.md`)
  - SEC-MK.c - mkfifoat sandbox implementation (mirrors SEC-MK.b for FIFOs)
  - SEC-MK.d - receiver wiring design (PR #5013, merged)
- **CVE context**: CVE-2026-29518, CVE-2026-43619 - symlink-swap TOCTOU
  on path-based syscalls under `use_chroot=false`
- **Scope**: regression tests that verify device file creation routes
  through the sandboxed `mknodat(dirfd, leaf, ...)` path, and that the
  fallback path-based `mknod` still works when no sandbox is available.

---

## 1. Objective

Prove that the SEC-MK.b/c migration - from `mknodat(AT_FDCWD, full_path, ...)`
to `mknodat(sandbox_dirfd, leaf, ...)` - does not regress device file or
FIFO creation, and that the dirfd-anchored path resists the symlink-swap
TOCTOU class the SEC-1 series addresses.

The test file lives in `crates/transfer/tests/` alongside the existing
SEC-1.m harness (`sec_1_m_symlink_swap_attack.rs`) and the per-primitive
swap-resistance tests (`fstatat_swap_resistance.rs`,
`unlinkat_swap_resistance.rs`, `delete_sandbox_swap.rs`).

**File**: `crates/transfer/tests/sec_mk_e_mknod_sandbox_regression.rs`

---

## 2. Test fixtures

### 2.1 FIFO fixtures (no privilege required)

FIFOs (`S_IFIFO`) can be created by any user. They are the primary
vehicle for unprivileged testing of the `mknodat` sandbox path because
`mknodat(dirfd, leaf, S_IFIFO | 0o644, 0)` exercises the same
dirfd-anchored code path as block and character device creation - the
only difference is the type bits in `mode` and the value of `dev`.

| Fixture | Mode | Dev | Purpose |
|---------|------|-----|---------|
| `test.fifo` | `S_IFIFO \| 0o644` | `0` | Basic FIFO through sandbox |
| `test.sock` | `S_IFSOCK \| 0o644` | `0` | Socket node through sandbox |

### 2.2 Device fixtures (root required)

Block and character device nodes require `CAP_MKNOD` on Linux or root on
macOS. Tests that create real device nodes are gated on an `is_root()`
runtime check and skipped otherwise.

| Fixture | Mode | Dev (major, minor) | Purpose |
|---------|------|-------------------|---------|
| `null.dev` | `S_IFCHR \| 0o666` | `(1, 3)` | `/dev/null`-equivalent char device |
| `loop0.dev` | `S_IFBLK \| 0o660` | `(7, 0)` | `/dev/loop0`-equivalent block device |

**Major/minor rationale**: `(1, 3)` is the canonical null device on Linux.
`(7, 0)` is loop device 0. These are safe, well-known device numbers that
do not conflict with active devices in CI containers.

### 2.3 Fake-super placeholder fixtures (no privilege required)

When `--fake-super` is active, `create_device_node_with_fake_super` and
`create_fifo_with_fake_super` bypass mknod entirely and create a regular
`0o600` placeholder file via `open(O_CREAT|O_EXCL)`. This path must
still function correctly when a sandbox is wired - the
`mknodat_via_sandbox_or_fallback` adaptor delegates to the fallback
closure, which in turn calls the fake-super placeholder logic.

| Fixture | Expected result | Purpose |
|---------|----------------|---------|
| `fake-device` | Regular file, mode `0o600` | Verify placeholder substitution with sandbox present |
| `fake-fifo` | Regular file, mode `0o600` | Same, for FIFO path |

---

## 3. Root requirement handling

### 3.1 Runtime root detection

```rust
fn is_root() -> bool {
    #[cfg(unix)]
    {
        rustix::process::getuid().is_root()
    }
    #[cfg(not(unix))]
    {
        false
    }
}
```

### 3.2 Test gating strategy

Tests that require root use a guard at the top of the test body:

```rust
#[test]
fn sandbox_mknodat_creates_real_char_device() {
    if !is_root() {
        eprintln!("skipping: requires root / CAP_MKNOD");
        return;
    }
    // ...
}
```

This pattern is preferred over `#[ignore]` because:
- `#[ignore]` tests require `--ignored` to run and are invisible in
  default CI output.
- A runtime skip with `eprintln` produces visible output in nextest
  results, making it clear the test was evaluated but conditions were
  not met.
- CI interop containers that run as root will exercise the full path.

### 3.3 Unprivileged coverage via FIFO

FIFO creation exercises the identical `mknodat(dirfd, leaf, mode, dev)`
code path as device creation - only the `mode` type bits differ. All
sandbox-anchoring invariants (single-component leaf resolution, dirfd
pinning, symlink-swap resistance) are validated through FIFO tests that
run without privileges.

### 3.4 Unprivileged device coverage via fake-super

The `--fake-super` placeholder path exercises the
`mknodat_via_sandbox_or_fallback` adaptor's fallback branch. By testing
with `fake_super = true`, we verify that:
- The adaptor correctly delegates to the closure.
- The closure creates the placeholder file under the sandbox root.
- No `EPERM` is raised because the real `mknodat` is never called.

### 3.5 EPERM verification

An explicit test verifies that calling `mknodat` with `S_IFCHR` or
`S_IFBLK` as a non-root user returns `EPERM`. This confirms the kernel
enforces the privilege check through the dirfd-anchored path, not just
through the path-based path.

---

## 4. Test scenarios

### 4.1 Sandboxed FIFO creation (no root)

**Test**: `sandbox_mknodat_creates_fifo_under_dirfd`

1. Create a `TempDir`, canonicalize, open a `DirSandbox` on it.
2. Call `mknodat(sandbox.current_dirfd(), "test.fifo", S_IFIFO | 0o644, 0)`.
3. Verify the FIFO exists via `fstatat_nofollow(dirfd, "test.fifo")`.
4. Assert `meta.file_type().is_fifo()`.
5. Assert `meta.mode() & 0o7777 == 0o644` (modulo umask).

**Invariant**: the raw `mknodat` wrapper creates a FIFO beneath the
dirfd, not relative to cwd.

### 4.2 Sandboxed socket creation (no root)

**Test**: `sandbox_mknodat_creates_socket_under_dirfd`

Same as 4.1 but with `S_IFSOCK | 0o644`. Verify `is_socket()`.

### 4.3 Sandbox-or-fallback with sandbox present (FIFO, no root)

**Test**: `sandbox_or_fallback_uses_dirfd_for_single_component_fifo`

1. Create a `TempDir`, open a `DirSandbox`.
2. Call `mknodat_via_sandbox_or_fallback(Some(&sandbox), dest_dir, Path::new("leaf.fifo"), dest_dir.join("leaf.fifo"), S_IFIFO | 0o644, 0, || unreachable!("fallback must not fire"))`.
3. Verify `leaf.fifo` exists as a FIFO under the sandbox root.

**Invariant**: when `sandbox` is `Some` and the relative path is a single
component, the dirfd path is taken and the fallback closure is never
called.

### 4.4 Sandbox-or-fallback fires fallback when sandbox is None

**Test**: `sandbox_or_fallback_fires_fallback_when_no_sandbox`

1. Create a `TempDir`.
2. Set `fallback_called = AtomicBool::new(false)`.
3. Call `mknodat_via_sandbox_or_fallback(None, ..., || { fallback_called.store(true, ...); create_fifo_inner(...) })`.
4. Assert `fallback_called.load(...)` is `true`.
5. Verify the FIFO exists.

**Invariant**: `None` sandbox always delegates to the fallback.

### 4.5 Sandbox-or-fallback fires fallback for multi-component path

**Test**: `sandbox_or_fallback_fires_fallback_for_nested_path`

1. Create a `TempDir`, open a `DirSandbox`.
2. Create subdirectory `sub/` under the sandbox root.
3. Call `mknodat_via_sandbox_or_fallback(Some(&sandbox), dest_dir, Path::new("sub/leaf.fifo"), dest_dir.join("sub/leaf.fifo"), ..., || { fallback_called.store(true, ...); mknodat(CWD, full_path, ...) })`.
4. Assert `fallback_called` is `true`.
5. Verify `sub/leaf.fifo` exists.

**Invariant**: multi-component relative paths always take the fallback,
even when a sandbox is present. The `single_component_leaf` guard
rejects nested paths.

### 4.6 Sandboxed char device creation (root only)

**Test**: `sandbox_mknodat_creates_real_char_device`

1. Guard: `if !is_root() { return; }`
2. Create a `TempDir`, open a `DirSandbox`.
3. Call `mknodat(sandbox.current_dirfd(), "null.dev", S_IFCHR | 0o666, makedev(1, 3))`.
4. `fstatat_nofollow` the leaf. Assert `is_char_device()`.
5. Assert `rdev` matches `makedev(1, 3)`.

**Invariant**: real device node creation works through the sandbox dirfd
with correct major/minor propagation.

### 4.7 Sandboxed block device creation (root only)

**Test**: `sandbox_mknodat_creates_real_block_device`

Same as 4.6 but with `S_IFBLK | 0o660` and `makedev(7, 0)`. Assert
`is_block_device()`.

### 4.8 EPERM for unprivileged device creation

**Test**: `unprivileged_mknodat_device_returns_eperm`

1. Guard: `if is_root() { return; }` - skip when running as root.
2. Create a `TempDir`, open a `DirSandbox`.
3. Call `mknodat(sandbox.current_dirfd(), "no-priv.dev", S_IFCHR | 0o666, makedev(1, 3))`.
4. Assert the result is `Err` with `raw_os_error() == Some(libc::EPERM)`.

**Invariant**: the kernel enforces `CAP_MKNOD` even through the
dirfd-anchored syscall.

### 4.9 Fake-super placeholder with sandbox present

**Test**: `fake_super_creates_placeholder_not_device_with_sandbox`

1. Create a `TempDir`, open a `DirSandbox`.
2. Call `mknodat_via_sandbox_or_fallback(Some(&sandbox), ..., || { create_device_node_with_fake_super(dest, metadata, true) })`.
3. The adaptor should take the fallback because `fake_super = true`
   causes the caller (engine-layer `copy_device`) to short-circuit
   before reaching the adaptor, but verify the integration: if the
   adaptor were called with fake-super metadata, the fallback produces
   a regular placeholder file, not a device node.
4. Verify: `fstatat` reports a regular file with mode `0o600`.

### 4.10 Fallback parity: sandbox vs path-based produce identical state

**Test**: `sandbox_and_fallback_produce_identical_fifo_state`

1. Create two `TempDir`s: `sandbox_dir` and `fallback_dir`.
2. Open a `DirSandbox` on `sandbox_dir`.
3. Create a FIFO in `sandbox_dir` via `mknodat(dirfd, ...)`.
4. Create a FIFO in `fallback_dir` via `mknodat(CWD, full_path, ...)`.
5. `fstatat` both. Assert: `file_type`, `mode & 0o7777`, and
   `dev`/`ino` relationship to parent are identical (i.e., both are
   children of their respective parent directory).

**Invariant**: migration from `CWD` to `dirfd` does not change the
observable filesystem state.

---

## 5. TOCTOU symlink-swap tests

These tests mirror the SEC-1.m pattern
(`crates/transfer/tests/sec_1_m_symlink_swap_attack.rs`): an attacker
thread races the receiver to swap a leaf for a symlink pointing outside
the sandbox. The `mknodat(dirfd, leaf, ...)` call must either succeed
(creating the node under the sandbox) or fail with `EEXIST` (the
symlink occupies the name) - it must never follow the symlink to create
a node at the attacker's chosen location.

### 5.1 TOCTOU: symlink at leaf prevents escape (quiescent)

**Test**: `mknodat_does_not_follow_existing_symlink_at_leaf`

1. Create a `TempDir` with `sensitive/` sibling outside the sandbox.
2. Open a `DirSandbox` on `dest/`.
3. Place a symlink at `dest/leaf` pointing to `sensitive/target`.
4. Call `mknodat(sandbox.current_dirfd(), "leaf", S_IFIFO | 0o644, 0)`.
5. Assert `EEXIST` - the symlink occupies the name.
6. Assert `sensitive/target` does not exist (was never created).
7. Assert the symlink at `dest/leaf` is unchanged (still a symlink).

**Invariant**: `mknodat` with a real dirfd does not follow symlinks at
the terminal component. The kernel creates the node at `(dirfd, leaf)`
or fails if the name already exists - it does not resolve the existing
entry.

### 5.2 TOCTOU: live attacker race on mknod (SEC-1.m pattern)

**Test**: `scenario_mknodat_race_does_not_create_node_outside_sandbox`

Follows the SEC-1.m `RaceChannels` pattern:

1. Create `parent/sensitive/` and `parent/dest/`.
2. Open a `DirSandbox` on `parent/dest/`.
3. Spawn attacker thread. The attacker:
   a. Waits on `proceed_rx`.
   b. Removes any existing `dest/leaf`.
   c. Creates symlink `dest/leaf -> sensitive/target`.
   d. Signals `done_tx`.
4. Main thread:
   a. Creates a regular file at `dest/leaf` (pre-race state).
   b. Sends `proceed_tx` to arm the attacker.
   c. Calls `mknodat(sandbox.current_dirfd(), "leaf", S_IFIFO | 0o644, 0)`.
   d. Waits on `done_rx`.
5. Assert invariants regardless of race winner:
   - `sensitive/target` does not exist - no node was created outside
     the sandbox.
   - `sensitive/` directory is intact.
   - If `mknodat` returned `Ok`, verify `dest/leaf` is a FIFO (receiver
     won the race, created the FIFO before the attacker swapped).
   - If `mknodat` returned `Err(EEXIST)`, verify `dest/leaf` is a
     symlink (attacker won, symlink was already there).
   - If `mknodat` returned `Err(ENOENT)`, the attacker removed the
     original between our remove-old and create-new steps - still safe,
     the sensitive tree is untouched.

**Invariant**: the sensitive tree outside the sandbox is never modified,
regardless of which thread wins the race.

### 5.3 TOCTOU: repeated race stress (SEC-1.m scenario 3 pattern)

**Test**: `scenario_repeated_mknodat_race_keeps_sensitive_tree_untouched`

Same structure as SEC-1.m `scenario_3_repeated_race_keeps_sensitive_tree_untouched`:

1. 64 iterations of the swap-vs-mknodat race.
2. Per-iteration: clear leaf, create fresh file, arm attacker, issue
   `mknodat`, verify sensitive tree intact.
3. Deterministic channel handshakes, no sleeps.
4. Final assertion: sensitive tree byte-identical to initial state.

---

## 6. Platform gating

The entire test file is gated with `#![cfg(unix)]` at the module level.
Device nodes, FIFOs, sockets, and `mknodat` have no Windows equivalent.
The Windows stubs in `metadata::special` return `Ok(())` silently and
require no regression test.

### 6.1 macOS considerations

- `mknodat(2)` is available on macOS 11+ (Big Sur). The minimum
  deployment target for oc-rsync is macOS 11.
- `S_IFSOCK` via `mknodat` works on macOS.
- macOS `mode_t` is `u16`; the `as libc::mode_t` cast in the raw
  wrapper truncates safely because `S_IFIFO | 0o7777 = 0o17777` fits
  in `u16`.
- macOS `dev_t` is `i32`; the `as libc::dev_t` cast from `u64`
  truncates. The test device numbers `(1, 3)` and `(7, 0)` fit in
  `i32`. On macOS, device major/minor numbers differ from Linux, so
  the root-only tests use `#[cfg(target_os = "linux")]` gating since
  the major/minor constants are Linux-specific.

### 6.2 `cfg` gates summary

| Scope | Gate | Rationale |
|-------|------|-----------|
| Entire file | `#![cfg(unix)]` | No mknod on Windows |
| Root-only device tests | `if !is_root() { return; }` | `CAP_MKNOD` required |
| EPERM test | `if is_root() { return; }` | Skip when root (EPERM not raised) |
| Linux-specific major/minor | `#[cfg(target_os = "linux")]` | `makedev(1, 3)` is Linux-specific |

---

## 7. Integration with existing test harness

### 7.1 Test file location

`crates/transfer/tests/sec_mk_e_mknod_sandbox_regression.rs`

The `transfer` crate is the natural home because:
- The existing SEC-1.m and swap-resistance tests live here.
- The `transfer` crate depends on `fast_io` (where `DirSandbox` and the
  `*_via_sandbox_or_fallback` helpers live).
- The receiver pipeline that wires the sandbox (SEC-MK.d) lives in
  `transfer`.

### 7.2 Dependencies

The test file needs:

```toml
# crates/transfer/Cargo.toml [dev-dependencies]
tempfile = "..."          # already present
crossbeam-channel = "..." # already present (used by SEC-1.m)
fast_io = { ... }         # already present
rustix = { ... }          # for fstatat, makedev, getuid
```

No new dev-dependencies are needed.

### 7.3 Shared test helpers

Reuse the `canonical_tempdir()` pattern from `sec_1_m_symlink_swap_attack.rs`.
Reuse the `RaceChannels` struct for TOCTOU tests. If the pattern is
reused across 3+ test files, extract into a `support` module under
`crates/transfer/tests/support/`. For now, inline in the test file
(matching existing precedent - each SEC-1 test file duplicates the
helper).

### 7.4 CI interop harness integration

The root-only device tests (`sandbox_mknodat_creates_real_char_device`,
`sandbox_mknodat_creates_real_block_device`) run automatically in CI
containers where the test process runs as root. The runtime `is_root()`
guard ensures they are skipped in developer environments.

The FIFO and socket tests, TOCTOU tests, and fallback-parity tests
run in all environments (Linux, macOS CI, developer machines).

---

## 8. Test matrix summary

| # | Test name | Root? | Platform | Validates |
|---|-----------|-------|----------|-----------|
| 4.1 | `sandbox_mknodat_creates_fifo_under_dirfd` | No | Unix | Raw `mknodat` wrapper for FIFOs |
| 4.2 | `sandbox_mknodat_creates_socket_under_dirfd` | No | Unix | Raw `mknodat` wrapper for sockets |
| 4.3 | `sandbox_or_fallback_uses_dirfd_for_single_component_fifo` | No | Unix | Adaptor takes dirfd path; fallback not called |
| 4.4 | `sandbox_or_fallback_fires_fallback_when_no_sandbox` | No | Unix | Adaptor delegates to fallback when sandbox is `None` |
| 4.5 | `sandbox_or_fallback_fires_fallback_for_nested_path` | No | Unix | Multi-component paths always take fallback |
| 4.6 | `sandbox_mknodat_creates_real_char_device` | **Yes** | Linux | Real char device via dirfd |
| 4.7 | `sandbox_mknodat_creates_real_block_device` | **Yes** | Linux | Real block device via dirfd |
| 4.8 | `unprivileged_mknodat_device_returns_eperm` | No (skip if root) | Unix | Kernel enforces CAP_MKNOD through dirfd |
| 4.9 | `fake_super_creates_placeholder_not_device_with_sandbox` | No | Unix | Fake-super placeholder with sandbox wired |
| 4.10 | `sandbox_and_fallback_produce_identical_fifo_state` | No | Unix | Migration parity: dirfd vs CWD |
| 5.1 | `mknodat_does_not_follow_existing_symlink_at_leaf` | No | Unix | Quiescent TOCTOU: symlink at leaf |
| 5.2 | `scenario_mknodat_race_does_not_create_node_outside_sandbox` | No | Unix | Live race TOCTOU (SEC-1.m pattern) |
| 5.3 | `scenario_repeated_mknodat_race_keeps_sensitive_tree_untouched` | No | Unix | 64-iteration stress race |

13 tests total. 11 run unprivileged on all Unix platforms. 2 require root
and are Linux-specific.

---

## 9. Upstream rsync reference

- `syscall.c:163-211` - `do_mknod()`: the upstream implementation this
  test validates. The `am_root < 0` branch handles `--fake-super`
  placeholder substitution. The `S_ISFIFO` branch dispatches to
  `mkfifo()` on platforms where `mknod` does not create FIFOs. The
  `S_ISSOCK` branch uses `socket()+bind()` on platforms where `mknod`
  does not create sockets.
- `receiver.c` - device and FIFO creation during file-list application.
- `rsync.h:MAKEDEV()` macro - device number composition from
  major/minor.
- `rsync 3.4.3 security advisory` (CVE-2026-29518, CVE-2026-43619) -
  symlink-swap TOCTOU class this regression test guards against.

---

## 10. Files changed by SEC-MK.e implementation

| File | Change |
|------|--------|
| `crates/transfer/tests/sec_mk_e_mknod_sandbox_regression.rs` | New test file (13 tests) |

No production code changes. SEC-MK.e is a pure regression test addition.

---

## 11. Risks and mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Root-only tests never run in CI | Low | CI interop containers run as root; add a CI log assertion that at least one root-gated test executed |
| FIFO tests flake on NFS-backed tmpdir | Low | `canonical_tempdir()` canonicalizes the path; NFS supports `mknodat` |
| Race tests flake due to scheduler variance | Low | Deterministic channel handshakes (no sleeps), matching SEC-1.m pattern that has been stable since PR #4671 |
| macOS `dev_t` truncation in device tests | N/A | Device tests are `#[cfg(target_os = "linux")]`; macOS runs FIFO/socket/TOCTOU tests only |
| Test creates real device node that interferes with host | Low | Tests use `TempDir` (auto-cleaned); device nodes are metadata-only inodes with no I/O capability at test major/minor numbers |
