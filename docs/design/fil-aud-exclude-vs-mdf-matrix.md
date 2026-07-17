# FIL-AUD: UTS-DD-exclude root causes vs MDF-1..9 coverage matrix

Cross-references the five `UTS-DD-exclude.{1..5}` root causes against the
MDF-1..9 merge/dir-merge test family. The goal is to surface which MDF
specs (and their landed tests) would have caught each root cause and
which would not, so the gap cells become explicit follow-up work for
FIL-AUD-3..5.

This audit is paper-only. No code or tests are added here.

## 1. Inputs

### 1.1 UTS-DD-exclude root causes

Synthesized from `docs/design/uts-dd-fix-plan.md` (exclude/exclude-lsh
section, shipped via PR #5817 / #5842 / #5865 / #5869 / #5880 / #5888 /
#5897 / #5898 / #5899 / #5902).

| ID | Cause | Anchor |
|----|-------|--------|
| .1 | Per-directory scope fall-through on the Deletion path uses synthetic descendants leaked across scopes | `crates/filters/src/decision.rs` Deletion branch; design at `docs/design/uts-dd-fix-plan.md` |
| .2 | Negation `!` scope-isolation - per-scope decision state instead of cross-scope boolean | `crates/filters/src/chain/mod.rs` reset handling |
| .3 | Directory-only unanchored pattern `foo/*/` must not auto-add `/**` descendants | regression at `crates/filters/tests/uts_dd_exclude_3_dir_only_unanchored.rs` |
| .4 | Apply `FILTRULE_SENDER_SIDE` implicitly under `--delete-excluded` | `crates/filters/src/chain/mod.rs:86-91` (`delete_excluded` flag) |
| .5 | Skip implicit `**/` prefix when pattern already contains `**` | regression at `crates/filters/tests/uts_dd_exclude_5_no_double_recursive_wildcard.rs` |

### 1.2 MDF design + test anchors

| ID | Scope | Spec | Test anchor |
|----|-------|------|-------------|
| MDF-1 | Per-modifier coverage audit vs `exclude.c::add_rule()` | `docs/audit/merge-modifier-coverage.md` | audit-only (no test file) |
| MDF-2 | Per-modifier wire-byte test matrix | `docs/design/mdf-2-modifier-test-matrix.md` | wire prefix tests in `crates/protocol/src/filters/prefix.rs` + `crates/filters/src/merge/tests.rs` |
| MDF-3 | Nested-merge-depth regression (depths 3/5/10) | `docs/design/mdf-3-nested-merge-depth-test.md` | `crates/filters/tests/dir_merge_parsing_comprehensive.rs`, fixture under `tests/fixtures/filter-rules/mdf-7-complex/source/deep-nesting/` |
| MDF-4 | Anchor-inside-merged-rule wire-byte test | `docs/design/mdf-4-anchor-inside-merged-rule-test.md` | `crates/filters/tests/anchored_patterns.rs`, `crates/filters/src/chain/tests.rs::merge_file_anchor_*` |
| MDF-5 | Per-directory rule-scope regression | `docs/design/mdf-5-per-directory-rule-scope-test.md` | `crates/filters/src/chain/tests.rs::filter_chain_enter_directory_*` + `crates/filters/tests/dir_merge_rules.rs` |
| MDF-6 | `.rsync-filter` discovery edge cases | `docs/design/mdf-6-rsync-filter-discovery-edge-cases.md` | `crates/filters/src/chain/tests.rs` (presence / permission / parse-error) |
| MDF-7 | Complex `.rsync-filter` fixture for every filter PR | fixture only | `tests/fixtures/filter-rules/mdf-7-complex/source/` |
| MDF-8 | Upstream `--debug=FILTER1,2,3,4` diff harness | `docs/design/mdf-8-filter-diff-harness.md` | `scripts/mdf_8_filter_diff_harness.sh`, `.github/workflows/mdf-8-filter-diff.yml` |
| MDF-9 | Document remaining merge/dir-merge gaps | user doc only | `docs/user/filter-rules-status.md` |

## 2. Coverage matrix

Cells: `covered` = the existing MDF test asserts (or would assert) the
root-cause invariant. `partial` = the MDF test exercises an adjacent
case but not the exact failure mode. `GAP` = no MDF test in this column
covers the root cause.

| UTS-DD root cause | MDF-1 | MDF-2 | MDF-3 | MDF-4 | MDF-5 | MDF-6 | MDF-7 | MDF-8 | MDF-9 |
|---|---|---|---|---|---|---|---|---|---|
| .1 fall-through deletion descendants | GAP | GAP | partial | GAP | partial | GAP | partial | GAP | GAP |
| .2 `!` per-scope reset isolation | partial | GAP | partial | GAP | partial | GAP | partial | GAP | GAP |
| .3 directory-only unanchored `/**` synthesis | GAP | GAP | GAP | GAP | GAP | GAP | GAP | GAP | GAP |
| .4 implicit `SENDER_SIDE` under `--delete-excluded` | partial | partial | GAP | GAP | GAP | GAP | GAP | GAP | partial |
| .5 implicit `**/` over `**`-containing patterns | GAP | GAP | GAP | GAP | GAP | GAP | GAP | GAP | GAP |

Rough coverage: 5 rows x 9 columns = 45 cells. `covered` = 0. `partial`
= 9. `GAP` = 36. Coverage ratio (covered + partial) / total = 20 %.

## 3. Per-row gap analysis

### Row .1 - fall-through deletion descendants

MDF-3 / MDF-5 / MDF-7 exercise scope push/pop and nested merge files
but they all assert the include/exclude *decision* on the single-path
`FilterSet::allows` API, not the deletion-pass scope context that
`DecisionContext::Deletion` synthesises descendants for. The leak that
PR #5880 / #5897 closed was invisible to MDF-5 because the test asserts
on `chain.allows(...)` rather than on the deletion-pass walker.

### Row .2 - `!` per-scope reset

MDF-1 enumerates the `!` modifier in the upstream `add_rule()` audit
and MDF-5 exercises per-directory rule scoping, but no MDF test
asserts how a `!` inside an inner per-directory merge file interacts
with rules inherited from an outer directory's merge file. Per upstream
`exclude.c` (3.4.4), a per-directory `!` clears the WHOLE mergelist,
inherited outer rules included: `push_local_filters()` (`exclude.c:801`)
reclassifies the parent's rules as the inherited tail of the same list,
and the `FILTRULE_CLEAR_LIST` handler (`exclude.c:1399-1400`) runs
`pop_filter_list()` then `listp->head = NULL`, dropping that inherited
tail. Verified against the real rsync 3.4.4 binary (`rsync -n -r -i -F`
transfers `inner/f.outer` after `inner/.rsync-filter` = `!`).

### Row .3 - directory-only unanchored `/**` synthesis

No MDF column covers the descendant-synthesis pathway at all. MDF-2
mentions `FILTRULE_DIRECTORY` only as a wire-byte flag, not as a gate
on synthetic descendant insertion. The fix shipped its own regression
file (`uts_dd_exclude_3_dir_only_unanchored.rs`) outside the MDF
naming, so the matrix records this as an across-the-row gap.

### Row .4 - implicit `SENDER_SIDE` under `--delete-excluded`

MDF-1 lists `s` / `r` in the modifier inventory and the MDF-9 user-gap
doc mentions `--delete-excluded` interactions, but no MDF wire-byte
test (MDF-2) asserts that a plain `- *.tmp` rule emits with the
`s`-prefix on the wire when the chain was constructed with
`delete_excluded = true`. The on-wire divergence that PR #5899 closed
was invisible to MDF-2.

### Row .5 - implicit `**/` over `**`-containing patterns

No MDF column tests pattern rewrites that oc-rsync applies at compile
time. MDF-2 covers the wire prefix only; the pre-compile normalisation
that mutated `foo/**/bar` into `**/foo/**/bar` is a pattern-string
rewrite step with no MDF counterpart. Fix shipped its own regression
file (`uts_dd_exclude_5_no_double_recursive_wildcard.rs`).

## 4. Recommended new MDF tests (input to FIL-AUD-3)

- `MDF-5.1 deletion_context_scope_leak`: assert
  `DecisionContext::Deletion` does not see synthetic descendants from
  a sibling per-directory merge file (closes .1).
- `MDF-5.2 negation_reset_scope_isolation`: assert `!` inside an inner
  per-directory merge file DOES clear the inherited exclude introduced
  by the outer merge file (whole-mergelist reset), matching upstream
  `exclude.c:1399-1400` and the real 3.4.4 binary (closes .2).
- `MDF-2.1 dir_only_unanchored_no_descendants`: assert wire-byte
  parity for `- foo/*/` against upstream `--debug=FILTER` output,
  including the absence of a synthesised descendant rule (closes .3).
- `MDF-2.2 delete_excluded_implicit_sender_side`: wire-byte test
  asserting `- *.tmp` is emitted as `-s *.tmp` (i.e. with the `s`
  prefix) when `delete_excluded` is set on the chain (closes .4).
- `MDF-2.3 no_double_recursive_wildcard_prefix`: wire + decision test
  asserting `foo/**/bar` stays as `foo/**/bar` after normalisation
  and that `**/foo/**/bar` is NOT also emitted (closes .5).
- `MDF-1.a delete_excluded_in_modifier_audit`: extend the MDF-1 audit
  doc to record the implicit-`SENDER_SIDE` rule under
  `--delete-excluded` as a separate row, with a citation to upstream
  `exclude.c:1330-1332`.
- `MDF-8.a delete_excluded_diff_harness_fixture`: feed a
  `--delete-excluded` invocation through the MDF-8 diff harness to
  trip future regressions on .1 / .4 / .5 simultaneously.

## 5. Closure mapping

- FIL-AUD-1 (cross-reference itself): closed by Sections 2 + 3.
- FIL-AUD-2 (per root cause, identify the MDF test that missed it):
  closed by Section 3.
- FIL-AUD-3 (spec new MDF tests for the gap cells): seeded by
  Section 4. The actual specification work happens in the FIL-AUD-3
  follow-up doc.
- FIL-AUD-4 / FIL-AUD-5 remain open downstream of FIL-AUD-3.
