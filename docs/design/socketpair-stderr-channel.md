# Socketpair stderr channel design (SSE-2, #2371)

Tracker: #2371. Companion to `docs/audits/ssh-stderr-handling.md`
(SSE-1, #2370). No code changes in this PR.

Companion docs:

- `docs/audits/ssh-socketpair-claim-verification.md` (#1902) - confirms
  the wire is two anonymous pipes and stderr is one `AF_UNIX`
  socketpair today.
- `docs/audits/ssh-single-socketpair-bidirectional.md` (#1687) - rules
  out a single bidirectional socketpair for the wire path; this design
  inherits that conclusion and applies only to stderr.

## 1. Goal

Bring the async transport to parity with the sync transport for stderr
handling, and prepare both transports to drive the drain off a shared
event loop instead of a dedicated thread per connection.

Non-goals: replacing the wire (stdin/stdout) pipes; changing the
protocol the remote sees; changing upstream-matching real-time
forwarding semantics.

## 2. Pipe vs socketpair trade-off

Both kernel objects deliver a stream of bytes from the child's fd 2.
The trade-off matters only for the parent-side **read strategy** -
specifically how the parent waits for bytes without burning a thread.

| Property                                | Anonymous pipe (`pipe(2)`)    | UNIX socketpair (`socketpair(AF_UNIX, SOCK_STREAM, 0)`) |
|-----------------------------------------|-------------------------------|---------------------------------------------------------|
| Cross-platform availability             | Linux, macOS, Windows         | Unix only; Windows must simulate                        |
| Direction                               | Unidirectional                | Bidirectional                                           |
| Default kernel buffer                   | 64 KiB (Linux)                | ~208 KiB (Linux); platform-dependent                    |
| Back-pressure on full buffer            | child blocks in `write(2)`    | child blocks in `send(2)` (same effect)                 |
| Non-blocking with `O_NONBLOCK`          | yes                           | yes                                                     |
| Out-of-band wake (parent side)          | none (must close write end)   | `shutdown(SHUT_RD)`/`shutdown(SHUT_RDWR)`               |
| Registers with `epoll`/`kqueue`         | yes (read-only fd)            | yes (full socket semantics)                             |
| Registers with tokio `AsyncFd`          | yes (via `OwnedFd`)           | yes (via `UnixStream`)                                  |
| Registers with Windows IOCP             | named-pipe shim only          | not native; TCP loopback shim                           |
| Same primitive on the wire today        | yes (stdin, stdout)           | no                                                      |
| Failure mode if FD-exhausted at spawn   | n/a (pipe is the fallback)    | falls back to pipe                                      |

The two-line summary:

- **Pipe**: simpler, no out-of-band wake, parent must close the write
  end or rely on a bounded read timeout to unstick a stuck drain.
- **Socketpair**: bidirectional, larger default buffer on Linux,
  integrates cleanly with epoll/kqueue/IOCP-style event loops, supports
  `shutdown(2)` as the safe wake-up mechanism.

Conclusion: keep the existing dual-backend design. Socketpair on Unix,
pipe fallback on Windows and on FD-exhaustion. The async transport
should consume the same trait (`StderrAuxChannel`) the sync transport
already exposes.

## 3. Cross-platform construction

### 3.1 Unix

`socketpair(AF_UNIX, SOCK_STREAM, 0)` via `std::os::unix::net::UnixStream::pair`.
Hand the child end to the spawned process as fd 2; retain the parent
end on this side. This is already implemented in
`aux_channel.rs::configure_stderr_channel`.

### 3.2 Windows

`socketpair(2)` does not exist on Win32. The behaviourally equivalent
construction is:

1. Bind a `TcpListener` to `127.0.0.1:0` with an explicit address-reuse
   policy that prevents port hijacking.
2. `connect` to the listener address from the same process.
3. `accept` the inbound connection.
4. Hand the **accepted** socket to the child by duplicating it onto its
   stderr handle via the existing Windows process-spawn API in
   `fast_io`.
5. Close the listener immediately after `accept` returns.
6. Retain the **connecting** socket on the parent side; this is the
   `AsyncRead`-capable handle.

The shim must:

- Refuse to bind to any address other than `127.0.0.1` (no external
  exposure).
- Apply a 1-second handshake timeout to defend against another local
  process racing the connection.
- Fall back to `Stdio::piped()` on any failure, identical to the Unix
  FD-exhaustion path.

The Windows construction is implemented in the existing pattern used
elsewhere in the workspace (no new unsafe in `rsync_io`). It is gated
behind the same feature flag (Section 6) until verified on the CI
Windows runner.

## 4. AsyncSshTransport integration

The async transport currently uses `Stdio::inherit()` for stderr. The
target shape is:

1. `AsyncSshTransport` gains an `Option<StderrAuxChannel>` field with
   the same role it has on `SshConnection`.
2. Construction uses the same `configure_stderr_channel` /
   `build_stderr_channel` factories the sync path uses, but with a
   tokio-aware backend variant: `TokioSocketpairStderrChannel` wraps
   `tokio::net::UnixStream` (or the Windows shim) and drives the drain
   loop as a `tokio::spawn` task instead of a `std::thread`.
3. The drain task:
   - reads with `tokio::io::AsyncBufReadExt::read_until(b'\n', ...)`,
   - forwards each line to `tokio::io::stderr()` (so it interleaves
     with other tokio writers correctly),
   - appends to the same bounded `Arc<Mutex<Vec<u8>>>` buffer
     (`STDERR_BUFFER_CAP` unchanged),
   - exits on EOF or on a `oneshot` shutdown signal.
4. `AsyncSshTransport::wait` awaits a `oneshot::Sender::send(())` to
   wake the drain task before awaiting `child.wait()`, mirroring the
   sync path's `shutdown_read` + `join` sequence.
5. `AsyncSshTransport::stderr_output()` is added with the same
   signature and semantics as `SshConnection::stderr_output()`.
6. A `warnings: tokio::sync::mpsc::UnboundedSender<StderrLine>`
   sender is offered as an optional construction-time hook so future
   callers (interactive UI, structured logger) can subscribe to each
   line as it is captured, without breaking the existing
   `stderr_output()` snapshot accessor.

The new tokio backend implements `StderrAuxChannel` so the trait stays
the single seam both transports observe. The sync path is unchanged.

## 5. Drain to ring buffer + warning channel

The existing bounded `Vec<u8>` already behaves as a 64 KiB sliding
window. The new design retains that exactly. The optional warning
channel is layered on top of the buffer write, not in place of it:

```text
read_until(b'\n')
   |
   |--> append_bounded(&buffer, &line)              // unchanged path
   |--> if let Some(tx) = warnings { tx.send(line) } // new, optional
   |--> eprint!("{lossy_text}")                     // unchanged path
```

The warning channel is `try_send`; a slow consumer never blocks the
drain. Dropped warnings increment a counter exposed through the
`stderr_output()` snapshot accessor so callers can detect loss.

## 6. Backwards compatibility

Feature flag in `crates/rsync_io/Cargo.toml`:

```toml
[features]
ssh-socketpair-stderr = []  # default off
```

- **Off (default)**: behaviour is exactly what `master` ships today.
  Sync path keeps its current dual-backend selection. Async path keeps
  `Stdio::inherit()`.
- **On**: the async path additionally constructs a
  `TokioSocketpairStderrChannel`; the sync path's selection is
  unaffected (it already uses the socketpair when available).

The flag stays default-off until SSE-7 ships parity tests for the
async path on Linux, macOS, and the Windows TCP-shim. Promotion to
default-on is a separate PR with a one-line `Cargo.toml` change.

No public type is renamed, removed, or has its signature changed by
this design. Adding `AsyncSshTransport::stderr_output()` is purely
additive.

## 7. Implementation plan (SSE-3..SSE-7)

| Task   | Scope                                                                                  | Status |
|--------|----------------------------------------------------------------------------------------|--------|
| SSE-3  | Add the feature flag and a `TokioSocketpairStderrChannel` (Unix) implementing `StderrAuxChannel` via `tokio::net::UnixStream`. No call-site changes yet; cover the new type with unit tests that mirror `socketpair_channel_collects_stderr_data`, `socketpair_channel_handles_non_utf8_bytes`, `socketpair_channel_bounded_buffer_caps_memory`. | not started |
| SSE-4  | Wire `AsyncSshTransport::execute_remote_rsync` to call `configure_stderr_channel` + `build_stderr_channel` behind the flag; add `stderr_output()`, `wait_with_stderr()`. Replace `Stdio::inherit()` only when the channel is constructed; leave the inherit path as a fallback when the factory returns `None`. | not started |
| SSE-5  | Implement the Windows TCP-loopback shim in `aux_channel.rs` under `#[cfg(windows)]`. Add bind-address and handshake-timeout safeguards; fall back to `Stdio::piped()` on any error. Cover with a Windows-only integration test that spawns a `cmd /C echo ... 1>&2` child and asserts capture. | not started |
| SSE-6  | Add the optional warning channel (`UnboundedSender<StderrLine>`) and the dropped-warning counter to the shared buffer snapshot. Extend `stderr_output()` to surface the counter as a structured side-channel without altering the existing `Vec<u8>` return shape. | not started |
| SSE-7  | Parity tests: a matrix that runs an end-to-end transfer against the local interop daemon under both flag states and asserts identical captured-stderr bytes, identical surface-on-error output, and identical exit-code propagation. Flip the flag default to on once green on Linux, macOS, Windows. | not started |

Each task is independently mergeable behind the feature flag.

## 8. Risks and mitigations

| Risk                                                                                       | Mitigation                                                                                                       |
|--------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------|
| Windows TCP shim is racy / opens an exposure surface                                       | Bind `127.0.0.1` only, handshake timeout, fall back to pipe; SSE-5 lands behind the feature flag.                |
| Tokio drain task survives past child reap (ssh-askpass / ControlMaster keeps write end)    | `oneshot` shutdown + `tokio::time::timeout` on the join, mirroring `DRAIN_JOIN_TIMEOUT` from the sync path.      |
| Warning channel back-pressure stalls the drain                                              | `try_send` only; dropped warnings counted and surfaced through the snapshot accessor.                            |
| Behavioural drift between sync and async after promotion to default-on                     | Single `StderrAuxChannel` trait; parity tests (SSE-7) run the same input through both transports and diff bytes.|
| Feature-flag combinatorial explosion in CI                                                 | Only one new flag (`ssh-socketpair-stderr`); CI matrix gains a single additional column.                         |
