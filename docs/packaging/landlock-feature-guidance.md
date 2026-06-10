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

## Verifying the engaged ABI on a built binary

Run the daemon at info-level logging against a throwaway module and grep for the Landlock startup line:

```sh
oc-rsync --daemon --no-detach --log-file=- --log-file-format=info 2>&1 | \
    grep -i landlock
```

The line reports either the achieved ABI level (1, 2, or 3) or `landlock unavailable on this kernel`. Use this to confirm the package was built with the feature enabled and the host kernel exposes the LSM.

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
