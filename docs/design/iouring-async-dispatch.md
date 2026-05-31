# ASY-9.a: io_uring async dispatch design

Tracking task: ASY-9.a (#3001).

Predecessor and sibling design notes:

- `docs/design/asy-3-async-boundary-spec.md` - ASY-3 per-boundary
  disposition. Boundaries 9 and 10 place disk-commit + io_uring
  inside a long-lived `spawn_blocking` task. ASY-9 was punted there
  with: "native `tokio-uring` decision still deferred."
- `docs/design/async-iouring-integration-eval.md` (#1595) - evaluation
  of four async/io_uring crossing shapes. Recommended Option B+D:
  io_uring stays synchronous, `spawn_blocking` per session, no second
  runtime. This document builds on that conclusion and specifies what
  "ASY-9 native" would concretely look like.
- `docs/design/iur-2-per-thread-rings.md` - hybrid per-thread ring
  layout: `file_writer`, `file_reader`, `socket_writer` on per-thread
  rings; disk-commit and probes on shared singletons.
- `docs/design/iouring-per-thread-rings.md` (#2243) - per-thread ring
  primitive survey and work-stealing rejection.
- `docs/design/asy-2-tokio-runtime-feature.md` - `tokio-transfer`
  feature gate; `fast_io` is explicitly outside its scope.
- `docs/design/daemon-async-runtime-choice.md` - chose tokio
  `multi_thread` for the daemon accept loop.
- `docs/design/mmap-vs-sqpoll-decision.md` (SMR-2) - SQPOLL vs mmap
  conflict resolution; defensive fallback to non-SQPOLL when basis is
  mmap'd.

## 1. Problem statement

The per-thread ring topology (IUR-3.a) and the async daemon accept loop
(tokio `multi_thread`) coexist today through a `spawn_blocking` boundary
at session granularity. All io_uring submissions stay on OS threads -
rayon workers, the disk-commit thread, or the main thread for CLI
invocations. This works but imposes a hard constraint: every io_uring
consumer must own its calling thread for the duration of
`submit_and_wait`. No task-level interleaving is possible.

`tokio-uring` offers an alternative model: a tokio-integrated io_uring
driver where SQE submission and CQE reaping are async operations. Futures
yield while the kernel processes SQEs, and the tokio runtime can schedule
other work on the same thread.

This document answers: should we adopt `tokio-uring` alongside or in
place of the per-thread ring path, and if so, how?

## 2. tokio-uring API shape (0.5.x)

Key characteristics of the `tokio-uring` crate as of v0.5:

- **Requires `current_thread` runtime.** `tokio_uring::start(async { })` builds a single-threaded tokio runtime that owns one io_uring
  ring. The runtime drives both the io_uring CQ and the tokio reactor
  from the same thread. Tasks spawned inside are `!Send` because they
  hold references to thread-local ring state.
- **Buffer ownership transfer.** Every I/O operation takes ownership of
  the buffer (`Vec<u8>`, `bytes::BytesMut`, etc.) for the duration of
  the operation and returns it alongside the result:
  `(result, buffer) = file.read_at(buf, offset).await`. This prevents
  use-after-free across the kernel boundary but is incompatible with
  our `RegisteredBufferSlot` checkout/return pattern.
- **No registered buffers / fixed fds.** `IORING_REGISTER_BUFFERS`,
  `IORING_REGISTER_FILES`, `IORING_REGISTER_PBUF_RING` are not
  exposed. Our production path uses all three.
- **No SQPOLL.** `IORING_SETUP_SQPOLL` is not configurable through the
  public API.
- **No advanced opcodes.** `IORING_OP_LINKAT`, `IORING_OP_RENAMEAT`,
  `IORING_OP_STATX`, `IORING_OP_SEND_ZC` have no wrappers.

## 3. Key decision: replace or coexist?

### 3.1 Replace per-thread rings with tokio-uring

This would mean deleting `per_thread_ring.rs`, the `with_ring` API,
`BgidLease`, `RegisteredBufferGroup`, and the entire IUR-3 topology.
Every io_uring consumer would become an async function running inside a
`tokio-uring` runtime.

**Rejected.** Reasons:

1. **`current_thread` conflicts with `multi_thread` daemon.** The daemon
   listener runs on a `multi_thread` tokio runtime
   (`docs/design/daemon-async-runtime-choice.md`). `tokio-uring` tasks
   are `!Send` and cannot be spawned on a `multi_thread` runtime. The
   only bridge is `LocalSet` + a dedicated OS thread per io_uring
   runtime, which recreates `spawn_blocking` with more machinery.

2. **Feature regression.** tokio-uring 0.5.x lacks registered buffers,
   fixed fds, PBUF_RING, SQPOLL, LINKAT, RENAMEAT, STATX, and SEND_ZC.
   Every one of these is in production use:
   - `registered_buffers.rs` - `IORING_REGISTER_BUFFERS`
   - `config.rs:313` - `IORING_SETUP_SQPOLL`
   - `buffer_ring.rs` - `IORING_REGISTER_PBUF_RING`
   - `linkat.rs` - `IORING_OP_LINKAT`
   - `renameat2.rs` - `IORING_OP_RENAMEAT`
   - `statx.rs` - `IORING_OP_STATX`
   - `send_zc.rs` - `IORING_OP_SEND_ZC`

3. **Buffer ownership model incompatible.** Our `RegisteredBufferSlot`
   pattern pins pages once and cycles slots via an atomic bitset. The
   buffer never moves - the slot index is passed to the kernel in the
   SQE. tokio-uring's `BufResult` pattern surrenders and returns the
   buffer on every operation, which is fundamentally incompatible with
   kernel-registered pages.

4. **CLI becomes runtime-dependent.** The CLI calls the sync facade
   directly. Replacing per-thread rings with tokio-uring forces a tokio
   runtime into every CLI invocation for no benefit.

### 3.2 Coexist: tokio-uring for some paths, per-thread for others

A hybrid where the daemon's async socket I/O uses tokio-uring while
disk I/O keeps per-thread rings.

**Rejected.** Reasons:

1. **Two ring topologies, doubled test surface.** Every code path
   through `fast_io::io_uring` would need both a sync and an async
   variant. The test matrix doubles. Wire-byte parity across the two
   paths must be proven independently.

2. **The daemon socket I/O is already covered.** `SharedRing`
   (`shared_ring.rs`) co-locates reader + writer on one ring with
   `OpTag`-based CQ demux. The existing `IoUringSocketReader` /
   `IoUringSocketWriter` adapters run on dedicated threads and match
   upstream rsync's `io.c:io_loop` event model. tokio-uring adds a
   second way to do the same thing with no throughput gain (the socket
   path is not CPU-bound; it is waiting on the wire).

3. **`!Send` tasks cannot share state with rayon.** The receiver's
   parallel delta-apply uses rayon `par_iter` for checksum verification
   and writes results through a `DashMap`. A tokio-uring task cannot
   participate in this pipeline without `spawn_blocking` back into rayon,
   eliminating any async benefit.

4. **BGID lease (BGE-4) is thread-local by design.** `BgidLease`
   (`bgid_lease.rs`) amortises mutex acquisitions by caching bgids in
   `thread_local!` storage. tokio-uring tasks that migrate between
   threads (they do not in `current_thread`, but any future `multi_thread`
   uring runtime would) invalidate the lease model.

### 3.3 Recommendation: keep io_uring synchronous

**Adopt the same conclusion as `async-iouring-integration-eval.md`
(Option B+D) with no carve-out for tokio-uring.**

The per-thread ring topology (IUR-3) is the correct shape for this
workload. io_uring's value is syscall amortisation through batched
submission, not cooperative scheduling. The `submit_and_wait(n)` model
batches 64 SQEs per kernel entry and is already running on threads
dedicated to I/O (disk-commit, rayon writers). Making submission async
would add a scheduler hop per SQE for no throughput gain because the
thread has nothing else to do while waiting for the CQE.

## 4. Can tokio-uring coexist with per-thread rings for different use cases?

**No practical use case justifies the coexistence today.** The analysis:

| Use case | Per-thread ring | tokio-uring | Winner |
|----------|----------------|-------------|--------|
| Disk writes (receiver) | submit_and_wait batches of 64, pinned pages, registered buffers | await per-op, buffer ownership transfer | Per-thread: fewer syscalls, zero scheduler overhead |
| Disk reads (sender/basis) | batched READ_FIXED with PBUF_RING | await per-read | Per-thread: registered buffers give zero-copy kernel-to-user |
| Socket send (daemon TCP) | POLL_ADD + SEND on SharedRing | await send | Tie: both models wait for the kernel; per-thread avoids runtime overhead |
| Atomic metadata ops (linkat, renameat2, statx) | one-shot submit_and_wait(1) | not supported in tokio-uring | Per-thread: only option |
| Daemon accept loop | n/a (uses tokio TcpListener) | could use tokio-uring accept | Tokio: already shipped, multi_thread runtime |

Every hot path is better served by the per-thread ring. The only place
tokio-uring could theoretically win - interleaving socket I/O with
other async tasks on the same thread - is already handled by the
`SharedRing` + dedicated thread topology, which avoids the `!Send`
constraint and composes cleanly with rayon.

**Future reconsideration gate:** if `tokio-uring` ships a `multi_thread`
runtime with registered-buffer support, re-evaluate. Until then, the
per-thread topology has strictly better feature coverage and strictly
lower overhead.

## 5. BGID leasing under tokio-uring

The BGE-4 `BgidLease` primitive (`bgid_lease.rs`) is designed around
`thread_local!` storage:

- One `BgidLease` per OS thread, lazily built via `with_thread_lease`.
- Batch-allocates 16 bgids from the central `BgidAllocator` mutex.
- Returns bgids to the central pool on thread exit via TLS destructor.
- The lease cache is a `Vec<u16>` behind a `RefCell` - neither `Send`
  nor `Sync`.

tokio-uring's `current_thread` runtime pins tasks to one OS thread, so
the lease model would technically work inside a single tokio-uring
runtime instance. However:

1. **PBUF_RING is not exposed by tokio-uring.** Without
   `IORING_REGISTER_PBUF_RING`, bgids have no consumer. The lease
   infrastructure would be dead code.
2. **Multiple tokio-uring runtimes in one process** (one per session)
   would each run on a separate OS thread. Each gets its own TLS lease.
   This is functionally identical to the per-thread ring model - the
   lease works, but tokio-uring adds no value over `with_ring`.
3. **A hypothetical multi-threaded tokio-uring** would break the lease
   model because tasks could migrate between workers. The lease would
   need to become `Arc<Mutex<...>>` per task or per ring, re-introducing
   the central-pool contention that `BgidLease` was built to avoid.

**Decision:** BGID leasing remains thread-local. No adaptation is
needed because tokio-uring is not adopted. If a future multi-threaded
async uring runtime emerges, the lease model must be redesigned as a
per-ring (not per-thread) allocator.

## 6. SQPOLL interaction

### 6.1 Background: SQPOLL + mmap race (SQM series)

`IORING_SETUP_SQPOLL` enables a kernel thread that polls the submission
queue, eliminating `io_uring_enter` syscalls. The SQM series
(`docs/design/sqpoll-mmap-race-symptoms.md`,
`docs/design/mmap-vs-sqpoll-decision.md`) identified a race:

- The SQPOLL kthread asynchronously processes SQEs referencing
  user-space memory.
- If the basis file is mmap'd and the mmap is torn down (munmap or
  process exit) while the SQPOLL kthread holds a pending SQE
  referencing that mapping, the kernel faults on stale page tables.
- Mitigation: `IoUringConfig` sets `mmap_basis_active = true` when a
  basis file is memory-mapped, and `build_ring()` downgrades to
  non-SQPOLL mode when the flag is set.

### 6.2 How tokio-uring would interact with SQPOLL

tokio-uring does not expose `IORING_SETUP_SQPOLL` at all, so the race
cannot manifest through its API. This is a feature-regression resolution
rather than a safety improvement - the hazard is avoided by lacking the
feature that triggers it.

If a future version of tokio-uring exposes SQPOLL:

- The same defensive fallback (`mmap_basis_active` interlock) must be
  replicated at the tokio-uring ring-construction site.
- tokio-uring's buffer ownership model partially mitigates the race
  because buffers are surrendered for the duration of each operation.
  However, the mmap hazard is about the *basis file* mapping, not the
  I/O buffer. The SQE references the basis-file offset through a
  kernel-side `struct file` read, not through a user-space pointer, so
  the buffer-ownership model is irrelevant to this specific race.
- The `current_thread` runtime means all submissions and completions
  happen on one thread. Unlike per-thread rings where rayon workers
  submit concurrently and the SQPOLL kthread races all of them, a
  single-threaded tokio-uring runtime serializes submission naturally.
  The SQPOLL kthread still races the userspace thread, but there is
  exactly one submission queue to contend with.

### 6.3 SQPOLL under per-thread rings (status quo)

The per-thread ring topology already handles the SQPOLL/mmap interaction
correctly:

- `IoUringConfig::build_ring()` checks `mmap_basis_active` and falls
  back to regular submission mode when set.
- Each per-thread ring's SQPOLL kthread operates on a disjoint SQ/CQ
  pair. There is no cross-thread SQ contention because each ring is
  `!Sync` and accessed through `RefCell` in `thread_local!` storage.
- The SQPOLL fallback flag `SQPOLL_FALLBACK` is a process-wide atomic
  set once on the first `EPERM` failure, visible to all threads for
  diagnostics.

No changes needed for the per-thread path.

## 7. Architecture diagram

```
                    +--------------------------+
                    |   tokio multi_thread     |
                    |   (daemon accept loop)   |
                    +-----+--------------------+
                          |
                    spawn_blocking (per session)
                          |
                    +-----v--------------------+
                    |   core::session()        |
                    |   (sync transfer facade) |
                    +-----+--------------------+
                          |
              +-----------+-----------+
              |                       |
     +--------v--------+    +--------v--------+
     | rayon workers    |    | disk-commit     |
     | (file_writer,   |    | thread          |
     |  file_reader,   |    | (IoUringDisk-   |
     |  socket_writer) |    |  Batch)         |
     +--------+--------+    +--------+--------+
              |                       |
     +--------v--------+    +--------v--------+
     | per-thread ring  |    | singleton ring  |
     | (IUR-3.a)       |    | (disk_batch.rs) |
     | thread_local!   |    | !Send + !Sync   |
     | RefCell<Option>  |    | owned by thread |
     +--------+--------+    +--------+--------+
              |                       |
     +--------v--------+    +--------v--------+
     | BgidLease       |    | n/a (no PBUF    |
     | (BGE-4, TLS)    |    |  on this ring)  |
     +-----------------+    +-----------------+

     tokio-uring: NOT ADOPTED (see section 3)
```

## 8. What would change this decision

The recommendation to keep io_uring synchronous and not adopt
tokio-uring is contingent on these assumptions holding:

1. **tokio-uring stays `current_thread` only.** If a future release
   provides a `multi_thread` uring runtime where tasks are `Send` and
   the runtime owns multiple rings with work-stealing across them,
   the `!Send` constraint disappears and the "two runtimes" objection
   weakens.

2. **tokio-uring lacks registered buffers.** If a future release
   exposes `IORING_REGISTER_BUFFERS` and `IORING_REGISTER_PBUF_RING`
   with a zero-copy API (buffer pinning without ownership transfer),
   the feature-regression objection disappears.

3. **The workload remains batch-oriented.** If oc-rsync adds a use case
   where thousands of small I/O operations must be interleaved with
   non-I/O async work on the same thread (e.g., a daemon proxy mode
   with per-byte forwarding), the scheduler-hop cost of `spawn_blocking`
   per batch could dominate and an async uring path would pay off.

4. **SQPOLL becomes the default.** If SQPOLL adoption rises and the
   `mmap_basis_active` interlock is proven reliable across kernel
   versions, the value of a tokio-uring runtime that naturally
   serializes submission (section 6.2) might justify the migration
   cost - but only if assumptions 1 and 2 also flip.

None of these conditions hold today (2026-06-01). Re-evaluate at the
next major io_uring feature milestone or when tokio-uring reaches 1.0.

## 9. Concrete recommendation

1. **Do not add `tokio-uring` as a dependency.** No crate in the
   workspace depends on it.

2. **Keep all io_uring submission synchronous.** The `submit_and_wait`
   model in `fast_io::io_uring` is the only submission path.

3. **Keep per-thread rings as the hot-path topology.** `with_ring` /
   `PerThreadRing` / `BgidLease` / `RegisteredBufferGroup` remain the
   production stack for receiver writes, sender reads, and socket sends.

4. **Keep `spawn_blocking` as the sole async/sync boundary.** The tokio
   daemon hands off to `core::session()` through one `spawn_blocking`
   per connection. Inside that scope, all io_uring calls execute freely
   without crossing back into the async world.

5. **SQPOLL stays gated by `mmap_basis_active`.** The defensive fallback
   from SQM-2 is unchanged.

6. **Document the non-adoption.** This document serves as the decision
   record. Future work proposing tokio-uring integration must address
   the four conditions in section 8.

## 10. Cross-references

- ASY-3 boundary 9: `spawn_blocking` island for disk-commit. This
  document confirms the island remains synchronous inside.
- ASY-3 boundary 10: io_uring `submit_and_wait` co-located inside
  boundary 9. Confirmed unchanged.
- IUR-2: hybrid per-thread layout. Remains the production topology.
- IUR-3.a: `per_thread_ring.rs` - the `thread_local!` + `RefCell`
  primitive. No tokio-uring replacement.
- IUR-3.e / BGE-4: `bgid_lease.rs` - thread-local BGID batching.
  Stays TLS-based.
- SQM series: SQPOLL + mmap race. Interlock unchanged.
- `async-iouring-integration-eval.md`: predecessor evaluation. This
  document is the focused successor for the tokio-uring question
  specifically.
