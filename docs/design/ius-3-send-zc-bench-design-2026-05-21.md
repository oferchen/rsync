# IUS-3 - `IORING_OP_SEND_ZC` vs plain `IORING_OP_SEND` bench design

Date: 2026-05-21
Scope: bench plan + harness scaffold; numbers capture is the followup
Status: scaffold ships; multi-kernel execution requires hardware
Predecessor: `docs/audits/ius-2-send-zc-kernel-compat-matrix.md` (PR #4664, merged)
Successor: IUS-4 (default-on / opt-in decision); IUS-5 (`ZeroCopyPolicy::Auto` runtime probe wiring)
Tracker: IUS-3 (#2584); IUS-4 (#2585); IUS-5 (#2586)

## 1. Bench question

**Is `IORING_OP_SEND_ZC` enough of a win to justify promoting the
`iouring-send-zc` cargo feature to default-on?**

The decision is binary: the IUS-4 default-on flip either keeps the
feature opt-in (today's state, captured in IUS-1 PR #4661 docs and the
upstream IUS-2 audit) or promotes `ZeroCopyPolicy::Auto` to consume
SEND_ZC when both the build-time feature and the runtime probe at
[`crates/fast_io/src/io_uring/send_zc.rs:77`](../../crates/fast_io/src/io_uring/send_zc.rs#L77)
agree.

This document specifies the bench harness, the kernel x workload matrix
the bench has to span, the metrics the bench reports, and the decision
criteria that feed IUS-4. The harness scaffold ships in
`crates/fast_io/benches/ius_3_send_zc_vs_send.rs`; the numbers capture
is a followup PR because the bench needs multi-kernel hardware that the
default CI runner cannot provide.

## 2. Why a dedicated bench (instead of reusing the daemon harness)

The IUS-2 audit at section 4 already enumerates the workload-sensitivity
axes informally (large-NIC win, small-file regression guard). The
`docs/design/iouring-send-zc.md` bench plan at section 5 sketches a
two-workload daemon-driven benchmark (1 GiB transfer + 10 000 x 4 KiB
storm). That plan suffices for go/no-go on the production daemon path
but is too coarse to attribute the result to `IORING_OP_SEND_ZC` vs
ambient noise (compression, checksum, basis-read, sparse-write).

IUS-3 isolates the socket-send primitive. The bench drives a TCP
loopback pair on the same host and dispatches identical payloads through
either `try_send_zc` (SEND_ZC) or `opcode::Send` (plain SEND), both
against a freshly-built `IoUring`. No daemon, no filesystem I/O, no
compression. The delta the bench reports is attributable to the
SEND_ZC vs SEND primitive only.

## 3. Kernel matrix

The IUS-2 audit table at section 1.3 enumerates the distro x default
kernel landscape. The bench-relevant rows are the kernels that
materially affect the SEND_ZC dispatch:

| Kernel | Representative distro | SEND_ZC support | Why this row |
|--------|-----------------------|-----------------|--------------|
| 5.15 | Ubuntu 22.04 (default), Debian 11 backports | **No** | Floor for "io_uring present, SEND_ZC absent". The probe at `send_zc::is_supported` must return false and the writer must stay on the batched-SEND path. |
| 5.19 | Transient (few production distros) | **Partial** | `IORING_OP_SEND_ZC` is in the 5.20 merge window, renamed to 6.0 before release; some 5.19 prerelease kernels carry the opcode but lack the notification-CQE accounting. The probe should reject these as unsupported. |
| 6.0 | Debian 12 (6.1), Amazon Linux 2023 (6.1) | **Yes** | First stable kernel exposing `IORING_OP_SEND_ZC`. Registered-buffer fast path is unavailable (lands in 6.2); bench must run the unregistered SEND_ZC path here. |
| 6.6 LTS | First long-term-support kernel with SEND_ZC | **Yes (stable)** | The realistic deployment target for the next 2-3 years. Registered-buffer pool from [`send_zc::ZeroCopySender`](../../crates/fast_io/src/io_uring/send_zc.rs#L282) is fully usable. |
| 6.12 | Ubuntu 24.10 (6.11), RHEL 10 ETA (6.12) | **Yes (mature)** | Current LTS; demonstrates the steady-state performance once `IORING_RECVSEND_FIXED_BUF` and SEND_ZC have been in-tree long enough for the corner-case bugfixes to settle. |

Two rows the bench does **not** target:

- **< 5.6** (RHEL 8 4.18, Ubuntu 18.04 4.15): io_uring itself is
  unavailable; the writer falls back to standard `write(2)`. SEND_ZC is
  irrelevant.
- **6.2 - 6.5** (SLES 15 SP6 6.4, openSUSE Leap 15.6 6.4): in the
  audit's "best CPU-savings region" but not the bench's reference
  rows. If hardware is available, run as a bonus row; not gating.

## 4. Workload matrix

| Workload | Chunk size | Calls per iter | Why |
|----------|-----------|----------------|-----|
| `small_chunks` | 16 KiB | 10 000 | Dispatch-overhead-dominated. SEND_ZC's two-CQE drain doubles the kernel/userspace round-trip count; if SEND_ZC wins here, it wins everywhere. The 16 KiB floor matches the production `SEND_ZC_MIN_BYTES` constant at [`socket_writer.rs:25-28`](../../crates/fast_io/src/io_uring/socket_writer.rs#L25). |
| `medium_chunks` | 256 KiB | 1 000 | Matches the upstream-rsync 256 KiB literal-token chunk; also the per-slot size of the [`ZeroCopySender`](../../crates/fast_io/src/io_uring/send_zc.rs#L245) registered-buffer pool. This is the production daemon shape. |
| `large_chunks` | 1 MiB | 100 | Bulk-transfer regime. SEND_ZC's per-byte savings dominate fixed CQE-drain overhead; if SEND_ZC fails to win here, the feature has no path forward. |
| `mixed` | random 4 KiB to 1 MiB | ~1 000 | Production-shape: real rsync transfers interleave control frames, literal tokens, and `MSG_DATA` chunks of varying sizes. Driven by a deterministic LCG (no `rand` workspace dep) so reruns are byte-identical. |

The mixed workload uses an LCG seeded with a constant so the size
distribution is identical across runs and the SEND vs SEND_ZC delta is
not buried in seed jitter.

## 5. Network shape

The scaffold ships with **loopback** as the default (no real network
latency). Loopback isolates the syscall/CPU cost of SEND vs SEND_ZC
without depending on a NIC, switch, or peer host. The bench binds a
`TcpListener` on `127.0.0.1`, accepts a connection, and drives the
sender/receiver pair from the same process.

Two additional network shapes are out of scope for the scaffold but
covered in a followup PR:

- **1 Gbps via `tc qdisc`** - applies `tc qdisc add dev lo root netem
  rate 1gbit` before the bench; `netem` injects synthetic bandwidth and
  latency on the loopback interface so the bench captures the "slow NIC"
  regime where SEND_ZC's reduced kernel CPU should not change wall time
  but should reduce sys CPU.
- **10 Gbps via `tc qdisc`** - same primitive at 10 gbit. Closer to the
  production daemon-on-fast-NIC regime where SEND_ZC's CPU savings
  matter most.

The followup PR adds a `scripts/bench-ius-3-tc-setup.sh` that wraps the
`tc` calls with cleanup-on-exit guards. The bench itself remains
unchanged; the script wraps `cargo bench` with `tc qdisc add` ...
`cargo bench` ... `tc qdisc del`.

## 6. Measured metrics

The criterion harness reports wall-clock throughput by default
(`Throughput::Bytes` on each bench group). The followup numbers-capture
PR layers on the additional metrics out-of-band:

| Metric | Source | Interpretation |
|--------|--------|---------------|
| Wall-clock bytes/sec | criterion `Throughput::Bytes` | First-order go/no-go. SEND_ZC wins if `send_zc_throughput / send_throughput >= 1.1` on the bench. |
| User + system CPU% | `time -v` wrapper around the bench binary | Second-order signal. SEND_ZC's documented benefit is "25-40% sys CPU reduction" (per the kernel `io_uring-net` benchmarks cited in `docs/design/iouring-send-zc.md` section 5). Captures the case where wall time does not move on loopback but sys CPU drops. |
| Syscalls per second | `strace -c -p <bench_pid>` for the steady-state window | Disambiguates SEND_ZC's two-CQE drain vs SEND's one-CQE drain. Expected: SEND_ZC submits the same number of SQEs but drains 2x CQEs per submission. |
| `copy_to_user` bytes | `/proc/<bench_pid>/io` `rchar`/`wchar` deltas on Linux 6.x | The smoking gun: SEND_ZC's whole purpose is to avoid the kernel-to-user page copy. If `wchar` does not drop for SEND_ZC vs SEND on identical workloads, the kernel is not exercising the zero-copy path. |

The bench scaffold reports only the wall-clock signal. The other three
are layered on by the numbers-capture wrapper script (followup PR);
they are out of scope for the criterion harness because criterion does
not expose per-iteration sys CPU and does not own the strace pid.

## 7. Decision criteria

The IUS-4 default-on flip is gated on **both** of the following:

1. **Throughput**: `send_zc_throughput / send_throughput >= 1.1` on
   **at least 3 of 4 workload shapes** for kernels >= 6.0.
2. **CPU**: user+system CPU% reduction >= 10% on **at least 2 workload
   shapes** for kernels >= 6.0.

The asymmetry between the two thresholds reflects the documented
SEND_ZC benefit. SEND_ZC is primarily a CPU optimisation, not a
throughput optimisation - on loopback and slow NICs the wall-clock
delta is often noise while sys CPU drops materially. The throughput
criterion rejects "free 10% headroom on the host" claims that do not
also translate into observable speedup; the CPU criterion catches the
"SEND_ZC saves CPU but wall time is flat" cells which are still wins
on a CPU-bound host.

Negative outcomes and their IUS-4 implications:

- **Throughput fails, CPU passes**: SEND_ZC is a CPU optimisation only.
  Keep opt-in; the feature ships for operators who want the CPU
  headroom on CPU-bound hosts, but the default flip is not justified.
- **CPU fails, throughput passes**: anomalous; investigate before
  flipping. Most likely cause: bench captured wall-clock noise that
  did not reproduce in CPU accounting.
- **Both fail on `small_chunks`, both pass on the others**: confirm
  the production `SEND_ZC_MIN_BYTES` threshold at
  [`socket_writer.rs:25-28`](../../crates/fast_io/src/io_uring/socket_writer.rs#L25)
  rejects small payloads correctly; the bench result should match the
  threshold's intent.
- **Both fail across the matrix**: file IUS-4 as "decision: stay
  opt-in". The feature continues to ship; the README / man-page note
  from IUS-1 (PR #4661) documents the opt-in path. No code changes.

## 8. Runtime probe vs build flag interaction

Even when the bench passes the thresholds, the `iouring-send-zc` cargo
feature remains the build-time gate. The runtime probe at
[`crates/fast_io/src/io_uring/send_zc.rs:77`](../../crates/fast_io/src/io_uring/send_zc.rs#L77)
(`is_supported`) is the kernel-time gate that handles distros where
the operator built the binary on a 6.x host and runs it on a 5.x host
(or vice versa) without rebuilding.

The IUS-2 audit at section 2.4 enumerates the gating contract:

- Build-time gate (`iouring-send-zc` cargo feature): controls whether
  `ZeroCopySender` and the registered-buffer fast path compile in.
  Default-off today; IUS-4 may flip to default-on.
- Runtime gate (`send_zc::is_supported()`): cached in a process-wide
  `AtomicI8`; one-shot `IORING_REGISTER_PROBE` against a throwaway
  ring. Returns true only on kernels that advertise the opcode.

IUS-5 wires `ZeroCopyPolicy::Auto` to consult the runtime probe. Today
`allow_send_zc()` at
[`crates/fast_io/src/io_uring_common.rs:183`](../../crates/fast_io/src/io_uring_common.rs#L183)
returns true only for `ZeroCopyPolicy::Enabled`; `Auto` collapses to
false, which means flipping the cargo feature alone today does not
enable SEND_ZC dispatch.

The bench scaffold therefore tests the dispatch primitive
(`try_send_zc`) directly, not the policy-gated entry point. The
production wiring decision is IUS-5's concern; IUS-3 produces the
evidence that informs IUS-4 (default-on decision) which in turn
unblocks IUS-5 (policy wiring).

## 9. Out-of-scope (deliberate)

The scaffold does not measure:

- **Multi-sender concurrency on a shared ring**. The IUS-2 audit at
  section 3.5 flags the `user_data` mask at
  [`send_zc.rs:61`](../../crates/fast_io/src/io_uring/send_zc.rs#L61)
  as single-sender-only; multi-sender SEND_ZC needs a CQE demuxing
  layer that does not yet exist. Bench fixtures stay single-sender.
- **Registered-buffer vs unregistered SEND_ZC**. Both paths are
  zero-copy at the socket layer; the difference is the per-page
  pinning saving. Reported by
  [`ZeroCopySender::registered_buffers_active`](../../crates/fast_io/src/io_uring/send_zc.rs#L447)
  but not split out by the bench. A followup can add a `[bench]`
  group for it once the registered-buffer fast path is the production
  default.
- **Non-blocking sockets / `EAGAIN` handling**. The bench uses
  blocking loopback sockets; SEND_ZC's `EAGAIN` propagation is not
  exercised. Production daemon sockets are blocking; this matches the
  shape that matters.
- **Real-NIC bench between two hosts**. Captured in the
  `docs/design/iouring-send-zc.md` section 5 plan as workload C
  ("recorded but not gating"). The numbers-capture followup may add
  this if hardware is available; not required for the IUS-4 decision.

## 10. Followups

Tracked separately so this PR stays scoped to the design + scaffold:

- **Numbers capture on real hardware** (multi-kernel rsync-profile
  container fleet on Linux 6.0 / 6.6 / 6.12). Requires bare-metal or
  multi-VM hosts. Gated on hardware availability; not blocking IUS-4.
- **`tc qdisc` bandwidth-shape wrapper script**
  (`scripts/bench-ius-3-tc-setup.sh`). One-off operator script; does
  not change the bench source.
- **Registered-buffer split** (IUS-3b). Add a bench group that
  enforces `registered_buffers_active() == true` and reports the
  pinned-page benefit separately from the socket-layer benefit.
- **IUS-4** (default-on / opt-in decision). Consumes the bench
  numbers from this followup; updates the IUS-1 README / man-page
  wording and (if the decision is to flip) updates
  `crates/fast_io/Cargo.toml` to add `iouring-send-zc` to `default`
  on Linux targets.
- **IUS-5** (runtime probe wiring into `ZeroCopyPolicy::Auto`). Wires
  `allow_send_zc()` to consult `send_zc::is_supported()` so flipping
  the cargo feature also flips the default policy.

## 11. References

- Audit: `docs/audits/ius-2-send-zc-kernel-compat-matrix.md` (PR #4664, merged)
- IUS-1 README + man-page note: PR #4661 (merged)
- Design: `docs/design/iouring-send-zc.md`
- Runtime probe: `crates/fast_io/src/io_uring/send_zc.rs:77` (`is_supported()`)
- Dispatch primitive: `crates/fast_io/src/io_uring/send_zc.rs:130` (`try_send_zc()`)
- Production caller: `crates/fast_io/src/io_uring/socket_writer.rs:91-104`
- Policy resolution: `crates/fast_io/src/io_uring_common.rs:183` (`allow_send_zc()`)
- Project memory: `project_iouring_send_zc_optin_only.md`
