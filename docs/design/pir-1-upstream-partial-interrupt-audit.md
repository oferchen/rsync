# PIR-1: Upstream rsync Partial-Interrupt Signal Handler Audit

Reference: upstream rsync 3.4.1 source at `target/interop/upstream-src/rsync-3.4.1/`.

This document traces the exact sequence of events when a signal interrupts a
transfer while `--partial` is active. It is the reference spec for implementing
partial-interrupt parity in oc-rsync.

---

## 1. Signal Installation

Signals are registered in `main.c:1717-1790` after option parsing:

| Signal   | Handler            | Registered at |
|----------|--------------------|---------------|
| SIGUSR1  | `sigusr1_handler`  | main.c:1725   |
| SIGUSR2  | `sigusr2_handler`  | main.c:1726   |
| SIGCHLD  | `remember_children`| main.c:1727   |
| SIGINT   | `sig_int`          | main.c:1788   |
| SIGHUP   | `sig_int`          | main.c:1789   |
| SIGTERM  | `sig_int`          | main.c:1790   |
| SIGPIPE  | SIG_IGN            | main.c:1797   |

SIGINT, SIGHUP, and SIGTERM share the same handler: `sig_int()` in rsync.c:685.

## 2. Signal-to-Cleanup Sequence

### 2a. `sig_int()` (rsync.c:685-717)

```c
void sig_int(int sig_num)
{
    called_from_signal_handler = 1;
    msleep(400);  // let ssh restore tty settings

    // Daemon listener + SIGTERM => clean exit
    if (am_daemon && !am_server && sig_num == SIGTERM)
        exit_cleanup(0);

    // Server or receiver: defer signal for controlled shutdown
    if (!got_kill_signal && (am_server || am_receiver)) {
        got_kill_signal = sig_num;
        called_from_signal_handler = 0;  // reset - not immediate exit
        return;                          // deferred handling
    }

    exit_cleanup(RERR_SIGNAL);  // immediate cleanup
}
```

**Two paths diverge here:**

- **Generator/client (sender side):** Calls `_exit_cleanup(RERR_SIGNAL)` immediately
  from the signal handler. `called_from_signal_handler = 1` is set, so the
  process terminates via `_exit()` (not `exit()`) at cleanup.c:273.

- **Server or receiver:** Sets `got_kill_signal = sig_num` and returns. The
  signal is handled later at the next I/O operation, when `perform_io()` checks
  `got_kill_signal > 0` (io.c:750, 879, 901) and calls `handle_kill_signal(True)`.
  This calls `exit_cleanup(RERR_SIGNAL)` with `flush_ok_after_signal = True`,
  allowing the multiplexed output to be flushed so the remote side learns what
  happened.

### 2b. `sigusr1_handler()` (main.c:1597-1601)

```c
static void sigusr1_handler(UNUSED(int val))
{
    called_from_signal_handler = 1;
    exit_cleanup(RERR_SIGNAL1);
}
```

No deferral. Immediate cleanup. SIGUSR1 is the signal rsync uses to propagate
shutdown to child processes (cleanup.c:203).

### 2c. `handle_kill_signal()` (io.c:515-520)

```c
static void handle_kill_signal(BOOL flush_ok)
{
    got_kill_signal = -1;
    flush_ok_after_signal = flush_ok;
    exit_cleanup(RERR_SIGNAL);
}
```

Bridges the deferred path into `_exit_cleanup`. The `flush_ok` flag controls
whether step 4 of cleanup can flush I/O.

## 3. `_exit_cleanup()` Step-by-Step (cleanup.c:103-275)

The function uses `switch_step` and `case_N.h` includes to create a
non-repeatable, re-entrant state machine. Each `#include "case_N.h"` generates
the next `case N:` label. If cleanup triggers a recursive call (e.g., an I/O
error during flush), the switch picks up at the next step.

### Step 0: Entry (cleanup.c:127-141)
- Masks SIGUSR1 and SIGUSR2 with SIG_IGN.
- Preserves first error code.
- Sets `am_server = 2` on clean exit (suppresses log_exit output).
- Emits newline if `output_needs_newline`.
- Debug logging if EXIT >= 2.

### Step 1: Reap Child (cleanup.c:146-154)
- If `cleanup_child_pid != -1`, calls `wait_process(WNOHANG)`.
- Propagates child exit code upward if it is larger than current `exit_code`.

### Step 2: Partial File Handling (cleanup.c:159-183)

**This is the critical step for --partial.**

```c
if (cleanup_got_literal && (cleanup_fname || cleanup_fd_w != -1)) {
    // close read fd
    if (cleanup_fd_r != -1) {
        close(cleanup_fd_r);
        cleanup_fd_r = -1;
    }
    // flush and close write fd
    if (cleanup_fd_w != -1) {
        flush_write_file(cleanup_fd_w);
        close(cleanup_fd_w);
        cleanup_fd_w = -1;
    }
    // retain partial file if conditions met
    if (cleanup_fname && cleanup_new_fname && keep_partial
     && handle_partial_dir(cleanup_new_fname, PDIR_CREATE)) {
        int tweak_modtime = 0;
        const char *fname = cleanup_fname;
        cleanup_fname = NULL;         // prevent step 5 from unlinking
        if (!partial_dir) {
            tweak_modtime = 1;
            cleanup_file->modtime = 0;
        }
        finish_transfer(cleanup_new_fname, fname, NULL, NULL,
                        cleanup_file, tweak_modtime, !partial_dir);
    }
}
```

### Step 3: Flush I/O (cleanup.c:189-195)
- If `flush_ok_after_signal` and code is `RERR_SIGNAL`, flushes multiplexed I/O.
- On clean exit (code 0), flushes all I/O.

### Step 4: Unlink and Kill Children (cleanup.c:200-226)
- If `cleanup_fname` is still set (i.e., step 2 did NOT retain the file),
  `do_unlink(cleanup_fname)` removes the temp file.
- If exiting with error, `kill_all(SIGUSR1)` sends SIGUSR1 to all child processes.
- Removes daemon PID file if applicable.
- Calculates final exit code from `io_error` flags and `got_xfer_error`.

### Step 5: Log (cleanup.c:223-226)
- `log_exit()` for error exits or daemon/logfile contexts.

### Step 6: Debug (cleanup.c:231-237)
- Debug logging if EXIT >= 1.

### Step 7: MSG_ERROR_EXIT (cleanup.c:242-258)
- For protocol >= 31 or receiver: sends `MSG_ERROR_EXIT` with exit code.
- Calls `noop_io_until_death()` to drain remaining I/O.

### Step 8: Final (cleanup.c:263-274)
- `msleep(100)` for server side errors (lets client read buffered data).
- `close_all()` closes all file descriptors.
- If `called_from_signal_handler`, uses `_exit()`; otherwise `exit()`.

## 4. `cleanup_got_literal`: Tracking Real Progress

### Where Set to 0 (Start of File)

**receiver.c:674** - Reset at the top of the recv_files() per-file loop, after
stats are updated but before `receive_data()`:

```c
cleanup_got_literal = 0;
```

This happens for every file that enters the transfer path. It ensures that if
the file is interrupted before any literal data arrives, the partial file is
not retained (no real progress was made).

### Where Set to 1 (Literal Data Received)

**receiver.c:329** - Inside `receive_data()`, when a positive token (literal
data) is received from the sender:

```c
if (i > 0) {
    stats.literal_data += i;
    cleanup_got_literal = 1;
    sum_update(data, i);
    // write to fd...
}
```

This is set on the very first literal byte received. Copy tokens (negative `i`
values, referencing matched blocks from the basis file) do NOT set this flag.

### Where Reset (cleanup_disable)

**cleanup.c:277-282** - `cleanup_disable()` resets `cleanup_got_literal = 0`
along with all other cleanup state. Called at receiver.c:557 (top of main loop)
and receiver.c:935 (after successful file completion).

### Implication

A file that is 100% matched (all copy tokens, no literal data) will have
`cleanup_got_literal == 0`. If interrupted, such a file is always deleted -
no real data was transmitted, so there is nothing to preserve.

## 5. The Keep-Partial Decision Tree

During signal cleanup (step 2 of `_exit_cleanup`), the partial file is retained
only when ALL of these conditions are true:

```
cleanup_got_literal != 0           -- real data was written
AND (cleanup_fname != NULL         -- temp file name is known
     OR cleanup_fd_w != -1)        -- write fd is open
AND cleanup_fname != NULL          -- temp file name is known
AND cleanup_new_fname != NULL      -- destination name is known
AND keep_partial != 0              -- --partial or --partial-dir is active
AND handle_partial_dir() succeeds  -- partial dir is creatable
```

If any condition fails, the temp file is NOT retained by step 2, and step 4
will `do_unlink(cleanup_fname)` to remove it.

### Decision Flowchart

```
Signal arrives during file transfer
  |
  v
cleanup_got_literal > 0?
  |-- NO --> temp file deleted (step 4)
  |
  YES
  |
  v
cleanup_fname set? (non-NULL)
  |-- NO --> temp file deleted (step 4)
  |
  YES
  |
  v
cleanup_new_fname set? (non-NULL)
  |-- NO --> temp file deleted (step 4)
  |
  YES
  |
  v
keep_partial enabled?
  |-- NO --> temp file deleted (step 4)
  |
  YES
  |
  v
handle_partial_dir(PDIR_CREATE) succeeds?
  |-- NO --> temp file deleted (step 4)
  |
  YES
  |
  v
partial_dir set?
  |-- NO --> stamp modtime=0, finish_transfer (retained in-place)
  |-- YES -> finish_transfer into partial-dir (retained in partial-dir)
```

## 6. `finish_transfer()` on Interrupt (cleanup.c:181-182)

When the partial file is retained during cleanup, `finish_transfer` is called
with these specific arguments:

```c
finish_transfer(cleanup_new_fname, fname, NULL, NULL,
                cleanup_file, tweak_modtime, !partial_dir);
```

| Argument         | Value                  | Meaning |
|------------------|------------------------|---------|
| `fname`          | `cleanup_new_fname`    | Destination path (or partial-dir path) |
| `fnametmp`       | `cleanup_fname` (temp) | Temp file to rename |
| `fnamecmp`       | NULL                   | No basis comparison |
| `partialptr`     | NULL                   | No cross-filesystem fallback |
| `file`           | `cleanup_file`         | File metadata (possibly with modtime=0) |
| `ok_to_set_time` | `tweak_modtime`        | 1 if no partial_dir (stamp epoch), 0 if partial_dir |
| `overwriting_basis` | `!partial_dir`      | TRUE only when no partial_dir |

Inside `finish_transfer()` (rsync.c:724-785):
1. Since `inplace` is not the cleanup path, it proceeds to the rename branch.
2. Calls `set_file_attrs()` on the temp file to set permissions.
   - If `tweak_modtime == 1` (no partial_dir): uses `ATTRS_ACCURATE_TIME`,
     which applies `cleanup_file->modtime` (already set to 0 = epoch).
   - If `tweak_modtime == 0` (has partial_dir): uses
     `ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME` - mtime is
     NOT modified; the file keeps whatever mtime it got from the write.
3. Calls `robust_rename()` to move the temp file to the destination.
   - `temp_copy_name` is NULL (partialptr was passed as NULL), so no
     cross-filesystem copy fallback is attempted.

## 7. The modtime=0 Stamp (cleanup.c:174-179)

```c
if (!partial_dir) {
    tweak_modtime = 1;
    cleanup_file->modtime = 0;
}
```

**When:** Only when `--partial` is used WITHOUT `--partial-dir`. The partial
file will be left at the destination path (overwriting any previous version).

**Why:** Two reasons documented in the inline comment:
1. **Prevent `--update` skipping.** If the partial file retained a modern mtime,
   a subsequent `rsync --update` would see the partial file as newer than the
   source and skip it, leaving a corrupt file permanently.
2. **Visual identification.** A file with mtime of epoch (1970-01-01 00:00:00)
   stands out in directory listings as clearly incomplete.

**When NOT applied:** If `--partial-dir` is set, the partial file is moved into
the partial directory, not the final destination. Since it will not be
discovered by `--update` at the real destination path, the mtime is left as-is.
The partial-dir file will be used as a basis file on the next transfer attempt.

## 8. `cleanup_set()` / `cleanup_disable()` Bracket in `recv_files()`

### `cleanup_disable()` (cleanup.c:277-282)

```c
void cleanup_disable(void)
{
    cleanup_fname = cleanup_new_fname = NULL;
    cleanup_fd_r = cleanup_fd_w = -1;
    cleanup_got_literal = 0;
}
```

Called at two points in `recv_files()`:
1. **receiver.c:557** - Top of the main `while(1)` loop, before `read_ndx_and_attrs()`.
   Clears any state from the previous file.
2. **receiver.c:935** - After all file completion logic (finish_transfer, delay-updates
   bookkeeping, unlink). The file is done; cleanup state is disarmed.

### `cleanup_set()` (cleanup.c:285-293)

```c
void cleanup_set(const char *fnametmp, const char *fname,
                 struct file_struct *file, int fd_r, int fd_w)
```

Called at two points in `recv_files()`, depending on the transfer mode:

1. **Inplace mode (receiver.c:868):**
   ```c
   cleanup_set(NULL, NULL, file, fd1, fd2);
   ```
   Both filename args are NULL. This means cleanup step 2 will see
   `cleanup_fname == NULL` and skip partial retention entirely. The inplace
   file is either complete or not - there is no temp file to retain.

2. **Normal (temp file) mode (receiver.c:873):**
   ```c
   cleanup_set(fnametmp, partialptr, file, fd1, fd2);
   ```
   - `cleanup_fname = fnametmp` (the `.XXXXXX` temp file)
   - `cleanup_new_fname = partialptr` (the partial-dir path, or `fname` if
     no partial-dir)
   - `cleanup_file = file` (the file_struct metadata)
   - `cleanup_fd_r = fd1` (basis file read fd, or -1)
   - `cleanup_fd_w = fd2` (temp file write fd)

### The Bracket

The lifecycle for each file is:

```
cleanup_disable()          -- receiver.c:557 (top of loop)
  |
  read_ndx_and_attrs()     -- receiver.c:560
  |
  ... open basis, open temp ...
  |
  cleanup_set(...)         -- receiver.c:868 or 873
  |                          (signal can now trigger partial retention)
  |
  receive_data()           -- receiver.c:892
  |                          (cleanup_got_literal set to 1 on first literal)
  |
  close(fd1), close(fd2)  -- receiver.c:898-904
  |
  finish_transfer()/unlink -- receiver.c:906-933
  |
  cleanup_disable()        -- receiver.c:935 (disarm)
```

If a signal arrives between the two `cleanup_disable()` calls (and after
`cleanup_set()`), the partial file handling in step 2 of `_exit_cleanup` is
armed. If it arrives outside this window, cleanup_fname is NULL and the temp
file (if any) is simply unlinked.

## 9. `delay_updates` Interaction

### Overview

`--delay-updates` defers renaming completed files until the end of the transfer
phase. Files are written to the partial-dir during transfer, then renamed in
bulk at phase transition.

### Option Setup (options.c:2410-2444)

- `--inplace` and `--delay-updates` are mutually exclusive (error at 2410).
- `--append` and `--delay-updates` are mutually exclusive.
- When `--delay-updates` is active, `partial_dir` and `keep_partial` are
  always set (the delay-updates mechanism IS partial-dir).

### Normal Completion with `delay_updates` (receiver.c:906-933)

The post-transfer decision tree:

```c
if ((recv_ok && (!delay_updates || !partialptr)) || inplace) {
    // Normal success: rename temp -> dest immediately
    finish_transfer(fname, fnametmp, fnamecmp, partialptr, file, recv_ok, 1);
}
else if (keep_partial && partialptr && (!one_inplace || delay_updates)) {
    // delay_updates path: move to partial-dir, mark in delayed_bits
    handle_partial_dir(partialptr, PDIR_CREATE);
    finish_transfer(partialptr, fnametmp, fnamecmp, NULL, file, recv_ok, !partial_dir);
    if (delay_updates && recv_ok) {
        bitbag_set_bit(delayed_bits, ndx);
        recv_ok = 2;  // special return: delayed
    }
}
else if (!one_inplace)
    do_unlink(fnametmp);
```

When `delay_updates` is active and the transfer succeeds (`recv_ok` is true):
1. The condition `!delay_updates || !partialptr` is false, so the first branch
   is skipped.
2. The second branch activates: temp file is renamed into the partial-dir.
3. The file index is recorded in `delayed_bits` for the bulk rename later.
4. `recv_ok` is set to 2, which bypasses the `send_msg_success` in the switch
   below (only sent after the bulk rename in `handle_delayed_updates`).

### Interrupt with Both `--delay-updates` and `--partial`

On signal interrupt:
- **Files already moved to partial-dir and recorded in `delayed_bits`** remain
  in the partial-dir. They were successfully transferred and checksummed, but
  never renamed to the final destination. On the next rsync invocation, they
  serve as basis files (the generator detects them via `fnamecmp_type == FNAMECMP_PARTIAL_DIR`).
- **The in-flight file** (if any) follows the normal cleanup_got_literal logic
  in step 2 of `_exit_cleanup`. Since `keep_partial` is true (delay_updates
  implies it), the partial file is retained if literal data was received.
- **No bulk rename occurs.** The `handle_delayed_updates()` sweep never runs
  because the receiver loop exits via signal before reaching the phase
  transition.

## 10. `handle_delayed_updates()` Sweep at Phase 2 (receiver.c:422-450, 584-585)

```c
if (phase == 2 && delay_updates)
    handle_delayed_updates(local_name);
```

This is called when the receiver transitions from phase 1 to phase 2
(receiver.c:584-585). Phase 2 is the redo phase for files that failed checksum
verification in phase 1.

### The Sweep (receiver.c:422-450)

```c
static void handle_delayed_updates(char *local_name)
{
    for (ndx = -1; (ndx = bitbag_next_bit(delayed_bits, ndx)) >= 0; ) {
        file = cur_flist->files[ndx];
        fname = local_name ? local_name : f_name(file, NULL);
        partialptr = partial_dir_fname(fname);
        if (partialptr != NULL) {
            if (make_backups > 0 && !make_backup(fname, False))
                continue;
            do_rename(partialptr, fname);  // partial-dir -> final dest
            if (success)
                send_msg_success(fname, ndx);
            handle_partial_dir(partialptr, PDIR_DELETE);
        }
    }
}
```

For each file recorded in `delayed_bits`:
1. Constructs the partial-dir path.
2. Creates a backup of the existing destination if `--backup` is active.
3. Renames from partial-dir to final destination.
4. Sends `MSG_SUCCESS` to the generator (for `--remove-source-files` or
   hard-link tracking).
5. Removes the partial-dir subdirectory if it is now empty.

**Note:** There is also a second call at receiver.c:988 for `protocol_version < 29`,
which handles the case where max_phase is 1 (no redo phase):

```c
if (phase == 2 && delay_updates) /* for protocol_version < 29 */
    handle_delayed_updates(local_name);
```

### Timing

The sweep runs after all phase 1 files are transferred but before the phase 2
redo loop begins. This is important because redo files need the destination to
be in its final location, not still in the partial-dir.

## 11. Inplace Mode Cleanup Behavior

When `inplace` is true, `cleanup_set()` is called with NULL filenames
(receiver.c:868):

```c
cleanup_set(NULL, NULL, file, fd1, fd2);
```

In `_exit_cleanup` step 2:
- `cleanup_fname` is NULL, so `cleanup_fname` check fails.
- `cleanup_fd_w != -1` is true, so the outer condition passes.
- The `flush_write_file` and `close` calls execute on the write fd.
- But the inner condition (`cleanup_fname && cleanup_new_fname && keep_partial`)
  fails because `cleanup_fname` is NULL.
- Step 4: `cleanup_fname` is NULL, so `do_unlink` is skipped.

**Result:** The inplace file is flushed and closed but NOT deleted and NOT
renamed. Whatever was written in-place remains. This is correct behavior -
the file IS the destination, and any written data is already committed.

## 12. Summary: Implementation Requirements for oc-rsync

1. **Global mutable state:** `cleanup_got_literal`, `cleanup_fname`,
   `cleanup_new_fname`, `cleanup_file`, `cleanup_fd_r`, `cleanup_fd_w` must
   all be accessible from the signal handler path. In Rust, this requires
   either atomic/static-mut or a signal-safe mechanism.

2. **`cleanup_got_literal` must be set on the first literal token**, not on
   copy tokens. Only literal data represents real progress.

3. **The `cleanup_set`/`cleanup_disable` bracket** must precisely match the
   upstream window: armed after temp file creation, disarmed after
   finish_transfer completes.

4. **modtime=0 stamp** must be applied when `--partial` is used without
   `--partial-dir`, to prevent `--update` from skipping partial files.

5. **`finish_transfer` from cleanup** passes `ok_to_set_time = tweak_modtime`
   and `overwriting_basis = !partial_dir`. These differ from the normal
   completion path.

6. **Deferred signal handling** on server/receiver: set a flag, handle at next
   I/O boundary. Do not call cleanup from within the signal handler on the
   receiver side when multiplexed I/O is active.

7. **`delay_updates` files in partial-dir survive interrupt.** They are valid,
   fully checksummed files waiting for bulk rename. On re-run, the generator
   uses them as basis files.

8. **Inplace cleanup:** flush and close the fd, but do not delete or rename.
   The file is the destination itself.

9. **`called_from_signal_handler` determines exit method:** `_exit()` for
   signal context (no atexit handlers, no stdio flush), `exit()` for normal
   cleanup context.

10. **Step ordering is non-repeatable.** If cleanup triggers a recursive error,
    it resumes at the next step, never re-executes a completed step. The
    `switch_step` / `case_N.h` mechanism implements this.
