---
name: project_delete_pass_concurrency_integration
description: "INTEGRATION DESIGN (2026-06-21) for making the local-copy --delete path concurrent+performant alongside the exclude/exclude-lsh correctness fix. KEY INSIGHT: the delete path is already a two-phase pipeline (DECIDE single-threaded → frozen plan → EXECUTE parallel) and the phase boundary is the integration seam. Correctness fix + PEX live in DECIDE (serial, Rc/RefCell); DEL parallelizes EXECUTE only. DATA REALITY: all DEL bench tables are PLACEHOLDERS — G2/G3 gates never measured, DashMap verdict pending; DEL stays opt-in. Doc: docs/design/delete-pass-concurrency-integration.md."
metadata:
  node_type: memory
  type: project
  originSessionId: 38892565-d20d-449f-ac89-0a69dc002bb5
---

**Two-phase pipeline (already exists) = the integration seam.** Local-copy delete is DECIDE→freeze→EXECUTE:
- **DECIDE (Phase 1, single-threaded):** `build_plan_for_directory` (cleanup.rs:161) → `enter_destination_for_deletion` (transfer.rs:85) → `allows_deletion` (filter.rs:65) → `DeletePlan`. CopyContext filter structs are `Rc<RefCell>` = !Send/!Sync (context.rs:91-95,108), so this phase CANNOT be parallel.
- **Plan is FROZEN** (cleanup.rs:91-106): the parallel consumer consumes already-decided `DeleteEntry`s and NEVER touches filter state. Verified: `allows_deletion` only called from Phase-1 sites.
- **EXECUTE (Phase 2, parallel):** `ctx.emit_one` (cleanup.rs:138) → `ParallelDeleteEmitter::run` (delete/context/core.rs:392, feature-gated) → rayon intra-cohort unlink.

**Mapping (3 independent PRs, 3 axes):**
- keep_names correctness fix → DECIDE, serial. Changes plan MEMBERSHIP. (see [[project_upstream_deletion_exclusion_mechanism]])
- **PEX** (incremental dest filter stack) → DECIDE, serial. Win is ALGORITHMIC not parallel: `enter_destination_for_deletion` re-walks every ancestor per dir = O(depth²)/subtree (transfer.rs:95-139); upstream `change_local_filter_dir` (exclude.c:875) is O(1)-amortized depth-indexed stack (pop frames ≥depth, push one). Recursion already has dir-enter/leave hooks (guard pattern recursive/mod.rs:202) to install it.
- **DEL** (parallel consumer) → EXECUTE. ALREADY WIRED into engine local-copy path (not just receiver). Invariant: cross-cohort STRICT-SERIAL, intra-cohort parallel; cohort idx single-threaded pre-order; NDX_DEL_STATS one folded frame the consumer never emits.

**DATA REALITY (surfaced loud, governs "performant" claim):**
- DEL bench corpus (del-4a/4b, dashmap-vs-mutex-100k/1m, dmb-b) = ALL TEMPLATE PLACEHOLDERS. DEL-4.c gates G2(≥1.5×@100K)/G3(≥2.0×@1M)/G4(1t no-regress) NEVER measured. DashMap-vs-Mutex<HashMap> for DeletePlanMap = no verdict. DEL is opt-in (`--features parallel-delete-consumer`), unproven.
- PEX win likely SMALL: depth 3-10 → 9-100 extra per-dir merge probes, mostly NotFound stats, cheap vs readdir/unlink. Gate on a deep-tree microbench; DEFER (document) if negligible — don't ship an incremental-stack state machine for a non-win.

**SEQUENCING:** (1) correctness fix first, standalone (campaign blocker, data-loss critical, both-directions test). (2) PEX as pure refactor PR, gated on microbench. (3) DEL: run DEL-4.a/b, fill gates, THEN decide default-on per del-4c — do NOT bundle a default flip into the correctness fix.

**THIRD PILLAR — TOCTOU-safety by construction (added 2026-06-21 per user reframe "upstream deletion/exclusion + concurrency + structurally avoid TOCTOU"):** the frozen-plan seam carries all three properties. EXECUTE is ALREADY dirfd-anchored (SEC-1.q, delete/emitter/mod.rs): `open_plan_dirfd` (343) opens the dir ONCE vs `DirSandbox::root_dirfd`; `open_dir_at` (597) walks component-by-component O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC; deletions are `unlinkat(parent_fd, name)` (312-321) never `unlink(path)`. RESIDUAL WINDOW: Phase 2 RE-RESOLVES `plan.directory` by PATH instead of carrying the Phase-1 dirfd → TOCTOU-resistant (O_NOFOLLOW catches a swapped prefix at open) but not TOCTOU-FREE. STRUCTURAL FIX: carry the DECIDE-phase dirfd as `Arc<OwnedFd>` on the DeletePlan; readdir+fstatat+filter AND unlinkat all hit the SAME kernel object → no re-resolution → no window. `OwnedFd` is Send so the Arc survives the serial→parallel cohort handoff; composes with DEL (one dirfd per cohort) and the keep_names fix (decision independent of the descriptor). Target shape: `DeletePlan { dirfd: Arc<OwnedFd>, entries: [DeleteEntry] }`. Captured in doc §6.

**BINDING GUARDRAIL:** filter DECISION must never migrate to Phase 2. Rc/RefCell across rayon threads = UB + reintroduces source-frame-leak bug class. Two-phase freeze is load-bearing. New concurrent delete behaviour ships only with adversarial-ordering stress before any default-on (PIP-7 precedent, [[feedback_concurrent_path_discipline]]).

Related: [[project_upstream_deletion_exclusion_mechanism]], [[project_exclude_deletion_pass_perdir_merge]], [[project_delete_pass_dest_rooted_filter_design]], [[project_delete_consumer_single_threaded]].
