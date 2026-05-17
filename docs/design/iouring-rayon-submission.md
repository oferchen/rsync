# io_uring submission from rayon worker threads (#1284)

Tracking issue: oc-rsync task #1284. This is a documentation-only design
note; no code lands in this PR.

Sibling design notes and audits this document layers on top of:

- `docs/design/io-uring-rayon-composition.md` (#1283) - the broader
  composition design that frames per-worker vs shared-ring trade-offs.
- `docs/design/iouring-session-ring-pool.md` and
  `docs/design/iouring-session-ring-pool-impl.md` (#1937) - the
  session ring pool the submission policy plugs into.
- `docs/design/io-uring-submission-modes-bench-plan.md` (#1626) - the
  submission-mode benchmark plan that scopes regular vs SQPOLL vs
  fallback `read(2)`/`write(2)`.
- `docs/design/io-uring-submission-modes-bench.md` and the audit
  `docs/audits/per-file-vs-shared-uring-ring.md` (#1410) - per-file vs
  shared-ring evidence; tracks the data #4197 is meant to produce.
- `docs/audits/shared-iouring-session-instance.md` (#1408) - the
  per-thread-rings alternative still under discussion in #2243.
- `docs/design/io-uring-adaptive-buffer-pool.md` (#2045) - the adaptive
  registered-buffer pool the per-worker path would have to multiply
  per ring.

## 1. Scope

Task #1284 asks whether io_uring SQEs should be pushed by rayon worker
threads themselves rather than by the dedicated I/O threads in use
today. The answer is meaningful only after two upstream questions
converge:

- **#2243** - per-thread io_uring rings vs one shared ring (audit
  `docs/audits/shared-iouring-session-instance.md`).
- **#4197** - per-file ring vs shared session ring bench (plan tracked
  by `docs/audits/per-file-vs-shared-uring-ring.md`, executing the
  grid laid out in `docs/design/io-uring-submission-modes-bench-plan.md`).

Both are open. This note records the current submission topology, the
hazards a per-rayon-worker design would introduce, the cheaper
single-submitter alternative, the bench evidence still missing, and a
recommendation. No wire bytes change.

## 2. Current submission topology

Every io_uring submission today happens on a thread that is **not** a
rayon worker. There is no path in the tree where a rayon closure pushes
an SQE to a ring.

### 2.1 Disk-commit thread is the sole submitter for receiver writes

The receiver's destination-file writes flow through one dedicated
thread:

- `crates/transfer/src/disk_commit/thread.rs:47-56` -
  `spawn_disk_thread()` calls `thread::Builder::new().name("disk-commit"
  .into()).spawn(...)`. Exactly one thread per session.
- `crates/transfer/src/disk_commit/thread.rs:172-189` -
  `disk_thread_main()` constructs at most one
  `fast_io::IoUringDiskBatch` via `try_create_disk_batch()`
  (`thread.rs:74-92`), keeps it across the entire run loop, and never
  shares it.
- `crates/fast_io/src/io_uring/disk_batch.rs:38-54` -
  `IoUringDiskBatch` is documented as "not `Send` or `Sync` - designed
  for single-threaded use on the dedicated disk commit thread".
- `crates/fast_io/src/io_uring/disk_batch.rs:204-263` -
  `flush_current()` and `submit_fsync()` both call into
  `submit_write_batch()` and `self.ring.submit_and_wait(1)?`
  respectively. Those are the only producer call sites for this ring.

The network-side fan-in is an SPSC channel
(`crates/transfer/src/pipeline/spsc.rs`); rayon workers are not in this
producer chain.

### 2.2 Per-file rings are constructed on the calling thread (no rayon)

Where io_uring is not yet wired into the disk-commit batch (the
generator's basis-file reads, the receiver's whole-file writes outside
the batched path), each call constructs a fresh ring on the caller's
thread:

- `crates/fast_io/src/io_uring/mod.rs:205-261` -
  `writer_from_file_with_depth` builds one `RawIoUring` per output
  file via `IoUringConfig::default().build_ring()`.
- `crates/fast_io/src/io_uring/mod.rs:285-310` -
  `reader_from_path_with_depth` mirrors that for reads. The
  generator gates this entry point behind `IO_URING_READ_THRESHOLD
  = 1 MB` to avoid amortising ring setup across tiny inputs (see
  `docs/audits/per-file-vs-shared-uring-ring.md` section 1.1).
- `crates/fast_io/src/io_uring/file_writer.rs:31-47,57-101` -
  `IoUringWriter` owns its ring exclusively; `flush` drives
  `submit_write_batch` (`file_writer.rs:227` and `:414`) on the
  thread that owns the writer.
- `crates/fast_io/src/io_uring/file_reader.rs:138,260` - the per-file
  reader is the same shape on the read side.

None of these callers are rayon workers either. The transfer crate's
two `par_iter` sites
(`crates/transfer/src/parallel_io.rs:186` and
`crates/transfer/src/receiver/transfer/pipeline.rs:184-200`) compute
basis-file lookups and metadata, not disk I/O against io_uring rings.

### 2.3 Shared ring is single-owner

The shared ring topology in `crates/fast_io/src/io_uring/shared_ring.rs:98-189`
is per-session, single-owner. The module docs at
`shared_ring.rs:1-65` describe it as the io_uring analogue of
upstream's `io.c:io_loop` single-thread event loop. Nothing in the
shared-ring API is `Sync`; the type is constructed by one thread and
that thread keeps it.

### 2.4 Summary

Today every io_uring submission runs on:

- the disk-commit thread (writes), or
- the thread that opened the per-file reader/writer (reads and
  non-batched writes).

Zero submissions come from a rayon worker. Task #1284 is asking whether
to change that.

## 3. Per-rayon-worker hazards

The per-worker model (model A in
`docs/design/io-uring-rayon-composition.md` section 3) puts one ring
inside each rayon worker. The hazards below are why this note does not
recommend that shape unilaterally.

### 3.1 Worker lifetime does not match ring lifetime

Rayon workers are constructed once and reused for the lifetime of the
process. A `RawIoUring` registered against fd N (the destination file
for one transfer) cannot follow the worker into the next task that
touches fd M (the destination file for an unrelated file). The kernel's
fixed-file table is per-ring, so the registration has to be torn down
and rebuilt every time the worker takes a new piece of work. That
defeats the per-ring fixed-file optimisation that
`crates/fast_io/src/io_uring/disk_batch.rs:102-114` and
`crates/fast_io/src/io_uring/shared_ring.rs:381-387` rely on.

Two options exist, neither clean:

- **Re-register on every steal.** Each time rayon steals a task, the
  worker unregisters whatever it had (`io_uring_register(...,
  IORING_UNREGISTER_FILES, ...)`) and registers the new fd. The
  syscall cost is real: `IORING_REGISTER_FILES` involves a full RCU
  grace period inside the kernel. The whole point of fixed-file
  registration is amortising that cost across many ops.
- **Drop fixed-file registration entirely on the per-worker path.**
  Every SQE then carries a raw fd, which forces an extra
  `fget`/`fput` pair in the kernel per op
  (`crates/fast_io/src/io_uring/batching.rs:12-30` documents this
  trade-off via `sqe_fd` and `maybe_fixed_file`). For the small-file flood
  workload (`docs/design/io-uring-submission-modes-bench-plan.md`
  section 2), this is the dominant cost the registration was meant
  to eliminate.

Neither option is acceptable as a default. The correct shape, if
per-worker is chosen, is a **per-pool ring registry** keyed off
`rayon::current_thread_index()`, with one ring per worker lazily
constructed at first use and kept alive until pool teardown. That
matches what #2243 is debating.

### 3.2 Registered buffer pool multiplies per ring

The registered buffer set in
`crates/fast_io/src/io_uring/registered_buffers.rs:80-110,243-307` is
ring-scoped. The default `registered_buffer_count = 8` at
`IoUringConfig` (`crates/fast_io/src/io_uring_common.rs:82,107` and the
related `buffer_size` field) is 8 pages per ring. Per-worker rings
multiply that by `rayon::current_num_threads()` (16 on a typical
laptop, 64+ on a server). The kernel pins all of those.

The adaptive sizing planned under #2045 was scoped against the
session ring pool from #1937, not against an N-ring per-worker pool.
Multiplying it across workers reopens the memory-pressure analysis
that #2045 closed.

### 3.3 SQPOLL kthreads scale with ring count

If `IoUringConfig::sqpoll = true`
(`crates/fast_io/src/io_uring_common.rs:91`) and `CAP_SYS_NICE` is
held, each ring spawns a kernel thread that spins on its SQ tail.
Per-worker rings means N kthreads competing for the same disk queue.
The SQPOLL fallback path
(`crates/fast_io/src/io_uring/config.rs:313-340`) catches `EPERM` /
`ENOMEM` and disables SQPOLL when it cannot be set up, but it does
not catch "too many SQPOLL kthreads consuming a core each". The audit
`docs/audits/iouring-socket-sqpoll-defer-taskrun.md` (#1622, #1626)
already flagged this as the reason SQPOLL is not a default for the
daemon socket path.

### 3.4 Work-stealing breaks the FD-to-ring affinity

Rayon's work-stealing scheduler moves tasks across workers freely.
A task that ran on worker W1 and registered fd F can finish on worker
W2 when W2 steals the continuation. If the ring is bound to W1's
worker, the completion side runs on a different thread from the one
that needs the result. That forces a cross-thread completion handoff,
which is the same handoff a single-submitter design pays anyway -
without the per-worker proliferation cost.

### 3.5 Submission-queue size is wasted

Each ring's SQ default is 64 entries
(`crates/fast_io/src/io_uring_common.rs:82,107`). On a 16-core machine
with 16 worker rings, the process holds 16 x 64 = 1024 SQE slots, but
realistic disk-bound workloads keep only ~32 ops in flight at a time
(consistent with the inflight gates discussed in
`docs/design/io-uring-rayon-composition.md` section 6). Most slots sit
idle while consuming kernel memory.

## 4. Alternative: single-submitter queue

Rayon workers push descriptor tuples into a queue; a single submitter
thread drains it and pushes SQEs to one ring. This is structurally
identical to the disk-commit thread today, with one extra hop on the
producer side.

### 4.1 Shape

```text
rayon worker T_i
    1. compute its share of work (signatures, basis lookups, deltas)
    2. enqueue an `IoRequest { fd, op, buf, op_id }` to an MPSC channel
    3. continue with more CPU work
    4. (optional) await on a per-op condvar when the result is needed

submitter thread (the existing disk-commit thread, or a new "io-uring
dispatch" thread for the read side)
    1. dequeue an `IoRequest`
    2. push the SQE to the ring's SQ tail
    3. call `submit_and_wait(N)` (or rely on SQPOLL)
    4. reap CQEs, wake any waiting condvar
```

### 4.2 Why it works without #2243 / #4197 evidence

The single-submitter model is shape-preserving against the current
disk-commit thread (`crates/transfer/src/disk_commit/thread.rs:53-56`).
The only delta is an extra MPSC hop in front of the existing
ring-owning thread. None of the per-worker hazards apply: one ring,
one fixed-file table, one registered buffer set, one SQPOLL kthread
when enabled.

The cost is an additional context switch per submission. For workloads
where the SQE is on the order of a microsecond (small-file flood) the
hop is measurable. For workloads where the SQE drives kilobytes to
megabytes through the kernel (large-file sequential), the hop is in
the noise.

### 4.3 Why it is not free

The MPSC queue is a contention point under N producers. A
`crossbeam_channel::bounded` (the existing pattern in
`crates/transfer/src/pipeline/spsc.rs` upgraded to MPSC) handles
this, but every push is a CAS on a shared tail. Per-worker rings
avoid that CAS by making the SQ itself the queue.

The trade-off is empirically measurable - the bench in
`docs/design/io-uring-submission-modes-bench-plan.md` section 2
already covers it under the "parallel" configuration - but the
numbers are not in yet.

## 5. Bench evidence

The three benchmarks that bear on this decision:

- **#4197 - per-file vs shared session ring** (plan in
  `docs/audits/per-file-vs-shared-uring-ring.md`). Quantifies the
  ring-construction cost on the small-file flood workload. Without
  this number, the per-worker option's cost model is speculative.
- **#4201 - SQPOLL on/off across the privilege matrix** (plan in
  `docs/design/io-uring-submission-modes-bench-plan.md` section 3).
  SQPOLL changes the submit-side cost from a syscall to a memory
  store. Per-worker only beats single-submitter if SQPOLL is off; if
  SQPOLL is on, the single ring's submission cost is already near
  zero and the per-worker model adds nothing.
- **#4214 - drain_parallel contention measurement.** The drain bench
  at `crates/engine/benches/drain_parallel_benchmark.rs` and the
  stat-collector contention bench at
  `crates/transfer/benches/parallel_stat_collector_contention.rs`
  measure rayon-side throughput under load. They do not measure the
  io_uring submit hop, but they bound how often a worker re-enters
  the I/O path. Without that bound, the per-worker hop frequency is
  unknown.

### 5.1 What additional bench is needed

The current grid in #1626 covers 18 cells (3 workloads x 2
concurrency modes x 3 submission strategies). To decide #1284 it has
to add one axis:

- **Submitter topology.** Cells for (i) per-worker rings, (ii)
  shared ring + single submitter, (iii) shared ring + worker
  push (today's behaviour for the per-file path). That is 18 x 3 =
  54 cells.

Until those 54 cells are measured on at least the bench image
`localhost/oc-rsync-bench:latest` (Arch, kernel matching the bench
host) and the rsync-profile container (Debian, rust:latest), the
recommendation in section 6 stays "defer".

## 6. Recommendation

**Defer until #2243 (per-thread rings) and #4197 (per-file vs shared
ring bench) converge.**

Justifications:

1. **The composition design's preferred shape (#1283) is single-ring,
   single-dispatcher.** See `docs/design/io-uring-rayon-composition.md`
   sections 3 and 4. Per-worker rings (model A) is rejected there as
   a starting point. #1284 would be implementing model A inside
   model B's container, which is incoherent until the per-thread-ring
   debate in #2243 resolves the direction.
2. **The bench data is not in.** Both #4197 (per-file vs shared) and
   the SQPOLL-on/off matrix from #1626 / #4201 are still running.
   Implementing a per-worker submission path now risks landing the
   slower option and having to revert it.
3. **The disk-commit thread already eliminates the worst case.** The
   receiver write path (the highest-throughput hot path) is already a
   single-submitter design via
   `crates/transfer/src/disk_commit/thread.rs:53-56`. The most
   plausible win from #1284 is on the read side (generator basis-file
   reads), which is currently per-file rings on the calling thread.
   That win is bounded by `IO_URING_READ_THRESHOLD = 1 MB`
   (`crates/fast_io/src/io_uring/mod.rs:250-288`), so the addressable
   benefit is small-file-density-dependent. Without #4197, the win
   size is unknown.
4. **The borrowed-slice and async paths (#4217, #4218) just merged.**
   They change the buffer ownership shape under both the per-worker
   and single-submitter alternatives. Re-running the bench grid
   after they land is necessary regardless of the submission
   topology; doing it once after #1284 is cheaper than doing it
   twice.

### 6.1 What "defer" means concretely

- Do not add a per-rayon-worker submission path in this milestone.
- Keep the disk-commit thread as the sole submitter for writes
  (`crates/transfer/src/disk_commit/thread.rs:53-56`).
- Keep the per-file ring topology for the generator's read side
  (`crates/fast_io/src/io_uring/mod.rs:285-310`) until #4197
  measures the alternative.
- When #2243 lands a per-thread ring registry (or rejects it), update
  this note and pick one of:
  - per-worker submission against the per-thread rings #2243 lands;
  - single-submitter MPSC drain in front of the session ring pool
    from #1937;
  - status quo (no rayon-side submission), if neither alternative
    beats the disk-commit thread by a measurable margin.

### 6.2 What "defer" does **not** mean

- It does not block #1283's composition design. The composition
  design is independent of where the SQE push happens; both
  alternatives in section 6.1 are compatible with model B.
- It does not block #2045's adaptive buffer pool. The buffer pool
  count is ring-scoped; whichever topology #1284 picks, #2045
  applies per ring.

## 7. Cross-references

- **#1284** (this note) - implementation question.
- **#1283** - `docs/design/io-uring-rayon-composition.md` -
  composition policy that #1284 plugs into.
- **#2243** - per-thread rings vs single shared ring; audit at
  `docs/audits/shared-iouring-session-instance.md`. Blocks the
  per-worker option in section 3.
- **#2045** - `docs/design/io-uring-adaptive-buffer-pool.md` -
  adaptive registered buffer pool; sizing applies per ring under
  either topology.
- **#1937** - `docs/design/iouring-session-ring-pool-impl.md` -
  session ring pool; the single-submitter option in section 4
  drains into it.
- **#4197** - per-file vs shared session ring bench; plan at
  `docs/audits/per-file-vs-shared-uring-ring.md`. Section 5
  blocks on this data.
- **#4201** - SQPOLL on/off bench across privilege matrix; plan at
  `docs/design/io-uring-submission-modes-bench-plan.md` section 3.
  Section 5 blocks on this data.
- **#4214** - drain_parallel contention measurement; benches at
  `crates/engine/benches/drain_parallel_benchmark.rs` and
  `crates/transfer/benches/parallel_stat_collector_contention.rs`.
- **#4217** - async io_uring (just merged); see
  `docs/audits/async-io-uring-interaction.md` and
  `docs/design/async-io-uring-impact.md`. Changes the buffer
  ownership shape that section 6 calls out.
- **#4218** - borrowed-slice (just merged); changes the SQE buffer
  lifetime contract documented at
  `crates/fast_io/src/io_uring/shared_ring.rs:228-302`.

## 8. Wire compatibility

Zero impact. This note recommends deferring an implementation
question about where SQEs are pushed; no bytes on the wire change
under any of the alternatives.
