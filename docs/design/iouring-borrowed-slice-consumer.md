# io_uring borrowed-slice consumer vs registered-buffer copy (#2208)

## Status

Design analysis. No code changes proposed in this document.

## Problem

The io_uring `READ_FIXED` path in `crates/fast_io/src/io_uring/` writes into
page-aligned registered buffers owned by `RegisteredBufferGroup`
(`crates/fast_io/src/io_uring/registered_buffers.rs:105`-`registered_buffers.rs:122`).
The kernel pins these buffers once at `IORING_REGISTER_BUFFERS` time, eliding
`get_user_pages()` on every SQE. After completion, the current code copies the
freshly-read bytes out of the pinned buffer into a heap `Vec<u8>` that the
caller owns.

Task #2208 asks whether the consumer can borrow the slice directly out of the
registered buffer, skipping the second copy.

## 1. Current copy path

The completed bytes leave the registered buffer in exactly one place:

- `crates/fast_io/src/io_uring/registered_buffers.rs:578`-`registered_buffers.rs:584`
  inside `submit_read_fixed_batch`, after each CQE drains:

  ```text
  // Safety: the kernel wrote `bytes` into the registered buffer.
  // We copy from the registered buffer into the caller's output slice.
  unsafe {
      ptr::copy_nonoverlapping(
          slots[idx].ptr,
          output[out_start..].as_mut_ptr(),
          copy_len,
      );
  }
  ```

  The destination `output` is a `&mut [u8]` passed in by the caller. For the
  current sole caller `IoUringReader::read_all_batched`
  (`crates/fast_io/src/io_uring/file_reader.rs:163`-`file_reader.rs:194`) the
  destination is a freshly allocated `vec![0u8; size]` at
  `file_reader.rs:169`. So every batched read incurs:

  1. One heap allocation for `output`
     (`crates/fast_io/src/io_uring/file_reader.rs:169`).
  2. One `copy_nonoverlapping` per completed SQE from the registered buffer
     into that heap region
     (`crates/fast_io/src/io_uring/registered_buffers.rs:579`).

The write side has the mirror-image copy at
`crates/fast_io/src/io_uring/registered_buffers.rs:646`-`registered_buffers.rs:648`,
which stages caller bytes into the registered buffer before `WRITE_FIXED`.
That write-side copy is unavoidable unless the caller already owns
registered-buffer memory; this design focuses on the read side, which is what
#2208 targets.

The fallback path that uses plain `Read` SQEs
(`crates/fast_io/src/io_uring/file_reader.rs:199`-`file_reader.rs:287`) does
not copy: it submits with `output[out_start + done..].as_mut_ptr()` directly
(`file_reader.rs:240`-`file_reader.rs:241`). That path has no registered-buffer
indirection, so the copy in question is unique to the `READ_FIXED` slot path.

The buffer-ring (`IORING_OP_PROVIDE_BUFFERS` / `pbuf_ring`) path in
`crates/fast_io/src/io_uring/buffer_ring.rs` already exposes a borrowed-slice
accessor: `BufferRing::buffer_slice`
(`crates/fast_io/src/io_uring/buffer_ring.rs:617`-`buffer_ring.rs:621`)
returns `Option<&[u8]>` valid until `recycle_buffer`
(`buffer_ring.rs:640`-`buffer_ring.rs:676`). That API is `unsafe` and shifts
the lifetime contract to the caller. No first-party consumer uses
`buffer_slice` today; the only readers
(`crates/fast_io/src/io_uring/socket_reader.rs:81`-`socket_reader.rs:124`) hold
their own `Vec<u8>` and copy into the user's `buf` at `socket_reader.rs:121`.

## 2. Per-call cost

Per `READ_FIXED` completion the copy is `bytes` bytes wide, where `bytes` is
bounded by the slot's `buffer_size`. Defaults from
`crates/fast_io/src/io_uring_common.rs:115`-`io_uring_common.rs:159`:

| Config profile | `buffer_size` | `sq_entries` | `registered_buffer_count` |
|----------------|---------------|--------------|---------------------------|
| default        | 64 KB         | 64           | 8                         |
| large files    | 256 KB        | 256          | 16                        |
| small files    | 16 KB         | 128          | 8                         |

Assume sustained `memcpy` throughput of 12 GB/s on a modern x86 core with the
copy hitting L2 (page-aligned source, freshly allocated destination - the
typical worst case for cache temperature). One 64 KB copy then costs roughly
`65536 / 12e9 s = 5.4 us`, and one 256 KB copy roughly `21 us`. At
`sq_entries = 64` that is up to `350 us` of CPU time per batched
`submit_and_wait` cycle just for the copy, before the kernel side of the
`io_uring_enter` returns.

These numbers must be read against the actual benchmark deltas, not in
isolation:

- **#4197 (io_uring per-file vs shared ring)** records single-digit-percent
  throughput differences once the ring is reused across files. A 5 us copy
  per 64 KB completion is on the same order as the per-SQE overhead the
  shared-ring change was trying to remove; removing the copy would deliver
  similar percentage points of headroom on the read path.
- **#4201 (SQPOLL)** removes the `io_uring_enter` syscall on submit. With
  SQPOLL on, the copy becomes a relatively larger share of the
  per-completion cost. Pre-SQPOLL the copy is hidden behind syscall and
  context-switch latency; post-SQPOLL it is the next thing to fall.

The cost is real but bounded. It only pays off when the consumer can use the
data in place. Any consumer that itself buffers, hashes, decompresses, or
sends the bytes onward will end up touching them anyway. The copy avoidance
is meaningful only when the next op is a `WRITE_FIXED` from the same
registered region (true zero-copy basis-to-target) or a checksum/compress
kernel that streams in-place.

## 3. Borrowed-slice API sketch

The proposal is to expose the completion to the consumer as `&[u8]` valid
strictly between the CQE drain and the next ring-poll boundary. The slice's
lifetime is anchored to the `RegisteredBufferGroup`, but logically it lasts
only until the slot is checked back in or the ring submits another
`READ_FIXED` that reuses it.

The shape Rust expresses cleanly is a scoped callback:

```text
impl IoUringReader {
    pub fn read_batched_with<F>(&mut self, offset: u64, len: usize, mut consume: F) -> io::Result<usize>
    where
        F: for<'a> FnMut(&'a [u8]) -> io::Result<()>,
    { ... }
}
```

The `for<'a>` higher-ranked bound forbids the closure from saving the slice
into anything that outlives a single invocation. Inside `read_batched_with`,
the reader drains one batch of CQEs, invokes `consume(&slot[..bytes])` for
each completion, then recycles the slot and either submits the next round or
returns.

Lifetime contract for the borrowed `&[u8]`:

1. Valid from the moment the CQE for this slot has been observed.
2. Invalid as soon as `consume` returns, because the slot is recycled and
   the same registered-buffer page may be handed to a subsequent SQE.
3. Aliasing is exclusive: the kernel has finished writing (CQE seen) and the
   ring has not been re-submitted, so no other writer touches the page.
4. Recycling on early return: if `consume` returns `Err`, `read_batched_with`
   still recycles every checked-out slot before propagating the error,
   otherwise registered buffers leak into the in-use bitmap
   (`crates/fast_io/src/io_uring/registered_buffers.rs:114`-`registered_buffers.rs:116`).
5. Panic safety: the scoped recycle must live in `Drop` on a guard
   struct, not in an `if/else` after the callback, to keep the bitmap
   coherent if `consume` panics.

The unsafe primitive already exists: `BufferRing::buffer_slice`
(`crates/fast_io/src/io_uring/buffer_ring.rs:617`-`buffer_ring.rs:621`) is the
identical contract for the buffer-ring path. A `read_batched_with` wrapper
would just port that pattern to `RegisteredBufferGroup` and wrap it in a safe
scoped API.

## 4. Hazards

The borrow contract is correct on paper but brittle in practice:

- **Re-entrancy via the consumer.** If `consume` calls back into the same
  `IoUringReader` (for example to start the next batched read, or to issue a
  `WRITE_FIXED` of the freshly-read bytes through the same ring), the
  submission queue may overwrite the slot before the borrow ends. The
  `for<'a>` bound does not catch this: the closure owns no slice, but it
  can still mutate ring state via `&mut self` aliasing on the reader. The
  only enforcement is dynamic - assert in debug that the reader is not
  re-entered during a `consume` call.

- **Cross-thread escape via `Send` references.** A `&[u8]` is `Send` as long
  as `u8: Sync`, which it is. A closure that hands the slice to a rayon
  `par_iter` body (cf. `crates/fast_io/src/io_uring/registered_buffers.rs:124`-`registered_buffers.rs:130`
  documenting `Send + Sync` on the group) will compile. The
  borrow checker enforces that all spawned tasks join before the closure
  returns - rayon's scoped `scope()` honors this - but `std::thread::spawn`
  with a `'static` bound would not compile, and `tokio::spawn` with a
  `'static` bound would not either, which is what we want. The hazard is
  that a future maintainer rationalises "I just need this for a moment"
  and reaches for `crossbeam::scope` without realising the recycling races
  the join.

- **Short reads.** `submit_read_fixed_batch`
  (`crates/fast_io/src/io_uring/registered_buffers.rs:496`-`registered_buffers.rs:607`)
  retries short reads on slow filesystems (NFS, FUSE) by re-submitting the
  same slot with an adjusted offset
  (`registered_buffers.rs:589`-`registered_buffers.rs:604`). A borrowed-slice
  API has to defer the consumer call until the slot has the full
  caller-requested range, or expose partial slices and let the consumer
  handle reassembly. The first option negates the latency benefit of
  pipelining; the second pushes assembly logic into every consumer.

- **CQE out-of-order delivery.** CQEs arrive in arbitrary order
  (`crates/fast_io/src/io_uring/registered_buffers.rs:552`-`registered_buffers.rs:587`).
  The borrowed-slice API exposes completions as they land, so the
  consumer sees chunk 5 before chunk 2. Callers that need offset-ordered
  data (delta decoder, checksum streaming, compress framing) need a
  reorder buffer or explicit chunk indices. The current copy-based API
  hides this entirely by writing into the right offset of `output`.

- **Eager recycling vs the kernel pinning window.** The slot is recycled
  the moment `consume` returns. If the consumer started an async op that
  reads the slice in the background (a `splice` to a pipe, an
  `IORING_OP_SEND` via a sibling ring), recycling races the kernel reading
  from the same page. Rust's borrow checker does not see the kernel as
  another reader, so this hazard is invisible at compile time.

These hazards are exactly the same ones that kept `BufferRing::buffer_slice`
marked `unsafe` and unused by first-party readers. The safe scoped API
would suppress two of them (forbidding the slice from escaping a scope,
forbidding storage past the next poll), but three remain
- re-entrancy, short-read assembly, and out-of-order completions - and they
all push complexity onto the consumer.

## 5. Alternative: keep the copy but hand callers `Arc<[u8]>` / `Bytes`

If the goal is "callers can keep the data without managing lifetimes" rather
than "no copy at all", an indirection-based design is much simpler:

- `read_all_batched_owned() -> io::Result<Arc<[u8]>>` allocates once,
  performs the existing copy from registered slot to heap, then hands the
  caller a refcounted owning slice. The caller can clone the `Arc<[u8]>`,
  ship it across threads, hand it to a hash pipeline, and drop it
  whenever - the registered buffer is recycled the moment the copy
  finishes, exactly like today.

- `bytes::Bytes` (already a transitive dep through HTTP-ish crates if pulled
  in) offers the same shape plus refcount-free slicing and easy splitting
  into chunks, which the delta and compress pipelines already do via
  `&[u8]` arithmetic.

This alternative concedes the per-call copy cost from section 2 but buys
back ergonomics: no scoped closure, no `for<'a>` plumbing, no consumer-side
reorder buffer. It also leaves the registered-buffer slot free for the next
submission as soon as the copy finishes, which actually improves
throughput when the consumer is slow (today the consumer holds the heap
`Vec` long after the read completes, which is fine because the heap
allocation is decoupled).

The cost: one allocation per batched read, same as today. With
`registered_buffer_count = 8` and 64 KB buffers, peak working set is at most
`8 * 64 KB = 512 KB` of registered memory plus the `Arc<[u8]>` heap region.
No worse than current behaviour.

## 6. Recommendation

**Keep the current copy.** Do not adopt borrowed-slice consumer APIs at
`RegisteredBufferGroup` level.

Reasons, in order of weight:

1. **The lifetime contract is too brittle to expose as a safe API.** Three
   of the five hazards in section 4 (re-entrancy, short-read assembly,
   out-of-order CQEs) cannot be enforced by the borrow checker. A safe
   wrapper around them would either degrade to "scoped closure that
   internally still copies into an assembly buffer" - the copy we were
   trying to remove - or expose a confusing partial-data API.

2. **The per-call cost is small relative to the work that follows.** The
   immediate consumers of `read_all_batched` are the delta-basis read path
   and the local-copy executor. Both touch the bytes again (rolling
   checksum, block match, write to destination), so the data is already
   warm in cache when the next pass starts. Removing the registered-to-
   heap copy saves the 5-20 us per chunk from section 2 but does not
   shrink the dominant cost.

3. **Existing buffer-ring code already shows the pattern is unused.**
   `BufferRing::buffer_slice`
   (`crates/fast_io/src/io_uring/buffer_ring.rs:617`-`buffer_ring.rs:621`)
   has lived in the tree since the buffer-ring path landed and has zero
   first-party consumers. The same end users would face the same hazards
   with the registered-buffer flavour.

4. **The architectural direction is in the opposite direction.** #2243
   (per-thread rings) and #2045 (adaptive registered-buffer pool) both
   assume the consumer is decoupled from the slot lifecycle. Per-thread
   rings amplify the re-entrancy hazard (each thread is its own
   consumer-and-resubmitter). Adaptive sizing assumes slots can be
   resized between batches without coordinating with consumers, which a
   borrowed-slice contract would forbid.

If the per-call copy cost shows up as a measurable hot spot in a future
benchmark, the second-tier action is to adopt the **`Arc<[u8]>` / `Bytes`
indirection** from section 5. That keeps the existing copy semantics, costs
one heap allocation per batched read, and gives callers an ownership
handle they can keep without ring-lifetime entanglement. That option is a
two-line API change on `IoUringReader::read_all_batched` and does not
require touching `submit_read_fixed_batch`.

The "remove the copy by exposing borrows" option should be reopened only if
a benchmark proves the copy is the dominant cost on a representative
workload, and only after the consumer set has been audited for every hazard
in section 4.

## 7. Cross-references

- **#2243** Per-thread io_uring rings (planned). Per-thread rings make the
  re-entrancy hazard worse: any consumer running on the same thread as the
  ring can re-enter submission while holding a borrowed slice. A
  `for<'a>` closure does not prevent that.
  See `crates/fast_io/src/io_uring/shared_ring.rs` for today's
  shared-ring design that #2243 supersedes.

- **#2045** Adaptive registered-buffer pool (planned). The adaptive sizing
  loop wants to resize or replace the buffer set between batches. A
  borrowed-slice contract pins the slot for the consumer's scope and
  prevents resizing inside that window. See the telemetry counters at
  `crates/fast_io/src/io_uring/registered_buffers.rs:117`-`registered_buffers.rs:121`
  and the design notes in
  `docs/design/io-uring-submission-modes-bench-plan.md:313`-`io-uring-submission-modes-bench-plan.md:332`.

- **#1284** io_uring submission from rayon worker threads. Crossing rayon
  threads with a borrowed `&[u8]` is type-correct (`u8: Sync`) but couples
  the rayon `scope()` lifetime to the ring-poll cycle. The recommended
  decoupling is exactly what `Arc<[u8]>` from section 5 provides: workers
  take an owned handle, do their work, drop the handle. The composition
  rules are documented in `docs/design/io-uring-rayon-composition.md`.

- Related: `BufferRing::buffer_slice`
  (`crates/fast_io/src/io_uring/buffer_ring.rs:617`-`buffer_ring.rs:621`) is
  the precedent for an unsafe borrowed-slice accessor on registered I/O
  memory. The lack of first-party callers is itself evidence for the
  recommendation above.
