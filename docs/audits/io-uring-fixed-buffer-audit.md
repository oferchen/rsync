# io_uring fixed-buffer registration audit

Tracking issue: oc-rsync #2118. Sibling audits:
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md),
[`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md),
[`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md),
[`docs/audits/iouring-socket-sqpoll-defer-taskrun.md`](iouring-socket-sqpoll-defer-taskrun.md),
[`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md).

## Scope

Audits how `crates/fast_io/src/io_uring/` uses `IORING_REGISTER_BUFFERS`
and the `IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` opcode pair. The
goal is a single source of truth for: where pinned buffers are
registered, where fixed opcodes are used vs unfixed, the buffer
lifecycle, the sizing constants, code paths that should plausibly use
fixed buffers but currently do not, and the relationship to the
provided-buffer-ring (`IORING_REGISTER_PBUF_RING`, kernel 5.19+) work
tracked elsewhere.

Source files inspected (all paths repository-relative):

- `crates/fast_io/src/io_uring/registered_buffers.rs` - the pool
  itself, `RegisteredBufferGroup`, `submit_read_fixed_batch`,
  `submit_write_fixed_batch`.
- `crates/fast_io/src/io_uring/file_reader.rs` - `READ_FIXED` call site.
- `crates/fast_io/src/io_uring/file_writer.rs` - `WRITE_FIXED` call sites.
- `crates/fast_io/src/io_uring/shared_ring.rs` - shared reader/writer
  ring; registers a pool but submits only unfixed `Read`/`Send`.
- `crates/fast_io/src/io_uring/disk_batch.rs` - batched disk commit
  writer; does not use fixed buffers.
- `crates/fast_io/src/io_uring/socket_reader.rs`,
  `crates/fast_io/src/io_uring/socket_writer.rs` - unfixed `Recv`/`Send`.
- `crates/fast_io/src/io_uring/buffer_ring.rs` - `PBUF_RING` (separate
  kernel namespace, currently unwired).
- `crates/fast_io/src/io_uring/config.rs` - sizing knobs and defaults.
- `crates/fast_io/src/io_uring/mod.rs` - public re-exports and
  fallback flow.
- `crates/fast_io/src/io_uring/batching.rs` - shared SQE helpers
  (`maybe_fixed_file`, `submit_write_batch`, `try_register_fd`).

Upstream evidence: a recursive search for `io_uring`, `IORING_`,
`IOSQE_BUFFER_SELECT`, and `register_buffers` in
`target/interop/upstream-src/rsync-3.4.1/` returns no matches. Upstream
rsync 3.4.1 has no io_uring path; everything below is purely an
oc-rsync local optimisation with no wire-protocol implication.

## TL;DR

`RegisteredBufferGroup` is the single mechanism backing
`IORING_OP_READ_FIXED` / `IORING_OP_WRITE_FIXED` in oc-rsync. It is
allocated speculatively (best-effort) by `IoUringReader`,
`IoUringWriter`, and `SharedRing`; on registration failure the owner
keeps `registered_buffers = None` and the read / write paths fall back
to plain `IORING_OP_READ` / `IORING_OP_WRITE`. Defaults: `count = 8`
slots, `buffer_size = 64 KiB`, page-aligned, hard cap 1024 (kernel
limit). Lifecycle is owner-scoped: registration runs once at
construction; `Drop` deallocates user-side memory while the ring fd
close releases kernel pinning. There is no rebind, recycle, or growth
path today; adaptive resize is a separate follow-up tracked in
`io-uring-adaptive-buffer-sizing.md`.

Three known gaps where fixed buffers are not used today even though the
plumbing exists:

1. `SharedRing::submit_read` and `SharedRing::submit_send`
   intentionally use the unregistered opcodes despite owning a
   `RegisteredBufferGroup` (the group is stored but never checked
   out).
2. `IoUringDiskBatch` carries no `RegisteredBufferGroup` at all - it
   reuses one ring across many files but submits via the unfixed
   `submit_write_batch` helper.
3. `IoUringSocketReader` / `IoUringSocketWriter` use unfixed
   `IORING_OP_RECV` / `IORING_OP_SEND` and do not opt into
   `IORING_OP_SEND_ZC`; the `ZeroCopyPolicy::Auto` default is
   intentionally `IORING_OP_SEND` until `SEND_ZC` is wired through
   the registered-buffer ring.

`PBUF_RING` (`IORING_REGISTER_PBUF_RING`, kernel 5.19+) is implemented
in `buffer_ring.rs` and re-exported from `mod.rs` / `lib.rs`, but no
caller constructs a `BufferRing` outside the module's own unit tests.
The bgid namespace is therefore a documented design space, not a
runtime concern; see [`io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md).

## 1. Registration sites

`IORING_REGISTER_BUFFERS` is invoked from exactly one function. Every
other registration in the codebase routes through it:

- `RegisteredBufferGroup::new` -
  `crates/fast_io/src/io_uring/registered_buffers.rs:251` allocates
  `count` page-aligned buffers via `std::alloc::alloc_zeroed` with
  `Layout::from_size_align(aligned_size, page_size())`, builds a
  `Vec<libc::iovec>`, and calls
  `ring.submitter().register_buffers(&iovecs)` at line 307. The
  liburing-equivalent syscall is `io_uring_register(fd,
  IORING_REGISTER_BUFFERS, iovecs, count)`. The `unsafe` block at
  line 305-307 is the only direct `register_buffers` callsite in the
  workspace.
- `RegisteredBufferGroup::try_new` -
  `crates/fast_io/src/io_uring/registered_buffers.rs:352` is a
  best-effort wrapper that returns `None` on failure. This is what
  every owner uses, so an `EINVAL` / `ENOMEM` / seccomp rejection
  silently degrades to the unfixed code path.

Owners (all best-effort via `try_new`):

| Owner | File:line | When |
|---|---|---|
| `IoUringReader::open` | `crates/fast_io/src/io_uring/file_reader.rs:73-81` | Once per file open, when `config.register_buffers` is true. |
| `IoUringWriter::create` | `crates/fast_io/src/io_uring/file_writer.rs:56-64` | Once per `File::create`. |
| `IoUringWriter::from_file` | `crates/fast_io/src/io_uring/file_writer.rs:83-91` | Once per existing `File` wrap. |
| `IoUringWriter::with_ring` | `crates/fast_io/src/io_uring/file_writer.rs:118` | Used by `writer_from_file` in `mod.rs:184`; hard-codes `count = 8` (does not read `IoUringConfig::registered_buffer_count`). |
| `IoUringWriter::create_with_size` | `crates/fast_io/src/io_uring/file_writer.rs:143-151` | Pre-allocated file create. |
| `SharedRing::new_inner` | `crates/fast_io/src/io_uring/shared_ring.rs:267-275` | Registers a group but never checks it out (see Section 5). |

The kernel-side `IORING_REGISTER_BUFFERS` opcode is not referenced by
its raw integer value anywhere in oc-rsync; the
`io_uring::Submitter::register_buffers` wrapper from the `io_uring`
crate (3.x) issues the syscall on our behalf. This contrasts with
`buffer_ring.rs` which encodes the raw opcodes
(`IORING_REGISTER_PBUF_RING = 22`,
`IORING_UNREGISTER_PBUF_RING = 23`) directly because the `io_uring`
crate had no wrapper for PBUF_RING at the time the code was written.

## 2. Fixed vs unfixed opcode usage

`IORING_OP_READ_FIXED` and `IORING_OP_WRITE_FIXED` are issued only
through the helper functions in `registered_buffers.rs`; every other
read / write SQE in the module uses the unfixed `Read`, `Write`,
`Recv`, or `Send` opcode.

### Fixed paths

- `submit_read_fixed_batch` -
  `crates/fast_io/src/io_uring/registered_buffers.rs:498-610`. Builds
  `ReadFixed::new(fd, slot.ptr, want as u32, slot.buf_index)` SQEs
  (line 531), one per checked-out slot per round. Drives short-read
  detection by tracking requested vs actual bytes per SQE
  (line 595-601). Single caller: `IoUringReader::read_all_batched`
  at `crates/fast_io/src/io_uring/file_reader.rs:158-184`.
- `submit_write_fixed_batch` -
  `crates/fast_io/src/io_uring/registered_buffers.rs:617-701`. Copies
  the caller's `data` into each slot, then submits
  `WriteFixed::new(fd, slot.ptr, want as u32, slot.buf_index)`
  (line 652). Two callers, both inside `file_writer.rs`:
  `write_all_batched` (line 215-246) and `flush_buffer`
  (line 282-309).

Both helpers select `slot_count = reg.available().min(self.sq_entries
as usize)` and check out every available slot. If `available()` is
zero, the caller falls through to the unfixed path within the same
function.

### Unfixed paths (intentional or by-design)

- `IoUringReader::read_at` -
  `crates/fast_io/src/io_uring/file_reader.rs:111` uses
  `opcode::Read::new` because the caller supplies an arbitrary
  `&mut [u8]` whose memory is not registered. Used for one-off
  positioned reads.
- `IoUringReader::read_all_batched` fallback -
  `crates/fast_io/src/io_uring/file_reader.rs:186-275` is the
  fallback when `registered_buffers` is `None` or every slot is in
  use. Submits `opcode::Read` SQEs directly into the caller's
  output `Vec<u8>`.
- `IoUringWriter::write_at` -
  `crates/fast_io/src/io_uring/file_writer.rs:177` uses
  `opcode::Write::new` for the same reason as `read_at`: arbitrary
  caller buffer.
- `IoUringWriter::write_all_batched` and `flush_buffer` fallbacks
  call `submit_write_batch` in
  `crates/fast_io/src/io_uring/batching.rs:53-152` which submits
  `opcode::Write::new(fd, data[start + done..].as_ptr(), ...)`
  (line 91).
- `IoUringWriter::sync` - `file_writer.rs:371` submits
  `opcode::Fsync::new(fd)`. Fsync has no buffer to register; this is
  the correct opcode.
- `SharedRing::submit_read` -
  `crates/fast_io/src/io_uring/shared_ring.rs:329-346` uses
  `opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)` even
  though `SharedRing` owns a `RegisteredBufferGroup`. The doc-comment
  at line 320-322 records the intent: the registered-buffer fast path
  is reserved for higher-level batched submitters that own the buffer
  pool.
- `SharedRing::submit_send` -
  `crates/fast_io/src/io_uring/shared_ring.rs:383-398` uses
  `opcode::Send::new`, similarly intentional.
- `IoUringDiskBatch::flush_buffer` -
  `crates/fast_io/src/io_uring/disk_batch.rs` calls `submit_write_batch`
  unconditionally (no `RegisteredBufferGroup` field). See Section 5.
- `IoUringSocketReader::recv_into` and friends -
  `crates/fast_io/src/io_uring/socket_reader.rs:49,87` submit
  `opcode::Recv::new`. There is no `RegisteredBufferGroup` field on
  `IoUringSocketReader`.
- `IoUringSocketWriter` - `crates/fast_io/src/io_uring/socket_writer.rs`
  uses `opcode::Send::new`. The `ZeroCopyPolicy::Auto` default at
  `crates/fast_io/src/io_uring/config.rs:339` deliberately stays on
  `IORING_OP_SEND`; flipping to `SEND_ZC` is gated on wiring through
  the registered-buffer ring (see comment at `config.rs:332-339`).

### Quick reference

| Submission site | Opcode | Buffer source |
|---|---|---|
| `IoUringReader::read_all_batched` (registered path) | `READ_FIXED` | `RegisteredBufferGroup` slot, copied to caller `output` after CQE |
| `IoUringReader::read_all_batched` (fallback) | `Read` | Caller's `Vec<u8>` directly |
| `IoUringReader::read_at` | `Read` | Caller's `&mut [u8]` |
| `IoUringWriter::write_all_batched` (registered path) | `WRITE_FIXED` | `RegisteredBufferGroup` slot, filled by `ptr::copy_nonoverlapping` from caller `data` |
| `IoUringWriter::flush_buffer` (registered path) | `WRITE_FIXED` | Same, sourced from internal `self.buffer` |
| `IoUringWriter::write_all_batched` (fallback) | `Write` | Caller's `&[u8]` |
| `IoUringWriter::flush_buffer` (fallback) | `Write` | `self.buffer[..len]` |
| `IoUringWriter::write_at` | `Write` | Caller's `&[u8]` |
| `IoUringWriter::sync` | `Fsync` | n/a |
| `SharedRing::submit_read` | `Read` (unfixed) | Caller's `&mut [u8]` |
| `SharedRing::submit_send` | `Send` (unfixed) | Caller's `&[u8]` |
| `SharedRing::submit_poll_write` | `PollAdd` | n/a |
| `IoUringDiskBatch::flush_buffer` | `Write` (unfixed) | Internal `self.buffer` |
| `IoUringSocketReader` | `Recv` (unfixed) | Internal or caller buffer |
| `IoUringSocketWriter` | `Send` (unfixed) | Caller's `&[u8]` |

## 3. Buffer lifecycle

The lifecycle is documented in detail in the module docs at
`crates/fast_io/src/io_uring/registered_buffers.rs:1-67`. The five
phases:

1. **Allocate** - `RegisteredBufferGroup::new` calls
   `std::alloc::alloc_zeroed(layout)` for each of `count` buffers.
   `layout` is computed at line 273 as
   `Layout::from_size_align(aligned_size, page_size)` where
   `aligned_size = buffer_size.next_multiple_of(page_size())` and
   `page_size()` queries `libc::sysconf(_SC_PAGESIZE)` with a 4 KiB
   fallback (line 479-487). Page alignment is required so that the
   kernel can pin whole pages without splitting; a non-aligned base
   pointer would cause `register_buffers` to fail with `EFAULT` or
   `EINVAL` on some kernels.
2. **Register** - One `register_buffers` call covers all `count`
   buffers via the `iovec` array (line 307). The kernel pins these
   pages and assigns them indices `0..count`.
3. **Checkout** - `RegisteredBufferGroup::checkout` (line 387-414) is
   a lock-free atomic-bitset claim. The bitset is
   `Vec<AtomicU64>::div_ceil(count, 64)` words (line 318), each bit
   set to 1 means "free". Claiming a slot is a `compare_exchange_weak`
   on the trailing-zeros bit; a contended CAS retries the same word.
   Returns a `RegisteredBufferSlot<'_>` that holds `&self` and the
   index.
4. **Return** - `RegisteredBufferSlot::Drop` (line 232-236) calls
   `RegisteredBufferGroup::return_slot` (line 436-441), a
   `fetch_or(mask, Release)` on the appropriate bitset word. Returns
   are unconditional and panic-safe; a panic during slot use unwinds
   through `Drop` and frees the slot, asserted by
   `panic_during_slot_use_unwinds_cleanly` at line 1126-1154.
5. **Deregister** - Two paths:
   - **Implicit (default)**: when the parent ring's `RawIoUring::Drop`
     closes the ring fd, the kernel runs
     `fs/io_uring.c:io_sqe_buffers_unregister` and releases the
     pinning. Then `RegisteredBufferGroup::Drop` (line 453-476) frees
     the user-side memory via `std::alloc::dealloc`. Owners that hold
     both a ring and a group declare them in this order
     (`ring`, then `registered_buffers`) so that field-drop order
     matches the documented invariant - asserted by
     `struct_field_drop_order_matches_callers` at line 1098-1121.
   - **Explicit**: callers may invoke
     `RegisteredBufferGroup::unregister(&ring)` (line 448-450) which
     calls `submitter.unregister_buffers()`. Useful when a caller
     wants to release pinning before dropping the ring (e.g. to
     register a different buffer set). The wrapper surfaces the
     kernel's error code as `io::Result<()>`; a second unregister
     against the same group typically returns `EINVAL` and must not
     panic
     (`unregister_after_ring_closed_returns_error_or_ok` at line 1159-1182).

`Drop` does not call `unregister_buffers` itself. The module docs at
line 41-49 record the rationale: holding a `&RawIoUring` reference
inside the group would force a lifetime coupling that prevents the
ring from being dropped first. `unregister_buffers` is therefore
caller-driven only; the kernel always cleans up at ring fd close.

The two reorder cases are tested:

- `drop_group_before_ring_does_not_panic` (line 1043-1070) verifies
  that dropping the group while the ring is still alive leaves the
  ring usable for a `Nop` submission. The kernel still owns the
  pinning at this point; it releases it only when the ring fd later
  closes. No leak occurs.
- `drop_ring_before_group_frees_memory_cleanly` (line 1077-1093)
  verifies the natural order: ring fd closes (kernel releases
  pinning), then group drops (frees user-side memory).

`SIGKILL` and other forced exits skip every Drop. Both the ring fd
and the pinned pages are reclaimed by normal kernel process teardown.
This is documented at `registered_buffers.rs:58-62` and is the
standard io_uring contract.

## 4. Pool sizing decisions

All sizing constants live in `IoUringConfig`
(`crates/fast_io/src/io_uring/config.rs`) and in
`MAX_REGISTERED_BUFFERS` in the registered-buffers module:

| Knob | File:line | Default | `for_large_files()` | `for_small_files()` |
|---|---|---|---|---|
| `register_buffers` | `config.rs:323` | `true` | `true` | `true` |
| `registered_buffer_count` | `config.rs:326` | `8` | `16` | `8` |
| `buffer_size` | `config.rs:346` | `64 KiB` | `256 KiB` | `16 KiB` |
| `sq_entries` | `config.rs:345` | `64` | `256` | `128` |
| `MAX_REGISTERED_BUFFERS` | `registered_buffers.rs:80` | `1024` | `1024` | `1024` |

Defaults at `config.rs:342-356`. Variants at `config.rs:358-389`.

The page alignment in `RegisteredBufferGroup::new` rounds
`buffer_size` up to the next multiple of `_SC_PAGESIZE` via
`buffer_size.next_multiple_of(page_size)` (line 271-272). For the
default 64 KiB on a 4 KiB-page system this is a no-op; for the small
preset (16 KiB) or any non-aligned override, the rounded size is
returned by `buffer_size()` (line 366) so callers see the post-align
size, not the configured value.

Validation is light: zero count and zero size are rejected with
`InvalidInput`, and a count above `MAX_REGISTERED_BUFFERS` is
rejected before issuing the syscall (line 252-269). `buffer_size`
itself is otherwise unconstrained; values too large for the kernel's
`RLIMIT_MEMLOCK` are surfaced via the `register_buffers` error
return.

Total pinned memory at the defaults: `8 buffers * 64 KiB = 512 KiB`
per `IoUringReader` / `IoUringWriter` / `SharedRing`. The
`for_large_files` preset rises to `16 * 256 KiB = 4 MiB`; the
`for_small_files` preset is `8 * 16 KiB = 128 KiB`. These figures
are per-owner; in `crates/fast_io/src/io_uring/file_factory.rs` and
`mod.rs` each opened file or wrapped fd creates its own ring and its
own group, so concurrent transfers multiply this footprint linearly.
The ceiling at `count = 1024` is a kernel limit, not a policy cap.

`with_ring` at `file_writer.rs:118` deviates from the configured
sizing: it hard-codes `RegisteredBufferGroup::try_new(&ring,
buffer_capacity, 8)`. This call site is reached from
`mod.rs::writer_from_file_with_depth` (line 166-218) which builds a
`config` locally but only forwards `sq_entries` to `with_ring`. As a
result, callers of `writer_from_file` cannot influence
`registered_buffer_count` today; the default 8 is the only
permitted value. This is recorded as a known minor inconsistency,
not a correctness issue. A follow-up could extend `with_ring`'s
parameter list once we have a reason to vary the count from this
entry point.

The presets are a fixed-step staircase, not a dynamic policy. There
is no measurement loop that resizes the pool based on miss rate
today; the telemetry needed for one was added recently
(`RegisteredBufferStats { total_acquires, total_misses }` and
`miss_rate()` at `registered_buffers.rs:144-163`) and the design for
the resize loop is in
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md).

## 5. Code paths that should use fixed buffers but don't

Three deliberate gaps exist today. Each is documented inline; this
section records the reasoning so future readers do not file dupe
issues.

### 5a. `SharedRing` registers but does not consume

`SharedRing` owns a `RegisteredBufferGroup`
(`shared_ring.rs:205,267-275`) and exposes `has_registered_buffers`
(line 298) but `submit_read` and `submit_send` use the unfixed
opcodes
(`shared_ring.rs:329-346,383-398`). The doc-comment at line 320-322
explains: the registered-buffer fast path is reserved for batched
submitters that own the buffer pool. `SharedRing`'s API takes a
caller-owned `&mut [u8]` per submission; copying that into a slot
before submitting and back out after the CQE would defeat the point
of `read_at` / `submit_read` being a one-shot positioned op.

The intended consumer is a future batched session-level submitter
that drives `SharedRing` and owns its own slot bookkeeping. Until
that lands, the registration is essentially preallocation: the
kernel pinning is paid up-front so a later wiring PR can flip
batched paths to `READ_FIXED` / `WRITE_FIXED` without a registration
cycle. We could plausibly skip registration in `SharedRing` until
the consumer lands; keeping it preserves the API shape and the
asserts in `has_registered_buffers`.

### 5b. `IoUringDiskBatch` has no registered group

`IoUringDiskBatch::flush_buffer`
(`crates/fast_io/src/io_uring/disk_batch.rs`) reuses one ring across
many file open / close cycles via
[`disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md).
The struct (`disk_batch.rs:45-54`) carries `ring`, `config`,
`current_file`, `buffer`, `buffer_pos` but no
`Option<RegisteredBufferGroup>`. Every flush calls
`submit_write_batch` (the unfixed helper). This is a real
optimisation gap, not a deliberate choice. Wiring registered buffers
into the disk-batch path should be straightforward:

1. Add `registered_buffers: Option<RegisteredBufferGroup>` to
   `IoUringDiskBatch`, constructed via `try_new` once in `new` /
   `try_new` (`disk_batch.rs:65-90`) using
   `config.buffer_size` and `config.registered_buffer_count`.
2. In `flush_buffer`, mirror the `WRITE_FIXED` branch from
   `IoUringWriter::flush_buffer` (`file_writer.rs:282-308`): if
   `available() > 0`, build slot infos and call
   `submit_write_fixed_batch`; otherwise fall back to
   `submit_write_batch`.
3. Drop ordering: the existing field order
   (`ring` first, then `current_file`, `buffer`, `buffer_pos`)
   already places the ring before any user-side memory; the new
   `registered_buffers` field must be the last field so the ring's
   `Drop` runs first and the kernel releases pinning before the
   group's `Drop` deallocates.

This has not been done because the disk-batch path historically
focused on amortising ring construction; the registered-buffer
optimisation is orthogonal and was not part of the original
PR. Tracked alongside this audit.

### 5c. Socket I/O is unfixed; `SEND_ZC` is gated

`IoUringSocketReader` and `IoUringSocketWriter`
(`crates/fast_io/src/io_uring/socket_reader.rs`,
`crates/fast_io/src/io_uring/socket_writer.rs`) submit
`opcode::Recv` and `opcode::Send` against caller-owned buffers. The
`ZeroCopyPolicy::Auto` default at `config.rs:333-339` selects
`IORING_OP_SEND` over `IORING_OP_SEND_ZC` (Linux 6.0+) because
`SEND_ZC` only outperforms `SEND` on large pinned-buffer transfers
and we have not yet wired send paths through registered buffers.
This is a deliberate placeholder: when the registered-buffer ring
is integrated with socket sends, `Auto` will flip and the policy
table will need a kernel-version probe similar to the one in
`buffer_ring.rs:602-610`.

`Recv` is not a candidate for `RECV_FIXED` today because the
session protocol code reads into caller-supplied buffers whose
lifetimes are bound to the protocol state machine; a slot-based
indirection would require a copy on every receive, which is the
opposite of what fixed buffers exist to avoid.

## 6. PBUF_RING (`IORING_REGISTER_PBUF_RING`, kernel 5.19+) interaction

`PBUF_RING` is implemented in
`crates/fast_io/src/io_uring/buffer_ring.rs:1-900` and re-exported
from `mod.rs:103` and `lib.rs:155`, but no caller constructs a
`BufferRing` outside the module's own tests. The full picture is in
[`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md) and the
namespace analysis in
[`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md).

PBUF_RING and `IORING_REGISTER_BUFFERS` are different mechanisms in
different kernel namespaces. The contrast that matters for this
audit:

| Aspect | `IORING_REGISTER_BUFFERS` (this audit) | `IORING_REGISTER_PBUF_RING` |
|---|---|---|
| Indexed by | Slot index (`u16`), 0..count | Buffer-group ID (`bgid`, `u16`) |
| Cap | 1024 buffers per ring | 65 536 bgids per ring |
| Selection | Caller picks slot at submit time and passes `buf_index` to `READ_FIXED` / `WRITE_FIXED` | Kernel picks a buffer from the ring at submit time when `IOSQE_BUFFER_SELECT` is set; CQE reports the chosen `bid` |
| Buffer ownership | Pinned at register time, slot lifetime bound to caller's checkout | Returned to the kernel via the user-mapped ring; kernel reuses across operations |
| Min kernel | 5.6 | 5.19 |
| Best fit | Sequential I/O with known per-op buffer sizing | Receive-side workloads where the kernel knows demand better than the caller |
| Status in oc-rsync | Wired (this audit) | Unwired; `BufferRing` exists for a future receive-side path |

The two mechanisms are complementary, not alternatives. A future
session-level submitter that consumes `SharedRing` for socket reads
would plausibly use `PBUF_RING` (kernel-driven receive buffer
selection) for `Recv` while keeping `IORING_REGISTER_BUFFERS` for
file `READ_FIXED` / `WRITE_FIXED`. Mixing both on the same ring is
allowed by the kernel: the slot index namespace and the bgid
namespace are independent xarrays
(`io_uring/io_uring.c::io_sqe_buffers_register` vs
`io_uring/kbuf.c::io_register_pbuf_ring`).

`RegisteredBufferGroup` does not interact with PBUF_RING today and
must not be conflated with it. `MAX_REGISTERED_BUFFERS = 1024` is the
slot-table cap; `BufferRingConfig::bgid` is in a different namespace
and currently unused. Cross-checked in the bgid audit at
[`io-uring-bgid-namespace.md:81-87`](io-uring-bgid-namespace.md).

## 7. `bgid` namespace usage

oc-rsync allocates **zero** `bgid` values in production code today.
The `BufferRing` / `BufferRingConfig` types in
`crates/fast_io/src/io_uring/buffer_ring.rs` are the only place
`bgid` is referenced; no engine, transfer, daemon, or transport
code path constructs a `BufferRing`. The default
`BufferRingConfig::bgid = 0` is reserved for the primary buffer
ring on each `RawIoUring`; `1..=15` is informally reserved for
multi-tier buffer sizes if that ever lands.

The bgid namespace is per-ring (each `RawIoUring` has its own
`ctx->io_bl_xa` xarray in
`io_uring/kbuf.c::io_register_pbuf_ring`). Because oc-rsync builds
one ring per file / fd / direction (see the per-ring scope table in
[`io-uring-bgid-namespace.md:90-114`](io-uring-bgid-namespace.md)),
the realistic bgid count per ring is 1 once the wiring lands and
the 16-bit width is irrelevant. The namespace audit recommends
"option (a) document the limit, assert not a concern" with three
guard rails:

1. One `bgid` per ring; hard-code `0` for the primary ring.
2. Tie `BufferRing` lifetime to its parent `RawIoUring`; `Drop`
   already calls `IORING_UNREGISTER_PBUF_RING`.
3. Add a `tracing::warn!` if a single `RawIoUring` ever crosses
   256 live buffer groups.

Until `BufferRing` is wired, none of those apply. This audit's
contribution: confirm that **`RegisteredBufferGroup`'s slot-index
namespace is disjoint from the bgid namespace**. The 1024 cap on
slots and the 65 536 cap on bgids are independent kernel limits;
exhausting one says nothing about the other. Code comments and
future PRs must avoid the conflation.

## Recommendation summary

1. **Section 5b is the only actionable gap in this PR's scope.**
   Wire `RegisteredBufferGroup` into `IoUringDiskBatch` so the disk
   commit path uses `WRITE_FIXED` like every other writer. Tracked
   alongside this audit; not landing in the same PR.
2. **Sections 5a and 5c are deliberate; document them rather than
   change them.** The `SharedRing` registration is a forward
   declaration for a batched session submitter; flipping
   `ZeroCopyPolicy::Auto` to `SEND_ZC` is gated on a separate
   wiring PR that registers send buffers and probes Linux 6.0+.
3. **Sizing should remain static for the immediate term.** The
   adaptive-resize design lives in
   [`io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md);
   the telemetry that feeds it (`RegisteredBufferStats`) is already
   in place.
4. **Keep the slot-index and bgid namespaces strictly separate in
   docs and code comments.** Conflating them has been observed in
   external write-ups; the cross-references between this audit, the
   bgid audit, and the PBUF_RING audit are intentional.
5. **The `with_ring` hard-coded `count = 8`
   (`file_writer.rs:118`) is a minor wart.** Threading the
   configured `registered_buffer_count` through
   `writer_from_file_with_depth` is a one-line change worth doing
   when that entry point next gets touched.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no io_uring usage; everything in this audit is purely an oc-rsync
  local optimisation).
- oc-rsync code:
  - `crates/fast_io/src/io_uring/registered_buffers.rs:80` -
    `MAX_REGISTERED_BUFFERS = 1024` (kernel slot-index cap).
  - `crates/fast_io/src/io_uring/registered_buffers.rs:251-345` -
    `RegisteredBufferGroup::new` (only `register_buffers` callsite).
  - `crates/fast_io/src/io_uring/registered_buffers.rs:387-414` -
    lock-free `checkout` via atomic bitset.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:436-441` -
    slot return.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:448-450` -
    explicit `unregister`.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:453-476` -
    `Drop` (user-side dealloc only; kernel pinning released by ring
    fd close).
  - `crates/fast_io/src/io_uring/registered_buffers.rs:498-610` -
    `submit_read_fixed_batch`.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:617-701` -
    `submit_write_fixed_batch`.
  - `crates/fast_io/src/io_uring/file_reader.rs:73-81,158-184` -
    `READ_FIXED` registration and call site.
  - `crates/fast_io/src/io_uring/file_writer.rs:56-64,83-91,118,143-151,215-246,282-309` -
    `WRITE_FIXED` registration and call sites.
  - `crates/fast_io/src/io_uring/shared_ring.rs:205,267-275,298-300,329-346,383-398` -
    registration without consumption (Section 5a).
  - `crates/fast_io/src/io_uring/disk_batch.rs:45-90` -
    no registered group (Section 5b).
  - `crates/fast_io/src/io_uring/socket_reader.rs:49,87` -
    unfixed `Recv` (Section 5c).
  - `crates/fast_io/src/io_uring/socket_writer.rs` and
    `crates/fast_io/src/io_uring/config.rs:327-339` -
    `ZeroCopyPolicy::Auto` rationale.
  - `crates/fast_io/src/io_uring/buffer_ring.rs:40-50,213-460,516-554` -
    `PBUF_RING` (kernel 5.19+, separate namespace).
  - `crates/fast_io/src/io_uring/config.rs:323-389` -
    `register_buffers`, `registered_buffer_count`, presets.
  - `crates/fast_io/src/io_uring/mod.rs:67,89,117,166-218` -
    public re-exports and `writer_from_file` flow.
  - `crates/fast_io/src/io_uring/batching.rs:53-152` -
    `submit_write_batch` (unfixed fallback).
- Sibling audits:
  - [`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
    - telemetry + EMA design for resizing the registered pool.
  - [`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
    - bgid namespace exhaustion analysis.
  - [`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md) -
    PBUF_RING design for receive paths.
  - [`docs/audits/iouring-socket-sqpoll-defer-taskrun.md`](iouring-socket-sqpoll-defer-taskrun.md)
    - per-connection ring lifecycle.
  - [`docs/audits/disk-commit-iouring-batching.md`](disk-commit-iouring-batching.md)
    - batching strategy for the disk commit path.
- Linux man pages and kernel sources (verify against running kernel
  before citing in code):
  - `man 2 io_uring_register` - documents `IORING_REGISTER_BUFFERS`,
    `IORING_UNREGISTER_BUFFERS`, the `iovec` payload, and the
    1024 slot cap.
  - `man 7 io_uring` - overview and opcode list including
    `IORING_OP_READ_FIXED`, `IORING_OP_WRITE_FIXED`.
  - `io_uring/io_uring.c::io_sqe_buffers_register` - kernel-side
    register handler.
  - `io_uring/io_uring.c::io_sqe_buffers_unregister` - kernel-side
    unregister handler invoked by ring-fd close.
  - `io_uring/kbuf.c::io_register_pbuf_ring` - separate PBUF_RING
    handler (different namespace).
