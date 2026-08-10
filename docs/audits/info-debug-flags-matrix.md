# `--info=FLAGS` and `--debug=FLAGS` verbosity matrix (#2112, #2113)

Tracking tasks: #2112 (`--info=FLAGS` audit) and #2113
(`--debug=FLAGS` audit). This audit compares oc-rsync's info/debug
verbosity flag system against upstream rsync 3.4.1.

Upstream source: `target/interop/upstream-src/rsync-3.4.1/options.c`
(lines 228-578) and `rsync.h` (lines 1416-1460).

oc-rsync sources: `crates/logging/src/` (flag enums, levels, config,
thread-local storage, macros) and `crates/cli/src/frontend/execution/flags/`
(CLI parsing for `--info` and `--debug`).

Last verified: 2026-05-13.

---

## 1. Upstream architecture

Upstream rsync uses two parallel arrays - `info_levels[COUNT_INFO]` and
`debug_levels[COUNT_DEBUG]` - indexed by `INFO_*` / `DEBUG_*` constants
defined in `rsync.h`. Output is guarded by macros:

```c
#define INFO_GTE(flag, lvl)  (info_levels[INFO_##flag] >= (lvl))
#define DEBUG_GTE(flag, lvl) (debug_levels[DEBUG_##flag] >= (lvl))
```

A priority system (`DEFAULT_PRIORITY` = 0, `USER_PRIORITY` = 2)
ensures that explicit `--info` / `--debug` flags override the
implicit levels set by `-v`.

## 2. Upstream info_words[] (13 flags)

Source: `options.c:270-284`.

| Index | Flag | Where | Upstream description | Max useful level |
|-------|------|-------|---------------------|-----------------|
| 0 | BACKUP | W_REC | Mention files backed up | 1 |
| 1 | COPY | W_REC | Mention files copied locally on the receiving side | 1 |
| 2 | DEL | W_REC | Mention deletions on the receiving side | 1 |
| 3 | FLIST | W_CLI | Mention file-list receiving/sending | 2 |
| 4 | MISC | W_SND\|W_REC | Mention miscellaneous information | 2 |
| 5 | MOUNT | W_SND\|W_REC | Mention mounts that were found or skipped | 1 |
| 6 | NAME | W_SND\|W_REC | Mention 1) updated file/dir names, 2) unchanged names | 2 |
| 7 | NONREG | W_REC | Mention skipped non-regular files (default 1, 0 disables) | 1 |
| 8 | PROGRESS | W_CLI | Mention 1) per-file progress or 2) total transfer progress | 2 |
| 9 | REMOVE | W_SND | Mention files removed on the sending side | 1 |
| 10 | SKIP | W_REC | Mention files skipped due to transfer overrides | 2 |
| 11 | STATS | W_CLI\|W_SRV | Mention statistics at end of run | 3 |
| 12 | SYMSAFE | W_SND\|W_REC | Mention symlinks that are unsafe | 1 |

## 3. Upstream debug_words[] (24 flags)

Source: `options.c:289-314`.

| Index | Flag | Where | Upstream description | Max useful level |
|-------|------|-------|---------------------|-----------------|
| 0 | ACL | W_SND\|W_REC | Debug extra ACL info | 1 |
| 1 | BACKUP | W_REC | Debug backup actions | 2 |
| 2 | BIND | W_CLI | Debug socket bind actions | 1 |
| 3 | CHDIR | W_CLI\|W_SRV | Debug when the current directory changes | 1 |
| 4 | CONNECT | W_CLI | Debug connection events | 2 |
| 5 | CMD | W_CLI | Debug commands+options that are issued | 2 |
| 6 | DEL | W_REC | Debug delete actions | 3 |
| 7 | DELTASUM | W_SND\|W_REC | Debug delta-transfer checksumming | 4 |
| 8 | DUP | W_REC | Debug weeding of duplicate names | 1 |
| 9 | EXIT | W_CLI\|W_SRV | Debug exit events | 3 |
| 10 | FILTER | W_SND\|W_REC | Debug filter actions | 3 |
| 11 | FLIST | W_SND\|W_REC | Debug file-list operations | 4 |
| 12 | FUZZY | W_REC | Debug fuzzy scoring | 2 |
| 13 | GENR | W_REC | Debug generator functions | 1 |
| 14 | HASH | W_SND\|W_REC | Debug hashtable code | 1 |
| 15 | HLINK | W_SND\|W_REC | Debug hard-link actions | 3 |
| 16 | ICONV | W_CLI\|W_SRV | Debug iconv character conversions | 2 |
| 17 | IO | W_CLI\|W_SRV | Debug I/O routines | 4 |
| 18 | NSTR | W_CLI\|W_SRV | Debug negotiation strings | 1 |
| 19 | OWN | W_REC | Debug ownership changes in users & groups | 2 |
| 20 | PROTO | W_CLI\|W_SRV | Debug protocol information | 1 |
| 21 | RECV | W_REC | Debug receiver functions | 1 |
| 22 | SEND | W_SND | Debug sender functions | 1 |
| 23 | TIME | W_REC | Debug setting of modified times | 2 |

## 4. Upstream `-v` to flag mapping

Source: `options.c:228-243`, `set_output_verbosity()` at line 513.

The mapping is cumulative - each level includes all flags from lower
levels. `info_verbosity[0]` is always applied (even without `-v`).

### info_verbosity[]

| Level | Flags added |
|-------|------------|
| 0 | NONREG |
| 1 | COPY, DEL, FLIST, MISC, NAME, STATS, SYMSAFE |
| 2 | BACKUP, MISC2, MOUNT, NAME2, REMOVE, SKIP |

Info flags are fully set by `-vv`; higher verbosity levels add no more
info flags.

### debug_verbosity[]

| Level | Flags added |
|-------|------------|
| 0 | (none) |
| 1 | (none) |
| 2 | BIND, CMD, CONNECT, DEL, DELTASUM, DUP, FILTER, FLIST, ICONV |
| 3 | ACL, BACKUP, CONNECT2, DELTASUM2, DEL2, EXIT, FILTER2, FLIST2, FUZZY, GENR, OWN, RECV, SEND, TIME |
| 4 | CMD2, DELTASUM3, DEL3, EXIT2, FLIST3, ICONV2, OWN2, PROTO, TIME2 |
| 5 | CHDIR, DELTASUM4, FLIST4, FUZZY2, HASH, HLINK |

### Effective per-flag levels at each -v count

| Flag | -v=0 | -v=1 | -vv | -vvv | -vvvv | -vvvvv |
|------|------|------|-----|------|-------|--------|
| **Info** |||||||
| BACKUP | 0 | 0 | 1 | 1 | 1 | 1 |
| COPY | 0 | 1 | 1 | 1 | 1 | 1 |
| DEL | 0 | 1 | 1 | 1 | 1 | 1 |
| FLIST | 0 | 1 | 1 | 1 | 1 | 1 |
| MISC | 0 | 1 | 2 | 2 | 2 | 2 |
| MOUNT | 0 | 0 | 1 | 1 | 1 | 1 |
| NAME | 0 | 1 | 2 | 2 | 2 | 2 |
| NONREG | 1 | 1 | 1 | 1 | 1 | 1 |
| PROGRESS | 0 | 0 | 0 | 0 | 0 | 0 |
| REMOVE | 0 | 0 | 1 | 1 | 1 | 1 |
| SKIP | 0 | 0 | 1 | 1 | 1 | 1 |
| STATS | 0 | 1 | 1 | 1 | 1 | 1 |
| SYMSAFE | 0 | 1 | 1 | 1 | 1 | 1 |
| **Debug** |||||||
| ACL | 0 | 0 | 0 | 1 | 1 | 1 |
| BACKUP | 0 | 0 | 0 | 1 | 1 | 1 |
| BIND | 0 | 0 | 1 | 1 | 1 | 1 |
| CHDIR | 0 | 0 | 0 | 0 | 0 | 1 |
| CMD | 0 | 0 | 1 | 1 | 2 | 2 |
| CONNECT | 0 | 0 | 1 | 2 | 2 | 2 |
| DEL | 0 | 0 | 1 | 2 | 3 | 3 |
| DELTASUM | 0 | 0 | 1 | 2 | 3 | 4 |
| DUP | 0 | 0 | 1 | 1 | 1 | 1 |
| EXIT | 0 | 0 | 0 | 1 | 2 | 2 |
| FILTER | 0 | 0 | 1 | 2 | 2 | 2 |
| FLIST | 0 | 0 | 1 | 2 | 3 | 4 |
| FUZZY | 0 | 0 | 0 | 1 | 1 | 2 |
| GENR | 0 | 0 | 0 | 1 | 1 | 1 |
| HASH | 0 | 0 | 0 | 0 | 0 | 1 |
| HLINK | 0 | 0 | 0 | 0 | 0 | 1 |
| ICONV | 0 | 0 | 1 | 1 | 2 | 2 |
| IO | 0 | 0 | 0 | 0 | 0 | 0 |
| NSTR | 0 | 0 | 0 | 0 | 0 | 0 |
| OWN | 0 | 0 | 0 | 1 | 2 | 2 |
| PROTO | 0 | 0 | 0 | 0 | 1 | 1 |
| RECV | 0 | 0 | 0 | 1 | 1 | 1 |
| SEND | 0 | 0 | 0 | 1 | 1 | 1 |
| TIME | 0 | 0 | 0 | 1 | 2 | 2 |

Note: IO and NSTR are never set by `-v` at any level. They are only
accessible via explicit `--debug=io` or `--debug=nstr`.

## 5. oc-rsync implementation status

### 5.1 Flag definitions - COMPLETE

All 13 info flags and 24 debug flags are defined with matching names:

- `crates/logging/src/levels/info.rs`: `InfoFlag` enum, `InfoLevels` struct.
- `crates/logging/src/levels/debug.rs`: `DebugFlag` enum, `DebugLevels` struct.

The flag names map exactly to upstream: `InfoFlag::Backup` = `INFO_BACKUP`,
`DebugFlag::Deltasum` = `DEBUG_DELTASUM`, etc.

### 5.2 Verbosity-to-flag mapping - COMPLETE

`VerbosityConfig::from_verbose_level()` in `crates/logging/src/config.rs`
implements the cumulative mapping from `-v` level to individual flag
levels. Verified that all 6 levels (0-5) match the upstream tables
exactly. Levels above 5 are clamped to level 5, matching upstream's
`MAX_VERBOSITY`.

### 5.3 CLI parsing - COMPLETE

Two parallel parsing systems exist, both complete:

1. **`crates/cli/src/frontend/execution/flags/info.rs`** -
   `InfoFlagSettings` with level validation per flag (e.g., FLIST max 2,
   STATS max 3, PROGRESS max 2). Supports `all`, `none`, `help`,
   `no-` prefix negation, `-` prefix negation, and `0` suffix negation.

2. **`crates/cli/src/frontend/execution/flags/debug.rs`** -
   `DebugFlagSettings` with level validation per flag (e.g., BACKUP max 2,
   DELTASUM max 4, IO max 4, FLIST max 4). Supports `all`, `none`,
   `help`, `no-` prefix, `-` prefix, and `0` suffix negation.

3. **`crates/logging/src/config.rs`** - `apply_info_flag()` and
   `apply_debug_flag()` for the logging crate's own `VerbosityConfig`.

4. **`crates/cli/src/frontend/info_output.rs`** - `InfoFlags` wrapper
   with `parse_info_flags()` for the legacy code path.

### 5.4 Help output - COMPLETE

`--info=help` and `--debug=help` emit help text that matches upstream's
`output_item_help()` output format. The help text strings are defined as
constants `INFO_HELP_TEXT` and `DEBUG_HELP_TEXT` in the execution flags
module. All 13 info flags and 24 debug flags are listed with their
upstream descriptions and supported level ranges.

### 5.5 Thread-local storage - COMPLETE

`crates/logging/src/thread_local.rs` provides `info_gte()` and
`debug_gte()` functions mirroring upstream's `INFO_GTE()` and
`DEBUG_GTE()` macros. Per-thread `RefCell<VerbosityConfig>` replaces
upstream's global arrays, supporting multi-threaded transfer pipelines.

### 5.6 Logging macros - COMPLETE

`info_log!` and `debug_log!` macros gate formatting behind level checks,
matching upstream's pattern of checking `INFO_GTE()`/`DEBUG_GTE()` before
formatting.

## 6. Runtime output coverage per flag

The following table lists how many `info_log!`/`debug_log!` call sites
exist in production code (excluding tests and the logging crate itself)
for each flag, compared to the number of `INFO_GTE`/`DEBUG_GTE` usage
sites in upstream.

### Info flags

| Flag | Upstream usage sites | oc-rsync call sites | Status |
|------|---------------------|--------------------|--------|
| BACKUP | 6 | 0 | Not emitting |
| COPY | 1 | 0 | Not emitting |
| DEL | 2 | 7 | Covered |
| FLIST | 7 | 6 | Covered |
| MISC | 3 | 0 | Not emitting |
| MOUNT | 3 | 0 | Not emitting |
| NAME | 14 | 10 | Covered |
| NONREG | 3 | 0 | Not emitting |
| PROGRESS | 12 | 0 | Handled via separate progress system |
| REMOVE | 1 | 1 | Covered |
| SKIP | 7 | 2 | Partially covered |
| STATS | 5 | 0 | Handled via separate stats system |
| SYMSAFE | 3 | 0 | Not emitting |

Notes:
- PROGRESS and STATS are handled through dedicated subsystems
  (`crates/cli/src/frontend/progress/` and
  `crates/cli/src/frontend/stats_format.rs`) rather than through
  `info_log!` calls. The flag levels are checked at a higher level to
  control whether these subsystems are activated.
- COPY, BACKUP, MISC, MOUNT, NONREG, and SYMSAFE have zero `info_log!`
  call sites, meaning the flags are parsed and stored but never produce
  output.

### Debug flags

| Flag | Upstream usage sites | oc-rsync call sites | Status |
|------|---------------------|--------------------|--------|
| ACL | 1 | 0 | Not emitting |
| BACKUP | 6 | 0 | Not emitting |
| BIND | 1 | 0 | Not emitting |
| CHDIR | 1 | 0 | Not emitting |
| CMD | 5 | 0 | Not emitting |
| CONNECT | 4 | 7 | Covered |
| DEL | 5 | 1 | Partially covered |
| DELTASUM | 15 | 2 | Partially covered |
| DUP | 1 | 1 | Covered |
| EXIT | 8 | 1 | Partially covered |
| FILTER | 17 | 5 | Partially covered |
| FLIST | 23 | 23 | Covered |
| FUZZY | 3 | 0 | Not emitting |
| GENR | 7 | 0 | Not emitting |
| HASH | 3 | 0 | Not emitting |
| HLINK | 8 | 0 | Not emitting |
| ICONV | 2 | 0 | Not emitting |
| IO | 25 | 15 | Partially covered |
| NSTR | 5 | 0 | Not emitting |
| OWN | 3 | 0 | Not emitting |
| PROTO | 1 | 6 | Covered |
| RECV | 8 | 1 | Partially covered |
| SEND | 7 | 0 | Not emitting |
| TIME | 2 | 0 | Not emitting |

## 7. Gaps

### 7.1 Priority system not implemented

Upstream uses a 4-level priority system (`DEFAULT_PRIORITY`,
`HELP_PRIORITY`, `USER_PRIORITY`, `LIMIT_PRIORITY`) to control
precedence when both `-v` and `--info`/`--debug` are specified.
`USER_PRIORITY` (from `--info`/`--debug`) overrides `DEFAULT_PRIORITY`
(from `-v`). oc-rsync applies `--info`/`--debug` flags after the
verbose level is set, achieving the same override effect by
last-writer-wins. This works correctly for the common case but does
not support `limit_output_verbosity()` (used by the daemon to cap
client-requested verbosity).

### 7.2 `limit_output_verbosity()` not implemented

Upstream provides `limit_output_verbosity()` (options.c:527) which lets
the daemon cap the client's verbosity to a configured maximum. oc-rsync
has no equivalent. This matters for daemon mode where an untrusted
client should not be able to flood the log with `-vvvvv`.

### 7.3 `negate_output_levels()` not implemented

Upstream's `negate_output_levels()` (options.c:569) negates all
info/debug levels, used when forwarding options to the remote server
(so the server knows to suppress its own output). oc-rsync handles
server-side verbosity suppression through separate mechanisms but does
not implement level negation.

### 7.4 `reset_output_levels()` not implemented

Upstream's `reset_output_levels()` (options.c:555) zeroes all levels
and resets priorities. Used by the help system and on config reload.
oc-rsync has no direct equivalent, though constructing a new
`VerbosityConfig::default()` achieves the same effect.

### 7.5 `make_output_option()` not implemented

Upstream's `make_output_option()` (options.c:344) serializes current
flag levels back into `--info=...` / `--debug=...` strings for
forwarding to the remote server. oc-rsync does not yet need this
because it builds server args through a different mechanism, but it
will be needed for full remote-shell parity.

### 7.6 Info flags not producing output

The following info flags are parsed and stored but produce no runtime
output:

- **BACKUP** - no `info_log!(Backup, ...)` calls. Upstream emits
  "backed up %s to %s" in `backup.c`, `generator.c`, and `main.c`.
- **COPY** - no `info_log!(Copy, ...)` calls. Upstream emits
  "copying unsafe symlink" and local-copy messages in `generator.c`.
- **MISC** - no `info_log!(Misc, ...)` calls. Upstream emits
  miscellaneous messages in `batch.c`, `io.c`, `util1.c`.
- **MOUNT** - no `info_log!(Mount, ...)` calls. Upstream emits
  mount-skip messages in `flist.c` and `generator.c`.
- **NONREG** - no `info_log!(Nonreg, ...)` calls. Upstream emits
  "skipping non-regular file" in `backup.c` and `generator.c`.
- **SYMSAFE** - no `info_log!(Symsafe, ...)` calls. Upstream emits
  "skipping unsafe symlink" in `backup.c` and `flist.c`.

### 7.7 Debug flags not producing output

The following debug flags are parsed and stored but produce no runtime
output:

- **ACL** - no call sites. Upstream uses it in `acls.c`.
- **BACKUP** - no call sites. Upstream uses it in `backup.c`.
- **BIND** - no call sites. Upstream uses it in `socket.c`.
- **CHDIR** - no call sites. Upstream uses it in `util1.c`.
- **CMD** - no call sites. Upstream uses it in `clientserver.c`,
  `main.c`, `pipe.c`, `rsync.c`, `socket.c`.
- **FUZZY** - no call sites. Upstream uses it in `generator.c`.
- **GENR** - no call sites. Upstream uses it in `generator.c`.
- **HASH** - no call sites. Upstream uses it in `hashtable.c`.
- **HLINK** - no call sites. Upstream uses it in `flist.c`, `hlink.c`.
- **ICONV** - no call sites. Upstream uses it in `rsync.c`.
- **NSTR** - no call sites. Upstream uses it in `checksum.c`,
  `compat.c`.
- **OWN** - no call sites. Upstream uses it in `rsync.c`, `uidlist.c`.
- **SEND** - no call sites. Upstream uses it in `main.c`, `sender.c`.
- **TIME** - no call sites. Upstream uses it in `generator.c`,
  `util1.c`.

### 7.8 Partially covered flags

The following flags have some call sites but significantly fewer than
upstream:

- **Info SKIP** - 2 vs 7 upstream sites. Missing: generator skip
  messages for max-size, min-size, existing-only, ignore-existing.
- **Debug DEL** - 1 vs 5 upstream sites. Missing: detailed delete
  recursion and non-empty directory messages.
- **Debug DELTASUM** - 2 vs 15 upstream sites. Missing: per-block
  hash detail, match/miss reporting, token counting, sum statistics.
- **Debug EXIT** - 1 vs 8 upstream sites. Missing: `_exit_cleanup`
  chain messages, I/O error exit detail.
- **Debug FILTER** - 5 vs 17 upstream sites. Missing: dir-merge
  filter loading, CVS exclusion, per-file rule evaluation detail.
- **Debug IO** - 15 vs 25 upstream sites. Missing: multiplex frame
  I/O tracing, batch-mode I/O, tagged write detail.
- **Debug RECV** - 1 vs 8 upstream sites. Missing: receiver phase
  transitions, file-open detail, delta-apply tracing.

### 7.9 `W_CLI`/`W_SRV`/`W_SND`/`W_REC` side filtering not implemented

Upstream tags each flag with which side should emit it (client, server,
sender, receiver). For example, `INFO_FLIST` is `W_CLI` (client-only)
while `INFO_NAME` is `W_SND|W_REC`. oc-rsync does not enforce these
restrictions - all flags are checked on all sides. This can cause
duplicate or unexpected output in remote transfers where both client
and server process would check the same flags.

### 7.10 `ALL` with level suffix handling differences

Upstream supports `ALL2` (set all flags to level 2), `ALL3`, etc.
oc-rsync supports this in the `crates/cli/src/frontend/info_output.rs`
parser but the `crates/cli/src/frontend/execution/flags/info.rs`
parser only recognizes plain `all` (mapped to level 1). It does not
support `all2`, `all3`, etc.

## 8. Recommendations

### P0 - Required for correct interop

1. **Implement `make_output_option()` serialization.** When oc-rsync
   spawns a remote server over SSH, it must forward the effective
   `--info` and `--debug` settings so the remote side's verbosity
   matches. Without this, remote transfers may have mismatched
   verbosity output.

2. **Implement `limit_output_verbosity()` for daemon mode.** The
   daemon must be able to cap client verbosity. Without this, a
   malicious client can flood daemon logs with `-vvvvv`.

3. **Wire the `W_CLI`/`W_SND`/`W_REC`/`W_SRV` side filtering.**
   Each flag should only emit on the appropriate side to avoid
   duplicate output in remote transfers.

### P1 - Required for complete output parity

4. **Add `info_log!` calls for BACKUP, COPY, MISC, MOUNT, NONREG,
   and SYMSAFE.** These flags are fully parsed but silently ignored.
   Users who specify `--info=backup` or `--info=copy` get no output.

5. **Add `debug_log!` calls for ACL, BACKUP, BIND, CHDIR, CMD,
   FUZZY, GENR, HASH, HLINK, ICONV, NSTR, OWN, SEND, and TIME.**
   These flags are fully parsed but produce no output.

6. **Increase coverage for partially covered flags** (SKIP, DEL,
   DELTASUM, EXIT, FILTER, IO, RECV) to match upstream call-site
   count and message content.

### P2 - Nice to have

7. **Implement priority system.** The current last-writer-wins
   approach works for CLI but is not fully correct for the priority
   precedence that upstream guarantees.

8. **Support `ALLN` syntax in execution flags parser.** The
   `all2`, `all3`, `all4` forms should work in the execution-level
   parser, not just the `info_output.rs` parser.

9. **Implement `negate_output_levels()` for server-side
   suppression.** Currently handled through other mechanisms but
   exact parity would improve compatibility with older clients.

## 9. Summary

| Area | Status |
|------|--------|
| Flag definitions (enums, structs) | Complete - all 13 info + 24 debug flags match upstream |
| `-v` level mapping | Complete - all 6 levels (0-5) match upstream exactly |
| CLI parsing (`--info=`, `--debug=`) | Complete - all flags, levels, negation, all/none/help |
| Help output | Complete - matches upstream format and content |
| Thread-local level checking | Complete - `info_gte()`/`debug_gte()` mirror `INFO_GTE()`/`DEBUG_GTE()` |
| Runtime output production | Partial - 15 of 37 flags have zero call sites |
| Priority system | Not implemented |
| Verbosity limiting (daemon) | Not implemented |
| Level negation (remote server) | Not implemented |
| Option serialization (remote) | Not implemented |
| Side filtering (W_CLI/W_SND/etc.) | Not implemented |
