# PIR-4.b / PIR-5.b: Receiver Cleanup and Delay-Updates Audit

Research audit for wiring `CleanupManager::register_temp_file` into the
receiver pipeline and tracking committed-to-partial-dir files for the
`--delay-updates` rename sweep.

## PIR-4.b: All Temp File Creation Sites in `crates/transfer/src/`

### Production Sites (non-test)

There are **three distinct code paths** that create temp files. Each has
both a Unix (sandboxed) and non-Unix (plain) variant.

#### Site 1: `disk_commit/process.rs` - `open_output_file()` (line 267)

The pipelined receiver's disk-commit thread calls this function for every
file. Three branches:

| Branch | Lines | Creates temp file? | Guard type |
|--------|-------|--------------------|------------|
| Device target | 272-273 | No (opens device directly) | `TempFileGuard::new(file_path)` with `keep_on_drop=false`, but `needs_rename=false` |
| Inplace | 275-286 | No (opens dest directly) | `TempFileGuard::new(file_path)` with `needs_rename=false` |
| Temp+rename | 294-303 | **Yes** | `open_tmpfile_sandboxed()` / `open_tmpfile()` returns `(File, TempFileGuard)` |

The temp file path is `cleanup_guard.path()` after creation. The guard's
`keep()` is called at line 418 inside `commit_file()` after the rename
succeeds.

**Integration point for CleanupManager**: Register the guard path at line
303 (after `Ok((file, guard, true))`) and unregister at line 418 (after
`cleanup_guard.keep()`).

This site is called by both `process_file()` (chunked) and
`process_whole_file()` (coalesced single-chunk) - both invoke
`open_output_file()` at their top.

#### Site 2: `transfer_ops/response.rs` - `process_file_response()` (line 64)

The synchronous (non-pipelined) single-file response processor. Two
branches:

| Branch | Lines | Creates temp file? | Guard type |
|--------|-------|--------------------|------------|
| Inplace | 82-93 | No | `TempFileGuard::new(file_path)` |
| Temp+rename | 98-103 | **Yes** | `open_tmpfile_sandboxed()` / `open_tmpfile()` |

Guard `keep()` is called at line 356 after successful rename or inplace
truncation.

**Integration point for CleanupManager**: Register at line 103 (after
guard creation), unregister at line 356 (after `cleanup_guard.keep()`).

#### Site 3: `receiver/transfer/sync.rs` - `run_sync()` (line 204)

The synchronous receiver transfer loop creates a temp file per file:

| Lines | Creates temp file? | Guard type |
|-------|--------------------|------------|
| 204-211 | **Yes** | `open_tmpfile_sandboxed()` / `open_tmpfile()` |

Guard `keep()` is called at line 374 after the rename.

**Integration point for CleanupManager**: Register at line 211 (after
guard creation), unregister at line 374 (after `temp_guard.keep()`).

### Summary Table

| # | File | Function | Line | Needs CleanupManager? |
|---|------|----------|------|-----------------------|
| 1 | `disk_commit/process.rs` | `open_output_file` | 295/302 | Yes (temp+rename path) |
| 2 | `transfer_ops/response.rs` | `process_file_response` | 99/102 | Yes (temp+rename path) |
| 3 | `receiver/transfer/sync.rs` | `run_sync` | 204/211 | Yes |

Device-target and inplace branches create a `TempFileGuard` for API
uniformity but never create a temp file on disk, so they do not need
`CleanupManager` registration.

### Existing Cleanup Infrastructure

- **`TempFileGuard`** (`temp_guard.rs`): RAII guard that deletes the temp
  file on drop unless `keep()` is called. Handles panics and early
  returns. Does NOT register with `CleanupManager`.

- **`CleanupManager`** (`core/src/signal/cleanup.rs`): Global singleton
  using `OnceLock<Mutex<CleanupManagerState>>`. Stores a
  `HashSet<PathBuf>` of registered paths. `cleanup()` and
  `cleanup_temp_files()` delete all registered paths. Designed for signal
  handlers (SIGINT/SIGTERM) where RAII cannot run.

- **`cleanup_stale_temp_files()`** (`temp_cleanup.rs`): Startup-time
  scan that removes `.filename.XXXXXX` files older than 24 hours. Catches
  files orphaned by SIGKILL/OOM/power loss where neither RAII nor signal
  handlers ran.

### Gap Analysis

`TempFileGuard` handles normal cleanup (RAII). `temp_cleanup` handles
startup orphan removal. But between creation and `keep()`, a SIGINT could
arrive and the signal handler's `CleanupManager::cleanup()` would not
know about the temp file because nothing registers it. The PIR-4.b task
closes this gap.

---

## PIR-5.b: Current `delay_updates` Handling

### Configuration Flow

1. **CLI**: `--delay-updates` flag parsed in `cli/src/frontend/`.
2. **Core**: `ClientConfig.delay_updates` (bool) flows to
   `LocalCopyOptions.delay_updates(true)` and
   `ServerConfigBuilder.delay_updates(true)`.
3. **Transfer crate**: `WriteConfig.delay_updates` (bool) in
   `config/mod.rs:55`. Validated as mutually exclusive with `--inplace`
   in `builder.rs:442`.
4. **Engine crate**: `LocalCopyOptions.delay_updates` (bool) in
   `local_copy/options/types.rs:190`.

### How `delay_updates` Works in the Engine (Local Copy)

The local-copy engine has a **complete `--delay-updates` implementation**:

1. **Staging directory**: When `delay_updates=true` and no explicit
   `--partial-dir`, the partial dir defaults to `.~tmp~`
   (`DELAY_UPDATES_PARTIAL_DIR` constant in `staging.rs:15`). This
   matches upstream `options.c`'s `static char tmp_partialdir[] =
   ".~tmp~"`.

2. **Deferred updates**: Each transferred file is staged into the partial
   dir. Instead of committing (renaming) immediately,
   `finalize_guard_and_metadata()` in `finalize.rs:48` creates a
   `DeferredUpdate` and calls `context.register_deferred_update(update)`.

3. **`DeferredUpdate` struct** (`context.rs:317`): Holds the
   `DestinationWriteGuard` (temp file guard), `fs::Metadata`,
   `MetadataOptions`, execution mode, path context, and final destination
   path.

4. **`DeferredOperationQueue`** (`context.rs:286`): Contains a
   `Vec<DeferredUpdate>` for pending renames, plus a
   `HashSet<PathBuf>` of staging directories (`.~tmp~`) for cleanup.

5. **Flush at end**: `flush_deferred_updates()` in
   `context_impl/state.rs:458` iterates all deferred updates, calling
   `finalize_deferred_update()` which commits the guard (renames temp to
   final) and applies metadata.

6. **Delete timing**: `delay_updates` promotes `DeleteTiming::During` to
   `DeleteTiming::After` (`deletion.rs:114`) so deletions happen after
   the rename sweep.

7. **Hard link handling**: When `delay_updates` is enabled and a hard
   link target is being transferred, the code forces an early commit of
   the deferred update for the target (`commit_deferred_update_for` in
   `links.rs:126`) before creating the hard link.

### How `delay_updates` Works in Upstream C (receiver.c)

1. **Bitbag**: `delayed_bits = bitbag_create(cur_flist->used + 1)` at
   line 547 - one bit per file index.

2. **Per-file**: When transfer succeeds with `delay_updates` active, the
   temp file is renamed into the partial-dir (`.~tmp~/<filename>`) and
   `bitbag_set_bit(delayed_bits, ndx)` marks the index (line 927-929).
   `recv_ok` is set to 2 (deferred).

3. **Sweep**: `handle_delayed_updates()` at line 422 iterates all set
   bits, computes `partial_dir_fname(fname)`, creates backups if needed,
   and renames from partial-dir to final destination.

4. **Timing**: Called at phase 2 transition (line 585) and again at the
   end for protocol < 29 (line 988-989).

### Current State in Transfer Crate (Remote Transfers)

The transfer crate (used for remote/daemon transfers) **does NOT
implement the delay_updates rename sweep**:

- `WriteConfig.delay_updates` is stored but **never checked** in any of
  the receiver transfer paths (`pipeline.rs`, `pipelined.rs`,
  `pipelined_incremental.rs`, `sync.rs`).
- `DiskCommitConfig` has no `delay_updates` field.
- The disk commit thread (`process.rs`) always renames temp to final
  immediately in `commit_file()`.
- There is no `DeferredUpdate` equivalent in the transfer crate.
- There is no bitbag or deferred-rename list.
- There is no `handle_delayed_updates()` equivalent.

The local-copy engine's `DeferredOperationQueue` and `DeferredUpdate`
types are not available to the transfer crate (they are `pub(crate)` in
the engine).

---

## PIR-5.b: Proposed Data Structure for Delayed File Tracking

### Requirements

For `--delay-updates` in the remote transfer path, we need to:

1. Track which files were staged to the partial dir after successful
   transfer.
2. At phase 2 transition, iterate all tracked files and rename from
   partial-dir path to final destination.
3. Support backup creation before the rename (if `--backup` is active).

### Upstream Model

Upstream uses a bitbag (compact bitmap) keyed by file-list index, plus
`partial_dir_fname()` to recompute the partial-dir path at rename time.
This is memory-efficient (1 bit per file) but requires recomputing paths.

### Proposed Design

```rust
/// Entry tracking a file staged to the partial-dir for deferred rename.
///
/// Stored in a flat Vec during phase 1. Iterated at the phase 2 boundary
/// for the rename sweep.
///
/// upstream: receiver.c:927 - bitbag_set_bit(delayed_bits, ndx) + recv_ok=2
pub struct DelayedFile {
    /// File list index (used for metadata lookup during rename sweep).
    pub file_index: usize,
    /// Path inside the partial-dir where the file was staged.
    /// e.g., `/dest/.~tmp~/filename`
    pub partial_path: PathBuf,
    /// Final destination path where the file should be renamed to.
    /// e.g., `/dest/filename`
    pub final_path: PathBuf,
}

/// Accumulator for files deferred by `--delay-updates`.
///
/// Mirrors upstream's `delayed_bits` bitbag but stores full paths to
/// avoid recomputing `partial_dir_fname()` at sweep time.
///
/// The Vec is append-only during the transfer loop and consumed once
/// during `handle_delayed_updates()` at the phase 2 boundary.
pub struct DelayedUpdateQueue {
    files: Vec<DelayedFile>,
}

impl DelayedUpdateQueue {
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Pre-allocates capacity matching the file list size.
    pub fn with_capacity(n: usize) -> Self {
        Self { files: Vec::with_capacity(n) }
    }

    /// Records a file as staged in the partial-dir.
    pub fn push(&mut self, entry: DelayedFile) {
        self.files.push(entry);
    }

    /// Returns the number of deferred files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Drains all entries for the rename sweep.
    pub fn drain(&mut self) -> std::vec::Drain<'_, DelayedFile> {
        self.files.drain(..)
    }
}
```

### Design Rationale

- **Vec over bitbag**: The transfer crate does not have a bitbag
  implementation, and the file list indices alone are insufficient - we
  need both the partial-dir path and the final destination path. A Vec of
  structs is simpler and avoids recomputing paths at sweep time. The
  memory cost is small: ~100 bytes per deferred file (two PathBufs + one
  usize).

- **Append-only + drain**: Mirrors the upstream lifecycle: accumulate
  during transfer (append), sweep at phase boundary (drain). No random
  access or removal needed.

- **Stored in `ReceiverContext`**: The queue lives alongside the
  existing `file_list` and `config` in `ReceiverContext`. It is
  `None` when `delay_updates` is false.

- **Sweep location**: `handle_delayed_updates()` should be called from
  `finalize_transfer()` in `phases.rs` at the phase 2 boundary, matching
  upstream `receiver.c:585`.

### Integration Points

1. **Accumulation**: In `disk_commit/process.rs::commit_file()`, when
   `delay_updates` is true and `needs_rename` is true, instead of
   renaming temp to final, rename temp to partial-dir and push a
   `DelayedFile` entry. The `DiskCommitConfig` needs a
   `delay_updates: bool` field and a
   `partial_dir: Option<PathBuf>` (already has `temp_dir`).

2. **Sweep**: New `handle_delayed_updates()` method on `ReceiverContext`
   called from `finalize_transfer()`. Iterates the queue, creates backups
   if needed, and renames from partial to final.

3. **Thread boundary**: The disk-commit thread populates the deferred
   list. Since the thread communicates results back via
   `CommitResult`, add an optional `DelayedFile` field to
   `CommitResult` that the receiver drains into its queue.

4. **For sync.rs**: The synchronous path can accumulate directly into
   a local `DelayedUpdateQueue` since it runs single-threaded.

---

## Summary of Required Changes

### PIR-4.b (CleanupManager wiring)

Three sites need `register_temp_file` / `unregister_temp_file` calls:

1. `disk_commit/process.rs:open_output_file()` - register after
   `open_tmpfile*()` returns, unregister in `commit_file()` after
   `cleanup_guard.keep()`.
2. `transfer_ops/response.rs:process_file_response()` - register after
   temp file creation (line 103), unregister after `cleanup_guard.keep()`
   (line 356).
3. `receiver/transfer/sync.rs:run_sync()` - register after temp file
   creation (line 211), unregister after `temp_guard.keep()` (line 374).

### PIR-5.b (Delayed rename tracking)

1. New `DelayedFile` struct and `DelayedUpdateQueue` type.
2. `DiskCommitConfig` gains `delay_updates: bool` and a partial-dir path.
3. `CommitResult` gains an optional `DelayedFile` for the pipelined path.
4. `ReceiverContext` gains an optional `DelayedUpdateQueue`.
5. New `handle_delayed_updates()` method called from
   `finalize_transfer()`.
6. `commit_file()` in `disk_commit/process.rs` routes to partial-dir
   instead of final destination when delay_updates is active.
