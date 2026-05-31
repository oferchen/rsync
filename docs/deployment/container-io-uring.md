# Container Deployment with io_uring

This guide covers deploying oc-rsync in containers (Podman, Docker, Kubernetes)
with io_uring acceleration. The focus is on SQPOLL mode, which delivers the
highest throughput but requires explicit capability grants in containerised
environments.

## Kernel Version Requirements

io_uring support in oc-rsync requires specific minimum kernel versions depending
on which features are used:

| Feature | Minimum Kernel | Notes |
|---------|---------------|-------|
| Basic io_uring (read/write/send) | 5.6 | `IORING_SETUP_SQPOLL` also available from 5.6 |
| `IORING_OP_STATX` | 5.11 | Async stat operations |
| `IORING_OP_RENAMEAT` | 5.11 | Atomic file renames |
| `IORING_OP_LINKAT` | 5.15 | Hard link creation |
| Provided buffer rings (`PBUF_RING`) | 5.19 | Kernel-managed buffer pools |
| `IORING_OP_SEND_ZC` | 6.0 | Zero-copy socket sends |
| Full performance tier | 6.0+ | All opcodes and optimisations available |

When the kernel is below 5.6, oc-rsync falls back to standard buffered I/O
transparently. No configuration change is needed.

## SQPOLL Mode and CAP_SYS_NICE

### What is SQPOLL?

`IORING_SETUP_SQPOLL` creates a dedicated kernel thread that polls the
submission queue on behalf of the application. This eliminates one
`io_uring_enter(2)` syscall per I/O batch - a measurable win on high-IOPS
workloads (many small files or high-throughput streaming).

### Why CAP_SYS_NICE is Required

The SQPOLL kernel thread runs at elevated scheduling priority. The kernel
requires `CAP_SYS_NICE` (or root) to create it. Without this capability,
`io_uring_setup(2)` with `IORING_SETUP_SQPOLL` returns `EPERM`.

### Graceful Fallback Behaviour

When SQPOLL setup fails, oc-rsync handles it transparently:

1. The `build_ring()` function attempts SQPOLL if configured.
2. On `EPERM` (or `ENOMEM`), it records the fallback in a global flag.
3. It creates a regular io_uring ring instead (no SQPOLL thread).
4. Transfers proceed normally with slightly higher per-batch syscall cost.

This fallback is always safe. No data is lost, no errors are raised. The only
difference is a small performance reduction on I/O-bound workloads.

## Container Runtime Configuration

### Podman

```bash
# Grant CAP_SYS_NICE for SQPOLL support
podman run --cap-add SYS_NICE myimage oc-rsync ...

# Or use --privileged (grants all capabilities - less secure)
podman run --privileged myimage oc-rsync ...
```

### Docker

```bash
# Grant CAP_SYS_NICE for SQPOLL support
docker run --cap-add SYS_NICE myimage oc-rsync ...

# Or use --privileged (grants all capabilities - less secure)
docker run --privileged myimage oc-rsync ...
```

### Rootless Containers

Rootless Podman and Docker (running without the daemon as root) cannot grant
`CAP_SYS_NICE` because the user namespace does not map the capability. In
rootless mode:

- Basic io_uring works if the host kernel is 5.6+ and `io_uring_setup(2)` is
  not blocked by seccomp.
- SQPOLL is unavailable. The fallback to regular submission is automatic.
- Some container runtimes block `io_uring_setup(2)` entirely via seccomp
  profiles. In that case, oc-rsync falls back to standard buffered I/O.

To check whether io_uring is available inside your container:

```bash
podman run --rm myimage oc-rsync --io-uring-status
```

### Seccomp Considerations

Default Docker and Podman seccomp profiles allow `io_uring_setup`,
`io_uring_enter`, and `io_uring_register` on kernels 5.6+. Custom seccomp
profiles may block these syscalls. If oc-rsync reports io_uring as disabled
despite a sufficient kernel version, check whether your seccomp profile allows
the `io_uring_*` syscall family.

## Kubernetes Configuration

### Pod securityContext for SQPOLL

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: oc-rsync-transfer
spec:
  containers:
    - name: rsync
      image: myregistry/oc-rsync:latest
      command: ["oc-rsync", "--io-uring", "src/", "dst/"]
      securityContext:
        capabilities:
          add:
            - SYS_NICE
```

### Without SQPOLL (default, no extra capabilities)

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: oc-rsync-transfer
spec:
  containers:
    - name: rsync
      image: myregistry/oc-rsync:latest
      command: ["oc-rsync", "src/", "dst/"]
      # No extra capabilities needed - io_uring works without SQPOLL,
      # and standard I/O fallback engages if io_uring is blocked.
```

### Kubernetes Security Policies

If your cluster uses PodSecurityStandards or PodSecurityPolicies:

- **Restricted profile**: Blocks `SYS_NICE`. SQPOLL is unavailable; basic
  io_uring may still work depending on the seccomp profile.
- **Baseline profile**: Does not add `SYS_NICE` by default but does not block
  it if explicitly requested in the pod spec.
- **Privileged profile**: All capabilities available.

## Verifying io_uring Status

Use the `--io-uring-status` flag to print the full capability matrix:

```bash
oc-rsync --io-uring-status
```

Example output inside a container with CAP_SYS_NICE:

```
io_uring capability matrix:

  compiled in:        yes
  platform:           linux
  kernel version:     6.1
  available:          yes
  supported ops:      48
  pbuf_ring:          yes (kernel 5.19+)
  sqpoll fell back:   no

  feature gates:
    io_uring:             on
    iouring-data-reads:   on
    iouring-send-zc:      on
```

Example output in a rootless container without CAP_SYS_NICE:

```
io_uring capability matrix:

  compiled in:        yes
  platform:           linux
  kernel version:     6.1
  available:          yes
  supported ops:      48
  pbuf_ring:          yes (kernel 5.19+)
  sqpoll fell back:   yes (CAP_SYS_NICE likely missing)

  feature gates:
    io_uring:             on
    iouring-data-reads:   on
    iouring-send-zc:      on
```

The `sqpoll fell back: yes` line confirms that SQPOLL was requested but the
kernel rejected it. Transfers still use io_uring - just without the SQPOLL
kernel thread.

## Performance Implications

| Mode | Syscall Profile | Best For |
|------|----------------|----------|
| io_uring + SQPOLL | Zero `io_uring_enter` calls while the kernel thread is active (idle timeout: 1s) | High-IOPS workloads, many small files, sustained streaming |
| io_uring (no SQPOLL) | One `io_uring_enter` per submission batch | General-purpose transfers, most container deployments |
| Standard buffered I/O | One syscall per read/write | Kernels below 5.6, seccomp-restricted containers |

In practice, the difference between SQPOLL and non-SQPOLL io_uring is
measurable only at high submission rates (thousands of SQEs per second). For
typical rsync workloads transferring moderate numbers of files, non-SQPOLL
io_uring already provides most of the benefit over standard I/O.

## Environment Variables

| Variable | Effect |
|----------|--------|
| `OC_RSYNC_DISABLE_IOURING=1` | Force standard I/O fallback even on io_uring-capable kernels. Useful for troubleshooting. |

## Summary of Recommendations

1. **Most container deployments**: No special configuration needed. io_uring
   activates automatically on Linux 5.6+ and falls back gracefully otherwise.

2. **Performance-critical deployments on Linux 6.0+**: Add `--cap-add SYS_NICE`
   (Podman/Docker) or `capabilities.add: [SYS_NICE]` (Kubernetes) to enable
   SQPOLL mode.

3. **Rootless containers**: SQPOLL is unavailable. Basic io_uring still works
   unless blocked by seccomp. No action required - the fallback is automatic.

4. **Troubleshooting**: Run `oc-rsync --io-uring-status` inside the container
   to see the full capability matrix and identify which tier is active.
