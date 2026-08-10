# io_uring `bgid` namespace exhaustion audit

Tracking issue: oc-rsync task #2044. Sibling audits:
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md),
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md),
[`docs/audits/iouring-socket-sqpoll-defer-taskrun.md`](iouring-socket-sqpoll-defer-taskrun.md).

## Summary

Issue #2044 asks whether io_uring's 16-bit buffer-group identifier (`bgid`,
`u16`) can be exhausted by oc-rsync at scale. The kernel keeps the bgid
namespace **per io_uring instance**, not per-process: each `IoUring` ring
has its own `0..=u16::MAX` (65 536) slot table maintained by
`io_uring/kbuf.c` (`io_register_pbuf_ring()` / `io_buffer_select()`). Today
oc-rsync allocates **zero** `bgid` values in production: the
`BufferRing` / `BufferRingConfig` types in
[`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
exist, are publicly re-exported from
[`crates/fast_io/src/io_uring/mod.rs:100`](../../crates/fast_io/src/io_uring/mod.rs)
and [`crates/fast_io/src/lib.rs:155`](../../crates/fast_io/src/lib.rs), but
no caller constructs a `BufferRing` outside this crate's own unit tests.
The wiring is covered by the (still pending) provided-buffer-ring
integration work; until that lands, bgid exhaustion is not a runtime risk.

The recommended action when wiring lands is **option (a) document and
assert "not a concern"**: pin one bgid per ring (the `BufferRingConfig::bgid`
field is already there for that), keep ring lifetime bound to the file or
socket lifetime, and rely on the per-ring scope to make exhaustion
arithmetically impossible. A freelist allocator is unnecessary because
oc-rsync's design path never accumulates many bgids inside the same ring.
This audit records the reasoning so that a future change which spawns many
buffer groups inside a single ring revisits the conclusion.

Upstream evidence: a recursive search for `io_uring`, `IORING_`,
`IOSQE_BUFFER_SELECT`, and `bgid` in
`target/interop/upstream-src/rsync-3.4.1/` returns no matches. Upstream
rsync does not use io_uring at all; bgid is purely an oc-rsync side
concern with no wire-protocol implication.

## Where `bgid` lives in oc-rsync today

A grep of `bgid|BufferGroup|BufferRing|register_buf_ring|IORING_REGISTER_PBUF_RING|IOSQE_BUFFER_SELECT`
across the workspace returns matches in only two source files plus the
non-Linux stub:

- [`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs):
  - `IoUringBufReg.bgid: u16` at line 63 mirrors `struct io_uring_buf_reg`
    from the kernel uABI.
  - `BufferRingConfig.bgid: u16` at line 151 is the public configuration
    field (`Default::default().bgid == 0`, line 159).
  - `BufferRing::new` writes the configured `bgid` into the
    `IoUringBufReg` payload at line 363 before the
    `SYS_io_uring_register(IORING_REGISTER_PBUF_RING)` call (lines
    369-378). The opcode constant is defined at line 41
    (`IORING_REGISTER_PBUF_RING = 22`) and the unregister opcode at
    line 44 (`IORING_UNREGISTER_PBUF_RING = 23`).
  - `BufferRing::bgid()` at line 414 exposes the configured group ID for
    SQE construction with `IOSQE_BUFFER_SELECT`.
  - `Drop` for `BufferRing` (line 516) copies `bgid` into the
    unregister payload at line 522 to release the kernel-side slot.
- [`crates/fast_io/src/io_uring/mod.rs:100`](../../crates/fast_io/src/io_uring/mod.rs)
  re-exports `BufferRing`, `BufferRingConfig`, `BufferRingError`,
  `buffer_id_from_cqe_flags`. The mention is purely a re-export list.
- [`crates/fast_io/src/lib.rs:155`](../../crates/fast_io/src/lib.rs)
  re-exports the same three types up to crate root.
- [`crates/fast_io/src/io_uring_stub.rs`](../../crates/fast_io/src/io_uring_stub.rs)
  provides a no-op `BufferRing` for non-Linux / `feature = "io_uring"`-
  off builds. The stub `BufferRingConfig` keeps the `bgid: u16` field at
  line 146 for ABI parity with the Linux module; `BufferRing::new`
  always returns `Err(BufferRingError::Unsupported)` (line 171).

A search for actual call-sites (`BufferRing::new`, `BufferRing::try_new`,
`BufferRingConfig {`) returns matches **only** inside the unit tests in
`buffer_ring.rs` and the stub tests in `io_uring_stub.rs`. No engine,
transfer, transport, or daemon code path constructs a `BufferRing` today.
This matches the architectural note in
[`docs/audits/iouring-pipe-stdio.md`](iouring-pipe-stdio.md) under
"Buffer pool integration", which lists PBUF_RING as a phase-2
enhancement, not a phase-1 dependency.

The pre-existing `RegisteredBufferGroup`
([`crates/fast_io/src/io_uring/registered_buffers.rs:80`](../../crates/fast_io/src/io_uring/registered_buffers.rs))
is a different mechanism: it backs `IORING_OP_READ_FIXED` /
`IORING_OP_WRITE_FIXED` via `IORING_REGISTER_BUFFERS`, indexed by a
slot index (`u16`), which is **not** a `bgid` and lives in a separate
kernel namespace (capped at `MAX_REGISTERED_BUFFERS = 1024`). Conflating
the two would be incorrect.

## How rings are scoped in oc-rsync

The bgid namespace is per-ring, so the practical exhaustion question
reduces to "how many `BufferRing`s can a single `RawIoUring` host?" The
ring construction sites are:

- [`crates/fast_io/src/io_uring/file_reader.rs:60`](../../crates/fast_io/src/io_uring/file_reader.rs)
  - `IoUringReader::open` calls `config.build_ring()`. One ring per file
    reader.
- [`crates/fast_io/src/io_uring/file_writer.rs:54,81,141`](../../crates/fast_io/src/io_uring/file_writer.rs)
  - `IoUringWriter` constructors. One ring per file writer.
- [`crates/fast_io/src/io_uring/socket_reader.rs:32`](../../crates/fast_io/src/io_uring/socket_reader.rs)
  - `IoUringSocketReader::from_raw_fd` calls `config.build_ring()`. One
    ring per socket reader (per connection direction).
- [`crates/fast_io/src/io_uring/socket_writer.rs:32`](../../crates/fast_io/src/io_uring/socket_writer.rs)
  - `IoUringSocketWriter::from_raw_fd`. One ring per socket writer.
- [`crates/fast_io/src/io_uring/disk_batch.rs:71`](../../crates/fast_io/src/io_uring/disk_batch.rs)
  - `IoUringDiskBatch` carries one ring (`ring: RawIoUring`, line 46).

Every reader / writer / batch helper owns its own `RawIoUring`, so each
fd / direction has its own bgid namespace. There is no shared
"daemon-wide" or "process-wide" ring that would have to allocate bgids
across many connections or files. Sibling audit
[`docs/audits/iouring-socket-sqpoll-defer-taskrun.md:55-58`](iouring-socket-sqpoll-defer-taskrun.md)
records the same per-connection ring lifecycle and is the basis for the
DEFER_TASKRUN single-issuer recommendation.

`is_io_uring_available()`
([`crates/fast_io/src/io_uring/config.rs:167`](../../crates/fast_io/src/io_uring/config.rs))
is process-wide, but only as a probe cache. The rings themselves are
per-instance.

## Allocation policy assumed by current code

When the (unused) `BufferRing` is constructed today:

- The `bgid` is **explicitly chosen by the caller** through
  `BufferRingConfig.bgid`. There is no allocator.
- The default value is `0` (line 159).
- Tests use a small fixed value (`0` or `1`).
- The same `bgid` is used at register and unregister time. There is no
  rebind path.

This means the current API is consistent with **option (a)**: a single
caller (one `BufferRing` per `RawIoUring`) is expected, and that caller
picks a fixed bgid such as `0`. The 16-bit width is irrelevant when
only one slot is in use.

## Risk quantification

The realistic ceiling is `u16::MAX = 65535`. The kernel reserves no
slots, so the first 65 536 `IORING_REGISTER_PBUF_RING` calls on the
same ring with distinct `bgid` values would all succeed; the 65 537th
fails with `-EINVAL` from `io_register_pbuf_ring()` (or `-EBUSY` if it
collides with an existing slot).

For oc-rsync to hit that limit, the codebase would need to either:

1. Maintain a single long-lived `RawIoUring` shared across many
   connections / files, **and** allocate one `BufferRing` per
   connection / file with a unique bgid. This is **not** the current
   architecture: rings are per-fd, lifetimes match the fd lifetime,
   and `BufferRing` is unwired.
2. Maintain one ring per connection but allocate many `BufferRing`s
   per connection (e.g. one per outstanding chunk, per file, per
   stripe). Today nothing does this.
3. Suffer a fd / connection leak that fails to drop rings while
   continuing to register groups. That is a generic resource-leak bug
   class, independent of the bgid width.

Workloads that would stress (1) hypothetically: a daemon that serves
tens of thousands of concurrent transfers from a single shared ring
**and** chooses to register one provided-buffer ring per active file.
Workloads that would stress (2): heavily fragmented fan-out where a
single transfer carves the basis into per-block buffer groups. Neither
shape exists in oc-rsync source, the design path
([`docs/audits/io-uring-adaptive-buffer-sizing.md:387-399`](io-uring-adaptive-buffer-sizing.md)),
or the upstream rsync semantics we mirror.

For comparison: 65 536 concurrent connections served by a **single**
ring would pin tens of GiB of buffer memory regardless of bgid (each
ring entry holds one buffer of at least
`BufferRingConfig::default().buffer_size = 64 KiB`). Memory exhaustion
would precede bgid exhaustion by orders of magnitude.

The bgid namespace is per-ring (each `RawIoUring` has its own). The
kernel does not collapse bgid spaces across rings; see
`io_uring/kbuf.c::io_register_pbuf_ring()` which scopes the lookup to
`ctx->io_bl_xa` (per-ring xarray).

### Numeric recap

| Quantity | Value | Source |
|---|---|---|
| Max bgid per ring | 65 535 (`u16::MAX`) | `man 3 io_uring_register_buf_ring`, kernel `io_uring/kbuf.c` |
| `bgid`s allocated per ring today | 0 | grep of workspace; no caller |
| `bgid`s allocated per ring once `BufferRing` is wired (planned) | 1 | `BufferRingConfig.bgid` is fixed at construction; ring lifetime equals fd lifetime |
| Concurrent rings per process | bounded by fd ulimit | `RawIoUring` per fd, see scope table above |
| Concurrent bgids per process | unbounded by `u16` (per-ring namespace) | kernel `io_register_pbuf_ring()` is per-ring |

## Recommendation

**Option (a): document the limit, assert "not a concern", with two
guard rails when the `BufferRing` wiring lands.**

1. **One `bgid` per ring** until evidence demands otherwise.
   - Hard-code `bgid = 0` for the primary buffer ring on each
     `RawIoUring`. Reserve `1..=15` for known follow-on use cases
     (multi-tier buffer sizes, header vs payload split). Treat any
     allocation beyond that as a flag that this audit must be
     revisited.
   - The `BufferRingConfig::bgid` field stays public so callers that
     genuinely need multiple rings on one `RawIoUring` can opt in.
2. **Tie `BufferRing` lifetime to its parent ring.** The `Drop` impl
   already calls `IORING_UNREGISTER_PBUF_RING`
   ([`crates/fast_io/src/io_uring/buffer_ring.rs:516-553`](../../crates/fast_io/src/io_uring/buffer_ring.rs)),
   so per-fd ring teardown automatically frees the bgid slot. Keep
   that invariant: do not introduce a `BufferRing` cache that
   outlives its `RawIoUring`.
3. **Add a debug-assert / `tracing::warn!` if a single `RawIoUring`
   ever crosses, say, 256 live buffer groups.** Cheap to implement
   inside the wiring PR (a counter in the owning struct, not in
   `BufferRing` itself) and gives a loud signal long before the
   namespace is at risk.

A freelist or recycler (option (b)) is not justified by current
architecture. If a future change starts allocating per-file or
per-connection buffer groups inside a single shared ring, revisit and
sketch:

```rust
// Sketch only - not for implementation in this PR.
struct BgidAllocator {
    free: Vec<u16>,        // recycled IDs, LIFO for cache locality
    next: u16,             // first never-used ID; 0 reserved for default ring
    high_water: u16,       // for telemetry
}

impl BgidAllocator {
    fn alloc(&mut self) -> Option<u16> {
        self.free.pop().or_else(|| {
            let id = self.next.checked_add(1)?;
            self.high_water = self.high_water.max(id);
            self.next = id;
            Some(id)
        })
    }

    fn free(&mut self, id: u16) {
        // Caller MUST have unregistered the corresponding BufferRing
        // (or the kernel will -EINVAL the next register on this id).
        self.free.push(id);
    }
}
```

Option (c) (per-ring allocator with eviction) is unnecessary because
nothing today, or in the planned PBUF_RING wiring, evicts a live
buffer group: a `BufferRing` is dropped only when its parent
`IoUringReader` / `IoUringWriter` / equivalent is dropped, and the
parent's lifetime tracks the fd. If a multi-tier scheme is ever
introduced (small-buffer ring + large-buffer ring per connection),
option (a) plus the debug-assert remains adequate; eviction is a
solution looking for a problem.

## Test plan (when `BufferRing` is wired)

- Construct an `IoUringReader` with `BufferRingConfig::default()`
  (`bgid = 0`); assert `BufferRing::bgid() == 0`.
- Construct two `IoUringReader`s on different fds; assert each sees
  `bgid = 0` and registration succeeds because the namespace is
  per-ring.
- On a single ring, register `BufferRing` with `bgid = 0`, drop it,
  re-register with `bgid = 0`; assert success (the slot is reusable
  after `IORING_UNREGISTER_PBUF_RING`).
- Negative test: register two `BufferRing`s on the same ring with
  identical `bgid`; assert the second registration returns
  `RegisterFailed(EBUSY)` or `EINVAL`.
- Stress: construct 1024 `BufferRing`s on one synthetic ring with
  monotonically increasing bgids; assert all succeed and that the
  per-process bgid counter (proposed in step 3 above) never trips
  the warn threshold for a single ring.

These tests live in
[`crates/fast_io/src/io_uring/buffer_ring.rs`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
alongside the existing `buffer_ring_recycle_on_supported_kernel` and
`buffer_ring_new_on_supported_kernel` cases (lines 783, 718). They are
gated by `if !is_supported() { return; }` so they no-op on kernels
older than 5.19 and on non-Linux CI runners, mirroring the existing
gating pattern.

## References

- Upstream rsync 3.4.1 source: `target/interop/upstream-src/rsync-3.4.1/`
  (no io_uring usage; bgid is purely an oc-rsync concern).
- oc-rsync code:
  - `crates/fast_io/src/io_uring/buffer_ring.rs:41` -
    `IORING_REGISTER_PBUF_RING = 22`.
  - `crates/fast_io/src/io_uring/buffer_ring.rs:44` -
    `IORING_UNREGISTER_PBUF_RING = 23`.
  - `crates/fast_io/src/io_uring/buffer_ring.rs:60-66` -
    `IoUringBufReg` struct mirroring kernel uABI.
  - `crates/fast_io/src/io_uring/buffer_ring.rs:134-152` -
    `BufferRingConfig` (public), `bgid: u16` at line 151.
  - `crates/fast_io/src/io_uring/buffer_ring.rs:267-397` -
    `BufferRing::new` (registration path).
  - `crates/fast_io/src/io_uring/buffer_ring.rs:516-554` -
    `Drop for BufferRing` (unregister path).
  - `crates/fast_io/src/io_uring/mod.rs:100` - re-export.
  - `crates/fast_io/src/lib.rs:155` - top-level re-export.
  - `crates/fast_io/src/io_uring_stub.rs:121-216` - non-Linux stub.
  - `crates/fast_io/src/io_uring/registered_buffers.rs:76-80` -
    `MAX_REGISTERED_BUFFERS = 1024` (different namespace, do not
    conflate).
- Sibling audits:
  - `docs/audits/io-uring-adaptive-buffer-sizing.md:387-399` -
    "one registered group per ring per file handle".
  - `docs/audits/iouring-pipe-stdio.md:220-224` - PBUF_RING listed as
    phase-2.
  - `docs/audits/iouring-socket-sqpoll-defer-taskrun.md:55-58` -
    per-connection ring lifecycle.
- Linux man pages (verify before citing in code comments):
  - `man 2 io_uring_register` - documents
    `IORING_REGISTER_PBUF_RING` and the `struct io_uring_buf_reg`
    layout including the `__u16 bgid` field.
  - `man 3 io_uring_register_buf_ring` - liburing wrapper, documents
    the per-ring scope of `bgid`.
  - `man 7 io_uring` - overview, including `IOSQE_BUFFER_SELECT`.
- Kernel sources (verify against running kernel before citing in
  code):
  - `io_uring/kbuf.c` - `io_register_pbuf_ring()`,
    `io_buffer_select()`, `ctx->io_bl_xa` per-ring xarray.
  - Linux commit `c7fb19428d67` ("io_uring: add support for ring
    mapped supplied buffers", 5.19).
