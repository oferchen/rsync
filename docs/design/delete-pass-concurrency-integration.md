# Delete-pass concurrency + performance integration

Status: Design. Synthesizes the two-phase delete pipeline
(`docs/design/parallel-deterministic-delete.md`), the parallel delete
consumer (DEL series, `docs/design/del-4c-delete-threshold-decision.md`),
the destination-rooted per-directory filter reload, and the pending
local-copy delete correctness fix into a single integrated plan.
Audience: engine and transfer maintainers working on the local-copy
`--delete` path.
Scope: how the deletion correctness fix, the incremental destination
filter stack (PEX), and the parallel delete consumer (DEL) compose; the
phase boundary that keeps them independent; the sequencing and the data
gaps that gate each piece.

Out of scope: the deletion correctness fix's own root cause (covered in
the exclude/exclude-lsh investigation notes), the DEL wire-ordering
parity tests (DEL-3), and the bench harness implementations (DEL-4.a/b).

## 1. The phase boundary is the integration seam

The local-copy delete path is already a two-phase pipeline. The phase
boundary is exactly where each piece of work belongs, and it is why they
compose without entangling:

```
Phase 1 - DECIDE  (single-threaded; CopyContext is Rc<RefCell>, !Send/!Sync)
  build_plan_for_directory            cleanup.rs:161
    -> enter_destination_for_deletion transfer.rs:85   (per-dir filter frame)
    -> allows_deletion                filter.rs:65      (filter decision)
    -> DeletePlan { DeleteEntry, .. } plan.rs
                       |
                       |  plan is FROZEN here  (cleanup.rs:91-106)
                       v
Phase 2 - EXECUTE  (parallel; cohort-ordered)
  ctx.emit_one(fs)                    cleanup.rs:138
    -> ParallelDeleteEmitter::run     delete/context/core.rs:392 (feature-gated)
    -> rayon intra-cohort unlink      delete/parallel_consumer.rs
```

The decisive property: **the filter decision (which entries delete) is
fully resolved in Phase 1, before any cohort is dispatched.** The
parallel consumer consumes a list of already-decided `DeleteEntry`s and
never re-evaluates filter state. This is verified in code: `allows_deletion`
is called only from Phase-1 sites (`cleanup.rs:232`), and the comment at
`cleanup.rs:93-95` states the emitter "sees only the entries that should
actually unlink."

## 2. Mapping the three work items to phases

| Work item | Phase | Concurrency |
|---|---|---|
| **Deletion correctness fix** (decouple the keep-set from filter protection) | Decide | Serial by necessity. `CopyContext`'s filter structures are `Rc<RefCell<...>>` (`context.rs:91-95,108`), so the decision cannot cross threads. |
| **PEX** (incremental destination filter stack) | Decide | Serial. Its win is **algorithmic**, not parallel: replace the per-dir O(depth) ancestor re-walk with an O(1)-amortized depth-indexed stack. |
| **DEL** (parallel delete consumer) | Execute | The only genuinely concurrent lever. Cross-cohort strict-serial; intra-cohort parallel. |

Because the correctness fix lives wholly in the serial decide phase, it
needs no awareness of DEL or PEX. It changes plan *membership*; DEL
changes execution *throughput*; PEX changes decision *cost*. Three
independent axes, three independent PRs.

### 2.1 SOLID framing

The phase split is a single-responsibility boundary: *decide what to
delete* vs *execute the deletions*. Keeping the filter evaluator on the
decide side of that line is what lets the parallel executor stay a pure
mechanism with no policy. Any design that pushes filter evaluation into
the parallel phase collapses that boundary and reintroduces shared
mutable filter state across threads.

## 3. PEX - incremental destination filter stack

### 3.1 Current cost

`enter_destination_for_deletion` (`transfer.rs:85-142`) is not
incremental. On every deletion directory it:

1. save-and-clears the five shared filter stacks (`transfer.rs:95-101`),
2. recomputes `dest_root` by stripping `relative` from `destination`,
3. re-enters the dest root frame, then loops over **every** ancestor
   prefix of `relative`, reloading each level's merge files via
   `enter_directory_for_path` (`transfer.rs:133-139`).

A directory at depth `d` re-reads `d+1` merge frames; a subtree of depth
`D` costs the sum O(d) = **O(D^2)**.

### 3.2 Upstream's O(1)-amortized model

Upstream `change_local_filter_dir` (`exclude.c:875-901`) maintains a
static depth-indexed stack (`cur_depth`, `filt_array[]`). On each
dir-enter it pops only frames with index >= the new depth, then pushes
exactly one new frame. A depth-first walk does one push and at most one
pop per dir transition -> O(1) merge-file reads per directory. It is
driven from `delete_in_dir` with `F_DEPTH(file)` (`generator.c:308`).

### 3.3 PEX design

Install a destination-rooted depth-indexed stack maintained in lockstep
with the copy recursion. The recursion already has the hook: the source
dir-merge guard is pushed/popped at `recursive/mod.rs:202` via the
guard-drop pattern. PEX adds a parallel dest stack pushed on the same
dir-enter boundary and popped on dir-leave, mirroring
`change_local_filter_dir`. The five shared structures it maintains are
the same ones `enter_destination_for_deletion` save-and-clears today
(`dynamic_dir_merge_stack`, `dir_merge_layers`, `dir_merge_marker_layers`,
`dir_merge_ephemeral`, `dir_merge_marker_ephemeral`). EXCLUDE_SELF,
anchoring, and the `n`-modifier are preserved (already handled by
`enter_directory_for_path`).

### 3.4 Expected payoff and the deferral gate

For typical rsync trees (depth 3-10) the O(depth^2) re-walk is 9-100
extra per-dir merge-file probes, most of which are `NotFound` stats -
cheap relative to readdir and unlink. The payoff concentrates in
pathologically deep trees. **PEX must be gated on an actual deep-tree
microbench.** If the measured decision-time fraction is negligible,
document PEX as deferred rather than shipping an incremental-stack
state machine for a non-win. Do not ship complexity ahead of a measured
need.

## 4. DEL - parallel delete consumer

### 4.1 Wire-ordering invariant (must be preserved)

- **Cross-cohort: strict serial.** Cohort N+1 cannot begin until every
  op in cohort N has completed (`parallel_consumer.rs:110-119`). Cohort
  index is a dense `u32` assigned single-threaded in pre-order traversal.
- **Intra-cohort: may parallelize.** Ops within one parent-dir cohort
  dispatch via rayon because each targets a distinct destination leaf.
- **NDX_DEL_STATS:** exactly one frame per transfer; per-cohort stats are
  folded into a single total; the consumer never writes the frame - the
  unchanged generator goodbye writer serializes it.

### 4.2 Data status - the gates are unmeasured

The DEL bench corpus (`del-4a`, `del-4b`, `dashmap-vs-mutex-100k`,
`dashmap-vs-mutex-1m`, `dmb-b`) is template-only: the result tables are
placeholders. The DEL-4.c promotion gates were defined but never run:

| Gate | Threshold | Status |
|---|---|---|
| G2 parallel speedup at 100K (flat, 4t) | >= 1.5x | not measured |
| G3 parallel speedup at 1M (realistic, 4t) | >= 2.0x | not measured |
| G4 no 1-thread regression | within 5% | not measured |
| DashMap vs Mutex<HashMap> for DeletePlanMap | -- | no verdict |

DEL is therefore an **unproven** win and remains opt-in
(`--features parallel-delete-consumer`). Promotion to default-on must
follow `del-4c-delete-threshold-decision.md`: run DEL-4.a/b, evaluate
G1-G8, then choose unconditional / threshold-gated / opt-in.

## 5. Sequencing

1. **Correctness fix first, standalone.** Campaign-blocking and
   data-loss-sensitive; rides the serial decide phase; needs no DEL/PEX
   awareness. Both-directions regression test required (delete the
   source-twin extra; keep the dest-only excluded file plus the dest's
   own merge files).
2. **PEX second, as a pure refactor PR**, gated on a deep-tree
   microbench. Defer (document, do not ship) if the win is negligible.
3. **DEL bench-gate, separate effort.** Run DEL-4.a/b, fill the gates,
   then decide the default-on strategy. Do not bundle a default flip
   into the correctness fix.

## 6. TOCTOU-safety by construction (dirfd-anchored plan)

The goal is upstream's deletion/exclusion semantics with concurrency
AND structural TOCTOU-avoidance. Upstream achieves the latter by holding
the directory open and operating relative to that descriptor
(`delete_in_dir` works against an opened dir; deletions are `unlinkat`).
The frozen-plan boundary is the natural carrier for that descriptor, so
all three properties share one seam.

### 6.1 Current posture - EXECUTE is dirfd-anchored

The parallel emitter is already SEC-1.q hardened
(`delete/emitter/mod.rs`):

- `open_plan_dirfd` (line 343) opens the plan directory ONCE against
  `DirSandbox::root_dirfd`; `open_dir_at` (line 597) walks it
  component-by-component with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`.
- Every deletion dispatches through dirfd-anchored `*_at` trait methods
  with `parent_fd` (lines 312-321): `unlinkat(parent_fd, name, flag)`,
  never `unlink(full_path)`.

So the execute phase cannot be redirected by a mid-syscall symlink swap
on a path prefix.

### 6.2 Residual window and the structural fix

DECIDE does `readdir` + `fstatat` + filter on the directory and freezes
a plan keyed by PATH (`plan.directory`). EXECUTE then RE-RESOLVES that
path via `open_plan_dirfd`. The re-resolution is hardened (`O_NOFOLLOW`
per component, so a swapped prefix is caught at open time), making it
TOCTOU-resistant - but it re-resolves, so it is not TOCTOU-free.

The fully structural form: **carry the DECIDE-phase dirfd in the plan.**
Open the directory once in Phase 1, do `readdir` + `fstatat` + the filter
decision against that descriptor, store it as `Arc<OwnedFd>` on the
`DeletePlan`, and `unlinkat` against the SAME descriptor in Phase 2. No
re-resolution between decision and unlink -> no window. `OwnedFd` is
`Send`, so the `Arc<OwnedFd>` survives the serial-decide to
parallel-execute cohort handoff unchanged; it composes with DEL (one
dirfd per cohort/directory) and with the upstream-semantics fix (the
keep-set / Gate-B decision is independent of the descriptor).

### 6.3 Why this unifies all three pillars on one seam

The frozen plan is simultaneously: the concurrency boundary (decided
serially, executed in parallel), the security boundary (carries the
dirfd capability), and the semantics boundary (carries the
upstream-faithful keep/delete decision). One structure - `DeletePlan {
dirfd: Arc<OwnedFd>, entries: [DeleteEntry] }` - serves all three. This
is the target shape; the dirfd-carry is the delta from today's
re-resolving emitter.

## 7. Binding guardrail

The filter decision must never migrate into Phase 2. The moment any
`allows_deletion` / keep-set logic runs on a rayon worker, `Rc<RefCell>`
filter state crosses threads - undefined behaviour at worst, and it
reintroduces the source-frame-leak class of bug. The two-phase freeze is
load-bearing for correctness and for thread safety. New concurrent
behaviour on the delete path ships only with adversarial-ordering stress
coverage before any default-on flip
(`feedback_concurrent_path_discipline.md`; PIP-7 is the precedent).

## 7. Cross-references

- Two-phase model: `docs/design/parallel-deterministic-delete.md`
- DEL threshold decision: `docs/design/del-4c-delete-threshold-decision.md`
- DEL ordering audit: `docs/design/del-1a-upstream-ordering-audit.md`
- Superseded strict-order gate: `docs/design/delete-during-strict-order-gate.md`
- Correctness-fix sites: `cleanup.rs:232`, `filter.rs:65`,
  `planner.rs:113-116`
- PEX site: `transfer.rs:85` (`enter_destination_for_deletion`)
- DEL dispatch: `crates/engine/src/delete/context/core.rs:392`
