# RUSSH-11: Async-Native russh Implementation Spec

**Tracking:** RUSSH-11 (#2814)
**Status:** Implementation spec. Implements the design from RUSSH-9 (#2812) behind the back-compat shim from RUSSH-10 (#2813).
**Feature flag:** `russh-async-native` (Cargo feature on `rsync_io`, default OFF).

Cross-links: [[project_russh_spawn_blocking_ceiling]], [[project_no_async_threaded_only]], [[project_ssh_push_russh_v062]], [[project_ssh_stderr_socketpair_silent_fallback]].

## 1. Summary of RUSSH-9 Design

RUSSH-9 replaces the `tokio::task::spawn_blocking` bridge at the russh boundary with a direct async-task dispatch model:

- **Today:** each SSH session holds 2 long-lived blocking-pool slots (`writer_fanin` + `server_handle` in `async_ssh_transport.rs`), capping concurrency at ~256 sessions per process.
- **After:** each session uses one `std::thread::spawn`-managed OS thread for the sync transfer pipeline plus two lightweight tokio tasks for I/O pumping. Zero blocking-pool slots on the critical path.
- **Unchanged:** the synchronous transfer pipeline (`run_blocking_server`, protocol engine, handshake, file list, delta apply) stays threaded. Only the boundary layer between sync pipeline and russh I/O is reshaped.

The net effect is a ceiling shift from ~256 to low-thousands of concurrent sessions, with `--max-connections` as the operator-controllable cap.

RUSSH-10 specifies the back-compat shim: a `Box<dyn AsyncSshDispatch>` trait object on `SshConnection`/`SshChildHandle` and an enum dispatch on `SshReader`/`SshWriter`, keeping the public API surface identical across both backends.

## 2. Feature Flag

```toml
# crates/rsync_io/Cargo.toml
[features]
russh-async-native = ["embedded-ssh"]
```

The feature implies `embedded-ssh` (which pulls in `russh`, `tokio`, etc.). Both dispatch backends compile when the feature is enabled. Runtime selection via env var:

- `OC_RSYNC_SSH_DISPATCH=spawn_blocking` (default): legacy `SpawnBlockingDispatch`.
- `OC_RSYNC_SSH_DISPATCH=async_native`: new `AsyncNativeDispatch`. Rejected at config validation if the feature is not compiled.

The default flips to `async_native` only after RUSSH-14 confirms rollback criteria are not triggered.

## 3. Files to Create

| Path | Purpose |
|------|---------|
| `crates/rsync_io/src/ssh/dispatch/mod.rs` | `AsyncSshDispatch` trait, `DispatchKind` enum, `DispatchConfig`, factory function `resolve_dispatch()` |
| `crates/rsync_io/src/ssh/dispatch/spawn_blocking.rs` | `SpawnBlockingDispatch` - wraps the existing subprocess `Child`-based path into the new trait |
| `crates/rsync_io/src/ssh/dispatch/async_native.rs` | `AsyncNativeDispatch` - the new `std::thread` + shared-runtime + russh-channel path (cfg-gated on `russh-async-native`) |
| `crates/rsync_io/src/ssh/dispatch/config.rs` | `DispatchConfig::from_env()` implementation, env-var parsing, feature-gate validation |
| `crates/rsync_io/src/ssh/dispatch/sync_halves.rs` | Consolidated `SyncReader` / `SyncWriter` pair backed by `tokio::sync::mpsc`, replacing the duplicates in `sync_bridge.rs` and `async_ssh_transport.rs` |
| `crates/rsync_io/tests/ssh_dispatch_shim_compat.rs` | Public-surface compile + behaviour tests per RUSSH-10 Section 7 |
| `crates/rsync_io/tests/ssh_async_native_stress.rs` | Rapid open/close, mid-transfer abort, goodbye-phase ordering tests (feature-gated) |

## 4. Files to Modify

| Path | Change |
|------|--------|
| `crates/rsync_io/Cargo.toml` | Add `russh-async-native` feature |
| `crates/rsync_io/src/ssh/mod.rs` | Add `pub(crate) mod dispatch;` |
| `crates/rsync_io/src/ssh/connection.rs` | Replace struct fields with `Box<dyn AsyncSshDispatch>` for `SshConnection` and `SshChildHandle`; add `ReaderInner`/`WriterInner` enums for `SshReader`/`SshWriter`; delegate all methods to the dispatch trait |
| `crates/rsync_io/src/ssh/builder.rs` | `SshCommand::spawn()` reads `DispatchConfig::from_env()` and routes to `spawn_via_subprocess` or `spawn_via_russh` |
| `crates/rsync_io/src/ssh/connect.rs` | `SshConnection::connect_with_config()` follows the same dispatch factory |
| `crates/rsync_io/src/ssh/embedded/connect.rs` | Extract the `connect_and_exec_async` core into a reusable `open_russh_channel()` that `AsyncNativeDispatch` can call directly, avoiding runtime construction duplication |
| `crates/rsync_io/src/ssh/embedded/sync_bridge.rs` | Deprecate the standalone `SyncReader`/`SyncWriter` in favor of the consolidated pair under `dispatch/sync_halves.rs`; re-export from `sync_bridge` for backward compat within the crate |
| `crates/core/src/client/remote/async_ssh_transport.rs` | Remove the private `SyncReader`/`SyncWriter`/`SyncWriter::Drop` duplicates; import from `rsync_io::ssh::dispatch::sync_halves` instead. The `writer_fanin` and `server_handle` `spawn_blocking` calls become the `SpawnBlockingDispatch` implementation, extracted out of inline code |

## 5. Async Channel Bridge Design

### 5.1 Replacing spawn_blocking with native async reads/writes

The two `spawn_blocking` call sites in `async_ssh_transport.rs` are replaced as follows:

**Site 1 - `writer_fanin` (line 349):** Today this `spawn_blocking` drains a `std::sync::mpsc::Receiver` and feeds a `tokio::sync::mpsc::Sender`. The async-native path eliminates this hop entirely. The sync transfer thread calls `tokio::sync::mpsc::Sender::blocking_send(chunk)` directly into a `tokio::sync::mpsc` channel. The async outbound pump awaits `rx.recv()` on the other end and writes to the russh channel. One fewer thread, one fewer channel stage.

**Site 2 - `server_handle` (line 361):** Today this `spawn_blocking` runs `run_blocking_server` on the tokio blocking pool. The async-native path spawns a real `std::thread::Builder::new().name("oc-rsync-ssh-{id}").spawn(...)` OS thread instead. The thread is parented to the session, not to the blocking pool. A `tokio::sync::oneshot` signals completion; the async coordinator awaits it.

### 5.2 Channel topology

```
    sync transfer thread (std::thread, named "oc-rsync-ssh-{id}")
    |                                       ^
    | blocking_send on                      | blocking_recv on
    | tokio::sync::mpsc::Sender             | tokio::sync::mpsc::Receiver
    v                                       |
    outbound async pump (tokio::spawn)      inbound async pump (tokio::spawn)
    |                                       ^
    | channel.data(chunk).await             | channel.wait().await
    v                                       |
    russh Channel write half                russh Channel read half
```

Both directions use bounded `tokio::sync::mpsc` channels. Default capacity: 32 chunks, max 32 KiB per chunk (matching `CHANNEL_CAPACITY` and `PUMP_BUF` constants).

### 5.3 SyncReader / SyncWriter consolidation

Today there are three independent `SyncReader`/`SyncWriter` implementations:

1. `crates/rsync_io/src/ssh/embedded/sync_bridge.rs` (backed by `std::sync::mpsc`)
2. `crates/core/src/client/remote/async_ssh_transport.rs` (backed by `std::sync::mpsc`)
3. `crates/rsync_io/src/ssh/embedded/connect.rs` (`ChannelReader`/`ChannelWriter`)

RUSSH-11 consolidates these into a single `SyncReader`/`SyncWriter` pair under `dispatch/sync_halves.rs`, backed by `tokio::sync::mpsc`. The old types are deprecated and re-exported as type aliases pointing to the new location.

Key change: the inbound side uses `tokio::sync::mpsc::Receiver::blocking_recv()` instead of `std::sync::mpsc::Receiver::recv()`. This is required because both directions of the async-native path must go through tokio channels to avoid the extra `std_mpsc -> tokio_mpsc` bridge stage that the spawn_blocking path currently uses.

## 6. Back-Compat Shim Wiring (per RUSSH-10)

### 6.1 SshConnection field layout

```rust
// crates/rsync_io/src/ssh/connection.rs
pub struct SshConnection {
    dispatch: Box<dyn AsyncSshDispatch + Send + Sync>,
}

pub struct SshChildHandle {
    dispatch: Box<dyn AsyncSshDispatch + Send + Sync>,
}
```

All existing fields (`child`, `stdin`, `stdout`, `stderr_drain`, `connect_watchdog`) move into `SpawnBlockingDispatch`'s internal state. The public surface (`Read`, `Write`, `split`, `wait`, `wait_with_stderr`, `cancel_connect_watchdog`, `stderr_output`, `try_wait`, `close_stdin`, `Drop`) delegates to the dispatch trait.

### 6.2 SshReader / SshWriter enum dispatch

```rust
pub struct SshReader {
    inner: ReaderInner,
}

enum ReaderInner {
    Subprocess(ChildStdout),
    AsyncChannel(dispatch::sync_halves::SyncReader),
}

impl Read for SshReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            ReaderInner::Subprocess(stdout) => stdout.read(buf),
            ReaderInner::AsyncChannel(reader) => reader.read(buf),
        }
    }
}
```

Same pattern for `SshWriter` / `WriterInner`. Enum dispatch keeps the per-chunk branch inline-friendly; no vtable hop on the data hot path (per RUSSH-10 Section 3.4 rationale).

### 6.3 Construction flow

```rust
// crates/rsync_io/src/ssh/builder.rs
impl SshCommand {
    pub fn spawn(&self) -> io::Result<SshConnection> {
        let dispatch_cfg = DispatchConfig::from_env()?;
        match dispatch_cfg.kind {
            DispatchKind::SpawnBlocking => self.spawn_via_subprocess(dispatch_cfg),
            #[cfg(feature = "russh-async-native")]
            DispatchKind::AsyncNative => self.spawn_via_russh(dispatch_cfg),
        }
    }
}
```

`spawn_via_subprocess` wraps the existing `Command::spawn` + `SshConnection::new` path in a `SpawnBlockingDispatch`. `spawn_via_russh` calls into `embedded::connect::open_russh_channel()`, wires up the mpsc channels and pump tasks, and returns an `AsyncNativeDispatch`.

## 7. cfg-Gated Dispatch

The `AsyncNative` variant is fully cfg-gated:

```rust
// crates/rsync_io/src/ssh/dispatch/mod.rs
pub(crate) enum DispatchKind {
    SpawnBlocking,
    #[cfg(feature = "russh-async-native")]
    AsyncNative,
}
```

When `russh-async-native` is OFF:
- `DispatchKind::AsyncNative` does not exist.
- `DispatchConfig::from_env()` rejects `OC_RSYNC_SSH_DISPATCH=async_native` with `io::Error { kind: InvalidInput, msg: "feature `russh-async-native` not enabled" }`.
- The `async_native.rs` module is not compiled.
- The `SpawnBlockingDispatch` is the only implementation.

When `russh-async-native` is ON:
- Both backends compile.
- Runtime selection via the env var.
- Default remains `SpawnBlocking` until RUSSH-14 flips it.

## 8. Error Handling

### 8.1 Panic propagation from async tasks

Panics in the outbound/inbound pump tasks are caught by `tokio::JoinHandle::await`. The `AsyncNativeDispatch::wait_blocking()` method inspects the `JoinError`:

```rust
match pump_handle.await {
    Ok(Ok(())) => { /* clean exit */ }
    Ok(Err(io_err)) => {
        // Pump returned an I/O error. Map to BrokenPipe on the
        // SyncReader/SyncWriter side.
        return Err(io_err);
    }
    Err(join_err) if join_err.is_panic() => {
        // Propagate as a descriptive I/O error rather than
        // double-panicking. The panic payload is lost (opaque Any),
        // but the backtrace is already printed by tokio's panic hook.
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("SSH pump task panicked: {join_err}"),
        ));
    }
    Err(join_err) => {
        // Task was cancelled (runtime shutdown). Surface as BrokenPipe.
        return Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("SSH pump task cancelled: {join_err}"),
        ));
    }
}
```

### 8.2 Panic in the sync transfer thread

The `std::thread::spawn`-managed transfer thread captures panics via `std::panic::catch_unwind` (or the oneshot sender is dropped on panic, signalling the coordinator). The `DispatchHandle::completion` oneshot returns `Err(RecvError)` when the sender is dropped, which the coordinator maps to `io::Error { kind: Other, msg: "transfer thread panicked" }`.

### 8.3 Channel-close error propagation

When either pump drops its mpsc sender:
- The sync `SyncReader::read` returns `Ok(0)` (EOF).
- The sync `SyncWriter::write` returns `Err(BrokenPipe)`.

When the sync writer drops its sender:
- The outbound pump's `rx.recv().await` returns `None`.
- The pump drains residual chunks via `try_recv`, issues `channel.eof().await`, then `channel.close().await`.

This mirrors the goodbye-phase barrier from RUSSH-9 Section 6.2.

### 8.4 ExitStatus synthesis

The async-native path does not have a subprocess `ExitStatus`. It synthesises one:

| russh event | Synthesised ExitStatus |
|-------------|----------------------|
| Clean channel close | `ExitStatus::from_raw(0)` |
| `ExitStatus { exit_status }` message | `ExitStatus::from_raw(exit_status << 8)` (POSIX encoding) |
| `ExitSignal { signal_name, .. }` | Mapped via signal name to number, then `from_raw(signal \| 0x80)` |
| Connection lost | `ExitStatus::from_raw(255 << 8)` (matches ssh exit code 255) |

The synthesised value round-trips through `map_child_exit_status()` to produce the same `ExitCode` the subprocess path would.

## 9. Integration with Daemon Connection Lifecycle

The daemon's async listener (`crates/daemon/src/async_listener.rs`) uses a shared multi-thread tokio runtime. The async-native dispatch reuses this same shared runtime via `tokio::runtime::Handle::current()`:

1. When the daemon accepts a TCP connection via its tokio listener, it currently dispatches to `spawn_blocking` for the sync worker.
2. With the async-native path, the sync worker runs on a `std::thread` instead. The russh I/O pump tasks are spawned on the same shared runtime that the daemon listener uses.
3. The session lifecycle mirrors the existing daemon state machine (`Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing`) - only the I/O bridge is swapped.

For client-side SSH (non-daemon), the shared runtime is constructed once per process in a `OnceLock<tokio::runtime::Handle>`:

```rust
// crates/rsync_io/src/ssh/dispatch/async_native.rs
fn shared_runtime_handle() -> &'static tokio::runtime::Handle {
    static HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("oc-rsync-ssh-io")
            .enable_all()
            .build()
            .expect("failed to build shared SSH runtime")
            .handle()
            .clone()
    })
}
```

This replaces the per-session `Builder::new_current_thread()` construction in `connect_and_exec()` and `run_async_session()`. If a tokio runtime is already active (daemon context), `Handle::current()` is used instead.

## 10. Test Strategy

### 10.1 Unit tests (async-native path)

Located in `crates/rsync_io/src/ssh/dispatch/async_native.rs`:

| Test | What it validates |
|------|-------------------|
| `outbound_pump_drains_on_sender_drop` | Writer drop triggers channel.eof + channel.close sequencing |
| `inbound_pump_surfaces_eof_on_channel_close` | russh Eof message yields Ok(0) from SyncReader |
| `channel_backpressure_blocks_sync_writer` | Full channel causes blocking_send to block; drain unblocks |
| `panic_in_pump_surfaces_as_io_error` | Panicking pump task produces descriptive io::Error, not double-panic |
| `exit_status_synthesis_round_trips` | Each russh close reason maps through map_child_exit_status to correct ExitCode |
| `goodbye_drain_timeout_fires` | Hung channel triggers timeout within budget, does not hang indefinitely |
| `concurrent_session_ceiling` | 100 simultaneous mock sessions use zero blocking-pool slots (checked via tokio metrics) |

### 10.2 Wire-byte parity tests (RUSSH-12 prep)

Located in `crates/rsync_io/tests/ssh_dispatch_shim_compat.rs`:

- Each test runs the same I/O sequence (write payload, read echo, wait) against both `SpawnBlockingDispatch` and `AsyncNativeDispatch`.
- Byte-for-byte output comparison on the sync reader/writer halves.
- Exit status comparison after clean and error exits.
- Stderr collection comparison for equivalent remote error output.

These tests are structural guards for the shim; full wire-format validation against upstream rsync is RUSSH-12's scope.

### 10.3 Stress tests

Located in `crates/rsync_io/tests/ssh_async_native_stress.rs` (feature-gated on `russh-async-native`):

| Test | What it validates |
|------|-------------------|
| `rapid_open_close_no_leak` | 1000 sessions opened and closed in tight loop; thread count and memory stable |
| `mid_transfer_abort_surfaces_error` | Dropping SshConnection mid-transfer produces BrokenPipe, not hang |
| `goodbye_phase_ordering_under_load` | 50 concurrent sessions all complete goodbye phase within budget |
| `drop_without_wait_reaps_cleanly` | SshConnection/SshChildHandle Drop does not leak threads or tasks |

### 10.4 Existing tests that MUST pass on both backends

- `crates/rsync_io/tests/ssh_stderr_child_exit.rs` - exit code + stderr propagation
- `crates/rsync_io/tests/ssh_stderr_default_path.rs` - wait_with_stderr default path
- All `connection.rs` inline tests
- All `sync_bridge.rs` inline tests
- All `connect.rs` inline tests (ChannelReader/ChannelWriter)

## 11. Bench Integration (RUSSH-13)

RUSSH-13 re-benches the async-native path against the spawn_blocking baseline at 64/128/256/512/1024/2048 concurrent sessions. The bench harness from RUSSH-3 (#2806) is reused with the following additions:

### 11.1 Dispatch backend parameterization

The bench harness gains a `--dispatch` flag (`spawn_blocking` | `async_native`) that sets `OC_RSYNC_SSH_DISPATCH` before spawning the session pool. Both backends run on identical hardware and workload profiles.

### 11.2 Metrics collected

| Metric | Collection method |
|--------|-------------------|
| Per-session throughput (MB/s) | Wall-clock transfer time for a fixed 100 MiB payload |
| Per-session p99 latency | Time from session start to first byte received |
| Blocking-pool slot count | `tokio::runtime::RuntimeMetrics::num_blocking_threads()` sampled at 1 Hz |
| OS thread count | `/proc/self/status` `Threads:` field (Linux) or `task_info` (macOS) sampled at 1 Hz |
| Peak RSS | `/proc/self/status` `VmRSS:` field (Linux) or `mach_task_basic_info` (macOS) |
| Session failure rate | Count of sessions that returned error / total sessions |

### 11.3 Regression thresholds (from RUSSH-9 Section 8)

- Single-stream (1-4 sessions): < 15% throughput regression vs spawn_blocking baseline.
- High-concurrency (512 sessions): > 2x sustained throughput improvement.
- Peak RSS at 512 sessions: < 10% regression.

### 11.4 Bench binary location

`crates/rsync_io/benches/ssh_dispatch_bench.rs` - extends the existing `ssh_sync_vs_async` bench with the dispatch backend as a parameter.

## 12. Implementation Order

1. **Dispatch trait + config** (`dispatch/mod.rs`, `dispatch/config.rs`): Define `AsyncSshDispatch`, `DispatchKind`, `DispatchConfig::from_env()`. No functional change yet.
2. **SpawnBlockingDispatch** (`dispatch/spawn_blocking.rs`): Extract the existing `Child`-based logic from `connection.rs` into the trait impl. Verify all existing tests pass.
3. **SshConnection/SshChildHandle refactor** (`connection.rs`): Replace fields with `Box<dyn AsyncSshDispatch>`. Wire `SshReader`/`SshWriter` enum dispatch. Verify all existing tests and callers compile.
4. **SyncReader/SyncWriter consolidation** (`dispatch/sync_halves.rs`): Unify the three duplicated reader/writer pairs. Update imports in `async_ssh_transport.rs` and `sync_bridge.rs`.
5. **AsyncNativeDispatch** (`dispatch/async_native.rs`): Implement the new path - shared runtime, `std::thread` for transfer, tokio tasks for pumps, goodbye barrier, exit status synthesis. Feature-gated.
6. **Builder + connect wiring** (`builder.rs`, `connect.rs`): Wire `DispatchConfig` into `spawn()` and `connect_with_config()`. Both paths functional.
7. **Shim compat tests** (`ssh_dispatch_shim_compat.rs`): Validate public surface under both backends.
8. **Stress tests** (`ssh_async_native_stress.rs`): Rapid open/close, abort, goodbye ordering.
9. **Bench integration** (`ssh_dispatch_bench.rs`): Parameterized bench for RUSSH-13.

Steps 1-3 land as one PR (the shim, no new functionality). Steps 4-6 land as a second PR (the async-native impl). Steps 7-9 land as a third PR (tests + bench).

## 13. Rollback Mechanics

Per RUSSH-9 Section 8 and RUSSH-10 Section 8:

- **Code rollback:** flip `DispatchConfig::from_env()`'s default from `AsyncNative` to `SpawnBlocking`. One-line change.
- **Operator rollback:** set `OC_RSYNC_SSH_DISPATCH=spawn_blocking` in the environment.
- **Feature rollback:** remove `russh-async-native` from the default feature set.

The async-native implementation stays in-tree, gated, until the failure mode is resolved.

## 14. Cross-Links

- RUSSH-9 (#2812) - [`docs/design/russh-async-native-path.md`](./russh-async-native-path.md) - parent design.
- RUSSH-10 (#2813) - [`docs/design/russh-async-native-back-compat-shim.md`](./russh-async-native-back-compat-shim.md) - shim spec.
- RUSSH-12 (#2815) - wire-byte parity validation across both dispatchers.
- RUSSH-13 - re-bench at 64/128/256/512/1024/2048 concurrent sessions.
- RUSSH-14 - adopt/defer decision for the async-native default.
- [[project_russh_spawn_blocking_ceiling]] - root bottleneck.
- [[project_no_async_threaded_only]] - constraint: transfer pipeline stays threaded.
- [[project_ssh_push_russh_v062]] - prior russh migration history.
- [[project_ssh_stderr_socketpair_silent_fallback]] - stderr ordering invariant.
