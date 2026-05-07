# RERR_* exit-code coverage matrix

Tracks task #2114. Establishes a complete map between upstream rsync 3.4.1
`RERR_*` symbols (`target/interop/upstream-src/rsync-3.4.1/errcode.h`) and
oc-rsync's `ExitCode` enum (`crates/core/src/exit_code/codes.rs`). All exit
codes the binary returns must be wire-equal to upstream values - rsync clients,
daemons, and shell scripts switch on the integer.

## Authoritative sources

- Upstream symbols: `target/interop/upstream-src/rsync-3.4.1/errcode.h`
- Upstream description strings: `target/interop/upstream-src/rsync-3.4.1/log.c`
  (`rerr_names` table, lines 80-107).
- oc-rsync enum: `crates/core/src/exit_code/codes.rs`
- oc-rsync description map: `ExitCode::description()` in same file.
- oc-rsync `RERR_*` symbol map: `crates/core/src/client/error.rs::error_code_name`
  and `crates/daemon/src/error.rs::error_code_name`.

## Upstream RERR_* symbols (rsync 3.4.1)

| Value | Symbol            | Upstream description (`log.c`)                        |
|-------|-------------------|-------------------------------------------------------|
| 0     | RERR_OK           | success (no entry; implicit)                          |
| 1     | RERR_SYNTAX       | syntax or usage error                                 |
| 2     | RERR_PROTOCOL     | protocol incompatibility                              |
| 3     | RERR_FILESELECT   | errors selecting input/output files, dirs             |
| 4     | RERR_UNSUPPORTED  | requested action not supported                        |
| 5     | RERR_STARTCLIENT  | error starting client-server protocol                 |
| 6     | RERR_LOG_FAILURE  | (no string in `rerr_names`; daemon log-append)        |
| 10    | RERR_SOCKETIO     | error in socket IO                                    |
| 11    | RERR_FILEIO       | error in file IO                                      |
| 12    | RERR_STREAMIO     | error in rsync protocol data stream                   |
| 13    | RERR_MESSAGEIO    | errors with program diagnostics                       |
| 14    | RERR_IPC          | error in IPC code                                     |
| 15    | RERR_CRASHED      | sibling process crashed                               |
| 16    | RERR_TERMINATED   | sibling process terminated abnormally                 |
| 19    | RERR_SIGNAL1      | received SIGUSR1                                      |
| 20    | RERR_SIGNAL       | received SIGINT, SIGTERM, or SIGHUP                   |
| 21    | RERR_WAITCHILD    | waitpid() failed                                      |
| 22    | RERR_MALLOC       | error allocating core memory buffers                  |
| 23    | RERR_PARTIAL      | some files/attrs were not transferred (see prior errs)|
| 24    | RERR_VANISHED     | some files vanished before they could be transferred  |
| 25    | RERR_DEL_LIMIT    | the --max-delete limit stopped deletions              |
| 30    | RERR_TIMEOUT      | timeout in data send/receive                          |
| 35    | RERR_CONTIMEOUT   | timeout waiting for daemon connection                 |
| 124   | RERR_CMD_FAILED   | remote shell failed                                   |
| 125   | RERR_CMD_KILLED   | remote shell killed                                   |
| 126   | RERR_CMD_RUN      | remote command could not be run                       |
| 127   | RERR_CMD_NOTFOUND | remote command not found                              |

`errcode.h` itself defines no values between 6 and 10, between 16 and 19, or
between 25 and 30. Those gaps are reserved by upstream and must remain unused
by oc-rsync.

## oc-rsync ExitCode enum

`crates/core/src/exit_code/codes.rs` defines a `#[repr(i32)]` enum where every
discriminant is the upstream integer. The variant names are Rust-idiomatic but
the description strings come straight from `log.c`:

| Variant            | i32 | Maps to        | Helper classification           |
|--------------------|-----|----------------|---------------------------------|
| `Ok`               | 0   | RERR_OK        | `is_success`                    |
| `Syntax`           | 1   | RERR_SYNTAX    | -                               |
| `Protocol`         | 2   | RERR_PROTOCOL  | `is_fatal`                      |
| `FileSelect`       | 3   | RERR_FILESELECT| -                               |
| `Unsupported`      | 4   | RERR_UNSUPPORTED| -                              |
| `StartClient`      | 5   | RERR_STARTCLIENT| `is_fatal`                     |
| `LogFileAppend`    | 6   | RERR_LOG_FAILURE| `is_fatal`                     |
| `SocketIo`         | 10  | RERR_SOCKETIO  | `is_fatal`                      |
| `FileIo`           | 11  | RERR_FILEIO    | -                               |
| `StreamIo`         | 12  | RERR_STREAMIO  | `is_fatal`                      |
| `MessageIo`        | 13  | RERR_MESSAGEIO | -                               |
| `Ipc`              | 14  | RERR_IPC       | `is_fatal`                      |
| `Crashed`          | 15  | RERR_CRASHED   | `is_fatal`                      |
| `Terminated`       | 16  | RERR_TERMINATED| `is_fatal`                      |
| `Signal1`          | 19  | RERR_SIGNAL1   | -                               |
| `Signal`           | 20  | RERR_SIGNAL    | -                               |
| `WaitChild`        | 21  | RERR_WAITCHILD | -                               |
| `Malloc`           | 22  | RERR_MALLOC    | `is_fatal`                      |
| `PartialTransfer`  | 23  | RERR_PARTIAL   | `is_partial`                    |
| `Vanished`         | 24  | RERR_VANISHED  | `is_partial`                    |
| `DeleteLimit`      | 25  | RERR_DEL_LIMIT | `is_partial`                    |
| `Timeout`          | 30  | RERR_TIMEOUT   | `is_fatal`                      |
| `ConnectionTimeout`| 35  | RERR_CONTIMEOUT| `is_fatal`                      |
| `CommandFailed`    | 124 | RERR_CMD_FAILED| -                               |
| `CommandKilled`    | 125 | RERR_CMD_KILLED| -                               |
| `CommandRun`       | 126 | RERR_CMD_RUN   | -                               |
| `CommandNotFound`  | 127 | RERR_CMD_NOTFOUND| -                             |

`ExitCode::from_i32` round-trips every value. `From<ExitCode> for
std::process::ExitCode` clamps to `0..=255`, which is safe for all upstream
values.

## Coverage matrix: RERR_* -> production emission

"Defined" means the variant exists in the enum and round-trips in
`from_i32`/`description`/`error_code_name`. "Emitted" means non-test code
returns the variant on a real error path.

| Symbol            | Defined | Emitted in production paths | Status   |
|-------------------|---------|-----------------------------|----------|
| RERR_OK           | yes     | yes (success path)          | covered  |
| RERR_SYNTAX       | yes     | yes (cli, daemon parsing)   | covered  |
| RERR_PROTOCOL     | yes     | yes (handshake, multiplex)  | covered  |
| RERR_FILESELECT   | yes     | yes (`from_io_error`, fs)   | covered  |
| RERR_UNSUPPORTED  | yes     | yes (capability/feature)    | covered  |
| RERR_STARTCLIENT  | yes     | yes (ssh/daemon dial)       | covered  |
| RERR_LOG_FAILURE  | yes     | no (daemon log path n/a)    | partial  |
| RERR_SOCKETIO     | yes     | yes (transport, ssh)        | covered  |
| RERR_FILEIO       | yes     | yes (engine, metadata)      | covered  |
| RERR_STREAMIO     | yes     | yes (protocol decode)       | covered  |
| RERR_MESSAGEIO    | yes     | no (no diagnostic emit)     | partial  |
| RERR_IPC          | yes     | yes (ssh/remote exec)       | covered  |
| RERR_CRASHED      | yes     | no (no sibling reaper path) | partial  |
| RERR_TERMINATED   | yes     | yes (signal/child wait)     | covered  |
| RERR_SIGNAL1      | yes     | no (SIGUSR1 not handled)    | missing  |
| RERR_SIGNAL       | yes     | yes (SIGINT/TERM/HUP)       | covered  |
| RERR_WAITCHILD    | yes     | yes (ssh/r2r fallback)      | covered  |
| RERR_MALLOC       | yes     | no (Rust panics on alloc)   | partial  |
| RERR_PARTIAL      | yes     | yes (transfer summary)      | covered  |
| RERR_VANISHED     | yes     | yes (sender file gone)      | covered  |
| RERR_DEL_LIMIT    | yes     | yes (engine delete cap)     | covered  |
| RERR_TIMEOUT      | yes     | yes (transport, io_error)   | covered  |
| RERR_CONTIMEOUT   | yes     | yes (daemon connect)        | covered  |
| RERR_CMD_FAILED   | yes     | yes (ssh exit 255 map)      | covered  |
| RERR_CMD_KILLED   | yes     | yes (ssh signal map)        | covered  |
| RERR_CMD_RUN      | yes     | yes (ssh exec map)          | covered  |
| RERR_CMD_NOTFOUND | yes     | yes (ssh not-found map)     | covered  |

Production-emission evidence (non-exhaustive):

- `crates/core/src/timeout/error.rs` and `timeout/mod.rs` -> `Timeout`,
  `ConnectionTimeout`.
- `crates/core/src/signal/{unix,stub,mod}.rs` -> `Signal`, `Terminated`.
- `crates/core/src/client/remote/ssh_transfer.rs::map_child_exit_status` ->
  `CommandFailed`, `CommandKilled`, `CommandRun`, `CommandNotFound`,
  `WaitChild`, `Ipc`, `SocketIo`.
- `crates/core/src/client/remote/remote_to_remote.rs` -> `WaitChild` on
  child-wait fallback.
- `crates/engine/src/local_copy/filter_program/program.rs` -> `Syntax`,
  `Unsupported`, `FileSelect`, `Protocol`.
- `crates/daemon/src/error.rs` -> covers symbol-name lookup and unique numeric
  error codes for every variant; emits `Syntax`, `StartClient`, `SocketIo`,
  `FileSelect`, `Unsupported`, `Ipc`, `Protocol`, `Timeout`,
  `ConnectionTimeout`.
- `crates/core/src/exit_code/codes.rs::from_io_error` -> `FileSelect`,
  `SocketIo`, `Timeout`, `StreamIo`, `Signal`, `FileIo`.

`is_fatal` and `is_partial` classifications align with upstream's behaviour
(fatal codes abort the run; partial codes still imply data moved). `Signal1`
is intentionally excluded from `is_fatal` to mirror upstream, where SIGUSR1
is a "graceful shutdown" signal sent between siblings rather than a fatal
exit.

## Top 5 missing or mismatched mappings

1. **RERR_SIGNAL1 (19) - missing emitter.** The variant exists and round-trips
   through `from_i32`, but no production path raises `SIGUSR1` to children or
   maps a received `SIGUSR1` to exit 19. Upstream uses 19 for sibling-driven
   graceful shutdown (see `cleanup.c::_exit_cleanup`). oc-rsync's
   `crates/core/src/signal/unix.rs` only handles SIGINT/SIGTERM/SIGHUP. Add
   `SIGUSR1` to the signal handler so receivers/generators that need to wind
   down quietly return 19 to peer rsyncd instances.

2. **RERR_LOG_FAILURE (6) - daemon log-append path not wired.** Upstream
   returns 6 from `logfile_open` failure in `log.c`. oc-rsync's daemon
   (`crates/daemon/src/error.rs`) lists `RERR_LOG_FAILURE` but
   `log_file_open`/`set_log_file` map I/O failures through `from_io_error`,
   which routes `PermissionDenied` to `FileSelect` (3) rather than
   `LogFileAppend` (6). Daemons started with an unreadable `log file =` will
   emit the wrong exit code. Add a dedicated branch that returns
   `LogFileAppend` when the failure originates from log-rotation/open.

3. **RERR_MESSAGEIO (13) - never emitted.** Upstream raises 13 from
   `msg_list_push` when the diagnostic queue overflows during multiplex.
   oc-rsync's multiplex layer treats overflow as `StreamIo` (12). The numeric
   distinction matters for daemon log analysers that grep for exit 13.
   Audit `crates/protocol/src/multiplex/*` and route msg-channel overflow to
   `MessageIo`.

4. **RERR_CRASHED (15) - no sibling reaper.** Upstream reaps child rsync
   processes and translates segfault/abort into 15 (`cleanup.c`). oc-rsync's
   ssh helper only distinguishes `WaitChild`, `CommandKilled`, and
   `CommandFailed`; a remote rsync that core-dumps becomes 125 or 21. Inspect
   `WaitStatus::Signaled(SIGSEGV|SIGBUS|SIGABRT)` in
   `client/remote/ssh_transfer.rs::map_child_exit_status` and produce
   `Crashed`.

5. **RERR_MALLOC (22) - Rust panics never converted.** Upstream returns 22 on
   `out_of_memory`. Rust aborts on alloc failure by default, and our panic
   hook returns whatever `std::process::exit` was last set. Wire a
   `set_alloc_error_hook` (or a `catch_unwind` path in the binary entrypoint
   `crates/core/src/lib.rs`) that returns `Malloc` (22) when the panic payload
   is an allocation failure. Until then exit-code parity for OOM is not
   guaranteed.

## Maintenance checklist

- Every new error path that surfaces to `main` must terminate with an
  `ExitCode` variant - never a raw integer.
- New variants must update `as_i32`, `from_i32`, `description`, `is_fatal`,
  `is_partial`, the daemon and client `error_code_name` maps, and this audit.
- Description strings must remain byte-identical to `log.c::rerr_names`.
- Keep the gaps at 7-9, 17-18, 26-29, and 31-34 unused so future upstream
  additions can drop in cleanly.
