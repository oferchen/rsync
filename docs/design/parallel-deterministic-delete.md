# Parallel-flist deterministic delete (#2251)

Status: Design (task #2251; supersedes #1940 + `docs/design/delete-during-strict-order-gate.md`)
Audience: receiver, generator, engine, cli maintainers
Scope: replace today's batched `--delete-during` sweep and PR #4245's
opt-in `--delete-strict-order` gate with a single two-phase model that
reproduces upstream rsync 3.4.1's per-directory delete order
byte-for-byte while preserving internal parallelism. No new user-visible
flags; no fallback to the old batched sweep.

## 1. Problem statement

### 1.1 Today's divergence

oc-rsync's receiver runs a single deletion sweep before the transfer
pipeline (`crates/transfer/src/receiver/transfer.rs:540-544`):

```rust
if self.config.flags.delete {
    let (ds, exceeded) = self.delete_extraneous_files(&setup.dest_dir, writer)?;
    ...
}
```

Inside `delete_extraneous_files`
(`crates/transfer/src/receiver/directory/deletion.rs:40-186`):

- All `--delete-*` modes share one code path that batches every
  directory's extras into a single `HashMap<PathBuf, HashSet<OsString>>`
  before any unlink is issued.
- Above the deletion threshold the per-directory scans run on
  `parallel_io::map_blocking` workers; below it they run sequentially in
  whatever order `HashMap::keys()` yields. Either way, the `*deleting`
  itemize order, the wall-clock unlink order, and the interleave with
  transfers all diverge from upstream.
- The same code services `--delete-before`, `--delete-during`,
  `--delete-after`, and `--delete-delay`. Timing modes degrade into a
  single batched sweep; only the placement of the call changes.

Upstream, in contrast, drives deletion per directory inside the
generator loop (`target/interop/upstream-src/rsync-3.4.1/generator.c`):

- `do_delete_pass()` (lines 351-387) walks `cur_flist->sorted` in order
  and calls `delete_in_dir(fbuf, file, st.st_dev)` for each
  `FLAG_CONTENT_DIR` entry.
- The main `generate_files()` loop (lines 2282-2354) interleaves
  `delete_in_dir()` for the just-entered directory with
  `recv_generator()` for every child before descending. With
  `INC_RECURSE`, the same interleave happens segment-by-segment as each
  sub-list arrives (lines 2290-2310).
- Inside `delete_in_dir()` (lines 272-347) the destination directory is
  iterated in reverse (`for (i = dirlist->used; i--; )`), each entry is
  matched against the sender's flist via `flist_find_ignore_dirness`,
  and unmatched entries are unlinked via `delete_item(delbuf, mode,
  flags)`. With `delete_during == 2` (`--delete-delay`) the entries are
  appended to `deldelay_buf` instead, then replayed by
  `do_delayed_deletions()` at the very end (lines 252-265, 2408-2409).

### 1.2 Why PR #4245 was the wrong shape

PR #4245 (#1940) added `--delete-strict-order` as an opt-in flag. Audit
of the implementation
(`crates/engine/src/local_copy/executor/directory/recursive/mod.rs:188-200`,
plus 94 references across `cli`, `core`, `engine`):

- The flag is **opt-in**. Default behaviour stays divergent. Every user
  who wants upstream parity has to know to set it; the man-page,
  CHANGELOG, and interop docs all become two-mode.
- It forces sequential dispatch (`bypass parallel_io::map_blocking and
  the DEFAULT_DELETION_THRESHOLD cutoff`,
  `docs/design/delete-during-strict-order-gate.md:73-75`). Parallelism
  is sacrificed for ordering, instead of preserving both.
- It only covers `--delete-during`. `--delete-before`, `--delete-after`,
  and `--delete-delay` still ride the batched path.
- The flag exists only inside the local-copy executor. The receiver
  network path (`receiver/transfer.rs:540`) ignores it; remote pulls
  still diverge.
- Two diverging code paths is two diverging maintenance burdens; the
  golden tests bifurcate per mode.

### 1.3 Mandate

- Upstream-identical observable behaviour for every `--delete-*` mode,
  by default, with no opt-in.
- Parallelism preserved internally wherever it cannot change observable
  behaviour.
- Wall-clock delete event sequence (unlink syscall order, `*deleting`
  itemize order, `MSG_INFO` framing order) matches upstream
  byte-for-byte.

## 2. Two-phase model

```
                  +-----------------+      +------------------+
   flist segment  | compute_extras  |---->-| DeletePlan(D)    |
   arrives (#N)   | (rayon worker)  |      +------------------+
                  +-----------------+               |
                                                    v
                  +-----------------+      +------------------+
   flist segment  | compute_extras  |---->-| DeletePlan(D')   |
   arrives (#N+1) | (rayon worker)  |      +------------------+
                  +-----------------+               |
                                                    v
                  +---------------------------------------+
                  | DeletePlanMap (keyed by dir relpath)  |
                  +---------------------------------------+
                                    |
                                    v
                  +---------------------------------------+
                  | DirTraversalCursor (upstream order)   |
                  +---------------------------------------+
                                    |
                                    v
                  +---------------------------------------+
                  | single emitter thread:                |
                  | for each dir in upstream order        |
                  |   await DeletePlan(D)                 |
                  |   for each entry in plan order        |
                  |     unlink, itemize, stat++           |
                  +---------------------------------------+
```

### 2.1 Phase 1 - parallel `compute_extras`

Attach to the existing per-segment receiver hook in
`crates/transfer/src/receiver/file_list.rs:130-233`
(`receive_extra_file_lists`). For each arriving segment `S` describing
content directory `D`:

1. Determine the segment's content directories. For each, look up the
   destination directory's `read_dir` snapshot (filename, file_type,
   normalized for macOS NFC parity as today).
2. `extras(D) = readdir(D) - segment_entries(D)`, intersected with
   `FilterChain::allows_deletion()` for the snapshot of the chain that
   is in effect for that directory (including any `.rsync-filter` merge
   files loaded by `enter_directory` for that subtree).
3. Sort `extras(D)` with `compare_file_entries`
   (`crates/protocol/src/flist/sort.rs:60`, our existing port of
   upstream `f_name_cmp`,
   `target/interop/upstream-src/rsync-3.4.1/flist.c:3217-3343`). The
   sort uses `t_PATH` when `protocol_version >= 29`, matching
   `f_name_cmp` exactly. Upstream uses `qsort` (unstable); we use
   `sort_unstable_by` to match.
4. Reverse the sorted slice. Upstream iterates `for (i = dirlist->used;
   i--; )` (`generator.c:320`), so plan-order is the reverse of
   ascending `f_name_cmp`.
5. Wrap the result in a `DeletePlan` and publish it into
   `DeletePlanMap` keyed by the directory's relative path.

This work parallelises naturally on the existing rayon segment-dispatch
pool. `compute_extras` is pure (read-only filesystem stat + immutable
flist + immutable filter chain), so workers do not coordinate. The same
`PARALLEL_STAT_THRESHOLD = 64` knob used elsewhere in the receiver
gates whether per-directory scans inside a segment fan out further.

### 2.2 Phase 2 - single emitter

A single drain task owns the unlink, itemize, and stats sequence. It
walks directories in upstream traversal order via `DirTraversalCursor`
(section 4); for each directory `D` it blocks until `DeletePlanMap[D]`
is ready, then iterates the plan and:

- For each entry, evaluates `--max-delete` and `--filter` rules in
  upstream order, calls `unlink`/`rmdir`/recursive removal, and emits
  the `*deleting` itemize line via `writer.send_msg_info`.
- Updates `DeleteStats` counters (files, dirs, symlinks, devices,
  specials) and `io_error` flags exactly where upstream sets them.

Because every observable side effect happens on a single thread in
upstream order, the wall-clock event sequence is bit-identical.

### 2.3 Invariants the design locks down

The implementation tasks (#2252-#2285) MUST preserve these:

1. **Single emitter for observable effects.** Only the drain task
   calls `unlink`, emits itemize, mutates `DeleteStats`, or updates
   `io_error`. Workers compute candidates and stop.
2. **Order = upstream `f_name_cmp` reversed, per directory.** Plan
   order inside a directory is `compare_file_entries` ascending,
   reversed - identical to `delete_in_dir`'s decrementing loop.
3. **Directory order = upstream depth-first traversal.** The cursor
   yields directories in the order `do_delete_pass` /
   `generate_files()` would visit them; see section 4.
4. **Filter chain snapshot per directory.** The
   `compute_extras` worker uses the chain in effect when that
   directory's `.rsync-filter` merges have been applied, mirroring
   `change_local_filter_dir(fbuf, dlen, F_DEPTH(file))`
   (`generator.c:301`). The drain task does not re-evaluate filters.
5. **Workers are pure.** No syscalls beyond `read_dir`/`stat`; no
   mutation of shared state beyond inserting into `DeletePlanMap`.
6. **Plan publication is monotonic.** A `DeletePlan` is published once
   per directory and never mutated. The drain task may wait on it but
   never races with a writer.
7. **`--delete-delay` defers, does not reorder.** Plans are still
   built in phase 1; the emitter writes them to a delay buffer and
   replays during finalisation, in the same per-directory order.
8. **No fallback.** The old `delete_extraneous_files` batched sweep
   is deleted, not gated. The `--delete-strict-order` /
   `--no-delete-strict-order` flags are removed from the CLI surface.

## 3. `f_name_cmp` semantics

Upstream `flist.c:3217-3343` defines a four-state automaton over
`(dirname, basename)`:

- `s_DIR` walks `dirname`; `s_SLASH` injects the separator;
  `s_BASE` walks `basename`; `s_TRAILING` injects a trailing `/` for
  `S_ISDIR` entries (so `foo/` sorts after `foo` for the same prefix).
- `type` is `t_PATH` for directories at protocol >= 29 and `t_ITEM`
  otherwise. `type1 != type2` short-circuits the comparison and ensures
  a directory and a non-directory with the same name never compare
  equal.
- Bare-`.` basenames degrade to `t_ITEM, s_TRAILING` so the
  self-reference sorts before children.

Our existing port lives in `crates/protocol/src/flist/sort.rs`:

- `compare_file_entries(a, b, protocol_pre29)` at line 60.
- `sort_file_list(file_list, use_qsort, protocol_pre29)` at line 206.
- Re-exported from `crates/protocol/src/flist/mod.rs:66`.

The design REUSES this port. Phase 1 builds a transient `FileEntry` per
destination-side candidate (using the same `S_ISDIR` bit upstream
checks), then sorts with `compare_file_entries`. The cited golden tests
at `crates/protocol/src/flist/sort.rs:472-809` cover the comparator's
edge cases (qsort vs stable, pre-29 vs >=29, dir-vs-file trailing
slash); the implementation task adds one more golden file pinning the
reverse iteration order matches `generator.c:320`.

## 4. Data structures

These types live in a new module `crates/engine/src/delete_plan/`
(siblings: `mod.rs`, `plan.rs`, `cursor.rs`, `emitter.rs`).

### 4.1 `DeletePlan`

```rust
/// Sorted, frozen list of destination entries to delete in one directory.
#[derive(Debug)]
pub struct DeletePlan {
    /// Relative directory path from the destination root.
    pub dir: PathBuf,
    /// Entries to delete, in upstream `delete_in_dir` emission order
    /// (i.e. `compare_file_entries` ascending, reversed).
    pub entries: Vec<PlannedDelete>,
    /// Filter snapshot used to compute this plan (for diagnostics; the
    /// snapshot was already applied before publication).
    pub filter_generation: u64,
}

#[derive(Debug)]
pub struct PlannedDelete {
    pub file_name: OsString,
    pub file_type: FileTypeFlags,
    pub flags: DeleteFlags, // DEL_RECURSE, DEL_NO_UID_WRITE, etc.
}
```

`DeleteFlags` is a bitset mirroring upstream `rsync.h:291-301`
(`DEL_NO_UID_WRITE`, `DEL_RECURSE`, `DEL_DIR_IS_EMPTY`, `DEL_FOR_FILE`,
`DEL_FOR_DIR`, `DEL_MAKE_ROOM`). The plan stores only the flags upstream
sets in `delete_in_dir` (`DEL_RECURSE`, optional `DEL_NO_UID_WRITE`);
`DEL_FOR_*` and `DEL_MAKE_ROOM` belong to the in-loop replacement path
and are not part of the sweep.

### 4.2 `DeletePlanMap`

```rust
/// Lock-free map from directory relpath to publish-once `DeletePlan`.
pub struct DeletePlanMap {
    inner: DashMap<PathBuf, Arc<OnceLock<DeletePlan>>>,
}

impl DeletePlanMap {
    /// Worker side. Reserves a slot and returns a publisher handle.
    pub fn reserve(&self, dir: PathBuf) -> PlanPublisher { ... }

    /// Drain side. Blocks until `dir`'s plan is published; returns
    /// `None` if the cursor has been told this directory will never
    /// receive a plan (e.g. it does not exist at the destination).
    pub fn wait(&self, dir: &Path) -> Option<Arc<DeletePlan>> { ... }
}
```

`DashMap` is already in the workspace (used in the buffer pool and the
hardlink table). The `OnceLock` inside each slot makes publication
single-writer / multi-reader and lock-free on the read path. An
explicit sentinel marks "no plan ever" so the drain task does not
deadlock when a directory is missing from the destination.

### 4.3 `DirTraversalCursor`

```rust
/// Yields directories in upstream traversal order for the drain task.
pub struct DirTraversalCursor {
    flist: Arc<SegmentedFileList>,
    next_segment_ix: usize,
    segment_dir_queue: VecDeque<PathBuf>,
}

impl DirTraversalCursor {
    /// Returns the next directory to drain, or `None` when all
    /// segments are exhausted.
    pub fn next_dir(&mut self) -> Option<PathBuf>;
}
```

Order rules (mirrors `generate_files()` and `do_delete_pass()`):

- Without `INC_RECURSE`, iterate `cur_flist->sorted` in ascending
  index order, yield every entry with `FLAG_CONTENT_DIR`
  (`generator.c:369, 381`).
- With `INC_RECURSE`, for each segment in arrival order, yield the
  segment's parent directory once (`generator.c:2290-2310`), then
  iterate the segment's entries in NDX order and yield their
  `FLAG_CONTENT_DIR` children (`generator.c:2312-2338`). Segments
  arrive sender-sorted by `flist_sort_and_clean`; arrival order IS
  upstream traversal order.
- For `--delete-before`, the cursor is exhausted before the transfer
  loop begins. For `--delete-during`, it advances in lockstep with the
  generator's directory dispatch. For `--delete-after` and
  `--delete-delay`, it is exhausted after the transfer loop.

## 5. Per-timing-mode wiring

All four modes use the same Phase 1 (parallel `compute_extras`) and the
same Phase 2 emitter. The mode only changes WHEN the drain task runs
and WHAT it does with each plan.

| Mode               | Phase 1 trigger        | Phase 2 trigger                        | Per-plan action                        | Upstream reference                       |
|--------------------|------------------------|----------------------------------------|----------------------------------------|------------------------------------------|
| `--delete-before`  | as each segment lands  | after EOF on flist, before first xfer  | unlink immediately                     | `generator.c:2263-2264` (`do_delete_pass`) |
| `--delete-during`  | as each segment lands  | interleaved per directory with xfers   | unlink immediately, before children    | `generator.c:1523, 2307`                 |
| `--delete-delay`   | as each segment lands  | after all xfers complete               | append to `deldelay_buf`, replay last  | `generator.c:265, 2408-2409`             |
| `--delete-after`   | as each segment lands  | after all xfers complete               | unlink immediately                     | `generator.c:2410-2411` (`do_delete_pass`) |
| `--delete-excluded`| as each segment lands  | (per-mode above)                       | extras include filter-excluded entries | `exclude.c:1330, 1571, 1648`             |

`--delete-excluded` is orthogonal to timing: it widens
`compute_extras` so filter-excluded entries are eligible for deletion
on the sender side (`exclude.c:1571`) and on the receiver side
(`exclude.c:1648`). Phase 1 honours `delete_excluded` when computing
the `extras` set; phase 2 is unchanged.

`--delete-missing-args` keeps its existing path
(`receiver/transfer.rs::create_directories` / args resolution); it
operates on top-level arguments, not on directory contents, and is
out of scope for `DeletePlanMap`.

## 6. Hardlink coordination

`engine::hardlink::HardlinkTracker` and
`protocol::flist::HardlinkTable` describe the **source** side - which
entries in the flist are followers of an earlier leader. They are not
consulted when deciding what to delete on the destination: upstream's
`delete_in_dir` uses `flist_find_ignore_dirness` (`generator.c:333`),
which looks the destination entry up by name in the sender's sorted
flist regardless of its inode story.

The design therefore does NOT cross-reference the hardlink table when
building `extras`. A destination-side file that happens to be a
hardlink target of a kept file but whose name is absent from the
segment is still deleted (matching `unlink` semantics: only the name
goes; the inode persists if there are other links). The single
exception is the `FLAG_MOUNT_DIR` check (`generator.c:324-329`), which
suppresses mount-point deletion and is preserved verbatim in the
emitter.

`--remove-source-files` runs on the sender after the receiver
acknowledges each file. It does not flow through `DeletePlanMap`; its
ordering is governed by sender-side ACK arrival and is already
upstream-identical.

## 7. Error policy

Upstream's `delete_in_dir` and `delete_item` continue on most errors
and abort only on:

- `io_error & IOERR_GENERAL && !ignore_errors` (`generator.c:291-298`):
  print "IO error encountered - skipping file deletion" once, then
  stop emitting deletes for the rest of this directory. The plan is
  consumed but no unlinks are issued.
- `--max-delete` exhaustion (`main.c:1367` /
  `generator.c:2413-2418`): print "Deletions stopped due to
  --max-delete limit (N skipped)", set `IOERR_DEL_LIMIT`, stop
  emitting.

The emitter mirrors both. Other errors (`ENOENT`, `EACCES`,
`ENOTEMPTY`, etc.) are logged via `debug_log!(Del, 1, ...)` and the
loop advances. This matches today's `delete_extraneous_files` error
handling
(`crates/transfer/src/receiver/directory/deletion.rs:178-182`) and
upstream's `delete.c:165-200` continue-on-failure stance.

`DeleteStats` is updated only on successful unlink, exactly where
upstream increments `stats.deleted_files` and friends. The varint
encoding in `crates/protocol/src/stats/delete.rs` is unchanged; the
`NDX_DEL_STATS` writer in the generator goodbye phase (protocol >= 31)
continues to consume the same struct.

## 8. Removal plan

The following are deleted as part of the implementation series, NOT
deprecated, NOT gated:

### 8.1 CLI surface

Delete from `crates/cli/src/frontend/`:

- `command_builder/sections/transfer_behavior_options.rs:301-318`
  (`delete-strict-order` / `no-delete-strict-order` arg definitions).
- `arguments/parser/mod.rs:159-160, 724`
  (`delete_strict_order` parse hook).
- `arguments/parsed_args/mod.rs:229-235`
  (`pub delete_strict_order: bool` field).
- `arguments/parser/tests.rs:710-737`
  (`delete_strict_order_*` tests; rewritten as upstream-order parity
  tests, see section 9).
- `frontend/help.rs:44-45` and `frontend/defaults.rs:11`
  (help text and defaults manifest entries).
- `frontend/execution/drive/config.rs:36, 187`
  and `frontend/execution/drive/workflow/run.rs:74, 705`
  (config plumbing).

### 8.2 Config and engine surface

Delete from `crates/core/src/client/config/`:

- `builder/mod.rs:142, 359`
  (`delete_strict_order: bool` field and propagation).
- `builder/deletion.rs:89-104`
  (builder setter with doc aliases).
- `client/mod.rs:65-66, 249`
  (`delete_strict_order: bool` field + default).
- `client/deletion.rs:71-151`
  (accessor + default-is-false test).
- `client/run/mod.rs:494`
  (`.delete_strict_order(config.delete_strict_order())` call).

Delete from `crates/engine/src/local_copy/`:

- `options/types.rs:100-109, 268`
  (field and default).
- `options/deletion.rs:74-101`
  (setter, reader, `delete_strict_order_enabled` helper).
- `options/builder/validation.rs`, `options/builder/definition.rs`,
  `options/builder/setters_deletion.rs`
  (all `delete_strict_order` references).
- `executor/directory/recursive/mod.rs:188-200`
  (the `strict_order_active` branch; the new emitter consumes plans
  unconditionally).
- `tests/delete.rs` strict-order cases (rewritten under section 9).

### 8.3 Batched sweep

Delete from `crates/transfer/src/receiver/`:

- `directory/deletion.rs:40-214` in full. The whole
  `delete_extraneous_files` function and its `parallel_io::map_blocking`
  fan-out are replaced by the `DeletePlanMap` drain.
- `transfer.rs:540-544` (the unconditional pre-pipeline call site).
- `receiver/tests.rs:2999, 3044`
  (`delete_extraneous_files`-only tests; replaced by emitter tests).

### 8.4 Documentation

- Replace `docs/design/delete-during-strict-order-gate.md` with a stub
  pointing at this document.
- Replace `docs/architecture/delete-during.md`'s "current behaviour"
  section with the two-phase model. Audit cross-references in
  `docs/architecture/`, `docs/design/`, and `docs/audits/` for
  mentions of `--delete-strict-order` and remove them.
- Man page (`docs/man/oc-rsync.1` and generated equivalents) loses
  `--delete-strict-order` entirely.

## 9. Test plan

### 9.1 Interop event-order parity (gates this entire change)

Add `tests/delete_order_interop.rs` driven by `tools/ci/run_interop.sh`:

- Build a fixture tree with at least 4 levels of nesting, mixed file
  types (regular, symlink, FIFO, device on Linux), per-directory
  `.rsync-filter` merges, and 5+ extraneous entries per directory with
  names chosen to exercise the `f_name_cmp` automaton (e.g. `a`, `a/`,
  `a.txt`, `a-`, names crossing case folding under macOS NFC).
- Run upstream 3.4.1 with `-vv --itemize-changes` and capture the
  `*deleting` lines in `expected.log`.
- Run oc-rsync with the same args; assert exact string equality of
  `*deleting` line sequence.
- Repeat the matrix for `--delete-before`, `--delete-during`,
  `--delete-after`, `--delete-delay`, and `--delete-excluded`.
- Wire up the `tcpdump` capture from
  `docs/design/project_delta_stats_wire_evidence.md` so the
  `NDX_DEL_STATS` frame is byte-compared as well.

### 9.2 Determinism property tests

`crates/engine/src/delete_plan/tests.rs`:

- Generate a randomised destination directory and segment list, run
  `compute_extras` 100 times with shuffled rayon worker counts (1, 2,
  4, 8, 16). Assert that for every `(seed, worker_count)` pair the
  emitter's unlink sequence is identical.
- Property test that `DeletePlan::entries` is exactly the reverse of
  the `compare_file_entries`-ascending sort of `extras(D)`.

### 9.3 Unit tests for the comparator and cursor

- `f_name_cmp` parity: pre-existing tests in
  `crates/protocol/src/flist/sort.rs:472-809` already cover the
  comparator. Add a single golden that asserts the reverse iteration
  order matches a captured `delete_in_dir` trace from upstream
  3.4.1 built with `--debug=DEL,2`.
- `DirTraversalCursor` order: golden tests for a fixture with and
  without `INC_RECURSE`, comparing emitted directory sequence against
  upstream `--debug=DEL,1` output.

### 9.4 Filter snapshot tests

- Per-directory `.rsync-filter` with a `protect` rule must suppress
  the corresponding extra. Today's batched sweep does NOT see late
  `.rsync-filter` files (the audit in
  `docs/architecture/delete-during.md` calls this out); the new design
  MUST.

### 9.5 Hardlink and `--max-delete` tests

- `--max-delete=N` stop point: assert the Nth deletion (in plan order,
  not worker-arrival order) is the last `*deleting` line, matching
  upstream.
- Hardlink target deletion: assert that deleting a hardlink follower
  by name does not abort the sibling leader's transfer, matching
  upstream `unlink` semantics.

### 9.6 Performance benches

`benches/delete_throughput.rs` (new):

- Compare today's batched sweep vs the two-phase model at 1k, 10k,
  100k, and 1M extras spread across 100, 1k, and 10k directories.
- Track wall-clock and `getrusage` page faults. Acceptance: within 5%
  of today's parallel sweep on the 100k / 1k-dirs midpoint, faster
  than the sequential strict-order path at every size.

## 10. Implementation order (tasks #2252-#2285)

The work is sequenced so each step is independently shippable behind
the existing pipeline (no half-states observable to the user). Each
step has its own task tag inside the #2252-#2285 range.

1. **Step 1 - data structures (#2252-#2257).**
   Land `crates/engine/src/delete_plan/` with `DeletePlan`,
   `PlannedDelete`, `DeleteFlags`, `DeletePlanMap`,
   `DirTraversalCursor`. No callers yet; pure unit tests pinning the
   `f_name_cmp` reverse-iteration order and cursor traversal.

2. **Step 2 - emitter and stats wiring (#2258-#2264).**
   Land `delete_plan::emitter` with the single-threaded drain loop,
   wired through `MsgInfoSender`, `DeleteStats`, `--max-delete`, and
   the upstream error policy. Behind a fresh `cfg(test)` switch so
   nothing in the production receiver calls it yet.

3. **Step 3 - parallel `compute_extras` (#2265-#2271).**
   Hook into `receive_extra_file_lists`
   (`receiver/file_list.rs:130`). Build plans and publish to
   `DeletePlanMap`. No emitter consumption yet; assert plan content
   in unit tests.

4. **Step 4 - cut the receiver over (#2272-#2278).**
   Replace `receiver/transfer.rs:540-544` and
   `engine/local_copy/executor/directory/recursive/mod.rs:188-200`
   with the new emitter. Delete `delete_extraneous_files` and the
   strict-order field cluster from `cli` / `core` / `engine`.
   `--delete-strict-order` becomes an unknown-arg error.

5. **Step 5 - interop, perf, docs (#2279-#2285).**
   Land the interop matrix from section 9.1, the perf bench from
   section 9.6, and the doc rewrites from section 8.4. Close out the
   audit references in `docs/architecture/delete-during.md` and
   `docs/design/delete-during-strict-order-gate.md`.

## 11. Cross-references

- Supersedes: #1940, `docs/design/delete-during-strict-order-gate.md`.
- Audit: #1893, `docs/architecture/delete-during.md`.
- Upstream source of truth:
  `target/interop/upstream-src/rsync-3.4.1/flist.c:3217-3343`
  (`f_name_cmp`),
  `target/interop/upstream-src/rsync-3.4.1/generator.c:272-387`
  (`delete_in_dir`, `do_delete_pass`),
  `target/interop/upstream-src/rsync-3.4.1/generator.c:2263-2411`
  (timing-mode dispatch),
  `target/interop/upstream-src/rsync-3.4.1/delete.c`
  (`delete_item`, `delete_dir_contents`),
  `target/interop/upstream-src/rsync-3.4.1/rsync.h:291-301`
  (`DEL_*` flag bitset).
- Existing port we reuse:
  `crates/protocol/src/flist/sort.rs`
  (`compare_file_entries`, `sort_file_list`).
- Implementation tasks: #2252 - #2285 (this design).
