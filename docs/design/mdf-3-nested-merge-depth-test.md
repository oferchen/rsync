# MDF-3 - nested merge depth regression test (depth 3, 5, 10)

Design spec for the regression test that closes
[`docs/user/filter-rules-status.md` section 3.7](../user/filter-rules-status.md)
("Nested merge depth beyond two unvalidated"). The implementation PR
that lands the fixtures and the test file is tracked separately; this
document fixes the surface area, fixture layout, test functions, and
acceptance bar so the implementation work has no design ambiguity left.

## 1. Scope

MDF-3 specs a regression test suite that exercises nested `dir-merge`
directives at depths 3, 5, and 10. The suite validates that the
oc-rsync filter engine handles deep nesting without:

- Stack overflow in the recursive parser
  (`crates/filters/src/merge/parse.rs` and the chain's `enter_directory`
  hook at `crates/filters/src/chain/mod.rs:172`).
- Rule duplication across scopes (the same rule fires twice because the
  per-directory push/pop in `crates/filters/src/chain/scope.rs` leaks
  on re-entry).
- Out-of-order rule application (a deeper-level include is masked by a
  shallower exclude that should have gone out of scope).
- Performance pathology (O(N^2) on depth - e.g. re-walking the parent
  chain on every level descent).
- Behaviour divergence vs upstream rsync 3.4.1 for the same fixture.

Non-scope: MDF-3 does not introduce new modifiers, does not change
chain semantics, and does not extend the parser. It is regression
coverage only. Bug fixes uncovered while writing the test belong to
MDF-2 (parse-side) or MDF-5 (chain-side) per the MDF-1 audit
([`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md)).

## 2. Pre-conditions

The following already live on `master` and the implementation PR
consumes them as-is:

- MDF-7 complex fixture at
  `tests/fixtures/filter-rules/mdf-7-complex/source/` (PR #4946).
  Includes a `deep-nesting/` subdirectory that exercises a depth-4
  `dir-merge` chain (`l1/l2/l3/.rsync-filter`). MDF-3 picks up where
  MDF-7 stopped.
- MDF-1 audit doc at
  [`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md)
  (PR #4900) - confirms the modifier semantics each nested level must
  preserve.
- MDF-9 user-facing gap doc at
  [`docs/user/filter-rules-status.md`](../user/filter-rules-status.md)
  (PR #4919) - section 3.7 is the gap this test closes.
- MDF-8 diff harness at `scripts/mdf_8_filter_diff_harness.sh` and
  [`docs/design/mdf-8-filter-diff-harness.md`](mdf-8-filter-diff-harness.md)
  (PR #4950) - the upstream-vs-oc-rsync parity-check tool the
  implementation reuses.
- Filter chain implementation at `crates/filters/src/chain/` and
  merge-file parser at `crates/filters/src/merge/parse.rs`.
- `tempfile` is already a dev-dependency of `crates/filters` (used by
  `crates/filters/tests/dir_merge_rules.rs`).

## 3. Fixture extension

MDF-7's `deep-nesting/` exercises depth 4. MDF-3 adds three sibling
fixtures, one per target depth. The fixtures live alongside MDF-7
under `tests/fixtures/filter-rules/`:

```
tests/fixtures/filter-rules/mdf-3-nested-depth/
    README.md
    generate.sh
    depth-3/
        source/
            .rsync-filter           # level 1
            l1/.rsync-filter        # level 2
            l1/l2/.rsync-filter     # level 3
            l1/l2/keep-l3.txt
            l1/l2/drop-l3.txt
            l1/keep-l2.txt
            l1/drop-l2.txt
            keep-l1.txt
            drop-l1.txt
        expected/
            transfer-list.txt
            exclude-list.txt
    depth-5/
        source/
            .rsync-filter
            l1/.rsync-filter
            l1/l2/.rsync-filter
            l1/l2/l3/.rsync-filter
            l1/l2/l3/l4/.rsync-filter
            l1/l2/l3/l4/keep-l5.txt
            l1/l2/l3/l4/drop-l5.txt
            ... (keep-lN/drop-lN per level N in [1..5]) ...
        expected/
            transfer-list.txt
            exclude-list.txt
    depth-10/
        source/
            ... (10 nested levels, same pattern) ...
        expected/
            transfer-list.txt
            exclude-list.txt
```

### 3.1 Per-level rule shape

Every level N carries a `.rsync-filter` that adds exactly two rules:

```
# Level N
+ keep-lN.txt
- drop-lN.txt
```

Plus the dir-merge re-anchor that propagates discovery into the next
level:

```
: .rsync-filter
```

So each level's full `.rsync-filter` content is three non-comment lines.
The depth-N fixture therefore puts N rules of each kind in scope at the
deepest directory, and the expected transfer list is the union of the
`keep-lN.txt` files for every level (each present in its own
directory), with every `drop-lN.txt` excluded.

### 3.2 Fixture data invariants

- Every test file is empty (zero bytes). The test exercises the filter
  engine, not the transfer codec; size and content are irrelevant.
- File modes are the repository default (644 for files, 755 for
  directories). No special bits.
- No symlinks in the depth-3 / depth-5 / depth-10 source trees; the
  symlink edge case (section 6) is exercised by a separate fixture
  branch under `depth-5/`.
- Filenames are stable lowercase ASCII; no Unicode normalisation or
  case-folding traps. The MDF-7 fixture already covers those.

### 3.3 Expected lists

`expected/transfer-list.txt` and `expected/exclude-list.txt` follow the
exact MDF-7 conventions documented at
`tests/fixtures/filter-rules/mdf-7-complex/README.md`:

- Paths relative to the destination root.
- Sorted lexicographically with `LC_ALL=C sort`.
- One entry per line.
- Trailing slash denotes a directory.
- Encodes the **upstream rsync 3.4.1 target**, not current oc-rsync
  behaviour. If oc-rsync diverges, the test fails and the bug is
  filed under MDF-2 or MDF-5.

## 4. Test structure

A single test file consumes all three fixtures:

```rust
// crates/filters/tests/mdf_3_nested_merge_depth.rs

#[test]
fn dir_merge_depth_3_matches_upstream() {
    // Loads tests/fixtures/filter-rules/mdf-3-nested-depth/depth-3/,
    // walks oc-rsync's filter chain over the materialised source tree,
    // asserts A1+A2 against expected/transfer-list.txt and
    // expected/exclude-list.txt.
}

#[test]
fn dir_merge_depth_5_matches_upstream() {
    // Same shape, depth 5.
}

#[test]
fn dir_merge_depth_10_matches_upstream() {
    // Same shape, depth 10. The depth-10 case is the canary for
    // recursion-depth and rule-bookkeeping bugs.
}

#[test]
fn dir_merge_depth_10_no_stack_overflow() {
    // Stress test: spawns the depth-10 walk on a thread with a
    // 256 KiB stack and asserts the walk completes without abort.
    // Forces the regression we expect to break first if the parser
    // is recursive in a way that grows stack linearly with depth.
}

#[test]
fn dir_merge_depth_10_perf_bound() {
    // Asserts wall-clock < 100 ms on the depth-10 walk. Bound is
    // measured on ubuntu-latest GitHub Actions runners; section 10
    // documents the slack we allow on slower runners.
}
```

All five tests share helpers from a private `mod common;` block at the
top of the file (fixture loader, transfer-list normaliser, upstream
parity invoker). No helper is reused outside the test file - keep the
surface area small.

## 5. Behavioural assertions per fixture

Each `*_matches_upstream` test enforces A1-A5; the stress and perf
tests enforce A3 and A4 respectively in isolation:

- **A1** - file inclusion: the set of paths the filter chain decides to
  include matches `expected/transfer-list.txt` byte-for-byte after the
  same sort/normalise pass MDF-7 uses.
- **A2** - file exclusion: the set of paths the filter chain decides to
  exclude matches `expected/exclude-list.txt` byte-for-byte.
- **A3** - no panic: the depth-10 walk completes without panicking
  (stack overflow surfaces as SIGABRT on the worker thread; the test
  asserts the join handle returns `Ok`).
- **A4** - perf bound: at depth 10, the full filter-engine walk over
  the fixture completes in under 100 ms wall-clock on
  ubuntu-latest. Tighter than this is unnecessary; looser is a
  regression vs the small file count.
- **A5** - upstream parity: at every level, the filter chain's
  per-decision log matches upstream rsync 3.4.1's
  `--debug=FILTER2,3` output set-equal after the MDF-8 harness
  normalisation pass. A5 is enforced only when the MDF-8 harness is
  available (rsync 3.4.1 binary present at the expected interop path);
  otherwise the assertion is skipped with a `println!` note, never with
  a panic. The pre-MDF-8 fallback compares the include/exclude sets
  only (A1 + A2), which is a strict subset of A5.

## 6. Edge cases to include

The implementation PR must add these as additional fixtures under
`depth-5/` (the middle depth, deep enough to be representative,
shallow enough that failure points are easy to read). Each edge case
gets its own subdirectory and its own pair of expected lists, so a
failure pinpoints the modifier under test.

- **`n` modifier at depth N**: a `dir-merge` `:n .rsync-filter` rule at
  level 3 of a 5-deep tree. Rules from level 3 must NOT inherit into
  level 4 or level 5. Tests the gap reported in
  [`filter-rules-status.md` section 3.3](../user/filter-rules-status.md)
  at depth.
- **`e` modifier at depth N**: a `dir-merge` `:e .rsync-filter` rule at
  level 3. The merge file at level 3 itself must be excluded from
  the transfer. Tests the gap reported in
  [`filter-rules-status.md` section 3.2](../user/filter-rules-status.md)
  at depth.
- **Empty `.rsync-filter` at depth N**: a zero-byte merge file at
  level 3. Must not break parent inheritance and must not panic the
  parser. Pulls in the discovery edge cases listed in
  [`filter-rules-status.md` section 3.10](../user/filter-rules-status.md).
- **Symlinked `.rsync-filter` at depth N**: at level 4, the merge file
  is a symlink to `../l3/.rsync-filter`. The fixture documents the
  expected oc-rsync behaviour (follow the symlink, apply the merged
  rules at level 4's scope) in its `README.md` and pins the
  expected lists accordingly. If oc-rsync currently treats symlinked
  merge files as opaque, the expected lists encode the upstream target
  and the test fails - that failure is the bug report.
- **Cyclic symlink loop at depth 5**: at level 5 of a depth-5 tree,
  `l5/.rsync-filter` is a symlink to `../l3/.rsync-filter`, and
  `l3/.rsync-filter` references `: .rsync-filter` so re-discovery
  would recurse. The chain must not loop infinitely; either it
  detects the cycle and terminates, or it relies on the filesystem
  `realpath` cap. Test asserts the walk completes in bounded time.
  Symlinks are skipped on Windows via `#[cfg(unix)]`.

## 7. Test infrastructure

Concrete file layout for the implementation PR:

- **`tests/fixtures/filter-rules/mdf-3-nested-depth/generate.sh`** -
  idempotent POSIX shell script that re-creates the entire
  `depth-3/`, `depth-5/`, `depth-10/` tree from a seed config block at
  the top of the file. The script is checked into the repo. The
  fixtures themselves are also checked in (the script exists as the
  authoritative re-creation recipe, not as a build step). Running
  `generate.sh` after a fixture edit must produce a `git status` diff
  that is empty - i.e. the script's output and the committed tree are
  bit-identical. CI does not invoke `generate.sh`; it is operator
  tooling.
- **`tests/fixtures/filter-rules/mdf-3-nested-depth/README.md`** -
  documents the per-level rule shape, the keep/drop file convention,
  the edge-case branches under `depth-5/`, and the regeneration
  recipe. Mirrors the MDF-7 fixture's README in tone and structure.
- **`crates/filters/tests/mdf_3_nested_merge_depth.rs`** - the test
  file. Uses `tempfile::TempDir` to copy each fixture into a
  short-lived working tree per test (the test must not mutate the
  checked-in fixture). Materialisation step is a simple recursive
  copy; the MDF-3 fixtures have no `dot-git/` rename quirk so no
  `materialize.sh` is needed.
- No new workspace dependencies. `tempfile` is already in
  `crates/filters`'s `dev-dependencies`. Upstream rsync 3.4.1 is
  located via the same path probe MDF-8 uses
  (`target/interop/upstream-src/rsync-3.4.1/rsync` or
  `$RSYNC_BIN`); absence skips A5 with a printed note.

## 8. Upstream parity check

A5 piggybacks on the MDF-8 harness:

1. Locate the upstream rsync 3.4.1 binary via `RSYNC_BIN` env or the
   interop cache path (`target/interop/upstream-src/rsync-3.4.1/rsync`).
   Build it if missing via the same fetch recipe the interop harness
   uses (`tools/ci/run_interop.sh`).
2. Run
   `rsync --dry-run -av --debug=FILTER2,3 source/ dest/ 2>&1 \
   | grep '\[FILTER'`
   against the materialised fixture tree.
3. Run the equivalent `oc-rsync` invocation with `--debug=FILTER` (the
   level-1 wiring MDF-8 documents as the cross-binary common
   denominator).
4. Diff the two streams through the normalisation pipeline at
   [`docs/design/mdf-8-filter-diff-harness.md`](mdf-8-filter-diff-harness.md)
   section 3. The set-equality assertion is A5.
5. If the upstream binary is not found, the test logs `[mdf-3] skipping
   A5: upstream rsync 3.4.1 not available at <path>` and asserts
   A1+A2 only. No panic, no skip macro, no env-flag gymnastics.

The skip-cleanly behaviour matches the pattern used in
`crates/filters/tests/dir_merge_rules.rs` for environment-dependent
helpers.

## 9. Acceptance criteria for the implementation PR

The implementation PR is accepted when:

1. Three fixture trees committed at depths 3, 5, 10 with deterministic
   `.rsync-filter` content and `expected/transfer-list.txt` +
   `expected/exclude-list.txt` snapshots per fixture (plus the
   edge-case branches under `depth-5/`).
2. `generate.sh` is checked in and produces zero diff against the
   committed tree.
3. `crates/filters/tests/mdf_3_nested_merge_depth.rs` ships the five
   test functions named in section 4. All five pass on master at the
   time of the merge; if a test exposes a real bug that requires an
   MDF-2 or MDF-5 fix to pass, the test is committed with `#[ignore]`
   and a tracking comment naming the follow-up PR.
4. Total test runtime (all five tests) under 30 seconds on a single
   ubuntu-latest runner. The depth-10 perf bound (A4) is the long
   pole; if it climbs past 100 ms it is tuned to the 200 ms slack
   documented in section 10.
5. No new dependencies added to the workspace (`Cargo.toml` diff is
   limited to no-op).
6. `docs/user/filter-rules-status.md` section 3.7 is updated in the
   same PR to point at the new test file and to move "Nested merge
   depth beyond two unvalidated" from the gap list to the "Fully
   supported" list, contingent on all A1-A5 assertions being green.

## 10. Risk surface

Known risks and pre-emptive mitigations:

- **Depth-10 may surface a real recursion bug in the parser.** This is
  the point - the test exists precisely to catch it. If it does, the
  test ships `#[ignore]` with a tracking comment and the fix is
  scheduled as MDF-2 or MDF-5 follow-up.
- **A4 (perf bound) may fail on slower CI runners.** macOS runners
  and Windows runners are roughly 1.5-3x slower than Linux on file
  walks. The mitigation is a tiered bound: 100 ms on ubuntu-latest,
  200 ms on macos-latest, 300 ms on windows-latest. The test reads
  `cfg!(target_os = ...)` to pick the bound at compile time.
- **Fixture traversal order may differ.** Upstream rsync walks
  depth-first; oc-rsync's chain also walks depth-first
  (`crates/filters/src/chain/mod.rs:172` is invoked per directory in
  the receiver's depth-first scan). The fixture and expected lists are
  authored against depth-first order to match.
- **Symlink edge case may not work on Windows.** Cyclic-symlink and
  symlinked-merge-file branches are gated `#[cfg(unix)]`. Windows runs
  the canonical fixtures only.
- **`generate.sh` may drift from the committed tree.** Mitigation: the
  PR description names the regeneration recipe; a future CI lint
  (out of MDF-3 scope) can compare `generate.sh` output to the
  committed tree.

## 11. Cross-references

- [MDF-1 audit](../audit/merge-modifier-coverage.md) - PR #4900,
  per-modifier coverage table that motivates the edge-case branches
  in section 6.
- [MDF-7 complex fixture](../../tests/fixtures/filter-rules/mdf-7-complex/README.md)
  - PR #4946, the depth-4 baseline this spec extends.
- [MDF-8 filter-decision diff harness](mdf-8-filter-diff-harness.md)
  - PR #4950, the A5 upstream-parity tooling.
- [MDF-9 user-facing gap doc](../user/filter-rules-status.md)
  - PR #4919, section 3.7 is the gap this test closes.
- Memory note: `[[project_merge_dir_merge_filter_incomplete]]`.
