# ASY-7.a: Receiver tokio prototype design

Status: Design sketch. Tracks #2995. Prerequisite: ASY-6 (defer)
exit criteria met (ASY-4 bench >= 5% uplift, ASY-5 embeddability gap
confirmed). This document sketches the migration path for
`crates/transfer/src/receiver/` to a tokio-driven pipeline under the
`tokio-transfer` feature flag defined in ASY-2.

## 1. Scope

Convert the receiver's transfer loop
(`run_pipeline_loop_decoupled`) from the current threaded model
(SPSC spin channels + disk-commit OS thread + rayon signature batch)
to a tokio task graph. The prototype targets the receiver only; the
generator side is ASY-7.b.

Boundaries addressed (ASY-1/ASY-3 numbering):

| # | Boundary | Disposition |
|---|----------|-------------|
| 4 | Wire read (`reader.read` for delta tokens) | `.await` |
| 5 | Wire flush (gated on `flushed_pending == 0`) | `.await` |
| 6 | SPSC send (network -> disk) | `tokio::sync::mpsc` |
| 7 | SPSC recv (disk thread) | `tokio::sync::mpsc` |
| 8 | Signature batch (`find_basis_file_with_config`) | `spawn_blocking` |
| 9 | Disk commit (`process_file` write + fsync) | Long-lived `spawn_blocking` |
| 10 | io_uring `submit_and_wait` | Co-located inside #9 |

Out of scope: boundaries 1-3 (generator), 11 (daemon accept, already
async), 12 (SSH dissolve).

## 2. Current receiver architecture

```
                    ┌─────────────────────────────────────┐
                    │  run_pipeline_loop_decoupled         │
                    │                                     │
  network ────────►│  1. Collect batch from file_iter    │
  (sync Read)      │  2. rayon par_iter: signatures      │
                    │  3. Sequential send_file_request    │
                    │  4. writer.flush()                  │
                    │  5. process_file_response_streaming │
                    │  6. SPSC send -> disk commit thread │
                    │  7. drain_ready_results (try_recv)  │
                    └────────────────┬────────────────────┘
                                     │ SPSC channel
                    ┌────────────────▼────────────────────┐
                    │  disk_commit thread (OS thread)      │
                    │  - process_file: write, fsync, rename│
                    │  - io_uring ring (single owner)      │
                    │  - buf_return channel back to recv   │
                    └─────────────────────────────────────┘
```

Key properties:

- **Single-threaded pipeline loop.** The receiver loop is sequential:
  fill window, flush, read one response, hand to disk, repeat.
- **SPSC spin channels** (`pipeline/spsc.rs`): zero-syscall userspace
  spin between the pipeline loop and the disk-commit thread.
- **Rayon signature batch**: one `par_iter().collect()` per window fill,
  blocking the pipeline thread until all signatures complete.
- **Disk-commit thread**: one OS thread per connection, owns the
  io_uring ring, processes files in submission order.

## 3. Proposed tokio architecture

```
                    ┌─────────────────────────────────────────────┐
                    │  receiver_task (tokio::spawn)                │
                    │                                             │
  network ────────►│  1. Collect batch from file_iter            │
  (AsyncRead)      │  2. spawn_blocking: rayon par_iter sigs     │
                    │  3. Sequential write_frame().await          │
                    │  4. writer.flush().await                    │
                    │  5. read_response().await (token_loop)      │
                    │  6. file_tx.send(msg).await (mpsc)          │
                    │  7. result_rx.try_recv() / .recv().await    │
                    └───────────────────┬─────────────────────────┘
                                        │ tokio::sync::mpsc
                    ┌───────────────────▼─────────────────────────┐
                    │  disk_task (spawn_blocking + Handle::block_on)│
                    │  - async loop: file_rx.recv().await           │
                    │  - sync body: process_file, fsync, rename     │
                    │  - io_uring ring (single owner, same OS thrd) │
                    │  - buf_tx.send().await (mpsc back-channel)    │
                    └──────────────────────────────────────────────┘
```

### 3.1 Per-file futures vs OS threads

**Decision: one tokio task per connection, not per file.**

Rationale:

- Wire protocol requires NDX requests in file-list order.
  Per-file futures would need a sequencer to serialize writes,
  adding complexity for no throughput gain (the wire is serial).
- The existing pipeline fills a sliding window and processes one
  response at a time. This maps naturally to a single async loop
  that yields at I/O points.
- Per-file parallelism stays in rayon (signature computation) and
  the `ParallelDeltaApplier` (chunk verify). These are CPU-bound
  and belong in `spawn_blocking` / rayon, not tokio tasks.

### 3.2 Token loop polling shape

The current `process_file_response_streaming` reads delta tokens
from the wire in a tight loop:

```rust
// current (sync)
loop {
    let token = token_reader.read_token(&mut reader)?;
    match token {
        Token::Data(buf) => { /* accumulate */ }
        Token::Block(offset, len) => { /* copy from basis */ }
        Token::End => break,
    }
}
```

Under tokio, this becomes:

```rust
// proposed (async, cfg(feature = "tokio-transfer"))
loop {
    let token = token_reader.read_token_async(&mut reader).await?;
    match token {
        Token::Data(buf) => { /* accumulate */ }
        Token::Block(offset, len) => { /* copy from basis */ }
        Token::End => break,
    }
}
```

The `read_token_async` method wraps the existing codec with
`AsyncRead` polling. Each `.await` yields the task when the
socket has no data, letting other connections (daemon mode) or the
disk-commit feedback channel make progress.

**Critical invariant**: the token loop must not yield between
partial token reads. The multiplex frame parser maintains internal
state; yielding mid-frame would require the parser to be
restartable. Solution: use `tokio::io::BufReader` with a large
enough buffer (128 KB, matching today's `BUF_SIZE`) so most
tokens resolve from the buffer without hitting `.await`. When the
buffer is exhausted, the underlying `poll_read` fills it in one
shot from the kernel socket buffer.

### 3.3 Channel choices

| Channel | Current | Proposed (tokio path) | Rationale |
|---------|---------|----------------------|-----------|
| Network -> disk (file messages) | SPSC spin (`spsc::Sender`) | `tokio::sync::mpsc` (bounded, cap=128) | Async backpressure; no spin waste when disk is slow |
| Disk -> receiver (commit results) | SPSC spin (`spsc::Receiver`) | `tokio::sync::mpsc` (bounded, cap=128) | Receiver can `.await` results without busy-polling |
| Disk -> receiver (buf recycle) | SPSC spin | `tokio::sync::mpsc` (bounded, cap=64) | Same reasoning |
| DeltaConsumer stream | `crossbeam_channel` (bounded) | Keep `crossbeam_channel` | Runs inside `spawn_blocking`; sync is fine |
| ParallelDeltaApplier dispatch | rayon + DashMap | Keep rayon + DashMap | CPU-bound; lives inside `spawn_blocking` scope |

**Why `tokio::sync::mpsc` over `crossbeam_channel`:**

- `tokio::sync::mpsc::Sender::send().await` yields the task on
  backpressure instead of spinning or blocking an OS thread.
- The receiver task can `tokio::select!` over wire reads and
  result-channel receives, enabling true bidirectional async I/O
  (mirroring upstream's `select()` in `io.c:perform_io()`).
- The SPSC ring (`pipeline/spsc.rs`) stays compiled for the
  threaded path; the swap is `#[cfg(feature = "tokio-transfer")]`.

**Why keep crossbeam inside `spawn_blocking`:**

- The `DeltaConsumer` and `ParallelDeltaApplier` are CPU-bound
  rayon workloads. They run inside a `spawn_blocking` island.
  Adding tokio channels inside rayon workers would require a
  `Handle` and `block_on` at every send, which is worse than
  sync channels.

## 4. ParallelDeltaApplier interaction

The `ParallelDeltaApplier` in `crates/engine/src/concurrent_delta/`
is the parallel chunk-verify + ordered-write engine. Under the
tokio prototype, it continues to operate synchronously inside the
disk-commit `spawn_blocking` task.

### 4.1 Current flow

```
disk_commit thread
  └─► for each FileMessage:
        register_file(ndx, writer)
        for each chunk:
          apply_one_chunk(chunk)  // rayon::join(verify, || ())
        finish_file(ndx)         // flush_workers barrier + Arc::try_unwrap
```

### 4.2 Proposed flow (tokio path)

```
disk_task (spawn_blocking + Handle::block_on)
  └─► async loop:
        let msg = file_rx.recv().await;
        // Now we are inside spawn_blocking, so sync code is fine:
        applier.register_file(ndx, writer);
        for chunk in msg.chunks:
            applier.apply_one_chunk(chunk);  // rayon as before
        applier.finish_file(ndx);
```

**No change to `ParallelDeltaApplier` internals.** The applier's
`DashMap`, per-file `Mutex`, `SlotBarrier`/`DecrementGuard`, and
rayon dispatch all remain synchronous. They live inside the
`spawn_blocking` OS thread and never cross an `.await` boundary.

The only change is how chunks arrive: via `tokio::sync::mpsc`
instead of SPSC spin. The disk task uses `Handle::block_on` to
drive the async recv loop from within the blocking thread, as
specified in ASY-3 section 2.9.

### 4.3 Concurrency interaction

The `ParallelDeltaApplier` uses rayon's ambient thread pool for
the verify step (`rayon::join`). Under tokio, the rayon pool is
shared with signature computation (boundary #8). This is already
the case today - both run on the same rayon pool. No contention
change.

## 5. flush_workers / drain barrier in async context

### 5.1 Problem statement

`ParallelDeltaApplier::flush_workers(ndx)` parks the calling
thread on a `Condvar::wait_while` until the per-file in-flight
counter reaches zero. In the current model, this blocks the
disk-commit OS thread - acceptable because nothing else needs
that thread until the flush completes.

Under tokio, the disk-commit task runs inside `spawn_blocking`.
The same blocking wait is acceptable here because
`spawn_blocking` threads are allowed to block (that is their
purpose). The tokio worker pool is not starved.

### 5.2 Design: no change needed

The `flush_workers` barrier stays as-is:

```rust
// Inside spawn_blocking disk task:
applier.flush_workers(ndx)?;  // blocks OS thread, fine
let writer = applier.finish_file(ndx)?;
```

This works because:

1. The disk task is a `spawn_blocking` task with its own OS
   thread. Blocking it does not block any tokio worker.
2. The rayon workers that decrement the in-flight counter run on
   rayon's thread pool, which is independent of tokio's blocking
   pool.
3. The `Condvar` notify from `DecrementGuard::drop` wakes the
   disk task's OS thread directly.

### 5.3 Alternative considered: async barrier

An async alternative would replace the `Condvar` with a
`tokio::sync::Notify`:

```rust
// Hypothetical - NOT proposed for prototype
async fn flush_workers_async(&self, ndx: FileNdx) {
    let notify = self.get_notify(ndx);
    while self.inflight(ndx) > 0 {
        notify.notified().await;
    }
}
```

**Rejected for the prototype** because:

- It would require `ParallelDeltaApplier` to depend on tokio,
  violating the rule that `engine` stays sync (ASY-2 section 2.2).
- The `DecrementGuard` drop path (rayon worker) would need a
  tokio `Handle` to call `notify.notify_one()`, coupling rayon
  teardown to tokio runtime availability.
- The current Condvar approach is correct and efficient inside
  `spawn_blocking`. No benefit from making it async.

### 5.4 drain_inflight in shutdown

`ParallelDeltaApplier::drain_inflight()` iterates all registered
files and calls `flush_workers` for each. Under the tokio path,
this still runs inside the disk task's `spawn_blocking` thread.
On cancellation (connection drop, SIGINT):

1. The receiver task drops `file_tx` (mpsc sender half).
2. The disk task observes `file_rx.recv().await -> None`.
3. The disk task calls `applier.drain_inflight()` (blocking but
   bounded - rayon workers are CPU-bound, not I/O-waiting).
4. The disk task returns from `spawn_blocking`.
5. The receiver task's `JoinHandle<()>` resolves.

## 6. io_uring integration: tokio-uring vs current direct path

### 6.1 Current state

The io_uring ring in `fast_io` is synchronous:
`submit_and_wait(n)` blocks until `n` CQEs complete. It lives
inside the disk-commit OS thread (single owner). Under the tokio
prototype, nothing changes: the ring still lives inside the
`spawn_blocking` disk task, on the same OS thread for the
connection's lifetime.

### 6.2 tokio-uring option (ASY-9, deferred)

`tokio-uring` provides a tokio-native event loop that polls
io_uring CQEs as readiness events. It would allow the disk task
to `.await` completions directly without blocking:

```rust
// Hypothetical tokio-uring path (ASY-9)
async fn write_file(ring: &IoUring, fd: RawFd, data: &[u8]) {
    ring.write(fd, data, 0).await.unwrap();
}
```

**Deferred to ASY-9** for these reasons:

1. `tokio-uring` requires a `current_thread` runtime per ring.
   The daemon uses `multi_thread`. Composition requires one
   `current_thread` runtime per disk task inside
   `spawn_blocking`, adding nesting complexity.
2. The shared-ring bottleneck
   (`project_io_uring_shared_ring_bottleneck.md`) means moving to
   per-thread rings first (IUR-3), then building async on top.
3. Measured benefit at current bench scale is marginal
   (`project_iouring_marginal_at_small_bench_scale.md`). The win
   is expected only at multi-GB / high-IOPS workloads that
   ASY-4's bench has not yet characterized.

### 6.3 Prototype path

For ASY-7.a, io_uring stays synchronous inside `spawn_blocking`.
The architecture cleanly allows a future swap to `tokio-uring`
inside the disk task without affecting the receiver task or
channel topology.

## 7. Wire-byte parity

The tokio path must produce byte-identical wire output (ASY-2
section 7). The receiver's wire writes are:

1. NDX request codec (`MonotonicNdxWriter::write_ndx`)
2. Signature blocks (`write_signature_blocks`)
3. `writer.flush()`

All three are deterministic, single-threaded (the receiver task),
and order-preserving. The async path calls the same codec
functions; only the I/O transport changes (sync `Write` ->
`AsyncWrite`). The prototype must include a capture-replay
assertion (ASY-5 harness) confirming no frame reordering.

## 8. Cancellation and graceful shutdown

```
CancellationToken (shared across receiver_task + disk_task)
  │
  ├─► receiver_task: select! { token_read.await, cancel.cancelled() }
  │     on cancel: drop file_tx, stop reading wire
  │
  └─► disk_task: loop { select! { file_rx.recv(), cancel.cancelled() } }
        on cancel: drain_inflight(), flush ring, close channels, return
```

Partial writes are safe because the disk-commit path uses
temp-file + atomic rename. A cancelled transfer leaves the
destination unchanged (only fully-committed renames are visible).

## 9. Migration path (phased)

### Phase 1: Channel swap (ASY-7.a.1)

- Add `tokio::sync::mpsc` channels behind `#[cfg(feature = "tokio-transfer")]`.
- Keep the pipeline loop synchronous but swap SPSC for mpsc.
- Disk-commit thread becomes `spawn_blocking` + `Handle::block_on`.
- Validates: channel semantics, backpressure, shutdown.

### Phase 2: Wire I/O async (ASY-7.a.2)

- Wrap `ServerReader<R>` with `AsyncRead` shim (boundaries #4, #5).
- The pipeline loop becomes `async fn`.
- `tokio::select!` for bidirectional I/O (read response vs flush).
- Validates: token loop shape, wire-byte parity, latency.

### Phase 3: Signature batch bridging (ASY-7.a.3)

- `spawn_blocking` around the rayon `par_iter` batch (boundary #8).
- The receiver task `.await`s the batch result.
- Validates: rayon/tokio composition, pool sizing.

### Phase 4: Integration + bench (ASY-4 overlap)

- Full end-to-end receiver running on tokio.
- Wire-byte parity golden tests.
- `rsync-profile` 100k-file benchmark comparison.
- Validates: ASY-2 section 10 #5 floor (>= 5% uplift).

## 10. Open questions

1. **`token_reader` statefulness.** The zstd decompression context
   (`DCtx`) persists across file boundaries. Under async, if the
   token loop yields mid-decompression, the `DCtx` state must be
   preserved across `.await` points. Since the receiver task is
   single-threaded (one task per connection), the `DCtx` lives in
   the task's local state and is never shared - no issue.

2. **Rayon pool sharing.** Both signature computation (#8) and
   `ParallelDeltaApplier::apply_one_chunk` use the ambient rayon
   pool. Under tokio, both callers are inside `spawn_blocking`.
   If many connections compete for the rayon pool, latency spikes.
   Mitigation: per-connection rayon scope with bounded parallelism
   (already enforced by `ParallelDeltaApplier::concurrency`).

3. **`PipelinedReceiver` lifetime.** The current `PipelinedReceiver`
   owns the disk-commit thread's join handle and drops it on
   `shutdown()`. Under tokio, it owns a `JoinHandle<()>` from
   `spawn_blocking`. The shutdown sequence must `.await` the join
   handle, requiring the pipeline loop to be async.

4. **Daemon multi-connection scaling.** Under `async-daemon` +
   `tokio-transfer`, each connection spawns one receiver task +
   one `spawn_blocking` disk task. With 1000 connections, the
   blocking pool needs 1000 threads (one per disk task, long-lived).
   This matches the current model (one OS thread per connection)
   but exercises tokio's blocking pool differently. Pool sizing
   guidance: `max_blocking_threads >= max_connections + rayon_threads`.

## 11. Cross-references

- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag design.
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contract.
- `docs/design/asy-6-adopt-or-defer-decision.md` - defer gate.
- `docs/design/async-io-uring-impact.md` - io_uring composition.
- `docs/design/parallel-receive-delta-application.md` - PDA design.
- `crates/transfer/src/pipeline/async_pipeline.rs` - existing async
  scaffold (producer/consumer pattern with `CancellationToken`).
- `crates/transfer/src/pipeline/spsc.rs` - SPSC ring to be swapped.
- `crates/engine/src/concurrent_delta/parallel_apply/` - PDA internals.
- `project_no_async_threaded_only.md` - standing constraint.
- `project_io_uring_shared_ring_bottleneck.md` - ring composition.
- `project_russh_spawn_blocking_ceiling.md` - blocking pool scaling.
