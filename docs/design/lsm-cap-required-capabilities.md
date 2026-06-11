# LSM-CAP required capability inventory

Audience: maintainers reviewing the LSM defense-in-depth surface and packagers writing systemd / Docker / setcap manifests for `oc-rsyncd`.

This document tracks every Linux capability the daemon code path can request, the syscall(s) that gate the requirement, and the operator-facing condition that promotes the capability from "not needed" to "must be granted". It is the source of truth for `crates/daemon/src/daemon/sections/capabilities.rs::required_capabilities_for_module` and for the pre-flight check that exits at startup when a configuration requires a capability the daemon was not granted.

The inventory composes with the LSM startup hardening (`PR_SET_NO_NEW_PRIVS` + active-LSM logging) and the seccomp BPF filter to form a three-layer kernel-enforced defense. Capability dropping is the second layer: even if a worker is compromised after Landlock and seccomp engage, the residual attack surface is bounded by whatever capabilities remain.

## Inventory

| Capability | Syscall gate | Configuration condition | Drop point |
|------------|-------------|-------------------------|------------|
| `CAP_NET_BIND_SERVICE` | `bind(2)` to a port < 1024 | Daemon `port = <1024>` in `rsyncd.conf` or `--port=873` on the command line | Immediately after `bind()` succeeds, before the accept loop starts (`drop_cap_net_bind_service`). Rare in containerized deployments where the port mapping is done by the orchestrator. |
| `CAP_CHOWN` | `fchown(2)`, `chown(2)`, `lchown(2)` | Module with `uid = root` that may invoke `--chown`, `--owner`, or `--group` against transferred files | Pre-flight check (`preflight_required_capabilities`) verifies it is held at startup; per-worker drop (`drop_worker_capabilities`) retains it only for modules whose `uid = 0`. |
| `CAP_DAC_READ_SEARCH` | `openat2(2)` with `O_RDONLY` on a file the daemon's effective uid cannot read | Daemon serves a module whose backing path contains files owned by other uids and the daemon was not configured with a per-module `uid =` matching the file owner | Dropped at worker fork via `drop_worker_capabilities`; not in the required set today because the chroot + module `uid =` directive should already cover the access surface. Documented for the audit so any future regression that reaches for it is intentional, not silent. |
| `CAP_FOWNER` | `chmod(2)`, `fchmodat(2)`, `utimensat(2)` on files not owned by the effective uid | Module with `uid = root` that may invoke `--chmod`, `--perms`, or `--times` against files owned by other uids | Dropped at worker fork. Modules that rely on permission writes on foreign-owned files must combine `uid = root` with explicit packaging (systemd `AmbientCapabilities=CAP_FOWNER`). Today the inventory drops it; module configurations that depend on it should land alongside a follow-up that promotes it to the required set. |
| `CAP_SETUID` | `setresuid(2)` triggered by `uid = …` directive | Module with `uid = <non-root>` directive distinct from the daemon process uid | Used in the pre-daemonize privilege drop (`platform::privilege::drop_privileges`) before any worker forks. Dropped wholesale at worker fork because subsequent privilege transitions are not permitted by upstream rsync's session model. |
| `CAP_SETGID` | `setresgid(2)` triggered by `gid = …` / `groups = …` directives | Same as `CAP_SETUID` but for the group switch and `setgroups(2)` | Same lifecycle as `CAP_SETUID`. |

`CAP_DAC_READ_SEARCH` and `CAP_FOWNER` deliberately ship in the "dropped" column today: the code path does not exercise them under any test configuration. Adding a module pattern that needs them is a deliberate change that must update both the inventory and the `required_capabilities_for_module` switch.

## Drop sequence

1. **Daemon startup (`run_daemon` / `run_daemon_stdio` / `serve_inetd_session`)**: configuration is parsed, the module table is built, and `preflight_required_capabilities` runs. A missing capability for a required configuration exits with an explicit multi-line error that lists the systemd / setcap / docker remediation. Composes with PR #5581's `apply_startup_hardening`: capability checks run after `PR_SET_NO_NEW_PRIVS` has been set so any `execve()` triggered during the pre-flight cannot regain capabilities the kernel previously denied.
2. **After listener bind (TCP path only)**: `drop_cap_net_bind_service` removes `CAP_NET_BIND_SERVICE` from the effective, permitted, and bounding sets. Workers forked after this point cannot acquire it.
3. **Per-worker fork (`process_approved_module` immediately before Landlock engagement)**: `drop_worker_capabilities` enumerates the daemon's current permitted set, computes the per-module required set via `required_capabilities_for_module`, and drops every capability not in the required set from effective, permitted, and bounding. Modules without `uid = root` end up with an empty residual capability set, which is the strongest posture available short of user-namespace isolation.

## Composition with the other LSM layers

| Layer | Defends against | Wired in |
|-------|----------------|----------|
| `PR_SET_NO_NEW_PRIVS` + active-LSM log | setuid/setcap execve regression; operator visibility of kernel LSM coverage | PR #5581 (`hardening.rs`) |
| Capability drop sequence | Residual privilege after worker fork; rebinding privileged ports from a compromised worker | This change (`capabilities.rs`) |
| seccomp BPF filter | Direct syscall surface of the worker process | PR #5589 |

All three layers run inside `apply_startup_hardening()` once PR #5581 merges. Until then the capability drop wires in at the same lifecycle points as Landlock (`engage_landlock_sandbox`) and at the daemon entry points, where the pre-flight check has access to the parsed module table.

## Operator remediation reference

When the pre-flight exits with `requires CAP_CHOWN but this capability is not granted`, the operator picks one of:

```ini
# /etc/systemd/system/oc-rsyncd.service
[Service]
AmbientCapabilities=CAP_CHOWN
CapabilityBoundingSet=CAP_CHOWN
```

```sh
# setcap (must be re-applied after every binary upgrade)
sudo setcap cap_chown=eip /usr/sbin/oc-rsyncd
```

```sh
# docker / podman
docker run --cap-add=CHOWN oc-rsync/oc-rsyncd:latest
```

The same pattern applies to every capability listed in the inventory above. Operators who configure a module that needs `CAP_FOWNER` or `CAP_DAC_READ_SEARCH` must extend the inventory entry to include their case before the pre-flight will accept the configuration.
