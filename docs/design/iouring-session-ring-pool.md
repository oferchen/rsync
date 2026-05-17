# io_uring Session Ring Pool (#1937)

## Status

Design only. Implementation is premature until the per-thread rings
work in #2243 lands and replaces the single `Arc<Mutex<RawIoUring>>`
shape of `SharedRing`. This document scopes the daemon-session pool
sketch so the work is shovel-ready when that constraint clears.

The earlier in-process MPMC pool design (#1409 / #1936) is captured in
[`io-uring-ring-pool.md`](./io-uring-ring-pool.md) and the partial
implementation plan in
[`iouring-session-ring-pool-impl.md`](./iouring-session-ring-pool-impl.md).
This document is the daemon-session framing: one pool keyed by
`SessionId`, one ring fleet shared across many daemon connections that
arrive back-to-back on the same `oc-rsync --daemon` process.

## 1. Current Per-Session Ring

Each daemon session today may construct its own io_uring. There is no
process-wide pool, no daemon-owned pool, and no cross-session reuse.
Concrete construction sites:

- `crates/fast_io/src/io_uring/config.rs:313` -
  `IoUringConfig::build_ring()`, the sole construction primitive. Every
  ring in the codebase eventually routes through this call, which
  invokes `io_uring_setup(2)` via the upstream `io_uring` crate.
- `crates/fast_io/src/io_uring/disk_batch.rs:71` -
  `IoUringDiskBatch::new` calls `config.build_ring()?` unconditionally
  on receiver startup. `try_new` at
  `crates/fast_io/src/io_uring/disk_batch.rs:86` short-circuits only
  when `is_io_uring_available()` returns false; on Linux 5.6+ it always
  builds a fresh ring.
- `crates/fast_io/src/io_uring/file_writer.rs:59,85,179` - three
  per-file constructor paths, each calling `config.build_ring()?`.
- `crates/fast_io/src/io_uring/file_reader.rs:65` - per-reader ring
  for `IoUringReader`.
- `crates/fast_io/src/io_uring/shared_ring.rs:151` - `SharedRing::new`
  builds one ring per transfer, shared by the reader and writer of
  that transfer but not across transfers.
- `crates/fast_io/src/io_uring/linkat.rs:113,181` - per-call rings for
  `linkat` operations (hardlink commit path).

The daemon side has no awareness of io_uring; the wire happens via
`crates/daemon/src/daemon/async_session/session.rs:91` (`AsyncSession`
`Drop`), which calls
`crates/daemon/src/daemon/session_registry.rs:186`
(`SessionRegistry::unregister`) and drops the session value. Rings
constructed inside the transfer pipeline are torn down with that
value; the next session repeats `io_uring_setup(2)` from scratch.

`SessionId` lives at
`crates/daemon/src/daemon/session_registry.rs:17`, allocated by
`SessionRegistry::register` at
`crates/daemon/src/daemon/session_registry.rs:145`. It is the natural
key for a pool.

## 2. Cost of a Fresh Ring per Session

`io_uring_setup(2)` is a kernel call that:

1. Allocates and mmaps the SQ ring (page-aligned).
2. Allocates and mmaps the CQ ring (page-aligned, double the SQ size
   by default).
3. Registers the io_uring instance fd.
4. Optionally spawns an SQPOLL kthread when `setup_sqpoll` is set
   (`crates/fast_io/src/io_uring/config.rs:328`).
5. Optionally registers file slots and buffer groups when
   `register_files = true` and `register_buffers = true` (defaults
   from `crates/fast_io/src/io_uring/config.rs:368`).

Measured cost is in the low-to-mid microsecond range on a warm host
(roughly 50 to 200 us per ring), plus the per-page mmap and any
`io_uring_register(2)` calls for buffers. The defaults pin
approximately 520 KiB per ring (one page SQ + one page CQ + 8 * 64 KiB
registered buffers, see
`crates/fast_io/src/io_uring/registered_buffers.rs:80` for the
`MAX_REGISTERED_BUFFERS = 1024` cap and
`crates/fast_io/src/io_uring/registered_buffers.rs:272` for the page
alignment).

At one session this is invisible. At 100+ concurrent or back-to-back
daemon sessions on a single `oc-rsync --daemon` process (a routine
backup-server profile), three costs compound:

- **Per-connection latency.** Each new session pays the setup cost
  before the first read or write. On bursty arrivals (rsync-as-cron
  fan-in) this stacks behind the accept loop.
- **Pinned kernel memory.** 100 concurrent sessions at defaults =
  ~52 MiB pinned. With `for_large_files()` preset
  (`crates/fast_io/src/io_uring/config.rs:386`, 16 * 256 KiB
  registered buffers) the same load is ~410 MiB pinned.
- **SQPOLL kthread churn.** Each SQPOLL ring spawns its own kthread.
  100 sessions = 100 short-lived kthreads, defeating the per-CPU
  affinity SQPOLL was designed to win.

The async io_uring track (#4217) and the rayon-submission track
(#4220) both increase the ring count per transfer if implemented
without a shared pool, so the cost ceiling rises before any pool work
is done.

## 3. Ring Pool Sketch

The proposed shape is a per-daemon (or per-process) `SessionRingPool`
held alongside `SessionRegistry`:

```text
struct SessionRingPool {
    rings: Vec<Arc<Mutex<RawIoUring>>>,
    free: ArrayQueue<usize>,          // free-slot indices
    leases: DashMap<SessionId, Lease>, // active per-session bindings
    max_size: usize,
    config: IoUringConfig,
}
```

Behaviour:

- **Lazy initialisation.** No rings exist at daemon startup. On the
  first I/O call for a session, `lease(session_id)` either reuses an
  existing slot bound to that `SessionId` or pops from `free` and
  builds the ring via `IoUringConfig::build_ring()`. If `free` is
  empty and `rings.len() < max_size`, push a new slot. If both checks
  fail, fall back to building a private ring for that session (current
  behaviour).
- **Max pool size.** `max_size = min(num_cpus, 16)` by default,
  bounded above by an operator knob `--io-uring-pool-max=N` (env
  `OC_RSYNC_IO_URING_POOL_MAX=N` for systemd / cron use, mirroring the
  knob layout in
  `crates/cli/src/frontend/server/flags.rs:128` for `--io-uring`).
  Sixteen is the highest power-of-two that stays under the typical
  `RLIMIT_NOFILE` of 1024 with 64 fixed-file slots per ring.
- **Eviction policy when sessions outlast their ring.** Two cases:
  1. *Session ends, ring stays.* On `AsyncSession::Drop`
     (`crates/daemon/src/daemon/async_session/session.rs:91`) call
     `pool.release(session_id)`, which moves the slot from `leases`
     into `free` without tearing down the ring. The next session
     reuses it.
  2. *Pool pressure, evict idle ring.* When `lease()` finds `free`
     empty but the LRU slot in `leases` has been idle longer than
     `--io-uring-pool-idle-s=N` (default 30 s, picked to outlast a
     typical multi-transfer rsync `--server` session), drop that
     ring and replace it. The dropped ring closes its kernel fd,
     freeing the SQPOLL kthread.
- **No cross-session ring sharing during use.** A ring is leased to
  exactly one `SessionId` at a time. This preserves single-consumer
  CQ semantics (the upstream `io_uring` crate is `!Sync` on the
  consumer side) and avoids the registered-fd table races discussed
  below.

The "in-process MPMC pool keyed by `IoUringConfig`" variant from
#1409 (already partly implemented per
[`iouring-session-ring-pool-impl.md`](./iouring-session-ring-pool-impl.md))
is a sibling, not a competitor: the session pool is the outer layer
that hands a leased ring to the worker; the MPMC pool inside that
ring is what makes multi-worker access on one ring possible. This
document assumes #2243 will resolve the inner contention first.

## 4. Hazards

- **Rings are kernel objects.** `RawIoUring` holds a kernel fd plus
  SQPOLL kthread (when enabled). Closing must be deterministic: the
  pool's `Drop` must reap all slots, and forced eviction must
  `Drop` the `RawIoUring` value, not just steal the slot. Leaking a
  ring leaks an SQPOLL kthread and a kernel mmap. The drop order
  inside `SharedRing` (`crates/fast_io/src/io_uring/shared_ring.rs:98`,
  field declared first) is the existing template - the pool must
  follow the same rule.
- **Registered-fd races across sessions.** `try_register_fd`
  (referenced in `iouring-session-ring-pool-impl.md:84`) writes into
  the ring's fixed-file table. If session A leaves a slot table half
  populated and session B reuses the ring without `unregister_files`
  first, B sees A's residual slots. Mitigation: `release` must call
  `io_uring_unregister_files`, or the pool must wrap the fixed-file
  table in a per-lease epoch and require lookups to validate epoch.
- **Registered-buffer scope.** `RegisteredBufferGroup` is ring-scoped
  (`crates/fast_io/src/io_uring/file_writer.rs:56` in the older
  per-file path; same pattern in the disk-batch ring at
  `crates/fast_io/src/io_uring/disk_batch.rs:71`). Pooled rings must
  register their buffer set once at slot init; per-lease re-registration
  is a syscall storm. The
  [`io-uring-adaptive-buffer-pool.md`](./io-uring-adaptive-buffer-pool.md)
  work (#2045) interacts with this - the buffer set sizing must be
  picked at pool init, not per-lease.
- **`SharedRing` is `Arc<Mutex<RawIoUring>>` under the hood today.**
  Per the bottleneck noted in #1876, the single mutex serialises all
  submissions. Adding a session pool on top of single-mutex rings just
  moves the contention point. #2243 (per-thread rings) is the
  precondition; without it, the pool serialises N sessions across one
  mutex per ring.
- **EMFILE on the constructor.** `build_ring()` returns
  `io::Error::other("io_uring init failed: ...")` on `EMFILE`
  (`crates/fast_io/src/io_uring/config.rs:338`). The pool must treat
  `EMFILE` as "lower `max_size` permanently and fall back to private
  ring", not retry. The existing `is_io_uring_available()` cache
  (`crates/fast_io/src/io_uring/config.rs:155`) is process-wide and
  does not currently track per-ring failures; the pool needs its own
  watermark.
- **Mixing rings across sessions.** Even with strict lease ownership,
  a registered-fd slot from session A persists on the ring after
  release. If A registered fd 7 in slot 0 and B opens an unrelated fd
  that happens to be reused as 7, B can submit a fixed-file op
  referencing slot 0 and operate on its own fd by coincidence -
  correct, but only because slot 0 was overwritten by B's
  `register_files_update`. The hazard is when B forgets to update:
  the SQE silently uses A's stale fd. Mitigation: clear the fixed-fd
  table on `release`.

## 5. Bench and Measurement Plan

Existing io_uring benches cover steady-state throughput, not setup
overhead:

- `crates/fast_io/benches/iouring_per_file_vs_shared.rs` (#4197) -
  compares per-file ring construction against the shared ring; the
  benched cost includes setup but does not isolate it.
- `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs` (#4201) -
  isolates SQPOLL throughput delta but builds one ring per iter.

Neither targets the "100 concurrent session startups" profile. A new
bench under `crates/fast_io/benches/iouring_session_pool.rs` would:

1. Measure single-session startup latency: `io_uring_setup(2)` +
   `register_files` + `register_buffers` at defaults and at
   `for_large_files()`.
2. Sweep concurrent session counts (1, 10, 50, 100, 200), recording:
   - Wall time from accept to first ring-backed write.
   - Peak pinned kernel memory (`/proc/self/status` `VmPin`).
   - SQPOLL kthread count (`ls /proc/self/task | wc -l` delta).
3. Compare three configurations under the same sweep:
   - **Baseline.** Per-session ring (current behaviour).
   - **Session pool.** Lazy `SessionRingPool`, `max_size = 16`,
     `idle = 30 s`.
   - **Session pool + #2243.** Once per-thread rings land, repeat to
     confirm the pool layer adds no measurable overhead on top.

The bench must gate on `is_io_uring_available()` and skip gracefully
on non-Linux hosts (the existing benches use the same pattern at
`crates/fast_io/benches/iouring_per_file_vs_shared.rs:97`). It must
also degrade when the kernel rejects `io_uring_setup(2)` under load
(report "pool fell back to private rings at N sessions" rather than
fail the bench).

A complementary daemon-side integration measurement lives in
`scripts/benchmark_remote.sh`: add a "concurrent session fan-in" mode
that opens N rsync connections to the same daemon and records first-
byte latency per connection. This is the operator-facing signal the
synthetic bench predicts.

## 6. Recommendation

**Defer until #2243 (per-thread rings) lands** and one round of
concurrent-session bench data is collected with the new bench from
section 5. Rationale:

- Without #2243, the pool layer wraps `Arc<Mutex<RawIoUring>>` and
  serialises N sessions per ring on the mutex. The mutex is the
  bottleneck noted in #1876 and inherited by any pool design.
- The current per-session ring cost is real but bounded; at the
  defaults a 100-session burst pins ~52 MiB and pays ~5-20 ms of
  cumulative setup. This is below the operator pain threshold that
  would justify a feature flag and a stabilisation window.
- The async io_uring work (#4217) and rayon submission work (#4220)
  may shift the per-transfer ring count up. Building a pool against
  today's count risks immediate redesign.
- The companion in-process pool partly landed via #1409 (per
  [`iouring-session-ring-pool-impl.md`](./iouring-session-ring-pool-impl.md));
  its lessons (registered-buffer scope, fd-slot races) carry directly
  into the session-pool design. Letting that work mature before
  layering session keys on top reduces churn.

Action when #2243 closes:

1. Land the bench from section 5 first, on baseline behaviour.
2. Prototype `SessionRingPool` behind a `--io-uring-session-pool`
   flag, default off.
3. Re-run the bench. Promote to default only if startup latency drops
   by >50 % at 100 sessions and pinned memory drops by >30 % with no
   regression on the existing #4197 / #4201 benches.
4. Wire `release(session_id)` from
   `crates/daemon/src/daemon/async_session/session.rs:91`
   (`AsyncSession::Drop`), matching the existing
   `SessionRegistry::unregister` call site.

## 7. Cross-references

- #2243 - per-thread rings (precondition for this work).
- #4197 - `iouring_per_file_vs_shared.rs` bench.
- #4201 - `iouring_sqpoll_vs_regular.rs` bench.
- #1876 - `SharedRing` `Arc<Mutex<RawIoUring>>` bottleneck.
- #4217 - async io_uring composition.
- #4220 - io_uring submission from rayon worker threads.
- #1409 / #1936 - in-process MPMC pool design and partial
  implementation; see
  [`io-uring-ring-pool.md`](./io-uring-ring-pool.md) and
  [`iouring-session-ring-pool-impl.md`](./iouring-session-ring-pool-impl.md).
- #2045 - adaptive registered-buffer sizing; see
  [`io-uring-adaptive-buffer-pool.md`](./io-uring-adaptive-buffer-pool.md).
- #2044 - `BgidAllocator` (buffer-group id namespace bound),
  `crates/fast_io/src/io_uring/buffer_ring.rs:275`.
