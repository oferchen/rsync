# DG-2.c: Decide atomic swap vs phased migration for DG-2.a

DG-2.a (`docs/design/dg-2a-option-b-spec.md`) fixed the target shape for
the `SlotBarrier` split into `BarrierState` + `SlotData`, and recommended
an atomic single-PR cutover in s.7. DG-2.b
(`docs/design/dg-2b-migration-order.md`) specified the opposite end of
the planning space: a seven-step phased recipe sized so each step is
independently shippable, bisectable, and reversible. This document picks
between the two and closes the DG-2 design series (#2605, #2647, #2648,
#2649) so DG-3.a (#2690) can start immediately.

## 1. The two options

### Phased (DG-2.b's recommendation)

Seven PRs, landing sequentially per DG-2.b s.3:

- **DG-3.a** - introduce `BarrierState`, `SlotData`, `SlotEntry`
  alongside the existing `SlotBarrier`, gated under `#[allow(dead_code)]`.
- **DG-3.b** - swap the DashMap value type from `Arc<SlotBarrier>` to
  `SlotEntry`; retarget `finish_file`'s `Arc::try_unwrap` to
  `entry.data`. Spin-then-yield loop stays in place verbatim;
  `SlotBarrier` survives as a transitional adapter.
- **DG-3.c** - migrate `DecrementGuard` + `SlotHandle` to the new
  ownership shape. This is the structural race-fix step: the worker's
  drop-body Arc and the flusher's unwrap target become disjoint
  allocations. Delete the transitional `SlotBarrier` adapter.
- **DG-3.d** - audit-only verification of `finish_file` `try_unwrap`
  invariants under the new shape.
- **DG-3.e** - stress test `concurrent_register_and_dispatch` under
  Option B; confirm SSC-1's `registrations_done` gate from PR #4667
  still holds.
- **SPL-38.e** - extract `finish_file` + `flush_workers` into
  `drain.rs`; delete the spin-then-yield workaround as part of the
  move, gated on DG-3.e's stress test.
- **DG-4** - cleanup marker; either deletes the spin loop from
  `drain.rs` (if SPL-38.e moved it verbatim) or closes as a no-op.

The spin-then-yield workaround stays in place from master through
DG-3.b and DG-3.c, then is removed in SPL-38.e or DG-4 once DG-3.e
proves the race is closed.

### Atomic swap

One PR that adds `BarrierState` + `SlotData` + `SlotEntry`, retypes
`DecrementGuard.barrier` and the `SlotHandle` fields, migrates all 25
call sites enumerated in DG-2.b s.1 (T1..T8, F1, C1..C7, W1, H1..H4,
U1, S1, B1, N1), deletes `SlotBarrier`, and removes the
spin-then-yield workaround - all in the same commit.

DG-2.a s.7 made the case for this option: the affected types are
crate-private, the change does not cross any public API boundary, and
the diff is mechanical. The single PR avoids the transitional
`SlotBarrier` adapter that DG-3.b introduces (DG-2.b s.7 calls out
that adapter as the phased recipe's main design risk).

## 2. Per-criterion comparison

| Criterion | Phased | Atomic |
|---|---|---|
| Bisectability | Each step bisectable; regression isolates to one of seven small diffs | Single giant diff hides which sub-change introduced any regression |
| Diff size per PR | Small: ~70 LoC (DG-3.a), ~120 LoC (DG-3.b), ~60+80 LoC (DG-3.c), audit/test-only for d/e, ~170 LoC move (SPL-38.e), ~65 LoC delete (DG-4) | ~1500 LoC across 25 call sites in one PR |
| Review cost | 7 small reviews, each ~30 min for a familiar reviewer | 1 review needing several hours and full reload of DG-1/DG-2.a context |
| Risk of mid-flight regression | Spread across 7 PRs; per-step nextest + clippy gate at each step | Concentrated in 1 PR; any escaped regression lands as a single large revert target |
| Time to "race fixed in master" | ~3 PRs (DG-3.a + DG-3.b + DG-3.c); race-fix lands in DG-3.c | 1 PR |
| Reversibility per step | Each step has a clean `git revert`; compound rollbacks (DG-2.b s.6) revert in reverse order | All-or-nothing; revert of the atomic PR rolls back the entire restructure including the type definitions |
| Test-coverage gate at each step | DG-3.e is the empirical gate that bounds when the spin-then-yield workaround can come out; stress test runs against the production shape | Single PR must pass the same stress test, but failure means the whole restructure goes back to the drawing board |
| Risk of correlated regression | Lower: a small diff makes correlated changes (e.g. a test fixture flake landing alongside a real race) easy to disentangle | Higher: any correlated regression in the atomic PR is hard to isolate from the 25-site mechanical churn |
| Window of intermediate state on master | DG-3.a through DG-3.e ship coexisting `SlotBarrier` + Option B shapes for days/weeks; spin loop remains observable in master that whole time | None - master flips in one commit |
| Author cognitive load | Medium: split the work across seven PRs; each PR holds a bounded portion of context | High: carry the whole 25-site change set through review, rebase, and CI cycles |
| CI cost | 7 CI cycles (fmt+clippy, nextest stable, Windows, macOS, Linux musl + interop) | 1 CI cycle |
| Merge contention with concurrent work | Higher: 7 PRs to rebase against `parallel_apply.rs` if concurrent work lands on the same file | Lower: 1 PR to rebase |
| Documentation overhead | 7 PR descriptions, 7 commit messages, 7 sets of release-note labels | 1 of each |
| Risk of phased recipe-specific design hazard | DG-3.b's transitional `SlotBarrier` adapter is the phased recipe's main design risk per DG-2.b s.7; reviewers must confirm it does not accidentally close the race window earlier than DG-3.c intends | None - no adapter, no transitional shape |

## 3. Recommendation: phased (DG-2.b's proposal)

**Adopt the phased recipe.** The stronger bisectability and
smaller-review story outweigh the 7x CI cost. The "race fixed in
master in 3 PRs" milestone is acceptable for a non-user-visible race
that has been latent for months and is currently mitigated by the
spin-then-yield workaround in production.

Supporting reasoning:

- **Bisectability is structurally valuable.** The DG-1 race took
  multiple platform-specific stress tests to surface
  (`project_concurrent_dispatch_test_flake`,
  `project_slothandle_decrementguard_release_race`). If the Option B
  restructure introduces a new race on a path the existing stress
  suite does not cover, the phased recipe lets `git bisect` pinpoint
  the offending step in minutes. The atomic recipe forces a
  full-context re-read of a ~1500 LoC diff to localise the same
  regression.
- **DG-3.b's transitional adapter is mechanical, not deep.** The
  primary objection to the phased approach (DG-2.b s.7) is that the
  `SlotBarrier` adapter could accidentally close the race window
  before DG-3.c. The adapter as specified in DG-2.b s.3 (DG-3.b
  section) is a thin wrapper that delegates each method to
  `BarrierState` or `SlotData`; it adds no third Arc allocation and
  retains the existing `Arc<SlotBarrier>` graph for
  `DecrementGuard.barrier`. The race window stays open across DG-3.b
  exactly as in master.
- **Reviewer bandwidth.** Seven reviews of ~30 min each are easier to
  schedule than one review of several hours. The codebase has
  multiple reviewers familiar with `parallel_apply.rs`; the phased
  recipe spreads load across them.
- **Race-fix delivery time.** DG-3.c lands the structural race fix.
  At a normal cadence of one PR per few days, the race is closed in
  master within ~1-2 weeks of starting the sequence. This is
  acceptable given the race has been mitigated by the spin-then-yield
  workaround since PR #4665.
- **Compound rollback path exists.** DG-2.b s.6 documents the
  compound rollback procedure (revert in reverse order); each
  individual revert is a clean `git revert`. The phased recipe does
  not lose the safety net the atomic recipe offers.

## 4. When atomic would be the better choice

The phased recommendation is not universal. Atomic becomes preferable
under any of:

- **Beta lockdown.** If the project is in a release-stabilisation
  window where only one cycle of CI risk is acceptable, the atomic PR
  produces one risk event instead of seven. The phased recipe spreads
  risk over time, which is the wrong shape for a stabilisation window.
- **Reviewer-bandwidth scarcity.** If the team cannot schedule seven
  small reviews within a reasonable window (e.g. one week), the
  phased steps will queue and the race fix slips past the time the
  atomic PR would have landed and been reviewed.
- **Mid-flight contention on `parallel_apply.rs`.** If another large
  change is already in flight on the same file (for example, the
  pending SPL-38.a through SPL-38.d submodule extractions), the
  phased recipe forces seven rebases against that work. The atomic
  recipe rebases once. The DG-2 series is explicitly ordered to land
  before SPL-38.e for exactly this reason; if a different concurrent
  change appears, reassess.
- **Risk appetite for one large diff.** If the team has direct
  reviewer experience with similar large-diff restructures in
  `parallel_apply.rs` and trusts the mechanical-churn assessment in
  DG-2.a s.7, the atomic recipe is operationally simpler. The
  phased recipe optimises for "what if something goes wrong"; the
  atomic recipe optimises for "this is mechanical and will land
  cleanly".

## 5. Compromise option: 2-stage atomic (mentioned, not recommended)

A middle path exists: collapse the seven steps into two larger PRs.

- **Stage 1**: combine DG-3.a (add new types) and DG-3.b (migrate
  DashMap value + `finish_file` unwrap target) into one "add new
  types + migrate field" PR. Diff size ~190 LoC. Bisectable boundary:
  before stage 1, master shape; after stage 1, transitional adapter
  shape with `DecrementGuard` still racy and spin loop still in
  place.
- **Stage 2**: combine DG-3.c (race-fix), DG-3.d (audit), DG-3.e
  (stress test), and SPL-38.e (drain extraction + spin deletion)
  into one "swap `DecrementGuard` + verify + stress test + extract
  drain" PR. Diff size ~480 LoC plus the audit doc and stress test.
  Bisectable boundary: after stage 2, race is closed and spin is gone.

This halves the PR count and keeps the race-fix landing under
focused review. The trade-off is that stage 2 bundles four concerns
into one diff: a structural type swap, a written invariant proof, a
new stress test, and a module extraction. If stage 2's stress test
fails, the entire bundle blocks and rolls back the audit and
extraction work along with the race fix.

**Not recommended** because:

- It surrenders the phased recipe's main strength (bisectability)
  for a modest reduction in PR count.
- Stage 2's diff size (~480 LoC) is no longer small enough for a
  one-sitting review.
- DG-3.e is the empirical gate for the spin removal. Bundling
  DG-3.e into the same PR as SPL-38.e removes the gate's function -
  the stress test and the spin removal land together, so a stress
  failure forces both back. The phased recipe lets DG-3.e land
  green first, then SPL-38.e deletes the spin with confidence.

The compromise option exists in this document so DG-2.c does not
silently rule it out. If DG-3.a's review feedback indicates the team
prefers fewer, larger PRs over the seven-step recipe, the compromise
is the structured fallback.

## 6. Open question for future iteration

This decision can be revisited at DG-3.b time. Two concrete signals
should trigger a re-evaluation:

- **If DG-3.a's PR review shows the project bias is "land big and
  fast"** - if reviewers ask why the new types are being added in a
  separate PR instead of with their first use site, the cultural
  signal is to switch to the compromise option starting at DG-3.b.
  Combine DG-3.b through DG-3.e + SPL-38.e + DG-4 into a single
  follow-up PR, keeping DG-3.a as the bisectability anchor.
- **If DG-3.a's PR review is bisect-first** - if reviewers
  explicitly call out the value of the dead-code-gated types as a
  bisect anchor, stay phased. The phased recipe is then validated
  by the review culture and there is no reason to revisit.

This document does not predict which signal will appear. It records
the decision criterion so the next iteration is structured rather
than ad-hoc.

## 7. Closing

This document closes out the DG-2 design series:

- DG-2.a (#2605, merged in PR #4748): target shape specified -
  `BarrierState` + `SlotData` + `SlotEntry`.
- DG-2.b (#2647, merged in PR #4769): phased migration order spec -
  seven steps, per-step risk + reversibility + rollback path.
- DG-2.c (#2648, this document): decision - **adopt the phased
  recipe**. The atomic recommendation from DG-2.a s.7 is retained
  as a target shape for the post-DG-4 state.
- DG-2.d (#2649): unblocks DG-3.a once this decision lands.

DG-3.a (#2690) can start immediately on the phased plan: add
`BarrierState`, `SlotData`, `SlotEntry` alongside the existing
`SlotBarrier`, gated under `#[allow(dead_code)]`. The seven-step
recipe in DG-2.b s.3 is the implementation playbook.

The atomic recipe is not discarded - it is the fallback if DG-3.b's
transitional adapter proves harder to review than DG-2.b s.7
estimates, or if any of the triggers in s.4 above appear during
execution.
