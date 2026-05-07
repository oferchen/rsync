# Incremental flist memory benchmark plan (post-#1862 sender INC_RECURSE)

Task: #1864. Branch: `docs/incremental-flist-memory-1864`.

## Scope

Re-baseline peak resident-set memory of a 100K-file and 1M-file directory
push now that the sender-side INC_RECURSE state machine has shipped under
#1862 (`feat(transfer): enable INC_RECURSE sender by default (#3557)`).
The 2026-05-01 baseline (`docs/benchmarks/flist-memory-baseline-2026-05-01.md`)
landed before the sender state machine was wired in, so its Mode C row
("sender INC_RECURSE") was filled with `n/a`. The plan here defines what
to measure and how to gate it once #1862's runtime path has had a
performance audit, given that the v0.6.1 ship-default was reverted in
#3744 after a 95-201x push regression (#2088 follow-up). No code changes
are proposed in this document; a follow-up PR will land the harness
extensions and CI gate.

## TL;DR

- Sender INC_RECURSE state machine is implemented across
  `crates/transfer/src/generator/file_list/inc_recurse.rs` (241 LoC),
  `walk.rs` (372 LoC), and `mod.rs` (273 LoC) plus the
  `IncrementalState` / `SegmentScheduler` / `PendingSegment` triple in
  `crates/transfer/src/generator/mod.rs:154-351`. It is correctness-tested
  but not performance-tuned: `inc_recursive_send` was reverted to `false`
  by default in commit `b3a264061` because turning it on regressed push
  paths 95-201x at v0.6.1.
- Receiver-side INC_RECURSE has been on the default path since v0.5 and
  is the reason upstream-as-receiver hits 7.5 MB peak RSS at 1M files
  while oc-rsync-as-sender still buffers 218 MB.
- The memory cost asymmetry is structural: full flist holds every
  `FileEntry` (96 B inline + extras + interned `Arc<Path>`) up front;
  incremental holds only the current segment plus a `Vec<PendingSegment>`
  queue of `(parent_dir_ndx: i32, flist_start: usize, count: usize)`
  triples (24 B per directory).
- The proposed bench extends `scripts/benchmark_flist_memory.sh` with
  Mode C wired through `--inc-recursive-send`, adds two tree-shape
  variants (deep 10+ level nesting, flat 1-level), captures
  `VmRSS`/`VmHWM` plus jemalloc/mimalloc highwater on both ends of the
  push, and adds a CI regression gate keyed off the post-#1862 numbers.
- Until Mode C wall-clock parity is restored, the gate must run
  benchmark-only (no auto-flip of the `inc_recursive_send` default).
  Closing the #966/#971/#972 RSS gap requires Mode C to land with both
  RSS *and* wall-clock within 10% of upstream Mode B.

## 1. Current INC_RECURSE state machine

### 1.1 Sender side (post-#1862)

The sender pipeline lives entirely under
`crates/transfer/src/generator/`:

- `mod.rs:162` `MIN_FILECNT_LOOKAHEAD = 1000` mirrors upstream
  `flist.c:46`. The sender accumulates this many unsent entries before
  flushing the next sub-list, so peak working set per segment is bounded
  by `1000 * sizeof(FileEntry)` plus the interned dirname pool.
- `mod.rs:174-181` `PendingSegment { parent_dir_ndx: i32, flist_start:
  usize, count: usize }` is 24 bytes per directory and references file
  list entries by range, not by clone.
- `mod.rs:259-298` `SegmentScheduler` is a cursor-based dispatcher that
  yields the next segment when `remaining_in_current <
  MIN_FILECNT_LOOKAHEAD`, matching `sender.c:227,261` and
  `flist.c:2498-2510`.
- `mod.rs:308-338` `IncrementalState` holds the pending-segments vector,
  the EOF flag, the cached `FileListWriter` (preserves `prev_name`,
  `prev_mode`, `prev_uid`, `prev_gid` between sub-lists like upstream's
  static variables in `flist.c:send_file_entry()`), the
  `initial_segment_count` cap, and the `(flat_start, ndx_start)` segment
  boundary table.
- `file_list/inc_recurse.rs:38-53`
  `partition_file_list_for_inc_recurse()` runs once after the full walk:
  it classifies entries into top-level vs nested, builds a
  `DirectoryTree`, then reorders `file_list` and `full_paths` so the
  initial segment comes first and sub-segments follow in depth-first
  order. The reorder is move-not-clone (`Vec<Option<FileEntry>>` +
  `take()`), so the only memory peak during partition is one extra
  `Vec` of `Option<T>` slots (8 B per entry overhead for the discriminant
  and tag).
- `file_list/inc_recurse.rs:143-227` builds `Vec<PendingSegment>` with
  wire `dir_ndx` values that match the upstream receiver's `dir_flist`
  growth order: initial dirs first, then sub-list dirs in depth-first
  reception order. This guarantees the receiver's
  `flist.c:2652-2659` dirname validation never aborts.

The state machine is `Build -> Partition -> SendInitial ->
[Schedule -> EmitSegment]* -> EmitFlistEof -> EnterTransferLoop`, with
the schedule/emit cycle interleaved with the regular send loop.

### 1.2 Receiver side (default since v0.5)

The receiver path lives in
`crates/transfer/src/receiver/file_list.rs:130-233`
(`receive_extra_file_lists`):

- Loops on `read_ndx()` until `NDX_FLIST_EOF`. Each iteration reads a
  `dir_ndx = NDX_FLIST_OFFSET - ndx`, computes the segment's
  `seg_ndx_start = prev_ndx_start + prev_used + 1` (matches
  `flist.c:2931`), reads entries via the cached `FileListReader` (which
  preserves the same `prev_*` compression state as the sender's writer),
  appends them to `self.file_list`, runs sort and hardlink resolution
  on the *segment slice* only, and pushes a new
  `(flat_start, seg_ndx_start)` boundary onto `self.ndx_segments`.
- The cached reader (`flist_reader_cache`) is the receiver-side mirror
  of the sender's `flist_writer_cache`. Without it, every sub-list would
  start with empty compression state and the wire would diverge from
  upstream.
- Hardlink leader assignment runs per-segment: leader GNUMs are
  readdir-order wire NDX values assigned before the per-segment sort,
  matching `flist.c:1646`.

The receiver's per-segment cost is bounded by the segment size plus the
incremental `ndx_segments` table (16 B per segment). For a 1M-file tree
with `MIN_FILECNT_LOOKAHEAD = 1000`, the table caps out at ~16 KB.

### 1.3 Negotiation surface

`build_capability_string()` in `crates/transfer/src/setup.rs` advertises
the `'i'` capability bit on the SSH command line. Whether the bit is
sent is governed by `core_config.inc_recursive_send` for sender-direction
transfers and unconditionally on for receiver-direction transfers.
`inc_recursive_send` defaulted to `true` after #3557 landed, then was
reverted to `false` in #3744 (`fix(core): restore inc_recursive_send=false
default to fix v0.6.1 push regression`) because push paths over both
SSH and daemon went 95-201x slower in production. The CLI flag
`--inc-recursive-send` (added in commit `ab1cd8d0f`) lets users opt in
for interop validation runs.

## 2. Memory cost: full vs incremental

### 2.1 Full flist

`crates/transfer/src/generator/mod.rs:371-376` carries `file_list:
Vec<FileEntry>` and `full_paths: Vec<PathBuf>` in lockstep. With Mode A
or pre-#1862 sender behaviour, the entire walk is buffered before the
first byte goes to the wire. Per-entry cost:

- `FileEntry` inline 96 B (verified by `crates/protocol/benches/file_entry_memory.rs`,
  PR #1037).
- `PathBuf` 24 B inline + ~22 B for the typical
  `dir_NNN/file_NNNNN.dat` path content. The `Arc<Path>` dirname
  interning collapses repeated parent strings to one allocation per
  directory (`crates/protocol/src/flist/intern/`), saving ~13 B per
  entry at 1000-files-per-dir density.
- `FileEntryExtras` (`Box<...>`) is `None` for vanilla regular files so
  carries only the 8 B `Option` discriminant inline. Symlinks, devices,
  hardlinks, ACLs, xattrs, atimes, crtimes, and checksums each pull in
  the boxed allocation.

At the 100K-file 100-dir baseline (`flist-memory-baseline-2026-05-01.md`):
oc-rsync = 42.7 MB, upstream Mode A = 14.2 MB. At 1M files: oc-rsync =
218.2 MB, upstream Mode A = 76.8 MB. The 3-5x gap above upstream Mode A
is driven by `PathBuf`/`Arc<Path>` overhead and the `Vec<FileEntry>`
allocator (no upstream-style pool); both are tracked under #1048
(`docs/audits/pathbuf-arc-path-rss-overhead.md`), #1049 (interning),
and #1050 (`Vec` vs pool).

### 2.2 Incremental flist

Mode B (receiver-only INC_RECURSE) gives oc-rsync no benefit on a push
because the parent process is the sender; the only saving is on the
remote receiver. Mode C (sender INC_RECURSE) is where the structural
asymmetry kicks in:

- Steady state: one segment of up to `MIN_FILECNT_LOOKAHEAD = 1000`
  entries plus the queue of `PendingSegment` triples. For a 1M-file
  1000-dir tree, that's `1000 * 96 B + 1000 * 24 B = 120 KB`, ignoring
  path content. Even doubling for `PathBuf` content and `extras`, the
  inline working set stays under 500 KB.
- Once the partition step reorders `file_list`, the steady-state Vec
  shrinks as each segment is sent and the per-segment slice is dropped.
  Today's implementation does *not* truncate `file_list` after a
  segment ships; it keeps the entire vector live for hardlink
  cross-segment resolution and stats reporting. That is the next
  optimization opportunity (#1050) and the most likely source of the
  oc-rsync-vs-upstream Mode B gap that #966 still tracks.
- Upstream's allocator is a slab pool (`pool_alloc.c`), so even when it
  keeps every `file_struct` live the steady-state RSS stays bounded:
  7.9 MB at 100K, 7.5 MB at 1M. The `Vec<FileEntry>` we use today does
  not match that bound until #1050 lands.

## 3. Proposed bench

### 3.1 Workload matrix

| Scale | Tree shape | Dirs | Files/dir | Depth | Total |
|------:|------------|-----:|----------:|------:|------:|
| 100K  | shallow    |  100 |     1 000 |     2 | 100K  |
| 100K  | deep       |  100 |     1 000 |   10+ | 100K  |
| 100K  | flat       |    1 |   100 000 |     1 | 100K  |
| 1M    | shallow    | 1 000 |     1 000 |    2 | 1M    |
| 1M    | deep       | 1 000 |     1 000 |   10+ | 1M    |
| 1M    | flat       |    1 | 1 000 000 |    1 | 1M    |

The deep variant stresses the `DirectoryTree` depth-first traversal and
`partition_file_list_for_inc_recurse`'s segment ordering. The flat
variant collapses to a single segment, so Mode C should match Mode A
within noise; that case is the regression sentinel for any future
partition-overhead change.

### 3.2 Modes

- **Mode A** - full flist (`--no-inc-recursive`).
- **Mode B** - default (receiver INC_RECURSE; sender always full).
- **Mode C** - sender INC_RECURSE via `--inc-recursive-send`. New in
  this plan; gated on #1862 perf-tune work.

All three modes run on a local push (`oc-rsync src/ dst/`) inside the
`rsync-profile` container so no bind-mount paths are exercised.

### 3.3 Metrics

Captured per (scale, shape, mode) cell:

- `Peak RSS (MB)` - `/usr/bin/time -v` "Maximum resident set size".
  Already in the baseline harness.
- `Peak heap (MB)` - jemalloc `stats.allocated` highwater via
  `MALLOC_CONF=stats_print:true,prof:true` (Linux build). For mimalloc
  builds, `mi_stats_print_out`. Captures the difference between
  allocator-reported heap and `/usr/bin/time`'s OS-counted RSS.
- `Allocator highwater (MB)` - peak `arenas.bins.0.curslabs *
  arenas.bins.0.size` for the largest size class on jemalloc, which
  isolates `Vec<FileEntry>` reallocation churn from incidental I/O
  buffers.
- `Wall (s)` - already in the baseline. Mode C must stay within 10%
  of Mode B or the gate fails.
- `flist_buildtime` and `flist_xfertime` from the `--stats` output
  (protocol >= 29). These are visible via `TransferTiming` in
  `crates/transfer/src/generator/mod.rs:220-244`.

### 3.4 Harness extensions

- `scripts/benchmark_flist_memory.sh` gains a Mode C branch driven by
  `OC_RSYNC_INC_RECURSIVE_SEND=1` and the `--inc-recursive-send` flag.
- A new `--shape` flag selects shallow / deep / flat; the existing
  `generate_fixture()` is parameterised over depth and files-per-dir.
- TSV output adds `peak_heap_mb`, `allocator_highwater_mb`,
  `flist_buildtime_ms`, `flist_xfertime_ms` columns next to the
  existing `peak_rss_mb` and `wall_s`.
- Run count stays at N=3 for ad-hoc runs; CI gate variant uses N=5
  with median + IQR for stability under shared-runner noise.

### 3.5 CI regression gate

- Gate fires when oc-rsync Mode B `peak_rss_mb` regresses by more than
  10% from the prior baseline at either scale and either shape.
- Mode C is benchmark-only (no gate) until #1862 perf-tune work lands.
  Once Mode C wall is within 10% of Mode B and Mode C peak RSS is
  within 1.5x of upstream Mode B at 1M, gate flips to Mode C and the
  default of `inc_recursive_send` can be revisited.
- Gate runs on the existing `benchmark.yml` workflow on tag pushes
  only; PR runs publish numbers but do not block.

## 4. Cross-references

- **#966** (RSS gap, oc-rsync vs upstream): the headline 5.4x gap at
  100K and 29x gap at 1M is the open metric this benchmark continues
  to track. Closing it requires Mode C parity *and* `Vec<FileEntry>`
  pool-allocator work (#1050).
- **#971** (1M-file RSS scaling): the linear-vs-bounded contrast is
  visible only at 1M; the deep and flat shapes stress the same axis
  along orthogonal directions and so refine the regression model.
- **#972** (peak working set at intermediate scales): the 100K shallow
  cell is the existing pin; deep and flat shallow cells refine the
  curve between 100K and 1M without paying the 1M wall-clock cost on
  every CI tag.
- **#2088** push regression (v0.6.1): the 95-201x slowdown that
  prompted the #3744 revert is the headline reason Mode C cannot be
  default-on. The bench plan must capture wall-clock alongside RSS so
  the next default-flip attempt has wall-clock evidence as well as
  memory evidence. The regression itself was not a memory pathology;
  it was a syscall-rate pathology in the sender's per-segment dispatch
  loop. Mode C peak RSS is expected to be *lower* than Mode A even
  while wall-clock is worse; the bench gate must therefore be a
  two-axis (wall, RSS) gate, not RSS-only.

## 5. Out of scope

- Daemon transfers. The first cut measures push only because the
  partition + segment scheduler runs identically regardless of
  transport, and SSH/daemon wall-clock variance would mask the memory
  signal. Daemon and SSH variants land in a follow-up once the local
  push numbers are stable.
- Remote-receiver upstream variants. Upstream-as-receiver Mode C
  numbers are already captured in the 2026-05-01 baseline; they do
  not need re-running unless the upstream version pin changes.
- `FileEntryExtras` regressions. Symlink, hardlink, ACL, and xattr
  paths each pull in the boxed extras allocation; covering them needs
  a separate workload generator and a separate audit.
