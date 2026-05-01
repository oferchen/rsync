# SSH stdio transport: socketpair(AF_UNIX, SOCK_STREAM) vs anonymous pipes

Tracker: #1938 (formal audit). Branch: `docs/ssh-socketpair-1938`.
No code changes - documentation only.

Prerequisite trackers:

- #1686 (evaluate socketpair vs stdio pipes for SSH subprocess) - completed
  via PR #3438; the merged audit was the working notebook this report
  consolidates.
- #1858 (document SSH stdio limitation: io_uring socket path is unreachable
  for SSH transfers) - completed via PR #3418, recorded in
  `crates/rsync_io/src/ssh/mod.rs:57-75`.
- #1689 (add SSH subprocess stderr forwarding via socketpair auxiliary
  channel) - completed via PR #3383; the socketpair stderr code lives in
  `crates/rsync_io/src/ssh/aux_channel.rs:138-193`.

Pending follow-up trackers this audit informs:

- #1687 (prototype SSH subprocess using socketpair for bidirectional I/O).
- #1902 (verify SSH socketpair vs anonymous-pipe wire claim against
  `rsync_io` source).

Companion audits:

- `docs/audits/iouring-pipe-stdio.md` (#1859 - io_uring on pipe FDs).
- `docs/audits/splice-ssh-stdio.md` (#1860 - splice/vmsplice for SSH stdio).

## 1. Summary

Upstream rsync 3.4.1 sets up the SSH child via two `socketpair(AF_UNIX,
SOCK_STREAM, 0)` invocations (one for each direction of the wire), with a
configure-time fallback to `pipe(2)` on hosts that lack `socketpair`. Both
ends are forced non-blocking. The same `fd_pair` helper is used for the
read-batch back-channel and the receiver error pipe, so socketpair is the
default IPC primitive throughout upstream. oc-rsync currently spawns SSH
with two anonymous `pipe(2)` pairs - `Command::stdin(Stdio::piped())` and
`Command::stdout(Stdio::piped())` - for the wire, and (since #1689) a
`UnixStream::pair()` socketpair for the auxiliary stderr channel only.

The question this audit answers: should oc-rsync's SSH wire migrate to a
socketpair-backed transport to match upstream, and what concrete behaviour
changes does that unlock?

**Recommendation:** keep anonymous pipes as the cross-platform default and
do **not** pursue the socketpair migration on the wire at this time. The
behavioural differences that motivated upstream's choice (non-blocking I/O,
graceful half-close via `shutdown(SHUT_WR)`, unified poll registration) are
either already covered by oc-rsync's threaded blocking model or fall under
the larger async-transport refactor tracked by #1655 / #2068, which would
subsume the socketpair work as a small step. Pipes also keep `splice(2)`
eligibility on the file<->wire edge (#1860) and require no Windows-only
fallback path. The audit closes #1687 as "do not implement" and #1902 as
"verified: upstream uses `socketpair(AF_UNIX, SOCK_STREAM, 0)` via
`util1.c::fd_pair`; oc-rsync intentionally diverges with `pipe(2)`." All
six findings below are informational, not severity-medium-or-higher
defects.

## 2. Upstream behaviour reference

Source of truth: `target/interop/upstream-src/rsync-3.4.1/`. All citations
were verified by reading the C source.

### 2.1 SSH child setup (`pipe.c::piped_child`)

`pipe.c:48-97` is the only call site that spawns the SSH client (or any
remote-shell program selected via `RSYNC_RSH`). The structure is:

```c
pid_t piped_child(char **command, int *f_in, int *f_out)
{
    int to_child_pipe[2];
    int from_child_pipe[2];

    if (fd_pair(to_child_pipe) < 0 || fd_pair(from_child_pipe) < 0) { ... }
    pid = do_fork();
    if (pid == 0) {
        dup2(to_child_pipe[0], STDIN_FILENO);
        dup2(from_child_pipe[1], STDOUT_FILENO);
        ...
        set_blocking(STDIN_FILENO);
        if (blocking_io > 0) set_blocking(STDOUT_FILENO);
        execvp(command[0], command);
    }
    *f_in = from_child_pipe[0];
    *f_out = to_child_pipe[1];
    return pid;
}
```

Key observations:

- Two `fd_pair` calls produce four FDs total: `to_child_pipe[0..1]` for
  parent->child (stdin) and `from_child_pipe[0..1]` for child->parent
  (stdout). The parent retains `to_child_pipe[1]` (write end) and
  `from_child_pipe[0]` (read end); the child receives the opposite ends
  and `dup2`s them onto fds 0 and 1.
- The child explicitly forces blocking mode on stdin
  (`set_blocking(STDIN_FILENO)`) and on stdout when `blocking_io > 0`
  (`pipe.c:80-82`). The parent ends remain non-blocking (set by `fd_pair`
  itself, see 2.3 below).
- `local_child` (`pipe.c:99-178`) uses the same `fd_pair`-based topology
  for forked-local rsync (no SSH involved). The block comment at
  `pipe.c:99-108` explicitly names the endpoints "socket pairs" and
  documents the four-end ownership pattern.

### 2.2 `fd_pair` socketpair-or-pipe helper (`util1.c::fd_pair`)

`util1.c:74-96`:

```c
/**
 * Create a file descriptor pair - like pipe() but use socketpair if
 * possible (because of blocking issues on pipes).
 *
 * Always set non-blocking.
 */
int fd_pair(int fd[2])
{
    int ret;

#ifdef HAVE_SOCKETPAIR
    ret = socketpair(AF_UNIX, SOCK_STREAM, 0, fd);
#else
    ret = pipe(fd);
#endif

    if (ret == 0) {
        set_nonblocking(fd[0]);
        set_nonblocking(fd[1]);
    }
    return ret;
}
```

Key observations:

- The implementation is a configure-time switch. `HAVE_SOCKETPAIR` is
  defined on every modern Unix the autotools probe inspects. The pipe
  branch exists as a fallback for hosts that lack `socketpair`.
- The doc comment cites "blocking issues on pipes" as the reason
  socketpair is preferred. This refers to two upstream-relevant
  properties: `shutdown(SHUT_WR)` for orderly half-close, and the absence
  of the 64 KiB pipe-buffer fence post that can cause stalls when both
  parties write large bursts before either reads.
- Both ends are forced non-blocking unconditionally on success. The
  child re-enables blocking on its inherited stdin via `set_blocking`
  later in `piped_child` (see 2.1) because rsh/SSH binaries expect
  blocking stdin.

### 2.3 Auxiliary uses of `fd_pair`

The same helper is reused for two non-wire channels:

- `main.c:629` - read-batch back-channel from the generator to the
  client when `--read-batch` is in effect.
- `main.c:985` - receiver error pipe used by the receiver process to
  signal IPC errors back to the generator.

Both rely on the same socketpair-or-pipe semantics (`fd_pair`) and both
are non-blocking. There is no code path in upstream rsync that uses raw
`pipe(2)` directly; every IPC pair goes through `fd_pair`.

### 2.4 Daemon connection setup (`clientserver.c`)

For daemon-mode connections (`rsync://` URLs) the wire is a TCP socket,
not a stdio pipe. `clientserver.c:116-148` `start_socket_client()`:

```c
fd = open_socket_out_wrapped(host, rsync_port, bind_address, default_af_hint);
if (fd == -1)
    exit_cleanup(RERR_SOCKETIO);
...
ret = start_inband_exchange(fd, fd, user, remote_argc, remote_argv);
return ret ? ret : client_run(fd, fd, -1, argc, argv);
```

Both directions share the same socket FD (`f_in == f_out`). This is a
TCP `AF_INET` / `AF_INET6` socket via `connect(2)`, not a socketpair.
`rsync_module()` at `clientserver.c:692` is the daemon-side counterpart
and accepts the connection on the listener socket.

### 2.5 `RSYNC_CONNECT_PROG` test escape (`socket.c::sock_exec`)

`socket.c:805-846` provides a test-only escape that runs a local program
across a TCP socketpair (`socket.c:736-802` `socketpair_tcp`) so the
daemon-mode path can be exercised without a real TCP connection. This
is gated by `RSYNC_CONNECT_PROG` and is the only place upstream uses a
TCP-style socketpair built from `socket(PF_INET, SOCK_STREAM, 0)` plus
`connect`/`accept`. Not relevant to SSH transport - it substitutes for
the daemon's TCP socket, not for the SSH child stdio.

### 2.6 Multiplex envelope replaces a third FD

Upstream does not allocate a separate fd 2 channel back from the remote
rsync. Remote-rsync diagnostics flow through the multiplex envelope
(`io.c::send_msg`, see `io.c:983-1031`) wrapped in `MSG_INFO` /
`MSG_ERROR` / `MSG_WARNING` frame codes on the wire when
`iobuf.multiplex_writes` is set. Only the *local* SSH client's own
stderr (host-key warnings, `Permission denied`, banner messages) is
out-of-band and is left attached to the inherited terminal stderr.

`cleanup.c:46-67` `close_all` walks every open FD and calls
`shutdown(fd, 2)` before `close(fd)` when the FD is a socket
(gated on `SHUTDOWN_ALL_SOCKETS`). This logic exists specifically to
take advantage of the fact that `fd_pair` returns sockets - the
shutdown is a no-op on pipes.

### 2.7 Summary of upstream defaults

| Channel | Primitive | Citation |
|---|---|---|
| SSH wire (parent<->child stdio) | `socketpair(AF_UNIX, SOCK_STREAM, 0)` per direction (2 pairs) | `pipe.c:57`, `util1.c:84-85` |
| Local-fork wire | same | `pipe.c:119`, `util1.c:84-85` |
| Read-batch back-channel | same | `main.c:629`, `util1.c:84-85` |
| Receiver error pipe | same | `main.c:985`, `util1.c:84-85` |
| Daemon `rsync://` wire | TCP `AF_INET`/`AF_INET6` socket | `clientserver.c:137`, `socket.c::open_socket_out` |
| `RSYNC_CONNECT_PROG` test escape | TCP socketpair (`socket.c::socketpair_tcp`) | `socket.c:740-802`, `socket.c:811-846` |
| Remote-rsync diagnostics | multiplex envelope on the wire | `io.c:983-1031` |
| Local SSH-client stderr | inherited terminal stderr | (no code; default child stderr) |

## 3. Current oc-rsync implementation

All citations are line numbers verified against the worktree at
`crates/rsync_io/src/ssh/`.

### 3.1 SSH wire setup (`builder.rs::SshCommand::spawn`)

`crates/rsync_io/src/ssh/builder.rs:285-340` is the single spawn site:

```rust
pub fn spawn(&self) -> io::Result<SshConnection> {
    let mut command = Command::new(&program);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.args(args.iter());
    ...
    let parent_socketpair_end = configure_stderr_channel(&mut command);
    let mut child = command.spawn()?;
    ...
```

- `builder.rs:300-301` configures stdin and stdout as `Stdio::piped()`.
  Each call creates an anonymous `pipe(2)` pair internally
  (`std::sys::pal::unix::process::process_unix::AnonPipe::pipe`). The
  parent retains the write end of the stdin pipe and the read end of the
  stdout pipe; the child receives the opposite ends and the standard
  library calls `dup2` to plant them on fds 0 and 1 before `execvp`.
- `builder.rs:312` calls `configure_stderr_channel`, which on Unix
  attempts a socketpair for stderr (see 3.3) and falls back to
  `Stdio::piped()` on failure or non-Unix targets.
- The wire channels (stdin/stdout) are never reconfigured to a
  socketpair anywhere in the codebase. There is no `socketpair`,
  `UnixStream::pair`, or manual `dup2` of a non-pipe FD onto the wire.

### 3.2 Connection state (`connection.rs::SshConnection`)

`crates/rsync_io/src/ssh/connection.rs:30-39`:

```rust
pub struct SshConnection {
    child: Arc<Mutex<Option<Child>>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr_drain: Option<BoxedStderrChannel>,
    connect_watchdog: Option<ConnectWatchdog>,
}
```

- The wire is two unidirectional handles: `ChildStdin` (write side of the
  stdin pipe) and `ChildStdout` (read side of the stdout pipe). They are
  never combined into a single FD.
- `connection.rs:217-221` `impl Read for SshReader` delegates to
  `ChildStdout::read` -> `read(2)` on the pipe FD.
- `connection.rs:229-237` `impl Write for SshWriter` delegates to
  `ChildStdin::write` and `flush` -> `write(2)` on the pipe FD.
- Both ends are blocking. There is no `set_nonblocking` call anywhere in
  `crates/rsync_io/src/ssh/`. This contrasts with upstream's
  `util1.c:90-93` which forces non-blocking on every `fd_pair` result.

### 3.3 Stderr socketpair (`aux_channel.rs::configure_stderr_channel`)

`crates/rsync_io/src/ssh/aux_channel.rs:263-291`:

```rust
#[cfg(unix)]
pub(super) fn configure_stderr_channel(command: &mut Command) -> Option<UnixStream> {
    match UnixStream::pair() {
        Ok((parent, child)) => {
            let child_fd: std::os::fd::OwnedFd = child.into();
            command.stderr(Stdio::from(child_fd));
            Some(parent)
        }
        Err(_) => { command.stderr(Stdio::piped()); None }
    }
}

#[cfg(not(unix))]
pub(super) fn configure_stderr_channel(command: &mut Command) -> Option<()> {
    command.stderr(Stdio::piped());
    None
}
```

`std::os::unix::net::UnixStream::pair()` is the safe stdlib wrapper around
`socketpair(AF_UNIX, SOCK_STREAM, 0)`. It returns two connected
`UnixStream` handles. On success we keep one half on the parent side
(wrapped by `SocketpairStderrChannel` at `aux_channel.rs:146-172`) and
hand the other half to the child as its stderr fd via
`Stdio::from(OwnedFd)`. On any failure we fall back to `Stdio::piped()`,
matching upstream's `fd_pair` failover from socketpair to pipe.

The `cfg(not(unix))` arm always uses `Stdio::piped()` because Windows
does not support handing an `AF_UNIX` socket to `CreateProcess` as a
standard handle (see Section 5.2).

### 3.4 Half-close, half-open semantics

`connection.rs:96-102` is the only "half-close" path:

```rust
pub fn close_stdin(&mut self) -> io::Result<()> {
    if let Some(mut stdin) = self.stdin.take() {
        stdin.flush()?;
    }
    Ok(())
}
```

This drops `ChildStdin`, which closes the parent's view of the stdin
pipe entirely. There is no `shutdown(SHUT_WR)` call - pipes do not
support `shutdown(2)`.

`SshWriter::close` at `connection.rs:241-243` is the same pattern after
`split()`:

```rust
pub fn close(mut self) -> io::Result<()> {
    self.stdin.flush()
}
```

Drop closes the FD when the function returns.

### 3.5 io_uring boundary

`crates/rsync_io/src/ssh/mod.rs:57-75` documents the consequence of the
pipe topology:

```text
The SSH data channel is the spawned `ssh` child's inherited stdio: a
`(stdin, stdout)` pipe pair created by `Command::spawn`, not a socket.
The `fast_io` io_uring `socket_reader` / `socket_writer` fast paths
require an `AF_INET`/`AF_INET6` socket FD and are therefore unreachable
for SSH transfers - regardless of kernel version or `io_uring_policy`.
```

The pipe-FD io_uring path (`IORING_OP_READ` / `IORING_OP_WRITE`) is
tracked separately by #1859 and audited in
`docs/audits/iouring-pipe-stdio.md`. The splice/vmsplice zero-copy plan
is tracked by #1860 in `docs/audits/splice-ssh-stdio.md`.

### 3.6 Watchdog as the "non-blocking" workaround

`connection.rs:246-322` implements `ConnectWatchdog`, a background
thread that calls `Child::kill()` after a configurable timeout. This
exists because the inherited pipes are blocking; without a watchdog, a
hung SSH client (waiting on host-key prompt, connecting to an
unreachable address) would block the parent's first `read` indefinitely.
Upstream avoids this by setting non-blocking on its `fd_pair` ends and
using the rsync select loop in `io.c::perform_io`; oc-rsync substitutes
a watchdog-plus-blocking-IO model.

### 3.7 Summary of oc-rsync defaults

| Channel | Primitive | Citation |
|---|---|---|
| SSH wire stdin (parent->child) | `pipe(2)` (anonymous pipe) | `builder.rs:300` |
| SSH wire stdout (child->parent) | `pipe(2)` (anonymous pipe) | `builder.rs:301` |
| SSH stderr (Unix) | `socketpair(AF_UNIX, SOCK_STREAM, 0)` | `aux_channel.rs:265` |
| SSH stderr (Windows) | `pipe(2)` (anonymous pipe) | `aux_channel.rs:288-291` |
| SSH stderr (Unix fallback) | `pipe(2)` (anonymous pipe) | `aux_channel.rs:276` |
| Daemon `rsync://` wire | `TcpStream` (unaffected by this audit) | `crates/transport/` |
| Local-fork wire | not implemented (`oc-rsync` does not fork-and-exec itself) | n/a |

## 4. Trade-off analysis

### 4.1 Bidirectional ergonomics

- **Pipes (today):** Two unidirectional FDs. Each direction is half-duplex.
  oc-rsync's `SshConnection::split()` (`connection.rs:178-208`) returns
  `SshReader` (holds `ChildStdout`) and `SshWriter` (holds `ChildStdin`),
  one FD each. They can be moved to separate threads without any sharing,
  which matches the current threaded blocking model used by
  `crates/core/src/client/remote/ssh_transfer.rs`.
- **Socketpair:** One bidirectional FD, used for both reading and writing.
  A single `Arc<UnixStream>` shared between the reader and writer halves
  would replace the two-FD model. Future event-loop integration
  (`tokio::io::AsyncFd`, `mio::Poll`, io_uring registered FDs) needs
  exactly one registration instead of two and exposes a single readiness
  mask covering both directions.

The threaded blocking model oc-rsync uses today is straightforward and
works for both. The socketpair advantage materializes only after the
async-transport refactor (#2068, see also `docs/audits/async-ssh-transport.md`)
which would unify the two halves anyway.

### 4.2 Backpressure and buffer accounting

- **Pipes:** Default kernel buffer is 64 KiB on Linux (`PIPE_BUF` is 4096
  for atomic-write guarantees, but pipe capacity is separate; see
  `man 7 pipe`, `/proc/sys/fs/pipe-max-size`). The capacity can be lifted
  via `fcntl(F_SETPIPE_SZ)` up to `pipe-max-size` (typically 1 MiB). When
  the parent's stdin buffer fills, `write(2)` blocks (or returns `EAGAIN`
  if non-blocking).
- **Socketpair (`SOCK_STREAM`):** Buffer is `SO_SNDBUF` / `SO_RCVBUF` per
  direction. Linux defaults are `net.core.{r,w}mem_default` (typically
  212 KiB) and `net.core.{r,w}mem_max` (4 MiB). Tunable per-FD via
  `setsockopt(2)`. A `getsockopt(SO_SNDBUF)` returns the doubled value
  the kernel actually allocated.

The default socket buffers are larger than the default pipe buffer by ~3x,
which means a socketpair-backed wire absorbs more burst-write before
backpressure stalls a writer. In oc-rsync's threaded model this shows up
only if the engine pushes >64 KiB without a corresponding read on the
other side, which is rare because the rsync multiplex envelope chunks
payloads to `MAX_PAYLOAD_LENGTH = 0x00FF_FFFF` (~16 MiB), and the file
list / delta phases interleave reads and writes.

oc-rsync has no measured stall attributable to pipe-buffer pressure on
the SSH wire today. There is no concrete regression to fix.

### 4.3 Auxiliary channels (`SCM_RIGHTS`, `SO_PASSCRED`)

- **Pipes:** No ancillary data. `sendmsg(2)` / `recvmsg(2)` on a pipe
  return `ENOTSOCK`. Pipes cannot pass file descriptors between
  processes; cannot pass credentials; cannot do anything beyond byte
  transfer.
- **Socketpair:** Supports `SCM_RIGHTS` (FD passing) and `SCM_CREDENTIALS`
  on Linux, `SCM_CREDS` on FreeBSD. The SSH child is an `ssh(1)` binary
  oc-rsync does not control - it does not invoke `sendmsg` with
  ancillary data on its stdio. So even if the parent had a socketpair,
  the child would not use it for FD passing or credentials.

oc-rsync has no upcoming feature that needs `SCM_RIGHTS` or
`SCM_CREDENTIALS` on the SSH wire. Upstream rsync does not use ancillary
data either. This is a hypothetical advantage with no current consumer.

### 4.4 io_uring opcode compatibility

This was audited in detail in `docs/audits/iouring-pipe-stdio.md` and
`crates/rsync_io/src/ssh/mod.rs:57-75`. Summary:

| FD type | `IORING_OP_READ` / `OP_WRITE` | `IORING_OP_RECV` / `OP_SEND` | Registered buffers (`IORING_REGISTER_BUFFERS`) |
|---|---|---|---|
| Pipe | works (Linux 5.1+) | rejects with `ENOTSOCK` | works on read/write paths |
| `AF_UNIX SOCK_STREAM` socketpair | works | works | works |
| `AF_INET` TCP socket | works | works (preferred fast path) | works |

The "socket fast path" advantage refers to `IORING_OP_RECV` /
`IORING_OP_SEND` plus zero-copy variants (`SEND_ZC`). Those opcodes
require a socket FD. A socketpair gets them; a pipe does not.

The actual io_uring wins on the SSH path (syscall amortization via
`IORING_SETUP_SQPOLL`, batched submissions) work on pipe FDs too. The
`iouring-pipe-stdio.md` analysis records no measurable throughput
difference between `OP_READ`/`OP_WRITE` on pipes and `OP_RECV`/`OP_SEND`
on sockets for multi-MiB sequential transfers; the socketpair advantage
is theoretical for bulk I/O.

### 4.5 `splice(2)` / `vmsplice(2)` eligibility

Per `man 2 splice`, one of the two FDs must refer to a pipe. `man 2
vmsplice` requires the destination to be a pipe. The two zero-copy paths
oc-rsync wants are `splice(file_fd, NULL, wire, NULL, ...)` (sender) and
`splice(wire, NULL, file_fd, NULL, ...)` (receiver); both require the
wire to be a pipe.

**A socketpair-backed wire breaks the splice path.**
`splice-ssh-stdio.md:60-67` confirms the dependency. Migrating the wire
to a socketpair would force the splice plan to insert an intermediate
`pipe(2)` in user space and double-splice through it, negating the
zero-copy benefit. This is the strongest argument against a socketpair
migration and the primary reason this audit recommends keeping pipes.

### 4.6 Stderr separation

Both topologies need a separate FD for stderr because `ssh(1)` writes
diagnostic output (host-key warnings, banners, `Permission denied`) to
its inherited fd 2. oc-rsync must capture and forward it in real time
so the user sees prompts and connection errors immediately. The current
Unix path uses `socketpair(AF_UNIX, SOCK_STREAM)` for stderr (#1689 /
`aux_channel.rs:263-285`); Windows uses an anonymous pipe
(`aux_channel.rs:287-291`). Independent of the wire choice.

### 4.7 Half-close semantics

- **Pipes:** No `shutdown(2)`. Closing `ChildStdin` (by drop) closes the
  entire FD; the child sees EOF on its stdin. The parent retains the
  stdout FD and can drain remaining bytes - this is the rsync
  end-of-transfer dance.
- **Socketpair:** `shutdown(SHUT_WR)` cleanly half-closes the write
  direction on the same FD while leaving the read side open. Matches
  TCP-socket semantics.

The behavioural difference is negligible for oc-rsync: with two FDs the
half-close is implicit in the drop, and no caller wants to issue a
write-side shutdown and continue reading on the same handle.

### 4.8 Compatibility with the SSH child program

Both topologies are transparent to the SSH child. `ssh(1)` reads from
fd 0 and writes to fd 1 without calling `getsockopt`/`fstat` to
discriminate sockets from pipes. `Command::spawn` calls `dup2` onto fds
0 and 1 in either case. Read/write semantics are identical for a child
that uses the FDs as plain bytestreams.

### 4.9 Code complexity

- **Pipes:** zero extra code. `Stdio::piped()` is the stdlib idiom.
- **Socketpair:** at minimum, the same factory pattern as
  `aux_channel.rs::configure_stderr_channel` repeated for two FDs (or
  one FD `dup`ed to both). Add a `WireChannelKind::{Pipe, Socketpair}`
  enum to keep the `SshReader`/`SshWriter` API surface portable. Add
  Windows fallback. Add tests for `OwnedFd::try_clone` + `Stdio::from`
  ordering.

The socketpair migration is small (estimated 100-200 LOC across
`builder.rs`, `connection.rs`, plus tests), but it is non-zero.

### 4.10 Upstream parity

- **Pipes:** diverges from upstream `pipe.c::piped_child`. Upstream uses
  socketpair via `fd_pair`.
- **Socketpair:** matches upstream exactly when `HAVE_SOCKETPAIR` is set
  (which is the universal case for modern Unix builds).

Upstream parity is a soft goal; oc-rsync deviates from upstream in
several places (Rust stdlib, threaded blocking model, no select loop)
and the deviation here is invisible on the wire.

## 5. Decision matrix

The two topologies compared on the dimensions Section 4 enumerates.
"Better" entries are bolded.

| Dimension | Pipes (status quo) | Socketpair (`AF_UNIX`, `SOCK_STREAM`) | Notes |
|---|---|---|---|
| Bidirectional ergonomics (single FD vs two) | two FDs | **one FD** | Matters only with an event loop; oc-rsync threads today. |
| Backpressure / default buffer | 64 KiB | **~212 KiB (`SO_*BUF`)** | No measured oc-rsync stall on either. |
| Tunable buffers | `F_SETPIPE_SZ` to `pipe-max-size` (1 MiB typical) | **`SO_*BUF` to `r/w mem_max` (4 MiB typical)** | Linux only; equivalents differ per OS. |
| Ancillary channel (`SCM_RIGHTS`, creds) | not supported | supported | No oc-rsync consumer. |
| io_uring opcode coverage | `OP_READ` / `OP_WRITE` | **also `OP_RECV` / `OP_SEND` / `SEND_ZC`** | Sequential bulk I/O parity in practice. |
| `splice(2)` / `vmsplice(2)` eligibility | **pipe-end satisfied directly** | requires intermediate pipe | Splice plan (#1860) relies on pipes. |
| Stderr separation | **independent fd 2 (already done)** | independent fd 2 (already done) | Orthogonal to wire choice. |
| Half-close (`shutdown(SHUT_WR)`) | no, by drop only | **yes** | Implicit half-close already works for oc-rsync. |
| Code complexity (LOC, branches) | **0** | ~100-200 | Plus Windows fallback. |
| Upstream parity (`pipe.c::piped_child`) | divergent | **matches `fd_pair` default** | Soft goal. |
| Cross-platform consistency (Unix vs Windows) | **identical primitive on both** | Unix-only; pipes on Windows | Asymmetry imposes cfg branches. |
| Connect-timeout strategy | watchdog kills child | poll(2) timeout possible | Watchdog already shipped. |
| Compatibility with SSH child | identical | identical | Both use `dup2` onto fd 0/1. |

The matrix splits roughly evenly. The decisive entry for "do not
implement" is the splice eligibility row, because the splice plan
(#1860) is the highest-value zero-copy work in the SSH transport
roadmap and a socketpair migration would force it to thread an
intermediate pipe, defeating the point.

## 6. Recommendation

**Keep anonymous pipes for the SSH wire.** Close #1687 (prototype
socketpair wire) as "do not implement" with this audit as the
justification. Close #1902 (verify socketpair vs pipe wire claim against
`rsync_io` source) as "verified" with the upstream/oc-rsync table in
Sections 2 and 3.

The reasoning is the union of three points:

1. **Splice eligibility (Section 4.5).** `splice(2)` and `vmsplice(2)`
   require one FD to be a pipe. oc-rsync's zero-copy roadmap (#1860)
   wins back full memory-bandwidth on sender and receiver file<->wire
   edges only when the wire is a pipe. A socketpair-backed wire would
   force a double-splice through a user-space `pipe(2)`, eliminating the
   benefit. The io_uring `OP_RECV` / `OP_SEND` socket fast path does
   not recover this on an `AF_UNIX` socketpair because neither end is
   a TCP socket, so `SEND_ZC` MSG-zerocopy paths do not apply.

2. **No measured backpressure regression (Section 4.2).** The 64 KiB
   pipe-buffer default is large enough that oc-rsync's multiplex-framed
   writes do not stall. No issue points to pipe-buffer pressure on the
   SSH wire.

3. **Async-transport refactor subsumes the unified-FD argument
   (Sections 4.1, 4.4).** The "one poll registration" and
   `OP_RECV`/`OP_SEND` advantages only matter inside an event loop. The
   async-transport audit in `docs/audits/async-ssh-transport.md` and
   the wider `docs/audits/async-file-writer-trait.md` work (#1655)
   plan an async I/O migration that consolidates readiness tracking
   regardless of primitive. If that refactor lands and we measure a
   socket-specific win, we reconsider with concrete numbers.

The Unix stderr socketpair (`aux_channel.rs:263-285`, #1689) stays as
is - it is the right primitive for stderr (`epoll`/`kqueue`-registrable,
line-oriented, no splice need, gracefully shutdown-able).

## 7. Findings (informational)

All findings are informational. None require code changes; they document
deliberate divergences from upstream and the reasoning behind them.

### Finding 1: wire uses `pipe(2)` where upstream uses `socketpair`

- **Evidence:** `crates/rsync_io/src/ssh/builder.rs:300-301`
  (`Stdio::piped()` for stdin and stdout). Upstream:
  `target/interop/upstream-src/rsync-3.4.1/pipe.c:57` and
  `util1.c:84-85`.
- **Status:** intentional divergence per Section 6.
- **Impact:** no `shutdown(SHUT_WR)`, no `OP_RECV`/`OP_SEND` opcodes,
  but full `splice(2)` eligibility is preserved. Cross-version interop
  is unaffected because both topologies write the same wire bytes.

### Finding 2: parent stdio is blocking, upstream is non-blocking

- **Evidence:** No `set_nonblocking` call in
  `crates/rsync_io/src/ssh/builder.rs` or `connection.rs`. Upstream:
  `util1.c:90-93` forces both ends non-blocking inside `fd_pair`.
- **Status:** intentional. oc-rsync's threaded blocking model uses
  separate threads for the read and write halves, so non-blocking is
  not required to avoid deadlock.
- **Impact:** the connect-timeout watchdog
  (`connection.rs:246-322`) substitutes for upstream's `select`-based
  IO timeout. The watchdog is a correct, simple alternative.

### Finding 3: stderr socketpair already in place; wire is not

- **Evidence:** `crates/rsync_io/src/ssh/aux_channel.rs:263-285` installs
  a stderr socketpair on Unix; `builder.rs:300-301` keeps the wire on
  pipes.
- **Status:** intentional. The stderr socketpair was added (#1689) for
  reasons that do not apply to the wire (line-oriented diagnostics,
  potential future event-loop integration with no splice requirement).
- **Impact:** the IPC primitives on a single SshConnection are mixed
  (sockets for stderr, pipes for the wire). Documented here to prevent
  surprise during future refactors.

### Finding 4: `close_stdin` cannot half-close cleanly

- **Evidence:** `crates/rsync_io/src/ssh/connection.rs:96-102` and
  `connection.rs:241-243` (`SshWriter::close`). Both flush and drop
  `ChildStdin`, which closes the entire pipe FD.
- **Status:** acceptable. The two-FD topology already provides
  half-open semantics implicitly: closing stdin signals EOF to the
  child while leaving stdout open for the parent to drain.
- **Impact:** none observable.

### Finding 5: io_uring socket fast paths unreachable on the SSH wire

- **Evidence:** `crates/rsync_io/src/ssh/mod.rs:57-75` (already
  documented for #1858).
- **Status:** intentional. The socket-fast-path advantage is
  theoretical for sequential bulk I/O on a `SOCK_STREAM` socketpair
  versus pipe-FD `OP_READ`/`OP_WRITE`.
- **Impact:** none. The pipe-FD io_uring plan (#1859) covers the
  achievable wins.

### Finding 6: stderr forwarding is not multiplexed onto the wire

- **Evidence:** `crates/rsync_io/src/ssh/aux_channel.rs:208-228` writes
  SSH-client stderr lines directly to `eprint!`. Upstream's multiplex
  path (`io.c::send_msg`) only multiplexes diagnostic output produced
  by the rsync process itself, not by SSH.
- **Status:** matches upstream. The local SSH client's stderr is
  out-of-band; it never appears on the rsync wire.
- **Impact:** none. Documented to prevent future refactors from
  inadvertently routing SSH-client stderr into the rsync multiplex
  envelope.

## 8. Implementation notes (for the "keep" recommendation)

Because the recommendation is to keep pipes on the wire, the
implementation note is the rationale for closing the pending trackers:

### Closing #1687

> Prototype SSH subprocess using socketpair for bidirectional I/O.

Disposition: **do not prototype**. Justified by Section 6, primarily
the splice eligibility argument (Section 4.5) and the no-measured-stall
argument (Section 4.2). If the async-transport refactor (#2068) lands
and benchmarks show a socket-specific win, this tracker can be
reopened with concrete numbers. The audit closes #1687 in this PR's
description so it is removed from the open queue.

### Closing #1902

> Verify SSH socketpair vs anonymous-pipe wire claim against
> `rsync_io` source.

Disposition: **verified**. The wire in `rsync_io` is two anonymous
pipes (`crates/rsync_io/src/ssh/builder.rs:300-301`). Upstream uses
two socketpairs (`pipe.c:57` plus `util1.c:84-85`). The audit closes
#1902 in this PR's description.

### What stays on the roadmap

- **#1859** io_uring on pipe FDs - audited in
  `iouring-pipe-stdio.md`. Pipes are eligible for `OP_READ`/`OP_WRITE`,
  which is the path forward.
- **#1860** splice/vmsplice for SSH stdio - audited in
  `splice-ssh-stdio.md`. Pipes are required for splice.
- **#2068** async SSH transport - audited in
  `async-ssh-transport.md`. Will subsume the unified-FD discussion
  whenever it lands.

### Documentation hygiene

- `crates/rsync_io/src/ssh/mod.rs:57-75` already records the io_uring
  consequence of using pipes. That paragraph remains accurate after this
  audit and does not need a follow-up edit.
- `crates/rsync_io/src/ssh/aux_channel.rs:1-22` already explains why
  the stderr channel uses a socketpair while the wire does not need to.
  No edit required.

### If the recommendation is ever reversed

For completeness, the touch-points a future socketpair migration would
modify (recorded so a maintainer does not need to redo the search):

- `crates/rsync_io/src/ssh/builder.rs:300-301` - replace `Stdio::piped()`
  with a factory that attempts `UnixStream::pair()` plus
  `OwnedFd::try_clone` for the stdin child end and passes both child
  ends as `Stdio::from(OwnedFd)`. Pipe fallback on failure.
- `crates/rsync_io/src/ssh/connection.rs:30-39` - replace
  `stdin`/`stdout` fields with a `wire: WireEnd` enum
  (`Pipe { stdin, stdout }` or `Socket(Arc<UnixStream>)`).
- `crates/rsync_io/src/ssh/connection.rs:178-208`,
  `crates/rsync_io/src/ssh/connection.rs:241-243` - dispatch
  `Read`/`Write`/`close` over the enum; socket variant uses
  `shutdown(Shutdown::Write)`.
- New tests: `shutdown(SHUT_WR)` surfaces EOF on the child's stdin while
  the parent can still read; cloexec survives `dup2` into stdio.
- Cross-version interop suite (`tools/ci/run_interop.sh`) must show no
  regressions vs rsync 3.0.9, 3.1.3, 3.4.1.
- Re-audit `splice-ssh-stdio.md` (#1860) - the splice plan would have
  to thread an intermediate `pipe(2)`. Estimated ~100-200 LOC plus
  tests, consistent with Section 4.9.

## 9. References

Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`):

- `pipe.c:48-97` `piped_child` - SSH child setup, two `fd_pair` calls.
- `pipe.c:99-178` `local_child` - forked-local rsync, same topology.
- `util1.c:74-96` `fd_pair` - `socketpair`-or-`pipe` wrapper,
  non-blocking on success.
- `main.c:504-663` `do_cmd` - dispatch among `read_batch`,
  `local_server`, `piped_child`.
- `main.c:629`, `main.c:985` - auxiliary `fd_pair` uses (read-batch
  back-channel, receiver error pipe).
- `clientserver.c:116-148` `start_socket_client` - daemon TCP wire.
- `clientserver.c:692` `rsync_module` - daemon per-connection handler.
- `socket.c:736-846` `socketpair_tcp` + `sock_exec` -
  `RSYNC_CONNECT_PROG` test escape (not used on real SSH paths).
- `io.c:983-1031` `send_msg` - multiplex envelope (substitute for an
  auxiliary stderr channel).
- `cleanup.c:46-67` `close_all` - orderly socket shutdown.

oc-rsync source:

- `crates/rsync_io/src/ssh/builder.rs:285-340` `SshCommand::spawn`.
- `crates/rsync_io/src/ssh/builder.rs:300-301` - `Stdio::piped()` for
  wire stdin/stdout.
- `crates/rsync_io/src/ssh/connection.rs:30-39` `SshConnection`.
- `crates/rsync_io/src/ssh/connection.rs:96-102` `close_stdin`.
- `crates/rsync_io/src/ssh/connection.rs:178-208`
  `SshConnection::split`.
- `crates/rsync_io/src/ssh/connection.rs:217-237` blocking `Read` /
  `Write` impls.
- `crates/rsync_io/src/ssh/connection.rs:241-243` `SshWriter::close`.
- `crates/rsync_io/src/ssh/connection.rs:246-322` `ConnectWatchdog`.
- `crates/rsync_io/src/ssh/aux_channel.rs:138-193`
  `SocketpairStderrChannel` (#1689).
- `crates/rsync_io/src/ssh/aux_channel.rs:263-291`
  `configure_stderr_channel`.
- `crates/rsync_io/src/ssh/mod.rs:57-75` io_uring boundary (#1858).

Companion audits:

- `docs/audits/iouring-pipe-stdio.md` (#1859), `splice-ssh-stdio.md`
  (#1860), `async-ssh-transport.md` (#2068),
  `async-file-writer-trait.md` (#1655),
  `ssh-cipher-compression.md`.

External references:

- `man 2 socketpair`, `man 2 pipe`, `man 7 pipe`, `man 2 splice`,
  `man 2 vmsplice`, `man 7 unix`.
- Linux io_uring opcodes: `IORING_OP_READ`, `IORING_OP_WRITE`,
  `IORING_OP_RECV`, `IORING_OP_SEND`, `SEND_ZC`.
