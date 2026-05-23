# IUR-2: per-thread io_uring rings (hybrid layout)

Tracking task: **IUR-2**. Predecessor and sibling design notes:

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 caller-surface audit.
  Inventories every `SharedRing`, `RawIoUring`, and factory call site and
  identifies the three hot-path factories (`file_writer`, `file_reader`,
  `socket_writer`), the disk-commit singleton, and the one-shot probes.
- `docs/design/iouring-per-thread-rings.md` - the predecessor option survey
  (#2243) whose recommendation IUR-2 narrows to the hybrid layout below.
- `docs/design/iouring-session-ring-pool.md` - the bounded
  `Mutex<RawIoUring>` pool that ships today as `SessionRingPool` and the
  thread-local primitive `ThreadLocalRingPool` (`session_pool.rs:332-422`).
- `docs/design/io-uring-bgid-namespace.md` and BGE-4 (#2296) - the
  process-global bgid free-list this design leases against.
- `docs/design/mmap-vs-sqpoll-decision.md` and SMR-3c (#2290) - the
  defensive SQPOLL-vs-mmap fallback every per-thread ring inherits.

This is a design-only document. No `.rs` files change. IUR-3.a..g
(#2650-#2656) implement the layout below; IUR-4 stresses it; IUR-5 benches
it; IUR-6 decides the rollout default.

## 1. Hybrid layout

### 1.1 The split

| Path | Topology | Reason |
|------|----------|--------|
| `file_writer` factory (`file_writer.rs:59,85,179,470`) | per-thread ring | per-file ring construction is the dominant kernel-page churn on the receiver write path |
| `file_reader` factory (`file_reader.rs:65`) | per-thread ring | mirror of the writer path on the sender / basis-read side; parallel rayon readers compete for setup syscalls |
| `socket_writer` factory (`socket_writer.rs:64`) | per-thread ring | wire-send loop runs on a dedicated submitter thread today; rayon-parallel staging would otherwise serialize on a shared SQ |
| `socket_reader` factory (`socket_reader.rs:32`) | **shared** (status quo) | one socket reader per session; no contention to relieve; keeps the pull-side simpler |
| `IoUringDiskBatch` (`disk_batch.rs:71`) | **shared** singleton (status quo) | already documented `!Send + !Sync`, pinned to the disk-commit thread; one ring for the life of the session |
| `LinkAt::probe`, `RenameAt::probe`, `Statx::probe` | **shared** (one-shot) | runs once at startup, contributes zero hot-path syscalls; per-thread rings would inflate fd count for nothing |
| `ZeroCopySender::ring` (`send_zc.rs:283-292`) | **deferred** | `Arc<Mutex<RawIoUring>>` is uncontested today (no `from_shared_ring` caller); migration gated by IUS-8 trait abstraction |

The three rows tagged "per-thread ring" are the high-frequency factories
IUR-1 (section 3.4, "contention model") identified as the actual cost
center. The three rows tagged "shared" are the dormant or singleton paths
where per-thread duplication buys nothing.

### 1.2 Why hybrid and not pure-per-thread

Pure-per-thread duplicates rings for paths that have no contention to
dissolve, and pays for that duplication in pinned kernel pages, ring fds,
and (if SQPOLL ever flips on) kthread count.

- **One-shot probes run once per process.** `LinkAt::probe`,
  `RenameAt::probe`, and `Statx::probe` each build a ring, issue one
  capability-detection SQE, reap one CQE, and drop the ring
  (`linkat.rs:113,181`, `renameat2.rs:76,165`, `statx.rs:138,223,305`).
  These are not on any hot path. Routing them through per-thread storage
  would add a TLS slot per probe site for no measurable gain.
- **Disk-commit is a singleton by construction.** The disk-commit thread
  is spawned once per session
  (`crates/transfer/src/disk_commit/thread.rs:47-56`), owns its
  `IoUringDiskBatch`, and that batch owns its `RawIoUring`
  (`disk_batch.rs:46`) for the full session. No second thread submits to
  this ring; making it thread-local would be a no-op rename.
- **Resource cost of pure-per-thread.** Section 4 of
  `iouring-per-thread-rings.md` quantifies the cost at ~520 KiB pinned
  per ring; on a 64-core box pure-per-thread gives 33 MiB of pinned
  kernel pages plus 64 ring fds. Hybrid caps the per-thread count at
  the rayon worker pool (today `min(cores, 16)`,
  `parallel.rs:159,217`) and keeps the disk-commit and probe paths on
  their existing single rings.
- **The bench evidence already supports hybrid.** IUR-1 section 6.4
  recommends "hybrid (per-thread for hot path, shared for rare path)"
  on the same grounds and on the same bench results
  (`benches/iouring_per_file_vs_shared.rs`).

### 1.3 Open issue: `ZeroCopySender` migration

`ZeroCopySender` (`send_zc.rs:283-292`, feature `iouring-send-zc`) holds
`ring: Arc<Mutex<RawIoUring>>` and acquires that mutex around every
`try_send_zc` (`send_zc.rs:419-422`, `:436-439`). IUR-1 section 2.4 flags
it as the only shipped `Arc<Mutex<...>>` over an `IoUring`. Migration is
deferred for three reasons:

1. The mutex is uncontested in practice - `from_shared_ring`
   (`send_zc.rs:345`) has no production caller and there is at most one
   sender per session, so the lock acquire is a fast-path uncontended
   `compare_exchange`.
2. The send-zc feature is opt-in (cargo feature `iouring-send-zc`) and
   default-off (per the memory note on `iouring_send_zc_optin_only`), so
   migrating the ring shape before the feature itself becomes default
   is premature optimisation.
3. The clean migration depends on IUS-8 (the `IoUringSubmitter` trait
   abstraction) so that `ZeroCopySender` can submit through whatever
   per-thread or shared ring fits the active topology, instead of
   owning its own ring directly. Without IUS-8 the migration would
   either duplicate the per-thread plumbing or freeze `ZeroCopySender`
   into the hybrid layout it should outlive.

IUR-3.g tracks this as a future task; it does not block any other IUR-3
subtask. See section 7 for the sequencing.

## 2. Ring construction

### 2.1 Storage shape: `thread_local!` + `OnceLock`

The shipped `ThreadLocalRingPool` already implements the right pattern:
a `thread_local!` `RefCell<Option<RawIoUring>>` keyed by pool id, lazily
populated on first acquire (`session_pool.rs:285-300`). IUR-3.a reuses
that storage. The acquire path is unchanged from
`session_pool.rs:380-422`.

The choice is `RefCell<Option<RawIoUring>>` (existing) over
`OnceLock<RawIoUring>` because:

- The `RefCell` enforces single-borrow per thread at runtime, which
  surfaces re-entrant submit/reap as `None` from `acquire` instead of a
  deadlock. `OnceLock` has no such guard; nested submits on the same
  thread would silently alias the ring cursor.
- `RawIoUring` is `!Sync` and cannot live inside an `OnceLock`-backed
  `Sync` container directly. The `RefCell` wrapper is the simplest
  cell that satisfies the `!Sync` requirement and gives us re-entrancy
  detection for free.
- The "lazy init via `OnceLock`" wording in the IUR-2 spec is a
  description of the lazy-init semantics; the implementation realises
  it with `RefCell::borrow_mut() ... if guard.is_none() ... build`
  (`session_pool.rs:414-419`), which is the same observable contract.

### 2.2 Capacity sizing

Per-ring `sq_entries` defaults to 64
(`crates/fast_io/src/io_uring/config.rs:369-383`, `IoUringConfig`).
Per-thread rings inherit this default. The CLI flag
`--io-uring-depth` already plumbs through to `IoUringConfig::sq_entries`;
no new flag is needed.

The choice of "32, 256, or configurable" is **64 by default, configurable
via the existing `IoUringConfig::sq_entries`** with an env override
`OC_RSYNC_IOURING_SQ_ENTRIES` for bench scenarios:

- 32 is too shallow once a writer batches POLL_ADD + SEND pairs per chunk
  (each chunk consumes two SQEs on the shared-ring path; per-thread rings
  do not pair POLL_ADD but still batch multi-SQE submissions for
  registered-buffer reads).
- 256 inflates the per-ring SQ + CQ allocation to ~2 MiB (the kernel
  rounds up to powers of two for SQ tail, CQ tail, and the SQE array;
  see `iouring-per-thread-rings.md:159-163`) without bench evidence that
  the receiver write path queues that deeply.
- 64 matches what `IoUringDiskBatch` and the bench harness use today, so
  bench results between the shared and per-thread layouts compare like
  for like.

The env override `OC_RSYNC_IOURING_SQ_ENTRIES` is read once at first
ring construction per thread, cached in the `SessionPoolConfig`, and
clamped to `[8, 4096]`. This is IUR-3.a wiring; IUR-5 benches sweep it.

### 2.3 Cleanup on thread exit

Thread-local destructors run when the OS thread exits. The
`RawIoUring` `Drop` impl issues `close(2)` on the ring fd and unmaps
the SQ/CQ pages. No explicit shutdown is needed because:

- Rayon workers are long-lived. The rayon pool tears down at process
  exit; the threads' TLS destructors run as part of normal thread join.
  The kernel reclaims ring fds at process exit regardless.
- Ad-hoc `thread::spawn` workers reach their TLS destructor on `join()`
  and the ring drops there.
- The `RegisteredBufferGroup` drop-order invariant
  (`shared_ring.rs:94-97`,
  `registered_buffers.rs:30-37`) is satisfied because the ring field is
  declared first in any struct that owns both, so kernel-side pinning
  is released before user-side pages drop. The per-thread ring is bare
  `RawIoUring` until IUR-3.e wires the bgid lease (section 3); the
  buffer group then lives in the same `RefCell` slot, ordered after
  the ring.

Explicit shutdown would buy ordering control - drain CQEs before the
ring drops - but the kernel already handles in-flight cancels on
ring-fd close (`IORING_OP_ASYNC_CANCEL` semantics, see
`cancel.rs:1-50`). Nothing on the per-thread path needs deterministic
post-mortem draining; the disk-commit path that does, owns its ring
directly and is not migrated.

The TLS Drop path does have one foot-gun: if `thread_local!`
destructors run in unspecified order at process exit, the ring's
`RegisteredBufferGroup` must drop with the ring. The existing
`ThreadLocalRingPool` storage holds only the ring; the bgid lease
(section 3) lives in a sibling TLS slot **for the same pool id** so
order between the two TLS slots is unspecified. The fix is to colocate
the bgid lease inside the same `(pool_id, ring, bgid_lease)` tuple in
`THREAD_RINGS`, not split across two `thread_local!` cells. IUR-3.e
implements this; the storage shape change is local to
`session_pool.rs:285-300`.

## 3. BGID lease per thread

### 3.1 Current allocator

`BgidAllocator` (`buffer_ring/allocator.rs`) is process-global:

- `NEXT_BGID` (`allocator.rs:39`) is an `AtomicU32` counter handing out
  fresh ids when the free-list is empty.
- `bgid_free_list()` (`allocator.rs:99-101`) is a `Mutex<Vec<u16>>`
  populated by `deallocate` (`allocator.rs:253-285`) and drained first
  by `allocate` (`allocator.rs:215-251`).
- The 16-bit `u16` id space caps total live ids at 65 535; the existing
  `MAX_REGISTERED_BUFFERS = 1024` ceiling is per ring, not global.

The free-list mutex is acquired once per `BufferRing::new` and once per
`BufferRing::Drop`. It is not on the SQE submit path. Per IUR-1
section 4, today's contention on it is below the flame-graph noise floor.

### 3.2 The proposed lease

Each per-thread ring leases a contiguous slice of `u16` bgids from the
global allocator at first use. The slice lives in the same TLS slot as
the ring; `BufferRing::new` calls inside this thread pull bgids from
the slice without touching `bgid_free_list()`. Lease release on thread
drop returns the entire slice to the free-list in one mutex acquisition.

Storage shape (extends `session_pool.rs:285-300`):

```rust
thread_local! {
    static THREAD_RINGS: RefCell<Vec<(
        usize,                                // pool id
        Box<RefCell<Option<RawIoUring>>>,     // existing
        Box<RefCell<Option<BgidLease>>>,      // IUR-3.e addition
    )>> = const { RefCell::new(Vec::new()) };
}

struct BgidLease {
    base: u16,           // first id in the leased slice
    len: u8,             // slice length (matches recommendation in 3.3)
    used: u8,            // next-free offset into the slice
    free_within: Vec<u8>, // returned-but-not-yet-reused offsets
}
```

`BgidLease::allocate()` returns `base + used` and increments `used` when
`free_within` is empty, or pops a returned offset. `BgidLease::deallocate(id)`
pushes `(id - base) as u8` into `free_within`. `Drop` of the lease
returns `[base, base + len)` to `bgid_free_list()` in one batched lock
acquisition.

### 3.3 Slice size

Recommendation: **16 ids per thread**.

Sizing rationale:

- The existing `for_large_files()` default registers 16 buffers per ring
  (`docs/design/io-uring-adaptive-buffer-pool.md`); slice = 16 covers
  the steady-state working set without ever falling back to the global
  allocator on a hot thread.
- At 16 per thread, 100 threads consume 1600 ids - 2.4% of the 65 535-id
  space, well under the 50% warning threshold
  (`allocator.rs:78-85`). 64 rayon workers consume 1024 ids - 1.5%.
- Slice = 32 would double the per-thread reservation with no win at
  current per-ring buffer counts; the only scenario where 32 helps is
  if the adaptive sizing work in BGE-7 / #2045 grows the per-ring
  buffer count past 16. If it does, the lease size is a single
  `const` change in `session_pool.rs`; it is not a layout-level
  commitment.
- Slice = 8 risks falling back to the global free-list under bursty
  load (e.g., a thread that opens 9 files in quick succession), which
  reintroduces the lock acquire the lease exists to avoid.

The lease size is read from `OC_RSYNC_IOURING_BGID_SLICE` at first lease
acquisition per thread for bench sweeps, clamped to `[1, 64]`. Default
constant `BGID_SLICE_PER_THREAD: u8 = 16`.

### 3.4 Cap interaction

`MAX_REGISTERED_BUFFERS = 1024` (`registered_buffers.rs:80`) is the
per-ring registered-buffer ceiling. Per-thread rings inherit this
unchanged; the bgid lease is orthogonal (the lease is just an id
reservation, not a buffer allocation). The 65 535 total-id cap is the
soft ceiling on (`threads` * `slice_size`); at slice = 16 the safe
thread count is 4095, far above any realistic worker count.

If a future deployment uses > 4095 io_uring-submitting threads, the
fallback path in section 5.2 ("first failure routes that thread through
the shared_ring fallback for the rest of its life") kicks in
automatically.

## 4. Submission path

### 4.1 Same-thread submit and reap

Each thread uses its TLS ring directly via the existing
`ThreadLocalRingLease` returned from `acquire`
(`session_pool.rs:387-422`). Submit and reap happen on the same thread,
so the SQE tail is a plain non-atomic write to a per-thread cache line
(`io_uring::SubmissionQueue::push` is `!Sync` for exactly this reason).
No cross-thread atomic is required on the per-ring path.

The factory call sites (`file_writer.rs:59,85,179,470`,
`file_reader.rs:65`, `socket_writer.rs:64`) change from
"`config.build_ring()?`" to "`ring_pool.acquire().or_else(fallback)?`".
The lease wraps the existing per-file submission code; the SQE-construction
code is unchanged.

### 4.2 Cross-thread completion fan-in

Some completions need to land on a different thread than the submitter.
The two cases in scope:

- **Disk-commit fan-in.** The disk-commit thread drains write chunks
  from multiple network-reader threads. Today the chunks travel via
  `crates/transfer/src/disk_commit/...` channels (existing
  crossbeam-backed plumbing); the io_uring ring on the disk-commit
  thread is private to that thread and is unchanged by IUR-2.
- **Per-thread ring CQE drain after the submitter moves on.** A rayon
  worker that submits a basis-file read may finish its CPU task before
  the kernel completes the SQE. The worker can either (a) drain the
  CQE before releasing the lease (today's pattern in
  `disk_batch.rs:204-263`), or (b) defer the drain to a later
  acquire on the same thread. (a) is the path of least surprise and
  matches the existing per-file ring usage; IUR-2 adopts it. (b) is a
  follow-up optimisation tracked under IUR-7 if bench evidence shows
  drain-latency dominates.

There is **no shared CQE bus**. Cross-thread fan-in (case 1) stays on
the existing `crossbeam-channel` (or `crossbeam-queue::ArrayQueue`,
matching the `spsc.rs` style at
`crates/transfer/src/pipeline/spsc.rs`); chunks travel as Rust values,
not as raw CQEs. This is the same shape IUR-1 section 3.4 already
endorses.

### 4.3 Tail synchronisation

Per-ring SQE tail is local to the owning thread because the
`RefCell<Option<RawIoUring>>` is `!Sync`. The `IoUring::completion()`
cursor is similarly per-ring. There is no shared SQ/CQ across rings, so
the kernel-side ordering across rings is independent and unsynchronised.

The kernel sees N independent io_uring contexts, exactly as designed by
the io_uring authors (`man 7 io_uring`, the "scaling considerations"
section). This is the model `glommio` and `tokio-uring` use today.

## 5. Fallback story

### 5.1 What can fail

Per-thread ring construction (`build_ring(&config)` in
`session_pool.rs:267-283`) can fail for:

- `EPERM` - missing `CAP_SYS_NICE` when `IORING_SETUP_SQPOLL` is set
  (handled by the SMR-3c fallback at `config.rs:346-373`).
- `ENFILE` / `EMFILE` - process or system fd table is full; the calling
  thread is the (N+1)th to try to build a ring and the kernel is out
  of headroom.
- `ENOMEM` - the kernel cannot pin the SQ/CQ pages.
- `ENOSYS` - the syscall is absent (kernel < 5.6 or seccomp blocks
  `io_uring_setup(2)`).

The first failure on a thread is recoverable; the rest of the process
keeps running. The choice is whether to fall back to the shared_ring
pool or to error loudly.

### 5.2 Fallback policy

**Per-thread init returns `Result`. First failure on a thread routes
that thread through the shared_ring fallback for the rest of its life;
the failure is recorded in a `THREAD_RING_DISABLED` per-thread `Cell<bool>`
and the metrics counter `THREAD_RING_FALLBACK_COUNT`.**

Rationale:

- Loud failure on a single thread is the wrong reaction. The kernel
  will usually let *some* threads build a ring even when the cap is
  reached; failing the whole process makes the io_uring fast path
  brittle on co-tenant boxes.
- A per-thread permanent fallback avoids hot-path retries. Once a
  thread has failed, every subsequent factory call on that thread
  silently goes to the shared `SessionRingPool` (or, if the shared
  pool also fails, to standard `Read`/`Write` fallback - the same
  path the existing `IoUringOrStd...` enums already encode).
- The metrics counter is observable via `bgid_inflight()`-style
  process-wide getters in `lib.rs`. A small bump is benign; a large
  bump signals that the per-thread layout should be turned off for
  this workload.

The fallback is **not** "fall back to per-call shared ring". The
fallback ring is the `SessionRingPool` IUR-1 section 6.4 identified as
the right home for the metadata path - it is already bounded
(`min(cores, 16)` slots) and already exists in the tree
(`session_pool.rs:145-220`). Routing the thread that lost its
per-thread ring through this pool keeps it on io_uring; only complete
io_uring exhaustion (the pool itself fails) falls back to standard I/O.

### 5.3 Process-wide kill switch

The feature gate `per-thread-rings` (section 8) is the kill switch. If
the bench in IUR-5 shows a regression, flipping the gate off routes
all factories through the existing per-file pattern unchanged. The
fallback policy above operates per thread within the enabled state;
the kill switch operates globally.

## 6. Test plan (drives IUR-4 stress test)

### 6.1 Lock-freedom under fan-in

A test that spawns 16 threads, each submitting 6250 SQEs against its
own per-thread ring (total: 100K submissions), must finish in time
proportional to the per-thread workload, not to the total. The
acceptance criterion: median wall-clock per thread within 10% of the
single-thread baseline. Any super-linear slowdown indicates a hidden
shared lock.

Anti-pattern check: run the same workload through `SessionRingPool`
(`acquire().slot()` + `submit_and_wait`) and confirm the per-thread
variant has measurably lower wall-clock under 8+ threads. This
re-uses the existing bench harness at
`crates/fast_io/benches/iouring_per_file_vs_shared.rs` with a new
`per_thread` row (already itemised in
`iouring-per-thread-rings.md:352-358`).

### 6.2 Throughput scaling

Submission throughput should scale linearly with thread count up to
`num_cpus::get()`. The acceptance grid:

| Threads | Expected throughput | Tolerance |
|---------|---------------------|-----------|
| 1 | baseline | n/a |
| 2 | 1.8x baseline | -10% |
| 4 | 3.6x baseline | -15% |
| 8 | 7.0x baseline | -20% |
| 16 | 12.0x baseline | -25% |

The 25% tail allows for kernel-side contention on shared resources
(page cache, block layer) that no userspace design can eliminate. A
result outside this grid indicates a userspace bottleneck (likely the
bgid free-list or the metrics counters; see section 3.4).

### 6.3 Single-thread parity

Single-thread benchmark vs `SharedRing` baseline must be within +/- 5%.
The per-thread layout exists to remove cross-thread contention; on a
single thread there is no contention to remove and the per-thread
path must not regress against the existing single-owner ring.

This is the stricter of the two acceptance bars: if the single-thread
case regresses by >5%, the per-thread plumbing has overhead beyond
the lease acquisition, which means a latent cost in the TLS lookup or
the bgid lease path. The fix is to optimise those paths before
shipping the migration, not to ship the regression.

### 6.4 Failure injection

Three failure modes from section 5.1 are testable:

- **fd exhaustion** - drop process `RLIMIT_NOFILE` to a low value,
  spawn `N+1` threads where `N` is the resulting per-thread ring
  count, confirm the `N+1`th thread falls back without panicking and
  the process completes the workload.
- **bgid lease exhaustion** - set `OC_RSYNC_IOURING_BGID_SLICE` to a
  value that, multiplied by the thread count, would exceed 65 535,
  confirm the lease allocator surfaces `BgidAllocError::Exhausted`
  and the affected thread falls back to per-call `bgid_free_list()`.
- **seccomp blocks io_uring_setup** - run under a seccomp filter that
  returns `ENOSYS`, confirm every thread falls back to standard I/O
  and the process completes (this is the existing
  `IoUringOrStd...::Std` fallback path, verified end-to-end).

## 7. Migration order (drives IUR-3.a..g sequencing)

| Subtask | Title | Files touched | Acceptance |
|---------|-------|---------------|------------|
| IUR-3.a (#2650) | TLS ring init via thread_local + OnceLock | `session_pool.rs`, `mod.rs` (re-export) | `ThreadLocalRingPool` exposes a lazy-init API; existing tests at `session_pool.rs:683-737` extend to cover the env-override path |
| IUR-3.b (#2651) | Migrate `file_writer` factory | `file_writer.rs:59,85,179,470` | `IoUringWriter::create*` consults the per-thread pool; falls back per section 5.2 |
| IUR-3.c (#2652) | Migrate `file_reader` factory | `file_reader.rs:65` | `IoUringReader::open` consults the per-thread pool |
| IUR-3.d (#2653) | Migrate `socket_writer` factory | `socket_writer.rs:64` | `IoUringSocketWriter::new` consults the per-thread pool |
| IUR-3.e (#2654) | BGID per-thread lease from BGE-4 pool | `session_pool.rs:285-300`, `buffer_ring/allocator.rs` | `BgidLease` lives in the same TLS slot; allocate/deallocate land on the lease before the global free-list |
| IUR-3.f (#2655) | Keep one-shot probes + disk-commit ring shared | (no migration) | `linkat.rs`, `renameat2.rs`, `statx.rs`, `disk_batch.rs` are explicitly excluded from IUR-3; the design doc cited as the source of the exclusion |
| IUR-3.g (#2656) | Track `ZeroCopySender` migration as future task | (post-IUS-8) | Issue marked blocked-on IUS-8; do not start until the submitter trait exists |

The ordering matters in two places:

- IUR-3.a is a prerequisite for 3.b, 3.c, 3.d (all three need the lazy
  init primitive). IUR-3.b..d are independent of each other and can
  ship in parallel PRs.
- IUR-3.e is a prerequisite for any factory that registers buffers
  per ring (today: `file_writer` with the
  `RegisteredBufferGroup` path at `file_writer.rs:79-101`). If
  IUR-3.b ships before 3.e, the `file_writer` ring uses the global
  `bgid_free_list()` directly - functional but loses the lease win.
  Recommend landing 3.e between 3.a and 3.b.

3.f is a no-op record entry; it exists in the subtask graph so the
exclusion is documented and the design doc is the citable source.

3.g is purely a tracking marker; the work itself blocks on IUS-8
landing.

## 8. Rollback plan

### 8.1 Feature gate

Add a cargo feature `per-thread-rings` in `crates/fast_io/Cargo.toml`,
default-off. Every IUR-3.b..d migration is gated:

```rust
#[cfg(feature = "per-thread-rings")]
let writer = match thread_local_pool().acquire() {
    Some(lease) => IoUringWriter::from_lease(lease, /* ... */)?,
    None => IoUringWriter::create_per_file(/* ... */)?,
};
#[cfg(not(feature = "per-thread-rings"))]
let writer = IoUringWriter::create_per_file(/* ... */)?;
```

Default-off means the master branch ships the per-file behaviour even
after IUR-3 lands. CI gains a second job that builds + tests with
`--features per-thread-rings` so the migration code does not bit-rot.

### 8.2 Default-on criteria

Flip the default on only after:

1. IUR-4 stress test passes the section 6 grid.
2. IUR-5 bench shows >= 25% throughput uplift on the tiny-file
   workload vs the per-file baseline (matches the acceptance bar at
   `docs/audits/per-file-vs-shared-uring-ring.md:230-237`).
3. IUR-5 bench shows single-thread parity within +/- 5% (section 6.3).
4. Two consecutive nightly runs of the full interop suite are green
   with the feature enabled.

If any of 1-4 misses, the gate stays off; the migration is reversible
by editing the default-features line in `Cargo.toml`.

### 8.3 Emergency disable

The runtime kill switch is the env var `OC_RSYNC_IOURING_PER_THREAD=0`
read once at process start. When set, every `thread_local_pool()`
acquire returns `None`, the section 5.2 fallback kicks in for every
thread, and the process behaves as if compiled without the feature.
This is the production escape hatch for the case where the build
shipped with `per-thread-rings` enabled and a customer hits the path
described in section 5.1.

## 9. What stays out of scope for IUR-2

- `SharedRing` itself. IUR-1 confirmed it has zero production callers
  (`io-uring-shared-ring-audit.md` section 2.2). IUR-6 decides whether
  to retire or rewire it.
- `BgidAllocator` ceiling lift past 65 535. The 16-bit cap is a kernel
  ABI constant; lifting it is not a userspace decision.
- IOCP integration. The IOCP path is a sibling fast_io strategy and
  has its own IUS / WPG tracker family; per-thread rings are a Linux
  io_uring concept.
- Adaptive ring sizing per thread. The per-ring SQ depth stays at
  `IoUringConfig::sq_entries` with an env override. Adaptive sizing
  is tracked separately under BGE-7 and (if approved) post-IUR.

## 10. Cross-references

- `docs/design/io-uring-shared-ring-audit.md` - IUR-1 caller-surface
  audit; the inventory IUR-2 picks from.
- `docs/design/iouring-per-thread-rings.md` - the predecessor option
  survey (#2243); IUR-2 narrows its 6.1-6.5 enumeration to 6.4 hybrid.
- `docs/design/iouring-session-ring-pool.md` and
  `docs/design/iouring-session-ring-pool-impl.md` - the
  `SessionRingPool` IUR-2 reuses as the per-thread fallback target.
- `docs/design/io-uring-bgid-namespace.md` - BGE-4 (#2296), the
  free-list this design leases against.
- `docs/design/mmap-vs-sqpoll-decision.md` - SMR-3c (#2290), the
  per-ring SQPOLL fallback every per-thread ring inherits.
- `docs/design/iouring-borrowed-slice-consumer.md:277-283` - the
  re-entrancy warning that motivates the section 2.1 `RefCell` choice
  over `OnceLock`.
- `crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297` -
  the existing bench harness IUR-4 / IUR-5 extend with the
  `per_thread` row.
