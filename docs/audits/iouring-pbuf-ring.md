# Provided-buffer rings (`IORING_REGISTER_PBUF_RING`) for io_uring read paths

Tracking issue: oc-rsync task #2043. Sibling audits:
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md) (task #1859),
[`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
(task #1086), [`docs/audits/mmap-iouring-co-usage.md`](mmap-iouring-co-usage.md)
(task #1660).

Last verified: 2026-05-01 against master @ `83c8aa41`. Files spot-checked:
`crates/fast_io/src/io_uring/mod.rs`,
`crates/fast_io/src/io_uring/buffer_ring.rs`,
`crates/fast_io/src/io_uring/registered_buffers.rs`,
`crates/fast_io/src/io_uring/file_reader.rs`,
`crates/fast_io/src/io_uring/socket_reader.rs`,
`crates/fast_io/src/io_uring/socket_writer.rs`,
`crates/fast_io/src/io_uring/config.rs`,
`crates/fast_io/src/lib.rs`.

## Scope

Evaluate whether `IORING_REGISTER_PBUF_RING` (Linux 5.19+) is the right
mechanism for any oc-rsync io_uring read path. The audit answers four
questions:

1. What does PBUF_RING add over the registered fixed buffers
   (`IORING_REGISTER_BUFFERS`) we already use in
   `crates/fast_io/src/io_uring/registered_buffers.rs`?
2. Which oc-rsync call sites would benefit, and which would not?
3. How is feature detection layered onto the existing io_uring kernel
   probe so that the fallback chain stays explicit and observable?
4. What is the recommended phasing for an in-tree implementation, given
   that the current PBUF_RING module
   (`crates/fast_io/src/io_uring/buffer_ring.rs`) ships unwired?

This is a documentation-only audit. No Rust code changes are proposed
here; all proposals route through subsequent issues.

## TL;DR

PBUF_RING is the right primitive for completion-time buffer selection on
**stream-oriented** receive paths (sockets, pipes, recv-multishot). It is
not a replacement for `IORING_REGISTER_BUFFERS` on positional file reads
where the submitter already knows offset and length. oc-rsync's only
positional read path,
[`crates/fast_io/src/io_uring/file_reader.rs:30`](../../crates/fast_io/src/io_uring/file_reader.rs)
(`IoUringReader`), should keep using `READ_FIXED` against
`RegisteredBufferGroup`. The natural PBUF_RING consumers are the
**stream receive** paths:

- `crates/fast_io/src/io_uring/socket_reader.rs:16` (`IoUringSocketReader`,
  daemon-side `IORING_OP_RECV`).
- A future io_uring SSH-stdio reader (audit
  [`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md), task
  #1859), if/when phase 1 of that audit lands and adds an
  `IoUringPipeReader`.

Today the PBUF_RING module
[`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
exists, is publicly re-exported through
[`crates/fast_io/src/lib.rs:154-160`](../../crates/fast_io/src/lib.rs)
as `BufferRing` / `BufferRingConfig` / `BufferRingError` /
`buffer_id_from_cqe_flags`, and is exercised by its own unit tests, but
no caller submits an SQE with `IOSQE_BUFFER_SELECT` and no socket / pipe
reader uses `IORING_OP_RECV_MULTISHOT`. The module-private helper
[`buffer_ring::is_supported`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
(line 183) is reachable through the `io_uring::buffer_ring` submodule
but is not yet re-exported at the crate root. The module is
production-quality infrastructure waiting for a consumer.

Recommendation: do not migrate `IoUringReader`. Wire PBUF_RING into the
stream receive paths (`IoUringSocketReader` first, future
`IoUringPipeReader` second) only after a benchmark gate confirms the
expected reduction in submit-side syscalls and per-SQE bookkeeping.

Upstream evidence: a recursive grep for `io_uring`, `IORING_`, `liburing`
in `target/interop/upstream-src/rsync-3.4.1/` returns no matches.
Upstream rsync uses plain `read(2)` / `write(2)` for all I/O; PBUF_RING
is a pure oc-rsync optimisation with no wire-protocol implication.

## 1. What `IORING_REGISTER_PBUF_RING` provides

### 1.1 Mechanism

PBUF_RING was introduced in Linux 5.19 (kernel commit `c7fb19428d67`,
"io_uring: add support for ring mapped supplied buffers"). It exposes
two opcodes via `io_uring_register(2)`:

- `IORING_REGISTER_PBUF_RING` (opcode 22).
- `IORING_UNREGISTER_PBUF_RING` (opcode 23).

These match the constants defined at
[`crates/fast_io/src/io_uring/buffer_ring.rs:40-50`](../../crates/fast_io/src/io_uring/buffer_ring.rs).

The userspace flow:

1. Allocate a contiguous buffer region (page-aligned).
2. `mmap` the io_uring fd at offset
   `IORING_OFF_PBUF_RING | (bgid << 16)` to obtain a shared ring of
   `struct io_uring_buf` descriptors.
3. Populate the descriptors with `(addr, len, bid)` tuples and publish
   the tail.
4. Call `io_uring_register(IORING_REGISTER_PBUF_RING, &io_uring_buf_reg)`
   to associate the ring with a **buffer group ID** (`bgid`).
5. Submit SQEs with `IOSQE_BUFFER_SELECT` and the matching `bgid`. The
   submitter does **not** pre-bind a buffer.
6. On completion, the CQE carries `IORING_CQE_F_BUFFER` and the chosen
   buffer ID encoded in the upper 16 bits of `cqe->flags`.
7. Userspace recycles the buffer by advancing the ring tail.

The `BufferRing` API in
[`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
implements this dance: `BufferRing::new` (line 267), `bgid` (line 414),
`buffer_ptr` / `buffer_slice` (lines 441-462), `recycle_buffer`
(line 472), and the free function `buffer_id_from_cqe_flags`
(line 563). Authoritative kernel reference: `io_uring/kbuf.c`,
`io_register_pbuf_ring()`. Authoritative man pages:
`io_uring_register(2)` (`IORING_REGISTER_PBUF_RING` /
`IORING_UNREGISTER_PBUF_RING` sections),
`io_uring_register_buf_ring(3)`,
`io_uring_buf_ring_init(3)`.

### 1.2 What it adds over `IORING_REGISTER_BUFFERS`

`IORING_REGISTER_BUFFERS` (kernel 5.1+, used at
[`crates/fast_io/src/io_uring/registered_buffers.rs:260`](../../crates/fast_io/src/io_uring/registered_buffers.rs)
via `submitter().register_buffers()`) and PBUF_RING solve **different**
problems:

| Concern                    | `IORING_REGISTER_BUFFERS`                       | `IORING_REGISTER_PBUF_RING`                    |
| -------------------------- | ----------------------------------------------- | ---------------------------------------------- |
| Min kernel                 | 5.1 (READ_FIXED in 5.6)                         | 5.19                                           |
| Buffer selection time      | Submission (caller binds `buf_index`)           | Completion (kernel picks from group)           |
| Ideal target               | Positional `READ_FIXED` / `WRITE_FIXED`         | Stream `RECV` / `READ` with multishot          |
| SQE bookkeeping            | One slot checkout per SQE (atomic bitset)       | None; submitter passes only `bgid`             |
| Per-buffer pinning         | All N buffers pinned at register time           | All N buffers pinned at register time          |
| Unknown-length reads       | Wastes whole buffer on short read               | Returns exact length via CQE result            |
| Multishot (`*_MULTISHOT`)  | Not applicable                                  | Required for true zero-syscall recv loop       |
| Recycling protocol         | RAII slot drop returns to bitset                | Atomic tail-pointer advance in shared mmap     |
| Userspace copy             | One copy on `READ_FIXED`/`WRITE_FIXED`          | Same one copy; PBUF_RING is not zero-copy DMA  |
| Documented in oc-rsync     | Yes, used by `IoUringReader`/`IoUringWriter`    | Yes, infrastructure unwired                    |

The widely cited "zero-copy receive" framing is misleading: PBUF_RING is
not a DMA-direct path. The kernel still copies bytes from the socket
buffer or pipe buffer into the chosen registered buffer. What is
eliminated is the **per-SQE bookkeeping**: the submitter no longer
allocates a buffer index up front, no longer carries a slot-return
RAII guard, and (with `IORING_OP_RECV_MULTISHOT`, kernel 6.0+) does not
need to resubmit an SQE for every datagram or pipe segment. `recvmsg`
zero-copy proper requires `MSG_ZEROCOPY` plus `IORING_OP_SEND_ZC` /
`IORING_OP_SENDMSG_ZC` and is orthogonal to this audit.

### 1.3 Composability with multishot

PBUF_RING shines in combination with `IORING_OP_RECV_MULTISHOT` (kernel
6.0+, opcode 36) or `IORING_OP_READ_MULTISHOT` (kernel 6.0+, opcode 39):
one SQE produces N CQEs, each carrying a different buffer ID, until the
fd reports EOF / error or the ring runs dry. For a long-lived TCP
connection (rsync daemon) or SSH stdio pipe, this collapses the
recv-loop's submission cost from O(N) syscalls to O(1) for the entire
session.

## 2. Current oc-rsync io_uring read surface

### 2.1 Positional file reads (`IoUringReader`)

[`crates/fast_io/src/io_uring/file_reader.rs`](../../crates/fast_io/src/io_uring/file_reader.rs)
defines `IoUringReader` (line 30). The hot path is `read_all_batched`
(line 149): for files where `RegisteredBufferGroup::try_new` succeeded,
it calls `submit_read_fixed_batch` against checked-out slots; otherwise
it falls back to plain `IORING_OP_READ` SQEs at known offsets. Single-
shot `read_at` (line 99) issues one `opcode::Read::new(fd, ptr, len)
.offset(offset)` per call.

For positional file reads, the submitter already knows:

- The file size (`self.size`, line 33).
- The current offset and the chunk count.
- Exactly which buffer slot to bind for each SQE.

PBUF_RING would force completion-time buffer selection where it adds
nothing: there is no length ambiguity, no kernel decision to defer, and
the existing `RegisteredBufferGroup` checkout already amortises page
pinning across the file. Migrating `IoUringReader` to PBUF_RING would
trade a deterministic slot bitset for a kernel-driven ring tail and gain
no observable performance benefit. **Do not migrate.**

### 2.2 Stream receive (`IoUringSocketReader`)

[`crates/fast_io/src/io_uring/socket_reader.rs`](../../crates/fast_io/src/io_uring/socket_reader.rs)
defines `IoUringSocketReader` (line 16). It maintains a single
`BufReader`-style buffer (`self.buffer: Vec<u8>`, line 20) and on each
fill submits one `opcode::Recv` SQE via `fill_buffer` (line 47). The
read path also has a "large read" bypass (line 84) that submits a
single `Recv` directly into the caller's buffer when the requested size
exceeds the internal buffer.

This is the canonical PBUF_RING use case:

- Receive size is unknown until the kernel reports the CQE result.
- The driver runs in a tight loop for the full lifetime of the daemon
  connection.
- A multishot recv plus a buffer ring would replace the current
  one-SQE-per-fill pattern with a single submission feeding many CQEs.
- Each CQE result is the exact byte count, eliminating the "did we
  fill the buffer" bookkeeping.

### 2.3 Stream send (`IoUringSocketWriter`)

[`crates/fast_io/src/io_uring/socket_writer.rs`](../../crates/fast_io/src/io_uring/socket_writer.rs)
defines `IoUringSocketWriter` (line 16). Sends are submitter-driven:
the caller knows the exact byte slice to send. PBUF_RING is a receive
primitive and does not apply. Listed here only to bound the audit.

### 2.4 Future SSH-stdio reader

[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md) recommends
a phase-1 `IoUringPipeReader` mirroring `IoUringSocketReader` with
`IORING_OP_READ` instead of `IORING_OP_RECV` on pipe fds. That audit
explicitly flags PBUF_RING as a phase-2 enhancement
([`docs/audits/iouring-pipe-stdio.md:218-224`](iouring-pipe-stdio.md)).
Pipes are stream-oriented receive paths with unknown segment lengths
and tight inner loops; PBUF_RING fits the same way it fits sockets.

### 2.5 Other io_uring reads

`IoUringDiskBatch` ([`crates/fast_io/src/io_uring/disk_batch.rs`](../../crates/fast_io/src/io_uring/disk_batch.rs))
is write-only.
`mmap_reader.rs` does not use io_uring.
`o_tmpfile/`, `splice.rs`, `sendfile.rs` are non-io_uring paths or
write-only paths.
There is no other io_uring read site to consider.

## 3. Feature detection and fallback chain

### 3.1 Existing io_uring probe

[`crates/fast_io/src/io_uring/config.rs`](../../crates/fast_io/src/io_uring/config.rs)
already implements a layered probe:

1. `get_kernel_release()` (line 58) parses `uname(2).release`.
2. `parse_kernel_version()` (line 50) extracts `(major, minor)`.
3. `MIN_KERNEL_VERSION = (5, 6)` (line 19) gates io_uring entirely.
4. `check_io_uring_reason()` (line 256) attempts `IoUring::new(4)` and
   returns one of `Available { supported_ops }` / `NoKernelRelease` /
   `UnparsableVersion` / `KernelTooOld { major, minor }` /
   `SyscallBlocked { major, minor }` (lines 184-210).
5. Op count is collected via `count_supported_ops` using
   `register_probe` (line 246-253).

The result is cached process-wide in `IO_URING_AVAILABLE` /
`IO_URING_CHECKED` (lines 22-23) and surfaced through
`io_uring_availability_reason()` and `io_uring_kernel_info()` for
`--version` output.

### 3.2 Existing PBUF_RING probe

[`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
already supplies the per-feature check:

- `MIN_PBUF_RING_KERNEL = (5, 19)` (line 50).
- `is_supported()` (line 183) reads the same uname release string and
  reports whether the running kernel is >= 5.19.
- `check_kernel_version()` (line 574) returns
  `BufferRingError::KernelTooOld` / `BufferRingError::KernelVersionUnknown`.
- `BufferRing::try_new` (line 404) is the speculative entry point: it
  swallows every error and returns `Option<BufferRing>`, mirroring the
  `RegisteredBufferGroup::try_new` (line 303) convention used elsewhere
  in `fast_io`.

The kernel version check is necessary but not sufficient. Even on
5.19+, the actual `IORING_REGISTER_PBUF_RING` syscall can fail with
`-EINVAL` if the kernel was built without the feature, with `-ENOMEM`
under pressure, or with `-EPERM` under restrictive seccomp profiles.
`BufferRing::new` therefore unwinds the allocation and `mmap` if the
register call returns an error
([`buffer_ring.rs:379-386`](../../crates/fast_io/src/io_uring/buffer_ring.rs)).
This matches the pattern at
`registered_buffers.rs:260-268` for `IORING_REGISTER_BUFFERS`.

### 3.3 Layered fallback table

The intended runtime decision tree:

```
+-- io_uring not available (probe failed, kernel < 5.6, seccomp blocked)
|       -> standard pread / pwrite / read / write via traits::Std*
|
+-- io_uring available, kernel >= 5.6 but < 5.19
|       -> READ_FIXED via RegisteredBufferGroup (existing path)
|       -> single-shot RECV / READ for stream paths (existing path)
|
+-- io_uring available, kernel >= 5.19, PBUF_RING register fails
|       -> log diagnostic, fall back to the >= 5.6 path
|
+-- io_uring available, kernel >= 5.19, PBUF_RING register succeeds
        +-- kernel < 6.0
        |   -> single-shot RECV with IOSQE_BUFFER_SELECT (proposed)
        +-- kernel >= 6.0
            -> IORING_OP_RECV_MULTISHOT with IOSQE_BUFFER_SELECT (proposed)
```

The probe should be **opcode-aware**, not just version-aware. The
existing `count_supported_ops` ([`config.rs:246`](../../crates/fast_io/src/io_uring/config.rs))
already calls `register_probe` once per process. The phase-2 work
should extend that probe with explicit checks for opcode 22
(`IORING_REGISTER_PBUF_RING`), opcode 36
(`IORING_OP_RECV_MULTISHOT`, optional), and opcode 39
(`IORING_OP_READ_MULTISHOT`, optional), and surface them as fields on
`IoUringKernelInfo` ([`config.rs:73-85`](../../crates/fast_io/src/io_uring/config.rs)).
The diagnostic `io_uring: enabled (kernel 6.1, 48 ops supported)` line
should grow a `pbuf_ring=yes,recv_multishot=yes` suffix so that
operators can see the active mode without `strace`.

This composition mirrors the layering already in place for SQPOLL in
[`config.rs:30-47`](../../crates/fast_io/src/io_uring/config.rs)
(`SQPOLL_FALLBACK` / `sqpoll_fell_back`): probe, attempt, log on
fallback, never panic.

## 4. Why we should not migrate `IoUringReader`

A blunt list of reasons positional file reads should keep
`READ_FIXED` + `IORING_REGISTER_BUFFERS`:

- **Length is known.** Positional reads against a regular file are not
  length-ambiguous. The submitter knows exactly how many bytes will
  return (modulo end-of-file). PBUF_RING's "let the kernel pick the
  buffer at completion time" is overhead, not advantage.
- **Slot reuse is already cheap.** The atomic bitset in
  `RegisteredBufferGroup` ([`registered_buffers.rs:107`](../../crates/fast_io/src/io_uring/registered_buffers.rs))
  is contention-free for the single-thread submission pattern used by
  `IoUringReader`. Replacing it with a kernel-shared ring tail adds
  cross-CPU coherency cost.
- **No multishot equivalent.** `IORING_OP_READ_FIXED` does not have a
  multishot variant; the multishot family is for receive-style paths.
- **Bigger break radius.** `IoUringReader::read_all_batched` is on the
  hot path for the sender's basis-file scan. Replacing it touches
  every basis read. The current `READ_FIXED` path is exercised by
  `crates/fast_io/tests/io_uring_probe_fallback.rs`; reverting on a
  PBUF_RING regression would mean reverting a merge.

Conversely, the stream paths (sockets, pipes) all share the properties
that make PBUF_RING worthwhile: completion-time length discovery, hot
inner loops, and a natural pairing with multishot.

## 5. Phased implementation plan

### Phase 1 (this audit, no code change)

- Document the call-site survey, fallback chain, and recommended
  scoping.
- Cross-reference with `iouring-pipe-stdio.md`,
  `disk-commit-iouring-batching.md`, `mmap-iouring-co-usage.md` so a
  reader can reach the relevant io_uring context in one click.
- Status: **delivered by this file**.

### Phase 2 (feature detection only, no behaviour change)

Goal: make the kernel and op-level support observable in
`--version` output and in logs, without changing any code path.

Concrete steps for a follow-up PR:

1. Extend `IoUringKernelInfo` ([`config.rs:73-85`](../../crates/fast_io/src/io_uring/config.rs))
   with explicit booleans for the opcodes relevant to PBUF_RING:
   `pbuf_ring_supported`, `recv_multishot_supported`,
   `read_multishot_supported`. Populate them by calling
   `probe.is_supported(opcode)` inside `count_supported_ops`
   ([`config.rs:246`](../../crates/fast_io/src/io_uring/config.rs)).
2. Extend `io_uring_availability_reason()` to append the active
   PBUF_RING / multishot summary so that
   `oc-rsync --version` shows
   `io_uring: enabled (kernel 6.1, 48 ops, pbuf_ring=yes, recv_multishot=yes)`.
3. Wire `BufferRing::is_supported()`
   ([`buffer_ring.rs:183`](../../crates/fast_io/src/io_uring/buffer_ring.rs))
   into the same probe so callers do not duplicate the uname parsing.
4. Add a `tracing::info!` log on the first PBUF_RING register attempt
   that fails on a kernel that the version probe reported as
   sufficient (covers the `-EINVAL` / `-EPERM` cases the version
   probe cannot detect).
5. No SQE in master submits with `IOSQE_BUFFER_SELECT` after phase 2.
   The phase is purely diagnostic.

Acceptance criteria: `--version` output reflects the matrix above on
5.6, 5.19, and 6.0+ test kernels; no behavioural changes; no new
warnings under `cargo clippy --workspace --all-targets`.

### Phase 3 (actual PBUF_RING usage)

Goal: bring stream receive paths onto PBUF_RING + multishot.

Concrete steps, **gated** on phase 2 landing and a benchmark gate
matching the `iouring-pipe-stdio.md` precedent:

1. Add an `IoUringSocketReader` constructor variant that accepts a
   `BufferRing` and an opcode preference (`Recv` /
   `RecvMultishot`). Default behaviour preserves today's
   `Recv`-with-internal-buffer path.
2. Implement the `BufferRing` consumer loop: on each CQE, extract the
   buffer ID via `buffer_id_from_cqe_flags`
   ([`buffer_ring.rs:563`](../../crates/fast_io/src/io_uring/buffer_ring.rs)),
   borrow the buffer slice via `BufferRing::buffer_slice`
   ([`buffer_ring.rs:458`](../../crates/fast_io/src/io_uring/buffer_ring.rs)),
   copy bytes out to the caller's buffer (the
   `io::Read::read` contract), and call `recycle_buffer`
   ([`buffer_ring.rs:472`](../../crates/fast_io/src/io_uring/buffer_ring.rs)).
3. Add a `criterion` benchmark under `crates/fast_io/benches/` that
   measures `io_uring_enter` and `recvfrom` syscall counts for a
   sustained 1 GiB receive on a `tcp_pair` fixture, with and without
   PBUF_RING. Acceptance: >= 30% reduction in syscall count on
   kernel 6.0+, no regression on kernel 5.19, automatic fallback on
   kernel 5.6-5.18.
4. After the daemon-side soak, repeat the work for the future
   `IoUringPipeReader` planned in
   [`iouring-pipe-stdio.md`](iouring-pipe-stdio.md). PBUF_RING reuse
   is the natural phase-2 enhancement called out at
   [`iouring-pipe-stdio.md:218-224`](iouring-pipe-stdio.md).
5. Do **not** migrate `IoUringReader`. Add a top-of-file note to
   `crates/fast_io/src/io_uring/file_reader.rs` explaining why
   `READ_FIXED` is the correct primitive for positional reads, and
   referencing this audit so the choice is recoverable.

Cross-platform: PBUF_RING is Linux-only and io_uring-feature-gated.
The non-Linux stub `crates/fast_io/src/io_uring_stub.rs` already mirrors
the public surface, so phase 3 changes route through the same
`#[cfg(...)]` path as the rest of `io_uring/`.

## 6. Risks and open questions

- **`IORING_REGISTER_PBUF_RING` is not idempotent.** Re-registering
  a different ring under the same `bgid` returns `-EBUSY` until the
  prior ring is unregistered. The phase-3 design must own the
  `bgid` namespace per ring instance, exactly as
  [`buffer_ring.rs:130-152`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
  models with `BufferRingConfig::bgid`.
- **Buffer exhaustion semantics.** If the ring runs dry mid-recv, the
  CQE returns `-ENOBUFS`. The driver must treat this as backpressure
  (resubmit after recycle) rather than a connection error. The
  current `IoUringSocketReader::fill_buffer`
  ([`socket_reader.rs:47`](../../crates/fast_io/src/io_uring/socket_reader.rs))
  has no `-ENOBUFS` handling because the existing path uses a single
  caller-owned buffer, so it cannot exhaust.
- **Head-of-line blocking under multishot.** `RECV_MULTISHOT` cancels
  on first error. The driver must convert the cancel CQE
  (`-ECANCELED` or peer error) into the same shutdown path used by
  the today's `Recv` failure
  ([`socket_reader.rs:71`](../../crates/fast_io/src/io_uring/socket_reader.rs)).
- **Page-pinning footprint.** PBUF_RING pins all N buffers for the
  ring's lifetime, identically to `IORING_REGISTER_BUFFERS`. The
  daemon-side ring should size the buffer count and per-buffer size
  conservatively; the
  [`mmap-iouring-co-usage.md`](mmap-iouring-co-usage.md) audit
  documents the equivalent pinning concern for registered fixed
  buffers and applies verbatim here.
- **Container friction.** Some seccomp profiles
  (`SECCOMP_RET_ERRNO(EPERM)` on `io_uring_register`) succeed in
  detecting `IORING_REGISTER_BUFFERS` but reject opcode 22. The
  fallback chain in section 3.3 must treat both `-EINVAL` and
  `-EPERM` as "PBUF_RING not available" without disabling io_uring
  itself.
- **Mixed ring-fd lifetime.** The `Drop` impl in
  [`buffer_ring.rs:516`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
  unregisters and `munmap`s; this requires the io_uring fd to still
  be valid at drop time. Phase 3 must declare the `BufferRing` field
  **after** the `RawIoUring` field in any owning struct, mirroring
  the documented order at
  [`registered_buffers.rs:18-39`](../../crates/fast_io/src/io_uring/registered_buffers.rs).

## 7. Why now

The PBUF_RING module already exists and is unwired
([`buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
shipped with no in-tree consumer). Documenting the decision to delay
wiring it - and specifying the conditions under which we would wire
it - prevents three predictable regressions:

1. A future contributor migrates `IoUringReader` to `BufferRing`
   because "we already have it", regressing positional read
   throughput.
2. The PBUF_RING module rots: tests stay green but no real workload
   exercises it, and a kernel-side change breaks oc-rsync silently.
3. The recv-side opportunity is missed because no audit ties
   PBUF_RING to the SSH-stdio audit (#1859) or the daemon socket
   audit (#1593, merged).

This audit is the canonical answer for the next reviewer who asks
"should we use PBUF_RING here?".

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no `io_uring` references; receive paths use plain `read(2)` /
  `recv(2)` in `io.c`).
- Existing oc-rsync io_uring infrastructure:
  `crates/fast_io/src/io_uring/mod.rs`,
  `crates/fast_io/src/io_uring/buffer_ring.rs` (PBUF_RING module),
  `crates/fast_io/src/io_uring/registered_buffers.rs`
  (`IORING_REGISTER_BUFFERS`),
  `crates/fast_io/src/io_uring/file_reader.rs`
  (`IoUringReader`),
  `crates/fast_io/src/io_uring/socket_reader.rs`
  (`IoUringSocketReader`),
  `crates/fast_io/src/io_uring/socket_writer.rs`
  (`IoUringSocketWriter`),
  `crates/fast_io/src/io_uring/config.rs` (kernel + op probe).
- Kernel-version probe and policy:
  `crates/fast_io/src/io_uring/config.rs`,
  `crates/fast_io/src/kernel_version.rs`.
- Sibling audits:
  [`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md)
  (task #1859, merged),
  [`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
  (task #1086, merged),
  [`docs/audits/mmap-iouring-co-usage.md`](mmap-iouring-co-usage.md)
  (task #1660, merged),
  [`docs/audits/iouring-socket-sqpoll-defer-taskrun.md`](iouring-socket-sqpoll-defer-taskrun.md).
- Linux man pages: `io_uring_register(2)` (sections
  `IORING_REGISTER_PBUF_RING`, `IORING_UNREGISTER_PBUF_RING`),
  `io_uring_register_buf_ring(3)`, `io_uring_buf_ring_init(3)`,
  `io_uring_buf_ring_add(3)`, `io_uring_buf_ring_advance(3)`,
  `io_uring_setup(2)`, `io_uring_enter(2)`.
- Kernel sources (verify before citing in comments): `io_uring/kbuf.c`
  (`io_register_pbuf_ring`, `io_unregister_pbuf_ring`),
  `include/uapi/linux/io_uring.h` (`struct io_uring_buf`,
  `struct io_uring_buf_reg`, `IORING_OFF_PBUF_RING`,
  `IORING_CQE_F_BUFFER`).
- Kernel commit `c7fb19428d67` ("io_uring: add support for ring mapped
  supplied buffers", merged for Linux 5.19). Verify against the
  upstream `linux.git` log before citing in code comments.
- Multishot opcodes (kernel 6.0+): `IORING_OP_RECV_MULTISHOT`,
  `IORING_OP_READ_MULTISHOT`. Opcode IDs and exact min-kernel match
  must be reverified via `IORING_REGISTER_PROBE` at runtime, not
  hard-coded from this audit.
