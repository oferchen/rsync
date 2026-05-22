# SEC-1 chain - TOCTOU mitigation for CVE-2026-29518 + CVE-2026-43619 - final state as of 2026-05-22

Single-page audit reference for the SEC-1 sub-task chain. Maps every sub-task
(.a through .p) to its shipping PR or closure doc so future security audits do
not have to reconstruct the chain from `git log` archaeology.

## Threat model

Under daemon configuration `use_chroot = false`, the receiver resolves
destination paths from attacker-controllable filesystem state. Path-based
syscalls (`lstat`, `unlink`, `mkdir`, `symlink`, `link`, `chmod`, `lchown`,
`utimes`, `rename`, `mknod`) are vulnerable to symlink-swap races between path
resolution and kernel syscall dispatch. The mitigation is to anchor every
receiver-side syscall on a parent dirfd that the attacker cannot redirect,
gated by `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` when the kernel
exposes it. Windows NTFS handle-based APIs structurally sidestep this window
and need no migration; macOS exposes the same BSD `*at` family as Linux.

## Architecture (4-layer defense)

```
+---------------------------------------------------------------+
|                  Layer 4: Landlock LSM                        |
|  (Linux 5.13+, defense-in-depth, kernel-enforced allowlist)   |
|  Design: #4699  /  Impl in flight: #4702                      |
+---------------------------------------------------------------+
                              ^
                              | (rejects out-of-tree paths even if a
                              |  syscall escapes the *at* helpers)
                              |
+---------------------------------------------------------------+
|       Layer 3: *at syscall helpers (per-call enforcement)     |
|       fast_io::dir_sandbox::at_syscalls                       |
|       fstatat / unlinkat / mkdirat / symlinkat / linkat       |
|       fchmodat / fchownat / utimensat / renameat              |
+---------------------------------------------------------------+
                              ^
                              | (every *at call takes (dirfd, leaf,
                              |  RESOLVE_BENEATH) -- no path traversal
                              |  past the carrier root)
                              |
+---------------------------------------------------------------+
|       Layer 2: openat2(RESOLVE_BENEATH) runtime detection     |
|       fast_io::dir_sandbox::openat2_supported()               |
|       Linux 5.6+; falls back to AT_SYMLINK_NOFOLLOW otherwise |
+---------------------------------------------------------------+
                              ^
                              | (kernel refuses to traverse symlinks or
                              |  escape the dirfd's subtree)
                              |
+---------------------------------------------------------------+
|       Layer 1: DirSandbox carrier (per-transfer root dirfd)   |
|       fast_io::dir_sandbox::DirSandbox                        |
|       O_DIRECTORY | O_NOFOLLOW root + dirfd stack             |
|       Threaded through receiver pipeline (SEC-1.e)            |
+---------------------------------------------------------------+
```

## Sub-task ledger

| ID | Description | Status | PR / Doc |
|----|-------------|--------|----------|
| SEC-1.a | Audit path-based syscall surface (107 sites / 36 files) | SHIPPED | #4614, `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` |
| SEC-1.b | DirSandbox carrier design | SHIPPED | #4617, `docs/design/sec-1-b-dirfd-carrier.md` |
| SEC-1.c | `secure_open_dir` helper | SHIPPED | #4618 |
| SEC-1.d | `openat2(RESOLVE_BENEATH)` runtime detection probe | SHIPPED | #4643 |
| SEC-1.e | Wire parent-dirfd `DirSandbox` through receiver pipeline | SHIPPED | #4650 |
| SEC-1.f | `lstat` / `symlink_metadata` -> `fstatat(AT_SYMLINK_NOFOLLOW)` | SHIPPED | #4668 |
| SEC-1.g | `remove_file` / `remove_dir` -> `unlinkat` | SHIPPED | #4671 |
| SEC-1.h | `mkdir` / `symlink` / `link` -> `mkdirat` / `symlinkat` / `linkat` | SHIPPED (mknodat DEFERRED) | #4683; closure `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md` (#4694) |
| SEC-1.i | `chmod` / `lchown` / `utimes` -> `fchmodat` / `fchownat` / `utimensat` | SHIPPED (receiver wiring DEFERRED) | #4690; closure `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md` (#4701) |
| SEC-1.j | `rename` -> `renameat` | SHIPPED (receiver wiring PARTIAL: 1/3 sites) | helper #4693; partial receiver wiring #4697 |
| SEC-1.k | macOS `*at` syscall availability audit | SHIPPED | #4623, `docs/audits/sec-1-k-macos-at-syscalls-2026-05-21.md` |
| SEC-1.l | Windows NTFS handle-based dispatch audit (CVEs N/A on Windows) | SHIPPED | #4622, `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` |
| SEC-1.m | Symlink-swap attack regression test | SHIPPED | #4675 |
| SEC-1.n | Legitimate-symlink interop regression | SHIPPED | #4678 |
| SEC-1.o | `SECURITY.md` status update | SHIPPED | partial #4672; full #4691 + #4698 |
| SEC-1.p | Landlock LSM defense-in-depth | DESIGN SHIPPED #4699 / IMPL IN FLIGHT #4702 | design `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` |

Hygiene follow-up (not a sub-task, listed for completeness): `*at` helpers
re-folded into a single `at_syscalls` module post-`.j` ship - planned #4695,
shipped #4700. Receiver-side wiring tracked separately in #4703 (README) and
#4705 (man pages).

## Open follow-ups

Three deferrals remain after the main chain. Each is captured in a closure
doc with explicit re-open triggers.

1. **SEC-1.h - `mknodat` for device / FIFO / socket nodes**
   - Closure: `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md`
   - Reason: opt-in via `--devices` / `--specials`, default-deny on daemon
     write paths, sits behind `metadata` crate boundary that does not yet
     carry `DirSandbox`.
   - Re-open trigger: daemon default-allows device creation, or `metadata`
     crate gains a `fast_io` dependency.

2. **SEC-1.i - receiver wiring for `fchmodat` / `fchownat` / `utimensat` (6 sites)**
   - Closure: `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md`
   - Reason: helpers shipped in `fast_io::dir_sandbox::at_syscalls`, but the
     receiver-side metadata applier lives in the `metadata` crate, which
     cannot today depend on `fast_io` without forming a crate cycle. Carrier
     refactor (lift `DirSandbox` to a leaf crate) is the unblocker.
   - Re-open trigger: SEC-1.b carrier refactor lands, OR receiver metadata
     application is moved into a crate that can depend on `fast_io`.

3. **SEC-1.j - receiver wiring for `renameat` (2 of 3 sites)**
   - PR #4697 wired one of three deferred call sites
     (`disk_commit`); the remaining two (`transfer_ops/response`,
     `local_copy/executor`) are marked with TODO comments in that PR.
   - Reason: those two sites need cross-thread `DirSandbox` plumbing that
     is not currently available on the executor worker.
   - Re-open trigger: cross-thread carrier (`Arc<DirSandbox>`) lands, OR the
     executor worker is refactored to receive the carrier on entry.

## CVE status mapping

| CVE | Surface | Mitigated by | Residual risk |
|-----|---------|--------------|---------------|
| CVE-2026-29518 | TOCTOU symlink race on receiver path resolution | SEC-1.a..g (carrier + `fstatat` + `unlinkat`) plus SEC-1.h create-class helpers; receiver wiring is end-to-end for the daemon-reachable surface | Future receiver-adjacent code that bypasses `DirSandbox` (mitigated by SEC-1.p Landlock layer once shipped) |
| CVE-2026-43619 | Symlink races on chmod / lchown / utimes / rename / unlink / mkdir / symlink / mknod / link / rmdir / lstat | SEC-1.f (lstat), .g (unlink/rmdir), .h (mkdir/symlink/link), .i (chmod/lchown/utimes), .j (rename) helpers all shipped | mknod deferred (SEC-1.h closure); SEC-1.i + SEC-1.j receiver wiring deferrals still rely on fallback `std::fs::*` on the un-wired sites (`metadata` crate boundary); Landlock layer (SEC-1.p) closes the gap once shipped |
| CVE-2026-29518 + CVE-2026-43619 | Windows | N/A | Windows NTFS handle-based APIs structurally sidestep the TOCTOU window (SEC-1.l) |
| CVE-2026-29518 + CVE-2026-43619 | macOS | SEC-1.k confirms BSD `*at` family is available; same Linux migration applies | None beyond Linux residuals |

`SECURITY.md` currently reports both CVEs as "Mostly fixed". They flip to
"Fixed" when:
1. SEC-1.i receiver wiring (all 6 sites) lands, AND
2. SEC-1.j receiver wiring (remaining 2 of 3 sites) lands, AND
3. SEC-1.p Landlock LSM defense-in-depth ships OR is closed as N/A.

## Files of interest

For future maintainers tracing the chain:

- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` - unified `*at` helper
  module (post-#4700 re-fold): `fstatat_nofollow`, `unlinkat`, `mkdirat`,
  `symlinkat`, `linkat`, `fchmodat`, `fchownat`, `utimensat`, `renameat`,
  plus `_via_sandbox_or_fallback` wrappers and `single_component_leaf`
  normalizer.
- `crates/fast_io/src/dir_sandbox/mod.rs` - `DirSandbox` carrier
  (`O_DIRECTORY | O_NOFOLLOW` root dirfd, dirfd stack, in-tree cache).
- `crates/fast_io/src/dir_sandbox/tests.rs` - sandbox unit and
  symlink-swap attack tests.
- `crates/fast_io/src/landlock.rs` - Linux Landlock allowlist layer (lands
  with #4702).
- `crates/fast_io/src/landlock_stub.rs` - non-Linux stub (lands with #4702).
- `crates/daemon/src/daemon/sections/module_access/transfer.rs` - single
  Landlock call-site in the daemon (lands with #4702).
- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` - per-site
  enumeration of the 107 path syscalls audited.
- `docs/design/sec-1-b-dirfd-carrier.md` - carrier design rationale and
  in-tree cache invariants.
- `docs/design/sec-1-at-syscalls-refold-2026-05-21.md` - post-`.j` module
  consolidation plan.
- `SECURITY.md` - public-facing CVE status (`SEC-1 progress` note under
  "Upstream rsync 3.4.3 audits").

## Re-open trigger summary

Reopen the SEC-1 chain (re-audit + new sub-tasks) if any of the following
occur:

1. **New CVE on a path-based syscall not in the current `*at` set.** Add a
   new sub-task per missing syscall and wire it through `DirSandbox` using
   the existing helper template in `at_syscalls.rs`.
2. **Receiver wiring deferral blockers unstick.** Specifically, the
   `metadata` crate gains a `fast_io` dependency (no crate cycle) - which
   unblocks SEC-1.i receiver wiring, or the cross-thread `DirSandbox`
   carrier ships - which unblocks SEC-1.j sites 2 and 3.
3. **Daemon gains new writable paths outside `module.path`.** Any new
   operand that introduces an additional root (analogous to `--backup-dir`,
   `--temp-dir`, `--partial-dir`, `--link-dest`, `--copy-dest`,
   `--compare-dest`) must be registered with the dirfd cache and audited
   against the SEC-1.p Landlock ruleset.
4. **`single_component_leaf` normalizer is modified.** It is the
   single-point-of-trust for `*at`-leaf safety; any change requires
   re-running SEC-1.m regression tests plus a fresh fuzz pass.
5. **A future commit re-introduces direct `std::fs::*` or `libc::*at`
   calls with attacker-controlled paths outside `DirSandbox`.** Treated as
   a regression on SEC-1.a's surface enumeration; flag during review and
   re-route through the carrier.
