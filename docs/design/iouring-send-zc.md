# IORING_OP_SEND_ZC for network zero-copy on kernel >= 6.0

Tracking issue: oc-rsync #1832. Status: design. Companion to the
fixed-buffer audit at `docs/audits/io-uring-fixed-buffer-audit.md` and
the daemon TCP wiring tracked at #1876.

This document specifies how the io_uring socket writer at
`crates/fast_io/src/io_uring/socket_writer.rs` will graduate from
`IORING_OP_SEND` to `IORING_OP_SEND_ZC` on kernels that support it,
without regressing throughput, latency, or buffer safety on older
kernels.

## 1. Current network send path

The writer is a `Write` adapter that batches user writes through an
internal buffer and submits them via io_uring SQEs.

- `IoUringSocketWriter` is the only socket writer; it is documented as
  a `IORING_OP_SEND` writer at
  `crates/fast_io/src/io_uring/socket_writer.rs:1` and
  `crates/fast_io/src/io_uring/socket_writer.rs:11-22`.
- The `allow_send_zc` field is plumbed in at
  `crates/fast_io/src/io_uring/socket_writer.rs:31-32` and populated
  from `IoUringConfig::allow_send_zc()` at
  `crates/fast_io/src/io_uring/socket_writer.rs:52`. It is currently
  read-only and unused (`#[allow(dead_code)]`).
- The flush path lives at
  `crates/fast_io/src/io_uring/socket_writer.rs:57-83`: it calls
  `submit_send_batch` with the active fixed-fd slot and a slice of the
  internal buffer.
- The big-write fast path at
  `crates/fast_io/src/io_uring/socket_writer.rs:100-111` bypasses the
  internal buffer entirely and forwards the caller's slice straight to
  `submit_send_batch`.
- `submit_send_batch` itself is at
  `crates/fast_io/src/io_uring/batching.rs:270-379`. It pushes
  `opcode::Send` SQEs (line 315) gated by a `POLLOUT` linked-timeout
  poll at `crates/fast_io/src/io_uring/batching.rs:155-254` (issue
  #1872 fix), and reaps one CQE per chunk at
  `crates/fast_io/src/io_uring/batching.rs:338-369`.
- Configuration policy lives in `IoUringConfig`. The `zero_copy_policy`
  field is documented at
  `crates/fast_io/src/io_uring/config.rs:354-366`. The accessor
  `allow_send_zc()` at
  `crates/fast_io/src/io_uring/config.rs:418-430` returns `true` only
  for `ZeroCopyPolicy::Enabled`; `Auto` and `Disabled` both return
  `false`. No code path consumes the `true` value yet.

The contract today: every byte the caller hands to `write()` is copied
once into the writer's internal `Vec<u8>` (line 93 / line 113), then
the kernel copies it again from that `Vec<u8>` into socket buffers
when the SEND SQE is processed. The big-write fast path skips the
first copy but still pays the kernel-side copy. `IORING_OP_SEND_ZC` is
about removing the second copy.

## 2. IORING_OP_SEND_ZC semantics

`IORING_OP_SEND_ZC` is a zero-copy socket send opcode added in Linux
6.0. The kernel does not memcpy the user buffer into socket memory;
instead it pins the user pages with `get_user_pages_fast` and hands
the page references to the network stack. The pages stay pinned until
the NIC, loopback peer, or local socket consumer signals completion.

The completion model is the load-bearing change. A regular
`IORING_OP_SEND` posts one CQE per SQE: `cqe.result()` is the byte
count or `-errno`. `IORING_OP_SEND_ZC` posts **two CQEs** per SQE:

1. **Notification CQE** (`IORING_CQE_F_MORE` set on the first CQE,
   `IORING_CQE_F_NOTIF` set on the second). The first CQE reports the
   transfer outcome as soon as the data has been queued for transmit
   - `result()` carries the byte count or `-errno` exactly like
   `IORING_OP_SEND`.
2. **Release CQE.** The second CQE fires once the kernel has released
   the user-page reference: the NIC has DMA'd the bytes, the peer has
   ack'd (for loopback / kTLS shortcut), or the send was cancelled.
   `result()` is unused; `flags()` carries `IORING_CQE_F_NOTIF`.

The two CQEs are correlated by `user_data`. The kernel preserves the
SQE's `user_data` on both completions, so the writer must distinguish
them by inspecting `cqe.flags()`:

- `flags & IORING_CQE_F_MORE` (on the first CQE only) tells the
  application "more CQEs with this user_data are coming".
- `flags & IORING_CQE_F_NOTIF` (on the second CQE only) marks the
  release.

This doubles the CQE pressure. The `sq_entries` calibration in
`IoUringConfig::default` (64) was sized assuming one CQE per SQE, so
either the ring's CQ depth must be raised or the in-flight SQE count
halved. The shared-ring path uses 2x CQ over SQ by default, which
absorbs this safely; the dedicated socket-writer ring at
`crates/fast_io/src/io_uring/socket_writer.rs:41` is built from
`IoUringConfig::build_ring()` and inherits the io_uring crate's
default 2x CQ ratio - that is sufficient as long as we keep
`max_sqes` <= `sq_entries / 2`.

## 3. Buffer lifetime contract

The single most important rule: **the buffer passed to a SEND_ZC SQE
must remain valid and unmodified until the release CQE
(`IORING_CQE_F_NOTIF`) arrives**. Modifying it before the release CQE
races the kernel's DMA / page handling and is undefined behaviour at
the kernel level.

This breaks two assumptions baked into the current writer:

1. The internal buffer is reused immediately. After
   `flush_buffer` returns, `Write::write` at
   `crates/fast_io/src/io_uring/socket_writer.rs:113-115` copies the
   next caller payload into the same `Vec<u8>` storage. Under
   `IORING_OP_SEND` this is fine because the kernel has already copied
   the bytes. Under `IORING_OP_SEND_ZC` the release CQE may not have
   arrived yet; reusing the slot would corrupt the in-flight send.
2. The big-write fast path at
   `crates/fast_io/src/io_uring/socket_writer.rs:100-111` borrows the
   caller's slice. Under SEND_ZC the writer must not return from
   `Write::write` until both CQEs have been observed for that
   submission, otherwise the caller is free to drop or mutate the
   slice while pages are still pinned.

Two contract options, with the design picking option B:

- **Option A: synchronous wait for release.** Every SEND_ZC submission
  blocks until both CQEs are observed. Correct, but serialises sends
  and erases most of the latency gain.
- **Option B: pin-counted buffer pool.** The writer owns a small pool
  of detachable buffers. A buffer is checked out at submission, the
  pin count is bumped to 1, and only returned to the pool when the
  release CQE for that user_data arrives. The internal write buffer
  becomes one slot of that pool; the big-write fast path is converted
  into "copy into a pool slot and submit".

The pool lives in `socket_writer.rs` next to `IoUringSocketWriter`,
not in `registered_buffers.rs`. Registered buffers are for fixed I/O
opcodes (`READ_FIXED` / `WRITE_FIXED`) and indexed by buffer-group ID
- `IORING_OP_SEND_ZC` does not require pre-registration. Mixing the
two namespaces would muddy the bgid pool described in
`docs/design/io-uring-bgid-namespace.md`.

Pool sizing: depth 4 is the floor (matches `max_sqes = sq_entries / 2
= 32` divided by an expected 8x batching factor). Slot size mirrors
`IoUringConfig::buffer_size`. Total pinned memory is bounded by `pool
depth * buffer_size`, e.g. 4 * 64 KiB = 256 KiB on the default
config, 4 * 256 KiB = 1 MiB on `for_large_files`. This sits below the
`RLIMIT_MEMLOCK` budget for unprivileged processes (64 KiB on stock
Linux, but io_uring registered pages are accounted differently;
we audit at probe time).

## 4. Kernel version probe and fallback

Two-stage probe, executed at writer construction in
`IoUringSocketWriter::from_raw_fd`:

1. **Static gate.** `IoUringConfig::allow_send_zc()` is the policy
   filter: `ZeroCopyPolicy::Disabled` short-circuits to
   `IORING_OP_SEND` with no probing. `ZeroCopyPolicy::Auto` becomes
   "probe and use if available" once this design ships;
   `ZeroCopyPolicy::Enabled` becomes "probe and fail loudly if the
   kernel rejects it".
2. **Kernel probe.** Submit a one-shot probe via the io_uring
   `register(IORING_REGISTER_PROBE, ...)` interface and inspect the
   `op_supported` bit for `IORING_OP_SEND_ZC` (opcode 41). The probe
   is cheap (one syscall per writer is acceptable; it can be cached
   per `IoUringConfig` if it shows up in profiles).

The probe result is cached on `IoUringSocketWriter` as an `enum
SendOp { Send, SendZc }` and consulted in the flush path. If the
probe returns "unsupported", the writer falls back to
`IORING_OP_SEND` and behaves exactly as today; the pin-counted pool
collapses to a single slot reused immediately, matching current
semantics.

The kernel probe is preferred over a uname() check: distros backport
features, container runtimes lie about kernel version, and uname()
parses are fragile. `IORING_REGISTER_PROBE` is the upstream-blessed
interrogation path.

## 5. Daemon TCP integration (#1876)

The daemon-side wiring tracked at #1876 plugs `IoUringSocketWriter`
into the accept loop in the `daemon` crate. The integration points
are:

- The daemon transport layer constructs an `IoUringSocketWriter` per
  accepted TCP connection and uses it as the `Write` half of the
  multiplex stream. SEND_ZC is wholly invisible above the `Write`
  trait - the socket writer keeps the same `impl Write` it has today
  (`crates/fast_io/src/io_uring/socket_writer.rs:86-121`).
- `IoUringConfig` for the daemon path defaults to
  `ZeroCopyPolicy::Auto` once SEND_ZC ships. The daemon's TPC
  benchmark plan (`docs/design/daemon-tpc-benchmark-plan.md`) is the
  acceptance gate: SEND_ZC must demonstrate a measurable CPU
  reduction at >= 1 GiB/s sustained or `Auto` stays on
  `IORING_OP_SEND`.
- Per-connection ring sizing is governed by the session ring pool
  (`docs/design/iouring-session-ring-pool.md`); SEND_ZC's doubled CQE
  pressure is absorbed by the existing 2x CQ-over-SQ default, but the
  pool must verify CQ headroom at ring construction.

This file documents the writer-level design only; the daemon integration
is the consumer.

## 6. Throughput vs latency trade-off

`IORING_OP_SEND_ZC` is a CPU-saving optimisation, not an unconditional
throughput optimisation. The trade-offs:

- **Wins.** Removing the kernel memcpy saves cycles proportional to
  the payload size. On 256 KiB sends from `for_large_files`, the
  saved memcpy is the dominant CPU cost; SEND_ZC has measured 30-40%
  CPU reductions in the kernel's own testing.
- **Losses.** Two CQEs per send doubles CQE-handling overhead. For
  small payloads (`<= 4 KiB`), the per-CQE overhead exceeds the saved
  memcpy and SEND_ZC is slower than SEND. The kernel documents this
  explicitly in `io_uring_enter(2)`.
- **Pinning cost.** `get_user_pages_fast` is not free; on small,
  short-lived sends the pinning amortisation never pays back.

The decision rule encoded in the writer:

- `payload_size < ZC_PAYLOAD_THRESHOLD` (initial value: 16 KiB) ->
  `IORING_OP_SEND`, regardless of `allow_send_zc()`.
- `payload_size >= ZC_PAYLOAD_THRESHOLD` and probe says supported ->
  `IORING_OP_SEND_ZC`.

The threshold is configurable via a new `IoUringConfig::send_zc_threshold`
field defaulting to 16 KiB. The `for_large_files` preset will lower
this to 8 KiB once benchmarks confirm; `for_small_files` keeps the 16
KiB floor so SEND_ZC effectively never triggers on the small-files
preset.

The pin-counted buffer pool ties the latency story together. Pool
exhaustion blocks the writer until a release CQE drains a slot, so
pool depth is the ceiling on in-flight SEND_ZC bytes per writer. Depth
4 with 64 KiB slots caps in-flight ZC traffic at 256 KiB per writer,
which is comfortable below typical `tcp_wmem` (4 MiB on stock Linux).

## Test plan

- Unit: probe path returns `SendOp::SendZc` on a simulated 6.0 kernel
  (mock `register(PROBE)` response), `SendOp::Send` on 5.x.
- Unit: pin-counted pool returns the same slot only after the release
  CQE arrives. Drive the writer with a synthetic ring that delays
  the second CQE by N submissions; verify the writer waits.
- Integration: daemon TPC benchmark (#1876 acceptance gate) shows CPU
  reduction at sustained 1 GiB/s. No regression on small-files preset.
- Interop: SEND_ZC must be transparent to the wire format. Existing
  daemon push / pull interop tests in `tools/ci/run_interop.sh` cover
  this; no protocol-level test is needed.
- Negative: `ZeroCopyPolicy::Enabled` on a 5.x kernel returns a clear
  error from `IoUringSocketWriter::from_raw_fd` instead of falling
  back silently. `Auto` must always succeed.

## Open questions

- Whether to expose `send_zc_threshold` as a public CLI tunable. The
  existing `--io-uring-policy` flag already gates the policy enum;
  adding a threshold knob risks surface bloat. Recommend keeping it
  internal until benchmarks demand otherwise.
- Whether the pin-counted pool should share a backing allocator with
  the registered-buffer pool described in
  `docs/design/iouring-adaptive-buffer-pool.md`. They have different
  lifetime models, so the bias is to keep them separate; revisit once
  both ship.
