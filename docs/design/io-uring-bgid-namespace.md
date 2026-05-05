# Bounding the io_uring `bgid` u16 Namespace (#2044)

## Summary

The Linux io_uring `IORING_REGISTER_PBUF_RING` interface identifies a
provided-buffer ring by a 16-bit buffer group id (`bgid`). At most
65 536 distinct provided-buffer rings can be live on a single
`io_uring_fd`, and each `bgid` is the namespace key the kernel uses
to map an `IOSQE_BUFFER_SELECT` SQE to its ring of buffers.

Today the `fast_io` crate exposes the PBUF_RING primitive
([`crates/fast_io/src/io_uring/buffer_ring.rs:235`][buf-ring]) but
performs no allocation or recycling of `bgid` values. Every
`BufferRingConfig` carries a caller-supplied `bgid: u16` with default
`0` ([`buffer_ring.rs:169-179`][buf-ring-default]). No in-tree caller
constructs a `BufferRing` outside the unit tests in the same file, so
the namespace is unused in production. This document plans the work
that must precede broader use - in particular the session-level ring
pool design ([`iouring-session-ring-pool.md`][session-pool]) and the
adaptive sizing audit (#2045).

[buf-ring]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L235
[buf-ring-default]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L169
[session-pool]: ./iouring-session-ring-pool.md

## 1. Problem Statement

The kernel UAPI struct in `include/uapi/linux/io_uring.h`:

```c
struct io_uring_buf_reg {
    __u64 ring_addr;
    __u32 ring_entries;
    __u16 bgid;
    __u16 flags;
    __u64 resv[3];
};
```

The mirroring Rust definition is at
[`buffer_ring.rs:58-66`][reg-struct]. The mmap that maps the
kernel-side ring page encodes `bgid` in bits 16-31 of the offset
([`buffer_ring.rs:331`][mmap-offset]):

```rust
let mmap_offset = IORING_OFF_PBUF_RING | (u64::from(config.bgid) << 16);
```

Because the kernel uses `bgid` as a hash-table key in
`io_uring/kbuf.c::io_register_pbuf_ring`, only one ring can be live
for a given `(io_uring_fd, bgid)` pair at any instant.

### Allocation strategy today

There is no central allocator. `bgid` is a public field on
`BufferRingConfig` ([`buffer_ring.rs:152-170`][config-struct]),
defaults to `0` ([`buffer_ring.rs:172-180`][config-default]), and
every caller picks a literal. All non-test instantiations use the
default value `0`. Tests pick `0`, `1`, or `2` manually
([`buffer_ring.rs:666,760,824,854`][buf-ring]). A future caller that
wants per-file PBUF_RING isolation has no API guidance and no
protection against collision with another caller in the same session.

[reg-struct]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L58
[mmap-offset]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L331
[config-struct]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L152
[config-default]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L172

### Exhaustion mode

Two failure modes follow:

1. **Collision** - two callers reuse the same `bgid` in one session.
   The second `IORING_REGISTER_PBUF_RING` returns `-EEXIST`. The
   error path at [`buffer_ring.rs:397-404`][register-fail] cleans up
   user-side mmap and allocation, returning
   `BufferRingError::RegisterFailed`. Rejected cleanly but not
   self-explanatory.
2. **Exhaustion** - a future caller that mints a fresh `bgid` per
   file with no recycling silently wraps after 65 536 registrations,
   colliding with the first ring ever registered. The kernel returns
   `-EEXIST`; userspace has no warning that the namespace is
   exhausted.

The first mode is a programming error, evident from the API. The
second is the topic of this document.

[register-fail]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L397

## 2. Current Allocation Policy

| Concern | File:Line |
|---|---|
| Public field declaration | [`buffer_ring.rs:169`][config-struct] |
| Default value (`0`) | [`buffer_ring.rs:177`][config-default] |
| Kernel-facing struct | [`buffer_ring.rs:63`][reg-struct] |
| mmap offset encoding | [`buffer_ring.rs:331`][mmap-offset] |
| Register call | [`buffer_ring.rs:381`][register-call] |
| Unregister at Drop | [`buffer_ring.rs:545-583`][unregister] |
| Stub mirror (non-Linux) | `crates/fast_io/src/io_uring_stub.rs:146,154` |

There is no allocator function, no atomic counter, and no free-list.
`bgid` is freed by `Drop` calling `IORING_UNREGISTER_PBUF_RING` with
the same value the ring was registered with
([`buffer_ring.rs:551`][unregister]). The kernel removes its
hash-table entry; userspace tracks nothing.

[register-call]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L381
[unregister]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L545

## 3. Current Safety Net

What happens today if a caller exhausts the namespace?

- **No panic in the recycler.** `BufferRing::recycle_buffer` returns
  `BufferRingError::BufferIdOutOfRange` rather than panicking
  ([`buffer_ring.rs:499-535`][recycle]). This concerns *buffer ids*
  (`bid`), not *group ids* (`bgid`).
- **No silent fallback in `new`.** `BufferRing::new` propagates the
  kernel error via `BufferRingError::RegisterFailed`
  ([`buffer_ring.rs:105-107`][register-error]).
- **Silent fallback in `try_new`.** `try_new` collapses any error to
  `Option::None` ([`buffer_ring.rs:421-424`][try-new]), so callers
  using `try_new` silently fall back to non-PBUF_RING reads.
- **No retry loop.** Nothing in tree retries a different `bgid` on
  collision.

Today, exhaustion is impossible because no one allocates. For future
allocators, the kernel's `-EEXIST` and the `RegisterFailed` path are
the only safety net: fail-closed, but with no diagnosability and no
graceful degradation across the namespace.

[recycle]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L499
[register-error]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L105
[try-new]: ../../crates/fast_io/src/io_uring/buffer_ring.rs#L421

## 4. Theoretical Exhaustion Thresholds

### Per-ring

Hard limit 65 536 per `io_uring_fd`. Each `bgid` in `[0, 65535]` maps
to at most one provided-buffer ring on that fd. The limit applies
independently to each fd.

### Per-process

`RLIMIT_NOFILE * 65536` if every io_uring fd is used to capacity. In
practice the fd ceiling binds first. `IoUringConfig::build_ring`
([`crates/fast_io/src/io_uring/config.rs:381`][build-ring]) creates
one ring per call, and the session-pool design caps the count at
`min(num_cpus, 4)` ([`iouring-session-ring-pool.md`][session-pool]).
A 4-ring pool offers 262 144 distinct `(ring, bgid)` pairs.

### Effective working budget

The namespace is not the binding constraint - memory is. Each
PBUF_RING pins `ring_size * buffer_size` bytes. With defaults
(`ring_size = 64`, `buffer_size = 64 KiB` -
[`buffer_ring.rs:174-178`][config-default]) one ring pins 4 MiB.
65 536 rings would pin 256 GiB, well past `RLIMIT_MEMLOCK` (default
64 KiB on stock Debian) and any reasonable host RAM. The practical
bound is:

```
N_max = min( 65536,
             RLIMIT_MEMLOCK / (ring_size * buffer_size),
             available_RAM  / (ring_size * buffer_size) )
```

A 64 GiB server with default ring shape can host ~16 384 rings before
RAM, far fewer before `MEMLOCK`. The u16 ceiling is the *outer*
bound, not the binding constraint, so the recycler must address
memory pressure before namespace pressure.

[build-ring]: ../../crates/fast_io/src/io_uring/config.rs#L381

## 5. Realistic Scenarios

### S1: Long-running daemon, millions of files

A daemon module transfers hundreds of millions of files over its
lifetime. Per-file PBUF_RING with prompt Drop keeps the live count at
`concurrent_transfers` only. With the session pool capped at
`min(num_cpus, 4)` rings, the live `bgid` set never approaches the
ceiling. But a *naive monotonic counter* still exhausts after 65 536
file transfers per io_uring fd regardless of how many are live. The
recycler must distinguish "currently live" from "cumulative mints".

### S2: Multi-tenant ring pool

The session-pool design shares 1-4 rings across many concurrent
transfers. If each transfer wants its own PBUF_RING, contention is on
`bgid` *within* a shared ring. Allocate and free must be thread-safe
across worker threads holding ring leases.

### S3: Adaptive resizing

The adaptive sizing audit (#2045) plans to grow the buffer count when
EMA-smoothed miss rate exceeds a threshold. A grow event today
rebuilds the `RegisteredBufferGroup`; the analogous PBUF_RING
operation unregisters and reregisters. With a naive allocator the
replacement gets a fresh `bgid`, leaking the old slot until Drop.
Under sustained pressure that leaks O(grow events) bgids, hitting the
u16 ceiling far sooner than the RAM budget. The recycler MUST reuse
slots on rebuild.

## 6. Proposed Recycler

### Goals

- O(1) allocation and free.
- Lock-free in the steady state.
- Detect both wrap (cumulative count exceeds u16) and live-set
  exhaustion (all 65 536 slots in use).
- Surface a typed error so callers can pick a fallback.
- Zero cost when no caller constructs a `BufferRing`.

### Shape

A new submodule `crates/fast_io/src/io_uring/bgid_alloc.rs`:

```rust
pub struct BgidAllocator {
    free: ArrayQueue<u16>,        // crossbeam, capacity 65536
    next_fresh: AtomicU32,        // monotonic mint, never decremented
    in_use: AtomicU32,            // diagnostics
}

pub struct BgidLease<'a> {
    alloc: &'a BgidAllocator,
    bgid: u16,
}

impl Drop for BgidLease<'_> { /* releases slot to free queue */ }
```

Public API:

- `BgidAllocator::new() -> Self` - one allocator per `io_uring_fd`.
- `acquire(&self) -> Result<BgidLease<'_>, BgidExhausted>`.
- `BgidLease::value(&self) -> u16`.
- `live(&self) -> u32` - diagnostics.

### Allocation algorithm

1. Pop from `free`. If `Some(bgid)`, increment `in_use`, return lease.
2. Otherwise, fetch-add `next_fresh`. If the prior value `< 65536`,
   return that as a fresh bgid.
3. Otherwise, return `BgidExhausted`. Do not loop. Do not wrap.

`ArrayQueue` is wait-free fast path under SPSC, near-lock-free under
MPMC. Push (release) is symmetric.

### Generation counter (defence-in-depth)

A free `bgid` must not be returned to the kernel while its old ring
is being torn down. `BufferRing::Drop`
([`buffer_ring.rs:545-583`][unregister]) is synchronous: by the time
the lease's Drop runs, `IORING_UNREGISTER_PBUF_RING` has executed and
the slot is reusable. To guard against future async cleanup or a Drop
bypassed by panic, each lease carries a 16-bit generation that the
allocator increments on release. Debug builds assert the lease holds
the current generation; release builds rely on the kernel's
`-EEXIST` to reject double-registration.

### Why monotonic-then-wrap is rejected

A wrapping counter silently collides with a stale slot that has not
yet been freed. The kernel returns `-EEXIST`; the caller retries and
either loops or escalates. Loop bounds are hard to set; escalation
defeats the point. The free-list strategy gives deterministic O(1)
and a clean error.

## 7. Hard Cap and Fallback

When `acquire` returns `BgidExhausted`, the caller falls back to the
non-PBUF_RING path. This mirrors the existing fallback in
[`file_writer.rs:56-64`][writer-fallback]: a `None` from
`RegisteredBufferGroup::try_new` means "use plain `IORING_OP_WRITE`
instead of `WRITE_FIXED`". The same pattern extends to PBUF_RING.

Error type:

```rust
#[derive(Debug, thiserror::Error)]
#[error("io_uring bgid namespace exhausted: {live} / 65536 in use")]
pub struct BgidExhausted { pub live: u32 }
```

`BufferRingError` gains:

```rust
#[error("bgid allocation failed: {0}")]
BgidExhausted(#[from] BgidExhausted),
```

Mapped to `io::ErrorKind::Other`. No upstream errno cleanly matches:
`EAGAIN` is misleading, `EMFILE` is wrong because no fd is involved,
and `E2BIG`-style is a documentation preference, not a kernel
mapping.

[writer-fallback]: ../../crates/fast_io/src/io_uring/file_writer.rs#L56

## 8. Test Strategy

### Unit tests for `BgidAllocator`

In `crates/fast_io/src/io_uring/bgid_alloc.rs`:

1. `acquire_returns_distinct_bgids` - first 65 536 acquires return
   distinct values.
2. `acquire_after_release_reuses` - acquire, drop, acquire returns
   the same bgid.
3. `acquire_past_capacity_returns_exhausted` - 65 537th acquire with
   no releases returns `BgidExhausted`.
4. `release_makes_room_for_new_acquire` - acquire to capacity, drop
   one lease, acquire succeeds.
5. `concurrent_acquire_release_no_double_assign` - 64 worker threads
   each acquire 1 024 leases; assert the live set has unique bgids
   (mutex-guarded `HashSet<u16>`).
6. `live_counter_tracks_outstanding_leases` - after `N` acquires and
   `M` drops, `live() == N - M`.

### Integration test: simulated exhaustion

`crates/fast_io/tests/io_uring_bgid_exhaustion.rs`:

1. Skip if `buffer_ring::is_supported()` is false.
2. Skip if the process cannot bump `RLIMIT_MEMLOCK` to 64 MiB. Use
   `getrlimit`/`setrlimit` via the helper pattern in
   `crates/fast_io/tests/io_uring_probe_fallback.rs`.
3. Build a single io_uring with `IoUringConfig::default()`.
4. Allocate `BgidAllocator::new()`.
5. Loop `BufferRing::new_with_lease(&alloc, &uring, cfg)` with
   `ring_size = 1, buffer_size = 4096` until error.
6. Assert the terminating error is `BgidExhausted` or a memory
   rlimit error; print which constraint actually bound.
7. Drop all rings; one more acquire succeeds.

Real exhaustion needs root for `MEMLOCK` and a permissive kernel.
The integration test is gated by `IO_URING_BGID_STRESS_TEST=1`.
Default CI runs only the unit tests; a privileged workflow can set
the env.

### Property test (optional)

A `proptest` strategy interleaves random acquire/release operations
and asserts the live set never contains a duplicate. Catches ABA
bugs in the lock-free path.

## 9. Cross-Reference: #2045 Adaptive Sizing

#2045 (`docs/audits/io-uring-adaptive-buffer-sizing.md`) bounds the
*per-ring* buffer count. This document bounds the *across-ring*
`bgid` namespace. The two are orthogonal but compose:

- Adaptive sizing rebuilds a `RegisteredBufferGroup` (or future
  `BufferRing`) under sustained pressure. Each rebuild that gets a
  fresh `bgid` would burn an entry in the namespace. The recycler
  here ensures the bgid is returned when the old ring drops.
- The miss-rate signal that drives adaptive sizing should also hint
  the bgid recycler: if many rings are at high miss rate, growing
  each is preferable to spawning new ones, conserving both pinning
  budget and namespace.

A `BufferRing::resize` operation should: (1) acquire a fresh bgid
*before* unregistering the current one so a failed rebuild can
revert; (2) on success, free the old bgid immediately so the
allocator's live count reflects the kernel state.

The two designs share test fixtures but exercise different
exhaustion axes - "many slots, one ring" vs. "many rings, one slot".

## 10. Open Questions

1. **Reserve `bgid = 0`?** Today every default-constructed
   `BufferRingConfig` carries `bgid: 0`. If the allocator hands out
   `0` to a non-default caller, a default-config `BufferRing::new`
   later in the same session collides. Options: (a) start the
   allocator at `1`, reserve `0` for legacy default callers,
   document that mixing default config with the allocator is
   unsupported; (b) deprecate the public `bgid` field on
   `BufferRingConfig` and require all production callers to go
   through the allocator. (b) is cleaner but a breaking change.
2. **Per-fd vs. per-process scope?** The kernel namespace is per
   `io_uring_fd`. With the session-pool design holding 1-4 rings,
   an allocator-per-pool-slot is natural. The lease type carries a
   borrow back to the allocator, so the type system enforces no
   cross-slot leakage at compile time.
3. **`ArrayQueue` vs. hand-rolled bitmap?** A 65 536-bit
   `[AtomicU64; 1024]` bitmap supports O(words) acquire and O(1)
   release, uses 8 KiB. `ArrayQueue` is O(1) on both but uses
   ~768 KiB. The bitmap is the better final choice; the sketch
   above used `ArrayQueue` for clarity.
4. **`IORING_FEAT_REG_REG_RING` (kernel 6.3+)?** Newer kernels allow
   registering a buffer ring against a registered ring fd. The bgid
   namespace is unchanged; the fd handling shifts. Out of scope for
   #2044; the allocator is fd-agnostic.
5. **In-flight recovery?** The current proposal returns
   `BgidExhausted` from `acquire` and the caller falls back. A
   future enhancement could expose a `wait_for_release` API that
   blocks instead. Requires a `Notify` primitive; out of scope.
6. **Daemon-global allocator?** Each daemon worker has its own
   io_uring fd, so allocator scope is per-worker. A daemon-global
   allocator would only matter if workers shared an `io_uring_fd`,
   which the current design does not do.

## Implementation Plan

The work splits into three commits:

1. Add `bgid_alloc.rs` with the allocator, lease type, error type,
   unit tests. No call-site changes.
2. Add `BufferRing::new_with_lease` and `try_new_in_pool` that
   consume a `BgidLease`. Existing `new`/`try_new` keep working
   with caller-supplied `bgid`.
3. Migrate the session-pool design (when it lands - see
   [`iouring-session-ring-pool.md`][session-pool]) to use the
   allocator. Add the integration test gated by env var.

No wire-protocol or upstream-rsync compatibility surface is touched.
The change is internal to `fast_io`.

## References

- Kernel UAPI: `include/uapi/linux/io_uring.h` (`io_uring_buf_reg`).
- Kernel implementation: `io_uring/kbuf.c::io_register_pbuf_ring`.
- `io_uring_register(2)` man page.
- [`crates/fast_io/src/io_uring/buffer_ring.rs`][buf-ring].
- [`docs/design/iouring-session-ring-pool.md`][session-pool].
- `docs/audits/io-uring-adaptive-buffer-sizing.md` (#2045).
- Issue history: #1677 (`IORING_UNREGISTER_BUFFERS` in Drop), #1739
  (registered buffer rings), #2042 (`recycle_buffer` assertion),
  #2043 (PBUF_RING probe), #2045 (adaptive sizing).
