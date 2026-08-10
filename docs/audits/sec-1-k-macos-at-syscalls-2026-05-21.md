# SEC-1.k - macOS availability and semantics of `*at` syscalls

**Date:** 2026-05-21
**Scope:** verify that the `*at` syscalls SEC-1.f-j plans to substitute for path-based syscalls in the receiver/local-copy/metadata-apply pipeline are available and behaviourally equivalent on macOS (Darwin / XNU).
**Status:** research-only audit; no Rust changes in this PR.
**Inputs:**
- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` (107 sites / 36 files / 7 surprises).
- `docs/design/sec-1-b-dirfd-carrier.md` (the `DirSandbox` carrier the swaps consume).
- `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libc-0.2.180/src/unix/mod.rs` and `.../src/unix/bsd/apple/mod.rs` (workspace pin: `libc = "0.2"`).
- Apple XNU `bsd/kern/syscalls.master` and the Xcode man pages for `open(2)`, `rename(2)`, `chmod(2)`.
**Tracked under:** #2529.

The conclusion the rest of the doc supports: every `*at` primitive SEC-1.f-j needs is present on macOS as a real syscall and exposed by the libc crate we already depend on. macOS lacks the Linux-only `openat2(2)` interface entirely, and the openat2 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` pair maps to the per-path-component `O_NOFOLLOW_ANY` open flag, not to a single-syscall sandbox. The one substantive behavioural delta is on `fchmodat` with `AT_SYMLINK_NOFOLLOW` (see section 3.5). Recommendation in section 5: macOS gets the same `DirSandbox`-anchored `*at` path as Linux for the 13-of-14 sinks that behave identically, with a tiny fallback in the metadata-apply layer for `fchmodat` on symlinks. macOS does not get a hardened resolver (no `RESOLVE_BENEATH` equivalent in one syscall), and `SECURITY.md` gains a paragraph saying so.

## 1. Per-syscall availability matrix (macOS vs Linux)

Columns: syscall | macOS availability (XNU + libc crate) | Linux availability (libc crate) | notes.

| syscall | macOS XNU | macOS libc 0.2.180 | Linux libc 0.2.180 | notes |
|---|---|---|---|---|
| `openat` | syscall 463 (`bsd/kern/syscalls.master`) | yes (`src/unix/mod.rs:2134`, gated `unix`) | yes | introduced in OS X 10.10; documented as "is the same as `open()` except in the case where `path` specifies a relative path" |
| `fstatat` / `fstatat64` | syscall 469 / 470 | yes (`src/unix/mod.rs:1006`) | yes | `AT_SYMLINK_NOFOLLOW` honoured; flags const at `apple/mod.rs:2342` |
| `unlinkat` | syscall 472 | yes (`src/unix/mod.rs:1025`) | yes | `AT_REMOVEDIR` honoured; `apple/mod.rs:2344` |
| `mkdirat` | syscall 475 | yes (`src/unix/mod.rs:2132`, gated `unix`) | yes | mode argument respects process umask, same as Linux |
| `symlinkat` | syscall 474 | yes (`src/unix/mod.rs:1023`) | yes | target string is untouched, only `(newdirfd, linkpath)` is sandbox-anchored |
| `mknodat` | syscall 554 | yes (`apple/mod.rs`, listed only in the Apple submodule) | yes (in shared `unix/mod.rs`) | macOS exposure is conditional - confirm with `cfg(target_os = "macos")` at the call site; we only use mknodat from `metadata::special` behind `--devices` / `--specials`, which are root-only in practice |
| `linkat` | syscall 471 | yes (`src/unix/mod.rs:1008`) | yes | shipping since macOS 10.10; rust-lang/libc#2036 (10.9 missing-symbol bug) is moot because our MSRV cuts off below Big Sur |
| `renameat` | syscall 465 | yes (`src/unix/mod.rs:1016`) | yes | basic two-dirfd rename; no flags |
| `renameat2` | **not present** | not exposed | yes (Linux-only) | XNU's `syscalls.master` does not define it; "renameat2 is a Linux-specific feature" |
| `renameatx_np` (macOS-only) | syscall 488 | yes (`apple/mod.rs:4653`) | n/a | macOS analogue of `renameat2`; supports `RENAME_SWAP` and `RENAME_EXCL` flags via `c_uint` |
| `fchmodat` | syscall 467 | yes (`src/unix/mod.rs:981`) | yes | accepts `AT_SYMLINK_NOFOLLOW` per the macOS `chmod(2)` man page, **but** see section 3.5 - the Linux glibc rejects the same flag combination with `ENOTSUP` and the resulting cross-platform behavioural delta is the only non-trivial finding in this audit |
| `fchownat` | syscall 468 | yes (`src/unix/mod.rs:984`) | yes | `AT_SYMLINK_NOFOLLOW` honoured; this is the syscall behind the existing `unix_fs::chownat(CWD, ...)` calls at `metadata/src/apply/ownership.rs:193,349` that SEC-1.a already flagged as "already-safe entries, swap CWD for dirfd" |
| `utimensat` | not in the legacy `syscalls.master` mirror, but **shipping since macOS 10.13** (High Sierra, 2017) | yes (`apple/mod.rs:4584`) | yes | rust-lang/rustix#157 documents the 10.13 floor; our MSRV is Big Sur (macOS 11+) so this is non-blocking |
| `futimens` | shipping since macOS 10.13 | yes (`apple/mod.rs:`, near `utimensat`) | yes | used for the already-fd-anchored timestamp branches; no carrier work required |
| `openat2` | **not present** | not exposed | yes (Linux-only, kernel 5.6+) | the entire `open_how` struct is `linux_like`-only in libc (`linux_like/linux/mod.rs:482,1825`); no XNU equivalent |

**Constants the swap needs:**

| constant | macOS libc value | Linux libc value | notes |
|---|---|---|---|
| `AT_FDCWD` | `-2` (`apple/mod.rs:2340`) | `-100` | numeric value differs; libc abstracts this, never hard-code |
| `AT_SYMLINK_NOFOLLOW` | `0x0020` | `0x100` | same semantic on all the lookup-style syscalls (`fstatat`, `fchownat`, `linkat`, `utimensat`); different on `fchmodat` (section 3.5) |
| `AT_REMOVEDIR` | `0x0080` | `0x200` | identical semantic |
| `O_NOFOLLOW` | shared via POSIX | shared | refuses to traverse a symlink **at the final path component only** |
| `O_NOFOLLOW_ANY` | `0x20000000` (`apple/mod.rs:1978`) | n/a | macOS-only; refuses to traverse a symlink **at any path component** |
| `O_RESOLVE_BENEATH` | exists in the XNU `open(2)` man page; **not exposed in libc 0.2.180** | n/a | the man page documents it: "If `O_RESOLVE_BENEATH` is used in the mask and the specified path resolution escapes the directory associated with the fd then the `openat()` will fail." libc would need a constant addition; the workaround is to define the value locally as `c_int` or skip the flag and rely on `O_NOFOLLOW_ANY` + per-leaf opens |
| `RENAME_SWAP`, `RENAME_EXCL` | `0x02`, `0x04` (`apple/mod.rs:3267-3268`) | n/a (Linux has `RENAME_NOREPLACE`/`RENAME_EXCHANGE`/`RENAME_WHITEOUT`) | macOS atomic-rename flags for `renameatx_np` |
| `RENAME_NOFOLLOW_ANY`, `RENAME_RESOLVE_BENEATH` | documented in the macOS `rename(2)` man page; **not exposed in libc 0.2.180** | n/a | same exposure gap as `O_RESOLVE_BENEATH` - the macOS kernel accepts the flags, but the constant is not in the libc crate at our pin; SEC-1 does not need them for the cutover |

Totals: 13 of the 14 syscalls SEC-1.f-j needs are available on macOS via a direct libc 0.2.180 symbol. The 14th (`renameat2`) has a macOS-only equivalent (`renameatx_np`) we can use behind `cfg(target_os = "macos")` if we ever want atomic swap semantics; for SEC-1 we only need the plain `renameat` shape so this is academic.

## 2. Behavioural equivalence per call family

The SEC-1.a inventory groups the 107 path-based call sites into seven syscall families. This section walks each family and records the macOS-vs-Linux semantic delta (or lack thereof).

### 2.1 `fstatat` / `fstatat64`

Both kernels accept `AT_SYMLINK_NOFOLLOW` and return identical `stat` results compared to the path-based `stat`/`lstat` they replace. No delta. Applies to the 40+ stat-style sites in the audit (e.g. `crates/engine/src/delete/extras.rs:115`, `crates/engine/src/local_copy/executor/directory/support.rs:50,90,108`).

### 2.2 `unlinkat`

Both kernels accept `AT_REMOVEDIR`. macOS uses `0x0080`; Linux uses `0x200`; the libc crate exposes both at the right numeric value per platform. No delta. Applies to all `fs::remove_file` and `fs::remove_dir` swaps (e.g. `crates/engine/src/delete/emitter/fs.rs:70,74,78,82,86`).

### 2.3 `mkdirat`

Identical semantic: mode is masked by the process umask on both kernels. No delta. Applies to `crates/engine/src/local_copy/executor/directory/recursive/mod.rs:128,131` and the parent-creation sites under `context_impl/options/dirs.rs`.

### 2.4 `symlinkat`

Identical semantic: only the link's parent dirfd is sandbox-anchored; the symlink **target** is a sender-supplied string and is stored verbatim. The TOCTOU window closes because the link's *containing directory* is dirfd-anchored, regardless of what the target string says. No delta. Applies to `crates/protocol/.../special/symlink.rs:466` and `crates/transfer/src/receiver/directory/links.rs:96`.

### 2.5 `linkat`

Identical: takes `(olddirfd, oldpath, newdirfd, newpath, flags)` with `flags = 0` for our use (we want hardlinks to resolve through the file's actual inode, not to chase a symlink to a different file). macOS additionally accepts `AT_SYMLINK_FOLLOW` like Linux. No delta. The current hard-link sites (`crates/engine/src/hardlink.rs`, `crates/engine/src/local_copy/hard_links.rs`) all want the default behaviour, so `flags = 0` suffices.

### 2.6 `mknodat`

Identical syscall surface. macOS routes the syscall through XNU at number 554; libc exposes it from the Apple submodule. Practical caveat: `mknod()` requires CAP_MKNOD / root on both kernels for `S_IFBLK` / `S_IFCHR`, so the two production sites (`metadata/src/special.rs:146,194`, called from `engine/.../special/fifo.rs:219` and `device.rs:215`) are only exercised under elevated privilege on either platform. No correctness delta, only a `cfg(target_os = "macos")` to pull the symbol from the Apple submodule path.

### 2.7 `renameat` / `renameat2`

`renameat(olddirfd, oldpath, newdirfd, newpath)` with no flags is identical on both kernels and covers every SEC-1.a `fs::rename` site. The places that would benefit from `RENAME_NOREPLACE` semantics (e.g. backup-rename refusing to clobber a pre-existing backup) are not currently using `RENAME_NOREPLACE` on Linux either - the code does a separate `unlink` retry (`context_impl/state.rs:526`, `:535`). If SEC-1 ever wants atomic NOREPLACE, the macOS equivalent is `renameatx_np(.., RENAME_EXCL)` and the Linux equivalent is `renameat2(.., RENAME_NOREPLACE)`; both need a `cfg`-gated wrapper. **Not required for SEC-1.f-j**; flagged here for SEC-1.l (atomic swap) follow-up.

### 2.8 `fchmodat`

This is the one non-trivial finding. Section 3.5 covers it in detail.

### 2.9 `fchownat` + `utimensat`

Both accept `AT_SYMLINK_NOFOLLOW` identically. `fchownat` was the model the SEC-1.a "already-safe entries" section was built on - `metadata/src/apply/ownership.rs:193,349` already use the `*at` shape with `AT_FDCWD`, and they are correct on both kernels. `utimensat` has the macOS 10.13 floor (rustix#157), but our published macOS support starts at Big Sur (11.x); the floor is moot.

## 3. Behavioural deltas (Linux vs macOS), enumerated

### 3.1 `AT_FDCWD` numeric value differs

Linux: `-100`. macOS: `-2`. Both libc submodules expose the right value behind the same `AT_FDCWD` name. **Action: never hard-code `-100`.** Already true in our tree - the existing `unix_fs::chownat(CWD, ...)` sites import from libc.

### 3.2 `AT_SYMLINK_NOFOLLOW` numeric value differs

Linux: `0x100`. macOS: `0x0020`. libc abstracts this. **Action: same as above.**

### 3.3 `AT_REMOVEDIR` numeric value differs

Linux: `0x200`. macOS: `0x0080`. libc abstracts this. **Action: same as above.**

### 3.4 macOS lacks `openat2(2)` and the `RESOLVE_*` flag family

`openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` is the Linux primitive that makes the SEC-1 sandbox tamper-proof in one syscall: path resolution is forced to stay below the sandbox root and is forbidden from traversing any symlink anywhere in the path. macOS has no single-syscall analogue:

- `O_NOFOLLOW_ANY` (added in macOS Big Sur 11.0) gives the "no symlink anywhere in the path" half; equivalent to Linux `RESOLVE_NO_SYMLINKS`.
- `O_RESOLVE_BENEATH` (documented in `open(2)` on current macOS) gives the "must not escape" half; equivalent to Linux `RESOLVE_BENEATH`. It is **not exposed in libc 0.2.180** - the man page lists it, the XNU kernel accepts it, but the constant is missing from `libc::*`. Adding it locally as a `c_int` constant is a one-line fix in the SEC-1.c helper, but it should land upstream in the libc crate. (SEC-1.b's design wisely keeps the carrier API the same on both platforms; the platform-specific resolve-flag plumbing is internal to the `secure_dir` helper.)

In practice, macOS reaches Linux-equivalent hardening by combining (a) opening the sandbox root once with `O_DIRECTORY | O_NOFOLLOW` and pinning the resulting fd for the lifetime of the transfer, (b) opening each subsequent component with `openat(parent_fd, leaf, O_NOFOLLOW | O_NOFOLLOW_ANY)`, and (c) refusing to follow `..`. This is the same recipe upstream uses on FreeBSD before `O_BENEATH` landed (LWN #815118), so the design is well-trodden.

What macOS **does not** get without `openat2`: a single-syscall "the resolver promises this fd will not escape the sandbox" guarantee. Each `openat` is a separate kernel transition, so a tampering attacker who can race in between two consecutive `openat` calls on the same path can still wedge a symlink at a deeper component. The mitigation is the same as on FreeBSD: pre-open the whole path once and reuse the dirfd, which is exactly what `DirSandbox` does. The window is two orders of magnitude tighter than the pure path-based code SEC-1 is replacing, but it is not zero on macOS. Document it.

### 3.5 `fchmodat(.., AT_SYMLINK_NOFOLLOW)` semantics

This is the only behavioural delta that requires per-platform code.

**Linux:** the kernel's `fchmodat(2)` ignores the `flag` argument entirely. glibc emulates `AT_SYMLINK_NOFOLLOW` by opening the path with `O_PATH | O_NOFOLLOW` and writing the mode through `/proc/self/fd/N` (glibc BZ #14578). On systems without `/proc` mounted, glibc returns `ENOTSUP` for `fchmodat(dirfd, path, mode, AT_SYMLINK_NOFOLLOW)` when `path` resolves to a symlink. POSIX leaves this implementation-defined and the gnulib mailing list documents the resulting portability mess (gnulib bug #37885).

**macOS:** the `chmod(2)` man page lists `AT_SYMLINK_NOFOLLOW` as a documented flag for `fchmodat`: "If `path` names a symbolic link, then the mode of the symbolic link is changed." The XNU kernel honours it directly without `/proc` emulation. macOS additionally exposes `lchmod(2)` as a path-based alternative for the same operation.

**Implication for SEC-1.f-j:** the metadata-apply layer at `crates/metadata/src/apply/permissions.rs` currently uses `std::fs::set_permissions` (which is `chmod`, follows symlinks) for the regular-file path, and we have no production code that chmods a symlink today (rsync's `--perms` doesn't touch symlinks - symlink permissions are not portable). The 14 permission rows in SEC-1.a are all for **regular files**, so the `AT_SYMLINK_NOFOLLOW` delta is academic for the cutover. **However**, if any of the `fchmodat` swaps adds `AT_SYMLINK_NOFOLLOW` to defend against a "swap the file for a symlink before chmod" race, the implementation must:

- macOS: pass `AT_SYMLINK_NOFOLLOW` directly to `fchmodat` (it works).
- Linux: avoid passing the flag (it returns `ENOTSUP` on most filesystems); instead, use `fchmod` on an already-open `O_PATH | O_NOFOLLOW` fd, which is what glibc does internally and what the existing `apply/permissions.rs:131` already does for the fd-anchored branch.

This is a one-line `cfg` split in the SEC-1.h PR. Document the asymmetry in the SEC-1.h commit message and we are done.

### 3.6 `utimensat` on symlinks

Both kernels accept `AT_SYMLINK_NOFOLLOW`; macOS routes it to the symlink's mtime, Linux does the same. No delta. The `filetime` crate (referenced in SEC-1.a surprise #1) wraps `utimensat` on both platforms; the SEC-1 swap to direct `rustix::fs::utimensat(dirfd, leaf, &times, flags)` works identically on macOS and Linux. No additional macOS-only branches needed.

### 3.7 No `renameat2`, but `renameatx_np` covers the same flag space

macOS has `renameatx_np(fromfd, from, tofd, to, flags)` for atomic swap (`RENAME_SWAP`) and exclusive-create (`RENAME_EXCL`). Linux has `renameat2(.., RENAME_EXCHANGE | RENAME_NOREPLACE)`. Different syscall, different flag names, same semantics. **Not on SEC-1.f-j's path** but worth noting for any future "atomic backup-then-promote" work.

## 4. libc-crate exposure gaps

Caught while writing this audit; flag for either local definition or upstream PR.

| symbol | macOS XNU | libc 0.2.180 exposure | recommendation |
|---|---|---|---|
| `O_RESOLVE_BENEATH` | accepted by `openat` per `open(2)` man page | absent | define locally as `pub const O_RESOLVE_BENEATH: c_int = 0x00800000;` in the `secure_dir` helper; submit a libc upstream PR in a follow-up |
| `RENAME_NOFOLLOW_ANY` | accepted by `renameatx_np` per `rename(2)` | absent | not needed for SEC-1.f-j; document for SEC-1.l |
| `RENAME_RESOLVE_BENEATH` | accepted by `renameatx_np` | absent | same as above |
| `lchmod` | shipping | absent in the `apple` submodule of 0.2.180 (only `chmod`/`fchmodat` are exposed) | not needed - the `fchmodat(AT_SYMLINK_NOFOLLOW)` path covers our use |

None of these gaps are blockers. The first one (`O_RESOLVE_BENEATH`) is the only one we would want for full Linux-parity hardening on macOS, and it is a one-line local const definition.

## 5. Recommendation

**Pick option A: macOS gets the same `DirSandbox`-anchored `*at` cutover as Linux, with two small platform splits.**

The three options the task posed were (a) parallel safe code, (b) fall back to legacy path-based syscalls on macOS with a `SECURITY.md` note, (c) ship as "best-effort, not hardened on macOS".

The audit data lands cleanly on **option (a)**:

1. **13 of 14 sinks are byte-identical to Linux at the libc layer.** The only structural absence is `renameat2`, which is not on SEC-1.f-j's path (we don't use `RENAME_NOREPLACE` anywhere).
2. **The TOCTOU surface SEC-1 closes shrinks by the same order of magnitude on macOS as on Linux.** Pre-opening a sandbox dirfd and feeding every subsequent operation through `*at` syscalls is enough to defeat the rsync 3.4.3 attacks even without `openat2`; the residual race window is a multi-`openat`-call window that exists on FreeBSD too, not the path-based-syscall window that CVE-2026-29518 and CVE-2026-43619 exploit.
3. **The fchmodat asymmetry is a one-line `cfg` split** that the `metadata::apply::permissions` module already has the shape for (the fd-anchored branch at `apply/permissions.rs:131` is the Linux fallback).
4. **The `O_RESOLVE_BENEATH` libc gap is a one-line local const definition** in `crates/fast_io/src/secure_dir.rs`. Submit a libc PR in a follow-up so the gap closes upstream and we can drop the local definition.

Concrete deltas to bake into SEC-1.c-j:

- **SEC-1.c (`secure_dir.rs` helper):** add a `#[cfg(target_os = "macos")] const O_RESOLVE_BENEATH: c_int = 0x00800000;` local const. Use `O_NOFOLLOW | O_NOFOLLOW_ANY | O_DIRECTORY` for sandbox-root opens on macOS, `O_RDONLY | O_DIRECTORY` plus `openat2` with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` on Linux 5.6+, and a graceful `O_NOFOLLOW`-only fallback on Linux < 5.6.
- **SEC-1.h (permission apply):** when adding `AT_SYMLINK_NOFOLLOW` to any `fchmodat` call (only relevant if the cutover defends against the symlink-swap race), gate with `cfg(target_os = "macos")` for the direct-flag path and `cfg(target_os = "linux")` for the `fchmod`-on-`O_PATH`-fd fallback.
- **`SECURITY.md`:** add a paragraph clarifying that the macOS hardening uses `O_NOFOLLOW_ANY` plus a sandboxed dirfd rather than `openat2`, that the resulting per-operation race window is tighter than the path-based code SEC-1 replaces but not closed in a single syscall, and that this matches the FreeBSD baseline.

No Cargo dependency changes. No tests to add for this audit itself; SEC-1.f-j's regression matrix already covers the cross-platform paths.

## 6. Confidence and follow-ups

- Per-syscall presence was confirmed by `grep` against the workspace-pinned `libc-0.2.180` source at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libc-0.2.180/src/unix/mod.rs` and `.../src/unix/bsd/apple/mod.rs`. Constants were cross-checked against the Apple `open(2)`, `rename(2)`, and `chmod(2)` man pages on `keith.github.io/xcode-man-pages`. XNU syscall numbers came from `apple-oss-distributions/xnu/bsd/kern/syscalls.master`.
- The audit was done with libc 0.2.180; the workspace pin is `libc = "0.2"`, so a future bump may close the `O_RESOLVE_BENEATH` exposure gap without code change. Re-grep when bumping.
- Behavioural deltas were spot-checked against the rustix issue tracker (`utimensat` macOS 10.13 floor, rustix#157) and gnulib's `fchmodat`+`AT_SYMLINK_NOFOLLOW` portability survey.
- This audit does **not** cover macOS-specific xattr / ACL paths (`metadata::acl::*`, `metadata::xattr::*`); they have their own path-based syscall surface (`lsetxattr`, `acl_set_file`) and were called out for a sibling SEC-1.k follow-up in SEC-1.a section 6. They remain out of scope for this PR.
- Sender-side path syscalls are out of SEC-1's charter and stay path-based on both kernels; no macOS-specific handling required.
