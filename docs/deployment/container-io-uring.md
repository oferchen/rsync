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

### Seccomp Considerations

Default Docker and Podman seccomp profiles allow `io_uring_setup`,
`io_uring_enter`, and `io_uring_register` on kernels 5.6+. Custom seccomp
profiles may block these syscalls. If oc-rsync reports io_uring as disabled
despite a sufficient kernel version, check whether your seccomp profile allows
the `io_uring_*` syscall family.

## SQPOLL in rootless containers

Rootless Podman and Docker (and Kubernetes Pods with `runAsNonRoot`) are
the most common production deployment shape for oc-rsync, and they are
also the environments where `IORING_SETUP_SQPOLL` cannot be granted.
This section consolidates the problem, the runtime behaviour, and the
operator-facing controls so deployers can pick the right configuration
without reading the rest of the guide.

### The problem

`IORING_SETUP_SQPOLL` spawns a kernel thread that polls the submission
queue on the application's behalf. The kernel grants that thread
elevated scheduling priority and therefore requires `CAP_SYS_NICE` (or
real root) on the calling task. Rootless containers run inside a user
namespace where `CAP_SYS_NICE` is structurally unmapped:

- Rootless Podman drops the capability bounding set before exec.
- Rootless Docker (`--userns=...`) maps the host UID to an unprivileged
  user inside the namespace.
- Kubernetes Pods with `securityContext.runAsNonRoot: true` (or any
  Pod under the `restricted` Pod Security Admission profile) reject
  `capabilities.add: ["SYS_NICE"]` at admission time.

Without `CAP_SYS_NICE`, `io_uring_setup(2)` with the SQPOLL flag
returns `EPERM`. A naive implementation would either abort io_uring
entirely or surface an opaque "Operation not permitted" error to the
operator.

### Runtime behaviour: automatic detection and fallback

oc-rsync detects rootless containers up-front and skips SQPOLL before
the syscall is issued. The detection helper
`fast_io::detect_rootless_container()` inspects three signals (see
[`../design/sqpoll-rootless-container-detection.md`](../design/sqpoll-rootless-container-detection.md)
for the full specification):

| Signal | Source | Triggers verdict |
|--------|--------|------------------|
| User namespace map | `/proc/self/uid_map` shows a non-identity map | `UserNamespace` |
| Podman marker | `/run/.containerenv` exists | `Podman` |
| Docker marker | `/.dockerenv` exists | `Docker` |

The verdict is cached for the lifetime of the process via `OnceLock`
(one `open` + `read` of `/proc/self/uid_map` plus up to two `stat`
calls; total cost under five microseconds, once per process).

When any signal fires, `IoUringConfig::build_ring()` takes the
non-SQPOLL path immediately and emits one structured info-level log
line under the `Io` target at level 1 (visible with `--debug=io1`):

```text
io_uring SQPOLL disabled: rootless container detected (signal=podman, no CAP_SYS_NICE available in this user namespace); falling back to standard polling
```

The `signal=` label is one of `userns`, `podman`, or `docker` depending
on which probe fired first. The log is gated by a one-shot `Once`
guard so daemon workloads building many rings per process do not flood
operator logs - one decision, one log line. The emitter lives in
SQP-LAND.7 (`crates/fast_io/src/io_uring/config.rs:log_rootless_skip`).

The fallback ring is fully race-free and otherwise identical to the
SQPOLL ring: file and socket I/O, BGID-based buffer rings, registered
buffers, file registration, and zero-copy socket sends all stay
active. Only the kernel polling thread is omitted. The fallback is
also reflected in `--io-uring-status`:

```text
sqpoll fell back:   no
sqpoll opt-out:     yes (rootless container)
```

`sqpoll fell back: no` here means the kernel never rejected SQPOLL
because oc-rsync never asked for it. Contrast with the case where the
detection signal is absent and SQPOLL is requested anyway: the kernel
returns `EPERM` and the status shows `sqpoll fell back: yes
(CAP_SYS_NICE likely missing)`. Both paths converge on the same
non-SQPOLL ring; the detection path saves one failed syscall and
produces a clearer log line.

### Explicit opt-out: `--no-io-uring-sqpoll`

Operators who want a deterministic guarantee that SQPOLL is never
requested - regardless of how the rootless detector classifies the
environment - can pass `--no-io-uring-sqpoll` on the command line.
This is the policy `IoUringPolicy::SqpollOff`:

```bash
oc-rsync --no-io-uring-sqpoll src/ dst/
```

The flag is useful in three situations:

1. **Audit-restricted production deployments** where the security
   posture requires a positive, configuration-level statement that
   the SQPOLL kthread will never be created. The detection path is
   transparent but implicit; the flag is explicit and auditable.
2. **Bare-metal hosts that simulate rootless behaviour**. The
   detector returns `false` on a real host with `CAP_SYS_NICE`
   available, so the SQPOLL request would otherwise succeed.
   `--no-io-uring-sqpoll` lets a non-container test environment
   reproduce the production codepath exactly.
3. **Kubernetes Pods under the `restricted` Pod Security Admission
   profile** where the implicit `EPERM` fallback works but the flag
   removes the failed syscall from the audit log entirely.

The flag suppresses only `IORING_SETUP_SQPOLL`. All other io_uring
features remain available. See the CLI flag table at the bottom of
this guide for the full matrix.

### Granting SQPOLL inside Kubernetes (when you can)

Trusted clusters that allow capability grants can run SQPOLL inside
Pods by adding `SYS_NICE` to `securityContext.capabilities.add`:

```yaml
securityContext:
  capabilities:
    add:
      - SYS_NICE
```

This snippet only works under the `baseline` or `privileged` Pod
Security Admission profile and only when no admission controller
(OPA Gatekeeper, Kyverno) blocks the capability cluster-wide. The
full Pod spec, the PSA profile interaction table, and the daemon-Pod
deployment manifest live in
[kubernetes.md](kubernetes.md#2-sqpoll-and-cap_sys_nice-inside-pods).

### Throughput delta

The non-SQPOLL ring adds one `io_uring_enter(2)` syscall per
submission batch. For most rsync workloads (moderate file counts,
network-bound transfers) the difference is below measurement noise
because the batch amortises the syscall over many SQEs.

The cost becomes measurable on a specific workload shape: large
delta transfers with high `COPY`-token ratios against an mmap'd
basis file on NVMe storage. The bench plan in
[`../design/sqpoll-nvme-rebench.md`](../design/sqpoll-nvme-rebench.md)
estimates a **10-15% throughput reduction** for that workload
class. Two practical consequences:

- If your rootless deployment moves multi-gigabyte basis files over
  NVMe with a tight `COPY`-heavy delta, plan for the 10-15% overhead
  or schedule transfers on a host with `CAP_SYS_NICE` available.
- For network-bound transfers, mixed-size file trees, or anything
  smaller than ~1 GiB of basis I/O, the SQPOLL/non-SQPOLL delta is
  effectively noise. The rootless fallback is free.

The estimate is the upper bound; the in-container bench harness will
narrow it when the SQM/SQP-LAND closeout benches land. Until then,
treat 10-15% as the upper bound for the SQPOLL-eligible NVMe
workload and "no measurable delta" as the expectation for everything
else.

### Forcing detection for tests

The environment variable `OC_RSYNC_FORCE_ROOTLESS_CONTAINER` lets
integration tests and CI cells exercise the SQPOLL fall-back path on
hosts that are not actually rootless. Setting it to `1`, `true`, `yes`,
or `on` makes `fast_io::detect_rootless_container` and
`fast_io::rootless_signal` report rootless regardless of the real host
state, and the env hook runs before the cached `/proc` probe so it is
effective even after detection has already cached the host result. The
override is read by every `IoUringConfig::build_ring` call, so the same
graceful-fallback path that a real container would take is exercised
end-to-end. It is a test-only hook; setting it in production is harmless
on rootless hosts (the verdict was already going to be rootless) but
forces the SQPOLL skip on host systems, costing the same 10-15% NVMe
delta noted above. Do not set it in production manifests.

### Checking the active tier

To confirm which path is active inside your container:

```bash
podman run --rm myimage oc-rsync --io-uring-status
```

The full output format and example matrices are documented in
[Verifying io_uring Status](#verifying-io_uring-status) below.

## Kubernetes Configuration

For the full Kubernetes deployment guide - Job manifests, daemon-as-Pod
deployment, Pod Security Admission profile interactions, and the
quantitative tradeoff table for `--no-io-uring-sqpoll` - see
[kubernetes.md](kubernetes.md). The brief recipes below cover the most
common Pod spec snippets; refer to the dedicated guide for the full
context.

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

### Explicit SQPOLL opt-out for rootless Pods

Rootless Kubernetes Pods (most production clusters) cannot grant
`CAP_SYS_NICE` because Pod Security Admission rejects
`capabilities.add: ["SYS_NICE"]` under `restricted` and stricter
profiles. The default `Auto` policy still works - SQPOLL is attempted
and falls back transparently on `EPERM` - but operators who want a
deterministic guarantee that the SQPOLL kthread is never requested can
pass `--no-io-uring-sqpoll`:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: oc-rsync-rootless
spec:
  containers:
    - name: rsync
      image: myregistry/oc-rsync:latest
      command:
        - "oc-rsync"
        - "--no-io-uring-sqpoll"
        - "src/"
        - "dst/"
      # io_uring stays active for file and socket I/O; only
      # IORING_SETUP_SQPOLL is suppressed. BGID, registered buffers,
      # file registration and SEND_ZC remain available where supported.
```

The flag is also useful in non-K8s test environments that want to
reproduce production rootless behaviour exactly: the kernel never sees
a SQPOLL setup request, so the codepath under test matches the
audit-restricted production deployment.

### Kubernetes Security Policies

If your cluster uses PodSecurityStandards or PodSecurityPolicies:

- **Restricted profile**: Blocks `SYS_NICE`. SQPOLL is unavailable; basic
  io_uring may still work depending on the seccomp profile. Use
  `--no-io-uring-sqpoll` to suppress the SQPOLL request at the source.
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

## CLI Flags

| Flag | Effect |
|------|--------|
| `--io-uring` | Force io_uring on; error if the kernel does not support it. Policy = `Enabled`. |
| `--no-io-uring` | Disable io_uring entirely; always use standard buffered I/O. Policy = `Disabled`. |
| `--no-io-uring-sqpoll` | Keep io_uring on but suppress `IORING_SETUP_SQPOLL`. Policy = `SqpollOff`. Recommended for rootless containers and Kubernetes Pods without `CAP_SYS_NICE`. |
| `--io-uring-depth=N` | Override submission queue depth (default 64). |
| `--io-uring-status` | Print the capability matrix and exit. |

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
   Operators who want a deterministic opt-out (no SQPOLL request at all)
   should pass `--no-io-uring-sqpoll`; this matches the production behaviour
   under Pod Security Admission `restricted` profiles exactly. See
   [kubernetes.md](kubernetes.md) for the full Kubernetes deployment guide.

4. **Troubleshooting**: Run `oc-rsync --io-uring-status` inside the container
   to see the full capability matrix and identify which tier is active.
