# SSH socketpair vs anonymous-pipe wire claim - source verification (#1902)

Tracker: #1902. Companion to `docs/audits/ssh-socketpair-vs-pipes.md` (#1938).
No code changes - documentation only.

## 1. Scope

This document re-verifies the claims made in
`docs/audits/ssh-socketpair-vs-pipes.md` against the current
`crates/rsync_io` and `crates/core` source tree on branch
`chore/remove-delta-stats-known-failure`. The original audit was written
against an earlier checkout; this verification confirms the cited
behaviour still holds and refreshes line numbers that have drifted.

The questions the task tracker asks:

1. Does oc-rsync use `socketpair()` or `stdin`/`stdout` pipes for the SSH
   subprocess wire?
2. Is bidirectional traffic over a single socketpair the actual data
   flow, or is it a `stdin` pipe plus a `stdout` pipe pair?
3. Does stderr go through a socketpair auxiliary channel or a separate
   stderr pipe?

The short answer to all three: **two anonymous pipes for the wire (one
for stdin, one for stdout); one auxiliary `socketpair(AF_UNIX,
SOCK_STREAM)` for stderr on Unix, falling back to an anonymous pipe on
Windows or socketpair-creation failure.** This matches Sections 3.1 and
3.3 of `ssh-socketpair-vs-pipes.md` and contradicts any reading of the
audit that would suggest the wire itself is socketpair-backed.

There is no separate `transport` crate in this workspace; the
`crates/transport/...` paths referenced in the task description map to
`crates/rsync_io/src/ssh/` (transport primitives) and
`crates/core/src/client/remote/ssh_transfer.rs` (client orchestration).

## 2. Claim-by-claim verification

Each row cites a specific assertion from
`docs/audits/ssh-socketpair-vs-pipes.md`, the current source location
that supports or refutes it, and the verification status.

### 2.1 Wire primitive

| Claim | Source assertion | Source path:line | Status |
|---|---|---|---|
| oc-rsync wire uses anonymous `pipe(2)` for stdin and stdout | `Stdio::piped()` for both stdin and stdout | `crates/rsync_io/src/ssh/builder.rs:322-323` | VERIFIED |
| Stdin and stdout are not reconfigured to a socketpair anywhere | No `socketpair`/`UnixStream::pair`/`dup2` of a non-pipe FD onto wire stdio | `crates/rsync_io/src/ssh/builder.rs` (entire file), `crates/rsync_io/src/ssh/connection.rs` (entire file) | VERIFIED |
| The wire is two unidirectional handles (`ChildStdin`, `ChildStdout`) | `SshConnection { stdin: Option<ChildStdin>, stdout: Option<ChildStdout>, ... }` | `crates/rsync_io/src/ssh/connection.rs:30-39` | VERIFIED |

Doc claim "wire uses pipes, not socketpair": **VERIFIED** at
`crates/rsync_io/src/ssh/builder.rs:322-323`. The lines read:

```rust
command.stdin(Stdio::piped());
command.stdout(Stdio::piped());
```

These are the only configurations applied to the wire stdio of the
spawned SSH child. `Stdio::piped()` is the stdlib idiom that creates an
anonymous `pipe(2)` pair internally.

### 2.2 Bidirectional flow uses two FDs, not one

| Claim | Source assertion | Source path:line | Status |
|---|---|---|---|
| Read half wraps `ChildStdout` | `SshReader { stdout: ChildStdout }` and `Read` impl delegates to `self.stdout.read` | `crates/rsync_io/src/ssh/connection.rs:213-221` | VERIFIED |
| Write half wraps `ChildStdin` | `SshWriter { stdin: ChildStdin }` and `Write` impl delegates to `self.stdin.write` / `flush` | `crates/rsync_io/src/ssh/connection.rs:225-237` | VERIFIED |
| `split()` returns two independent FDs | `Ok((SshReader { stdout }, SshWriter { stdin }, SshChildHandle { ... }))` | `crates/rsync_io/src/ssh/connection.rs:178-208` | VERIFIED |
| `close_stdin` flushes and drops `ChildStdin`, no `shutdown(SHUT_WR)` | `if let Some(mut stdin) = self.stdin.take() { stdin.flush()?; }` | `crates/rsync_io/src/ssh/connection.rs:97-102` | VERIFIED |
| Half-close on `SshWriter` is also a flush+drop (no `shutdown`) | `pub fn close(mut self) -> io::Result<()> { self.stdin.flush() }` | `crates/rsync_io/src/ssh/connection.rs:241-243` | VERIFIED |
| No `shutdown(2)` is called on the wire FDs anywhere in `crates/rsync_io/src/ssh` | Repository-wide grep for `shutdown\|SHUT_WR` returns zero matches in `crates/rsync_io/src/ssh/` | `crates/rsync_io/src/ssh/` (zero matches) | VERIFIED |

Doc claim "bidirectional traffic flows over two unidirectional pipes,
not one socketpair": **VERIFIED**. The `SshConnection` carries two
distinct `Option<ChildStdin>`/`Option<ChildStdout>` fields and `split()`
hands them to two distinct half-types (`SshReader`, `SshWriter`). The
`Read` impl reads only from stdout, the `Write` impl writes only to
stdin, and there is no single FD that backs both directions.

The `crates/core/src/client/remote/ssh_transfer.rs:551-553` consumer
makes the two-FD usage explicit:

```rust
let (mut reader, mut writer, mut child_handle) = connection
    .split()
    .map_err(|e| invalid_argument_error(...))?;
```

The transfer loop then passes `&mut reader` and `&mut writer` to
`server::run_server_with_handshake` as separate trait objects.

### 2.3 Stderr uses a socketpair auxiliary channel (Unix), pipe fallback (Windows / failure)

| Claim | Source assertion | Source path:line | Status |
|---|---|---|---|
| Unix path tries `socketpair(AF_UNIX, SOCK_STREAM, 0)` via `UnixStream::pair()` | `match UnixStream::pair() { Ok((parent, child)) => { ... command.stderr(Stdio::from(child_fd)); Some(parent) } Err(_) => { command.stderr(Stdio::piped()); None } }` | `crates/rsync_io/src/ssh/aux_channel.rs:264-285` | VERIFIED |
| Unix fallback to `Stdio::piped()` when `UnixStream::pair()` fails | The `Err(error)` arm sets `command.stderr(Stdio::piped())` | `crates/rsync_io/src/ssh/aux_channel.rs:275-283` | VERIFIED |
| Non-Unix arm always uses `Stdio::piped()` | `#[cfg(not(unix))] pub(super) fn configure_stderr_channel(...) { command.stderr(Stdio::piped()); None }` | `crates/rsync_io/src/ssh/aux_channel.rs:287-291` | VERIFIED |
| `SocketpairStderrChannel` reads from the parent half via a drain thread | `thread::Builder::new().name("ssh-stderr-drain-socketpair".into()).spawn(move \|\| drain_loop(parent_end, &thread_buffer))` | `crates/rsync_io/src/ssh/aux_channel.rs:146-193` | VERIFIED |
| `PipeStderrChannel` exists as the cross-platform fallback | `pub(super) struct PipeStderrChannel { handle: Option<JoinHandle<()>>, buffer: Arc<Mutex<Vec<u8>>> }` and its `spawn` method | `crates/rsync_io/src/ssh/aux_channel.rs:97-118` | VERIFIED |
| `build_stderr_channel` selects socketpair when present, pipe otherwise | `if let Some(parent) = parent_socketpair_end { Some(Box::new(SocketpairStderrChannel::spawn(parent))) } else { child_stderr.map(\|stderr\| Box::new(PipeStderrChannel::spawn(stderr)) as BoxedStderrChannel) }` | `crates/rsync_io/src/ssh/aux_channel.rs:299-308` | VERIFIED |

Doc claim "stderr uses a socketpair auxiliary channel on Unix, separate
from the wire": **VERIFIED**. The stderr socketpair is always a third
file descriptor, distinct from the stdin and stdout pipe FDs. On
Windows or when `UnixStream::pair()` fails, stderr is an anonymous pipe
(via `Stdio::piped()`), still distinct from the wire pipes.

The drain is line-oriented (`read_until(b'\n')`) and never multiplexes
back onto the rsync wire; SSH-client diagnostics are forwarded to the
parent's own stderr via `eprint!`
(`crates/rsync_io/src/ssh/aux_channel.rs:208-228`).

### 2.4 Findings 1-6 from the original audit

| Finding | Original assertion | Current evidence | Status |
|---|---|---|---|
| F1: wire uses `pipe(2)` where upstream uses `socketpair` | `Stdio::piped()` at `builder.rs:300-301` | Now `crates/rsync_io/src/ssh/builder.rs:322-323` | VERIFIED (line numbers drifted by ~22) |
| F2: parent stdio is blocking, upstream is non-blocking | "No `set_nonblocking` call in `crates/rsync_io/src/ssh/builder.rs` or `connection.rs`" | Repository-wide grep for `set_nonblocking\|nonblocking` in `crates/rsync_io/src/ssh/` returns zero matches | VERIFIED |
| F3: stderr socketpair already in place; wire is not | `aux_channel.rs:263-285` socketpair, `builder.rs:300-301` pipes | `crates/rsync_io/src/ssh/aux_channel.rs:264-285` and `crates/rsync_io/src/ssh/builder.rs:322-323` | VERIFIED (line numbers drifted by ~1 / ~22) |
| F4: `close_stdin` cannot half-close cleanly | `connection.rs:96-102` and `connection.rs:241-243` flush+drop, no `shutdown(2)` | `crates/rsync_io/src/ssh/connection.rs:97-102` and `crates/rsync_io/src/ssh/connection.rs:241-243` | VERIFIED (line numbers drifted by ~1) |
| F5: io_uring socket fast paths unreachable on the SSH wire | `mod.rs:57-75` documents the consequence | `crates/rsync_io/src/ssh/mod.rs:57-75` text matches verbatim | VERIFIED |
| F6: stderr forwarding is not multiplexed onto the wire | `aux_channel.rs:208-228` writes to local `eprint!` | `crates/rsync_io/src/ssh/aux_channel.rs:208-228` confirms `eprint!("{text}")` | VERIFIED |

### 2.5 Section 3.7 "Summary of oc-rsync defaults" table

The table claims:

| Channel | Primitive | Original cite | Current cite | Status |
|---|---|---|---|---|
| SSH wire stdin (parent->child) | `pipe(2)` | `builder.rs:300` | `crates/rsync_io/src/ssh/builder.rs:322` | VERIFIED |
| SSH wire stdout (child->parent) | `pipe(2)` | `builder.rs:301` | `crates/rsync_io/src/ssh/builder.rs:323` | VERIFIED |
| SSH stderr (Unix) | `socketpair(AF_UNIX, SOCK_STREAM, 0)` | `aux_channel.rs:265` | `crates/rsync_io/src/ssh/aux_channel.rs:265` | VERIFIED |
| SSH stderr (Windows) | `pipe(2)` | `aux_channel.rs:288-291` | `crates/rsync_io/src/ssh/aux_channel.rs:287-291` | VERIFIED (line numbers drifted by 1) |
| SSH stderr (Unix fallback) | `pipe(2)` | `aux_channel.rs:276` | `crates/rsync_io/src/ssh/aux_channel.rs:276` | VERIFIED |
| Daemon `rsync://` wire | `TcpStream` (out of scope here) | `crates/transport/` | Daemon TCP path lives in `crates/core/src/client/remote/daemon_transfer/` and `crates/rsync_io/src/`; no separate `transport` crate | VERIFIED (path nomenclature differs; primitive is unchanged) |
| Local-fork wire | not implemented | n/a | No fork-and-exec of `oc-rsync` itself; remote operands always go through SSH or the daemon TCP socket | VERIFIED |

### 2.6 Companion claim: io_uring boundary docstring

The audit relies on the `mod.rs` docstring being accurate as a hand-off
to #1859 / #1860. The current docstring at
`crates/rsync_io/src/ssh/mod.rs:57-75` states verbatim:

> The SSH data channel is the spawned `ssh` child's inherited stdio: a
> `(stdin, stdout)` pipe pair created by `Command::spawn`, not a socket.

VERIFIED. No edit required.

## 3. Line-number drift summary

The original audit (`ssh-socketpair-vs-pipes.md`) was written against an
earlier checkout. Several citations have drifted because new code was
added higher in the same files (notably an `arm()` watchdog that grew
from 246 to 257). All drift is mechanical; no claim was invalidated.

| Original cite | Current cite | Drift | Cause |
|---|---|---|---|
| `builder.rs:285-340` (`spawn`) | `crates/rsync_io/src/ssh/builder.rs:307-362` | +22 | New rustdoc paragraph added above `spawn` |
| `builder.rs:300-301` (`Stdio::piped()`) | `crates/rsync_io/src/ssh/builder.rs:322-323` | +22 | Same |
| `connection.rs:30-39` (`SshConnection`) | `crates/rsync_io/src/ssh/connection.rs:30-39` | 0 | unchanged |
| `connection.rs:96-102` (`close_stdin`) | `crates/rsync_io/src/ssh/connection.rs:97-102` | +1 | Adjacent rustdoc growth |
| `connection.rs:178-208` (`split`) | `crates/rsync_io/src/ssh/connection.rs:178-208` | 0 | unchanged |
| `connection.rs:217-237` (`Read`/`Write` impls) | `crates/rsync_io/src/ssh/connection.rs:217-237` | 0 | unchanged |
| `connection.rs:241-243` (`SshWriter::close`) | `crates/rsync_io/src/ssh/connection.rs:241-243` | 0 | unchanged |
| `connection.rs:246-322` (`ConnectWatchdog`) | `crates/rsync_io/src/ssh/connection.rs:257-329` | +11 | `ConnectWatchdog` rustdoc grew |
| `aux_channel.rs:138-193` (`SocketpairStderrChannel`) | `crates/rsync_io/src/ssh/aux_channel.rs:146-193` | +8 | Trait `StderrAuxChannel` rustdoc grew |
| `aux_channel.rs:263-291` (`configure_stderr_channel`) | `crates/rsync_io/src/ssh/aux_channel.rs:264-291` | +1 | Adjacent comment growth |
| `aux_channel.rs:208-228` (`drain_loop`) | `crates/rsync_io/src/ssh/aux_channel.rs:208-228` | 0 | unchanged |
| `mod.rs:57-75` (io_uring boundary docstring) | `crates/rsync_io/src/ssh/mod.rs:57-75` | 0 | unchanged |

If `ssh-socketpair-vs-pipes.md` is republished, refreshing these line
numbers is the only edit needed; the prose is correct.

## 4. Items NEEDS-MORE-EVIDENCE

None at the source-of-truth level. The original audit's upstream
citations (`pipe.c:48-97`, `util1.c:74-96`, `main.c:629`, `main.c:985`,
`clientserver.c:116-148`, `socket.c:736-846`, `io.c:983-1031`,
`cleanup.c:46-67`) cannot be re-verified from this worktree because
`target/interop/upstream-src/rsync-3.4.1/` is not unpacked here. They
are sourced from the upstream tarball pinned by `tools/ci/run_interop.sh`
and were verified during the original audit (#1938 / PR #3438).
Re-fetching the tarball is a one-command operation
(`bash tools/ci/run_interop.sh` or the `curl | tar` invocation in the
project notes); the verification status of those upstream citations is
NEEDS-MORE-EVIDENCE only in the narrow sense that this PR's diff did
not re-derive them.

The on-our-side claims-which is what the task tracker asks-are all
VERIFIED.

## 5. Items REFUTED

None. Every claim from `ssh-socketpair-vs-pipes.md` about
`crates/rsync_io/src/ssh/` survives a line-by-line re-read of the
current source.

## 6. Conclusion

The three task-tracker questions resolve as follows:

1. **Pipes or socketpair for SSH subprocess communication?** Anonymous
   pipes for the wire (stdin and stdout). Verified at
   `crates/rsync_io/src/ssh/builder.rs:322-323`.
2. **Single socketpair or stdin pipe + stdout pipe?** Two pipes, one
   per direction, with two distinct half-types
   (`SshReader`/`SshWriter`). Verified at
   `crates/rsync_io/src/ssh/connection.rs:30-39, 178-237`.
3. **Stderr socketpair or separate stderr pipe?** Socketpair on Unix
   when `UnixStream::pair()` succeeds (always a third FD, never
   multiplexed onto the wire), pipe fallback otherwise. Verified at
   `crates/rsync_io/src/ssh/aux_channel.rs:264-308`.

#1902 disposition: **verified**, with the line-number refresh in
Section 3 as the only follow-up edit if `ssh-socketpair-vs-pipes.md` is
republished.

## 7. References

oc-rsync source paths verified in this audit:

- `crates/rsync_io/src/ssh/mod.rs:57-75` - io_uring boundary docstring.
- `crates/rsync_io/src/ssh/builder.rs:307-362` - `SshCommand::spawn`.
- `crates/rsync_io/src/ssh/builder.rs:322-323` - `Stdio::piped()` for
  wire stdin/stdout.
- `crates/rsync_io/src/ssh/builder.rs:334` - call to
  `configure_stderr_channel`.
- `crates/rsync_io/src/ssh/connection.rs:30-39` - `SshConnection` fields.
- `crates/rsync_io/src/ssh/connection.rs:97-102` -
  `SshConnection::close_stdin`.
- `crates/rsync_io/src/ssh/connection.rs:178-208` -
  `SshConnection::split`.
- `crates/rsync_io/src/ssh/connection.rs:213-221` - `SshReader` and
  blocking `Read` impl.
- `crates/rsync_io/src/ssh/connection.rs:225-243` - `SshWriter`,
  blocking `Write` impl, and `SshWriter::close`.
- `crates/rsync_io/src/ssh/connection.rs:257-329` - `ConnectWatchdog`.
- `crates/rsync_io/src/ssh/aux_channel.rs:97-136` - `PipeStderrChannel`.
- `crates/rsync_io/src/ssh/aux_channel.rs:146-193` -
  `SocketpairStderrChannel` (Unix only).
- `crates/rsync_io/src/ssh/aux_channel.rs:208-228` - `drain_loop` (line
  forwarding via `eprint!`, not multiplexed onto the wire).
- `crates/rsync_io/src/ssh/aux_channel.rs:264-291` -
  `configure_stderr_channel` (Unix socketpair / pipe fallback / non-Unix
  pipe).
- `crates/rsync_io/src/ssh/aux_channel.rs:299-316` -
  `build_stderr_channel` (selects socketpair when available, pipe
  otherwise).
- `crates/core/src/client/remote/ssh_transfer.rs:300` - the only
  `SshCommand::spawn` call site.
- `crates/core/src/client/remote/ssh_transfer.rs:551-553` -
  `SshConnection::split` consumer (the two-FD wire model in active use).

Companion documents:

- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - the source-of-truth
  audit this document re-verifies.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - io_uring on pipe FDs.
- `docs/audits/async-ssh-transport.md` (#2068) - async migration that
  would re-open the socketpair question.
