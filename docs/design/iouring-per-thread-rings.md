# io_uring per-thread rings + work stealing (#2243)

Tracking issue: oc-rsync task #2243. This is a docs-only design note;
no code lands in this PR.

Sibling design notes and audits this document layers on top of:

- `docs/audits/shared-iouring-session-instance.md` (#1408) - the
  audit that first listed per-thread rings as a design option and
  recommended a bounded session pool instead.
- `docs/audits/per-file-vs-shared-uring-ring.md` (#1410) - the
  per-file vs shared-ring evidence plan that #4197 executes.
- `docs/design/iouring-session-ring-pool.md` (#1409 / #1937) - the
  in-process round-robin pool that is the closest already-designed
  alternative to per-thread rings.
- `docs/design/io-uring-rayon-composition.md` (#1283) and
  `docs/design/iouring-rayon-submission.md` (#1284, #4220) - rayon
  composition; per-thread rings would change the answer here.
- `docs/design/io-uring-adaptive-buffer-pool.md` (#2045) - registered
  buffer pool sizing the per-thread design would have to multiply per
  ring.
- `docs/design/iouring-borrowed-slice-consumer.md` - calls out at
  `docs/design/iouring-borrowed-slice-consumer.md:277` that per-thread
  rings amplify re-entrancy hazards on borrowed completion slices.

## 1. Scope

Task #2243 asks whether `Arc<Mutex<...>>` over a single shared ring
should be replaced with **one ring per worker thread plus work
stealing for load balancing**. The reference design at
`docs/design/iouring-session-ring-pool.md:37-39` proposes exactly
this `Mutex<RawIoUring>` shape:

```rust
pub struct RingPool { rings: Vec<Mutex<RawIoUring>>, cursor: AtomicUsize }
pub struct RingLease<'a> { /* &mut RawIoUring guard, returns on drop */ }
```

Per `docs/design/iouring-session-ring-pool.md:51-59`, "rings are
`!Sync` in the upstream crate, so each slot sits behind a
`Mutex<RawIoUring>`. ... shared pool rings become MPMC across workers,
so the lease guard must hold the mutex for the entire submit-and-wait
cycle". The per-thread alternative removes the mutex entirely at the
cost of multiplying kernel resources.

This note records the current submission topology, sketches the
per-thread + work-stealing alternative, lists the hazards that make
work stealing across rings unsafe, surveys the bench evidence, and
recommends a path forward. No wire bytes change.

## 2. Current ring topology

### 2.1 No `Arc<Mutex<...>>` over a ring exists in the tree today

A repository-wide search for `Mutex<RawIoUring>`, `Mutex<SharedRing>`,
or `Arc<Mutex<...>>` over an `IoUring` returns no production matches.
The `Arc<Mutex<...>>` framing from the task title refers to the **proposed**
session ring pool in `docs/design/iouring-session-ring-pool.md:37-59`,
not to code that has shipped. Today every io_uring instance is owned
exclusively by one thread.

### 2.2 The disk-commit thread owns the only session-scoped ring

`IoUringDiskBatch` (`crates/fast_io/src/io_uring/disk_batch.rs:45-54`)
is documented as `!Send + !Sync` and is pinned to a single thread by
construction:

- `crates/fast_io/src/io_uring/disk_batch.rs:42-44` - "This type is
  not `Send` or `Sync` - it is designed for single-threaded use on
  the dedicated disk commit thread."
- `crates/transfer/src/disk_commit/thread.rs:47-56` - the disk thread
  is spawned exactly once per session via `thread::Builder::new()
  .name("disk-commit".into()).spawn(...)`.
- `crates/transfer/src/disk_commit/thread.rs:74-92` -
  `try_create_disk_batch()` builds at most one `IoUringDiskBatch`
  whose `RawIoUring` field (`disk_batch.rs:46`) lives for the entire
  transfer.

The submission sites for this ring are
`crates/fast_io/src/io_uring/disk_batch.rs:204-233` (`flush_current`,
which calls `submit_write_batch`) and
`crates/fast_io/src/io_uring/disk_batch.rs:236-263` (`submit_fsync`).
Both run on the disk-commit thread; neither requires a lock.

### 2.3 Per-file rings on every other I/O path

The receiver write path outside the disk-commit batch and the
generator read path each construct a fresh `RawIoUring` per file:

- `crates/fast_io/src/io_uring/file_writer.rs:59,85,179` - three
  `IoUringWriter` constructors each call `config.build_ring()?` per
  file.
- `crates/fast_io/src/io_uring/file_reader.rs:65` - one ring per
  file on the read side.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `crates/fast_io/src/io_uring/socket_writer.rs:41` - one ring per
  socket endpoint.
- `crates/fast_io/src/io_uring/config.rs:313-340` - the single
  `build_ring()` entry point that all of the above funnel through.

`SharedRing` (`crates/fast_io/src/io_uring/shared_ring.rs:98-111`)
exists as a primitive that co-locates one reader fd and one writer
fd on a single ring, but it is not wired into the production
receiver write path; see
`docs/design/io-uring-ring-pool.md:46-51`.

### 2.4 Net result

The actually-shipping serialization is **per-file ring construction
overhead**, not lock contention. There is no shared ring under a
`Mutex` to contend on - the bench in
`crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297` is
measuring the cost of `io_uring_setup(2)` churn vs single-thread
reuse, not the cost of mutex acquisition.

## 3. Per-thread rings + work stealing: design sketch

### 3.1 One ring per worker thread

Mirror the existing per-thread `BufferPool` pattern: a thread-local
`OnceCell<IoUring>` initialised on first use. This matches the
design option described at
`docs/audits/shared-iouring-session-instance.md:397-412`:

- Thread storage: `thread_local!(static RING: RefCell<Option<RawIoUring>>)`
  or an attached field on `rayon::ThreadPoolBuilder::start_handler`.
- Lifetime: one ring per rayon worker, dropped at thread exit. The
  rayon pool is centralised at `crates/fast_io/src/parallel.rs:159`
  and `:217`, so the init hook lives in one place.
- Sizing: `rayon::current_num_threads()` rings, each at the existing
  `IoUringConfig::sq_entries = 64` (`crates/fast_io/src/io_uring/config.rs:369-383`).

Pros (per `shared-iouring-session-instance.md:403-408`):
- Zero contention. SQE push and CQE drain are lock-free inside one
  thread.
- The kernel's "happy path" - per-thread submission is the model
  io_uring was tuned for.
- Predictable per-ring resource use; SQPOLL kthread, if enabled,
  has one thread to pin to.

Cons:
- N rings * (SQ + CQ + fixed-file table + RegisteredBufferGroup).
  At defaults this is ~520 KiB pinned per ring
  (`docs/design/io-uring-ring-pool.md:96-114`), so on a 64-core box
  that is 33 MiB of pinned kernel pages and 64 ring fds.
- `RegisteredBufferGroup` and `BufferRing` bgid allocation are
  ring-scoped (`crates/fast_io/src/io_uring/buffer_ring.rs:174-245`,
  `crates/fast_io/src/io_uring/registered_buffers.rs`), so the
  adaptive sizing work in #2045 has to be multiplied per ring.
- `bgid` namespace pressure: `BgidAllocator`
  (`crates/fast_io/src/io_uring/buffer_ring.rs:174`, capped at
  `MAX_REGISTERED_BUFFERS = 1024` per
  `crates/fast_io/src/io_uring/registered_buffers.rs:80`) is process
  global. 64 rings * 16 buffers per ring at `for_large_files()`
  exhausts the namespace.

### 3.2 Consumer thread / completion fan-in

With N rings the CQE side becomes interesting. Two options:

1. **Each worker drains its own CQ.** Submitter and reaper are the
   same thread. Simplest and lowest-latency. Used by `glommio` and
   `tokio-uring`. Matches today's
   `IoUringDiskBatch::flush_current` /
   `submit_fsync` (`disk_batch.rs:204-263`) where the same thread
   pushes the SQE and reaps the CQE.
2. **One central consumer thread polls all rings.** Either
   round-robin over `io_uring::IoUring::completion()` on each ring,
   or `epoll_ctl(EPOLL_CTL_ADD, ring.as_raw_fd())` over the ring
   fds and waiting on a single `epoll_wait`. The ring fd becomes
   readable when CQEs are available (Linux 5.4+). This decouples
   completion handling from submission and lets a single CQE
   reaper feed the next pipeline stage (`crates/transfer/src/pipeline/spsc.rs`).

Option 1 is the natural fit for the receiver-side disk write path
because the disk-commit thread already does both. Option 2 is what
would be needed if a per-thread ring is to be useful from a rayon
worker that finishes its CPU work and moves on; otherwise CQE drain
gets deferred until the worker happens to revisit io_uring.

### 3.3 Work-stealing claim

The task asks whether a worker whose ring's SQ is full can hand the
SQE to an idle worker's ring. The mechanics would have to be:

1. Worker A's `submission().push(&entry)` returns "queue full".
2. Worker A locates an idle worker B (some `AtomicUsize` last-touched
   counter or a steal-deque per `rayon` semantics).
3. Worker A pushes the SQE into B's ring.
4. The CQE eventually surfaces on B's CQ; A is notified via a
   shared channel.

This pattern can be made to compile, but every step has a cost that
defeats the lock-free win:

- Step 3 requires synchronising with B. `io_uring::IoUring`
  (`RawIoUring` in the upstream crate v0.7) is `!Sync`; multi-producer
  push needs B's ring behind a `Mutex` or B's submission inside a
  bounded channel that B drains on its next poll. The first option
  reintroduces the mutex that per-thread rings exist to avoid. The
  second adds a channel hop per stolen SQE.
- Step 4 requires routing the CQE back to A. `user_data` is the only
  carrier (`crates/fast_io/src/io_uring/shared_ring.rs:30-42` shows
  the OpTag scheme), so A must encode its identity in the SQE and
  the CQE reaper on B must dispatch to A's completion handler. This
  cross-thread dispatch needs another channel or shared map.
- The stolen SQE references buffers from A's
  `RegisteredBufferGroup`. The kernel pins the registered set to the
  ring fd
  (`crates/fast_io/src/io_uring/registered_buffers.rs:30-37`); B
  cannot submit `READ_FIXED` / `WRITE_FIXED` against A's buffer
  group at all. The stolen SQE has to fall back to unregistered
  `READ` / `WRITE`, losing the fast path.
- Fixed-file slots (`IORING_REGISTER_FILES`) are also per-ring
  (`crates/fast_io/src/io_uring/batching.rs::try_register_fd`). B's
  ring does not know about A's fd registrations; the stolen SQE has
  to use raw fds or re-register on B (another syscall).

The "idle thread submits on behalf of the busy thread" pattern is
recognisable from work-stealing schedulers (rayon, Tokio), but those
schedulers steal **CPU-bound tasks** whose state is fully
self-contained. An io_uring SQE is **not self-contained**: it
references ring-scoped registered buffers, ring-scoped fixed-file
tables, and a ring-scoped `bgid`. The steal-and-submit path requires
either (a) demoting the SQE to the unregistered slow path or (b)
synchronising state across rings, both of which negate the per-thread
fast-path that motivated the design.

### 3.4 Backpressure as the alternative to work stealing

The simpler reaction to "SQ full" is to back-pressure the submitter:
spin briefly, drain the CQ, retry the push. This is what
`crates/fast_io/src/io_uring/batching.rs::submit_write_batch` already
does. The cost of an occasional retry on the owning thread is
typically less than the cost of a cross-thread channel hop plus a
fallback to unregistered I/O.

If "SQ full" is frequent enough to matter, the right answer is a
**deeper SQ per ring**
(`IoUringConfig::sq_entries`, currently 64 -
`crates/fast_io/src/io_uring/config.rs:369-383`) or fewer concurrent
submitters per ring, not a steal pipeline.

## 4. Hazards

### 4.1 Cross-thread submission is unsafe without ring-local locks

Re-stating section 3.3 as a hazard list:

- `io_uring::IoUring` is `!Sync`. The only Send-safe wrapper is a
  `Mutex<RawIoUring>`. Cross-thread submission means a lock per SQE
  push at the destination, which is the very contention per-thread
  rings exist to avoid (`docs/design/iouring-session-ring-pool.md:51-59`).
- Registered buffers are ring-scoped
  (`crates/fast_io/src/io_uring/registered_buffers.rs:30-37`). A
  stolen SQE cannot use the originating ring's registered buffers.
- Fixed-file tables are ring-scoped. The stolen SQE either skips
  registration or re-registers on the destination ring (one extra
  syscall).
- `bgid` namespace is process-global with a 1024 ceiling
  (`crates/fast_io/src/io_uring/registered_buffers.rs:80`). N rings
  with adaptive buffer counts (#2045) burn through it faster.

### 4.2 Re-entrancy on borrowed completion slices

`docs/design/iouring-borrowed-slice-consumer.md:277-283` warns that
"per-thread rings amplify the re-entrancy hazard (each thread is its
own consumer-and-resubmitter)". Any consumer that holds a borrowed
slice across `consume()` calls is unsafe the moment the same thread
submits another SQE that may reuse the slot. The borrowed-slice
optimisation is unrelated to #2243's primary goal but the hazard
shows up at the same source.

### 4.3 Ring teardown order

`RegisteredBufferGroup::Drop` must run **after** `RawIoUring::Drop`
to avoid use-after-free of pinned pages
(`crates/fast_io/src/io_uring/registered_buffers.rs:30-37`,
`crates/fast_io/src/io_uring/shared_ring.rs:94-97`). Per-thread
storage with thread-local destructors can run in any order at thread
exit; the implementation has to ensure the ring drops before its
buffers regardless of `thread_local!` ordering.

### 4.4 SQPOLL kthread multiplication

`IoUringConfig::sqpoll` defaults off
(`crates/fast_io/src/io_uring/config.rs:369-383`), but if a user
turns it on each ring spawns its own `io_uring-sq` kernel thread
(`docs/audits/iouring-sqpoll-bench-plan.md:80-91`). N rings = N
kthreads, each pinned to 100% of a core when busy. This is
catastrophic on `--io-uring-depth` rings shared across rayon workers
on a 64-core box.

### 4.5 Adaptive buffer pool multiplication

The adaptive sizing work in #2045 grows the registered-buffer count
under sustained load
(`docs/design/io-uring-adaptive-buffer-pool.md`). Per ring this is
fine; multiplied by N rings the total pinned page budget is the
product of the per-ring growth and the worker count. The current
single-ring assumption in #2045 has to be revisited as a prerequisite
to per-thread rings.

## 5. Bench evidence

### 5.1 What exists today

- `crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297` -
  `shared_ring` group. Measures one long-lived ring servicing 100K
  4 KiB writes vs a fresh ring per file. The "shared" prototype is
  not behind a mutex; the bench measures `io_uring_setup` /
  `mmap` churn, not lock contention. Per
  `crates/fast_io/benches/iouring_per_file_vs_shared.rs:51-62`, the
  outcome rubric is: shared wins by >= 25% on 4 KiB workload =>
  promote #2243 to P1; within +/- 10% => keep at current priority.
- #4197 records "single-digit-percent throughput differences once
  the ring is reused across files" per
  `docs/design/iouring-borrowed-slice-consumer.md:98-102`. That is
  the magnitude available from removing **construction** overhead.
- #4201 (SQPOLL) is orthogonal. SQPOLL removes the
  `io_uring_enter` syscall on submit
  (`docs/design/iouring-borrowed-slice-consumer.md:103-106`); it
  does not address mutex contention.
- The per-file vs shared bench plan
  (`docs/audits/per-file-vs-shared-uring-ring.md:230-253`)
  explicitly does **not** include a "shared-with-mutex" row. There
  is no published number for how much a `Mutex<RawIoUring>` would
  cost in the receiver write path.

### 5.2 What is missing

To justify implementing per-thread rings, the bench grid needs a new
row that does not yet exist:

| Topology | Description | Currently benched? |
|----------|-------------|--------------------|
| per-file | Fresh ring per file | yes (`iouring_per_file_vs_shared::per_file_ring`) |
| shared, single owner | One ring, one thread | yes (`iouring_per_file_vs_shared::shared_ring`) |
| shared, `Mutex<RawIoUring>` | One ring, N producers | **no** |
| per-thread, N rings | One ring per worker | **no** |
| per-thread, N rings + steal | One ring per worker, cross-ring steal on SQ full | **no** |

The third row is the actual baseline for #2243. Without it, every
claim about "per-thread rings beat the shared ring" is unsubstantiated.

### 5.3 Target win

`docs/audits/per-file-vs-shared-uring-ring.md:230-237` puts the
acceptance bar at **>= 25% throughput uplift on the tiny-file
shape** vs the next-best topology, anchored on the rationale that
"the shared ring carries new lifetime hazards". The same bar
applies to per-thread rings, with one addition: the per-thread
design also has to clear the **session pool** alternative
(`docs/design/iouring-session-ring-pool.md`), not just the
single-ring baseline. The pool already gets the SQ-tail mutex onto
a path of length 1 lock per lease; per-thread rings only win if
they beat the pool by enough to justify N rings of pinned kernel
state.

A useful upper bound: SQ-tail mutex acquisition is a single
`compare_exchange` on a cache line. On contended workloads
(>10 producers per ring) this is 50-200 ns per submit. At
`sq_entries = 64` and `submit_and_wait` cycles in the microsecond
range, the mutex is < 5% of submit cost. **Per-thread rings cannot
deliver a 25% win unless mutex contention >= 20%, which the
bench grid above is designed to measure.**

## 6. Recommendation

**Defer #2243 until the bench grid in section 5.2 is complete and
shows the shared-ring mutex is the actual bottleneck.**

Justification:

1. The `Arc<Mutex<...>>` over a single ring that #2243 is asking to
   replace **does not exist in tree today**. It is a proposed
   design (`docs/design/iouring-session-ring-pool.md:37-59`) that
   has not been implemented. There is nothing to replace; there is
   only a choice between two future designs.
2. The cheapest next step is the session pool from #1937, which
   gives a measurable amortisation of ring construction (the actual
   bottleneck per `docs/audits/shared-iouring-session-instance.md:65-83`)
   while keeping per-lease ownership single-threaded. The mutex on
   the pool slot is only acquired once per file, not once per SQE,
   so contention scales with file count not submission count.
3. Per-thread rings have real cons (sections 3.1 and 4.4): N rings
   of pinned state, N SQPOLL kthreads, `bgid` namespace pressure
   multiplied by N. These are not free.
4. Work stealing across rings is not actually feasible without
   either (a) demoting the stolen SQE to the unregistered slow
   path or (b) synchronising state across rings with a mutex that
   defeats the design (section 3.3). Either way, the steal pipeline
   is not the answer to backpressure; deeper per-ring SQs are
   (section 3.4).
5. The acceptance bar at
   `docs/audits/per-file-vs-shared-uring-ring.md:230-237` is
   **>= 25% throughput uplift**. Without the
   `shared+Mutex<RawIoUring>` baseline row, no number meets this
   bar.

Concrete sequencing:

1. **Land #1937 first** (`iouring-session-ring-pool-impl.md`). The
   pool is the cheapest design and serves as the baseline for any
   future per-thread comparison.
2. **Extend the bench harness** at
   `crates/fast_io/benches/iouring_per_file_vs_shared.rs` with the
   `shared+Mutex` and `per_thread` rows from section 5.2. The bench
   is already env-gated (`OC_RSYNC_BENCH_IOURING_RING=1`); adding
   rows does not affect default CI cost.
3. **Re-evaluate #2243** with numbers. If mutex contention on the
   pool >= 20% under representative concurrency, per-thread rings
   are warranted and this doc converts to an implementation plan.
   If contention < 20%, close #2243 as "not the bottleneck".

This recommendation matches the explicit guidance in the upstream
audit at
`docs/audits/shared-iouring-session-instance.md:414-447`, which
selected the bounded session pool over per-thread rings on the
grounds of bounded resource use.

## 7. Cross-references

Tracking issues, in scope order:

- **#2243** (this doc) - per-thread io_uring rings + work stealing.
- **#1408** Shared io_uring session instance audit
  (`docs/audits/shared-iouring-session-instance.md`) - the parent
  audit; per-thread rings appear in section 3.2.
- **#1409** Per-session ring pool design
  (`docs/design/iouring-session-ring-pool.md`) - the alternative
  this doc compares against.
- **#1937** Per-session ring pool implementation
  (`docs/design/iouring-session-ring-pool-impl.md`) - the
  prerequisite for any meaningful per-thread comparison.
- **#1410** Per-file vs shared ring bench plan
  (`docs/audits/per-file-vs-shared-uring-ring.md`) - the bench
  grid that needs the `shared+Mutex` row added.
- **#4197** io_uring per-file vs shared ring bench (results).
  Records single-digit-percent throughput differences from ring
  reuse alone, per
  `docs/design/iouring-borrowed-slice-consumer.md:98-102`.
- **#4201** SQPOLL submission mode evaluation
  (`docs/audits/iouring-sqpoll-bench-plan.md`,
  `docs/design/io-uring-submission-modes-bench-plan.md`) -
  orthogonal to mutex contention; removes the per-submit syscall.
- **#4214** Per-file ring bench (the bench at
  `crates/fast_io/benches/iouring_per_file_vs_shared.rs`) - the
  harness this doc would extend with the `shared+Mutex` and
  `per_thread` rows.
- **#4220** io_uring submission from rayon worker threads
  (`docs/design/iouring-rayon-submission.md`) - if per-thread rings
  ever land, this is the integration point.
- **#2045** Adaptive registered-buffer pool
  (`docs/design/io-uring-adaptive-buffer-pool.md`,
  `docs/design/iouring-adaptive-buffer-pool.md`) - per-thread
  rings multiply the per-ring buffer budget by N; #2045 has to be
  revisited as a prerequisite.

Source file citations:

- `crates/fast_io/src/io_uring/config.rs:313-340` - the single
  `build_ring()` entry point all rings flow through.
- `crates/fast_io/src/io_uring/config.rs:369-383` -
  `IoUringConfig::default` (`sq_entries = 64`, `sqpoll = false`).
- `crates/fast_io/src/io_uring/disk_batch.rs:42-54` -
  `IoUringDiskBatch` documented `!Send + !Sync`.
- `crates/fast_io/src/io_uring/disk_batch.rs:46` - the `RawIoUring`
  field that today's "shared ring" actually is, single-owner.
- `crates/fast_io/src/io_uring/disk_batch.rs:204-263` -
  `flush_current` and `submit_fsync` submission sites.
- `crates/fast_io/src/io_uring/shared_ring.rs:98-111` -
  `SharedRing`, the reader+writer co-located primitive.
- `crates/fast_io/src/io_uring/shared_ring.rs:94-97` - drop order
  invariant (ring fd before buffer group).
- `crates/fast_io/src/io_uring/registered_buffers.rs:30-37` -
  kernel-side pinning cleanup contract.
- `crates/fast_io/src/io_uring/registered_buffers.rs:80` -
  `MAX_REGISTERED_BUFFERS = 1024` bgid ceiling.
- `crates/fast_io/src/io_uring/buffer_ring.rs:174` -
  `BgidAllocator` process-global namespace.
- `crates/fast_io/src/io_uring/file_writer.rs:59,85,179` and
  `crates/fast_io/src/io_uring/file_reader.rs:65` - per-file ring
  construction sites.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `crates/fast_io/src/io_uring/socket_writer.rs:41` - per-socket
  ring construction sites.
- `crates/fast_io/benches/iouring_per_file_vs_shared.rs:264-297` -
  the `shared_ring` bench group.
- `crates/transfer/src/disk_commit/thread.rs:47-92` - the only
  long-lived ring owner today.
