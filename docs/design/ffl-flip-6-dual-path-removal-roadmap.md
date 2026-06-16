# FFL-FLIP.6: dual-path removal roadmap (FFL-7 through FFL-15)

Task: FFL-FLIP.6 (#4013). Branch: `docs/ffl-flip-6-dual-path-removal-roadmap`.
Status: ROADMAP - activates after FFL-FLIP.1/.2/.3 conclude GO.
Scope: sequence the PR-by-PR removal of the `flat-flist` Cargo feature, the
`DualFileList` wrapper, the legacy `Vec<FileEntry>` backing store, and the
`#[cfg(feature = "flat-flist")]` site set, in a wire-compatible order with
CI gating and rollback paths defined.

## 1. Prerequisite gate

This roadmap does not activate until ALL of the following conclude GO:

| Gate | Owner | Acceptance signal | Cross-link |
|---|---|---|---|
| FFL-FLIP.1 | RSS comparison at 1M files | Documented projection: flat path -63% RSS vs legacy on vanilla workload | `docs/design/ffl-flip-1-rss-comparison.md` (PR #5827) |
| FFL-FLIP.2 | Throughput regression check | `RSS-A.LAND.4 - RSS-A.LAND.3 <= 5%` measured in podman bench container | `docs/design/flat-flist-throughput-post-migration.md` |
| FFL-FLIP.3 | Decision matrix execution | GO verdict per FFL-FLIP.1 sec. 5 criteria table | `docs/design/ffl-4-flat-flist-flip-decision.md` |
| FFL-FLIP.4 | Cargo flip + 14-day bake | `flat-flist` default-on; no regression flags in nightly CI for 14 days | TBD: tracked by FFL-FLIP.5 bake monitor |

FFL-FLIP.1 (RSS comparison, PR #5827) currently records HOLD pending the
RSS-A.LAND.2/.4 bench numbers; FFL-FLIP.6 stays gated until that HOLD lifts.

If any gate flips to REVERT, this roadmap is shelved and the REVERT path
in FFL-FLIP.1 sec. 5 supersedes it.

## 2. PR-by-PR breakdown

Each row below maps one FFL step to one PR. Sequence is strict because each
PR removes a layer the next depends on. LoC deltas are estimates from the
current cfg-site inventory (37 `#[cfg(feature = "flat-flist")]` sites across
13 files, no `#[cfg(not(feature = "flat-flist"))]` sites - the negative
arm is implicit in compile-out today).

| Step | Scope | Files / cfg-sites | LoC delta | Depends on | Risk | Rollback |
|---|---|---|---|---|---|---|
| FFL-7 | Inventory `#[cfg(feature = "flat-flist")]` sites and classify each as `unconditional-keep` vs `gated-removable`. Produces `docs/audits/ffl-7-cfg-site-inventory.md` table covering 37 known sites. | docs only; no code change | +120 / -0 | FFL-FLIP.4 bake (14 days) passed | Low - audit only | Re-run audit; nothing to revert |
| FFL-8 | Remove `#[cfg(feature = "flat-flist")]` attribute from every site marked `unconditional-keep` in FFL-7 (i.e. drop the gate, keep the body). Legacy `Vec<FileEntry>` impls left in place. | `protocol/src/flist/{mod,sort,accessor,dual}.rs`; `engine/src/delete/{mod,traversal}.rs`; `transfer/src/generator/{mod,sender_accessor}.rs`; `transfer/src/receiver/{mod,entry_accessor}.rs`; `filters/src/lib.rs`; `protocol/tests/flat_flist_transfer_regression.rs` | ~-40 / +0 | FFL-7 | Low-Medium - touches every cfg-gated site, but each individual change is mechanical | Revert single PR; legacy path unaffected |
| FFL-9 | Remove legacy `Vec<FileEntry>` backing store and its `impl FileEntryAccessor for Vec<FileEntry>`. Tests now run exclusively against flat backing store. | `protocol/src/flist/{mod,entry/*,builder.rs}` plus the dual-emit impl on the legacy side | -800 / +20 | FFL-8 (must run first so no live `#[cfg(not(...))]` arms reference removed legacy types) | High - largest single removal; any leftover caller using the legacy type fails to compile | Revert PR; restore legacy backing store; FFL-10..15 stall |
| FFL-10 | Remove `DualFileList` wrapper. Replace usages with direct `FlatFileList`. | `protocol/src/flist/dual.rs` (deletion); all `DualFileList::push/get/iter/flat/legacy` call sites in the workspace | -300 / +120 | FFL-9 (the wrapper has no purpose once the legacy side is gone) | Medium - mechanical rename, but every flist construction site is touched | Revert PR; `DualFileList` returns as a thin alias |
| FFL-11 | Remove `flat-flist` Cargo feature entry from `crates/protocol/Cargo.toml` and any propagating crates. | `Cargo.toml` files; CI matrix removes `--features flat-flist` cells; remove feature-cell job from `_test-features.yml` | -30 / +0 | FFL-10 (the feature has no remaining cfg-site consumer) | Low - cfg removal once nothing references it | Revert PR; feature returns as a no-op |
| FFL-12 | Audit `FileEntryAccessor` trait. Two outcomes: (a) keep as the read-side seam for future arena variants (RSS-A.A.5 follow-up etc.); (b) remove and replace with direct `FlatFileList` API. Pick one based on the open-question outcome in section 5. | `protocol/src/flist/accessor.rs`; consumer crates (`engine/delete`, `transfer/generator`, `transfer/receiver`, `filters`) | (a) +0 / -0; (b) -400 / +200 | FFL-11 | Medium - if (b), trait removal cascades across read sites | Per FFL-9 rollback if (b) destabilises |
| FFL-13 | RSS bench at 1M files with master post FFL-7..12. Compares against FFL-FLIP.1 prediction (~44 MB) and FFL-FLIP.2 baseline (RSS-A.LAND.2). | `xtask/src/commands/benchmark.rs` (cell add); `docs/design/ffl-13-rss-final-validation.md` | docs only; bench cell ~+50 LoC | FFL-12 | Low - measurement; no production code path change | N/A - data only |
| FFL-14 | Document FlatFileList completion. CHANGELOG entry + release notes section under "Internals" (no user-facing behaviour change). | `CHANGELOG.md`; `.github/RELEASE_TEMPLATE.md` if release-note category changes | +60 / +0 | FFL-13 (confirms the win lands as predicted) | Low - docs | N/A |
| FFL-15 | Close FFL series in memory notes. Mark `project_rss_arena_not_landed.md`, `project_rss_arena_hardening.md`, `project_rss_3_11x_upstream.md` as RESOLVED. Update `MEMORY.md` index. | `~/.claude/projects/-Users-ofer-devel-rsync/memory/*.md` (local only - not in PR) | 0 in PR; memory-only edits | FFL-14 | Low - memory hygiene | N/A |

## 3. CI gating

Per-PR required cells (matches existing required checks in branch protection):

| PR | fmt+clippy | nextest (stable, full workspace) | Windows | macOS | Linux musl | Interop Validation | Notes |
|---|---|---|---|---|---|---|---|
| FFL-7 | required | not required (docs only) | not required | not required | not required | not required | Docs-only PR |
| FFL-8 | required | required | required | required | required | required | All cfg-gates removed; both paths exercised by every cell |
| FFL-9 | required | required | required | required | required | required | Largest behavioural surface; full matrix mandatory |
| FFL-10 | required | required | required | required | required | required | Construction-site rename; full matrix mandatory |
| FFL-11 | required | required | required | required | required | required | Loss of `--features flat-flist` matrix cell is THE point |
| FFL-12 | required | required | required | required | required | required | Trait surface change |
| FFL-13 | required | required | required | required | required | not required | Bench-only PR; interop cell unaffected |
| FFL-14 | required | not required | not required | not required | not required | not required | Docs only |

FFL-FLIP.4 (the `flat-flist` default-on flip itself) is the gating event:
FFL-7 does not start until that flip has been default-on in nightly CI for
14 consecutive days without a regression flag per FFL-FLIP.5 bake criteria.
This mirrors the PIP-9.f.1 N-cycle bake pattern.

CI matrix cell `--features flat-flist` is REMOVED at FFL-11. Before FFL-11
lands, the cell must be green continuously to confirm the dual-write path
still works as a control - regression there would force a hold on FFL-9.

## 4. Memory-note hygiene (FFL-15)

FFL-15 updates memory notes in `~/.claude/projects/-Users-ofer-devel-rsync/memory/`.
Memory edits stay local to the user's machine and are NOT shipped in the
final PR - the PR carries only the CHANGELOG/release-notes deltas from
FFL-14.

| Memory file | Update |
|---|---|
| `project_rss_arena_not_landed.md` | Status: RESOLVED. Note: FlatFileList shipped + production path unified through FFL-7..12. |
| `project_rss_arena_hardening.md` | Status: RESOLVED. Cross-link FFL-13 bench evidence. |
| `project_rss_3_11x_upstream.md` | Status: RESOLVED. Quote FFL-13 1M-file RSS final number. |
| `MEMORY.md` | Demote the three above from `Planned Work` to `Completed Initiatives`. Add `feedback_flat_flist_lessons.md` if any post-mortem warranted. |
| `project_post_v059_perf.md` | If FlatFileList was on the list, mark that bullet DONE. |

## 5. Open questions

1. **Keep `FileEntryAccessor` trait or remove (FFL-12)?**
 - Keep argument: the trait is the read-side seam that absorbs future
 arena variants (header compaction beyond 24 B, per-segment arenas under
 INC_RECURSE growth, NUMA-aware arena sharding). Removing it forces
 every future arena experiment back into the legacy-vs-flat coordination
 problem.
 - Remove argument: with one production read path, the trait is dead
 abstraction. SOLID's single-responsibility argues for direct
 `FlatFileList` APIs once there is exactly one implementer. Re-introducing
 a trait later is cheap; carrying an unused one bloats every call site.
 - Decision criterion: are there at least two arena variants planned
 within the next 6 months? If yes (RSS-A.A.5/6/7 sketches under
 discussion), keep the trait. If no, remove it and re-introduce on
 demand. This question stays OPEN until FFL-11 lands; the data point
 is whether anyone has filed a follow-up arena task by then.

2. **Bake-window length after FFL-FLIP.4?**
 - 14 days matches PIP-9.f.1 and ISI.i precedent.
 - Argument for longer (30 days): flat-flist touches the hottest data
 structure in the workspace; regressions may surface only under
 production-scale workloads CI does not exercise.
 - Argument for shorter (7 days): the dual-write path is the riskier
 state, and prolonging it amplifies the FFL-FLIP.1 dual-write floor.
 - Default: 14 days. Revisit if FFL-FLIP.4 bake surfaces any signal.

3. **CHANGELOG category for FFL-14?**
 - "Internals" is the safe category - no user-visible behaviour changes.
 - "Performance" is honest if FFL-13 confirms the -63% RSS prediction
 since RSS at 1M files is a deployment-relevant figure.
 - Recommendation: dual-list under both, with the Performance entry
 carrying the bench number and the Internals entry carrying the
 architecture note.

## 6. Cross-links

- FFL-FLIP.1 RSS comparison: `docs/design/ffl-flip-1-rss-comparison.md` (PR #5827)
- FFL-4 decision matrix: `docs/design/ffl-4-flat-flist-flip-decision.md`
- FFL-1 dual-write audit: `docs/audits/ffl-1-dualfilelist-overhead.md`
- RSS-A.5.a header definition: `docs/design/flat-flist-representation.md`
- RSS-A.LAND series tasks: #3627-#3638
- FFL-FLIP series tasks: #4008-#4013
- FFL-7..15 tasks: #3713-#3721
- Concurrent-path discipline (PIP-7 cautionary tale): `feedback_concurrent_path_discipline.md`
