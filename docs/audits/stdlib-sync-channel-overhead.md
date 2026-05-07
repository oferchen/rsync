# `std::sync` channel overhead in the transfer hot path (#1592)

Code-only follow-up to #1744 (which migrated `work_queue.rs` from
`std::sync::mpsc::sync_channel` to `crossbeam_channel::bounded`) and
to the broader static audit at
`docs/audits/transfer-hot-path-channel-overhead-static.md`. Scope is
narrower than the static audit: enumerate every remaining
`std::sync::mpsc::*` and `Arc<Mutex<channel>>` site reachable from the
receiver-side transfer hot path, summarise the per-op cost difference
between `std`, `crossbeam`, and `parking_lot`-backed alternatives, and
specify the criterion benchmark that has to land before any further
migration is justified.

Citations anchor to commit `d3d69a090` on branch
`docs/stdlib-sync-channel-1592`. No runtime numbers appear in this
document - section 4 specifies how to produce them.

## 1. Status of the previous migration

PR #1744 replaced the synchronisation primitive used by the parallel
delta work queue:

- Before: `std::sync::mpsc::sync_channel(2 * threads)` with a clonable
  `Sender` smuggled through `Arc<Mutex<Sender>>` for the multi-producer
  build feature.
- After: `crossbeam_channel::bounded(2 * threads)` at
  `crates/engine/src/concurrent_delta/work_queue/bounded.rs:8`, with a
  non-`Clone` `WorkQueueSender` outside the `multi-producer` feature
  (rationale at
  `crates/engine/src/concurrent_delta/work_queue/mod.rs:18-21`).

The migration removed the per-send mutex acquisition and replaced
condvar park / wake on full / empty with crossbeam's parker. The
`bounded.rs:48-49` invariant - `WorkQueueSender: !Clone` outside the
feature flag - replaces the runtime-checked `Arc<Mutex<Sender>>`
pattern with a compile-time SPMC guarantee. Tests at
`crates/engine/src/concurrent_delta/work_queue/tests.rs:853-857`
verify drop semantics for both backends.

The remaining work for #1592 is the survey of sites that did not
migrate, plus a profile plan that decides the next migration on data.

## 2. Remaining `std::sync::mpsc` sites on the receiver hot path

In data-flow order. "Hot" means a rate of `O(files)` or higher. "Cold"
means `O(1)` per transfer.

| # | Site | File:Line | Pattern | Bound | Hot? |
|---|------|-----------|---------|-------|------|
| A | `DeltaResult` ordered output | `crates/engine/src/concurrent_delta/consumer.rs:130` | MPSC | unbounded | hot, parallel delta only |
| B | `SignatureRequest` queue | `crates/signature/src/async_gen.rs:215` | MPMC via `Arc<Mutex<Receiver>>` | unbounded | hot, async signature only |
| C | `SignatureResult` queue | `crates/signature/src/async_gen.rs:216` | MPSC | unbounded | hot, async signature only |
| D | Pipelined block reader | `crates/checksums/src/pipelined/reader.rs:75` | SPSC | unbounded | hot, large files only |
| E | Reorder buffer integration test | `crates/transfer/src/reorder_buffer.rs:519` | MPSC | unbounded | cold (test) |
| F | `tokio::sync::mpsc` `FileJob` dispatch | `crates/transfer/src/pipeline/async_pipeline.rs:164` | MPSC | bounded | hot, async only |

Sites A through D run on the receiver-side transfer hot path. Site F is
a `tokio` channel and is included for cost-model symmetry only - it is
out of scope for any `std` -> `crossbeam` migration.

The remote-to-remote relay site at
`crates/core/src/client/remote/remote_to_remote.rs:254-255` is on the
client orchestration path, not the receiver hot path, and is excluded.
Sites in `crates/fast_io/src/iocp/`, `crates/daemon/src/.../connection.rs`,
and SSH embedded transports are control-plane channels and excluded.

### 2.1 Site A - `DeltaResult` ordered output

```text
WorkQueue --crossbeam_channel::bounded--> drain
drain    --crossbeam_channel::bounded--> reorder
reorder  --std::sync::mpsc--> consumer  // site A
```

`consumer.rs:130` allocates `mpsc::channel()` (unbounded, linked-list
backed). Each `result_tx.send(ready)` allocates a `Box<Node<T>>` for
the queue node, performs one CAS to splice it onto the tail, and one
condvar notify. Each `result_rx.recv()` performs one CAS to detach the
head, one read, then frees the node.

Per file at one result: 1 alloc + 1 free + ~3 atomics. The reorder
buffer ahead of it serialises the stream, so unbounded is not
load-bearing - a `crossbeam_channel::bounded(reorder_capacity)` would
cover the same demand without per-message allocation.

### 2.2 Site B - `SignatureRequest` queue (async signature generator)

`async_gen.rs:215` opens an `mpsc::channel()`. `async_gen.rs:218`
wraps the receiver in `Arc<std::sync::Mutex<Receiver<_>>>` to share
across N worker threads. Each worker calls
`receiver.lock().unwrap().recv()` per request:

- one `Mutex::lock` (a CAS on uncontended path, futex park on
  contention),
- one `mpsc::Receiver::recv` (one CAS, one alloc-free for the node,
  potentially one condvar park if empty),
- one `Mutex` drop (release store).

This is the canonical "MPMC over `Arc<Mutex<mpsc::Receiver>>`"
pattern. Static cost vs `crossbeam_channel::unbounded`: 2 extra
atomics per recv on the uncontended path, one extra futex park /
wake on contention, plus the per-message node allocation that
`mpsc` always pays.

`crossbeam_channel` produces a clonable `Sender` and a
`Receiver<T>: Clone` for unbounded (or a non-`Clone` `Receiver` that
can be shared by reference). In both cases the `Mutex` is unnecessary.

### 2.3 Site C - `SignatureResult` queue

`async_gen.rs:216` is the symmetric reply path. Workers send,
main thread receives. Pure MPSC, no `Mutex` wrapper. Same per-op cost
as site A: 1 alloc + 1 free + ~3 atomics + condvar park / wake on
empty.

### 2.4 Site D - pipelined block reader

`crates/checksums/src/pipelined/reader.rs:75` allocates
`mpsc::channel()` between an I/O thread that reads file blocks and a
hashing consumer. Pure SPSC. Each block send: 1 node alloc + 1 atomic
splice + 1 condvar notify. Each recv: 1 atomic + 1 read + 1 free.

Block size defaults to 64 KiB per
`crates/checksums/src/pipelined/config.rs:32-34`, with a 256 KiB
minimum file size (`min_file_size`) gating whether pipelining runs at
all. For a 1 GiB file at 64 KiB blocks that is ~16k sends. The
unbounded channel buys nothing - the disk reads ahead at I/O speed,
hashing consumes at CPU speed, and a bounded SPSC ring would supply
the same overlap with O(1) memory and zero per-message allocations.

### 2.5 Site E - test channel

`reorder_buffer.rs:519` is `#[cfg(test)]`. No production cost.

## 3. Per-op overhead, std vs crossbeam vs parking_lot

Per message, fast path (uncontended), assuming the queue is neither
full nor empty:

| Backend | Atomics | Allocs / message | Park on empty | Park on full |
|---------|--------:|----------------:|---------------|--------------|
| `std::sync::mpsc::channel` (unbounded) | ~3 | 1 box + 1 free | condvar | n/a |
| `std::sync::mpsc::sync_channel` (bounded) | ~3 | 0 | condvar | condvar |
| `Arc<Mutex<mpsc::Receiver>>` shared (MPMC) | ~3 + 2 (mutex acq+rel) | 1 + 1 | condvar (mutex+chan) | n/a |
| `crossbeam_channel::unbounded` | ~3 | 0 (slab segments) | parker (custom) | n/a |
| `crossbeam_channel::bounded` | ~3 | 0 | parker | parker |
| `crossbeam_queue::ArrayQueue` (no parker) | ~2 | 0 | spin | spin |
| `parking_lot::Mutex<VecDeque<T>>` (DIY MPMC) | ~3 + parker | 1 amortised | manual condvar | manual |

Sources for the cost figures:

- `std::sync::mpsc` per-message box allocation:
  https://doc.rust-lang.org/std/sync/mpsc/index.html#disconnection.
  The current Rust 1.88.0 implementation uses `crossbeam-channel`
  internally for unbounded since Rust 1.67; even so, the public API
  still imposes node-shaped messages because `mpsc::Sender` does not
  expose batching and the wrapper adds a `Box<T>` indirection in some
  configurations. Treat the per-message alloc as a documented worst
  case.
- `crossbeam_channel` send / recv ops:
  https://docs.rs/crossbeam-channel/0.5/crossbeam_channel/#performance.
  The bounded channel uses a Treiber-style array; the unbounded
  channel uses a chunked linked-list with no per-message allocation
  inside steady-state segments.
- `parking_lot::Mutex`:
  https://docs.rs/parking_lot/0.12/parking_lot/struct.Mutex.html.
  Atomic CAS plus thread-local parking. Cheaper than
  `std::sync::Mutex` only on contention, where it skips the OS
  futex syscall in the fast path. Not a channel by itself - quoted
  here because `Arc<Mutex<VecDeque<T>>>` is a candidate replacement
  for site B.
- `parking_lot::Condvar` is the equivalent of `std::sync::Condvar`
  but never allocates internally. A from-scratch `parking_lot`-based
  bounded MPSC would still need explicit sleep / wake plumbing,
  whereas `crossbeam_channel::bounded` provides that out of the box.

The relevant comparison for the migration plan is the row pair:

```text
Arc<Mutex<mpsc::Receiver>>  vs  crossbeam_channel::unbounded
```

For site B specifically, swapping to `crossbeam_channel::unbounded`
removes 2 atomics per recv on the fast path and the futex park on
mutex contention. The lock-free path also removes the priority
inversion where a worker holding the mutex during `recv` blocks all
other workers.

## 4. Profile plan

The recommendation in section 5 hinges on whether the static
per-message difference (single-digit atomics, one alloc on the box
backing) is observable end to end. The criterion benchmark below
gives that answer.

### 4.1 Criterion benches to land

A new criterion bench file `crates/engine/benches/channel_overhead.rs`
under the existing engine bench harness (compare with the
already-present `crates/engine/benches/drain_parallel_benchmark.rs`).
One bench file is enough - the benchmarks below are independent
groups inside it.

Each group exercises the same workload at four producer counts:
`N in {1, 4, 16, 64}` producers per single consumer. The consumer
drains until it has read `messages_per_run` items. `messages_per_run`
defaults to `1_000_000` divided across producers; criterion's
throughput mode reports messages/sec.

Groups:

1. `mpsc_unbounded` - `std::sync::mpsc::channel` (unbounded). Spawns
   `N` producer threads cloning the sender, one consumer.
2. `mpsc_bounded_8` - `std::sync::mpsc::sync_channel(8)`.
3. `mpsc_arc_mutex` - `mpsc::channel` plus
   `Arc<Mutex<Receiver>>` shared across `N` producers / consumers.
   Mirrors site B exactly.
4. `crossbeam_unbounded` - `crossbeam_channel::unbounded`.
5. `crossbeam_bounded_8` - `crossbeam_channel::bounded(8)`.
6. `parking_lot_mutex_deque` - `Arc<parking_lot::Mutex<VecDeque<T>>>`
   plus `Condvar` (proxy for "DIY" backend).

Payload type is `u64` (no allocation overhead in the bench). A second
sweep with payload `Vec<u8>` of 4 KiB simulates the
`SignatureResult` / `DeltaResult` path - this isolates the allocator
contribution from the synchronisation contribution.

Capacity sweep: bench `_bounded_8`, `_bounded_64`, `_bounded_256`
to cover the full / empty park rate at low and high capacities.

### 4.2 What we measure

Criterion's default `throughput(Throughput::Elements(messages_per_run))`
is enough. Track:

- mean and p99 messages / sec at each `N`,
- `criterion::Profiler`-driven `perf stat` counts of
  `task-clock`, `context-switches`, `cycles`, `instructions`,
  `cache-misses` (Linux only, gated behind a feature),
- on macOS, `dtrace`-driven `os_signpost` ranges around the bench
  body to capture mach IPC counts.

`crates/engine/Cargo.toml` already exposes a `criterion` dev-dep, so
the new bench file slots in next to `drain_parallel_benchmark.rs`.
No new dependency is required for the synthetic groups; the
`parking_lot` group adds `parking_lot = "0.12"` as a dev-dependency.

### 4.3 Workload realism

The synthetic bench above isolates the channel layer. Two end-to-end
checks anchor the result:

1. Real receiver, 100k 1 KiB files, parallel delta enabled. Compare
   wall-clock of the migration branch against master. The static
   estimate (~1 alloc / file at site A) translates into ~6 MiB of
   short-lived `Box<Node>` allocations - small but possibly visible
   on Linux glibc malloc.
2. Real receiver, single 4 GiB file, signature pipelining enabled.
   Site D fires at ~64k sends. This is the most send-dense path in
   the codebase outside of the existing SPSC ring at sites 1-3 of
   the static audit.

Both are existing harnesses (`scripts/benchmark.sh` and
`scripts/benchmark_hyperfine.sh`); no new infrastructure required.

### 4.4 Pass / fail criteria

Migrate site `X` if and only if:

- the `crossbeam` group at the producer count `X` exposes is at
  least 5% faster on the synthetic bench at `messages/sec`, OR
- the end-to-end harness shows a wall-clock improvement at the
  90th percentile that matches the static cost difference within
  one standard deviation.

Otherwise leave site `X` on `std::sync::mpsc`. The static cost
difference is small enough that profile data is required - the
default position is no change.

## 5. Migration recommendations

Recommendations are conditional on the criterion data above. They are
ordered by static expected gain.

### 5.1 Site B - swap `Arc<Mutex<mpsc::Receiver>>` for `crossbeam_channel::unbounded`

Highest-confidence migration. The `Arc<Mutex<Receiver>>` pattern at
`crates/signature/src/async_gen.rs:218` is exactly the pattern
crossbeam was designed to replace. Both static cost and concurrency
correctness improve:

- per recv: drop 2 atomics, drop the priority-inversion risk,
- code: drop 4 lines (`Arc::new`, `Mutex::new`, `Arc::clone`, the
  `lock().unwrap()` in `worker_thread_main_shared`).

Replace `request_sender: Sender<SignatureRequest>` with
`crossbeam_channel::Sender<SignatureRequest>`, and pass the
crossbeam `Receiver` (which is `Send + Clone`) directly to each
worker. No `Arc<Mutex<_>>` wrapper.

### 5.2 Site D - bound the pipelined block reader on `crossbeam_channel::bounded`

Second-highest priority on grounds of send rate. The unbounded
`std::sync::mpsc` at `pipelined/reader.rs:75` allocates `Box<Node>`
on every block. For a 1 GiB file at 64 KiB blocks that is ~16k
allocations the receiver immediately frees.

Replace with `crossbeam_channel::bounded(prefetch_depth)` where
`prefetch_depth` is a small constant (proposed: 4). Block reader
naturally backpressures on a slow consumer. Memory caps at
`prefetch_depth * block_size` (256 KiB at the default block size).

### 5.3 Site A - swap unbounded `std::sync::mpsc` for `crossbeam_channel::bounded`

The reorder buffer ahead of `consumer.rs:130` already serialises
the stream. `bounded(reorder_capacity)` is sufficient and removes
the per-file `Box<Node>` alloc. Estimated savings at 100k files:
100k allocs avoided. Static cost is small in absolute terms; the
migration is justified primarily because it makes site A consistent
with the bounded crossbeam channels above and below it in the
pipeline (sites 5 and 6 in the static audit).

### 5.4 Sites C, E, F - no change

- Site C (`SignatureResult` MPSC): low send rate (one per file),
  no `Mutex` wrapper, no shared receiver. Migration value below the
  bench noise floor.
- Site E: test only.
- Site F: `tokio::sync::mpsc`, out of `std` -> `crossbeam` scope.
  Tracked in the static audit's open question 5.

### 5.5 No `parking_lot` migration recommended

`parking_lot::Mutex` is faster than `std::sync::Mutex` only on
contended paths. The `Mutex` at site B is a channel-receiver guard,
not a shared data lock - replacing the channel with crossbeam removes
the lock altogether, which dominates any `parking_lot` win. There
are no other `Mutex<channel>` patterns on the receiver hot path
(grep at section 6).

## 6. Verification grep set

The full set of patterns audited:

```text
rg 'std::sync::mpsc|sync_channel|mpsc::channel|mpsc::sync_channel' crates/
rg 'Arc<\s*Mutex<\s*[A-Za-z_:]*Receiver' crates/
rg 'parking_lot' crates/
```

Results at the audited commit:

- ~38 matches across the workspace for `std::sync::mpsc` family.
- 1 match for `Arc<Mutex<...Receiver` outside tests
  (`crates/signature/src/async_gen.rs:218`).
- 0 production `parking_lot` usages.

Hot-path subset (after filtering out CLI orchestration, daemon
control plane, IOCP / io_uring control channels, SSH transport,
and `#[cfg(test)]`): 4 sites - A, B, C, D in section 2.

## 7. Open questions deferred to runtime

1. Does site D's `Box<Node>` rate matter on glibc's tcache? The
   freelist hot path may absorb 16k short-lived 24-byte allocations
   per GiB file without a measurable hit. 4.1 group `mpsc_unbounded`
   with `Vec<u8; 4096>` payload simulates this.
2. Is the `Arc<Mutex<Receiver>>` at site B ever contended in
   production? Async signature pre-computation defaults to
   `num_threads = 4`. With four workers blocking on `recv`, futex
   contention on the mutex is plausible but unverified.
3. Should site A's bound be `reorder_capacity` or
   `2 * rayon::current_num_threads()`? The reorder buffer guarantees
   in-order delivery but does not bound the channel ahead of itself;
   a tighter bound caps memory at the cost of producer stalls.
4. Should the `parking_lot_mutex_deque` group be replaced by an
   `arc_swap` lock-free queue once a mature crate is selected?
   Static cost would drop further but the maintenance trade-off
   needs a separate audit.

## 8. Companion work

- #1744 - `work_queue.rs` migration (completed; baseline for this
  document).
- #1745, #1746, #1747 - other crossbeam migrations completed in
  the same series.
- Static audit:
  `docs/audits/transfer-hot-path-channel-overhead-static.md` -
  full inventory including SPSC ring, tokio dispatch, and parallel
  delta sites.
- #1369 - SPSC contention metrics (completed; informs the `spin`
  vs `park` split in section 3).
