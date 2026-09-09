# Exit-cleanup funnel inventory

Design inventory for mirroring upstream's `_exit_cleanup` funnel
(cleanup.c) end to end. Stage b1 landed the `ExitCodeLatch` and wired
it at the frontend tail and the signal watchdog (PR #7768). This doc
enumerates every upstream funnel step, names the oc site that owns it
today, and lists where the funnel's `record()` calls must land so an
error latches at the site where its RERR code is decided - the gap b1
deliberately left open.

All upstream citations are from rsync 3.5.0
(`target/interop/upstream-src/rsync-3.5.0/`), read, not recalled. All
oc citations are from master at 0870dafa3.

## 1. Upstream `_exit_cleanup` walk (cleanup.c:103-283)

`_exit_cleanup(code, file, line)` is `NORETURN`. Its state lives in
function-local statics (`switch_step`, `exit_code`, `exit_file`,
`exit_line`, `first_code`, cleanup.c:105-108), so recursion from a
cleanup step or a signal handler resumes after the last completed step
and can never overwrite the first error.

Pre-switch prologue (runs on every entry):

- cleanup.c:110-111 - ignore SIGUSR1/SIGUSR2 so `kill_all` from step 4
  cannot re-enter mid-cleanup.
- cleanup.c:113-117 - the latch: `if (!exit_code)` record code, file,
  and `abs(line)`. First non-zero writer wins; zero never latches.
- cleanup.c:119-122 - `am_server && code == 0` promotes `am_server`
  to 2 so `log_exit` (log.c:947) suppresses the server-side message on
  a clean run.

The `switch (switch_step)` ladder (cleanup.c:126-270; each
`case_N.h` include is one step, executed at most once):

| step | lines | what it does |
|------|-------|--------------|
| 0 | 127-141 | `first_code = code`; flush a pending progress newline (`output_needs_newline` -> `fputc('\n', stdout)`); `DEBUG_GTE(EXIT, 2)` "entered" trace |
| 1 | 143-154 | reap `cleanup_child_pid` with `wait_process(..., WNOHANG)`; worst-wins: `if (status > exit_code) exit_code = status` |
| 2 | 156-184 | partial-file finalization, gated on `cleanup_got_literal` (:159): close `cleanup_fd_r`, `flush_write_file` + close `cleanup_fd_w`; if `keep_partial && handle_partial_dir(PDIR_CREATE)` then `finish_transfer()` renames the temp onto `cleanup_new_fname`, stamping `modtime = 0` when `!partial_dir` (:174-180) so `--update` never skips the stub |
| 3 | 186-195 | `flush_ok_after_signal` -> `io_flush(FULL_FLUSH)` when code is RERR_SIGNAL; a fully clean exit (`!exit_code && !code`) also gets `io_flush(FULL_FLUSH)` |
| 4 | 197-226 | `do_unlink_at(cleanup_fname)` (a non-partial temp is unlinked, :200-201); `if (exit_code) kill_all(SIGUSR1)` (:202-203); daemon pid-file unlink when `cleanup_pid == getpid()` (:204-208); io_error -> RERR mapping when `exit_code == 0` (:210-219, three independent ifs: DEL_LIMIT=25, then VANISHED=24, then `GENERAL \|\| got_xfer_error`=23, so precedence is GENERAL > VANISHED > DEL_LIMIT); `log_exit()` gate (:221-226): emit when `exit_code && line > 0`, or always for a daemon, or when a logfile wants the stats line - `line < 0` marks an exit caused by a received MSG_ERROR_EXIT and suppresses the duplicate |
| 5 | 228-237 | `DEBUG_GTE(EXIT, 1)` "about to call exit(N)" trace |
| 6 | 239-258 | peer notification: unless the code is RERR_SOCKETIO/STREAMIO/SIGNAL1/TIMEOUT or `shutting_down` (:242-243), and `protocol_version >= 31 \|\| am_receiver` (:244), send `send_msg_int(MSG_ERROR_EXIT, exit_code)` when `line > 0` (:245-251); non-senders `io_flush(MSG_FLUSH)` (:252-253, :256-257); then `noop_io_until_death()` (:254) parks reading until the peer FINs |
| 7 | 260-265 | server-side `msleep(100)` grace when exiting non-zero (:263-264); `close_all()` (:265) shuts down every socket fd |

Exit proper (cleanup.c:272-282): `_exit(exit_code)` when
`called_from_signal_handler` (with a gcov dump under coverage builds),
otherwise `exit(exit_code)`.

`log_exit` itself (log.c:937-963): the sender (or a clean exit) prints
the `sent/received/total size` FLOG stats line (:941-946); a non-zero
code with `am_server != 2` prints `rsync error: <name> (code N) at
file(line) [role=version]` as FERROR, or FWARNING for RERR_VANISHED
(:947-962).

Supporting entry points: `cleanup_set` (cleanup.c:293-301) records the
temp/dest pair per in-flight file, `cleanup_disable` (:285-290) clears
it after commit, `cleanup_set_pid` (:303-306) arms the pid-file
unlink, and `cleanup_got_literal` (:88) gates step 2.

## 2. Upstream step -> oc site

oc is one process with threads where upstream forks, so several steps
map to thread seams rather than child handling. "NO ANALOGUE" means no
code performs the behavior today, not that it is unreachable.

| upstream step (cleanup.c) | oc owner today |
|---------------------------|----------------|
| latch :113-117, :210-212 | `crates/core/src/exit_code/latch.rs:53` `record()`, `:66` `resolve()`; process-wide instance `:104` `process_latch()` |
| single-entrant `switch_step` :105, :126 | `latch.rs:79` `claim_exit()`; wired at `crates/cli/src/frontend/mod.rs:432` `latched_run_exit` (tail) and `:453` `abort_exit` (watchdog, called at `:441`/`:531`); epoch reset at `:315` |
| SIGUSR1/SIGUSR2 ignore :110-111 | NO ANALOGUE - oc has no `kill_all` sibling-signal storm to mask (single process) |
| `am_server = 2` message suppression :119-122 | folded into the server tail: `crates/cli/src/frontend/server/run.rs:751-762` only writes a message on `Err` |
| step 0 newline flush :132-135 | progress observer `finish()` at `crates/cli/src/frontend/execution/drive/summary.rs:234-240` (Ok arm) and `:345-351` (Err arm); not run on the watchdog abort path |
| step 0/5 EXIT debug traces :137-141, :231-237 | NO ANALOGUE - `--debug=exit` levels are not implemented |
| step 1 child reap + worst-wins :146-154 | SSH child only: `crates/core/src/client/remote/ssh_transfer/exit_status.rs:149` `map_child_exit_status()`, worst-wins comparison per its doc (:145) |
| step 2 partial finalization :159-183 | unwind path: `crates/transfer/src/disk_commit/process/commit.rs:339` `retain_partial_file` (task 402's retention rule); abort path: `crates/engine/src/util/cleanup.rs:371` `CleanupManager::finalize_partials` -> `:184` `finalize_partial` (mtime-0 tweak per cleanup.c:174-180); registration mirror of `cleanup_set`/`cleanup_got_literal`: `crates/transfer/src/disk_commit/process/file_ops.rs:46` `register_temp_with_cleanup`, `:69` `partial_destination`; `handle_partial_dir` split: `cleanup.rs:161` `remove_partial_dir` |
| step 3 output flush :189-195 | frontend tail: `flush_diagnostics` + adapter flush at `frontend/mod.rs:370-390`; transfer side: `ServerWriter::flush_all_pending` (called from `announce_error_exit`, `crates/transfer/src/lib.rs:1278`) |
| step 4 temp unlink :200-201 | `cleanup.rs:300` `CleanupManager::cleanup` / `:312` `cleanup_temp_files`; RAII `TempFileGuard` drop on the unwind path |
| step 4 `kill_all(SIGUSR1)` :202-203 | NO ANALOGUE - threads, not processes; the watchdog wakes blocked I/O instead (`frontend/mod.rs:541-545`) |
| step 4 pid-file unlink :204-208 | `crates/daemon/src/daemon/sections/server_runtime/pid_file.rs:37` `PidFileGuard::drop` - RAII, not funnel-driven, so it is skipped on `process::exit` from the watchdog |
| step 4 io_error -> RERR :210-219 | rule owner: `crates/transfer/src/generator/io_error_flags.rs:53` `to_exit_code`; applied (with the `got_xfer_error` lift, :217-218) at `crates/core/src/client/remote/daemon_transfer/orchestration/stats.rs:114-123` and `crates/core/src/client/remote/ssh_transfer/exit_status.rs:120-129`; honored at the CLI tail via `summary.io_error_exit_code()` (`summary.rs:342`) |
| step 4 `log_exit` :221-226 | error line: `Message` rendering at the CLI (`summary.rs:353-358` via `error.message()`); stats line: `emit_transfer_summary` / `emit_log_output` (`summary.rs:276-336`); the `line < 0` duplicate-suppression maps to `RemoteExitError` detection (`transfer/src/lib.rs:1291` `remote_exit_code`, checked at `:1247`) |
| step 6 MSG_ERROR_EXIT :242-258 | `crates/transfer/src/lib.rs:1242` `announce_error_exit` (PR #7609), called from the server dispatch tail at `:1198`; code classing via `crates/transfer/src/error.rs:280` `rerr_for_io_error` |
| step 6 `noop_io_until_death` :254 | `crates/fast_io/src/stdio_shutdown.rs` (half-close so the peer reads EOF); TCP side `crates/transfer/src/writer/server.rs:588` `shutdown_send_side` |
| step 7 server `msleep(100)` :263-264 | NO ANALOGUE - the linger timeout in `shutdown_send_side` bounds the drain instead |
| step 7 `close_all()` :265 | NO ANALOGUE as a sweep - fds close by drop; `shutdown_send_side` covers the socket half-close case |
| exit :272-282 | normal path: `run()` returns the resolved i32 (`frontend/mod.rs:418`); abort path: `std::process::exit(code)` in the watchdog (`frontend/mod.rs:533`) |

## 3. `record()` insertion inventory

b1's latch only closes the exit-code race once a code has been
recorded. Today the first `record()` happens at the frontend tail
(`latched_run_exit`, `frontend/mod.rs:418`), after `execute()` has
returned. A second interrupt landing while an `Err` is still
propagating up inside `execute()` finds the latch empty and the
watchdog latches RERR_SIGNAL - upstream instead latches at the error
site, because the first `_exit_cleanup(RERR_*)` call happens where the
error occurs (cleanup.c:113). The funnel must therefore `record()`
where the RERR code is decided. Sites, ranked; rank 1 is the minimal
b2 shape:

1. `crates/cli/src/frontend/execution/drive/summary.rs:359` - the
   `Err` arm of `execute_transfer` calls `error.exit_code()`. This is
   the one place `execute()`'s error mapping produces the i32 for
   every failed transfer (local, SSH, daemon). One
   `process_latch().record(error.exit_code())` at the top of the `Err`
   arm (`:344`) closes the propagation window from `run_client_with_observer`
   (`:225`) upward and pins the code before message rendering starts.
2. `summary.rs:342` - the Ok arm's
   `summary.io_error_exit_code().unwrap_or(0)`. A transfer that
   completes its summary but owes 23/24/25 has decided its code here;
   record it so an interrupt during summary rendering cannot replace a
   PARTIAL/VANISHED/DEL_LIMIT verdict with RERR_SIGNAL (upstream
   keeps the io_error verdict because step 4 runs before any second
   entry can matter).
3. `crates/core/src/client/remote/daemon_transfer/orchestration/stats.rs:114-123`
   and `crates/core/src/client/remote/ssh_transfer/exit_status.rs:120-129` -
   the earliest points where the io_error bitfield becomes an i32.
   Recording here latches while the wire teardown that follows can
   still fail; both flow into site 2, so these matter only if 431/432
   move the mapping (see section 4).
4. `crates/core/src/client/error.rs:61` - `ClientError::new`, the
   constructor where every client error's `ExitCode` is decided. This
   is the closest analogue to upstream's latch-at-the-error-site, and
   the deepest single chokepoint. Deferred beyond b2's minimal shape:
   it must first be shown that no `ClientError` is ever constructed
   and then recovered from, since the latch has no unrecord and a
   discarded error would poison a clean exit.
5. Pre-transfer decisions at the frontend: the clap-parse arm
   (`frontend/mod.rs:394-402`) and the `fail_with_message` returns in
   `crates/cli/src/frontend/execution/drive/workflow/run.rs` (helper:
   `.../drive/messages.rs:22-30`). No transfer is running yet, so a
   racing signal genuinely is the first error; these need no
   `record()`.

Server side (for completeness, outside `execute()`):
`crates/cli/src/frontend/server/run.rs:754`
(`ExitCode::from_io_error`) and `crates/transfer/src/error.rs:280`
(`rerr_for_io_error`, feeding `announce_error_exit`) are the
server-role deciders; the server process exit funnels through the same
frontend `run()` tail.

## 4. Funnel constraints (tasks 431-433)

- **Abort disconnects ropes, never joins threads (433).** Upstream's
  step 1 waits on a child with WNOHANG and never blocks; oc's abort
  path must do the same with its pipeline threads. The watchdog's
  winning branch (`frontend/mod.rs:531-533`) runs
  `finalize_partials()` then `process::exit` without joining the disk
  or network threads - correct, keep it. The funnel must signal
  shutdown by dropping/disconnecting the SPSC rope ends
  (`crates/transfer/src/pipeline/spsc.rs:212` `is_disconnected`; the
  disk thread exits on channel disconnect,
  `crates/transfer/src/disk_commit/thread.rs:233-235`), never by
  `JoinHandle::join`, which can deadlock behind a blocked `recv`.
  Pinned by `spsc.rs` tests `sender_drop_disconnects` (:347) and
  `receiver_drop_disconnects` (:358), and the parked-consumer
  wake-on-disconnect behavior (:227, :292).
- **--partial retention moves into the funnel (431).** Today
  retention is split: `retain_partial_file` (`commit.rs:339`) on the
  unwind path vs `finalize_partials` (`cleanup.rs:371`) on the abort
  path, kept in sync only by `partial_destination`
  (`file_ops.rs:69`) mirroring `retain_partial_file`'s branches. The
  funnel becomes the single caller, as upstream's step 2 is
  (cleanup.c:159-183), with `register_temp_with_cleanup`
  (`file_ops.rs:46`) staying the `cleanup_set`/`cleanup_got_literal`
  mirror. Pinned by
  `crates/transfer/src/disk_commit/tests_partial_interrupt_parity.rs`,
  `crates/core/tests/sigint_temp_cleanup.rs`,
  `crates/core/tests/signal_integration.rs`, and
  `crates/core/tests/cleanup_manager.rs`.
- **io_error -> RERR de-dup (432).** The selection rule has one owner
  (`io_error_flags::to_exit_code`, cleanup.c:210-219 ordering), but
  the `got_xfer_error` lift (cleanup.c:217-218) is pasted verbatim at
  both call sites: `stats.rs:117-123` and `exit_status.rs:123-129`.
  Fold both into one helper (or into `to_exit_code` behind a
  `got_xfer_error` parameter) when the funnel takes over step 4.
  Precedence pinned by `io_error_flags.rs` tests (:68-95, including
  the else-if regression case); the end-to-end lift by the daemon and
  SSH stats paths' consumers of `io_error_exit_code`.
- **Latch semantics stay fixed.** First-non-zero-writer-wins, zero
  never latches, exactly one claimant. Pinned by `latch.rs` tests
  (:109-154) and the two-thread race harness
  `frontend/mod.rs:564` `exit_latch_race_tests`.
- **MSG_ERROR_EXIT stays a funnel step.** `announce_error_exit` is
  step 6 ported arm for arm; when the funnel forms, it must run after
  the code is latched and before the exit, with the same four-code
  transport exclusion and the `RemoteExitError` echo guard. Pinned by
  `crates/transfer/src/tests/error_exit_gate.rs`.
- **Pid-file unlink is RAII, the funnel's exit is not.** A watchdog
  `process::exit` skips `PidFileGuard::drop` (`pid_file.rs:37`);
  upstream unlinks inside the funnel (cleanup.c:204-208). When the
  daemon joins the funnel, the unlink becomes a funnel step or the
  abort path must run the guard explicitly. Pinned by
  `crates/daemon/src/tests/chunks/run_daemon_writes_and_removes_pid_file.rs`.

## 5. Notes against the b1 record

- Verified: the tail wiring and watchdog wiring are exactly as b1
  recorded (`latched_run_exit` at `frontend/mod.rs:418`/`:432`,
  `abort_exit` at `:453` used by the watchdog at `:531`), and the
  propagation window is real - no `record()` exists anywhere between
  the error site and `:418` today.
- One nuance b1's record does not state: the watchdog's winning abort
  branch finalizes partials after claiming (`:531-533`), which
  compresses upstream's ordering (partials are step 2, before the
  step 4 code mapping). Harmless today because the abort code is
  already resolved, but the b2 funnel should adopt upstream's step
  order so the partial rename can never race the code decision.
