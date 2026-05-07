# SPSC disk-commit channel utilization and backpressure

Last verified: 2026-05-07 against
`crates/transfer/src/disk_commit/{mod,config,thread,process,writer}.rs` and
`crates/transfer/src/pipeline/{spsc,messages}.rs`.

Tracking issue: #1081.

## Scope

The decoupled-receiver pipeline splits network ingest from disk commit on two
threads connected by three lock-free SPSC channels. This audit catalogues the
queue, models the producer/consumer rates, explains the capacity choice and
backpressure semantics, and proposes 3-5 measurements + improvements covering
queue-depth telemetry, dynamic capacity, write-coalescing, and fsync deferral.

## 1. SPSC channel implementation

The channel is a single-producer/single-consumer wrapper around
`crossbeam_queue::ArrayQueue` with `AtomicBool` liveness flags and pure
spin-wait synchronization (no futex, condvar, or `thread::park`).

- `Shared<T>` holds the bounded `ArrayQueue<T>` plus
  `producer_alive`/`consumer_alive` flags
  (`crates/transfer/src/pipeline/spsc.rs:17-21`).
- `Sender::send` spin-loops on `queue.push` until a slot opens, returning
  `SendError(item)` if the consumer flag drops
  (`spsc.rs:74-87`). The hot path is `consumer_alive.load(Relaxed)` ->
  `queue.push` -> `std::hint::spin_loop`.
- `Receiver::recv` spin-loops on `queue.pop`, draining once after observing
  `producer_alive == false` and returning `RecvError`
  (`spsc.rs:110-121`). `try_recv` is the non-blocking analogue
  (`spsc.rs:127-136`).
- Drops mark the corresponding flag `false` with `Release` ordering to wake
  the peer's spin loop (`spsc.rs:96-100, 139-143`).
- `channel(capacity)` builds the pair and is the only constructor
  (`spsc.rs:150-157`).

The disk-commit module wires three of these channels:

- `file_tx` (network -> disk) carries `FileMessage` items at
  `DEFAULT_CHANNEL_CAPACITY = 128`
  (`crates/transfer/src/disk_commit/config.rs:32, 99-115`,
  `disk_commit/thread.rs:49`).
- `result_rx` (disk -> network) carries `io::Result<CommitResult>` per
  committed file at `capacity * 2`
  (`disk_commit/thread.rs:50`).
- `buf_return_rx` (disk -> network) recycles `Vec<u8>` chunk buffers at
  `capacity * 2` (`disk_commit/thread.rs:51`). Capacity bounds are clamped
  to `[MIN_CHANNEL_CAPACITY=8, MAX_CHANNEL_CAPACITY=4096]`
  (`config.rs:35-38, 117-127`).

`FileMessage` is the wire format between the two threads: `Begin -> Chunk*
-> Commit | Abort`, with a `WholeFile { begin, data }` coalescer for single
chunk files and a `Shutdown` terminator
(`crates/transfer/src/pipeline/messages.rs:21-45`). The disk thread main
loop dispatches on each variant via `process_file` /
`process_whole_file` (`disk_commit/thread.rs:172-234`).

## 2. Producer vs consumer rate model

### Producer (network reader)

The network thread hot path performs three sends per non-coalesced file
(`Begin`, one or more `Chunk`, `Commit`) and one send per coalesced
single-chunk file (`WholeFile`). The `WholeFile` shortcut exists precisely
to collapse the 3-message cost for small files
(`messages.rs:32-37`,
`disk_commit/process.rs:149-206`).

Per-chunk producer cost on the steady state is dominated by:

- Receiving and decompressing one delta token from the multiplex stream.
- Optional rolling/strong checksum work on the network thread (the per-file
  `ChecksumVerifier` is moved to the disk thread to overlap hashing with
  disk I/O - see `process.rs:58-79`).
- One spin-bounded `Sender::send` per chunk plus one recv on
  `buf_return_rx` to recycle the previous buffer.

### Consumer (disk writer)

The disk thread allocates one 256 KiB scratch buffer (`WRITE_BUF_SIZE`,
`writer.rs:20`) reused for the lifetime of the thread, mirroring upstream's
static `wf_writeBuf` (fileio.c:161). For each file it:

1. Opens the output (temp+rename, inplace, or device target -
   `process.rs:225-249`).
2. Constructs a `Writer` variant: `Buffered` via `ReusableBufWriter`,
   `IoUring` via the persistent `IoUringDiskBatch`, or `Iocp` via the
   persistent `IocpDiskBatch` (`writer.rs:141-151`,
   `process.rs:269-296`). Sparse mode and append mode force the buffered
   path because the batched writers do not implement `Seek`.
3. Pulls `Chunk` messages, optionally hashes, writes, and recycles the
   buffer through `buf_return_tx` (`process.rs:73-90`).
4. On `Commit` flushes / fsyncs, runs `commit_file` (rename, backup,
   inplace truncation - `process.rs:299-327`), then applies metadata,
   ACLs, and xattrs (`process.rs:333-405`).

### Rate asymmetry

Steady state network throughput is bounded by network bandwidth and per-chunk
checksum cost. Steady state disk throughput depends on:

- Backend: io_uring/IOCP batched submissions versus buffered `write_all` /
  `writev` (`writer.rs:90-122, 177-244`).
- `do_fsync` setting: a synchronous `sync_all` per file collapses the
  pipeline because `process_file` blocks on fsync before sending
  `CommitResult` (`process.rs:91-116`, `writer.rs:192-209`).
- Metadata application latency: `apply_metadata_from_file_entry` plus ACL
  and xattr application happens before the next file's recv starts
  (`process.rs:107, 333-405`).

The two threads are asymmetric: the network thread can produce a `Chunk`
roughly every 32 KB of decompressed delta, while the disk thread spends a
non-trivial fraction of each file in fsync + metadata which the producer
cannot front-load past the channel's capacity.

## 3. Capacity choice and backpressure semantics

The chosen defaults (`config.rs:32, 99-115`) are:

- `DEFAULT_CHANNEL_CAPACITY = 128` slots for `file_tx`. The doc-comment
  budgets this as `128 * ~32 KiB = ~4 MiB` peak buffered messages. In
  practice each `Chunk` owns its own `Vec<u8>` whose capacity is governed
  by upstream token sizing, so 4 MiB is a lower bound.
- `result_rx` and `buf_return_rx` are sized at `capacity * 2 = 256` slots
  (`thread.rs:49-51`). The doubled size keeps the result-return path from
  becoming a head-of-line blocker when the network thread is briefly
  busy parsing the next file's flist entry.
- `MIN_CHANNEL_CAPACITY = 8`, `MAX_CHANNEL_CAPACITY = 4096`
  (`config.rs:35-38`). The minimum prevents single-slot starvation; the
  maximum caps memory at roughly `4096 * chunk_bytes` per channel.
- The capacity is exposed via `DiskCommitConfig::channel_capacity` and
  clamped at runtime by `effective_channel_capacity`
  (`config.rs:65-72, 117-127`). There is currently no CLI / env override
  surface for it.

Backpressure is implemented as a userspace spin-wait. When the queue is
full, `Sender::send` loops in `std::hint::spin_loop` rather than parking
(`spsc.rs:74-87`). Implications:

- The network thread cannot block the kernel scheduler while waiting for
  the disk thread; it consumes a CPU core while spinning. On an idle
  receiver this is the desired latency profile, but on an oversubscribed
  CPU (small VM, container with `--cpus=1`) the spin can starve the disk
  thread and deadlock progress except for the kernel preempting the
  spinner.
- Once the queue is full the producer's effective rate equals the
  consumer's drain rate, with no buffering benefit beyond the queue depth.
- `buf_return_rx` errors (full or disconnected) are deliberately ignored
  on the disk side (`process.rs:89, 183`), so the network thread simply
  allocates a fresh `Vec<u8>` if recycling falls behind. Steady state
  recycling is therefore best-effort, not load-bearing.

There is no observability around occupancy today: no counters, no histogram,
no log-line at high-water mark.

## 4. Proposed measurements and improvements

### M1: queue-depth telemetry

Instrument all three channels with sampled depth counters. `ArrayQueue`
exposes `len()` and `capacity()`, which are eventually-consistent reads
suitable for periodic sampling. Land a thin wrapper around `spsc::channel`
that records:

- Per-second min/max/avg occupancy.
- Producer spin-count (incremented on every `push` retry in
  `Sender::send`, `spsc.rs:79-84`).
- Consumer spin-count (analogous in `Receiver::recv`,
  `spsc.rs:110-120`).

Surface the histogram behind a `--debug io2` log level mirroring existing
disk-IO logging (`disk_commit/thread.rs:114-164`) so users can confirm
whether the bottleneck is the network or the disk side without tracing.
Acceptance: a benchmark run against `tools/ci/run_interop.sh` yields a
distribution with at least one sample per file.

### M2: dynamic capacity sizing

Replace the fixed `DEFAULT_CHANNEL_CAPACITY = 128` with a capacity derived
from observable transfer parameters at `spawn_disk_thread` time:

- File-count hint from the flist: small flists (less than 128 files) keep
  the default; large flists scale toward `MAX_CHANNEL_CAPACITY` to absorb
  bursty arrivals.
- Average chunk size from upstream block-size negotiation (the same input
  consumed by `BufferPool` adaptive sizing). Targeting a fixed memory
  budget (for example 8 MiB) instead of a fixed slot count keeps the
  buffered-message memory cap independent of token sizing.
- Optional `OC_RSYNC_DISK_CHANNEL_CAPACITY` env override mirroring the
  `OC_RSYNC_BUFFER_POOL_SIZE` knob already used for `BufferPool` tuning.

Acceptance: bench shows fewer producer-spin samples on large-file pulls
without a regression in small-file flist throughput.

### M3: write coalescing on the consumer

The disk thread already coalesces single-chunk files via `WholeFile`
(`messages.rs:32-37`, `process.rs:149-206`) but multi-chunk files re-enter
the recv loop on every chunk. Two coalescing improvements:

- **Drain in batches.** Replace the single `file_rx.recv()` in
  `process_file`'s loop (`process.rs:66`) with a primary `recv` followed
  by a bounded `try_recv` drain into a small local `Vec<FileMessage>`.
  This lets the writer issue a single `writev` (or io_uring chain) over
  multiple small chunks. The `Writer::Buffered` path already issues
  `writev` for buffered + new chunk pairs (`writer.rs:35-65, 91-113`);
  extending it to cover N chunks amortizes the syscall over more data.
- **Promote multi-chunk small files to `WholeFile`-equivalent batching.**
  When the network thread observes that a file's total token bytes fit
  inside a single buffer, it can hold the chunks in the buffer pool and
  send a single `WholeFile`. This keeps the channel's per-file cost at
  one send even when the sender ships multiple tokens.

Acceptance: under a workload of files in the 32 KiB-1 MiB range, syscalls
per file (counted with `perf trace -p $(pgrep oc-rsync)`) drop measurably
versus master.

### M4: fsync deferral and group commit

Today `do_fsync` causes a per-file `sync_all` inside `flush_and_sync` on
the buffered path (`writer.rs:192-209`) and a per-file
`commit_file(do_fsync)` on the io_uring/IOCP paths
(`writer.rs:226-244`). Each fsync stalls the consumer until the disk
flushes, which is the dominant source of channel saturation on workloads
with many small files.

Two complementary changes:

- **Group commit.** Track a queue of "committed but not synced" files on
  the disk thread. Issue a single fsync (or `syncfs`, where supported)
  every N files or every T milliseconds, then publish the corresponding
  `CommitResult` items. Mirrors the deferred-fsync approach already used
  in `engine` (see project performance notes on deferred fsync).
- **Async fsync via io_uring.** On Linux 5.6+ with `io_uring`, submit
  `IORING_OP_FSYNC` from `IoUringDiskBatch::commit_file` and let the
  next file's writes overlap the fsync wait. The current `commit_file`
  already integrates fsync into the batch (`writer.rs:226-244`); the
  follow-up is making the wait happen out-of-line of `process_file`'s
  return path.

Acceptance: on a `--fsync` run, channel-occupancy telemetry from M1
spends materially less time at full capacity.

### M5: guarded fallback away from spin-wait

`Sender::send` spins indefinitely while the disk thread is alive
(`spsc.rs:74-87`). On constrained CPU environments this is undesirable.
Add a fallback that, after a bounded number of spins, yields with
`std::thread::yield_now` and (after another bound) parks via
`std::thread::park_timeout`. The disk-side `recv` would unpark on push.
This is the same staircase used by `crossbeam-channel`'s bounded
implementation and keeps the lock-free fast path intact while restoring
fairness when the producer is starved.

Acceptance: a `--cpus=1` containerized run completes a transfer that
otherwise stalls under the spin-only design, with no regression on
unconstrained runs.

## 5. Out of scope

- Replacing the SPSC primitive itself (`crossbeam-channel` or async
  `tokio::sync::mpsc`) - the lock-free `ArrayQueue` is the deliberate
  choice for the network->disk hot path and is documented in the project
  performance notes.
- Changing `FileMessage` to a wire-shared form. The pipeline lives within
  one process and uses owned `Vec<u8>` deliberately for buffer recycling
  via `buf_return_tx`.

## References

- `crates/transfer/src/pipeline/spsc.rs` - SPSC primitive.
- `crates/transfer/src/pipeline/messages.rs` - `FileMessage` /
  `BeginMessage` / `CommitResult`.
- `crates/transfer/src/disk_commit/{mod,config,thread,process,writer}.rs`
  - disk thread, channel wiring, processing logic.
- `crates/engine/src/local_copy/buffer_pool/` - parallel adaptive-capacity
  precedent referenced by M2.
- Issue #1081 - this audit's tracking item.
