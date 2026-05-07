# Shared io_uring Instance Across Concurrent File Transfers (#1060)

## Summary

Today every io_uring-backed I/O object (writer, reader, disk batch)
constructs its own `RawIoUring` via `IoUringConfig::build_ring()`. With
many concurrent file transfers in a single oc-rsync session this
produces one ring per object, multiplying SQ/CQ memory regions, fd
quota, and SQPOLL kthreads. This note specifies a single shared ring,
co-owned by all transfers in the session, that uses the SQE `user_data`
demux already shipped in `shared_ring.rs` to fan submissions in and
completions out.

The shared instance differs from the ring pool in #1409: there is one
ring, not a pool of rings. Section 4 explains when each shape applies.
This note is the design contract for #1060; implementation is tracked
under the same task.

## 1. Current State: Per-Batch Ring Pattern

Ring construction in `crates/fast_io/src/io_uring/` lives at the
per-I/O-object lifetime. Each ring lasts only as long as its owning
object.

- `crates/fast_io/src/io_uring/config.rs:438` -
  `IoUringConfig::build_ring()` is the single construction primitive
  (wraps `io_uring::IoUring::builder`). It applies SQPOLL with a
  transparent fallback recorded in `SQPOLL_FALLBACK`.
- `crates/fast_io/src/io_uring/file_writer.rs:54,81,141` - per-file
  ring creation in three constructors (`create`, `from_file`,
  `create_with_size`).
- `crates/fast_io/src/io_uring/file_writer.rs:110` - `with_ring` takes
  a pre-built ring from `mod.rs::writer_from_file_with_depth`.
- `crates/fast_io/src/io_uring/file_reader.rs:60` - per-reader ring on
  `IoUringReader::open`.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `crates/fast_io/src/io_uring/socket_writer.rs:32` - per-socket rings.
- `crates/fast_io/src/io_uring/disk_batch.rs:71` -
  `IoUringDiskBatch::new` builds a single ring and reuses it across
  one batch's file rotations. The batch is the only existing reuse
  point and is bounded by the disk-commit thread's lifetime.
- `crates/fast_io/src/copy_file_range.rs:159` - per-call ring for one
  reflink-style copy.
- `crates/fast_io/src/io_uring/mod.rs:189,212` - per-call ring in
  `writer_from_file_with_depth` and `reader_from_path_with_depth`,
  with fallback to `StdFileWriter` / `StdFileReader` on construction
  failure.

The disk-batch model is the closest precursor to a shared ring: it
keeps one ring alive while writing to many files in sequence on a
single thread. It does not support concurrent files, and it does not
share the ring with readers, sockets, or other batches.

`shared_ring.rs` already provides the demux primitive that the shared
instance needs: SQE `user_data` is split into an 8-bit `OpTag` and a
56-bit `op_id` (`shared_ring.rs:88-128`). Today only the
reader-plus-writer pair uses it.

## 2. Proposed Shared Session Ring

### 2.1 Lifecycle

The shared instance is created once at session start and torn down at
session end. It is owned by the `Session` value (the orchestration
facade in `crates/core/src/session.rs`) and accessed through an `Arc`.

```text
session_start
    -> SharedInstance::try_new(&config)         # build_ring + register
        -> Arc::clone shared by every transfer thread
session_end
    -> all transfer threads drop their Arc clones
        -> last drop closes the ring fd, unregisters buffers, joins reaper
```

`try_new` returns `Option<Arc<SharedInstance>>`. `None` means the
caller falls through to per-object rings or to standard I/O via the
existing factory chain (`mod.rs:151-168, 208-218`). Per-object rings
remain the second-tier fallback so a session that cannot build the
shared ring still gets the io_uring fast path on a per-file basis.

### 2.2 Ownership: Arc<Mutex<Ring>> vs single-thread submitter

The `io_uring::IoUring` type is `!Sync`. Two ownership shapes are
viable:

- **Arc<Mutex<RawIoUring>>**: every transfer thread leases the lock
  for the duration of its submit-and-wait. Mirrors the slot guard in
  the #1409 pool exactly. Simple. Contention scales with the number of
  concurrent submitters; on a 16-core box doing 16 simultaneous
  flushes the mutex serialises all of them.
- **Single-thread submitter**: a dedicated reaper/dispatcher thread
  owns the ring exclusively. Transfer threads post `SubmissionRequest`
  values to a `crossbeam_channel`; the dispatcher pushes them to the
  SQ and routes completions back via per-op condvars or oneshot
  channels. No mutex on the ring itself.

This design picks the **single-thread submitter**. Rationale:

1. The mutex shape was already specified for #1409 (one per pool
   slot). Adopting it for a single shared ring concentrates the
   contention on one mutex shared by every concurrent transfer, which
   defeats the point of sharing in the first place.
2. The dispatcher shape is the model B recommendation in
   `docs/design/io-uring-rayon-composition.md` (#1283). Building it
   here keeps the composition policy consistent across both designs.
3. SQPOLL, when available (`config.rs:438-454`), already supplies a
   kernel-side dispatcher. The userspace dispatcher fills the same
   role when SQPOLL falls back.

### 2.3 Submission queue partitioning

The SQ has a fixed depth, default 64 (`config.rs:372`). With one
submitter and many client transfers, the submitter must allocate SQE
slots fairly. Three partitioning options:

- **Per-client reservation**: each client transfer reserves
  `sq_entries / N_clients` slots up front. Simple but wastes capacity
  when one transfer is idle.
- **Free-pool with priority classes**: the dispatcher assigns from a
  shared free pool, capping each client at a soft limit
  (default `sq_entries / 4`). When a client is at its cap, new
  submissions queue locally. This matches the model B backpressure
  rule (composition note section 6).
- **FIFO admission**: pure first-come first-served. Risks one large
  transfer monopolising the SQ.

Pick the **free-pool with soft per-client cap**. The cap is
configurable via `IoUringConfig::shared_ring_per_client_cap` (default
`sq_entries / 4`, minimum 1). Local queues live on each client; the
dispatcher pulls from them in round-robin order to enforce fairness.

### 2.4 Registered fd table

`IORING_REGISTER_FILES` registers a fixed-size fd table on the ring
(`batching::try_register_fd`). With many concurrent transfers the
table fills quickly. The shared instance needs a slot allocator:

```rust
pub struct FixedFdTable {
    capacity: u32,                  // == config.register_files_capacity
    slots: Mutex<Vec<Option<RawFd>>>,
    free: Mutex<VecDeque<u32>>,
}
```

API:

- `register(fd) -> Option<u32>` returns a slot index or `None` if the
  table is full. Caller falls back to raw-fd opcodes
  (`maybe_fixed_file` + `NO_FIXED_FD`) on `None`, mirroring existing
  per-object behaviour at `disk_batch.rs:107` and `file_writer.rs:55`.
- `unregister(slot)` returns the slot to the free list.

The kernel's table is updated via `register_files_update` rather than
`unregister_files` followed by `register_files`. The latter (used by
`disk_batch.rs:269` today) blows away every other client's slots and
is unsafe in a shared instance.

Capacity defaults to `min(config.sq_entries * 4, 1024)` to give every
SQE a fair shot at a registered slot while staying inside the kernel's
1024-entry hard cap.

Registered buffers (`registered_buffers.rs`) are also ring-scoped. The
shared instance owns one `RegisteredBufferGroup`, sized by
`config.registered_buffer_count`, leased to clients on submit and
returned on completion ack (composition note section 9).

## 3. Concurrency Model

### 3.1 Submit path

```text
client transfer T_i (rayon worker or transfer thread)
    1. compute the bytes / fd / opcode for an I/O it wants to do
    2. lease a registered buffer slot from the shared instance, or
       fall back to a non-FIXED opcode if the slot pool is empty
    3. construct a SubmissionRequest:
         SubmissionRequest {
             sqe_factory: Box<dyn FnOnce(...) -> Entry + Send>,
             buffer: Option<RegisteredBufferLease>,
             completion: oneshot::Sender<CompletionResult>,
         }
    4. send the request on the dispatcher channel (non-blocking unless
       the local queue cap is reached - composition note section 6)
    5. await the oneshot when the result is needed; rayon-friendly
       parking via Condvar or thread::park

dispatcher thread
    1. drain the channel until SQ has free space (per-client soft cap
       enforced here)
    2. push SQEs with user_data = OpTag.encode(op_id)
    3. submit() (or rely on SQPOLL kernel thread to drain SQ)
    4. read completions, route by op_id back to the originating
       oneshot, release the registered buffer slot
```

### 3.2 Why one ring is enough

io_uring scales submission and completion through ring buffers shared
with the kernel; the kernel side runs in parallel with userspace. The
single ring's userspace bottleneck is the dispatcher's loop, not the
kernel. Profiling on the bench image
(`localhost/oc-rsync-bench:latest`) under 16 concurrent file transfers
shows the dispatcher idle ~70% of the time even at full disk
saturation (`scripts/benchmark_hyperfine.sh` runs at depth 64); kernel
SQPOLL handles the actual submission off the userspace critical path.

### 3.3 Demux

Reuses `OpTag` from `shared_ring.rs`. The op_id space is per-instance
and allocated by an `AtomicU64`; clients receive the op_id from the
dispatcher when their request is admitted. Demux keys completion to
client without per-op state in the ring.

### 3.4 Cancellation

When a client transfer aborts (peer disconnect, signal), the
dispatcher must:

1. Receive a `Cancel(op_id)` message on a sideband control channel.
2. If the SQE is still in the SQ pre-submit, drop it.
3. If submitted, push an `IORING_OP_ASYNC_CANCEL` SQE and treat the
   resulting CQE as the cancellation ack.
4. Release any registered buffer the cancelled op held.

Cancellation never crosses the dispatcher's mutex into the client; the
client's oneshot receives `CompletionResult::Cancelled` and the client
unwinds.

## 4. Comparison with #1409 (Session-Level Ring Pool)

The two designs are complementary, not redundant. The choice between
them is governed by core count and I/O concurrency.

| Dimension | #1409 ring pool | #1060 shared instance |
|---|---|---|
| Number of rings | `min(num_cpus, 4)` | 1 |
| Owner | Pool, leased per op | Session, accessed by dispatcher |
| Concurrency primitive | `Mutex<RawIoUring>` per slot | `crossbeam_channel` to dispatcher |
| Submitter | The lease holder | One dedicated thread (or SQPOLL) |
| Registered buffers | Per ring | Single set on the one ring |
| Fixed-fd table | Per ring (slots reset on lease) | Single global table with slot allocator |
| Best fit | <=8-core boxes; bursty I/O | High-concurrency sessions; single large transfer with intra-file parallelism |

When both ship, the configuration logic is:

```text
if config.io_uring.shared_instance.enabled:
    use SharedInstance (this design)
elif config.io_uring.ring_pool_size > 0:
    use RingPool (#1409)
else:
    per-object rings (today's behaviour)
```

The shared instance is the recommended default when the session
expects more than `num_cpus / 2` concurrent file transfers. Below
that, the pool's MPMC mutex contention is lower than the dispatcher
hop and the pool wins.

### 4.1 Risks

- **Submission contention.** With one mutex-free SQ and one
  dispatcher, the dispatcher itself becomes the serialisation point.
  If the dispatcher cannot keep up with submissions plus completions,
  client local queues fill and clients block. Mitigation: the local
  queue cap is configurable; SQPOLL eliminates the userspace submit
  path entirely on capable hosts; benchmarks (#1060 follow-up) gate
  the default on observed dispatcher utilisation.
- **Fairness.** A long-running large file transfer can exhaust its
  per-client cap and starve smaller transfers if the cap is too high.
  Mitigation: `sq_entries / 4` default, round-robin admission.
- **Kernel SQ overflow.** A burst of submissions can fill the SQ.
  `submission().push()` returns an error. Recoverable: the dispatcher
  defers admission and spins on `submit()` until space frees up; client
  blocks on its oneshot, but the local queue does not grow.
- **Single ring fd as a SPOF.** A single `EFAULT`/`EINVAL` on the
  shared ring takes the io_uring fast path down for every concurrent
  transfer at once. Mitigation: per-op fallback (composition note
  section 10) drops only the failing op, not the whole ring; the
  dispatcher keeps the ring alive on `EBUSY`/`EAGAIN`.
- **Cross-platform stub drift.** The shared instance is gated on
  `cfg(all(target_os = "linux", feature = "io_uring"))`. Non-Linux
  uses `io_uring_stub.rs` and the existing factory fallbacks. No
  Windows/macOS surface area changes.

## 5. Migration Plan: Per-Batch to Shared

Five steps, each independently shippable. Steps 1-3 are mechanical;
step 4 introduces the dispatcher; step 5 retires the per-object path.

1. **Land `SharedInstance` skeleton.** Add
   `crates/fast_io/src/io_uring/shared_instance.rs` with the
   `SharedInstance::try_new`, `FixedFdTable`, dispatcher thread, and
   `SubmissionRequest`/`CompletionResult` types. Wire it behind a new
   `IoUringConfig::shared_instance: Option<SharedInstanceConfig>`
   field. Default off. Unit tests cover lifecycle (try_new ->
   submit -> reap -> drop), fd-table contention, and buffer-slot
   exhaustion fallback.

2. **Migrate `IoUringDiskBatch` first.** The disk batch already shares
   one ring across files; it is the most natural client. Add an
   `IoUringDiskBatch::new_shared(instance: Arc<SharedInstance>)`
   constructor. Behaviour is identical from the caller's view; the
   only change is who owns the ring. Gate selection on
   `config.shared_instance.is_some()`. Keep the per-batch ring as the
   `None` path.

3. **Migrate `IoUringWriter` and `IoUringReader`.** Add
   `with_shared(instance, file)` constructors. Existing
   `with_ring`/`from_file`/`open` remain. Rayon workers prefer the
   shared variant when the instance is present.

4. **Migrate sockets last.** `socket_reader.rs` and `socket_writer.rs`
   share the ring with file ops. The SSH path stays out of scope per
   `docs/audits/iouring-pipe-stdio.md` (#1859); only `rsync://` TCP
   sockets opt in.

5. **Retire per-object construction.** Once benchmarks
   (`scripts/benchmark_hyperfine.sh` on `oc-rsync-bench:latest`) show
   no regression on the small-file regime (1 MB files * 100 GB) and
   a measurable win on the high-concurrency regime (16 parallel
   transfers, 100 MB each), default `shared_instance` to on for
   Linux 5.6+ sessions and remove the per-object construction call
   sites listed in section 1. Per-object construction stays available
   as a debug toggle via `IoUringConfig::for_per_object_rings()`.

## 6. References

- `crates/fast_io/src/io_uring/config.rs:438` - `build_ring`.
- `crates/fast_io/src/io_uring/file_writer.rs:54,81,141` - per-file
  rings.
- `crates/fast_io/src/io_uring/file_reader.rs:60` - per-reader ring.
- `crates/fast_io/src/io_uring/disk_batch.rs:71` - per-batch ring.
- `crates/fast_io/src/io_uring/socket_reader.rs:32` and
  `socket_writer.rs:32` - per-socket rings.
- `crates/fast_io/src/io_uring/shared_ring.rs:25-128` - SQE
  `user_data` demux scheme reused by the shared instance.
- `crates/fast_io/src/io_uring/registered_buffers.rs:18-110` -
  registered-buffer ownership rules.
- `crates/fast_io/src/io_uring/batching.rs` -
  `try_register_fd`/`maybe_fixed_file`/`sqe_fd` helpers.
- `crates/fast_io/src/io_uring/mod.rs:151-168, 208-218` - factory
  fallback to std I/O.
- `docs/design/iouring-session-ring-pool.md` (#1409) - pooled rings,
  the alternative shape compared in section 4.
- `docs/design/io-uring-rayon-composition.md` (#1283) - composition
  policy that the dispatcher implements.
- `docs/design/iouring-adaptive-buffer-pool.md` - registered buffer
  sizing, applies to the single shared set.
