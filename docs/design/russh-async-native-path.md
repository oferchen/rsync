# Async-Native russh Path - Design Spec

**Tracking:** RUSSH-9 (#2812)
**Status:** Design only. Implementation lands under RUSSH-11 (#2814). Back-compat shim under RUSSH-10 (#2813). Wire-byte parity under RUSSH-12 (#2815). Re-bench under RUSSH-13. Adopt/defer decision under RUSSH-14.
**Scope:** Replaces the `tokio::task::spawn_blocking` bridge at the russh boundary with a direct async-task dispatch path. The synchronous transfer pipeline (token loop, generator, sender, receiver) stays threaded. Only the bridge between the synchronous transfer pipeline and the russh I/O surface is reshaped.

Cross-links: [[project_russh_spawn_blocking_ceiling]], [[project_no_async_threaded_only]], [[project_ssh_push_russh_v062]], [[project_ssh_stderr_socketpair_silent_fallback]].

## 1. Problem Statement

Each russh session today uses `tokio::task::spawn_blocking` to bridge the sync transfer pipeline into russh's async I/O. Each `spawn_blocking` grabs a tokio blocking-pool thread for the lifetime of the transfer. At 512 concurrent sessions: 512 blocking threads (plus tokio I/O reactor threads), exhausting the default 512-thread blocking pool ceiling on Linux.

The exact call sites are inventoried in [`docs/audit/russh-spawn-blocking-ceiling-inventory.md`](../audit/russh-spawn-blocking-ceiling-inventory.md) (RUSSH-1, #2804). The relevant production sites on the russh boundary are:

| Site | Closure | Per-session cost |
|------|---------|------------------|
| `crates/core/src/client/remote/async_ssh_transport.rs:349` | `writer_fanin`: drain sync `std_mpsc::Receiver` and feed `tokio_mpsc::Sender` via `blocking_send`. | 1 long-lived blocking-pool slot per session. |
| `crates/core/src/client/remote/async_ssh_transport.rs:361` | `server_handle`: runs `run_blocking_server` (handshake, file list, delta apply, finalize). | 1 long-lived blocking-pool slot per session, full session lifetime. |
| `crates/engine/src/async_io/copier.rs:184` | Per-file metadata application (`set_permissions`, `set_file_mtime`). | 0..N transient blocking-pool slots per session. |

The two long-lived slots per session put the hard ceiling at roughly `max_blocking_threads / 2` concurrent SSH sessions per process before `spawn_blocking` queues. Tokio's default `max_blocking_threads = 512` gives a ceiling near 256 concurrent sessions per process. Per-file metadata bursts erode the headroom further.

The bottleneck is not CPU, memory, or socket count - it is the size of a fixed-policy blocking thread pool that the async runtime applies indiscriminately to every `spawn_blocking` callsite in the process.

## 2. Replacement Architecture

The async-native path replaces the `spawn_blocking` bridge with a direct async-task dispatch model. The transfer pipeline itself stays threaded (`[[project_no_async_threaded_only]]` - we do not async-rewrite the protocol engine). Only the boundary between the sync pipeline and the russh I/O surface becomes async-native.

### Pre-state (today)

```
                        sync transfer thread (OS thread, blocking-pool slot #1)
                        |
                        | std::sync::mpsc + spawn_blocking pump
                        |
        +-------------- writer_fanin (blocking-pool slot #2) -----+
        |                                                        |
        v                                                        v
  tokio::mpsc::Sender                                       tokio::mpsc::Receiver
        |                                                        |
        v                                                        v
  outbound pump task                                         inbound pump task
  (tokio task)                                               (tokio task)
        |                                                        ^
        v                                                        |
  russh Channel write half                              russh Channel read half
```

Each session reserves three blocking-pool slots in the worst case (server thread, writer fan-in, occasional per-file metadata). At 512 sessions concurrent, the pool is exhausted before any single session has issued its first byte.

### Post-state (async-native)

```
        sync transfer thread (OS thread, NOT a blocking-pool slot)
        |
        | tokio::sync::mpsc::channel  (bounded, backpressured)
        |
        v
  outbound async task on shared tokio runtime  -->  russh Channel write half
        ^
        |
        v
  inbound async task on shared tokio runtime  <--  russh Channel read half
        |
        | tokio::sync::mpsc::channel  (bounded, backpressured)
        |
        v
        sync transfer thread (same OS thread)
```

Key design moves:

1. **Drop the spawn_blocking bridge.** Replace `writer_fanin` (`spawn_blocking` draining `std_mpsc` into `tokio_mpsc`) with a direct `tokio::sync::mpsc::channel`. The sync transfer thread calls `tx.blocking_send(chunk)` on the tokio sender; the async pump on the receiver side awaits `rx.recv()`. No blocking-pool slot is held.
2. **Drop the second spawn_blocking (`server_handle`).** The sync transfer pipeline runs on a real `std::thread::spawn`-managed OS thread that the session owns directly. The thread is parented to the session, not to the blocking pool. The async coordinator awaits a `tokio::sync::oneshot` for completion, joining the thread on the synchronous side once oneshot signals.
3. **Reuse the synchronous transfer pipeline unchanged.** `run_blocking_server` keeps its `Read + Write` signature. The reader half is a thin sync adapter over `tokio::sync::mpsc::Receiver` (via `blocking_recv`); the writer half is a thin sync adapter over `tokio::sync::mpsc::Sender` (via `blocking_send`). No protocol-engine code changes.
4. **Share a single tokio runtime across sessions.** A process-wide multi-thread runtime hosts all russh I/O tasks (and the daemon hybrid listener if enabled). `Builder::new_current_thread()` per session is replaced by `Handle::current()` capture at session construction. RUSSH-2 (#2805) already flagged the per-session runtime build cost; this design fixes it.
5. **Symmetric reverse direction.** The russh -> transfer direction mirrors the forward direction: an async task receives from the russh channel and pushes into a tokio mpsc; the transfer thread blocks on `Receiver::blocking_recv()`. Same channel type both directions, opposite producer/consumer alignment.

The net effect: each russh session costs **one OS thread for the transfer pipeline plus one or two tokio tasks for I/O pumping**, not three blocking-pool slots. The blocking pool stops being the binding ceiling.

## 3. Concurrency Model

Per session, the async-native path uses:

| Resource | Count | Where it lives | Notes |
|----------|-------|----------------|-------|
| Tokio task: russh outbound pump | 1 | Shared process-wide tokio runtime | `tokio::spawn`. Awaits `rx.recv()` from sync transfer thread, calls `russh::Channel::data().await`. |
| Tokio task: russh inbound pump | 1 | Same runtime | `tokio::spawn`. Awaits `channel.wait().await`, pushes into mpsc for sync transfer thread. |
| Tokio task: russh control / channel lifecycle | 0 or 1 | Same runtime | Optional: a single supervisor task that owns the russh `Channel` and forwards `Eof`/`Close` events. Folded into the inbound pump when feasible. |
| OS thread: sync transfer pipeline | 1 | `std::thread::spawn` parented to the session | Runs `run_blocking_server`. Joins on completion. Not a blocking-pool slot. |
| tokio mpsc channel | 2 | Heap | One per direction. Bounded; default capacity 32 chunks of up to 32 KiB. |
| Blocking-pool slots | 0 | n/a | Zero on the critical path. Per-file metadata application (`copier.rs:184`) remains as today and is orthogonal. |

### Comparison to today

| Per-session cost | Today (spawn_blocking) | Async-native |
|------------------|------------------------|--------------|
| Blocking-pool slots | 2 long-lived + 0..N transient | 0 long-lived |
| Tokio tasks | 2 pump tasks | 2 pump tasks (unchanged) |
| OS threads | 1 (sync server, inside `spawn_blocking`) | 1 (sync server, on `std::thread`) |
| Per-session tokio runtime | 1 current-thread runtime built and dropped per session | 0 (shared runtime) |
| Net wins | - | 2 blocking-pool slots per session + 1 runtime build/drop |
| Net costs | - | 2 mpsc channels (memory-neutral vs the existing 2-stage `std_mpsc` + `tokio_mpsc` pair) |

### Ceiling impact

With the blocking-pool slot count dropped from 2 to 0 per session, the binding ceiling shifts from `max_blocking_threads / 2` (~256 sessions) to whichever of the following hits first:

- Tokio task count: cheap; tasks are ~2 KB each. 10k tasks ~= 20 MB.
- OS thread count for sync transfer pipelines: bounded by process `RLIMIT_NPROC` and stack reservation (`RUST_MIN_STACK`, default 2 MB). 4k threads with 2 MB stacks = 8 GB VSS, several hundred MB RSS in practice.
- Daemon `--max-connections` admission gate: already exists, already enforced (see [[project_daemon_max_connections_v062]]).

The expected new ceiling is in the low thousands of concurrent sessions per process, with `--max-connections` as the operator-controllable cap. RUSSH-13 re-bench will quantify the new ceiling at 64 / 128 / 256 / 512 / 1024 / 2048 concurrent sessions.

## 4. Tradeoff

Flag-controlled rollout. Default OFF in v0.6.x. Default decision deferred to RUSSH-14 once RUSSH-13 has data.

**Pros:**

- Dissolves the `spawn_blocking` ceiling. Was hundreds; expected to be thousands of concurrent sessions per process.
- Eliminates per-session blocking-pool reservation. Other `spawn_blocking` users in-process (`copier.rs` metadata, future async-daemon listener) get the full 512-slot budget back.
- Removes per-session current-thread runtime construct/drop cost. Sub-millisecond per session today, but visible at the hundreds-of-sessions-per-second arrival rate flagged in the RUSSH-2 audit.
- Simpler error-path mental model: a tokio task failure surfaces as an `mpsc` close + `JoinError`, instead of `spawn_blocking` join + `tokio_mpsc` close + `std_mpsc` close cascade.

**Cons:**

- Adds an async-channel hop per byte chunk in both directions. Latency cost is measurable on single-stream throughput. We expect 5-10% slowdown on the 1-stream benchmark (RUSSH-13 will confirm). The 2-stage `std_mpsc` + `tokio_mpsc` bridge today already has two hops, so we are not adding hops; we are swapping the `std_mpsc` hop for a `tokio_mpsc` hop. The slowdown comes from `blocking_send` / `blocking_recv` having a higher minimum cost than `std::sync::mpsc::SyncSender::send` for the uncontended case.
- Error propagation through `tokio::sync::mpsc` requires careful drop/close semantics to avoid silent transfer hangs. Specifically: dropping a sender mid-transfer must surface as `BrokenPipe` on the reader; closing a receiver mid-transfer must surface as `BrokenPipe` on the writer. The goodbye-phase barrier (Section 6) must drain the outbound channel before the russh `Channel::eof()` is issued, or the remote side will see a truncated stream.
- Shared process-wide runtime introduces shared-state contention that per-session runtimes do not have. A misbehaving session task can stall the runtime for the rest. Mitigation: budgeted task supervision plus a hard per-session deadline on the inbound pump.
- The sync transfer thread's OS-thread lifetime is now explicit and visible to operators. Previously the blocking-pool reuse hid the per-session thread cost; now it shows up as a named thread per session. Naming convention: `oc-rsync-ssh-{session_id}`.

## 5. API Shape

The existing public surface of [`SshConnection`](../../crates/rsync_io/src/ssh/connection.rs#L30) and [`SshChildHandle`](../../crates/rsync_io/src/ssh/connection.rs#L396) does **not** change. Both remain `std::io::Read + std::io::Write` for `SshConnection` and `wait`/`wait_with_stderr`/`stderr_output`/`cancel_connect_watchdog` for `SshChildHandle`. The async-native path swaps out only the internal bridge that connects these handles to the russh layer.

### New internal trait

In `crates/rsync_io/src/ssh/embedded/`:

```rust
/// Dispatch backend that bridges a synchronous transfer pipeline to a
/// russh::Channel. Selects between the legacy spawn_blocking bridge and
/// the async-native pump.
pub(crate) trait AsyncSshDispatch: Send + 'static {
    /// Spawn the inbound and outbound pumps and return sync read/write
    /// halves the transfer pipeline can use. The pumps are owned by the
    /// dispatch implementation and are dropped when both halves are
    /// dropped.
    fn split(
        self,
        channel: russh::Channel<russh::client::Msg>,
        config: AsyncSshDispatchConfig,
    ) -> (SyncReader, SyncWriter, DispatchHandle);
}

/// Tunable knobs that govern channel capacity and backpressure policy.
pub struct AsyncSshDispatchConfig {
    /// Outbound (transfer -> russh) chunk-channel capacity. Default 32.
    pub outbound_capacity: usize,
    /// Inbound (russh -> transfer) chunk-channel capacity. Default 32.
    pub inbound_capacity: usize,
    /// Maximum chunk size submitted to russh per `data()` call.
    /// Default 32 KiB; matches the existing PUMP_BUF.
    pub max_chunk_bytes: usize,
    /// Hard deadline for the goodbye-phase drain. Caps the time the
    /// outbound pump may spend flushing after the sync writer is dropped.
    /// Default 30 s.
    pub goodbye_drain_timeout: Duration,
}

/// Handle returned alongside the sync halves so the caller can await
/// final completion (joined sync thread + drained pumps) and propagate
/// errors. The handle is movable, holds no thread-local state, and is
/// safe to await from any task on the shared runtime.
pub struct DispatchHandle {
    /// Fires once both pumps have exited and the channel is closed.
    completion: tokio::sync::oneshot::Receiver<DispatchOutcome>,
}

pub struct DispatchOutcome {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub close_reason: CloseReason,
}

pub enum CloseReason {
    SyncWriterDropped,       // normal goodbye-phase shutdown
    InboundEof,              // remote sent Eof on the russh channel
    PumpError(io::Error),    // either pump returned an error
    Timeout,                 // goodbye-drain deadline exceeded
}
```

Two implementations behind the `russh-async-native` Cargo feature:

- `SpawnBlockingDispatch` (existing path, factored into the new trait, default when feature OFF).
- `AsyncNativeDispatch` (new, default when feature ON; default decision deferred to RUSSH-14).

The dispatch trait is `pub(crate)` and lives behind the existing `embedded-ssh` feature. Callers (`async_ssh_transport.rs`, `connect.rs`) construct the dispatch via a `cfg`-gated factory function, so neither call site contains feature-gated branching beyond the factory.

### Channel buffer-size knob

`AsyncSshDispatchConfig::outbound_capacity` / `inbound_capacity` plumb through to the existing `into_sync_halves_with_capacity` shape. Operator-facing exposure is via an env var on first delivery, with a CLI flag follow-up to be specced in RUSSH-10:

- `OC_RSYNC_SSH_CHANNEL_CAP=N` overrides both directions. Default 32.
- `OC_RSYNC_SSH_CHUNK_BYTES=N` overrides `max_chunk_bytes`. Default 32768.

### Backpressure semantics

Both directions are bounded `tokio::sync::mpsc` channels. The sync side uses `blocking_send` (outbound) and `blocking_recv` (inbound); the async side uses `await` on both. When the channel is full, the producer blocks until the consumer drains capacity. There is no spill-to-disk; backpressure is the only safety valve. The default capacity of 32 chunks * 32 KiB max = 1 MiB per direction per session, matching the existing `CHANNEL_CAPACITY = 32` constant in `async_ssh_transport.rs:74` and `DEFAULT_CHANNEL_CAPACITY = 64` in `sync_bridge.rs:56` (we converge on 32 for both directions; rationale: the existing 64 value was set for the unidirectional bridge and is too large for symmetric bidirectional flow).

## 6. Ordering Invariants

The async-native path must preserve every ordering invariant the spawn_blocking path holds today.

### 6.1 SSH stream byte ordering

Monotonic byte ordering on each direction. Single producer per direction (sync transfer thread on the outbound side, russh inbound task on the inbound side) plus single consumer per direction (russh outbound task on the outbound side, sync transfer thread on the inbound side). `tokio::sync::mpsc` preserves order between a single producer and single consumer, so this invariant is free.

### 6.2 Goodbye-phase handshake barrier

When the sync transfer pipeline finishes, it drops the `SyncWriter`. This must:

1. Surface as channel-close (`Sender` drop) to the async outbound pump.
2. Cause the outbound pump to drain any remaining chunks already in the channel into the russh `Channel`.
3. Once the channel is drained, call `russh::Channel::eof().await` to signal EOF to the remote.
4. Optionally call `russh::Channel::close().await` to tear down the channel.
5. Only then signal completion via the `DispatchHandle::completion` oneshot.

Failure mode if the barrier is skipped: russh sends `eof` before draining the in-flight chunks; the remote sees a truncated transfer; the receiver reports `unexpected EOF on socket`. The existing spawn_blocking path gets this right by virtue of the `outbound` pump's `async_writer.shutdown().await` (`async_ssh_transport.rs:315`) running after `writer_fanin` exits. We must preserve the exact same sequencing in the async-native path: outbound pump exits its `recv` loop on channel-close, drains its own buffer, then issues `eof` + `shutdown`.

Explicit barrier point: the outbound pump's loop terminates with:

```rust
// rx.recv() returned None (sender dropped). Drain any pending chunk
// that arrived before the sender drop was observed, then signal EOF.
while let Ok(chunk) = rx.try_recv() {
    channel.data(chunk).await?;
}
channel.eof().await?;
channel.close().await?;
```

The `try_recv` loop is required because `mpsc::Receiver::recv` may return `None` before all in-flight `send` calls have woken the receiver; the `try_recv` drain catches the residual.

### 6.3 Exit-code propagation

The SSH child exit-code path (cf. [[project_ssh_stderr_socketpair_silent_fallback]]) must arrive in correct ordering with respect to the transfer. Concretely:

1. Sync transfer pipeline returns its result (`ClientSummary` or `ClientError`) to the session coordinator.
2. The session coordinator awaits the `DispatchHandle::completion` oneshot. The oneshot only fires after both pumps have drained and the russh channel is closed.
3. The session coordinator then awaits the russh `Handle::wait_close().await` (or equivalent in russh's API) to capture the remote exit status.
4. If the remote exit status disagrees with the sync pipeline's result, the worst (highest) exit code wins, matching the existing precedent (`map_child_exit_status()`).

Sequence guarantee: the transfer-result exit code is observed before the remote-exit-status code, but both are reconciled before the session returns to the caller. No race window between "transfer completed" and "remote exit status available."

Stderr drain ordering: the existing stderr-drain thread (`SshConnection::stderr_drain`) is unaffected by this change. It runs on a separate OS thread and is joined during `SshChildHandle::wait`/`wait_with_stderr`. The async-native path adds a `wake_on_close` hook so the drain thread is unblocked promptly when the russh channel closes, mirroring the existing `shutdown_read()` call in `SshConnection::wait`.

### 6.4 Multiplexed frame boundaries

The sync transfer pipeline emits and consumes multiplex frames at the protocol layer (above the byte-channel layer). The byte channel preserves byte order; frame boundaries are reconstructed by the protocol layer. No new invariant here. We must, however, ensure the outbound pump does not split a single `write_all` call across two russh `data()` calls in a way that wakes the remote receiver between halves of a frame header. The pump batches by chunk, and chunks are whole `write` invocations (one chunk per `SyncWriter::write`), so frame headers are atomic per chunk by construction. The `max_chunk_bytes` knob defaults to 32 KiB, well above the largest single frame header (~64 bytes) and below the russh window-update threshold.

## 7. Backwards-Compatibility

Gated behind Cargo feature `russh-async-native`, default OFF. Implementation lives alongside the existing spawn_blocking path; both are compiled when the feature is enabled. Selection at runtime is via the existing env-var pattern.

```toml
# crates/rsync_io/Cargo.toml
[features]
default = ["..."]
embedded-ssh = ["..."]
russh-async-native = ["embedded-ssh"]
```

Runtime selection (additive to `OC_RSYNC_ASYNC_SSH`):

- `OC_RSYNC_SSH_DISPATCH=spawn_blocking` (default): legacy `SpawnBlockingDispatch`.
- `OC_RSYNC_SSH_DISPATCH=async_native`: new `AsyncNativeDispatch`. Requires the `russh-async-native` feature; otherwise rejected at config validation with a clear error.

The default value flips to `async_native` only after RUSSH-14 confirms the rollback criteria below are not triggered. Until then, async-native is opt-in.

RUSSH-10 (#2813) will spec the back-compat shim API surface in detail, including the exact factory function signature, the env-var validation path, and the CLI flag (likely `--ssh-dispatch=spawn_blocking|async_native`) for operator-facing rollback control. This design intentionally stops at the trait surface; the shim ergonomics are RUSSH-10's deliverable.

The existing `SyncReader` / `SyncWriter` types in both `sync_bridge.rs` and `async_ssh_transport.rs` consolidate into a single pair under `crates/rsync_io/src/ssh/embedded/dispatch/` as part of the implementation (RUSSH-11). Today's two copies are not API-public.

## 8. Rollback Criteria

The async-native default flip is gated on every one of these. Any failure reverts the default to `spawn_blocking`. The feature stays in the codebase as a non-default until the failure mode is understood and fixed.

1. **Wire-byte parity fails (RUSSH-12, #2815).** The golden byte test suite under `crates/protocol/tests/golden/` plus the interop suite (`tools/ci/run_interop.sh`) must produce byte-identical wire output between spawn_blocking and async_native dispatch backends for every test scenario. Any single divergence blocks the flip.
2. **Bench shows < 2x ceiling improvement at 512 sessions (RUSSH-13).** The RUSSH-4..7 baselines (#2807-#2810) establish the spawn_blocking ceiling at 64/128/256/512 concurrent sessions. RUSSH-13 reruns the same harness on the async-native backend. If the 512-session run does not show at least 2x more sustained throughput (or 2x lower per-session p99 latency, or both), the design did not deliver its core promise. Re-evaluate before flipping.
3. **Bench shows > 15% regression at 1-4 sessions (RUSSH-13).** Single-stream and small-deployment throughput is the common case. A 5-10% slowdown is expected and acceptable; > 15% is not. If the regression is in this band, investigate the per-chunk channel overhead (likely `blocking_send` cost) before flipping.
4. **Stress test reveals exit-code or goodbye-phase ordering bug.** A new stress test under `crates/rsync_io/tests/` exercises rapid session open/close, mid-transfer aborts, and remote-side exits at every protocol phase. Any silent hang, truncated transfer, or exit-code regression blocks the flip.
5. **Memory regression > 10% peak RSS at 512 sessions.** The shared runtime model removes per-session runtime allocations but adds shared-runtime task state. If the net is a > 10% RSS regression at 512 sessions vs the spawn_blocking baseline, investigate (likely culprits: task-stack default, oversized channel capacity, leaked pumps).

Rollback mechanics:

- Code rollback: flip the `OC_RSYNC_SSH_DISPATCH` default from `async_native` to `spawn_blocking` in the dispatch factory. One-line change.
- Operator rollback: set `OC_RSYNC_SSH_DISPATCH=spawn_blocking` in the environment. No restart required for clients; daemons require restart since the dispatch is chosen at session open.
- Feature rollback: drop the `russh-async-native` feature from the default feature set. The implementation stays in-tree, gated, until the failure mode is resolved.

## 9. Cross-Links

- [[project_russh_spawn_blocking_ceiling]] - bottleneck source; this design's reason for existing.
- [[project_no_async_threaded_only]] - constraint: transfer pipeline stays threaded; only the boundary becomes async.
- [[project_ssh_push_russh_v062]] - prior russh migration history (v0.6.2 fixed the 200x SSH push regression by adopting russh; this design extends that line of work).
- [[project_ssh_stderr_socketpair_silent_fallback]] - stderr ordering invariant carried forward unchanged; see Section 6.3.
- [`docs/audit/russh-spawn-blocking-ceiling-inventory.md`](../audit/russh-spawn-blocking-ceiling-inventory.md) - RUSSH-1 (#2804) call-site inventory.
- RUSSH-2 (#2805) - tokio runtime sizing audit (per-session current-thread runtime cost flagged here).
- RUSSH-3 (#2806) - N-concurrent-sessions bench harness (consumes this design's ceiling claim).
- RUSSH-4..7 (#2807-#2810) - baseline runs at 64/128/256/512 concurrent sessions.
- RUSSH-10 (#2813) - back-compat shim API surface.
- RUSSH-11 (#2814) - implementation.
- RUSSH-12 (#2815) - wire-byte parity test.
- RUSSH-13 - re-bench.
- RUSSH-14 - adopt/defer decision.
