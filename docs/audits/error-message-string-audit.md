# Error message string verbatim audit vs upstream rsync

Tracking: oc-rsync task #2115. Audit date: 2026-05-13.

## Scope

Compare every user-visible error, warning, and diagnostic string produced by
oc-rsync against the verbatim output of upstream rsync 3.4.1, identifying
divergences that could break scripts, monitoring tools, or confuse users
accustomed to upstream formatting. The audit covers:

- Exit-code banner strings (`rerr_names[]` in `log.c`).
- Severity prefixes (`rsync error:`, `rsync warning:`, `rsync info:`, `rsync:`).
- Role trailers (`[sender=<version>]`, `[receiver=<version>]`, etc.).
- Source location format (`at <file>(<line>)` vs `at <path>:<line>`).
- Exit-code numeric mapping (upstream `errcode.h` vs `ExitCode` enum).
- Per-file warnings (`file has vanished`, `link_stat ... failed`,
  `send_files failed to open`, `read error`, `connection unexpectedly closed`).
- `@ERROR:` daemon rejection strings.
- Errno/strerror inclusion (`rsyserr` format).
- Path quoting conventions.
- Version string in trailers.

## Methodology

All upstream citations reference files at
`target/interop/upstream-src/rsync-3.4.1/`. All oc-rsync citations reference
repository-relative paths under `crates/`.

Upstream sources examined:

- `errcode.h` - numeric `RERR_*` constants (24 defined codes).
- `log.c:80-107` - `rerr_names[]` table mapping codes to text.
- `log.c:453-473` - `rsyserr()` format: `rsync: [<role>] <msg>: <strerror> (<errno>)\n`.
- `log.c:884-910` - `log_exit()` envelope: `rsync error: <name> (code <N>) at <file>(<line>) [<role>=<version>]\n`.
- `rsync.c:823-831` - `who_am_i()` role identification.
- `cleanup.c:103-275` - `_exit_cleanup()` exit-code routing.
- `io.c:199,228,804,806,847` - I/O timeout and read/write error messages.
- `flist.c:1289,1810,2398` - file-list stat warnings.
- `sender.c:173,362` - sender file-open failures.
- `generator.c:1116,1322,1477,1794,1871` - generator I/O errors.
- `clientserver.c` - `@ERROR:` daemon rejection strings.
- `version.h` - `RSYNC_VERSION "3.4.1"`.
- `rsync.h:29` - `RSYNC_NAME "rsync"`.

oc-rsync sources examined:

- `crates/core/src/exit_code/codes.rs` - `ExitCode` enum, `description()`.
- `crates/core/src/message/strings.rs` - `EXIT_CODE_TABLE` (26 entries).
- `crates/core/src/message/severity.rs` - `Severity::prefix()`.
- `crates/core/src/message/role.rs` - `Role` enum.
- `crates/core/src/message/message_impl/mod.rs` - `Message::as_segments()` rendering.
- `crates/core/src/message/macros.rs` - `rsync_error!`, `rsync_warning!`,
  `error_location!`.
- `crates/core/src/message/source.rs` - `SourceLocation` workspace-relative paths.
- `crates/logging/src/error_format.rs` - `format_rsync_error()`,
  `format_rsync_warning()`.
- `crates/transfer/src/role_trailer.rs` - `sender()`, `receiver()`,
  `generator()`, `daemon()`.
- `crates/transfer/src/generator/file_list/walk.rs` - per-file stat errors.
- `crates/transfer/src/generator/protocol_io.rs` - sender-side file-open errors.

---

## 1. Exit-code banner table

Upstream definition: `log.c:80-107` (`rerr_names[]`). Rendered by
`log.c:903-907` inside:

```
rsync error: <name> (code <N>) at <file>(<line>) [<role>=<version>]\n
rsync warning: <name> (code <N>) at <file>(<line>) [<role>=<version>]\n
```

oc-rsync maintains two parallel tables for exit-code descriptions:

- `crates/core/src/exit_code/codes.rs:147-176` - `ExitCode::description()`,
  used by `Display for ExitCode` and by `ErrorCodification::error_code_name()`.
- `crates/core/src/message/strings.rs:89-132` - `EXIT_CODE_TABLE`, used by
  `Message::from_exit_code()` and `rsync_exit_code!()` to produce rendered
  diagnostic messages.

The `strings.rs` table is the one that appears in user-visible stderr output.
The `codes.rs` table is primarily internal but surfaces through `Display` and
some error paths.

### Banner string comparison

| Code | Upstream (`rerr_names[]`) | `codes.rs` (`description()`) | `strings.rs` (`EXIT_CODE_TABLE`) | Parity |
|------|---------------------------|------------------------------|----------------------------------|--------|
| 1  | `syntax or usage error` | `syntax or usage error` | `syntax or usage error` | Match |
| 2  | `protocol incompatibility` | `protocol incompatibility` | `protocol incompatibility` | Match |
| 3  | `errors selecting input/output files, dirs` | `errors selecting input/output files, dirs` | `errors selecting input/output files, dirs` | Match |
| 4  | `requested action not supported` | `requested action not supported` | `requested action not supported` | Match |
| 5  | `error starting client-server protocol` | `error starting client-server protocol` | `error starting client-server protocol` | Match |
| 6  | (none in upstream `rerr_names[]`) | `daemon unable to append to log-file` | `daemon unable to append to log-file` | Extra |
| 10 | `error in socket IO` | `error in socket IO` | `error in socket IO` | Match |
| 11 | `error in file IO` | `error in file IO` | `error in file IO` | Match |
| 12 | `error in rsync protocol data stream` | `error in rsync protocol data stream` | `error in rsync protocol data stream` | Match |
| 13 | `errors with program diagnostics` | `errors with program diagnostics` | `errors with program diagnostics` | Match |
| 14 | `error in IPC code` | `error in IPC code` | `error in IPC code` | Match |
| 15 | `sibling process crashed` | `received SIGSEGV or SIGBUS or SIGABRT` | `sibling process crashed` | **Drift** in `codes.rs` |
| 16 | `sibling process terminated abnormally` | `received SIGINT, SIGTERM, or SIGHUP` | `sibling process terminated abnormally` | **Drift** in `codes.rs` |
| 19 | `received SIGUSR1` | `received SIGUSR1` | `received SIGUSR1` | Match |
| 20 | `received SIGINT, SIGTERM, or SIGHUP` | `received SIGINT, SIGTERM, or SIGHUP` | `received SIGINT, SIGTERM, or SIGHUP` | Match |
| 21 | `waitpid() failed` | `waitpid() failed` | `waitpid() failed` | Match |
| 22 | `error allocating core memory buffers` | `error allocating core memory buffers` | `error allocating core memory buffers` | Match |
| 23 | `some files/attrs were not transferred (see previous errors)` | `partial transfer` | `some files/attrs were not transferred (see previous errors)` | **Drift** in `codes.rs` |
| 24 | `some files vanished before they could be transferred` | `some files vanished before they could be transferred` | `some files vanished before they could be transferred` | Match |
| 25 | `the --max-delete limit stopped deletions` | `max delete limit stopped deletions` | `the --max-delete limit stopped deletions` | **Drift** in `codes.rs` |
| 30 | `timeout in data send/receive` | `timeout in data send/receive` | `timeout in data send/receive` | Match |
| 35 | `timeout waiting for daemon connection` | `timeout waiting for daemon connection` | `timeout waiting for daemon connection` | Match |
| 124 | `remote shell failed` | `remote command failed` | `remote shell failed` | **Drift** in `codes.rs` |
| 125 | `remote shell killed` | `remote command killed` | `remote shell killed` | **Drift** in `codes.rs` |
| 126 | `remote command could not be run` | `remote command could not be run` | `remote command could not be run` | Match |
| 127 | `remote command not found` | `remote command not found` | `remote command not found` | Match |

### Banner divergence summary

- `codes.rs` diverges from upstream for **6 codes** (15, 16, 23, 25, 124, 125).
- `strings.rs` matches upstream verbatim for all codes.
- Code 6 exists only in oc-rsync and is an intentional extension for daemon
  log-file failures. Upstream uses the numeric slot but has no `rerr_names[]`
  entry. This is **acceptable as Extra**.
- The two internal tables (`codes.rs` vs `strings.rs`) disagree for codes 15,
  16, 23, 25, 124, 125. This creates confusion because `Display for ExitCode`
  produces different text than `Message::from_exit_code()`.

---

## 2. Severity prefix format

### Upstream format

Upstream uses `RSYNC_NAME` (defined as `"rsync"` in `rsync.h:29`) as the
program name prefix. There are two distinct prefix patterns:

1. **Exit banner** (via `log_exit()` in `log.c:903-907`):
   - Error: `rsync error: <text> (code <N>) at <file>(<line>) [<role>=<version>]\n`
   - Warning: `rsync warning: <text> (code <N>) at <file>(<line>) [<role>=<version>]\n`

2. **Per-file I/O messages** (via `rsyserr()` in `log.c:453-473`):
   - `rsync: [<role>] <text>: <strerror> (<errno>)\n`

3. **Info-level prefix**: upstream does not use `rsync info:`. Info messages
   go through `rprintf(FINFO, ...)` without a program-name prefix.

### oc-rsync format

oc-rsync uses `Severity::prefix()` (`crates/core/src/message/severity.rs:64-69`):
- `rsync info: ` (note trailing space)
- `rsync warning: `
- `rsync error: `

The `Brand::Upstream` variant always uses `"rsync"` as the program name. The
`Brand::Oc` variant uses `"oc-rsync"` for client messages and the daemon
program name for server/daemon messages.

### Parity assessment

| Pattern | Upstream | oc-rsync | Parity |
|---------|----------|----------|--------|
| Error banner prefix | `rsync error: ` | `rsync error: ` | **Match** |
| Warning banner prefix | `rsync warning: ` | `rsync warning: ` | **Match** |
| Info prefix | (none - bare text) | `rsync info: ` | **Drift** |
| Per-file I/O prefix | `rsync: [<role>] ` | `rsync: ` (no role bracket) | **Drift** |

**Finding D-1**: oc-rsync adds `rsync info:` as a prefix for informational
messages. Upstream rsync does not prefix info messages with `rsync info:` -
they are bare text. This is unlikely to break scripts since scripts typically
grep for `rsync error:` or `rsync warning:`.

**Finding D-2**: Per-file I/O errors use `rsyserr()` in upstream, which
formats as `rsync: [<role>] <msg>: <strerror> (<errno>)\n`. oc-rsync formats
these differently - see section 5.

---

## 3. Source location format

### Upstream format

Upstream uses `src_file()` (from `util2.c`) to strip the directory prefix,
keeping only the C source basename. The location format is:

```
at <basename>(<line>)
```

Example: `at io.c(234)`, `at main.c(1337)`.

### oc-rsync format

oc-rsync has **two** source location systems:

1. **`Message::as_segments()`** (in `crates/core/src/message/message_impl/mod.rs:107-113`):
   Uses `SourceLocation` with workspace-relative paths and a colon separator:
   ```
   at <workspace-relative-path>:<line>
   ```
   Example: `at crates/core/src/message/source.rs:42`.

2. **`error_location!()`** (in `crates/core/src/message/macros.rs:23-31` and
   `crates/transfer/src/role_trailer.rs:33-41`):
   Uses `file_basename()` with parentheses, matching upstream:
   ```
   at <basename>(<line>)
   ```
   Example: `at walk.rs(327)`.

3. **`format_rsync_error()`** (in `crates/logging/src/error_format.rs:74-76`):
   Uses `strip_repo_prefix()` to produce crate-relative paths with colon:
   ```
   at <crate-relative-path>:<line>
   ```
   Example: `at logging/src/error_format.rs:75`.

### Parity assessment

| Formatting path | Format | Upstream match? |
|-----------------|--------|-----------------|
| `Message::as_segments()` | `at <workspace-relative>:<line>` | **Drift** - colon instead of parentheses, full path instead of basename |
| `error_location!()` | `at <basename>(<line>)` | **Match** |
| `format_rsync_error()` | `at <crate-relative>:<line>` | **Drift** - colon instead of parentheses, relative path instead of basename |

**Finding D-3**: The `Message` rendering system (which produces the exit-code
banner messages) uses `:` between file and line, while upstream uses
`(<line>)`. This divergence appears in every `rsync error:` and
`rsync warning:` exit banner message.

**Finding D-4**: The `Message` rendering system includes full workspace-relative
paths (e.g. `crates/core/src/message/source.rs`) whereas upstream shows only
the C source basename (e.g. `main.c`). This is more verbose but not harmful.
Some scripts may parse the file portion.

**Finding D-5**: `error_location!()` correctly matches upstream's `at <basename>(<line>)`
format. This is used in per-file `eprintln!` warnings in the transfer crate
but not in the `Message` rendering system.

---

## 4. Role trailers

### Upstream roles

`rsync.c:823-831` (`who_am_i()`):

```c
const char *who_am_i(void)
{
    if (am_starting_up)
        return am_server ? "server" : "client";
    return am_sender ? "sender"
         : am_generator ? "generator"
         : am_receiver ? "receiver"
         : "Receiver"; /* pre-forked receiver */
}
```

Upstream uses the trailer format `[<role>=<version>]` in exit banners
(`log.c:903-907`) and `rsyserr()` uses `[<role>]` without version
(`log.c:459`). Per-file warnings (e.g. `file has vanished: %s\n`) do **not**
carry any trailer.

### oc-rsync roles

`crates/core/src/message/role.rs:43-50` defines six roles: `sender`,
`receiver`, `generator`, `server`, `client`, `daemon`.

The trailer format `[<role>=<version>]` is produced in two places:

1. `Message::as_segments()` uses `Role::as_str()` + `VERSION_SUFFIX`.
2. `crates/transfer/src/role_trailer.rs:46-67` uses `CARGO_PKG_VERSION`.

### Parity assessment

| Role string | Upstream | oc-rsync | Parity |
|-------------|----------|----------|--------|
| `sender` | `sender` | `sender` | **Match** |
| `receiver` | `receiver` | `receiver` | **Match** |
| `generator` | `generator` | `generator` | **Match** |
| `server` | `server` | `server` | **Match** |
| `client` | `client` | `client` | **Match** |
| `daemon` | (not in `who_am_i()`) | `daemon` | **Extra** (acceptable) |
| `Receiver` (pre-fork) | `Receiver` (capitalized) | (not emitted) | n/a |

**Finding D-6**: oc-rsync attaches role trailers to per-file warnings where
upstream does not. Upstream emits role trailers only in the `rsync error:` /
`rsync warning:` envelope and in `rsyserr()` messages. Per-file warnings like
`file has vanished: <path>` have no trailer in upstream. oc-rsync appends
`[sender=<version>]` or `[generator=<version>]` to these.

---

## 5. rsyserr() format - I/O error messages with errno

### Upstream format

`rsyserr()` (`log.c:453-473`) formats I/O-level errors as:

```
rsync: [<role>] <msg>: <strerror> (<errno>)\n
```

Example: `rsync: [sender] send_files failed to open /test/file: Permission denied (13)`.

Note the prefix is `rsync:` (not `rsync error:`), followed by `[<role>]` in
brackets, then the message, then `: <strerror> (<errno>)`.

### oc-rsync format

oc-rsync does not have a direct `rsyserr()` equivalent. Instead, per-file
I/O errors are formatted inline with `eprintln!` calls in the transfer crate:

| Message | Upstream (via `rsyserr`) | oc-rsync (`eprintln!`) | Parity |
|---------|--------------------------|------------------------|--------|
| link_stat failure | `rsync: [sender] link_stat %s failed: <strerror> (<errno>)` | `rsync: link_stat "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<version>]` | **Drift** |
| send_files open | `rsync: [sender] send_files failed to open %s: <strerror> (<errno>)` | `rsync: send_files failed to open "<path>": <e> (<errno>) at <basename>(<line>) [generator=<version>]` | **Drift** |
| opendir failure | `rsync: [sender] opendir %s failed: <strerror> (<errno>)` | `rsync: opendir "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<version>]` | **Drift** |
| readdir failure | `rsync: [sender] readdir(%s): <strerror> (<errno>)` | `rsync: readdir "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<version>]` | **Drift** |
| make_file failure | (not a direct rsyserr; just stat failure) | `rsync: make_file failed for "<path>": <e> (<errno>) at <basename>(<line>) [sender=<version>]` | **Drift** |

### Specific divergences

**Finding D-7**: oc-rsync prefixes per-file I/O errors with `rsync:` but
omits the `[<role>]` bracket that upstream places immediately after `rsync:`.
Instead, oc-rsync appends the role trailer at the end of the line. The
upstream pattern is:

```
rsync: [sender] link_stat /path failed: No such file or directory (2)
```

oc-rsync produces:

```
rsync: link_stat "/path" failed: No such file or directory (2) at walk.rs(327) [sender=0.5.8]
```

The differences are:
1. Missing `[sender]` bracket after `rsync:` prefix.
2. Path is quoted with `"` (upstream prints raw).
3. Appended `at <basename>(<line>)` (upstream does not include source location
   for `rsyserr()` messages).
4. Appended `[sender=<version>]` trailer (upstream does not include it for
   `rsyserr()` messages).

**Finding D-8**: The `strerror()` representation differs. Upstream C uses
`strerror(errno)` which returns the platform C library message. Rust's
`std::io::Error` `Display` implementation produces similar but not necessarily
identical text (e.g. `Permission denied` in both, but edge cases may differ).
The errno number format `(<errno>)` is produced identically by both via
`raw_os_error().unwrap_or(0)`.

---

## 6. Per-file warning messages

### file has vanished

| Site | Upstream | oc-rsync | Parity |
|------|----------|----------|--------|
| sender open | `file has vanished: %s\n` (`sender.c:358`) | `file has vanished: {path} at <basename>(<line>) [generator=<version>]` (`protocol_io.rs:134`) | **Drift** - extra location and role trailer |
| flist stat | `file has vanished: %s\n` (`flist.c:1289`) | `file has vanished: {path} at <basename>(<line>) [generator=<version>]` (`walk.rs:319`) | **Drift** - extra location and role trailer |
| client error | (same) | `file has vanished: '{path}'` (`client/error.rs:246`) | **Drift** - single quotes around path |

**Finding D-9**: The `file has vanished` warning is bare text in upstream
with no location or role suffix. oc-rsync appends source location and
role trailer in the transfer crate, and uses single quotes around the path
in the client error module.

### connection unexpectedly closed

Upstream (`io.c:228`):
```c
rprintf(FERROR, RSYNC_NAME ": connection unexpectedly closed "
        "(%s bytes received so far) [%s]\n",
        big_num(stats.total_read), who_am_i());
```

oc-rsync: No equivalent string emitted. A comment at
`crates/daemon/src/daemon/sections/module_access/transfer.rs:521`
acknowledges this gap.

**Finding D-10**: oc-rsync does not emit the `connection unexpectedly closed`
message that scripts commonly grep for when diagnosing network failures.

### read error

Upstream (`io.c:804,806`): `rsyserr(FERROR, errno, "read error")` -
rendered as `rsync: [<role>] read error: <strerror> (<errno>)\n`.

oc-rsync uses `"network read error: {e}"` at
`crates/transfer/src/transfer_ops/token_loop.rs:103,137`.

**Finding D-11**: oc-rsync qualifies read errors with `network` - upstream
uses bare `read error`. The `rsync:` prefix and `[role]` bracket are also
missing from oc-rsync's variant.

### skipping non-regular file

Upstream (`generator.c:1687`): `rprintf(FERROR_XFER, "skipping non-regular file \"%s\"\n", ...)`.

oc-rsync: `"skipping non-regular file \"{}\""` at
`crates/cli/src/frontend/progress/render.rs:396`.

**Parity: Match.**

---

## 7. Exit-code numeric mapping

Upstream `errcode.h` defines 24 exit codes. oc-rsync defines 27 (the same 24
plus codes 6, 15, 16 - though 15 and 16 exist in upstream too with different
semantics in `codes.rs`).

| Code | Upstream constant | oc-rsync `ExitCode` variant | Numeric match |
|------|-------------------|-----------------------------|---------------|
| 0 | `RERR_OK` | `Ok` | **Match** |
| 1 | `RERR_SYNTAX` | `Syntax` | **Match** |
| 2 | `RERR_PROTOCOL` | `Protocol` | **Match** |
| 3 | `RERR_FILESELECT` | `FileSelect` | **Match** |
| 4 | `RERR_UNSUPPORTED` | `Unsupported` | **Match** |
| 5 | `RERR_STARTCLIENT` | `StartClient` | **Match** |
| 6 | (not in `errcode.h`) | `LogFileAppend` | **Extra** |
| 10 | `RERR_SOCKETIO` | `SocketIo` | **Match** |
| 11 | `RERR_FILEIO` | `FileIo` | **Match** |
| 12 | `RERR_STREAMIO` | `StreamIo` | **Match** |
| 13 | `RERR_MESSAGEIO` | `MessageIo` | **Match** |
| 14 | `RERR_IPC` | `Ipc` | **Match** |
| 15 | `RERR_CRASHED` | `Crashed` | **Match** |
| 16 | `RERR_TERMINATED` | `Terminated` | **Match** |
| 19 | `RERR_SIGNAL1` | `Signal1` | **Match** |
| 20 | `RERR_SIGNAL` | `Signal` | **Match** |
| 21 | `RERR_WAITCHILD` | `WaitChild` | **Match** |
| 22 | `RERR_MALLOC` | `Malloc` | **Match** |
| 23 | `RERR_PARTIAL` | `PartialTransfer` | **Match** |
| 24 | `RERR_VANISHED` | `Vanished` | **Match** |
| 25 | `RERR_DEL_LIMIT` | `DeleteLimit` | **Match** |
| 30 | `RERR_TIMEOUT` | `Timeout` | **Match** |
| 35 | `RERR_CONTIMEOUT` | `ConnectionTimeout` | **Match** |
| 124 | `RERR_CMD_FAILED` | `CommandFailed` | **Match** |
| 125 | `RERR_CMD_KILLED` | `CommandKilled` | **Match** |
| 126 | `RERR_CMD_RUN` | `CommandRun` | **Match** |
| 127 | `RERR_CMD_NOTFOUND` | `CommandNotFound` | **Match** |

All numeric values match. Code 6 is an oc-rsync extension.

---

## 8. Severity classification

Upstream treats exit code 24 as a warning (`FWARNING`) and all other non-zero
codes as errors (`FERROR`). This is implemented in `log.c:901-907`:

```c
if (code == RERR_VANISHED) {
    rprintf(FWARNING, "rsync warning: ...");
} else {
    rprintf(FERROR, "rsync error: ...");
}
```

oc-rsync mirrors this exactly: `EXIT_CODE_TABLE` in `strings.rs:117-119`
assigns `Severity::Warning` only to code 24. All other entries use
`Severity::Error`. The test `only_exit_code_twenty_four_is_a_warning` in
`strings.rs:387-400` enforces this invariant.

**Parity: Match.**

---

## 9. Version string in trailers

### Upstream

Upstream uses `rsync_version()` which returns `RSYNC_VERSION` (`"3.4.1"`).
The trailer format is `[<role>=<version>]`, e.g. `[sender=3.4.1]`.

### oc-rsync

oc-rsync uses `VERSION_SUFFIX` (set to `crate::version::RUST_VERSION` which
is the workspace package version, e.g. `0.5.8`). The trailer format is
identical: `[<role>=<version>]`, e.g. `[sender=0.5.8]`.

The `role_trailer.rs` module uses `CARGO_PKG_VERSION` which should match.

**Finding D-12**: The version number in trailers is oc-rsync's own version
(e.g. `0.5.8`) rather than upstream's `3.4.1`. This is intentional - the
version identifies the binary producing the output. No parity issue.

---

## 10. @ERROR: daemon strings

These strings are wire-visible. Upstream clients parse them to classify
rejection reasons.

| Upstream verbatim | Upstream source | oc-rsync equivalent | Parity |
|-------------------|-----------------|---------------------|--------|
| `@ERROR: Unknown module '%s'\n` | `clientserver.c:730` | `@ERROR: Unknown module '{module}'` | **Match** |
| `@ERROR: access denied to %s from %s (%s)\n` | `clientserver.c:733-734` | `@ERROR: access denied to {module} from {host} ({addr})` | **Match** |
| `@ERROR: max connections (%d) reached -- try again later\n` | `clientserver.c:752` | `@ERROR: max connections ({limit}) reached -- try again later` | **Match** |
| `@ERROR: auth failed on module %s\n` | `clientserver.c:762` | `@ERROR: auth failed on module {module}` | **Match** |
| `@ERROR: failed to open lock file\n` | `clientserver.c:748` | `@ERROR: failed to open lock file` | **Match** |
| `@ERROR: protocol startup error\n` | `clientserver.c:182` | (none) | **Missing** |
| `@ERROR: chdir failed\n` | `clientserver.c:647` | (none) | **Missing** |
| `@ERROR: Unknown command '%s'\n` | `clientserver.c:1379` | (none) | **Missing** |
| `@ERROR: invalid uid %s\n` | `clientserver.c:783` | (none) | **Missing** |
| `@ERROR: invalid gid %s\n` | `clientserver.c:656` | (none) | **Missing** |
| `@ERROR: invalid gid setting.\n` | `clientserver.c:802,811` | (none) | **Missing** |
| `@ERROR: no path setting.\n` | `clientserver.c:826` | (none) | **Missing** |
| `@ERROR: getpwuid failed\n` | `clientserver.c:682` | (none) | **Missing** |
| `@ERROR: setgid failed\n` | `clientserver.c:1010` | (none) | **Missing** |
| `@ERROR: setuid failed\n` | `clientserver.c:1039` | (none) | **Missing** |
| `@ERROR: fork failed\n` | `clientserver.c:910` | (none) | **Missing** (intentional - no fork) |
| `@ERROR: chroot failed\n` | `clientserver.c:981` | (none) | **Missing** (intentional - no chroot) |
| `@ERROR: early_input length\n` | `clientserver.c:1360` | (none) | **Missing** (intentional - not implemented) |
| (none) | n/a | `@ERROR: daemon functionality is unavailable in this build` | **Extra** |

**Finding D-13**: The four most-parsed `@ERROR:` strings (Unknown module,
access denied, max connections, auth failed) match upstream verbatim. The
lock-file error uses different wording. Remaining strings are missing because
the underlying daemon features (chroot, fork, uid/gid switching) are not yet
implemented.

---

## 11. Path quoting

### Upstream

Upstream passes paths to `rsyserr()` and `rprintf()` without quoting. The
path is printed raw via `%s` substitution:

```c
rsyserr(FERROR_XFER, errno, "send_files failed to open %s", fname);
```

Produces: `rsync: [sender] send_files failed to open /test/file: Permission denied (13)`.

For some messages, upstream uses explicit `\"` quoting:

```c
rprintf(FERROR_XFER, "skipping non-regular file \"%s\"\n", fname);
```

### oc-rsync

oc-rsync inconsistently applies path quoting:

| Message | Upstream quoting | oc-rsync quoting | Parity |
|---------|-----------------|------------------|--------|
| `send_files failed to open` | raw `%s` | `\"<path>\"` (double-quoted) | **Drift** |
| `link_stat ... failed` | raw `%s` | `\"<path>\"` (double-quoted) | **Drift** |
| `opendir ... failed` | raw `%s` | `\"<path>\"` (double-quoted) | **Drift** |
| `readdir(...)` | `%s` inside parens | `\"<path>\"` (double-quoted) | **Drift** |
| `file has vanished` | raw `%s` | raw `{path}` | **Match** |
| `skipping non-regular file` | `\"%s\"` (double-quoted) | `\"{}\"` (double-quoted) | **Match** |

**Finding D-14**: oc-rsync double-quotes paths in `rsyserr`-equivalent
messages where upstream uses raw paths. This is a minor aesthetic difference
that is unlikely to break scripts, but it creates noise in byte-level
comparisons.

---

## 12. Missing upstream messages

The following upstream messages have no oc-rsync equivalent. They are listed
in order of user-visible importance.

| Upstream message | Source | Impact |
|------------------|--------|--------|
| `connection unexpectedly closed (%s bytes received so far) [%s]` | `io.c:228` | High - scripts grep for this |
| `[%s] io timeout after %d seconds -- exiting` | `io.c:199-200` | Medium - timeout diagnostics |
| `change_dir#1 %s failed` (and #2, #3, #4) | `main.c:749,807,827,936,1161` | Medium - directory errors |
| `change_dir %s failed` | `flist.c:369,2242` | Medium |
| `read errors mapping %s` | `sender.c:437` | Low |
| `readlink_stat(%s) failed` | `flist.c:1294` | Low |
| `recv_generator: mkdir %s failed` | `generator.c:1323,1477` | Low |
| `failed to open %s, continuing` | `generator.c:1871` | Low |
| `unexpected tag %d [%s%s]` | `io.c:1703` | Low - protocol debug |
| `ERROR: cannot stat destination %s` | `main.c:770` | Low |

---

## Summary of findings

### Divergence inventory

| ID | Category | Severity | Description |
|----|----------|----------|-------------|
| D-1 | Prefix | Low | `rsync info:` prefix added where upstream has no prefix |
| D-2 | Prefix | Medium | `rsyserr()` format `rsync: [role]` not replicated |
| D-3 | Location | High | Colon separator (`:`) instead of parentheses in exit banners |
| D-4 | Location | Low | Full workspace-relative path instead of basename |
| D-5 | Location | n/a | `error_location!()` correctly matches upstream (positive) |
| D-6 | Trailer | Medium | Role trailers attached to per-file warnings where upstream omits them |
| D-7 | rsyserr | Medium | Missing `[role]` bracket after `rsync:` prefix; trailer at end instead |
| D-8 | rsyserr | Low | Rust `Display` vs C `strerror()` - minor text differences possible |
| D-9 | Warning | Medium | `file has vanished` carries extra location/trailer |
| D-10 | Missing | High | `connection unexpectedly closed` not emitted |
| D-11 | Warning | Medium | `network read error` instead of `read error` |
| D-12 | Version | n/a | Intentional - own version in trailers |
| D-13 | Daemon | Low | Lock-file `@ERROR:` wording differs; other strings missing (features unimplemented) |
| D-14 | Quoting | Low | Paths double-quoted where upstream uses raw |

### Internal table divergence

The `ExitCode::description()` table in `codes.rs` diverges from both upstream
and from oc-rsync's own `EXIT_CODE_TABLE` in `strings.rs` for 6 codes:

| Code | `codes.rs` says | Upstream/`strings.rs` say |
|------|-----------------|---------------------------|
| 15 | `received SIGSEGV or SIGBUS or SIGABRT` | `sibling process crashed` |
| 16 | `received SIGINT, SIGTERM, or SIGHUP` | `sibling process terminated abnormally` |
| 23 | `partial transfer` | `some files/attrs were not transferred (see previous errors)` |
| 25 | `max delete limit stopped deletions` | `the --max-delete limit stopped deletions` |
| 124 | `remote command failed` | `remote shell failed` |
| 125 | `remote command killed` | `remote shell killed` |

---

## Recommendations

Sorted by user-visible impact. None requires wire protocol changes.

### P0 - High impact

1. **Fix the source location separator.** Change `Message::as_segments()` in
   `crates/core/src/message/message_impl/mod.rs:108-113` to use `(<line>)`
   instead of `:<line>`. This affects every `rsync error:` and
   `rsync warning:` exit banner. Upstream format: `at main.c(1337)`. Current
   oc-rsync format: `at crates/core/src/message/source.rs:42`. Also update
   `crates/logging/src/error_format.rs:75,99` to match.

2. **Reconcile `ExitCode::description()` with upstream.** Update
   `crates/core/src/exit_code/codes.rs:147-176` to match the upstream
   wording for codes 15, 16, 23, 25, 124, 125. This eliminates the internal
   table divergence and aligns `Display for ExitCode` with
   `Message::from_exit_code()`.

3. **Emit `connection unexpectedly closed` on unexpected EOF.** Add the
   upstream format string at the appropriate I/O drain site so scripts that
   grep for this message continue to work.

### P1 - Medium impact

4. **Remove extra location/trailer from per-file warnings.** Strip the
   `error_location!()` and role trailer from `file has vanished`,
   `link_stat ... failed`, `send_files failed to open`, `opendir ... failed`,
   and `readdir ... failed` messages. Upstream emits these as bare text
   through `rsyserr()` without source location or role trailer suffixes.

5. **Implement `rsyserr()` format.** Create a helper that formats I/O-level
   errors as `rsync: [<role>] <msg>: <strerror> (<errno>)\n` to match
   upstream's `rsyserr()`. This replaces the current `eprintln!`-based
   formatting.

6. **Use bare `read error` wording.** Change
   `crates/transfer/src/transfer_ops/token_loop.rs:103,137` from
   `network read error` to `read error`.

7. **Match upstream path quoting.** Remove double-quotes from paths in
   `rsyserr`-equivalent messages where upstream uses raw paths.

### P2 - Low impact

8. **Use upstream `@ERROR: failed to open lock file` verbatim.** Edit
   `crates/daemon/src/daemon.rs` to match the upstream string.

9. **Add missing `@ERROR:` strings as daemon features land.** The missing
   `@ERROR:` entries should use upstream wording verbatim when the
   corresponding code paths are implemented.

10. **Add a cross-table parity test.** Create a test that verifies
    `ExitCode::description()` and `EXIT_CODE_TABLE` produce identical text
    for all shared codes, preventing future drift.

---

## Test coverage

The existing test file `crates/core/src/message/tests/part8.rs` contains
comprehensive format-parity tests including:

- Prefix format verification (error, warning, info).
- Code suffix format (`(code N)`).
- Role trailer format (`[role=version]`).
- Segment ordering (prefix, text, code, source, trailer).
- Full `EXIT_CODE_TABLE` text comparison against upstream `rerr_names[]`.
- Severity classification (only code 24 is warning).
- Detail-appended message format.

The test `exit_code_table_text_matches_upstream_rerr_names` (`part8.rs:259-306`)
is the primary guard against `strings.rs` drift. No equivalent test exists
for `codes.rs::description()`.

---

## Conclusion

oc-rsync achieves strong parity for the most critical user-visible strings:
the `@ERROR:` daemon family, the `rerr_names[]` exit-code banners (via
`strings.rs`), severity classification, role names, and exit-code numeric
values. The wire protocol is not affected by any of the identified
divergences.

The most impactful divergences are the source-location separator format
(colon vs parentheses in exit banners), the missing `connection unexpectedly
closed` message, and the extra source location / role trailers on per-file
warnings. Fixing these three categories would bring oc-rsync's diagnostic
output to near-verbatim parity with upstream rsync 3.4.1.

Total distinct divergences: **14** findings (D-1 through D-14), of which
D-5 and D-12 are non-issues (positive match and intentional difference
respectively), leaving **12 actionable items**.
