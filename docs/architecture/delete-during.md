# `--delete-during` ordering: oc-rsync vs upstream rsync 3.4.1

This document captures the audit findings for issues #1893 and #1894. It
contrasts the per-directory interleaved deletion model used by upstream rsync
3.4.1 with the batched pre-transfer sweep used by oc-rsync, calls out the
behavioural differences a user can observe, and lists the follow-up work
required to close any gap.

`--delete-during` removes destination entries that no longer exist on the
sender. Upstream interleaves the deletion with the transfer of each
directory; oc-rsync performs a single sweep before any file content is sent.
Both approaches converge on the same final filesystem state for a successful
transfer. The difference matters for failure modes, observability, and
filter-rule semantics.

## Upstream behaviour

Upstream rsync runs deletion as part of the generator's main loop. For each
directory entry returned by `recv_generator()`, the generator immediately
calls `delete_in_dir()` before generating signatures or transfer requests
for the files inside that directory. The relevant entry points are:

- `generator.c::recv_generator()` -- per-entry dispatch from the generator
  loop. Calls `delete_in_dir()` on every directory it visits.
- `generator.c::delete_in_dir()` -- enumerates the destination directory,
  matches entries against the in-memory file list for that directory, and
  removes anything not present.
- `rsync.c::do_delete()` (a.k.a. `delete_item()` in newer trees) -- shared
  removal primitive used by all delete modes.

Key properties of the upstream model:

1. **Phase ordering is interleaved per directory.** The generator emits
   `Delete(dir_A) -> Transfer(dir_A) -> Delete(dir_B) -> Transfer(dir_B)`.
   Files inside `dir_A` are written into a directory whose extraneous
   entries have already been removed.
2. **Single-threaded, deterministic order.** Deletion uses reverse
   iteration of the destination directory list and is dispatched from the
   generator's serial loop. Itemize output (`*deleting`) appears in a
   stable, reproducible order for a given input.
3. **Filter rules re-evaluate per directory.** Because deletion runs after
   the per-directory `.rsync-filter` and any merge files have been
   loaded for that directory, protect/risk and per-dir merge rules apply
   at deletion time exactly as they do at transfer time.
4. **Errors are logged and the transfer continues.** A failed
   `delete_item()` is reported via the message log and the generator
   proceeds to the next entry. The transfer does not abort on a
   deletion error.

## oc-rsync behaviour

oc-rsync performs deletion as a single batch sweep that runs before any
file content is selected for transfer. The dispatch site is in
`crates/transfer/src/receiver/transfer.rs` (line 532 at
`run_pipelined`):

```rust
if self.config.flags.delete {
    let (ds, exceeded) = self.delete_extraneous_files(&setup.dest_dir, writer)?;
    delete_stats = ds;
    delete_limit_exceeded = exceeded;
}
```

This call sits between the metadata pre-pass (directory creation and
symlink materialization at lines 522 and 527) and the file-candidate
construction (`build_files_to_transfer` at line 544). The deletion logic
itself lives in `crates/transfer/src/receiver/directory/deletion.rs`.

Key properties of the oc-rsync model:

1. **Phase ordering is batched.** The receiver runs a metadata phase, then
   a single deletion phase that walks every directory at once, then the
   transfer phase. There is no per-directory interleave.
2. **Parallel and non-deterministic delete order.** Directories are
   dispatched through `crate::parallel_io::map_blocking` using
   `tokio::spawn_blocking` once the count exceeds
   `DEFAULT_DELETION_THRESHOLD = 64`
   (`crates/transfer/src/parallel_io.rs:33`). Below the threshold, the
   sweep falls back to a sequential loop; above it, the order in which
   directories are scanned (and therefore the order of `*deleting`
   itemize lines) depends on worker scheduling.
3. **Filter snapshot is taken once at the start.** The deletion workers
   share an `Arc<FilterChain>` cloned from the receiver's global rules
   (`deletion.rs:93`). Per-directory `.rsync-filter` merge files are not
   re-read inside the deletion sweep -- only the rules already loaded
   when deletion begins are evaluated.
4. **Error semantics differ.** `delete_extraneous_files` returns
   `io::Result`, and `?` at line 532 propagates the error up the
   `run_pipelined` stack. Per-entry `read_dir`/`remove` failures inside
   the worker are swallowed and the directory is skipped, but a failure
   in the surrounding orchestration aborts the transfer rather than
   logging and continuing.

## Differences

| Aspect              | Upstream 3.4.1                            | oc-rsync                                                |
| ------------------- | ----------------------------------------- | ------------------------------------------------------- |
| Phase ordering      | Interleaved per directory                 | Batched sweep before transfer                           |
| Determinism         | Reverse-iteration single-threaded         | `tokio::spawn_blocking` workers above 64-dir threshold  |
| Filter evaluation   | Re-evaluated per directory (merge files)  | Single snapshot of `FilterChain` for the whole sweep    |
| Error handling      | Logged, transfer continues                | Orchestration error may propagate via `?` and abort     |
| Itemize order       | Stable for a given input                  | Worker-scheduling dependent above the parallel threshold |
| Final filesystem state | Identical for successful transfers     | Identical for successful transfers                      |

The risk surface is therefore concentrated in three areas:

1. **Error propagation.** A user who relied on upstream's "delete failures
   do not stop the transfer" behaviour may see oc-rsync abort earlier.
2. **Observable delete order.** Tooling that scrapes
   `--itemize-changes` for `*deleting` lines must not assume a stable
   order on oc-rsync.
3. **Per-dir merge files.** Users whose `.rsync-filter` merge files would
   protect a file in directory `B` only after a deeper merge file in
   directory `B/.rsync-filter` is loaded see different behaviour: upstream
   honours the deeper merge file because deletion runs after it loads;
   oc-rsync evaluates only the snapshot taken before deletion.

## Recommendation

- Document the batching model in the CHANGELOG and the `oc-rsync(1)` man
  page so users do not assume per-directory interleave (#1894).
- Add an interop test that mixes new and deleted entries across multiple
  directories to assert final-state parity with upstream.
- Investigate whether real-world `.rsync-filter` users see different
  outcomes; capture cases with deep per-dir merge files.
- Consider an opt-in `--delete-strict-order` flag that forces the
  sequential, deterministic upstream order if user-visible delete
  ordering or per-directory filter re-evaluation is required for
  parity.
- Audit `delete_extraneous_files` error paths to align with upstream's
  "log and continue" policy where an abort is not strictly necessary.

## Upstream references

- `generator.c::recv_generator()` -- per-entry generator dispatch.
- `generator.c::delete_in_dir()` -- per-directory delete enumeration.
- `generator.c::do_delete_pass()` -- full-tree sweep used by
  `--delete-before`.
- `rsync.c::do_delete()` / `delete_item()` -- shared removal primitive.
- `main.c` deletion-count check that enforces `--max-delete`.

## oc-rsync references

- `crates/transfer/src/receiver/transfer.rs:532` -- delete entry point in
  `run_pipelined`.
- `crates/transfer/src/receiver/directory/deletion.rs:40` --
  `delete_extraneous_files` definition; documents the parallel scan and
  `--max-delete` enforcement.
- `crates/transfer/src/receiver/directory/deletion.rs:93` -- single
  filter-chain snapshot shared across workers.
- `crates/transfer/src/receiver/directory/deletion.rs:99` --
  `parallel_io::map_blocking` dispatch.
- `crates/transfer/src/parallel_io.rs:33` --
  `DEFAULT_DELETION_THRESHOLD = 64`, the parallel cutoff.
