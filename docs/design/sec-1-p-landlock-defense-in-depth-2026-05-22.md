# SEC-1.p - Landlock LSM as defense-in-depth for the daemon receiver path access

**Date:** 2026-05-22
**Scope:** evaluate whether to add Linux Landlock LSM (via the `landlock` Rust crate) as a kernel-enforced allowlist layered *above* the SEC-1 `*at` syscall chain. Audit + design only - no Rust changes in this PR.
**Status:** PROPOSED - pending decision to implement.
**Author crate target:** `crates/fast_io/src/landlock.rs` (new), plus a single call from `crates/daemon/src/daemon/sections/module_access/transfer.rs`.
**Inputs:**
- `docs/design/sec-1-b-dirfd-carrier.md` (carrier design that grounded the `*at` rewrite).
- `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` (107 path syscalls / 36 files).
- `docs/audits/sec-1-k-macos-at-syscalls-2026-05-21.md` (macOS coverage).
- `docs/audits/sec-1-l-windows-ntfs-handle-audit-2026-05-21.md` (Windows out-of-scope rationale).
- Crate readme: https://github.com/landlock-lsm/rust-landlock

## 1. Threat model recap

The SEC-1 series mitigates CVE-2026-29518 (TOCTOU symlink race in the daemon receiver with `use chroot = no`) and CVE-2026-43619 (the broader chmod / lchown / utimes / rename / unlink / mkdir / symlink family of symlink swap races). Today's posture, with .f / .g / .h / .i / .j / .m / .n shipped:

- A receiver-side `DirSandbox` carrier holds an `O_DIRECTORY | O_NOFOLLOW` root dirfd plus a depth-first dirfd stack.
- Every receiver-side mutation has an `*at` helper anchored on the parent dirfd, gated by `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` when the runtime probe (`openat2_supported()`) reports the kernel exposes the call. The full helper set lives in `crates/fast_io/src/dir_sandbox/`:
  - `at_syscalls.rs` - `fstatat_nofollow`, `unlinkat`, `mkdirat`, `symlinkat`, `linkat` (plus `_via_sandbox_or_fallback` wrappers).
  - `at_syscalls_metadata.rs` - `fchmodat`, `fchownat`, `utimensat` (plus `_via_sandbox_or_fallback`).
  - `at_syscalls_rename.rs` - `renameat` (plus `_via_sandbox_or_fallback`).
- macOS audit (SEC-1.k) confirmed the BSD `*at` family behaves identically; Windows audit (SEC-1.l) confirmed NTFS handle-based APIs structurally sidestep the path TOCTOU window. SEC-1 is therefore a Linux-and-friends story; Landlock would tighten only Linux further.

What the chain still does **not** prevent:

1. **Missed call sites.** Future receiver-adjacent code (a new metadata applier, a new hardlink path, a new test harness, a refactor of `xattrs` or `acls`) that calls `std::fs::*` or `libc::*at` directly with an attacker-controlled path - bypassing `DirSandbox`. There is no compile-time barrier preventing this. The audit at `docs/audits/sec-1-a-path-syscall-surface-2026-05-20.md` listed 107 call sites; verifying *no future commit* re-introduces a 108th relies on review discipline.
2. **Normalization bugs in `single_component_leaf`.** The carrier's leaf-extraction logic assumes well-formed single components. A bug that lets a `../` slip through, or a Unicode-normalization corner case, defeats the per-call helper guarantee even though every site routes through the sandbox.
3. **Out-of-tree operand expansion.** `--backup-dir`, `--temp-dir`, `--partial-dir`, `--link-dest`, `--copy-dest`, `--compare-dest` introduce additional roots. The dirfd-side-cache currently scopes these correctly, but a regression that registered a new operand without going through the cache would write outside the intended tree.
4. **External code paths.** A name converter or a pre/post-xfer-exec hook spawned by the daemon inherits the same uid/gid and could write anywhere the unix user can reach (modulo chroot if configured).

Landlock does not fix (4) on its own (the hook is its own process); it would inherit the parent's ruleset after `restrict_self()`, which actually makes (4) safer too.

## 2. What Landlock adds

Landlock is a Linux LSM that lets an unprivileged process enrol itself into a per-thread "no access except via this allowlist" sandbox. After `restrict_self()` succeeds, **every** filesystem syscall that resolves a path outside the allowlist returns `EACCES` from the kernel, regardless of which syscall the userspace code chose. It is purely additive: a `PathBeneath` rule grants access to a subtree, never broadens it; the union of an empty ruleset and a `restrict_self()` denies everything except already-open file descriptors.

For SEC-1.p this is **defense in depth**, not a replacement:

- The `*at` helpers remain the first line of defense - they close the TOCTOU window precisely and keep behaviour identical when Landlock is unavailable.
- Landlock is the second line - even if a future commit calls `std::fs::remove_file(attacker_path)` directly, the kernel rejects it because `attacker_path` is not under the allowed roots.
- The check happens in the kernel, after path resolution, so neither symlink trickery nor `..` sequences nor bind mounts can route a write outside the rule set.

The combination matches the layered model upstream OpenSSH and systemd use today (seccomp + landlock) and the layered model Chrome's renderer uses (namespaces + seccomp + landlock).

## 3. API sketch

The helper lives in `crates/fast_io/` because that is the existing home for platform syscall wrappers, owns the `dir_sandbox` carrier the helper layers on top of, and is already the crate gated for `iouring-send-zc` style Linux-only opt-in features. No new crate is required.

```rust
// crates/fast_io/src/landlock.rs
// Linux + feature = "landlock" only; everywhere else this resolves to a no-op
// returning Ok(LandlockOutcome::Unavailable).

use std::io;
use std::path::Path;

/// Outcome of a `restrict_to_paths` call, for caller logging.
pub enum LandlockOutcome {
    /// Ruleset created and `restrict_self()` succeeded at the best ABI the
    /// kernel and crate jointly understood. The carried `u8` is the ABI level
    /// actually used (1, 2, or 3 for SEC-1.p; higher levels do not change
    /// behaviour because we do not request network or ioctl rights).
    Enforced(u8),
    /// Kernel does not expose Landlock at all (pre-5.13, or LSM disabled).
    /// SEC-1 `*at` helpers remain the only defense.
    Unavailable,
    /// Kernel exposes Landlock but `restrict_self()` failed. Caller should
    /// treat this as an unrecoverable security regression for the connection.
    Error(io::Error),
}

/// Restrict the current thread to read+write access only under `allowed_roots`.
///
/// Call exactly once per daemon connection, after the module is resolved and
/// any chroot/uid drop has happened, before any user-controlled file
/// operation begins. All roots must be absolute and must already exist; the
/// helper does *not* create them. Best-effort downgrade per
/// `landlock::ABI::set_best_effort(true)` is enabled, so a kernel that
/// understands v1 but not v3 will accept the v1 subset and silently drop
/// the TRUNCATE / REFER bits.
///
/// Returns `LandlockOutcome::Unavailable` (not an error) when the kernel
/// lacks Landlock, so the daemon keeps running with SEC-1 `*at` helpers as
/// the sole defense. Returns `LandlockOutcome::Error` only when the kernel
/// said yes to ruleset creation but no to `restrict_self()`; the caller must
/// abort the connection because the intended sandbox did not engage.
#[cfg(all(target_os = "linux", feature = "landlock"))]
pub fn restrict_to_paths(allowed_roots: &[&Path]) -> LandlockOutcome {
    use landlock::{ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetStatus};

    // Target ABI v3 (Linux 6.2+): READ + WRITE + CREATE + DELETE + RENAME +
    // SYMLINK + REFER + TRUNCATE. set_best_effort(true) lets the runtime
    // strip what older kernels do not understand.
    let abi = ABI::V3;
    let access = AccessFs::from_all(abi);

    let ruleset = match Ruleset::default()
        .set_best_effort(true)
        .handle_access(access)
        .and_then(|b| b.create())
    {
        Ok(b) => b,
        Err(e) if is_unsupported(&e) => return LandlockOutcome::Unavailable,
        Err(e) => return LandlockOutcome::Error(io_err(e)),
    };

    let mut ruleset = ruleset;
    for root in allowed_roots {
        let fd = match PathFd::new(root) {
            Ok(fd) => fd,
            Err(e) => return LandlockOutcome::Error(io_err(e)),
        };
        ruleset = match ruleset.add_rule(PathBeneath::new(fd, access)) {
            Ok(r) => r,
            Err(e) => return LandlockOutcome::Error(io_err(e)),
        };
    }

    match ruleset.restrict_self() {
        Ok(status) => LandlockOutcome::Enforced(status.ruleset.abi() as u8),
        Err(e) => LandlockOutcome::Error(io_err(e)),
    }
}

#[cfg(not(all(target_os = "linux", feature = "landlock")))]
pub fn restrict_to_paths(_allowed_roots: &[&Path]) -> LandlockOutcome {
    LandlockOutcome::Unavailable
}
```

The `is_unsupported` and `io_err` shims map `landlock::*Error` to `std::io::Error` so the public API does not leak the crate type. The exact ABI level returned in `Enforced(_)` comes from `RulesetStatus::ruleset.abi()` after best-effort downgrade; this is what the daemon log will record so operators can confirm v3 is actually in effect on production kernels.

## 4. Daemon integration point

The single call site is `crates/daemon/src/daemon/sections/module_access/transfer.rs:352`, immediately after `apply_module_privilege_restrictions(...)` returns `Ok` (line 352 for the `log_sink`-bearing branch, line 359 for the fallback-sink branch). Both branches reach this point with the chroot applied (if `use_chroot`) and uid/gid dropped (if configured). That is exactly the moment to engage Landlock:

```text
auth complete -> validate_module_path -> apply_module_privilege_restrictions
                                       -> [SEC-1.p insertion: restrict_to_paths]
                                       -> name converter spawn (inherits ruleset)
                                       -> transfer
```

The set of paths passed to `restrict_to_paths` is the union of:

1. `module.path` - mandatory, always present (`crates/daemon/src/rsyncd_config/sections.rs:142`).
2. The module lock file directory, if configured (`module.lock_file` - the daemon needs to update the file during the connection).
3. Any auxiliary log paths the daemon writes per-connection (transfer log, xfer log).

The current `ModuleConfig` (`sections.rs:140`-`176`) does **not** expose `temp_dir`, `partial_dir`, or `backup_dir` as per-module fields - those are client options requested via the wire-protocol args. If the audit's recommendation goes forward, the implementation PR must intercept the parsed client args at the same point in the session where `validate_module_path` runs (`module_access/transfer.rs:342`), expand any client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir` paths, and either:

- (a) reject the connection if the client requested a path outside `module.path` (matching `use_chroot=true` behaviour), or
- (b) widen the Landlock allowlist to include the requested path before calling `restrict_self()`.

Option (a) is the safer default and matches upstream's behaviour under chroot. Option (b) would be required only if oc-rsync intends to support cross-tree client operands without chroot, which is the riskier configuration today.

After `restrict_self()` engages, the thread (and any child process spawned afterwards, per Landlock inheritance semantics) cannot reach paths outside the allowlist regardless of which syscall it tries. The name converter spawned at `module_access/transfer.rs:368-379` will inherit the ruleset, which is the desired outcome - a malicious converter cannot escape the module tree either.

## 5. Kernel version matrix

| Kernel       | Landlock ABI | Features available                            | SEC-1.p outcome                                                                                       |
|--------------|--------------|-----------------------------------------------|--------------------------------------------------------------------------------------------------------|
| < 5.13       | n/a          | none                                          | `Unavailable`; SEC-1 `*at` helpers are the only defense. Log: `landlock unavailable; kernel < 5.13`.   |
| 5.13 - 5.18  | v1           | READ / WRITE / CREATE / DELETE / SYMLINK      | `Enforced(1)`. Receiver works. Cross-module `rename` fails. In-place `truncate` works (no v3 needed). |
| 5.19 - 6.1   | v2           | v1 + REFER (cross-hierarchy rename)           | `Enforced(2)`. `--backup-dir` across trees works. `truncate` still relies on caller having WRITE.     |
| 6.2 - 6.6    | v3           | v2 + TRUNCATE                                 | `Enforced(3)`. Full receiver semantics, including explicit `O_TRUNC` opens on existing files.         |
| >= 6.7       | v4 or v5     | v3 + network (v4) + ioctl (v5)                | `Enforced(3)`. SEC-1.p does not request v4/v5 rights; extra ABI bits are inert here.                  |

Best-effort downgrade (`ABI::set_best_effort(true)`) lets the helper request `AccessFs::from_all(ABI::V3)` unconditionally; the crate strips REFER on v1 and TRUNCATE on v1/v2, so the same call site works on every supported kernel. The downgrade is logged at WARN so operators see the actual enforcement level in their daemon log:

- v3 - full enforcement; matches the design intent.
- v2 - `O_TRUNC` writes succeed even on files outside the allowlist *if* the file is already open. The receiver does not do this against attacker-controlled fds, so the residual risk is low.
- v1 - additionally, cross-hierarchy `rename` is blocked. The receiver's in-tree renames stay within a single subtree and are unaffected; cross-tree `--backup-dir` against an out-of-module target would fail at the syscall boundary. Document this as a known limitation of running on 5.13 - 5.18.

GitHub Actions Ubuntu 22.04 runners ship 5.15 (v1), Ubuntu 24.04 runners ship 6.8 (v3). CI must exercise both - the v1 path is the one most likely to have ABI-related surprises.

## 6. Best-effort downgrade rationale

`set_best_effort(true)` is the only sane default for a userspace daemon that ships as a single binary across distros. The alternative (`false`) requires the helper to enumerate the kernel ABI at runtime and request exactly the supported subset; the crate already does this internally when best-effort is on, and the resulting `RulesetStatus` exposes the level actually achieved.

Security implication of each downgrade:

- **v3 -> v2** loses TRUNCATE. An attacker who can place a symlink already pointing inside the module tree (which SEC-1 `*at` helpers already block from being followed) and somehow open it could `O_TRUNC` zero it. With SEC-1's `RESOLVE_NO_SYMLINKS`, this preconditions are unreachable.
- **v2 -> v1** additionally loses REFER. A receiver that wanted to rename a tempfile from `/tmp/oc-rsync/` to the module tree would fail at `renameat`. The daemon does not do this today (temp files are written under the module tree), so impact is zero.

In both downgrade cases, SEC-1 `*at` helpers continue to provide the primary defense; Landlock at the lower ABI provides strictly less than at v3 but strictly more than nothing.

## 7. Feature gating

Match the precedent established by `iouring-send-zc` (`crates/fast_io/Cargo.toml:110-117`):

```toml
# fast_io/Cargo.toml additions

[target.'cfg(target_os = "linux")'.dependencies]
landlock = { version = "0.4", optional = true }

[features]
# default on Linux: landlock is cheap, security-positive, and the runtime probe
# means a kernel that does not expose it gracefully degrades to SEC-1 *at* only.
# Off everywhere else (the cfg gate already takes care of that).
default = ["io_uring", "iocp", "landlock"]
landlock = ["dep:landlock"]
```

(If `default = ["io_uring", "iocp"]` cannot accept a Linux-only addition without a feature-resolver workaround, fall back to `default = ["io_uring", "iocp"]` plus a Cargo target-specific `default-features` override in the workspace `Cargo.toml`. The implementation PR confirms the exact wording against `cargo build --workspace` on each platform.)

Runtime gating uses `landlock::ABI::query()` (or equivalent crate-level probe) inside `restrict_to_paths`; on a kernel without Landlock the function returns `LandlockOutcome::Unavailable` and the daemon logs a single WARN at startup: `landlock unavailable on this kernel; relying on SEC-1 *at* helpers`. No per-connection log spam.

## 8. Test plan

Three integration tests under `crates/daemon/tests/landlock_sandbox.rs` (new), gated `#[cfg(all(target_os = "linux", feature = "landlock"))]` and skipped at runtime when `LandlockOutcome::Unavailable` is returned (so CI on a pre-5.13 kernel does not fail):

1. **`landlock_blocks_write_outside_module_root`** - start a daemon with a single module pointing at `tmp/mod_a/`, call `restrict_to_paths(&[tmp.path()])`, then attempt `std::fs::write("/tmp/outside.txt", b"x")` from the same thread. Assert `ErrorKind::PermissionDenied` (i.e. `EACCES`).
2. **`landlock_allows_write_inside_module_root`** - same setup, attempt `std::fs::write(tmp.path().join("inside.txt"), b"x")`. Assert success and content readback. Confirms the rule grants the rights we documented (`AccessFs::from_all(ABI::V3)` covers create + write).
3. **`landlock_unavailable_logs_warning_and_continues`** - mock the helper to force `LandlockOutcome::Unavailable`, assert the daemon completes a small transfer end-to-end, and assert the WARN was emitted exactly once. This is the regression test for the "kernel does not support Landlock" branch on CI runners that *do* support it.

Plus a unit test in `crates/fast_io/src/landlock.rs::tests` that asserts the ABI returned in `Enforced(_)` matches what `landlock::ABI::query()` reports on the current kernel.

## 9. Tradeoffs

- **Linux-only.** Same gating pattern as `iouring-send-zc`. macOS daemon path keeps SEC-1 `*at` only (SEC-1.k confirmed this is sufficient). Windows daemon path is structurally immune (SEC-1.l).
- **Adds a dep and startup surface.** `landlock` v0.4 is the current crate version; it has `#![forbid(unsafe_code)]` and is maintained by the upstream Landlock LSM project (https://github.com/landlock-lsm/rust-landlock). The implementation PR must confirm via `cargo deny check` and `cargo audit` before merge.
- **Requires accurate enumeration of writable prefixes.** Missing a prefix breaks the daemon (legitimate writes get `EACCES`), not the attacker. The integration point in section 4 lists the exact set; any future feature that needs a new writable prefix must update the call site or fall back to a path inside `module.path`.
- **Inherited by children.** Name converter, pre/post-xfer-exec hooks all run under the same ruleset. This is a security win but might surprise an operator who expected a hook to touch `/etc/foo`. Document in `oc-rsyncd.conf` man page that hooks run under Landlock when SEC-1.p is enabled and that paths outside the module tree are unavailable.
- **Per-connection cost.** A single `restrict_self()` call per connection; sub-millisecond on every kernel measured by upstream. Negligible vs. the rest of the connection setup.

## 10. Decision criteria for proceeding

The implementation PR is justified when **all three** of the following hold:

1. **Crate quality bar met.** `landlock` v0.4 (or current stable) is actively maintained (last release < 12 months), enforces `forbid(unsafe_code)`, has no open critical advisories on https://rustsec.org/, and is in production use (`cargo info landlock` reverse-deps include systemd-resolved-equivalents or Firefox's sandbox; GitHub stars > 100 as a coarse signal of community vetting).
2. **Path set is complete.** Audit confirms the union {`module.path`, optional `module.lock_file` dir, optional log paths} is the **only** writable set the daemon connection touches. Either:
   - intercept client args (`--temp-dir`, `--partial-dir`, `--backup-dir`) at `module_access/transfer.rs:342` and reject any out-of-module path (option (a) in section 4), or
   - widen the allowlist for those args before `restrict_self()` (option (b)).

   Option (a) is the recommended default.
3. **CI exercises both ABI tiers.** GitHub Actions Ubuntu 22.04 (5.15, ABI v1) and Ubuntu 24.04 (6.8, ABI v3) jobs both run the three integration tests. The v1 job confirms best-effort downgrade does not error; the v3 job confirms full enforcement.

## 11. Re-open trigger if N/A

If the audit reveals the daemon needs path access outside what Landlock can express, document and either widen the allowlist or skip SEC-1.p:

- **Per-connection ephemeral paths created after `restrict_self()`.** Landlock allows creating files under an allowed root (`AccessFs::MakeReg` etc. are part of `from_all`), but it does **not** allow creating a new root directory at an unanticipated location. If a feature spawns a worker that needs to write under `/var/run/oc-rsync/<pid>/` and that directory is not in the rule set, the worker will get `EACCES`. Mitigation: pre-create the per-connection directory under a known parent and include the parent in the rule set.
- **Bind mounts.** Landlock follows the kernel's path resolution, so a bind mount of `/etc/passwd` under the module tree is reachable through the allowed root. SEC-1 `*at` helpers handle this correctly (the mount is *inside* the sandbox by construction); Landlock matches the same behaviour. Not a regression, document for completeness.
- **`/proc` and `/sys` reads.** Landlock blocks paths outside the rule set including `/proc/self/`. The daemon currently reads `/proc/self/stat` and similar for telemetry. If those paths are not in the allowlist, telemetry breaks. Resolution: open the relevant fds *before* `restrict_self()` and reuse them for the lifetime of the connection. The fast_io `iocp` and `io_uring` modules already follow this pattern.

If any of the above cannot be resolved without widening the rule set to include sensitive paths (e.g. `/etc`), close SEC-1.p with NEEDS-NEW-DATA and document the blocker.

## 12. Implementation effort estimate

Single PR, conventional prefix `feat:`:

- `crates/fast_io/src/landlock.rs` - new file, ~120 LoC (helper + LandlockOutcome enum + tests module).
- `crates/fast_io/src/lib.rs` - one `pub mod landlock;` line plus a re-export.
- `crates/fast_io/Cargo.toml` - one optional dep stanza, one feature stanza, default-features adjustment. ~6 lines.
- `crates/daemon/Cargo.toml` - depend on `fast_io` with the `landlock` feature on Linux. ~2 lines.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs` - ~30 LoC: build the allowed-roots vec, call `restrict_to_paths`, branch on the outcome, log, abort connection on `Error`.
- `crates/daemon/tests/landlock_sandbox.rs` - new file, ~150 LoC for the three integration tests.
- `SECURITY.md` - bump the SEC-1 status table to add SEC-1.p row.
- `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` - mark **Status: ACCEPTED** and link the implementation PR.

Total: ~310 LoC across 7 files. Estimated single PR, no decomposition needed. Suitable for one reviewer-day of review effort.

## 13. Recommendation

**PROCEED.** The Linux-only gating precedent (`iouring-send-zc`) is in place, the integration point is a single line in `transfer.rs:352`, the crate is unsafe-free and small, and the kernel-version matrix maps cleanly onto best-effort downgrade with no per-connection runtime cost worth measuring. Landlock complements SEC-1's `*at` chain by closing the "future regression / missed call site" gap that pure call-site routing cannot guarantee on its own.
