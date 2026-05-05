# Static analysis of channel overhead in the transfer hot path (#1592)

Code-only audit of every synchronisation channel data flows through on
the receiver-side transfer. No runtime numbers. The goal is to enumerate
channel sites, classify each one as hot or amortised cold, derive
ops-per-message from the source, and compare against upstream rsync's
single-threaded loop.

Companion work:

- #1369 - SPSC contention metrics (completed).
- #1744 / #1745 / #1746 / #1747 - crossbeam migration (completed).
- #1589 - rayon + stdlib audit (completed).
- #1591 - async channel design (completed).
- #1854 - parallel audit written alongside this one.

## 1. Methodology

### 1.1 Scope and source of truth

The audit covers every channel constructor and send / recv site reachable
from the receiver-side transfer hot loop on protocol 32. Entry points
are `crates/transfer/src/pipeline/mod.rs:62-77` (sync receiver) and
`crates/engine/src/concurrent_delta/mod.rs:171-186` (parallel delta
pipeline). Citations anchor to commit 60e83fd96.

No runtime data is used. Every cost claim is derived from the visible
code path or from the documented contract of crossbeam 0.5.x or
tokio 1.x. When a derivation depends on the queue rather than user code,
the citation still points at the call site - the queue contract is taken
as given.

### 1.2 Cost model

Per message we account:

- **Atomic ops** - any read-modify-write or load/store with explicit
  ordering on shared state in the queue or wrapper.
- **Allocations** - heap allocations on the message lifetime, broken
  out by allocator vs deallocator. Buffer-pool reuse is amortised free.
- **Syscalls** - anything that can reach the kernel. `std::hint::spin_loop`
  and `std::thread::yield_now` do not count; condvar park, futex park,
  and tokio mio wake-ups do.
- **Payload move** - whether the message is moved by value, boxed, or
  held by reference / `Arc`.

Spin-wait length is bounded statically only by capacity vs rate ratio,
which is a runtime quantity. Section 8 defers all spin-budget questions
to runtime work.

### 1.3 What counts as the hot path

A site is hot if its rate is `O(files)` or `O(chunks)` for `N` files.
Cold if it is `O(1)` per transfer (setup, shutdown). Retry-rate sites
(bounded by `MAX_RETRY_COUNT = 2` at
`crates/transfer/src/pipeline/job.rs:23`) are treated as cold.

## 2. Channel taxonomy

### 2.1 Inventory

In data-flow order. SPSC = single-producer single-consumer, MPSC =
multi-producer single-consumer, SPMC = single-producer multi-consumer.

| # | Site | File:Line | Pattern | Backing | Bound | Hot? |
|---|------|-----------|---------|---------|-------|------|
| 1 | `FileMessage` (network -> disk) | `crates/transfer/src/disk_commit/thread.rs:49` | SPSC | `crossbeam_queue::ArrayQueue` (custom) | `channel_capacity` (default 128) | hot |
| 2 | `io::Result<CommitResult>` (disk -> network) | `crates/transfer/src/disk_commit/thread.rs:50` | SPSC | same | `capacity * 2` (default 256) | hot |
| 3 | Buffer recycle `Vec<u8>` (disk -> network) | `crates/transfer/src/disk_commit/thread.rs:51` | SPSC | same | `capacity * 2` (default 256) | hot |
| 4 | `FileJob` async dispatch | `crates/transfer/src/pipeline/async_pipeline.rs:164` | MPSC (one producer) | `tokio::sync::mpsc` | `job_channel_capacity` (default 32) | hot |
| 5 | `DeltaWork` work queue | `crates/engine/src/concurrent_delta/work_queue/bounded.rs:102` | SPMC (rayon scope) | `crossbeam_channel::bounded` | `2 * threads` (adaptive `2x..8x`) | hot |
| 6 | `DeltaResult` stream (drain -> reorder) | `crates/engine/src/concurrent_delta/consumer.rs:135` | MPSC (rayon -> reorder) | `crossbeam_channel::bounded` | `max(reorder_cap, 2 * threads)` | hot |
| 7 | `DeltaResult` ordered output | `crates/engine/src/concurrent_delta/consumer.rs:130` | MPSC unbounded | `std::sync::mpsc` | unbounded | hot |
| 8 | `Payload` reorder buffer test | `crates/transfer/src/reorder_buffer.rs:519` | MPSC | `std::sync::mpsc` | unbounded | cold (test only) |

Site 4 fires only when `run_pipeline` is used; the synchronous receiver
does not allocate it. Site 8 is in `#[cfg(test)]`.

The `multi-producer` cargo feature converts site 5 from SPMC to MPMC by
enabling `Clone` on the sender at
`crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17`.
Default builds remain SPMC.

### 2.2 No production MPMC

Section 2.1 confirms there are no MPMC channels in the production
receiver. Compile-time enforcement is `WorkQueueSender: !Clone` outside
the feature - see `bounded.rs:48-49` and the rationale at
`crates/engine/src/concurrent_delta/work_queue/mod.rs:18-21` which
notes "the rsync wire protocol is inherently single-threaded on the
receiving side".

This matters for the cost model: crossbeam's MPMC `bounded` takes a
fairness path with extra atomics; SPMC always hits the producer
fast path.

## 3. Per-channel detail

### 3.1 Site 1 - `FileMessage` (network -> disk)

Constructed at `crates/transfer/src/disk_commit/thread.rs:49` via
`spsc::channel::<FileMessage>(capacity)`. Capacity comes from
`DiskCommitConfig::effective_channel_capacity()` clamping
`channel_capacity` to `8..=4096` (default 128 -
`crates/transfer/src/disk_commit/config.rs:32`).

Sender: the network ingest thread. Send sites at
`crates/transfer/src/transfer_ops/streaming.rs:156-184`,
`crates/transfer/src/transfer_ops/token_loop.rs:118-148`,
`crates/transfer/src/transfer_ops/token_loop.rs:187`, and
`crates/transfer/src/pipeline/receiver.rs:350,377`.

Receiver: `disk_thread_main` at
`crates/transfer/src/disk_commit/thread.rs:184` plus per-file recv at
`crates/transfer/src/disk_commit/process.rs:66`.

Variants: `Begin(Box<BeginMessage>)`, `Chunk(Vec<u8>)`, `Commit`,
`WholeFile { begin, data }`, `Abort { reason }`, `Shutdown`
(`crates/transfer/src/pipeline/messages.rs:21-45`).

Lifetime: spawned in
`PipelinedReceiver::new` (`crates/transfer/src/pipeline/receiver.rs:78`)
and torn down in `shutdown` (`receiver.rs:346-357`) or `Drop`
(`receiver.rs:374-382`).

### 3.2 Site 2 - `CommitResult` (disk -> network)

`crates/transfer/src/disk_commit/thread.rs:50`. Capacity `capacity * 2`
(default 256) - sized larger than the request channel because the disk
thread can produce results before the network thread drains them.
Sends at `thread.rs:197,211,220`. Drains at
`crates/transfer/src/pipeline/receiver.rs:148` (non-blocking) and
`receiver.rs:206` (blocking). Message is `io::Result<CommitResult>`
(`messages.rs:110-119`); heap allocation only on the recoverable
metadata-error path which is rare.

### 3.3 Site 3 - Buffer recycle

`crates/transfer/src/disk_commit/thread.rs:51`. Capacity `capacity * 2`,
default 256. Producer is the disk thread after consuming a `Chunk`
(`crates/transfer/src/disk_commit/process.rs:89`). Consumer is the
network thread, via `try_recv` only, inside `recycle_or_alloc`
(`crates/transfer/src/transfer_ops/token_loop.rs:35-44`). Message is
`Vec<u8>`, cleared and resized on the network side. With `WholeFile`
coalescing the channel is bypassed for single-chunk files.

### 3.4 Site 4 - Async `FileJob` dispatch

`crates/transfer/src/pipeline/async_pipeline.rs:164` via
`mpsc::channel(capacity)`. Capacity clamped to `1..=256`, default 32
(`crates/transfer/src/pipeline/mod.rs:206`). Producer is
`produce_file_jobs` at
`crates/transfer/src/pipeline/async_dispatch.rs:48`; consumer is
`consume_jobs` at
`crates/transfer/src/pipeline/async_pipeline.rs:224`. Message is
`FileJob` (`crates/transfer/src/pipeline/job.rs:103`) carrying `ndx`,
`dest_path`, `Arc<FileEntry>`, `TransferFlags`. Each job is a fresh
`Arc::new(entry.clone())` (`async_dispatch.rs:43`). Synchronous
`PipelinedReceiver` does not consume `FileJob` - it consumes
`FileMessage` directly.

### 3.5 Site 5 - `DeltaWork` work queue

`crates/engine/src/concurrent_delta/work_queue/bounded.rs:102` via
`crossbeam_channel::bounded(capacity)`. Default capacity
`rayon::current_num_threads() * 2` (`capacity.rs:36`); adaptive policy
returns `2x`, `4x`, or `8x` thread count (`capacity.rs:66-76`).
Producer: `ParallelDeltaPipeline::submit_work`
(`crates/transfer/src/delta_pipeline.rs:223-234`) - single producer
enforced by `!Clone`. Consumers: rayon workers inside
`drain_parallel_into` (`drain.rs:136-156`, invoked from
`consumer.rs:141`); each worker pulls via the iterator at
`work_queue/iter.rs:33`. Constructed only when
`ParallelDeltaPipeline::new` runs (`delta_pipeline.rs:209-219`).

### 3.6 Site 6 - `DeltaResult` stream

`crates/engine/src/concurrent_delta/consumer.rs:135` via
`crossbeam_channel::bounded::<DeltaResult>(stream_capacity)`. Capacity
`reorder_capacity.max(rayon::current_num_threads() * 2)` (line 134).
Producers: every rayon worker inside `drain_parallel_into` clones the
sender (`drain.rs:144`) and sends one `DeltaResult` (`drain.rs:149`),
giving N concurrent producers. Consumer: the `delta-reorder` thread
(`consumer.rs:151`).

### 3.7 Site 7 - `DeltaResult` ordered output

`crates/engine/src/concurrent_delta/consumer.rs:130` via
`mpsc::channel()` - unbounded. Producer: the `delta-reorder` thread
(`consumer.rs:158,172,180`). Consumer: `DeltaConsumer::iter()` consumed
by the receiver pipeline. The migration in #1744 left this on
`std::sync::mpsc` because the consumer side polls via `try_recv()` and
the in-order ring buffer ahead of it is the bounded backpressure point.

### 3.8 Site 8 - test channel

`crates/transfer/src/reorder_buffer.rs:519` builds an
`mpsc::channel::<Payload>()` inside `#[cfg(test)]`. Listed only for
inventory completeness.

## 4. Crossbeam ArrayQueue cost model

Per-message cost of each backend, derived from the call sites above.

### 4.1 SPSC over `crossbeam_queue::ArrayQueue` (sites 1, 2, 3)

`crates/transfer/src/pipeline/spsc.rs:74-87` (send) and `spsc.rs:110-121`
(recv) wrap `ArrayQueue::push` / `ArrayQueue::pop`. Steady state:

- **Send fast path** (`spsc.rs:79-80`): one relaxed load of
  `consumer_alive` (line 76) + one `ArrayQueue::push` which performs one
  CAS on the tail-slot stamp + one `store(Release)` on the value. Two
  atomics. Zero syscalls. Zero allocations.
- **Recv fast path** (`spsc.rs:111-114`): one `ArrayQueue::pop` which
  performs one CAS on the head-slot stamp + one `load(Acquire)` on the
  value, then a branch. Two atomics. Zero syscalls.
- **Backpressure** (full queue, `spsc.rs:81-85`): `std::hint::spin_loop`.
  No syscall, no thread park - the trade-off is that a long stall pegs
  one core. Section 8.1 lists the runtime work.
- **Empty path** (`spsc.rs:115-120`): same spin pattern.
- **Disconnect**: one `AtomicBool::load(Relaxed)` on send (line 76), one
  `AtomicBool::load(Acquire)` on recv (line 115). Drop sets the flag with
  `Release` (lines 98, 141).

Steady state: 4 atomics per message round trip (2 send + 2 recv) plus 2
cheap liveness loads. Zero syscalls.

### 4.2 `crossbeam_channel::bounded` (sites 5, 6)

Different layout from `ArrayQueue` - a Treiber-style array with futex
park on full / empty. Per message, fast path: one CAS on tail/head + one
slot write/read + at most one notify. The notify is a wake-only path -
costs a relaxed load on the parker state if no one is parked. On full /
empty: one syscall to park, one to wake. Once the channel runs full or
empty repeatedly, syscalls per message rise. This is the practical
reason crossbeam is preferred over `std::sync::mpsc` (which mandates
park) under contention but is heavier than `ArrayQueue` under steady
streaming.

### 4.3 `tokio::sync::mpsc` (site 4)

Per message, no suspension: similar cost to crossbeam bounded - one CAS
on the in-flight count + one slot push + one waker notify if a receiver
is parked. Per message that suspends: tokio waker registration through
the runtime, plus mio readiness signalling if cross-worker. Same-task
same-worker is cheaper - no eventfd write in modern tokio. The async
state machine adds bookkeeping that does not appear on crossbeam.

### 4.4 `std::sync::mpsc` (site 7)

Unbounded linked-list channel. Per send: one heap allocation
(`Box<Node<T>>`), one push, one condvar notify (cheap when nobody
parked). Per recv: one pop, one read, one node free. So one alloc + one
free + ~3 atomics per message + condvar park / wake on empty. Heaviest
per-op cost in the inventory, but at site 7 the rate equals file rate
(not chunk rate) because the reorder buffer ahead of it serialises.

### 4.5 Summary

| Site | Backend | Atomics | Allocs | Park / wake | Hot |
|------|---------|--------:|-------:|-------------|-----|
| 1 | SPSC ArrayQueue | 4 | 0 (payload pre-alloc) | none | yes |
| 2 | SPSC ArrayQueue | 4 | 0 | none | yes |
| 3 | SPSC ArrayQueue | 4 | 0 (recycled buf) | none | yes |
| 4 | tokio mpsc | ~4-6 | 0 | waker + async | yes |
| 5 | crossbeam bounded | ~4 | 0 | futex on full / empty | yes |
| 6 | crossbeam bounded | ~4 | 0 | futex on full / empty | yes |
| 7 | std mpsc | ~3 | 1 + 1 | condvar park | per file |
| 8 | std mpsc | n/a | n/a | n/a | test |

## 5. Static estimate at 100k files

Workload: 100,000 small files end-to-end through the synchronous
(non-async, non-parallel-delta) pipeline. Average file = single chunk
coalesced into `FileMessage::WholeFile` per
`crates/transfer/src/transfer_ops/streaming.rs:107-172`. The buffer
recycle does not fire for `WholeFile` because the `Vec<u8>` is owned by
the message itself.

### 5.1 Synchronous receiver, small-file majority

Per file:

- Site 1: 1 `WholeFile` send + 1 recv = 4 atomics.
- Site 2: 1 `CommitResult` round trip = 4 atomics.

Total: 8 atomics per file in the channel layer, 0 allocs by channels,
0 syscalls. At 100,000 files: 800,000 atomics, no syscalls in channels.

The `Box<BeginMessage>` allocation at `streaming.rs:110` is 1 alloc per
file = 100,000 allocs - it is a per-message allocation but not a channel
cost.

### 5.2 Synchronous receiver, multi-chunk file

K chunks per file:

- Site 1: `Begin` + K `Chunk` + `Commit` sends, K+2 recvs = `4*(K+2)`
  atomics.
- Site 3: K returns + K `try_recv` = `4K` atomics.
- Site 2: 1 round trip = 4 atomics.

Total: `8K + 12` atomics per file. K=8, 100,000 files:
`(64 + 12) * 100000 = 7.6M` atomics.

### 5.3 Parallel delta pipeline

When `ParallelDeltaPipeline` is wired
(`crates/transfer/src/delta_pipeline.rs:212`), each file additionally
crosses sites 5, 6, 7:

- Site 5: 1 send + 1 recv = 4 atomics.
- Site 6: 1 send + 1 recv = 4 atomics.
- Site 7: 1 send + 1 recv = 3 atomics + 1 alloc + 1 free.

Per file: 11 atomics + 1 alloc + 1 free. At 100,000 files: 1.1M atomics,
100,000 allocs, 100,000 frees, on top of 5.1 / 5.2.

The single-allocation per file at site 7 is the largest concrete static
cost identified. Section 8.3 records the runtime question of whether
those ops are visible in a profiler relative to the file system metadata
work that follows (open / write / fsync / rename).

### 5.4 Async dispatch

Site 4 adds 4-6 atomics per file plus tokio waker bookkeeping. Per
100,000 files: ~500,000 atomics, plus an unknown wake count bounded by
the channel capacity (default 32).

## 6. Hot vs cold

| Channel | Per-transfer | Per-file | Per-chunk | Class |
|---------|-------------:|---------:|----------:|-------|
| 1 | 1 (Shutdown) | 1 (WholeFile) or 2 (Begin+Commit) | K (Chunk) | hot |
| 2 | 0 | 1 | 0 | hot |
| 3 | 0 | 0 | up to K | hot, skipped on WholeFile |
| 4 | 0 | 1 | 0 | hot, async only |
| 5 | 0 | 1 | 0 | hot, parallel delta only |
| 6 | 0 | 1 | 0 | hot, parallel delta only |
| 7 | 0 | 1 | 0 | hot, parallel delta only |

There are no amortised cold channels in the production pipeline. Every
listed channel fires at file rate or chunk rate. The ack-batcher
(referenced at `crates/transfer/src/pipeline/mod.rs:79-82`) is a
buffered counter, not a channel.

## 7. Comparison with upstream

Upstream rsync 3.4.1 has zero in-process channels. The receiver runs a
single while loop in `recv_files()` at
`target/interop/upstream-src/rsync-3.4.1/receiver.c:522-588`, calling
`receive_data()` at `receiver.c:240` for each file. There is no
producer / consumer split, no work queue, no ordered output buffer.
Network reads, delta application, and disk writes all happen on the
same OS thread, in the same call stack.

Upstream pays:

- 0 atomics for in-process synchronisation.
- 0 channel allocations.
- 0 cross-thread wake-ups.

Upstream costs are concentrated in the syscall layer (`read`, `write`,
`fsync`, `rename`) and in libc allocation for the message buffer
(`fileio.c` `wf_writeBuf`, a single static reused for every file). That
single-buffer reuse is the C analogue of our buffer recycle channel
(site 3) - except there is no channel, just a static.

The cost the parallel implementation pays for parallelism is therefore
the entire content of section 5: 8-11 atomics per file synchronously,
plus chunk-rate atomics for multi-chunk files, plus an `mpsc::channel()`
allocation per file in the parallel delta path. Whether that cost is
recovered by overlapped network and disk I/O is a runtime question.

The static-only conclusion: in-process channel cost per file on the
synchronous path is bounded by a constant - it does not scale with file
size, only with chunk count - and that constant is small relative to
the per-file syscall budget (open, close, rename, optional fsync, plus
per-chunk write). The parallel delta pipeline adds one allocation per
file at site 7 which is visible in absolute terms but small relative to
the message payload allocations.

## 8. Recommended runtime measurements for #1592

Static analysis leaves several questions only runtime data can answer.

### 8.1 SPSC spin budget under backpressure

Sites 1, 2, 3 spin rather than park. The cost model in 4.1 assumes the
queue does not stall. If the network ingest stalls on a full
`FileMessage` queue, or the disk thread stalls on a full result queue,
the static estimate is replaced by an unbounded spin. Measure mean / p99
time inside `Sender::send` rotating in the spin path
(`crates/transfer/src/pipeline/spsc.rs:79-85`), the same for
`Receiver::recv` (`spsc.rs:110-120`), and the rate of `try_recv`
returning `Empty` from `recycle_or_alloc`
(`crates/transfer/src/transfer_ops/token_loop.rs:35`) which signals
buffer exhaustion forcing an alloc instead of recycle. Counter hooks
behind a feature flag are the cheapest path.

### 8.2 `WholeFile` coalescing rate

5.1 assumes single-chunk files always coalesce. Measure the ratio of
`WholeFile` to `Begin + Chunk + Commit` sends at site 1 over a real
workload. Coalescing is gated by "first token is a literal and next
token is end-of-file"
(`crates/transfer/src/transfer_ops/streaming.rs:144-172`), which depends
on basis presence and sender chunking.

### 8.3 Allocation per file at site 7

Convert the 1-alloc-1-free static cost at site 7 into wall-clock time.
Two paths to the same answer: hook a counting allocator and report
`mpsc` allocations per file; or replace site 7 with a bounded
`crossbeam_channel` and measure the delta. The reorder buffer ahead of
it already serialises the stream, so unboundedness is not load-bearing.

### 8.4 Tokio wake-up rate at site 4

When the async path is in use, every `send` may wake the consumer.
Capture waker invocations per 100,000 jobs and compare to the static
4-6 atomics per message baseline.

### 8.5 Crossbeam park / wake at sites 5, 6

When the work queue runs full or the stream channel runs empty,
crossbeam parks. Measure park rate as a function of worker count and
file size. The adaptive capacity at
`crates/engine/src/concurrent_delta/work_queue/capacity.rs:66-76`
already attempts to size the queue away from this; runtime data should
confirm it does.

### 8.6 End-to-end channel-vs-syscall ratio

Section 5 predicts channel atomics are dwarfed by per-file syscalls on
the synchronous receiver. Confirm by counting atomics (or sampled
cycles inside send / recv) and comparing to syscall counts from
`strace -c` for the same workload. Hypothesis: channel layer accounts
for less than 1% of receiver-side cycles for large transfers.

## 9. Open questions

1. **Should the buffer-return channel (site 3) be replaced by a
   thread-local cache?** With `WholeFile` coalescing, the channel is
   only useful for multi-chunk files. A per-thread `RefCell<Vec<Vec<u8>>>`
   in the network thread plus an `Arc<Mutex<Vec<Vec<u8>>>>` shared with
   the disk thread would still recycle without one of the channels.
   Static cost is 4 atomics avoided per chunk; whether that matters
   depends on chunk rate (8.1, 8.2).

2. **Is the `std::sync::mpsc` at site 7 a real allocator hot spot?**
   The static estimate shows 100,000 allocations on a 100,000-file
   transfer through the parallel delta pipeline. The reorder buffer
   ahead of it has no inherent need for an unbounded channel - a
   `crossbeam_channel::bounded(worker_count * 2)` would suffice.
   Section 8.3 is the corresponding measurement.

3. **What is the SPSC spin budget under disk backpressure?** Sites 1 and
   2 spin rather than park. If the disk thread blocks on `fsync` or a
   slow `rename`, the network thread will spin in send for the duration.
   Mitigations: bounded spin with `thread::yield_now` after N iterations,
   or futex park after a longer threshold. Decision deferred until 8.1
   data exists.

4. **Should the parallel delta pipeline (sites 5-7) be merged with the
   network -> disk SPSC pipeline (sites 1-3)?** Today they are two
   independent pipelines. A unified queue from `DeltaWork` straight to
   `FileMessage` would remove sites 6 and 7. The blocker is that the
   parallel pipeline currently runs ahead of the network pipeline
   (delta workers compute ahead of disk writes), and merging them would
   couple their backpressure. Static analysis cannot decide; runtime
   data on queue-depth correlation between the two would.

5. **Tokio vs crossbeam for site 4?** Site 4 is `tokio::sync::mpsc`
   because the consumer is an async task. The dispatcher does not need
   async semantics - it sends once per file, then drops. Replacing with
   `crossbeam_channel::bounded` and a blocking consumer would remove
   tokio runtime overhead from dispatch. Static cost delta is modest
   (~2 atomics per file plus waker bookkeeping); whether it is worth
   the architectural change depends on whether the consumer ever awaits
   anything other than `rx.recv()`.

6. **Does `multi-producer` ever ship?** The feature at
   `crates/engine/src/concurrent_delta/work_queue/multi_producer.rs:17`
   would convert site 5 from SPMC to MPMC. Static cost rises (crossbeam
   adds fairness atomics) but throughput could rise if the producer is
   the bottleneck. The wire protocol is single-threaded, so the only
   producer is the network thread - the feature exists for future
   re-architecting and is not on any current roadmap.
