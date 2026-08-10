# DIS-4.d: flist build cold-start time

Tracks DIS-4.d: scope the sender-side file-list build cost on cold
start as a contributor to the daemon initial-sync gap surfaced by
DIS-1 (PR #4813). DIS-3 (PR #4849) decomposed the full cold-start
into phases and named rows 17, 18, 19 - filesystem walk + sort +
INC_RECURSE partition - as a single contributor in the 20-65 ms gap
band on the 500-file / 1 KiB-each `small_files` scenario. This audit
zooms in on those three rows.

Scope is **flist build only**: from `build_file_list()` entry to the
end of `partition_file_list_for_inc_recurse()`. It deliberately
excludes the first-block send (rows 20-22, covered by DIS-4.e) and
the rsyncd greeting / module-select / auth path (rows 1-16, covered
by DIS-4.a-c).

This is a docs-only audit. No `.rs` files are modified.

## 1. Per-entry allocation breakdown

The sender constructs one `protocol::flist::FileEntry` per filesystem
entry via `GeneratorContext::create_entry`
(`crates/transfer/src/generator/file_list/entry.rs:31-248`). Each
construction goes through `FileEntry::new_with_type`
(`crates/protocol/src/flist/entry/constructors.rs:19-46`) which in
turn calls `extract_dirname()` (`core.rs:78-83`) to derive the
parent path.

Inputs to `create_entry` are the already-allocated `full_path:
&Path` and a fresh `relative_path: PathBuf` that the caller built
via `path.strip_prefix(base).to_path_buf()` in
`walk_path_with_metadata`
(`walk.rs:67`). The relative `PathBuf` is one heap allocation that
is paid before `create_entry` is even entered.

### Allocation table (per entry, vanilla `-a` transfer)

| Field | Type | Heap ops | Upstream equivalent | Diff |
|---|---|---:|---|---:|
| `relative_path` arg (caller-built via `strip_prefix(...).to_path_buf()`) | `PathBuf` | 1 | re-use `lastdir`-relative basename pointer into the readdir buffer | +1 |
| `name` (move of `relative_path` into `FileEntry`) | `PathBuf` | 0 (move) | basename copy into pool tail (1 memcpy, no malloc) | +0 |
| `dirname` via `extract_dirname()` | `Arc<Path>` | 1 (`ArcInner` + bytes, **not** interned at sender) | `dirname` set from `lastdir` cache - 0 allocations on the hit path | +1 |
| `extras` for symlinks (`new_symlink` → `Some(Box<FileEntryExtras>)`) | `Box<FileEntryExtras>` | 1 conditional | `linkname` appended to pool extent | +1 conditional |
| `user_name` (`set_user_name` from `lookup_user_name`) under `-o` minus `--numeric-ids` | `String` inside `FileEntryExtras` | 1 conditional + 1 for `Box<FileEntryExtras>` if not yet allocated | `idlist` cache returns `const char *` shared across entries | +1 to +2 conditional |
| `group_name` (`set_group_name`) under `-g` minus `--numeric-ids` | `String` inside `FileEntryExtras` | 1 conditional | `idlist` cache shared pointer | +1 conditional |
| `xattr_list` under `-X` | `XattrList` (vec of (key, value)) | 1 + N for each entry | `rsync_xal_l` linked-list reuse + per-name strdup | +1 to +k conditional |
| `file_list` growth | `Vec<FileEntry>` (amortised) | < 1 amortised | `flist->files = realloc(...)` on `flist_expand` (every `FLIST_EXTENT_BLOCKS`) | ~equal |
| `full_paths` parallel growth | `Vec<PathBuf>` | 1 per entry (each `PathBuf` is the canonical `path.clone()` from `push_file_item`) | upstream does not retain a per-entry full path; basename + dirname pointer reconstruct on demand | +1 |
| Walker-step `OsString` per readdir name + `PathBuf::join` per child (`walk.rs:255`, `process_dir_entries_batched`) | `OsString` + `PathBuf` | 2 per entry (transient, dropped after `walk_path_with_metadata`) | upstream uses `dirent->d_name` directly into the per-dir static buffer | +2 transient |
| Optional: `FileListEntry` (in `crates/flist/src/entry.rs:6-13`, when build is driven through `FileListBuilder`) | `FileListEntry { full_path, relative_path, .. }` | 2 transient | upstream has no traversal-step wrapper - the walker writes `file_struct` directly | +2 transient |

Counting the **steady-state vanilla case** (no `-X`, no name
lookups, no symlinks):

- oc-rsync: 1 (`relative_path`) + 1 (`dirname Arc`) + 1
  (`full_paths` push) + 2 transient (`OsString` + child `PathBuf`)
  = **5 heap ops per entry**, 3 of which survive in the file list.
- Upstream: 1 amortised pool bump per entry, dirname is a shared
  `lastdir` pointer (0 fresh alloc on run-of-same-dir), basename is
  a memcpy into the pool tail. **~1 amortised heap op per entry**.

Heap-op ratio: **~5x** vanilla, climbing to **~7x** when `-o -g`
without `--numeric-ids` adds two `String` user/group names per
entry, and **~9x** under `--hard-links` or `-X`. This matches the
"~5-7 allocations per entry" figure in DIS-3 row 17.

### Allocation table (per entry, `-a -X --hard-links`)

| Stage | oc-rsync heap ops | Upstream heap ops | Diff |
|---|---:|---:|---:|
| `relative_path` build | 1 | 0 | +1 |
| `dirname Arc` | 1 | 0 (lastdir hit) | +1 |
| `full_paths` push | 1 | 0 | +1 |
| `extras` box allocation | 1 (first time `set_uid`/`set_gid`/`set_hardlink_dev` is called on a `None` extras) | 0 | +1 |
| `user_name` String | 1 | 0 (idlist shared `const char *`) | +1 |
| `group_name` String | 1 | 0 (idlist shared `const char *`) | +1 |
| `xattr_list` head + N pairs | 1 + N | 1 (xal_l) + N (each `strdup`) | ~equal in pairs, +1 in head |
| Transient `OsString` + child `PathBuf` | 2 | 0 | +2 |
| **Total surviving** (excluding transients) | 7 + N | 1 + N | **+6** |
| **Total including transients** | 9 + N | 1 + N | **+8** |

## 2. Sort, INC_RECURSE, and post-sort cost

### 2.1 Sort phase

`build_file_list` (`mod.rs:88-108`) builds an `indices: Vec<usize>`
permutation, calls `sort_by(cmp)` or `sort_unstable_by(cmp)` with a
closure that re-borrows `file_list[a]` / `file_list[b]`, then
`apply_permutation_in_place` reorders both `file_list` and
`full_paths` via cycle-following swaps (`mod.rs:369-398`).

| Step | Heap ops | Cost on 500 entries |
|---|---:|---|
| `Vec<usize>` of length N | 1 | ~4 KiB allocation, single growth |
| `dest_perm: Vec<usize>` (inverse permutation) | 1 | ~4 KiB allocation |
| `sort_by` closure capture | 0 (closure is zero-sized after capture) | ~`O(N log N)` comparator calls; each compares two `FileEntry::name()` via `compare_file_entries` |
| `apply_permutation_in_place` swaps | 0 | ~N memcpy of `FileEntry` (88 B) and `PathBuf` (24 B) per cycle position |

Compared with upstream `flist.c:f_name_cmp` /
`qsort(flist->files, flist->count, sizeof(struct file_struct*),
file_compare)`, upstream sorts pointers into the pool. oc-rsync
sorts indices and then *moves* the entries (and parallel
`full_paths`). The move is in-place via cycle-following so there are
no clones, but it still pays the memcpy cost.

DIS-3 row 18 estimate: **1-3 ms** for the sort on 500 entries,
compared with upstream's **0.5 ms**. The closure-borrow shape and
the 88-byte entry payload account for the ~2-3x gap; permutation
size and quality of `sort_by` are not the bottleneck.

### 2.2 INC_RECURSE partition

`partition_file_list_for_inc_recurse`
(`crates/transfer/src/generator/file_list/inc_recurse.rs:38-53`)
runs only when `self.inc_recurse()` is true. For the cold-start
small_files scenario the directory tree is **flat** (one parent
holding 500 leaf files), so:

- `classify_file_list_entries` iterates the 500 entries once,
  computing `Path::new(name).parent().to_string_lossy().to_string()`
  per entry (1 transient `String` per entry, but the result is
  empty / "." for every leaf so the `HashMap` is never populated by
  child entries).
- `initial_entries: Vec<TaggedIndex>` and `segments:
  Vec<DirSegment>` both stay small (only `.` plus the source
  directory).
- `reorder_and_build_segments` wraps `file_list` and `full_paths`
  in `Vec<Option<...>>` (2 allocations) just to support `Option::take()`,
  rebuilds the two `Vec`s, then drops the `Option` wrappers. This is
  one extra full-length traversal even when classification produces
  almost no segments.

Heap ops for the 500-entry flat tree:

| Step | Heap ops |
|---:|---:|
| `Vec<Option<FileEntry>>` build | 1 (length-N) |
| `Vec<Option<PathBuf>>` build | 1 (length-N) |
| Replacement `Vec<FileEntry>::with_capacity(N)` | 1 |
| Replacement `Vec<PathBuf>::with_capacity(N)` | 1 |
| Per-entry `String` for parent name | N transients |
| `HashMap<String, ...>` resizes | log2(unique dirs) - typically 0-2 for flat trees |
| `node_to_wire`, `node_to_seg` dense `Vec<i32>` / `Vec<usize>` | 2 (one each, sized to num_dirs) |

For 500 leaf files in 1 directory the HashMap sees only 1 insertion
(the source dir itself plus `.`); the dominant cost is the per-entry
`String` allocation in the classification loop. Estimate:
**0.3-0.5 ms** for the partition. DIS-3 row 19 gives the same
order-of-magnitude (`0.5 ms`).

Note: even when `inc_recurse()` is *false* (the default for daemon
PUSH from oc-rsync, see `build_capability_string(!is_sender)` per
MEMORY notes), the partition step exits immediately on the
`if !self.inc_recurse()` guard. The 0.3-0.5 ms estimate above is
only paid when the receiver also negotiates `CF_INC_RECURSE`.

### 2.3 Post-sort hardlink scan

`assign_hardlink_indices`
(`hardlinks.rs:34-`) only runs under `--hard-links`. For the
cold-start scenario (`-a` includes `-H` only when explicitly
requested, not by default), this path is skipped.

## 3. Cold-start scenario decomposition (500-entry small_files)

DIS-1's harness writes 500 x 1 KiB regular files in a single
directory and times a cold-cache pull. Estimated per-phase
breakdown for the flist build phase, using the DIS-3 row 17 / 18 /
19 figures plus the per-entry alloc table above:

| Sub-phase | oc-rsync | Upstream | Gap | Notes |
|---|---:|---:|---:|---|
| `readdir` + 500 transient `OsString` builds | ~3 ms | ~2 ms | ~1 ms | `process_dir_entries_batched` collects all 500 names before stat dispatch |
| `batch_stat_dir_entries` (500 above default `Stat` threshold of 64) | ~20 ms | ~10 ms (sequential `lstat`) | ~10 ms | oc-rsync goes parallel via rayon's `map_blocking`; cold-cache `newfstatat` dominates regardless |
| 500 x `create_entry` + `FileEntry::new_file` | ~12 ms | ~3 ms | ~9 ms | 5 heap ops/entry × 500 × ~3 ns malloc = ~7.5 ms allocator + ~4-5 ms copy/setup |
| 500 x `push_file_item` (push entry + full path into `Vec`s) | ~1 ms | < 0.2 ms | ~0.8 ms | second `PathBuf` per entry has no upstream analogue |
| Sort 500 entries | ~2 ms | ~0.5 ms | ~1.5 ms | indirect-permutation closure plus 2 length-N `Vec<usize>` |
| INC_RECURSE partition (only if negotiated) | ~0.4 ms | ~0.4 ms | 0 | symmetric |
| Hardlink assign (skipped, no `-H`) | 0 | 0 | 0 | |
| `collect_id_mappings` | ~0.3 ms | ~0.3 ms | 0 | symmetric `Vec` build of unique uids/gids |
| **Total** | **~39 ms** | **~16 ms** | **~23 ms** | within DIS-3 row 17-19 band of 21-67 ms |

The dominant gaps are **stat parallelism overhead amortising
imperfectly on 500 entries** (~10 ms; the rayon dispatch is paid but
the cold-cache wall-clock saving is bounded by single-disk
seek-time) and **per-entry `FileEntry` construction** (~9 ms,
fully attributable to the +4 to +6 heap ops/entry over upstream).

Sort and INC_RECURSE together add **~1.5 ms** - below the noise
floor.

## 4. Cross-reference: RSS-7..12

The `RSS 3-11x upstream` finding
(`docs/audits/rss-pathbuf-arcpath-overhead.md`,
`docs/audits/flist-building-100k-files.md`) tracks the same
per-entry allocation cost as a steady-state memory metric. The
ranked reductions there map 1:1 to wins on cold-start latency:

| Reduction (per `rss-pathbuf-arcpath-overhead.md`) | Cold-start ms saved on 500 entries |
|---|---|
| **RSS-7** Replace `PathBuf` with `Box<Path>` for `name` | ~1-2 ms (one fewer `Vec` capacity field allocation per entry; allocator-class savings) |
| **RSS-8** Per-flist arena for basenames (single `Vec<u8>`) | ~4-6 ms (collapses `relative_path PathBuf` + `name PathBuf` into one bump per entry; eliminates per-entry malloc-class allocation) |
| **RSS-9** Pack `extras` into arena tail | ~0 for vanilla, ~2-3 ms under `-o -g` (collapses `Box<FileEntryExtras>` + `String user_name`/`group_name` into one bump) |
| **RSS-10** Basename interning for repeating names | ~0-0.5 ms on this scenario (500 unique names) |
| **RSS-11** Per-FileList bump allocator (`bumpalo` or hand-rolled) | ~5-8 ms combined-with-RSS-8 (lowers per-allocation metadata; O(1) free on drop) |
| **RSS-12** Sender-side `PathInterner` for dirnames (mirror upstream `lastdir`) | ~0.5-1 ms on flat trees, ~3-5 ms on deep trees (skips fresh `Arc<Path>` per entry) |

Combined estimate when **RSS-7 + RSS-8 + RSS-9 + RSS-12** land:
**~10-15 ms saved on the 500-entry build**, or roughly **4-7x
heap-op reduction** matching the per-entry ratio in section 1. Wall
clock on this scenario would shrink from ~39 ms to ~24-29 ms,
landing within ~10 ms of upstream's 16 ms.

The cold-start gap on rows 17-19 is therefore *fully addressable* by
the open RSS work. No additional flist-specific arena design is
needed.

## 5. Recommendation

Three options were considered:

- **(a)** Wait for the RSS arena migration (RSS-7..12) to land. No
  duplicated work; the cold-start gap on rows 17-19 collapses as a
  side effect of the steady-state RSS work.
- **(b)** Interim micro-optimizations in the flist builder:
  pre-allocate `Vec<Option<...>>` only when INC_RECURSE actually has
  segments; skip the indices `Vec<usize>` allocation by sorting in
  place (requires `FileEntry: Ord`); drop the per-entry `full_paths`
  retention when the wire encoder no longer needs it after a future
  refactor.
- **(c)** Parallelize the `create_entry` loop with rayon (entries
  already arrive in a `Vec<StatResult>` from `batch_stat_dir_entries`
  and could be mapped through `par_iter().map(|r|
  ctx.create_entry(...))`).

**Recommendation: (a) wait for RSS-7 + RSS-8 + RSS-12.**

Reasons:

1. The RSS work is already prioritised in the open project list and
   has a documented sequencing (RSS-7 first, then RSS-8/9, then
   RSS-11/12). Doubling up with interim flist-only micro-optimizations
   risks merge conflicts on `FileEntry` layout for ~1-2 ms of cold-
   start latency, well below the rsyncd accept-loop signal-poll fix
   (DIS-4.a) which alone clears 200-500 ms off the p99.
2. Option (c) - rayon-parallel `create_entry` - is not safe today.
   `create_entry` calls `self.fake_super_override` which takes
   `&self`, and the loop pushes into `self.file_list` /
   `self.full_paths` (`&mut self`), so a `par_iter` rewrite would
   need a builder pattern that collects entries into a thread-local
   `Vec` and merges them at the end. That builder is exactly the
   bump-arena design called for by RSS-8 / RSS-11. Building it now
   only to throw it away when the arena lands is wasted churn.
3. Option (b) interim wins are small (~1-2 ms total) and the
   `Vec<Option<...>>` allocation hot-spot is only paid when
   INC_RECURSE has segments, which is *off* on the DIS-1 push-from-
   oc-rsync measurement (see MEMORY: "Sender-side code exists in
   generator but interop not validated - disabled for push
   transfers").

If RSS-7 has not landed by the time DIS-6 picks up the next cold-
start sprint, the fallback is to do the *transient* allocation
cleanup only: skip the `Vec<Option<...>>` wrap in
`reorder_and_build_segments` when INC_RECURSE is inactive and drop
the per-entry parent `String` in `classify_file_list_entries` in
favour of a `Path::parent()` comparison. Those two changes save
~0.5 ms and unblock no other work.

## 6. What DIS-6 should re-measure under `perf`

The per-phase ms estimates in section 3 are derived from code reading
plus the DIS-3 row 17-19 band, not from instrumented measurement.
Before sequencing RSS-7..12 work specifically against the cold-start
metric, DIS-6 should produce a flame graph that confirms:

- Per-entry `malloc` count comes out to ~5-9 calls (matching
  section 1's table).
- `batch_stat_dir_entries`' rayon dispatch overhead is bounded
  below ~5 ms for the 500-entry case (the per-stat wall-clock floor
  on cold cache is ~30 us per `lstat`, so 500 sequential lstat ≈
  15 ms; the parallel path is justified only if it beats that
  number after rayon overhead).
- `apply_permutation_in_place` on 500 entries actually hits the
  cycle-following swap path rather than degenerating to N
  individual `swap()` calls (the algorithm assumes few long cycles;
  worst case is one swap per element).

The flame graph also distinguishes between "alloc fast path"
(stack-cached `tcache` bin hits) and "alloc slow path" (`mmap` fall-
back). The 5x heap-op ratio in section 1 hurts more on cold start
because the `tcache` is empty - this is one place where a steady-
state metric (RSS) and a cold-start metric (DIS-1 wall clock) diverge,
and where the bump-arena fix wins disproportionately on cold start.

## 7. File index

Direct evidence files cited (all paths relative to worktree root):

- `crates/transfer/src/generator/file_list/mod.rs`
- `crates/transfer/src/generator/file_list/entry.rs`
- `crates/transfer/src/generator/file_list/walk.rs`
- `crates/transfer/src/generator/file_list/batch_stat.rs`
- `crates/transfer/src/generator/file_list/inc_recurse.rs`
- `crates/transfer/src/generator/file_list/hardlinks.rs`
- `crates/protocol/src/flist/entry/core.rs`
- `crates/protocol/src/flist/entry/constructors.rs`
- `crates/protocol/src/flist/entry/extras.rs`
- `crates/flist/src/entry.rs`
- `crates/transfer/src/parallel_io.rs`
- `docs/audits/dis-3-cold-start-phase-decomposition.md` (parent task)
- `docs/audits/rss-pathbuf-arcpath-overhead.md` (cross-referenced RSS work)
- `docs/audits/flist-building-100k-files.md` (steady-state 100K analogue)
- `target/interop/upstream-src/rsync-3.4.1/flist.c` (upstream
  `make_file`, `send_file_list`, `lastdir` cache)
- `target/interop/upstream-src/rsync-3.4.1/rsync.h` (upstream
  `file_struct`, `FILE_STRUCT_LEN`, `NORMAL_EXTENT` pool constant)
