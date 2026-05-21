# IUS-2: `IORING_OP_SEND_ZC` kernel-compatibility matrix

Audit for the IUS chain captured in
[`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md). Inputs to
this doc are IUS-1 (PR #4661, merged) which surfaced the build-time
dependency of the `iouring-send-zc` cargo feature in README and man page.
Outputs feed IUS-3 (bench plan) and IUS-4 (the default-on / opt-in
decision).

Research-only. No source changes. No cargo touches.

## Table of contents

1. [Kernel-floor analysis](#1-kernel-floor-analysis)
2. [Runtime probe story](#2-runtime-probe-story)
3. [Behaviour caveats and current code path](#3-behaviour-caveats-and-current-code-path)
4. [Workload-sensitivity preview](#4-workload-sensitivity-preview)
5. [Recommendation matrix](#5-recommendation-matrix)
6. [Top-level recommendation](#6-top-level-recommendation)

## 1. Kernel-floor analysis

### 1.1 When did `IORING_OP_SEND_ZC` land?

`IORING_OP_SEND_ZC` was merged into the io_uring tree in the **5.20 merge
window** by Pavel Begunkov (commit `b48c312be05e8` `io_uring: add
IORING_OP_SEND_ZC`). The 5.20 release was renamed to **6.0** before
shipping, so the first stable kernel exposing `IORING_OP_SEND_ZC` is
**Linux 6.0** (2022-10-02).

The README / man-page wording landed in IUS-1 (PR #4661) cites "Linux
5.16+". That figure traces back to the related but distinct opcode
`IORING_OP_SENDMSG_ZC` and to the earlier non-io_uring `MSG_ZEROCOPY`
socket flag (Linux 4.14+). For the specific opcode the in-tree socket
writer dispatches in
[`crates/fast_io/src/io_uring/send_zc.rs:1`](../../crates/fast_io/src/io_uring/send_zc.rs),
the floor is **6.0**, which matches the doc-block at the top of that file
("Linux 6.0+") and section 2 of
[`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md). IUS-1
text should be revisited in IUS-4 when the default-on decision lands.

### 1.2 Notable bugfixes / behaviour changes 6.0 -> 6.x

| Kernel | Change |
|--------|--------|
| 6.0 | `IORING_OP_SEND_ZC` lands. Two-CQE completion model (transfer + notification) becomes part of the io_uring ABI. |
| 6.1 | `IORING_OP_SENDMSG_ZC` lands as the `sendmsg`-shaped sibling. Stable kernel; first LTS to ship SEND_ZC. |
| 6.2 | `IORING_RECVSEND_FIXED_BUF` for SEND_ZC against registered buffers solidifies; behaviour matches the `RegisteredBufferGroup` path in [`send_zc.rs:282`](../../crates/fast_io/src/io_uring/send_zc.rs). |
| 6.6 LTS | Stabilisation. No behavioural change but the long-term-support window starts here, so most production fleets that adopt SEND_ZC will land on 6.6 first. |
| 6.10 | `IORING_SETUP_SINGLE_ISSUER` interactions tightened; affects multi-thread issuers, not the single-threaded socket writer in [`socket_writer.rs:91`](../../crates/fast_io/src/io_uring/socket_writer.rs). |

There are no known correctness regressions in the 6.0 -> 6.6 series for
SEND_ZC against TCP / loopback. The only behaviour change material to
this audit is the page-pin accounting in 6.2 which the
`RegisteredBufferGroup` path relies on; pre-6.2 kernels still post the
notification CQE correctly, they just lack the registered-buffer fast
path.

### 1.3 Distro x default-kernel x SEND_ZC matrix

Default kernel = the GA / point-zero stock kernel for the distro version.
Many distros offer HWE / backport kernels (Ubuntu HWE, Debian
`backports`, RHEL `kernel-mainline`); those are noted in the "Notes"
column but do not change the default verdict.

| Distro | Version | Default kernel | SEND_ZC (>= 6.0) | Notes |
|--------|---------|----------------|-----------------|-------|
| Ubuntu LTS | 20.04 (Focal) | 5.4 | **No** | HWE rolls forward (5.15 on latest point release); still < 6.0. |
| Ubuntu LTS | 22.04 (Jammy) | 5.15 | **No** | HWE 6.8 (jammy-hwe-6.8) yes; default no. |
| Ubuntu LTS | 24.04 (Noble) | 6.8 | **Yes** | First Ubuntu LTS with SEND_ZC out of the box. |
| Ubuntu interim | 24.10 (Oracular) | 6.11 | **Yes** | |
| Debian | 11 (oldstable, Bullseye) | 5.10 | **No** | Backports kernel currently 6.1; opt-in only. |
| Debian | 12 (stable, Bookworm) | 6.1 | **Yes** | First stable Debian with SEND_ZC. |
| Debian | 13 (Trixie, freeze) | 6.10 | **Yes** | |
| RHEL / Alma / Rocky | 8.x | 4.18 (RHEL backport) | **No** | Far below 6.0; no SEND_ZC. |
| RHEL / Alma / Rocky | 9.x | 5.14 (RHEL backport) | **No** | 5.14-based; SEND_ZC not backported. |
| RHEL / Alma / Rocky | 10 (planned) | 6.12 (expected) | **Yes** (expected) | Not yet GA; ETA mid-2025. |
| Amazon Linux | 2 | 5.10 (kernel-5.10) | **No** | Default `kernel` package; `kernel-5.15` available, still < 6.0. |
| Amazon Linux | 2023 | 6.1 | **Yes** | First AL with SEND_ZC. |
| SUSE SLES | 15 SP5 | 5.14 | **No** | |
| SUSE SLES | 15 SP6 | 6.4 | **Yes** | |
| openSUSE Leap | 15.6 | 6.4 | **Yes** | Tracks SLES 15 SP6. |
| Container hosts | RHCOS 4.14 (OpenShift) | 5.14 | **No** | Tracks RHEL 9. |
| Container hosts | Bottlerocket 1.20+ | 6.1 | **Yes** | AWS / EKS-targeted. |

#### Production-fleet reality check

The distros most commonly deployed in production - **RHEL 8, RHEL 9,
Ubuntu 20.04, Ubuntu 22.04, Amazon Linux 2, SLES 15 SP5** - all ship
default kernels **below 6.0**. SEND_ZC dispatch on these hosts is
unreachable regardless of the cargo feature flag; the runtime probe at
[`send_zc::is_supported`](../../crates/fast_io/src/io_uring/send_zc.rs#L77)
correctly returns `false`.

The cohort that **does** see SEND_ZC today: Ubuntu 24.04, Debian 12,
Amazon Linux 2023, Bottlerocket, SLES 15 SP6, openSUSE Leap 15.6. A
safe IUS-4 heuristic: assume the median oc-rsync user in 2026 is still
on a kernel below 6.0; the trend reverses in the 2027-2028 window once
RHEL 10 + Ubuntu 26.04 supersede their LTS predecessors.

## 2. Runtime probe story

### 2.1 Existing probe analog: `openat2_supported`

The SEC-1.d precedent is
[`crates/fast_io/src/linux_capabilities.rs:38`](../../crates/fast_io/src/linux_capabilities.rs#L38),
which exposes `openat2_supported() -> bool`. It issues a no-op
`openat2(AT_FDCWD, ".", &how)` syscall, distinguishes `ENOSYS` (kernel
too old or seccomp-blocked) from "kernel reached, ABI present", and
caches the verdict in a process-wide `OnceLock`.

### 2.2 SEND_ZC equivalent: already exists

The SEND_ZC primitive ships with the same shape:
[`crates/fast_io/src/io_uring/send_zc.rs:77`](../../crates/fast_io/src/io_uring/send_zc.rs#L77)
exposes `is_supported() -> bool`. The implementation at lines 97-106:

```rust
fn probe_send_zc() -> bool {
    let Ok(ring) = RawIoUring::new(4) else {
        return false;
    };
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        return false;
    }
    probe.is_supported(opcode::SendZc::CODE)
}
```

This uses `IORING_REGISTER_PROBE` (the upstream-blessed interrogation
path), is cached in a 3-state `AtomicI8` (`0` = unprobed, `1` =
supported, `-1` = unsupported), and is consulted at writer construction
time at
[`socket_writer.rs:66`](../../crates/fast_io/src/io_uring/socket_writer.rs#L66):

```rust
let send_zc_active = config.allow_send_zc() && send_zc::is_supported();
```

Equivalent to the openat2 helper in shape and semantics. The probe is
preferred over `uname()` floor checks because distros backport features
and container runtimes lie about kernel versions; this matches the
documented rationale on
[`send_zc.rs:71`](../../crates/fast_io/src/io_uring/send_zc.rs#L71) and
the precedent for the existing io_uring opcode probe at
[`config.rs:275`](../../crates/fast_io/src/io_uring/config.rs#L275)
(`count_supported_ops`).

### 2.3 `liburing::io_uring_op_supported`

`liburing` exposes a convenience helper `io_uring_opcode_supported(p,
op)` (header `liburing.h`, implemented in `src/register.c`). It is a thin
wrapper around the same `IORING_REGISTER_PROBE` ioctl this audit
references. The Rust `io_uring` crate exposes the same primitive via
`Probe::is_supported(opcode::SendZc::CODE)` which
[`send_zc.rs:105`](../../crates/fast_io/src/io_uring/send_zc.rs#L105)
already calls. We do not need to FFI into `liburing`; the in-tree
helper is sufficient and stays in the existing "use safe wrapper" lane.

### 2.4 Probe gating recommendation for IUS-5

The probe is already in place. IUS-5 (the implementation step) does
**not** need to add a new probe; it needs to decide what to wire the
existing probe into. Recommendation:

- **Keep the cargo feature gate.** The `iouring-send-zc` feature gates
  the `ZeroCopySender` higher-level wrapper at
  [`send_zc.rs:282`](../../crates/fast_io/src/io_uring/send_zc.rs#L282)
  and the registered-buffer pool. The feature provides the buffer
  lifecycle primitive; it is the build-time half of the contract.
- **Also gate dispatch on the runtime probe.** The hot path at
  [`socket_writer.rs:91-104`](../../crates/fast_io/src/io_uring/socket_writer.rs#L91)
  already does this:
  `if self.send_zc_active && data.len() >= SEND_ZC_MIN_BYTES`. The
  `send_zc_active` field is the AND of policy + probe, resolved once at
  construction so the hot path never re-probes.
- **Two-stage probe is the right shape.** Static policy
  (`ZeroCopyPolicy::Enabled` via `allow_send_zc()` at
  [`io_uring_common.rs:183`](../../crates/fast_io/src/io_uring_common.rs#L183))
  first, kernel probe second. `Auto` policy should be extended (in IUS-5)
  to mean "probe and use if available"; today it means "do not use" per
  the `matches!(policy, Enabled)` check in `allow_send_zc()`.

Net: IUS-5 wires `ZeroCopyPolicy::Auto -> allow_send_zc() = true`
**without** changing the runtime probe. The probe is already correct
and already cached.

## 3. Behaviour caveats and current code path

### 3.1 Two-stage CQE contract

`IORING_OP_SEND_ZC` posts two CQEs per SQE, not one:

1. **Transfer CQE** - `IORING_CQE_F_MORE` (`1 << 1`) set in `flags`,
   `result` carries the byte count or `-errno`.
2. **Notification CQE** - `IORING_CQE_F_NOTIF` (`1 << 3`) set in
   `flags`, `result` unused. Signals "kernel has released its reference
   to the user pages."

Documented in detail at
[`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md) section
2 and at the top of
[`crates/fast_io/src/io_uring/send_zc.rs:1`](../../crates/fast_io/src/io_uring/send_zc.rs).

The flag constants live as private `const` at
[`send_zc.rs:50`](../../crates/fast_io/src/io_uring/send_zc.rs#L50) and
[`send_zc.rs:53`](../../crates/fast_io/src/io_uring/send_zc.rs#L53)
respectively, matching the upstream `include/uapi/linux/io_uring.h`
values.

### 3.2 Buffer-lifetime contract

The buffer passed to a SEND_ZC SQE must remain valid and unmodified
until the notification CQE arrives. The current implementation upholds
this by **draining both CQEs before returning**, exposing SEND_ZC as a
synchronous primitive to callers:

- `try_send_zc` at
  [`send_zc.rs:130`](../../crates/fast_io/src/io_uring/send_zc.rs#L130)
  has an explicit wait loop at
  [`send_zc.rs:167-190`](../../crates/fast_io/src/io_uring/send_zc.rs#L167)
  that does not return until `transfer_result.is_some() &&
  saw_notification`.
- The classification helper at
  [`send_zc.rs:206-225`](../../crates/fast_io/src/io_uring/send_zc.rs#L206)
  also synthesises a missing notification when `IORING_CQE_F_MORE` is
  cleared on a transfer error (e.g., `-EBADF` before any data is
  queued), so the wait loop terminates instead of hanging.

The unit tests at
[`send_zc.rs:484-511`](../../crates/fast_io/src/io_uring/send_zc.rs#L484)
cover the three classification arms: pure notification, transfer with
`F_MORE`, and the error path without `F_MORE`. The loopback round-trip
test at
[`send_zc.rs:528-579`](../../crates/fast_io/src/io_uring/send_zc.rs#L528)
runs a 64 KiB SEND_ZC end-to-end and verifies both CQEs are observed
(the call would not return otherwise).

### 3.3 Per-writer wiring (the production caller)

`IoUringSocketWriter` is the production caller. The integration is
correct but conservative:

- Probe resolved once at construction:
  [`socket_writer.rs:66`](../../crates/fast_io/src/io_uring/socket_writer.rs#L66).
- Hot path at
  [`socket_writer.rs:88-114`](../../crates/fast_io/src/io_uring/socket_writer.rs#L88)
  dispatches SEND_ZC only when payload >= `SEND_ZC_MIN_BYTES`
  (16 KiB without the cargo feature, 4 KiB with it; see
  [`socket_writer.rs:25-28`](../../crates/fast_io/src/io_uring/socket_writer.rs#L25)).
- Falls back to plain `submit_send_batch` on
  `io::ErrorKind::Unsupported` and turns off the per-writer flag at
  [`socket_writer.rs:100`](../../crates/fast_io/src/io_uring/socket_writer.rs#L100)
  so subsequent flushes skip the futile syscall.
- The flush loop at
  [`socket_writer.rs:117-144`](../../crates/fast_io/src/io_uring/socket_writer.rs#L117)
  rebuilds the slice from `as_ptr()` on each iteration; the SAFETY
  comment at
  [`socket_writer.rs:125-137`](../../crates/fast_io/src/io_uring/socket_writer.rs#L125)
  explicitly cites the "wait for notification CQE before returning"
  invariant.

### 3.4 `ZeroCopySender` wrapper (feature-gated)

The higher-level wrapper exposed under `iouring-send-zc` at
[`send_zc.rs:282`](../../crates/fast_io/src/io_uring/send_zc.rs#L282)
adds a registered-buffer pool of 8 slots x 256 KiB
([`send_zc.rs:244-255`](../../crates/fast_io/src/io_uring/send_zc.rs#L244)).
The fast path at
[`send_zc.rs:394-441`](../../crates/fast_io/src/io_uring/send_zc.rs#L394)
copies caller bytes into a pinned slot once, then submits SEND_ZC
against the registered memory so the kernel can DMA without another
userland touch.

The `try_send_zc` wait loop is reused, so the pool slot is never reused
while a kernel page reference is outstanding. The SAFETY comment at
[`send_zc.rs:411-414`](../../crates/fast_io/src/io_uring/send_zc.rs#L411)
makes this explicit: "the previous send has already drained both CQEs
(try_send_zc is synchronous) before we returned to the caller."

### 3.5 Will IUS-3 surface bugs?

The two-CQE handling looks correct in the cases the unit tests cover.
IUS-3 bench scope guards (not correctness bugs in the single-sender
case):

- **Submission-queue full mid-batch.** Error at
  [`send_zc.rs:161`](../../crates/fast_io/src/io_uring/send_zc.rs#L161);
  confirm `submit_send` falls back to batched SEND cleanly rather than
  failing the whole flush.
- **Concurrent senders sharing the ring.** The wait loop at
  [`send_zc.rs:167-190`](../../crates/fast_io/src/io_uring/send_zc.rs#L167)
  drops foreign CQEs at line 175-179 via `continue`; the `user_data`
  mask at line 61 keeps single-sender cases straight, but multi-sender
  use needs a demuxing layer. IUS-3 should keep bench fixtures
  single-sender.
- **EAGAIN / EWOULDBLOCK.** Not retried; propagated from the transfer
  CQE. Bench fixtures should use blocking sockets.

## 4. Workload-sensitivity preview

Bench shapes worth measuring in IUS-3 (informational; not commitments):

### Where SEND_ZC is expected to win

- **Large files over fast network.** >= 1 MiB payloads on a >= 10 GbE NIC
  saturate the host page-cache + memcpy cost; SEND_ZC removes the
  per-byte copy and is documented to save 25-40% sys CPU on
  `io_uring-net` kernel benchmarks (cited in
  [`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md)
  section 5).
- **High-bandwidth daemon transfers.** Multiple concurrent
  `rsync://` push transfers from one host to another over a 25/40 GbE
  fabric; CPU savings compound across sessions.
- **Low-MTU networks where TCP send-buffer churn is high.** SEND_ZC
  avoids the per-segment copy; the win scales with packet count, not
  byte count.
- **Containerised hosts with limited memory bandwidth.** Sharing a
  single memory channel between guest and host magnifies the avoided
  copy.

### Where SEND_ZC may lose

- **Many small files.** Sub-page sends (< 4 KiB) are dominated by
  `get_user_pages_fast` page-pin overhead; this is the motivation for
  the `SEND_ZC_MIN_BYTES` floor at
  [`socket_writer.rs:25-28`](../../crates/fast_io/src/io_uring/socket_writer.rs#L25).
  Workload B in
  [`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md)
  section 5 (10 000 x 4 KiB files) is the regression guard.
- **Loopback transfers.** Loopback skips DMA; SEND_ZC's "wait for NIC"
  step collapses to "wait for the loopback consumer to drain" which is
  microseconds but adds a syscall pair per send. Wall time often does
  not move; sys CPU may improve marginally.
- **CPU-bound clients.** If the bottleneck is compression
  (`-z` / `--zstd`) or strong-checksum computation, SEND_ZC frees sys
  CPU that the client cannot use; net throughput is flat.
- **Old SSDs / cold-cache.** I/O cost dominates; SEND_ZC savings are in
  the noise.
- **Pre-6.2 kernels.** Registered-buffer fast path is unavailable; the
  fallback unregistered SEND_ZC path still wins at the socket layer but
  loses the pinned-page benefit. IUS-3 should split bench results by
  registered-buffer status (the `registered_buffers_active()` accessor
  at
  [`send_zc.rs:447`](../../crates/fast_io/src/io_uring/send_zc.rs#L447)
  exposes this).

## 5. Recommendation matrix

| Kernel bucket | Distros (representative) | Default-on safe? | Need runtime probe? | Expected user impact |
|---------------|--------------------------|------------------|---------------------|----------------------|
| **< 5.6** | RHEL 8 (4.18), Ubuntu 18.04 (4.15) | N/A - no io_uring at all | N/A | No change; whole io_uring stack disabled by [`config.rs:300-302`](../../crates/fast_io/src/io_uring/config.rs#L300) `KernelTooOld`. |
| **5.6 - 5.15** | Ubuntu 20.04 HWE (5.15), RHEL 9 (5.14), AL2 (5.10), SLES 15 SP5 (5.14) | **No** - SEND_ZC unsupported | Yes - probe returns false, falls back to `IORING_OP_SEND` | No change vs today; probe at [`send_zc.rs:97`](../../crates/fast_io/src/io_uring/send_zc.rs#L97) catches this and the writer stays on the batched SEND path. |
| **5.16 - 5.19** | (transient; few production distros) | **No** - SEND_ZC opcode missing | Yes - same as above | Same fallback. The "5.16+" wording in IUS-1 docs is a known overstatement; IUS-4 should correct it. |
| **6.0 - 6.1** | Debian 12 (6.1), AL 2023 (6.1), Ubuntu 22.04 HWE 6.x | **Yes**, but with caveats | Yes (always) - container runtimes may lie about kernel version | SEND_ZC available; unregistered-buffer path only (registered-buffer fast path stabilises in 6.2). Wins on workload A, parity-or-loss on workload B without `SEND_ZC_MIN_BYTES` floor. |
| **6.2 - 6.5** | SLES 15 SP6 (6.4), openSUSE Leap 15.6 (6.4) | **Yes** | Yes | Registered-buffer path stable; expected best CPU-savings region. |
| **6.6+ LTS** | Ubuntu 24.04 (6.8), Debian 13 (6.10), Bottlerocket, AL 2023 rolling | **Yes** | Yes | Best target; long-term-support window means deployments here will run the build for years. |

Notes:

- "Default-on safe" assumes the cargo feature `iouring-send-zc` is the
  build-time gate and `ZeroCopyPolicy::Auto` is the policy default. The
  runtime probe is mandatory in every row to handle distros that
  backport, container runtimes that lie, and seccomp profiles that
  block `io_uring_register(2)`.
- "Need runtime probe?" is always **Yes** for any bucket where
  `IoUringConfig::allow_send_zc()` could return true. Static gating on
  kernel version is unsafe per the rationale at
  [`send_zc.rs:71-73`](../../crates/fast_io/src/io_uring/send_zc.rs#L71).
- The CI runner image is currently in the 5.x bucket per
  [`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md)
  section 6 point 3; IUS-3 must bump it (or document an explicit
  skip-on-pre-6.0 path) before IUS-5 lands, otherwise the SEND_ZC code
  path has no integration coverage on any CI runner.

## 6. Top-level recommendation

**Keep `iouring-send-zc` opt-in (default-off) today. Promote to
default-on for Linux targets only after both conditions are met:**

1. **Distro kernel floor crosses 6.0.** Today, RHEL 8 / 9, Ubuntu 20.04
   / 22.04, AL2, and SLES 15 SP5 - the bulk of long-lived production
   deployments - ship default kernels below 6.0. Defaulting on costs
   them nothing (runtime probe still returns false) but it is also
   confusing: users on those hosts will see the `--zero-copy` flag
   advertise SEND_ZC and silently get plain SEND. The earliest the
   median fleet crosses 6.0 is the 2027-2028 RHEL 10 / Ubuntu 26.04
   adoption window.
2. **Runtime probe is wired into `ZeroCopyPolicy::Auto`.** The probe at
   [`send_zc::is_supported`](../../crates/fast_io/src/io_uring/send_zc.rs#L77)
   already exists. IUS-5 must extend `allow_send_zc()` at
   [`io_uring_common.rs:183`](../../crates/fast_io/src/io_uring_common.rs#L183)
   so that `Auto` returns `true` when the probe succeeds. Today `Auto`
   maps to `false`, which means flipping the cargo feature alone would
   still not enable dispatch.

Independent doc fixes feeding back into IUS-1:

- **Kernel floor wording.** README (PR #4661) and man page say "Linux
  5.16+"; the correct floor for `IORING_OP_SEND_ZC` is **6.0**. The
  `ZeroCopySender` doc-block at
  [`send_zc.rs:1`](../../crates/fast_io/src/io_uring/send_zc.rs#L1) and
  the design doc both say 6.0; the README / man page is the outlier.
  IUS-4 should land the correction in the same PR that flips the
  default (or sooner, as an IUS-1 follow-up).
- **`--zero-copy` help text.** `crates/cli/src/frontend/help.rs:151`
  advertises `io_uring SEND_ZC` unconditionally; IUS-4 should annotate
  it as build-conditional.

Outstanding IUS-3 dependencies: bench workloads A (1 GiB loopback) and
B (10 000 x 4 KiB) per
[`docs/design/iouring-send-zc.md`](../design/iouring-send-zc.md)
section 5; confirm `SEND_ZC_MIN_BYTES` floor (16 KiB / 4 KiB at
[`socket_writer.rs:25-28`](../../crates/fast_io/src/io_uring/socket_writer.rs#L25))
is correct on a 6.6 kernel.

Until the two default-on conditions land, **default-off is the right
call**: opt-in dispatch is available to operators who enable it,
documentation (IUS-1) tells them how, and the runtime probe degrades
safely on every kernel that does not support it.
