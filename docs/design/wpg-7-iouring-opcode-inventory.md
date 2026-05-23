# WPG-7.a - io_uring opcode inventory

Audit-only inventory of every io_uring opcode the `crates/fast_io/src/io_uring/`
subtree actually submits or invokes. Compiled from a static grep of the source
tree on branch `docs/wpg-7a-iouring-opcode-inventory`.

This document feeds:

- **WPG-7.b** - IOCP equivalent mapping (which opcodes have a Windows peer).
- **WPG-7.c** - gap-list (which opcodes have no portable counterpart).

The kernel-version column lists the minimum Linux release that ships the
opcode (`include/uapi/linux/io_uring.h`). Where the codebase pins this value
itself, the constant is named in parentheses; values without a constant are
sourced from the upstream kernel UAPI history.

## Scope

Only opcodes that the codebase **actually uses** are listed. The following
opcodes appear in comments only (not submitted) and are intentionally
excluded: `IORING_OP_PROVIDE_BUFFERS`, `IORING_OP_READV`, `IORING_OP_WRITEV`,
`IORING_OP_SENDFILE`, `IORING_OP_SPLICE`, `IORING_OP_TEE`,
`IORING_OP_OPENAT`, `IORING_OP_OPENAT2`, `IORING_OP_CLOSE`,
`IORING_OP_FALLOCATE`, `IORING_OP_FADVISE`, `IORING_OP_MADVISE`,
`IORING_OP_UNLINKAT`, `IORING_OP_MKDIRAT`, `IORING_OP_SYMLINKAT`.

The data path remains read/write-centric; metadata extensions cover hard link,
rename, and statx only. See `project_io_uring_scope_metadata_only.md` for the
rationale.

## Submission-queue opcodes (SQE ops)

| Opcode | Where (file:line) | Purpose | Linux min kernel |
|---|---|---|---|
| `IORING_OP_NOP` | `io_uring/registered_buffers/tests/drop_contract.rs:31` | Test-only stub SQE for drop-contract assertions. | 5.1 |
| `IORING_OP_READ` | `io_uring/file_reader.rs:125`, `io_uring/file_reader.rs:244`, `io_uring/shared_ring.rs:235`, `io_uring/linked_chain.rs:275`, `io_uring/linked_chain.rs:318` | Buffered file reads (positional and stream); fallback path when registered buffers are unavailable; first half of every linked READ -> WRITE copy chain. | 5.6 |
| `IORING_OP_WRITE` | `io_uring/file_writer.rs:214`, `io_uring/batching.rs:90`, `io_uring/linked_chain.rs:285`, `io_uring/linked_chain.rs:323` | Buffered file writes; default flush opcode when registered buffers are unavailable; second half of every linked READ -> WRITE copy chain. | 5.6 |
| `IORING_OP_READ_FIXED` | `io_uring/registered_buffers/submit.rs:62` | Reads into pre-registered buffer slots; eliminates per-SQE page pinning. Engaged whenever the buffer registry has free slots. | 5.1 |
| `IORING_OP_WRITE_FIXED` | `io_uring/registered_buffers/submit.rs:194` | Writes from pre-registered buffer slots; symmetric to `READ_FIXED`. | 5.1 |
| `IORING_OP_FSYNC` | `io_uring/file_writer.rs:407`, `io_uring/disk_batch.rs:238` | Async durability barriers issued at file close and at the end of each disk batch. | 5.1 |
| `IORING_OP_SEND` | `io_uring/shared_ring.rs:289`, `io_uring/batching.rs:317` | Socket writes for the daemon/TCP path; batched after a `POLL_ADD(POLLOUT)` readiness gate. | 5.6 |
| `IORING_OP_SEND_ZC` | `io_uring/send_zc.rs:150` | Zero-copy socket send (Linux 6.0+). Posts a value CQE plus a notification CQE that signals page release. Dispatched only when the `iouring-send-zc` cargo feature is enabled **and** the runtime probe reports the opcode supported. See `project_iouring_send_zc_optin_only.md`. | 6.0 |
| `IORING_OP_RECV` | `io_uring/socket_reader.rs:49`, `io_uring/socket_reader.rs:90` | Socket reads for the daemon/TCP path. | 5.6 |
| `IORING_OP_POLL_ADD` | `io_uring/shared_ring.rs:262`, `io_uring/batching.rs:189`, `io_uring/cancel.rs:392`, `io_uring/cancel.rs:454` | Readiness gate. `POLLOUT` precedes each `SEND` batch (issue #1872 fix); `POLLIN` is the cancel-suite's blocking target. The numeric value `IORING_OP_POLL_ADD = 6` is asserted in `shared_ring.rs:83`. | 5.1 |
| `IORING_OP_STATX` | `io_uring/statx.rs:177`, `io_uring/statx.rs:346` | Async `statx(2)` for batched metadata lookups. Constant `IORING_OP_STATX = 21` lives in `io_uring_common.rs:58` (`STATX_MIN_KERNEL = (5, 11)`). | 5.11 |
| `IORING_OP_RENAMEAT` | `io_uring/renameat2.rs:142` | Async `renameat2(2)` for atomic file commits. Constant `IORING_OP_RENAMEAT = 35` lives in `io_uring_common.rs:43`. | 5.11 |
| `IORING_OP_LINKAT` | `io_uring/linkat.rs:152` | Async hard-link creation for `--link-dest` and the hard-link recreation path. Constant `IORING_OP_LINKAT = 39` lives in `io_uring_common.rs:34` (`LINKAT_MIN_KERNEL = (5, 15)`). | 5.15 |
| `IORING_OP_ASYNC_CANCEL` | `io_uring/cancel.rs:160` | Cancel an in-flight SQE matched by `user_data`. Constant `IORING_OP_ASYNC_CANCEL = 14` lives in `io_uring_common.rs:69` (`ASYNC_CANCEL_MIN_KERNEL = (5, 5)`). | 5.5 |
| `IORING_OP_ASYNC_CANCEL` (extended) | `io_uring/cancel.rs:205` | Same opcode as above but submitted via the `AsyncCancel2` builder with `CancelBuilder::fd(...)` / `ALL` flags. Cancel-by-fd and cancel-all need `ASYNC_CANCEL_FD_MIN_KERNEL = (5, 19)` (`io_uring_common.rs:77`). | 5.19 |
| `IORING_OP_LINK_TIMEOUT` | `io_uring/batching.rs:194` | Linked timeout that bounds the preceding `POLL_ADD(POLLOUT)` so a back-pressured socket cannot deadlock the batched-send path. | 5.5 |

## Registration / setup opcodes (`io_uring_register(2)`, `io_uring_setup(2)`)

These are not SQE ops; they are operations on the ring itself, issued through
the `io-uring` crate's `Submitter` / `Builder` wrappers or via raw `syscall`.

| Opcode | Where (file:line) | Purpose | Linux min kernel |
|---|---|---|---|
| `IORING_REGISTER_FILES` | `io_uring/file_reader.rs:70`, `io_uring/shared_ring.rs:396`, `io_uring/batching.rs:37` (via `try_register_fd`) | Registers file descriptors against the ring so SQEs reference an integer slot instead of a raw fd, eliminating per-submission file-table lookups. Gated by `IoUringConfig::register_files`. | 5.1 |
| `IORING_UNREGISTER_FILES` | `io_uring/disk_batch.rs:270` | Releases the registered file slot when a `DiskBatch` rotates to a new active file. | 5.1 |
| `IORING_REGISTER_BUFFERS` | `io_uring/registered_buffers/registry.rs:216` | Pins an `iovec` array so `READ_FIXED` / `WRITE_FIXED` SQEs can reference slot indices instead of user pointers. Gated by `IoUringConfig::register_buffers`. | 5.1 |
| `IORING_UNREGISTER_BUFFERS` | `io_uring/registered_buffers/registry.rs:387`, `io_uring/registered_buffers/tests/drop_contract.rs:213` | Releases the pinned `iovec` array. Drop-time test coverage verifies the kernel slot table stays consistent across re-registration. | 5.1 |
| `IORING_REGISTER_PROBE` | `io_uring/linkat.rs:118`, `io_uring/renameat2.rs:81`, `io_uring/statx.rs:143`, `io_uring/send_zc.rs:102`, `io_uring/shared_ring.rs:370`, `io_uring/config.rs:281` | Capability detection. Each one-shot probe asks the kernel whether a specific opcode is supported and caches the answer in a `OnceLock`. Used for `LINKAT`, `RENAMEAT`, `STATX`, `SEND_ZC`, and `POLL_ADD`, plus the generic config-time bring-up probe. | 5.6 |
| `IORING_REGISTER_PBUF_RING` | `io_uring/buffer_ring/mod.rs:312` (raw `SYS_io_uring_register` syscall, opcode `22`) | Registers a provided-buffer ring so the kernel hands out buffer IDs in CQE flags. Used by the experimental adaptive buffer-pool path. Opcode constant: `buffer_ring/registration.rs:15`. | 5.19 |
| `IORING_UNREGISTER_PBUF_RING` | `io_uring/buffer_ring/mod.rs:543` (raw `SYS_io_uring_register` syscall, opcode `23`) | Unregisters the provided-buffer ring on drop. Opcode constant: `buffer_ring/registration.rs:18`. | 5.19 |
| `IORING_SETUP_SQPOLL` | `io_uring/config.rs:361`, `io_uring/session_pool.rs:278` (via `Builder::setup_sqpoll`) | Spawns the kernel-side submission polling thread to drop submit-syscall overhead. Gated by `IoUringConfig::sqpoll`. Defensively disabled when an mmap'd basis is live (`project_sqpoll_disabled_with_mmap.md`). | 5.13 (stable, unprivileged) |

## Dispatch classification

Each row is tagged as one of: **default-on** (engaged in every io_uring
submission of that class), **conditional** (engaged when a runtime path or
build configuration enables it), **feature-gated** (gated by a cargo
feature), or **one-shot probe** (used only during startup capability
detection).

| Opcode | Default-on | Conditional | Feature-gated | One-shot probe |
|---|---|---|---|---|
| `IORING_OP_NOP` | | test-only | | |
| `IORING_OP_READ` | yes (fallback when no fixed buffer slot is free) | yes (chained copy mode) | | |
| `IORING_OP_WRITE` | yes (fallback when no fixed buffer slot is free) | yes (chained copy mode) | | |
| `IORING_OP_READ_FIXED` | | yes (when `register_buffers = true` and a slot is free) | | |
| `IORING_OP_WRITE_FIXED` | | yes (when `register_buffers = true` and a slot is free) | | |
| `IORING_OP_FSYNC` | yes (per disk batch and on file close) | | | |
| `IORING_OP_SEND` | yes (socket-write path) | | | |
| `IORING_OP_SEND_ZC` | | yes (runtime probe + payload-size floor) | yes (`iouring-send-zc`) | yes (probe via `register_probe`) |
| `IORING_OP_RECV` | yes (socket-read path) | | | |
| `IORING_OP_POLL_ADD` | yes (gates every batched `SEND`) | yes (cancel-suite blocking target) | | yes (probe via `register_probe`) |
| `IORING_OP_STATX` | | yes (when probe reports supported) | | yes (probe via `register_probe`) |
| `IORING_OP_RENAMEAT` | | yes (when probe reports supported) | | yes (probe via `register_probe`) |
| `IORING_OP_LINKAT` | | yes (when probe reports supported) | | yes (probe via `register_probe`) |
| `IORING_OP_ASYNC_CANCEL` (classic) | | yes (cancel-by-`user_data` callers) | | |
| `IORING_OP_ASYNC_CANCEL` (extended) | | yes (cancel-by-fd / cancel-all on >= 5.19) | | |
| `IORING_OP_LINK_TIMEOUT` | yes (paired with every batched `SEND` poll gate) | | | |
| `IORING_REGISTER_FILES` | yes (when `register_files = true`, default) | | | |
| `IORING_UNREGISTER_FILES` | | yes (`DiskBatch` rotation) | | |
| `IORING_REGISTER_BUFFERS` | yes (when `register_buffers = true`, default) | | | |
| `IORING_UNREGISTER_BUFFERS` | | yes (on group drop) | | |
| `IORING_REGISTER_PROBE` | | | | yes (per-opcode capability detection) |
| `IORING_REGISTER_PBUF_RING` | | yes (adaptive buffer-pool path on >= 5.19) | | |
| `IORING_UNREGISTER_PBUF_RING` | | yes (paired with the registration above) | | |
| `IORING_SETUP_SQPOLL` | | yes (when `sqpoll = true` and not paired with an mmap'd basis) | | |

## Counts

- **SQE ops in use:** 14 distinct opcodes (`NOP`, `READ`, `WRITE`,
  `READ_FIXED`, `WRITE_FIXED`, `FSYNC`, `SEND`, `SEND_ZC`, `RECV`,
  `POLL_ADD`, `STATX`, `RENAMEAT`, `LINKAT`, `ASYNC_CANCEL`,
  `LINK_TIMEOUT`).
- **Registration / setup ops in use:** 9 (`REGISTER_FILES`,
  `UNREGISTER_FILES`, `REGISTER_BUFFERS`, `UNREGISTER_BUFFERS`,
  `REGISTER_PROBE`, `REGISTER_PBUF_RING`, `UNREGISTER_PBUF_RING`,
  `SETUP_SQPOLL`, plus the `AsyncCancel2` builder which still maps to
  `IORING_OP_ASYNC_CANCEL` and is counted once above).
- **Total distinct opcodes in use:** **23** (15 SQE-side counting the
  classic and extended `ASYNC_CANCEL` variants as one opcode plus
  `LINK_TIMEOUT`, plus 8 register/setup opcodes - the buffer-ring pair
  counts twice because they are distinct register sub-opcodes).

## Follow-ups (WPG-7.b, WPG-7.c inputs)

- **Data-path gap.** No `IORING_OP_SPLICE`, `IORING_OP_TEE`,
  `IORING_OP_SENDFILE` are submitted. The reconstructed-file write path
  still goes through plain `WRITE` / `WRITE_FIXED`; see
  `project_no_copy_file_range_in_delta_apply.md`.
- **Filesystem-op gap.** No `OPENAT`, `OPENAT2`, `CLOSE`, `UNLINKAT`,
  `MKDIRAT`, `SYMLINKAT`, `FALLOCATE`, `FADVISE`, `MADVISE` are submitted.
  Metadata coverage is deliberately limited to `STATX`, `RENAMEAT`, and
  `LINKAT` per `project_io_uring_scope_metadata_only.md`.
- **Vectored I/O gap.** `READV` / `WRITEV` are unused; all batched I/O is
  emitted as multiple scalar `READ` / `WRITE` SQEs.
- **Zero-copy posture.** `SEND_ZC` is the only zero-copy primitive wired
  in; it is feature-gated and skipped below the `MIN_SEND_ZC_PAYLOAD`
  floor (`socket_writer.rs:12`).
- **Capability detection.** Every opt-in opcode (`STATX`, `RENAMEAT`,
  `LINKAT`, `SEND_ZC`, `POLL_ADD`) routes through `register_probe`. The
  probe result is cached in a `OnceLock` for the lifetime of the process.
