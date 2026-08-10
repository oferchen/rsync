# SSH stderr handling audit (SSE-1, #2370)

Tracker: #2370. No code changes - documentation only.

Scope: catalogue every call site in `crates/rsync_io/src/ssh/` that
creates, configures, drains, surfaces, or shuts down the SSH subprocess
stderr stream. Feeds the design doc `docs/design/socketpair-stderr-channel.md`
(SSE-2, #2371) and the implementation tasks SSE-3..SSE-7.

Related audits already on disk (do not repeat their content here):

- `docs/audits/ssh-socketpair-claim-verification.md` (#1902) - wire vs
  stderr topology confirmation.
- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - rationale for keeping
  anonymous pipes on the wire.
- `docs/audits/ssh-process-management.md` - lifecycle of `SshChildHandle`
  and zombie-reap invariants.

## 1. Module map

`crates/rsync_io/src/ssh/`:

| File                  | Role for stderr                                                                 |
|-----------------------|---------------------------------------------------------------------------------|
| `aux_channel.rs`      | The trait `StderrAuxChannel`, both backends, drain loop, factory functions.     |
| `builder.rs`          | The single sync spawn site that wires stderr through the factory functions.    |
| `connection.rs`       | Owns `BoxedStderrChannel` on `SshConnection` and `SshChildHandle`; joins/surfaces. |
| `async_transport.rs`  | Async (tokio) spawn site - currently bypasses the channel entirely.            |
| `mod.rs`              | Re-exports; comment block records the socketpair-on-Unix decision.             |
| `connect.rs`          | Argv composition only; never touches stderr.                                   |

## 2. Sync path: configuration -> drain -> surface

### 2.1 Stderr endpoint configuration

`aux_channel.rs:337-365` (`configure_stderr_channel`)

- Unix: calls `UnixStream::pair()`. On success, converts the child end via
  `OwnedFd -> Stdio` (safe stdlib path, no FFI) and installs it as the
  child's stderr; returns the parent end. On failure, falls back to
  `Stdio::piped()` and returns `None`.
- Non-Unix: unconditionally `Stdio::piped()`, returns `None`.

Sole caller: `builder.rs:339` inside `SshCommand::spawn`.

### 2.2 Channel construction

`aux_channel.rs:367-390` (`build_stderr_channel`)

- `Some(parent_socketpair_end)` -> `SocketpairStderrChannel::spawn` (Unix
  only).
- Otherwise -> wraps the child's `ChildStderr` in `PipeStderrChannel::spawn`.
- Both backends spawn a dedicated drain thread named
  `ssh-stderr-drain-socketpair` or `ssh-stderr-drain-pipe`.

Sole caller: `builder.rs:358`.

### 2.3 Drain loop

`aux_channel.rs:282-302` (`drain_loop<R: Read>`)

- `BufReader::new(source)` wrapping either `ChildStderr` or `UnixStream`.
- Reads with `read_until(b'\n', ...)` rather than `lines()` so non-UTF-8
  payloads do not abort the drain (locale-encoded SSH messages, banner
  bytes, etc.).
- Forwards each line to the parent process stderr in real time via
  `eprint!` plus a `debug_log!(Connect, 3, ...)`.
- Appends to a bounded buffer (sliding-window) via `append_bounded`.
- Exits on `Ok(0)` (EOF) or on any `Err(_)` (broken pipe, child exited).

### 2.4 Bounded buffer

`aux_channel.rs:65-70, 304-321`

- `STDERR_BUFFER_CAP = 64 * 1024` (matches typical OS pipe buffer).
- `append_bounded` drops oldest bytes when the total exceeds the cap.
- `snapshot` returns a `Vec<u8>` clone under the `Mutex` for read-side
  access (`collected()`).

### 2.5 Shutdown and join policy

`aux_channel.rs:42-61, 95-110, 165-191, 238-267`

- `DRAIN_JOIN_TIMEOUT = 50ms`. `join_with_timeout` polls
  `JoinHandle::is_finished` then either joins or **leaks the
  `JoinHandle`**. The deliberate leak handles the case where an ssh
  helper subprocess (ssh-askpass, ControlMaster persistence) inherits
  the write end and EOF therefore lags the parent's exit arbitrarily.
- `PipeStderrChannel::shutdown_read` is a no-op (anonymous pipes have no
  out-of-band wake-up); the bounded `join_with_timeout` is the safety
  net.
- `SocketpairStderrChannel::shutdown_read` calls
  `sock.shutdown(Both)` on a `try_clone` of the parent endpoint; the
  drain thread's parked `read()` returns 0 immediately, the loop exits,
  and `join` completes.
- Both backends are `Drop`-safe and `join` is idempotent.

### 2.6 Surface-on-error

`aux_channel.rs:118-134` (`StderrAuxChannel::join_and_surface_on_error`)

- Default trait method, used from `Drop` impls on `SshConnection`
  (`connection.rs:577-585`) and `SshChildHandle` (`connection.rs:510-512`).
- Shuts down the read end, joins the drain, and if `status.is_err() ==
  false && !status.success()`, writes the captured bytes to local
  stderr with `eprintln!("ssh process exited with status {exit}:\n{trimmed}")`.

### 2.7 Owners

- `SshConnection.stderr_drain: Option<BoxedStderrChannel>`
  (`connection.rs:37`).
- `SshChildHandle.stderr_drain: Option<BoxedStderrChannel>`
  (`connection.rs:398`).
- Transferred on `SshConnection::split` (`connection.rs:205-213`).
- Inspected via `stderr_output()` on both types
  (`connection.rs:90-94, 446-451`).
- Joined inside `wait` and `wait_with_stderr` on both types
  (`connection.rs:105-160, 457-490`).

## 3. Async path: stderr today

`async_transport.rs:53, 116-122`

- Uses `tokio::process::Command`.
- `command.stderr(Stdio::inherit())` - the child's stderr is wired
  straight through to the parent's fd 2.
- No drain task. No bounded buffer. No surface-on-error policy.
- No way for the caller to retrieve "the stderr we just printed".
- `kill_on_drop(true)` reaps the child but does not capture stderr.

Doc comment at `async_transport.rs:18-21` explicitly records that the
async drain and connect-watchdog are deferred. This audit confirms the
deferral: every behaviour the sync path documents (real-time forwarding,
bounded capture, surface-on-error, deadlock avoidance) is absent on the
async path. The synchronous and asynchronous transports are not
behaviourally equivalent for stderr today.

## 4. Behaviour matrix

| Concern                                  | Sync (`builder.rs::spawn`) | Async (`async_transport.rs`)        |
|------------------------------------------|----------------------------|--------------------------------------|
| Endpoint                                 | socketpair (Unix) / pipe   | parent fd 2 (inherit)                |
| Drain                                    | dedicated thread           | none (kernel copies to terminal)     |
| Real-time forwarding                     | per-line `eprint!`         | direct write by child to parent fd 2 |
| Bounded capture                          | 64 KiB sliding window      | none                                 |
| Snapshot accessor                        | `stderr_output()`          | none                                 |
| Surface-on-error                         | `Drop` impl                | none                                 |
| Multi-line buffering                     | `read_until(b'\n')`        | n/a                                  |
| Wake on shutdown                         | `Shutdown::Both` (socket); timeout (pipe) | n/a                   |
| Bounded join                             | 50 ms timeout, then leak   | n/a                                  |
| Cross-platform                           | yes (pipe fallback)        | yes (inherit on all platforms)       |
| Event-loop integrable                    | socket-only, not wired in  | no                                   |

## 5. Findings

1. The sync path already uses `socketpair(AF_UNIX, SOCK_STREAM, 0)` on
   Unix; only the parent-side **read strategy** (a parked drain thread)
   is the legacy bit. The socket descriptor is the right primitive for
   future event-loop registration.
2. The pipe fallback is non-removable: it is the only stderr path on
   Windows and the FD-exhaustion contingency on Unix.
3. The async transport does not participate in any stderr policy. Adding
   parity is the SSE-3..SSE-7 work.
4. The drain thread per connection is the only thread the sync stderr
   path spawns. Moving it onto an existing event loop removes one
   thread per concurrent SSH transfer.
5. Multi-line buffering is line-delimited (`read_until(b'\n')`) and
   `String::from_utf8_lossy` is used at print time; binary payloads are
   captured but rendered with replacement characters. This is upstream-
   matching behaviour and must be preserved by any new design.
6. `join_and_surface_on_error` is the only entry that writes to local
   stderr from a `Drop` impl; new designs must keep its idempotency
   (drain `take()`s the `JoinHandle`).

## 6. Call-site index (file:line)

| Concern                          | Site                                                   |
|----------------------------------|--------------------------------------------------------|
| Trait + buffer cap               | `crates/rsync_io/src/ssh/aux_channel.rs:65,87`         |
| Pipe backend spawn               | `crates/rsync_io/src/ssh/aux_channel.rs:147-163`       |
| Pipe backend shutdown            | `crates/rsync_io/src/ssh/aux_channel.rs:170-184`       |
| Socketpair backend spawn         | `crates/rsync_io/src/ssh/aux_channel.rs:211-236`       |
| Socketpair backend shutdown      | `crates/rsync_io/src/ssh/aux_channel.rs:244-259`       |
| Drain loop                       | `crates/rsync_io/src/ssh/aux_channel.rs:282-302`       |
| Bounded append                   | `crates/rsync_io/src/ssh/aux_channel.rs:306-316`       |
| Surface-on-error                 | `crates/rsync_io/src/ssh/aux_channel.rs:118-134`       |
| Endpoint configurator            | `crates/rsync_io/src/ssh/aux_channel.rs:337-365`       |
| Channel factory                  | `crates/rsync_io/src/ssh/aux_channel.rs:372-390`       |
| Sync spawn wiring                | `crates/rsync_io/src/ssh/builder.rs:308-367`           |
| Owner on connection              | `crates/rsync_io/src/ssh/connection.rs:30-69`          |
| Snapshot accessor (conn)         | `crates/rsync_io/src/ssh/connection.rs:89-94`          |
| Drain shutdown + join in wait    | `crates/rsync_io/src/ssh/connection.rs:112-128`        |
| wait_with_stderr                 | `crates/rsync_io/src/ssh/connection.rs:136-160`        |
| Transfer drain to child handle   | `crates/rsync_io/src/ssh/connection.rs:205-213`        |
| Drop surface (conn)              | `crates/rsync_io/src/ssh/connection.rs:562-585`        |
| Owner on child handle            | `crates/rsync_io/src/ssh/connection.rs:396-400`        |
| Snapshot accessor (handle)       | `crates/rsync_io/src/ssh/connection.rs:446-451`        |
| Drain shutdown + join (handle)   | `crates/rsync_io/src/ssh/connection.rs:457-489`        |
| Drop surface (handle)            | `crates/rsync_io/src/ssh/connection.rs:492-513`        |
| Async stderr (inherit)           | `crates/rsync_io/src/ssh/async_transport.rs:118`       |
| Async deferral note              | `crates/rsync_io/src/ssh/async_transport.rs:18-21`     |
