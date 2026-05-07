# Error Message Strings Verbatim Audit

Tracking: oc-rsync task #2115.

## Summary

This audit compares the most user-visible error message strings emitted by
oc-rsync against the verbatim strings produced by upstream rsync 3.4.1
(`target/interop/upstream-src/rsync-3.4.1`). The audit covers 38 strings drawn
from the heavily-grepped categories listed in the tracking issue: the 26
`RERR_*` exit-code banners, the family of `@ERROR:` daemon responses, the
`change_dir` / `failed to open` / `read error` / `connection unexpectedly
closed` / `file has vanished` / `some files vanished` / `skipping non-regular
file` and `link_stat` warnings, and the surrounding `rsync error:` /
`rsync warning:` envelope.

Wire-visible parity is good for the family of `@ERROR:` strings, the per-file
warnings produced by the sender / generator, and the multiplexed message
contents. The largest divergences are concentrated in three places:

1. The `ExitCode::description()` table in `crates/core/src/exit_code/codes.rs`
   uses non-upstream wording for six exit codes (15, 16, 23, 25, 124, 125).
   Note that `crates/core/src/message/strings.rs` carries the upstream wording
   for the same codes, so the project actually carries two diverging tables.
2. The `rsync error: ... at <file>:<line>` envelope produced by
   `crates/logging/src/error_format.rs` uses a colon between file and line,
   while upstream uses parentheses (`at <file>(<line>)`). All formatted error
   lines share this divergence.
3. Several per-file warnings prepend an `rsync:` literal and append an
   `error_location!()` plus role trailer that upstream does not emit
   (`file has vanished`, `link_stat`, `send_files failed to open`,
   `client error`).

A full table of recommendations appears in the [Recommendations](#recommendations)
section. The total count of distinct verbatim divergences identified by this
audit is **24**. None of the divergences is a wire-protocol break - they only
affect human-readable diagnostics on stderr and the daemon `@ERROR:` lines
already match upstream byte-for-byte where it counts (module name, host, addr,
auth module).

## How to read this document

* "Upstream" cites the verbatim format string from
  `target/interop/upstream-src/rsync-3.4.1/<file>:<line>`. C `%s`/`%d`
  placeholders are kept verbatim. Trailing `\n` is shown explicitly.
* "Ours" cites the equivalent Rust format literal from `crates/`. Rust
  `{name}` / `{}` placeholders are kept verbatim.
* "Parity" is one of:
  * **Match** - byte-for-byte equivalent once placeholders are filled in.
  * **Drift** - same intent but the wording, punctuation, ordering, or
    surrounding decoration differs.
  * **Missing** - the upstream string has no equivalent in oc-rsync.
  * **Extra** - oc-rsync emits a string that upstream does not.

## RERR exit-code banner table

Upstream definition: `target/interop/upstream-src/rsync-3.4.1/log.c:80-107`
(`rerr_names[]`). Rendered by `log.c:903-907` inside the
`rsync error: <name> (code <N>) at <file>(<line>) [<role>=<version>]\n`
envelope.

Our codebase carries the same banners in two places:

* `crates/core/src/exit_code/codes.rs:147-176` (`ExitCode::description()`),
* `crates/core/src/message/strings.rs:89-132` (`EXIT_CODE_TABLE`).

These two tables disagree with each other for codes 15, 23, 25. The
`message/strings.rs` table tracks upstream more closely. The
`exit_code/codes.rs` table is what `Display for ExitCode` returns and what is
surfaced in unit tests under `crates/core/tests/exit_code_comprehensive.rs`.

| Code | Upstream verbatim (`log.c:80-107`) | `codes.rs` (`description()`) | `strings.rs` (`EXIT_CODE_TABLE`) | Parity |
|------|------------------------------------|------------------------------|----------------------------------|--------|
| 1    | `syntax or usage error`            | `syntax or usage error`      | `syntax or usage error`          | Match  |
| 2    | `protocol incompatibility`         | `protocol incompatibility`   | `protocol incompatibility`       | Match  |
| 3    | `errors selecting input/output files, dirs` | `errors selecting input/output files, dirs` | `errors selecting input/output files, dirs` | Match |
| 4    | `requested action not supported`   | `requested action not supported` | `requested action not supported` | Match  |
| 5    | `error starting client-server protocol` | `error starting client-server protocol` | `error starting client-server protocol` | Match |
| 6    | (none in upstream)                 | `daemon unable to append to log-file` | `daemon unable to append to log-file` | Extra  |
| 10   | `error in socket IO`               | `error in socket IO`         | `error in socket IO`             | Match  |
| 11   | `error in file IO`                 | `error in file IO`           | `error in file IO`               | Match  |
| 12   | `error in rsync protocol data stream` | `error in rsync protocol data stream` | `error in rsync protocol data stream` | Match |
| 13   | `errors with program diagnostics`  | `errors with program diagnostics` | `errors with program diagnostics` | Match |
| 14   | `error in IPC code`                | `error in IPC code`          | `error in IPC code`              | Match  |
| 15   | `sibling process crashed`          | `received SIGSEGV or SIGBUS or SIGABRT` | `sibling process crashed` | **Drift** in `codes.rs` |
| 16   | `sibling process terminated abnormally` | `received SIGINT, SIGTERM, or SIGHUP` | `sibling process terminated abnormally` | **Drift** in `codes.rs` |
| 19   | `received SIGUSR1`                 | `received SIGUSR1`           | `received SIGUSR1`               | Match  |
| 20   | `received SIGINT, SIGTERM, or SIGHUP` | `received SIGINT, SIGTERM, or SIGHUP` | `received SIGINT, SIGTERM, or SIGHUP` | Match |
| 21   | `waitpid() failed`                 | `waitpid() failed`           | `waitpid() failed`               | Match  |
| 22   | `error allocating core memory buffers` | `error allocating core memory buffers` | `error allocating core memory buffers` | Match |
| 23   | `some files/attrs were not transferred (see previous errors)` | `partial transfer` | `some files/attrs were not transferred (see previous errors)` | **Drift** in `codes.rs` |
| 24   | `some files vanished before they could be transferred` | `some files vanished before they could be transferred` | `some files vanished before they could be transferred` | Match |
| 25   | `the --max-delete limit stopped deletions` | `max delete limit stopped deletions` | `the --max-delete limit stopped deletions` | **Drift** in `codes.rs` |
| 30   | `timeout in data send/receive`     | `timeout in data send/receive` | `timeout in data send/receive` | Match  |
| 35   | `timeout waiting for daemon connection` | `timeout waiting for daemon connection` | `timeout waiting for daemon connection` | Match |
| 124  | `remote shell failed`              | `remote command failed`      | `remote shell failed`            | **Drift** in `codes.rs` |
| 125  | `remote shell killed`              | `remote command killed`      | `remote shell killed`            | **Drift** in `codes.rs` |
| 126  | `remote command could not be run`  | `remote command could not be run` | `remote command could not be run` | Match  |
| 127  | `remote command not found`         | `remote command not found`   | `remote command not found`       | Match  |

`codes.rs` divergences: 6 (codes 15, 16, 23, 25, 124, 125). `strings.rs` is in
parity. Code 6 (`daemon unable to append to log-file`) has no upstream
equivalent in `rerr_names[]` and is an oc-rsync extension matching the
upstream `RERR_LOG_FAILED` numeric slot used inside `log.c` for the same
condition - acceptable as **Extra**.

## Per-message verbatim audit (non-banner messages)

| Upstream string (verbatim) | Upstream `file:line` | Our equivalent | Our `file:line` | Parity |
|----------------------------|----------------------|----------------|-----------------|--------|
| `RSYNC_NAME ": connection unexpectedly closed (%s bytes received so far) [%s]\n"` | `io.c:228` | (only referenced inside a comment, no emitted equivalent) | `crates/daemon/src/daemon/sections/module_access/transfer.rs:521` | **Missing** in our code (we exit on the same condition without ever producing this banner string) |
| `"file has vanished: %s\n"` (where `%s = full_fname(name)`) | `flist.c:1289`, `sender.c:358` | `"file has vanished: {path_display} {error_location!()}{role_trailer::generator()}"` | `crates/transfer/src/generator/protocol_io.rs:134` | **Drift** (we append `error_location!` and a role trailer that upstream does not emit) |
| `"file has vanished: %s\n"` | `flist.c:1289` | `format!("file has vanished: '{path_display}'")` (single quotes around path) | `crates/core/src/client/error.rs:246` | **Drift** (single-quoted path; upstream prints raw) |
| `"file has vanished: {} {}{}"` (walk-time stat error) | `flist.c:1289` | `eprintln!("file has vanished: {} {}{}", ...)` | `crates/transfer/src/generator/file_list/walk.rs:319` | **Drift** (extra location + role trailer) |
| `"send_files failed to open %s"` (rsyserr - prepends `rsync: ` and appends `: <strerror> (<errno>)`) | `sender.c:362-363` | `"rsync: send_files failed to open \"{path_display}\": {} ({}) {}{}"` | `crates/transfer/src/generator/protocol_io.rs:142` | **Drift** (we add `\"...\"` quoting and an extra `error_location!` + role trailer; upstream emits the path verbatim with no quoting) |
| `"send_files failed to open %s"` | `sender.c:362` | `"rsync: send_files failed to open {:?}: Permission denied (13) {}{}"` (Rust `Debug` format) | `crates/transfer/src/pipeline/receiver.rs:163,221` | **Drift** (`{:?}` produces `"path"` quoting; literal `Permission denied (13)` instead of platform `strerror`; extra location/role trailer) |
| `"read errors mapping %s"` | `sender.c:437` | (no equivalent literal) | n/a | **Missing** |
| `"read error"` (rsyserr - prepends `rsync: ` and appends `: <strerror>`) | `io.c:804,806` | `"network read error: {e}"` | `crates/transfer/src/transfer_ops/token_loop.rs:103,137` | **Drift** (we use `network read error:`; upstream uses `read error:`) |
| `"read error"` | `io.c:804,806` | `"{direction} read error: {e}"` (where `direction` is `"sender"` or `"receiver"`) | `crates/core/src/client/remote/remote_to_remote.rs:421` | **Drift** (we prepend a direction word) |
| `"change_dir#1 %s failed"` | `main.c:749` | (no equivalent literal; we surface a `failed to access destination directory` error instead) | `crates/core/src/client/error.rs:260` | **Drift** (different wording, same exit code 3) |
| `"change_dir#2 %s failed"` | `main.c:807` | (no equivalent literal) | n/a | **Missing** |
| `"change_dir#3 %s failed"` | `main.c:827, 936` | (no equivalent literal) | n/a | **Missing** |
| `"change_dir#4 %s failed"` | `main.c:1161` | (no equivalent literal) | n/a | **Missing** |
| `"change_dir %s failed"` (flist sender push) | `flist.c:369, 2242` | (no equivalent literal) | n/a | **Missing** |
| `"failed to open %s, continuing"` (generator partial-dir open) | `generator.c:1871` | (no equivalent literal) | n/a | **Missing** |
| `"failed to open %s"` (early-input file) | `clientserver.c:270` | (not implemented; early-input is unsupported in oc-rsync) | n/a | **Missing** (intentional - feature out of scope) |
| `"failed to open lock file %s"` | `clientserver.c:746` | (no equivalent literal; we send a different `@ERROR:` line) | `crates/daemon/src/daemon.rs:124` | **Drift** (different `@ERROR:` payload, see `@ERROR` table below) |
| `"failed to open log-file %s"` | `log.c:163` | (no equivalent literal; logs unconditionally on log file open failure) | n/a | **Missing** |
| `"failed to open files-from file %s: %s\n"` | `options.c:2486` | `"failed to open --files-from {}: {e}"` | `crates/core/src/client/remote/daemon_transfer/orchestration/transfer.rs:290` | **Drift** (we prepend `--files-from` rather than emitting `files-from`; identical intent) |
| `"skipping non-regular file \"%s\"\n"` | `generator.c:1687` | `"skipping non-regular file \"{}\""` | `crates/cli/src/frontend/progress/render.rs:396` | **Match** |
| `"make_bak: skipping non-regular file %s\n"` | `backup.c:308` | (no equivalent literal; backup path is not implemented) | n/a | **Missing** (intentional - `--backup` mkdir branch not yet implemented) |
| `"link_stat %s failed"` | `flist.c:1810, 2398` | `"rsync: link_stat \"{}\" failed: {} ({}) {}{}"` | `crates/transfer/src/generator/file_list/walk.rs:327` | **Drift** (we prepend `rsync: `, quote the path, append `: <e> (<errno>)`, plus location and sender trailer; upstream relies on the `rsyserr` envelope) |
| `"readlink_stat(%s) failed"` | `flist.c:1294` | (no equivalent literal) | n/a | **Missing** |
| `"mkdir %s failed"` (top-level destination) | `main.c:789` | (no equivalent verbatim literal; surfaced via `io::Error` chain) | n/a | **Missing** |
| `"recv_generator: mkdir %s failed"` | `generator.c:1323, 1477` | (no equivalent literal) | n/a | **Missing** |
| `"ERROR: cannot stat destination %s"` | `main.c:770` | (no equivalent literal) | n/a | **Missing** |
| `"unexpected tag %d [%s%s]\n"` (multiplex desync) | `io.c:1703` | (no equivalent literal; we return `io::ErrorKind::InvalidData` from `MplexReader`) | `crates/protocol/tests/network_interruption.rs:375` | **Missing** |
| `"rsync error: %s (code %d) at %s(%d) [%s=%s]\n"` (envelope) | `log.c:906` | `"rsync error: {message} (code {exit_code}) at {rel_path}:{line} [{role}={version}]"` | `crates/logging/src/error_format.rs:75` | **Drift** (we use `:` between path and line; upstream uses `(<line>)`) |
| `"rsync warning: %s (code %d) at %s(%d) [%s=%s]\n"` (envelope) | `log.c:903` | `"rsync warning: {message} (code {exit_code}) at {rel_path}:{line} [{role}={version}]"` | `crates/logging/src/error_format.rs:99` | **Drift** (same `:` vs `(line)` divergence) |

## `@ERROR:` daemon family

These strings are wire-visible: clients parse them to surface daemon-rejection
reasons. Upstream emits them via `io_printf(f_out, ...)` from
`clientserver.c`. oc-rsync emits them via templated string constants in
`crates/daemon/src/daemon.rs:108-125`.

| Upstream verbatim                                            | Upstream `file:line`     | Our verbatim                                                  | Our `file:line`            | Parity |
|--------------------------------------------------------------|--------------------------|---------------------------------------------------------------|----------------------------|--------|
| `@ERROR: protocol startup error\n`                           | `clientserver.c:182`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: your client omitted the subprotocol value: %s\n`    | `clientserver.c:191`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: your client omitted the digest name list: %s\n`     | `clientserver.c:207`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: chdir failed\n`                                     | `clientserver.c:647`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: invalid gid %s\n`                                   | `clientserver.c:656`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: getpwuid failed\n`                                  | `clientserver.c:682`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: Unknown module '%s'\n`                              | `clientserver.c:730`     | `@ERROR: Unknown module '{module}'`                           | `crates/daemon/src/daemon.rs:119` | **Match** (modulo `\n` injected by emitter) |
| `@ERROR: access denied to %s from %s (%s)\n`                 | `clientserver.c:733-734` | `@ERROR: access denied to {module} from {host} ({addr})`      | `crates/daemon/src/daemon.rs:113` | **Match** |
| `@ERROR: failed to open lock file\n`                         | `clientserver.c:748`     | `@ERROR: failed to update module connection lock; please try again later` | `crates/daemon/src/daemon.rs:124-125` | **Drift** (different wording for the same condition) |
| `@ERROR: max connections (%d) reached -- try again later\n`  | `clientserver.c:752`     | `@ERROR: max connections ({limit}) reached -- try again later` | `crates/daemon/src/daemon.rs:121-122` | **Match** |
| `@ERROR: auth failed on module %s\n`                         | `clientserver.c:762`     | `@ERROR: auth failed on module {module}`                      | `crates/daemon/src/daemon.rs:117` | **Match** |
| `@ERROR: invalid uid %s\n`                                   | `clientserver.c:783`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: invalid gid setting.\n`                             | `clientserver.c:802, 811`| (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: no path setting.\n`                                 | `clientserver.c:826`     | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: fork failed\n`                                      | `clientserver.c:910`     | (no equivalent literal; oc-rsync does not fork)               | n/a                        | **Missing** (intentional) |
| `@ERROR: chroot failed\n`                                    | `clientserver.c:981`     | (no equivalent literal; chroot is not implemented)            | n/a                        | **Missing** (intentional) |
| `@ERROR: setgid failed\n`                                    | `clientserver.c:1010`    | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: setuid failed\n`                                    | `clientserver.c:1039`    | (no equivalent literal)                                       | n/a                        | **Missing** |
| `@ERROR: invalid early_input length\n`                       | `clientserver.c:1360`    | (no equivalent literal; early-input not implemented)          | n/a                        | **Missing** (intentional) |
| `@ERROR: Unknown command '%s'\n`                             | `clientserver.c:1379`    | (no equivalent literal)                                       | n/a                        | **Missing** |
| (none in upstream)                                           | n/a                      | `@ERROR: daemon functionality is unavailable in this build`   | `crates/daemon/src/daemon.rs:108` | **Extra** (compile-time fallback when daemon support is stubbed out) |

## Format-specifier ordering analysis

The audited messages either have a single `%s` substitution (path or module
name) or a fixed prefix/suffix. We found no case of swapped `%s` / `%d`
ordering between upstream and oc-rsync. Specific calls examined:

* `"@ERROR: access denied to %s from %s (%s)\n"` -
  `(name, host, addr)` upstream vs `({module}, {host}, {addr})` oc-rsync.
  Same order.
* `"@ERROR: max connections (%d) reached -- try again later\n"` -
  `(limit)` in both.
* `"connection unexpectedly closed (%s bytes received so far) [%s]\n"` -
  `(big_num(stats.total_read), who_am_i())` upstream. oc-rsync does not
  emit this string at all (see Recommendations).
* `"rsync error: %s (code %d) at %s(%d) [%s=%s]\n"` -
  `(name, code, src_file, line, who_am_i, version)` in both. Same order;
  only the separator between `%s` (file) and `%d` (line) differs.

## Trailing punctuation and newline differences

Upstream consistently terminates user-visible messages with `\n` inside the
format string. oc-rsync uses `eprintln!` / `println!` (which append the
platform line separator) or `format!` with no trailing newline (the message
is later rendered through `Message::render()` which adds a single `\n`).

Specific punctuation drift:

| String                                | Upstream tail                | Ours tail                        | Note |
|---------------------------------------|------------------------------|----------------------------------|------|
| `@ERROR: invalid gid setting.\n`      | `setting.\n`                 | n/a                              | Trailing period in upstream. |
| `@ERROR: no path setting.\n`          | `setting.\n`                 | n/a                              | Trailing period in upstream. |
| `@ERROR: max connections (...) reached -- try again later\n` | `... try again later\n` | `... try again later`            | Same wording; `\n` injected by writer. |
| `rsync error: ... at <file>(<line>) [<role>=<version>]\n` | `(<line>)` parens         | `:<line>` colon                  | Separator divergence. |
| `link_stat %s failed`                 | `failed` (no trailing punct) | `link_stat "<path>" failed: <e> (<errno>) <loc> <trailer>` | We append `: <e> (<errno>) <loc> <trailer>`. |
| `send_files failed to open %s`        | `open %s` (no trailing colon)| `open "<path>": <e> (<errno>) <loc> <trailer>` | We append the strerror locally; upstream relies on the `rsyserr` wrapper to append `: <strerror>`. Net wire-visible output is similar but quoting differs. |

## Role-trailer presence per message

Upstream emits the `[sender=<version>]` / `[receiver=<version>]` /
`[generator=<version>]` / `[server=<version>]` / `[client=<version>]` /
`[daemon=<version>]` trailer **only** through the `rsync error:` /
`rsync warning:` envelope produced by `log.c:903-907`. Per-file warnings
(`file has vanished: %s\n`, `link_stat %s failed`, `send_files failed to
open %s`) **do not** carry a role trailer in upstream.

oc-rsync, by contrast, attaches a role trailer to several per-file warnings
directly inside the `eprintln!` literal:

| Message                                               | Upstream trailer | Our trailer            | Parity |
|-------------------------------------------------------|------------------|------------------------|--------|
| `file has vanished: <path>` (generator open path)     | none             | `[generator=<version>]`| **Drift** |
| `file has vanished: <path>` (walk-time stat path)     | none             | `[generator=<version>]`| **Drift** |
| `link_stat "<path>" failed: ...`                      | none             | `[sender=<version>]`   | **Drift** |
| `send_files failed to open <path>: ...` (generator)   | none             | `[generator=<version>]`| **Drift** |
| `send_files failed to open <path>: ...` (receiver)    | none             | `[receiver=<version>]` | **Drift** |
| `rsync error: ...` envelope                           | yes              | yes                    | **Match** |
| `rsync warning: ...` envelope                         | yes              | yes                    | **Match** |
| `@ERROR: access denied to %s from %s (%s)`            | n/a (separate channel) | n/a              | **Match** (no trailer either side) |

## Recommendations

The recommendations are sorted by user-visible impact. None requires a wire
protocol change; all are localised string edits.

### High-impact (visible to interop scripts and parity tests)

1. **Reconcile `ExitCode::description()` with upstream.** Edit
   `crates/core/src/exit_code/codes.rs:147-176` to use the upstream wording for
   codes 15, 16, 23, 25, 124, 125. Eliminates the divergence with
   `crates/core/src/message/strings.rs` and matches what scripts grep for in
   stderr.
2. **Switch the error/warning envelope separator from `:` to `(<line>)`.**
   Edit `crates/logging/src/error_format.rs:75` and `:99` to format `at
   <file>(<line>)` instead of `at <file>:<line>`. This matches `log.c:903-907`
   and the contents of upstream's existing parity tests in
   `crates/core/src/message/tests/part1.rs` and `part8.rs`.
3. **Emit `connection unexpectedly closed (<n> bytes received so far) [<role>]`.**
   Add the literal in `crates/transfer/src/transfer_ops/token_loop.rs` or the
   appropriate I/O drain site (`crates/protocol/src/...`) so unexpected EOF on
   the remote side surfaces the same banner upstream emits at `io.c:228`.
   Currently we only return an `io::Error`, and the comment at
   `crates/daemon/src/daemon/sections/module_access/transfer.rs:521` already
   acknowledges the gap.

### Medium-impact (per-file warnings)

4. **Drop the `rsync:` prefix and the `error_location!()` + role trailer from
   per-file warnings.** Edit:
   * `crates/transfer/src/generator/protocol_io.rs:134` (`file has vanished`),
   * `crates/transfer/src/generator/protocol_io.rs:142` (`send_files failed to open`),
   * `crates/transfer/src/generator/file_list/walk.rs:319,327` (`file has vanished`, `link_stat ... failed`),
   * `crates/transfer/src/pipeline/receiver.rs:163,221` (`send_files failed to open`),
   * `crates/core/src/client/error.rs:246` (`file has vanished`).
   Per upstream, these emit only the bare warning text. Move the location/
   role trailer attachment into the centralised `rsync_warning_fmt!` /
   `rsync_error_fmt!` envelope where upstream applies it consistently.
5. **Use `read error:` (no `network` qualifier) and let the standard envelope
   prepend `rsync:`.** Edit
   `crates/transfer/src/transfer_ops/token_loop.rs:103,137` and
   `crates/core/src/client/remote/remote_to_remote.rs:421` so the literal
   matches upstream `io.c:804,806`.
6. **Replace `failed to access destination directory` with the upstream
   `change_dir#<N> %s failed` family.** `crates/core/src/client/error.rs:260`
   currently emits an oc-rsync-specific phrasing. The upstream variants live
   at `main.c:749, 807, 827, 936, 1161`; pick #1 for the generic case unless
   the call site is one of the more specific ones.

### Low-impact (rare daemon paths)

7. **Use `@ERROR: failed to open lock file` verbatim.** Edit
   `crates/daemon/src/daemon.rs:124-125` to emit the upstream literal, which
   is what clients (including upstream rsync) compare for retry-classification
   purposes.
8. **Add the missing `@ERROR:` literals as the underlying daemon features
   land.** The Missing entries in the `@ERROR:` table above (`chdir failed`,
   `getpwuid failed`, `invalid uid`, `invalid gid`, `setgid failed`,
   `setuid failed`, `no path setting.`, `Unknown command`) should be emitted
   verbatim once oc-rsync grows the equivalent code paths. Today they would
   be emitted via the generic `HANDSHAKE_ERROR_PAYLOAD` fallback if reached.
9. **Document the intentional "Missing" rows.** The chroot, fork, and
   early-input messages will never have an oc-rsync equivalent because the
   underlying capabilities are not implemented. Annotate them in
   `crates/daemon/src/daemon.rs` with `// upstream: clientserver.c:NNN -
   intentionally not emitted (feature unimplemented)` so future readers can
   tell the difference.

### Cleanup

10. **Add a parity test that walks `EXIT_CODE_TABLE` and `ExitCode::description()`
    side by side** so the two tables cannot drift again. The natural home is
    `crates/core/src/message/tests/part8.rs`, which already exercises the
    upstream wording for exit codes 1, 2, 3, 15, 23, 30. Extend it to cover
    the full 26-row table.

## Divergence count

* Banner table (`codes.rs` only): **6**
* Banner table extras (`codes.rs` and `strings.rs`): **1** (code 6, intentional)
* Per-message verbatim audit: **14** drift / missing rows that map to active
  oc-rsync code paths (excludes intentional misses for unimplemented features)
* `@ERROR:` family: **3** (1 wording drift, 2 active code paths missing the
  literal: `Unknown command`, `chdir failed`)

**Total actionable divergences: 24.**
