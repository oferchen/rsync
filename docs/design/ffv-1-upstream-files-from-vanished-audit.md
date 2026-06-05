# FFV-1: Upstream --files-from Vanished File Behavior Audit

## Summary

This document audits upstream rsync 3.4.1's behavior when a file listed in
`--files-from` (or specified as a command-line source argument) cannot be
found on disk. It covers the three modes controlled by `missing_args`, the
warning/error messages emitted, and the resulting exit codes. It then
compares oc-rsync's current implementation against upstream.

## Upstream Behavior

### Option Parsing (options.c)

The `missing_args` global variable controls behavior (options.c:95):

```c
int missing_args = 0; /* 0 = FERROR_XFER, 1 = ignore, 2 = delete */
```

Both options use `POPT_BIT_SET`, so they OR their value into `missing_args`
(options.c:718-719):

```c
{"delete-missing-args", 0, POPT_BIT_SET, &missing_args, 2, 0, 0},
{"ignore-missing-args", 0, POPT_BIT_SET, &missing_args, 1, 0, 0},
```

If both options are specified, `missing_args` becomes 3 (1|2), which is
simplified to 2 at options.c:2218-2219:

```c
if (missing_args == 3)
    missing_args = 2;
```

`--delete-missing-args` requires delete cooperation from both sides -
the sender sends the option to the server (options.c:2848-2853). The sender
can handle `--ignore-missing-args` by itself, so it is only forwarded to
the remote side when the remote is the sender (`!am_sender`).

### send_file_list() Stat Failure Handling (flist.c:2390-2409)

The same loop processes both `--files-from` entries and command-line argv
entries. When `link_stat()` fails, the behavior is determined by
`missing_args`:

```c
if (link_stat(fbuf, &st, copy_dirlinks || name_type != NORMAL_NAME) != 0
 || (name_type != DOTDIR_NAME && is_excluded(...))
 || (relative_paths && path_is_daemon_excluded(...))) {
    if (errno != ENOENT || missing_args == 0) {
        /* Non-ENOENT errors, or ENOENT with default mode */
        if (errno != ENOENT)
            io_error |= IOERR_GENERAL;
        rsyserr(FERROR_XFER, errno, "link_stat %s failed",
            full_fname(fbuf));
        continue;
    } else if (missing_args == 1) {
        /* --ignore-missing-args: silently skip */
        continue;
    } else /* (missing_args == 2) */ {
        /* --delete-missing-args: send as mode-0 "missing" entry */
        memset(&st, 0, sizeof st);
    }
}
```

### Mode 0: Default (no flags)

When a file from `--files-from` returns ENOENT on `link_stat()`:

- **Warning**: `rsync: [sender] link_stat "<path>" failed: No such file or directory (2)`
  - Emitted via `rsyserr(FERROR_XFER, errno, "link_stat %s failed", ...)`
  - FERROR_XFER is logged as an error on both the local and remote side
- **io_error**: `IOERR_GENERAL` is NOT set for ENOENT (only for non-ENOENT errors)
  - However, the FERROR_XFER message itself sets `got_xfer_error` in the receiver
- **Exit code**: 23 (RERR_PARTIAL) - "some files/attrs were not transferred"
  - The FERROR_XFER message triggers the partial-transfer exit code

For non-ENOENT errors (e.g., EACCES), `IOERR_GENERAL` is explicitly set,
which also maps to exit code 23.

### Mode 1: --ignore-missing-args

- **Warning**: None. The entry is silently skipped.
- **io_error**: Not modified.
- **Exit code**: 0 (success), assuming no other errors occur.

This only applies to initial source argument resolution. Files that vanish
later during the transfer (after initial stat succeeded) still produce the
normal "file has vanished" warning and exit code 24 (RERR_VANISHED).

### Mode 2: --delete-missing-args

- **Warning**: None during file list building.
- **io_error**: Not modified during file list building.
- **Wire format**: The missing file is sent as a file list entry with `mode = 0`
  (all stat fields zeroed). This is identified by `IS_MISSING_FILE(st)` macro
  (`rsync.h:917: #define IS_MISSING_FILE(statbuf) ((statbuf).st_mode == 0)`).
- **Generator behavior** (generator.c:1348-1354): When the generator encounters
  a mode-0 entry, it calls `delete_item()` on the corresponding destination
  path, removing it if it exists.
- **List-only output** (generator.c:1159-1162): Missing entries display as
  `*missing` instead of the normal permissions/size/date line.
- **Exit code**: 0 (success), assuming the deletion succeeds.

### Server-Side Argument Forwarding (options.c:2848-2853)

```c
/* --delete-missing-args needs the cooperation of both sides, but
 * the sender can handle --ignore-missing-args by itself. */
if (missing_args == 2)
    args[ac++] = "--delete-missing-args";
else if (missing_args == 1 && !am_sender)
    args[ac++] = "--ignore-missing-args";
```

`--delete-missing-args` always forwards to the server because the generator
(receiver side) must know to delete matching destination entries.
`--ignore-missing-args` only forwards when the remote is the sender, since
the local sender can handle it autonomously.

## oc-rsync Current State

### Flag Parsing - IMPLEMENTED

Both flags are fully parsed in the CLI layer:

- `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs:314-322` -
  Clap argument definitions for both `--ignore-missing-args` and `--delete-missing-args`.
- `crates/cli/src/frontend/arguments/parser/mod.rs:152-155` - Parser logic
  correctly implements upstream's "delete implies ignore" semantics.
- `crates/cli/src/frontend/arguments/parsed_args/mod.rs:183-227` - Fields on
  `ParsedArgs`.

### Config Propagation - IMPLEMENTED

Both flags propagate through the config chain:

- `ParsedArgs` -> `CoreConfigBuilder` (cli drive/config.rs:204,250)
- `CoreConfigBuilder` -> `ClientConfig` (core builder/mod.rs:140,187,368,415)
- `ClientConfig` -> `LocalCopyOptions` (core client/run/mod.rs:624-625)
- `LocalCopyOptions` stores both flags (engine options/types.rs:99,184)

### Local Copy Behavior - IMPLEMENTED

The local-copy executor implements both modes for direct source arguments
(`crates/engine/src/local_copy/executor/sources/metadata.rs:29-51`):

- ENOENT + `delete_missing_args` -> calls `delete_missing_source_entry()` to
  remove the destination entry.
- ENOENT + `ignore_missing_args` -> returns `Handled` (silently skipped).
- ENOENT + neither flag -> returns `NotFoundError`, which is reported as an
  error and contributes to exit code 23.

### Remote Transfer (Sender) - NOT IMPLEMENTED

The transfer crate's `GeneratorContext` has zero references to
`ignore_missing_args` or `delete_missing_args`. The flags are not wired
into the generator's file list builder.

When `build_file_list_with_base()` processes `--files-from` entries for
remote transfers, it calls `walk_path()` for each entry. If `walk_path()`
encounters ENOENT, it:

1. Prints `file has vanished: "<path>"` (walk.rs:337)
2. Sets `IOERR_VANISHED` via `record_io_error()` (walk.rs:43)
3. Skips the entry and continues

This means:

- `--ignore-missing-args` has no effect during remote transfers. Missing
  files still produce the "file has vanished" warning and set
  `IOERR_VANISHED` (exit code 24).
- `--delete-missing-args` has no effect during remote transfers. Missing
  files are not sent as mode-0 entries, so the generator never deletes the
  corresponding destination entries.

### Server-Side Argument Forwarding - NOT AUDITED

Whether oc-rsync forwards `--ignore-missing-args` / `--delete-missing-args`
in server args for remote shell transfers has not been verified in this
audit.

## Gap Analysis

| Capability | Upstream | oc-rsync | Status |
|---|---|---|---|
| Parse `--ignore-missing-args` | Yes | Yes | OK |
| Parse `--delete-missing-args` | Yes | Yes | OK |
| `delete` implies `ignore` | Yes | Yes | OK |
| Both flags -> simplify to `delete` | Yes (3->2) | Yes (parser) | OK |
| Local copy: ignore missing | Yes | Yes | OK |
| Local copy: delete missing | Yes | Yes | OK |
| Local copy: default ENOENT error | Yes (exit 23) | Yes (exit 23) | OK |
| Remote sender: ignore missing | Silently skip, exit 0 | Warns + exit 24 | **GAP** |
| Remote sender: delete missing | Send mode-0, generator deletes | Not implemented | **GAP** |
| Remote sender: default ENOENT | FERROR_XFER, exit 23 | "vanished" + exit 24 | **GAP** |
| Generator: mode-0 delete logic | Deletes dest entry | Not implemented | **GAP** |
| `--list-only` `*missing` display | Shows `*missing` | Not implemented | **GAP** |
| Server args forwarding | Conditional per mode | Not audited | **UNKNOWN** |

### Default ENOENT Exit Code Divergence

Even without `--ignore-missing-args` or `--delete-missing-args`, there is a
behavioral difference in the remote sender path:

- **Upstream**: ENOENT on an initial source arg emits `link_stat %s failed`
  via `FERROR_XFER`, does NOT set `IOERR_GENERAL` (the `errno != ENOENT`
  guard at flist.c:2396 skips it), but the `FERROR_XFER` message itself
  triggers exit code 23 (RERR_PARTIAL) on the client side.
- **oc-rsync**: ENOENT on an initial source arg calls `record_io_error()`
  which sets `IOERR_VANISHED` (exit 24, RERR_VANISHED). The warning
  message is "file has vanished" rather than upstream's
  `link_stat "<path>" failed: No such file or directory (2)`.

This is incorrect. Upstream distinguishes between:

1. **Initial source arg missing** (flist.c:2398) - `link_stat %s failed` -
   treated as a transfer error, exit 23.
2. **File vanished during scan** (flist.c:1289) - `file has vanished: %s` -
   treated as a vanished warning, exit 24.

oc-rsync conflates both cases as "vanished" in the remote sender path.

## Upstream Examples

### Default mode (no flags)

```
$ rsync --files-from=list.txt /src/ /dst/
rsync: [sender] link_stat "/src/nonexistent.txt" failed: No such file or directory (2)
rsync error: some files/attrs were not transferred (code 23) at main.c(1337) [sender=3.4.1]
```

### --ignore-missing-args

```
$ rsync --ignore-missing-args --files-from=list.txt /src/ /dst/
(no output, exit 0)
```

### --delete-missing-args

```
$ rsync --delete-missing-args --files-from=list.txt /src/ /dst/
(no output, exit 0; destination entries for missing sources are deleted)
```

### --list-only with --delete-missing-args

```
$ rsync --list-only --delete-missing-args nonexistent.txt /dst/
            *missing nonexistent.txt
```

## Recommended Follow-Up Tasks

1. **FFV-2**: Wire `missing_args` config into `GeneratorContext` and
   `build_file_list_with_base()`. Implement the three-mode branch in the
   remote sender's stat failure path to match upstream flist.c:2393-2409.

2. **FFV-3**: Implement mode-0 `FileEntry` (missing file sentinel) in the
   protocol flist encoding. Add `IS_MISSING_FILE` predicate. Wire into
   generator deletion logic.

3. **FFV-4**: Fix the default ENOENT exit code divergence - initial source
   arg ENOENT should produce `link_stat %s failed` and exit 23, not
   "file has vanished" and exit 24.

4. **FFV-5**: Implement `*missing` display for `--list-only` output.

5. **FFV-6**: Audit and implement server-side argument forwarding for both
   flags in the remote-shell argument builder.
