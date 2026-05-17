# IORING_OP_SEND_ZC for network zero-copy on kernel >= 6.0

Tracking issue: #1832. Status: design, implementation deferred.
Companion to the borrowed-slice audit (#4218,
`docs/design/iouring-borrowed-slice-consumer.md`) and the daemon TCP
socket I/O work tracked at #1876
(`docs/design/iouring-daemon-tcp.md`).

This document specifies how the io_uring socket writer in
`crates/fast_io/src/io_uring/socket_writer.rs` will, in a future
patch, graduate from `IORING_OP_SEND` to `IORING_OP_SEND_ZC` on
kernels that support it without regressing throughput, latency, or
buffer safety on older kernels - and recommends the order in which
that work should be sequenced against #1876, #4217, #4218, #4220, and
#2243.

## 1. Current network send path

Today, none of the daemon, transfer, or rsync_io crates submit
network bytes through io_uring. The dispatch fans out into three
flavours.

### 1.1 Daemon TCP socket (`std::net::TcpStream`)

- Accepted connections are owned as `std::net::TcpStream` and threaded
  through every daemon section
  (`crates/daemon/src/daemon/sections/module_access/transfer.rs:108`-`crates/daemon/src/daemon/sections/module_access/transfer.rs:109`),
  with a per-transfer entry point that takes `read_stream` and
  `write_stream` halves.
- The legacy session greeting and module dispatch loop writes are
  blocking `write_all` calls on the same `TcpStream`, with an
  optional bandwidth-limiter chunker
  (`crates/daemon/src/daemon/sections/session_runtime.rs:176`-`crates/daemon/src/daemon/sections/session_runtime.rs:193`).
- The daemon multiplex `Write` wrapper that wraps payload bytes in
  `MSG_DATA` frames is a thin shim over the underlying stream
  (`crates/daemon/src/daemon/multiplex_stream.rs:142`-`crates/daemon/src/daemon/multiplex_stream.rs:177`).
  It calls `protocol::send_msg` per write, which ultimately reduces
  to `Write::write_all` on the inner `TcpStream`.

### 1.2 Transfer multiplex writer (buffered + `MSG_DATA`)

- `crates/transfer/src/writer/multiplex.rs:42`-`crates/transfer/src/writer/multiplex.rs:60`
  constructs a 64 KiB `MultiplexWriter` and flushes by handing the
  buffer to `protocol::send_msg` at
  `crates/transfer/src/writer/multiplex.rs:54`-`crates/transfer/src/writer/multiplex.rs:56`.
- The bulk-data fast path bypasses the internal buffer when a single
  write exceeds the buffer size and forwards the caller slice
  directly to `protocol::send_msg`
  (`crates/transfer/src/writer/multiplex.rs:104`-`crates/transfer/src/writer/multiplex.rs:108`).
- The vectored-write path emits a header `write_all` followed by per
  slice `write_all` calls on the inner writer
  (`crates/transfer/src/writer/multiplex.rs:159`-`crates/transfer/src/writer/multiplex.rs:162`),
  again ending in a blocking `Write::write_all` on the underlying
  socket.

### 1.3 Protocol envelope (multiplex framing)

- `protocol::send_msg` is the only frame-emitter on the wire side
  (`crates/protocol/src/multiplex/io/send.rs:16`-`crates/protocol/src/multiplex/io/send.rs:22`).
  For non-empty payloads it dispatches through
  `write_all_vectored` at
  `crates/protocol/src/multiplex/io/send.rs:130`, which prefers a
  two-slice `writev` and falls back to sequential `write` on
  `Unsupported` / `InvalidInput`
  (`crates/protocol/src/multiplex/io/send.rs:255`-`crates/protocol/src/multiplex/io/send.rs:304`).

### 1.4 SSH stdio path

- The SSH transport in `rsync_io` uses `ChildStdin` for the writer
  half
  (`crates/rsync_io/src/ssh/connection.rs:234`-`crates/rsync_io/src/ssh/connection.rs:246`).
  This is pipe I/O, not socket I/O - `IORING_OP_SEND_ZC` is socket
  only (`getsockopt`-style preconditions), so the SSH path is out of
  scope for #1832 and stays on the existing `Write::write` path.

### 1.5 The dormant io_uring socket writer

`crates/fast_io/src/io_uring/socket_writer.rs` already defines an
`IoUringSocketWriter` documented as a `IORING_OP_SEND` writer
(`crates/fast_io/src/io_uring/socket_writer.rs:1`,
`crates/fast_io/src/io_uring/socket_writer.rs:11`-`crates/fast_io/src/io_uring/socket_writer.rs:22`)
with a `#[allow(dead_code)] allow_send_zc` field plumbed in at
`crates/fast_io/src/io_uring/socket_writer.rs:31`-`crates/fast_io/src/io_uring/socket_writer.rs:32`
and populated from `IoUringConfig::allow_send_zc()` at
`crates/fast_io/src/io_uring/socket_writer.rs:52`. The flush path
calls `submit_send_batch` at
`crates/fast_io/src/io_uring/socket_writer.rs:65`-`crates/fast_io/src/io_uring/socket_writer.rs:72`,
the big-write fast path bypasses the internal buffer and forwards
the caller slice at
`crates/fast_io/src/io_uring/socket_writer.rs:100`-`crates/fast_io/src/io_uring/socket_writer.rs:110`,
and `submit_send_batch` itself pushes `opcode::Send` SQEs at
`crates/fast_io/src/io_uring/batching.rs:314`-`crates/fast_io/src/io_uring/batching.rs:320`
gated by a `POLLOUT` linked-timeout poll at
`crates/fast_io/src/io_uring/batching.rs:155`-`crates/fast_io/src/io_uring/batching.rs:253`
(the #1872 fix). Configuration policy lives on
`IoUringConfig::zero_copy_policy` at
`crates/fast_io/src/io_uring_common.rs:108`-`crates/fast_io/src/io_uring_common.rs:109`,
and the accessor `allow_send_zc()` at
`crates/fast_io/src/io_uring_common.rs:170`-`crates/fast_io/src/io_uring_common.rs:173`
returns `true` only for `ZeroCopyPolicy::Enabled`. No first-party
caller in `crates/daemon/`, `crates/transfer/`, or
`crates/rsync_io/` constructs an `IoUringSocketWriter` today; that
wiring is the explicit subject of #1876.

The contract today: every byte the caller hands to the daemon socket
is copied once into the `MultiplexWriter` buffer
(`crates/transfer/src/writer/multiplex.rs:110`), then the kernel
copies it again from that buffer into socket send buffers when
`send(2)` is processed. `IORING_OP_SEND_ZC` is about removing the
second copy.

## 2. IORING_OP_SEND_ZC semantics

`IORING_OP_SEND_ZC` is a zero-copy socket send opcode added in Linux
6.0. The kernel does not memcpy the user buffer into socket memory;
instead it pins the user pages with `get_user_pages_fast` and hands
the page references to the network stack. The pages stay pinned
until the NIC, loopback peer, or local socket consumer signals
completion.

The completion model is the load-bearing change. A regular
`IORING_OP_SEND` posts one CQE per SQE: `cqe.result()` is the byte
count or `-errno`. `IORING_OP_SEND_ZC` posts **two CQEs** per SQE:

1. **Transfer CQE** - posted as soon as the data is queued for
   transmit. `IORING_CQE_F_MORE` is set in `flags()` to signal
   "more CQEs with this `user_data` will follow", and `result()`
   carries the byte count (or `-errno`) exactly like a regular SEND.
2. **Notification CQE** - posted once the kernel has released its
   reference to the user pages: the NIC has DMA'd the bytes, the
   loopback peer has consumed them, or the send was cancelled.
   `IORING_CQE_F_NOTIF` is set; `result()` is unused.

The two CQEs share `user_data`, and the writer must demux them by
inspecting `cqe.flags()`. The latency from submission to the
notification CQE is workload-dependent: for loopback / `AF_UNIX`
it is essentially the same as the transfer CQE, but for a NIC under
backpressure it lags by the TCP send-buffer drain time plus the
DMA-complete interrupt. The kernel test suite documents single-digit
microseconds for warm-cache loopback and tens of microseconds for
saturated 10 GbE.

This doubles CQE pressure. The dedicated socket-writer ring at
`crates/fast_io/src/io_uring/socket_writer.rs:41` is built from
`IoUringConfig::build_ring()`
(`crates/fast_io/src/io_uring/config.rs:313`-`crates/fast_io/src/io_uring/config.rs:340`)
and inherits the io_uring crate's default 2x CQ-over-SQ ratio. With
`IoUringConfig::default().sq_entries == 64`
(`crates/fast_io/src/io_uring_common.rs:115`) that gives a 128-entry
CQ ring, sufficient for SEND_ZC as long as the writer caps in-flight
SEND_ZC SQEs at `sq_entries / 2`.

## 3. Buffer-ownership model

The single most important rule: **the buffer passed to a SEND_ZC SQE
must remain valid and unmodified until the notification CQE
(`IORING_CQE_F_NOTIF`) arrives.** Modifying it before the
notification races the kernel's DMA / page handling and is undefined
behaviour at the kernel level.

This breaks two assumptions baked into the current writer:

1. The internal buffer is reused immediately. After `flush_buffer`
   returns, `Write::write` at
   `crates/fast_io/src/io_uring/socket_writer.rs:113`-`crates/fast_io/src/io_uring/socket_writer.rs:115`
   copies the next caller payload into the same `Vec<u8>` storage.
   Under `IORING_OP_SEND` this is fine because the kernel has
   already copied the bytes. Under `IORING_OP_SEND_ZC` the
   notification CQE may not have arrived yet; reusing the slot would
   corrupt the in-flight send.
2. The big-write fast path at
   `crates/fast_io/src/io_uring/socket_writer.rs:100`-`crates/fast_io/src/io_uring/socket_writer.rs:110`
   borrows the caller's slice. Under SEND_ZC the writer must not
   return from `Write::write` until both CQEs have been observed
   for that submission, otherwise the caller is free to drop or
   mutate the slice while pages are still pinned.

### 3.1 Registered buffers as a natural fit

The borrowed-slice audit
(`docs/design/iouring-borrowed-slice-consumer.md`) and the
registered-buffer machinery in
`crates/fast_io/src/io_uring/registered_buffers.rs:105` give us the
right primitive. A `RegisteredBufferGroup::checkout()` returns a
`RegisteredBufferSlot<'_>` (declared at
`crates/fast_io/src/io_uring/registered_buffers.rs:138` with the
checkout API at
`crates/fast_io/src/io_uring/registered_buffers.rs:385`) whose
lifetime is bounded by the group; the slot is pinned for the
duration of any in-flight SQE referencing it. Bumping a pin counter
on submission and decrementing it only on the notification CQE keeps
the existing safety story intact and reuses the same Linux-pinned
memory the registered-buffer path already accounts to
`RLIMIT_MEMLOCK`.

Concretely, the writer migrates as follows:

- `IoUringSocketWriter` gains a small pool of `RegisteredBufferSlot`
  handles (depth `>= sq_entries / 2`, slot size = `buffer_size`).
- `Write::write` copies caller bytes into a checked-out slot,
  submits `IORING_OP_SEND_ZC` with that slot's pointer, and returns
  immediately. The slot is returned to the pool only when the
  notification CQE for the matching `user_data` fires.
- When the pool is exhausted, `Write::write` blocks on the next
  notification CQE - this is the natural in-band backpressure.

The pool intentionally lives next to `IoUringSocketWriter`, not
inside the existing buffer-group namespace
(`docs/audits/iouring-bgid-namespace.md`). `IORING_OP_SEND_ZC` does
not require buffer pre-registration in the buffer-group sense; the
registered-buffer infrastructure is reused only for pinned-page
accounting.

### 3.2 Pool sizing

Floor depth = `max_sqes = sq_entries / 2 = 32` on the default config
divided by an expected 8x batching factor, so depth 4 is the
minimum. Slot size mirrors `IoUringConfig::buffer_size`
(`crates/fast_io/src/io_uring_common.rs:116`). Total pinned memory
is bounded by `depth * buffer_size`: 4 x 64 KiB = 256 KiB on the
default config, 4 x 256 KiB = 1 MiB on the `for_large_files` preset
at `crates/fast_io/src/io_uring_common.rs:132`-`crates/fast_io/src/io_uring_common.rs:145`.

## 4. Kernel version gate

Two-stage probe, executed lazily at writer construction in
`IoUringSocketWriter::from_raw_fd`
(`crates/fast_io/src/io_uring/socket_writer.rs:40`-`crates/fast_io/src/io_uring/socket_writer.rs:54`):

1. **Static policy gate.** `IoUringConfig::allow_send_zc()` at
   `crates/fast_io/src/io_uring_common.rs:170`-`crates/fast_io/src/io_uring_common.rs:173`
   stays the policy filter. `ZeroCopyPolicy::Disabled` short
   circuits to `IORING_OP_SEND` with no probing.
   `ZeroCopyPolicy::Auto` becomes "probe and use if available";
   `ZeroCopyPolicy::Enabled` becomes "probe and fail loudly if the
   kernel rejects it".
2. **Kernel feature probe.** Use `IORING_REGISTER_PROBE` and check
   the `is_supported(opcode::SendZc::CODE)` bit. The mechanism is
   the same one already used to count supported opcodes in
   `count_supported_ops` at
   `crates/fast_io/src/io_uring/config.rs:246`-`crates/fast_io/src/io_uring/config.rs:253`
   - so the probe is a one-line addition next to the existing
   io_uring kernel-availability cache
   (`crates/fast_io/src/io_uring/config.rs:144`-`crates/fast_io/src/io_uring/config.rs:167`)
   and reuses the same `AtomicBool`-cache pattern.

This mirrors the runtime detection precedent: kernel-version parsing
via `uname(2)` at
`crates/fast_io/src/io_uring/config.rs:49`-`crates/fast_io/src/io_uring/config.rs:67`
sets the minimum (`MIN_KERNEL_VERSION = (5, 6)` at
`crates/fast_io/src/io_uring/config.rs:19`) and the probe pins down
opcode-level availability. For SEND_ZC the version floor rises to
(6, 0); the existing `IoUringProbeResult::KernelTooOld` variant
already carries `(major, minor)` for diagnostics
(`crates/fast_io/src/io_uring/config.rs:184`-`crates/fast_io/src/io_uring/config.rs:189`)
and the same shape extends naturally to a `SendZcUnsupported`
variant.

The kernel-feature probe is preferred over a `uname()` check:
distros backport features, container runtimes lie about kernel
version, and `uname()` parses are fragile.
`IORING_REGISTER_PROBE` is the upstream-blessed interrogation path.

The probe result is cached on `IoUringSocketWriter` as a small
`enum SendOp { Send, SendZc }` and consulted in the flush path. If
the probe returns "unsupported", the writer falls back to
`IORING_OP_SEND` and behaves exactly as today; the pin-counted pool
collapses to a single slot reused immediately, matching current
semantics.

## 5. Bench plan

Goal: prove SEND_ZC is a net win on representative oc-rsync
workloads before flipping `ZeroCopyPolicy::Auto` to opt in.

Test rig: rsync-profile container (`rust:latest`, Debian, kernel
>= 6.0). Workspace bind-mounted; both `rsync 3.4.1` and `oc-rsync`
binaries available.

Workload A - synthetic 1 GiB transfer over loopback (the target the
issue calls out):

- Generate a single 1 GiB file of pseudo-random bytes
  (`dd if=/dev/urandom of=src/big bs=1M count=1024`).
- Bring up an oc-rsync daemon on `127.0.0.1`; transfer to a sibling
  module on the same daemon (worst-case: same machine, same NIC
  loopback, where TCP-copy cost dominates).
- Measure with `hyperfine` (3 runs, warm cache):
  `time oc-rsync rsync://127.0.0.1/dst/`. Compare three
  configurations: `--no-zero-copy` (forces `IORING_OP_SEND`),
  default (`Auto`, probes SEND_ZC), `--zero-copy` (forces SEND_ZC,
  hard-fails on pre-6.0).
- Metrics: wall time, user+sys CPU, `perf stat -e
  cycles,instructions,cache-misses`. The expected signal is a 25-40%
  sys-CPU reduction on the SEND_ZC path (matching kernel-suite
  numbers for `io_uring-net`); wall time may not move on loopback
  but should improve on a real NIC.

Workload B - small-file storm (regression guard):

- 10 000 files of 4 KiB each. SEND_ZC is documented to lose to
  regular SEND for sub-page sends, so this is the path where the
  policy must not regress.
- Same daemon, same `hyperfine` driver. Acceptance: SEND_ZC path
  within 5% of SEND-only path on both wall time and sys CPU. If
  SEND_ZC loses by more than 5%, the writer must apply a payload
  threshold (initial proposal: `payload >= 16 KiB -> SEND_ZC`,
  smaller -> SEND).

Workload C - real-NIC bench (recorded but not gating):

- The same 1 GiB transfer over a 10 GbE link between two
  rsync-profile peers. Useful for the release notes; not part of
  the merge gate because the CI runner cannot reproduce it.

Acceptance gate for promoting `Auto` to consume SEND_ZC: a
measurable CPU reduction at sustained `>= 1 GiB/s` on workload A
**and** no regression on workload B. This is the same shape as the
daemon TPC benchmark gate in
`docs/design/daemon-tpc-benchmark-plan.md`.

## 6. Recommendation

**Defer until #1876 lands first**, then implement.

Reasoning:

1. None of the daemon, transfer, or rsync_io crates currently route
   network bytes through io_uring. `IoUringSocketWriter` is dead
   code outside `fast_io`'s own tests; there is no production
   caller for `IORING_OP_SEND_ZC` to graduate. The wiring that
   creates that caller is the explicit subject of #1876
   (`docs/design/iouring-daemon-tcp.md`).
2. The same buffer-lifetime rework needed for SEND_ZC (pinned
   slots, deferred-completion accounting) is the missing piece for
   #4218 (borrowed-slice audit,
   `docs/design/iouring-borrowed-slice-consumer.md`). Doing both
   in one pass amortises the audit and the pool-sizing bench.
3. The kernel-6.0 baseline is not yet a fair assumption in CI - the
   musl Linux runner image is currently 5.x. Implementing SEND_ZC
   first would land code with no integration coverage on any CI
   runner.
4. The `ZeroCopyPolicy::Enabled` opt-in surface and CLI flag
   (`--zero-copy` / `--no-zero-copy`) already exist
   (`crates/cli/src/frontend/arguments/parser/mod.rs:474`-`crates/cli/src/frontend/arguments/parser/mod.rs:479`),
   so the user-facing change is already accounted for; the only
   missing piece is the consumer path.

Concrete sequencing:

1. Land #1876 (daemon TCP through `IoUringSocketWriter`). This
   gives SEND_ZC a real caller and a real benchmark surface.
2. Land #4218 borrowed-slice resolution (pick the `Arc<[u8]>` or
   pinned-slot path). SEND_ZC reuses whichever buffer-ownership
   primitive that audit chooses.
3. Bump CI runner to kernel >= 6.0 (or document an explicit
   skip-on-pre-6.0 path for the SEND_ZC integration tests).
4. Implement SEND_ZC behind the `Auto` policy, with the threshold
   logic from section 5, gated on the
   `IORING_REGISTER_PROBE(SendZc)` cache from section 4.
5. Promote `Auto` to consume SEND_ZC after two consecutive green
   runs of the workload-A / workload-B bench from section 5 plus
   `tools/ci/run_interop.sh`.

Rejecting SEND_ZC outright is not the right call: the kernel
trajectory (Ubuntu LTS 22.04 ships 6.5, Debian 13 ships 6.1) means
the kernel >= 6.0 floor will be the common case on every long-lived
deployment within the next two release cycles, and a 25-40% sys-CPU
saving on loopback transfers is large enough to justify the work
once the prerequisites land.

## 7. Cross-references

- **#1876** Daemon TCP through io_uring socket I/O. Prerequisite.
  `docs/design/iouring-daemon-tcp.md`. SEND_ZC has no first-party
  caller until this lands; the doc explicitly defers the SEND_ZC
  migration to #1832.
- **#4218** Borrowed-slice consumer audit. Same buffer-lifetime
  shape as SEND_ZC's pinned-page contract.
  `docs/design/iouring-borrowed-slice-consumer.md`. The pin-counted
  pool described in section 3 is the buffer-ownership primitive
  that audit recommends adopting.
- **#4217** Async io_uring path. Both SEND_ZC CQEs are already
  asynchronous; the async machinery's `oneshot` waker pattern is
  exactly what the SEND_ZC notification CQE needs.
- **#4220** io_uring submission from rayon worker threads.
  Composes the same way as today: SEND_ZC SQEs are pushed from
  whatever thread owns the writer, the completion path is unchanged
  by the source thread. Per-thread rings (#2243) and SEND_ZC are
  orthogonal.
- **#2243** Per-thread io_uring rings. If this lands first, the
  SEND_ZC notification path is per-thread by construction and the
  pool sizing in section 3 becomes per-thread, not per-writer. No
  semantic change; just a scaling factor.
- **Kernel-detection precedent** for the runtime probe in
  section 4: existing io_uring availability cache at
  `crates/fast_io/src/io_uring/config.rs:144`-`crates/fast_io/src/io_uring/config.rs:167`
  and the `count_supported_ops` opcode-probe at
  `crates/fast_io/src/io_uring/config.rs:246`-`crates/fast_io/src/io_uring/config.rs:253`.
