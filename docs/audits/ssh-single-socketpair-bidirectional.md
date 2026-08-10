# SSH single bidirectional socketpair feasibility (#1687)

Tracker: #1687 (prototype SSH subprocess using socketpair for bidirectional
I/O). No code changes - documentation only.

Companion audits:

- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - compared anonymous
  pipes against upstream's *two-socketpairs-per-direction* topology and
  recommended keeping pipes.
- `docs/audits/ssh-socketpair-claim-verification.md` (#1902) - confirmed
  oc-rsync's wire is two anonymous pipes, stderr is one `AF_UNIX`
  socketpair.

This audit narrows the scope to the specific variant #1687 describes:
**replace both wire pipes with a single bidirectional `socketpair(AF_UNIX,
SOCK_STREAM)`** where the same FD is `dup2`-ed onto the child's fd 0 and
fd 1. The #1938 audit ruled out the two-socketpair variant; this one
addresses why the more aggressive single-FD variant also does not
work.

## 1. Summary

The "single socketpair shared between stdin and stdout" topology cannot
ship as a supported configuration:

1. **OpenSSH separates protocol traffic from prompts via fd 2.** Forcing
   the child's fd 0 and fd 1 onto the same socket end does not break
   prompts (those go to fd 2, which we already wire to a separate
   socketpair via `aux_channel.rs`). But it *does* conflate any data the
   ssh client legitimately writes to fd 1 (e.g., the
   `Pseudo-terminal will not be allocated...` banner upstream emits when
   `LogLevel=INFO` and TTY allocation fails) with the rsync protocol
   bytes the remote rsync writes to its stdout. Pipes today have the same
   conflation risk, mitigated by `-oBatchMode=yes`; a single socketpair
   does not improve that.

2. **No upstream parity gain.** Upstream rsync uses *two* socketpairs
   (one per direction), not one shared FD - see Section 2.1 below. A
   single-FD variant diverges from upstream just as the current pipe
   topology does, while removing the property that motivated upstream's
   choice (independent half-close per direction).

3. **Loses `splice(2)` eligibility.** Same blocker as the two-socketpair
   variant audited in `ssh-socketpair-vs-pipes.md:471-484`. `splice(2)`
   requires one of its two FDs to be a pipe. A socket on the wire
   negates the zero-copy plan in #1860.

4. **No simplification on the parent side.** The `Read` and `Write`
   halves of `SshConnection` would have to share the single FD via
   `Arc<UnixStream>` or `OwnedFd::try_clone` to keep
   `SshReader`/`SshWriter` separable for the existing threaded model.
   That is more code, not less, than the current two-FD model where
   `ChildStdin` and `ChildStdout` are independently owned.

5. **Half-close semantics regress, not improve.** With separate FDs,
   dropping `ChildStdin` cleanly closes only the write direction (the
   child sees EOF on fd 0; the parent retains fd 1 for draining). With
   a shared FD, `shutdown(SHUT_WR)` on the parent end half-closes the
   parent's write side, but the *child* still sees both fd 0 and fd 1 as
   the same socket - closing the parent's write half does not close the
   child's read half on fd 0 because the child holds its own reference
   via fd 1.

**Recommendation:** close #1687 as "do not implement" with this audit
(and the companion #1938 audit) as justification. Add a one-line note to
`crates/rsync_io/src/ssh/mod.rs` cross-referencing both audits so the
disposition is discoverable from the source.

## 2. Upstream reference for "single socketpair"

### 2.1 Upstream does not use this topology

`target/interop/upstream-src/rsync-3.4.1/pipe.c:48-97` (`piped_child`)
allocates *two* `fd_pair` results - one for the parent->child direction
(stdin) and one for the child->parent direction (stdout). The child
inherits two distinct FDs (the read end of the first pair on fd 0; the
write end of the second pair on fd 1). Each direction's parent and
child ends are independent.

The single-FD variant would be `dup2(sp[0], 0); dup2(sp[0], 1);` on the
child after a single `socketpair`. Upstream does not do this and there
is no `HAVE_SOCKETPAIR`-guarded code path that does. The closest
analogue is the daemon-mode `f_in == f_out` pattern at
`clientserver.c:174-175`, but that uses a **TCP** socket (`open_socket_out`)
where bidirectional sharing is natural, not a `socketpair`.

### 2.2 Why a single FD looks attractive on paper

The argument for one FD is symmetry: `f_in == f_out` is already legal
inside the rsync engine for daemon-mode transfers (Section 2.1 of the
companion audit). Reusing the same pattern for SSH avoids the two-half
`SshConnection` split. In the daemon path, a single TCP socket carries
both directions and `shutdown(Shutdown::Write)` is the canonical
half-close. Extending that uniformity to SSH appears to consolidate
the API surface.

The rest of this document explains why the appearance is misleading
for SSH specifically.

## 3. The SSH-client interaction model

`ssh(1)` is a third-party binary oc-rsync invokes via
`Command::spawn`. The relevant I/O contract is documented in
`ssh(1)` and `OpenSSH 9.x` source under `clientloop.c::client_loop`:

- **fd 0 (stdin):** read end of the client_loop data pump. Bytes here
  are forwarded to the remote `exec` channel's stdin (the remote
  rsync's stdin).
- **fd 1 (stdout):** write end of the client_loop data pump. Bytes
  arriving from the remote `exec` channel's stdout are written here.
- **fd 2 (stderr):** out-of-band diagnostics from the ssh client
  itself: host-key fingerprint prompts (`StrictHostKeyChecking=ask`),
  password/passphrase prompts (when no askpass helper is available
  and stdin is non-TTY), `Permission denied`, `Connection refused`,
  banner messages, debug output.

The ssh client uses standard `read(2)` / `write(2)` on fd 0 / fd 1.
It does not call `fstat`/`getsockopt` to discriminate sockets from
pipes; either is acceptable as a byte sink. **No prompt is ever written
to fd 1**: see `OpenSSH 9.x sshconnect2.c::userauth_passwd` and
`sshlogin.c::sys_auth_password`, both of which route to `readpass.c`
which writes to `/dev/tty` if available or `STDERR_FILENO` otherwise.
The same routing applies to `ssh-askpass` launches.

This is the basis for the task's hint that prompts go to stderr.
The hint is correct and confirms that prompts do not contaminate the
data stream regardless of whether fd 0 and fd 1 are pipes or a
socketpair. So prompt routing alone does not block the migration.

**What prompts do affect:** `oc-rsync` injects `-oBatchMode=yes` for
ssh clients (`builder.rs:381-383`). That option suppresses all prompts:
host-key acceptance, password, passphrase. So in the default
non-interactive configuration prompts are not emitted at all. The only
remaining stderr traffic is error messages and banners, which the
existing stderr socketpair handles correctly.

## 4. Why "single socketpair" still does not ship

### 4.1 Child-side fd 0 / fd 1 are not interchangeable to the kernel

`Command::spawn` (`std::process::Command`) calls `dup2` on the child
side after fork to map the supplied `Stdio` handles onto fds 0, 1, 2.
The stdlib's `Stdio::from(OwnedFd)` machinery accepts any
`OwnedFd`, including one cloned from a `UnixStream`. To get a single
socket onto both fd 0 and fd 1 in the child, the parent must:

1. Create the socketpair: `let (parent, child) = UnixStream::pair()?`.
2. `try_clone` the child end so the same underlying socket appears
   twice: `let child_b = child.try_clone()?`.
3. Pass each clone as a separate `Stdio`: `cmd.stdin(Stdio::from(child.into()))`
   and `cmd.stdout(Stdio::from(child_b.into()))`.

After fork, the kernel `dup2`s clone A onto fd 0 and clone B onto
fd 1 in the child. Both fds reference the same underlying open file
description, so reads on fd 0 and writes on fd 1 hit the same
socket buffer pair as one bidirectional endpoint.

This is mechanically possible. The cost is two extra `dup`-class
syscalls per spawn and a slightly subtler ownership model in the
parent. So far, neutral.

### 4.2 ssh client behaviour with shared fd 0 / fd 1

`ssh(1)`'s client_loop performs blocking `read(STDIN_FILENO, ...)` and
blocking `write(STDOUT_FILENO, ...)` independently. If both FDs back
the same socket, there is no semantic change because socket reads and
writes are independent operations on a duplex endpoint - the kernel
buffer separates ingress and egress.

The risk is asymmetric `shutdown(2)` propagation, addressed in 4.4.

### 4.3 The remote rsync's view is the same as today

The remote `rsync --server` runs on the SSH server side and reads
its own fd 0, writes its own fd 1, both `pipe(2)` pairs created by
`sshd`. The wire protocol between the two rsync processes is unchanged
- the ssh tunnel marshals bytes blindly. Choice of pipe vs socketpair
between the *local* oc-rsync and the *local* ssh client is invisible
on the wire. This rules out interop regressions but does not motivate
the change either.

### 4.4 Half-close semantics on a shared FD

The current two-FD model:

```
parent's ChildStdin (write end of stdin pipe)  --[pipe]--> child's fd 0 (ssh reads)
child's fd 1 (ssh writes)  --[pipe]-->  parent's ChildStdout (read end of stdout pipe)
```

Dropping `ChildStdin` closes only the parent's write end of the stdin
pipe. The child sees EOF on fd 0, the ssh client's client_loop
forwards EOF to the remote rsync, but the stdout pipe is untouched and
the parent can still read any pending bytes from the remote. This is
the rsync end-of-transfer dance.

The single-FD model:

```
parent's UnixStream  <--socketpair-->  child's fd 0 == fd 1 (same socket end)
```

`parent.shutdown(Shutdown::Write)?` half-closes the parent's write
direction on the socket. The kernel signals EOF on the child's read
side. **But the child holds two file-descriptor references to the same
socket end** (fd 0 and fd 1, planted by `dup2`). Closing the parent's
write side does signal read EOF to the kernel buffer the child is
reading from; that part works. The complication is the *converse*:
if the child closes its fd 0 (e.g., the ssh client decides the input
channel is done because the remote closed stdin), the kernel's
reference count on the underlying socket end is still 1 because fd 1
remains open. The parent's read side of the socket does not see EOF
until the child closes fd 1 too. With pipes, fd 0 and fd 1 are
independent kernel objects and the parent observes EOF on stdout
exactly when the child closes its fd 1.

In rsync's protocol this matters at the goodbye phase. The sender
signals end-of-transfer by closing its stdin (the child reads EOF on
fd 0); the receiver drains and then closes its stdout (the parent
reads EOF on its stdout pipe). With one shared socket, the receiver's
EOF-on-stdout signal is delayed until the child explicitly closes fd 1,
which is not guaranteed because ssh's client_loop typically only closes
fd 1 when the underlying channel is closed by the remote.

This is not a hypothetical: it would require the ssh client to
cooperate with the half-close protocol on the shared FD, which it
does not.

### 4.5 `splice(2)` eligibility regression

Identical analysis to `ssh-socketpair-vs-pipes.md:471-484`. The
zero-copy plan tracked by #1860 (`splice-ssh-stdio.md`) requires the
wire FD to be a pipe so `splice(file_fd, NULL, wire, NULL, ...)`
remains legal. A socketpair-backed wire makes splice illegal and
forces a user-space pipe intermediary, defeating zero copy.

### 4.6 No parent-side simplification

The parent already exposes `SshReader` and `SshWriter` after `split()`
(`connection.rs:178-208`). The current code holds `ChildStdin` and
`ChildStdout`, each one owning its own FD. To preserve the split API
with a shared socket, the parent must wrap the single `UnixStream` in
`Arc<UnixStream>` (or `OwnedFd::try_clone` per half) and dispatch
`Read`/`Write` through the wrapper. That is strictly more code, more
indirection, and the same number of `read`/`write` syscalls. There is
no parent-side win.

The only API surface that genuinely simplifies is `close_stdin`,
which would become `parent.shutdown(Shutdown::Write)?`. But the
current implementation (`connection.rs:96-102`) is already two lines.
Saving one syscall in the parent does not justify the other costs.

### 4.7 Cross-platform impact

Windows has no `socketpair(2)`. `std::os::unix::net::UnixStream::pair`
is `#[cfg(unix)]`. Any prototype must `#[cfg(unix)]`-gate the
single-FD variant and keep a `#[cfg(windows)]` pipe path. This
doubles the test matrix for a feature that yields no measured win on
either platform.

## 5. Decision matrix (single FD vs status quo)

| Dimension | Pipes (status quo) | Single socketpair | Notes |
|---|---|---|---|
| Wire FD count (parent side) | 2 | **1** | API surface essentially unchanged after `Arc<UnixStream>` wrapping. |
| Child FD count | 2 (distinct objects) | 2 (clones of one object) | Two `dup2`s either way. |
| ssh prompts go to stderr | yes | yes | Orthogonal; both topologies use a separate fd 2 socketpair. |
| ssh client compatibility | identical | identical | ssh treats fd 0 / fd 1 as bytestreams. |
| Wire bytes on the line | identical | identical | rsync protocol unchanged. |
| Half-close on stdin (`SHUT_WR`) | by drop only | `shutdown(SHUT_WR)` available | Loses one-direction-only EOF semantics, see 4.4. |
| EOF on stdout from child's fd 1 close | observed immediately | **delayed until child closes fd 1 as well** | Goodbye phase regression risk. |
| Upstream parity (`pipe.c::piped_child`) | divergent | divergent | Upstream uses two FDs (two socketpairs). |
| `splice(2)` eligibility (#1860) | **preserved** | broken | Same blocker as the two-socketpair variant. |
| io_uring opcode coverage | `OP_READ`/`OP_WRITE` only | also `OP_RECV`/`OP_SEND` | Sequential bulk I/O parity in practice. |
| Backpressure / default buffer | 64 KiB | ~212 KiB (`SO_*BUF`) | No measured oc-rsync stall on either. |
| Code complexity | 0 | ~150 LOC + Windows fallback | Plus shared-FD ownership wrapping. |
| Cross-platform | identical Unix/Windows | Unix only; pipes on Windows | Asymmetric `cfg` branches. |
| `Arc<UnixStream>` contention | n/a | per-half lock or `try_clone` per spawn | Indirection for no measured win. |

The matrix tilts to the status quo for the same reason as the
two-socketpair variant, with the additional EOF-propagation concern
specific to the shared-FD case.

## 6. Recommendation

**Do not prototype the single bidirectional socketpair variant.** Close
#1687 as "do not implement" with this audit as justification. The
combined argument is:

1. **No splice eligibility (Section 4.5)** - identical blocker to the
   two-socketpair variant. The zero-copy plan in #1860 takes priority.
2. **EOF-propagation regression at goodbye (Section 4.4)** - shared-FD
   semantics delay the parent's observation of EOF on the stdout
   direction until the child explicitly closes its fd 1, which the
   ssh client does not do on a per-direction basis.
3. **No parent-side simplification (Section 4.6)** - `Arc<UnixStream>`
   plus per-half dispatch is at best a wash and at worst more code than
   the current two-FD model.
4. **No upstream parity (Sections 2.1, 5)** - upstream does not use this
   topology either, so the change does not move oc-rsync closer to
   `pipe.c::piped_child`. It picks a third variant unrelated to either.
5. **Cross-platform asymmetry (Section 4.7)** - Windows has to keep the
   pipe path, so the variant adds a Unix-only code path with no
   companion behaviour change on Windows.

The existing per-direction half-close (drop `ChildStdin`) already
provides the rsync goodbye semantics correctly, and the existing
`ConnectWatchdog` (`connection.rs:246-322`) covers the connect-timeout
case the task description cites under "cleanup behaviour".

If a future async-transport refactor (#2068,
`docs/audits/async-ssh-transport.md`) lands and benchmarks demonstrate
a socket-specific win, this disposition may be revisited - but the
revisit should consider the two-socketpair (per-direction) variant
from `ssh-socketpair-vs-pipes.md` before the single-FD variant, because
the per-direction variant preserves correct EOF semantics.

## 7. Documentation note

Add a one-line cross-reference at
`crates/rsync_io/src/ssh/mod.rs` pointing at both audits so future
contributors investigating this topology find the disposition without
re-deriving it. This audit deliberately stays in `docs/audits/` and
does not modify code.

## 8. References

Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`):

- `pipe.c:48-97` `piped_child` - two `fd_pair` calls, not one.
- `util1.c:74-96` `fd_pair` - `socketpair`-or-`pipe` wrapper.
- `clientserver.c:174-175` - daemon `f_in == f_out` over a TCP socket
  (not a socketpair; not analogous to SSH).
- `io.c:983-1031` `send_msg` - multiplex envelope for remote-rsync
  diagnostics (independent of SSH stderr).

OpenSSH 9.x:

- `clientloop.c::client_loop` - blocking `read(STDIN_FILENO)` /
  `write(STDOUT_FILENO)` pump.
- `readpass.c::read_passphrase` - prompts routed to `/dev/tty` or
  `STDERR_FILENO`, never `STDOUT_FILENO`.
- `sshconnect2.c::userauth_passwd`, `sshlogin.c::sys_auth_password`
  - password / passphrase prompt entry points.

oc-rsync source:

- `crates/rsync_io/src/ssh/builder.rs:300-340` `SshCommand::spawn`.
- `crates/rsync_io/src/ssh/builder.rs:381-383` `-oBatchMode=yes`
  injection.
- `crates/rsync_io/src/ssh/connection.rs:30-39` `SshConnection`.
- `crates/rsync_io/src/ssh/connection.rs:96-102` `close_stdin`
  (current half-close path).
- `crates/rsync_io/src/ssh/connection.rs:178-217`
  `SshConnection::split`.
- `crates/rsync_io/src/ssh/connection.rs:246-322` `ConnectWatchdog`
  (current connect-timeout path).
- `crates/rsync_io/src/ssh/aux_channel.rs:330-365`
  `configure_stderr_channel` - the existing fd-2 socketpair pattern
  that the wire variant would mirror.
- `crates/rsync_io/src/ssh/mod.rs:57-75` - io_uring boundary note.

Companion audits:

- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - two-socketpair
  variant.
- `docs/audits/ssh-socketpair-claim-verification.md` (#1902) - source
  verification of the wire primitive.
- `docs/audits/splice-ssh-stdio.md` (#1860) - splice/vmsplice plan
  that requires pipes on the wire.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - io_uring on pipe FDs.
- `docs/audits/async-ssh-transport.md` (#2068) - async transport
  refactor that subsumes the unified-FD discussion.

External references:

- `man 2 socketpair`, `man 2 pipe`, `man 7 pipe`, `man 7 unix`,
  `man 2 shutdown`, `man 2 dup2`, `man 2 splice`.
