# io_uring `bgid` u16 namespace bounds (task #2044)

Companion to [`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md),
[`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md), and
[`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md).
This file enumerates the concrete `bgid` allocation surface in oc-rsync,
quantifies the exhaustion scenarios that the bare `u16` cap admits, and
sketches three mitigation strategies so the next caller that adds a
`BufferRing` can pick one without re-deriving the analysis.

## Scope

`bgid` is the kernel-side identifier for a provided buffer ring registered
via `IORING_REGISTER_PBUF_RING` (`io_uring/kbuf.c::io_register_pbuf_ring`).
The kernel scopes the namespace **per `io_uring` instance** (the `ctx`
xarray `io_bl_xa`), and represents the id as a `__u16` in
`struct io_uring_buf_reg`. That sets a hard ceiling of `u16::MAX = 65 535`
buffer groups per ring; the 65 537th distinct id on the same ring fails
with `-EINVAL`.

The pre-existing `RegisteredBufferGroup` slot index
([`crates/fast_io/src/io_uring/registered_buffers.rs:80`](../../crates/fast_io/src/io_uring/registered_buffers.rs))
backs `IORING_OP_READ_FIXED`/`WRITE_FIXED` and is not a `bgid`; it is a
distinct kernel namespace capped at `MAX_REGISTERED_BUFFERS = 1024`. This
audit deliberately excludes it.

## Allocation sites in current source

A workspace-wide grep for `bgid` and `buffer_group` returns three concrete
allocation surfaces in `crates/fast_io/src/io_uring/`:

| File:line | Role |
|---|---|
| [`buffer_ring.rs:92`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `IoUringBufReg.bgid: u16` (mirrors kernel uABI, written at register time) |
| [`buffer_ring.rs:198`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `BufferRingConfig.bgid: u16` (public config, set by caller) |
| [`buffer_ring.rs:206`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `BufferRingConfig::default()` returns `bgid: 0` |
| [`buffer_ring.rs:387`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `mmap_offset = IORING_OFF_PBUF_RING \| (u64::from(config.bgid) << 16)` (encodes id into mmap offset) |
| [`buffer_ring.rs:437`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `IoUringBufReg.bgid = config.bgid` (passed to `IORING_REGISTER_PBUF_RING`) |
| [`buffer_ring.rs:488-490`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `BufferRing::bgid()` accessor |
| [`buffer_ring.rs:607`](../../crates/fast_io/src/io_uring/buffer_ring.rs) | `Drop for BufferRing` writes `bgid` into the unregister payload |
| [`registered_buffers.rs:80`](../../crates/fast_io/src/io_uring/registered_buffers.rs) | `MAX_REGISTERED_BUFFERS = 1024` (different namespace - included for contrast only) |

The id is **caller-supplied**: there is no allocator, counter, free list,
or recycler. `BufferRingConfig::bgid` is a plain `u16` field with no
validation past the kernel registration call. Existing in-tree callers
are restricted to:

- Unit tests in `buffer_ring.rs` that hard-code `bgid: 0`, `1`, or `2`
  ([`buffer_ring.rs:687`, `722`, `732`, `816`, `880`, `910`](../../crates/fast_io/src/io_uring/buffer_ring.rs)).
- Stub tests in `io_uring_stub.rs` for non-Linux targets.

No production caller currently constructs a `BufferRing`. Every
`RawIoUring` in the production path is created per-fd:

| File:line | Owner | Rings per object |
|---|---|---|
| [`file_reader.rs:60`](../../crates/fast_io/src/io_uring/file_reader.rs) | `IoUringReader::open` | 1 per file fd |
| [`file_writer.rs:54`](../../crates/fast_io/src/io_uring/file_writer.rs) | `IoUringWriter::open` | 1 per file fd |
| [`file_writer.rs:81`](../../crates/fast_io/src/io_uring/file_writer.rs) | `IoUringWriter::open_at` | 1 per file fd |
| [`file_writer.rs:141`](../../crates/fast_io/src/io_uring/file_writer.rs) | `IoUringWriter::from_file` | 1 per file fd |
| [`socket_reader.rs:32`](../../crates/fast_io/src/io_uring/socket_reader.rs) | `IoUringSocketReader::from_raw_fd` | 1 per socket direction |
| [`socket_writer.rs:32`](../../crates/fast_io/src/io_uring/socket_writer.rs) | `IoUringSocketWriter::from_raw_fd` | 1 per socket direction |
| [`disk_batch.rs:71`](../../crates/fast_io/src/io_uring/disk_batch.rs) | `IoUringDiskBatch::new` | 1 per batch helper |

Each owner gets its own `bgid` namespace, so today the design implicitly
relies on **one ring per fd** and would assign at most one `bgid`
(value `0`) per ring once `BufferRing` is wired.

## Current allocation strategy

There is no strategy in the dispatch sense. The mechanism is:

- **ad-hoc:** `BufferRingConfig::bgid` is set by the caller at construction.
- **no counter:** no global or per-ring allocator increments an id.
- **no recycler:** ids are not returned to a free pool; the only release
  path is `IORING_UNREGISTER_PBUF_RING` from `BufferRing::Drop` at
  [`buffer_ring.rs:607`](../../crates/fast_io/src/io_uring/buffer_ring.rs),
  which frees the kernel-side slot but produces no userspace bookkeeping.
- **no validation:** any `u16` value is accepted and forwarded to the
  kernel; collisions surface as `EBUSY` from the syscall, not an early
  Rust-level error.

This is acceptable while no production caller constructs `BufferRing`,
but any first wiring PR must replace ad-hoc assignment with one of the
options below before the same `RawIoUring` ever has to host more than
one buffer group.

## Exhaustion scenarios

The `u16` ceiling is per-ring, not per-process. Five plausible shapes
can drive that ceiling:

1. **Long-running daemon, single shared ring.** Sibling design
   [`docs/audits/shared-iouring-session-instance.md`](shared-iouring-session-instance.md)
   contemplates a process-wide `RawIoUring`. If that lands and each
   inbound transfer registers a fresh `BufferRing` with a unique id,
   the ring saturates after 65 535 transfers. Daemons that serve
   millions of small transfers per day reach this in hours under
   monotonically increasing assignment without recycle.
2. **65 K+ concurrent transfers, per-fd rings.** Today's per-fd ring
   model bounds the namespace to a single `bgid` per ring, so the
   theoretical 65 K cap on `bgid` itself is never hit. The real cap
   becomes `RLIMIT_NOFILE` (typically 1024 - 1 048 576). bgid is
   not the limiting resource in this shape, but the audit notes it
   so future fan-out designs do not assume the per-fd shape persists.
3. **Per-block buffer groups (multi-tier).** A reader that splits
   payload between a small-header ring and a large-payload ring needs
   2 ids per fd. Variants that add a third tier (small/medium/large)
   need 3. Single-digit ids per fd are negligible relative to 65 K
   but still demand a deterministic assignment scheme (option (c)
   below) so collisions cannot occur when two concurrent fds share a
   ring (option 1).
4. **Leaked groups on dropped buffers.** If a code path forgets to
   drop the `BufferRing` (e.g. holds it across a panic that leaks the
   owning struct via `mem::forget`, or stashes it in a static), the
   `Drop` impl at [`buffer_ring.rs:601-639`](../../crates/fast_io/src/io_uring/buffer_ring.rs)
   never runs and the kernel slot is held until ring fd close. On a
   long-lived ring (option 1), a small steady leak rate exhausts the
   namespace independently of legitimate allocations. This is
   **structurally identical to a generic resource leak** but the
   `u16` ceiling makes it visible orders of magnitude sooner than fd
   or memory pressure.
5. **Adversarial id selection.** The current API accepts
   `bgid = u16::MAX`. A hostile caller (or a buggy one passing
   uninitialised memory) can collide with a legitimate id and cause
   `EBUSY`. Today this is moot because no production caller exists,
   but any allocator added later must reject ids it did not issue.

The kernel itself imposes no per-process cap on the **sum** of bgids
across rings. Process-wide exhaustion would require memory or fd
exhaustion first; those bound the workload long before bgid does.

## Proposed mitigations

Three options, ordered by complexity. Pick the simplest that fits the
caller's shape; this audit does not mandate one because the right
choice depends on which of the five scenarios the caller is in.

### (a) Recycle on drop (per-ring free list)

Add a per-ring `BgidAllocator` that the ring owner consults at
`BufferRing::new` time. The allocator hands out the lowest free id and
reclaims it when the corresponding `BufferRing` drops.

```rust
// Sketch only - implementer must add tests and wire into RawIoUring.
struct BgidAllocator {
    free: Vec<u16>,    // recycled ids, LIFO for cache locality
    next: u16,         // first never-used id; 0 reserved for default ring
    high_water: u16,   // for `tracing::warn!` telemetry
}

impl BgidAllocator {
    fn alloc(&mut self) -> Option<u16> {
        if let Some(id) = self.free.pop() {
            return Some(id);
        }
        let id = self.next;
        self.next = self.next.checked_add(1)?;  // None at u16::MAX + 1
        self.high_water = self.high_water.max(id);
        Some(id)
    }

    fn release(&mut self, id: u16) {
        // Caller must have unregistered the ring before release;
        // otherwise the next register on this id would -EINVAL.
        debug_assert!(!self.free.contains(&id), "double-release of bgid");
        self.free.push(id);
    }
}
```

`BufferRing::Drop` ([`buffer_ring.rs:601`](../../crates/fast_io/src/io_uring/buffer_ring.rs))
already issues `IORING_UNREGISTER_PBUF_RING`; release into the
allocator happens after that syscall. Pairs with scenario 1 (long-lived
ring) and scenario 4 (leak detection via `high_water` telemetry).

**Cost:** one `Vec<u16>` and a `u16` per ring; alloc/release are O(1).
**Caveat:** the allocator must outlive every `BufferRing` it issued,
or the `release` call dangles. Hold it inside the same struct that
owns the `RawIoUring`.

### (b) Per-session reset

If a `RawIoUring` represents a single transfer session (one connection,
one file), drop the entire ring at session end. The kernel reclaims
every bgid slot when the ring fd closes
(`io_uring/io_uring.c::io_ring_ctx_free`), so no userspace bookkeeping
is required. This is what the current per-fd architecture already
does - the audit names it explicitly so future per-connection rings
inherit the same invariant.

**When this fits:** scenario 2 (per-fd rings) and any wiring where the
ring's lifetime equals the transfer's lifetime. The default for new
callers should remain "per-session reset" until concrete profiling
shows ring construction overhead dominates.

**Cost:** zero. **Caveat:** does not generalise to scenario 1; if the
ring outlives the session, fall back to (a) or (c).

### (c) Partitioned namespace (low/high split)

Reserve disjoint sub-ranges of `0..=u16::MAX` for distinct purposes.
Concrete partitioning that matches today's planned consumers:

| Range | Purpose | Allocation policy |
|---|---|---|
| `0` | Default / single-ring callers (current `BufferRingConfig::default()`) | Hard-coded |
| `1..=15` | Multi-tier buffer rings on the same fd (header/payload/large) | Hard-coded by tier |
| `16..=255` | `ack_batcher` and other low-volume control planes | Counter, no recycle |
| `256..=u16::MAX` | High-volume per-transfer rings (option 1 daemon shape) | Counter with recycle (option (a)) |

Partitioning gives two benefits: (i) any out-of-range id is
immediately diagnosable as a bug rather than colliding silently with
a legitimate caller; (ii) the high range can use a recycler without
the low range needing one.

`ack_batcher` (`crates/transfer/src/ack_batcher/`) is named here as a
forward reference because it is the first non-`BufferRing` caller
likely to want a deterministic id when the io_uring write path lands;
today it does not touch `bgid`. The low/high split costs a single
constant per partition.

## Recommendation

For the first production wiring of `BufferRing` (currently planned in
[`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md)):

1. Default to **option (b) per-session reset** because the current
   per-fd ring lifetime already provides it for free.
2. If a future PR shares a `RawIoUring` across sessions, layer
   **option (a) recycle on drop** on top, with `tracing::warn!` at
   `high_water > 256` per ring and a debug-assert at
   `high_water > 4096`.
3. Reserve the partitioning in **option (c)** as documentation only
   until a second non-test caller appears; encoding the table
   prematurely entrenches a design without evidence.
4. Reject any caller that passes `bgid >= 1024` until partitioning
   lands. Today no caller passes anything other than `0`-`2`; the
   upper bound is cheap insurance against scenario 5.

Cite this audit and
[`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
in the wiring PR so the reviewer has the analysis at hand.

## References

- `io_uring_register(2)` - documents `IORING_REGISTER_PBUF_RING`,
  `IORING_UNREGISTER_PBUF_RING`, and `struct io_uring_buf_reg`
  including the `__u16 bgid` field.
- `io_uring_register_buf_ring(3)` - liburing wrapper, scope of `bgid`.
- Linux kernel `io_uring/kbuf.c` - `io_register_pbuf_ring()`,
  `io_buffer_select()`, per-ring `ctx->io_bl_xa` xarray.
- Linux commit `c7fb19428d67` ("io_uring: add support for ring mapped
  supplied buffers", 5.19).
- Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`)
  contains no io_uring usage; `bgid` has no wire-protocol implication.
- Sibling oc-rsync audits:
  - [`docs/audits/io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md)
    - prior "not a concern" framing.
  - [`docs/audits/iouring-pbuf-ring.md`](iouring-pbuf-ring.md) - PBUF_RING
    wiring plan.
  - [`docs/audits/io-uring-adaptive-buffer-sizing.md`](io-uring-adaptive-buffer-sizing.md)
    - adaptive sizing, references one-group-per-fd shape.
  - [`docs/audits/iouring-socket-sqpoll-defer-taskrun.md`](iouring-socket-sqpoll-defer-taskrun.md)
    - per-connection ring lifecycle.
  - [`docs/audits/shared-iouring-session-instance.md`](shared-iouring-session-instance.md)
    - shared-ring design that motivates option (a) and (c).
