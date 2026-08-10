# INC_RECURSE sender-side regression: profile plan

Task: #2088. Branch: `docs/inc-recurse-sender-investigation-2088`.

This document maps the disable mechanism, surveys the sender-side code,
cites the v0.6.1 regression evidence, and lists concrete instrumentation
points a developer can wire up immediately. No code changes; the next
step is a profile run on a million-file push.

## 1. Disable mechanism

### 1.1 The gate

The sender advertises the `'i'` capability bit only when the client
config carries `inc_recursive_send = true`. The default is `false`.

The capability builder is **single source of truth**:

- `crates/transfer/src/setup/capability.rs:138` -
  `pub fn build_capability_string(allow_inc_recurse: bool) -> String`.
- `crates/transfer/src/setup/capability.rs:144` -
  `if mapping.requires_inc_recurse && !allow_inc_recurse { continue; }`
  is what strips the `'i'` row from `CAPABILITY_MAPPINGS` when the gate
  is closed.
- `crates/transfer/src/setup/capability.rs:40-48` - the `'i' -> CF_INC_RECURSE`
  row with `requires_inc_recurse: true`.

Two call sites supply the gate:

- `crates/core/src/client/remote/invocation/builder.rs:184-186` -
  SSH remote invocation: `args.push(build_capability_string(self.config.inc_recursive_send()))`.
- `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs:155` -
  daemon transfer: `args.push(build_capability_string(config.inc_recursive_send()))`.

The default for `inc_recursive_send` is wired in
`crates/core/src/client/config/builder/mod.rs:383`:
`inc_recursive_send: self.inc_recursive_send.unwrap_or(false)`. The
getter is `crates/core/src/client/config/client/performance.rs:184`.

The doc comment in `performance.rs:165-186` still describes the default
as matching upstream's `allow_inc_recurse = 1` ("default `true`"), which
is stale relative to the actual `unwrap_or(false)`. That is a cosmetic
mismatch worth a follow-up cleanup but not a behavioural bug.

The CLI flag for opting in is `--inc-recursive-send` (added in commit
`ab1cd8d0f`). `--inc-recursive` / `--no-inc-recursive` also drive the
flag for backward compatibility with the upstream CLI surface.

### 1.2 Receiver-side flow (works correctly)

The receiver-side INC_RECURSE handler is what the gate intentionally
keeps unaffected. The client always advertises `'i'` when it is the
receiver because the gate only suppresses sender-direction advertisement:

- `crates/transfer/src/receiver/file_list.rs:130-233` -
  `receive_extra_file_lists()` reads one sub-list per loop iteration:
  reads `dir_ndx`, computes `seg_ndx_start = prev_ndx_start + prev_used + 1`
  (`flist.c:2931`), reads entries via cached `FileListReader`, sorts and
  resolves hardlinks per segment, appends `(flat_start, seg_ndx_start)`
  onto `ndx_segments`. Per-segment cost is bounded; the receiver never
  buffers the full flist.
- `crates/protocol/src/flist/incremental/mod.rs:75-160` -
  `IncrementalFileList`: dependency-tracking state machine that yields
  ready entries (parent dir already created) and queues the rest. The
  receiver streams entries to the filesystem as soon as their parent
  directories are ready.

The asymmetry stays visible on a million-file push: receiver-side memory
hits 7.5 MB on upstream, while oc-rsync as sender buffers 218 MB
(`docs/audits/incremental-flist-memory-bench.md`, section 2.1).

## 2. Sender-side INC_RECURSE survey

The sender state machine is **fully implemented**, not partial. It is
gated off because turning it on caused a wall-clock regression, not
because of missing functionality. There are no `TODO`, `FIXME`, `XXX`,
`todo!`, `unimplemented!`, or "not yet implemented" comments in:

- `crates/transfer/src/generator/file_list/inc_recurse.rs`
- `crates/transfer/src/generator/protocol_io.rs`
- `crates/transfer/src/generator/transfer.rs`
- `crates/transfer/src/generator/mod.rs`
- `crates/transfer/src/generator/file_list/walk.rs`
- `crates/protocol/src/flist/incremental/`
- `crates/protocol/src/flist/segment.rs`

(A single `grep -rn 'TODO|FIXME|XXX|disabled|not yet implemented'` over
these paths returns one unrelated hit in `itemize.rs:269` for a test
context comment.)

### 2.1 Major sender-side components

- **State machine entry**: `crates/transfer/src/generator/mod.rs:32-78` -
  module-level doc captures `Idle -> ScanDir -> SendChunk -> WaitAck ->
  NextDir -> Done`.
- **`MIN_FILECNT_LOOKAHEAD`**: `crates/transfer/src/generator/mod.rs:162` -
  `pub const MIN_FILECNT_LOOKAHEAD: usize = 1000` (mirrors upstream
  `flist.c:46`).
- **`PendingSegment`**: `crates/transfer/src/generator/mod.rs:174-181` -
  `{ parent_dir_ndx: i32, flist_start: usize, count: usize }` (24 B per
  directory).
- **`SegmentScheduler`**: `crates/transfer/src/generator/mod.rs:259-298` -
  cursor over the pending vec; `next_if_needed()` yields when
  `remaining_in_current < MIN_FILECNT_LOOKAHEAD` (mirrors
  `flist.c:2498-2510`).
- **`IncrementalState`**: `crates/transfer/src/generator/mod.rs:308-351` -
  holds pending segments, `flist_eof_sent` flag, cached `FileListWriter`
  (the writer cache is what preserves `prev_name`/`prev_mode`/
  `prev_uid`/`prev_gid` across sub-lists, mirroring upstream's static
  variables in `flist.c:send_file_entry()`), `initial_segment_count`,
  and the `(flat_start, ndx_start)` segment boundary table.
- **Partition**: `crates/transfer/src/generator/file_list/inc_recurse.rs:38-227` -
  `partition_file_list_for_inc_recurse()` runs *after* a full walk:
  classifies entries as top-level vs nested, builds a `DirectoryTree`,
  reorders `file_list` and `full_paths` (move-not-clone via
  `Vec<Option<T>>` + `take()`), assigns wire `dir_ndx` values that match
  the receiver's `dir_flist` growth order. The reorder cost is one extra
  `Vec` of `Option<T>` slots (8 B per entry overhead).
- **Initial segment send**: `crates/transfer/src/generator/protocol_io.rs:214-259` -
  `send_file_list()` sends only `initial_segment_count` entries when
  INC_RECURSE is active, then caches the writer.
- **Per-segment dispatch**: `crates/transfer/src/generator/protocol_io.rs:273-326` -
  `encode_and_send_segment()` writes `NDX_FLIST_OFFSET - parent_dir_ndx`,
  then each entry, then the end marker (zero byte). It does **not**
  flush.
- **NDX_FLIST_EOF**: `crates/transfer/src/generator/protocol_io.rs:370-386` -
  `send_flist_eof()` writes `NDX_FLIST_EOF` and flushes.
- **Send-loop scheduling**: `crates/transfer/src/generator/transfer.rs:108-129`
  (top-of-loop, mirrors `sender.c:227`) and
  `crates/transfer/src/generator/transfer.rs:404-417` (bottom-of-loop,
  mirrors `sender.c:261`). Both call `scheduler.next_if_needed(remaining)`
  and call `encode_and_send_segment()` for each yielded segment.
- **Drain on exit**: `crates/transfer/src/generator/transfer.rs:432-445` -
  if the read loop exits before all segments have been sent, the
  remaining segments are flushed before `NDX_FLIST_EOF`.

### 2.2 Sender does not stream the walk

A critical observation for any profile run: the partition step in
`crates/transfer/src/generator/transfer.rs:740-746` runs **after**
`build_file_list()`, which walks the entire filesystem, sorts the full
vector, and resolves hardlinks before partitioning. The "incremental"
sender today reorders an already-buffered Vec; it does not interleave
walk and send. This means:

- The memory peak on the sender stays at the full-flist size; sender
  INC_RECURSE saves wire-format bytes (no `dir_flist`-size duplication)
  and receiver-side memory, not sender-side memory.
- The regression is **not** memory-driven. The audit
  (`docs/audits/incremental-flist-memory-bench.md:267-275`) explicitly
  characterises it as "a syscall-rate pathology in the sender's
  per-segment dispatch loop". The bench plan calls for a two-axis
  (wall, RSS) gate on the next default-flip attempt for that reason.

The eventual streaming-walk redesign that would deliver the
"million-file memory spike" benefit is tracked separately under #1050
(`Vec<FileEntry>` pool allocator) and the broader "stream walk and send
in lockstep" rework, neither of which is in scope for re-enabling the
gate.

## 3. v0.6.1 regression evidence

### 3.1 Commit chain

1. **Pre-regression** - `854aa753a feat(transfer): enable INC_RECURSE
   sender by default (#1862)`. Sender default flipped from `false` to
   `true`. Removed the explicit `--inc-recursive-send` opt-in flag and
   wired the existing `--inc-recursive` / `--no-inc-recursive` tri-state
   through `ConfigInputs.inc_recursive_send: Option<bool>` so unset
   preserved the upstream default in `ClientConfigBuilder::build()`.
2. **Squash to master** - `39d47722b feat(transfer): enable INC_RECURSE
   sender by default (#1862) (#3557)`. This is the SHA on `master` that
   shipped the default-on behaviour. Merged 2026-05-02.
3. **Release** - `d51c95c6a chore: release v0.6.1`. Tagged shortly after
   #3557 landed.
4. **Regression report** - field reports of 95-201x slowdown on push
   over both SSH and daemon transports.
5. **Disable** - `bd12b6ac5 fix(core): default inc_recursive_send to
   false to fix push regression (#1862)`. Reverts the default to
   `false`. Authored 2026-05-06 (four days after #3557).
6. **Backport / re-merge** - `b3a264061 fix(core): restore
   inc_recursive_send=false default to fix v0.6.1 push regression
   (#3744)`. The PR-shaped commit that landed the disable on master.

The commit message on `bd12b6ac5` is the canonical statement of the
regression:

> PR #3557 (commit 39d47722b) flipped the sender-side INC_RECURSE
> default to true, advertising the `'i'` capability bit on push
> transfers. This caused severe performance regressions in v0.6.1 -
> push paths went 95-201x slower over both SSH and daemon transports.

It also documents the suspected cause:

> The sender-side INC_RECURSE state machine has not been validated
> against upstream rsync interop, and enabling it by default exercises
> a code path that is not yet performance-tuned.

### 3.2 Cross-reference

- `docs/audits/incremental-flist-memory-bench.md:267-275` corroborates:
  "The regression itself was not a memory pathology; it was a
  syscall-rate pathology in the sender's per-segment dispatch loop."
  No commit on master since #3744 has profiled the sender's per-segment
  dispatch or modified `encode_and_send_segment()`,
  `SegmentScheduler::next_if_needed`, or the cadence of the
  `writer.flush()` calls in `transfer.rs`.

### 3.3 Where the regression most likely lives (a priori hypothesis)

The disable commits do not localise the slowdown beyond "per-segment
dispatch loop". A 95-201x factor is too large for a per-entry overhead
issue and points at a per-segment system effect. The most likely
mechanisms, in order of priority for the profile run:

1. **Send-side buffer flush cadence**. The transfer loop calls
   `writer.flush()` at `transfer.rs:135` once per iteration of the
   NDX read loop. With INC_RECURSE on, the same iteration may have
   dispatched one or more sub-list segments before that flush. Per
   iteration we issue up to one segment plus the file ack, so a
   1000-file lookahead translates to one flush per ~1000 entries on
   the wire. The flush itself is upstream-faithful, but if the
   `ServerWriter` buffer fills mid-segment (the multiplex frame budget
   is 32 KB) we will fragment a single segment into many small writes.
   `encode_and_send_segment()` writes one `ndx`, N entries, and a
   one-byte terminator without an interior flush
   (`protocol_io.rs:273-326`); if any of those triggers an implicit
   flush at the buffered-writer boundary, we get O(entries) `sendto`
   calls rather than upstream's O(segments).
2. **NDX request stall**. Upstream interleaves sub-list dispatch with
   `read_ndx()` in a single multiplex stream; our reader is a separate
   `Read` trait object. The `writer.flush()` at line 135 is documented
   as a deadlock-avoidance measure; it is also the only point at which
   the receiver sees the new sub-list before being asked to respond.
   If the receiver sends back signatures referencing files in a
   not-yet-dispatched sub-list, we walk the whole `ndx_segments`
   partition_point on every wire NDX (see `wire_to_flat_ndx` at
   `crates/transfer/src/generator/mod.rs:459-467`). That table grows
   linearly with directory count.
3. **Per-segment writer cache state churn**. `flist_writer_cache` is
   `take()`-d at the top of `run_transfer_loop` (`transfer.rs:91-95`)
   and reinstalled at the end (`transfer.rs:448`). Each call to
   `encode_and_send_segment()` invokes `flist_writer.set_first_ndx`
   and the underlying `FileListWriter` runs its compression-state
   bookkeeping. If `prepare_pending_acl()` (called per entry from
   `protocol_io.rs:339-363`) takes the slow path for any non-symlink
   when `--acls` is on but no ACL exists, the syscall budget per
   segment balloons.
4. **DirectoryTree depth-first traversal cost**. The classification
   path in `inc_recurse.rs:61-132` builds a `DirectoryTree` and then
   `reorder_and_build_segments` traverses it with `tree.next_directory()`.
   At million-file scale with a 10+ deep tree, this is a one-shot cost
   but it still runs before the first byte of the initial segment goes
   on the wire. A 95-201x factor is unlikely to be caused here unless
   the tree implementation is super-linear in some shape.

## 4. Instrumentation points

These are the five concrete points a developer should wire up with
`tracing::trace!` (or `Instant::now()` checkpoints) before re-running
the push benchmark in `scripts/benchmark_flist_memory.sh` Mode C with
`--inc-recursive-send`.

### 4.1 First-byte latency

**Where**: `crates/transfer/src/generator/protocol_io.rs:214`
(`send_file_list` entry) and `crates/transfer/src/generator/protocol_io.rs:250`
(after the `write_end` + `flush` for the initial segment).

**What to measure**: Time from `send_file_list` entry to the post-flush
instant. This isolates "build-then-send-initial-segment" from the rest
of the transfer. Compare Mode A vs Mode C. Mode A measures one
monolithic send; Mode C should measure only the initial segment. The
difference should be negative or near-zero. A positive delta means the
partition step is more expensive than expected.

### 4.2 Per-segment dispatch cost

**Where**: `crates/transfer/src/generator/protocol_io.rs:273-326`
(`encode_and_send_segment`).

**What to measure**: Wrap the body in a `PhaseTimer` (the codebase
pattern, see `transfer.rs:738`) keyed by `segment.count`. Emit
`tracing::trace!` with `(segment.parent_dir_ndx, segment.count,
elapsed_us)` per call. On a 1M-file push that produces 1000 to 10000
trace lines, which a developer can aggregate with `awk` for
mean/p99/sum. A pathological per-entry overhead (e.g., per-entry
`write` syscall vs upstream's amortised buffer) will show as
`elapsed_us / segment.count >> 1`.

### 4.3 Writer flush rate

**Where**: `crates/transfer/src/generator/transfer.rs:135`
(`writer.flush()?` inside the main NDX loop) and any
`flist_writer.write_entry` call site that may flush implicitly.

**What to measure**: Count `writer.flush()` calls per second and
correlate with files sent per second. Upstream `sender.c` flushes once
per `MIN_FILECNT_LOOKAHEAD` segment plus once per phase boundary; a
flush-per-NDX rate suggests our top-of-loop flush is too aggressive
when INC_RECURSE is on. Compare the flush-to-files-sent ratio Mode A
vs Mode C. A flush ratio that climbs by 10x or more in Mode C is the
smoking gun.

Suggested approach: wrap `ServerWriter` to increment an `AtomicU64` per
flush, plus a `tracing::trace!` every 1000 flushes with the
`files_transferred` snapshot.

### 4.4 wire_to_flat_ndx hot path

**Where**: `crates/transfer/src/generator/mod.rs:459-467`
(`wire_to_flat_ndx`) and `crates/transfer/src/generator/mod.rs:469-486`
(`flat_to_wire_ndx`).

**What to measure**: Both functions run a `partition_point` over
`incremental.ndx_segments`. With INC_RECURSE on, this table grows to
~1000 entries for a 1000-directory tree. Each call site is on the hot
path of NDX request handling. Add a counter incremented per call and
log it once at end-of-transfer with the final `ndx_segments.len()`.

If the per-call count is in the millions and `ndx_segments.len()` is
in the thousands, the `partition_point` is doing real work and a
linear scan would lose. If `ndx_segments.len()` is small (~10-100), the
`partition_point` is fine; suspect elsewhere.

### 4.5 ACL preparation cost per segment

**Where**: `crates/transfer/src/generator/protocol_io.rs:339-363`
(`prepare_pending_acl`), called from `protocol_io.rs:309` for every
entry in every segment.

**What to measure**: When `--acls` is on (and only then), wrap
`metadata::get_rsync_acl` with a duration trace and a hit/miss
counter. If we are issuing `getxattr`-style syscalls per entry even
when the entry has no ACL, the per-segment syscall budget multiplies
the regression. Compare with `--no-acls` on the same workload to
isolate.

This point also doubles as a regression sentinel for any caller that
adds `--xattrs`-style per-entry probing to the segment loop.

## 5. Re-enable criteria

Mirrors task #2089. These benchmark targets must be met before the
default for `inc_recursive_send` flips back to `true`. All targets are
*against upstream rsync 3.4.1 on a matched harness inside the
`rsync-profile` container*, push direction, local file:// transport
to remove SSH/TCP variance.

### 5.1 Wall-clock

| Workload | Mode C wall-clock target |
|----------|--------------------------|
| 100K shallow (100 dirs * 1000 files, depth 2) | within 5% of Mode B |
| 100K deep (100 dirs * 1000 files, depth 10+) | within 10% of Mode B |
| 100K flat (1 dir * 100K files, depth 1) | within 5% of Mode B (single-segment case) |
| 1M shallow (1000 dirs * 1000 files, depth 2) | within 10% of Mode B |
| 1M deep (1000 dirs * 1000 files, depth 10+) | within 15% of Mode B |
| 1M flat (1 dir * 1M files, depth 1) | within 5% of Mode B (single-segment case) |

A 95-201x regression on the 1M shallow cell is the floor that must
disappear; the targets above are what counts as "fixed". If Mode C is
slower than Mode A by any margin at a given cell, that is a hard
re-enable failure for that cell.

### 5.2 Peak RSS

| Workload | Mode C peak RSS target |
|----------|------------------------|
| 1M shallow push, oc-rsync sender | within 1.5x of upstream 3.4.1 sender (Mode A baseline ~7.5 MB receiver, ~76.8 MB sender per the 2026-05-01 baseline) |
| 1M deep push | within 1.5x of upstream |
| 100K shallow push | within 2x of upstream (smaller absolutes amplify the ratio) |

Until `Vec<FileEntry>` pool work (#1050) lands, Mode C peak RSS will
not drop to upstream parity even when the walk-then-send path is
perfectly tuned. The 1.5x bound is the floor we can hit *with the
current Vec*; below it requires the pool-allocator work.

### 5.3 Syscall budget

Captured with `strace -c -p $(pgrep oc-rsync)` over a steady-state
window of the 1M shallow push. Mode C must show:

- `sendto` count per second within 1.2x of Mode B.
- `write` count per second within 1.2x of Mode B.
- Total syscalls within 1.3x of Mode B.

Any factor above 2x at this layer is the signature of the regression
we are chasing and must be fixed before flipping the default.

### 5.4 Interop validation

- `tools/ci/run_interop.sh` push test against upstream rsync 3.0.9,
  3.1.3, and 3.4.1 must pass with `--inc-recursive-send` enabled.
- No `flist.c:2652-2659` "ABORTING due to invalid path from sender"
  on any tested upstream version.
- The new Mode C row in `scripts/benchmark_flist_memory.sh` (per the
  audit) must be green for two consecutive nightly runs before the
  flip lands.

### 5.5 Gating

Once all of 5.1, 5.2, 5.3, and 5.4 pass on the same nightly run, the
re-enable PR may flip `inc_recursive_send: self.inc_recursive_send.unwrap_or(false)`
to `unwrap_or(true)` in `crates/core/src/client/config/builder/mod.rs:383`
and update the corresponding tests. The doc comment in
`crates/core/src/client/config/client/performance.rs:165-186` (which
already says "default `true`") will then match reality without further
editing. The disable revert in `bd12b6ac5` / `b3a264061` becomes the
canonical "do not undo without satisfying section 5" reference.

## 6. Out of scope

- Streaming-walk redesign (interleaving filesystem walk with sub-list
  dispatch). That removes the sender-side memory ceiling but is a
  multi-PR refactor tracked under #1050 and the broader walk-and-send
  rework, not under #2088.
- Daemon-specific syscall profiles. Mode C should be measured on local
  push first to isolate the per-segment dispatch pathology from
  transport variance, per the audit plan.
- `FileEntryExtras` regressions on symlink/hardlink/ACL/xattr paths.
  Each pulls in a boxed allocation that the partition step does not
  touch; covering them needs the audit's separate workload generator.

## 7. Cross-references

- Disable commits: `bd12b6ac5`, `b3a264061`.
- Enable-then-revert commit: `39d47722b` / `854aa753a` (squash + merge).
- Release tag: `d51c95c6a`.
- Audit: `docs/audits/incremental-flist-memory-bench.md`.
- Related trackers: #966 (RSS gap), #971 (1M scaling), #972
  (intermediate-scale peak), #1050 (`Vec<FileEntry>` pool), #1862
  (sender state machine), #2089 (re-enable criteria companion task).
