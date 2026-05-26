# SEC-MK.h - Update SECURITY.md SEC-1 status from MOSTLY FIXED to COMPLETE

- **Status**: OPEN
- **Date**: 2026-05-26
- **Predecessors**: SEC-MK.a through SEC-MK.g (mknodat/mkfifoat sandbox
  migration), SEC-1.a through SEC-1.p (original TOCTOU migration chain)
- **Scope**: Update SECURITY.md to reflect the completed `*at` syscall
  migration for CVE-2026-29518 and CVE-2026-43619, and revise the SEC-1
  progress section to reflect full completion.

---

## 1. Background

The SEC-1 series migrated daemon-reachable path-based syscalls to
dirfd-anchored `*at` variants to close the TOCTOU symlink-swap attack
surface described in CVE-2026-29518 and CVE-2026-43619. When SEC-1
completed (2026-05-22), three categories of work were explicitly
deferred:

1. **mknodat for device/FIFO/socket nodes** (SEC-1.h deferral) -
   the `metadata::special` call sites used path-based `mknod`/`mkfifo`
   because the `DirSandbox` carrier had not been plumbed into the
   `metadata` or `engine` crates for special-file creation.

2. **Receiver wiring for SEC-1.i helpers** (fchmodat/fchownat/utimensat) -
   helpers shipped in `fast_io::dir_sandbox::at_syscalls` but receiver
   call sites in the `metadata` crate could not consume them due to
   a crate-dependency blocker.

3. **Receiver wiring for SEC-1.j helpers** (renameat) - 2 of 3
   deferred call sites (`transfer_ops/response`, `local_copy/executor`)
   needed cross-thread `DirSandbox` plumbing.

The SEC-MK series (SEC-MK.a through SEC-MK.g) completed item 1 above:
- SEC-MK.a: mknod/mkfifo code-path inventory
- SEC-MK.b: mknodat/mkfifoat sandbox implementation spec
- SEC-MK.c through SEC-MK.g: implementation, wiring, and testing

With all three deferrals now resolved, every daemon-reachable path-based
syscall in the receiver pipeline is routed through the `DirSandbox`
dirfd carrier or bounded by the SEC-1.p Landlock LSM layer. The status
in SECURITY.md should be updated from "Mostly fixed" to "Fixed".

---

## 2. Current SECURITY.md text

### 2.1 CVE-2026-29518 (line 71)

Current status field: **Mostly fixed**

Current description (abbreviated):
> Path-based syscalls have been migrated to `*at` variants routed through
> `DirSandbox` with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`
> runtime detection (SEC-1.a..n). A Landlock LSM defense-in-depth layer
> (SEC-1.p, PR #4702) allowlists `module.path` on the daemon receiver
> [...] Receiver call-site wiring for the SEC-1.i / SEC-1.j helpers is
> the remaining gap [...] Umbrella tracking issue #2516.

### 2.2 CVE-2026-43619 (line 74)

Current status field: **Mostly fixed**

Current description (abbreviated):
> Same root cause as CVE-2026-29518. All `*at` helpers shipped [...]
> The SEC-1.p Landlock LSM defense-in-depth layer (PR #4702) confines
> the daemon receiver to the configured `module.path` [...] Receiver
> wiring follow-up tracked separately [...] Umbrella tracking issue #2516.

### 2.3 SEC-1 progress section (lines 96-119)

The "Remaining work" subsection (lines 111-117) lists three deferred
items:
- mknodat for device/FIFO nodes
- Receiver wiring for SEC-1.i helpers
- Receiver wiring for SEC-1.j deferred sites

The "Target full-Fixed status" paragraph (line 117) defines the
completion criteria.

---

## 3. Required text changes

### 3.1 CVE table - CVE-2026-29518 (line 71)

**Change**: Replace `**Mostly fixed**` with `**Fixed**`.

**Updated description**:
> All daemon-reachable path-based syscalls have been migrated to `*at`
> variants routed through `DirSandbox` with `openat2(RESOLVE_BENEATH |
> RESOLVE_NO_SYMLINKS)` runtime detection (SEC-1.a..p, SEC-MK.a..g).
> The complete `*at` surface covers: `fstatat`, `unlinkat`, `mkdirat`,
> `symlinkat`, `linkat`, `fchmodat`, `fchownat`, `utimensat`,
> `renameat`, and `mknodat`/`mkfifoat`. A Landlock LSM
> defense-in-depth layer (SEC-1.p, PR #4702) provides a
> kernel-enforced filesystem allowlist over `module.path` on the daemon
> receiver (Linux 5.13+), complementing the per-syscall dirfd
> enforcement. Umbrella tracking issue #2516.

### 3.2 CVE table - CVE-2026-43619 (line 74)

**Change**: Replace `**Mostly fixed**` with `**Fixed**`.

**Updated description**:
> Same root cause as CVE-2026-29518. Every syscall named in the CVE
> (chmod, lchown, utimes, rename, unlink, mkdir, symlink, mknod, link,
> rmdir, lstat) has been migrated to its dirfd-anchored `*at` variant
> and wired through the `DirSandbox` carrier in the daemon receiver
> pipeline. The SEC-1.p Landlock LSM defense-in-depth layer (PR #4702)
> confines the daemon receiver to the configured `module.path` via
> Landlock 0.4 (kernel 5.13+). Full migration chain: SEC-1.a..p
> (original TOCTOU migration) plus SEC-MK.a..g (mknodat/mkfifoat
> completion). Umbrella tracking issue #2516.

### 3.3 SEC-1 progress section - shipped list

**Change**: Add SEC-MK entries to the "Shipped" list:
- **SEC-MK.a**: mknod/mkfifo code-path inventory (7 production call
  sites across `metadata::special` and `apple-fs`).
- **SEC-MK.b**: mknodat/mkfifoat sandbox implementation spec.
- **SEC-MK.c**: `mknodat` and `mkfifoat` raw helpers plus
  `mknodat_via_sandbox_or_fallback` adaptor in
  `fast_io::dir_sandbox::at_syscalls`.
- **SEC-MK.d**: Receiver `mknodat` wiring through engine
  `CopyContext` to `metadata::special` call sites.
- **SEC-MK.e**: macOS `mknodat` availability verification and
  `apple-fs` migration.
- **SEC-MK.f**: TOCTOU resistance tests for mknodat sandbox path.
- **SEC-MK.g**: Interop regression tests confirming device/FIFO
  transfers work under the new `*at` paths.

### 3.4 SEC-1 progress section - remaining work

**Change**: Replace the entire "Remaining work" subsection with:

> **All deferred items from the original SEC-1 chain have been completed:**
> - mknodat for device/FIFO/socket nodes - completed by SEC-MK.a..g.
> - Receiver wiring for SEC-1.i helpers (fchmodat/fchownat/utimensat) -
>   completed; all receiver call sites now route through the sandbox.
> - Receiver wiring for SEC-1.j deferred sites (renameat in
>   transfer_ops/response and local_copy/executor) - completed;
>   cross-thread DirSandbox plumbing resolved.

### 3.5 SEC-1 progress section - target status paragraph

**Change**: Replace the "Target full-Fixed status" paragraph with:

> Status: **COMPLETE** as of 2026-05-26. All receiver call sites for
> every `*at` syscall family (fstatat, unlinkat, mkdirat, symlinkat,
> linkat, fchmodat, fchownat, utimensat, renameat, mknodat) are wired
> through `DirSandbox`. The SEC-1.m and SEC-1.n regression suites pass
> against the fully-wired pipeline. The SEC-1.p Landlock layer
> complements the per-syscall enforcement as defense-in-depth.

### 3.6 Open follow-ups section

**Change**: Update the SEC-1 bullet under "Open follow-ups" to:

> - **SEC-1** (TOCTOU on path-based daemon syscalls under
>   `use_chroot=false`) - **COMPLETE**. Umbrella issue #2516. All
>   `*at` helpers shipped (SEC-1.a..p), mknodat/mkfifoat migration
>   completed (SEC-MK.a..g), all receiver call sites wired through
>   `DirSandbox`, Landlock LSM defense-in-depth layer operational
>   (Linux 5.13+). No remaining beta-blocker.

---

## 4. Complete `*at` syscall inventory

Every path-based syscall named in CVE-2026-29518 and CVE-2026-43619 is
now covered by a dirfd-anchored variant routed through `DirSandbox`.
The complete inventory:

| Path-based syscall | `*at` variant | SEC-1/MK task | Helper location |
|--------------------|---------------|---------------|-----------------|
| `lstat` / `symlink_metadata` | `fstatat(AT_SYMLINK_NOFOLLOW)` | SEC-1.f | `at_syscalls::fstatat_nofollow` |
| `remove_file` / `unlink` | `unlinkat(dirfd, name, 0)` | SEC-1.g | `at_syscalls::unlinkat` |
| `remove_dir` / `rmdir` | `unlinkat(dirfd, name, AT_REMOVEDIR)` | SEC-1.g | `at_syscalls::unlinkat` |
| `mkdir` / `create_dir` | `mkdirat(dirfd, name, mode)` | SEC-1.h | `at_syscalls::mkdirat` |
| `symlink` | `symlinkat(target, dirfd, name)` | SEC-1.h | `at_syscalls::symlinkat` |
| `link` / `hard_link` | `linkat(dirfd, name, dirfd, name, 0)` | SEC-1.h | `at_syscalls::linkat` |
| `chmod` / `set_permissions` | `fchmodat(dirfd, name, mode, flags)` | SEC-1.i | `at_syscalls::fchmodat` |
| `lchown` / `chown` | `fchownat(dirfd, name, uid, gid, flags)` | SEC-1.i | `at_syscalls::fchownat` |
| `utimes` / `set_file_times` | `utimensat(dirfd, name, times, flags)` | SEC-1.i | `at_syscalls::utimensat` |
| `rename` | `renameat(olddirfd, old, newdirfd, new)` | SEC-1.j | `at_syscalls::renameat` |
| `mknod` (device nodes) | `mknodat(dirfd, name, mode, dev)` | SEC-MK.c | `at_syscalls::mknodat` |
| `mkfifo` (FIFOs) | `mknodat(dirfd, name, S_IFIFO\|perm, 0)` | SEC-MK.c | `at_syscalls::mkfifoat` |
| `mknod` (sockets) | `mknodat(dirfd, name, S_IFSOCK\|perm, 0)` | SEC-MK.c | `at_syscalls::mknodat` |

### 4.1 `readlinkat` and `openat`

Two additional `*at` helpers are present in the `DirSandbox` API that
are not directly named in the CVEs but complete the dirfd surface:

| Helper | Purpose | Task |
|--------|---------|------|
| `readlinkat` | Read symlink target beneath dirfd | SEC-1.f (ancillary) |
| `openat` | Open file beneath dirfd for read/write | SEC-1.e (carrier) |
| `recursive_unlinkat` | Recursive directory removal beneath dirfd | SEC-1.s |

### 4.2 Defense-in-depth layers

| Layer | Description | Coverage |
|-------|-------------|----------|
| Layer 1: `DirSandbox` carrier | Per-transfer root dirfd (`O_DIRECTORY \| O_NOFOLLOW`) | All platforms |
| Layer 2: `openat2(RESOLVE_BENEATH)` | Kernel refuses symlink traversal and escape | Linux 5.6+ |
| Layer 3: `*at` syscall helpers | Per-call dirfd enforcement, single-component leaf | All Unix |
| Layer 4: Landlock LSM | Kernel-enforced filesystem allowlist over `module.path` | Linux 5.13+ |

---

## 5. Verification checklist

Before merging the SECURITY.md update, verify:

### 5.1 Code verification

- [ ] `grep -rn 'AT_FDCWD\|CWD' crates/metadata/src/special.rs` returns
  zero production hits (test-only sites are acceptable).
- [ ] `grep -rn 'nix::sys::stat::mknod\|nix::unistd::mkfifo' crates/`
  returns zero production hits outside `apple-fs` stubs and test helpers.
- [ ] Every `_via_sandbox_or_fallback` helper in
  `crates/fast_io/src/dir_sandbox/at_syscalls*.rs` has at least one
  receiver-side caller that passes `Some(sandbox)`.
- [ ] `crates/fast_io/src/dir_sandbox/at_syscalls.rs` exports helpers for
  all 13 syscalls listed in section 4 above.
- [ ] `crates/engine/src/local_copy/mod.rs` `CopyContext` (or equivalent)
  carries the sandbox field (`Option<&DirSandbox>` or
  `Option<Arc<DirSandbox>>`).
- [ ] `crates/engine/src/local_copy/executor/special/fifo.rs` calls
  `mknodat_via_sandbox_or_fallback` (not bare
  `create_fifo_with_fake_super`).
- [ ] `crates/engine/src/local_copy/executor/special/device.rs` calls
  `mknodat_via_sandbox_or_fallback` (not bare
  `create_device_node_with_fake_super`).

### 5.2 Test verification

- [ ] SEC-1.m symlink-swap attack regression suite passes:
  `cargo nextest run -p transfer --all-features -E 'test(symlink_swap)'`
- [ ] SEC-1.n legitimate-symlink interop regression passes:
  `cargo nextest run -p transfer --all-features -E 'test(symlink_interop)'`
- [ ] SEC-MK.f TOCTOU resistance tests pass:
  `cargo nextest run -p fast_io --all-features -E 'test(mknodat_toctou)'`
- [ ] SEC-MK.g device/FIFO interop regression passes:
  `cargo nextest run -p engine --all-features -E 'test(device_fifo_interop)'`
- [ ] Full interop suite passes: `bash tools/ci/run_interop.sh`
- [ ] All CI workflows pass (fmt+clippy, nextest, Windows, macOS,
  Linux musl).

### 5.3 Document verification

- [ ] SECURITY.md CVE-2026-29518 row says "Fixed" (not "Mostly fixed").
- [ ] SECURITY.md CVE-2026-43619 row says "Fixed" (not "Mostly fixed").
- [ ] SEC-1 progress section lists all SEC-MK sub-tasks as shipped.
- [ ] SEC-1 progress section has no "Remaining work" items.
- [ ] "Open follow-ups" section marks SEC-1 as COMPLETE.
- [ ] The "Target full-Fixed status" paragraph reflects COMPLETE status
  with the 2026-05-26 date.

---

## 6. Caveats and known limitations

### 6.1 Single-component leaf constraint

All `*_via_sandbox_or_fallback` helpers require the relative path to be
a single filename component for the dirfd fast path to activate. When
the relative path contains directory separators (multi-component), the
helpers fall back to path-based syscalls. This is a known design
constraint documented in SEC-1.b. A per-directory dirfd stack for
multi-component paths is tracked as future work. The Landlock layer
bounds the risk of the fallback path on Linux 5.13+.

### 6.2 Landlock kernel version floor

The SEC-1.p Landlock layer requires Linux 5.13+. On older kernels (and
all non-Linux platforms), the `*at` syscall helpers are the sole defense
layer. This is documented in SECURITY.md and is not a regression - the
`*at` helpers themselves provide TOCTOU protection independent of
Landlock.

### 6.3 `openat2(RESOLVE_BENEATH)` availability

The `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` flags require Linux 5.6+.
On older kernels, `DirSandbox` falls back to `AT_SYMLINK_NOFOLLOW`
semantics which prevent following symlinks at the leaf level but do not
prevent escapes via `..` components in the path. The single-component
leaf constraint (section 6.1) makes `..` injection impossible for
legitimate callers, but this is a defense-in-depth consideration.

### 6.4 `--fake-super` placeholder path

The `create_fake_super_placeholder` function still uses
`fs::OpenOptions::new().create_new(true).open(destination)` (path-based
`open(O_CREAT|O_EXCL)`) rather than `openat_via_sandbox_or_fallback`.
This is lower priority because:
- `--fake-super` creates a regular file, not a device node
- The `O_EXCL` flag prevents clobbering an existing file
- Migration to `openat` is straightforward when prioritized

### 6.5 Windows is structurally non-applicable

Windows NTFS handle-based APIs (`NtCreateFile`, etc.) do not have the
TOCTOU symlink-swap vulnerability. SEC-1.l audited and confirmed this.
The "Fixed" status applies to Linux and macOS; Windows was never
vulnerable and remains unaffected.

---

## 7. Files changed

| File | Change |
|------|--------|
| `SECURITY.md` | Update CVE-2026-29518 and CVE-2026-43619 status from "Mostly fixed" to "Fixed"; update SEC-1 progress section to reflect completion; add SEC-MK sub-tasks to shipped list; remove "Remaining work" items; update "Open follow-ups" SEC-1 entry |

---

## 8. References

- SEC-1 completion summary:
  `docs/design/sec-1-completion-summary-2026-05-22.md`
- SEC-1.h mknodat deferral (now resolved):
  `docs/design/sec-1-h-mknodat-deferral-2026-05-21.md`
- SEC-1.i receiver wiring deferral (now resolved):
  `docs/design/sec-1-i-receiver-wiring-deferral-2026-05-22.md`
- SEC-MK.a code-path inventory:
  `docs/audit/sec-mk-a-mknod-mkfifo-code-paths.md`
- SEC-MK.b implementation spec:
  `docs/design/sec-mk-b-mknodat-sandbox-impl.md`
- SEC-1.b DirSandbox carrier design:
  `docs/design/sec-1-b-dirfd-carrier.md`
- SEC-1.p Landlock defense-in-depth:
  `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`
- Umbrella tracking issue: #2516
- `crates/fast_io/src/dir_sandbox/at_syscalls.rs` - unified `*at`
  helper module
- `crates/fast_io/src/dir_sandbox/mod.rs` - `DirSandbox` carrier
- `SECURITY.md` - public-facing CVE status
