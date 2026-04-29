# socketpair(2) vs anonymous pipes for the SSH subprocess

Tracking issues: oc-rsync tasks #1686 (transport channel topology) and #1689
(stderr multiplexing).

## Summary

Upstream rsync 3.4.1 uses `socketpair(AF_UNIX, SOCK_STREAM, 0)` for the wire
between the parent rsync and its forked or `exec`-ed peer. The pair is
created twice (one per direction) in `pipe.c::piped_child` and
`pipe.c::local_child`, with a `pipe(2)` fallback selected at configure time
when the host lacks `socketpair`. oc-rsync currently spawns SSH with
`Command::stdin(Stdio::piped()) + Command::stdout(Stdio::piped())`, i.e. two
half-duplex anonymous pipes. The stderr channel is already wired through a
`socketpair` on Unix (added for #1689 groundwork), so half of the upstream
topology is already in place.

This audit recommends a phased migration to a socketpair-backed wire on
Unix while keeping anonymous pipes as the Windows fallback. The migration is
additive, requires no protocol or wire-format changes, and consolidates the
read and write FDs into a single bidirectional descriptor that can be polled
once. The auxiliary stderr socketpair stays as is and continues to provide a
real socket FD for future poll/epoll/kqueue integration.

## 1. Current oc-rsync model

Single spawn site:

- `crates/rsync_io/src/ssh/builder.rs:280-282` configures the child stdio:
  ```rust
  let mut command = Command::new(&program);
  command.stdin(Stdio::piped());
  command.stdout(Stdio::piped());
  ```
  These calls each create an anonymous `pipe(2)` pair under the hood
  (Rust's `process_unix.rs::AnonPipe::pipe`). The parent retains the write
  end of the stdin pipe and the read end of the stdout pipe; the child
  receives the opposite ends as fds 0 and 1.

- `crates/rsync_io/src/ssh/builder.rs:293` installs the stderr channel:
  ```rust
  let parent_socketpair_end = configure_stderr_channel(&mut command);
  ```
  On Unix this attempts `UnixStream::pair()` and hands the child end to the
  command via `Stdio::from(child_fd)`; on failure or on Windows the child
  inherits a conventional `Stdio::piped()` stderr.
  See `crates/rsync_io/src/ssh/aux_channel.rs:264-291`.

- `crates/rsync_io/src/ssh/connection.rs:30-39` stores the parent ends:
  ```rust
  pub struct SshConnection {
      child: Arc<Mutex<Option<Child>>>,
      stdin: Option<ChildStdin>,
      stdout: Option<ChildStdout>,
      stderr_drain: Option<BoxedStderrChannel>,
      connect_watchdog: Option<ConnectWatchdog>,
  }
  ```

- `crates/rsync_io/src/ssh/connection.rs:178-208` splits into half-duplex
  reader/writer:
  ```rust
  pub fn split(mut self) -> io::Result<(SshReader, SshWriter, SshChildHandle)>
  ```
  The reader wraps `ChildStdout`, the writer wraps `ChildStdin`. There is no
  shared FD - the two halves cannot be polled together with a single
  registration.

- `crates/rsync_io/src/ssh/connection.rs:217-237` performs blocking
  `read(2)`/`write(2)` directly on the inherited pipes. No `O_NONBLOCK` is
  set on the parent ends. There is no `EAGAIN` retry path because the
  pipes are blocking.

- `crates/rsync_io/src/ssh/connection.rs:97-102` performs a half-close by
  flushing and dropping the stdin handle. There is no `shutdown(2)` because
  pipes do not support it; the only way to half-close is to drop the FD,
  which closes the pipe entirely.

- `crates/rsync_io/src/ssh/mod.rs:57-72` documents the consequence: io_uring
  socket fast paths in `fast_io` cannot be applied to SSH stdio because the
  parent FDs are pipes, not sockets. Splice (#1860) is feasible because
  splice supports pipe FDs; sockets are not.

Blocking, EAGAIN, and half-close behaviour today:

- The pipes are inherited blocking. The watchdog at
  `connection.rs:271-322` kills the child to unblock pending I/O when a
  connect timeout fires.
- There is no select/poll on the wire. `Read::read` and `Write::write`
  block in the kernel.
- `close_stdin` is a one-way half-close by drop; the read half stays open
  until the child closes its stdout or exits.

## 2. Upstream model

Source of truth: `target/interop/upstream-src/rsync-3.4.1/`.

- `pipe.c:48-97` `piped_child(char **command, int *f_in, int *f_out)`:
  ```c
  int to_child_pipe[2];
  int from_child_pipe[2];
  if (fd_pair(to_child_pipe) < 0 || fd_pair(from_child_pipe) < 0) {
      rsyserr(FERROR, errno, "pipe");
      exit_cleanup(RERR_IPC);
  }
  pid = do_fork();
  ...
  if (pid == 0) {
      if (dup2(to_child_pipe[0], STDIN_FILENO) < 0
          || close(to_child_pipe[1]) < 0
          || close(from_child_pipe[0]) < 0
          || dup2(from_child_pipe[1], STDOUT_FILENO) < 0) {
          ...
      }
      set_blocking(STDIN_FILENO);
      if (blocking_io > 0)
          set_blocking(STDOUT_FILENO);
      execvp(command[0], command);
      ...
  }
  ...
  *f_in = from_child_pipe[0];
  *f_out = to_child_pipe[1];
  ```
  Both directions go through `fd_pair`, not `pipe`. The parent keeps two
  ends, the child keeps the other two and `dup2`s them onto stdin/stdout.

- `util1.c:74-96` `fd_pair`:
  ```c
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
  Both ends are forced non-blocking on success, and the implementation
  silently falls back to `pipe(2)` when `HAVE_SOCKETPAIR` is undefined.
  `config.h:432` records `HAVE_SOCKETPAIR` as a configure-time probe.

- The same `fd_pair` is used at `main.c:629` (read-batch back-channel) and
  `main.c:985` (receiver error pipe). These are auxiliary channels used in
  parallel with the wire.

- Stderr handling on the upstream side never travels back through a
  separate FD. The remote rsync multiplexes its own diagnostic output as
  `MSG_INFO`/`MSG_ERROR` envelopes on the wire (`io.c::send_msg`, gated on
  `iobuf.multiplex_writes`); only the local SSH client's own stderr remains
  out-of-band and is forwarded by SSH directly to the user's terminal.
  Upstream therefore does not need a third FD for remote-rsync diagnostics
  - they are folded into the multiplex layer that already runs on the
  wire. There is no SO_PASSCRED or auxiliary stderr socketpair in upstream
  rsync.

- `cleanup.c:46-67` `close_all` walks every open FD and, when the FD is a
  socket, calls `shutdown(fd, 2)` before `close(fd)` to avoid the abortive
  RST sent by Winsock-based peers. This logic is gated on
  `SHUTDOWN_ALL_SOCKETS` and exploits exactly the fact that `fd_pair`
  returns sockets - shutdown is a no-op on pipes.

- `local_child` (`pipe.c:99-178`) uses `fd_pair` twice for forked-local
  rsync. The accompanying comment at `pipe.c:99-108` explicitly names the
  endpoints "socket pairs" and explains the four-end ownership pattern.

## 3. Trade-off analysis

### Anonymous pipes (status quo for stdin/stdout)

- 2 FDs per direction, 4 FDs total. Half-duplex in each direction.
- Universally available, including on Windows where the anonymous-pipe
  primitive is the only portable child-stdio mechanism. Rust's
  `Stdio::piped` provides identical semantics on every supported platform.
- No `shutdown(2)` support. Half-close is achieved only by dropping the
  whole FD (which closes the pipe). Receiver cannot signal "no more
  writes, but keep reading" without losing the read direction.
- Read-readiness and write-readiness live on different FDs, so a poll-set
  needs two registrations. Today oc-rsync sidesteps this by performing
  blocking I/O on each side from separate threads.
- `splice(2)` works on pipes (this is the path explored in
  `splice-ssh-stdio.md`). `IORING_OP_READ`/`IORING_OP_WRITE` accept pipe
  FDs from Linux 5.1, but the io_uring socket fast path
  (`IORING_OP_RECV`/`SEND_ZC` and registered buffers) requires a socket
  FD and is therefore inaccessible on pipes.
- Pipe capacity is fixed at 64 KiB by default and can be lifted to
  `/proc/sys/fs/pipe-max-size` via `fcntl(F_SETPIPE_SZ)`.

### `socketpair(AF_UNIX, SOCK_STREAM, 0)` (upstream's choice)

- 2 FDs total, each fully bidirectional. The same FD can be used for read
  and write, so a poll-set needs one registration per direction-of-flow.
  When both halves of oc-rsync's `split()` run on the same thread (e.g.,
  in a future event loop) they could share an `epoll`/`kqueue`/`io_uring`
  poll group.
- `shutdown(SHUT_WR)` cleanly half-closes the write side while leaving the
  read side open. This maps directly to "I am done sending; flush and
  drain the response" semantics that the rsync end-of-transfer phase
  needs. With pipes today, dropping `ChildStdin` (`connection.rs:97-102`)
  also tears down the parent's view of the write FD entirely; the child
  sees EOF on its stdin but the parent loses the ability to retry on
  EAGAIN.
- `SO_RCVBUF`/`SO_SNDBUF` are tunable per-direction. Linux's default is
  `net.core.{r,w}mem_default` (typically 212 KiB), already larger than the
  default 64 KiB pipe buffer.
- Stream-oriented (`SOCK_STREAM`), so byte boundaries are preserved. No
  message-boundary semantics.
- Available on every Unix oc-rsync targets (Linux, macOS, *BSD,
  illumos). NOT available on Windows: there is no `AF_UNIX, SOCK_STREAM`
  socketpair; `WSASocket`/`WSADuplicateSocket` over a TCP loopback pair is
  the only equivalent and is a much heavier dance.
- io_uring socket fast paths apply: `IORING_OP_RECV/SEND` and registered
  buffers work on AF_UNIX sockets. Splice still works through the pair via
  an intermediate pipe (splice requires one of the two FDs to be a pipe),
  so the splice plan in `splice-ssh-stdio.md` keeps working unchanged
  because we splice between a regular file and the pipe-backed page cache;
  the SSH wire is not part of the splice fast path on either layout.
- `tcgetpgrp`-style controlling-terminal interactions do not apply to
  `AF_UNIX` socketpairs; SSH already declines to allocate a PTY in the
  invocation oc-rsync uses today.

### Auxiliary stderr channel

Upstream does not use one. Remote-rsync diagnostics flow back as `MSG_*`
envelopes on the wire. Only the *local* SSH binary's own stderr (host-key
warnings, connect errors) is out-of-band, and upstream simply lets the SSH
client write to the inherited terminal stderr.

oc-rsync needs the auxiliary channel because:

1. We capture the local SSH client's stderr to surface it on failure
   (`connection.rs:124-151`, `aux_channel.rs:74-89`).
2. We forward each line in real time to the controlling terminal so the
   user sees host-key prompts and `Permission denied` immediately
   (`aux_channel.rs:208-228`).

Three implementations are conceivable:

1. **Anonymous pipe** (`Stdio::piped()`): default, what we fall back to.
   Pure pipe(2) - pollable but no shutdown.
2. **Socketpair** (current Unix path): `UnixStream::pair()` with one end
   passed as the child stderr. Already implemented at
   `aux_channel.rs:264-285`. Gives a real `AF_UNIX` socket FD on the
   parent side, ready for future event-loop integration without touching
   call sites.
3. **`SO_PASSCRED` plus a third socketpair**: would let the SSH child
   forward arbitrary FDs to the parent (e.g., a separate channel for
   structured diagnostics). Not used by upstream, not needed by
   oc-rsync, and would force SSH-server cooperation we do not have.

Recommendation for #1689: keep the existing socketpair stderr channel as
the Unix default; do not pursue `SO_PASSCRED` or a third pair. The
socketpair already provides every property we need (pollable, real socket
FD, bounded capture buffer, shutdown semantics) and the implementation
landed under #1689's groundwork.

## 4. Cross-platform impact

| Platform | Wire (stdin/stdout) | Stderr | Notes |
|---|---|---|---|
| Linux | `pipe(2)` today, socketpair recommended (Unix path of phase 1) | `socketpair(AF_UNIX)` today | Both supported by `Command` via `Stdio::from(OwnedFd)`. |
| macOS | same as Linux | `socketpair(AF_UNIX)` today | `socketpair` is POSIX; macOS `Stdio::from(OwnedFd)` is stable. |
| *BSD / illumos | same as Linux | `socketpair(AF_UNIX)` today | Same `UnixStream::pair()` path. |
| Windows | `pipe(2)` (anonymous pipe) - permanent | anonymous pipe - permanent | Windows `Command::stdin/stdout/stderr(Stdio::piped())` creates anonymous pipes with `CreatePipe`. There is no `AF_UNIX` socketpair on Windows that we can hand to a child as a stdio handle: `socketpair` is unavailable in WinSock, and although Windows 10 1809+ supports `AF_UNIX` for `socket(2)`, neither `WSASocket`/`DuplicateHandle` round trip nor `WSADuplicateSocket` produces a handle that `CreateProcess` can plant on stdin/stdout/stderr. The Rust `Stdio::from(OwnedFd)` path does not have a stable Windows analogue for sockets. The fallback strategy is therefore to keep the pipe path on Windows indefinitely. |

The asymmetry is acceptable: Windows does not run io_uring or
`epoll(7)`, so the unified-poll motivation does not apply there. The
existing `cfg(unix)` /  `cfg(not(unix))` factory pattern in
`aux_channel.rs:263-291` is the model to follow for the wire too.

## 5. Findings

### Finding 1 (medium severity): wire uses pipes where upstream uses socketpair

- **Evidence:** `crates/rsync_io/src/ssh/builder.rs:280-282`. Upstream:
  `target/interop/upstream-src/rsync-3.4.1/pipe.c:57` and
  `util1.c:84-85`.
- **Impact:** No `shutdown(SHUT_WR)` available on the parent side, so
  half-close on the write direction collapses the FD entirely; the
  parent loses the ability to inspect or retry pending writes after
  signalling EOF. `splice` and `vmsplice` continue to work, but the
  io_uring socket fast path is unreachable on the wire FDs (already noted
  in `mod.rs:57-72`). Cross-version interop is unaffected because
  upstream's socketpair side reads/writes the same wire bytes either way.
- **Recommended fix:** Implement phase 1 of section 6: spawn a
  `socketpair(AF_UNIX, SOCK_STREAM, 0)` on Unix, hand one end to the
  child as fd 0 and 1 (a single FD `dup2`-ed to both), retain the other
  end as a single bidirectional `UnixStream` on the parent. Keep the
  pipe path as the Windows fallback.

### Finding 2 (medium severity): no non-blocking mode on inherited stdio

- **Evidence:** No `set_nonblocking` call in
  `crates/rsync_io/src/ssh/builder.rs` or `connection.rs`. Upstream:
  `util1.c:90-93` forces both ends non-blocking inside `fd_pair`.
- **Impact:** The parent must run separate threads for read and write to
  avoid deadlock when the wire fills. `fast_io`'s registered-buffer
  rings cannot be driven against blocking pipes. The connect-watchdog at
  `connection.rs:271-322` works around the blocking I/O by killing the
  child, which is a sledgehammer compared to a `poll(2)` timeout.
- **Recommended fix:** When the socketpair migration lands, set
  `O_NONBLOCK` on the parent end via `UnixStream::set_nonblocking(true)`
  and surface `WouldBlock` to callers with a thin retry helper. Keep
  blocking semantics on the pipe-backed Windows path.

### Finding 3 (low severity): split halves cannot share a poll registration

- **Evidence:** `crates/rsync_io/src/ssh/connection.rs:178-208`
  (`SshReader { stdout }` and `SshWriter { stdin }` hold separate FDs).
- **Impact:** Future event-loop integration (epoll, kqueue, io_uring)
  requires two FD registrations and tracks read- and write-readiness
  independently. With socketpair, a single FD covers both directions
  and the readiness mask is unified.
- **Recommended fix:** Phase 2 below: after the socketpair migration,
  rewrite `SshReader`/`SshWriter` to share an `Arc<UnixStream>` and
  attach the readiness machinery to the single FD. The `Read`/`Write`
  trait surfaces stay identical so call sites do not change.

### Finding 4 (low severity): `close_stdin` cannot half-close cleanly

- **Evidence:** `crates/rsync_io/src/ssh/connection.rs:97-102` and
  `connection.rs:241-243` (`SshWriter::close`). The implementation
  flushes and drops the `ChildStdin`, which closes the entire pipe FD.
- **Impact:** On a pipe there is no other choice. If the protocol
  requires the parent to keep reading after sending EOF (today the
  receiver does this implicitly because read and write live on
  different FDs, but a future unified-FD design would require explicit
  half-close), pipes cannot express it.
- **Recommended fix:** With the socketpair migration, replace the drop
  with `socket.shutdown(Shutdown::Write)`. The parent retains the read
  half until the child closes its end. Match upstream's
  `cleanup.c:46-67` pattern by calling `shutdown(fd, SHUT_WR)` before
  close on graceful exits.

### Finding 5 (low severity): stderr socketpair already in place but wire is not

- **Evidence:** `crates/rsync_io/src/ssh/aux_channel.rs:264-285`
  installs a stderr socketpair on Unix. The wire still uses pipes
  (Finding 1), so the Unix data path is mixed: stderr uses one socket,
  the wire uses two pipes.
- **Impact:** The work to make stderr pollable has already been paid
  for, but the wire does not benefit. The asymmetry also makes the
  `SshConnection` struct slightly harder to reason about (two different
  IPC primitives on the same connection).
- **Recommended fix:** Land Finding 1's fix to unify the two paths and
  factor `configure_stderr_channel` and `configure_wire_channel` behind
  a shared `WireChannelKind::{Pipe, Socketpair}` enum. The factory
  pattern from `aux_channel.rs:263-291` is the model.

### Finding 6 (informational): no `MSG_INFO`/`MSG_ERROR` plumbing for local SSH-client stderr

- **Evidence:** `aux_channel.rs:208-228` forwards SSH-client stderr
  lines to `eprint!` rather than wrapping them in multiplex envelopes.
- **Impact:** This is correct because the lines come from the *local*
  SSH client (host-key warnings, connect errors) and never appear on
  the rsync wire. Upstream rsync does the same: see comment block at
  `pipe.c:99-108` and the fact that `io.c::send_msg` only multiplexes
  output produced by the rsync process itself, not by SSH. No fix
  needed; record this so we do not accidentally route SSH-client
  stderr into the multiplex envelope when refactoring.
- **Recommended fix:** none. Document the boundary in
  `aux_channel.rs` so future refactors do not blur it.

## 6. Recommendation

**Implement** the socketpair migration on Unix; keep the pipe path as the
permanent Windows fallback. The work is wire-compatible (no protocol
changes), additive, and aligns oc-rsync with upstream's IPC topology. The
auxiliary stderr socketpair stays as is.

### Phase 1: socketpair-backed wire on Unix

- **Spawn site:** in `crates/rsync_io/src/ssh/builder.rs::spawn`, before
  the existing `Command::spawn`, attempt `UnixStream::pair()`. On
  success, convert the child end to `OwnedFd` and pass it to the command
  via `Stdio::from(child_fd)` for both stdin and stdout (the same FD,
  duplicated by `Command` internally because the OS will `dup` for
  position 0 and 1). Retain the parent end as a single bidirectional
  `UnixStream`.
- On failure (e.g., `EMFILE`), fall back to the existing
  `Stdio::piped()` path. Mirror the structure of
  `aux_channel.rs::configure_stderr_channel` for symmetry.
- `SshConnection` keeps its current shape; the `stdin`/`stdout` fields
  become `Option<Box<dyn Read + Write + Send>>` or are unified into a
  single `wire: WireEnd` enum (`Pipe { stdin, stdout } | Socket(Arc<UnixStream>)`).
- `split()` returns reader/writer halves that, on the socket variant,
  hold an `Arc<UnixStream>` and a half-close marker. `SshWriter::close`
  becomes `shutdown(Shutdown::Write)` on the socket variant and a drop
  on the pipe variant.
- Set `set_nonblocking(true)` on the parent end (Finding 2) and add a
  small retry helper for `WouldBlock` to keep the blocking
  `Read`/`Write` API contract the rest of the crate expects.

Acceptance criteria:

- All existing SSH integration tests pass on Linux, macOS, and Windows
  unchanged (Windows continues on pipes).
- A new socketpair-specific test asserts that
  `shutdown(SHUT_WR)` from the parent surfaces EOF on the child's
  stdin while the parent can still read responses.
- Cross-version interop suite reports no regressions against rsync
  3.0.9, 3.1.3, and 3.4.1 daemons.

### Phase 2: unified-FD event loop integration

- Once phase 1 is in place, attach a single
  `mio::Poll`/`tokio::io::AsyncFd`/`io_uring` registration to the
  parent socketpair FD. Drive read- and write-readiness through one
  poll set, eliminating the need for separate reader and writer
  threads on the SSH path.
- Map readiness to the existing `SshReader`/`SshWriter` API surface so
  call sites in `crates/core/src/client/remote/ssh_transfer.rs` do not
  change.

### Phase 3: half-close hygiene

- Replace `SshConnection::close_stdin` (`connection.rs:97-102`) with a
  `shutdown(Shutdown::Write)` on the socket variant. Match upstream
  `cleanup.c:46-67` by issuing `shutdown(fd, 2)` before `close(fd)` on
  the abnormal-exit path so the kernel sends an orderly FIN rather than
  a RST. Keep the drop semantics on the pipe variant; pipes cannot
  shut down.

### Out of scope

- Any change to the wire protocol or multiplex envelope.
- A third FD for SSH-client stderr (Finding 6: no need; upstream does
  not do this and oc-rsync's existing stderr socketpair already covers
  every observable property).
- Windows. The pipe path stays. There is no productive way to wire an
  `AF_UNIX` socketpair to `CreateProcess` standard handles on Windows
  today.

## Risks

- **Rust FD-as-stdio semantics.** `Stdio::from(OwnedFd)` consumes the
  fd. To attach the *same* socketpair child end to both stdin and stdout,
  we must `dup` the fd before constructing the second `Stdio`. This is
  safe via `OwnedFd::try_clone` (which calls `fcntl(F_DUPFD_CLOEXEC)`)
  but the test must verify the cloexec flag survives `dup2`-into-stdio
  on every supported platform.
- **Different read/write buffer accounting on socket vs pipe.** Code
  that assumes 64 KiB pipe-buffer behaviour (today: none in oc-rsync's
  SSH path) would need to switch to `SO_RCVBUF`/`SO_SNDBUF` queries.
- **`splice(file -> wire)` no longer applies.** Splice requires one
  side to be a pipe. With the socketpair migration the wire is no
  longer a pipe, so the splice plan in `splice-ssh-stdio.md` does not
  fire on the parent->child direction. Mitigation: that plan already
  notes splice happens on the file<->page-cache edge, not across the
  SSH child; the SSH wire was never a splice candidate. Confirmed by
  re-reading `splice-ssh-stdio.md` lines 60-68.
- **`Drop` ordering.** Today `SshConnection::Drop` (`connection.rs:543`)
  drops the watchdog, then closes stdin, then waits on the child. With
  socketpair the close-stdin step becomes a `shutdown(SHUT_WR)`. Drop
  must still call `shutdown` before the inner `UnixStream` drops to
  preserve the orderly-FIN behaviour.
- **EAGAIN semantics under `set_nonblocking(true)`.** The blocking
  `Read`/`Write` impls must wrap a retry loop or, in the event-loop
  variant, hand readiness off to the runtime. Mistakes here cause busy
  spins. Mitigation: add a unit test that pins the FD blocking and
  asserts a single-shot read returns the expected number of bytes.

## Follow-up tasks

- [ ] #1690 implement Unix socketpair-backed wire in
      `crates/rsync_io/src/ssh/builder.rs::spawn`, with pipe fallback.
- [ ] #1691 unify `SshReader`/`SshWriter` over an `Arc<UnixStream>` on
      the socket variant and switch `close_stdin` to
      `shutdown(SHUT_WR)`.
- [ ] #1692 set `O_NONBLOCK` on the parent end and add a `WouldBlock`
      retry helper (`crates/rsync_io/src/ssh/connection.rs`).
- [ ] #1693 add cross-version interop test that exercises
      `shutdown(SHUT_WR)`-based half-close against rsync 3.0.9, 3.1.3,
      3.4.1.
- [ ] #1694 (event loop) wire the unified FD into the poll/epoll/io_uring
      readiness machinery; eliminate per-direction threads on the SSH
      path. Depends on #1690-#1692.
- [ ] #1695 document Windows pipe-only path in
      `crates/rsync_io/src/ssh/mod.rs` once the Unix path lands so the
      asymmetry is explicit.

## References

- Upstream: `target/interop/upstream-src/rsync-3.4.1/pipe.c:48-178`
  (`piped_child`, `local_child`).
- Upstream: `target/interop/upstream-src/rsync-3.4.1/util1.c:74-96`
  (`fd_pair` socketpair-or-pipe wrapper).
- Upstream: `target/interop/upstream-src/rsync-3.4.1/cleanup.c:46-67`
  (`close_all`, orderly socket shutdown).
- Upstream: `target/interop/upstream-src/rsync-3.4.1/io.c:983-1031`
  (multiplex `send_msg`, the upstream substitute for an auxiliary
  stderr channel).
- oc-rsync: `crates/rsync_io/src/ssh/builder.rs:266-321`
  (`SshCommand::spawn`).
- oc-rsync: `crates/rsync_io/src/ssh/connection.rs:178-208`
  (`SshConnection::split`).
- oc-rsync: `crates/rsync_io/src/ssh/aux_channel.rs:138-193`
  (`SocketpairStderrChannel` - the existing socketpair user).
- oc-rsync: `crates/rsync_io/src/ssh/aux_channel.rs:263-291`
  (`configure_stderr_channel` - the factory pattern to mirror for the
  wire).
- oc-rsync: `crates/rsync_io/src/ssh/mod.rs:57-72` (io_uring/splice
  applicability notes that constrain phase 2/3 design).
- Companion audit: `docs/audits/splice-ssh-stdio.md` (splice plan that
  remains valid under both topologies).
