# Delete / Exclude Parallelism Redesign

## Overview

oc-rsync runs the `--delete` sweep and the per-entry exclusion/filter
matching that gates it across rayon worker threads, while preserving
byte-for-byte wire compatibility with upstream rsync. This document records
the current state of that parallelism and the model that keeps it
wire-safe.

The governing idea is a strict phase boundary. The deletion pipeline is
split into a **DECIDE** half (scan a directory, evaluate the filter chain
per candidate, choose the deletion SET, fix the emission ORDER) and an
**EXECUTE** half (issue the `unlink`/`rmdir` syscalls and emit the
itemize / `NDX_DEL_STATS` records). Parallelism is allowed to touch only
two things: the *cost* of filter matching inside DECIDE, and the *cost* of
the `unlink` syscalls inside EXECUTE. The deletion SET and the emission
ORDER are always produced in serial code and re-serialized before they
reach the wire. Nothing order-bearing is computed on a worker thread.

Two independent code paths implement deletion and each follows this rule:

- the network/receiver delete pass in
  `crates/transfer/src/receiver/directory/deletion.rs`, and
- the local-copy delete path in `crates/engine/src/delete/` plus
  `crates/engine/src/local_copy/executor/cleanup.rs`.

## Current Parallelism State

### Receiver delete pass (transfer crate)

`ReceiverContext::delete_extraneous_files`
(`crates/transfer/src/receiver/directory/deletion.rs:53-489`) is already
rayon-parallel at directory granularity. The cohort is one destination
directory.

The serial setup builds the directory-to-children keep map and the
scan-target list from the file list (`deletion.rs:78-129`), always
registering the transfer root `.` as a scan target (`deletion.rs:121-127`)
to match upstream's `delete_in_dir()` running for the root even when every
source entry is filter-excluded. The filter chains are frozen for sharing
across workers: the flat global chain is wrapped in an `Arc`
(`deletion.rs:158`), and the per-directory-merge chain is cloned, has its
transfer root set, and is wrapped in a second `Arc`
(`deletion.rs:159-163`). The optional `DirSandbox` (SEC-1.q2) is cloned
into the worker closure as an `Arc` (`deletion.rs:172-173`).

The parallel region is `crate::parallel_io::map_blocking`
(`deletion.rs:188-451`), gated by the `ParallelOp::Deletion` threshold
(`deletion.rs:191-192`). Inside one worker, DECIDE and EXECUTE are *fused*
per cohort:

- DECIDE: each worker clones the merge chain and replays `enter_directory`
  down to its own directory so per-dir merge rules are active
  (`deletion.rs:246-259`); it then evaluates `allows_deletion` against
  either the per-directory chain or the flat global chain per candidate
  (`deletion.rs:311-314`).
- EXECUTE: in the same loop body the worker performs the `unlink` /
  recursive `rmdir` via the sandbox-anchored helpers
  (`deletion.rs:377-408`).

Each worker accumulates its own `DeleteStats` and a `Vec<PathBuf>` of
deleted relative paths and returns them in a tuple (`deletion.rs:188-189`,
`deletion.rs:449`). `--max-delete` is enforced across workers via a shared
`AtomicU64` (`deletion.rs:139`, `deletion.rs:332-338`).

Emission is **serial after the parallel region**: the writer is not
`Send`, so the `*deleting` itemize `MSG_INFO` frames are emitted in the
serial fold over `per_dir_results` (`deletion.rs:460-478`). The same fold
folds the per-worker counters into the combined `DeleteStats` that drives
`NDX_DEL_STATS` (`deletion.rs:461-465`) and ORs the fail-loud `io_error`
bits (`deletion.rs:475-477`).

Error discrimination is also serial-safe: a worker threads back an
`Option<io::Error>` (`deletion.rs:188`), and `fail_loud_unlink_error`
(`deletion.rs:533-539`) / `classify_scan_error` (`deletion.rs:510-515`)
split EACCES/NotFound (upstream-parity non-fatal) from every other class
(fail-loud security boundary).

### Local-copy delete path (engine crate)

The local-copy delete path is structured around an explicit DECIDE/EXECUTE
split in code.

DECIDE produces a `DeletePlan` (`crates/engine/src/delete/plan.rs:147-235`):
a frozen, per-directory list of `DeleteEntry` values in upstream
`delete_in_dir` emission order. `DeletePlan::sort_by_name`
(`plan.rs:214-224`) sorts with upstream's `f_name_cmp` ascending and then
reverses, matching upstream's reverse-directory loop (`generator.c:320`).
The plan tracks a `sorted` flag so the ordering invariant can be asserted
before publication (`plan.rs:155`, `plan.rs:202-205`).

`crates/engine/src/local_copy/executor/cleanup.rs` drives DECIDE through
three sub-phases in `build_plan_for_directory` (`cleanup.rs:172-312`):

- Phase A (serial): scan the destination in readdir order, apply the cheap
  keep-set and partial-dir filters, and collect `DeletionCandidate` values
  holding only `Send` data (`cleanup.rs:210-252`, `cleanup.rs:318-320`).
- Phase B (parallel above threshold): compute the pure `allows_deletion`
  decision for every candidate against an immutable snapshot
  (`cleanup.rs:254-275`). Above `PARALLEL_DELETION_MATCH_THRESHOLD = 64`
  (`cleanup.rs:23`) this runs `candidates.par_iter().map(...).collect()`;
  below it the identical closure runs serially. `par_iter().map().collect()`
  preserves index order so decisions stay aligned with the readdir-order
  candidates.
- Phase C (serial): apply the decisions in readdir order, log
  filter-protected drops, count against `--max-delete`, push into the plan,
  and call `sort_by_name` to fix the wire emission order
  (`cleanup.rs:277-311`).

EXECUTE is the emitter. The sequential emitter
(`crates/engine/src/delete/emitter/`) is the sole caller of `DeleteFs`
methods (`cleanup.rs:84-87`, `cleanup.rs:148-152`) and anchors each plan
directory's dispatch on a SEC-1.q dirfd. The cohort is one parent
directory, keyed by `DeleteCohortKey`
(`crates/engine/src/delete/reorder_buffer.rs:141-162`), which wraps the
destination-relative parent directory path.

A feature-gated parallel consumer
(`crates/engine/src/delete/parallel_consumer.rs`, `ParallelDeleteEmitter`)
exists behind the `parallel-delete-consumer` Cargo feature
(`crates/engine/Cargo.toml`, `crates/engine/src/delete/mod.rs:42-43`). With
the feature off these symbols are not compiled and the sequential emitter
remains the only consumer (`parallel_consumer.rs:10-15`). The consumer
drains sealed cohorts in strict rank order through a dedicated OS thread
driven by a `Condvar`, dispatching one cohort at a time; within a cohort
the ops dispatch in parallel via `rayon::par_iter`, but cohort `N + 1`
cannot begin until every op in cohort `N` has completed
(`parallel_consumer.rs:115-124`).

## The DECIDE/EXECUTE Phase Boundary as the Wire-Compat Seam

The wire image of a delete sweep is two things: the SET of paths deleted,
and the ORDER in which `NDX_DEL_STATS` / itemize records are produced. Both
are upstream-defined and order-sensitive. The redesign treats the
DECIDE/EXECUTE boundary as the seam that protects them:

- The deletion SET is the union of per-candidate `allows_deletion`
  decisions. That decision is a pure function of `(path, is_dir, frozen
  filter chain)` with no order dependence (see the snapshot doc comment at
  `crates/engine/src/local_copy/context_impl/options/filter.rs:177-189`),
  so it can be computed on any thread. Parallelism changes WHERE the
  decision runs, never WHAT it decides.
- The emission ORDER is produced by serial code. In the local-copy path,
  `DeletePlan::sort_by_name` re-serializes the plan into upstream order
  after the parallel decision (`plan.rs:214-224`, `cleanup.rs:310`). In the
  receiver path, the order-bearing itemize emission runs serially in the
  post-parallel fold (`deletion.rs:460-478`).
- Cross-cohort ordering is preserved even when cohorts run in parallel: the
  reorder buffer is a `BTreeMap` keyed by a dense pre-order rank and drains
  strictly in rank order (`reorder_buffer.rs:286-292`); the parallel
  consumer fully drains cohort `N` before pulling cohort `N + 1`
  (`parallel_consumer.rs:115-124`).

The net effect: parallelism touches only `cost(matching)` in DECIDE and
`execute(unlink)` in EXECUTE. The SET and the ORDER are produced in serial
DECIDE and re-serialized before the wire.

## Filter-Decision Immutability (Arc Snapshot, No Rc Across Threads)

The local-copy filter state lives in `Rc<RefCell<...>>` stacks on the
owning thread: `dir_merge_layers`, `dir_merge_marker_layers`,
`dir_merge_ephemeral`, `dir_merge_marker_ephemeral`, and
`dynamic_dir_merge_stack`
(`crates/engine/src/local_copy/context.rs:91-108`). These are neither
`Send` nor `Sync` and must never cross a rayon boundary.

The XMP work introduced an immutable snapshot to bridge that gap.
`CopyContext::deletion_filter_snapshot`
(`crates/engine/src/local_copy/context_impl/options/filter.rs:117-132`)
freezes the effective deletion chain by cloning the global filter program,
the static layers, the active ephemeral frame, and the top dynamic
`dir-merge` frame by value into a `DeletionFilterSnapshot`
(`filter.rs:190-198`). The snapshot is `Send + Sync` and read-only: the
live `Rc<RefCell<...>>` stacks are cloned, never mutated
(`filter.rs:106-116`).

`DeletionFilterSnapshot::allows_deletion` (`filter.rs:200-228`) mirrors
`CopyContext::allows_deletion` (`filter.rs:65-103`)
instruction-for-instruction, so the per-entry decision is byte-for-byte
identical regardless of which thread evaluates it. The Phase B closure in
`build_plan_for_directory` captures ONLY the snapshot, never the
`CopyContext` (`cleanup.rs:254-275`), which is what makes the `par_iter`
sound.

The receiver path applies the same discipline with its own chain types:
the flat and per-dir-merge chains are wrapped in `Arc` before the parallel
region (`deletion.rs:158-163`), and `enter_directory` (which takes
`&mut self`) is replayed onto a per-worker clone of the merge chain rather
than shared (`deletion.rs:246-259`).

The destination-side merge load is also isolated from source-side
evaluation. `DirectoryFilterGuard` can be armed to restore a whole-stack
`FilterStateSnapshot` on drop (`context.rs:545-566`, `context.rs:616-630`),
so a destination deletion scan that loads `.rsync-filter` rules can never
perturb the sender-visible filter stacks - mirroring upstream's separate
`delete_filt` chain.

## dirfd Anchoring (SEC-1.q, Sequential and Parallel Consumer)

Both EXECUTE paths anchor each cohort's deletions on a directory file
descriptor opened against the sandbox root, closing the mid-delete
prefix-symlink-swap TOCTOU that a path-based re-resolution leaves open.

Sequential emitter: the plan directory is opened once against
`DirSandbox::root_dirfd` via `open_dir_at` / `plan_directory_to_relative`
just before dispatch, and every op routes through the dirfd-anchored `*_at`
trait methods (`crates/engine/src/delete/emitter/mod.rs:312-346`,
`emitter/mod.rs:425-438`). A failed open or an unattached sandbox falls
back to the path-based methods, preserving the pre-SEC-1.q contract.

Parallel consumer: `ParallelDeleteEmitter::with_sandbox` attaches the
sandbox; `dispatch_cohort` opens the cohort's destination-relative parent
directory once against the sandbox root and anchors every op on that fd,
with `dispatch_one` selecting the `*_at` method per kind and falling back
to the path-based method when no dirfd is available. The anchor is proven
by program-order tests: opening the cohort dirfd first, swapping an
ancestor to an outside-pointing symlink, then dispatching the `unlinkat`
hits the real in-sandbox inode and spares the outside sentinel; the
end-to-end test confirms the component walk refuses the planted ancestor
symlink under `O_NOFOLLOW`.

Receiver pass: SEC-1.q2 routes both the scan and the per-entry deletions
through `read_dir_via_sandbox_or_fallback` (`deletion.rs:219-224`),
`recursive_unlinkat_via_sandbox_or_fallback` (`deletion.rs:381-386`), and
`unlink_via_sandbox_or_fallback` (`deletion.rs:396-402`). Top-level and
single-component entries take the sandbox-anchored fast path; deeper
entries take the documented path-based fallback.

## Upstream Parity

The model is anchored to upstream rsync 3.4.x:

- One cohort per destination directory matches
  `generator.c:delete_in_dir()`. Upstream runs the per-directory filter
  reload `change_local_filter_dir()` (which drives `push_local_filters` /
  `pop_local_filters`) before iterating one directory's candidates
  (`target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`, in
  particular the `change_local_filter_dir(fbuf, dlen, F_DEPTH(file))` call
  at `generator.c:301` followed by the reverse-directory candidate loop at
  `generator.c:320-344`). oc-rsync's per-worker `enter_directory` replay
  (`deletion.rs:246-259`) and the local-copy destination merge guard
  (`cleanup.rs:184-185`) reproduce that reload.

- The reverse emission order matches upstream's
  `for (i = dirlist->used; i--; )` loop (`generator.c:320`), reproduced by
  `DeletePlan::sort_by_name`'s ascending-then-reverse (`plan.rs:214-224`).

- The per-candidate exclusion test matches upstream's
  `exclude.c:check_filter()` / `rule_matches()`; oc-rsync's wildcard
  matcher `dowild` is a direct byte-for-byte port of
  `lib/wildmatch.c:dowild()` (`crates/filters/src/wildmatch.rs:1-13`,
  `wildmatch.rs:67`), and `FilterChain::allows_deletion`
  (`crates/filters/src/chain/mod.rs:243-284`) reproduces the
  `delete_excluded` global's effect on protect/risk rules.

- `NDX_DEL_STATS` carries one frame per goodbye cohort with five varints;
  the buffer and consumer preserve cohort identity and never emit the frame
  themselves (`reorder_buffer.rs:63-71`, `parallel_consumer.rs:40-48`,
  `parallel_consumer.rs:115-124`), leaving the unchanged goodbye writer to
  ship the totals.

- Error classes follow `delete.c:delete_item` (EACCES non-fatal, io_error
  bit drives `RERR_PARTIAL`); see the upstream citations at
  `deletion.rs:528-538`.

The invariant holds across all of these: `Rc`/`RefCell` never crosses
rayon (snapshot to `Arc`/by-value immutable), the decision SET is a pure
function evaluated off-thread, and the emission ORDER is produced and
re-serialized in serial code.

## Remaining Work

- DEL-R residual: the receiver pass fuses DECIDE and EXECUTE per cohort
  inside `map_blocking` and folds emission serially afterward
  (`deletion.rs:188-478`). It does not yet route through the `DeletePlan` /
  reorder-buffer / cohort-consumer machinery the engine path uses, so the
  two paths have divergent cohort plumbing. Converging the receiver onto
  the plan/emitter model is residual work.

- Default-on gating: `ParallelDeleteEmitter` remains behind the
  `parallel-delete-consumer` feature (`crates/engine/src/delete/mod.rs:42-43`)
  and is not yet wired into the receiver-side call site. Promoting it past
  the feature gate requires the call-site migration and a wire-byte parity
  gate against the sequential emitter.

- Benches: the parallel thresholds (`PARALLEL_DELETION_MATCH_THRESHOLD = 64`
  at `cleanup.rs:23`; `DEFAULT_DELETION_THRESHOLD = 64` at
  `crates/transfer/src/parallel_io.rs`; `MAX_BUFFERED_COHORTS` and
  `DRAIN_BATCH_CAP` at `reorder_buffer.rs`) are fixed compile-time constants
  justified by design notes rather than by checked-in benchmarks.
  Establishing benches that confirm the crossover points on representative
  trees, and that confirm the parallel consumer's throughput gain over the
  sequential emitter, is outstanding.
