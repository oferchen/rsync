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

### Sender-side dedup (NDX-sync fix)

A receiver-only dedup desyncs the wire NDX: oc's sender never ran
`flist_sort_and_clean`, so with `--files-from` a redundant list entry produced
a duplicate name in the transmitted list that the new receiver pass then
removed - the receiver's entry count no longer matched the sender's NDX
numbering, and the remote leg died with `RERR_PROTOCOL` (code 16). The local
leg was unaffected because sender and receiver share one in-memory list.

Upstream runs the pass on BOTH sides (`send_file_list` at `flist.c:2544`,
`recv_file_list` at `flist.c:2771`); the sender dedups first so the receiver's
identical pass is idempotent. This PR mirrors that: a parallel-aware
`DualFileList::dedup_with_parallel` runs on the sender right after the sort,
removing duplicates while keeping the generator's `source_bases` array aligned.
Both sides now share one tie-break helper (`resolve_duplicate`), so the sender
and receiver converge to the same sorted+cleaned list.

`am_sender` asymmetry and `strip_root`: upstream's sender marks a duplicate
directory `FLAG_DUPLICATE` and keeps it for its in-place `dir_flist` tree
(`flist.c:3067`); oc has no such tree and converges both sides to the same list,
so keeping a dup dir only on the sender would itself desync the NDX count.
oc therefore drops symmetrically - verified dest-correct for INC_RECURSE
(recursive push/pull and `--relative` overlapping paths) over `lsh`. The dedup
primitive does not implement `strip_root` (no leading-slash strip), so neither
side's `strip_root` semantics change.

Verified locally over `support/lsh.sh`: all four `files-from` host combinations
(both `filehost` x `srchost`) exit 0 with `todir == chkdir`; plain and recursive
remote push/pull match a local copy byte-for-byte (no NDX shift on
duplicate-free lists).

### Overlap with the INC_RECURSE sub-list dedup work

This closes the duplicate-clean portion of `flist_sort_and_clean` for the
**initial received file list** only. The INC_RECURSE per-sub-list decode path
(`receive_one_extra_segment`) still sorts each segment without a dedup pass;
that, plus the sub-list path-belongs check, remains a separate concern and is
not touched here.

Upstream reference: `flist.c:3016-3082` (`flist_sort_and_clean`), receiver call
site `flist.c:2771`.
