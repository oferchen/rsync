# Landlock feature guidance for Linux distro packagers

Audience: Linux distribution maintainers packaging `oc-rsync` (Arch, Debian, Fedora, Alpine, RHEL/CentOS, NixOS, etc.).

## TL;DR

Linux distros SHOULD enable `--features landlock` when packaging oc-rsync, even on older distro kernels. The runtime probe plus best-effort ABI downgrade handle pre-5.13 kernels gracefully (no enforcement, no error, single startup log line). There is no "minimum kernel" gate beyond what the rest of oc-rsync already requires. Builds that opt out simply lose a defense-in-depth layer; the SEC-1 `*at` syscall chain remains the sole defense in that case.

## Build command for distro packagers

```sh
cargo build --release --bin oc-rsync --features landlock --locked
```

`--locked` keeps the resolved `Cargo.lock` set; combine with `cargo vendor` if the distro forbids fetching crates at build time.

## Runtime behaviour matrix

| Running kernel | Landlock ABI engaged | Enforcement summary |
|----------------|----------------------|---------------------|
| `>= 6.2`       | v3                   | Full enforcement: READ / WRITE / CREATE / DELETE / RENAME / SYMLINK + REFER (cross-hierarchy rename) + TRUNCATE. |
| `5.19 - 6.1`   | v2                   | v3 minus TRUNCATE. REFER still lets cross-hierarchy renames work. |
| `5.13 - 5.18`  | v1                   | v2 minus REFER. Single-tree operations work; cross-tree renames (e.g. `--backup-dir` outside the module root) are rejected. |
| `< 5.13`       | none                 | No enforcement. Daemon logs once at INFO and continues. SEC-1 `*at` chain is the sole defense. |

Best-effort downgrade is unconditional: the same binary requests v3 on every kernel and the `landlock` crate strips the bits the running kernel does not understand. The achieved ABI is logged once per daemon connection so operators can confirm the enforcement level.

## Per-distro guidance

Copy-pasteable recommendations. "Enable" means add `--features landlock` to the cargo build invocation in the distro spec / PKGBUILD / debian rules file.

### Debian

- Debian 12 (bookworm, stable): kernel 6.1 -- enable, v2 enforcement.
- Debian 13 (trixie, testing): kernel 6.6+ -- enable, full v3.
- Older oldstable users on 5.10 (bullseye): enable; v1 enforcement, single-tree operations only.

### Ubuntu

- Ubuntu 22.04 LTS (Jammy): kernel 5.15 -- enable, v1 enforcement.
- Ubuntu 24.04 LTS (Noble): kernel 6.8 -- enable, full v3.
- Ubuntu 24.10+ (Oracular and later): kernel 6.11+ -- enable, full v3.

### Fedora

- Fedora 39 / 40 / 41 (current): kernel 6.x -- enable, full v3.

### Arch / Manjaro / NixOS

- Rolling, kernel 6.x or newer -- enable, full v3.

### Alpine

- Alpine 3.20+: kernel 6.6 -- enable, full v3.
- Alpine 3.19 and earlier: kernel 6.1 -- enable, v2 enforcement.

### CentOS Stream / RHEL

- RHEL 9 / CentOS Stream 9: kernel 5.14 -- enable, v1 enforcement.
- RHEL 10 / CentOS Stream 10: kernel 6.x -- enable, full v3.

### openSUSE

- Leap 15.6: kernel 6.4 -- enable, full v3.
- Tumbleweed (rolling): kernel 6.x -- enable, full v3.

## Build-time dependencies

- The `landlock` Rust crate (currently 0.4.x) is pulled in transitively when the feature is enabled. Ensure `Cargo.lock` is checked into the source tarball, or vendor the dependency tree with `cargo vendor` for distros that disallow online fetches during build.
- No new system libraries. Landlock is a kernel LSM accessed via the `landlock_create_ruleset(2)`, `landlock_add_rule(2)`, and `landlock_restrict_self(2)` syscalls plus `prctl(2)`; all three live in `libc` and require no additional `-dev` / `-devel` package.
- Kernel headers are not required at build time. The `landlock` crate ships its own ABI definitions.

## Runtime dependencies

None. Landlock is part of the kernel; there is no userspace daemon, helper binary, or service to enable. If the running kernel exposes the LSM the sandbox engages automatically; if not, oc-rsync logs once and continues.

## Compatibility and conflicts

- Stackable with seccomp filters, mount namespaces, user namespaces, AppArmor, and SELinux. Landlock is purely additive (it only restricts; it never grants) so it composes cleanly with any other LSM the distro ships.
- Does not interact with the existing `use chroot = yes` daemon setting; both can be enabled together and each closes a different class of escape.
- Inherited by child processes spawned after `restrict_self()` engages, including the rsync name converter and any pre/post-xfer-exec hooks. Distributions that ship hooks expecting to touch paths outside the module tree should document the constraint.

## Disabling the feature

If a distro policy forbids the dependency or the kernel target is too old to be worth the runtime probe, build without the feature:

```sh
cargo build --release --bin oc-rsync --no-default-features \
    --features "io_uring,iocp" --locked
```

Adjust the explicit feature list to whatever else the distro normally enables. The SEC-1 `*at` syscall chain remains the sole defense in that case; the daemon is still hardened against the CVE-2026-29518 / CVE-2026-43619 TOCTOU symlink race class, just without the kernel-enforced second layer.

## AppArmor + SELinux templates

Landlock is stackable with classic LSMs. For distros where AppArmor (Ubuntu LTS, openSUSE, Debian) or SELinux (RHEL, Fedora, CentOS Stream) is the primary mandatory access control layer, ship the templates from `contrib/security/` alongside the binary:

| Template | Path | Audience |
|----------|------|----------|
| AppArmor profile | [`contrib/security/usr.sbin.oc-rsyncd.apparmor`](../../contrib/security/usr.sbin.oc-rsyncd.apparmor) | AppArmor-first distros |
| SELinux type enforcement | [`contrib/security/oc_rsyncd.te`](../../contrib/security/oc_rsyncd.te) | SELinux-enforcing distros |
| SELinux file contexts | [`contrib/security/oc_rsyncd.fc`](../../contrib/security/oc_rsyncd.fc) | SELinux-enforcing distros |
| SELinux interfaces | [`contrib/security/oc_rsyncd.if`](../../contrib/security/oc_rsyncd.if) | SELinux-enforcing distros |
| Install + verify guide | [`contrib/security/README.md`](../../contrib/security/README.md) | Packagers + ops |

These are templates, not strict requirements. Operators MUST customize the module-root stanzas to match the `path =` entries in their `oc-rsyncd.conf`. The templates leave the module roots commented out by default so a fresh install enforces only the configuration, log, and PID-file paths.

The SELinux template reuses the `rsync_data_t` label shipped by the base `selinux-policy` package, so it composes with any pre-labelled module trees the host already exposes to upstream `rsync`.

## Verifying the engaged ABI on a built binary

Run the daemon at info-level logging against a throwaway module and grep for the Landlock startup line:

```sh
oc-rsync --daemon --no-detach --log-file=- --log-file-format=info 2>&1 | \
    grep -i landlock
```

The line reports either the achieved ABI level (1, 2, or 3) or `landlock unavailable on this kernel`. Use this to confirm the package was built with the feature enabled and the host kernel exposes the LSM.

## Companion startup hardenings (always on, no operator action required)

Two Linux-only defense-in-depth measures run automatically at daemon startup, regardless of whether the `landlock` feature is compiled in:

1. **`PR_SET_NO_NEW_PRIVS`** is applied to the daemon process before the listener binds. The flag is a one-way bit that prevents any subsequent `execve()` from acquiring elevated privileges via setuid/setgid binaries, file capabilities, or LSM-mediated privilege grants. It also satisfies the precondition some kernels apply to Landlock engagement. The flag inherits across `fork()`, so every per-connection worker and every `pre-xfer-exec` / `post-xfer-exec` hook spawned by the daemon runs with it set.
2. **Active LSM detection** reads `/sys/kernel/security/lsm` and emits a single info-level line listing the kernel-side defenses (for example `lockdown,capability,landlock,yama,bpf`). Operators can grep this line to confirm which LSMs cover the daemon process - in particular whether Landlock is loaded alongside the SEC-1 `*at` chain. When `/sys/kernel/security/lsm` is absent (minimal containers without securityfs mounted) the daemon logs the skip and continues; LSM detection is observability, not enforcement.

Neither hardening requires configuration. Both short-circuit to a no-op on non-Linux targets. Verifying both at runtime:

```sh
oc-rsync --daemon --no-detach --log-file=- 2>&1 | \
    grep -E "PR_SET_NO_NEW_PRIVS|Linux Security Modules"
```

## Layered defense: seccomp BPF (`daemon-seccomp`)

`daemon-seccomp` adds a kernel-enforced syscall allowlist on top of Landlock. Where Landlock denies a path-based syscall with `EACCES`, seccomp denies an unlisted syscall with `SIGSYS` before the kernel ever consults the LSM stack. The two layers compose: a regression that bypasses `*at` helpers still hits Landlock; one that skips Landlock still hits seccomp.

```sh
cargo build --release --bin oc-rsync \
    --features "landlock,daemon-seccomp" --locked
```

- Opt-in only until the 14-day bake window in `docs/design/lsm-seccomp-allowlist.md` completes. Default builds remain seccomp-free; distros that want the extra layer enable both flags.
- Default action is `KILL_PROCESS`: an unlisted syscall delivers `SIGSYS` synchronously and terminates the worker. The parent `accept(2)` loop survives, so the daemon keeps serving other clients.
- Per-architecture: `x86_64` and `aarch64` are supported. On other architectures the helper logs `seccomp BPF unavailable in this build` and Landlock remains the sole layer.
- Stackable with chroot, mount namespaces, AppArmor, SELinux, and Landlock. No extra system dependencies; `seccompiler` is a pure-Rust crate that talks to `seccomp(2)` directly.

The 14-day bake window starts at merge of the opt-in feature. After zero missing-syscall reports, a follow-up PR flips the feature on by default for Linux release builds; operators who need to opt out get `--no-default-features` or a build-time exclude.

## Diagnostics: `--lsm-status` flag

Run the client with `--lsm-status` to print a one-shot Linux Security Module diagnostic for the current process and exit. The output covers the active LSM list, Landlock probe, seccomp state, and io_uring SQPOLL opt-out policy:

```sh
oc-rsync --lsm-status
```

Sample output on a 6.x Linux host:

```text
oc-rsync LSM diagnostic:
  active LSMs: lockdown,capability,landlock,yama,apparmor,bpf
  Landlock: available (kernel >= 5.13)
  seccomp: NOT applied (current process is the CLI, not a daemon worker)
  --no-io-uring-sqpoll: not set (SQPOLL would be requested if available)
```

The diagnostic is process-local: it reports the security posture of the CLI process itself, which is **not** a daemon worker. Daemon-side hardening (seccomp filter, Landlock allowlist, capability drop) engages only inside the per-connection worker. Use the daemon startup log to inspect those layers; use `--lsm-status` to confirm that the binary's compile-time `landlock` feature is honoured by the running kernel.

When a mandatory access control LSM (SELinux, AppArmor, Smack, Tomoyo) is present and the receiver path swallows a `Permission denied` while creating destination directories, the client emits a single info-level hint pointing at `ausearch -m AVC -ts recent` so operators can correlate the EACCES with an LSM AVC denial. The hint fires at most once per transfer to keep large file counts from flooding the log.

## Layered defense: capability drop

`oc-rsyncd` drops Linux process capabilities at two well-defined lifecycle points so that a compromised worker cannot regain privileges Landlock alone cannot revoke. The drop sequence composes with the startup hardenings above to form a three-layer kernel-enforced defense:

1. `PR_SET_NO_NEW_PRIVS` + active-LSM detection (always on; see "Companion startup hardenings" above).
2. **Capability drop** (this section).
3. seccomp BPF syscall filter (opt-in; tracked separately).

### Drop points

| Lifecycle phase | Action | Rationale |
|-----------------|--------|-----------|
| Startup, before the listener binds | Pre-flight check: every module with `uid = root` requires `CAP_CHOWN`; missing the capability exits with an operator-facing error pointing at `setcap` / `AmbientCapabilities` / `--cap-add` | Failing loud at startup beats producing a `chown failed` mid-transfer once clients are connected. |
| Immediately after `bind()` succeeds | `CAP_NET_BIND_SERVICE` dropped from effective, permitted, bounding sets | A worker compromised after bind cannot rebind 80, 443, or 22 to intercept traffic. |
| Per-worker, at the same point Landlock engages | Every capability not in the per-module required set is dropped from all three sets | Workers run with the minimum capability surface needed; modules without `uid = root` end up with an empty capability set. |

The full inventory of capabilities the daemon code path can request, together with the gating condition for each one, lives in `docs/design/lsm-cap-required-capabilities.md`. Distros packaging `oc-rsyncd` should consult that inventory when deciding which `AmbientCapabilities` line to ship in their default systemd unit.

### Pre-flight diagnostic

When a configured module needs a capability the daemon was not granted, the startup error follows this exact shape:

```
oc-rsyncd: error: rsyncd.conf module(s) uploads requires CAP_CHOWN but this capability is not granted.
Grant via:
  - systemd: AmbientCapabilities=CAP_CHOWN
  - setcap:  setcap cap_chown=eip /usr/sbin/oc-rsyncd
  - docker:  --cap-add=CHOWN
```

The diagnostic is emitted to the daemon log sink (or stderr when no log file is configured) and the daemon exits with the standard `FEATURE_UNAVAILABLE` exit code. No client connection is accepted, so packagers' pre-flight smoke tests (`systemctl start oc-rsyncd && systemctl is-active`) detect the misconfiguration without traffic in flight.

### Cross-references

- Per-capability inventory and rationale: `docs/design/lsm-cap-required-capabilities.md`.
- Active-LSM detection and `PR_SET_NO_NEW_PRIVS` (companion startup hardenings): see PR #5581.
- seccomp BPF filter (third layer): see PR #5589.
