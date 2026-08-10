# Error message format - upstream rsync 3.4.1 comparison audit

Branch: `docs/error-format-upstream-audit`. Audit date: 2026-05-05.

## Scope

Audit the user-visible diagnostic messages that oc-rsync writes to stderr (and
that flow over the wire as multiplexed `MSG_ERROR*` frames) for byte-level
parity with upstream rsync 3.4.1. Specifically:

- Text content of the canonical exit-code messages (`rerr_names`).
- Severity prefix (`rsync error:` / `rsync warning:` / `rsync info:` / `rsync:`).
- Role trailers (`[sender]`, `[receiver]`, `[generator]`, `[server]`,
  `[client]`, optional `[daemon]`).
- File:line source attribution format (the `at FILE:LINE` suffix).
- Exit-code numeric mapping (errcode.h vs `crates/core/src/exit_code/codes.rs`).
- Channel routing - whether errors flow over `MSG_ERROR` /
  `MSG_ERROR_XFER` / `MSG_ERROR_SOCKET` / `MSG_ERROR_UTF8` /
  `MSG_ERROR_EXIT` multiplex frames or land directly on stderr.

Tooling parses rsync output (CI scripts, monitoring, log-grep heuristics) and
silent drift breaks downstream consumers, so the audit treats "byte-identical
to upstream" as the bar.

## Methodology

The upstream C source is the only authoritative reference. All upstream
citations refer to files unpacked at `target/interop/upstream-src/rsync-3.4.1/`:

- `errcode.h` - numeric `RERR_*` constants.
- `log.c` - `rerr_names[]`, `rerr_name()`, `rwrite()`, `rsyserr()`,
  `log_exit()`, `who_am_i()` plumbing through formatting.
- `cleanup.c` - `_exit_cleanup()`, the routing of `MSG_ERROR_EXIT`.
- `rsync.c` - `who_am_i()` returning the role string used in trailers.
- `util2.c` - `src_file()`, the basename-stripping helper that produces the
  `at io.c(234)` form.
- `io.c` (`send_msg_int`, `MSG_ERROR_EXIT` handling).

oc-rsync sources audited (all paths repository-relative):

- `crates/core/src/exit_code/codes.rs` - `ExitCode` enum, descriptions,
  `from_io_error`.
- `crates/core/src/exit_code/convert.rs` - i32 / `process::ExitCode`
  conversions.
- `crates/core/src/message/mod.rs` - module entry, `VERSION_SUFFIX`.
- `crates/core/src/message/role.rs` - `Role` enum and parser.
- `crates/core/src/message/source.rs` - `SourceLocation`, `file_basename`,
  workspace prefix stripping.
- `crates/core/src/message/macros.rs` - `error_location!`, `message_source!`,
  `tracked_message_source!`, `rsync_error!`, `rsync_warning!`, `rsync_info!`,
  `rsync_exit_code!`.
- `crates/core/src/message/strings.rs` - `EXIT_CODE_TABLE` mirror of upstream
  `rerr_names`.
- `crates/core/src/message/message_impl/mod.rs` - `Message`, `as_segments`,
  prefix rendering.
- `crates/core/src/message/message_impl/render.rs` - newline / vectored
  emission.
- `crates/core/src/client/error.rs` - higher-level `rsync_error!` call sites
  for `Role::Client`.
- `crates/cli/src/frontend/server/run.rs` - server-side error path attaching
  `Role::Server`.
- `crates/daemon/src/daemon/sections/...` - daemon-side error paths attaching
  `Role::Daemon`.
- `crates/protocol/src/envelope/message_code.rs` - `MessageCode` enum
  numbering.
- `crates/protocol/src/multiplex/writer.rs` - `MplexWriter::write_error`,
  `write_warning`, `write_info`.
- `crates/transfer/src/reader/multiplex.rs` - receiver-side dispatch of
  `MessageCode::Error*` to stderr.

Test / golden assets reviewed:

- `tests/interop/messages/golden-3.4.1.toml`,
  `tests/interop/messages/golden-3.1.3.toml`,
  `tests/interop/messages/golden-3.0.9.toml`.
- `tests/interop/exit_codes/golden-3.4.1.toml`,
  `tests/interop/exit_codes/scenarios.toml` and the older-version goldens.
- `xtask/src/commands/interop/messages/{matcher,extractor}.rs`.
- `.github/workflows/interop-validation.yml` - "Exit Code Validation" and
  "Message Format Validation" jobs.

## TL;DR

oc-rsync's error format machinery is largely byte-compatible with upstream
3.4.1 for the externally visible parts: exit-code numbering, canonical
text strings (`rerr_names` mirror), `rsync error:` / `rsync warning:` prefix,
and `MSG_ERROR*` multiplex routing. There are six divergences worth
addressing, of which two affect the rendered output in ways that downstream
parsers can detect:

1. The `Message::as_segments` renderer emits `at PATH:LINE`
   (colon-separated, repo-relative path) at
   `crates/core/src/message/message_impl/mod.rs:108-113`. Upstream emits
   `at FILE.c(LINE)` (basename with parens) at
   `target/interop/upstream-src/rsync-3.4.1/log.c:903-907` via
   `src_file()` in `util2.c:132`. **DIVERGE on rendered output.**
2. The `error_location!` macro at
   `crates/core/src/message/macros.rs:23-31` does emit the upstream
   `at FILE.rs(LINE)` form, but it is not the format used when a `Message`
   carries a `SourceLocation`. The two formatters are inconsistent inside
   oc-rsync.
3. oc-rsync defines `Role::Daemon` (`crates/core/src/message/role.rs:20`).
   Upstream's `who_am_i()` (`rsync.c:823-831`) only ever returns
   `client`, `server`, `sender`, `generator`, `receiver` (5 values). A
   `[daemon]` trailer therefore never appears in upstream output.
4. oc-rsync defines `ExitCode::LogFileAppend = 6`
   (`crates/core/src/exit_code/codes.rs:46`). Upstream's `errcode.h` does
   not define `RERR_LOG_FAILURE = 6`; no `_exit_cleanup(6)` call site exists
   in the upstream tree.
5. The `Role::Sender`, `Role::Receiver`, and `Role::Generator` trailers are
   defined and exercised in unit tests, but no production call site in
   `crates/transfer/`, `crates/engine/`, or the receiver/generator pipeline
   attaches them to emitted `Message`s. Today every transfer-time error in
   oc-rsync surfaces with no role trailer, while the equivalent upstream
   error carries `[sender]`, `[receiver]`, or `[generator]`.
6. `ExitCode::description()` in `codes.rs:147-177` returns several strings
   that diverge from upstream `rerr_names` (e.g. `"partial transfer"` vs
   upstream's `"some files/attrs were not transferred (see previous
   errors)"`). The canonical wording is correctly mirrored in
   `crates/core/src/message/strings.rs:89-132`, so message rendering is
   correct - but `description()` is a separate helper that surfaces in
   `Display for ExitCode` (`exit_code/convert.rs:5-9`) and any caller that
   uses it as a fallback string will diverge.

The `Message Format Validation` and `Exit Code Validation` interop jobs at
`.github/workflows/interop-validation.yml:84-101` and `:103-162` exist and
exercise the goldens in `tests/interop/{exit_codes,messages}/`, which gives
us coverage for divergences 1-5 once the matcher is taught to enforce the
parens form.

## Error categorisation

Error sites in oc-rsync fall into four buckets, mirroring upstream's `rwrite`
log codes (`log.c:251-345`):

| Bucket | Severity | Process exit | Multiplex code | Upstream `rwrite` code | Examples |
|--------|----------|--------------|----------------|------------------------|----------|
| Hard error | `rsync error:` | non-zero | `MSG_ERROR` (3) or `MSG_ERROR_XFER` (1) | `FERROR`, `FERROR_XFER` | `crates/core/src/client/error.rs:158-167` (missing operands -> exit 23), socket / file-select / protocol fatals. |
| Per-file error | `rsync error:` | exit 23 in aggregate | `MSG_ERROR_XFER` | `FERROR_XFER` | Per-file open / stat / xattr failures during transfer. Sets `got_xfer_error` upstream (`log.c:311`). |
| Warning | `rsync warning:` | 0 (or 24) | `MSG_WARNING` (4) | `FWARNING` | "file has vanished" path at `crates/core/src/client/error.rs:244-247`; vanished/RERR_VANISHED downgraded to warning by `strings.rs:116-120`. |
| Info | `rsync info:` (oc-rsync) / `rsync:` (upstream) | 0 | `MSG_INFO` (2) | `FINFO` | Daemon connect/auth notices (`crates/daemon/src/daemon/sections/...`). |

The severity prefix is rendered in `Message::render_prefix`
(`crates/core/src/message/message_impl/mod.rs:134-158`); the program name is
sourced from the `Brand` and is `rsync` for the `Brand::Upstream` mode, matching
upstream verbatim.

## Per-role audit

Upstream's role string is produced exclusively by `who_am_i()`
(`rsync.c:823-831`):

```c
const char *who_am_i(void) {
    if (am_starting_up)
        return am_server ? "server" : "client";
    return am_sender ? "sender"
         : am_generator ? "generator"
         : am_receiver ? "receiver"
         : "Receiver";
}
```

There is no `daemon` value. The exact-match interop matcher at
`xtask/src/commands/interop/messages/extractor.rs:48-71` allows `daemon` as a
recognised trailer, which is necessary for oc-rsync goldens but never
populated by upstream goldens.

| Role | Trailer wired in production? | Citation | Status |
|------|------------------------------|----------|--------|
| `[client]` | Yes | `crates/core/src/client/error.rs:165, 176, 183, 206, 214, 220, 230, 250, 261, 273, 284`; `crates/cli/src/frontend/filter_rules/merge.rs:49, 112, 136, 170, 212, 225`; `crates/cli/src/frontend/filter_rules/parsing/merge.rs:34, 46, 60, 83`; `crates/core/src/client/config/{compress_env.rs:29, enums/checksum.rs:93, skip_compress.rs:41}`. | MATCH (consistent with upstream `client` from `who_am_i()`). |
| `[server]` | Yes | `crates/cli/src/frontend/server/run.rs:233` (`write_server_error`). | MATCH. |
| `[daemon]` | Yes | `crates/daemon/src/daemon/sections/privilege.rs:10`; `module_access/{helpers.rs:74,86,99, transfer.rs:37,123,178,192,289,465,478,496}`; `daemonize.rs:21`; `xfer_exec.rs:198`. | DIVERGE - upstream never emits `[daemon]` because `who_am_i()` collapses the daemon to `server`. The trailer is intentional in oc-rsync but parsers expecting upstream output will not see it. |
| `[sender]` | No production call site found | `grep -rn 'with_role(Role::Sender)' crates/` outside `crates/core/src/message/**` returns only test code (e.g. `crates/core/src/message/message_impl/{classification.rs:182, mutators.rs:118-186}`, `segments/io.rs:227`). | GAP - the role exists but is never attached to live errors. |
| `[receiver]` | No production call site found | Same `grep` returns only test code (`mutators.rs:126`, `segments/io.rs:250`). | GAP. |
| `[generator]` | No production call site found | No matches outside docs/tests. | GAP. |

Practical impact: errors raised inside `crates/transfer/` (file open / read /
write / fsync errors during the delta loop) are emitted without a role
trailer today. Upstream emits the same errors with `[sender]`, `[receiver]`,
or `[generator]` depending on which subprocess raised them. The
`tests/interop/messages/golden-*.toml` files do encode `role = "sender"` and
`role = "client"` expectations against scenarios such as `missing_source`
and `invalid_option`, so the gap is partially covered by the
`Message Format Validation` interop job.

## File:line source attribution format

Upstream emits exactly one source-line format, in `log_exit()` at
`target/interop/upstream-src/rsync-3.4.1/log.c:903-907`:

```c
rprintf(FERROR, "rsync error: %s (code %d) at %s(%d) [%s=%s]\n",
        name, code, src_file(file), line, who_am_i(), rsync_version());
```

`src_file()` in `target/interop/upstream-src/rsync-3.4.1/util2.c:132-145`
strips the directory prefix so the rendered path is always a basename, e.g.
`at main.c(1338)`. The `tests/interop/messages/golden-3.4.1.toml:9` sample
confirms the form:

```
"rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1338) [sender]"
```

(The trailing `=3.4.1` of the version is stripped by the golden post-processor;
the wire output has it.)

oc-rsync has two formatters, only one of which matches:

1. `error_location!()` macro -
   `crates/core/src/message/macros.rs:23-31`:

   ```rust
   format!("at {}({})", $crate::message::file_basename(file!()), line!())
   ```

   Produces `at source.rs(42)` - matches upstream form. The macro is
   exercised by the format tests at
   `crates/core/src/message/source.rs:533-597`.

2. `Message::as_segments` -
   `crates/core/src/message/message_impl/mod.rs:107-113`:

   ```rust
   if let Some((path_bytes, start, len)) = source_info {
       push(b" at ");
       push(path_bytes);
       push(b":");
       let digits = &scratch.line_digits[start..start + len];
       push(digits);
   }
   ```

   Produces `at crates/core/src/whatever.rs:42` - colon-separated,
   repo-relative path. **This is what the `rsync_error!` macro chain
   actually renders** because the macros (`macros.rs:132-142`) attach a
   `SourceLocation` via `tracked_message_source!()`, not the
   `error_location!()` string.

The repo-relative path is produced by
`SourceLocation::from_parts` in `crates/core/src/message/source.rs:33-64`,
which canonicalises against `RSYNC_WORKSPACE_ROOT` /
`CARGO_WORKSPACE_DIR` and strips the workspace prefix
(`source.rs:151-159`). It is stable across builds when one of those env
vars is set; without them the path falls back to the absolute filesystem
path, which is **build-host-dependent** and therefore unstable across
CI runners.

This is the single most visible divergence:

- Upstream: `at main.c(1338)`
- oc-rsync: `at crates/core/src/client/error.rs:165` (or worse,
  `at /home/runner/work/.../error.rs:165` when the workspace root is
  unknown).

## Exit-code mapping

| Upstream `RERR_*` (`errcode.h`) | Numeric | oc-rsync variant (`exit_code/codes.rs`) | Status |
|--------------------------------|---------|------------------------------------------|--------|
| `RERR_OK` | 0 | `ExitCode::Ok` (`codes.rs:13`) | MATCH |
| `RERR_SYNTAX` | 1 | `ExitCode::Syntax` (`codes.rs:19`) | MATCH |
| `RERR_PROTOCOL` | 2 | `ExitCode::Protocol` (`codes.rs:25`) | MATCH |
| `RERR_FILESELECT` | 3 | `ExitCode::FileSelect` (`codes.rs:30`) | MATCH |
| `RERR_UNSUPPORTED` | 4 | `ExitCode::Unsupported` (`codes.rs:36`) | MATCH |
| `RERR_STARTCLIENT` | 5 | `ExitCode::StartClient` (`codes.rs:41`) | MATCH |
| (no upstream code) | 6 | `ExitCode::LogFileAppend` (`codes.rs:46`) | DIVERGE - not in upstream `errcode.h` or `_exit_cleanup` call sites. |
| `RERR_SOCKETIO` | 10 | `ExitCode::SocketIo` (`codes.rs:51`) | MATCH |
| `RERR_FILEIO` | 11 | `ExitCode::FileIo` (`codes.rs:56`) | MATCH |
| `RERR_STREAMIO` | 12 | `ExitCode::StreamIo` (`codes.rs:61`) | MATCH |
| `RERR_MESSAGEIO` | 13 | `ExitCode::MessageIo` (`codes.rs:66`) | MATCH |
| `RERR_IPC` | 14 | `ExitCode::Ipc` (`codes.rs:71`) | MATCH |
| `RERR_CRASHED` | 15 | `ExitCode::Crashed` (`codes.rs:76`) | MATCH |
| `RERR_TERMINATED` | 16 | `ExitCode::Terminated` (`codes.rs:81`) | MATCH |
| `RERR_SIGNAL1` | 19 | `ExitCode::Signal1` (`codes.rs:84`) | MATCH |
| `RERR_SIGNAL` | 20 | `ExitCode::Signal` (`codes.rs:87`) | MATCH |
| `RERR_WAITCHILD` | 21 | `ExitCode::WaitChild` (`codes.rs:90`) | MATCH |
| `RERR_MALLOC` | 22 | `ExitCode::Malloc` (`codes.rs:93`) | MATCH |
| `RERR_PARTIAL` | 23 | `ExitCode::PartialTransfer` (`codes.rs:99`) | MATCH numeric, DIVERGE wording in `description()`. |
| `RERR_VANISHED` | 24 | `ExitCode::Vanished` (`codes.rs:104`) | MATCH (correctly classified as warning by `strings.rs:116-120`). |
| `RERR_DEL_LIMIT` | 25 | `ExitCode::DeleteLimit` (`codes.rs:109`) | MATCH |
| `RERR_TIMEOUT` | 30 | `ExitCode::Timeout` (`codes.rs:114`) | MATCH |
| `RERR_CONTIMEOUT` | 35 | `ExitCode::ConnectionTimeout` (`codes.rs:119`) | MATCH |
| `RERR_CMD_FAILED` | 124 | `ExitCode::CommandFailed` (`codes.rs:122`) | MATCH |
| `RERR_CMD_KILLED` | 125 | `ExitCode::CommandKilled` (`codes.rs:125`) | MATCH |
| `RERR_CMD_RUN` | 126 | `ExitCode::CommandRun` (`codes.rs:128`) | MATCH |
| `RERR_CMD_NOTFOUND` | 127 | `ExitCode::CommandNotFound` (`codes.rs:133`) | MATCH |

`description()` strings diverging from upstream `rerr_names`
(`target/interop/upstream-src/rsync-3.4.1/log.c:80-107`):

| Code | `codes.rs::description()` | Upstream `rerr_names` |
|------|---------------------------|-----------------------|
| 6 | `"daemon unable to append to log-file"` | (not defined) |
| 15 | `"received SIGSEGV or SIGBUS or SIGABRT"` | `"sibling process crashed"` |
| 16 | (no entry; covered by `Crashed` description above) | `"sibling process terminated abnormally"` |
| 23 | `"partial transfer"` | `"some files/attrs were not transferred (see previous errors)"` |
| 24 | `"some files vanished before they could be transferred"` | identical (MATCH) |
| 25 | `"max delete limit stopped deletions"` | `"the --max-delete limit stopped deletions"` |
| 124 | `"remote command failed"` | `"remote shell failed"` |
| 125 | `"remote command killed"` | `"remote shell killed"` |

`Message::from_exit_code` and the macros route through
`crates/core/src/message/strings.rs::EXIT_CODE_TABLE` which **does** mirror
upstream verbatim (`strings.rs:89-132`). So actual rendered messages match.
The drift is contained to `ExitCode::description()` and its `Display` impl
(`exit_code/convert.rs:5-9`); any caller using that helper as fallback text
will produce upstream-divergent output.

`ExitCode::from_io_error` (`codes.rs:264-288`) is an oc-rsync convenience for
mapping `std::io::ErrorKind` to an exit code. It has no direct upstream
analogue, but the buckets it produces (`FileSelect` for `NotFound` /
`PermissionDenied`, `SocketIo` for connection errors, `Timeout` for
`TimedOut`) are consistent with the way upstream main.c / io.c branches on
`errno`.

## MSG_ERROR multiplex framing

`crates/protocol/src/envelope/message_code.rs:17-75` defines `MessageCode`
with the upstream `enum msgcode` numbering. The error-bearing codes are:

| Variant | Numeric | Upstream alias | Wire role |
|---------|---------|----------------|-----------|
| `Data` | 0 | `MSG_DATA` | normal payload |
| `ErrorXfer` | 1 | `MSG_ERROR_XFER` | per-file transfer error (`FERROR_XFER`) |
| `Info` | 2 | `MSG_INFO` | informational (`FINFO`) |
| `Error` | 3 | `MSG_ERROR` | hard error (`FERROR`) |
| `Warning` | 4 | `MSG_WARNING` | warning (`FWARNING`) |
| `ErrorSocket` | 5 | `MSG_ERROR_SOCKET` | sibling-pipe error (`FERROR_SOCKET`) |
| `Log` | 6 | `MSG_LOG` | daemon-only log (`FLOG`) |
| `Client` | 7 | `MSG_CLIENT` | client stdout (`FCLIENT`) |
| `ErrorUtf8` | 8 | `MSG_ERROR_UTF8` | UTF-8 conversion error (`FERROR_UTF8`) |
| `IoError` | 22 | `MSG_IO_ERROR` | sender-side I/O error |
| `ErrorExit` | 86 | `MSG_ERROR_EXIT` | synchronised exit code (proto >= 31) |

Routing on the writer side:

- `crates/protocol/src/multiplex/writer.rs:337-340` -
  `MplexWriter::write_error` wraps a UTF-8 string in a `MSG_ERROR` frame.
- `:345-348` - `write_warning` -> `MSG_WARNING`.
- `:353-356` - `write_info` -> `MSG_INFO`.

Routing on the receiver side:

- `crates/transfer/src/reader/multiplex.rs:166-223` -
  `dispatch_message`. `MessageCode::Error`, `ErrorXfer`, `ErrorSocket`,
  `ErrorUtf8` (lines 182-190) all `eprint!` to stderr - matching upstream
  `log.c:309-316` where `FERROR_XFER`, `FERROR`, and `FWARNING` all
  end up at `f = stderr`.
- `MessageCode::ErrorExit` (lines 191-207) decodes the 4-byte little-endian
  exit code and surfaces it as an `io::Error` for the transfer loop -
  mirroring `cleanup.c:242-258` where `MSG_ERROR_EXIT` triggers
  `_exit_cleanup(val)`.
- `MessageCode::Info` and `MessageCode::Client` (lines 169-175) go to
  stdout (`print!`) - matching upstream `log.c:288-289` (FCLIENT folded
  into FINFO and routed to stdout).
- `MessageCode::Warning` and `MessageCode::Log` (lines 176-181) go to
  stderr - matching upstream `log.c:309-316` for FWARNING and the
  daemon-only log fallthrough.
- `MessageCode::IoError`, `MessageCode::NoSend`, `MessageCode::Redo` are
  decoded inline (`handle_io_error_msg`, `handle_no_send_msg`,
  `handle_redo_msg`) with explicit upstream citations on each branch.

The multiplex routing matches upstream byte-for-byte. **MATCH.**

## Known divergences (deliberate)

| # | Divergence | Rationale | Risk |
|---|-----------|-----------|------|
| 1 | `Role::Daemon` trailer (`[daemon]`) | oc-rsync runs `oc-rsync` as a single binary with `--daemon` mode and surfaces structured daemon errors with a distinct trailer. Upstream collapses daemon errors into `[server]`. | Low - trailer is additive; tooling that whitelists upstream's five values must add `daemon`. |
| 2 | `ExitCode::LogFileAppend = 6` | oc-rsync explicitly distinguishes log-file open failure from the more general `MessageIo` (13). | Low - the code is unused on upstream wire paths, but a script `case`-matching exit codes will see 6 in oc-rsync output that never appears upstream. Recommend mapping to upstream-equivalent `MessageIo` (13) for parity, or document. |
| 3 | `rsync info:` severity prefix | oc-rsync labels `MSG_INFO` output as `rsync info: ...`. Upstream emits info text without an `info:` prefix (`log.c:317-320` falls through to plain stdout). | Medium - any test that asserts upstream-exact info-line text will fail. |

## Test coverage

- `Exit Code Validation` job at
  `.github/workflows/interop-validation.yml:39-101` runs
  `cargo xtask interop exit-codes --verbose` against
  `tests/interop/exit_codes/scenarios.toml` and the per-version goldens
  (`golden-{3.0.9,3.1.3,3.4.1}.toml`). It catches divergences in numeric
  exit codes for the 14 documented scenarios (dry_run_success,
  permission_denied_read, etc.).
- `Message Format Validation` job at
  `interop-validation.yml:103-162` runs
  `cargo xtask interop messages --verbose` against
  `tests/interop/messages/golden-*.toml`. It uses the `MessageMatcher`
  strategy at `xtask/src/commands/interop/messages/matcher.rs:22-80` -
  exact text + role match, with optional regex / group fallbacks
  (`matcher.rs:42-47, 80+`).
- `tools/ci/run_interop.sh` runs the full daemon push/pull harness against
  upstream 3.0.9, 3.1.3, and 3.4.1. It does not assert byte-exact stderr
  text, only exit codes and transfer outcomes.
- Unit-test coverage of the format itself:
  - `crates/core/src/message/source.rs:531-597` - `error_location!`
    format invariants (`at BASENAME.rs(LINE)`).
  - `crates/core/src/message/message_impl/mutators.rs:118-186` - role
    attachment.
  - `crates/core/src/message/segments/io.rs:227-250` - role + source
    rendering.
  - `crates/core/src/message/strings.rs:299-409` - exit-code wording table.

There is no test that asserts the full upstream rendering form
`<text> (code N) at <basename>(<line>) [<role>=<version>]` byte-for-byte
against a `Message` produced via `rsync_error!`. The closest is
`source.rs:580-597` (`error_location_matches_upstream_format`) which only
covers the macro's standalone output, not the `Message::as_segments`
output that real call sites use.

## Findings

| # | Concern | Status | Evidence | Action |
|---|---------|--------|----------|--------|
| 1 | Source-attribution format in rendered `Message`s | DIVERGE | `crates/core/src/message/message_impl/mod.rs:108-113` emits `at PATH:LINE`; upstream `target/interop/upstream-src/rsync-3.4.1/log.c:903-907` and `util2.c:132-145` emit `at FILE.c(LINE)`. | Switch `as_segments` to use `file_basename(path) + '(' + line + ')'` form. Update `tests/interop/messages/golden-*.toml` matcher rules. |
| 2 | `error_location!` vs `Message` source format inconsistent | DIVERGE | Macro at `crates/core/src/message/macros.rs:23-31` matches upstream; `Message::as_segments` does not. | Resolve by aligning `as_segments` to the macro's form (action 1). |
| 3 | Repo-relative paths in `SourceLocation` not byte-stable | GAP | `crates/core/src/message/source.rs:222-243` - falls back to absolute path when `RSYNC_WORKSPACE_ROOT` / `CARGO_WORKSPACE_DIR` are unset. | Switching to basename (action 1) makes this moot. |
| 4 | `[sender]` / `[receiver]` / `[generator]` never attached at runtime | GAP | No `with_role(Role::Sender / Receiver / Generator)` call in `crates/transfer/`, `crates/engine/`, or sender/generator paths. | Wire role attachment in transfer-loop error helpers. Add unit tests asserting the trailer for each path. |
| 5 | `[daemon]` role trailer not emitted by upstream | DIVERGE (deliberate) | `crates/core/src/message/role.rs:20`; upstream `who_am_i()` at `rsync.c:823-831` has 5 values. | Document in user-facing release notes. Keep the trailer; goldens already accept it. |
| 6 | Exit code 6 (`LogFileAppend`) | DIVERGE | `crates/core/src/exit_code/codes.rs:46`. No upstream `RERR_LOG_FAILURE`. | Either remap log-failure to upstream `RERR_MESSAGEIO` (13) or document the additional code in user-facing docs. |
| 7 | `ExitCode::description()` strings drift from `rerr_names` | DIVERGE | `codes.rs:147-177` vs `target/interop/upstream-src/rsync-3.4.1/log.c:80-107`. | Replace `description()` body with a delegation to `crate::message::strings::exit_code_message(code).text()` so the wording lives in one place. Add a parity test. |
| 8 | `rsync info:` prefix vs upstream `rsync:` for info | DIVERGE | `Severity::as_str()` (search via `crates/core/src/message/severity.rs`) and `Message::render_prefix` at `mod.rs:134-158`. | Document or omit the prefix for `Severity::Info` to match upstream. |
| 9 | Exit-code numeric mapping (24 codes) | MATCH | `codes.rs:13-134` vs `errcode.h:24-64` and `log.c:80-107`. | None. |
| 10 | Multiplex frame routing for errors | MATCH | `crates/protocol/src/multiplex/writer.rs:337-356`, `crates/transfer/src/reader/multiplex.rs:166-223`. | None. |
| 11 | Severity downgrade for code 24 (vanished) | MATCH | `crates/core/src/message/strings.rs:116-120` matches upstream `log.c:903` (`if (code == RERR_VANISHED) ... rsync warning:`). | None. |
| 12 | Interop coverage of error format | PARTIAL | `tests/interop/messages/golden-3.4.1.toml`, `interop-validation.yml:103-162`. Goldens enforce text and role but post-process the version suffix. | Extend goldens to assert source-attribution form (action 1). |

## Recommendations

1. Land a `core::message` change that makes `Message::as_segments` emit
   `at BASENAME(LINE)` matching upstream, deduplicating with the
   `error_location!` macro. Add a byte-level golden test that renders a
   representative `rsync_error!` and compares to the upstream format
   string. Closes findings 1, 2, 3, 12.
2. Wire `Role::Sender`, `Role::Receiver`, `Role::Generator` into the
   transfer-loop error helpers in `crates/transfer/src/` and the
   generator paths in `crates/engine/src/`. Cover with role-attachment
   unit tests and extend the interop goldens with at least one scenario
   per role. Closes finding 4.
3. Replace `ExitCode::description()` with a thin wrapper over
   `message::strings::exit_code_message(code).text()`. Add a parity test
   asserting that `description()` and `rerr_names` agree for every defined
   code. Closes finding 7.
4. Decide on the future of `ExitCode::LogFileAppend = 6` and the
   `rsync info:` prefix. Either align with upstream or document the
   deviation in `docs/UPSTREAM_COMPARISON.md`. Closes findings 6 and 8.
5. Document `[daemon]` as an oc-rsync-specific trailer in release notes
   so downstream parsers can plan for it. Closes finding 5.
