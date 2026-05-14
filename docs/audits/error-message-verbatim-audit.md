# Error message verbatim audit vs upstream rsync 3.4.1

Tracking issue: #2115. Last verified: 2026-05-14 against `origin/master`.

Sources:
`target/interop/upstream-src/rsync-3.4.1/log.c`,
`target/interop/upstream-src/rsync-3.4.1/io.c`,
`target/interop/upstream-src/rsync-3.4.1/main.c`,
`target/interop/upstream-src/rsync-3.4.1/compat.c`,
`target/interop/upstream-src/rsync-3.4.1/clientserver.c`,
`target/interop/upstream-src/rsync-3.4.1/receiver.c`,
`target/interop/upstream-src/rsync-3.4.1/util2.c`;
`crates/core/src/message/strings.rs`,
`crates/core/src/message/severity.rs`,
`crates/core/src/message/message_impl/mod.rs`,
`crates/core/src/client/error.rs`,
`crates/transfer/src/generator/file_list/walk.rs`,
`crates/transfer/src/generator/protocol_io.rs`,
`crates/transfer/src/pipeline/receiver.rs`,
`crates/transfer/src/transfer_ops/token_loop.rs`,
`crates/daemon/src/daemon.rs`,
`crates/daemon/src/daemon/sections/module_access/transfer.rs`,
`crates/protocol/src/error.rs`.

This audit complements the broader `error-message-string-audit.md` (which is
sectioned by oc-rsync subsystem). The goal here is a tight per-family verbatim
comparison: every row pins one upstream string to one oc-rsync string and
labels the result EXACT, DIVERGENT, or MISSING. Status legend mirrors the
sibling audits: EXACT = byte-identical to upstream `%s`-expanded text;
DIVERGENT = same code path, different wording, ordering, or trailer;
MISSING = upstream emits the string, oc-rsync emits nothing equivalent on
the same code path.

Status column codes carried into the recommendation tables: FIXED in this
audit; OPEN remains; CLOSED was re-evaluated and already matches upstream.

## 1. Connection and transport errors

Upstream emits these through `rprintf(FERROR, RSYNC_NAME ": ...")` and the
`MSG_ERROR*` multiplex frames. Each one is a documented grep target for
backup-monitoring and alerting tools.

| # | Upstream wording (cited) | oc-rsync wording (cited) | Status |
|---|---|---|---|
| C1 | `rsync: connection unexpectedly closed (<N> bytes received so far) [<role>]\n` (`io.c:228-230`) | `connection_unexpectedly_closed_error(bytes, role)` builds `connection unexpectedly closed (<N> bytes received so far)` with `Role` trailer and exit code 12 (`crates/core/src/client/error.rs:connection_unexpectedly_closed_error`); rendered envelope uses `rsync error:` per oc-rsync's standard message format | DIVERGENT |
| C2 | `[<role>] io timeout after <N> seconds -- exiting\n` (`io.c:199-200`) | No `[<role>]`-bracket equivalent; timeouts surface through `ExitCode::Timeout` with body text `transfer timed out after <s> seconds without progress` (`crates/core/src/client/error.rs:202-205`) | DIVERGENT |
| C3 | `rsync: [<role>] read error: <strerror> (<errno>)\n` (`io.c:804,806`, via `rsyserr`) | `network read error: <e>` sent via `send_abort` (`crates/transfer/src/transfer_ops/token_loop.rs:103,137`); no `rsync:` prefix, no `[<role>]`, extra `network` qualifier | DIVERGENT |
| C4 | `rsync: writefd_unbuffered failed to write <N> bytes [<role>]: <strerror> (<errno>)\n` (`io.c:847`) | None emitted on the writer path | MISSING |
| C5 | `Invalid packet at end of run [<role>]\n` (`main.c:1077`) | None emitted | MISSING |
| C6 | `Your options have been rejected by the server.\n` (`main.c:1222`) | None emitted | MISSING |

Family totals: 0 EXACT, 3 DIVERGENT, 3 MISSING.

## 2. Protocol negotiation errors

Upstream emits these from `compat.c` and `log.c` during the version /
capability handshake. They are routed over `MSG_ERROR` (multiplexed) and
mapped to exit code 2 (`RERR_PROTOCOL`).

| # | Upstream wording (cited) | oc-rsync wording (cited) | Status |
|---|---|---|---|
| P1 | `protocol version mismatch -- is your shell clean?\n` followed by `(see the rsync manpage for an explanation)\n` (`compat.c:620-621`) | `protocol version mismatch -- is your shell clean?\n(see the rsync manpage for an explanation)` (`crates/protocol/src/error.rs`, `NegotiationError::UnsupportedVersion`) | EXACT (FIXED #2172) |
| P2 | `The protocol version in the batch file is too new (%d > %d).\n` (`compat.c:609-610`) | None emitted on the batch-read path | MISSING |
| P3 | `--protocol must be at least %d on the %s.\n` (`compat.c:629-630`) | None emitted | MISSING |
| P4 | `--protocol must be no more than %d on the %s.\n` (`compat.c:634-635`) | None emitted | MISSING |
| P5 | `protocol incompatibility` (banner text from `rerr_names[]` at `log.c:82`) | `protocol incompatibility` (`crates/core/src/message/strings.rs:91`) | EXACT |
| P6 | `(Server) Protocol versions: remote=%d, negotiated=%d\n` debug line (`compat.c:614-616`) | None emitted; no equivalent INFO log | MISSING |
| P7 | `unexpected tag %d [%s%s]\n` (`io.c:1703`) | None emitted | MISSING |
| P8 | `your client does not support one of our daemon-auth checksums: %s\n` (`compat.c:872`) | None emitted | MISSING |

Family totals: 2 EXACT, 0 DIVERGENT, 6 MISSING.

## 3. Daemon `@ERROR:` wire lines

These are wire-visible greeting-line errors that the client receives before
multiplex starts. Backup tooling parses them to classify rejection reasons,
so the leading `@ERROR: ` literal must be byte-identical. All upstream
citations are in `clientserver.c`.

| # | Upstream wording (cited) | oc-rsync wording (cited) | Status |
|---|---|---|---|
| D1 | `@ERROR: Unknown module '%s'\n` (`clientserver.c:730`) | `@ERROR: Unknown module '{module}'` (`crates/daemon/src/daemon.rs:120`) | EXACT |
| D2 | `@ERROR: access denied to %s from %s (%s)\n` (`clientserver.c:733-734`) | `@ERROR: access denied to {module} from {host} ({addr})` (`crates/daemon/src/daemon.rs:114`) | EXACT |
| D3 | `@ERROR: max connections (%d) reached -- try again later\n` (`clientserver.c:752`) | `@ERROR: max connections ({limit}) reached -- try again later` (`crates/daemon/src/daemon.rs:123`) | EXACT |
| D4 | `@ERROR: auth failed on module %s\n` (`clientserver.c:762`) | `@ERROR: auth failed on module {module}` (`crates/daemon/src/daemon.rs:118`) | EXACT |
| D5 | `@ERROR: failed to open lock file\n` (`clientserver.c:748`) | `@ERROR: failed to open lock file` (`crates/daemon/src/daemon.rs:125-126`) | EXACT |
| D6 | `@ERROR: chdir failed\n` (`clientserver.c:647`) | None emitted | MISSING |
| D7 | `@ERROR: protocol startup error\n` (`clientserver.c:182`) | None emitted | MISSING |
| D8 | `@ERROR: Unknown command '%s'\n` (`clientserver.c:1379`) | None emitted | MISSING |
| D9 | `@ERROR: invalid uid %s\n` (`clientserver.c:783`) | None emitted | MISSING |
| D10 | `@ERROR: invalid gid %s\n` (`clientserver.c:656`) | None emitted | MISSING |
| D11 | `@ERROR: no path setting.\n` (`clientserver.c:826`) | None emitted | MISSING |
| D12 | `ERROR: module is read only\n` via `rprintf(FERROR, ...)` (`main.c:1149`, no `@` prefix; arrives through `MSG_ERROR`) | `@ERROR: module is read only` via daemon greeting (`crates/daemon/src/daemon/sections/module_access/transfer.rs:332`); wrong frame and extra `@` prefix | DIVERGENT |
| D13 | `ERROR: module is write only\n` via `rprintf(FERROR, ...)` (`main.c:917`) | `@ERROR: module is write only` (`crates/daemon/src/daemon/sections/module_access/transfer.rs:337`) | DIVERGENT |
| D14 | (none - upstream has no equivalent) | `@ERROR: daemon functionality is unavailable in this build` (`crates/daemon/src/daemon.rs:109`) | Extra (oc-rsync only) |
| D15 | (none - upstream has no equivalent) | `@ERROR: The server is configured to refuse <flag>` (`crates/daemon/src/daemon/sections/module_access/request.rs:127`) | Extra (oc-rsync only) |
| D16 | (none) | `@ERROR: chroot/privilege setup failed: <err>` (`crates/daemon/src/daemon/sections/module_access/transfer.rs:353,360`) | Extra (oc-rsync only) |

Family totals: 5 EXACT, 2 DIVERGENT, 6 MISSING, 3 Extra.

## 4. File operation warnings (per-file `rsyserr`)

Upstream formats these via `rsyserr()` (`log.c:453-473`) as
`rsync: [<role>] <msg>: <strerror> (<errno>)\n`. No source location is
emitted; no `[<role>=<version>]` trailer is appended; the path is raw `%s`
(not quoted). oc-rsync emits the same code paths via `eprintln!` with three
systematic additions: double-quoted path, appended `error_location!()`
`at <basename>(<line>)`, and appended `[<role>=<version>]` trailer.

| # | Upstream wording (cited) | oc-rsync wording (cited) | Status |
|---|---|---|---|
| F1 | `rsync: [sender] link_stat %s failed: <strerror> (<errno>)\n` (`flist.c:1289`, `rsyserr`) | `rsync: link_stat "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<ver>]\n` (`crates/transfer/src/generator/file_list/walk.rs:327`) | DIVERGENT |
| F2 | `rsync: [sender] send_files failed to open %s: <strerror> (<errno>)\n` (`sender.c:362`) | `rsync: send_files failed to open "<path>": <e> (<errno>) at <basename>(<line>) [generator=<ver>]\n` (`crates/transfer/src/generator/protocol_io.rs:142`) | DIVERGENT |
| F3 | `rsync: [sender] opendir %s failed: <strerror> (<errno>)\n` (`flist.c:2398`) | `rsync: opendir "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<ver>]\n` (`crates/transfer/src/generator/file_list/walk.rs:149,215`) | DIVERGENT |
| F4 | `rsync: [sender] readdir(%s): <strerror> (<errno>)\n` (`flist.c:2418`) | `rsync: readdir "<path>" failed: <e> (<errno>) at <basename>(<line>) [sender=<ver>]\n` (`crates/transfer/src/generator/file_list/walk.rs:246`) | DIVERGENT |
| F5 | `file has vanished: %s\n` bare (`flist.c:1289`, `sender.c:358`) | `file has vanished: "<path>" at <basename>(<line>) [generator=<ver>]\n` (transfer crate); single-quoted variant `file has vanished: '<path>'` on client path (`crates/core/src/client/error.rs:246`) | DIVERGENT |
| F6 | `skipping non-regular file "%s"\n` (`generator.c:1687`) | `skipping non-regular file "<path>"` (`crates/cli/src/frontend/progress/render.rs:396`) | EXACT |
| F7 | `WARNING: %s failed verification -- update %s%s.\n` where `%s` cycles {`discarded`, `put into partial-dir`, `retained`} and `%s%s` is ` (will try again)` or ` (may try again)` (`receiver.c:965-968`) | Hardcoded `WARNING: "<path>" failed verification -- update discarded (will try again). at <basename>(<line>) [receiver=<ver>]` (`crates/transfer/src/pipeline/receiver.rs:277-281`) | DIVERGENT |
| F8 | `ERROR: %s failed verification -- update %s.\n` (`receiver.c:965-968`, redo phase) | Hardcoded `ERROR: "<path>" failed verification -- update discarded. at <basename>(<line>) [receiver=<ver>]` (`crates/transfer/src/pipeline/receiver.rs:287`) | DIVERGENT |
| F9 | `rsync: [sender] make_file failed for %s: <strerror>` (no upstream direct site; `flist.c` paths surface as plain `rsyserr` strings) | `rsync: make_file failed for "<path>": <e> (<errno>) at <basename>(<line>) [sender=<ver>]` (`crates/transfer/src/generator/file_list/walk.rs:129`) | DIVERGENT |

Family totals: 1 EXACT, 8 DIVERGENT, 0 MISSING.

## 5. Exit-code banner (envelope) format

Upstream emits the exit envelope through `log_exit()`:

```
rsync error: <name> (code <N>) at <basename>(<line>) [<role>=<version>]\n
rsync warning: <name> (code <N>) at <basename>(<line>) [<role>=<version>]\n
```

`<name>` comes from `rerr_names[]` (`log.c:80-107`), `<basename>` from
`src_file()` (`util2.c:132-145`) which strips the directory prefix, and
`<role>` from `who_am_i()` (`rsync.c:823-831`). oc-rsync renders the envelope
through `Message::as_segments()` at
`crates/core/src/message/message_impl/mod.rs:80-126`.

| Aspect | Upstream | oc-rsync | Status |
|---|---|---|---|
| Error severity prefix | `rsync error: ` (`log.c:906`) | `rsync error: ` (`crates/core/src/message/severity.rs:68`) | EXACT |
| Warning severity prefix | `rsync warning: ` (`log.c:903`) | `rsync warning: ` (`crates/core/src/message/severity.rs:67`) | EXACT |
| Info severity prefix | (none - bare text via `rprintf(FINFO, ...)`) | `rsync info: ` (`crates/core/src/message/severity.rs:66`) | DIVERGENT |
| Body text - code 1 | `syntax or usage error` (`log.c:81`) | `syntax or usage error` (`strings.rs:90`) | EXACT |
| Body text - code 2 | `protocol incompatibility` (`log.c:82`) | `protocol incompatibility` (`strings.rs:91`) | EXACT |
| Body text - code 23 | `some files/attrs were not transferred (see previous errors)` (`log.c:97`) | `some files/attrs were not transferred (see previous errors)` (`strings.rs:114`) | EXACT |
| Body text - code 24 | `some files vanished before they could be transferred` (`log.c:98`) | `some files vanished before they could be transferred` (`strings.rs:119`) | EXACT |
| `(code <N>)` suffix | `(code %d)` (`log.c:903,906`) | `(code <digits>)` (`message_impl/mod.rs:101-105`) | EXACT |
| Source separator | `at <basename>(<line>)` parens-delimited (`log.c:903,906` and `util2.c:132-145`) | `at <workspace-relative-path>:<line>` colon-delimited (`message_impl/mod.rs:108-113`) | DIVERGENT |
| Source path style | basename only via `src_file()` | full workspace-relative path | DIVERGENT |
| Role trailer | `[<role>=<version>]` (`log.c:903,906`) | `[<role>=<version>]` (`message_impl/mod.rs:115-121`) | EXACT |
| Version in trailer | `3.4.1` (`version.h`) | own crate version (e.g. `0.5.8`); intentional - identifies the producing binary | EXACT (intentional difference) |
| Trailing `\n` | yes (`log.c:903,906`) | yes when `include_newline` set (`message_impl/mod.rs:123-125`) | EXACT |
| Severity for code 24 | warning (`log.c:902`) | warning (`strings.rs:117`) | EXACT |
| Severity for all other codes | error (`log.c:906`) | error (`strings.rs:90-131`) | EXACT |

Family totals: 11 EXACT, 3 DIVERGENT, 0 MISSING.

Project-memory cross-check: an earlier note in agent memory described
oc-rsync's trailer as `... (code N) at <repo-rel-path>:<line> [<role>=<version>]`
with the claim "upstream has no `[role=version]` trailer". Source evidence
contradicts that claim - `log.c:903` and `log.c:906` emit the trailer as
`[%s=%s]\n` keyed on `who_am_i()` and `rsync_version()`. The `[role=version]`
trailer is therefore upstream-faithful and must stay; the actual envelope
divergences are the source separator and the path style (see the row pair
above). The project-memory note should be revised accordingly.

## 6. Tally

| Family | EXACT | DIVERGENT | MISSING | Extra |
|---|---:|---:|---:|---:|
| 1. Connection / transport | 0 | 2 | 4 | 0 |
| 2. Protocol negotiation | 2 | 0 | 6 | 0 |
| 3. Daemon `@ERROR:` lines | 5 | 2 | 6 | 3 |
| 4. Per-file `rsyserr` warnings | 1 | 8 | 0 | 0 |
| 5. Exit-code envelope | 11 | 3 | 0 | 0 |
| **Total** | **19** | **15** | **16** | **3** |

## 7. Top 5 divergences by user impact

Ranked by visibility in real-world tooling (Ansible/backup-script greps,
log-aggregation alerting, monitoring dashboards). Severity reflects how
frequently the target string appears in published rsync wrapper scripts.

| Rank | ID | Severity | Title |
|---|---|---|---|
| 1 | C1 | High | `connection unexpectedly closed` never emitted - the canonical TCP-failure marker that Ansible, Bacula, rsnapshot, and BackupPC all grep on |
| 2 | E1 | High | Source-location separator uses `:` (oc-rsync) instead of `(...)` (upstream) in every `rsync error:` / `rsync warning:` envelope; log-parsing regex `at \S+\(\d+\)` fails |
| 3 | F1-F4 | Medium | `rsyserr` family (`link_stat`, `send_files`, `opendir`, `readdir`) drops upstream's `[<role>]` bracket immediately after `rsync:` and appends an extra source-location + role-version trailer that upstream omits for these per-file lines |
| 4 | C3 | Medium | `network read error` instead of upstream's bare `read error`; alerting rules keyed on `[%s] read error:` miss the event |
| 5 | D12/D13 | Medium | Daemon `module is read only` / `module is write only` emitted as `@ERROR:` greeting lines; upstream sends them through `MSG_ERROR` after the handshake using `ERROR:` (no `@`, no greeting-frame). Clients that branch on `@ERROR:` vs `ERROR:` mis-classify the error |

## 8. Recommended next-fix

**Pick: divergence E1 (envelope source separator and path style).** It is
the highest-leverage fix because every code-bearing envelope on every
platform passes through `Message::as_segments()`, so a single edit moves the
oc-rsync output from `at <workspace-path>:<line>` to upstream's
`at <basename>(<line>)` form for the entire envelope channel. Tooling that
greps `at \S+\(\d+\)` (the documented upstream form quoted in
`man rsync`(1)) starts matching immediately, and the change has no wire
impact.

Concrete edit:

1. `crates/core/src/message/message_impl/mod.rs:107-113` - swap the
   ` at `+`<path>`+`:`+`<digits>` sequence for
   ` at `+`<basename>`+`(`+`<digits>`+`)`. The basename is already available
   from `SourceLocation::file_basename` in
   `crates/core/src/message/source.rs`; switching the segment writer to use
   it - instead of the full workspace-relative path - matches upstream's
   `src_file()` behaviour (`util2.c:132-145`).
2. `crates/logging/src/error_format.rs:74-76,98-100` - mirror the same
   change in the fallback formatter so multi-line wrapped errors do not
   reintroduce the colon form.
3. Add a regression test in
   `crates/core/src/message/tests/part8.rs` asserting the rendered
   envelope matches `^rsync (error|warning): .+ \(code \d+\) at \S+\(\d+\) \[\w+=[\d.]+\]\n$`,
   keyed off `log_exit` example output captured under `LC_ALL=C` from
   `target/interop/upstream-src/rsync-3.4.1/`.
4. Update the project-memory entry that claims "upstream has no
   `[role=version]` trailer" to read instead "upstream emits
   `[role=version]` but uses `(line)` parens, not `:line` colons" so the
   audit trail stays correct.

This single change converts three rows in family 5 (envelope source
separator, envelope source path style, plus the project-memory drift) from
DIVERGENT to EXACT and unblocks downstream regex parity. C1 is the
second-highest-impact fix but requires a new emission site at the I/O
drain - file the follow-up issue against `crates/transfer/src/reader/` once
E1 lands.

## 9. Test plan for the recommended fix

- Add `tests/golden/error-envelope/error-code-23.txt` containing the
  byte-exact upstream output captured under `LC_ALL=C` from a forced
  `RERR_PARTIAL` exit; verify oc-rsync output matches via `diff -u`.
- Extend `crates/core/src/message/tests/part8.rs` with regex assertions
  that pin the new separator and path style.
- Capture upstream behaviour with:

```sh
# Force RERR_PARTIAL with a non-existent source.
rsync /nonexistent/path /tmp/dst 2>up.err; echo "exit=$?"
oc-rsync /nonexistent/path /tmp/dst 2>oc.err; echo "exit=$?"
diff -u up.err oc.err
```

## 10. Upstream source references

- `log.c:80-107` - `rerr_names[]` table.
- `log.c:453-473` - `rsyserr()` format
  (`rsync: [<role>] <msg>: <strerror> (<errno>)`).
- `log.c:884-910` - `log_exit()` envelope.
- `io.c:199-202` - `[<role>] io timeout after <N> seconds -- exiting`.
- `io.c:228-232` - `rsync: connection unexpectedly closed (<N> bytes received so far) [<role>]`.
- `io.c:804-806,847` - `read error` / `writefd_unbuffered failed`.
- `io.c:1703` - `unexpected tag %d`.
- `compat.c:609-636,872` - protocol-mismatch and daemon-auth wording.
- `clientserver.c:182-1386` - all `@ERROR:` greeting lines.
- `main.c:917,1149` - `ERROR: module is write only` / `ERROR: module is read only`.
- `main.c:1077,1222` - `Invalid packet at end of run` / `Your options have been rejected`.
- `receiver.c:947-968` - `failed verification -- update %s` matrix.
- `rsync.c:823-831` - `who_am_i()`.
- `util2.c:132-145` - `src_file()` basename-strip helper.
- `version.h` - `RSYNC_VERSION "3.4.1"`.

## 11. oc-rsync source references

- `crates/core/src/message/strings.rs:88-132` - `EXIT_CODE_TABLE`
  (mirrors `rerr_names[]`).
- `crates/core/src/message/severity.rs:64-69` - severity prefixes.
- `crates/core/src/message/message_impl/mod.rs:80-126` - envelope renderer
  (`Message::as_segments`).
- `crates/core/src/message/source.rs` - `SourceLocation` + `file_basename`.
- `crates/core/src/client/error.rs:202-252` - higher-level client errors.
- `crates/transfer/src/generator/file_list/walk.rs:129,149,215,246,327` -
  per-file `rsyserr`-equivalent emissions.
- `crates/transfer/src/generator/protocol_io.rs:142` - `send_files failed
  to open` line.
- `crates/transfer/src/pipeline/receiver.rs:277-291` - verification-fail
  WARNING/ERROR.
- `crates/transfer/src/transfer_ops/token_loop.rs:103,137` - `network read
  error` emission.
- `crates/daemon/src/daemon.rs:109-126` - daemon `@ERROR:` payload
  constants.
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:332,337` -
  module read/write-only emissions.
- `crates/protocol/src/error.rs:24-44` - protocol-negotiation error
  wording.
- `crates/logging/src/error_format.rs:74-100` - fallback formatter
  (mirrors the `Message::as_segments` separator).
