# Audit: daemon chroot + uid/gid drop sequence

Closes #2129.

## Scope

Verify that oc-rsync's per-module privilege reduction matches upstream rsync 3.4.1
ordering (chroot before group/user drop), drops supplementary groups, and assess
which Linux hardening primitives (capability bounding, `PR_SET_NO_NEW_PRIVS`,
seccomp, privilege-separation worker) are missing.

Upstream reference (`target/interop/upstream-src/rsync-3.4.1/`):

- `clientserver.c:704` -- `use_chroot = lp_use_chroot(i)`.
- `clientserver.c:829-842` -- chroot-test fallback when `use chroot` is unset.
- `clientserver.c:978-985` -- per-module `chroot(module_chdir)`.
- `clientserver.c:1006-1030` -- `setgid(gid_array[0])` then `setgroups(gid_list)`.
- `clientserver.c:1032-1045` -- `setuid(uid)` (+ `seteuid` when available); recomputes `am_root`.
- `clientserver.c:1301-1312` -- daemon-wide `chroot(lp_daemon_chroot())` before accept loop.
- `clientserver.c:1313-1339` -- daemon-wide `setgid`/`setuid` from `lp_daemon_gid`/`lp_daemon_uid`.

Upstream sequence per connection:
`load module` -> `chdir(module_chdir)` -> (if `use_chroot`) `chroot(module_chdir)` ->
`setgid(primary_gid)` -> `setgroups(gid_list)` -> `setuid(uid)`.

## Current oc-rsync handling

- Module flags parsed: `crates/daemon/src/daemon/config_parsing/global_directives.rs:387` (`use chroot`),
  `:710` (`daemon chroot`), `crates/daemon/src/daemon/config_parsing/module_directives.rs:113` (`use chroot`),
  `:131` (`numeric ids`); per-module `uid`/`gid` resolved into `ModuleDefinition.uid`/`gid`.
- Per-module privilege application: `crates/daemon/src/daemon/sections/privilege.rs:7-77`
  (`apply_chroot`, `drop_privileges`, `apply_module_privilege_restrictions`).
- Call site: `crates/daemon/src/daemon/sections/module_access/transfer.rs:346-364`
  invokes `apply_module_privilege_restrictions(module, log_sink)` before `build_server_config`.
- Platform implementation: `crates/platform/src/privilege.rs:27-68` -- `nix::unistd::chroot`,
  `setgid`, `setuid`, with `setgroups([gid])` (Linux via `nix`, macOS via `libc::setgroups`).
- Daemon-startup uid/gid drop: `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs:229-246`
  calls `drop_privileges(daemon_uid, daemon_gid, sink)` after bind/daemonize/PID-file.

## Findings

| # | Severity | Area | Title |
|---|----------|------|-------|
| F1 | HIGH   | daemon | `daemon chroot` directive is parsed but never applied |
| F2 | MEDIUM | platform | Supplementary-group drop only reaches `setgroups([gid])`, not `setgroups([])` when `gid` is unset |
| F3 | MEDIUM | daemon | Module `apply_chroot` skips upstream's chroot-test fallback when `use chroot` is unset |
| F4 | MEDIUM | platform | `setuid` is not paired with `seteuid` on platforms that expose it |
| F5 | LOW    | daemon | No Linux capability bounding-set drop after `setuid` (CAP_SYS_CHROOT, CAP_SETUID, ...) |
| F6 | LOW    | daemon | `PR_SET_NO_NEW_PRIVS` / `SECBIT_NOROOT` not asserted before privilege drop |
| F7 | LOW    | daemon | No seccomp-bpf filter narrowing the post-drop syscall surface |
| F8 | LOW    | daemon | No privilege-separation worker; transfer runs in the same process that read auth secrets |

### F1 -- daemon chroot is parsed but never applied
`RuntimeOptions::daemon_chroot()` exists (`runtime_options/accessors.rs:46`),
but `crates/daemon/src/daemon/sections/server_runtime/accept_loop.rs` never
calls `platform::privilege::apply_chroot(...)` for the daemon-wide path.
Upstream chroots once at startup before the accept loop (`clientserver.c:1301`).
Remediation: call `apply_chroot(daemon_chroot_path)` plus `chdir("/")` between
`load_config` and the `drop_privileges(daemon_uid, daemon_gid, ...)` block.

### F2 -- supplementary groups not always cleared
`platform::privilege::drop_privileges` only invokes `set_supplementary_groups`
when `gid.is_some()` (`crates/platform/src/privilege.rs:55-60`). If a module
sets `uid = nobody` but no `gid`, the worker keeps root's supplementary groups.
Remediation: when running as root and either `uid` or `gid` is set, call
`setgroups(&[primary_gid])` (or `setgroups(&[])` when no primary gid is supplied)
before `setgid` / `setuid`, mirroring `clientserver.c:1013-1020`.

### F3 -- chroot-test fallback missing
Upstream tries `chroot("/")` when `use chroot` is unset to detect insufficient
privileges, then downgrades to `use_chroot = 0` (`clientserver.c:832-836`).
oc-rsync only honours an explicitly configured boolean; an unprivileged daemon
fails the entire connection instead of falling back. Remediation: when
`use_chroot` is `None` in `module_definition::finish.rs:27`, probe
`chroot("/")` once at startup (or first connection) and cache the result.

### F4 -- `setuid` without paired `seteuid`
`crates/platform/src/privilege.rs:62-65` calls only `nix::unistd::setuid`.
Upstream additionally calls `seteuid(uid)` on platforms that expose it
(`clientserver.c:1033-1037` `#ifdef HAVE_SETEUID`). On Linux this is a no-op
because `setuid` already resets euid, but BSD-derived hosts can leave a
saved-set-uid that lets a compromised worker `seteuid(0)` back. Remediation:
follow `setuid` with `seteuid` on `target_os = "freebsd" | "netbsd" | "openbsd" | "dragonfly"`.

### F5 -- no capability bounding-set drop (Linux)
After `setuid(non_root)`, the worker still keeps capabilities granted via file
caps or ambient sets (e.g. running under `systemd` with `AmbientCapabilities=`).
There is no call to `prctl(PR_CAPBSET_DROP, ...)` or `cap_set_proc` in
`crates/daemon/` or `crates/platform/`. Remediation: after `drop_privileges`
on Linux, drop all caps from the bounding set except those required for
transfer (`CAP_DAC_READ_SEARCH` for read-only modules; none for read/write).
Use the `caps` crate (safe wrapper) inside `crates/platform/src/privilege.rs`.

### F6 -- `PR_SET_NO_NEW_PRIVS` / `SECBIT_NOROOT` not asserted
No call to `prctl(PR_SET_NO_NEW_PRIVS, 1, ...)` or `prctl(PR_SET_SECUREBITS,
SECBIT_NOROOT | SECBIT_NOROOT_LOCKED)` before privilege drop. Without
`NO_NEW_PRIVS` a setuid binary in the new root could re-elevate. Remediation:
gate behind `target_os = "linux"`, call before `setuid`, ignore `EINVAL` on
older kernels.

### F7 -- no seccomp-bpf filter
The worker continues to expose every syscall after the privilege drop.
Upstream rsync also lacks seccomp, but this is the natural place to add a
narrow allow-list (`read`, `write`, `openat`, `pread64`, `pwrite64`, `fstat`,
`stat`, `mmap`, `close`, `lseek`, `getdents64`, `unlinkat`, `renameat`,
`mkdirat`, `linkat`, `symlinkat`, `fchmodat`, `fchownat`, `utimensat`,
`fsync`, `clock_gettime`, `rt_sigreturn`, `exit`, `exit_group`, plus
io_uring entry points when the `fast_io` feature is on). Remediation: optional,
gated by a `daemon-seccomp` cargo feature, implemented through `libseccomp`.

### F8 -- no privilege-separation worker
Authentication, secrets-file parsing, TLS termination, and transfer all run in
the same address space. A heap-overflow before chroot would still see the
secrets file contents and the listening socket. Upstream takes the same risk;
adopting `fork()` after auth (parent retains root + listening socket; child
runs the transfer) would shrink the post-auth attack surface. Remediation:
introduce `crates/daemon/src/daemon/sections/privilege_sep.rs` that, on Unix,
forks before `apply_module_privilege_restrictions` and wires stdin/stdout
through a socketpair.

## Remediation priority

1. F1 -- 1-2 hours; no protocol risk, restores upstream parity for `daemon chroot`.
2. F2 -- 1 hour; correctness fix in `platform::privilege::drop_privileges`.
3. F3 -- half day; needs probe + cache + tests.
4. F4 -- 1 hour; trivial `cfg`-gated `seteuid` call.
5. F5/F6 -- 1 day; Linux-only, behind `daemon-hardening` feature flag.
6. F7 -- 2-3 days; needs allow-list calibration via interop runs.
7. F8 -- 3-5 days; design discussion required (process model change).
