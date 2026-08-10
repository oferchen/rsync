# ASY-7.b: Receiver tokio implementation plan

Status: Implementation plan. Tracks #2996. Builds on ASY-7.a
(`docs/design/receiver-tokio-prototype.md`) which defined the target
architecture. This document specifies the concrete implementation
phases, file changes, feature-flag wiring, testing strategy, and
rollback plan.

## 1. Implementation phases

The conversion is split into three phases that can each land as a
separate PR. Each phase compiles and tests independently - no phase
leaves the build broken.

### Phase 1: Async skeleton loop (ASY-7.b.1)

**Goal:** Compile an async receiver transfer loop that delegates to the
existing sync internals via `spawn_blocking`. Validates the task
topology and channel wiring without changing any protocol logic.

Steps:

1. Add `async fn run_pipeline_loop_async` in a new file
   `crates/transfer/src/receiver/transfer/async_loop.rs`, gated behind
   `#[cfg(feature = "tokio-transfer")]`.
2. The async loop reads file candidates from a `Vec` (same input as
   `run_pipeline_loop_decoupled`), dispatches each file's signature
   computation + request + response processing into a single
   `spawn_blocking` call, and collects results via
   `tokio::sync::mpsc`.
3. Wire reads and writes stay synchronous inside `spawn_blocking` in
   this phase - the loop structure is async but I/O is bridged.
4. The disk-commit thread is replaced by a long-lived
   `spawn_blocking` task that owns its own `tokio::sync::mpsc::Receiver`
   for `FileMessage` items and sends `CommitResult` back through a
   second mpsc channel.
5. Shutdown: dropping the `file_tx` sender half signals the disk task
   to drain and exit.

Deliverables:
- `async_loop.rs` - async skeleton with `spawn_blocking` islands.
- `async_disk_task.rs` - long-lived blocking task replacing the OS
  thread in `disk_commit/thread.rs`.
- Unit tests confirming channel lifecycle and graceful shutdown.

### Phase 2: Wire I/O migration to `.await` (ASY-7.b.2)

**Goal:** Replace synchronous wire reads/writes with async I/O so the
receiver task yields to the tokio scheduler at network boundaries.

Steps:

1. Introduce `AsyncServerReader<R: AsyncRead>` wrapper that implements
   the same frame-parsing logic as `ServerReader<R: Read>` but polls
   via `AsyncRead`. Lives in
   `crates/transfer/src/reader/async_reader.rs`.
2. Introduce `AsyncMsgWriter<W: AsyncWrite>` for multiplex frame
   writes. Lives in `crates/transfer/src/writer/async_writer.rs`.
3. Convert `read_ndx`, `read_sender_attrs`, `write_signature_blocks`
   to async variants gated behind `#[cfg(feature = "tokio-transfer")]`.
4. The token loop (`process_file_response_streaming`) becomes an async
   function that `.await`s each `read_token_async` call. The existing
   `TokenReader` codec is wrapped with an `AsyncTokenReader` adapter
   that uses `tokio::io::BufReader` (128 KB buffer).
5. `tokio::select!` in the main loop enables bidirectional progress:
   reading the next response while the disk task processes the
   previous file.

Deliverables:
- `async_reader.rs`, `async_writer.rs` - async transport wrappers.
- `async_token_reader.rs` - async delta-token parser.
- Wire-byte parity assertion: capture-replay test comparing sync vs
  async output for a fixed transfer scenario.

### Phase 3: Channel migration and full integration (ASY-7.b.3)

**Goal:** Remove the `spawn_blocking` bridge for wire I/O (installed
in phase 1 as scaffolding) so the receiver task is fully async. Swap
SPSC spin channels for `tokio::sync::mpsc` on the async path.

Steps:

1. The receiver task's main loop now calls async wire I/O directly
   (from phase 2) instead of delegating to `spawn_blocking`.
2. Signature batch computation stays in `spawn_blocking` (CPU-bound
   rayon work, boundary #8 from ASY-3).
3. The SPSC channel (`pipeline/spsc.rs`) remains compiled for the
   sync path. Under `tokio-transfer`, the async loop uses
   `tokio::sync::mpsc` with capacity 128 for network-to-disk
   communication and capacity 128 for result/buffer-return channels.
4. Integration with `core::session()`: the async path is selected by
   `CoreConfig` when `tokio-transfer` is enabled and the tokio runtime
   is available (detected via `tokio::runtime::Handle::try_current()`).
5. End-to-end validation with the full interop suite.

Deliverables:
- Wired `run_pipeline_loop_async` callable from `core::session()`.
- Feature-gated dispatch in `ReceiverContext::run`.
- Full interop suite passing under `tokio-transfer`.

## 2. Feature flag wiring

### 2.1 Cargo.toml changes

The `tokio-transfer` feature is defined per ASY-2's design. Files
touched:

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Add `tokio-transfer` forwarding to `core`, `transfer`, `daemon` |
| `crates/transfer/Cargo.toml` | Add `tokio-transfer = ["async", "dep:tokio", "dep:tokio-util"]` |
| `crates/core/Cargo.toml` | Add `tokio-transfer = ["transfer/tokio-transfer", "dep:tokio"]` |
| `crates/daemon/Cargo.toml` | Add `tokio-transfer = ["async-daemon", "core/tokio-transfer"]` |

### 2.2 Conditional compilation pattern

```rust
// crates/transfer/src/receiver/transfer/mod.rs
#[cfg(feature = "tokio-transfer")]
mod async_loop;

impl ReceiverContext {
    pub fn run<R: Read, W: Write + MsgInfoSender + ?Sized>(
        &mut self,
        reader: ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        // Sync path - always compiled, used when tokio-transfer is off
        // or when no tokio runtime is current.
        self.run_pipelined(reader, writer)
    }

    #[cfg(feature = "tokio-transfer")]
    pub async fn run_async<R: AsyncRead + Unpin + Send, W: AsyncWrite + Unpin + Send>(
        &mut self,
        reader: AsyncServerReader<R>,
        writer: &mut AsyncMsgWriter<W>,
    ) -> io::Result<TransferStats> {
        self.run_pipeline_loop_async(reader, writer).await
    }
}
```

### 2.3 Runtime selection in core::session

```rust
// crates/core/src/session.rs (sketch, cfg-gated)
#[cfg(feature = "tokio-transfer")]
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return handle.block_on(receiver.run_async(async_reader, &mut async_writer));
    }
}
// Fallback: sync path
receiver.run(reader, &mut writer)
```

This ensures the sync path is always available as fallback and the
async path activates only when both the feature is compiled in and a
tokio runtime is driving the call.

## 3. Files to modify or create

### 3.1 New files

| Path | Role |
|------|------|
| `crates/transfer/src/receiver/transfer/async_loop.rs` | Async pipeline loop (main receiver task) |
| `crates/transfer/src/disk_commit/async_task.rs` | Long-lived `spawn_blocking` disk task with mpsc recv |
| `crates/transfer/src/reader/async_reader.rs` | `AsyncServerReader` - async multiplex frame reader |
| `crates/transfer/src/writer/async_writer.rs` | `AsyncMsgWriter` - async multiplex frame writer |
| `crates/transfer/src/token_reader/async_token_reader.rs` | Async delta-token parser adapter |

### 3.2 Modified files

| Path | Change |
|------|--------|
| `crates/transfer/Cargo.toml` | `tokio-transfer` feature definition |
| `crates/transfer/src/receiver/transfer/mod.rs` | Conditional `mod async_loop` |
| `crates/transfer/src/receiver/mod.rs` | `run_async` method on `ReceiverContext` |
| `crates/transfer/src/disk_commit/mod.rs` | Re-export `async_task` module |
| `crates/transfer/src/reader/mod.rs` | Conditional `mod async_reader` |
| `crates/transfer/src/writer/mod.rs` | Conditional `mod async_writer` |
| `crates/transfer/src/token_reader/mod.rs` | Conditional `mod async_token_reader` |
| `crates/core/Cargo.toml` | `tokio-transfer` feature forwarding |
| `crates/core/src/session.rs` | Runtime selection dispatch |
| `crates/daemon/Cargo.toml` | `tokio-transfer` feature forwarding |
| `Cargo.toml` (workspace) | Workspace-level feature forwarding |

### 3.3 Unchanged files (intentionally)

| Path | Reason |
|------|--------|
| `crates/transfer/src/pipeline/spsc.rs` | Stays compiled for sync path; no code changes |
| `crates/transfer/src/disk_commit/thread.rs` | Sync disk thread unchanged; async path uses `async_task.rs` |
| `crates/transfer/src/disk_commit/process.rs` | Disk commit logic shared by both sync and async paths |
| `crates/engine/src/concurrent_delta/` | `ParallelDeltaApplier` stays sync, called from inside `spawn_blocking` |
| `crates/protocol/` | Wire codec stays sync; async wrappers live in `transfer` |

## 4. Integration with parallel-receive-delta (PIP-9)

The `ParallelDeltaApplier` (rayon-based chunk verification and ordered
writes) operates entirely within the disk-commit execution context.
Under the tokio path:

### 4.1 Execution model

```
receiver_task (tokio::spawn)
  |
  | tokio::sync::mpsc (cap=128)
  v
disk_task (spawn_blocking, long-lived)
  |
  +-- ParallelDeltaApplier::register_file(ndx, writer)
  +-- for each chunk: apply_one_chunk(chunk)  // rayon::join
  +-- ParallelDeltaApplier::finish_file(ndx)  // Condvar wait
  |
  | tokio::sync::mpsc (cap=128)
  v
receiver_task (collects CommitResult)
```

### 4.2 Key invariants preserved

1. **Rayon pool stays ambient.** The rayon thread pool is shared
   between signature batch (`spawn_blocking` boundary #8) and
   `ParallelDeltaApplier::apply_one_chunk`. Both callers are inside
   `spawn_blocking` - no change from the current model.

2. **Condvar wait inside spawn_blocking.** The `flush_workers` barrier
   blocks the disk task's OS thread. This is correct because
   `spawn_blocking` threads are allowed to block. The tokio worker pool
   is not starved.

3. **DashMap access is sync.** The `ApplierState` DashMap is only
   accessed from the disk task thread and rayon workers. No `.await`
   crosses the DashMap boundary.

4. **Buffer recycling.** The disk task sends exhausted `Vec<u8>`
   buffers back via the result mpsc channel (piggy-backed on
   `CommitResult`). The receiver task recycles them into the next
   `FileMessage`. This replaces the current `buf_return_rx` SPSC with
   a field on `CommitResult`.

### 4.3 Concurrency budget

The rayon pool is configured with `rayon::ThreadPoolBuilder` at
process start (default: num_cpus). Under tokio, both the receiver's
signature batches and the disk task's chunk verification draw from
this pool. Since only one batch and one chunk-verify can be active per
connection (serial pipeline), the pool contention is unchanged from
the threaded model.

For daemon mode with N concurrent connections, the blocking pool
needs N threads (one per disk task) plus headroom for signature
batches. Pool sizing: `max_blocking_threads >= max_connections + 8`.

## 5. Testing strategy

### 5.1 Wire-byte parity (golden tests)

The `crates/protocol/tests/golden/` directory contains byte-exact
captures of wire frames. Each golden test runs under both feature
configurations:

```rust
#[test]
fn golden_receiver_wire_parity() {
    // Runs the same transfer scenario through:
    // 1. Sync path (feature off or runtime unavailable)
    // 2. Async path (feature on + tokio runtime)
    // Asserts byte-identical wire output.
}
```

New golden tests added for:
- Single file delta transfer (small file, single block).
- Multi-file batch with phase 1 + phase 2 redo.
- INC_RECURSE multi-segment transfer.
- Whole-file transfer (no basis).
- Compressed transfer (zlib, zstd).

### 5.2 Interop suite

`tools/ci/run_interop.sh` exercises the full interop matrix (upstream
3.0.9, 3.1.3, 3.4.1, 3.4.2). The CI workflow gains a new matrix axis:

```yaml
strategy:
  matrix:
    features: ["default", "tokio-transfer"]
```

Both feature sets must produce identical interop results. This
validates that the async path is wire-compatible with all supported
upstream versions.

### 5.3 Performance regression gate

The `rsync-profile` benchmark container provides the regression gate:

| Metric | Threshold | Action on breach |
|--------|-----------|------------------|
| Throughput (files/sec) | >= 95% of sync path | Block merge |
| Peak RSS | <= 110% of sync path | Block merge |
| P99 latency per file | <= 120% of sync path | Warning, investigate |
| Total transfer time (100k files) | <= 105% of sync path | Block merge |

Benchmark runs compare `--features default` vs
`--features default,tokio-transfer` on the same corpus.

### 5.4 Unit and integration tests

- **Channel lifecycle:** Verify graceful shutdown when sender drops,
  when disk task panics, when cancellation token fires.
- **Backpressure:** Verify that a slow disk task causes the receiver
  to park on `send().await` (not spin).
- **Cancellation:** Verify that mid-transfer cancellation leaves no
  partially-committed files (temp-file + rename atomicity).
- **Redo mechanism:** Verify phase 2 redo works through the async
  pipeline (checksum mismatch queues file for retransmit).
- **spawn_blocking pool exhaustion:** Verify that exceeding
  `max_blocking_threads` degrades gracefully (backpressure, not panic).

### 5.5 CI matrix addition

A new CI job compiles and tests with `tokio-transfer` enabled:

```yaml
- name: nextest (tokio-transfer)
  features: "default,tokio-transfer"
  runs-on: ubuntu-latest
```

This catches compilation errors from cfg-gate mismatches and ensures
the async path is exercised in CI from phase 1 onward.

## 6. Migration risks and rollback plan

### 6.1 Risk matrix

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Wire-byte divergence between sync/async paths | Medium | Critical (interop failure) | Golden tests + interop CI on every PR |
| `spawn_blocking` pool starvation under high connection count | Low | High (daemon throughput collapse) | Configurable pool sizing; documented in operator guide |
| Rayon + tokio blocking pool interaction causing deadlock | Low | Critical (hang) | Rayon pool is independent; no `block_in_place` inside rayon |
| Token loop yields mid-frame causing parser corruption | Medium | Critical (data corruption) | `BufReader` with 128 KB buffer; parser state never crosses `.await` |
| Performance regression from task scheduling overhead | Medium | Medium (unacceptable for default-on) | Feature stays off until ASY-12 gate passes |
| `ParallelDeltaApplier` Condvar deadlock under tokio | Low | Critical | Condvar is inside `spawn_blocking` - same semantics as OS thread |
| Compilation time increase from tokio dep in transfer crate | Certain | Low | Already optional dep via `async` feature; `tokio-transfer` implies it |

### 6.2 Rollback plan

The `tokio-transfer` feature is additive and default-off. Rollback at
any phase:

1. **Compile-time:** Remove `--features tokio-transfer` from the
   build. All async code is `#[cfg]`-gated and compiles out entirely.
   Zero runtime cost.

2. **Runtime (if feature enabled but broken):** The
   `Handle::try_current()` check in `core::session()` falls back to
   the sync path when no tokio runtime is present. For emergency
   rollback without recompilation, callers can avoid entering a tokio
   runtime.

3. **Code-level:** Each phase's new files are isolated modules. To
   revert a phase: `git revert` the phase PR. No existing sync-path
   code is modified - only new `#[cfg(feature = "tokio-transfer")]`
   modules are added.

### 6.3 Compatibility guarantee

The sync path (`run_pipelined`, `run_pipeline_loop_decoupled`,
`spawn_disk_thread`) is never modified. It remains the production
path until ASY-12 flips the default. Both paths are compiled and
tested in CI simultaneously.

## 7. Estimated effort and dependencies

### 7.1 Per-phase effort

| Phase | Estimated effort | Blocking dependencies |
|-------|------------------|-----------------------|
| Phase 1 (skeleton) | 3-4 days | ASY-2 feature flag landed |
| Phase 2 (wire async) | 5-7 days | Phase 1 merged |
| Phase 3 (integration) | 3-4 days | Phase 2 merged |
| Golden tests + interop | 2-3 days | Phase 3 merged |
| Benchmark tuning | 2-3 days | Golden tests passing |
| **Total** | **15-21 days** | |

### 7.2 ASY task dependencies

| Dependency | Status | Blocking? |
|------------|--------|-----------|
| ASY-2 (`tokio-transfer` feature flag) | Design complete | Yes - feature must exist in Cargo.toml |
| ASY-3 (per-boundary disposition) | Design complete | No - informs design but no code dep |
| ASY-5 (embeddability harness) | Design complete | No - capture-replay tests use the harness but it can be stubbed |
| ASY-6 (adopt-or-defer gate) | Defer gate passed | No - decision is made |
| ASY-7.a (receiver prototype design) | Merged | No - this is the implementation of that design |
| ASY-8.a (sender prototype) | Design complete | No - sender is independent |

### 7.3 External dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `tokio` | 1.x (workspace) | Runtime, spawn_blocking, mpsc channels |
| `tokio-util` | 0.7.x (workspace) | `CancellationToken`, codec utilities |

Both are already workspace dependencies used by the `async` feature
and the daemon's async accept loop. No new external dependencies are
introduced.

## 8. Success criteria

The implementation is complete when:

1. `cargo build --features tokio-transfer` compiles on all three
   platforms (Linux, macOS, Windows).
2. `cargo nextest run --features default,tokio-transfer` passes the
   full test suite.
3. Golden wire-byte parity tests confirm bit-identical output between
   sync and async paths.
4. `tools/ci/run_interop.sh` passes with `tokio-transfer` enabled
   against all supported upstream versions.
5. Benchmark regression gate (section 5.3) passes on the
   `rsync-profile` 100k-file corpus.
6. Code review confirms no `unsafe`, no `unwrap` on fallible paths,
   no new clippy warnings.

## 9. Cross-references

- `docs/design/receiver-tokio-prototype.md` - ASY-7.a architecture.
- `docs/design/sender-tokio-prototype.md` - ASY-8.a companion design.
- `docs/design/asy-2-tokio-runtime-feature.md` - Feature flag spec.
- `docs/design/asy-3-async-boundary-spec.md` - Per-boundary contracts.
- `docs/design/asy-6-adopt-or-defer-decision.md` - Defer gate.
- `crates/transfer/src/pipeline/async_pipeline.rs` - Existing async
  scaffold (producer/consumer pattern with `CancellationToken`).
- `crates/transfer/src/pipeline/spsc.rs` - SPSC ring kept for sync.
- `crates/transfer/src/disk_commit/thread.rs` - Sync disk thread.
- `crates/engine/src/concurrent_delta/parallel_apply/` - PDA internals.
