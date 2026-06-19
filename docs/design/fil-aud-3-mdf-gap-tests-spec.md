# FIL-AUD-3 - MDF gap-cell test specification

Specifies the new MDF tests that close the gap cells flagged by
[`fil-aud-exclude-vs-mdf-matrix.md`](fil-aud-exclude-vs-mdf-matrix.md)
(FIL-AUD-2). Each cell maps a UTS-DD-exclude root cause to the MDF
column that should have caught it but did not. This doc is the spec
only; FIL-AUD-4 implements the Rust tests and FIL-AUD-5 runs the
upstream-diff verification.

Upstream `exclude.c` references throughout cite
`target/interop/upstream-src/rsync-3.4.1/exclude.c`. Fetch with
`bash tools/ci/run_interop.sh` if absent.

No Rust code is added by FIL-AUD-3. Test sketches use pseudo-form
(setup / invocation / assert) so FIL-AUD-4 can lift them directly
into `crates/filters/tests/mdf_*.rs` or `crates/filters/src/chain/tests.rs`.

## 1. Gap-cell -> new-test map

| New test | Gap cell | Root cause | MDF column |
|---|---|---|---|
| `MDF-5.1 deletion_context_scope_leak` | row .1 x MDF-5 | Per-directory scope fall-through on Deletion uses synthetic descendants | MDF-5 |
| `MDF-5.2 negation_reset_scope_isolation` | row .2 x MDF-5 | `!` per-scope reset isolation across scopes | MDF-5 |
| `MDF-2.1 dir_only_unanchored_no_descendants` | row .3 x MDF-2 | Directory-only unanchored `foo/*/` must not auto-add `/**` | MDF-2 |
| `MDF-2.2 delete_excluded_implicit_sender_side` | row .4 x MDF-2 | Apply `FILTRULE_SENDER_SIDE` implicitly under `--delete-excluded` | MDF-2 |
| `MDF-2.3 no_double_recursive_wildcard_prefix` | row .5 x MDF-2 | Skip implicit `**/` when pattern already contains `**` | MDF-2 |
| `MDF-1.a delete_excluded_modifier_audit_row` | row .4 x MDF-1 | Document implicit-`s` flip in modifier audit | MDF-1 |
| `MDF-8.a delete_excluded_diff_harness_fixture` | rows .1/.4/.5 x MDF-8 | Wire-diff harness catches regressions of .1/.4/.5 together | MDF-8 |

The 5 row x 9 column matrix had 36 GAP cells. The seven new tests close
the highest-value cells (the ones with no partial coverage elsewhere on
the row). Remaining `partial` cells stay partial; row .3 and row .5 had
no `partial` cells at all, so the wire-byte (MDF-2) tests close the only
columns where regression coverage is feasible without re-running the
single-path API tests already shipped in the UTS-DD regressions.

## 2. Test specifications

### 2.1 MDF-5.1 `deletion_context_scope_leak`

**Closes:** UTS-DD-exclude.1 (per-directory scope fall-through on
Deletion uses synthetic descendants).

**Upstream:** `exclude.c:rule_matches()` (around lines 903-960) returns
"no match" for descendants of a per-directory scope when the parent
scope already popped. `exclude.c:check_filter()` (around lines 770-820)
walks rules per scope; the Deletion path must observe the same scope
visibility. See also `crates/filters/src/decision.rs:26-65` for the
`Deletion` context wiring already in tree.

**File:** `crates/filters/tests/mdf_5_1_deletion_context_scope_leak.rs`.

**Setup:**

```
src/
  alpha/
    .rsync-filter   -> "- *.tmp"
    a.tmp
    a.keep
  beta/
    b.tmp
    b.keep
```

Build a `FilterChain` with a `dir-merge .rsync-filter` directive.
Walk `alpha/` then `beta/` via `enter_directory` / `leave_directory`.

**Assertions:**

1. Inside `alpha/`, `decide(DecisionContext::Deletion, "a.tmp", false)`
   reports excluded; `a.keep` reports allowed.
2. After `leave_directory("alpha")` and `enter_directory("beta")`,
   `decide(DecisionContext::Deletion, "b.tmp", false)` reports allowed
   (no synthetic descendant of `alpha/.rsync-filter` survives).
3. `chain.scope_depth()` returns 1 inside `beta/`, 0 after leave.
4. Sanity: the same assertions hold for
   `DecisionContext::Transfer`, isolating the regression to the
   Deletion branch in `decision.rs:89-99`.

**Negative control:** A nested `alpha/inner/` inheriting the rule must
still exclude `inner/c.tmp` under Deletion. This guards against an
overcorrection that drops inheritance entirely.

---

### 2.2 MDF-5.2 `negation_reset_scope_isolation`

**Closes:** UTS-DD-exclude.2 (`!` cross-scope reset isolation).

**Upstream:** `exclude.c:parse_rule_tok()` around the `FILTRULE_CLEAR_LIST`
handler (PR #5898 cited `exclude.c:1393-1402`, see also the existing
oc-rsync comment at `crates/filters/src/merge/read.rs:87`). `!` must
clear only the rules in the active per-directory scope, not rules
inherited from outer merge files.

**File:** `crates/filters/tests/mdf_5_2_negation_reset_scope_isolation.rs`.

**Setup:**

```
src/
  .rsync-filter   -> "- *.outer"
  inner/
    .rsync-filter -> "!\n- *.inner"
    f.outer
    f.inner
```

Build a `FilterChain` with `dir-merge .rsync-filter`. Walk
`src/` then `src/inner/`.

**Assertions:**

1. Inside `src/inner/`, `chain.allows("f.inner", false)` is `false`
   (inner exclude fires).
2. Inside `src/inner/`, `chain.allows("f.outer", false)` is still
   `false` (outer exclude is NOT reset by inner `!`).
3. After `leave_directory("inner")`, `chain.allows("f.outer", false)`
   stays `false` (outer scope intact, inner reset was scope-local).
4. After `leave_directory("src")`, `chain.allows("f.outer", false)`
   is `true` (no scopes active).

**Negative control:** A top-level `--filter "!"` (outside any merge
file) MUST still clear the global list, mirroring upstream
`exclude.c:1393-1402` for the non-merge code path.

---

### 2.3 MDF-2.1 `dir_only_unanchored_no_descendants`

**Closes:** UTS-DD-exclude.3 (directory-only unanchored `foo/*/`
must not auto-synthesise `foo/*/**`).

**Upstream:** `exclude.c:rule_matches()` line 938-939 returns no match
for non-directory candidates when `FILTRULE_DIRECTORY` is set, so no
implicit descendant rule is emitted. Compare against the existing
single-path regression at
`crates/filters/tests/uts_dd_exclude_3_dir_only_unanchored.rs`; the new
test extends the assertion to the **wire bytes** emitted for the rule.

**File:** Extend
`crates/filters/tests/dir_merge_parsing_comprehensive.rs` or add
`crates/filters/tests/mdf_2_1_dir_only_unanchored_wire.rs`.

**Setup:**

```
filter rules: "- foo/*/"
              "+ foo/s?b/"
```

Compile through `FilterChain` and serialise the on-wire rule list
via the protocol crate's `filters::wire::encode` path used by
`crates/protocol/src/filters/prefix.rs`.

**Assertions:**

1. Exactly two rules appear on the wire: `- foo/*/` and `+ foo/s?b/`.
2. No synthetic `- foo/*/**` rule is emitted.
3. Decision parity: `set.allows("foo/sub/file1", false)` is `true`.
4. Decision parity under `DecisionContext::Deletion`: still allowed
   (the dir-only gate suppresses synthetic descendants on both paths).

**Diff harness tie-in:** Feed the same fixture into the MDF-8 harness
under `scripts/mdf_8_filter_diff_harness.sh` so a regression flips
both this unit test and the upstream-diff CI gate.

---

### 2.4 MDF-2.2 `delete_excluded_implicit_sender_side`

**Closes:** UTS-DD-exclude.4 (apply `FILTRULE_SENDER_SIDE` implicitly
under `--delete-excluded`).

**Upstream:** `exclude.c:parse_rule_tok()` lines 1324-1332 - when the
`delete_excluded` global is set, per-token exclude rules acquire
`FILTRULE_SENDER_SIDE` implicitly. See the existing wiring at
`crates/filters/src/chain/mod.rs:86-91, 527-541`
(`apply_merge_implicit_sender_side`).

**File:** `crates/filters/tests/mdf_2_2_delete_excluded_sender_side_wire.rs`.

**Setup:**

```
merge file `.rsync-filter`: "- *.tmp"
chain config: FilterChain::new(...).with_delete_excluded(true)
```

Build the chain, enter a directory containing the merge file, then
inspect the rule list both via the decision API and via the protocol
wire encoder.

**Assertions:**

1. After `enter_directory`, the in-memory `FilterRule` carries the
   `applies_to_sender` flag (FILTRULE_SENDER_SIDE).
2. The protocol wire encoding emits the rule with the `s` short-prefix
   (`-s *.tmp`), not bare `- *.tmp`.
3. With `delete_excluded = false` on the chain, the same merge file
   yields a bare `- *.tmp` rule (sanity baseline).
4. The implicit flip applies only to merge-expanded rules, not to
   user-typed `--filter` rules already carrying explicit `r` or `s`
   prefixes (no double-application).

**Negative control:** An `include` rule (`+ *.keep`) MUST NOT acquire
the implicit `s` flip even under `delete_excluded` (upstream gates
the OR on the exclude branch only).

---

### 2.5 MDF-2.3 `no_double_recursive_wildcard_prefix`

**Closes:** UTS-DD-exclude.5 (skip implicit `**/` prefix when pattern
already contains `**`).

**Upstream:** `exclude.c:rule_matches()` lines 903-960 use
`wildmatch_array(..., slash_handling = -1)` (see
`lib/wildmatch.c:316`) for unanchored patterns with a `**` element;
upstream does **no** pattern-string rewriting. The single-path parity
already lives at
`crates/filters/tests/uts_dd_exclude_5_no_double_recursive_wildcard.rs`;
the new test extends parity to the **wire bytes** emitted for the
normalised rule list.

**File:** `crates/filters/tests/mdf_2_3_no_double_star_prefix_wire.rs`.

**Setup:**

```
filter rules: "- foo/**/bar"
              "- **/baz"
              "- bar"  // baseline: implicit prefix still applies
```

Compile through `FilterChain` and inspect the post-normalisation rule
list via the protocol wire encoder.

**Assertions:**

1. `- foo/**/bar` survives as exactly one wire rule. No
   `- **/foo/**/bar` companion is emitted.
2. `- **/baz` survives as exactly one wire rule.
3. `- bar` is rewritten to `- **/bar` (the implicit prefix DOES fire
   for patterns without `**`, matching the existing UTS-20 carve-out).
4. Decision parity: `set.allows("foo/x/y/bar", false)` is `false`
   under both `Transfer` and `Deletion` contexts (the kept matcher
   honours cross-segment `**` semantics).

---

### 2.6 MDF-1.a `delete_excluded_modifier_audit_row`

**Closes:** Row .4 x MDF-1 (the modifier audit doc had no row for
the implicit `s` flip under `--delete-excluded`).

**Upstream:** `exclude.c:parse_rule_tok()` lines 1324-1332.

**File:** Extend `docs/audit/merge-modifier-coverage.md` with a new row.
This is doc-only (no test code); FIL-AUD-4 lifts it directly when
landing the MDF-2.2 wire test, since the audit row and the test share
a single upstream citation.

**Audit row content:**

| Modifier / hook | Upstream | oc-rsync | Wire flag |
|---|---|---|---|
| Implicit FILTRULE_SENDER_SIDE under `delete_excluded` | `exclude.c:1324-1332 parse_rule_tok` | `chain/mod.rs:537 apply_merge_implicit_sender_side` | `s` short-prefix on the wire (`-s pattern`) |

**Assertion:** doc presence; FIL-AUD-5 CI lint asserts the row text
is byte-stable.

---

### 2.7 MDF-8.a `delete_excluded_diff_harness_fixture`

**Closes:** Combined rows .1, .4, .5 x MDF-8. A single `--delete-excluded`
invocation through the MDF-8 wire-diff harness trips on any of the
three root causes simultaneously, giving CI a wide net for future
regressions without each one needing its own harness run.

**Upstream:** Same as 2.3 / 2.4 / 2.5 above; the harness asserts wire
parity against upstream `rsync --debug=FILTER1,2,3,4`.

**File:** New fixture under
`scripts/fixtures/mdf-8/delete-excluded/` plus a harness invocation
entry in `scripts/mdf_8_filter_diff_harness.sh` and the matching
GitHub Actions step in `.github/workflows/mdf-8-filter-diff.yml`.

**Setup:**

```
src/
  .rsync-filter -> |
    - *.tmp
    - foo/*/
    - foo/**/bar
  alpha/
    a.tmp
    foo/sub/file1
    foo/x/y/bar
```

Invocation: `rsync -av --delete --delete-excluded --filter=': .rsync-filter' src/ dst/`.

**Assertions (driven by the harness, not by Rust unit tests):**

1. The MDF-8 diff harness compares the upstream `--debug=FILTER`
   output against the oc-rsync trace produced by
   `crates/filters/src/debug.rs`. They MUST match byte-for-byte after
   the canonical trailing-whitespace strip the harness already applies.
2. The destination tree after the invocation matches between upstream
   and oc-rsync (same set of `Only in:` lines from `diff -ruN`, i.e.
   empty).
3. The harness JSON summary records `delete_excluded = true` so the
   FIL-AUD-5 close-out can grep for the fixture's presence.

---

## 3. Out of scope

- `partial` cells in rows .1, .2, .4 (MDF-3 / MDF-5 / MDF-7 / MDF-9)
  are left as-is. Their adjacent coverage is sufficient given the
  new tests close the primary regression vectors. Promoting a
  `partial` to `covered` would duplicate the assertions in 2.1 / 2.2
  without gaining new wire or fixture surface.
- UTS-DD-exclude.1..5 root causes themselves are already pinned by
  the single-path regression files
  (`uts_dd_exclude_3_*.rs`, `uts_dd_exclude_5_*.rs`,
  `uts20_exclude_lsh_repro.rs`). FIL-AUD-3 adds the MDF-shaped
  coverage that closes the wire-byte and per-directory-scope gaps.
- New behavioural fixes. If any new MDF test exposes a residual bug
  not closed by the UTS-DD PR chain, FIL-AUD-4 lands it `#[ignore]`
  with a tracking comment and the fix becomes its own PR.

## 4. Follow-ups

- **FIL-AUD-4** implements the seven test sketches in this doc.
  Each test lives under `crates/filters/tests/mdf_<n>_*.rs` (matching
  the existing UTS-DD regression file naming) except for 2.6
  (`merge-modifier-coverage.md` audit row) and 2.7 (MDF-8 fixture +
  harness script + workflow step).
- **FIL-AUD-5** verifies the new tests on both `master` and a synthetic
  revert of each UTS-DD fix PR (#5817 / #5842 / #5865 / #5869 / #5880 /
  #5888 / #5897 / #5898 / #5899 / #5902) and records the
  pass-on-master / fail-on-revert evidence in the FIL-AUD-5 close-out
  doc.

## 5. References

- FIL-AUD-2 matrix: `docs/design/fil-aud-exclude-vs-mdf-matrix.md`
- MDF-1 audit: `docs/audit/merge-modifier-coverage.md`
- MDF-2 spec: `docs/design/mdf-2-modifier-test-matrix.md`
- MDF-5 spec: `docs/design/mdf-5-per-directory-rule-scope-test.md`
- MDF-8 harness: `docs/design/mdf-8-filter-diff-harness.md`
- UTS-DD fix plan: `docs/design/uts-dd-fix-plan.md`
- Upstream `exclude.c`: `target/interop/upstream-src/rsync-3.4.1/exclude.c`
- In-tree decision wiring: `crates/filters/src/decision.rs`
- In-tree chain wiring: `crates/filters/src/chain/mod.rs`
- Existing UTS-DD regressions:
  `crates/filters/tests/uts_dd_exclude_3_dir_only_unanchored.rs`,
  `crates/filters/tests/uts_dd_exclude_5_no_double_recursive_wildcard.rs`
