# SEC-MK.f - Regression test: mkfifo through sandboxed path

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessors**:
  - SEC-MK.a - mknod/mkfifo code-path inventory (`docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`)
  - SEC-MK.b - mknodat/mkfifoat sandbox implementation spec (`docs/design/sec-mk-b-mknodat-sandbox-impl.md`)
  - SEC-MK.c/d - implementation of `mknodat` and `mkfifoat` helpers in `at_syscalls.rs`
  - SEC-1.h - mknodat deferral closure (`docs/design/sec-1-h-mknodat-deferral-2026-05-21.md`)
- **CVE coverage**: CVE-2026-29518, CVE-2026-43619 (path-based TOCTOU under `use_chroot=false`)
- **Scope**: Regression tests that pin the FIFO-creation sandbox invariant
  after the SEC-MK.b implementation lands. Tests verify both the
  dirfd-anchored (`mknodat`) path and the path-based fallback, plus a
  TOCTOU symlink-swap resistance scenario.

---

## 1. Motivation

The SEC-MK.b spec adds `mknodat_via_sandbox_or_fallback` to the
`DirSandbox` surface. Without regression tests that exercise FIFO
creation through the sandboxed path, a future refactor could silently
revert to path-based `mknod(CWD, ...)` and reopen the TOCTOU window
the SEC-MK series closes.

FIFOs are the ideal regression target because:

- **No root required.** Unlike device nodes (`CAP_MKNOD` on Linux, root
  on macOS), FIFOs can be created by any unprivileged user. Every CI
  runner can exercise the full syscall path without elevation or
  `--fake-super` substitution.
- **Identical syscall surface.** `mknodat(dirfd, leaf, S_IFIFO | mode, 0)`
  exercises the same `mknodat` wrapper and `single_component_leaf`
  dispatch logic that device nodes use - only the mode type bits differ.
- **Round-trip verifiable.** `fstatat(AT_SYMLINK_NOFOLLOW)` on the
  created entry reports `S_IFIFO`, providing a strong post-condition.

---

## 2. Test location

All tests go in a new integration test file:

```
crates/transfer/tests/sec_mk_f_mkfifo_sandbox.rs
```

This mirrors the placement of existing SEC-1 regression tests:
- `crates/transfer/tests/sec_1_m_symlink_swap_attack.rs`
- `crates/transfer/tests/fstatat_swap_resistance.rs`
- `crates/transfer/tests/unlinkat_swap_resistance.rs`
- `crates/transfer/tests/delete_sandbox_swap.rs`

The file is gated with `#![cfg(unix)]` at the top.

---

## 3. Test fixtures

### 3.1 Common helpers

```rust
#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use fast_io::DirSandbox;
use tempfile::{TempDir, tempdir};

/// Canonicalise tempdir to avoid symlink-prefixed paths that fail
/// sandbox open under RESOLVE_NO_SYMLINKS (macOS /var -> /private/var).
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}
```

### 3.2 Source metadata fixture

Tests need a `fs::Metadata` value whose `file_type().is_fifo()` returns
`true` and whose permission mode is a known value. The fixture creates a
real FIFO on disk and reads its metadata:

```rust
/// Creates a FIFO at `path` with the given permission mode and returns
/// its metadata for use as the source argument to creation helpers.
fn fifo_metadata(path: &Path, mode: u32) -> std::fs::Metadata {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    mknodat(CWD, path, FileType::Fifo, Mode::from_bits_truncate(mode.into()), makedev(0, 0))
        .expect("create source fifo");
    std::fs::symlink_metadata(path).expect("read fifo metadata")
}
```

On Apple platforms, this helper uses `apple_fs::mkfifo` instead (the same
platform branch `create_fifo_inner` uses):

```rust
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "tvos", target_os = "watchos"))]
fn fifo_metadata(path: &Path, mode: u32) -> std::fs::Metadata {
    apple_fs::mkfifo(path, mode as libc::mode_t).expect("create source fifo");
    std::fs::symlink_metadata(path).expect("read fifo metadata")
}
```

### 3.3 Permission values

Tests use `0o644` as the standard permission mode. This is a non-trivial
value that lets assertions distinguish "permissions were preserved" from
default `0o000` or umask-only results.

---

## 4. Test cases

### 4.1 Sandboxed path creates FIFO via dirfd

**Purpose**: verify that `mknodat_via_sandbox_or_fallback` with
`Some(sandbox)` and a single-component relative path creates the FIFO
through the dirfd, not through the full path.

**Setup**:
1. Create a `TempDir`. Open a `DirSandbox` on it.
2. Create a source FIFO in a separate directory to obtain metadata.

**Action**:
Call `mknodat_via_sandbox_or_fallback` with:
- `sandbox = Some(&sandbox)`
- `dest_dir = sandbox_root`
- `relative_path = Path::new("test.fifo")`
- `node_path = sandbox_root.join("test.fifo")`
- `mode = S_IFIFO | 0o644`
- `dev = 0`
- `fallback = || panic!("fallback must not be called for single-component leaf")`

**Assertions**:
1. The call returns `Ok(())`.
2. `fstatat_nofollow(sandbox.current_dirfd(), "test.fifo")` reports
   `is_fifo() == true`.
3. `symlink_metadata(sandbox_root.join("test.fifo"))` confirms `S_IFIFO`.
4. Permission bits `& 0o777` match the requested mode (subject to umask;
   assert `created_mode & requested_mode == created_mode`).
5. The fallback closure was **not** invoked (verified by the `panic!`).

### 4.2 Fallback path invoked when sandbox is None

**Purpose**: verify that `mknodat_via_sandbox_or_fallback` with
`sandbox = None` delegates to the fallback closure.

**Setup**:
1. Create a `TempDir`.
2. Create a source FIFO for metadata.

**Action**:
Call `mknodat_via_sandbox_or_fallback` with:
- `sandbox = None`
- Remaining parameters as in 4.1.
- `fallback = || { /* create FIFO via path-based metadata::create_fifo */ }`

**Assertions**:
1. The fallback closure was invoked (use `AtomicBool` flag).
2. The FIFO exists at the destination path.
3. `symlink_metadata` reports `is_fifo() == true`.

### 4.3 Multi-component relative path falls back even with sandbox

**Purpose**: verify that when `relative_path` has more than one component
(e.g., `subdir/test.fifo`), the helper delegates to the fallback even
when a sandbox is available. This matches the `single_component_leaf`
dispatch logic shared by all `*_via_sandbox_or_fallback` helpers.

**Setup**:
1. Create a `TempDir` with a `subdir/` subdirectory.
2. Open a `DirSandbox` on the root.

**Action**:
Call `mknodat_via_sandbox_or_fallback` with:
- `sandbox = Some(&sandbox)`
- `relative_path = Path::new("subdir/test.fifo")`
- `node_path = sandbox_root.join("subdir/test.fifo")`
- `fallback` closure that creates the FIFO and sets an `AtomicBool`.

**Assertions**:
1. The fallback closure was invoked (atomic flag is `true`).
2. The FIFO exists at `subdir/test.fifo`.

### 4.4 TOCTOU resistance: symlink at leaf does not redirect creation

**Purpose**: verify that `mknodat(dirfd, leaf, S_IFIFO | mode, 0)`
with a `DirSandbox` dirfd does not follow a symlink that occupies the
leaf name. This is the core TOCTOU invariant the SEC-MK series defends.

**Setup**:
1. Create a `TempDir`. Open a `DirSandbox` on it.
2. Create a `sensitive/` directory outside the sandbox root.
3. Place a symlink at `sandbox_root/evil_fifo` pointing to
   `sensitive/target`.

**Action**:
Call `mknodat_via_sandbox_or_fallback` with:
- `sandbox = Some(&sandbox)`
- `relative_path = Path::new("evil_fifo")`
- `node_path = sandbox_root.join("evil_fifo")`
- `mode = S_IFIFO | 0o644`
- `dev = 0`
- `fallback = || panic!("must use dirfd path")`

**Assertions**:
1. The call returns `Err` with `EEXIST`. The symlink occupies the name
   and `mknodat` does not follow it - it sees the name already exists
   and refuses to overwrite.
2. The symlink is still present and still points to `sensitive/target`.
3. `sensitive/target` does **not** exist. No FIFO was created at the
   symlink target. This confirms the dirfd-anchored call never resolved
   through the symlink to an attacker-chosen location.
4. The sensitive directory is untouched.

### 4.5 TOCTOU resistance: symlink-swap race during FIFO creation

**Purpose**: exercise the attacker model from SEC-1.m (symlink-swap
race) against `mknodat_via_sandbox_or_fallback`. An attacker thread
races to replace a regular file at the leaf with a symlink pointing
outside the sandbox between the receiver's decide-to-create moment
and the `mknodat` syscall.

**Setup**:
1. Create a `TempDir` with `dest/` (sandbox root) and `sensitive/`.
2. Open a `DirSandbox` on `dest/`.
3. Write `sensitive/secret` with known contents.

**Pattern**: follows `sec_1_m_symlink_swap_attack.rs` scenario 3
(repeated-race stress loop with `crossbeam-channel` handshakes, not
sleep-based). Deterministic rendezvous prevents scheduler-dependent
flakes.

**Per-iteration**:
1. Place a regular file at `dest/leaf`.
2. Signal attacker to arm the swap.
3. Attacker removes `dest/leaf`, replaces with symlink to
   `sensitive/secret`.
4. Main thread calls `mknodat_via_sandbox_or_fallback` to create a
   FIFO at `dest/leaf`.

**Acceptable interleavings**:
- Receiver wins: `mknodat` sees the regular file, returns `EEXIST`.
  No FIFO created, no sensitive tree touched.
- Attacker wins: `mknodat` sees the symlink, returns `EEXIST`.
  No FIFO created at the symlink target, no sensitive tree touched.
- Tight gap: receiver's pre-check saw the file, attacker removed it,
  `mknodat` succeeds - FIFO is created under the sandbox root (this
  is correct behaviour).

**Per-iteration invariant**: `sensitive/secret` is byte-identical to
its initial contents. The sensitive directory is never modified.

**Iteration count**: 64 (matches `sec_1_m_symlink_swap_attack.rs`).

### 4.6 Round-trip: FIFO survives local-copy transfer

**Purpose**: end-to-end test that creates a source FIFO, runs it through
the `CopyPlan::execute` pipeline with `--specials`, and verifies the
destination entry is `S_IFIFO` with matching permissions.

**Setup**:
1. Create a source `TempDir` with a FIFO at `src/pipe` (mode `0o640`).
2. Create an empty destination `TempDir`.

**Action**:
Build a `LocalCopyPlan` with `specials(true)` and execute with
`LocalCopyExecution::Apply`.

**Assertions**:
1. `summary.fifos_created() == 1`.
2. `symlink_metadata(dest/pipe)` reports `is_fifo() == true`.
3. Permission bits `& 0o777` of the destination match the source
   (subject to umask).
4. The source FIFO still exists (transfer is a copy, not a move).

### 4.7 Sandbox path with specific permissions (0o600, 0o755)

**Purpose**: verify that the `mknodat` dirfd path preserves the
requested permission bits, not just a single hardcoded value.

**Setup**: create a `DirSandbox` on a `TempDir`.

**Action**: call the raw `mknodat` wrapper (or `mkfifoat` convenience)
twice with `S_IFIFO | 0o600` and `S_IFIFO | 0o755`.

**Assertions**: for each FIFO, `symlink_metadata` reports
`permissions().mode() & 0o777` equal to the requested mode (modulo
umask).

### 4.8 Fallback parity: sandbox vs path-based produce identical metadata

**Purpose**: verify that the sandboxed and fallback paths produce
filesystem entries with the same `st_mode` (type + permissions). This
catches regressions where one path encodes the mode differently.

**Setup**:
1. Create two `TempDir`s: one for the sandbox path, one for fallback.
2. Create source metadata from a real FIFO.

**Action**:
- Sandbox path: `mknodat(dirfd, "fifo_a", S_IFIFO | 0o644, 0)`.
- Fallback path: `metadata::create_fifo(&fifo_b_path, &source_metadata)`.

**Assertions**:
1. Both entries are `S_IFIFO`.
2. Both entries have the same permission bits `& 0o7777`.

---

## 5. Platform gating

All test cases are gated with `#![cfg(unix)]` at the file level.
Individual tests that use Linux-only APIs (e.g., `rustix::fs::mknodat`
with `FileType` enum) use:

```rust
#[cfg(not(any(target_os = "ios", target_os = "macos", target_os = "tvos", target_os = "watchos")))]
```

Tests that exercise Apple-specific FIFO creation via `apple_fs::mkfifo`
use the inverse gate.

### 5.1 macOS compatibility

`mknodat(2)` is available since macOS 11.0 (Big Sur). The oc-rsync
minimum deployment target is macOS 11.0, so no runtime version check is
needed. The `DirSandbox` module compiles on macOS and the dirfd-anchored
path works identically to Linux.

The `canonical_tempdir()` helper is required on macOS because
`tempdir()` returns paths under `/var/folders/...` which is a symlink to
`/private/var/folders/...`. Without canonicalization, `DirSandbox::open_root`
may fail under `RESOLVE_NO_SYMLINKS`.

### 5.2 Windows

No FIFO tests compile on Windows. The `#![cfg(unix)]` file-level gate
excludes the entire file. The existing `#[cfg(not(unix))]` stubs in
`metadata::special` (which return `Ok(())`) and
`engine::local_copy::executor::special::fifo` (which creates an empty
placeholder file) are covered by their own unit tests.

---

## 6. Dependencies

| Crate | Used for |
|-------|----------|
| `fast_io` | `DirSandbox`, `mknodat_via_sandbox_or_fallback`, `fstatat_nofollow`, `lstat_via_sandbox_or_fallback` |
| `metadata` | `create_fifo`, `create_fifo_with_fake_super` (fallback-path parity tests) |
| `tempfile` | `TempDir` fixtures |
| `crossbeam-channel` | Deterministic race handshakes (TOCTOU stress test, section 4.5) |

All are existing dev-dependencies of the `transfer` crate.

---

## 7. Relationship to other SEC-MK tasks

| Task | Description | Relationship |
|------|-------------|-------------|
| SEC-MK.a | Code-path inventory | Identified the seven unsandboxed call sites this test series covers |
| SEC-MK.b | Implementation spec for `mknodat`/`mkfifoat` helpers | Defines the API surface these tests exercise |
| SEC-MK.c | `mknodat` raw wrapper implementation | Prerequisite - tests import this function |
| SEC-MK.d | `mknodat_via_sandbox_or_fallback` adaptor implementation | Prerequisite - tests import this function |
| SEC-MK.e | Device-node sandbox regression test | Parallel task covering `S_IFCHR`/`S_IFBLK`; requires root so uses `--fake-super` placeholder path |
| **SEC-MK.f** | **This document** | FIFO regression test - no root needed, exercises real `mknodat` syscall |

---

## 8. Success criteria

1. All eight test cases pass on Linux (CI nextest matrix).
2. All applicable test cases pass on macOS (CI macOS matrix).
3. `cargo clippy --workspace --all-targets --all-features` reports no
   warnings from the new file on any platform.
4. The TOCTOU stress test (4.5) passes 100 consecutive runs without
   flakes (verified by `--retry 3` in CI nextest config).
5. The fallback-parity test (4.8) catches any mode-encoding divergence
   between the sandbox and path-based creation paths.

---

## 9. Files changed

| File | Change |
|------|--------|
| `crates/transfer/tests/sec_mk_f_mkfifo_sandbox.rs` | New integration test file (8 test functions) |

No production code changes. This task is test-only.
