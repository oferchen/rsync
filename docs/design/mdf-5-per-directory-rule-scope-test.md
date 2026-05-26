# MDF-5 - per-directory rule-scope regression test

Design spec for a regression test suite that validates per-directory
filter rule scoping in the `FilterChain` implementation. When a
`dir-merge` directive (e.g., `: .rsync-filter`) is active, rules read
from per-directory filter files must apply only within that directory
and its descendants (when inheriting) and must be removed from the
active rule set when traversal leaves the directory. This is the core
"rule-scope" invariant that upstream rsync enforces via
`push_local_filters()` / `pop_local_filters()` in `exclude.c`.

The MDF-1 audit ([`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md))
identified the chain-side gap: the `n` modifier (no-inherit) is parsed
but `crates/filters/src/chain/mod.rs` does not drop inherited rules on
directory re-entry when the parent merge rule carries `n` (upstream
`exclude.c:802-803`). MDF-5 is the regression test that ensures both
inheriting and non-inheriting per-directory scopes are correctly
pushed and popped.

## 1. Scope

MDF-5 adds a test module exercising:

- Rules from a per-directory `.rsync-filter` apply to files within that
  directory.
- Rules from a per-directory `.rsync-filter` do NOT apply to files in
  sibling or ancestor directories after the traversal leaves.
- Nested merge files: inner-directory rules override outer-directory
  rules for their subtree (first-match-wins from innermost scope outward).
- Rule removal on `leave_directory`: after the guard is consumed, all
  rules introduced by that directory's merge file are absent from
  evaluation.
- Interaction between anchored patterns (`/pattern`) inside per-dir
  merge files and the scope boundary.
- The `n` (no-inherit) modifier: rules do not propagate to
  subdirectories when `DirMergeConfig::with_inherit(false)` is set.

Non-scope: MDF-5 does not fix bugs in the chain implementation. If
the test exposes a behavioural divergence from upstream, the test is
committed with `#[ignore]` and a tracking comment. The fix is a
separate PR.

## 2. Pre-conditions

The following already exist on `master`:

- Filter chain implementation at `crates/filters/src/chain/` with
  `enter_directory` / `leave_directory` push/pop mechanics and
  `DirFilterGuard`.
- `DirMergeConfig` at `crates/filters/src/chain/config.rs` with
  `with_inherit`, `with_exclude_self`, and modifier application.
- `DirScope` and `has_matching_rule` at
  `crates/filters/src/chain/scope.rs`.
- MDF-1 audit doc confirming the gap
  ([`docs/audit/merge-modifier-coverage.md`](../audit/merge-modifier-coverage.md)).
- MDF-3 design spec at
  [`docs/design/mdf-3-nested-merge-depth-test.md`](mdf-3-nested-merge-depth-test.md)
  covering deep nesting (depth 3/5/10) but not rule-scope isolation.
- Existing chain tests at `crates/filters/src/chain/tests.rs` covering
  basic enter/leave with merge files; MDF-5 extends these with
  additional targeted scenarios.
- `tempfile` is a dev-dependency of `crates/filters`.

## 3. Test scenarios

### 3.1 Rules apply inside their directory

**Setup:** Root directory with `.rsync-filter` containing `- *.log`.
Create files `root/file.log` and `root/file.txt`.

**Assert:** After `enter_directory(root)`, `chain.allows("file.log", false)`
returns `false` and `chain.allows("file.txt", false)` returns `true`.

### 3.2 Rules do NOT apply outside their directory

**Setup:** Two sibling directories `alpha/` and `beta/`. Only `alpha/`
has a `.rsync-filter` with `- *.tmp`. `beta/` has no filter file.

**Assert:**
1. Enter `alpha/` - `chain.allows("file.tmp", false)` is `false`.
2. Leave `alpha/`.
3. Enter `beta/` - `chain.allows("file.tmp", false)` is `true`.

This is the core regression for rule-scope leakage.

### 3.3 Rules do not apply to parent after leave

**Setup:** Directory `parent/child/` where only `child/` has a
`.rsync-filter` with `- secret.*`.

**Assert:**
1. Enter `parent/` (no merge file found - guard has `pushed_count == 0`).
2. Enter `child/` - `chain.allows("secret.key", false)` is `false`.
3. Leave `child/` - `chain.allows("secret.key", false)` is `true`.
4. Leave `parent/` - `chain.allows("secret.key", false)` remains `true`.

### 3.4 Nested merge files - inner overrides outer for its subtree

**Setup:** `outer/` has `.rsync-filter` with `- *.dat`. `outer/inner/`
has `.rsync-filter` with `+ important.dat` followed by `- *.dat`.

**Assert:**
1. Enter `outer/` - `chain.allows("important.dat", false)` is `false`
   (matches the outer exclude).
2. Enter `outer/inner/` - `chain.allows("important.dat", false)` is
   `true` (inner include fires first, inner scope evaluated before outer).
3. `chain.allows("other.dat", false)` is `false` (inner exclude catches it).
4. Leave `outer/inner/` - `chain.allows("important.dat", false)` reverts
   to `false` (only outer scope active).
5. Leave `outer/` - `chain.allows("important.dat", false)` is `true`
   (no rules remain).

### 3.5 Rule removal on dir-leave restores prior state exactly

**Setup:** Global rules contain `- *.bak`. Enter a directory whose
`.rsync-filter` adds `- *.tmp`. Leave that directory.

**Assert:** After leave, `chain.allows("file.bak", false)` is still
`false` (global persists) and `chain.allows("file.tmp", false)` is
`true` (dir-scope rule is gone). `chain.scope_depth()` returns 0.

### 3.6 Anchored patterns inside per-dir merge files

**Setup:** Directory `project/` has `.rsync-filter` with `/build` (an
anchored exclude pattern - leading `/` means it only matches at the
root of that merge file's scope).

**Assert:**
1. Enter `project/` - `chain.allows("build", true)` is `false`
   (anchored pattern matches at this scope's root).
2. A nested path `sub/build` should NOT match the anchored pattern
   (upstream rsync anchors per-dir patterns relative to the directory
   containing the merge file).

### 3.7 No-inherit modifier (`n`) - rules scoped to containing dir only

**Setup:** `DirMergeConfig::new(".rsync-filter").with_inherit(false)`.
Directory tree: `root/child/grandchild/`. Only `root/` has a
`.rsync-filter` with `- *.secret`.

**Assert:**
1. Enter `root/` - `chain.allows("data.secret", false)` is `false`.
2. Enter `root/child/` - with `no-inherit`, the rule from `root/`
   must NOT propagate. `chain.allows("data.secret", false)` should
   be `true`.
3. Enter `root/child/grandchild/` - same: `true`.

This test directly exercises the MDF-1 gap ("`n` parsed but chain does
not drop inherited rules on directory re-entry"). If the current
implementation fails this assertion, the test ships `#[ignore]` with
a tracking comment for the follow-up fix PR.

### 3.8 Inheriting mode (default) - rules propagate to descendants

**Setup:** Same tree as 3.7 but with default `DirMergeConfig::new(".rsync-filter")`
(inheriting). Only `root/` has `.rsync-filter` with `- *.secret`.

**Assert:**
1. Enter `root/` - `chain.allows("data.secret", false)` is `false`.
2. Enter `root/child/` - rule inherited: still `false`.
3. Enter `root/child/grandchild/` - still `false`.
4. Leave `grandchild/`, leave `child/`, leave `root/` - `true`.

This is the positive baseline for 3.7.

## 4. Edge cases

### 4.1 Empty `.rsync-filter` in a subdirectory

**Setup:** `root/` has `.rsync-filter` with `- *.log`. `root/sub/` has
an empty (zero-byte) `.rsync-filter`.

**Assert:** The empty merge file does not push a scope (guard's
`pushed_count == 0`). Parent rules remain active: `chain.allows("file.log", false)`
is `false` while inside `root/sub/`. Leaving `root/sub/` does not
corrupt the parent scope.

### 4.2 Symlinked `.rsync-filter`

**Setup (Unix only):** `root/real/` has `.rsync-filter` with `- *.cache`.
`root/linked/` has `.rsync-filter` as a symlink to `../real/.rsync-filter`.

**Assert:** `enter_directory("root/linked/")` follows the symlink and
parses the target's rules. `chain.allows("data.cache", false)` is
`false` inside `root/linked/`. Leaving `root/linked/` removes those
rules.

Gate: `#[cfg(unix)]`.

### 4.3 Deeply nested (3+ levels) with independent rules per level

**Setup:** Four-level tree `l1/l2/l3/l4/`. Each level has its own
`.rsync-filter`:
- l1: `- *.l1`
- l2: `- *.l2`
- l3: `- *.l3`
- l4: `- *.l4`

**Assert (inside l4):**
- `*.l1`, `*.l2`, `*.l3`, `*.l4` all excluded.
- `*.txt` included.

**Assert (after leaving l4, inside l3):**
- `*.l1`, `*.l2`, `*.l3` excluded.
- `*.l4` included (scope removed).

**Assert (after leaving l3, inside l2):**
- `*.l1`, `*.l2` excluded.
- `*.l3`, `*.l4` included.

And so on up the stack. This validates correct pop ordering across
multiple levels.

### 4.4 Multiple dir-merge directives active simultaneously

**Setup:** Two merge configs registered:
- `DirMergeConfig::new(".rsync-filter")`
- `DirMergeConfig::new(".exclude")`

Directory has both files:
- `.rsync-filter` contains `- *.log`
- `.exclude` contains `- *.tmp`

**Assert:**
1. Enter directory - `pushed_count == 2`.
2. Both rules active: `*.log` excluded, `*.tmp` excluded, `*.txt` included.
3. Leave directory - both scopes removed: `*.log` included, `*.tmp` included.

### 4.5 Comments-only `.rsync-filter` (no effective rules)

**Setup:** Directory has `.rsync-filter` containing only `# comment` lines
and blank lines.

**Assert:** `pushed_count == 0`. No scope pushed. Parent rules unaffected.

### 4.6 Re-entering a directory after leaving it

**Setup:** Directory `alpha/` has `.rsync-filter` with `- *.bak`.

**Assert:**
1. Enter `alpha/` - `*.bak` excluded.
2. Leave `alpha/` - `*.bak` included.
3. Enter `alpha/` again - `*.bak` excluded again.
4. Leave `alpha/` - `*.bak` included.

This validates that repeated enter/leave cycles do not accumulate stale
scopes.

### 4.7 Anchored pattern does not escape its scope on leave

**Setup:** `root/` has `.rsync-filter` with `/private`. `root/sibling/`
has no filter file.

**Assert:**
1. Enter `root/` - `chain.allows("private", true)` is `false`.
2. Leave `root/`.
3. Enter `root/sibling/` - `chain.allows("private", true)` is `true`
   (the anchored pattern from `root/` is gone).

## 5. Module placement

```
crates/filters/tests/mdf_5_per_dir_rule_scope.rs
```

A single integration test file under `crates/filters/tests/`. This
follows the same pattern as existing test files:
- `crates/filters/tests/dir_merge_rules.rs`
- `crates/filters/tests/dir_merge_parsing_comprehensive.rs`

The test file uses `tempfile::TempDir` for filesystem fixtures.
No new dev-dependencies are needed.

## 6. Fixture strategy

All fixtures are created at test runtime using `tempfile::TempDir` and
`std::fs::write`. There are no committed fixture files for MDF-5 (unlike
MDF-3 which uses committed golden fixtures). The rationale:

- MDF-5 scenarios are small (1-4 directories, 1-3 files per directory).
  Inline construction in each test function is more readable and
  self-contained than external fixture trees.
- Filter evaluation is tested against `FilterChain::allows()` directly,
  not against a full transfer simulation. No expected-transfer-list
  snapshots are needed.
- Each test constructs its own `TempDir`, writes the required directory
  structure and `.rsync-filter` files, then drives `FilterChain` through
  the enter/leave sequence.

### 6.1 Helper pattern

A small private helper at the top of the test file:

```rust
fn setup_chain(configs: &[&str]) -> FilterChain {
    let mut chain = FilterChain::empty();
    for filename in configs {
        chain.add_merge_config(DirMergeConfig::new(*filename));
    }
    chain
}
```

Each test then calls `setup_chain(&[".rsync-filter"])` (or with
multiple filenames for scenario 4.4) and manually drives
`enter_directory` / `leave_directory` over `TempDir` paths.

## 7. Assertion approach

Each test asserts which files would be included or excluded at each
traversal point by calling:

- `chain.allows(Path::new("filename"), is_dir)` - returns `true` for
  include, `false` for exclude.
- `chain.allows_deletion(Path::new("filename"), is_dir)` - where
  protect/risk rules are involved.
- `chain.scope_depth()` - validates the internal scope stack size.
- `guard.pushed_count()` - validates how many scopes a directory
  contributed.

The tests do NOT perform filesystem walks or simulate full rsync
transfers. They exercise the `FilterChain` API directly, which is the
unit under test. Integration with the actual file-list builder
(generator/receiver) is covered by interop tests elsewhere.

### 7.1 Assertion matrix per scenario

| Scenario | Key assertion | Expected outcome |
|----------|---------------|-----------------|
| 3.1 | `allows("file.log")` inside dir | `false` |
| 3.2 | `allows("file.tmp")` in sibling without filter | `true` |
| 3.3 | `allows("secret.key")` after leaving child | `true` |
| 3.4 | `allows("important.dat")` in inner override | `true` |
| 3.5 | `scope_depth()` after full leave cycle | `0` |
| 3.6 | anchored `/build` does not match `sub/build` | `true` for nested |
| 3.7 | no-inherit: child does not see parent rule | `true` (may `#[ignore]`) |
| 3.8 | inherit: child sees parent rule | `false` |
| 4.1 | empty filter does not corrupt parent | `false` (parent rule active) |
| 4.2 | symlink target rules active in linked dir | `false` |
| 4.3 | correct pop ordering across 4 levels | per-level exclusion |
| 4.4 | two merge configs both contribute | `pushed_count == 2` |
| 4.5 | comments-only produces no scope | `pushed_count == 0` |
| 4.6 | re-entry produces fresh scope | `scope_depth == 1` on re-enter |
| 4.7 | anchored pattern gone after leave | `true` in sibling |

## 8. Known implementation gaps

Based on the MDF-1 audit and code review of
`crates/filters/src/chain/mod.rs`:

1. **No-inherit (`n`) not enforced in chain.** The `enter_directory`
   method does not consult `DirMergeConfig::inherits()` when deciding
   whether parent scopes should be visible to the new directory. The
   current implementation always leaves parent scopes on the stack.
   Scenario 3.7 will expose this if the chain does not mask or pop
   inherited scopes for non-inheriting configs. The test should ship
   `#[ignore]` if this assertion fails on current `master`.

2. **Anchored pattern relativity.** Upstream rsync interprets
   `/pattern` in a per-dir merge file as anchored to the directory
   containing that file. The oc-rsync `FilterSet` anchors patterns to
   the transfer root. Scenario 3.6 may expose a divergence. If so,
   document and `#[ignore]`.

## 9. Test file structure

```rust
// crates/filters/tests/mdf_5_per_dir_rule_scope.rs

use std::fs;
use std::path::Path;
use tempfile::TempDir;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};

fn setup_chain(configs: &[&str]) -> FilterChain { ... }

// --- Section 3: Core scenarios ---

#[test]
fn rules_apply_inside_directory() { ... }           // 3.1

#[test]
fn rules_do_not_apply_in_sibling_directory() { ... } // 3.2

#[test]
fn rules_do_not_apply_to_parent_after_leave() { ... } // 3.3

#[test]
fn nested_inner_scope_overrides_outer() { ... }      // 3.4

#[test]
fn leave_restores_prior_state_exactly() { ... }      // 3.5

#[test]
fn anchored_pattern_scoped_to_containing_dir() { ... } // 3.6

#[test]
// #[ignore] if chain does not enforce no-inherit
fn no_inherit_blocks_propagation_to_child() { ... }  // 3.7

#[test]
fn inherit_propagates_to_descendants() { ... }       // 3.8

// --- Section 4: Edge cases ---

#[test]
fn empty_filter_file_does_not_affect_parent() { ... } // 4.1

#[cfg(unix)]
#[test]
fn symlinked_filter_file_applies_target_rules() { ... } // 4.2

#[test]
fn deeply_nested_four_levels_correct_pop_order() { ... } // 4.3

#[test]
fn multiple_merge_configs_simultaneous() { ... }     // 4.4

#[test]
fn comments_only_filter_no_scope_pushed() { ... }    // 4.5

#[test]
fn reenter_directory_fresh_scope() { ... }           // 4.6

#[test]
fn anchored_pattern_absent_after_leave() { ... }     // 4.7
```

## 10. Acceptance criteria

The implementation PR is accepted when:

1. All test functions from section 9 are present in
   `crates/filters/tests/mdf_5_per_dir_rule_scope.rs`.
2. Tests that exercise known implementation gaps (3.7, possibly 3.6)
   are committed with `#[ignore]` and a comment naming the follow-up
   fix PR when the current chain does not pass them.
3. All non-ignored tests pass on Linux, macOS, and Windows CI.
4. The symlink test (4.2) is gated with `#[cfg(unix)]`.
5. No new workspace dependencies.
6. Total added LoC under 500 (test code only, no production changes).
7. This design spec is referenced in the PR description.

## 11. Upstream references

- `exclude.c:push_local_filters()` (lines 759-825) - enter directory,
  read per-dir merge files, push rules.
- `exclude.c:pop_local_filters()` (lines 830-860) - leave directory,
  restore rule state.
- `exclude.c:802-803` - the `FILTRULE_NO_INHERIT` branch that prevents
  inherited rules from persisting into child directories.
- `exclude.c:change_local_filter_dir()` - depth-tracked push/pop
  coordination.

## 12. Cross-references

- [MDF-1 audit](../audit/merge-modifier-coverage.md) - identifies the
  chain-side gap for `n` (no-inherit) as MDF-5's primary target.
- [MDF-3 design spec](mdf-3-nested-merge-depth-test.md) - deep-nesting
  regression tests (complements MDF-5's scope-isolation focus).
- [MDF-8 filter-diff harness](mdf-8-filter-diff-harness.md) - upstream
  parity tooling (not directly used by MDF-5's unit-level tests, but
  available for future integration-level parity checks).
- `crates/filters/src/chain/mod.rs` - the `FilterChain` implementation
  under test.
- `crates/filters/src/chain/scope.rs` - `DirScope`, `DirFilterGuard`,
  `has_matching_rule`.
- `crates/filters/src/chain/config.rs` - `DirMergeConfig` with
  `with_inherit(false)`.
