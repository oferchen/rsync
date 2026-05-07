# Profile: stdlib sync channel overhead in transfer hot path (#1592)

Tracks remaining `std::sync::mpsc` use in the transfer/engine hot path,
quantifies suspected overhead, and lays out a profile + decision plan
for migrating the rest to `crossbeam-channel`.

## 1. Current channel usage (engine pipeline)

The engine SPSC/SPMC plumbing lives under
`crates/engine/src/concurrent_delta/`, not `pipeline/spsc.rs` or
`pipeline/work_queue.rs` (those names predate the consolidation).

Already-migrated to `crossbeam-channel = "0.5"` (workspace `Cargo.toml`):

- `concurrent_delta/work_queue/bounded.rs` -
  `crossbeam_channel::bounded(capacity)` backs `WorkQueueSender` /
  `WorkQueueReceiver`. Capacity defaults to `2 * num_threads`.
- `concurrent_delta/work_queue/drain.rs` - the `drain_parallel_into`
  streaming variant emits via `crossbeam_channel::Sender<R>`.
- `concurrent_delta/consumer.rs:135` - bounded crossbeam channel
  between the `delta-drain` thread and the `delta-reorder` thread
  (`stream_capacity = max(reorder_capacity, 2 * num_threads)`).

Still on stdlib mpsc inside the same pipeline:

- `concurrent_delta/consumer.rs:47,130` - `mpsc::channel()` carries
  in-order `DeltaResult`s out of the `delta-reorder` thread to
  `DeltaConsumer::iter()`. SPSC, unbounded.
- `concurrent_delta/reorder.rs:719` - test-only `mpsc` usage.

## 2. Remaining `std::sync::mpsc` / `sync_channel` sites

`rg "std::sync::mpsc|sync_channel"` (production paths only):

- `crates/checksums/src/pipelined/reader.rs:7` - `DoubleBufferedReader`
  spawns an I/O thread; SPSC unbounded `mpsc::channel` carries `Block`
  / `Eof` / `Error`. **Hot:** runs once per file under `--checksum`
  and per pipelined verify.
- `crates/engine/src/concurrent_delta/consumer.rs` - see section 1.
- `crates/transfer/src/reorder_buffer.rs:429` - test-only.
- `crates/core/src/client/remote/remote_to_remote.rs:40` - daemon-mode
  pump synchroniser (low frequency; outside delta hot path).
- `crates/daemon/src/daemon/sections/server_runtime/connection.rs:286`
  - test-only.
- `crates/rsync_io/src/ssh/embedded/connect.rs:169` - bounded
  `sync_channel::<Vec<u8>>(64)` carries inbound SSH frames from a
  tokio task to the blocking `ChannelReader`. **Hot on SSH transfers.**
- `crates/fast_io/src/iocp/pump.rs:543-545`,
  `crates/fast_io/src/iocp/socket.rs:366`,
  `crates/fast_io/src/iocp_stub.rs:667-669` - Windows IOCP completion
  pump (one-shot per op).
- `crates/fast_io/src/io_uring/tests.rs`,
  `crates/rsync_io/src/ssh/tests.rs`,
  `crates/core/tests/test_timeout.rs` - tests only.

Production SPSC sites still on stdlib: **3** (checksums reader,
delta consumer output, embedded SSH inbound). IOCP pump is
per-operation, not per-byte, so it is not a hot path.

## 3. Suspected overhead

stdlib `mpsc` (since the 2023 rewrite to `crossbeam` internals) is
competitive on throughput but still pays for:

- **Park/unpark wakeups.** Each `recv()` on an empty queue parks via
  `Thread::park`; the producer's `unpark` is a syscall on Linux
  (futex) and a `WakeByAddressSingle` on Windows. `crossbeam-channel`
  uses its own `Backoff` spin first, which avoids the syscall when
  the producer arrives within the spin window.
- **Allocator contention.** `mpsc::channel()` allocates a fresh
  `Block<T>` per ~32 sends. On a 1 M-message stream that is ~31 K
  allocations on the global allocator, contending with any concurrent
  buffer-pool churn in `engine`.
- **False sharing.** stdlib's block layout co-locates the head/tail
  cursors on adjacent cache lines for sub-block messages. With
  `DeltaResult` (~64 B today) two threads can ping-pong the same
  64 B line on every send/recv pair.

`crossbeam-channel::bounded(N)` uses an array-backed slot table with
per-slot stamps and explicit padding to `CACHE_LINE` between producer
and consumer cursors, eliminating both the per-block alloc and the
cursor false sharing.

## 4. Profile plan

**Microbench** (criterion, new
`crates/engine/benches/spsc_channel.rs`):

- One producer / one consumer, 1 M `DeltaResult`-shaped messages.
- Variants: `std::sync::mpsc` unbounded, `mpsc::sync_channel(N)` for
  `N in {64, 1024}`, `crossbeam_channel::unbounded`,
  `crossbeam_channel::bounded(N)` for the same `N`.
- Metrics: ns/msg, allocations/op (`dhat`), peak RSS delta.
- Pin producer and consumer to separate physical cores; report
  best-of-3 medians to absorb scheduler noise.

**Cache-line evidence** (`perf c2c`, Linux only, run inside the
`rsync-profile` container):

- `perf c2c record -F 99 -- ./bench --variant std-mpsc --msgs 5_000_000`
- `perf c2c report --stats-only` - look for HITM lines pointing at
  the channel's cursor struct. Repeat for crossbeam; HITM count
  should drop ~10x if false sharing dominates.

**Real-workload validation:** rerun `scripts/benchmark_hyperfine.sh`
on a 100 K small-file tree before and after migrating each call site.

## 5. Decision

Migrate the **two production hot SPSC sites** (`checksums` pipelined
reader, `concurrent_delta::consumer` output) to
`crossbeam_channel::bounded` with an explicit capacity (`64` for the
reader, `reorder_capacity` for the consumer) once microbench shows
>=15% ns/msg improvement and `perf c2c` confirms HITM drop. The
embedded SSH `sync_channel::<Vec<u8>>(64)` is already bounded and
crosses an async/blocking boundary; migration there is gated on
ensuring `russh`'s tokio task tolerates `crossbeam`'s blocking
`send` (it does - the task is already long-lived).

Accept stdlib `mpsc` for **test-only** sites and the **IOCP pump**
(one-shot per op, no measurable benefit). Track migrations under
issue #1592.
