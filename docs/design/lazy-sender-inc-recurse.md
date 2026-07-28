# Lazy sender under INC_RECURSE - conversion plan (LS-1)

Status: design note gating LS-2..LS-4. No code changes in this stage.

## 1. Problem

The oc sender pre-builds the ENTIRE file list before the first byte of the
transfer: walk everything, sort everything, partition everything, then stream
segments out of the finished array. Resident memory is O(N) in the total file
count - measured 405 MB at 1M files vs upstream's flat 8 MB. Upstream stays
flat because under INC_RECURSE it never materializes the whole list: it builds
only the first segment eagerly, expands one diverted directory per extra list
on demand, and frees each list once the receiver is done with it.

The measured sender 2x-vs-local overhead is diffuse build-all cost - the
parallel stat index, sort transients, Vec-doubling slack, and un-interned
dirnames all scale with N. A per-segment build subsumes all of them: each
transient becomes O(segment).

## 2. Upstream model (rsync 3.4.4, protocol 32)

### 2.1 Data structures

- Three globals: `cur_flist`, `first_flist`, `dir_flist`
  (`// upstream: flist.c:101`). Transfer lists form a doubly linked ring
  (`first_flist` -> ... -> `cur_flist` -> ...); `dir_flist` is a separate
  FLIST_TEMP list holding only directory entries, alive for the whole run -
  the accepted O(#dirs) memory floor
  (`// upstream: flist.c:2268`, FLIST_TEMP pool branch `flist.c:2948`).
- `flist_new` chains each new transfer list after the previous one with
  `ndx_start = prev->ndx_start + prev->used + 1`
  (`// upstream: flist.c:2966`) - the +1 NDX gap between segments.
- `flist_done_allocating` (`// upstream: flist.c:335`) records the pool
  boundary of a finished list so a later free releases exactly that list's
  extent.

### 2.2 Initial list - `send_file_list` (`// upstream: flist.c:2227`)

- INC_RECURSE setup: a separate `dir_flist` is created and FLAG_DIVERT_DIRS
  (`// upstream: rsync.h:99`) is added to the walk flags
  (`// upstream: flist.c:2267-2270`); the non-inc path aliases
  `dir_flist = cur_flist` (`// upstream: flist.c:2272`).
- `make_file` allocates a directory entry from `dir_flist->file_pool` when
  diverting, with extra room for parent/sibling/child node links
  (`// upstream: flist.c:1376-1384`).
- `send_directory` scans exactly ONE directory; it only recurses into
  subdirectories when `!divert_dirs` (`// upstream: flist.c:1929-1933`). So
  the initial list holds the arguments plus the first-level children of
  directory arguments; every subdirectory is diverted into `dir_flist`
  instead of being descended.
- Tail: `add_dirs_to_tree(-1, flist, stats.num_dirs)` links the initial
  list's dirs into the traversal tree (`// upstream: flist.c:2571`,
  function at `flist.c:1799`), `file_total`/`file_old_total` both grow by
  the list's size so the lookahead backlog starts at 0
  (`// upstream: flist.c:2545-2546`), and a transfer with no queued dirs
  emits NDX_FLIST_EOF immediately (`// upstream: flist.c:2576`). A 1-entry
  initial list triggers one eager extra list to detect 1-file transfers
  (`// upstream: flist.c:2585`).
- Id lists are NOT sent up front under INC_RECURSE
  (`// upstream: flist.c:2548-2549`); uid/gid names ride inline with the
  entries that introduce them.

### 2.3 Lazy expansion - `send_extra_file_list` (`// upstream: flist.c:2124`)

Called from the send loop top and bottom
(`// upstream: sender.c:231,265`) with `at_least = MIN_FILECNT_LOOKAHEAD`.

- Loop condition: `while (file_total - file_old_total < at_least)`
  (`// upstream: flist.c:2139`) - the backlog of entries queued in lists
  beyond the one the receiver is working through.
- Per iteration: take the dir at `send_dir_ndx` from `dir_flist`, allocate a
  fresh transfer list (`// upstream: flist.c:2145`), announce it with
  `write_ndx(f, NDX_FLIST_OFFSET - dir_ndx)` (`// upstream: flist.c:2152`,
  constant `rsync.h:311`), then `send1extra` (`// upstream: flist.c:2046`)
  scans that ONE directory via `send_directory` with
  `FLAG_DIVERT_DIRS | FLAG_CONTENT_DIR` (`// upstream: flist.c:2050`) -
  newly found subdirs are again diverted, not descended.
- Duplicate dirs (same name from multiple args, FLAG_DUPLICATE) are coalesced
  into the same extra list (`// upstream: flist.c:2160-2172`).
- Each extra list is sorted and cleaned independently
  (`flist_sort_and_clean(flist, 0)`), its dirs are linked into the tree
  (`// upstream: flist.c:2192`), and `file_total += flist->used`
  (`// upstream: flist.c:2195`).
- Depth-first cursor advance over the dir tree: first child, else next
  sibling, else pop to parent; popping past the root writes NDX_FLIST_EOF
  and sets `flist_eof` (`// upstream: flist.c:2207`).

### 2.4 Freeing - `flist_free` (`// upstream: flist.c:2980`)

The RSS mechanism. On each receiver NDX_DONE for a finished list the sender
does `file_old_total -= first_flist->used; flist_free(first_flist)`
(`// upstream: sender.c:246-252`), unlinking the oldest ring entry and
releasing its pool extent via `pool_free_old(flist->file_pool,
flist->pool_boundary)` (`// upstream: flist.c:3006`). The FLIST_TEMP
`dir_flist` is never freed mid-run (`// upstream: flist.c:2983`). Resident
transfer-list memory is therefore O(in-flight lists), never O(total).

### 2.5 Pacing

- `MIN_FILECNT_LOOKAHEAD 1000` / `MAX_FILECNT_LOOKAHEAD 10000`
  (`// upstream: rsync.h:151-152`).
- Send loop keeps at least MIN queued ahead (`// upstream: sender.c:231,265`).
- While blocked waiting for input, `perform_io` opportunistically sends more
  lists until MAX is queued (`// upstream: io.c:753-758,771-775`), gated by
  `extra_flist_sending_enabled` (`// upstream: sender.c:232,238`).
- The receiver applies the mirror-image half-window
  (`// upstream: generator.c:2231,2302`).

### 2.6 Gating predicate

`set_allow_inc_recurse` (`// upstream: compat.c:162-180`): cleared when
`!recurse || use_qsort`, when a receiving side uses
`delete_before/delete_after/delay_updates/prune_empty_dirs`, or when a server
peer did not advertise `i`. The server folds it into CF_INC_RECURSE
(`// upstream: compat.c:713`); both sides then set `inc_recurse` from the
negotiated flag (`// upstream: compat.c:746`).

## 3. oc today - pre-build-all sites

The sender-side machinery lives in `crates/transfer/src/generator/`.

Build chain (`crates/transfer/src/generator/transfer/orchestrator.rs:85-103`):
`build_file_list` (or `build_file_list_with_base`) ->
`partition_file_list_for_inc_recurse` -> `send_file_list`.

- Full walk: `build_file_list`
  (`crates/transfer/src/generator/file_list/mod.rs:74`) recurses the whole
  tree up front - `walk_path_with_metadata` descends every directory
  (`crates/transfer/src/generator/file_list/walk.rs:186,301-311`) via
  `scan_directory_batched` (`walk.rs:402`), which fans stat calls out to
  rayon (`crates/transfer/src/generator/file_list/batch_stat.rs:38`).
- Full retention: every entry lands in `GeneratorContext::file_list` with a
  parallel `source_bases: Vec<Arc<Path>>`
  (`crates/transfer/src/generator/context.rs:59,74`).
- Full sort + dedup: one global permutation sort and duplicate pass over N
  (`mod.rs:136-151`; `crates/protocol/src/flist/dual.rs:129,183`), with an
  N-sized index Vec and sort-key transients
  (`crates/protocol/src/flist/sort.rs:195-250`).
- Full hardlink/id pass: `assign_hardlink_indices` and
  `collect_id_mappings` iterate the complete list
  (`crates/transfer/src/generator/file_list/hardlinks.rs:34,85`).
- Full partition: `partition_file_list_for_inc_recurse`
  (`crates/transfer/src/generator/file_list/inc_recurse.rs:45`) classifies
  all N entries (`inc_recurse.rs:78`) and then REORDERS them through a
  second full-size `Vec<Option<FileEntry>>` (`inc_recurse.rs:175,185-198`) -
  peak RSS is briefly ~2x the list during this move.
- Segments are ranges into the flat array: `PendingSegment`
  (`crates/transfer/src/generator/segments.rs:36`) carries
  `flist_start/count`, not its own storage.
- Initial send: `send_file_list` writes only the first
  `initial_segment_count` entries
  (`crates/transfer/src/generator/protocol_io.rs:727,754`); extra lists go
  out via `encode_and_send_segment` (`protocol_io.rs:803`) driven by the
  `SegmentScheduler` (`segments.rs:113`) from the transfer loop
  (`crates/transfer/src/generator/transfer/transfer_loop.rs:191-204`), with
  the end-of-run flush and NDX_FLIST_EOF at `transfer_loop.rs:1131-1144`.
- Pacing already mirrors upstream's MIN window: `MIN_FILECNT_LOOKAHEAD`
  and the backlog accounting live in `SegmentScheduler::next_to_send` /
  `retire_current_flist` (`segments.rs:24,148,179`).
- Partial freeing already exists: on each sub-list NDX_DONE the loop calls
  `reclaim_oldest_segment` then `retire_current_flist`
  (`transfer_loop.rs:422,429`; `context.rs:946`), which drops each entry's
  heap payloads in place (`crates/protocol/src/flist/dual.rs:246`;
  `crates/protocol/src/flist/entry/accessors.rs:696`).

Why the existing reclaim does not fix RSS: it trims heap payloads AFTER the
whole list was built, sorted, and reordered - the peak is set before the
first segment ships. And reclaimed slots keep their fixed-size `FileEntry`
structs and the Vec capacity, so even post-peak residency stays O(N).

The gating predicate already matches upstream: `compute_allow_inc_recurse`
(`crates/transfer/src/lib.rs:428,643-644`) mirrors `compat.c:162-180`.
Note: the `incremental-flist` cargo feature (default-on,
`crates/transfer/Cargo.toml:62,134`) gates receiver-side machinery, not this
sender path.

## 4. Staged conversion

### LS-2 - dir queue structure (data structure only, wire-unchanged)

Introduce oc's `dir_flist` analog: a `DirQueue` owned by `IncrementalState`
(`segments.rs:210`) holding, per pending directory, its retained `FileEntry`,
wire `dir_ndx`, source base, and parent/first-child/next-sibling links -
upstream's DIRNODE trio (`// upstream: flist.c:1380-1383,1799`). Dir entries
are retained for the whole run, exactly as upstream's FLIST_TEMP list; this
is the O(#dirs) floor.

- Populate it from the existing classification: diverted dirs go into the
  queue in the same depth-first order `DirectoryTree::next_directory` yields
  today (`inc_recurse.rs:246`), so segment emission order is unchanged.
- `PendingSegment` gains nothing; the scheduler, NDX table
  (`ndx_segments`, `segments.rs:239`) and all wire paths are untouched.
- The full pre-build stays in place in this stage; LS-2 is refactoring the
  bookkeeping so LS-3 can swap the producer.

### LS-3 - per-segment scan + free processed segments (the RSS win)

Replace "walk all, sort all, reorder all" with upstream's shape:

- Initial list: walk arguments plus first-level children of dir arguments
  only (`// upstream: flist.c:1929-1933,2493`); push subdirs onto the
  `DirQueue` instead of descending. Sort, dedup, iconv-drop
  (`mod.rs:132`), and hardlink/id collection run over this segment only.
- On demand: when `SegmentScheduler::next_to_send` admits a segment, pop the
  next queued dir, scan that ONE directory (`send_directory` equivalent:
  `scan_directory_batched` bounded to one level), sort its children with the
  same comparator (per-segment sort == global sort restricted to the
  segment, since the global order is (dir, name)-lexicographic), assign
  NDX values from the running `ndx_start + used + 1` counter
  (`// upstream: flist.c:2966`), encode, ship, and append newly found dirs
  to the queue.
- Free: on the receiver's per-list NDX_DONE, drop the retired segment's
  storage entirely (today's `reclaim_oldest_segment` becomes a real free of
  a per-segment `Vec`, `// upstream: sender.c:248`, `flist.c:3006`).
  `resolve_itemize_ndx` and the delta loop resolve NDX values through the
  live-segment window (the existing `ndx_segments` +
  `segment_parent_flat` tables, `segments.rs:239,258`) instead of one flat
  O(N) array; a gap NDX resolves to the owning dir's entry in the
  `DirQueue` (`// upstream: sender.c:267-272`).
- Pacing: reuse the existing `SegmentScheduler` MIN-lookahead window
  unmodified - the scan happens when the scheduler admits the segment, so
  the window itself bounds resident segments. This is pure backpressure; no
  controller, no new tuning knobs. (Upstream's idle-time MAX-lookahead
  fill, `// upstream: io.c:753-758`, is a pre-existing oc gap and stays out
  of scope.)
- Incremental cross-segment state, all O(state) not O(N): hardlink
  dev/ino -> first-NDX map (replaces the post-sort full pass,
  `hardlinks.rs:34`; upstream inits at `// upstream: flist.c:2262`),
  uid/gid interning as entries are created (upstream sends ids inline under
  INC_RECURSE, `// upstream: flist.c:2548-2549`), and the cached
  `FileListWriter` compression state already carried across sub-lists
  (`segments.rs:225`).
- The non-inc path keeps the current build-all pipeline untouched: the lazy
  producer is selected by the same `inc_recurse()` test that gates
  partitioning today (`inc_recurse.rs:46`).

### LS-4 - validation

- Wire identity: byte-for-byte capture of oc-before vs oc-after and vs
  upstream 3.4.4 across local, daemon, and ssh transports - segment
  content, order, per-segment end markers, NDX_FLIST_OFFSET headers, +1 NDX
  gaps, and NDX_FLIST_EOF placement must be identical. Existing golden and
  interop suites plus a deep-tree fixture whose segment count exceeds the
  lookahead window.
- RSS: 1M-file benchmark (containerized, non-bind-mounted data dir) showing
  sender peak RSS O(segment window); target parity-class with upstream's
  flat profile, closing the measured 49.9x gap.
- Behavior: full nextest + upstream-testsuite + interop matrix green;
  `--dry-run`, `--itemize-changes`, hardlinks, `--iconv`, and daemon module
  paths exercised under INC_RECURSE.

## 5. Invariants each stage preserves

- Wire fidelity first: identical segment content and order, identical
  end-of-list markers and io_error propagation, identical NDX framing
  (NDX_FLIST_OFFSET header, +1 gaps, NDX_FLIST_EOF), identical flist stats
  (`stats.flist_size`, file counts).
- The non-inc sender path is unchanged in every stage.
- The INC_RECURSE gate stays exactly upstream's predicate
  (`compat.c:162-180` == `lib.rs:428`); no new negotiation, no new
  capability advertisement.
- Existing observable divergences (e.g. sender-side duplicate handling vs
  upstream's FLAG_DUPLICATE coalescing, `// upstream: flist.c:2160-2172`)
  are not silently changed by LS stages; any fix there is a separate,
  wire-verified task.

## 6. Build transients capped by construction

Per-segment building bounds every measured build-all overhead:

- `source_bases`: one interned `Arc<Path>` set per live segment instead of
  an N-length parallel Vec (`context.rs:74`).
- Sort: index and key transients sized to the segment
  (`dual.rs:129`, `sort.rs:195-250`), not N.
- Vec slack: per-segment Vecs pre-sized to the scanned child count (known
  after the one-level `read_dir` batch), eliminating doubling waste; the
  reorder's second full-size `Vec<Option<FileEntry>>`
  (`inc_recurse.rs:185-198`) disappears outright.
- Dirnames: only the `DirQueue` retains directory paths; file entries free
  their names with their segment.

## 7. RSS target

Resident sender memory becomes O(segment window) - bounded by
MIN_FILECNT_LOOKAHEAD-driven in-flight lists plus the O(#dirs) `DirQueue` -
instead of O(N). At 1M files this replaces the measured 405 MB (49.9x
upstream) with a flat profile in upstream's class.
