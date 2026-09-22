# Lazy sender under INC_RECURSE - producer specification (LF-0a) and conversion plan (LS-1)

Status: design note gating LS-2..LS-4 (= LF-2..LF-8). No code changes in this
stage.

Companion documents - this is deliberately NOT a third document:

- `docs/design/rss-flist-segmentation.md` is the segment CONTAINER design
  (the `struct file_list` store, NDX resolution, segment free). Its Section 3
  holds the field-by-field upstream-vs-oc table, and its Section 3.2 the
  allocation-pool and index-range field semantics.
- This document is the lazy PRODUCER specification: how upstream builds,
  paces, and frees the incremental lists, and how oc converts to that shape.

Anchor provenance: every upstream `file:line` below was re-verified by reading
`target/interop/upstream-src/rsync-3.5.0/` (the current source-of-truth pin).
The original LS-1 capture cited 3.4.4 line numbers; those no longer resolve
and have been retargeted here (rsync.h and compat.c anchors were the only
survivors). Section 10 is the consolidated verified anchor table.

## 1. Problem

The oc sender pre-builds the ENTIRE file list before the first byte of the
transfer: walk everything, sort everything, partition everything, then stream
segments out of the finished array. Resident memory is O(N) in the total file
count, while upstream stays flat because under INC_RECURSE it never
materializes the whole list: it builds only the first segment eagerly, expands
one diverted directory per extra list on demand, and frees each list once the
receiver is done with it. Section 9 carries the measured numbers.

The measured build-all overhead is diffuse - the parallel stat index, sort
transients, Vec-doubling slack, and un-interned dirnames all scale with N. A
per-segment build subsumes all of them: each transient becomes O(segment).

## 2. Upstream model (rsync 3.5.0, protocol 32)

### 2.1 Data structures

- `struct file_list` (`rsync.h:983-994`): `next`/`prev` ring links,
  `files`/`sorted` pointer arrays, `file_pool` + `pool_boundary` (allocation
  pool), `used`/`malloced`, `low`/`high` (index range), `ndx_start`,
  `flist_num`, `parent_ndx`, `in_progress`/`to_redo`. The per-field
  upstream-vs-oc mapping lives in `rss-flist-segmentation.md` Section 3.1;
  pool and index-range semantics in its Section 3.2.
- Three globals: `cur_flist, first_flist, dir_flist` (`flist.c:107`).
  Transfer lists form a doubly linked ring; `dir_flist` is a separate
  FLIST_TEMP list holding only directory entries, alive for the whole run -
  the accepted O(#dirs) memory floor.
- `flist_new` (`flist.c:3244-3279`): a FLIST_TEMP list gets its OWN
  SMALL_EXTENT pool (`:3250-3252`); the first transfer list creates the
  chain's single NORMAL_EXTENT pool and starts at
  `ndx_start = flist_num = inc_recurse ? 1 : 0` (`:3256-3262`); every later
  transfer list ALIASES that same pool (`:3266`) and chains
  `ndx_start = prev->ndx_start + prev->used + 1` (`:3268`) - the +1 NDX gap
  between segments. Each list snapshots `pool_boundary` at creation
  (`:3274`).
- `flist_done_allocating` (`flist.c:418-425`) seals a finished list's pool
  extent via `pool_boundary(pool, 8*1024)` so a later free releases exactly
  that list's extent (`lib/pool_alloc.c:353 pool_boundary`,
  `:300 pool_free_old`).
- Growth policy: `flist_expand` (`flist.c:366`) with `FLIST_START` 32,
  `FLIST_START_LARGE` 32*1024, `FLIST_LINEAR` (`rsync.h:965-967`); both the
  initial list and `dir_flist` are pre-expanded to FLIST_START_LARGE
  (`flist.c:2538,2541`).

### 2.2 The directory tree - THREE node links, not two

Correction of record (task 719): an earlier plan draft modeled the dir tree
with two links. Upstream has THREE, and without `DIR_FIRST_CHILD` the
depth-first walk cannot descend.

- Each diverted directory entry carries a 3-slot node
  (`DIRNODE_EXTRA_CNT` = 3, `rsync.h:875`) addressed by `F_DIR_NODE_P`
  (`rsync.h:928`), with `DIR_PARENT(a) = (a)[0]`,
  `DIR_FIRST_CHILD(a) = (a)[1]`, `DIR_NEXT_SIBLING(a) = (a)[2]`
  (`rsync.h:955-957`).
- `make_file` reserves the node under FLAG_DIVERT_DIRS and allocates the
  entry from `dir_flist->file_pool` (`flist.c:1527-1530`).
- `add_dirs_to_tree(parent_ndx, from_flist, dir_cnt)` (`flist.c:1964-2002`)
  appends the scanned list's dirs to `dir_flist` in sorted order (the
  sending side keeps `dir_flist->sorted = dir_flist->files`,
  `flist.c:1972-1974`), links the first dir as the parent's
  `DIR_FIRST_CHILD` (`:1991`) and later ones as the previous dir's
  `DIR_NEXT_SIBLING` (`:1989`), initializes each node with
  `DIR_PARENT = parent_ndx`, `DIR_FIRST_CHILD = -1` (`:1996-1997`), and
  terminates the last sibling with -1 (`:2000`). "." entries are appended
  but not linked as children.
- The walk cursor is `send_dir_ndx`/`send_dir_depth`; the initial list's
  dirs are rooted with `add_dirs_to_tree(-1, flist, stats.num_dirs)`
  (`flist.c:2843`) after `send_dir_depth = 1` (`:2842`).
- Naming note: 3.5.0 has NO `F_DIR_DEFER` accessor (zero occurrences in the
  tree); it appears only in older sources and stale notes. The 3.5.0
  dir-node accessors are exactly the set above.

### 2.3 Initial list - `send_file_list` (`flist.c:2499`)

- INC_RECURSE setup (`flist.c:2537-2544`): the transfer list and a separate
  `dir_flist = flist_new(FLIST_TEMP, ...)` are created and
  `FLAG_DIVERT_DIRS` (`rsync.h:99`) is added to the walk flags; the non-inc
  path aliases `dir_flist = cur_flist`.
- `send_directory` (`flist.c:2092`) scans exactly ONE directory; it only
  recurses into subdirectories when `!divert_dirs`
  (`flist.c:2099` flag decode, `:2200` recursion gate). So the initial list
  holds the arguments plus the first-level children of directory arguments;
  every subdirectory is diverted into `dir_flist` instead of being
  descended.
- --relative deferral: implied-dir names are NOT expanded up front. Each
  pending name is queued on its lastpath dir entry through
  `F_DIR_RELNAMES_P` (`rsync.h:930`; producer `flist.c:2293-2296`) and
  replayed per directory by `send1extra` (Section 2.4).
- Tail (`flist.c:2811-2858`): `sorted` alias-or-clone (`:2811-2815`, see
  Section 2.7), `flist_sort_and_clean(flist, 0)` (`:2816`), then BOTH
  `file_total` and `file_old_total` grow by the list's size (`:2817-2818`)
  so the lookahead backlog starts at 0. Id lists are NOT sent up front under
  INC_RECURSE (`:2820-2821`); uid/gid names ride inline with the entries
  that introduce them. `add_dirs_to_tree(-1, ...)` roots the tree (`:2843`),
  `flist_done_allocating` seals the pool extent (`:2846`), a transfer with
  no queued dirs emits NDX_FLIST_EOF immediately (`:2847-2853`), and a
  1-entry initial list triggers one eager extra list to detect 1-file
  transfers (`:2854-2858`, `send_extra_file_list(f, 1)` at `:2857`).

### 2.4 Lazy expansion - `send_extra_file_list(f, at_least)` (`flist.c:2396`)

Called from the send loop top and bottom (`sender.c:515,549`) with
`at_least = MIN_FILECNT_LOOKAHEAD`, and from `perform_io` with -1
(`io.c:855`), which means "one more list" (`flist.c:2406-2407`).

- Loop condition: `while (file_total - file_old_total < at_least)`
  (`flist.c:2411`) - the backlog of entries queued in lists beyond the one
  the receiver is working through.
- Per iteration: take the dir at `send_dir_ndx` from `dir_flist->sorted`
  (`:2412`), allocate a fresh transfer list (`:2417`), announce it with
  `write_ndx(f, NDX_FLIST_OFFSET - dir_ndx)` (`:2424`, constant
  `rsync.h:318`), remember the sorted ndx in `flist->parent_ndx` (`:2425`),
  then `send1extra` (`flist.c:2317`) expands that ONE directory:
  `change_pathname` + `change_local_filter_dir(fbuf, dlen, send_dir_depth)`
  (`:2328-2331`), and - only if the dir still carries FLAG_CONTENT_DIR - a
  one-level `send_directory` with `FLAG_DIVERT_DIRS | FLAG_CONTENT_DIR`
  (`:2321,2333-2342`), so newly found subdirs are again diverted, not
  descended. The dir's deferred --relative names (`F_DIR_RELNAMES_P`) are
  then replayed (`:2346-2394`).
- Duplicate dirs (same name from multiple args, FLAG_DUPLICATE) are
  coalesced into the same extra list (`:2434-2444`), skipping a rescan of a
  same-pathname content dir (`:2439-2440`). See Section 2.8.
- Each extra list is closed with an end-of-flist marker (`:2446-2453`,
  io_error/safe-flist arms), gets its `sorted` alias-or-clone
  (`:2455-2459`), is sorted and cleaned independently
  (`flist_sort_and_clean(flist, 0)`, `:2462`), its dirs are linked into the
  tree (`add_dirs_to_tree(send_dir_ndx, flist, ...)`, `:2464`), its pool
  extent is sealed (`:2465`), and `file_total += flist->used` (`:2467`).
- Depth-first cursor advance over the dir tree (`:2473-2491`): descend to
  `DIR_FIRST_CHILD` if any (`:2473-2475`); otherwise pop to `DIR_PARENT`
  while there is no `DIR_NEXT_SIBLING` (`:2477-2489`) - popping past the
  root writes NDX_FLIST_EOF, sets `flist_eof = 1`, and resets the local
  filter state (`change_local_filter_dir(NULL, 0, 0)`, `:2478-2484`); else
  move to `DIR_NEXT_SIBLING` (`:2490`).
- Deferred io_error is flushed at `finish` for protocol 30 (`:2495-2497`).

### 2.4b Receiving an extra list - `recv_file_list(f, dir_ndx)` (`flist.c:2929`)

The receiver frames each sub-list by `dir_ndx` - an index into ITS OWN
`dir_flist`, not the transfer list - and defends that numbering:

- dir_ndx guards (`flist.c:2895-2927`): a sub-flist after the final list was
  freed, an out-of-range `dir_ndx`, a `dir_ndx` whose slot
  `flist_sort_and_clean` cleared, and a duplicate flist for a dir already
  marked FLAG_GOT_DIR_FLIST are each refused with `RERR_PROTOCOL`.
- The hard dirname invariant (`flist.c:2976-2991`): inside the read loop,
  whenever an entry's dirname changes to a value that is neither the cached
  `good_dirname` nor `f_name(dir_flist->files[dir_ndx])`, the receiver
  prints "ABORTING due to invalid path from sender: %s/%s" (`:2984-2986`)
  and exits `RERR_UNSUPPORTED` (`:2987`). It fires when a sender smuggles an
  entry outside the directory the sub-list announced.
- The receiver grows its own `dir_flist` in the same read loop
  (`flist.c:2996-2998`) and sorts just the appended range
  (`:3041-3050`), before `flist_sort_and_clean(flist, relative_paths)`
  (`:3065`).

### 2.5 Freeing - `flist_free` (`flist.c:3282`)

The RSS mechanism. On each receiver NDX_DONE for a finished list the sender
does `file_old_total -= first_flist->used; flist_free(first_flist)`
(`sender.c:530-532`), echoes NDX_DONE and continues without advancing phase
while more lists remain (`sender.c:533-538`); the phase advances only once
the chain is empty (`:540-545`).

`flist_free` unlinks the ring entry (`flist.c:3284-3303`), then releases the
list's pool extent via `pool_free_old(flist->file_pool, flist->pool_boundary)`
(`:3308`) - or `pool_destroy` when the chain empties or the list is the
FLIST_TEMP `dir_flist` (`:3305-3306`) - and frees `sorted` (only when it is a
clone, `:3310-3311`), `files`, and the object (`:3312-3313`). The FLIST_TEMP
`dir_flist` is never freed mid-run. Resident transfer-list memory is
therefore O(in-flight lists), never O(total).

NDX resolution against the live window: `flist_for_ndx` (`rsync.c:951-984`)
walks the ring from `cur_flist` in either direction until
`ndx_start-1 <= ndx < ndx_start + used`; out of range is the fatal
"File-list index %d not in %d - %d" protocol error. A gap NDX
(`ndx == ndx_start - 1`) resolves to the segment's parent directory entry in
`dir_flist` (`sender.c:551-557`); a cleared slot is refused
(`sender.c:558-560`).

### 2.6 Pacing

- `MIN_FILECNT_LOOKAHEAD 1000` / `MAX_FILECNT_LOOKAHEAD 10000`
  (`rsync.h:151-152`).
- The send loop keeps at least MIN queued ahead (`sender.c:515,549`), and
  brackets the blocking read with `extra_flist_sending_enabled`
  (`sender.c:516,522`).
- While blocked waiting for input, `perform_io` opportunistically produces
  more lists until MAX is queued: under the ceiling it polls with
  `poll_timeout = 0` (`io.c:836-844`) and, when the socket is idle, calls
  `send_extra_file_list(sock_f_out, -1)` (`io.c:853-856`).
- The generator applies the mirror-image half-window: a flush hint when the
  remaining backlog drops under MIN/2 (`generator.c:2703`) and, with
  hardlinks, an initial wait until MIN/2 entries exist
  (`generator.c:2775`).

### 2.7 The sorted-vs-files rule

`sorted` is USUALLY AN ALIAS of `files`, not a copy: both the initial and
every extra list set `flist->sorted = flist->files` unless
`need_unsorted_flist` (the iconv/unsorted-index case) forces a clone of the
POINTER array (`flist.c:2455-2459`, `:2811-2815`). `flist_sort_and_clean`
sorts `flist->sorted` (`flist.c:3331`) - the pointer array, never the
ndx-addressed `files[]` order - and computes `low`/`high`
(`:3325-3328` empty case; scan from both ends otherwise). The sending side
keeps `dir_flist` sorted-as-built (`flist.c:1972-1974`). `flist_free` frees
`sorted` only when `sorted != files` (`:3310-3311`).

### 2.8 Duplicate-directory merge under inc_recurse

`flist_sort_and_clean` resolves same-name entries by keeping a dir over a
non-dir and otherwise the first occurrence; for two dirs on the SENDER under
inc_recurse the later one is MARKED `FLAG_DUPLICATE` (`rsync.h:84`) and kept
(`flist.c:3374-3375`) - the receiver instead merges the vital flags onto the
kept entry (`:3376-3378`). `send_extra_file_list` then expands the
FLAG_DUPLICATE run as ONE extra list (`flist.c:2434-2444`), so a directory
reachable through multiple arguments is announced once and its children are
listed once.

### 2.9 Gating predicate

`set_allow_inc_recurse` (`compat.c:162-181`, clauses `:172-180`): cleared
when `!recurse || use_qsort`, when a receiving side uses
`delete_before/delete_after/delay_updates/prune_empty_dirs`, or when a server
peer did not advertise `i`. The server folds it into CF_INC_RECURSE
(`compat.c:724`); a client that forced --inc-recursive against a
non-advertising server errors out (`compat.c:780`). oc's mirror is
`compute_allow_inc_recurse` (`crates/transfer/src/lib.rs:549`), which also
carries the (currently load-bearing) receiver-role restriction - see
Section 9.

## 3. oc today - pre-build-all sites

State note: the oc `file:line` references in this section were captured at
LS-1 time. Since then the NdxMap consolidation landed (`ndx_segments` and
`segment_parent_flat` collapsed into `NdxMap`,
`crates/transfer/src/generator/ndx_map.rs`, consumed by
`generator/segments.rs`), and `compute_allow_inc_recurse` now lives at
`crates/transfer/src/lib.rs:549`. Re-verify any other oc line below before
acting on it; the upstream anchors above are the verified ones.

The sender-side machinery lives in `crates/transfer/src/generator/`.

Build chain (`crates/transfer/src/generator/transfer/orchestrator.rs`):
`build_file_list` (or `build_file_list_with_base`) ->
`partition_file_list_for_inc_recurse` -> `send_file_list`.

- Full walk: `build_file_list`
  (`crates/transfer/src/generator/file_list/mod.rs`) recurses the whole
  tree up front - `walk_path_with_metadata` descends every directory
  (`crates/transfer/src/generator/file_list/walk.rs`) via
  `scan_directory_batched`, which fans stat calls out to rayon
  (`crates/transfer/src/generator/file_list/batch_stat.rs`).
- Full retention: every entry lands in `GeneratorContext::file_list` with a
  parallel `source_bases: Vec<Arc<Path>>`
  (`crates/transfer/src/generator/context.rs`).
- Full sort + dedup: one global permutation sort and duplicate pass over N
  (`crates/protocol/src/flist/dual.rs`, `crates/protocol/src/flist/sort.rs`).
- Full hardlink/id pass: `assign_hardlink_indices` and
  `collect_id_mappings` iterate the complete list
  (`crates/transfer/src/generator/file_list/hardlinks.rs`).
- Full partition: `partition_file_list_for_inc_recurse`
  (`crates/transfer/src/generator/file_list/inc_recurse.rs`) classifies
  all N entries and then REORDERS them through a second full-size
  `Vec<Option<FileEntry>>` - peak RSS is briefly ~2x the list during this
  move.
- Segments are ranges into the flat array: `PendingSegment`
  (`crates/transfer/src/generator/segments.rs`) carries
  `flist_start`/`count`, not its own storage.
- Initial send: `send_file_list` writes only the first
  `initial_segment_count` entries
  (`crates/transfer/src/generator/protocol_io.rs`); extra lists go out via
  `encode_and_send_segment` driven by the `SegmentScheduler`
  (`segments.rs`) from the transfer loop
  (`crates/transfer/src/generator/transfer/transfer_loop.rs`).
- Pacing already mirrors upstream's MIN window: `MIN_FILECNT_LOOKAHEAD`
  and the backlog accounting live in `SegmentScheduler::next_to_send` /
  `retire_current_flist` (`segments.rs`), with the MAX ceiling reachable
  through `next_when_idle` (inert until the producer is lazy - Section 9).
- Partial freeing already exists: on each sub-list NDX_DONE the loop calls
  `reclaim_oldest_segment` then `retire_current_flist`, which drops each
  entry's heap payloads in place. Why that does not fix RSS: it trims heap
  payloads AFTER the whole list was built, sorted, and reordered - the peak
  is set before the first segment ships - and reclaimed slots keep their
  fixed-size `FileEntry` structs and the Vec capacity, so even post-peak
  residency stays O(N). The full analysis is
  `rss-flist-segmentation.md` Sections 1-3.

Note: the `incremental-flist` cargo feature (default-on) gates
receiver-side machinery, not this sender path.

## 4. Staged conversion

### LS-2 - dir queue structure (data structure only, wire-unchanged)

Introduce oc's `dir_flist` analog: a `DirFlist` owned by the incremental
state, holding, per pending directory, its retained `FileEntry`, wire
`dir_ndx`, source base, and the parent/first-child/next-sibling node trio -
upstream's DIRNODE encoding (`rsync.h:955-957`, `flist.c:1527-1530`,
`add_dirs_to_tree` `flist.c:1964`). Dir entries are retained for the whole
run, exactly as upstream's FLIST_TEMP list; this is the O(#dirs) floor.

- Populate it from the existing classification: diverted dirs go into the
  queue in the same depth-first order `DirectoryTree::next_directory`
  yields today, so segment emission order is unchanged.
- ONE-TYPE constraint (LF-8f / task 749): this type IS the IR programme's
  `dir_flist` (IR-5a/5b/5c). LF-1a/1b/1c and IR-5a/5b/5c describe the same
  type reached from two construction sites - the initial-list tail
  (`flist.c:2843`) and the per-extra-list link (`flist.c:2464`). Build it
  once; do not create an LF dir-queue and an IR dir_flist separately.
- `PendingSegment` gains nothing; the scheduler, `NdxMap`, and all wire
  paths are untouched.
- The full pre-build stays in place in this stage; LS-2 is refactoring the
  bookkeeping so LS-3 can swap the producer.

### LS-3 - per-segment scan + free processed segments (the RSS win)

Replace "walk all, sort all, reorder all" with upstream's shape:

- Initial list: walk arguments plus first-level children of dir arguments
  only (`flist.c:2200` recursion gate); push subdirs onto the `DirFlist`
  instead of descending. Sort, dedup, iconv-drop, and hardlink/id
  collection run over this segment only.
- On demand: when `SegmentScheduler::next_to_send` admits a segment, pop
  the next queued dir, scan that ONE directory (`send1extra` equivalent:
  `scan_directory_batched` bounded to one level), sort its children with
  the same comparator (per-segment sort == global sort restricted to the
  segment, since the global order is (dir, name)-lexicographic), assign
  NDX values from the running `ndx_start + used + 1` counter
  (`flist.c:3268`), encode, ship, and append newly found dirs to the
  queue.
- Filter state rides the walk, not the pre-build: enter each directory's
  local filter scope via the `change_local_filter_dir` analog exactly where
  upstream does (`flist.c:2331` per extra dir; reset at EOF
  `flist.c:2483`), so dir-merge (`.rsync-filter`) rules load and unload
  per scanned directory (LF-3a).
- --relative: defer implied-dir names per directory through the
  `F_DIR_RELNAMES_P` analog (`flist.c:2293-2296` produce, `:2346-2394`
  replay), emitting exactly one implied-parent level in the initial list
  (LF-4a/4b).
- Duplicate dirs: mark-and-coalesce per Section 2.8 (`flist.c:3374-3375`,
  `:2434-2444`) so a directory reachable twice is expanded exactly once
  (LF-5a/5b).
- Free: on the receiver's per-list NDX_DONE, drop the retired segment's
  storage entirely (today's `reclaim_oldest_segment` becomes a real free of
  a per-segment store, `sender.c:530-532`, `flist.c:3308`); this is the
  container work specified in `rss-flist-segmentation.md` Section 4.
  `resolve_itemize` and the delta loop resolve NDX values through the
  live-segment window (`NdxMap`); a gap NDX resolves to the owning dir's
  entry in the `DirFlist` (`sender.c:551-557`).
- Pacing: reuse the existing `SegmentScheduler` MIN-lookahead window
  unmodified - the scan happens when the scheduler admits the segment, so
  the window itself bounds resident segments. This is pure backpressure; no
  controller, no new tuning knobs. Upstream's idle-time MAX-lookahead fill
  (`io.c:836-856`) becomes reachable through `next_when_idle` (LF-7b);
  the ceiling is dead code until this stage lands (task 400 caveat).
- Incremental cross-segment state, all O(state) not O(N): hardlink
  dev/ino -> first-NDX map (replaces the post-sort full pass; upstream
  inits hardlinks before the walk, `flist.c:2532-2535`), uid/gid interning
  as entries are created (upstream sends ids inline under INC_RECURSE,
  `flist.c:2820-2821`), and the cached `FileListWriter` compression state
  already carried across sub-lists.
- Per-directory scan errors are REPORTED from the lazy producer (LF-2g,
  closing task 659): a failed one-level scan follows upstream's
  `interpret_stat_error` shape (`flist.c:2004`) rather than silently
  skipping the segment.
- The non-inc path keeps the current build-all pipeline untouched: the lazy
  producer is selected by the same `inc_recurse()` test that gates
  partitioning today.

### LS-4 - validation

The gates in Section 8; wire identity, RSS, and behavior legs as listed
there.

## 5. Invariants each stage preserves

- Wire fidelity first: identical segment content and order, identical
  end-of-list markers and io_error propagation, identical NDX framing
  (NDX_FLIST_OFFSET header, +1 gaps, NDX_FLIST_EOF), identical flist stats
  (`stats.flist_size`, file counts).
- The non-inc sender path is unchanged in every stage.
- The INC_RECURSE gate stays exactly upstream's predicate
  (`compat.c:162-181` == `crates/transfer/src/lib.rs:549`), including the
  oc receiver-role restriction until tasks 309-313 lift it; no new
  negotiation, no new capability advertisement.
- FLAG_DUPLICATE coalescing (Section 2.8) is part of the specification, not
  an observable divergence to preserve: oc's sender-side duplicate handling
  converges on it in LS-3 (LF-5a), with byte-capture proof.

## 6. Build transients capped by construction

Per-segment building bounds every measured build-all overhead:

- `source_bases`: one interned `Arc<Path>` set per live segment instead of
  an N-length parallel Vec.
- Sort: index and key transients sized to the segment, not N.
- Vec slack: per-segment Vecs pre-sized to the scanned child count (known
  after the one-level `read_dir` batch), eliminating doubling waste; the
  reorder's second full-size `Vec<Option<FileEntry>>` disappears outright.
- Dirnames: only the `DirFlist` retains directory paths; file entries free
  their names with their segment.

## 7. RSS target

Resident sender memory becomes O(segment window) - bounded by
MIN_FILECNT_LOOKAHEAD-driven in-flight lists plus the O(#dirs) `DirFlist` -
instead of O(N). See Section 9 for the measured baseline this must close.

## 8. Staging plan - flag and gates

The lazy producer ships behind `OC_RSYNC_LAZY_FLIST`, default OFF (unset,
empty, or `0` = eager producer, byte-identical to today), wired through the
generator at the same seam that selects the INC_RECURSE partition today
(LF-0c). It is an oc-internal staging flag: it never changes a wire byte by
itself, only which producer fills the segments, so no capability or `-e`
letter is involved. Flip to default ON (LF-8e) only on explicit user
direction after all four gates hold:

| Gate | Requirement | Task |
|------|-------------|------|
| G1 byte-neutrality | Flag OFF: wire transcripts byte-identical to pre-change oc on the LF-0b harness (all four network cells) | LF-8a (744) |
| G2 cross-impl interop | Flag ON: lazy oc sender against real upstream 3.5.0 receiver (and 3.0.9/3.1.3/3.4.4 legs), plus oc-oc | LF-8b (745) |
| G3 no deadlock | Flag ON: all four INC_RECURSE cells (daemon pull/push, ssh pull/push) on trees past the 1000-entry boundary (the task-1029 1024-entry repro) | LF-8c (746) |
| G4 RSS flat curve | Flag ON: sender peak RSS O(window) on the 1M-file bench (containerized, non-bind-mounted data dir), closing the Section 9 gap | LF-8d (747) / IR-9d |

The byte-neutrality harness (LF-0b) must exist BEFORE the producer changes;
gates are per-stage, not end-loaded.

## 9. Measured constraints of record (prior measurements - cited, not re-run)

- Deadlock boundary is EXACTLY the MIN_FILECNT_LOOKAHEAD floor of 1000.
  Task-1029 A/B (upstream 3.5.0 `hardlinks` cell, `-aHivv` push, entry-count
  bisect): 400 pass, 961 pass, 1024 DEADLOCK, 1296 DEADLOCK when the
  receiver-role gate term is dropped; both peers block on I/O at 0% CPU.
- oc's RECEIVER never enables INC_RECURSE today, and that role gate is
  LOAD-BEARING: removing it deadlocks a currently-passing cell. Lifting it
  (tasks 309-313) is sequenced behind receiver throttle-safety and the
  delete-pass completeness predicate (task 1165), not behind this producer.
- oc flist RSS is O(N) where upstream's is FLAT (task 739 baseline,
  macOS release build vs the rsync 3.5.0 oracle). Per-entry slope measured
  ~422 B/entry on a loopback client/server pair; an earlier 584 B/entry
  figure is retracted - it came from a local `--list-only` instrument, and
  a local `--list-only` never builds an flist in oc. The 100M-file target
  extrapolates to tens of GB resident vs upstream's ~flat MBs.
- The MAX_FILECNT_LOOKAHEAD ceiling (`next_when_idle`) is dead code under
  the eager producer: `next_to_send` returns None at >= 1000, which
  subsumes >= 10000 (task 400; measured over 20k files, backlog never
  exceeded exactly 1000). It becomes live with this conversion (LF-7b).
- ONE-TYPE constraint (task 749): the LF dir tree (LF-1a/1b/1c) and the IR
  `dir_flist` (IR-5a/5b/5c) are the SAME type at TWO construction sites -
  see Section 4 LS-2. Neither programme builds its own copy.

## 10. Verified anchor table (rsync 3.5.0)

Every row read in `target/interop/upstream-src/rsync-3.5.0/` on the date of
this revision. 3.4.4 equivalents are listed only where a prior oc doc or
comment cites them.

| Construct | 3.5.0 anchor | Notes (3.4.4 origin where retargeted) |
|---|---|---|
| `MIN_FILECNT_LOOKAHEAD 1000` / `MAX_FILECNT_LOOKAHEAD 10000` | `rsync.h:151-152` | unchanged from 3.4.4 |
| `NDX_FLIST_OFFSET -101` | `rsync.h:318` | was `rsync.h:311` |
| `FLAG_CONTENT_DIR` / `FLAG_DUPLICATE` / `FLAG_DIVERT_DIRS` | `rsync.h:81` / `:84` / `:99` | unchanged |
| `DIRNODE_EXTRA_CNT 3` | `rsync.h:875` | |
| `F_DIR_NODE_P` / `F_DIR_RELNAMES_P` | `rsync.h:928` / `:930` | |
| `DIR_PARENT` / `DIR_FIRST_CHILD` / `DIR_NEXT_SIBLING` | `rsync.h:955-957` | THREE macros (task-719 correction) |
| `FLIST_START` / `FLIST_START_LARGE` / `FLIST_LINEAR` | `rsync.h:965-967` | |
| `NORMAL_EXTENT` / `SMALL_EXTENT` / `FLIST_TEMP` | `rsync.h:978-981` | |
| `struct file_list` | `rsync.h:983-994` | was `:964-975`; same twelve members |
| `cur_flist, first_flist, dir_flist` globals | `flist.c:107` | was `:101` |
| `flist_expand` | `flist.c:366` | |
| `flist_done_allocating` (8 KiB pool boundary) | `flist.c:418-425` | was `:335` |
| `make_file` DIRNODE reservation + dir_flist pool | `flist.c:1527-1530` | was `:1376-1384` |
| `add_dirs_to_tree` | `flist.c:1964-2002` (links `:1989,1991,1996-1997,2000`) | was `:1799` |
| `send_directory` (one dir; `!divert_dirs` recursion gate) | `flist.c:2092`, `:2099`, `:2200` | was `:1929-1933` |
| --relative relname deferral (producer) | `flist.c:2293-2296` | |
| `send1extra` | `flist.c:2317-2394` (filter `:2331`, scan `:2333-2342`, relname replay `:2346-2394`) | was `:2046` |
| `send_extra_file_list` | `flist.c:2396-2498` | was `:2124` |
| lookahead loop condition | `flist.c:2411` | was `:2139` |
| `write_ndx(f, NDX_FLIST_OFFSET - dir_ndx)` | `flist.c:2424` | was `:2152` |
| duplicate-dir coalescing loop | `flist.c:2434-2444` | was `:2160-2172` |
| per-list sort/tree-link/seal | `flist.c:2462,2464,2465` | was `:2192` |
| depth-first cursor advance + NDX_FLIST_EOF | `flist.c:2473-2491` | was `:2207` |
| `send_file_list` | `flist.c:2499` | was `:2227` |
| hardlink init before walk | `flist.c:2532-2535` | was `:2262` |
| INC_RECURSE setup (dir_flist, FLAG_DIVERT_DIRS) | `flist.c:2537-2544` | was `:2267-2272` |
| tail: sort, totals, inline ids | `flist.c:2811-2821` | was `:2545-2549` |
| tail: root tree / seal / EOF / 1-file probe | `flist.c:2842-2858` | was `:2571-2585` |
| `recv_file_list` (receiver dir_flist) | `flist.c:2929,2934` | |
| `flist_new` (pool alias + `ndx_start = prev + used + 1`) | `flist.c:3244-3279` (gap `:3268`) | was `:2960-2977` (`:2966`) |
| `flist_free` (`pool_free_old` on the boundary) | `flist.c:3282-3314` (`:3308`) | was `:2980-3012` (`:3006`) |
| `flist_sort_and_clean` (sorts `sorted[]`; low/high; FLAG_DUPLICATE mark) | `flist.c:3318` (`:3331`, `:3325-3328`, `:3374-3378`) | |
| sender loop lookahead calls | `sender.c:515,549` (+ enable `:516,522`) | was `:231,265` |
| NDX_DONE: `flist_free(first_flist)` + echo | `sender.c:524-546` (`:530-532`) | was `:240-258` |
| gap-NDX parent resolution + cleared-slot refusal | `sender.c:551-560` | was `:266-272` |
| `successful_send` via `flist_for_ndx` | `sender.c:408-411` | |
| `flist_for_ndx` | `rsync.c:951-984` | was `:787-821` |
| `read_ndx_and_attrs` flist lookup | `rsync.c:394` | |
| `perform_io` idle production (MAX gate; `-1` call) | `io.c:836-844`, `:853-856` | was `:753-758,771-775` |
| generator half-window (flush hint; hardlink pre-wait) | `generator.c:2703`, `:2775` | was `:2231,2302` |
| `set_allow_inc_recurse` | `compat.c:162-181` (clauses `:172-180`) | unchanged |
| CF_INC_RECURSE fold / mismatch error | `compat.c:724`, `:780` | was `:713`, `:746` |
| `change_local_filter_dir` | `exclude.c:974` | |
| pool primitives | `lib/pool_alloc.c:300` (`pool_free_old`), `:353` (`pool_boundary`) | |
| `fsort` stability contract | `flist.c:1944-1958` (comment `:1944-1947`) | identical names keep original order so flist sort matches dir_flist sort |
| `recv_file_list` dir_ndx guards | `flist.c:2895-2927` | refuses post-final sub-flist, out-of-range, cleared slot, duplicate flist |
| receiver hard dirname invariant | `flist.c:2976-2991` (message `:2984-2986`, exit `:2987`) | "ABORTING due to invalid path from sender", RERR_UNSUPPORTED |
| receiver dir_flist append / range sort / clean | `flist.c:2996-2998` / `:3041-3050` / `:3065` | |
| `F_DIR_DEFER` | ABSENT in 3.5.0 | zero occurrences; older-source name, do not cite |

## 11. oc divergence column (verified on master at this revision)

Per mechanism: what oc has TODAY, with its own file:line (each read for this
revision), and a verdict. PRESENT-FAITHFUL = same rule, same observable
semantics; PRESENT-DIVERGENT = the mechanism exists but its shape or timing
differs; ABSENT = nothing implements it. The upstream anchors are Section 2's.

| # | Mechanism | oc today | Verdict |
|---|-----------|----------|---------|
| 1a | dir_flist lifecycle, SENDER (FLIST_TEMP list of retained dir entries) | `DirectoryTree` (`crates/protocol/src/flist/dir_tree.rs:75`) holds node ids and names, built during the partition (`crates/transfer/src/generator/file_list/inc_recurse.rs:125-130`); the entries themselves stay in the flat transfer flist - no separate retained dir list, no FLIST_TEMP-style lifetime split | PRESENT-DIVERGENT (tree yes, owning dir list no - LF-1a) |
| 1b | dir_flist lifecycle, RECEIVER (own numbering; slot survives a cleared entry) | `DirFlist` with `DirSlot::{Active,Cleared}` (`crates/transfer/src/receiver/file_list/dir_flist.rs:14-60`), grown in the read loop; two-phase `record_pre_clean`/`resolve_survivors` reproduces the slot-retention rule | PRESENT-FAITHFUL |
| 2 | Node encoding: 3 int32 slots, `DIR_PARENT`/`DIR_FIRST_CHILD`/`DIR_NEXT_SIBLING` = node[0]/[1]/[2], -1 sentinels | `DirNode { first_child, next_sibling, parent }` as `Option<usize>` (`dir_tree.rs:51-61`) under a virtual root (`:89-95`, `ROOT = 0` `:106`); oc additionally REFUSES an out-of-range parent index (`:26-42`) where upstream trusts its own construction | PRESENT-DIVERGENT (semantics equivalent; memory-only on both sides, so no wire impact; the int32/-1 layout is deliberately not reproduced) |
| 3 | `add_dirs_to_tree()`: link a sorted sibling run under its parent, per sub-list and for the initial list | Links are made at CLASSIFICATION time via a parent-name map (`inc_recurse.rs:91-130`), not by post-sort sibling-run linking; walk order then comes from `DirectoryTree::next_directory` (`dir_tree.rs:193`) | PRESENT-DIVERGENT (LF-1c/1d must pin that the link rule equals upstream's sorted-run rule) |
| 4 | `send_extra_file_list(f, at_least)` + `send1extra()`: scan ONE dir on demand, one fresh `flist_new(0, ...)` per segment, at_least budget | ABSENT as a producer: oc pre-builds the whole list, then `SegmentScheduler::next_to_send` (`crates/transfer/src/generator/segments.rs:174`) admits PRE-BUILT range segments which `encode_and_send_segment` (`crates/transfer/src/generator/protocol_io.rs:856`) emits. The one-level resumable scan primitive exists (`crates/transfer/src/generator/file_list/walk.rs`, LF-2a) but is not wired as the producer; segments are ranges (`PendingSegment`), not owned lists | ABSENT (the LF-2b/2c core) |
| 5 | Descend/sibling walk picking the next dir; `flist->parent_ndx = send_dir_ndx` | Walk exists: `next_directory` descend/sibling/pop (`dir_tree.rs:193`); parent recorded as `PendingSegment.parent_dir_ndx`/`parent_flat_idx` (`inc_recurse.rs:283-286`) and the initial gap via `NdxMap::set_initial_parent_flat` (`inc_recurse.rs:300`) | PRESENT-DIVERGENT (runs at build time to ORDER pre-built segments, not at send time to drive scanning) |
| 6 | `ndx_start` arithmetic; the +1 gap IS the parent dir's own NDX | `NdxMap` (`crates/transfer/src/generator/ndx_map.rs:80`), gap applied at `:145` (`prev.ndx_start + prev_used + 1`); gap-NDX resolves to the parent for itemize via `NdxMap::resolve_itemize`; sender-side gap resolution mirrors `sender.c:551-557` | PRESENT-FAITHFUL |
| 7 | Receiver hard dirname invariant (`flist.c:2976-2991`) and what fires it | `crates/transfer/src/receiver/file_list/receive.rs:548-575` - same predicate, byte-identical "ABORTING due to invalid path from sender" text; exercised by tests in `receiver/file_list/on_demand.rs:833-855`. Note: the in-code citation reads `flist.c:2684-2695`, which is a DRIFTED pre-3.5.0 line range (3.5.0: `:2976-2991`) - flagged for the next citation sweep, not edited in this docs change | PRESENT-FAITHFUL (one stale citation) |
| 8 | Sort parity: stable sort so flist order matches dir_flist order (`flist.c:1944-1947`, alias comment `:1972-1974`) | `sort_file_list` sorts a key array over stable `sort_by` by default, `sort_unstable_by` only under `--qsort` (`crates/protocol/src/flist/sort.rs:209-245`), same comparator both sides (`compare_file_entries`). Cross-list parity holds today BY CONSTRUCTION (one global sort feeds both); under the lazy producer it becomes LF-1d's explicit obligation | PRESENT-FAITHFUL (parity mechanism differs; the guarantee holds) |
| 9a | MIN lookahead floor (`sender.c:515,549`; `generator.c:2703,2775`) | `MIN_FILECNT_LOOKAHEAD` + backlog accounting in `SegmentScheduler::next_to_send`/`retire_current_flist` (`segments.rs:28,174,233`) | PRESENT-FAITHFUL (sender side) |
| 9b | MAX ceiling (`io.c:836-844`) | `next_when_idle` (`segments.rs:202-203`) - reachable but DEAD under the eager producer (task 400: next_to_send's >=1000 subsumes >=10000) | PRESENT-INERT until LF lands |
| 9c | Idle top-up `send_extra_file_list(sock_f_out, -1)` (`io.c:853-856`) | No analogue: nothing produces lists while blocked on input | ABSENT (LF-7b/7c territory) |
| 9d | Receiver half-window pacing (MIN/2, `generator.c:2703,2775`) | No analogue; moot while oc's receiver never enables INC_RECURSE (Section 9). The receiver DOES have on-demand segment receive (`crates/transfer/src/receiver/file_list/on_demand.rs`) - the marker-driven receive half, not the pacing | ABSENT (pacing), PRESENT (on-demand receive) |
| - | dir_ndx guards on a received sub-list (`flist.c:2895-2927`) | The receiver `DirFlist` resolves a wire `dir_ndx` to `DirSlot::{Active,Cleared}` (enum at `dir_flist.rs:14-20`) and refuses cleared or out-of-range slots, matching upstream's refusals | PRESENT-FAITHFUL |
