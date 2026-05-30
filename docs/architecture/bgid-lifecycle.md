# BGID lifecycle (architecture)

Task: BGE-7 (#2299). Companion audit: `docs/audits/bgid-lifecycle.md` (PR
[#4331](https://github.com/oferchen/rsync/pull/4331), tasks BGE-1 / BGE-2).
Session topology context: `docs/architecture/session-overview-ddp-async-iouring.md`
section 3 ("io_uring topology: session pool vs per-thread pool"). This
document is the durable architecture view; the audit freezes the call
graph at one point in time, this file explains the rules and how the
lifecycle composes with the surrounding io_uring fast path.

Buffer Group IDs (bgid) name the kernel-side provided buffer rings
(`IORING_REGISTER_PBUF_RING`) that the io_uring fast path reads into.
The id is a 16-bit field carried in `IOSQE_BUFFER_SELECT` SQEs and
echoed back on CQEs, so its lifecycle must outlive every in-flight
request that names it and must be recycled the instant the ring is
torn down.

## 1. Allocation flow

```
                +------------------------+
                |  caller (BufferRing::  |
                |  new_with_allocator)   |
                +-----------+------------+
                            |
                            v
            +-----------------------------+
            | BgidAllocator::allocate()   |
            +-----------------------------+
                            |
              free-list pop |  (Mutex<Vec<u16>>, OnceLock-initialised)
              succeeded?    |
                            |
              +-------------+--------------+
              | yes                        | no
              v                            v
       +-------------+        +-----------------------------+
       | return id   |        | NEXT_BGID.fetch_add(1)      |
       +------+------+        | (AtomicU32, Relaxed)        |
              |               +--------------+--------------+
              |                              |
              |                  id < 65 536 ?
              |                              |
              |                +-------------+-------------+
              |                | yes                       | no
              |                v                           v
              |        +---------------+   +-----------------------+
              |        | return id     |   | fetch_sub(1) to cap   |
              |        | as u16        |   | counter at 65 536;    |
              |        +-------+-------+   | return BgidExhausted  |
              |                |           +-----------+-----------+
              v                v                       v
       +------------------------------------------------------+
       | BufferRing::new() registers the ring with the kernel |
       | (IORING_REGISTER_PBUF_RING). On success the wrapper  |
       | sets allocator_owned = true so Drop will recycle the |
       | id; on failure it calls deallocate(bgid) immediately |
       | so no slot is leaked.                                |
       +------------------------------------------------------+
```

Single entry point: `BgidAllocator::allocate()`
(`crates/fast_io/src/io_uring/buffer_ring.rs`). Wrapper that hands the
id to a freshly registered ring: `BufferRing::new_with_allocator()`.
Caller-supplied bgids (raw `BufferRing::new()`) bypass the allocator
entirely and leave `allocator_owned = false`, matching the original
fixed-purpose-probe intent.

## 2. Recycling rules

```
   +-----------------+      drop()         +------------------+
   | BufferRing      |  ---------------->  | unregister kernel|
   | (allocator_     |                     | ring + munmap +  |
   |  owned = true)  |                     | dealloc slab     |
   +--------+--------+                     +---------+--------+
            |                                        |
            |                                        v
            |                       +-------------------------------+
            |                       | BgidAllocator::deallocate(id) |
            |                       | takes free-list mutex,        |
            |                       | push if !contains(id)         |
            |                       +-------------+-----------------+
            |                                     |
            v                                     v
   allocator_owned = false:           free-list now exposes id
   kernel slot unregistered,          for the next allocate() call
   caller retains namespace slot      (drains before fetch_add)
```

Drop-driven recycling guarantees the namespace slot returns to the
free-list synchronously with the kernel unregister. The free-list is a
process-wide `OnceLock<Mutex<Vec<u16>>>`; `OnceLock` ensures a single
shared instance across all rings, `Mutex` linearises push/pop, and the
`!contains(&bgid)` guard makes `deallocate` idempotent against
double-drop (e.g., `Option::take` + drop of the original holder).

The monotonic counter (`NEXT_BGID: AtomicU32`) never wraps: when
`fetch_add` overshoots, the allocator issues a compensating
`fetch_sub(1)` and returns `BgidExhausted`. The counter therefore
saturates at `BGID_NAMESPACE_SIZE` and subsequent calls re-trip the
exhaustion check rather than silently issuing colliding ids.

## 3. u16 namespace exhaustion math

The bgid field is `u16` inside `struct io_uring_buf_reg`
(`io_uring/kbuf.c::io_register_pbuf_ring`), so the namespace is exactly
`0..=65 535`:

```
total namespace      = u16::MAX + 1     = 65 536 ids
reserved/sentinel    = 1                (id 0 historically reserved by
                                        callers for fixed-purpose probes;
                                        the allocator itself does not
                                        currently reserve any value, but
                                        BGE-7 treats one id as conceptually
                                        reserved for future telemetry /
                                        sentinel use)
concurrent rings cap = 65 536 - 1       = 65 535 simultaneously live rings
```

Each live ring consumes one bgid for its lifetime; pre-session wiring
(`#1936`/`#1937`) will turn one accepted connection into two consumed
bgids (one read PBUF ring, one write PBUF ring). At that ratio the
practical ceiling is `(65 535) / 2 = 32 767` concurrent sessions per
process before allocation begins to fail. The mmap offset packing
(`IORING_OFF_PBUF_RING | (u64::from(bgid) << 16)`) folds the id into
bit 16 of the offset, so the kernel ABI cannot widen the namespace
without breaking every existing PBUF ring caller.

## 4. Pre-sized free-list rationale

`bgid_free_list()` lazily initialises the `Vec<u16>` on first call. The
current `Vec::new()` initial capacity triggers ~16 reallocation rounds
on the way from 0 to ~4 096 entries during ramp-up of a daemon that
churns rings. A pre-sized `Vec::with_capacity(1 << 12)` (4 096 entries,
8 KiB) covers the steady-state working set of any per-session wiring
without growing, and amortises to zero allocations under the expected
churn profile. The vector still grows monotonically up to 65 536
entries (128 KiB packed) under pathological churn, but the early
realloc cliff disappears. This is the BGE-4 plumbing the audit calls
out (`docs/audits/bgid-lifecycle.md` section 7).

The free-list is intentionally unbounded above the pre-size: trimming
would force a second mutex round on every drop, and the worst-case
memory footprint (128 KiB) is negligible against the 64 KiB-per-buffer
arenas the rings themselves carry.

## 5. High-water-mark stat (planned via BGE-3)

`BgidAllocator::remaining()` already returns
`BGID_NAMESPACE_SIZE - used + free`, but there is no observation of
peak in-use bgids. BGE-3 (audit section 6 item 1, audit section 7
item 2) lands the missing telemetry:

- `static PEAK_USED: AtomicU32` alongside `NEXT_BGID`.
- Every successful `allocate()` computes
  `in_use = NEXT_BGID.load(Relaxed) - free_list.len()` and calls
  `PEAK_USED.fetch_max(in_use, Relaxed)`.
- `BgidAllocator::peak_used() -> u32` exposes the value so the daemon
  can surface it through the existing metrics endpoint.
- A throttled `tracing::warn!(target: "fast_io::bgid", ...)` fires once
  per minute when `in_use >= BGID_NAMESPACE_SIZE / 2` (32 768), gated
  by a `OnceLock<Mutex<Instant>>`.

The architecture commitment is that every allocation path observes the
peak and every release path is reflected in `remaining()`; BGE-3 only
adds the read-side accessor and the warning hook.

## 6. Fallback when exhausted (planned via BGE-6)

Today `BgidExhausted` propagates upward as
`io::ErrorKind::InvalidInput` (`io_uring_common.rs`
`From<BufferRingError> for io::Error`). The receiving call site has no
documented downgrade path: a per-session PBUF ring allocation that
fails will fail the session.

BGE-6 will add the graceful fallback:

1. On `BgidExhausted`, the per-session wiring degrades to the
   non-PBUF read path (`recv(2)` into a heap buffer) for that session
   only. The fast path stays primed for every session that does receive
   a bgid.
2. The downgrade emits one `tracing::warn!` per occurrence, distinct
   from the BGE-3 50 % warning, so operators can correlate the two.
3. A counter (`BgidAllocator::degraded_sessions()`) exposes how many
   sessions have run on the fallback path since process start, surfaced
   through the same metrics endpoint as the BGE-3 peak counter.

Until BGE-6 lands, `BgidExhausted` is a hard error. The architecture
guarantee is that the failure mode is *loud* (typed error, never silent
id reuse) rather than *graceful*.

## 7. Daemon-aware exhaustion warnings (BGW series)

Long-lived daemons that run for days or weeks may exhaust the bgid
namespace repeatedly as sessions come and go. The original one-shot
`BGID_FALLBACK_WARNED` flag fired only on the first occurrence,
leaving operators blind to recurring exhaustion.

The BGW series (BGW-1 through BGW-7) replaces the one-shot flag with:

- **Throttled periodic warnings.** `warn_bgid_fallback()` fires at
  most once per 60 seconds, including cumulative exhaustion count,
  current in-flight occupancy, and peak usage.
- **`BgidSnapshot`** captures all four operator-facing counters
  (`exhausted_count`, `in_flight`, `peak_used`, `remaining`) in a
  single call for structured logging and health-check endpoints.
- **`BgidSessionStats`** tracks per-session exhaustion by snapshotting
  the process-wide counter at session start and computing the delta at
  session end. Positive `in_flight_delta()` at teardown signals a
  potential bgid leak.

The throttle intervals are:

| Warning | Interval | Condition |
|---------|----------|-----------|
| Namespace pressure | 30 s | in-flight > 50 % of namespace (32 768) |
| Exhaustion fallback | 60 s | `BgidAllocError::Exhausted` returned |

Operator guidance is in `docs/operations/bgid-monitoring.md`.

## 8. Cross-references

| Topic                                          | Document                                                              |
|------------------------------------------------|-----------------------------------------------------------------------|
| BGID allocation + release call graph (audit)   | `docs/audits/bgid-lifecycle.md` (PR #4331, BGE-1 / BGE-2)             |
| Namespace exhaustion deep dive                 | `docs/audits/io-uring-bgid-exhaustion.md`                             |
| Namespace primer                               | `docs/audits/io-uring-bgid-namespace.md`, `docs/audits/iouring-bgid-namespace.md` |
| Provided-buffer-ring primitive overview        | `docs/audits/iouring-pbuf-ring.md`                                    |
| Session topology (where rings are created)     | `docs/architecture/session-overview-ddp-async-iouring.md` section 3   |
| Fixed buffer registration audit                | `docs/audits/io-uring-fixed-buffer-audit.md`                          |
| Per-session ring wiring (in-flight)            | tracking issues `#1936`, `#1937`                                      |
| Daemon operator monitoring guide               | `docs/operations/bgid-monitoring.md` (BGW-7)                          |
