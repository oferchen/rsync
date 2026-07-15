## Receiver must run the duplicate-clean pass of `flist_sort_and_clean`

Upstream `flist.c:flist_sort_and_clean()` (rsync-3.4.4, `flist.c:3016`) runs
three steps after the receiver decodes the file list, called from
`recv_file_list()` at `flist.c:2771`:

1. **sort** (`fsort` on the sorted array),
2. **remove duplicate names** (`flist.c:3046-3082`) - runs on the receiver
   (`!am_sender`), and
3. **prune empty dirs** (`flist.c:3121-3184`, only under `--prune-empty-dirs`).

### The gap

The receiver (`crates/transfer/src/receiver/file_list/receive.rs`) ran step 1
(sort) and step 3 (`prune_empty_dirs_pass`) but **never ran step 2**. A sender
that emits the same normalized name twice - redundant, or a hostile peer -
left both entries in `self.file_list`. The generator would then request the
same path twice: wasted work and an NDX divergence driven by untrusted bytes.

A full sort+dedup primitive already existed
(`crates/protocol/src/flist/sort.rs`: `sort_and_clean_file_list` /
`flist_clean`, "mirrors upstream's clean_flist()"), but it was only exercised
by unit tests - no receiver path called it.

### The fix

Wire the shared `sort_and_clean_file_list` primitive into the receiver in
place of the sort-only call, preserving upstream ordering (sort -> dedup ->
prune) and the existing `--prune-empty-dirs` and iconv-suppression behaviour.
Both sides now clean via one primitive (DRY - upstream has one
`flist_sort_and_clean`).

### Tie-break mirrored (`flist.c:3050-3082`)

- file vs file, same name: keep the **first**.
- directory vs plain file, same name: keep the **directory** "because it might
  have contents in the list" (`flist.c:3060`).
- directory vs directory, same name: keep the first, merge content-dir flag.

Dedup is gated on the same non-iconv condition as the sort: under `--iconv`,
upstream keeps `flist->files[]` in scan order and only reorders/clears a
parallel `flist->sorted[]` pointer array, which oc does not maintain, so we
skip the in-place reorder+dedup to preserve NDX order (matching the existing
sort-suppression rationale).

### Tests (encode WHY)

- `receiver_removes_duplicate_file_name` - a name emitted twice collapses to
  one entry (RED before this change).
- `receiver_keeps_distinct_names` - distinct names sharing a prefix/basename
  all survive (no over-dedup).
- `receiver_keeps_directory_over_file_duplicate` - pins the dir-over-file
  survivor.

All three drive the real `receive_file_list` decode path with crafted wire
buffers via the existing receiver flist harness.

### Overlap with the INC_RECURSE sub-list dedup work

This closes the duplicate-clean portion of `flist_sort_and_clean` for the
**initial received file list** only. The INC_RECURSE per-sub-list decode path
(`receive_one_extra_segment`) still sorts each segment without a dedup pass;
that, plus the sub-list path-belongs check, remains a separate concern and is
not touched here.

Upstream reference: `flist.c:3016-3082` (`flist_sort_and_clean`), receiver call
site `flist.c:2771`.
