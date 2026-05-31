# SQPOLL CAP_SYS_NICE capability requirements across kernel versions (SQP-1)

Tracking: SQP-1 (#3295). Sibling issues: SQP-2 through SQP-6 (#3296-#3300).

## Summary

io_uring's `IORING_SETUP_SQPOLL` flag spawns a dedicated kernel polling
thread that services the submission queue without requiring the application
to call `io_uring_enter(2)`. The capability requirements for this feature
have changed substantially across kernel versions. This audit documents the
per-version matrix, the failure modes oc-rsync encounters, and
recommendations for improved runtime detection.

## Per-kernel-version capability matrix

| Kernel | SQPOLL available | Capability requirement | Notes |
|--------|-----------------|----------------------|-------|
| < 5.1 | No | N/A | io_uring not present in kernel. |
| 5.1 - 5.10 | Yes | `root` (UID 0) | SQPOLL unconditionally required root. No capability-based alternative. |
| 5.11 - 5.12 | Yes | `CAP_SYS_NICE` or root | Commit `73572984` relaxed the root requirement. Non-root processes with `CAP_SYS_NICE` can create SQPOLL rings. Without the capability, `io_uring_setup` returns `EPERM`. |
| 5.13 - 5.18 | Yes | `CAP_SYS_NICE` or root | Stable behaviour. The SQPOLL kthread is shared across rings of the same task since 5.11, reducing per-ring kernel thread overhead. Idle timeout (`sq_thread_idle`) governs when the kthread sleeps. |
| 5.19+ | Yes | `CAP_SYS_NICE` or root | Same requirement. Additional SQPOLL-related fixes landed (e.g., `IORING_SETUP_SQ_AFF` cpu pinning stability). |
| 6.0+ | Yes | `CAP_SYS_NICE` or root | No further relaxation. `DEFER_TASKRUN` (6.1+) provides an alternative syscall-reduction mechanism without privilege requirements. |

Key observations:

- There is no kernel version where SQPOLL works for an unprivileged process
  without `CAP_SYS_NICE`. The 5.11 relaxation only moved the bar from
  "must be root" to "must have CAP_SYS_NICE".
- Unlike basic io_uring (which needs no capabilities on 5.6+), SQPOLL
  remains a privileged operation across all kernel versions.
- `DEFER_TASKRUN` (6.1+) is the unprivileged alternative that provides
  similar syscall reduction for mostly-idle ring patterns like oc-rsync's
  daemon path.

## Failure modes and error codes

When SQPOLL setup fails, the kernel returns specific error codes that
oc-rsync must handle:

| Error | Meaning | Kernel versions | Recovery |
|-------|---------|-----------------|----------|
| `EPERM` | Process lacks `CAP_SYS_NICE` (or root on < 5.11) | All | Fall back to regular `io_uring_enter`-based submission. |
| `ENOMEM` | Kernel cannot allocate resources for the SQPOLL kthread | All | Fall back to regular ring. Transient - memory pressure. |
| `EINVAL` | Invalid `sq_thread_idle` value or incompatible flags | All | Fatal - configuration bug. Should not occur with valid `IoUringConfig`. |
| `ENOSYS` | `io_uring_setup(2)` not present (kernel < 5.1 or disabled) | < 5.1, or seccomp-blocked | Entire io_uring path disabled. |

In oc-rsync, `IoUringConfig::build_ring()` at
`crates/fast_io/src/io_uring/config.rs:357-387` implements the fallback:

```
if sqpoll_safe {
    let mut builder = io_uring::IoUring::builder();
    builder.setup_sqpoll(self.sqpoll_idle_ms);
    match builder.build(self.sq_entries) {
        Ok(ring) => return Ok(ring),
        Err(_) => {
            // SQPOLL requires CAP_SYS_NICE on most kernels. Record
            // the fallback so callers can surface it in diagnostics.
            SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
        }
    }
}
RawIoUring::new(self.sq_entries)
```

The error from `builder.build()` is not inspected - any failure triggers
the fallback. The `SQPOLL_FALLBACK` atomic is exposed via
`sqpoll_fell_back()` for diagnostics.

## Current graceful fallback behaviour in oc-rsync

The fallback chain is layered and fully transparent to callers:

1. **SQPOLL ring construction fails** - `build_ring()` catches any error
   from the SQPOLL `builder.build()` call, sets `SQPOLL_FALLBACK = true`,
   and retries with `RawIoUring::new(sq_entries)` (a regular ring).

2. **mmap basis conflict** - When `mmap_basis_active` is set and the
   `sqpoll-mlock-basis` feature is off, `build_ring()` refuses SQPOLL
   before even attempting it, sets `SQPOLL_FALLBACK`, and builds a regular
   ring. This prevents the SQPOLL kthread page-fault hazard.

3. **mlock downgrade** - With `sqpoll-mlock-basis` on (default), the
   `WiredBasisWindow` in `sqpoll_basis.rs` pins mmap'd pages via `mlock(2)`
   before each SQPOLL submission. If `mlock` returns `EAGAIN`, `EPERM`, or
   `ENOMEM`, the submission is routed through the regular (non-SQPOLL) ring
   for that batch. Counters track the downgrade ratio.

4. **Complete io_uring unavailability** - If the base io_uring probe
   (`is_io_uring_available()`) fails, the entire path degrades to standard
   `read(2)`/`write(2)` via `BufReader`/`BufWriter`.

Diagnostics surfacing:
- `sqpoll_fell_back()` returns `true` if SQPOLL was attempted and failed.
- `io_uring_availability_reason()` emits a human-readable string.
- `detect_io_uring_restriction()` returns a typed `IoUringRestriction` enum.
- `mlock_downgrades()` / `mlock_attempts()` expose the SQM-3 rollback ratio.

## Container implications

### Rootless Podman / Docker

Rootless containers (the default for Podman, optional for Docker) run the
container process as an unprivileged user mapped via user namespaces. In
this configuration:

- **Basic io_uring**: Usually works on 5.6+ unless the seccomp profile
  blocks `io_uring_setup(2)`. Docker's default seccomp profile blocked
  io_uring before v20.10.2 (December 2020). Podman's default profile
  allows io_uring.
- **SQPOLL**: Always fails with `EPERM`. The container process has no
  capabilities in the initial user namespace, and `CAP_SYS_NICE` in a user
  namespace does not grant the real-namespace capability needed for SQPOLL.
- **oc-rsync behaviour**: Falls back silently to regular io_uring. No
  user-visible error. The `sqpoll_fell_back()` flag is set for diagnostics.

### Privileged containers

Containers started with `--privileged` or `--cap-add=SYS_NICE`:

- **Basic io_uring**: Works.
- **SQPOLL**: Works. `CAP_SYS_NICE` in the initial namespace is sufficient.
- **oc-rsync behaviour**: SQPOLL ring construction succeeds.

### Kubernetes pods

- Default pods: No `CAP_SYS_NICE`. SQPOLL fails with `EPERM`.
- Pods with explicit `securityContext.capabilities.add: ["SYS_NICE"]`:
  SQPOLL works.
- gVisor/Kata containers: `io_uring_setup(2)` may be entirely blocked.
  oc-rsync detects this via the startup probe and disables all io_uring.

### Docker default seccomp profile

| Docker version | `io_uring_setup` | `io_uring_enter` | SQPOLL |
|----------------|-----------------|-----------------|--------|
| < 20.10.2 | Blocked | Blocked | N/A |
| >= 20.10.2 | Allowed | Allowed | Needs `CAP_SYS_NICE` |

### Summary table: SQPOLL in container environments

| Environment | Basic io_uring | SQPOLL | Recommendation |
|-------------|---------------|--------|----------------|
| Bare metal, root | Yes | Yes | SQPOLL available if opted in |
| Bare metal, user + `CAP_SYS_NICE` | Yes | Yes | SQPOLL available if opted in |
| Bare metal, unprivileged user | Yes | No | Silent fallback works correctly |
| Docker (rootful, default) | Yes (>= 20.10.2) | No | Add `--cap-add=SYS_NICE` if needed |
| Docker `--privileged` | Yes | Yes | SQPOLL available |
| Podman rootless | Yes | No | Cannot grant real CAP_SYS_NICE |
| Podman rootful + `--cap-add=SYS_NICE` | Yes | Yes | SQPOLL available |
| Kubernetes default | Yes | No | Add SYS_NICE to securityContext |
| Kubernetes + SYS_NICE | Yes | Yes | SQPOLL available |
| gVisor | No | No | All io_uring disabled |

## Recommendations for runtime detection improvements

### 1. Log the specific errno on SQPOLL failure

Currently `build_ring()` discards the error from `builder.build()` and
only sets a boolean flag. Logging the specific errno would help operators
distinguish between `EPERM` (capability issue - addressable) and `ENOMEM`
(resource pressure - transient).

**Location:** `crates/fast_io/src/io_uring/config.rs:375-381`

Proposed change: log the error at debug level before setting the fallback
flag:

```rust
Err(e) => {
    logging::debug_log!(
        Io, 1,
        "io_uring SQPOLL setup failed: {e}; falling back to regular submission \
         (CAP_SYS_NICE required on kernel >= 5.11, root required on < 5.11)"
    );
    SQPOLL_FALLBACK.store(true, Ordering::Relaxed);
}
```

### 2. Expose kernel version in SQPOLL fallback diagnostic

The `sqpoll_fell_back()` API returns a bare boolean. Callers cannot
distinguish "SQPOLL failed because kernel is 5.8 and we are not root" from
"SQPOLL failed because kernel is 6.1 and we lack CAP_SYS_NICE". A richer
return type or companion function would help:

```rust
pub fn sqpoll_fallback_reason() -> Option<SqpollFallbackReason> { ... }
```

### 3. Recommend DEFER_TASKRUN for unprivileged daemon paths

For the daemon TCP socket path, `DEFER_TASKRUN` (6.1+) provides most of
the syscall-reduction benefit of SQPOLL without any capability requirement.
This is already documented in
`docs/audits/iouring-socket-sqpoll-defer-taskrun.md` but not yet
implemented. Priority: low - the daemon defaults to regular submission
which is correct and performs well.

### 4. Add a VersionRequirement for SQPOLL

The `kernel_version.rs` module defines `VersionRequirement` implementors
for io_uring, PBUF_RING, LINKAT, STATX/RENAMEAT, and SEND_ZC - but not
for SQPOLL itself. Adding one would centralise the documentation:

```rust
/// SQPOLL basic availability (Linux 5.1+ with root, 5.11+ with CAP_SYS_NICE).
pub struct SqpollRequirement;

impl VersionRequirement for SqpollRequirement {
    fn min_version(&self) -> KernelVersion {
        KernelVersion { major: 5, minor: 1 }
    }
    fn feature_name(&self) -> &str {
        "SQPOLL"
    }
}
```

Note: the `VersionRequirement` trait only tracks the kernel floor; it
cannot express the capability requirement. A doc comment should note that
SQPOLL requires `CAP_SYS_NICE` (or root) in addition to the kernel version.

### 5. Container-specific documentation in --io-uring-status output

When `--io-uring-status` or `--version` reports SQPOLL fallback, it could
hint at the container-specific fix:

```text
io_uring: enabled (kernel 6.1, 50 ops supported, pbuf_ring=yes)
SQPOLL: fell back to regular submission (EPERM - add CAP_SYS_NICE or run privileged)
```

## Cross-references

- `crates/fast_io/src/io_uring/config.rs` - Ring construction and SQPOLL
  fallback logic.
- `crates/fast_io/src/kernel_version.rs` - `VersionRequirement` trait and
  kernel detection.
- `crates/fast_io/src/sqpoll_basis.rs` - SQM-3 mlock wiring for SQPOLL +
  mmap pairing.
- `crates/fast_io/src/status.rs` - `IoUringRestriction` enum and
  `detect_io_uring_restriction()`.
- `docs/audit/iouring-kernel-support-matrix.md` - Per-opcode kernel floor
  matrix (IKV-10).
- `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` - DEFER_TASKRUN as
  the unprivileged alternative for daemon sockets.
- `docs/audits/io-uring-sqpoll-mmap-interaction.md` - SQPOLL + mmap
  page-fault hazard audit.
- `docs/design/mmap-vs-sqpoll-decision.md` - Decision framework for basis
  file I/O strategy.
