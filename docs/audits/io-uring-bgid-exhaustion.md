# io_uring `bgid` u16 namespace exhaustion bound

Tracking issue: oc-rsync #2044. Sibling to
[`per-file-vs-shared-uring-ring.md`](per-file-vs-shared-uring-ring.md)
and
[`iouring-registered-buffer-adaptive-sizing.md`](iouring-registered-buffer-adaptive-sizing.md).
Documents the buffer-group-id namespace shape, audits whether the
current `BufferRing` lifecycle can leak it, sketches a worst case,
and proposes a hard-cap allocator.

## 1. `bgid` is u16 - 65 535 distinct groups per ring

The kernel `struct io_uring_buf_reg` carries `bgid` as a 16-bit field
(`crates/fast_io/src/io_uring/buffer_ring.rs:88-95`). The Rust mirror
`BufferRingConfig::bgid: u16`
(`buffer_ring.rs:194-199`) is the only path that produces it; the
mmap offset encodes the group at bit 16
(`buffer_ring.rs:387` -
`IORING_OFF_PBUF_RING | (u64::from(config.bgid) << 16)`), so the
namespace is hard-capped at `2^16 = 65 536` IDs (`0..=u16::MAX`) per
io_uring instance. CQE flags surface `bid`, also `u16`, with the
group implied by the SQE's `IOSQE_BUFFER_SELECT` target
(`buffer_ring.rs:97-104`, `:641-680`).

Today the ID is supplied by the caller. There is no central
allocator: `BufferRingConfig::default()` hard-codes `bgid = 0`
(`buffer_ring.rs:201-209`) and every production call site that exists
in-tree passes `0` or a small literal in tests (`:687`, `:692`,
`:701`, `:710`, `:719`, `:729`, `:813`, `:880`, `:910`). No ring,
file factory, socket factory, or registered-buffer module currently
mints fresh `bgid` values; that work is deferred to the per-session
follow-ups below.

## 2. Per-session ring path (#1936 / #1937)

The shared per-session ring landed in
`crates/fast_io/src/io_uring/shared_ring.rs` (header at `:1`, see
`docs/audits/shared-iouring-session-instance.md`). When PBUF_RING
support is wired through it (#1936/#1937 follow-ups), each new
reader/writer pair in a session will need its own buffer group so
SQEs can disambiguate. A long-running daemon that opens one ring per
TCP connection and provisions a `BufferRing` per direction (read and
write) consumes two `bgid` values per session. At 30 k concurrent
sessions the namespace is half-full; at 32 768 it is full.

## 3. Audit: do we free `bgid` on drop?

Yes for the kernel side, no for any allocator we own.

`BufferRing::Drop` issues `IORING_UNREGISTER_PBUF_RING` with the
matching `bgid` (`buffer_ring.rs:601-622`), then unmaps the shared
region and frees the buffer slab. The kernel reclaims the group
immediately - subsequent `IORING_REGISTER_PBUF_RING` with the same
`bgid` is accepted. So id reuse is safe at the kernel boundary.

What is missing is an in-process owner of the namespace. There is
no `BgidAllocator`, no free list, no metric for live groups per
ring. If a future session-scoped factory hands out monotonically
increasing `bgid` values without consulting a slab, dropped
`BufferRing`s vacate the kernel slot but the next-id counter still
walks toward `u16::MAX`. The exhaustion is purely an allocator
artefact, not a kernel one.

## 4. Worst case

A daemon under attack opens millions of short-lived rsync
connections per hour. Each connection's per-session ring
provisions and drops one `BufferRing` per direction. With a naive
`AtomicU16::fetch_add` allocator the counter wraps after 32 768
sessions (or 65 536 if both directions share an id). Two distinct
failure modes appear:

- **Wraparound collision.** `fetch_add` on `u16` wraps to `0`. The
  next registration claims `bgid = 0` while a still-live ring also
  holds `bgid = 0`, so `IORING_REGISTER_PBUF_RING` returns `EEXIST`
  (or worse, succeeds against a different ring fd and silently
  steers reads into the wrong session's buffers).
- **Linear growth without reuse.** If the allocator instead
  saturates at `u16::MAX` and rejects further requests, every new
  session past the cap silently downgrades to non-PBUF reads. The
  daemon keeps serving but loses the zero-copy path, and the
  miss is invisible without telemetry.

## 5. Mitigation

Land a single in-tree owner of the `bgid` namespace before #1936 /
#1937 wires PBUF_RING through the per-session ring.

- **Hard cap with reservation.** Cap soft-allocations at
  `1 << 14 = 16 384` (a quarter of the namespace), reserving
  `16 384..=u16::MAX` for explicit pinning (probes, fixed-purpose
  rings, future kernel extensions).
- **Slab allocator.** A `Vec<bool>` (or `bit_vec`) of length 16 384
  guarded by a `Mutex` (allocations are off the hot path). Allocate
  the lowest free index, return it to the slab on
  `BufferRing::Drop` via an `Arc<BgidAllocator>` handle stored in
  the ring. This guarantees reuse and bounds peak occupancy.
- **Telemetry.** Track `live_bgids` and `peak_bgids`. Emit a
  `tracing::warn!` once when occupancy crosses 50 % (`>= 8 192`)
  per allocator, throttled to one log per minute so a steady-state
  busy daemon does not spam.
- **Exhaustion is an error.** When the slab is full, return a typed
  `BufferRingError::BgidNamespaceExhausted` rather than silently
  falling back. The caller decides whether to degrade to
  non-PBUF I/O or refuse the new session; today's silent fallback
  hides capacity loss from operators.
- **Reserved range tests.** Add unit tests that exercise the
  16 384 boundary, drop-and-reuse, and concurrent allocation under
  `loom` (already used elsewhere in `fast_io`).

Acceptance: the allocator ships behind `BufferRing::with_allocator`
with the existing `BufferRing::new(_, BufferRingConfig)` retained
for tests and explicit-id callers. No protocol or wire change.
