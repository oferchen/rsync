# SPL-8 Still Blocked (#2330)

## Status

SPL-8 (extract the `#[cfg(test)] mod tests` block from
`crates/engine/src/concurrent_delta/spill.rs` into a sibling file)
remains blocked on `origin/master` as of this audit. Splitting tests
alone does not drop the parent under the 650 LoC cap enforced by
`tools/enforce_limits.sh`.

## Math on origin/master

Measured on a worktree branched from `origin/master`
(`refactor/spl-8-tests-split-retry`):

- `wc -l crates/engine/src/concurrent_delta/spill.rs` -> **1232 lines**
- `#[cfg(test)] mod tests { ... }` opens at line 692 and runs to EOF
  (line 1232).
- Test block size: `1232 - 692 + 1 = 541 lines`.
- Non-test code (lines 1-691): **691 lines**.
- Projected parent LoC after a pure test-block split:
  `691 + 2 = 693 lines` (the two added lines are
  `#[cfg(test)] #[path = "spill_tests.rs"] mod tests;` plus its
  attribute, written one per line in the parent).

693 LoC is still above the 650 LoC cap, so a tests-only extraction
does not unblock `enforce_limits.sh`. SPL-8 cannot ship in isolation
from `origin/master`.

## What needs to land first

The decomposition plan
(`docs/audits/spill-rs-decomposition-plan.md`) targets six code
submodules; two of them are the load-bearing reductions for the
parent file:

| Task  | Submodule           | PR    | Status on origin/master | Effect on parent |
|-------|---------------------|-------|--------------------------|------------------|
| SPL-3 | `spill/tempfile.rs` | #4434 | Open, not merged         | Removes `SpillBackend`, `ReadWriteSeek`, `open_backend`, plus the spill/reload/dir-recreate I/O helpers. |
| SPL-4 | `spill/buffer.rs`   | #4426 | Open, not merged         | Removes `SpillableReorderBuffer<T>` and its full `impl` block (the largest single chunk in the file). |

PR #4426's own description states the parent shrinks from 1232 to
**282 lines** once SPL-4 lands. Once both SPL-3 and SPL-4 are merged,
the residual parent sits well below 650 LoC even before tests move,
so the SPL-8 test split becomes purely cosmetic - the cap is no
longer the constraint.

## Why a tests-only split now is not enough

Tracing the test block boundary against `origin/master`:

```
parent total          = 1232
non-test (1..=691)    =  691
test block (692..=1232) = 541
projected parent post-split = 691 + 2 = 693   (> 650, still over cap)
```

The non-test surface (691 lines) is itself over the 650-line cap, so
no amount of test relocation alone fixes it. The bulk of the
non-test code is the `SpillableReorderBuffer<T>` type and its impl
block (the SPL-4 target) plus the tempfile-backed storage primitives
(the SPL-3 target). Both must move before the parent fits.

## Unblock criteria

SPL-8 can be re-attempted as soon as either of the following is
true on the base branch:

1. **Both SPL-3 (#4434) and SPL-4 (#4426) are merged into
   `master`.** Per the SPL-4 PR body the parent drops to ~282 LoC;
   moving the residual tests to a sibling becomes a tidy follow-up
   (~282 + 2 = 284 LoC parent, far under the cap), and SPL-8
   delivers the original goal of co-locating each test next to its
   submodule per the test split plan.
2. **Only SPL-4 (#4426) merges.** Even alone, removing
   `SpillableReorderBuffer<T>` and its impl + buffer tests is
   sufficient to drop the parent under 650 LoC; SPL-3 is then
   independent and SPL-8's residual tests-split work is again a
   small cleanup rather than a cap-driven extraction.

If only SPL-3 merges and SPL-4 stays open, recompute before
retrying: SPL-3 lifts ~150 LoC of non-test code (lines 163-190 and
564-687 per the decomposition plan) and a few tempfile-specific
tests, so the parent likely still exceeds the cap and SPL-8 stays
blocked.

## Action

No code change in this PR. Re-run SPL-8 only after the
prerequisites above are satisfied; at that point the test split
plan in `docs/audits/spill-rs-decomposition-plan.md` (table under
"Test Split Plan (SPL-8)") gives the per-test destination for the
final move.
