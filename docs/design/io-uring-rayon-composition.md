# io_uring + rayon Composition Pipeline (#1283)

Tracking issue: oc-rsync task #1283 (this design note).
Implementation follow-up: task #1284.

Related design notes:

- `docs/design/iouring-session-ring-pool.md` (#1409, completed) - the
  session-level ring pool this composition layers on top of.
- `docs/design/basis-file-io-policy.md` (#1666) - keeps mmap pointers out
  of the io_uring data path; that invariant is assumed here.
- `docs/design/intra-file-parallelism.md` - rayon-driven sharding for
  single large files.
- `docs/audits/iouring-pipe-stdio.md` (#1859) and
  `docs/audits/ssh-socketpair-vs-pipes.md` (#1938, includes #1858 close-out)
  - the SSH stdio path that this design intentionally does not cover.

## 1. Summary

oc-rsync uses two different concurrency primitives in the same hot path:

- **rayon**, a CPU-parallel work-stealing pool that runs synchronous
  closures across N worker threads (one per logical CPU by default), and
- **io_uring**, a single-thread asynchronous I/O interface where one
  ring instance owns a submission queue (SQ) and completion queue (CQ)
  shared between userspace and a kernel ring buffer.

Today the two compose by accident. A rayon worker doing parallel
signature computation or parallel match dispatch eventually needs to
read from or write to disk; if io_uring is active, the worker calls into
`fast_io::io_uring` and blocks on `submit_and_wait` (see
`crates/fast_io/src/io_uring/file_writer.rs:190` and
`:381`). That blocks the rayon worker for the duration of the kernel
transfer, defeating the asynchronous benefit, and - if every worker
brings its own ring as it does today
(`crates/fast_io/src/io_uring/file_writer.rs:54,81,141`,
`crates/fast_io/src/io_uring/file_reader.rs:60`,
`crates/fast_io/src/io_uring/socket_reader.rs:32`,
`crates/fast_io/src/io_uring/socket_writer.rs:32`) - oversubscribes
the kernel with N rings competing for the same bandwidth.

This document defines how the two primitives compose. It locks in:

- one io_uring instance per session (the session ring pool from #1409),
- non-blocking submission from rayon workers,
- a dispatcher role filled by the kernel SQPOLL thread when available
  and by a userspace reaper thread when it is not,
- an explicit ownership rule for registered buffers,
- a fallback chain when io_uring submission fails.

Implementation lands under #1284. This note is design-only; no code
changes ship in this PR.

## 2. Problem Statement

### 2.1 rayon is CPU-parallel and synchronous

The codebase reaches for rayon at five places that matter for this
discussion. Each is a synchronous, CPU-bound work loop that fans out
across workers, returns ordered results, and blocks the caller until
done.

- `crates/transfer/src/parallel_io.rs:11,107-125` -
  `map_blocking::<T, R, F>` is the generic helper. Threshold-gated:
  below `min_parallel`, sequential; above, `into_par_iter().map(f).collect()`.
- `crates/transfer/src/receiver/transfer/pipeline.rs:160-200` -
  parallel basis-file lookup during pipeline fill. The closure runs
  `find_basis_file_with_config`, which on its slow path opens and
  `stat`s candidate files. That is exactly the kind of I/O that ought
  to go through io_uring on Linux 5.6+ but currently uses synchronous
  `openat`/`fstatat`.
- `crates/transfer/src/delta_pipeline.rs:324` -
  `rayon::current_num_threads()` sizes the parallel delta pipeline.
- `crates/match/src/index/mod.rs:131-142,205-217` - parallel candidate
  verification when the rolling-hash bucket has more candidates than
  `PARALLEL_THRESHOLD`. Each worker computes a strong checksum and
  compares; the window data already lives in memory, so this is pure
  CPU. No I/O composition issue here, but it occupies a rayon worker
  while disk I/O is queued elsewhere.
- `crates/signature/src/parallel.rs:11,27-86` -
  `generate_file_signature_parallel` reads all blocks into memory then
  computes rolling and strong checksums in parallel. The read step is
  sequential today; the parallelism is on the CPU side.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:62-141` -
  `rayon::scope` based work-queue drain with per-thread sharded
  buffers. Workers are bursty; idle quanta exist between bursts, which
  matters in section 4.

The thread count from `rayon::current_num_threads()` is the world's
default global pool. There is no `rayon::ThreadPoolBuilder` override in
the codebase, so on a 16-core box rayon spins up 16 workers. That
means up to 16 simultaneous synchronous calls into the rest of the
system, including io_uring.

### 2.2 io_uring is single-thread asynchronous

io_uring's userspace contract is single-threaded per ring. The
`io_uring::IoUring` struct from the `io-uring` crate is `!Sync`; the
upstream pool design (`docs/design/iouring-session-ring-pool.md`)
already wraps each ring in a `Mutex<RawIoUring>` for that reason.

Two consequences:

- A submission operation (`ring.submission().push(&sqe)`) is mutually
  exclusive across threads. Two rayon workers cannot push to the same
  ring concurrently.
- A `submit_and_wait(N)` call (used today at
  `file_writer.rs:190` and `file_reader.rs` analogues) blocks the
  caller until N completions land. If a rayon worker calls it, that
  worker is parked while the kernel transfers bytes - and rayon's
  work-stealing scheduler does not reclaim the worker because the call
  looks synchronous to it.

### 2.3 The accidental composition

Today's call graph, when io_uring is active for file writes, is:

```
rayon worker N
    -> par_iter map closure
        -> writer.write_all(bytes)
            -> IoUringWriter::flush
                -> ring.submit_and_wait(K)   [blocks worker N]
```

If every rayon worker holds its own `IoUringWriter` (the per-object
ring construction model that #1409 set out to replace), the kernel
also has N rings, each with an SQPOLL kthread if SQPOLL is on, each
with its own SQ/CQ memory mapping, and each fighting over the same
disk queue depth. That is the oversubscription cost.

If we move to a shared ring (the #1409 pool), the synchronous
`submit_and_wait` becomes a contention point: every rayon worker
serialises through the ring's mutex.

Both extremes are unsatisfactory. Sections 3 and 4 build the policy
that resolves them.

## 3. Three Composition Models

We considered three models. The recommendation is model B with the
SQPOLL kernel thread filling the dispatcher role when available.

### Model A: Per-worker io_uring

Every rayon worker owns its own `RawIoUring`, sized at default 64 SQ
entries (`config.rs:332`).

- Pro: no contention. Each worker submits to its own SQ and reaps its
  own CQ. No mutex.
- Pro: matches today's per-object construction pattern - minimal code
  change.
- Con: N rings on a 16-core box equals 16 SQ/CQ memory regions, 16
  entry tables in the kernel, and (with SQPOLL on) 16 kthreads. Kernel
  fd quota and `RLIMIT_NOFILE` pressure scale with N.
- Con: registered buffers (`registered_buffers.rs:243-307`) are
  ring-scoped, so each ring needs its own registered buffer set.
  Registration cost is duplicated N times.
- Con: even with SQPOLL, SQPOLL kthreads compete for the same disk
  queue, so the kernel-side parallelism is bounded by hardware queue
  depth - not by ring count.

### Model B: Shared io_uring + dispatch thread

One ring per session. All rayon workers send submission requests to
the dispatcher; the dispatcher batches them, calls `submit`, and
relays completions back. The dispatcher can be either a userspace
thread reading from a `crossbeam_channel::Receiver` of
`SubmissionRequest`, or - if SQPOLL is available - the kernel thread
itself.

- Pro: one ring, simple kernel resource model. One SQ/CQ memory map,
  one fd, one registered buffer set, one (optional) SQPOLL kthread.
- Pro: the ring's mutex moves out of the hot path: workers do not
  compete for it because they post to a channel instead.
- Con: a userspace dispatcher thread is now the bottleneck. Every
  submission goes through it.
- Con: introduces an extra hop on the latency path. In the SQPOLL
  case this hop is paid by the kernel thread instead of a userspace
  one, which is exactly what we want.

### Model C: Per-NUMA-node io_uring with affinity

One ring per NUMA node, with rayon workers pinned to nodes via
`rayon::ThreadPoolBuilder::start_handler` and CPU affinity syscalls.

- Pro: NUMA-locality. SQ/CQ pages live on the same node as the worker
  that submits.
- Con: complex. Requires CPU topology probing
  (`hwloc`-equivalent) and platform-specific affinity APIs.
- Con: only matters at very high core counts. A 64-core dual-socket
  server might benefit; a 16-core laptop will not. oc-rsync's typical
  workload does not target the high end.
- Con: rayon's work-stealing runs across the entire pool by default,
  so node-pinning fights the scheduler.

We treat model C as a future evaluation, not a starting point.

## 4. Recommended Approach: Model B with SQPOLL Dispatcher

The recommendation is model B. The dispatcher role is filled by:

- the **SQPOLL kernel thread** when `IoUringConfig::sqpoll` is true and
  the process holds `CAP_SYS_NICE` (or runs as root) - the kernel
  polls the SQ continuously, so userspace never makes an
  `io_uring_enter` syscall on the submit path;
- a **userspace reaper thread** in the session when SQPOLL is off or
  fell back (`crates/fast_io/src/io_uring/config.rs:382-396`,
  `sqpoll_fell_back()` reflects this).

Justification:

1. The session ring pool from #1409 already sized rings at
   `min(num_cpus, 4)` and parked them behind a `Mutex<RawIoUring>`.
   Model B is the natural CPU-parallel client layer on top of that
   pool: rayon workers do not own rings, they lease them; the lease
   guard serialises submit-and-wait in the existing pool, and the
   dispatcher upgrade in #1284 replaces the wait with a non-blocking
   submit (section 5).
2. SQPOLL was already wired in `config.rs` (#1623, #1626 - the
   SQPOLL-defer-taskrun audit at `docs/audits/iouring-socket-sqpoll-defer-taskrun.md`).
   The cost model is: kernel thread spins on the SQ; userspace pushes
   SQEs and updates the tail pointer with no syscall; the kernel
   thread sees the new tail and submits. This is exactly the
   non-blocking submit shape model B requires.
3. The fallback (`sqpoll_fell_back`) is observable, so a userspace
   reaper is easy to gate on.

## 5. Dispatch Model

The integration shape under #1284 is:

```text
rayon worker T_i
    1. compute its share of work (signatures, basis lookups, deltas)
    2. for each I/O it would do today:
         a. lease a ring from the session pool (#1409)
         b. push SQE(s) to the SQ tail (no syscall under SQPOLL,
            one io_uring_enter without it)
         c. release the lease without waiting
    3. continue with more CPU work; the buffer used by the SQE is
       owned by the registered buffer pool (#1739) and is leased to
       the worker until completion ack

dispatcher (SQPOLL kernel thread, or userspace reaper)
    1. observe new SQ entries (kernel polls under SQPOLL; userspace
       reaper waits on an eventfd or short timeout otherwise)
    2. drive completions out of the CQ
    3. wake any worker that is waiting on a specific completion via a
       per-op eventfd or a `Condvar` keyed on `op_id`
    4. release the registered buffer back to the pool
```

The submit step on the worker side is **non-blocking**: it never calls
`submit_and_wait`. A worker that needs a result before continuing
parks on a per-op condition variable while letting rayon steal its
next task; this is the same idiom as `crossbeam_channel::Receiver::recv`
during `rayon::scope`. The reaper thread's job is to make those
condition variables fire.

For the common case where the worker does **not** need the result
immediately (e.g. flushing intermediate data while computing the next
checksum), the worker submits and proceeds. The completion is reaped
asynchronously and the buffer is returned to the pool with no worker
involvement.

## 6. Backpressure

The SQ has a fixed depth, default 64
(`crates/fast_io/src/io_uring/config.rs:332`). When the SQ is full,
`submission().push(&sqe)` returns an error. The worker must not spin.

Policy:

- **Local queue.** Each rayon worker has a small local FIFO of
  pending `SubmissionRequest` items, capacity N_local = 4. When the
  SQ is full, new requests go to the local FIFO; the worker continues
  with the next CPU task. The reaper drains the local FIFO into the
  SQ as space becomes available.
- **Block when local queue full.** If the local FIFO reaches N_local,
  the worker blocks on a `Semaphore` waiting for at least one
  completion. This bounds memory growth and prevents the pipeline
  from buffering an unbounded amount of work.
- **Registered buffers as the slowest gate.** The registered buffer
  pool from #1739 (the kernel `IORING_REGISTER_BUFFERS` set in
  `crates/fast_io/src/io_uring/registered_buffers.rs:80-110`) is
  capped at 1024 by the kernel and at `registered_buffer_count`
  (default 8) by config. A worker cannot submit a `READ_FIXED` /
  `WRITE_FIXED` SQE without holding a registered buffer slot. If the
  pool is empty the worker must either fall back to a non-`FIXED`
  opcode (which is allowed - see fallback chain in section 9) or
  block on the buffer pool's checkout semaphore.

The default `registered_buffer_count = 8` and `sq_entries = 64`
together imply that the SQ is the wider gate; backpressure usually
trips on the buffer pool first. That is intentional: registered
buffers are scarce, so we starve them last and drop to non-`FIXED`
opcodes early.

## 7. Interaction with `tokio::task::spawn_blocking` (#1751)

The async daemon listener (#1934) and the async pipeline behind the
`async` feature (`crates/transfer/src/pipeline/async_pipeline.rs:21`)
run inside a tokio runtime. Two things follow.

First, **CLI mode is sync**. The CLI binary
(`crates/cli/...`) drives transfers through `core::session()` without a
tokio runtime. It uses rayon directly for parallelism. The composition
design in this document covers the CLI path end to end without
introducing a tokio runtime for it.

Second, **daemon mode has a runtime**. When the daemon accepts a
connection, blocking calls that cannot go through io_uring (rare on
Linux 5.6+, more common on 5.4 / 5.5 fallback per the chain in #1748)
go through `tokio::task::spawn_blocking` rather than rayon. The
distinction matters: rayon's pool is sized for CPU parallelism, while
tokio's blocking pool is sized for I/O wait. Mixing the two on the
daemon side keeps rayon free to do the CPU work the io_uring workers
also need.

The existing daemon path uses `tokio::task::spawn_blocking` in only
one place (`crates/engine/src/async_io/copier.rs:184`), and two more
sites carry plans to use it in `transfer/src/receiver/directory/`
(`creation.rs:26`, `deletion.rs:26`). The composition policy says: any
new daemon-side blocking call that **cannot** go through io_uring -
`fsync`-on-rename, `chmod`/`chown` on Linux without `IORING_OP_FCHMOD`
support, batch directory unlinks - uses `spawn_blocking`. Calls that
**can** go through io_uring (read, write, openat on 5.15+,
fallocate, statx on 5.6+) go through the session ring instead.

CLI mode never imports `tokio::task::spawn_blocking`. It relies on
the rayon pool plus the session ring pool, with the local-queue
backpressure described in section 6.

## 8. Wire-Compat Invariant

Zero impact on the wire protocol. The composition design is purely
about how userspace orchestrates disk I/O against the kernel. None of
the following byte-level invariants change:

- File-list message ordering. Senders still emit file entries in the
  order required by protocol 32; receivers still consume them in
  index order.
- Checksum computation. Strong-checksum bytes are bit-identical
  whether the bytes were read via `read(2)`, `pread64(2)`, or
  `IORING_OP_READ`.
- Multiplex envelope. The `MPLEX_BASE`/`MSG_*` framing in
  `crates/protocol/src/envelope/` runs above the transport and is
  unaware of the I/O backend.
- Delta token order. The receiver still consumes COPY/DATA tokens in
  the order the sender emitted them. The disk-commit thread
  (`crates/transfer/src/disk_commit/process.rs:26`) writes to disk in
  that order; reordering happens only in the kernel I/O scheduler,
  which is unobservable on the wire.

The session ring pool (#1409) already preserves this invariant
because each lease holds the ring mutex for a full submit-and-wait
cycle. The model B upgrade keeps it because completion order is
demuxed by `op_id` (`crates/fast_io/src/io_uring/shared_ring.rs:25-42`),
so the disk-commit consumer can reorder completions back into wire
order if needed - but for sequential write-out it does not need to,
because the disk-commit thread submits in order and the kernel
guarantees that two writes to the same fd at increasing offsets
serialise correctly under `IORING_OP_WRITE`.

## 9. Memory Model

io_uring registered buffers (#1739) are pinned, owned by the ring,
and leased to workers. The ownership transfer rule:

1. **Ring owns memory.** The `RegisteredBufferGroup` allocates
   page-aligned memory once (`registered_buffers.rs:69-110`),
   registers it with the kernel via `IORING_REGISTER_BUFFERS`, and
   keeps the allocation alive for the lifetime of the ring. The
   kernel pins those pages.
2. **Worker leases a slot.** Before submitting a `READ_FIXED` /
   `WRITE_FIXED` SQE, the worker checks out a slot index from
   the group's atomic bitset. The slot grants exclusive use of one
   page-aligned buffer.
3. **Lease lasts from submit to completion ack.** While the SQE is
   in flight, the kernel may read from (`WRITE_FIXED`) or write to
   (`READ_FIXED`) the buffer. The worker MUST NOT touch the buffer
   during this window; touching it is a data race against the
   kernel.
4. **Reaper releases on completion.** When the CQE for the SQE
   arrives, the reaper (SQPOLL kthread + userspace ack, or the
   userspace reaper thread) clears the slot's bit, returning it to
   the free list. Any worker that was waiting on a slot wakes up.
5. **Drop ordering.** As documented in
   `registered_buffers.rs:18-39`, the `RawIoUring` field MUST be
   declared before the `RegisteredBufferGroup` field in any owning
   struct, so that the ring fd is closed first (releasing kernel
   pinning) and the user memory is freed second. Reversing the order
   is sound but breaks the documented contract.

The model B dispatcher does not change this rule. It only changes
*who* releases the slot: under SQPOLL, the kernel thread completes
the SQE and the userspace reaper thread releases the slot bit; under
non-SQPOLL, the userspace reaper does both.

## 10. Failure Semantics

An io_uring submit can fail in three ways:

- **`-EBUSY`** - the SQ is full and the kernel has not yet drained
  it. Recoverable: queue locally (section 6) and retry.
- **`-EAGAIN`** - the operation cannot proceed without blocking and
  the ring was set up with `IOSQE_ASYNC` off. Recoverable: re-submit
  with `IOSQE_ASYNC` set, or fall back.
- **Unrecoverable kernel errors** - `-EFAULT` on a buffer outside the
  registered set, `-EINVAL` on a bad SQE, `-ENOSPC` on the disk.
  Surface to the worker as an `io::Error`.

When a submit fails recoverably and the local queue is full, the
worker drops out of the io_uring path entirely for that operation
and falls through the chain documented in #1748:

```text
io_uring (preferred on Linux 5.6+)
    |
    | submit failure or io_uring unavailable
    v
pread64(2) / pwrite64(2)  on Linux
    |
    | EINTR retry exhausted, or non-Linux platform
    v
fread(3) / fwrite(3)      via std::fs::File
```

The fallback is per-operation, not per-session: a single SQE failure
does not disable io_uring for the rest of the transfer. The session
ring stays alive; only the failed op falls through. This keeps the
common-case fast path hot when one outlier syscall happens to fail.

The fallback chain mirrors the existing factory shape in
`crates/fast_io/src/io_uring/mod.rs:151-168` and `:208-218`, which
already degrades from `IoUringReader` / `IoUringWriter` to
`StdFileReader` / `StdFileWriter` on ring construction failure. The
addition under #1284 is degrading **per submit**, not just per ring.

## 11. Activation Threshold

io_uring is enabled by default on Linux 5.6 and later (kernel
parsing in `crates/fast_io/src/io_uring/config.rs:262-280`,
`MIN_KERNEL_VERSION`). On older kernels, the runtime probe at startup
(`is_io_uring_available`) returns `false` and every site falls
through to standard I/O. The composition design in this document
applies only when io_uring is active.

A consequence: rayon-only workloads (CLI on Linux 5.4, Linux 5.5,
macOS, Windows, FreeBSD - any non-Linux platform - and any container
runtime that blocks `io_uring_setup` via seccomp) do not exercise the
composition path at all. They use rayon for CPU parallelism and
synchronous `pread`/`pwrite` for disk I/O. The performance ceiling on
those configurations is governed by the rayon worker count and the
filesystem's queue depth, not by anything in this document.

The platform/version surface is recorded in the existing fallback
chain doc (#1273, #1748) and in
`crates/fast_io/src/lib.rs` (the cross-platform fast-path dispatch).
This composition design plugs in cleanly: the io_uring branch
becomes the model-B dispatcher; the non-io_uring branches stay as
they are.

## 12. SSH Path Note

SSH transfers are out of scope for this design. The SSH transport
runs the rsync engine through the SSH child's stdio pipes
(`crates/rsync_io/src/ssh/connection.rs:30,178,217-237`), and io_uring
on those pipe FDs is currently unreachable for SSH transfers
(documented in `crates/rsync_io/src/ssh/mod.rs:57-75` per #1858, and
the surrounding audit `docs/audits/iouring-pipe-stdio.md` #1859).

For SSH transfers all I/O on the wire is via standard
`read(2)`/`write(2)` against the pipe FDs. The io_uring composition
design applies only to **disk** I/O - reads of source files on the
sender, writes of destination files on the receiver, basis-file
reads during delta apply. Those happen on regular file FDs and are
unrelated to the SSH transport's pipe FDs.

If a future change wires the io_uring socket reader/writer
infrastructure through to SSH pipe FDs (the work tracked in #1859),
the composition policy applies unchanged: the SSH pipe-side ring
becomes another participant in the session ring pool, and the model
B dispatch policy continues to work because the non-blocking submit
contract is shape-preserving across fd kinds.

## 13. Risks

The composition opens three new risk surfaces. Each is bounded by
existing tracking work.

- **SQPOLL needs `CAP_SYS_NICE` on some configs.** Most distro
  kernels gate `IORING_SETUP_SQPOLL` behind `CAP_SYS_NICE` or root
  (audit at `docs/audits/iouring-socket-sqpoll-defer-taskrun.md`,
  tracker #1622). Falling back to non-SQPOLL means the userspace
  reaper thread takes the dispatcher role; throughput drops because
  every submit becomes an `io_uring_enter` syscall. The fallback is
  observable via `sqpoll_fell_back()` (`config.rs:45`); the
  composition design degrades gracefully but the user-visible perf
  changes.
- **Ring pool exhaustion at very high concurrency.** The default pool
  size of `min(num_cpus, 4)` from #1409 was sized for a 4-8 core
  laptop. On a 64-core server with thousands of small files in flight
  the pool can become a serialisation point. Tracked under
  #2045 (adaptive sizing). The composition policy works for any pool
  size; the choice of pool size is orthogonal.
- **Registered-buffer pinning under memory pressure.** Each registered
  buffer pins one or more pages in the kernel
  (`registered_buffers.rs:80-110`). On a memory-constrained host with
  many concurrent oc-rsync processes (e.g. a CI runner doing parallel
  builds), the pinned pages can starve other processes.
  `registered_buffer_count = 8` at 64 KB each = 512 KB per ring; with
  `min(num_cpus, 4) = 4` rings that is 2 MB per session, which is
  modest. Adaptive pool sizing (#2045) and per-session pool overrides
  give us the knobs to bound it; the composition design itself does
  not make this worse than #1409 already did.

Risks the composition design **does not** introduce:

- **Wire compatibility** - section 8 above; zero change.
- **Test coverage** - the existing io_uring fallback test
  (`crates/fast_io/tests/io_uring_probe_fallback.rs`) covers the
  ring-construction fallback. The follow-up #1284 will add a per-
  submit fallback regression test (section 14 below).
- **Cross-platform drift** - the composition path is gated on
  `cfg(all(target_os = "linux", feature = "io_uring"))`. macOS,
  Windows, FreeBSD, and Linux without the feature flag use
  `crates/fast_io/src/io_uring_stub.rs`, where the composition design
  is dormant.

## 14. Follow-up Tasks

These are the tracking items the follow-up work expects. They are
listed here only for cross-reference; they are not added to the
persistent project TODO from this design note.

1. **Implementation: io_uring + rayon composition wiring.** Tracked
   under #1284. Lands `RingPool::submit_async`, the local-queue
   backpressure, the userspace reaper thread for the non-SQPOLL
   case, and the per-submit fallback chain.
2. **Benchmark: per-worker rings vs shared ring + dispatcher.**
   Compare model A and model B head-to-head on a 16-core box with a
   100 GB transfer of 1 MB files (small-file regime) and a 100 GB
   transfer of 10 GB files (large-file regime). Use the existing
   harness in `scripts/benchmark_hyperfine.sh` and the bench image
   `localhost/oc-rsync-bench:latest`.
3. **NUMA-aware variant evaluation.** Probe model C on a dual-socket
   server. If the gain over model B is below 5% across realistic
   workloads, defer indefinitely.
4. **Fallback-chain regression test.** Add a test under
   `crates/fast_io/tests/` that forces an `-EBUSY` on submit and
   verifies the worker correctly falls through to `pread64`/`pwrite64`
   without disabling io_uring for subsequent ops in the same session.

## 15. Test Plan

This is a design document; tests land with the implementation
follow-up #1284. The test surface should include, at minimum:

- A unit test that confirms `submit_and_wait` is no longer called
  from a rayon worker context. A static check on a feature-gated
  test build catches regressions: instrument the io_uring entry
  points to assert `rayon::current_thread_index()` is `None` (i.e.
  the call is on the dispatcher thread, not a rayon worker).
- A property test for the local-queue backpressure: for any sequence
  of submission attempts, the local queue never exceeds `N_local`,
  the SQ never exceeds `sq_entries`, and the reaper drains both
  monotonically.
- A stress test that runs 10 K mixed read/write operations across 16
  rayon workers on a single shared ring with `sq_entries = 64`,
  `registered_buffer_count = 8`, and verifies no submit is dropped
  and every completion is observed by the issuing worker.
- A fallback-chain test (item 4 above): inject `-EBUSY` on submit,
  verify the worker falls through to `pread64`/`pwrite64`, verify
  subsequent ops on the same ring still use io_uring.
- An interop test running the full benchmark suite against upstream
  rsync 3.4.1 with io_uring enabled, confirming bit-identical output
  files and matching wire byte streams. The wire-compat invariant
  in section 8 is the load-bearing one; this test is the empirical
  check.

The existing fixtures cover the foundations: ring pool lease/return
under contention (`docs/design/iouring-session-ring-pool.md`),
registered-buffer drop ordering
(`crates/fast_io/src/io_uring/registered_buffers.rs:719-1226`), and
per-channel fallback to standard I/O on ring creation failure
(`crates/fast_io/src/io_uring/mod.rs:151-168`).

## 16. Alternatives Rejected

- **Async runtime everywhere.** Convert the entire transfer pipeline
  to tokio. Rejected: the CLI has no runtime today; rayon
  work-stealing outperforms tokio on the tight CPU loops in
  signature/match/checksum (existing benches in
  `crates/transfer/benches/`); the async-channel-abstraction note
  (#1591) already settled this trade-off in favour of keeping rayon
  on sync hot paths.
- **Block rayon workers on `submit_and_wait`, accept the cost.**
  Today's accidental composition. Rejected: every blocked worker is
  a CPU core stolen from work rayon could otherwise do; the whole
  point of io_uring is to overlap I/O with compute.
- **One ring per file, lazy per-call.** Small files use a private
  ring, large files use the pool. Rejected: ring construction cost
  amortises poorly across many small files; the session pool already
  handles small files well by reusing a fixed pool.

## 17. References

- `crates/fast_io/src/io_uring/config.rs:262-280` - kernel version
  parsing and the `MIN_KERNEL_VERSION = (5, 6)` gate.
- `crates/fast_io/src/io_uring/config.rs:381-396` - `build_ring`
  and the SQPOLL fallback.
- `crates/fast_io/src/io_uring/file_writer.rs:54,81,141,190,381` -
  per-object ring construction and `submit_and_wait` blocking calls.
- `crates/fast_io/src/io_uring/file_reader.rs:60` - per-reader ring.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `socket_writer.rs:32` - per-socket rings.
- `crates/fast_io/src/io_uring/registered_buffers.rs:18-39,
  69-110, 243-307` - registered buffer ownership rules.
- `crates/fast_io/src/io_uring/shared_ring.rs:25-42` - SQE
  `user_data` demux for shared rings.
- `crates/fast_io/src/io_uring/buffer_ring.rs:1-50` - PBUF_RING
  zero-copy reads (#1739, kernel 5.19+).
- `crates/fast_io/src/io_uring/mod.rs:151-168, 208-218` - factory
  fallback to std I/O on ring construction failure.
- `crates/transfer/src/parallel_io.rs:11,107-125` - rayon
  `map_blocking` helper and threshold defaults.
- `crates/transfer/src/receiver/transfer/pipeline.rs:160-200` -
  parallel basis-file lookup.
- `crates/transfer/src/delta_pipeline.rs:324` -
  `rayon::current_num_threads()` sizing the parallel delta pipeline.
- `crates/match/src/index/mod.rs:131-142,205-217` - parallel
  candidate verification.
- `crates/signature/src/parallel.rs:11,27-86` -
  `generate_file_signature_parallel`.
- `crates/engine/src/concurrent_delta/work_queue/drain.rs:62-141` -
  `rayon::scope` work-queue drain.
- `crates/rsync_io/src/ssh/mod.rs:57-75` - SSH stdio limitation
  (#1858).
- `docs/design/iouring-session-ring-pool.md` (#1409) - the session
  ring pool this composition layers on top of.
- `docs/design/basis-file-io-policy.md` (#1666) - mmap-vs-buffered
  selector that fences off mmap pointers from io_uring SQEs.
- `docs/audits/iouring-pipe-stdio.md` (#1859) and
  `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - SSH stdio
  audit.
- `docs/audits/iouring-socket-sqpoll-defer-taskrun.md` (#1622,
  #1626) - SQPOLL and `IORING_SETUP_DEFER_TASKRUN` evaluation.
