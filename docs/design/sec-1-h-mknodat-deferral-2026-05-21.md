# SEC-1.h - mknodat deferral closure

- **Status**: DEFERRED (audit-acknowledged, not scheduled)
- **Date**: 2026-05-21
- **Ship reference**: SEC-1.h landed in PR #4683 (master commit `2cfb8ba6e`,
  `feat(fast_io): mkdirat/symlinkat/linkat sandbox helpers (SEC-1.h)`).
- **Scope owner**: SEC-1 audit chain
- **Re-audit trigger**: see [Re-open triggers](#re-open-triggers) below.

## Summary

SEC-1.h shipped the create-class `*at` cutover used by the receiver:
`mkdirat`, `symlinkat`, and `linkat` helpers anchored on a
[`DirSandbox`](../../crates/fast_io/src/dir_sandbox/at_syscalls.rs)
dirfd. Three sibling syscalls remain outside the cutover:

1. `mknodat` for character/block device nodes.
2. `mknodat` (with `S_IFIFO`) for FIFOs.
3. `mknodat` (with `S_IFSOCK`) for Unix-domain sockets.

These three call sites all flow through
`metadata::create_device_node_with_fake_super`,
`metadata::create_fifo_with_fake_super`, and (on Apple targets)
`metadata::apple_fs::mknod`. SEC-1.h did **not** thread the sandbox
into them. This document records why that punt was made, what the
shipped helper would look like, and the explicit triggers that flip
the deferral back into work.

## Why mknodat is the only `*at` helper outside SEC-1.h's shipped set

`mknodat` is the only remaining receiver-side `*at` create syscall
that lives outside the sandbox cutover. Three reasons made the
deferral the right call at the time SEC-1.h landed:

1. **Opt-in capability gate.** Device and special-file creation in the
   receiver is gated on the upstream `--devices` / `--specials`
   command-line flags. The daemon path typically refuses these without
   explicit `[module] write only = no` plus client opt-in; the
   default-deny posture means the daemon-reachable surface that
   CVE-2026-29518 and CVE-2026-43619 mitigations target does not
   include device-node creation.
2. **Carrier plumbing not in place.** The three call sites
   (`metadata::create_device_node_with_fake_super`,
   `metadata::create_fifo_with_fake_super`, and
   `metadata::apple_fs::mknod`) live in the `metadata` crate, which
   currently has no awareness of `DirSandbox`. Threading the sandbox
   through these signatures requires a carrier-plumbing refactor of
   `metadata` (plus the engine `local_copy::executor::special::*`
   callers in `crates/engine/src/local_copy/executor/special/`) just
   to reach the syscall. SEC-1.h kept the surface narrow on purpose;
   pulling that refactor in would have doubled the patch size for a
   call path the threat model does not currently reach.
3. **Not on the daemon-reachable TOCTOU surface.** The
   `use_chroot=false` CVE chain (CVE-2026-29518, CVE-2026-43619)
   targets path-name resolution against attacker-staged symlink swaps
   under the module root. The mknod path operates on already-resolved
   leaves, runs only with caller opt-in, and produces inodes whose
   content cannot be exfiltrated by the attacker who would race the
   symlink: there is no payload to redirect because device nodes are
   metadata-only. The CVE chain therefore does not reach
   `create_device_node_with_fake_super` or its siblings under any
   currently-known daemon configuration.

## Helper sketch (when implemented)

When the deferral is re-opened, the helper will live alongside the
SEC-1.h helpers in `crates/fast_io/src/dir_sandbox/at_syscalls.rs`
and mirror the shape of `mkdirat_via_sandbox_or_fallback`:

```rust
// Raw libc wrapper, peer of `mkdirat`, `symlinkat`, `linkat`.
pub fn mknodat(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    mode: u32,
    dev: u64,
) -> io::Result<()>;

// SEC-1.h-style adaptor: anchors the create on the sandbox dirfd
// when the destination is a single-component leaf under
// `dest_dir`; falls back to `metadata::create_device_node_with_fake_super`
// or `metadata::create_fifo_with_fake_super` otherwise.
pub fn mknodat_via_sandbox_or_fallback(
    sandbox: Option<&super::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    mode: u32,
    dev: u64,
) -> io::Result<()>;
```

The fallback semantics are identical to `mkdirat_via_sandbox_or_fallback`
in `at_syscalls.rs`:

- `sandbox: Some(_)` and `relative_path` has a single component:
  resolve the leaf through `sandbox.current_dirfd()` and issue
  `libc::mknodat`. A mid-syscall symlink swap on the leaf cannot
  redirect the create to an attacker-chosen parent.
- Every other case: fall back to the existing `metadata` entry points
  (`create_device_node_with_fake_super`,
  `create_fifo_with_fake_super`, or `apple_fs::mknod`) verbatim. The
  fallback preserves the `--fake-super` placeholder substitution
  upstream rsync performs in `syscall.c:do_mknod()` when
  `am_root < 0`.

## Re-open triggers

This deferral re-opens immediately under any of the following
conditions:

1. **Daemon-reachable surface expands.** Any change that allows the
   daemon to accept `--devices` or `--specials` without explicit
   per-module opt-in, or that exposes device-node creation to an
   unauthenticated peer, requires a CVE re-audit and a mknodat
   cutover.
2. **`metadata` crate gains `DirSandbox` plumbing for an unrelated
   reason.** The carrier-plumbing refactor is the long pole. If any
   other initiative threads `DirSandbox` (or an equivalent dirfd
   carrier) through `create_device_node_with_fake_super`,
   `create_fifo_with_fake_super`, and `apple_fs::mknod`, folding
   mknodat into the SEC-1 mitigations becomes a same-PR addition
   rather than a standalone refactor.
3. **New CVE exercises path-based mknod in the receiver.** A
   published advisory that names mknod, mknodat, or device-node
   creation as a TOCTOU vector in any rsync (upstream or fork) flips
   this back to in-scope regardless of triggers 1 and 2.

## Cross-platform notes

When mknodat is implemented, the platform matrix matches the SEC-1.h
helpers:

- **Linux**: `libc::mknodat` is always available. Direct cutover, no
  fallback needed for OS-version reasons.
- **macOS**: `libc::mknodat` has been available since macOS 11 (Big
  Sur). On older macOS, the helper falls back to the existing
  `metadata::apple_fs::mknod` path. The `metadata::apple_fs::mknod`
  wrapper is already the fallback target for the Apple branch of
  `create_fifo_inner` and `create_device_node_inner` in
  `crates/metadata/src/special.rs`.
- **Windows**: device nodes have no POSIX equivalent. The fallback
  path in `metadata::special` already returns `Ok(())` for the
  non-Unix branch, and the sandbox helper would short-circuit to the
  fallback for the same reason. No new Windows surface is introduced.

## Cited surface

The helper pattern this deferral would extend is established by these
specific surfaces in the SEC-1.h ship:

- `crates/fast_io/src/dir_sandbox/at_syscalls.rs:583` -
  `pub fn mkdirat_via_sandbox_or_fallback(...)` (template for the
  mknodat adaptor signature and fallback semantics).
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs:439` -
  `pub fn mkdirat(dirfd: BorrowedFd<'_>, name: &OsStr, mode: u32)`
  (template for the raw `libc::mknodat` wrapper).
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs:13-14` - module
  docstring listing the SEC-1.h create-class members; mknodat would
  appear here when added.
- `crates/metadata/src/special.rs:39` -
  `pub fn create_fifo_with_fake_super(...)` (fallback target).
- `crates/metadata/src/special.rs:66` -
  `pub fn create_device_node_with_fake_super(...)` (fallback target).
- `crates/metadata/src/special.rs:228` and
  `crates/metadata/src/special.rs:281` -
  `apple_fs::mknod` call sites for Apple platforms (fallback target
  for the macOS-older-than-11 branch).

## Closure

The SEC-1 audit can treat the mknodat sub-question as **closed-deferred**:
the cutover is documented, the helper signature is sketched, the
re-open triggers are explicit, and the threat model that justifies
the defer is recorded next to the surface it covers. No further
work is scheduled until one of the [Re-open triggers](#re-open-triggers)
fires.
