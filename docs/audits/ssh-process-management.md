# SSH process management correctness audit

Tracking issue: oc-rsync (new). Read-only audit. Recommendations at the
bottom.

Last verified: 2026-05-05

## Summary

This audit verifies the correctness of SSH subprocess management for the
OpenSSH-child transport in `crates/rsync_io/src/ssh/` and its driver in
`crates/core/src/client/remote/`. The four concerns of interest are:

1. Child reaping (no zombies)
2. Exit-code propagation
3. Stderr drain (no deadlock from a full pipe)
4. Signal forwarding to the SSH child

The verdict is **largely PASS with two GAPs**. Child reaping, exit-code
mapping, and stderr drain are all correctly implemented and protected by
regression tests. The two gaps are (a) signal forwarding from the parent's
SIGINT/SIGTERM handlers to the SSH child (today this is implicit through
the controlling-terminal process group; there is no explicit `kill()` of
the child on signal), and (b) `Child::wait()` on the receiver side has no
upper bound (a wedged child blocks the Drop path forever, even after
`Child::kill()`). Both are documented further below.

The embedded-russh transport at `crates/rsync_io/src/ssh/embedded/` has no
`Child` handle (russh runs in-process), so the four concerns reduce to
"does the russh session shut down cleanly?" Treated separately at the
end.

## Architecture

```
+---------------------------------------------------------------+
|                  parent rsync (oc-rsync)                       |
|                                                                |
|   +-----------------+        +-------------------------+       |
|   | SshCommand      | -spawn | std::process::Child     |       |
|   | (builder.rs:307)|------->| stdin/stdout/stderr FDs |       |
|   +-----------------+        +-------------------------+       |
|            |                            |                      |
|            v                            v                      |
|   +-----------------------------------------------+            |
|   | SshConnection (connection.rs:30-209)           |            |
|   |   - Arc<Mutex<Option<Child>>>                  |            |
|   |   - SshReader (stdout) / SshWriter (stdin)     |            |
|   |   - BoxedStderrChannel (drain thread)          |            |
|   |   - ConnectWatchdog (kill-on-timeout thread)   |            |
|   +-----------------------------------------------+            |
|            |          split()                                  |
|            v                                                   |
|   +---------+   +---------+   +-----------------+              |
|   | SshReader|   |SshWriter|   |SshChildHandle  |              |
|   |(stdout)  |   |(stdin)  |   | + drain        |              |
|   |          |   |         |   | + watchdog     |              |
|   +---------+   +---------+   +-----------------+              |
|                                                                |
|   driver: crates/core/src/client/remote/ssh_transfer.rs        |
|     - run_server_over_ssh_connection (line 545)                |
|     - map_child_exit_status (line 487)                         |
|                                                                |
+----------|---------|---------|---------------------------------+
           v         v         v
       +-------+ +-------+ +-------+
       | stdin | | stdout| | stderr|         pipe pair / socketpair
       +-------+ +-------+ +-------+
           |         |         |
+----------v---------v---------v---------------------------------+
|                  SSH subprocess (system /usr/bin/ssh)          |
|                                                                |
|   ssh -oBatchMode=yes -oServerAliveInterval=20                 |
|       -oServerAliveCountMax=3 -oConnectTimeout=N <user>@<host> |
|       <remote-rsync-cmd> ...                                   |
+----------------------------------------------------------------+
                       | encrypted SSH channel
                       v
+----------------------------------------------------------------+
|                  remote rsync (any 3.x)                        |
+----------------------------------------------------------------+
```

Sites that own a `std::process::Child` (production code, transport
layer):

- `crates/rsync_io/src/ssh/builder.rs:336` - `Command::spawn()` for the
  SSH subprocess.
- `crates/rsync_io/src/ssh/connection.rs:34` - `SshConnection.child:
  Arc<Mutex<Option<Child>>>`.
- `crates/rsync_io/src/ssh/connection.rs:388` - `SshChildHandle.child:
  Child` (post-split).
- `crates/core/src/client/module_list/connect/program.rs:49` -
  `Command::spawn()` for `RSYNC_CONNECT_PROG`. Out of scope for SSH but
  shares the same Child-reaping concerns; called out in findings.

There is no `crates/transport/` crate; the OpenSSH-child path lives
entirely in `crates/rsync_io/src/ssh/` and is driven from
`crates/core/src/client/remote/{ssh_transfer.rs, remote_to_remote.rs}`.

## Concern 1 - Child reaping (no zombies)

The `SshConnection` and `SshChildHandle` types are the only owners of
the `Child` for SSH spawns. Both have `Drop` impls that:

1. Try `Child::try_wait()` (non-blocking).
2. If still running, `Child::kill()`.
3. `Child::wait()` to reap.

Citations:

- `SshConnection::Drop` at `crates/rsync_io/src/ssh/connection.rs:544-567`.
  Closes stdin first (line 550), then `try_wait` (554), `kill` (555),
  `wait` (557), then surfaces stderr on non-zero exit (562-564).
- `SshChildHandle::Drop` at `crates/rsync_io/src/ssh/connection.rs:474-496`.
  Drops the connect watchdog first (478), `try_wait` (483), `kill` (484),
  `wait` (486), surfaces stderr on non-zero exit (492-494).

Both Drop impls are tested:

- `drop_child_handle_reaps_process` at `crates/rsync_io/src/ssh/tests.rs:640-650`
  verifies that an early-exiting child is reaped without an explicit
  `wait()` call.
- `drop_child_handle_kills_running_process` at `crates/rsync_io/src/ssh/tests.rs:654-667`
  spawns a `sleep 60` child, drops the handle, and asserts the test
  completes well before 60 seconds (proving `Child::kill()` was issued).
- `child_handle_drop_surfaces_stderr_on_nonzero_exit` at `crates/rsync_io/src/ssh/tests.rs:1313-1335`
  covers the abnormal-exit path through Drop.
- `connection_drop_surfaces_stderr_on_nonzero_exit` at `crates/rsync_io/src/ssh/tests.rs:1337-1355`
  covers the same path on `SshConnection` (no split).

The `connection.split()` path correctly transfers ownership of the
`Child`, the stderr drain, and the connect watchdog from `SshConnection`
to `SshChildHandle` (`connection.rs:178-208`), so there is exactly one
Drop guard at all times. The shared `Arc<Mutex<Option<Child>>>` is moved
into the handle on split, so `SshConnection::Drop` becomes a no-op for
the child after that point (the `*guard` is `None`; lines 552-565).

The `ConnectProgramStream` analogous type at
`crates/core/src/client/module_list/connect/program.rs:227-232` also
has `Drop` that does `kill` then `wait`, with no try_wait probe; this
path always kills even successful processes. Acceptable for daemon
connect-program, since the program's lifetime equals the daemon-stream
lifetime.

**Verdict: PASS.** No bypass site identified.

## Concern 2 - Exit-code propagation

The mapper `map_child_exit_status` is at
`crates/core/src/client/remote/ssh_transfer.rs:487-506`. It mirrors
upstream `main.c:wait_process_with_flush()` and produces:

| Status | Maps to | Citation |
|--------|---------|----------|
| `success()` | `ExitCode::Ok` | line 488-490 |
| signal death (Unix `signal().is_some()`) | `ExitCode::CommandKilled` | line 492-498 |
| exit 127 | `ExitCode::CommandNotFound` | line 501 |
| exit 255 | `ExitCode::CommandFailed` | line 502 |
| exit 1..255 (rsync codes) | `ExitCode::from_i32(code)` | line 503 |
| unknown / fall-through | `ExitCode::PartialTransfer` | line 503 |
| `code() == None` (Windows / unusual) | `ExitCode::WaitChild` | line 504 |

Tests at `crates/core/src/client/remote/ssh_transfer.rs:772-839`:

- `maps_success_to_ok` (line 787-791) - exit 0.
- `maps_exit_127_to_command_not_found` (line 794-798) - exit 127.
- `maps_exit_255_to_command_failed` (line 801-805) - exit 255.
- `maps_rsync_exit_code_23_to_partial_transfer` (line 808-812) - exit 23
  (rsync `RERR_PARTIAL`).
- `maps_rsync_exit_code_24_to_vanished` (line 815-819) - exit 24
  (rsync `RERR_VANISHED`).
- `maps_unknown_exit_code_to_partial_transfer` (line 822-826) - exit 42
  falls back to `PartialTransfer`.
- `maps_signal_killed_to_command_killed` (line 829-838) - SIGKILL.

The "worst exit code wins" rule is implemented at
`crates/core/src/client/remote/ssh_transfer.rs:607-639`:

- If transfer succeeded but child failed, return the child's exit code
  (line 610-619).
- If transfer failed and the child's exit code is higher than the
  transfer's mapped code, return the child's (line 624-631).
- Otherwise return the transfer's exit code (line 632-636).

The same MAX rule is also applied to remote-to-remote proxy transfers
at `crates/core/src/client/remote/remote_to_remote.rs:295-332`:
collects both source and destination child exits, picks the higher of
the two via `as_i32()` comparison (line 311-315), then surfaces the
combined stderr.

Integration tests at `crates/core/tests/ssh_transfer.rs`:

- `ssh_command_not_found_exit_code` (line 297-330) - non-existent
  remote shell yields `CommandNotFound` / `CommandRun` /
  `StartClient` / `Ipc`.
- `ssh_connection_failure_exit_code` (line 339-380) - port 1 yields
  `CommandFailed` / `SocketIo` / `Ipc` / `StartClient`.
- `ssh_stderr_visible_on_connection_failure` (line 448-onwards) - fake
  ssh exits 255 with a stderr message; verifies stderr is surfaced in
  the error message.

**Verdict: PASS.** Mapping matches upstream semantics; tests cover
every documented case including signal death.

## Concern 3 - Stderr drain (no deadlock from full pipe)

The drain thread is started at `SshCommand::spawn()` time, before any
caller code can interact with the connection. Citations:

- `crates/rsync_io/src/ssh/builder.rs:334` - `configure_stderr_channel`
  installs a `socketpair(2)`-backed stderr (Unix) or falls back to
  `Stdio::piped()` (Windows / fd-exhaustion).
- `crates/rsync_io/src/ssh/builder.rs:353` - `build_stderr_channel`
  spawns the appropriate drain thread.
- `crates/rsync_io/src/ssh/aux_channel.rs:104-117` -
  `PipeStderrChannel::spawn` starts a `ssh-stderr-drain-pipe` thread
  unconditionally.
- `crates/rsync_io/src/ssh/aux_channel.rs:159-172` -
  `SocketpairStderrChannel::spawn` starts a `ssh-stderr-drain-socketpair`
  thread unconditionally.

(a) **Drain thread starts unconditionally.** Yes - both `spawn` impls
panic with `expect("failed to spawn ...")` on thread-spawn failure
(builder.rs lines 110, 165), so the drain is either running or the
process aborts. There is no opt-out.

(b) **Drained bytes are forwarded to user output.** Yes -
`drain_loop` at `aux_channel.rs:208-228` reads via `read_until(b'\n')`
and immediately `eprint!`s each line (line 220) so the user sees SSH
diagnostics in real time. The bytes are also captured into a bounded
buffer (line 222) for later retrieval via
`SshConnection::stderr_output` (`connection.rs:90-94`) or
`SshChildHandle::stderr_output` (`connection.rs:438-442`). Capture is
bounded to 64 KiB by `STDERR_BUFFER_CAP` and `append_bounded`
(`aux_channel.rs:39, 232-242`).

(c) **Thread joins or detaches cleanly on Drop.** Yes - both impls have
`Drop` that calls `self.join()` (`aux_channel.rs:132-136, 188-193`),
which blocks until the drain thread exits at EOF. `join()` is idempotent
(line 125-130, 181-186): the `Option<JoinHandle>` is taken on first
call. The trait method `join_and_surface_on_error`
(`aux_channel.rs:74-89`) is invoked from `SshConnection::Drop` and
`SshChildHandle::Drop` on non-zero exits to print captured stderr if
the wait_with_stderr happy path was bypassed.

(d) **Tests exist for the deadlock fix.**

- `stderr_drain_handles_large_output_without_deadlock`
  (`crates/rsync_io/src/ssh/tests.rs:1267-1289`) - writes ~128 KB of
  stderr (2 x pipe buffer) and asserts the child exits without hanging.
- `stderr_deadlock_regression_large_stderr_does_not_block_stdout`
  (`crates/rsync_io/src/ssh/tests.rs:1357-1418`) - writes ~640 KB of
  stderr interleaved with a stdout sentinel. Reads stdout on a
  background thread with a 5-second deadline; if the drain is broken
  the child blocks and the recv times out.
- `stderr_drain_forwards_error_output_after_split`
  (`crates/rsync_io/src/ssh/tests.rs:1239-1264`) - verifies the drain
  works after `split()`.
- `stderr_drain_joins_on_drop`
  (`crates/rsync_io/src/ssh/tests.rs:1293-1311`) - verifies join on
  Drop without explicit wait.
- `stderr_drain_with_no_stderr_output`
  (`crates/rsync_io/src/ssh/tests.rs:1422-1436`) - clean EOF case.
- `socketpair_channel_collects_stderr_data` and the trait-level tests
  in `aux_channel.rs:347-518` cover socketpair vs pipe parity, non-UTF-8
  bytes, bounded buffering, idempotent join, and concurrent
  `collected()` access.

**Verdict: PASS.** The drain is load-bearing, unconditional, and
covered by regression tests - including the two specific deadlock
scenarios the fix was designed for.

## Concern 4 - Signal forwarding

The signal-handling story is more nuanced. The relevant code lives in
`crates/core/src/signal/`:

- `crates/core/src/signal/mod.rs:79-94` - global atomic flags
  `SHUTDOWN_REQUESTED`, `ABORT_REQUESTED`, and a `SHUTDOWN_REASON_CODE`
  byte.
- `crates/core/src/signal/unix.rs:122-172` - `extern "C"` handlers for
  SIGINT, SIGTERM, SIGHUP, and SIGPIPE. Each handler sets the
  corresponding atomic flag and returns. None of them call any kill or
  shutdown helper on the SSH child.
- `crates/core/src/signal/cleanup.rs` - `CleanupManager` tracks
  temp-file paths only. There is no Child-tracking registry.

(4a) **Does the signal handler kill the SSH child?** No. The handlers
only set atomic flags. There is no explicit propagation path from
SIGINT to `SshChildHandle::child.kill()`.

(4b) **Does the SSH child receive the signal anyway?** On Unix, yes,
implicitly: when the user types Ctrl-C in the controlling terminal,
the kernel delivers SIGINT to every process in the foreground process
group, including the SSH child. The SSH child then exits, the parent
unblocks from any pending pipe read with EOF or `EPIPE`, control
returns to the rsync transfer loop, the loop returns an error, the
stack unwinds, and `SshConnection`/`SshChildHandle` Drop reaps the
zombie. This is the same behaviour as upstream rsync.

(4c) **What if the parent receives SIGTERM via `kill -TERM <pid>`
without a controlling terminal?** SIGTERM is delivered only to the
parent. The SSH child does not see it. The atomic flag is set, but the
parent must reach a checkpoint that observes `is_shutdown_requested()`
to act on it. The transfer loop currently does not consult these flags
on every read/write iteration; the SSH child therefore continues
running until the parent unwinds for some other reason or until Drop is
invoked. Drop will then `kill()` the SSH child via the standard path
(`connection.rs:484, 555`), so no zombie is left, but the latency
between the parent's SIGTERM and the SSH child's death equals the time
it takes the parent to unwind to a Drop scope.

(4d) **Tests.** `crates/core/tests/signal_integration.rs` and
`crates/core/tests/sigint_temp_cleanup.rs` exist for the atomic flag
mechanism and temp-file cleanup, but there is no test that asserts
"SIGTERM to the parent kills the SSH child within X milliseconds when
no controlling terminal is involved."

**Verdict: GAP.** The implicit terminal-process-group behaviour is
correct in interactive use, but there is no explicit forwarding path
for the headless case. Recommendation: register an SSH-child-kill
callback with `CleanupManager`, or have the SSH transport poll
`is_abort_requested()` between reads/writes and `Child::kill()` the
child when set. Track as a separate task; the current behaviour does
not leave zombies, only delays headless shutdown.

## Concern 5 - Connection-establishment timeout

`ConnectWatchdog` at `crates/rsync_io/src/ssh/connection.rs:257-378`
implements a condvar-driven thread that fires after a configurable
duration, sets a `fired` atomic, and calls `Child::kill()` on the shared
`Arc<Mutex<Option<Child>>>` so that any pending `read`/`write` on the
child's pipes unblocks (lines 305-312).

Plumbing: `--contimeout` flows from `ClientConfig::connect_timeout()`
through `build_ssh_connection` at
`crates/core/src/client/remote/ssh_transfer.rs:289-291` to
`SshCommand::set_connect_timeout`, which arms the watchdog at
`SshConnection::new` (`connection.rs:60-61`). The same duration is
also injected as `-oConnectTimeout=N` in the SSH argv
(`builder.rs:406-410`) so the SSH client itself enforces the TCP
timeout.

Tests:

- `connect_watchdog_fires_on_timeout` at
  `crates/rsync_io/src/ssh/tests.rs:1819-1870` - 200 ms timeout on a
  `sleep 60` child, asserts `cancel_connect_watchdog` returns
  `ErrorKind::TimedOut`.
- `connect_watchdog_cancelled_before_timeout` at
  `crates/rsync_io/src/ssh/tests.rs:1873-1904` - cancel before fire is
  successful and idempotent.
- `connect_watchdog_not_armed_when_timeout_is_none` at
  `crates/rsync_io/src/ssh/tests.rs:1907-1933` - cancel is a no-op when
  no timeout is configured.
- `connect_watchdog_transferred_to_child_handle_on_split` at
  `crates/rsync_io/src/ssh/tests.rs:1935-1957` - verifies the watchdog
  follows ownership through `connection.split()`.
- `connect_watchdog_fires_and_child_handle_reports_timeout` at
  `crates/rsync_io/src/ssh/tests.rs:1959-onwards` - same kill-on-fire
  behaviour through the split handle.

After cancellation, the watchdog thread is joined synchronously
(`connection.rs:347-349`), and the connection consults `has_fired()`
on every `read()` (`connection.rs:504-514`) so a stale watchdog cannot
let a frozen connection slip through.

**Verdict: PASS.** Timeout fires on dead remotes, kills the child,
returns `ErrorKind::TimedOut`, and is covered by tests at both
`SshConnection` and `SshChildHandle` levels.

## Concern 6 - Stdin/stdout handling vs stderr drain

Confirming that rsync wire data is uninterrupted by the stderr drain:

- The drain thread reads only from `ChildStderr` or the parent half of
  the stderr socketpair (`aux_channel.rs:208`). It never touches
  `ChildStdin` or `ChildStdout`.
- `SshReader::read` (`connection.rs:217-221`) and `SshWriter::write`
  (`connection.rs:229-237`) operate on independent FDs, so the drain
  cannot starve or interleave with rsync wire data.
- The shared `Arc<Mutex<Option<Child>>>` is locked only by the connect
  watchdog (to issue `Child::kill()`) and by `wait`/`try_wait`/`split`
  on the main thread. The drain thread does not take the mutex. It
  cannot block stdio reads.

Read/write blocking concerns are tracked in the existing audit at
`docs/audits/ssh-transport-timeout-coverage.md`. That audit documents
the gap that `--timeout` is not plumbed to `SshReader::read` /
`SshWriter::write` post-handshake. That is **not** in scope for this
process-management audit but is referenced for completeness.

**Verdict: PASS** for stdio independence from the drain. Steady-state
read/write deadlines are tracked elsewhere.

## Concern 7 - `Child::wait()` deadline

The Drop impls call `Child::kill()` then `Child::wait()`
(`connection.rs:484-486, 555-557`). On a well-behaved system this is
fine - SIGKILL is uncatchable and `wait()` returns within milliseconds.
On a pathological system where the SSH child is stuck in
uninterruptible sleep (D-state on Linux, e.g., a hung NFS mount), the
final `wait()` blocks indefinitely. Same concern applies to
`SshConnection::wait` (`connection.rs:111`) and `wait_with_stderr`
(`connection.rs:136`).

This is the same gap noted in
`docs/audits/ssh-transport-timeout-coverage.md` items #5, #6, #7, #8.
Re-stated here for completeness.

**Verdict: GAP.** Low severity in practice; no fix recommended unless
operators report stuck transfers in environments with unreliable
filesystem I/O.

## Concern 8 - Embedded russh transport

`crates/rsync_io/src/ssh/embedded/` runs russh in-process. There is no
`std::process::Child`, no zombie risk, no separate stderr stream from a
subprocess (russh surfaces server stderr via `ChannelMsg::ExtendedData`,
extended-data type 1; today our handler at
`crates/rsync_io/src/ssh/embedded/connect.rs` does not split it from
stdout). Process-management concerns reduce to "does the russh session
shut down cleanly?" which is governed by `Drop` on the channel-bridge
task (handled by tokio runtime shutdown) and the connect-timeout wrapper
documented in the timeout-coverage audit.

**Verdict: N/A** for this audit. Tracked separately.

## Findings table

| # | Concern | Status | Evidence | Recommendation |
|---|---------|--------|----------|----------------|
| 1 | Child reaping (no zombies) | PASS | `connection.rs:474-496, 544-567`; tests `tests.rs:640, 654, 1313, 1337` | None |
| 2 | Exit-code mapping (0/127/255/signal) | PASS | `ssh_transfer.rs:487-506`; tests `ssh_transfer.rs:787-838` | None |
| 3 | Worst-exit-code-wins rule | PASS | `ssh_transfer.rs:607-639`, `remote_to_remote.rs:295-332` | None |
| 4 | Stderr drain unconditional | PASS | `builder.rs:334-353`, `aux_channel.rs:104-117, 159-172`; tests `tests.rs:1239-1418` | None |
| 5 | Stderr forwarded to user output | PASS | `aux_channel.rs:208-228` (line 220 `eprint!`) | None |
| 6 | Stderr drain joins on Drop | PASS | `aux_channel.rs:132-136, 188-193`, `connection.rs:492, 562` | None |
| 7 | Connect timeout fires on dead remotes | PASS | `connection.rs:257-378`; tests `tests.rs:1819, 1874, 1908, 1937` | None |
| 8 | Stdio not blocked by drain | PASS | drain reads only stderr FD; no shared lock | None |
| 9 | Signal forwarding to SSH child | GAP | no explicit kill-on-signal; only implicit via foreground pgrp | Add SIGINT/SIGTERM checkpoint that calls `Child::kill()` when `is_abort_requested()` is set, or register an SSH-child kill callback with `CleanupManager`. Add a test that asserts a SIGTERM to a headless parent terminates the SSH child within a bounded interval. |
| 10 | `Child::wait()` has no deadline | GAP | `connection.rs:486, 557`; cross-referenced from `docs/audits/ssh-transport-timeout-coverage.md` items #5-#8 | Bound `wait()` with a polling try_wait loop and escalate to a second SIGKILL after a deadline. Low priority; only matters for D-state children. |

## Recommendations

The audit is **PASS for the four specific concerns the fixes targeted**:
zombies are reaped, exit codes are mapped to upstream values with the
worst-wins rule, the stderr drain prevents pipe-full deadlocks, and
those properties are covered by regression tests that would fail loudly
if the load-bearing code is removed.

Two non-blocking gaps remain:

1. **Signal forwarding (Gap #9, medium severity).** The implicit
   foreground-pgrp behaviour covers the interactive Ctrl-C case. The
   headless `kill -TERM <pid>` case relies on the parent unwinding to a
   Drop scope before the SSH child is killed. Recommend adding an
   abort-checkpoint between reads/writes in
   `crates/core/src/client/remote/ssh_transfer.rs` that polls
   `signal::is_abort_requested()` and calls `child_handle.kill()` when
   set. Track as task: "ssh: explicit signal-forwarding to SSH child on
   parent SIGINT/SIGTERM in headless mode".

2. **`Child::wait()` deadline (Gap #10, low severity).** Already
   tracked in `docs/audits/ssh-transport-timeout-coverage.md`. No
   additional action needed here; that audit's punch-list item 6
   covers it.

No code changes recommended within scope of this audit; both
recommendations are tracking-issue suggestions for a follow-up worker.

## Files audited

- `crates/rsync_io/src/ssh/mod.rs`
- `crates/rsync_io/src/ssh/builder.rs`
- `crates/rsync_io/src/ssh/connection.rs`
- `crates/rsync_io/src/ssh/aux_channel.rs`
- `crates/rsync_io/src/ssh/tests.rs`
- `crates/core/src/client/remote/ssh_transfer.rs`
- `crates/core/src/client/remote/remote_to_remote.rs`
- `crates/core/src/client/module_list/connect/program.rs`
- `crates/core/src/signal/mod.rs`
- `crates/core/src/signal/unix.rs`
- `crates/core/src/signal/cleanup.rs`
- `crates/core/tests/ssh_transfer.rs`
