# `.rsync-filter` Per-Directory Inheritance vs Upstream rsync 3.4.1

Compares oc-rsync's per-directory merge to upstream rsync 3.4.1,
focused on `.rsync-filter` inheritance and dir-merge modifiers.
Tracking issue: #2128.

## 1. Upstream behaviour

Source: `target/interop/upstream-src/rsync-3.4.1/exclude.c`.

- `parse_rule_tok()` (lines 1130-1292) parses `.` (merge) and `:`
  (dir-merge) prefixes. The `:` case at lines 1181-1184 sets
  `FILTRULE_PERDIR_MERGE | FILTRULE_FINISH_SETUP`. The modifier loop
  (1215-1289) recognises `-`/`+` (`NO_PREFIXES` / forced include),
  `/` (`ABS_PATH`), `!` (`NEGATE`), `C` (CVS preset, 1248-1255 forces
  `NO_PREFIXES|WORD_SPLIT|NO_INHERIT|CVS_IGNORE`), `e`
  (`EXCLUDE_SELF`, MERGE-only), `n` (`NO_INHERIT`, MERGE-only),
  `p` (`PERISHABLE`), `r`/`s` (sides), `w` (`WORD_SPLIT`),
  `x` (`XATTR`).
- `parse_filter_str()` (1404-1444) intercepts `MERGE_FILE` rules:
  plain `merge` slurps the file immediately; `dir-merge` during
  `parent_dirscan` pre-loads the parent chain (1419-1428).
- `add_rule()` (162-285) registers a per-dir-merge entry as a
  `filter_rule_list` attached to the rule (252-282).
- `check_filter()` (1038-1065) recurses into a nested
  `FILTRULE_PERDIR_MERGE` list, returning the first matching
  include/exclude.
- `push_local_filters()` / `pop_local_filters()` (759-825, 827-895)
  maintain the per-directory stack. On entry, `parse_filter_file()`
  parses `.rsync-filter`. Line 802 wipes `lp->head` if
  `FILTRULE_NO_INHERIT` is set, so descendants start empty. Line 729
  mirrors that during `parent_dirscan`.
- `FILTRULE_EXCLUDE_SELF` (1409-1418) prepends an exclude rule for
  the merge filename via `add_rule()`.

## 2. Inheritance rule

1. Rules from a parent's `.rsync-filter` apply to that directory and
   all descendants.
2. Descendant `.rsync-filter` rules are appended (and evaluated
   first), so innermost wins on a match while outer rules still apply
   to non-matching paths.
3. The `n` (no-inherit) modifier severs inheritance: descendants see
   only their own file. The `C` preset implies `n`.
4. The `e` (exclude-self) modifier hides the merge file from
   transfer.
5. `!` / `clear` inside a `.rsync-filter` wipes accumulated rules in
   that scope (and its inherited tail).

## 3. oc-rsync implementation

Filter chain (`crates/filters/src/chain.rs`):

- `DirMergeConfig` (53-171): `with_inherit` (90-95),
  `with_exclude_self` (100-104), `with_sender_only` /
  `with_receiver_only` (109-122), `with_anchor_root` (125-129),
  `with_perishable` (132-136).
- `FilterChain::enter_directory` (304-379) reads each configured
  merge file, applies `apply_modifiers`, optionally appends an
  exclude-self rule (355-358), and pushes a `DirScope` keyed by
  depth.
- `leave_directory` (389-392) pops every scope at that depth.
- Evaluation walks scopes innermost-first (`scopes.iter().rev()`)
  then falls back to the global `FilterSet` (`allows` 258-266;
  `allows_deletion` 274-282).

Engine loader (`crates/engine/src/local_copy/dir_merge/`):

- `mod.rs` (1-15) re-exports the recursive loader.
- `load.rs::load_dir_merge_rules_recursive` (115-238) parses a
  `.rsync-filter`, follows nested `merge` directives with cycle
  detection (120-129), and propagates `clear_inherited` through
  `DirMergeEntries::extend` (96-105).
- `parse/modifiers.rs` (130-217) maps modifier characters: `+`/`-`
  enforce kind, `c` (142-156) sets whitespace + `inherit(false)` +
  list-clearing, `e`/`n`/`w`/`s`/`r`/`/` mirror upstream spelling.

CLI: `crates/cli/src/frontend/filter_rules/arguments.rs:97-104`.
First `-F` injects `dir-merge /.rsync-filter`; second injects
`exclude .rsync-filter`.

## 4. Gaps

- **`n` not enforced at scope push.** `DirMergeConfig::inherits()`
  is plumbed but `FilterChain::enter_directory` never consults it.
  Outer scopes always remain visible. Upstream wipes `lp->head`
  (exclude.c:802).
- **Deep nesting (>= 10 levels).** No regression coverage with mixed
  `n` / `!` directives.
- **Implicit `-F` vs explicit `--filter='dir-merge .rsync-filter'`.**
  Both reach `FilterChain` but via different code paths; modifier
  and side-flag plumbing lacks parity tests.
- **Modifier coverage.** No interop assertions for `w`/`s`/`r`/`p`
  alongside `n`/`e`. `C` preset must imply `NO_INHERIT|WORD_SPLIT`
  - assert both. `a` is not an upstream modifier; verify rejection.
- **Case sensitivity.** Pattern matching is byte-exact; on
  case-insensitive filesystems (default Windows / APFS)
  `.rsync-filter` lookup is case-sensitive at the `fs::read_to_string`
  call (chain.rs:318), matching upstream POSIX behaviour. Document
  the cross-platform divergence.
- **`exclude-self` order.** oc-rsync appends the synthetic exclude
  after parsed rules (chain.rs:357); upstream prepends it via
  `add_rule()` (exclude.c:1416). Diverges only when a rule explicitly
  includes the merge filename.

## 5. Test plan

Fixture: `tests/integration_filter_inheritance.rs` running both
upstream rsync (when `target/interop/bin/rsync-3.4.1` is present) and
oc-rsync against the same source tree, diffing the destinations.

1. Build `level0/.../level12/` with a `.rsync-filter` at every level
   alternating include/exclude rules and randomised file inventories.
2. Cases:
   - Plain inheritance, no modifiers.
   - `dir-merge,n .rsync-filter` at level 4 - descendants must lose
     levels 0-3 rules.
   - `dir-merge,e .rsync-filter` - assert filter file absent in
     destination.
   - `dir-merge,Cw .rsync-filter` - CVS preset semantics.
   - `!` / `clear` inside level 7 - ancestor rules wiped only within
     that scope.
   - Side modifiers `s`/`r` combined with `--delete`.
3. For each case, run upstream and oc-rsync into separate
   destinations and `diff -r`. Skip with a warning if the upstream
   binary is missing.
4. Property test: random nesting depth 1-20 with random rules,
   confirming `FilterChain::scope_depth()` matches upstream's
   `mergelist_cnt` accounting after each enter/leave pair.
