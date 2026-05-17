# Hardlink-table access audit for parallel-deterministic-delete (#2263)

Status: Audit (task DDP-D1, #2263; preparation for DDP-D2, #2264)
Audience: receiver, generator, engine maintainers about to land the
parallel-deterministic-delete (DDP) plan compute (DDP-B1, DDP-B3) and
the cohort-tagging emitter changes.
Scope: every read and write of any hardlink data structure that is, or
will be, visible on the delete path under the DDP pipeline described in
`docs/design/parallel-deterministic-delete.md` (specifically section 6,
"Hardlink coordination").

Out of scope: hardlink wire encoding, leader/follower transfer
pipelining, `--remove-source-files`. Those touch hardlink state but
never run inside the delete sweep itself.

## 1. Hardlink-related data structures

### 1.1 In-process tables (Rust)

| Name | File | Storage | Thread-safe |
|------|------|---------|-------------|
| `protocol::flist::HardlinkTable` | `crates/protocol/src/flist/hardlink/table.rs:38-48` | `FxHashMap<DevIno, HardlinkEntry>` plus `FxHashMap<u64, ()>` (announced devices) | No - `&mut self` mutators |
| `engine::hardlink::HardlinkTracker` | `crates/engine/src/hardlink.rs:151-157` | `FxHashMap<HardlinkKey, HardlinkGroup>` plus `HashMap<i32, HardlinkAction>` | No - documented as not thread-safe (lines 142-145) |
| `engine::local_copy::hard_links::HardlinkApplyTracker` | `crates/engine/src/local_copy/hard_links.rs:33-40` | `FxHashMap<u32, PathBuf>` leaders, `FxHashMap<u32, Vec<PathBuf>>` deferred | No |
| `engine::local_copy::hard_links::HardLinkTracker` (Unix only) | `crates/engine/src/local_copy/hard_links.rs:184-225` | `FxHashMap<HardLinkKey, PathBuf>` | No |
| `ReceiverContext::prior_hlinks` | `crates/transfer/src/receiver/mod.rs:210-220` | `std::collections::HashMap<u32, bool>` | No - owned by the receiver thread |
| `ReceiverContext::hardlink_tracker` | `crates/transfer/src/receiver/mod.rs:200-209` | `Option<HardlinkApplyTracker>` | No |

No `Mutex`, `RwLock`, `dashmap`, atomics, or lock-free structures are
present anywhere in the hardlink-tracking modules. Every table is owned
by a single struct and accessed through `&self` / `&mut self`. Today
that is correct because every caller runs on one thread.

### 1.2 Per-entry hardlink fields on `FileEntry`

The wire-level cohort identifier lives on the entry itself, not in a
side table:

- `FileEntry::hardlink_idx() -> Option<u32>` (gnum / leader wire NDX) -
  read at `crates/transfer/src/receiver/file_list.rs:721, 730, 757-799`
  and `crates/transfer/src/receiver/directory/links.rs:164, 189`.
- `FileEntry::flags().hlink_first()` / `hlinked()` -
  `crates/transfer/src/receiver/directory/links.rs:163, 186` and
  `crates/transfer/src/receiver/file_list.rs:88, 195`.
- `FileEntry::hardlink_dev()` / `hardlink_ino()` -
  `crates/transfer/src/generator/file_list/hardlinks.rs:48`,
  `crates/transfer/src/receiver/file_list.rs:774-781`.

These are plain fields on `FileEntry`. The receiver's `file_list` is a
`Vec<FileEntry>` borrowed mutably during file-list reception and
borrowed immutably afterwards. Once the file list is frozen for the
delete pass, every read of `hardlink_idx()` is over a stable, read-only
slice.

### 1.3 New DDP delete-side state (the cohort id)

`crates/engine/src/delete/plan.rs` introduces:

- `HardlinkCohortId(u32)` - newtype wrapper over the leader's wire NDX
  (`plan.rs:41-56`). Distinct from `FileEntry::hardlink_idx()` so it
  cannot be accidentally fed back into the wire encoder.
- `DeleteEntry::hardlink_cohort: Option<HardlinkCohortId>` (`plan.rs:107-138`).
  Per-extras tag, set by `compute_extras` and consumed by the emitter.
- `DeleteEntry::with_cohort()` and `DeleteEntry::new()` constructors
  (`plan.rs:118-138`).
- Storage lives inside `DeletePlan::extras: Vec<DeleteEntry>`
  (`plan.rs:147-156`) and from there inside `DeletePlanMap`
  (`crates/engine/src/delete/plan_map.rs:48`), which is the only
  cross-thread channel in the DDP delete pipeline.

`DeletePlanMap` is the lone synchronised container: a
`Mutex<HashMap<PathBuf, DeletePlan>>` (`plan_map.rs:33-48`). The mutex
guards the map operation only - it is not held across any read or
write of the underlying hardlink tables. DDP-B4 (`plan_map.rs:9-29,
44-47`) tracks the bench-driven choice between `Mutex<HashMap>`,
`dashmap::DashMap`, and a sharded variant.

## 2. Current delete-path callers (read sites)

The "delete path" is the code that today runs to compute and apply
deletions on the receiver. With DDP it splits into phase 1
(`compute_extras`, parallel rayon) and phase 2 (single emitter).

### 2.1 Today's batched sweep

The current code is in
`crates/transfer/src/receiver/directory/deletion.rs:40-186`. A `grep`
for `hardlink|hard_link|hlink` returns zero hits in that file. The
batched sweep never consults any hardlink table when deciding what to
unlink, which matches upstream `delete_in_dir`
(`target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`): the
match is by name via `flist_find_ignore_dirness`
(`generator.c:333`), with no inode lookup.

### 2.2 New DDP scaffolding

`crates/engine/src/delete/emitter.rs:249-289` walks
`DeletePlanMap`-published plans and dispatches one `DeleteFs` call per
entry. It already carries `hardlink_cohort` through the plan but does
not yet read the engine-side `HardlinkTracker` or the protocol-side
`HardlinkTable`. The first read site lands in DDP-D2 (#2264) when the
emitter starts tagging itemize lines and avoiding double-stat for
already-seen cohorts.

### 2.3 Indirect reads on the delete path

The only delete-time read that touches hardlink state today is the
`prior_hlinks` / `hardlink_idx` lookup that happens during file-list
ingestion (`crates/transfer/src/receiver/file_list.rs:84-105, 193-210,
715-747`). That happens before `delete_extraneous_files` runs and is
purely on the receiver's own thread.

## 3. Current delete-path write sites

Today, the delete sweep does not write to any hardlink table. All
hardlink-table writes happen earlier:

- Generator (sender-side) writes:
  `crates/transfer/src/generator/file_list/hardlinks.rs:35-73` -
  `HardlinkTable::new()`, `announce_device`, `find_or_insert`. Runs
  inside `assign_hardlink_indices`, called from
  `crates/transfer/src/generator/file_list/mod.rs:110-114, 226-230`.
  Single-threaded: one generator role per transfer.
- Receiver file-list writes:
  `crates/transfer/src/receiver/file_list.rs:97, 205, 665` -
  `match_hard_links(&mut self.file_list, &mut self.prior_hlinks)`.
  `prior_hlinks` is updated inside `match_hard_links`
  (`file_list.rs:737, 743`). Single-threaded: owned by
  `ReceiverContext`.
- Receiver apply writes:
  `crates/transfer/src/receiver/directory/links.rs:157-307` -
  `HardlinkApplyTracker::record_leader`, `apply_follower`,
  `resolve_deferred`. Single-threaded: owned by the receiver thread.

The DDP-D2 plan (#2264) will introduce the first delete-time writes:
the emitter setting `DeleteStats` bookkeeping per cohort and (per the
design's section 6) optionally recording a "leader already seen" bit
to suppress redundant trace lines. Both writes stay on the single
emitter thread by the design's invariant 2.3.1 ("single emitter for
observable effects").

## 4. Today's threading: single-threaded delete = no contention

Every hardlink table on the delete path is reachable only through:

- `ReceiverContext`, owned by the receiver thread, or
- `GeneratorContext`, owned by the generator thread, or
- A worker that holds a `&mut` to its own local `HardlinkTracker`
  (e.g. local-copy executor).

The batched `delete_extraneous_files` path
(`receiver/directory/deletion.rs:40-186`) does fan out to rayon when
the per-directory count exceeds the threshold, but the rayon workers
only call `std::fs::remove_*` and compare against a pre-built
exclusion set; they never touch a hardlink table. There is no current
race because there is no current cross-thread hardlink-table access on
the delete path.

## 5. Tomorrow's threading under DDP

Per `docs/design/parallel-deterministic-delete.md` section 2:

- Phase 1: N rayon workers (sized by the existing rayon pool) run
  `compute_extras(D)` for distinct directories `D` in parallel. Each
  worker builds one `DeletePlan` and publishes it into `DeletePlanMap`.
- Phase 2: one emitter thread drains `DeletePlanMap` in
  `DirTraversalCursor` order and issues unlink / itemize / stats.

The cohort id requires phase 1 to look up, for each candidate extra
name in directory `D`:

1. The destination's `(dev, ino)` from `read_dir + stat`.
2. The leader wire NDX for that `(dev, ino)`, if the destination entry
   participates in a tracked hardlink cohort.

Step 2 is the new cross-thread access. There are three plausible
sources for the lookup, each with different threading implications:

### 5.1 If phase 1 reads the receiver-side file-list cohort table

The receiver-side cohort information lives in `FileEntry` fields on
the `file_list` slice plus the `prior_hlinks` map. Both are owned by
the receiver thread. By the time phase 1 starts, the file-list slice
is frozen (no more `match_hard_links` mutations for that segment) so a
read-only reference is safe to share across rayon workers via
`Arc<[FileEntry]>` or `&'static`-like scoped borrows.

Risk: if INC_RECURSE is in flight and a new segment arrives while
phase 1 workers are still computing extras for an earlier segment,
`prior_hlinks` is still being mutated on the receiver thread
(`receiver/file_list.rs:205`). A worker that reads `prior_hlinks`
concurrently sees a torn `HashMap`. This is a classic
read-during-write race on a non-`Sync` container.

### 5.2 If phase 1 builds its own per-directory cohort cache

`compute_extras` could `stat` every extras candidate and consult a
fresh `FxHashMap<(dev, ino), HardlinkCohortId>` built locally for that
directory. Per-worker maps are inherently race-free, at the cost of
re-statting files that belong to a multi-directory cohort.

Risk: cohort identity must be globally consistent so the emitter can
collapse trace lines across directories. A per-directory cache cannot
deliver that without a global merge step, which would either need a
shared `Mutex<HashMap>` (back to the read-during-write window) or a
post-phase-1 fold (defeats the parallelism).

### 5.3 If phase 1 reads the engine-side `HardlinkTracker`

`engine::hardlink::HardlinkTracker` is documented as not thread-safe
(`crates/engine/src/hardlink.rs:142-145`). The local-copy executor
holds one such tracker per session. Under DDP, the local-copy executor
is exactly the place where multi-thread cohort lookup would land,
because it already correlates `(dev, ino)` to `HardlinkAction`. Two
issues:

- The tracker mutates `actions` on every `register()`
  (`hardlink.rs:193-210`). If `compute_extras` workers also `register`
  on first observation of a cohort, two workers racing to register the
  same `(dev, ino)` race on `groups.insert` and `actions.insert` -
  classic write-during-write.
- The tracker's `groups` map is iterated by `is_hardlink_source`
  (`hardlink.rs:241-247`) and `groups()` (`hardlink.rs:269-271`).
  Either iteration concurrent with a `register()` write is undefined
  behaviour under `&self` / `&mut self` borrow rules.

## 6. Concrete races identified

The DDP delete pipeline introduces the following races unless DDP-D2
locks them down:

| # | Site | Pattern | Trigger |
|---|------|---------|---------|
| R1 | Phase-1 worker reads `prior_hlinks` while INC_RECURSE receiver writes it (`receiver/file_list.rs:205`) | Read-during-write on `HashMap<u32, bool>` | INC_RECURSE + delete + parallel `compute_extras` overlapping a new segment |
| R2 | Two phase-1 workers register a cohort with `HardlinkTracker::register` for the same `(dev, ino)` (`hardlink.rs:193-210`) | Write-during-write on `FxHashMap` | Same destination inode shows up as extras in two directories worked by two rayon threads (hardlinks across directories) |
| R3 | Phase-1 worker reads `HardlinkTable::get` while another phase-1 worker calls `find_or_insert` (`protocol/src/flist/hardlink/table.rs:90-108`) | Read-during-write on `FxHashMap<DevIno, HardlinkEntry>` | Multi-segment INC_RECURSE delete where workers cover overlapping cohorts |
| R4 | Phase-1 worker mutates `announced_devices` via `announce_device` while another worker reads `entries` (`protocol/src/flist/hardlink/table.rs:69-73`) | Independent mutation but on the same struct; violates `&self` borrow | Same as R3 |
| R5 | Emitter reads `HardlinkApplyTracker::leader_path` (`local_copy/hard_links.rs:110-112`) while a parallel commit thread calls `record_leader` | Read-during-write on `FxHashMap<u32, PathBuf>` | Only triggers if the apply path is ever moved off the receiver thread; today the tracker is held by `ReceiverContext` so this race is latent rather than active |
| R6 | Phase-1 worker takes `DeletePlanMap.insert` while emitter calls `take` for the same `dir` (`plan_map.rs:73-91`) | Already mutexed; listed for completeness - no race, but lock-contention regression candidate | Many small directories on a heavily threaded pool |

**Primary risk: cohort-ID lookup (R1-R4).** R1 and R3 are
read-during-write windows; R2 and R4 are write-during-write windows.
R1 is the only one that can trigger today (INC_RECURSE delete with
parallel `compute_extras`); R2-R4 trigger as soon as DDP-D2 wires a
shared cohort table into `compute_extras`.

R5 is latent: it becomes real only if the apply path is parallelised
in a later DDP task. Worth flagging now so DDP-D2 does not paint into
that corner.

## 7. Recommendations

The minimum invariant DDP-D2 must enforce: **no phase-1 worker may
mutate any shared hardlink table.** Phase 1 reads cohort identity from
a frozen snapshot; phase 2 may mutate.

Three viable shapes for the shared cohort table, plus a tradeoff
matrix:

### 7.1 Option A - frozen snapshot, no shared locks

Build a `CohortIndex` (read-only) at the boundary between file-list
reception and delete dispatch:

```rust
pub struct CohortIndex {
    by_dev_ino: FxHashMap<(u64, u64), HardlinkCohortId>,
}
```

Share it via `Arc<CohortIndex>` to every rayon worker. Phase-1 workers
only call `&self` lookup. Phase 2 also reads only.

Pros: zero contention, no locks, predictable performance, mirrors
upstream's "build idev table, then walk" structure
(`hlink.c:59-65`, `hlink.c:186-208`).

Cons: snapshot must be rebuilt per INC_RECURSE segment. Each rebuild
is O(n) over the segment's hardlinked entries. Storage cost: 16 bytes
per cohort entry on 64-bit Linux.

### 7.2 Option B - per-cohort `RwLock`

Wrap the existing tables with `parking_lot::RwLock` per cohort id:

```rust
struct LockedTable {
    inner: FxHashMap<u32, RwLock<HardlinkEntry>>,
}
```

Phase-1 workers acquire read locks for lookup; writes go through a
write lock.

Pros: bounded blocking. Allows late mutations from phase 1 if a future
extension lets workers discover new cohorts on the fly.

Cons: still allows write-during-read if the outer `FxHashMap` is
itself mutated. Each lookup pays a hash + lock acquire (typically
20-50 ns under no contention with `parking_lot`). Lock acquisitions on
the cohort hot path defeat the rayon parallelism advantage.

### 7.3 Option C - entry-level atomics

Replace `HardlinkEntry::link_count: u32` with `AtomicU32`, keep the
outer `FxHashMap` mutex-protected:

```rust
struct HardlinkEntry { first_ndx: u32, link_count: AtomicU32 }
```

Pros: cheap atomic increment for cohort visit counters.

Cons: `first_ndx` is the cohort identity and is immutable, so atomics
buy nothing for lookup. Only useful if DDP-D2 ends up counting cohort
visits during phase 1 - the design currently does not require this.

### 7.4 Option D - full table `Mutex`

Wrap the whole `HardlinkTable` in `Mutex<HardlinkTable>` everywhere
the delete path sees it.

Pros: minimal code change. Trivially correct.

Cons: serialises every lookup across all rayon workers. The whole
point of phase 1 is parallelism; this regresses to "sequential delete
through a hardlink-table mutex" for any transfer with hardlinks.

### 7.5 Tradeoff matrix

| Option | Locking cost / lookup | Memory overhead | Phase-1 contention | INC_RECURSE rebuild | Complexity |
|--------|-----------------------|-----------------|--------------------|---------------------|------------|
| A: frozen snapshot | None (FxHashMap lookup, ~20 ns) | 16 B / cohort | Zero | Per-segment rebuild O(n_hlinked) | Low |
| B: per-cohort RwLock | ~20-50 ns (parking_lot) | 8 B / cohort overhead | Bounded; reader-side cheap | Same as today | Medium |
| C: entry-level atomics | Atomic load (~5 ns) on counter only | 0 | Counter-only | Same as today | Medium |
| D: full table Mutex | ~30 ns + contention | 0 | High (every worker serialises) | Same as today | Lowest |

**Recommendation: Option A.** It is the only design that makes the
"phase-1 workers are pure readers" invariant a property of the type
system rather than a discipline. Costs are bounded (one rebuild per
INC_RECURSE segment, parallel with the rest of segment ingestion) and
matches the upstream structure of `init_hard_links` / `match_hard_links`
preceding `do_delete_pass`. Fallback to Option B is straightforward
if a future feature ever needs late phase-1 cohort mutation; the
public surface (`CohortIndex::lookup(&self, dev_ino) -> Option<HardlinkCohortId>`)
stays stable.

## 8. Trigger conditions

R1-R4 trigger when **all** the following hold during a single run:

- `--hard-links` (`-H`) is on.
- `--delete*` (any timing) is on.
- The transfer has hardlink cohorts that span directories (single-dir
  cohorts never cross worker boundaries).
- DDP is wired (post DDP-B1/B3) so `compute_extras` runs on rayon.
- INC_RECURSE is negotiated for R1; the others trigger without
  INC_RECURSE as soon as a shared cohort table is read by phase 1.

R5 triggers only if the apply path is parallelised - latent today.

R6 is a contention regression, not a correctness bug, and is already
flagged for follow-up by DDP-B4 (`plan_map.rs:44-47`).

## 9. Implementation plan for DDP-D2 (#2264)

1. **Land `CohortIndex` (Option A)** as a sibling to
   `DeletePlanMap` in `crates/engine/src/delete/`:
   - `pub struct CohortIndex { by_dev_ino: FxHashMap<(u64,u64), HardlinkCohortId> }`
   - `impl CohortIndex { pub fn build(file_list: &[FileEntry]) -> Self; pub fn lookup(&self, dev: u64, ino: u64) -> Option<HardlinkCohortId>; }`
   - Wrap in `Arc<CohortIndex>` at the receiver before phase 1
     dispatch.
2. **Plumb `Arc<CohortIndex>` through the rayon dispatch surface** in
   the existing `receive_extra_file_lists` hook
   (`crates/transfer/src/receiver/file_list.rs:130-233`). The hook
   already runs once per segment; rebuild the index here.
3. **Tag `compute_extras` output**: in the future
   `crates/engine/src/delete/extras.rs` (PR #4262), per the design
   section 6, attach `cohort = index.lookup(stat.dev, stat.ino)` to
   each `DeleteEntry`. Use `DeleteEntry::with_cohort` when present.
4. **Add a regression test** that exercises hardlinks across
   directories under `--delete` with rayon parallelism enabled. Use
   `tempfile::TempDir`, create a cohort that spans `a/` and `b/`,
   delete both ends, assert no torn-map panic and deterministic
   itemize order.
5. **Add a Loom or ThreadSanitizer test gate** (Loom strongly
   preferred for FxHashMap shapes) that exercises 16 rayon workers
   reading the `CohortIndex` while one builder thread populates a
   fresh one between segments. Loom proves the read-only invariant.
6. **Document the invariant** in
   `docs/design/parallel-deterministic-delete.md` section 6: phase 1
   reads `CohortIndex` only, no mutation. Update
   `crates/engine/src/hardlink.rs:142-145` thread-safety paragraph to
   reference the `CohortIndex` indirection.
7. **Out of scope for DDP-D2, tracked for follow-up**:
   - Reuse `CohortIndex` for `--itemize-changes` cohort tags
     (currently each itemize line stats independently).
   - Bench Option A snapshot rebuild cost on a transfer with >100k
     hardlinks to confirm the per-segment cost stays under the
     deletion-phase budget.

This plan keeps the DDP-D2 patch surgical (one new type, one new
plumbing wire) while closing R1-R4 by construction and leaving R5/R6
on the backlog with clear owners.

## 10. Upstream cross-check

Upstream rsync 3.4.1's `do_delete_pass`
(`target/interop/upstream-src/rsync-3.4.1/generator.c:351-387`) runs
strictly after `match_hard_links`
(`target/interop/upstream-src/rsync-3.4.1/hlink.c:186-208`), which
runs strictly after `init_hard_links`
(`target/interop/upstream-src/rsync-3.4.1/hlink.c:59-65`). The
`prior_hlinks` and `dev_tbl` hashtables are therefore frozen by the
time deletion starts; upstream achieves the read-only invariant by
sequencing, not by locking, because it has only one generator thread.

Option A in section 7.1 is the closest mechanical translation of that
sequencing into the DDP world: keep the table frozen for the duration
of phase 1, rebuild it between INC_RECURSE segments. The
implementation differs (Rust types vs C globals), but the invariant -
"hardlink tables are read-only during the delete sweep" - is
preserved byte-identically.
