# RERR_* Exit-Code Coverage Matrix (#2114)

Audit of every upstream `RERR_*` code, the matching oc-rsync constant,
production trigger sites, gaps, and the tests required for full
coverage in `tests/exit_codes.rs`.

Sources of truth: `target/interop/upstream-src/rsync-3.4.1/errcode.h`,
`log.c` (`rerr_names` lines 79-107); `crates/core/src/exit_code/codes.rs`.

## 1. Upstream RERR_* (rsync 3.4.1)

| Name             | Val | Description (from `log.c`)                       |
|------------------|----:|--------------------------------------------------|
| RERR_OK          |   0 | success                                          |
| RERR_SYNTAX      |   1 | syntax or usage error                            |
| RERR_PROTOCOL    |   2 | protocol incompatibility                         |
| RERR_FILESELECT  |   3 | errors selecting input/output files, dirs        |
| RERR_UNSUPPORTED |   4 | requested action not supported                   |
| RERR_STARTCLIENT |   5 | error starting client-server protocol            |
| RERR_SOCKETIO    |  10 | error in socket IO                               |
| RERR_FILEIO      |  11 | error in file IO                                 |
| RERR_STREAMIO    |  12 | error in rsync protocol data stream              |
| RERR_MESSAGEIO   |  13 | errors with program diagnostics                  |
| RERR_IPC         |  14 | error in IPC code                                |
| RERR_CRASHED     |  15 | sibling process crashed                          |
| RERR_TERMINATED  |  16 | sibling process terminated abnormally            |
| RERR_SIGNAL1     |  19 | received SIGUSR1                                 |
| RERR_SIGNAL      |  20 | received SIGINT, SIGTERM, or SIGHUP              |
| RERR_WAITCHILD   |  21 | waitpid() failed                                 |
| RERR_MALLOC      |  22 | error allocating core memory buffers             |
| RERR_PARTIAL     |  23 | some files/attrs were not transferred            |
| RERR_VANISHED    |  24 | some files vanished before transfer              |
| RERR_DEL_LIMIT   |  25 | the --max-delete limit stopped deletions         |
| RERR_TIMEOUT     |  30 | timeout in data send/receive                     |
| RERR_CONTIMEOUT  |  35 | timeout waiting for daemon connection            |
| RERR_CMD_FAILED  | 124 | remote shell failed                              |
| RERR_CMD_KILLED  | 125 | remote shell killed                              |
| RERR_CMD_RUN     | 126 | remote command could not be run                  |
| RERR_CMD_NOTFOUND| 127 | remote command not found                         |

`errcode.h` does not assign value 6. Older rsync briefly used "RERR_LOG"
there; it was removed before 3.0 and is unused on the wire today.

## 2. oc-rsync `ExitCode` (`crates/core/src/exit_code/codes.rs`)

All 26 upstream RERR values are present and numerically correct, plus
one extra `LogFileAppend = 6` mapped to a non-upstream `RERR_LOG_FAILURE`
string. Variant names: `Ok, Syntax, Protocol, FileSelect, Unsupported,
StartClient, LogFileAppend, SocketIo, FileIo, StreamIo, MessageIo, Ipc,
Crashed, Terminated, Signal1, Signal, WaitChild, Malloc, PartialTransfer,
Vanished, DeleteLimit, Timeout, ConnectionTimeout, CommandFailed,
CommandKilled, CommandRun, CommandNotFound`. No off-by-one, no missing
slots.

## 3. Trigger sites

| Variant            | Production trigger (file:line)                                   |
|--------------------|------------------------------------------------------------------|
| Ok                 | `client/remote/ssh_transfer.rs:489`; `signal::*::UserRequested`  |
| Syntax             | `client/error.rs:228` `compile_filter_error`; CLI parser         |
| Protocol           | `daemon/src/error.rs:230`; protocol negotiation                  |
| FileSelect         | `client/error.rs:258` `destination_access_error`                 |
| Unsupported        | none                                                             |
| StartClient        | only via constants/`with_code` in client handshake               |
| LogFileAppend      | none (non-upstream, dead)                                        |
| SocketIo           | `client/error.rs:271`; `signal::*::PipeBroken`                   |
| FileIo             | `daemon/src/error.rs:240`; `from_io_error` fallback              |
| StreamIo           | `from_io_error` for `UnexpectedEof`/`InvalidData`                |
| MessageIo          | none                                                             |
| Ipc                | none in production                                               |
| Crashed            | none                                                             |
| Terminated         | none                                                             |
| Signal1            | none                                                             |
| Signal             | `signal/unix.rs:52`, `signal/stub.rs:44` (INT/TERM/HUP)          |
| WaitChild          | `client/remote/remote_to_remote.rs:301,308`; `ssh_transfer.rs:504` |
| Malloc             | none (Rust aborts on OOM)                                        |
| PartialTransfer    | `client/error.rs:160,241,283` (and unknown-code fallback)        |
| Vanished           | `client/error.rs:239` (`NotFound`)                               |
| DeleteLimit        | `client/error.rs:210` `DeleteLimitExceeded`                      |
| Timeout            | `timeout/error.rs:119`; `client/error.rs:201`                    |
| ConnectionTimeout  | `timeout/error.rs:120`; `client/remote/ssh_transfer.rs:582`      |
| CommandFailed      | `ssh_transfer.rs:502` (exit 255)                                 |
| CommandKilled      | `ssh_transfer.rs:496` (signalled)                                |
| CommandRun         | none (126 not mapped)                                            |
| CommandNotFound    | `ssh_transfer.rs:501` (exit 127)                                 |

## 4. Gaps

1. `LogFileAppend` (6) is non-upstream and never triggered: drop the
   variant; daemon log-open failures should map to `MessageIo` (13) or
   `FileIo` (11) per upstream `log.c:163`.
2. `Unsupported` (4) has no trigger: wire into capability negotiation
   under `crates/core/src/protocol/capabilities` when the peer rejects.
3. `MessageIo` (13) has no trigger: wire into stderr/log write failures.
4. `Ipc` (14) has no production trigger: wire into `daemon::ipc` errors.
5. `Crashed` (15), `Terminated` (16), `Signal1` (19) have no triggers:
   add child supervision in `client/remote/*` so SIGSEGV/SIGABRT -> 15,
   signalled exit -> 16, SIGUSR1 (daemon nominal shutdown) -> 19.
6. `Malloc` (22) is unreachable on stable Rust; document and keep for
   wire compatibility only.
7. `CommandRun` (126) needs `map_child_exit_status` in
   `ssh_transfer.rs:497-505` to map shell exit 126.

## 5. Test plan (`tests/exit_codes.rs`)

Existing coverage: 0, 1, 2, 3, 5, 10, 11, 20, 23, 24, 25, 30. Add:

| New test                                       | Code | Strategy                                       |
|------------------------------------------------|-----:|------------------------------------------------|
| `unsupported_capability_returns_unsupported`    |   4 | peer rejects `--copy-devices`                   |
| `daemon_log_open_failure_returns_message_io`    |  13 | daemon `log file` on read-only path             |
| `ipc_break_returns_ipc_error`                   |  14 | SIGKILL daemon worker mid-transfer              |
| `child_segfault_returns_crashed`                |  15 | helper that aborts                              |
| `child_sigterm_returns_terminated`              |  16 | helper killed via SIGTERM                       |
| `sigusr1_returns_signal1`                       |  19 | SIGUSR1 to running transfer                     |
| `waitpid_failure_returns_waitchild`             |  21 | foreign-reaped child                            |
| `connect_timeout_returns_contimeout`            |  35 | unroutable host with `--contimeout=1`           |
| `remote_shell_exit_124_returns_cmd_failed`      | 124 | `--rsh` script exits 255                        |
| `remote_shell_killed_returns_cmd_killed`        | 125 | `--rsh` script killed by SIGTERM                |
| `remote_shell_unrunnable_returns_cmd_run`       | 126 | non-executable `--rsh` (chmod 000)              |
| `remote_shell_missing_returns_cmd_notfound`     | 127 | `--rsh /nonexistent/bin`                        |

Codes 6 and 22 intentionally not tested. Code 12 keeps the existing
`corrupted_stream_returns_stream_io_error` placeholder until #2087.
