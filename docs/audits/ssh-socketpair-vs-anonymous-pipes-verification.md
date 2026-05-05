# SSH socketpair vs anonymous pipes: verification of prior audit

Tracker: #1902 (verify SSH socketpair vs anonymous-pipe wire claim against
`rsync_io` source). Branch: `docs/ssh-socketpair-verification-1902`.
No code changes - documentation only.

Last verified: 2026-05-05.

Companion documents:

- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - the prior audit
  this report verifies. The claim catalogue, methodology, and
  per-claim verdict table below all reference that document.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - io_uring on pipe FDs.
- `docs/audits/splice-ssh-stdio.md` (#1860) - splice/vmsplice for SSH
  stdio.
- `docs/audits/async-ssh-transport.md` (#1593) - async transport
  evaluation.

## 1. Summary

The prior audit (`docs/audits/ssh-socketpair-vs-pipes.md`) concluded
that oc-rsync's SSH wire uses two anonymous `pipe(2)` pairs while
upstream rsync 3.4.1 uses two `socketpair(AF_UNIX, SOCK_STREAM, 0)`
pairs via `util1.c::fd_pair`, and recommended keeping pipes on the
oc-rsync wire. Task #1902 asks whether that claim is still accurate
against the *current* state of `crates/rsync_io/src/ssh/` after the
post-audit churn (PRs #3582 ssh comment cleanup, #3637 SSH/rsync
compression warning, #3658 multiplex frontend cleanup).

**Conclusion:** every load-bearing claim in the prior audit is still
accurate. The substantive behaviour - pipes on the wire, socketpair on
stderr (Unix), pipe stderr fallback (Windows and on Unix
file-descriptor exhaustion), blocking-IO with a connect watchdog,
`Drop`-time child reaping, no `socketpair` anywhere in the wire path -
is unchanged. Two of the file:LINE citations in the prior audit have
drifted because intervening commits added or moved lines, and a small
number of lines on the SSH compression warning surface (#3637) are
informational additions that the prior audit could not have known
about. The verdicts on #1687 ("do not prototype") and on #1902
("verified: oc-rsync intentionally diverges") therefore stand.

## 2. Methodology

The verification approach was:

1. Re-read `docs/audits/ssh-socketpair-vs-pipes.md` end to end and
   extract every *testable* claim - a claim is testable when it asserts
   either the existence or the file:LINE location of a specific code
   construct (struct, function, call, comment) inside oc-rsync.
2. Read each cited file in `crates/rsync_io/src/ssh/` and
   `crates/core/src/client/remote/` at the current `master` HEAD and
   verify the claim against the live source. Where the claim is a
   numeric file:LINE pair, the line number was checked against the
   live file rather than treating the prior audit as authoritative.
3. Run a workspace-wide `grep -rn 'socketpair\|UnixStream::pair'
   crates/` to verify that no socketpair use exists outside the audited
   surfaces. Any stray match would invalidate the "no socketpair on
   the wire" claim.
4. For every claim that has *changed* (verdict CHANGED), find the
   commit that changed it via `git log --oneline -- <file>` and cite
   it. For claims whose location merely drifted (verdict CONFIRMED but
   the line numbers shifted), record the drift in Section 4.
5. Cross-check the prior audit's open-question status against the
   current tracker state (#1687 still pending, #1689 still completed)
   and re-evaluate whether the prior recommendation needs revision.

Verification material is the live worktree at
`crates/rsync_io/src/ssh/` plus `crates/core/src/client/remote/`. The
prior audit's upstream-rsync claims (Section 2 of that document) were
not re-verified; they reference `target/interop/upstream-src/rsync-3.4.1/`
which is unchanged by oc-rsync work.

## 3. Claim inventory

The prior audit makes thirteen testable claims about oc-rsync source
code. They are reproduced verbatim (paraphrased) below, paired with the
file:LINE citation the audit provides and the answer from current code.

### 3.1 Wire stdin/stdout are anonymous pipes

> "oc-rsync currently spawns SSH with two anonymous `pipe(2)` pairs -
> `Command::stdin(Stdio::piped())` and `Command::stdout(Stdio::piped())` -
> for the wire."

- Prior citation: `crates/rsync_io/src/ssh/builder.rs:300-301`.
- Current code: `crates/rsync_io/src/ssh/builder.rs:322-323` -

  ```rust
  command.stdin(Stdio::piped());
  command.stdout(Stdio::piped());
  ```

- Verdict: **CONFIRMED** with line drift. The two `Stdio::piped()`
  calls remain the only place the SSH wire is configured. They moved
  by 22 lines because PR #3637 (`feat(ssh): warn when SSH and rsync
  compression both enabled`) added the `has_ssh_compression()` method
  and helpers above `spawn` in `builder.rs`.

### 3.2 Stderr drained on a separate background thread

> "A background thread is spawned at construction time to drain it via
> the configured `StderrAuxChannel`. This prevents deadlocks when the
> remote process writes more than the OS pipe buffer capacity to
> stderr."

- Prior citation: `crates/rsync_io/src/ssh/aux_channel.rs:138-193`
  (`SocketpairStderrChannel`) and the `PipeStderrChannel` definition
  above it.
- Current code: `crates/rsync_io/src/ssh/aux_channel.rs:97-118`
  (`PipeStderrChannel::spawn` builds a `thread::Builder` named
  `ssh-stderr-drain-pipe`); `crates/rsync_io/src/ssh/aux_channel.rs:159-172`
  (`SocketpairStderrChannel::spawn` builds a `thread::Builder` named
  `ssh-stderr-drain-socketpair`); both invoke
  `drain_loop(...)` at `aux_channel.rs:208-228`.
- Verdict: **CONFIRMED**. The thread topology is unchanged. Both
  variants spawn at construction time. The trait-based factory at
  `aux_channel.rs:298-308` (`build_stderr_channel`) selects between
  them. The auxiliary stderr work the prior audit credits to #1689
  remains intact.

### 3.3 Child reaping is on Drop via SshChildHandle

> "The `SshChildHandle` (returned by `SshConnection::split()`) reaps
> the SSH child in `Drop` so that callers cannot leak a zombie."

- Prior citation: implied by the prior audit's references to
  `connection.rs:178-208` (`split`) and the role of `SshChildHandle`
  in the connection state struct at `connection.rs:30-39`.
- Current code: `crates/rsync_io/src/ssh/connection.rs:474-496`
  (`impl Drop for SshChildHandle`):

  ```rust
  impl Drop for SshChildHandle {
      fn drop(&mut self) {
          drop(self.connect_watchdog.take());
          if let Ok(None) = self.child.try_wait() {
              let _ = self.child.kill();
          }
          let status = self.child.wait();
          if let Some(ref mut drain) = self.stderr_drain {
              drain.join_and_surface_on_error(&status);
          }
      }
  }
  ```

  The companion `impl Drop for SshConnection` at
  `connection.rs:544-567` performs the same reaping for
  pre-`split()` lifetimes.
- Verdict: **CONFIRMED**. Reaping is on `Drop`. The watchdog is
  dropped first to ensure its background thread exits before the
  child handle is touched. The `Ok(None)` branch handles the case
  where the child has not yet exited (kill, then wait). Both `Drop`
  impls also surface stderr on non-zero exit via
  `join_and_surface_on_error` (`aux_channel.rs:74-89`).

### 3.4 No socketpair is used anywhere in the SSH wire

> "There is no `socketpair`, `UnixStream::pair`, or manual `dup2` of a
> non-pipe FD onto the wire."

- Prior citation: the absence of any matching grep result in
  `crates/rsync_io/src/ssh/`.
- Current code: `grep -rn 'socketpair\|UnixStream::pair' crates/`
  produces matches only in:
  - `crates/rsync_io/src/ssh/aux_channel.rs` (stderr socketpair, #1689) -
    expected.
  - `crates/core/src/version/report/config.rs:146-160`
    (`socketpair_available()` runtime probe for the
    `--version` capability report) - unrelated to the wire.
  - `crates/core/src/version/tests/report.rs` and
    `crates/core/src/version/report/renderer.rs` - test/render code
    for the same capability.
  - `crates/fast_io/tests/io_uring_shared_ring.rs` and
    `crates/fast_io/tests/splice_integration.rs` - test fixtures
    that synthesise an FD that looks like a socket; not the SSH
    wire.
  No occurrence appears in any of `crates/rsync_io/src/ssh/builder.rs`,
  `crates/rsync_io/src/ssh/connection.rs`,
  `crates/core/src/client/remote/ssh_transfer.rs`, or
  `crates/core/src/client/remote/embedded_ssh_transfer.rs` (the four
  files that *would* matter if the wire were ever socketpair-backed).
- Verdict: **CONFIRMED**. The only `socketpair`/`UnixStream::pair`
  uses on the SSH transport are the stderr aux channel
  (`aux_channel.rs:265`) and its tests; neither replaces the wire.

### 3.5 SshConnection state shape

> "The wire is two unidirectional handles: `ChildStdin` (write side
> of the stdin pipe) and `ChildStdout` (read side of the stdout pipe).
> They are never combined into a single FD."

- Prior citation: `crates/rsync_io/src/ssh/connection.rs:30-39`.
- Current code: `crates/rsync_io/src/ssh/connection.rs:30-39` -

  ```rust
  pub struct SshConnection {
      child: Arc<Mutex<Option<Child>>>,
      stdin: Option<ChildStdin>,
      stdout: Option<ChildStdout>,
      stderr_drain: Option<BoxedStderrChannel>,
      connect_watchdog: Option<ConnectWatchdog>,
  }
  ```

- Verdict: **CONFIRMED** without drift. The struct shape is
  byte-identical to the prior audit's quote.

### 3.6 Read/Write delegate to ChildStdin/ChildStdout

> "`SshReader` -> `ChildStdout::read` -> `read(2)` on the pipe FD;
> `SshWriter` -> `ChildStdin::write` and `flush` -> `write(2)` on the
> pipe FD."

- Prior citation: `connection.rs:217-221` and `connection.rs:229-237`.
- Current code: `connection.rs:217-221` for `impl Read for SshReader`
  (delegates to `ChildStdout::read`); `connection.rs:229-237` for
  `impl Write for SshWriter` (delegates to `ChildStdin::write` /
  `ChildStdin::flush`).
- Verdict: **CONFIRMED** without drift. Both impls remain
  byte-identical.

### 3.7 Half-close path is `close_stdin` only

> "`close_stdin` flushes and drops `ChildStdin`. There is no
> `shutdown(SHUT_WR)` call - pipes do not support `shutdown(2)`."

- Prior citation: `connection.rs:96-102`.
- Current code: `connection.rs:96-102` is unchanged:

  ```rust
  pub fn close_stdin(&mut self) -> io::Result<()> {
      if let Some(mut stdin) = self.stdin.take() {
          stdin.flush()?;
      }
      Ok(())
  }
  ```

  `SshWriter::close` at `connection.rs:241-243` is the same pattern
  after `split()`.
- Verdict: **CONFIRMED** without drift.

### 3.8 No `set_nonblocking` calls in the SSH module

> "Both ends are blocking. There is no `set_nonblocking` call anywhere
> in `crates/rsync_io/src/ssh/`."

- Prior citation: implicit; an absence claim covering the whole
  `ssh/` subtree.
- Current code: `grep -rn 'set_nonblocking\b' crates/rsync_io/src/ssh/`
  returns no matches. The wire is blocking.
- Verdict: **CONFIRMED**. The prior audit's Finding 2 ("parent stdio
  is blocking, upstream is non-blocking") is still accurate.

### 3.9 Stderr socketpair installed via `configure_stderr_channel`

> "On success we keep one half on the parent side and hand the other
> half to the child as its stderr fd via `Stdio::from(OwnedFd)`. On
> any failure we fall back to `Stdio::piped()`."

- Prior citation: `aux_channel.rs:263-291`.
- Current code: `aux_channel.rs:263-285` (Unix arm) -

  ```rust
  match UnixStream::pair() {
      Ok((parent, child)) => {
          let child_fd: std::os::fd::OwnedFd = child.into();
          command.stderr(Stdio::from(child_fd));
          ...
          Some(parent)
      }
      Err(error) => {
          command.stderr(Stdio::piped());
          ...
          None
      }
  }
  ```

  and `aux_channel.rs:287-291` (the `cfg(not(unix))` arm).
- Verdict: **CONFIRMED** without drift. The function body and
  fallback path are unchanged.

### 3.10 ConnectWatchdog substitutes for non-blocking I/O

> "`ConnectWatchdog`, a background thread that calls `Child::kill()`
> after a configurable timeout. This exists because the inherited
> pipes are blocking; without a watchdog, a hung SSH client would
> block the parent's first `read` indefinitely."

- Prior citation: `connection.rs:246-322`.
- Current code: `connection.rs:246-378` - struct definition at
  `246-263`, `arm()` at `265-324`, `cancel()` at `336-362`, `Drop`
  at `365-378`. The kill-on-timeout body is at `connection.rs:298-313`:

  ```rust
  if result.timed_out() {
      thread_fired.store(true, Ordering::Release);
      ...
      if let Ok(mut guard) = shared_child.lock() {
          if let Some(ref mut child) = *guard {
              let _ = child.kill();
          }
      }
  }
  ```

- Verdict: **CONFIRMED** with line drift. The ConnectWatchdog
  region grew because the `Drop` impl (`connection.rs:365-378`)
  now correctly sits inside the watchdog block; the prior audit's
  cited range `246-322` ended where the `cancel()` body finished.
  The behaviour is unchanged.

### 3.11 io_uring boundary documentation

> "The SSH data channel is the spawned `ssh` child's inherited stdio:
> a `(stdin, stdout)` pipe pair created by `Command::spawn`, not a
> socket."

- Prior citation: `crates/rsync_io/src/ssh/mod.rs:57-75`.
- Current code: `mod.rs:57-75` is unchanged. The same paragraph
  still records that `fast_io`'s `socket_reader` /
  `socket_writer` paths are unreachable for SSH.
- Verdict: **CONFIRMED** without drift.

### 3.12 Daemon path uses TCP, not pipes or socketpair

> "For daemon-mode connections (`rsync://` URLs) the wire is a TCP
> socket, not a stdio pipe."

- Prior citation: prior audit Section 2.4
  (`clientserver.c:116-148`). The claim's oc-rsync flavour is that
  daemon-mode bypasses the SSH builder entirely.
- Current code: `crates/core/src/client/remote/daemon_transfer/`
  contains the daemon path; it does not import from
  `rsync_io::ssh::`. The TCP socket flow lives outside this audit's
  surface.
- Verdict: **CONFIRMED**. (The prior audit references a hypothetical
  `crates/transport/` crate at `aux_channel.rs:383`; that crate was
  consolidated into `rsync_io` and `core::client::remote` before the
  prior audit landed and is not a substantive issue. See Section 4.3
  below.)

### 3.13 SshChildHandle owns the stderr drain after split

> "When the connection is split, the stderr drain (spawned at
> connection creation time) is transferred to this handle."

- Prior citation: `connection.rs:178-208`.
- Current code: `connection.rs:178-208` (`SshConnection::split`)
  contains the transfer:

  ```rust
  let stderr_drain = self.stderr_drain.take();
  let connect_watchdog = self.connect_watchdog.take();
  Ok((
      SshReader { stdout },
      SshWriter { stdin },
      SshChildHandle { child, stderr_drain, connect_watchdog },
  ))
  ```

- Verdict: **CONFIRMED** without drift.

## 4. Per-claim verdict table

| # | Claim                                              | Original verdict      | Current verdict      | Evidence path:LINE                                       | Status     |
|---|----------------------------------------------------|-----------------------|----------------------|----------------------------------------------------------|------------|
| 1 | Wire uses anonymous pipes for stdin and stdout     | finding 1, divergent  | finding 1, divergent | `crates/rsync_io/src/ssh/builder.rs:322-323`             | CONFIRMED* |
| 2 | Stderr drained on a separate background thread     | section 3.3 / 3.6     | unchanged            | `crates/rsync_io/src/ssh/aux_channel.rs:104-117, 159-172, 208-228` | CONFIRMED  |
| 3 | Child reaping is on Drop via SshChildHandle        | implied               | confirmed            | `crates/rsync_io/src/ssh/connection.rs:474-496`          | CONFIRMED  |
| 4 | No socketpair anywhere on the wire                 | finding 1             | finding 1            | grep result; absence of match in builder/connection      | CONFIRMED  |
| 5 | SshConnection state shape (5 fields)               | section 3.2           | unchanged            | `crates/rsync_io/src/ssh/connection.rs:30-39`            | CONFIRMED  |
| 6 | Read/Write delegate to ChildStdin/ChildStdout      | section 3.2           | unchanged            | `crates/rsync_io/src/ssh/connection.rs:217-237`          | CONFIRMED  |
| 7 | `close_stdin` is the only half-close path          | section 3.4           | unchanged            | `crates/rsync_io/src/ssh/connection.rs:96-102, 241-243`  | CONFIRMED  |
| 8 | No `set_nonblocking` calls in `ssh/`               | finding 2             | unchanged            | grep result (zero matches)                               | CONFIRMED  |
| 9 | Stderr socketpair via `configure_stderr_channel`   | section 3.3           | unchanged            | `crates/rsync_io/src/ssh/aux_channel.rs:263-291`         | CONFIRMED  |
| 10| ConnectWatchdog substitutes for non-blocking I/O   | section 3.6 / find. 2 | unchanged            | `crates/rsync_io/src/ssh/connection.rs:246-378`          | CONFIRMED* |
| 11| io_uring boundary documented in `ssh/mod.rs`       | section 3.5           | unchanged            | `crates/rsync_io/src/ssh/mod.rs:57-75`                   | CONFIRMED  |
| 12| Daemon path uses TCP, not pipes or socketpair      | section 2.4           | unchanged            | `crates/core/src/client/remote/daemon_transfer/`         | CONFIRMED  |
| 13| `SshChildHandle` owns stderr drain after `split()` | section 3.2           | unchanged            | `crates/rsync_io/src/ssh/connection.rs:178-208`          | CONFIRMED  |

\* Line numbers shifted; behaviour unchanged. See Section 4.1 below.

## 4.1 Drift findings

Two of the prior audit's file:LINE citations have drifted. Both are
mechanical drift caused by additions above the cited region; neither
indicates a behavioural change.

### Drift 1: `Stdio::piped()` calls in `builder.rs::spawn`

- Prior citation: `builder.rs:300-301`.
- Current location: `builder.rs:322-323`.
- Cause: PR #3637 (commit `4d951fea4`,
  `feat(ssh): warn when SSH and rsync compression both enabled`)
  added the `has_ssh_compression()` method (and helpers
  `arg_enables_ssh_compression`) to `builder.rs`, increasing the
  file by ~60 lines and pushing the spawn body downward. The
  earlier `set_prefer_aes_gcm` setter region also expanded.
- Impact: none. The call sites are still the only `Stdio::piped()`
  invocations for the wire.

### Drift 2: `ConnectWatchdog` region in `connection.rs`

- Prior citation: `connection.rs:246-322`.
- Current location: `connection.rs:246-378`. The cited range in the
  prior audit ends with the `cancel()` body; the current range
  also covers the `Drop for ConnectWatchdog` impl.
- Cause: the prior audit cited the struct-and-arm-and-cancel
  block; later edits restructured the block so `Drop` and
  `has_fired` now sit inside the contiguous watchdog region.
  No commit added new behaviour - this is a citation-scope
  difference rather than a code change.
- Impact: none.

### Other drift candidates (no drift observed)

- `aux_channel.rs:138-193` (`SocketpairStderrChannel`) is unchanged.
- `aux_channel.rs:263-291` (`configure_stderr_channel` Unix arm and
  the non-Unix arm) is unchanged.
- `connection.rs:30-39`, `connection.rs:96-102`,
  `connection.rs:178-208`, `connection.rs:217-237`,
  `connection.rs:241-243`, `connection.rs:474-496` and
  `connection.rs:544-567` are all at their prior-audit locations.
- `mod.rs:57-75` is at its prior-audit location.

## 4.2 Out-of-scope additions since the prior audit

The following changes touched the SSH module after the prior audit
landed (PR #3525, commit `b3ebe792e`) but do not affect any of the
thirteen claims:

- PR #3582 (`docs(rsync_io): comment cleanup for ssh module`) -
  removed restating comments in `builder.rs`, `connection.rs`, and
  `embedded/`. The prior audit's quoted code blocks are unaffected
  because the cleanup did not touch the wire-setup or stderr-channel
  code; it only adjusted comments above other functions.
- PR #3637 (`feat(ssh): warn when SSH and rsync compression both
  enabled`) - added `SshCommand::has_ssh_compression()` and the
  spawn-time double-compression warning in
  `crates/core/src/client/remote/ssh_transfer.rs`. This is the
  cause of Drift 1 above. It does not interact with the wire
  topology.
- PR #3658 (`docs(rsync_io): rustdoc cleanup for multiplex
  frontend`) - touched the multiplex envelope rustdoc, not the SSH
  transport. No effect on this audit.
- PR #3628 / #3622 (`russh 0.45 -> 0.60.1` plus the auth gating
  fix) - upgraded the embedded-SSH dependency. The embedded path
  has its own audit and is out of scope here; the system-`ssh`
  path is unaffected.

## 4.3 Citation discrepancies in the prior audit

The prior audit's Section 3.7 table lists the daemon-mode wire as

> Daemon `rsync://` wire | `TcpStream` (unaffected by this audit) |
> `crates/transport/`

The `crates/transport/` crate does not exist in the current
workspace; daemon TCP connection setup lives in
`crates/core/src/client/remote/daemon_transfer/`. This is a
citation hygiene issue in the prior audit, not a behavioural drift.
Recording it here so a future maintainer does not chase a
non-existent crate.

The prior audit's Section 3.6 paragraph also references "this audit
informs #1902"; that informational claim does not bear on the
verification.

## 5. Open question revisited: socketpair prototype (#1687)

Tracker #1687 ("Prototype SSH subprocess using socketpair for
bidirectional I/O") was the prior audit's primary disposition: it
recommended closing #1687 as "do not implement". The verification
re-asks whether the recommendation still holds given that #1689
(stderr socketpair) is completed and that the system has a
production-tested example of a socketpair-backed SSH IPC channel.

### 5.1 Has the calculus changed?

The prior audit's no-go recommendation rests on three pillars
(Section 6 of `ssh-socketpair-vs-pipes.md`):

1. **Splice eligibility.** A socketpair-backed wire would force
   `splice(2)` and `vmsplice(2)` to thread an intermediate `pipe(2)`,
   doubling the syscall count and negating the zero-copy benefit
   that #1860 is designed to capture. This pillar is unchanged. No
   intervening commit altered the `splice` design or the wire
   primitive.

2. **No measured backpressure regression.** No oc-rsync issue tracks
   pipe-buffer pressure on the SSH wire. A check of the issue
   tracker between the prior audit and now finds no new report
   blaming the 64 KiB pipe buffer for a stall. This pillar is
   unchanged.

3. **Async-transport refactor subsumes the unified-FD argument.**
   The async-transport audit (`docs/audits/async-ssh-transport.md`,
   #1593) is the right vehicle for any "one poll registration"
   argument. Its conclusion - status quo on Windows, defer Linux
   work to #1859 - is unchanged.

### 5.2 Does #1689's stderr socketpair change anything?

No. The stderr socketpair was added because the stderr channel has
fundamentally different requirements from the wire: it is
line-oriented, low-volume, never spliced, and benefits from a real
socket FD for future event-loop registration. The prior audit
explicitly enumerates this distinction (Section 4.6 "Stderr
separation"). The fact that #1689 succeeded does not generalise to
the wire because the wire's bottleneck dimensions are different
(splice eligibility, bulk throughput, file<->wire zero-copy).

### 5.3 Recommendation

**Reaffirm the prior audit's recommendation.** #1687 should remain
closed as "do not prototype". The verification adds nothing that
would justify reopening it.

If a future change in the splice plan (#1860) adopts a topology that
is socketpair-friendly - for example, the receiver-side
`splice(wire, NULL, file_fd, NULL, ...)` is replaced by a
`recvmsg(2)`-plus-`pwrite(2)` path - then #1687 may be revisited
with a concrete benchmark. Until then, the no-go recommendation
holds.

## 6. Recommendation

1. **Close #1902 as verified.** The thirteen claims in
   `docs/audits/ssh-socketpair-vs-pipes.md` are all confirmed against
   current source. The two file:LINE drifts in Section 4.1 are
   mechanical and do not affect any verdict.
2. **Reaffirm #1687 as "do not prototype".** The three pillars of
   the prior audit's no-go recommendation are intact; #1689's
   completion does not generalise to the wire.
3. **Update the prior audit's `crates/transport/` reference.** A
   one-line follow-up edit to `ssh-socketpair-vs-pipes.md`
   Section 3.7 should change the daemon-wire citation to
   `crates/core/src/client/remote/daemon_transfer/`. This is purely
   citation hygiene; no claim depends on the corrected path. If
   that edit is preferred as a separate PR, the citation correction
   recorded in Section 4.3 above is sufficient documentation.
4. **No code changes.** The verification confirms behaviour the
   prior audit recommended, so nothing needs to change in the SSH
   transport.

## 7. References

oc-rsync source (verified at branch `master`):

- `crates/rsync_io/src/ssh/mod.rs:57-75` - io_uring boundary
  documentation.
- `crates/rsync_io/src/ssh/builder.rs:307-362` - `SshCommand::spawn`.
- `crates/rsync_io/src/ssh/builder.rs:322-323` - `Stdio::piped()`
  for wire stdin/stdout.
- `crates/rsync_io/src/ssh/connection.rs:30-39` - `SshConnection`
  state shape.
- `crates/rsync_io/src/ssh/connection.rs:96-102` - `close_stdin`.
- `crates/rsync_io/src/ssh/connection.rs:178-208` -
  `SshConnection::split`.
- `crates/rsync_io/src/ssh/connection.rs:217-237` - `Read`/`Write`
  impls for `SshReader` / `SshWriter`.
- `crates/rsync_io/src/ssh/connection.rs:241-243` -
  `SshWriter::close`.
- `crates/rsync_io/src/ssh/connection.rs:246-378` -
  `ConnectWatchdog`.
- `crates/rsync_io/src/ssh/connection.rs:474-496` - `Drop` for
  `SshChildHandle` (child reaping).
- `crates/rsync_io/src/ssh/connection.rs:544-567` - `Drop` for
  `SshConnection`.
- `crates/rsync_io/src/ssh/aux_channel.rs:97-117` -
  `PipeStderrChannel::spawn`.
- `crates/rsync_io/src/ssh/aux_channel.rs:138-193` -
  `SocketpairStderrChannel`.
- `crates/rsync_io/src/ssh/aux_channel.rs:208-228` - `drain_loop`.
- `crates/rsync_io/src/ssh/aux_channel.rs:263-291` -
  `configure_stderr_channel`.
- `crates/rsync_io/src/ssh/aux_channel.rs:298-316` -
  `build_stderr_channel`.
- `crates/core/src/client/remote/ssh_transfer.rs:545-640` -
  `run_server_over_ssh_connection` (consumer of `split()`).

Companion audits:

- `docs/audits/ssh-socketpair-vs-pipes.md` (#1938) - the prior
  audit verified here.
- `docs/audits/iouring-pipe-stdio.md` (#1859) - io_uring on pipe
  FDs.
- `docs/audits/splice-ssh-stdio.md` (#1860) - splice/vmsplice for
  SSH stdio.
- `docs/audits/async-ssh-transport.md` (#1593) - async transport
  evaluation.
- `docs/audits/ssh-cipher-compression.md` - SSH cipher and
  compression policy.

External references (unchanged from the prior audit):

- `man 2 socketpair`, `man 2 pipe`, `man 7 pipe`, `man 2 splice`,
  `man 2 vmsplice`, `man 7 unix`.
- Linux io_uring opcodes: `IORING_OP_READ`, `IORING_OP_WRITE`,
  `IORING_OP_RECV`, `IORING_OP_SEND`.
