# BGID lifecycle audit (BGE-1, BGE-2)

Tracking issues: #2293 (allocation audit) and #2294 (release audit). Sibling
material: [`io-uring-bgid-namespace.md`](io-uring-bgid-namespace.md),
[`io-uring-bgid-exhaustion.md`](io-uring-bgid-exhaustion.md),
[`iouring-bgid-namespace.md`](iouring-bgid-namespace.md). This document is
the precondition for BGE-3 (high-water-mark stat) and BGE-4 (free-list
recycling tuning); it freezes the current allocation + release call graph
so subsequent tasks can land surgically.

## 1. Namespace

- Kernel `struct io_uring_buf_reg.bgid` is `u16`, bounded to 65 536 IDs.
  Definition mirrored at
  `crates/fast_io/src/io_uring/buffer_ring.rs:90`
  (`IoUringBufReg.bgid: u16`).
- mmap offset folds the bgid at bit 16
  (`crates/fast_io/src/io_uring/buffer_ring.rs:439`,
  `IORING_OFF_PBUF_RING | (u64::from(config.bgid) << 16)`), so the
  per-io_uring-instance namespace cannot be widened without a kernel ABI
  change.
- `BGID_NAMESPACE_SIZE = u16::MAX as u32 + 1`
  (`crates/fast_io/src/io_uring/buffer_ring.rs:129`) is the hard cap the
  allocator enforces.

## 2. Allocation flow

Single entry point: `BgidAllocator::allocate()` at
`crates/fast_io/src/io_uring/buffer_ring.rs:193`.

1. Drain the process-wide free-list first
   (`buffer_ring.rs:197-203`, helper `bgid_free_list()` at `:146`). A
   `OnceLock<Mutex<Vec<u16>>>` returns the most recently deallocated id
   ahead of advancing the monotonic counter.
2. If the free-list is empty, bump the monotonic counter
   `NEXT_BGID: AtomicU32` (`buffer_ring.rs:138`) via
   `fetch_add(1, Relaxed)` at `buffer_ring.rs:208`. Stored as `u32` so the
   `>= 65 536` boundary is observable without wrap.
3. On overshoot the allocator undoes the fetch (`fetch_sub(1, Relaxed)`
   at `buffer_ring.rs:216`) and returns `BufferRingError::BgidExhausted`.
   The undo keeps the counter capped at `BGID_NAMESPACE_SIZE` so repeated
   calls after exhaustion never wrap back into the valid `u16` range and
   silently collide with a live ring.

Construction wrapper: `BufferRing::new_with_allocator()` at
`buffer_ring.rs:564` calls `BgidAllocator::allocate()`, threads the id
into `BufferRingConfig::bgid`, then sets `self.allocator_owned = true`
(`buffer_ring.rs:572`) so `Drop` knows to recycle. If `BufferRing::new()`
fails after allocation, the wrapper hands the id back via
`BgidAllocator::deallocate(bgid)` at `buffer_ring.rs:576` before
propagating the error, so a failed registration does not leak a slot.

## 3. Release flow

Single release path: `BufferRing::Drop` at
`crates/fast_io/src/io_uring/buffer_ring.rs:720`.

1. Issue `IORING_UNREGISTER_PBUF_RING` (opcode 23) with the matching bgid
   (`buffer_ring.rs:723-741`). Kernel reclaims the group slot
   immediately; the same bgid may then be re-registered.
2. `munmap` the ring descriptor region (`buffer_ring.rs:746-748`).
3. `dealloc` the buffer slab (`buffer_ring.rs:754-756`).
4. If `self.allocator_owned` is `true`
   (`buffer_ring.rs:761-763`), call `BgidAllocator::deallocate(bgid)`.
   `deallocate` (`buffer_ring.rs:245`) takes the free-list mutex and
   `push`es the id, guarded by `!free_list.contains(&bgid)` for
   idempotence on double-drop scenarios.

Caller-supplied bgids (`BufferRing::new`) leave `allocator_owned = false`,
so the kernel slot is unregistered but no process-side reuse happens.
The caller continues to own that namespace slot, matching the original
intent for fixed-purpose probes.

## 4. `BgidExhausted`

- Variant declared at
  `crates/fast_io/src/io_uring_common.rs:370`
  (`BgidExhausted` with display
  `io_uring buffer group ID namespace exhausted (limit: 65535)`).
- `From<BufferRingError> for io::Error` (`io_uring_common.rs:378`) maps
  it to `io::ErrorKind::InvalidInput` at `io_uring_common.rs:387` so
  upstream callers see a typed I/O error.
- Raised in two places:
  - `BgidAllocator::allocate()` at
    `crates/fast_io/src/io_uring/buffer_ring.rs:217` when the counter
    crosses the namespace limit and the free-list is empty.
  - Non-Linux stub
    `crates/fast_io/src/io_uring_stub/buffer_ring.rs:104`
    (`pub fn allocate() -> Result<u16, BufferRingError> { Err(BgidExhausted) }`)
    so the stub keeps a parity surface but never hands out IDs.

## 5. Daemon session usage

`BufferRing` and `BgidAllocator` are not yet wired into any production
caller outside `fast_io`. A workspace grep over `crates/daemon`,
`crates/engine`, `crates/core`, and `crates/cli` returns no hits for
`BufferRing` or `new_with_allocator`. The accept loop in
`crates/daemon/src/...` does not provision a per-session PBUF ring
today, so accepted connections do not consume a bgid, do not need to
return one on close, and the namespace cap is effectively untouched in
the steady state.

Implication: the allocator is currently defensive plumbing for the
follow-ups tracked under #1936/#1937 (per-session PBUF wiring) and the
shared-ring work in `shared_ring.rs`. When those land, every accepted
connection that opens a read PBUF ring consumes one bgid, and a write
ring consumes a second. Until then the only `allocate`/`deallocate`
exercise comes from the in-file unit tests at
`buffer_ring.rs:1067-1190`.

## 6. Risk profile

The lifecycle is correct on paper, but the surface is narrow enough to
hide regressions. The audit flagged four exposures.

1. **No high-water-mark telemetry.** `BgidAllocator::remaining()` exists
   (`buffer_ring.rs:259`) but there is no counter for peak in-use bgids,
   no `tracing::warn!` when occupancy crosses a threshold, and no Prom
   metric. A long-running daemon that leaks rings (forgotten `Box`,
   `Arc` cycle) consumes the namespace silently until the first
   `BgidExhausted` failure. BGE-3 should land
   `BgidAllocator::peak_used()` + a `tracing::warn!` at 50 % occupancy
   (>= 32 768 in flight) throttled to one log per minute.
2. **Free-list is unbounded.** `bgid_free_list()` returns a
   `Mutex<Vec<u16>>`. The vector grows monotonically up to 65 536
   entries (1 MiB at `u64` granularity, 128 KiB packed) and never
   shrinks. Harmless in size, but worth a `Vec::with_capacity(1 << 12)`
   to avoid 16+ realloc rounds during ramp-up.
3. **Free-list mutex on every allocate / deallocate.** The cited comment
   at `buffer_ring.rs:194-196` accepts this trade for "one buffer ring
   per long-running task". Once per-session rings land
   (#1936/#1937) the cadence rises to twice per accepted connection;
   acceptable at hundreds of accepts/sec, hot at tens of thousands.
   BGE-4 should consider a sharded free-list or a lock-free `ArrayQueue`
   if the per-session work materially raises contention.
3. **Idempotence comment vs. behaviour.** `deallocate` is idempotent for
   double-drop (`!free_list.contains(&bgid)`), but a stray
   `BgidAllocator::deallocate(constant)` from outside the crate would
   pollute the free-list with an id the allocator never issued. Public
   visibility on `deallocate` is necessary for the wrapper failure path
   at `buffer_ring.rs:576`; documentation at `buffer_ring.rs:236-244`
   already calls this out. No code change needed; the audit recommends
   `#[doc(hidden)]` plus a `pub(crate)` review if the visibility is not
   required by an external consumer (today it is not).
4. **Counter cap relies on `fetch_sub`.** Under a hypothetical 65 536
   concurrent threads racing past `BGID_NAMESPACE_SIZE`, each
   `fetch_add` past the limit is followed by a `fetch_sub`, so the
   counter never escapes by more than `thread_count - 1` and the next
   load still trips the exhaustion check. Correct, but subtle. A
   `compare_exchange` loop would be easier to reason about; the
   `fetch_add`+`fetch_sub` pair is preserved because it avoids the loop
   on the hot path.

## 7. BGE-4 fix plan: free-list pool keyed off Drop signal

The current implementation already wires a Drop signal
(`allocator_owned` -> `deallocate`). What is missing is the
*observability* and *bounded growth* hooks. Concrete deltas:

1. **Pre-size the free-list.**
   ```rust
   FREE_LIST.get_or_init(|| Mutex::new(Vec::with_capacity(1 << 12)))
   ```
   at `buffer_ring.rs:148`. Cap initial allocation; the `Vec` still
   grows under sustained churn but skips the early doubling rounds.
2. **Peak tracker.** Add `static PEAK_USED: AtomicU32 = AtomicU32::new(0)`
   alongside `NEXT_BGID`. On every successful `allocate`, compute
   `in_use = NEXT_BGID - free_list.len()` and `PEAK_USED.fetch_max(...)`.
   Expose via `BgidAllocator::peak_used()` so the daemon can surface
   the value to its existing metrics endpoint.
3. **Threshold warning.** When `in_use` crosses
   `BGID_NAMESPACE_SIZE / 2`, emit
   `tracing::warn!(target: "fast_io::bgid", peak = ..., remaining = ...,
   "io_uring bgid namespace 50% full")`. Throttle via a
   `OnceLock<Mutex<Instant>>` so steady-state busy daemons emit at most
   once per minute.
4. **Optional per-ring weak handle.** For BGE-4 stretch goal, replace
   the global free-list with `Arc<BgidPool>` carried inside
   `BufferRing` (mirrors the sketch in
   [`io-uring-bgid-exhaustion.md`](io-uring-bgid-exhaustion.md) section 5).
   That allows distinct pools per io_uring instance, useful once the
   shared-ring work in `shared_ring.rs` allows multiple rings per
   process.

## 8. BGE-5 stress-test plan

The existing tests cover correctness (`buffer_ring.rs:1066-1190`) but
not steady-state churn. BGE-5 should add:

1. **Churn loop.** `#[test]` that allocates and immediately drops
   `BufferRing::new_with_allocator` 200 000 times and asserts
   `BgidAllocator::peak_used() <= ring_size + thread_count`. Run under
   the existing test serialiser (`bgid_test_lock()` at
   `buffer_ring.rs:1081`) to avoid cross-test interference.
2. **Concurrent allocate / deallocate.** `loom` model (the `fast_io`
   crate already pulls `loom` for other tests) covering the
   `fetch_add` / `fetch_sub` cap and the free-list pop / push race.
3. **Exhaustion + recovery.** Allocate `u16::MAX + 1` ids without
   dropping, assert `BgidExhausted`, drop one, assert
   `BgidAllocator::allocate()` returns the freed id. Already partially
   covered by `bgid_allocator_reuses_freed_ids` at `buffer_ring.rs:1156`
   and `bgid_allocator_free_list_persists_after_exhaustion` at
   `buffer_ring.rs:1169`; extend with the recovery half.
4. **`tracing` capture.** Use `tracing-test` or a custom subscriber to
   assert the 50 % warning fires exactly once per throttle window under
   a synthetic ramp.
5. **Long-soak harness.** Optional `#[ignore]` test that runs for
   `RUST_TEST_TIME_SOAK=600` seconds with two threads allocating and
   dropping in tight loops, asserting `peak_used()` stays within
   tolerance. Useful before tagging a release that wires per-session
   PBUF rings (#1936/#1937).
