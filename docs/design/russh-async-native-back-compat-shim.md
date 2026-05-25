# Back-Compat Shim for Async-Native russh Path - Design Spec

**Tracking:** RUSSH-10 (#2813)
**Status:** Design only. Implementation lands under RUSSH-11 (#2814). Wire-byte parity under RUSSH-12 (#2815).
**Parent:** [`docs/design/russh-async-native-path.md`](./russh-async-native-path.md) (RUSSH-9, #2812 / PR #4912).

Cross-links: [[project_russh_spawn_blocking_ceiling]], [[project_no_async_threaded_only]], [[project_ssh_stderr_socketpair_silent_fallback]], [[project_finish_file_arc_unwrap_ergonomics]].

## 1. Scope

RUSSH-10 specifies the back-compat shim that wraps RUSSH-9's architectural change behind the existing public types in `crates/rsync_io/`. The architectural change in RUSSH-9 replaces the per-session `tokio::task::spawn_blocking` bridge with a `std::thread`-backed sync transfer pipeline plus a small number of tokio tasks on a shared runtime. Two dispatch backends must coexist:

- `SpawnBlockingDispatch` (today's path; default when the `russh-async-native` Cargo feature is OFF).
- `AsyncNativeDispatch` (new path; default when the `russh-async-native` Cargo feature is ON; default flip deferred to RUSSH-14).

The shim's job is to make the swap invisible to every public caller of [`SshConnection`](../../crates/rsync_io/src/ssh/connection.rs#L30) and [`SshChildHandle`](../../crates/rsync_io/src/ssh/connection.rs#L396).

**Acceptance criterion:** the CLI, daemon, and `core::session` MUST require zero code changes when:

1. The default `spawn_blocking` dispatch is in use (today's behavior).
2. The `russh-async-native` Cargo feature is enabled and the user flips `OC_RSYNC_SSH_DISPATCH=async_native` at runtime.

The shim's surface is internal (`pub(crate)`); the public API of `crates/rsync_io/src/ssh/` does not change.

Out of scope:

- The async-native pump implementation itself (RUSSH-11).
- Wire-byte parity validation across dispatchers (RUSSH-12).
- Operator-facing CLI flags for dispatch selection (deferred; env-var only on first delivery).
- Per-session metrics for dispatch type (deferred; covered if/when RUSSH-14 promotes async-native to default).

## 2. Caller Inventory

Every consumer of the `SshConnection` / `SshChildHandle` / `SshReader` / `SshWriter` public surface, as of the RUSSH-10 audit:

### 2.1 `crates/core/`

| File:line | Call | State assumption |
|-----------|------|------------------|
| `crates/core/src/client/remote/ssh_transfer.rs:28` | `use rsync_io::ssh::{SshCommand, SshConnection, parse_ssh_operand};` | Imports the public surface. |
| `crates/core/src/client/remote/ssh_transfer.rs:261` | `build_ssh_connection(...) -> Result<SshConnection, ClientError>` | Constructs via `SshCommand::spawn()`. Expects an owned `SshConnection`. |
| `crates/core/src/client/remote/ssh_transfer.rs:299` | `let mut connection = ssh.spawn()?;` | Spawn is synchronous; returns `io::Result<SshConnection>`. |
| `crates/core/src/client/remote/ssh_transfer.rs:323` | `protocol::secluded_args::send_secluded_args(&mut connection, ...)` | Uses `SshConnection: Write` for stdin push of secluded args. Expects blocking writes. |
| `crates/core/src/client/remote/ssh_transfer.rs:344` | `fn run_pull_transfer(..., connection: SshConnection, ...)` | Takes owned `SshConnection`; passes through to `run_server_over_ssh_connection`. |
| `crates/core/src/client/remote/ssh_transfer.rs:380` | `fn run_push_transfer(..., connection: SshConnection, ...)` | Same as pull. |
| `crates/core/src/client/remote/ssh_transfer.rs:553` | `fn run_server_over_ssh_connection(..., connection: SshConnection, ...)` | Owned. |
| `crates/core/src/client/remote/ssh_transfer.rs:557` | `let (mut reader, mut writer, mut child_handle) = connection.split()?;` | Triple unpack: `SshReader`, `SshWriter`, `SshChildHandle`. Reader/writer used as sync `Read + Write`. |
| `crates/core/src/client/remote/ssh_transfer.rs:572` | `child_handle.wait_with_stderr()` (handshake-failure branch) | Expects `wait_with_stderr` to consume the handle, block synchronously, return `(ExitStatus, Vec<u8>)`. |
| `crates/core/src/client/remote/ssh_transfer.rs:585` | `child_handle.cancel_connect_watchdog()` | Mutable, non-consuming; expects `Ok(())` on a disarmed watchdog. |
| `crates/core/src/client/remote/ssh_transfer.rs:605` | `child_handle.wait_with_stderr()` (post-transfer branch) | Same blocking-consume contract. |
| `crates/core/src/client/remote/remote_to_remote.rs:44` | `use rsync_io::ssh::{SshCommand, SshConnection, SshReader, SshWriter, parse_ssh_operand};` | Imports both halves explicitly. |
| `crates/core/src/client/remote/remote_to_remote.rs:180` | `fn spawn_ssh_connection(...) -> Result<SshConnection, ClientError>` | Spawn factory. |
| `crates/core/src/client/remote/remote_to_remote.rs:213` | `ssh.spawn()?` | Same spawn contract. |
| `crates/core/src/client/remote/remote_to_remote.rs:242-243` | `fn run_bidirectional_relay(source: SshConnection, dest: SshConnection)` | Two owned connections at once - proxy mode. |
| `crates/core/src/client/remote/remote_to_remote.rs:245` | `source.split()?` -> `(SshReader, SshWriter, SshChildHandle)` | Same `split` contract; both halves are passed to relay threads. |
| `crates/core/src/client/remote/remote_to_remote.rs:248` | `dest.split()?` -> same triple | Same. |
| `crates/core/src/client/remote/remote_to_remote.rs:296` | `source_handle.wait_with_stderr()` | Blocking consume, returns `(ExitStatus, Vec<u8>)`. |
| `crates/core/src/client/remote/remote_to_remote.rs:303` | `dest_handle.wait_with_stderr()` | Same. |
| `crates/core/src/client/remote/remote_to_remote.rs:345-346` | `fn run_relay_with_panic_guard(reader: SshReader, writer: SshWriter, ...)` | Owns `SshReader` / `SshWriter` across an OS-thread boundary; both must be `Send`. |
| `crates/core/src/client/remote/remote_to_remote.rs:383-384` | `fn relay_data(mut reader: SshReader, mut writer: SshWriter, ...)` | Uses `SshReader: Read` and `SshWriter: Write` directly. Calls `writer.close()` (the inherent `SshWriter::close`) on EOF. |

### 2.2 `crates/cli/`

| File:line | Call | State assumption |
|-----------|------|------------------|
| `crates/cli/src/frontend/execution/drive/config.rs:15` | `use rsync_io::ssh;` | Module import only; no direct construction or method calls on `SshConnection` / `SshChildHandle` from CLI today. |

The CLI never opens an `SshConnection` directly; it routes through `core::session()` which then calls into `core::client::remote::ssh_transfer`. The shim therefore does not touch the CLI crate at all.

### 2.3 `crates/daemon/`

The daemon does not consume `SshConnection` or `SshChildHandle` (grep returns zero hits). The daemon owns its TCP listener path independently and uses `russh::server` for the embedded SSH server only via the `embedded` submodule, which has its own separate surface. The shim does not touch the daemon crate.

### 2.4 `crates/transport/`

No `crates/transport/` exists in this tree (the workspace uses `crates/rsync_io/` instead). The grep field in the task brief is satisfied vacuously.

### 2.5 `crates/rsync_io/` (internal)

The shim itself lives in this crate. Existing internal callers that the shim must coexist with:

| File:line | Call | Notes |
|-----------|------|-------|
| `crates/rsync_io/src/ssh/builder.rs:342` | `pub fn spawn(&self) -> io::Result<SshConnection>` | Constructs `SshConnection::new(...)` directly. The shim wraps this construction; the public signature does not change. |
| `crates/rsync_io/src/ssh/connect.rs:185` | `pub fn connect_with_config(remote: &str, config: &SshConnectConfig) -> io::Result<Self>` | Same; thin wrapper over `SshCommand::spawn()`. |
| `crates/rsync_io/src/ssh/connection.rs:52` | `pub(super) fn SshConnection::new(child, stdin, stdout, stderr_channel, connect_timeout) -> Self` | Currently the only constructor. The shim adds an internal dispatch field; the parameter list stays the same for the subprocess (`spawn_blocking`) path and is unused for the async-native path's russh-channel construction. |
| `crates/rsync_io/tests/ssh_stderr_child_exit.rs:58` | Test of `split()` + `SshChildHandle` exit semantics | Must continue to pass on both dispatchers. |
| `crates/rsync_io/tests/ssh_stderr_default_path.rs:8` | Test of `wait_with_stderr` default path | Same. |

### 2.6 Summary of caller expectations the shim MUST preserve

- `SshConnection`, `SshChildHandle`, `SshReader`, `SshWriter` are all owned, `Send` types.
- `SshConnection: Read + Write` with blocking semantics.
- `SshReader: Read`, `SshWriter: Write`, `SshWriter::close(self) -> io::Result<()>` (inherent).
- `SshConnection::split(self) -> io::Result<(SshReader, SshWriter, SshChildHandle)>`.
- `SshConnection::wait(self) -> io::Result<ExitStatus>` and `wait_with_stderr(self) -> io::Result<(ExitStatus, Vec<u8>)>` consume `self` and block.
- `SshChildHandle::wait(self)` / `wait_with_stderr(self)` consume and block; `cancel_connect_watchdog(&mut self) -> io::Result<()>` mutates in place; `stderr_output(&self) -> Vec<u8>` is a non-consuming snapshot.
- `Drop` semantics MUST reap the child and surface collected stderr on error (current `SshConnection::Drop` and `SshChildHandle::Drop` behavior).
- `SshCommand::spawn(&self) -> io::Result<SshConnection>` is the sole public entry point for spawning.

Every one of these contracts is exercised by at least one caller cited above. The shim's correctness criterion is that all of them continue to hold under both dispatchers.

## 3. Shim Surface

### 3.1 PUBLIC (unchanged)

The following remain exactly as today. Same signatures, same `Send + 'static` (or `Send + Sync` where currently advertised), same lifetimes, same error types (`io::Error`).

```rust
// crates/rsync_io/src/ssh/connection.rs - unchanged surface
pub struct SshConnection { /* fields are private */ }
pub struct SshChildHandle { /* fields are private */ }
pub struct SshReader { /* private */ }
pub struct SshWriter { /* private */ }

impl SshConnection {
    pub fn cancel_connect_watchdog(&mut self) -> io::Result<()>;
    pub fn stderr_output(&self) -> Vec<u8>;
    pub fn close_stdin(&mut self) -> io::Result<()>;
    pub fn wait(self) -> io::Result<ExitStatus>;
    pub fn wait_with_stderr(self) -> io::Result<(ExitStatus, Vec<u8>)>;
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    pub fn split(self) -> io::Result<(SshReader, SshWriter, SshChildHandle)>;
}

impl Read for SshConnection { ... }
impl Write for SshConnection { ... }
impl Drop for SshConnection { ... }

impl SshChildHandle {
    pub fn cancel_connect_watchdog(&mut self) -> io::Result<()>;
    pub fn stderr_output(&self) -> Vec<u8>;
    pub fn wait(self) -> io::Result<ExitStatus>;
    pub fn wait_with_stderr(self) -> io::Result<(ExitStatus, Vec<u8>)>;
}

impl Drop for SshChildHandle { ... }

impl Read for SshReader { ... }
impl Write for SshWriter { ... }
impl SshWriter { pub fn close(self) -> io::Result<()>; }

// crates/rsync_io/src/ssh/connect.rs - unchanged surface
impl SshConnection {
    pub fn connect_with_config(remote: &str, config: &SshConnectConfig) -> io::Result<Self>;
}

// crates/rsync_io/src/ssh/builder.rs - unchanged surface
impl SshCommand {
    pub fn spawn(&self) -> io::Result<SshConnection>;
}
```

### 3.2 INTERNAL (new)

New types under `crates/rsync_io/src/ssh/dispatch/` (or sibling to `embedded/`; placement is RUSSH-11's call, surface is fixed here):

```rust
/// Dispatch backend that owns the wiring between the public sync handles
/// (`SshConnection`, `SshChildHandle`, `SshReader`, `SshWriter`) and the
/// underlying transport (subprocess pipes today, russh::Channel under
/// async-native).
pub(crate) trait AsyncSshDispatch: Send + Sync + 'static {
    /// Wait for the underlying transport to exit, draining any pending
    /// I/O. Blocks the calling thread. Returns the exit status as an
    /// `ExitStatus`-compatible value (subprocess path returns the real
    /// `ExitStatus`; russh path synthesises one from the channel close
    /// code, see Section 5).
    fn wait_blocking(&mut self) -> io::Result<ExitStatus>;

    /// Non-blocking poll for completion. `Ok(None)` if still running.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;

    /// Snapshot of stderr collected so far. Non-blocking, non-consuming.
    fn stderr_snapshot(&self) -> Vec<u8>;

    /// Cancel the connection-establishment watchdog, if armed.
    fn cancel_connect_watchdog(&mut self) -> io::Result<()>;

    /// Drain the in-flight outbound buffer, signal EOF on the underlying
    /// transport, and close it cleanly. Called by `Drop` and by the
    /// blocking `wait*` methods. Idempotent.
    fn close_outbound(&mut self) -> io::Result<()>;
}

/// Identifies which dispatch backend a `SshConnection` is using. The
/// public surface never exposes this enum; it is observable only
/// internally and via `#[cfg(test)]` accessors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchKind {
    /// Today's `std::process::Command` + blocking-thread bridge.
    SpawnBlocking,
    /// RUSSH-11's `std::thread` + shared-tokio-runtime + russh path.
    AsyncNative,
}

/// Construction-time config that callers (today: `SshCommand::spawn`,
/// `SshConnection::connect_with_config`) feed to the dispatch factory.
/// Built from env vars + Cargo feature flags; not part of the public
/// surface.
#[derive(Clone, Debug)]
pub(crate) struct DispatchConfig {
    pub kind: DispatchKind,
    pub connect_timeout: Option<Duration>,
    pub outbound_capacity: usize,
    pub inbound_capacity: usize,
    pub max_chunk_bytes: usize,
    pub goodbye_drain_timeout: Duration,
}

impl DispatchConfig {
    /// Builder entry point. Reads `OC_RSYNC_SSH_DISPATCH`,
    /// `OC_RSYNC_SSH_CHANNEL_CAP`, `OC_RSYNC_SSH_CHUNK_BYTES` and
    /// applies feature-gated defaults. Fails fast if the env value is
    /// `async_native` but the `russh-async-native` Cargo feature is OFF.
    pub(crate) fn from_env() -> io::Result<Self> { ... }
}
```

### 3.3 Field placement on `SshConnection`

`SshConnection` gains a single new private field:

```rust
pub struct SshConnection {
    dispatch: Box<dyn AsyncSshDispatch + Send + Sync>,
    /// Existing fields move into `SpawnBlockingDispatch` for that
    /// dispatcher's internal use. The struct shape from the outside is
    /// unchanged.
}
```

Equivalent change on `SshChildHandle`:

```rust
pub struct SshChildHandle {
    dispatch: Box<dyn AsyncSshDispatch + Send + Sync>,
}
```

`SshReader` and `SshWriter` gain trait-object backing only on the async-native path; on the spawn_blocking path they wrap `ChildStdout` / `ChildStdin` directly as today. The exact form is an internal enum:

```rust
pub struct SshReader {
    inner: ReaderInner,
}

enum ReaderInner {
    Subprocess(ChildStdout),
    AsyncChannel(SyncReader),  // mpsc-backed; impl Read via blocking_recv
}
```

Same shape for `SshWriter` / `WriterInner`.

### 3.4 Trait-object vs enum dispatch decision

**Pick: `Box<dyn AsyncSshDispatch>` for `SshConnection`/`SshChildHandle`, enum for `SshReader`/`SshWriter`.**

Justification:

- `SshConnection` and `SshChildHandle` are constructed once per session and used across long-lived blocking calls (`wait`, `wait_with_stderr`). One vtable lookup per session-level method call is unmeasurable.
- `SshReader`/`SshWriter` are on the per-chunk data hot path. A vtable dispatch on every `read`/`write` of a 32 KiB chunk would be 100K calls/sec on a fast SSD-to-network transfer. Inlining matters: enum dispatch keeps the match in a branch the optimizer can speculate.
- Concrete benchmark target: the SyncReader/SyncWriter inner ops must inline through the `Read`/`Write` impl. `match` over a small enum is the canonical pattern; `Box<dyn Read>` adds a vtable hop that prior bench work in adjacent paths (see [[project_finish_file_arc_unwrap_ergonomics]]'s SlotHandle critique) has shown to be measurable on tight loops.

If the trait-object cost on `SshConnection` itself turns out to be measurable (rollback criterion in Section 8), the fallback is enum dispatch there too. The split chosen here optimises for the path that matters per byte.

### 3.5 Construction flow

`SshCommand::spawn()` and `SshConnection::connect_with_config()` become:

```rust
pub fn spawn(&self) -> io::Result<SshConnection> {
    let dispatch_cfg = DispatchConfig::from_env()?;
    match dispatch_cfg.kind {
        DispatchKind::SpawnBlocking => self.spawn_via_subprocess(dispatch_cfg),
        #[cfg(feature = "russh-async-native")]
        DispatchKind::AsyncNative   => self.spawn_via_russh(dispatch_cfg),
    }
}
```

Both internal arms return a fully constructed `SshConnection` with the appropriate `Box<dyn AsyncSshDispatch>` populated. The public signature is identical.

No public constructor is added. No new public error type is exposed: the `from_env` failure path maps to `io::Error::new(io::ErrorKind::InvalidInput, ...)`.

## 4. State Transition Semantics

For every public method, the shim guarantees that both dispatchers honour the same observable contract.

### 4.1 `SshCommand::spawn()` / `SshConnection::connect_with_config()`

Both dispatchers complete synchronously from the caller's POV.

- `SpawnBlockingDispatch`: as today - spawns the subprocess via `std::process::Command::spawn`, returns once the OS has forked/execed. Subsequent connection establishment is asynchronous from a wall-clock POV, gated by the connect watchdog.
- `AsyncNativeDispatch`: blocks the calling thread on `Handle::current().block_on(russh_connect(...))` (or, if no runtime is current, on an ad-hoc `tokio::runtime::Builder::new_current_thread()` driver). Returns once the russh channel is established. The connect watchdog is replaced by a tokio `timeout(...)` around the connect future; semantically equivalent.

Both return `io::Result<SshConnection>`. Error mapping: Section 5.

### 4.2 `SshConnection::split()`

Both dispatchers return `(SshReader, SshWriter, SshChildHandle)`. Each of the three is `Send + 'static`.

- `SpawnBlockingDispatch`: as today - hands out `ChildStdout` / `ChildStdin` and constructs `SshChildHandle` from the owned `Child`.
- `AsyncNativeDispatch`: hands out mpsc-backed `SyncReader` / `SyncWriter` wrappers and a `SshChildHandle` whose internal dispatch wraps the tokio task that owns the russh channel.

The reader/writer enum (Section 3.3) keeps the public `Read`/`Write` impl monomorphic per call site even though the construction path differs.

### 4.3 `SshConnection::wait()` and `SshChildHandle::wait()`

Both block the calling thread until the underlying transport reports exit. The sync-from-caller-POV invariant is non-negotiable - every existing caller in Section 2 assumes blocking semantics.

- `SpawnBlockingDispatch`: as today - `Child::wait()` on the subprocess.
- `AsyncNativeDispatch`: `oneshot::Receiver::blocking_recv()` on the dispatch handle's completion oneshot. The oneshot fires only after both russh-channel pumps have drained and the channel is closed (per RUSSH-9 Section 6.2).

Both return `io::Result<ExitStatus>`. The async-native path synthesises an `ExitStatus` from the russh channel close code (Section 5).

### 4.4 `SshConnection::wait_with_stderr()` and `SshChildHandle::wait_with_stderr()`

Both block, consume `self`, return `io::Result<(ExitStatus, Vec<u8>)>`.

- `SpawnBlockingDispatch`: as today - `Child::wait()` + `BoxedStderrChannel::collected()`.
- `AsyncNativeDispatch`: `blocking_recv()` on the completion oneshot, then snapshot of the dispatch's collected stderr buffer. The russh path collects stderr via `russh::Channel::ExtendedData(stream_id=1, ...)`; the shim drains it into the same in-memory ring buffer the spawn_blocking path uses, so `collected()` returns byte-identical content for byte-identical remote stderr output.

### 4.5 `SshConnection::cancel_connect_watchdog()` / `SshChildHandle::cancel_connect_watchdog()`

Both mutate in place via `&mut self`, return `io::Result<()>`.

- `SpawnBlockingDispatch`: as today - cancels the `ConnectWatchdog` thread; returns `Err(TimedOut)` if it already fired.
- `AsyncNativeDispatch`: aborts the tokio `timeout(...)` future via a `CancellationToken::cancel()`; if the timeout already elapsed, returns `Err(TimedOut)` with the same message format. After cancellation, subsequent reads on the connection MUST surface `Ok(())` from `cancel_connect_watchdog`; this matches the spawn_blocking behaviour.

### 4.6 `SshConnection::stderr_output()` / `SshChildHandle::stderr_output()`

Both `&self`, non-blocking, non-consuming snapshot. Both return `Vec<u8>` bounded to the most recent 64 KiB (`ASYNC_STDERR_BUFFER_CAP`).

- `SpawnBlockingDispatch`: as today - copies from the in-memory ring buffer maintained by the stderr drain thread.
- `AsyncNativeDispatch`: copies from the same ring buffer maintained by the russh ExtendedData pump task. Same buffer size, same overflow policy, same byte content for equivalent remote output.

### 4.7 `SshConnection::try_wait()`

Both `&mut self`, non-blocking, return `io::Result<Option<ExitStatus>>`.

- `SpawnBlockingDispatch`: `Child::try_wait()`.
- `AsyncNativeDispatch`: `oneshot::Receiver::try_recv()` on the completion oneshot; `Ok(None)` if still pending.

### 4.8 `SshReader: Read` / `SshWriter: Write` / `SshWriter::close()`

Both are blocking sync I/O.

- `SpawnBlockingDispatch`: direct `ChildStdout::read` / `ChildStdin::write` / `ChildStdin::flush` (close = flush).
- `AsyncNativeDispatch`: `SyncReader::read` is `mpsc::Receiver::blocking_recv` + buffered slicing; `SyncWriter::write` is `mpsc::Sender::blocking_send`; `SyncWriter::close` drops the sender, triggering the goodbye-phase barrier in the outbound pump (RUSSH-9 Section 6.2).

### 4.9 Drop semantics

Both dispatchers MUST:

1. Drain pending bytes from any internal buffer.
2. Close the outbound channel cleanly (send EOF to remote).
3. Reap the child / abort the tokio task.
4. Surface collected stderr on error (matches today's `SshConnection::Drop` / `SshChildHandle::Drop`).

The shim funnels this through `AsyncSshDispatch::close_outbound()`, which is called from both `Drop` impls and from the blocking `wait*` methods. Both implementations of `close_outbound` are idempotent.

Specific async-native drop ordering:

1. `Drop` calls `dispatch.close_outbound()`.
2. `close_outbound` drops the outbound mpsc sender; the outbound pump observes channel-close, drains `try_recv` residue, issues `russh::Channel::eof().await`, then `russh::Channel::close().await`.
3. The completion oneshot fires.
4. Drop awaits the oneshot via `blocking_recv` with a bounded `goodbye_drain_timeout` (default 30 s; same default as RUSSH-9 Section 5).
5. On timeout, drop logs a warning, aborts the russh task, and proceeds; this matches the spawn_blocking path's `Child::kill()` fallback when `wait()` would block on a hung subprocess.

This mirrors the `finish_file` Arc-ownership precedent from [[project_finish_file_arc_unwrap_ergonomics]]: an explicit drain barrier on the public type's Drop, with a bounded timeout fallback to prevent indefinite hangs.

## 5. Error Mapping

Both dispatchers MUST return identical `io::Error` variants for equivalent failure modes. The shim is responsible for mapping russh-native error types into `io::Error` instances that match the spawn_blocking path byte-for-byte where possible, and that share an `ErrorKind` where the message necessarily differs.

| Failure mode | spawn_blocking | async_native | Reconciliation |
|--------------|----------------|--------------|----------------|
| Connection refused | `io::Error { kind: ConnectionRefused, msg: "ssh: connect to host X port N: Connection refused" }` (from ssh client stderr surfaced via wait_with_stderr) | `russh::Error::ConnectionFailed(ConnectionReset)` mapped to `io::Error { kind: ConnectionRefused, msg: "ssh: connect to host X port N: Connection refused" }` | Same `ErrorKind`; message constructed from the russh failure cause + host/port. |
| Auth failure | Surfaced via subprocess exit code 255 + stderr "Permission denied (publickey)"; `wait_with_stderr` returns the stderr text | `russh::Error::AuthFailed` mapped to `io::Error { kind: PermissionDenied, msg: "ssh: Permission denied (publickey)" }` plus the same text appended to the stderr buffer | `ErrorKind::PermissionDenied` for the russh path; `ExitStatus::from_raw(255 << 8)` synthesised so `map_child_exit_status()` (caller-side) routes to the same exit code. |
| Clean EOF (remote closed normally) | Reader returns `Ok(0)`; `wait` returns `ExitStatus::from_raw(0)` | Inbound pump observes `russh::ChannelMsg::Eof`, closes mpsc; reader returns `Ok(0)`; completion oneshot fires with `CloseReason::InboundEof`; `wait` returns synthesised `ExitStatus::from_raw(0)` | Identical observable behaviour. |
| Signal-induced child death | `ExitStatus::signal() == Some(N)`; `wait_with_stderr` returns the kill signal | russh reports `ExitSignal { signal_name, .. }`; shim maps to `ExitStatus::from_raw(N << 8 \| 0x80)` matching the `wait()` POSIX convention | Same `i32` representation; `map_child_exit_status()` produces the same `ExitCode::Killed`. |
| Connect timeout (watchdog fired) | `io::Error { kind: TimedOut, msg: "ssh connection establishment timed out after N seconds" }` from `ConnectWatchdog::cancel` | `tokio::time::error::Elapsed` mapped to `io::Error { kind: TimedOut, msg: "ssh connection establishment timed out after N seconds" }` with the same N | Identical message string + ErrorKind. |
| Goodbye-phase drain failure (outbound EOF/close errored) | `Child::kill()` fallback on Drop; `wait` returns the OS-reported status | `russh::Channel::eof()` or `close()` returns error; mapped to `io::Error { kind: BrokenPipe, msg: "ssh goodbye drain failed: <cause>" }`; completion oneshot fires with `CloseReason::PumpError` | Both surface as non-zero exit / `wait` error. Async path's message is more specific by design - it is a russh-layer signal, not just a subprocess exit. |
| Channel close failure (post-EOF russh::close errored) | Not applicable (subprocess just dies; status is captured) | `io::Error { kind: BrokenPipe, msg: "ssh channel close failed: <cause>" }`; treated as warning, does not fail `wait` | The shim downgrades close-after-EOF errors to a log line; the transfer already succeeded. |
| Underlying I/O error during transfer | Reader/writer returns `io::Error` from `read`/`write` directly | `SyncReader`/`SyncWriter` returns `io::Error { kind: <propagated from pump>, msg: <propagated> }`; pump errors are stamped with `BrokenPipe` if the russh channel is the root cause, else propagated verbatim | Same `ErrorKind` where determinable. |

ExitStatus synthesis for the async-native path uses `std::os::unix::process::ExitStatusExt::from_raw` on Unix and `std::os::windows::process::ExitStatusExt::from_raw` on Windows. The shim picks a raw value that round-trips through `map_child_exit_status()` to produce the same `ExitCode` the spawn_blocking path would produce for the same remote exit code or signal.

The shim does NOT introduce any new public error type or variant. All errors flow through `io::Error` to keep the public surface identical.

## 6. Performance Regression Budget

The shim itself adds:

| Cost | Per call | Per session | Amortised per byte |
|------|----------|-------------|--------------------|
| `Arc` clone in `SshCommand::spawn` factory | 1 | 1 | 0 |
| Atomic load to consult `DispatchKind` in the spawn factory | 1 | 1 | 0 |
| Vtable lookup on `Box<dyn AsyncSshDispatch>` method call | 1 per `wait` / `wait_with_stderr` / `cancel_connect_watchdog` / `stderr_output` / `try_wait` / `close_outbound` | ~5 calls | 0 |
| Enum branch on `SshReader::read` / `SshWriter::write` | 1 | N (one per chunk) | sub-nanosecond on a hot branch the optimizer speculates correctly |
| Allocation on data hot path | 0 | 0 | 0 |

The shim MUST NOT add new allocations on the data hot path. The enum branch in `SshReader`/`SshWriter` is the only per-chunk cost, and it is by design: the alternative (trait-object dispatch per chunk) would add a vtable hop that prior bench work has flagged as measurable on tight `read`/`write` loops.

Per-byte data-path overhead beyond the enum branch is owned by RUSSH-9's mpsc design, not by this shim. The 5-10% single-stream slowdown projected in RUSSH-9 Section 4 stays in RUSSH-9's budget. The shim's own contribution is sub-1% by construction.

## 7. Migration Tests

The following tests MUST exist in `crates/rsync_io/tests/` BEFORE RUSSH-11 lands, gated on the `russh-async-native` feature for the async-native variants:

### 7.1 Public-surface compile + behaviour test

```rust
// crates/rsync_io/tests/ssh_dispatch_shim_compat.rs

/// Exercises every public method on SshConnection / SshChildHandle via
/// the same code path, switching dispatcher at the env-var level. Must
/// compile and pass identically across both feature flag settings.
#[test]
fn shim_preserves_public_surface_under_each_dispatcher() {
    for dispatch in &["spawn_blocking", "async_native"] {
        if *dispatch == "async_native" && !cfg!(feature = "russh-async-native") {
            continue;  // feature off, skip the async variant
        }
        with_env("OC_RSYNC_SSH_DISPATCH", dispatch, || {
            let mut conn = SshCommand::new("localhost")
                .set_program("/bin/cat")
                .spawn()
                .expect("spawn");

            // open -> split -> wait_with_stderr round trip
            let (mut reader, mut writer, handle) = conn.split().expect("split");
            writer.write_all(b"hello").expect("write");
            writer.close().expect("close");
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).expect("read");
            assert_eq!(buf, b"hello");
            let (status, stderr) = handle.wait_with_stderr().expect("wait");
            assert!(status.success());
            assert!(stderr.is_empty());
        });
    }
}
```

Variants of the above MUST cover:

- `wait` (no stderr) instead of `wait_with_stderr`.
- `try_wait` polling loop.
- `cancel_connect_watchdog` on a configured short timeout.
- `stderr_output` snapshot mid-transfer.
- Drop without explicit `wait` (verify the child is reaped, no zombie).

### 7.2 Trait-object safety test

```rust
// crates/rsync_io/src/ssh/dispatch/mod.rs - inline test
#[cfg(test)]
mod safety {
    use super::AsyncSshDispatch;
    fn _assert_trait_object_safe(_: &dyn AsyncSshDispatch) {}
}
```

This forces a compile-time check that `AsyncSshDispatch` is dyn-compatible. Any associated type, `Self`-returning method, or generic method on the trait would break this check.

### 7.3 What this test suite is NOT

This is a structural / shape test. It does not validate wire-byte parity - that is RUSSH-12's responsibility. A pass here means the public surface compiles and the dispatcher selection works end-to-end; it does NOT mean the async-native pump produces byte-identical wire output. The full goldens + interop suite must run separately under RUSSH-12 before the default flips.

## 8. Rollback Criteria

If the shim itself causes problems independent of the underlying async-native impl, the rollback path depends on which symptom appears.

| Symptom | Trigger | Rollback action |
|---------|---------|-----------------|
| Public caller breaks at compile time | Any of the call sites in Section 2 stops building against the new `SshConnection` / `SshChildHandle` field layout | **Revert the shim.** Re-architect with a fully separate `AsyncSshConnection` / `AsyncSshChildHandle` type alongside the existing types. Last-resort path; doubles the public surface but isolates the experimental backend completely. |
| Trait-object dispatch adds measurable overhead | Single-session bench (1 SSH stream, 1 GiB transfer) shows > 1% throughput regression vs pre-shim baseline, attributable to the `Box<dyn AsyncSshDispatch>` on `SshConnection` | Switch from `Box<dyn AsyncSshDispatch>` to a `DispatchKind`-tagged enum on `SshConnection` (and same on `SshChildHandle`). Re-bench. If still regressed, revert per row 1. |
| `Send`/`Sync` bound mismatch | The shim's trait object is `Send + Sync + 'static`; if a future RUSSH-11 implementation needs interior `!Sync` state, the bound becomes infectious | Relax `AsyncSshDispatch: Send + Sync` to `AsyncSshDispatch: Send` and verify all callers via the public surface still satisfy `Send` only. The public types are `Send` today, not `Sync`, so this relaxation does not break the public contract. |
| Construction error mapping diverges between dispatchers | `DispatchConfig::from_env` returns a `kind` that requires a feature that isn't enabled | Fail fast with `io::Error { kind: InvalidInput }` carrying a clear "feature `russh-async-native` not enabled" message; do not silently fall back. The caller should see the misconfiguration, not a runtime surprise mid-transfer. |
| Drop hangs under async-native | The 30 s `goodbye_drain_timeout` is hit on a real workload | Lower the default to 10 s, log warning + abort the russh task. If the symptom is endemic (more than 0.1% of sessions), revert the default to `spawn_blocking` per RUSSH-9 Section 8 rollback mechanics. |

Code rollback is a one-line change: flip `DispatchConfig::from_env`'s default from `DispatchKind::AsyncNative` (post-flip) back to `DispatchKind::SpawnBlocking`. The shim itself stays in tree; the failure modes above all preserve the shim and adjust the inner dispatcher or its config.

If the shim is reverted entirely (row 1), the async-native work re-architects around a separate `AsyncSshConnection` type. That is the worst case; every other failure mode is recoverable without touching the public surface.

## 9. Cross-Links

- Parent design: [`docs/design/russh-async-native-path.md`](./russh-async-native-path.md) (RUSSH-9, #2812 / PR #4912).
- [[project_russh_spawn_blocking_ceiling]] - root bottleneck; the reason the async-native dispatcher exists and the reason this shim must hide it cleanly.
- [[project_no_async_threaded_only]] - architectural constraint: transfer pipeline stays threaded; only the boundary becomes pluggable. The shim enforces this by keeping `SshReader`/`SshWriter` as blocking sync `Read`/`Write`.
- [[project_ssh_stderr_socketpair_silent_fallback]] - stderr drain ordering carries forward into the async-native path's ExtendedData pump; Section 4.4 / 4.6 / 4.9 preserve the existing semantics.
- [[project_finish_file_arc_unwrap_ergonomics]] - precedent for explicit drain barrier on Drop with bounded-timeout fallback; Section 4.9 follows this pattern.
- RUSSH-1 (#2804) - spawn_blocking call-site inventory (`docs/audit/russh-spawn-blocking-ceiling-inventory.md`).
- RUSSH-9 (#2812 / PR #4912) - parent design.
- RUSSH-11 (#2814) - implementation of `AsyncNativeDispatch` against the shim trait.
- RUSSH-12 (#2815) - wire-byte parity validation across both dispatchers.
- RUSSH-13 - re-bench at 64/128/256/512/1024/2048 concurrent sessions.
- RUSSH-14 - adopt-or-defer decision for the async-native default.
