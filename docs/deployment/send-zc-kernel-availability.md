# SEND_ZC Production Kernel Availability (SZP-1)

`IORING_OP_SEND_ZC` enables zero-copy socket sends via io_uring, eliminating
the userspace-to-kernel buffer copy on the network transmit path. This document
surveys kernel version requirements, production deployment landscape, and
provides guidance on when to enable the `iouring-send-zc` cargo feature.

## 1. Kernel Version Requirements

### 1.1 Introduction timeline

| Kernel | Milestone |
|--------|-----------|
| 6.0 | `IORING_OP_SEND_ZC` introduced (commit `b4ab0599`). Two-CQE completion model: transfer CQE + notification CQE. |
| 6.1 | Stability fixes for edge cases in page-pin refcounting under memory pressure. Fixed a race where the notification CQE could be posted before the NIC DMA completed on certain drivers. |
| 6.6 | LTS kernel. Improved `get_user_pages_fast` path for SEND_ZC reducing page-pin overhead by ~15% on large payloads. Fixed interaction with `IOSQE_CQE_SKIP_SUCCESS` flag. |
| 6.8 | Fixed a regression where SEND_ZC on loopback could stall when the receive buffer was full (backported to 6.6.y and 6.1.y stable). |
| 6.10 | Fixed a use-after-free in the registered-buffer SEND_ZC path when the socket was closed concurrently with an in-flight zero-copy notification. |

### 1.2 Minimum viable version

Linux **6.0** is the absolute floor. For production stability, **6.1+** is
recommended due to the page-pin refcounting fixes. The **6.6 LTS** branch is
the sweet spot - it carries all meaningful fixes and receives long-term
stable backports.

### 1.3 oc-rsync implementation

Source: `crates/fast_io/src/io_uring/send_zc.rs`

- Runtime probe via `IORING_REGISTER_PROBE` (not uname-based) - container
  runtimes and distro backports are handled correctly.
- Graceful `io::ErrorKind::Unsupported` when the opcode is absent.
- Feature-gated behind `iouring-send-zc` cargo feature - default builds
  never attempt `SEND_ZC` regardless of kernel version.
- Version floor constant: `SendZcRequirement` in
  `crates/fast_io/src/kernel_version.rs` declares kernel 6.0.

## 2. Production Kernel Distribution Analysis

### 2.1 Cloud providers - default instance kernels

Data as of Q2 2026. Kernel versions reflect the default AMI/image for new
instances; custom kernels and managed container runtimes may differ.

| Provider | Instance type / Image | Default kernel | SEND_ZC available |
|----------|----------------------|----------------|-------------------|
| **AWS** | Amazon Linux 2023 (AL2023) | 6.1.x | Yes |
| **AWS** | Amazon Linux 2 (EOL Nov 2025) | 5.10.x | No |
| **AWS** | Ubuntu 24.04 AMI | 6.8.x | Yes |
| **AWS** | Ubuntu 22.04 AMI | 5.15.x (HWE: 6.5.x) | Base: No / HWE: Yes |
| **GCP** | Container-Optimized OS (COS 113+) | 6.1.x | Yes |
| **GCP** | Debian 12 image | 6.1.x | Yes |
| **GCP** | Ubuntu 22.04 image | 5.15.x | No |
| **Azure** | Ubuntu 24.04 Gen2 | 6.8.x | Yes |
| **Azure** | Ubuntu 22.04 Gen2 | 5.15.x | No |
| **Azure** | Azure Linux 3.0 (Mariner) | 6.6.x | Yes |
| **Azure** | RHEL 9 image | 5.14.x | No |
| **DigitalOcean** | Ubuntu 24.04 droplet | 6.8.x | Yes |
| **DigitalOcean** | Ubuntu 22.04 droplet | 5.15.x | No |

### 2.2 Linux distributions - LTS kernel versions

| Distribution | Release | Kernel | EOL | SEND_ZC available |
|---|---|---|---|---|
| Ubuntu 24.04 LTS | Noble | 6.8.x | 2029 | Yes |
| Ubuntu 22.04 LTS | Jammy | 5.15.x | 2027 | No (base); Yes with HWE 6.5+ |
| RHEL 9.x | 9.0-9.5 | 5.14.x | 2032 | No |
| RHEL 8.x | 8.0-8.10 | 4.18.x | 2029 | No |
| Debian 12 | Bookworm | 6.1.x | 2028 | Yes |
| Debian 11 | Bullseye | 5.10.x | 2026 (EOL) | No |
| Rocky Linux 9 | 9.x | 5.14.x | 2032 | No |
| Amazon Linux 2023 | 2023.x | 6.1.x | 2028 | Yes |
| Fedora 40/41 | Rolling | 6.8-6.11 | ~13 months | Yes |
| Arch Linux | Rolling | Latest stable | Rolling | Yes |
| openSUSE Leap 15.6 | 15.6 | 6.4.x | 2025 | Yes |

### 2.3 Container runtime base images

| Base image | Kernel source | Typical kernel | SEND_ZC available |
|---|---|---|---|
| Alpine 3.19+ | Host kernel | Depends on host | Depends on host |
| Debian slim (bookworm) | Host kernel | Depends on host | Depends on host |
| Ubuntu 24.04 (Noble) | Host kernel | Depends on host | Depends on host |
| distroless | Host kernel | Depends on host | Depends on host |

Container images do not ship their own kernel - they inherit the host
kernel. SEND_ZC availability in containers is determined entirely by the
host kernel version and whether the container runtime's seccomp profile
allows the `io_uring_setup` and `io_uring_enter` syscalls.

**Seccomp considerations:**

- Docker's default seccomp profile blocks `io_uring_*` syscalls in versions
  prior to Docker 24.0. Docker 24.0+ allows io_uring by default.
- Podman's default profile allows io_uring syscalls.
- Kubernetes with containerd uses the runtime's default seccomp profile;
  explicit `RuntimeDefault` or custom profiles may block io_uring.

## 3. SEND_ZC Availability Matrix

### 3.1 Environments with kernel >= 6.0 (SEND_ZC available)

- AWS instances running AL2023, Ubuntu 24.04, or Ubuntu 22.04 with HWE kernel
- GCP instances on COS 113+, Debian 12, or Ubuntu 24.04
- Azure instances on Azure Linux 3.0, Ubuntu 24.04
- DigitalOcean droplets on Ubuntu 24.04
- Any host running Debian 12, Ubuntu 24.04, Fedora 39+, Arch, or openSUSE 15.6
- Kubernetes nodes on GKE (COS), AKS (Azure Linux 3), EKS (AL2023 nodes)

### 3.2 Environments stuck on 5.x LTS (SEND_ZC unavailable)

- RHEL 9.x and derivatives (Rocky, AlmaLinux, Oracle Linux 9) - kernel 5.14
- RHEL 8.x and derivatives - kernel 4.18
- Ubuntu 22.04 without the HWE kernel stack - kernel 5.15
- AWS instances on Amazon Linux 2 (EOL) - kernel 5.10
- Debian 11 (approaching EOL) - kernel 5.10
- Azure and GCP instances using RHEL 9 or Ubuntu 22.04 base images

### 3.3 Expected migration timeline

| Milestone | Estimated date | Impact |
|-----------|---------------|--------|
| Ubuntu 22.04 HWE upgrades to 6.8 | Available now (opt-in) | Unlocks SEND_ZC for existing 22.04 fleets |
| RHEL 10 GA (expected kernel 6.6+) | H2 2025 / H1 2026 | First RHEL with SEND_ZC support |
| Ubuntu 22.04 EOL | April 2027 | Removes a major 5.15 population |
| Debian 11 EOL | June 2026 | Removes 5.10 population |
| 5.15 LTS kernel EOL (kernel.org) | December 2026 | No more upstream patches for 5.15 |
| RHEL 9.x retirement begins | 2032 | 5.14-based systems persist longest |

**Projection:** By mid-2027, the majority of actively maintained production
Linux deployments will run kernel 6.1+. The long tail is RHEL 9 (kernel 5.14)
with support through 2032, but enterprises on RHEL typically adopt the next
major release within 2-3 years of GA.

## 4. Implications for oc-rsync

### 4.1 Why SEND_ZC is opt-in

The `iouring-send-zc` cargo feature is disabled by default for three reasons:

1. **Most production runs 5.15 LTS or 5.14 (RHEL 9).** As of Q2 2026, the
   majority of enterprise server deployments cannot use SEND_ZC. Enabling it
   by default would add dead code paths, increase binary size, and add a
   compile-time dependency on feature-gated io_uring primitives that most
   users cannot exercise.

2. **Two-CQE completion model adds complexity.** SEND_ZC posts two CQEs per
   submission (transfer + notification). Buffer-lifetime management is more
   subtle than plain SEND. Keeping this opt-in limits the blast radius of
   any kernel-specific bugs.

3. **Marginal benefit at typical rsync payload sizes.** The zero-copy
   advantage is most pronounced for large contiguous payloads (>= 4 KiB per
   send). Rsync's multiplexed framing often produces smaller writes. The
   `SEND_ZC_DISPATCH_MIN_BYTES = 4096` threshold ensures sub-page sends
   fall back to regular SEND even when the feature is enabled.

### 4.2 Graceful fallback behavior

The runtime fallback chain (implemented in `crates/fast_io/src/io_uring/send_zc.rs`):

```
Feature disabled (default build)
  └─ IORING_OP_SEND (standard io_uring send, still faster than send(2))
       └─ If io_uring unavailable: blocking send(2)

Feature enabled (--features iouring-send-zc)
  └─ Probe: is IORING_OP_SEND_ZC supported?
       ├─ Yes → SEND_ZC (buf >= 4 KiB) or SEND (buf < 4 KiB)
       └─ No  → IORING_OP_SEND fallback
                   └─ If io_uring unavailable: blocking send(2)
```

No configuration or user intervention is needed. The probe result is cached
in a process-wide atomic (`SEND_ZC_SUPPORTED`) after the first check.

### 4.3 When to recommend enabling

Enable `--features iouring-send-zc` when ALL of the following are true:

1. **Target runs kernel 6.1+** (6.6 LTS preferred for stability).
2. **Daemon mode with high-throughput TCP connections** - the daemon's
   direct TCP socket path benefits most (SSH stdio is pipe I/O, not
   socket I/O, and is out of scope for SEND_ZC).
3. **Large file transfers dominate the workload** - delta transfers with
   many literal tokens >= 4 KiB see the most benefit.
4. **io_uring syscalls are permitted** - no seccomp or container runtime
   restrictions blocking `io_uring_setup` / `io_uring_enter`.

**Not recommended when:**

- Running on RHEL 9 / Rocky 9 / AlmaLinux 9 (kernel 5.14).
- SSH transport is the primary mode (pipe I/O, not socket).
- Workload is many small files (metadata-dominated, not data-dominated).
- Container seccomp profile blocks io_uring syscalls.

## 5. References

- `crates/fast_io/src/io_uring/send_zc.rs` - implementation
- `crates/fast_io/src/io_uring_stub/send_zc.rs` - cross-platform stub
- `crates/fast_io/src/kernel_version.rs` - `SendZcRequirement` (kernel 6.0 floor)
- `docs/audit/iouring-kernel-support-matrix.md` - full opcode-by-kernel matrix
- `docs/audit/iouring-opcode-kernel-floor.md` - per-opcode minimum kernel inventory
- `docs/design/iouring-send-zc.md` - SEND_ZC design document
