# io_uring Per-Feature Deployment Matrix

This document maps each io_uring feature used by oc-rsync to its minimum kernel
version, recommended kernel, and fallback behavior. Use it to understand which
optimizations are available on a given deployment target and what performance
impact to expect when features are unavailable.

## Feature-Kernel Requirements

| Feature | Min Kernel | Recommended Kernel | Fallback Behavior |
|---------|-----------|-------------------|-------------------|
| Basic io_uring (read/write/send) | 5.6 | 5.15+ | Standard buffered I/O (`BufReader`/`BufWriter`) |
| Registered buffers (`IORING_REGISTER_BUFFERS`) | 5.6 | 5.15+ | Regular `READ`/`WRITE` opcodes (no page-pin savings) |
| File registration (`IORING_REGISTER_FILES`) | 5.6 | 5.15+ | Per-SQE kernel file table lookup (~50ns overhead per op) |
| SQPOLL (`IORING_SETUP_SQPOLL`) | 5.6 + `CAP_SYS_NICE` | 6.0+ | Regular ring with `io_uring_enter` syscalls per batch |
| Async cancel (`IORING_OP_ASYNC_CANCEL`) | 5.5 | 5.19+ | N/A - internal primitive, not user-visible |
| Async cancel by fd (extended match) | 5.19 | 5.19+ | Per-`user_data` cancel only |
| STATX (`IORING_OP_STATX`) | 5.11 | 5.15+ | Synchronous `statx(2)` syscall per file |
| RENAMEAT (`IORING_OP_RENAMEAT`) | 5.11 | 5.15+ | Synchronous `renameat2(2)` syscall |
| LINKAT (`IORING_OP_LINKAT`) | 5.15 | 5.15+ | Synchronous `linkat(2)` syscall |
| Linked timeouts (`IOSQE_IO_LINK`) | 5.6 | 5.15+ | Separate timeout management |
| Provided buffer rings (PBUF_RING) | 5.19 | 6.1+ | Classic `IORING_OP_PROVIDE_BUFFERS` (5.6+), then standard I/O |
| SEND_ZC (`IORING_OP_SEND_ZC`) | 6.0 | 6.1+ | Regular `IORING_OP_SEND` (userspace copy to socket buffer) |
| Multi-CQ (multiple completion queues) | 6.0 | 6.1+ | Single shared ring with mutex serialization |

## LTS Kernel Coverage

Linux LTS kernels determine which features are realistically available in
production environments. Most enterprise and cloud deployments run LTS kernels.

| LTS Kernel | EOL | Available Features |
|-----------|-----|-------------------|
| 5.4 | Dec 2025 | None (below 5.6 floor) |
| 5.10 | Dec 2026 | None (below 5.11 for STATX/RENAMEAT; basic io_uring available but limited) |
| 5.15 | Dec 2026 | Basic io_uring, registered buffers, file registration, SQPOLL, STATX, RENAMEAT, LINKAT, linked SQE chains |
| 6.1 | Dec 2026 | All of 5.15 + PBUF_RING, SEND_ZC, multi-CQ, extended async cancel |
| 6.6 | Dec 2026 | All features (full io_uring feature set) |

### Production deployment summary

- **5.15 LTS** (RHEL 9, Ubuntu 22.04, Debian 12): Full file I/O acceleration
  but no SEND_ZC, no PBUF_RING. Socket sends use regular `IORING_OP_SEND`.
  This is the most common production kernel as of 2026.
- **6.1 LTS** (Ubuntu 24.04, Debian 13): All features available including
  SEND_ZC. Recommended minimum for deployments that want zero-copy network
  sends.
- **6.6 LTS** (upcoming distro releases): All features with the most mature
  io_uring implementation and fewest known race conditions in the SQPOLL path.

## Checking Runtime Feature Availability

Use `--io-uring-status` to print the full capability matrix for the running
system:

```
oc-rsync --io-uring-status
```

Example output on a 5.15 LTS kernel:

```
io_uring capability matrix:

  compiled in:        yes
  platform:           linux
  kernel version:     5.15
  available:          yes
  supported ops:      44
  pbuf_ring:          no
  sqpoll fell back:   yes (CAP_SYS_NICE likely missing)

  feature gates:
    io_uring:             on
    iouring-data-reads:   on
    iouring-data-writes:  on
    iouring-send-zc:      on
    sqpoll-mlock-basis:   on

  active fallback chain:
    1. io_uring (primary)
    2. standard buffered I/O (fallback on ring failure)
```

The `supported ops` count reflects how many opcodes the kernel advertises via
`IORING_REGISTER_PROBE`. Each feature is probed individually at runtime - the
kernel version check is a first gate, but the opcode probe is authoritative
because distros may backport features and container runtimes may restrict
opcodes regardless of kernel version.

## Runtime Detection Strategy

oc-rsync uses a layered probe approach rather than relying solely on kernel
version strings:

1. **Version floor** - `uname().release` must parse to >= 5.6. Below this,
   io_uring is not attempted.
2. **Ring construction** - `io_uring_setup(2)` is called with a 4-entry ring.
   Failure indicates seccomp blocking or container runtime restrictions.
3. **Per-opcode probe** - `IORING_REGISTER_PROBE` queries individual opcode
   support. Features like SEND_ZC, STATX, and LINKAT check their specific
   opcode bit before first use.
4. **Graceful degradation** - Each feature falls back independently. A failed
   SQPOLL does not prevent regular io_uring. A missing SEND_ZC does not prevent
   file I/O acceleration.

Results are cached in process-wide atomics after the first check. The
environment variable `OC_RSYNC_DISABLE_IOURING=1` forces all io_uring off for
debugging.

## Performance Impact of Missing Features

| Missing Feature | Performance Impact | Affected Workload |
|----------------|-------------------|-------------------|
| Basic io_uring | ~15-30% slower file I/O (loss of batched submissions) | All transfers |
| Registered buffers | ~5-10% slower (extra `get_user_pages` per SQE) | Large file reads/writes |
| File registration | ~2-5% slower (per-SQE file table lookup) | Many small files |
| SQPOLL | One `io_uring_enter` syscall per batch instead of zero | High-IOPS workloads |
| STATX | One synchronous syscall per file during enumeration | Directory traversal |
| RENAMEAT | One synchronous syscall per temp-file commit | Every file written |
| LINKAT | One synchronous syscall per hardlink | `--hard-links` transfers |
| PBUF_RING | Pre-assigned buffers per SQE (less flexible allocation) | Socket reads |
| SEND_ZC | Userspace copy into socket buffer (~4 GB/s CPU ceiling) | Daemon/network sends of large payloads |
| Multi-CQ | Single ring shared under mutex (serialized submissions) | Concurrent multi-file I/O |

### SEND_ZC specifics

`IORING_OP_SEND_ZC` eliminates the userspace-to-kernel copy for socket sends.
The kernel pins the user pages and DMA-transfers directly from them. The
benefit is most visible on high-bandwidth daemon transfers where the CPU would
otherwise saturate copying large payloads into the socket buffer.

Without SEND_ZC, oc-rsync uses regular `IORING_OP_SEND` which still benefits
from io_uring's batched submission model but requires the kernel to copy each
payload from userspace into the socket buffer. On 10 Gbps+ links, this copy
becomes the bottleneck.

Key constraints:
- Feature gate: `iouring-send-zc` Cargo feature must be enabled at compile time
- CLI gate: `--zero-copy` flag must be passed (policy defaults to `Auto` which
  does not enable SEND_ZC; only `Enabled` routes through the zero-copy path)
- Minimum payload: 4 KiB (`SEND_ZC_DISPATCH_MIN_BYTES`) - sub-page sends lose
  to regular SEND due to `get_user_pages_fast` overhead
- Two CQEs per submission: transfer CQE + notification CQE (buffer release)
- Buffer must remain valid until notification CQE arrives (the wrapper blocks
  synchronously for both CQEs)

## Privilege Requirements

| Feature | Privilege | Notes |
|---------|-----------|-------|
| Basic io_uring | None | Blocked by seccomp in Docker < 20.10.2, gVisor |
| SQPOLL | `CAP_SYS_NICE` or root | Falls back transparently on EPERM |
| Registered buffers | None | Pins pages in kernel; subject to RLIMIT_MEMLOCK |
| File registration | None | Eliminates per-SQE file table lookups |
| SEND_ZC | None (kernel 6.0+) | Page pinning uses same path as registered buffers |
| Container (general) | N/A | `io_uring_setup(2)` may be blocked entirely by seccomp profile |

## Container Runtime Compatibility

| Runtime | io_uring Support | Notes |
|---------|-----------------|-------|
| Docker >= 20.10.2 | Yes | Default seccomp profile allows io_uring |
| Docker < 20.10.2 | No | Blocked by seccomp; add `io_uring_setup` to allowlist |
| Podman (rootful) | Yes | No seccomp restriction on io_uring |
| Podman (rootless) | Partial | SQPOLL needs `CAP_SYS_NICE` (add via `--cap-add`) |
| gVisor | No | io_uring not implemented in user-space kernel |
| Kata Containers | Yes | Full VM, native kernel access |

## Recommendations

1. **Minimum viable deployment**: Linux 5.15 LTS gives file I/O acceleration
   (the primary win for rsync workloads). This covers the majority of the
   performance benefit.

2. **Full feature deployment**: Linux 6.1+ LTS for SEND_ZC and PBUF_RING.
   Only relevant for daemon-mode transfers over high-bandwidth links where
   the CPU copy into socket buffers is the bottleneck.

3. **SQPOLL**: Only beneficial for sustained high-IOPS workloads (many small
   files). Requires `CAP_SYS_NICE` which is often unavailable in containers.
   The syscall savings from SQPOLL are marginal compared to the batch
   submission model that regular io_uring already provides. Operators in
   rootless containers and Kubernetes Pods that cannot grant `CAP_SYS_NICE`
   should pass `--no-io-uring-sqpoll` for a deterministic opt-out (keeps
   io_uring on, suppresses only `IORING_SETUP_SQPOLL`). See
   [container-io-uring.md](container-io-uring.md) and
   [kubernetes.md](kubernetes.md) for the deployment recipes.

4. **Verify before deploying**: Always run `oc-rsync --io-uring-status` on the
   target system to confirm which features are actually available after
   accounting for kernel version, seccomp policy, and resource limits.
