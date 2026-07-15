## Summary

Extends the file-list dedup pass (#6631) to INC_RECURSE sub-lists and adds the
per-entry path-belongs validation upstream runs on every received sub-list.
Both were left open when #6631 wired `flist_sort_and_clean` into the initial
list only.

Two gaps closed in the receiver's sub-list receive path
(`receive_one_extra_segment` / its async twin):

### 1. Sub-list dedup

Upstream runs `flist_sort_and_clean()` on **each** INC_RECURSE sub-list on both
sides - the sender per directory (`flist.c:2190` `send_extra_file_list`) and the
receiver per `recv_file_list(f, dir_ndx)` call (`flist.c:2771`). Its clean pass
(`flist.c:3031`, active for the receiver because `!am_sender || inc_recurse`)
removes duplicate names, keeping the upstream tie-break (a directory over a
same-named file "because it might have contents in the list", else the first).

Before this change the sub-list path sorted the segment but skipped the dedup, so
a redundant or hostile duplicate survived and the generator requested the same
path twice - an NDX divergence driven by untrusted bytes, the exact class #6631
closed for the initial list. The fix reuses the shared `sort_and_clean_file_list`
primitive on each segment slice.

**Both sides stay NDX-identical.** The legitimate sender ships deduped sub-lists:
its per-directory segments are partitions of a full file list that was already
sorted+deduped (`dedup_with_parallel`) before partitioning, so no segment can
carry a duplicate. The receiver's pass is therefore a no-op on any legitimate
transfer and only collapses a hostile duplicate - it never changes the entry
count or NDX numbering the sender expects. This is the same symmetry lesson as
the initial-list fix: a receiver-only dedup of bytes the sender did *not* dedup
would desync (RERR_PROTOCOL 16); here the sender's list is deduped-by-construction
before it is split into sub-lists, so both sides converge to the same segment.

### 2. Sub-list path-belongs validation

Upstream rejects any sub-list entry whose dirname does not match the directory
named by the header's `dir_ndx`, aborting with `exit_cleanup(RERR_UNSUPPORTED)`
(`flist.c:2684-2695`, "ABORTING due to invalid path from sender"). This defends
against a hostile sender that frames a sub-list for a legitimate parent but fills
it with an entry that escapes that tree.

The existing range + duplicate `dir_ndx` guards (from the merged dir_ndx
validation work) do not catch this: `dir_ndx` itself is valid; only the
per-entry dirname comparison does. To perform it the receiver now records the
full path of every directory in wire `dir_ndx` order (`dir_flist_names`,
mirroring upstream's `dir_flist->files[]`, `flist.c:2704`), built after each
list/sub-list is sorted so the index matches the sender's `dir_ndx` assignment.
A mismatched entry is rejected with `io::ErrorKind::Unsupported`, which the core
exit-code mapper turns into `RERR_UNSUPPORTED` (4), matching upstream. Leading
slashes (present only under `--relative`, where upstream defers stripping to
`flist_sort_and_clean`'s `strip_root`) are ignored on both sides so a legitimate
relative transfer is never falsely rejected.

## Upstream references

- `flist.c:2190` `send_extra_file_list()` - `flist_sort_and_clean(flist, 0)` per sub-list (sender).
- `flist.c:2771` `recv_file_list()` - `flist_sort_and_clean(flist, relative_paths)` per sub-list (receiver).
- `flist.c:3016-3082` `flist_sort_and_clean()` / clean pass - the dedup tie-break, active for the receiver (`!am_sender || inc_recurse`, line 3031).
- `flist.c:2684-2695` - path-belongs check -> "ABORTING due to invalid path from sender" -> `exit_cleanup(RERR_UNSUPPORTED)`.
- `flist.c:2704` - `dir_flist->files[dir_flist->used++] = file` appends each directory so `dir_flist->files[dir_ndx]` names it later.

## Tests

- `sublist_duplicate_name_is_deduped` - a repeated name in a sub-list collapses to one entry.
- `sublist_entry_escaping_parent_is_rejected` - an entry whose dirname escapes its `dir_ndx` parent is rejected with `Unsupported` (exit 4) and its segment dropped.
- `sublist_entry_under_parent_is_accepted` - the normal case still passes (no false rejection).
- The existing real-upstream multi-segment sub-list frame test now exercises path-belongs against genuine upstream 3.4.4 bytes and passes, confirming the `dir_ndx` -> name ordering matches upstream.

Both wire surfaces (sync and the default-off async twin) receive the identical
change, keeping the async-parity contract.

## End-to-end verification

Built the binary and drove a deep recursive tree (8-level chain plus a wide
subtree, 84 files) over a two-process local-shell transport, exercising real
INC_RECURSE sub-lists across sender and receiver:

- `push` and `pull` both exit 0 (RERR_PROTOCOL-free) and are byte-identical to a
  local reference copy (`diff -r` clean).
- `-vv` confirms "sending/receiving incremental file list" - inc-recurse engaged.
- Idempotent re-sync transfers nothing and exits 0.
