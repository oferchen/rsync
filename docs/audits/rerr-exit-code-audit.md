# RERR_* Exit Code Coverage Matrix vs Upstream

This audit compares oc-rsync's exit code coverage against upstream rsync 3.4.1.
It maps every `RERR_*` constant from `errcode.h` to our `ExitCode` enum, lists
emission sites on both sides, and flags description-string drift, semantic
gaps, and test-coverage holes.

Sources reviewed:

- `target/interop/upstream-src/rsync-3.4.1/errcode.h` - numeric values.
- `target/interop/upstream-src/rsync-3.4.1/log.c` (`rerr_names[]`, lines
  77-107) - canonical strings.
- `crates/core/src/exit_code/codes.rs` - `ExitCode` enum and helpers.
- `crates/core/src/client/error.rs` - `ClientError` factories and the
  `error_code_name`/`error_code` mappings.
- `tests/exit_codes.rs` - integration tests for binary exit behaviour.

Issue: #2114.

## 1. Master Matrix

Parity column legend:

- `value` - numeric value matches upstream.
- `string` - description string matches upstream `rerr_names[]` exactly.
- `name` - we expose the same `RERR_*` symbol via `error_code_name()`.
- `emit` - we emit the code from at least one production path that mirrors an
  upstream emission site.

| RERR_* (upstream)   | Value | Upstream description (`rerr_names[]`)                          | Our `ExitCode` variant | Our description (`ExitCode::description`)              | Parity                          |
|---------------------|-------|----------------------------------------------------------------|------------------------|---------------------------------------------------------|---------------------------------|
| `RERR_OK`           | 0     | (not in `rerr_names`; success)                                 | `Ok`                   | `success`                                              | value, name, emit               |
| `RERR_SYNTAX`       | 1     | `syntax or usage error`                                        | `Syntax`               | `syntax or usage error`                                | value, string, name, emit       |
| `RERR_PROTOCOL`     | 2     | `protocol incompatibility`                                     | `Protocol`             | `protocol incompatibility`                             | value, string, name, emit       |
| `RERR_FILESELECT`   | 3     | `errors selecting input/output files, dirs`                    | `FileSelect`           | `errors selecting input/output files, dirs`           | value, string, name, emit       |
| `RERR_UNSUPPORTED`  | 4     | `requested action not supported`                               | `Unsupported`          | `requested action not supported`                       | value, string, name; **no emit** |
| `RERR_STARTCLIENT`  | 5     | `error starting client-server protocol`                        | `StartClient`          | `error starting client-server protocol`               | value, string, name, emit       |
| (none)              | 6     | (not defined upstream)                                         | `LogFileAppend`        | `daemon unable to append to log-file`                  | **EXTRA**: oc-rsync only        |
| `RERR_SOCKETIO`     | 10    | `error in socket IO`                                           | `SocketIo`             | `error in socket IO`                                   | value, string, name, emit       |
| `RERR_FILEIO`       | 11    | `error in file IO`                                             | `FileIo`               | `error in file IO`                                     | value, string, name; **no emit** |
| `RERR_STREAMIO`     | 12    | `error in rsync protocol data stream`                          | `StreamIo`             | `error in rsync protocol data stream`                  | value, string, name; **no emit** |
| `RERR_MESSAGEIO`    | 13    | `errors with program diagnostics`                              | `MessageIo`            | `errors with program diagnostics`                      | value, string, name; **no emit** |
| `RERR_IPC`          | 14    | `error in IPC code`                                            | `Ipc`                  | `error in IPC code`                                    | value, string, name; **no emit** |
| `RERR_CRASHED`      | 15    | `sibling process crashed`                                      | `Crashed`              | `received SIGSEGV or SIGBUS or SIGABRT`                | value, name; **string mismatch**, **no emit** |
| `RERR_TERMINATED`   | 16    | `sibling process terminated abnormally`                        | `Terminated`           | `received SIGINT, SIGTERM, or SIGHUP`                  | value, name; **string mismatch**, **no emit** |
| `RERR_SIGNAL1`      | 19    | `received SIGUSR1`                                             | `Signal1`              | `received SIGUSR1`                                     | value, string, name; **no emit** |
| `RERR_SIGNAL`       | 20    | `received SIGINT, SIGTERM, or SIGHUP`                          | `Signal`               | `received SIGINT, SIGTERM, or SIGHUP`                  | value, string, name, emit       |
| `RERR_WAITCHILD`    | 21    | `waitpid() failed`                                             | `WaitChild`            | `waitpid() failed`                                     | value, string, name, emit (SSH child mapping) |
| `RERR_MALLOC`       | 22    | `error allocating core memory buffers`                         | `Malloc`               | `error allocating core memory buffers`                | value, string, name; **no emit** |
| `RERR_PARTIAL`      | 23    | `some files/attrs were not transferred (see previous errors)` | `PartialTransfer`      | `partial transfer`                                     | value, name, emit; **string mismatch** |
| `RERR_VANISHED`     | 24    | `some files vanished before they could be transferred`         | `Vanished`             | `some files vanished before they could be transferred`| value, string, name, emit       |
| `RERR_DEL_LIMIT`    | 25    | `the --max-delete limit stopped deletions`                     | `DeleteLimit`          | `max delete limit stopped deletions`                   | value, name, emit; **string mismatch** |
| `RERR_TIMEOUT`      | 30    | `timeout in data send/receive`                                 | `Timeout`              | `timeout in data send/receive`                         | value, string, name, emit       |
| `RERR_CONTIMEOUT`   | 35    | `timeout waiting for daemon connection`                        | `ConnectionTimeout`    | `timeout waiting for daemon connection`                | value, string, name, emit       |
| `RERR_CMD_FAILED`   | 124   | `remote shell failed`                                          | `CommandFailed`        | `remote command failed`                                | value, name, emit; **string mismatch** |
| `RERR_CMD_KILLED`   | 125   | `remote shell killed`                                          | `CommandKilled`        | `remote command killed`                                | value, name, emit; **string mismatch** |
| `RERR_CMD_RUN`      | 126   | `remote command could not be run`                              | `CommandRun`           | `remote command could not be run`                      | value, string, name; **no emit** |
| `RERR_CMD_NOTFOUND` | 127   | `remote command not found`                                     | `CommandNotFound`      | `remote command not found`                             | value, string, name, emit       |

Totals:

- 26 upstream `RERR_*` constants. All 26 have matching numeric values and
  `RERR_*` names exposed via `ClientError::error_code_name()`.
- 20 / 26 description strings match upstream verbatim.
- 6 / 26 description strings drift (codes 15, 16, 23, 25, 124, 125).
- 1 oc-rsync-only code (`6` / `LogFileAppend`) has no upstream counterpart.
- 9 upstream-emitted codes have no oc-rsync emission site (gaps in section 3).

## 2. Per-Code Emission Sites

Upstream sites come from `grep RERR_<NAME>` over `target/interop/upstream-src/
rsync-3.4.1/*.c`. oc-rsync sites come from `grep ExitCode::<Variant>` over
`crates/`, excluding tests, doc-comments, and the `exit_code` module itself
(which only catalogues codes, never emits them).

| Code              | Upstream emission sites (representative)                                             | oc-rsync emission sites                                                                                                          |
|-------------------|---------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|
| `Ok` (0)          | normal return path                                                                    | `crates/core/src/signal/unix.rs::ShutdownReason::exit_code` (UserRequested), all happy paths                                     |
| `Syntax` (1)      | `main.c`, `options.c`, `clientserver.c`, `compat.c`, `exclude.c`, `authenticate.c`, `pipe.c` | `crates/core/src/client/error.rs::compile_filter_error`, `invalid_argument_error_typed`, `daemon_authentication_*`               |
| `Protocol` (2)    | `compat.c` (~11 sites), `flist.c`, `io.c` (~10 sites), `generator.c`, `exclude.c`     | `crates/core/src/client/error.rs::daemon_protocol_error`, `crates/daemon/src/error.rs` (protocol violations)                     |
| `FileSelect` (3)  | `main.c`, `clientserver.c`, `loadparm.c`, `flist.c`, `exclude.c`                      | `crates/core/src/client/error.rs::destination_access_error`                                                                       |
| `Unsupported` (4) | `acls.c`, `xattrs.c`, `compat.c`, `options.c`, `flist.c`, `hlink.c`, `batch.c`        | **NONE** (see Gaps)                                                                                                              |
| `StartClient` (5) | `clientserver.c`, `socket.c`                                                          | `crates/core/src/client/error.rs` (daemon connection failure factories via `daemon_error`)                                        |
| `LogFileAppend` (6) | **N/A**                                                                              | `crates/daemon/src/error.rs` (log file open/append failure)                                                                       |
| `SocketIo` (10)   | `socket.c`, `io.c`, `clientname.c`, `clientserver.c`                                  | `crates/core/src/client/error.rs::socket_error`, `crates/core/src/signal/unix.rs::ShutdownReason::PipeBroken`                     |
| `FileIo` (11)     | `receiver.c`, `generator.c`, `fileio.c`, `checksum.c`, `xattrs.c`, `acls.c`           | **NONE** at production call-sites (string is wired through `daemon_error`/`new` fallbacks but never selected as the primary code) |
| `StreamIo` (12)   | `io.c` (multiplexed stream framing errors), `token.c`                                 | **NONE**                                                                                                                          |
| `MessageIo` (13)  | `log.c`, `io.c` (diagnostic message handling)                                         | **NONE**                                                                                                                          |
| `Ipc` (14)        | `pipe.c`, `clientserver.c`, `main.c`, `util1.c`, `util2.c` (~10 sites)                | **NONE**                                                                                                                          |
| `Crashed` (15)    | `main.c::wait_process` (child died on SIGSEGV/SIGBUS/SIGABRT) -> set in cleanup       | **NONE** (only in tests as fixture)                                                                                              |
| `Terminated` (16) | `main.c::wait_process` (child terminated abnormally) -> set in cleanup                | **NONE** (only in tests as fixture)                                                                                              |
| `Signal1` (19)    | `main.c::sigchld_handler` (`exit_cleanup(RERR_SIGNAL1)`)                              | **NONE**                                                                                                                          |
| `Signal` (20)     | `io.c::sigusr1_handler`, `rsync.c::sigchld_handler`                                   | `crates/core/src/signal/unix.rs::ShutdownReason::{Interrupted,Terminated,HangUp}`                                                 |
| `WaitChild` (21)  | `main.c::wait_process` (waitpid fallback)                                             | `crates/core/src/client/remote/ssh_transfer.rs::map_child_exit_status` (status code unavailable), `remote_to_remote.rs`           |
| `Malloc` (22)     | `util2.c`, `loadparm.c`, `options.c`                                                  | **NONE**                                                                                                                          |
| `PartialTransfer` (23) | `cleanup.c::cleanup_and_exit` (`io_error & IOERR_GENERAL`), `main.c::_exit(RERR_PARTIAL)` | `crates/core/src/client/error.rs::missing_operands_error`, `io_error` (non-NotFound branch), `daemon_access_denied_error`, fallback for unknown codes |
| `Vanished` (24)   | `cleanup.c` (`io_error & IOERR_VANISHED`), `flist.c` (`f_name`/`is_excluded` warns)   | `crates/core/src/client/error.rs::io_error` (NotFound branch), `crates/core/src/client/remote/ssh_transfer.rs`                    |
| `DeleteLimit` (25)| `cleanup.c` (`io_error & IOERR_DEL_LIMIT`), `delete.c::skipped_deletes`               | `crates/core/src/client/error.rs::map_local_copy_error::DeleteLimitExceeded`                                                      |
| `Timeout` (30)    | `io.c::maybe_send_keepalive`, `io.c::read_timeout` (~2 sites)                         | `crates/core/src/client/error.rs::map_local_copy_error::Timeout`, `crates/core/src/timeout/error.rs`, `timeout/mod.rs`             |
| `ConnectionTimeout` (35) | `socket.c::open_socket_out_wrapped`                                            | `crates/core/src/client/remote/ssh_transfer.rs` (SSH connect timeout)                                                             |
| `CommandFailed` (124)    | shell exit 255 (from child wait status; comment in `errcode.h`)                | `crates/core/src/client/remote/ssh_transfer.rs::map_child_exit_status` (`Some(255)`)                                              |
| `CommandKilled` (125)    | shell killed by signal (from child wait status)                                | `crates/core/src/client/remote/ssh_transfer.rs::map_child_exit_status` (signal branch)                                            |
| `CommandRun` (126)       | shell cannot exec the command (from child wait status)                         | **NONE**                                                                                                                           |
| `CommandNotFound` (127)  | shell exit 127 (from child wait status)                                         | `crates/core/src/client/remote/ssh_transfer.rs::map_child_exit_status` (`Some(127)`)                                              |

## 3. Gaps (Upstream Emits, oc-rsync Does Not)

These are codes upstream rsync 3.4.1 actively emits via `exit_cleanup()` or
sets in `cleanup_and_exit()`, but oc-rsync has no production code path that
selects them as the primary exit code. We retain the variant and the numeric
value, so an upstream peer's exit code is still classifiable when relayed,
but we never originate these codes ourselves.

| Code                | Upstream trigger                                                                                                            | oc-rsync coverage                                            |
|---------------------|------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------|
| `Unsupported` (4)   | `acls.c` / `xattrs.c` when the platform lacks ACL/xattr support; `compat.c` when the peer rejects a feature; `options.c` for refused options | We surface these as `Syntax` (1) or transparent feature stubs. |
| `FileIo` (11)       | `receiver.c`/`generator.c` on local file read/write failure; `fileio.c::write_file` on partial writes                       | All file I/O errors funnel into `PartialTransfer` (23) via `io_error`. |
| `StreamIo` (12)     | `io.c` on multiplex frame corruption, oversized tags, unexpected EOF on the data stream                                     | We map `UnexpectedEof`/`InvalidData` to `StreamIo` only via `ExitCode::from_io_error`; no client/daemon factory selects it. |
| `MessageIo` (13)    | `log.c`/`io.c` when diagnostic message handling fails                                                                       | Not emitted; diagnostic failures fall through to `MessageIo` only by accident. |
| `Ipc` (14)          | `pipe.c::piped_child`, `main.c::do_cmd`, `clientserver.c` for pipe/dup failures                                              | Not emitted; pipe failures from `tokio` / `std::process` map to `SocketIo` or `FileIo`. |
| `Crashed` (15)      | `main.c::wait_process` when child died on `SIGSEGV`/`SIGBUS`/`SIGABRT`                                                       | Our `map_child_exit_status` collapses all signal deaths to `CommandKilled` (125). |
| `Terminated` (16)   | `main.c::wait_process` when child died on `SIGINT`/`SIGTERM`/`SIGHUP`                                                        | Same as above; we do not distinguish abnormal-termination subtypes. |
| `Signal1` (19)      | `main.c::sigchld_handler` and `io.c::sigusr1_handler`                                                                       | We do not handle `SIGUSR1` as a terminating signal.           |
| `Malloc` (22)       | `util2.c::out_of_memory`, `loadparm.c`, `options.c` allocation failures                                                     | Allocation failures abort via `panic!`/`abort` rather than mapping to exit 22. |
| `CommandRun` (126)  | Shell-side: command found but cannot be executed                                                                            | `map_child_exit_status` falls back to `PartialTransfer` (23) for `Some(126)`. |

## 4. Extras (oc-rsync Emits, Upstream Does Not)

Only one oc-rsync exit code has no upstream counterpart in `errcode.h`:

| Code                | Variant         | Source                                       | Justification |
|---------------------|-----------------|----------------------------------------------|---------------|
| `6` / `RERR_LOG_FAILURE` | `LogFileAppend` | `crates/daemon/src/error.rs`               | Daemon-only error for failure to append to the log file. Upstream rsync logs the message and continues with `RERR_FILEIO`/`RERR_MESSAGEIO`. We chose a distinct code so daemon operators can detect log-rotation failures without parsing stderr. |

This violates strict upstream parity and should be either retired (mapped to
`MessageIo` (13) per upstream `log.c` behaviour) or documented as an extension
in the man page. See follow-up #2114-A in the issue tracker.

## 5. Description-String Mismatches

`ExitCode::description()` is used by `tests/exit_codes.rs::assert_exit_code`
for test-failure context and by daemon log messages. Six descriptions drift
from upstream `rerr_names[]`:

| Code | RERR_*           | Upstream description                                          | Our description                            | Recommendation                                  |
|------|------------------|--------------------------------------------------------------|--------------------------------------------|-------------------------------------------------|
| 15   | `RERR_CRASHED`    | `sibling process crashed`                                     | `received SIGSEGV or SIGBUS or SIGABRT`    | Switch to upstream string. Our gloss is more precise but breaks log parity. |
| 16   | `RERR_TERMINATED` | `sibling process terminated abnormally`                       | `received SIGINT, SIGTERM, or SIGHUP`      | Switch to upstream string; the signal list also conflates with `Signal` (20). |
| 23   | `RERR_PARTIAL`    | `some files/attrs were not transferred (see previous errors)` | `partial transfer`                         | Switch to upstream string. Required for parity in `--info=stats` and daemon logs. |
| 25   | `RERR_DEL_LIMIT`  | `the --max-delete limit stopped deletions`                    | `max delete limit stopped deletions`       | Add the leading article and `--` prefix to match. |
| 124  | `RERR_CMD_FAILED` | `remote shell failed`                                         | `remote command failed`                    | Use `remote shell failed` for parity. |
| 125  | `RERR_CMD_KILLED` | `remote shell killed`                                         | `remote command killed`                    | Use `remote shell killed` for parity. |

These were known mismatches at the time of the audit. None are wire-protocol
visible (rsync transmits the numeric code, not the string), so the fix is
local to `ExitCode::description()` and any callers that compare strings.

## 6. Test-Coverage Gaps in `tests/exit_codes.rs`

Tests in `tests/exit_codes.rs` are organised by exit-code module
(`exit_code_<n>_<name>`). Coverage state:

### Implemented and asserting

| Code | Test module                        | Active assertions                                                                                                         |
|------|-------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| 0    | `exit_code_0_success`               | `--help`, `--version`, dry-run, local copy. Plus `binary_exit_codes`, `exit_code_edge_cases`.                             |
| 1    | `exit_code_1_syntax`                | invalid option, conflicting `--server --daemon`, empty filter pattern (lenient).                                          |
| 2    | `exit_code_2_protocol`              | `--protocol=99` rejected; `binary_exit_codes` adds `--protocol=1` (too low).                                              |
| 3    | `exit_code_3_file_select`           | nonexistent source (lenient: 3, 23, or 24); inaccessible destination (Unix only).                                         |
| 11   | `exit_code_11_file_io`              | unreadable source file (lenient: 11 or 23; we always return 23, never 11).                                                |
| 23   | `exit_code_23_partial_transfer`     | mixed readable/unreadable, missing operands.                                                                              |
| 25   | `exit_code_25_delete_limit`         | `--max-delete=2` with 5 files (lenient: 25, 1, or 0).                                                                     |
| Enum | `exit_code_enum_values`             | numeric value parity for all 26 codes; `from_i32` round-trip; description non-empty.                                       |

### Ignored placeholders (no live assertion)

| Code | Test module                  | Marked `#[ignore]` reason                                          |
|------|-------------------------------|--------------------------------------------------------------------|
| 4    | `exit_code_4_unsupported`     | `Exit code 4 requires specific compile-time conditions`            |
| 5    | `exit_code_5_start_client`    | `requires daemon connection error handling implementation` (x2)    |
| 10   | `exit_code_10_socket_io`      | `requires daemon connection error handling implementation`         |
| 12   | `exit_code_12_stream_io`      | `Data stream errors require protocol corruption simulation`        |
| 13   | `exit_code_13_message_io`     | `Message I/O errors require internal failure simulation`           |
| 14   | `exit_code_14_ipc`            | `IPC errors require internal process communication failure`        |
| 20   | `exit_code_20_signal`         | `Signal tests require process timing coordination`                 |
| 24   | `exit_code_24_vanished`       | `Vanished files require timing-sensitive file deletion`            |
| 30   | `exit_code_30_timeout`        | `Timeout tests require slow transfer simulation`                   |

### Codes with no test module at all

These have no `exit_code_<n>_<name>` module and no live assertion:

- 6   `LogFileAppend` - oc-rsync extension; daemon-only.
- 15  `Crashed`
- 16  `Terminated`
- 19  `Signal1`
- 21  `WaitChild`
- 22  `Malloc`
- 35  `ConnectionTimeout`
- 124 `CommandFailed`
- 125 `CommandKilled`
- 126 `CommandRun`
- 127 `CommandNotFound` (touched only via `binary_exit_codes::nonexistent_remote_shell_returns_error`, which accepts 1, 14, or 127.)

### Recommendations for closing the gaps

1. Replace ignored placeholders for codes 4, 5, 10, 14 with daemon-harness
   tests (the `tests/integration` daemon helpers can already drive these
   paths).
2. Add a `bin` test harness for codes 124/125/127 that uses a fake remote
   shell script with a controlled exit status; this exercises
   `map_child_exit_status` end-to-end. Code 126 needs an unexecutable shell
   path (e.g., a directory passed via `-e`).
3. Code 35 (`ConnectionTimeout`) is reachable via `--contimeout=1
   rsync://192.0.2.1:873/m/`; add a non-ignored test gated on
   `cfg(target_os = "linux")` to avoid macOS connect-EHOSTUNREACH variance.
4. Codes 15/16/19/21/22 are inherently hard to trigger; document them as
   "not currently emitted by oc-rsync" in the test module rather than
   leaving them silent.
5. Tighten lenient assertions: `exit_code_3_file_select::nonexistent_source`
   accepts 3, 23, or 24. Once `io_error` always returns `Vanished` (24) for
   `NotFound`, this can be tightened to a single expected code.

## Summary

- 26 codes total; 26 / 26 numeric-value parity; 26 / 26 RERR_* name parity.
- 20 / 26 description-string parity (6 known string mismatches at codes 15,
  16, 23, 25, 124, 125).
- 17 / 26 codes have at least one production emission site; 9 codes are
  reachable only via fallback or relayed peer exit status.
- 1 oc-rsync-only extension code (6 / `LogFileAppend`).
- `tests/exit_codes.rs` covers 8 codes with live assertions, 9 with ignored
  placeholders, and 9 with no module at all.
