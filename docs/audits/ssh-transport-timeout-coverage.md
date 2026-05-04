# SSH transport timeout coverage matrix

Tracking issue: oc-rsync (new). See punch-list at the bottom for suggested
sub-issues.

Last verified: 2026-05-01

## Summary

This audit enumerates every potentially blocking I/O site in the two SSH
transport stacks - the OpenSSH-child path that spawns the system `ssh`
binary (`crates/rsync_io/src/ssh/`, the `SshConnection` machinery, and the
`crates/core/src/client/remote/ssh_transfer.rs` driver) and the embedded
russh path (`crates/rsync_io/src/ssh/embedded/`) - and maps each one to
the timeout (or watchdog) that bounds it.

The verdict is mixed. **Connection establishment is well-covered**: both
the OpenSSH-child and embedded russh paths arm a connect-timeout knob that
unblocks pending I/O on expiry. **Steady-state reads/writes after
authentication are not bounded by any explicit timeout** on either path:
the `--timeout=SECONDS` CLI option (upstream `io.c:io_set_select_timeout()`
contract) is plumbed into `ClientConfig::timeout()` but never reaches the
SSH stdio I/O sites or the russh channel-bridge tasks. The largest
remaining gaps are (1) the absence of read/write deadlines on
`ChildStdout::read` / `ChildStdin::write` after handshake, (2) the
absence of any timeout around `Child::wait()` on the OpenSSH-child path,
and (3) unbounded `mpsc::recv()` and `tokio::sync::mpsc::Sender::blocking_send`
on the embedded path.

## Upstream contract

Upstream rsync's I/O timeout is implemented as a `select(2)` deadline
applied to every read and every write. The relevant entry points are:

- `socket.c:connect_loop()` / `socket.c:open_socket_out_wrapped()` -
  applies the connect timeout (`--contimeout`) before the TCP handshake
  completes, using `select(2)` on a non-blocking socket. This is the
  contract our `ConnectWatchdog` and `tokio::time::timeout` calls mirror
  for the SSH child spawn and the russh `connect()` future, respectively.
- `io.c:io_set_select_timeout()` and `io.c:read_timeout()` /
  `io.c:writefd_unbuffered()` - apply the steady-state I/O timeout
  (`--timeout=SECONDS`) on every read and every write. Each call to
  `select(2)` arms a deadline equal to the configured value; on expiry
  the process exits with `RERR_TIMEOUT` (exit code 30).
- `options.c` - parses both `--timeout` and `--contimeout` into the
  globals consulted by `io.c` and `socket.c`.

The upstream model therefore guarantees that **no read or write can hang
indefinitely** once `--timeout` is set. oc-rsync inherits this expectation
through the `ClientConfig::timeout()` / `TransferTimeout` plumbing, but
the SSH transport layer does not currently honour it.

## CLI to SSH layer plumbing

What is wired:

- `--contimeout=N` -> `ClientConfig::connect_timeout()` (a
  `TransferTimeout`) -> `SshCommand::set_connect_timeout()` (passed via
  `build_ssh_connection()` at
  `crates/core/src/client/remote/ssh_transfer.rs:295-299`). On the
  OpenSSH-child path this becomes both `-oConnectTimeout=N` on the SSH
  argv (line 379 of `builder.rs`) **and** the duration handed to the
  in-process `ConnectWatchdog` thread (line 338 of `builder.rs`).
- `EmbeddedSshOptions::connect_timeout_secs` -> `SshConfig::connect_timeout`
  -> `tokio::time::timeout(...)` wrapping `russh::client::connect(...)` at
  `crates/rsync_io/src/ssh/embedded/connect.rs:151-157`.

What is NOT wired:

- `--timeout=N` -> `ClientConfig::timeout()` is consumed by the daemon
  transfer path (`daemon_transfer/orchestration/transfer.rs:166,190` and
  `daemon_transfer/mod.rs:99,102`) and propagated into the server flag
  string via `RemoteInvocationBuilder` (`invocation/builder.rs:282`), but
  is **never read by `ssh_transfer.rs` or `embedded_ssh_transfer.rs`** to
  bound local-side reads/writes on the SSH stdio. Grep shows zero call
  sites of `config.timeout()` in either file.

The remote rsync process therefore enforces `--timeout` on its own
`select(2)` loops, but the local oc-rsync side will sit blocked in
`ChildStdout::read()` indefinitely if the remote silently stalls without
closing the pipe. Killing the SSH child by hand (Ctrl-C, signal) is the
only escape today.

## Coverage matrix - OpenSSH-child path (russh-free)

Sorted by gap severity (critical first).

| # | Location | Operation | Timeout source | Status |
|---|----------|-----------|----------------|--------|
| 1 | `crates/rsync_io/src/ssh/connection.rs:218-220` (`SshReader::read`) | blocking `read(2)` on `ChildStdout` pipe FD | none after handshake | **GAP** - no deadline. Suggest `--timeout` plumbed to a `set_read_timeout`-equivalent watchdog or a Linux `IORING_OP_READ` with deadline. |
| 2 | `crates/rsync_io/src/ssh/connection.rs:230-232,234-236` (`SshWriter::write`, `flush`) | blocking `write(2)` / `flush` on `ChildStdin` pipe FD | none | **GAP** - no deadline. A wedged remote process produces back-pressure that fills the OS pipe buffer (~64 KB) and blocks indefinitely. |
| 3 | `crates/rsync_io/src/ssh/connection.rs:498-521` (`SshConnection::read`) | blocking `read` (also pre-handshake) | `ConnectWatchdog::has_fired()` short-circuits BEFORE the read; no deadline AFTER it fires for the read itself | **PARTIAL** - the watchdog kills the child on timeout (line 308-311), which unblocks the read with EOF/EPIPE. After `cancel_connect_watchdog()` is called there is no fallback. |
| 4 | `crates/rsync_io/src/ssh/connection.rs:524-540` (`SshConnection::write`, `flush`) | blocking `write` / `flush` (pre- and post-handshake) | none | **GAP** - the connect watchdog only triggers on read paths (line 503-513). The pre-handshake write of the rsync version greeting can hang if the SSH child is stalled but not yet killed. |
| 5 | `crates/rsync_io/src/ssh/connection.rs:105-122` (`SshConnection::wait`) | `Child::wait()` blocking on subprocess exit | none | **GAP** - no upper bound. If the SSH child hangs in `close_wait` after we drop the writer (line 600 of `ssh_transfer.rs`), `wait_with_stderr` blocks forever. Suggest a wait-timeout escalating to `Child::kill()` on expiry. |
| 6 | `crates/rsync_io/src/ssh/connection.rs:130-151` (`SshConnection::wait_with_stderr`) | `Child::wait()` then `drain.join()` | none | **GAP** - same as #5; additionally `drain.join()` (line 137-139) is an unbounded thread join blocked on stderr EOF. |
| 7 | `crates/rsync_io/src/ssh/connection.rs:444-454,461-471` (`SshChildHandle::wait`, `wait_with_stderr`) | `Child::wait()` + drain join | none | **GAP** - same root cause as #5/#6, on the split-handle code path used after `connection.split()` in `ssh_transfer.rs:554`. |
| 8 | `crates/rsync_io/src/ssh/connection.rs:474-495` (`SshChildHandle::Drop`, `SshConnection::Drop`) | `Child::try_wait()` then `child.kill()` then `child.wait()` | none on the final `wait()` | **GAP** - the kill is best-effort (line 483, 553) but the subsequent `child.wait()` (line 485, 555) is unbounded. A pathological SSH child that ignores SIGKILL (e.g., uninterruptible sleep in D-state) hangs the drop. Cite upstream `main.c:wait_process_with_flush()` for the kill-then-wait contract; upstream uses `waitpid(WNOHANG)` polling. |
| 9 | `crates/rsync_io/src/ssh/connection.rs:154-163` (`SshConnection::try_wait`) | non-blocking | n/a (returns immediately) | **OK** - already non-blocking. Could be used to build a polling wait-with-timeout helper. |
| 10 | `crates/rsync_io/src/ssh/connection.rs:97-102` (`close_stdin`) | `stdin.flush()` then drop | none | **GAP** - `flush()` on a stalled pipe blocks. Same fix as #4. |
| 11 | `crates/rsync_io/src/ssh/aux_channel.rs:208-228` (`drain_loop`) | `BufReader::read_until(b'\n')` on `ChildStderr` / `UnixStream` | none | **GAP** - the drain thread blocks on stderr until EOF. Normally fine because the child closes stderr at exit, but a wedged child blocks the drain thread forever, which then blocks `drain.join()` in #6 and #7. Suggest periodic `try_wait`-style timeout via `set_read_timeout` on `UnixStream` (Unix) or non-blocking pipe + `poll(2)` (Linux). |
| 12 | `crates/rsync_io/src/ssh/builder.rs:285-340` (`SshCommand::spawn`) | `Command::spawn()` | none directly; `connect_timeout` is armed AFTER spawn returns | **PARTIAL** - `Command::spawn()` itself does fork+exec which is fast, but if the kernel is under fork pressure or the SSH binary is on a stalled NFS mount, this blocks without a deadline. Negligible in practice; record-only. |
| 13 | `crates/rsync_io/src/ssh/connection.rs:280-315` (`ConnectWatchdog::arm` thread) | `Condvar::wait_timeout_while` | the watchdog's own `timeout` Duration | **OK** - this is the timeout source for #3. |
| 14 | `crates/rsync_io/src/ssh/connection.rs:336-362` (`ConnectWatchdog::cancel`) | `JoinHandle::join()` after notify | implicit (the thread exits after `notify_one`) | **OK** - bounded by the watchdog thread's own short epilogue. |
| 15 | `crates/core/src/client/remote/ssh_transfer.rs:563-578` (handshake call) | `perform_handshake(&mut reader, &mut writer)` | `ConnectWatchdog` (kills child on timeout, surfacing EOF to reader) | **OK** for connect; **GAP** for read-during-handshake stalls after watchdog cancellation. |
| 16 | `crates/core/src/client/remote/ssh_transfer.rs:589-597` (`run_server_with_handshake`) | drives subsequent reads/writes through `&mut reader` / `&mut writer` | none (delegates to #1 / #2) | **GAP** - inherits #1, #2. |
| 17 | `crates/core/src/client/remote/ssh_transfer.rs:600-610` (`drop(writer)` then `wait_with_stderr`) | `Child::wait` + drain join | none | **GAP** - inherits #6. |

## Coverage matrix - embedded russh path

| # | Location | Operation | Timeout source | Status |
|---|----------|-----------|----------------|--------|
| 18 | `crates/rsync_io/src/ssh/embedded/connect.rs:151-157` | `russh::client::connect(...)` future | `tokio::time::timeout(ssh_config.connect_timeout, ...)` | **OK** - mirrors upstream `socket.c:connect_loop()`. |
| 19 | `crates/rsync_io/src/ssh/embedded/connect.rs:160` (`authenticate(...)`) | russh auth round-trips (agent, pubkey, password) | none | **GAP** - the entire auth handshake is unbounded. A misbehaving server that accepts the TCP connection but never responds to `SSH_MSG_USERAUTH_REQUEST` hangs here forever. Suggest extending the connect timeout to cover authentication, or adding a separate `--ssh-auth-timeout`. Cite `auth.rs:authenticate()` orchestrator which sequentially awaits each method. |
| 20 | `crates/rsync_io/src/ssh/embedded/connect.rs:163-166` (`channel_open_session().await`) | russh channel open round-trip | none | **GAP** - same as #19; should be inside the timeout window. |
| 21 | `crates/rsync_io/src/ssh/embedded/connect.rs:168-171` (`channel.exec(true, remote_command).await`) | russh exec round-trip | none | **GAP** - same as #19/#20. |
| 22 | `crates/rsync_io/src/ssh/embedded/connect.rs:31-58` (`ChannelReader::read`) | `std::sync::mpsc::Receiver::recv()` | none | **GAP** - the sync bridge `recv()` blocks the calling thread indefinitely waiting for the async channel-bridge task to forward a `ChannelMsg::Data`. No deadline, no `recv_timeout`. Suggest `recv_timeout(io_timeout)` plumbed from `--timeout`. |
| 23 | `crates/rsync_io/src/ssh/embedded/connect.rs:67-79` (`ChannelWriter::write`, `flush`) | `tokio::sync::mpsc::Sender::blocking_send` | none | **GAP** - blocks the calling thread until the async forwarder accepts the chunk. If the forwarder task wedges (e.g., russh handle mutex contention), this never returns. |
| 24 | `crates/rsync_io/src/ssh/embedded/connect.rs:186-221` (channel-bridge task) | `channel_for_read.wait()` and `write_rx.recv()` inside `tokio::select!` | none | **GAP** - both arms are unbounded awaits. A stalled SSH session never produces an `Eof`, so the task never breaks out, the bridge channels stay open, and #22/#23 stay blocked. |
| 25 | `crates/rsync_io/src/ssh/embedded/connect.rs:204-214` (forwarder `h.data(...).await`) | russh `Handle::data(...)` await | none | **GAP** - sending data over the russh handle is unbounded. The remote could refuse the channel window update and stall indefinitely. |
| 26 | `crates/rsync_io/src/ssh/embedded/resolve.rs:31-34` (`tokio::net::lookup_host`) | DNS lookup | none directly; wrapped by the connect-timeout span at #18 | **OK** - reachable only inside `tokio::time::timeout` because `connect_and_exec_async` calls `resolve_host` BEFORE `russh::client::connect`, but the timeout wrapper at line 151-157 does NOT enclose `resolve_host` (line 131). **GAP** - DNS resolution is outside the connect timeout window. A black-holed DNS server hangs `lookup_host` forever. |
| 27 | `crates/rsync_io/src/ssh/embedded/auth.rs:45-83` (`try_agent_auth` -> `agent.request_identities().await`, `authenticate_future`) | SSH agent socket round-trips | none | **GAP** - inherits #19. |
| 28 | `crates/rsync_io/src/ssh/embedded/auth.rs:96-119` (`try_identity_file_auth` -> `authenticate_publickey().await`) | russh public-key auth round-trip | none | **GAP** - inherits #19. |
| 29 | `crates/rsync_io/src/ssh/embedded/auth.rs:127-145` (`load_identity_key`) | `russh::keys::load_secret_key` -> sync file read | none | **OK** - local file I/O; bounded by filesystem latency. Record-only. |
| 30 | `crates/rsync_io/src/ssh/embedded/auth.rs:155-171` (`rpassword::prompt_password`) | interactive `read_line` on stdin | none | **OK** - user-driven; should not have a timeout. Record-only. |
| 31 | `crates/rsync_io/src/ssh/embedded/handler.rs:104-144` (`prompt_user`) | `stdin.read_line(...)` for host-key prompt | none | **OK** - user-driven. Record-only. |
| 32 | `crates/rsync_io/src/ssh/embedded/handler.rs:147-156` (`learn_host_key`) | known-hosts file write | none | **OK** - local file I/O. Record-only. |
| 33 | `crates/core/src/client/remote/embedded_ssh_transfer.rs:306-311` (`connect_and_exec`) | enters #18-25 | inherits whatever those sites enforce | **GAP** - inherits the embedded-path gaps above. |
| 34 | `crates/core/src/client/remote/embedded_ssh_transfer.rs:320-321` (`perform_handshake`) | reads/writes via #22/#23 | none | **GAP** - inherits #22/#23. |
| 35 | `crates/core/src/client/remote/embedded_ssh_transfer.rs:328-336` (`run_server_with_handshake`) | reads/writes via #22/#23 | none | **GAP** - inherits #22/#23. |

## Sites that are demonstrably timeout-bounded

For completeness, sites that DO satisfy the upstream contract:

- `SshCommand::spawn` -> `SshConnection::new` arms `ConnectWatchdog` (entry
  #13). The watchdog calls `Child::kill()` on expiry, which closes the
  pipe FDs and unblocks any in-flight `read`/`write`. Mirrors
  `socket.c:connect_loop()`.
- The OpenSSH-child path injects `-oConnectTimeout=N`,
  `-oServerAliveInterval=20`, `-oServerAliveCountMax=3` into the SSH argv
  (`builder.rs:362-381`). These are enforced by the SSH client itself,
  giving us a steady-state liveness probe even when oc-rsync's own I/O
  has no deadline. This is **not** equivalent to upstream's
  `io.c:io_set_select_timeout()` because the SSH client does not propagate
  rsync's `--timeout` semantics, and the SSH keepalive grace window
  (20 s * 3 = 60 s) is larger than the typical rsync `--timeout` value.
- The embedded path wraps `russh::client::connect(...)` in
  `tokio::time::timeout(ssh_config.connect_timeout, ...)` (entry #18).

## Cross-reference: --timeout reaches the SSH layer?

Direct answer: **no**.

- `crates/core/src/client/remote/ssh_transfer.rs` - zero references to
  `config.timeout()`. Only `config.connect_timeout()` is consulted
  (line 298).
- `crates/core/src/client/remote/embedded_ssh_transfer.rs` - zero
  references to `config.timeout()`. Only the embedded
  `connect_timeout_secs` override is consulted
  (`apply_cli_overrides`, line 239-241).
- The wire protocol forwards `--timeout` to the remote rsync via
  `RemoteInvocationBuilder` (`invocation/builder.rs:282`), so the REMOTE
  side enforces it. The LOCAL side does not.

This is the headline gap: closing it requires plumbing
`config.timeout()` into both `SshCommand`/`SshConnection` (for read/write
deadlines on the pipe FDs) and into the embedded `ChannelReader` /
`ChannelWriter` (for `recv_timeout` and `send_timeout`).

## Punch-list - fixable gaps

Sorted by severity. Each item should become a separate tracking issue.

1. **Plumb `--timeout` to OpenSSH-child stdio reads/writes.** Surface a
   read-deadline on `SshReader::read` and a write-deadline on
   `SshWriter::write` / `flush`. On Linux, register the pipe FDs with a
   `poll(2)` / `epoll(7)` loop and time-out via `ppoll` deadline. On
   macOS / BSD, use `kqueue` with `EVFILT_TIMER`. On Windows, use
   `WaitForMultipleObjects` with the pipe handle and a timer. Suggested
   issue title: "ssh: enforce --timeout on OpenSSH-child stdio reads/writes".
   Touches entries #1, #2, #4, #15, #16. Cite upstream
   `io.c:read_timeout()` and `io.c:writefd_unbuffered()`.

2. **Plumb `--timeout` to embedded-russh `ChannelReader::read`.**
   Replace `mpsc::Receiver::recv()` with `recv_timeout(io_timeout)` and
   surface a `TimedOut` `io::Error`. Suggested issue title: "ssh embedded:
   bound `ChannelReader::read` by --timeout". Touches #22, #34, #35.

3. **Plumb `--timeout` to embedded-russh `ChannelWriter::write`.** Replace
   `tokio::sync::mpsc::Sender::blocking_send` with a `send_timeout`-style
   bridge that fails fast on `--timeout` expiry. Suggested issue title:
   "ssh embedded: bound `ChannelWriter::write` by --timeout". Touches #23.

4. **Bound russh authentication and channel-open by the connect timeout.**
   Move the existing `tokio::time::timeout` wrapper at
   `embedded/connect.rs:151-157` out so it covers `authenticate(...)`,
   `channel_open_session().await`, and `channel.exec(...).await` in
   addition to `russh::client::connect`. Suggested issue title:
   "ssh embedded: extend connect-timeout to auth + channel-open + exec".
   Touches #19, #20, #21, #27, #28.

5. **Move DNS resolution inside the connect-timeout window.** The current
   call to `resolve_host` (line 131) precedes the
   `tokio::time::timeout` wrapper. A black-holed DNS server can hang
   `lookup_host` forever. Suggested issue title: "ssh embedded: include
   DNS resolution in connect-timeout window". Touches #26.

6. **Add a wait-timeout to `Child::wait()` on the OpenSSH-child path.**
   Replace the unbounded `child.wait()` in `SshConnection::wait`,
   `wait_with_stderr`, `SshChildHandle::wait`, and the `Drop` impls with
   a polling loop using `try_wait()` plus a deadline; on expiry,
   escalate to `Child::kill()` and re-wait with a short fallback
   deadline. Mirrors `main.c:wait_process_with_flush()`. Suggested issue
   title: "ssh: cap Child::wait() with a deadline-and-kill watchdog".
   Touches #5, #6, #7, #8, #17.

7. **Add a deadline to the stderr drain thread.** The `drain_loop`
   blocks in `read_until` until EOF. A wedged child that never closes
   stderr blocks `drain.join()` forever. On Unix, switch the
   `ChildStderr` / `UnixStream` to non-blocking and use `poll(2)` with a
   deadline. Suggested issue title: "ssh: bound stderr drain by
   --timeout to prevent join-on-stuck-child". Touches #11.

8. **Bound `flush` and `close_stdin` by --timeout.** `Stdin::flush` on a
   stalled pipe blocks; the `close_stdin` helper (line 97) wraps it with
   no deadline. Same fix as #1/#4. Suggested issue title: "ssh: enforce
   --timeout on Stdin::flush and close_stdin". Touches #4, #10.

9. **Bound `russh::Handle::data(...).await` in the channel-bridge task.**
   The `tokio::select!` arm at line 204-214 awaits the russh handle
   without a deadline; if russh internally stalls on a window-update,
   the task never makes progress. Wrap with `tokio::time::timeout(io_timeout, ...)`
   keyed off `--timeout`. Suggested issue title: "ssh embedded: bound
   russh `Handle::data` send by --timeout in channel-bridge task".
   Touches #24, #25.

10. **Add an end-to-end interop test that exercises `--timeout` on SSH.**
    A test that intentionally stalls a remote `cat` after handshake and
    asserts oc-rsync exits with `RERR_TIMEOUT` (exit code 30) within a
    bounded wall-clock window. Today such a test would hang. Suggested
    issue title: "tests: add ssh `--timeout` honouring interop test".

## Files audited

- `crates/rsync_io/src/ssh/mod.rs`
- `crates/rsync_io/src/ssh/aux_channel.rs`
- `crates/rsync_io/src/ssh/builder.rs`
- `crates/rsync_io/src/ssh/connection.rs`
- `crates/rsync_io/src/ssh/operand.rs`
- `crates/rsync_io/src/ssh/parse.rs`
- `crates/rsync_io/src/ssh/tests.rs` (read-only test code; no production sites)
- `crates/rsync_io/src/ssh/embedded/mod.rs`
- `crates/rsync_io/src/ssh/embedded/auth.rs`
- `crates/rsync_io/src/ssh/embedded/cipher.rs`
- `crates/rsync_io/src/ssh/embedded/config.rs`
- `crates/rsync_io/src/ssh/embedded/connect.rs`
- `crates/rsync_io/src/ssh/embedded/error.rs`
- `crates/rsync_io/src/ssh/embedded/handler.rs`
- `crates/rsync_io/src/ssh/embedded/resolve.rs`
- `crates/rsync_io/src/ssh/embedded/types.rs`
- `crates/core/src/client/remote/ssh_transfer.rs`
- `crates/core/src/client/remote/embedded_ssh_transfer.rs`

The repository does not have a `crates/transport/` crate; the
"OpenSSH-child path" referenced in the audit task lives entirely in
`crates/rsync_io/src/ssh/` and is driven by
`crates/core/src/client/remote/ssh_transfer.rs`.
