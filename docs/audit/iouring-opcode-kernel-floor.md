# io_uring opcode inventory by minimum-kernel floor (IKV-1)

Scope: every `IORING_OP_*` opcode dispatched by code under
`crates/fast_io/src/`. Comment-only references (doc strings, module
narratives) are excluded - only call sites that build an SQE or probe a
register opcode count. Kernel floors come from the upstream UAPI header
`include/uapi/linux/io_uring.h` and the per-opcode commits in
`io_uring/opcode.c` (kernel `io-uring.git`).

The constants enumerated in `crates/fast_io/src/io_uring_common.rs`
(`LINKAT_MIN_KERNEL`, `STATX_MIN_KERNEL`, `ASYNC_CANCEL_MIN_KERNEL`,
`ASYNC_CANCEL_FD_MIN_KERNEL`) and
`crates/fast_io/src/io_uring/buffer_ring/registration.rs`
(`MIN_PBUF_RING_KERNEL`) are the in-tree source of truth and are repeated
in the table below.

## Per-opcode inventory

| Opcode | Numeric | Min kernel | Call site (file:line) | Runtime fallback |
|---|---|---|---|---|
| `IORING_OP_NOP` | 0 | 5.1 | `crates/fast_io/src/io_uring/per_thread_ring.rs:181`, `crates/fast_io/src/io_uring/registered_buffers/tests/drop_contract.rs:31` | n/a (test scaffolding only) |
| `IORING_OP_READV` (via fixed-buffer path) | 1 | 5.1 | not dispatched directly; superseded by `READ_FIXED` / `READ` | n/a |
| `IORING_OP_WRITEV` (via fixed-buffer path) | 2 | 5.1 | not dispatched directly; superseded by `WRITE_FIXED` / `WRITE` | n/a |
| `IORING_OP_FSYNC` | 3 | 5.1 | `crates/fast_io/src/io_uring/file_writer.rs:289`, `crates/fast_io/src/io_uring/disk_batch.rs:238` | Standard `fsync(2)` via the `disk_commit` non-io_uring writer; selected when io_uring construction fails. |
| `IORING_OP_READ_FIXED` | 4 | 5.1 | `crates/fast_io/src/io_uring/registered_buffers/submit.rs:38` | Plain `IORING_OP_READ` when registered-buffer lease cannot be obtained; documented in `data_reader.rs:56` and `file_reader.rs:158`. |
| `IORING_OP_WRITE_FIXED` | 5 | 5.1 | `crates/fast_io/src/io_uring/registered_buffers/submit.rs:173` | Plain `IORING_OP_WRITE` when the bgid lease is absent; documented in `file_writer.rs:38`. |
| `IORING_OP_POLL_ADD` | 6 | 5.1 | `crates/fast_io/src/io_uring/shared_ring.rs:262`, `crates/fast_io/src/io_uring/cancel.rs:392`, `crates/fast_io/src/io_uring/cancel.rs:454`, `crates/fast_io/src/io_uring/batching.rs:189` | `shared_ring.rs:161` returns `io::ErrorKind::Unsupported` so the caller falls back to blocking writes outside io_uring. |
| `IORING_OP_ASYNC_CANCEL` | 14 | 5.5 (`ASYNC_CANCEL_MIN_KERNEL`) | `crates/fast_io/src/io_uring/cancel.rs:160` | `crates/fast_io/src/io_uring_stub/cancel.rs:40,48` returns `Unsupported`; cancel becomes a no-op. |
| `IORING_OP_ASYNC_CANCEL` (fd-targeted via `CancelBuilder::fd`) | 14 | 5.19 (`ASYNC_CANCEL_FD_MIN_KERNEL`) | `crates/fast_io/src/io_uring/cancel.rs:205` | Falls back to user-data cancel (the 5.5 form) when fd-targeted cancel is rejected by the kernel. |
| `IORING_OP_LINK_TIMEOUT` | 15 | 5.5 | `crates/fast_io/src/io_uring/batching.rs:194` | When `LinkTimeout` is unavailable the chained `PollAdd` still arms; the timeout is a best-effort safety rail and the call path tolerates its absence. |
| `IORING_OP_STATX` | 21 | 5.11 (`STATX_MIN_KERNEL`) | `crates/fast_io/src/io_uring/statx.rs:177`, `crates/fast_io/src/io_uring/statx.rs:346` | `crates/fast_io/src/io_uring_stub/statx.rs:39,55` returns `Unsupported`; callers fall back to libc `statx`/`stat`. |
| `IORING_OP_READ` | 22 | 5.6 | `crates/fast_io/src/io_uring/file_reader.rs:121`, `crates/fast_io/src/io_uring/file_reader.rs:213`, `crates/fast_io/src/io_uring/linked_chain.rs:275`, `crates/fast_io/src/io_uring/linked_chain.rs:318`, `crates/fast_io/src/io_uring/shared_ring.rs:235`, `crates/fast_io/src/copy_file_range.rs:167` | Whole io_uring path is gated on kernel >= 5.6 (`crates/fast_io/src/io_uring/config.rs:19` `MIN_KERNEL_VERSION`); below that the dispatcher selects standard `read(2)`. |
| `IORING_OP_WRITE` | 23 | 5.6 | `crates/fast_io/src/io_uring/file_writer.rs:134`, `crates/fast_io/src/io_uring/batching.rs:90`, `crates/fast_io/src/io_uring/linked_chain.rs:285`, `crates/fast_io/src/io_uring/linked_chain.rs:323`, `crates/fast_io/src/copy_file_range.rs:198` | Same 5.6 gate as `READ`; falls back to standard `write(2)`. |
| `IORING_OP_SEND` | 26 | 5.6 | `crates/fast_io/src/io_uring/shared_ring.rs:289`, `crates/fast_io/src/io_uring/batching.rs:317` | `socket_factory.rs:135` returns `Unsupported` so the socket writer reverts to blocking `send(2)`. |
| `IORING_OP_RECV` | 27 | 5.6 | `crates/fast_io/src/io_uring/socket_reader.rs:49`, `crates/fast_io/src/io_uring/socket_reader.rs:90` | `socket_factory.rs:87` returns `Unsupported`; the reader falls back to blocking `recv(2)`. |
| `IORING_OP_RENAMEAT` | 35 | 5.11 | `crates/fast_io/src/io_uring/renameat2.rs:142` | `renameat2.rs:125` returns `Unsupported` after a runtime probe; `io_uring_ops.rs::try_io_uring_rename` falls back to libc `renameat2`. Stub at `crates/fast_io/src/io_uring_stub/renameat2.rs:44,59` mirrors the error on non-Linux. |
| `IORING_OP_LINKAT` | 39 | 5.15 (`LINKAT_MIN_KERNEL`) | `crates/fast_io/src/io_uring/linkat.rs:152` | `linkat.rs:138` returns `Unsupported` after a runtime probe; `io_uring_ops.rs::try_io_uring_hardlink` falls back to libc `linkat`. Stub at `crates/fast_io/src/io_uring_stub/linkat.rs:35,46` returns the same error off-Linux. |
| `IORING_OP_SEND_ZC` | 44 | 6.0 | `crates/fast_io/src/io_uring/send_zc.rs:150` | `send_zc.rs:142` returns `Unsupported`; default builds also gate the dispatch behind the `iouring-send-zc` cargo feature and silently downgrade to `IORING_OP_SEND` (see `lib.rs:367`). |

Note: `IORING_REGISTER_PBUF_RING` (22) and `IORING_UNREGISTER_PBUF_RING`
(23) are register opcodes, not SQE opcodes, but they share the same
"unsupported below kernel 5.19" lifecycle. Source:
`crates/fast_io/src/io_uring/buffer_ring/registration.rs:14-24`. When
unsupported, the buffer-ring fast path is skipped and the legacy
provide-buffers path runs (or, if that is also missing, plain
`IORING_OP_READ` against an owned buffer).

## Summary by kernel-floor tier

| Tier | Opcodes used in fast_io |
|---|---|
| 5.1 (basic ring) | `NOP`, `FSYNC`, `READ_FIXED`, `WRITE_FIXED`, `POLL_ADD` |
| 5.5 (cancel + linked timeout) | `ASYNC_CANCEL` (user-data form), `LINK_TIMEOUT` |
| 5.6 (most common; oc-rsync hard floor) | `READ`, `WRITE`, `SEND`, `RECV` |
| 5.7+ | none dispatched (no `SPLICE` / `TEE` / `PROVIDE_BUFFERS` SQEs are built; the userspace `splice(2)` path lives in `crates/fast_io/src/splice/` and does not use io_uring) |
| 5.11 | `STATX`, `RENAMEAT` |
| 5.15 | `LINKAT` |
| 5.19 (register-side) | `IORING_REGISTER_PBUF_RING`, `IORING_UNREGISTER_PBUF_RING`, `IORING_OP_ASYNC_CANCEL` fd-targeted form |
| 6.0 | `SEND_ZC` |

## Effective minimum kernel and silent-degradation map

- **Hard floor for any io_uring at all: Linux 5.6.** Set by
  `MIN_KERNEL_VERSION = (5, 6)` in
  `crates/fast_io/src/io_uring/config.rs:19` and probed once at construction
  time (`config.rs:300`). Below 5.6 the entire io_uring path is bypassed
  and the standard syscall executors take over. This is also the floor
  required by the `READ` / `WRITE` / `SEND` / `RECV` opcodes the data
  plane is built on.
- **Below 5.11.** `STATX` and `RENAMEAT` submissions fail the probe and
  fall back to libc `statx` / `renameat2`. The receiver fast path that
  batches stat calls effectively becomes serial.
- **Below 5.15.** `LINKAT` falls back to libc `linkat`; hardlink-heavy
  transfers lose the io_uring batching win.
- **Below 5.19.** PBUF_RING is unavailable so reader and writer paths skip
  the kernel-side buffer ring and use owned buffers + plain `READ` /
  `WRITE`. Fd-targeted `ASYNC_CANCEL` (the 5.19 form via
  `CancelBuilder::fd`) downgrades to user-data cancel.
- **Below 6.0.** `SEND_ZC` is unavailable and the socket writer uses
  `SEND`. Note that even on 6.0+ the default build omits SEND_ZC unless
  compiled with the `iouring-send-zc` feature (see `lib.rs:367`), so the
  6.0 path is opt-in regardless of kernel.

The full feature tier - all opcodes available, including SEND_ZC and
fd-targeted cancel - effectively requires **Linux 6.0** with the
`iouring-send-zc` feature enabled. Every opcode above the 5.6 hard floor
has a runtime probe and a documented fallback path; oc-rsync will
continue to function on RHEL 8 era kernels (4.18) by selecting the
non-io_uring executors throughout.
